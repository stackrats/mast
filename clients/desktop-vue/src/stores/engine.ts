import { acceptHMRUpdate, defineStore } from "pinia";

import type {
  Action,
  DiscoveredProject,
  EnginePatch,
  EngineSnapshot,
  DockerStatus,
  HistoryEntry,
  IntegrationSettings,
  LogCapture,
  LogLine,
  OperationId,
  ProjectCommand,
  ProjectId,
  ProjectSummary,
  RepairOffer,
  UsageSample,
  WorkspaceSummary,
} from "../bindings";
import { EngineSync, type SyncPhase } from "../lib/engineSync";
import { stripAnsi } from "../lib/ansi";
import { formatElapsed } from "../lib/elapsed";
import { notify } from "../lib/notify";
import {
  loadCapturesSeen,
  loadLogsOpen,
  recordWorkspaceStart,
  saveCapturesSeen,
  saveLogsOpen,
} from "../lib/prefs";
import { applyPatchEvent } from "../lib/projects";
import { pushBounded } from "../lib/ring";
import {
  cancelOperation,
  dispatchAction,
  envReport,
  historyRecent,
  logCaptures,
  onPatchStreamItem,
  startCaptureStream,
  startHistoryStream,
  startUsageStream,
  stopUsageStream,
  streamServiceLogs,
  stopLogStream,
  tauriPatchTransport,
} from "../lib/transport";

const OP_LINE_CAP = 200;
const LOG_LINE_CAP = 500;
const ACTIVITY_CAP = 2000;
/** Matches the engine's own ring, so the panel never shows more than the
 * engine still remembers. */
const HISTORY_CAP = 300;
/** Captures held in memory. The engine keeps more on disk; this is what the
 * tab shows without asking for more, and each one carries up to 200 lines. */
const CAPTURES_CAP = 50;
/** Usage samples kept for the sparklines — 60 at the engine's 2s cadence is
 * about two minutes, which is long enough to tell a spike from a climb. */
const USAGE_CAP = 60;

/** One line in the global activity feed (the bottom logs panel). */
export interface ActivityLine {
  n: number;
  line: string;
  stderr: boolean;
}

export interface OperationView {
  /** Client-side generation token: identity must be compared by value, never
   * by object reference — Pinia hands out reactive proxies. */
  token: number;
  id: OperationId;
  label: string;
  /** Wall-clock start, for the elapsed readout. A cold image build runs for
   * tens of minutes, and "how long has this been going" is the question the
   * user actually has while watching it. */
  startedAt: number;
  lines: { line: string; stderr: boolean }[];
  /** Output lines ever received, not the number still held. `lines` is a
   * bounded ring, so its length plateaus once a chatty command fills it —
   * which would read to the readiness watcher as the command falling
   * silent, exactly when it is at its busiest. */
  received: number;
  terminal: "completed" | "failed" | "cancelled" | null;
  error: string | null;
  /** A one-click repair the engine matched to this operation's failure —
   * rendered as a Fix button whose preview says what will change. */
  /** Repairs the engine matched to this operation (a failure signature, or
   * follow-up system steps a feature must not do silently) — each renders
   * as its own Fix button. */
  fixes: { repair: RepairOffer; project: ProjectId }[];
}

export interface LogView {
  token: number;
  project: ProjectId;
  service: string;
  handle: number | null;
  lines: LogLine[];
}

export type Selection =
  | { kind: "home" }
  | { kind: "project"; id: ProjectId }
  | { kind: "workspace"; id: string };

let sync: EngineSync | null = null;
let nextToken = 0;
let nextActivity = 0;

/** Past this, an operation is assumed to have outlasted the user's attention,
 * and finishing is news. Under it, they are still looking at the card. */
const WALKED_AWAY_MS = 60_000;

/** How long a command must stay silent before its silence counts as "booted"
 * rather than "still working". Long enough to outlast the gap between a
 * server's own startup lines, short enough not to feel like a hang. */
const READY_SETTLE_MS = 3_000;
/** Give up waiting for a dependency here. Covers a first `pnpm install` on a
 * cold store, and bounds a wait on a command nobody ever starts. */
const READY_TIMEOUT_MS = 5 * 60_000;
const READY_POLL_MS = 250;

/** Operations-map key for a user-defined project command (M7.5). */
export function commandKey(project: string, name: string): string {
  return `${project}:cmd:${name}`;
}

/** Share-tunnel operations get their own op slot: the tunnel runs
 * indefinitely, and parking it on the project key would block the lifecycle
 * buttons (and their Cancel semantics) the whole time. */
export function shareKey(project: string): string {
  return `${project}:share`;
}

/** Local-domain (HTTPS proxy) operations get their own slot for the same
 * reason as shares: they must not block the lifecycle buttons. */
export function domainKey(project: string): string {
  return `${project}:domain`;
}

/** Operations-map key for a project being scaffolded — it has no id yet. */
export function createKey(name: string): string {
  return `new:${name}`;
}

/** The project name behind a [`createKey`], or null for any other key. */
export function createdName(key: string): string | null {
  return key.startsWith("new:") ? key.slice("new:".length) : null;
}

export const useEngineStore = defineStore("engine", {
  state: () => ({
    phase: "idle" as SyncPhase,
    resyncs: 0,
    readOnly: false,
    docker: null as DockerStatus | null,
    integrations: {
      terminal: null,
      editor: null,
      browser: null,
      autoPortRemap: true,
    } as IntegrationSettings,
    watchedDirectories: [] as string[],
    discovered: [] as DiscoveredProject[],
    projects: [] as ProjectSummary[],
    workspaces: [] as WorkspaceSummary[],
    operations: {} as Record<ProjectId, OperationView>,
    activity: [] as ActivityLine[],
    /** Effect history, oldest first — the logs panel's History tab. */
    history: [] as HistoryEntry[],
    /** Background upkeep (resolution, probes) is constant; showing it by
     * default would bury the commands the user actually caused. */
    historyShowBackground: false,
    /** Entry id to scroll to and highlight — set when jumping from a failure. */
    historyFocus: null as number | null,
    /** Log captures, newest first — the logs panel's Captures tab. */
    captures: [] as LogCapture[],
    /** Highest capture id the user has actually looked at, so the tab can
     * badge what arrived while they were elsewhere. */
    capturesSeen: loadCapturesSeen(),
    /** Recent usage samples, oldest first. One ring: every readout in the UI
     * derives from it, so there is no per-project bookkeeping to keep in sync. */
    usage: [] as UsageSample[],
    /** Whether the usage subscription is open. Tracked so the visibility
     * handler is idempotent — it fires on every focus change. */
    usageConnected: false,
    logsTab: "output" as "output" | "history" | "captures" | "resources",
    logsOpen: loadLogsOpen(),
    logs: null as LogView | null,
    selection: { kind: "home" } as Selection,
    busy: 0,
    error: null as string | null,
    /** What happened to each project while the user was not looking at it —
     * the dot that outlives a missed toast. A notification is gone the moment
     * it is dismissed; the project that raised it keeps a marker until the
     * user actually visits. Keyed by project id, newest last, capped small:
     * the dot says "something happened here", the titles say what. */
    attention: {} as Record<string, string[]>,
  }),

  getters: {
    /** Projects not belonging to any workspace. */
    standaloneProjects(state): ProjectSummary[] {
      const memberIds = new Set(state.workspaces.flatMap((w) => w.members.map((m) => m.project)));
      return state.projects.filter((p) => !memberIds.has(p.id));
    },

    /** History as shown: newest first, background hidden unless asked for.
     * A focused entry is always shown, or jumping to a background command
     * from a failure would land on nothing. */
    visibleHistory(state): HistoryEntry[] {
      return state.history
        .filter(
          (entry) =>
            state.historyShowBackground ||
            entry.origin === "user" ||
            entry.id === state.historyFocus,
        )
        .slice()
        .reverse();
    },

    backgroundHistoryCount(state): number {
      return state.history.filter((entry) => entry.origin === "background").length;
    },

    /** Captures the user has not looked at yet. A container dying is worth
     * noticing even when the panel is closed, which is what the badge is for. */
    unseenCaptureCount(state): number {
      return state.captures.filter((capture) => capture.id > state.capturesSeen).length;
    },

    /** The newest sample, or null before the first one lands. */
    latestUsage(state): UsageSample | null {
      return state.usage.at(-1) ?? null;
    },

    /** Whether an operation is still in flight under a given key — a project
     * id, or one of the `*Key()` slots. One definition because two places act
     * on it: the controls that must not be touched mid-operation, and the
     * spinner that says why they are dim. Unrelated to [`busy`], which counts
     * short non-lifecycle dispatches across the whole app. */
    hasRunningOp(state) {
      return (key: string): boolean => {
        const op = state.operations[key];
        return op != null && op.terminal === null;
      };
    },

    /** Alert titles a project accumulated while unwatched, oldest first —
     * empty for a project with nothing to catch up on. */
    attentionFor(state) {
      return (project: ProjectId): string[] => state.attention[project] ?? [];
    },
  },

  actions: {
    /** Fold a full snapshot into state (resync entry point). */
    applySnapshot(snapshot: EngineSnapshot) {
      this.projects = [...snapshot.projects].sort((a, b) => a.name.localeCompare(b.name));
      this.docker = snapshot.docker;
      this.readOnly = snapshot.readOnly;
      this.integrations = snapshot.integrations;
      this.watchedDirectories = snapshot.watchedDirectories;
      this.discovered = snapshot.discovered;
      this.workspaces = snapshot.workspaces;
    },

    /** Fold one patch into state; observed transitions drive notifications
     * and command auto-start (never snapshot loads). */
    applyPatch(patch: EnginePatch) {
      const event = patch.event;
      switch (event.type) {
        case "discoveryChanged":
          this.discovered = event.discovered;
          break;
        case "watchedDirectoriesChanged":
          this.watchedDirectories = event.directories;
          break;
        case "dockerStatusChanged": {
          const wasAvailable = this.docker?.available;
          this.docker = event.status;
          if (wasAvailable === true && !event.status.available) {
            void notify("docker", "Docker connection lost", event.status.error ?? "");
          } else if (wasAvailable === false && event.status.available) {
            void notify("docker", "Docker reconnected", "Projects resynchronized.");
          }
          break;
        }
        case "integrationsChanged":
          this.integrations = event.integrations;
          break;
        case "workspacesChanged":
          this.workspaces = event.workspaces;
          break;
        default: {
          const wasRunning = new Map(this.projects.map((p) => [p.id, p.status === "running"]));
          const wasUnhealthy = new Map(
            this.projects.map((p) => [p.id, p.status === "degraded" || p.status === "failed"]),
          );
          this.projects = applyPatchEvent(this.projects, event);
          for (const p of this.projects) {
            const unhealthy = p.status === "degraded" || p.status === "failed";
            if (unhealthy && wasUnhealthy.get(p.id) === false) {
              void notify("health", `${p.name} is ${p.status}`, "A service went unhealthy.");
              this.noteAttention(p.id, `went ${p.status}`);
            } else if (p.status === "running" && wasUnhealthy.get(p.id) === true) {
              void notify("health", `${p.name} recovered`, "All services healthy again.");
              this.noteAttention(p.id, "recovered");
            }
            if (p.status !== "running" || wasRunning.get(p.id) !== false) continue;
            for (const cmd of (p.commands ?? []).filter((c) => c.autoStart)) {
              // Each chain runs on its own: a command that waits parks in its
              // own promise rather than holding up the commands beside it.
              void this.autoStartCommand(p.id, cmd);
            }
          }
        }
      }
    },

    async connect() {
      if (sync) return;
      sync = new EngineSync(tauriPatchTransport, {
        reset: (snapshot) => this.applySnapshot(snapshot),
        apply: (patch) => this.applyPatch(patch),
        phase: (phase) => {
          this.phase = phase;
          this.resyncs = sync?.resyncs ?? 0;
        },
      });
      const s = sync;
      await onPatchStreamItem((streamId, item) => s.handleItem(streamId, item));
      try {
        await s.connect();
      } catch (e) {
        this.error = e instanceof Error ? e.message : String(e);
      }
      await this.connectHistory();
      await this.connectCaptures();
      await this.connectUsage();
    },

    /** History rides its own channel, not the patch stream — its volume would
     * evict real patches from the engine's replay window. Subscribe before
     * fetching the backlog so nothing falls between the two. */
    async connectHistory() {
      try {
        await startHistoryStream((entry) => this.upsertHistory(entry));
        for (const entry of await historyRecent()) this.upsertHistory(entry);
      } catch (e) {
        // History is a transparency aid; losing it must not break the app.
        console.warn("history unavailable", e);
      }
    },

    /** Entries arrive twice — on creation and on completion — so replace by
     * id, and keep the ring ordered by id (which is creation order). */
    upsertHistory(entry: HistoryEntry) {
      const at = this.history.findIndex((existing) => existing.id === entry.id);
      if (at >= 0) {
        this.history[at] = entry;
        return;
      }
      const before = this.history.findIndex((existing) => existing.id > entry.id);
      if (before >= 0) this.history.splice(before, 0, entry);
      else this.history.push(entry);
      if (this.history.length > HISTORY_CAP) {
        this.history.splice(0, this.history.length - HISTORY_CAP);
      }
    },

    /** Captures ride their own channel for the same reason history does.
     * Subscribe before fetching the backlog so a capture taken during startup
     * is not lost between the two calls. */
    async connectCaptures() {
      try {
        await startCaptureStream((capture) => this.addCapture(capture));
        for (const capture of await logCaptures(CAPTURES_CAP)) this.addCapture(capture);
      } catch (e) {
        // Same rule as history: a diagnostic aid must not break the app.
        console.warn("log captures unavailable", e);
      }
    },

    /** Newest first. Captures are append-only — the engine never revises one
     * after writing it — but the backlog and the live stream can overlap, so
     * this still guards against a duplicate id. */
    addCapture(capture: LogCapture) {
      if (this.captures.some((existing) => existing.id === capture.id)) return;
      const before = this.captures.findIndex((existing) => existing.id < capture.id);
      if (before >= 0) this.captures.splice(before, 0, capture);
      else this.captures.push(capture);
      if (this.captures.length > CAPTURES_CAP) {
        this.captures.splice(CAPTURES_CAP, this.captures.length - CAPTURES_CAP);
      }
    },

    /** Mark everything currently listed as seen, clearing the tab badge. */
    markCapturesSeen() {
      const newest = this.captures[0]?.id ?? 0;
      if (newest <= this.capturesSeen) return;
      this.capturesSeen = newest;
      saveCapturesSeen(newest);
    },

    /** Subscribing is what makes the engine sample — it does no work while
     * nobody is listening — so this is called when the window becomes visible
     * and `disconnectUsage` when it is hidden. Measuring the machine while
     * nobody is looking at the answer is pure cost. */
    async connectUsage() {
      if (this.usageConnected) return;
      this.usageConnected = true;
      try {
        await startUsageStream((sample) => {
          pushBounded(this.usage, sample, USAGE_CAP);
        });
      } catch (e) {
        this.usageConnected = false;
        // Same rule as history and captures: a readout must not break the app.
        console.warn("usage unavailable", e);
      }
    },

    async disconnectUsage() {
      if (!this.usageConnected) return;
      this.usageConnected = false;
      try {
        await stopUsageStream();
      } catch {
        // Best-effort: the engine stops sampling when the receiver drops
        // regardless of whether this call was acknowledged.
      }
    },

    /** Open the Resources tab — the path out of "why is my fan on". */
    showResources() {
      this.setLogsOpen(true);
      this.logsTab = "resources";
    },

    /** Open the Captures tab — the path out of "my container vanished". */
    showCaptures() {
      this.setLogsOpen(true);
      this.logsTab = "captures";
      this.markCapturesSeen();
    },

    /** Capture a service's recent output now, without stopping anything, and
     * show the result. */
    async captureServiceLogs(project: ProjectId, service: string) {
      await this.run({ type: "captureServiceLogs", id: project, service });
      this.showCaptures();
    },

    /** Drop every stored capture. They are on disk, so this is a real delete
     * rather than clearing a view. */
    async clearCaptures() {
      this.captures = [];
      await this.run({ type: "clearLogCaptures" });
    },

    /** Open the History tab on the command behind an operation, preferring the
     * one that actually failed — this is the path out of a failure message. */
    showOperationCommand(operation: OperationId) {
      const candidates = this.history.filter((entry) => entry.operation === operation);
      const failed = candidates.filter(
        (entry) =>
          entry.outcome.type === "failed" ||
          (entry.outcome.type === "exited" && entry.outcome.status !== 0),
      );
      // An operation can run several commands; the last failing one is what
      // the user is looking for, and its own last entry otherwise.
      const target = failed[failed.length - 1] ?? candidates[candidates.length - 1];
      if (!target) return;
      // The one place Mast opens the panel itself, because the user just
      // asked to be shown something that lives in it.
      this.setLogsOpen(true);
      this.logsTab = "history";
      this.historyFocus = target.id;
    },

    /** Dispatch a non-lifecycle action and resolve on its terminal event. */
    async run(action: Action): Promise<void> {
      this.error = null;
      this.busy += 1;
      try {
        await new Promise<void>((resolve, reject) => {
          dispatchAction(action, (event) => {
            if (event.kind.type === "completed" || event.kind.type === "cancelled") resolve();
            else if (event.kind.type === "failed") reject(new Error(event.kind.error));
          }).catch(reject);
        });
      } catch (e) {
        this.error = e instanceof Error ? e.message : String(e);
      } finally {
        this.busy -= 1;
      }
    },

    /** Show or hide the logs panel, remembering the choice across restarts. */
    setLogsOpen(open: boolean) {
      this.logsOpen = open;
      saveLogsOpen(open);
    },

    /** Append a line to the global activity feed (the bottom logs panel). */
    pushActivity(line: string, stderr: boolean) {
      pushBounded(this.activity, { n: ++nextActivity, line, stderr }, ACTIVITY_CAP);
    },

    /** Dispatch a lifecycle verb, tracking streamed output per project. */
    async runLifecycle(project: ProjectId, label: string, action: Action): Promise<void> {
      this.error = null;
      if (action.type === "startWorkspace") recordWorkspaceStart(action.id);
      const token = ++nextToken;
      // Registered BEFORE dispatch: channel events can beat the invoke reply.
      // Always re-read through the store (reactive proxy) and match by token.
      this.operations[project] = {
        token,
        id: -1,
        label,
        startedAt: Date.now(),
        lines: [],
        received: 0,
        terminal: null,
        error: null,
        fixes: [],
      };
      // The panel is the user's to open. It used to force itself open on every
      // operation, which stole vertical space mid-task and undid a deliberate
      // close; output still accumulates while it is shut.
      this.pushActivity(`▶ ${label}`, false);
      const current = (): OperationView | null => {
        const op = this.operations[project];
        return op && op.token === token ? op : null;
      };
      try {
        const id = await dispatchAction(action, (event) => {
          const op = current();
          if (!op) return; // superseded by a newer operation
          switch (event.kind.type) {
            case "output":
              pushBounded(
                op.lines,
                { line: event.kind.line, stderr: event.kind.stderr },
                OP_LINE_CAP,
              );
              op.received += 1;
              this.pushActivity(event.kind.line, event.kind.stderr);
              break;
            case "completed":
            case "cancelled":
              op.terminal = event.kind.type;
              this.pushActivity(
                event.kind.type === "completed" ? `✓ ${label} completed` : `⏹ ${label} cancelled`,
                false,
              );
              // Failures already notify. Success is worth it too once an
              // operation has run long enough that you went and did something
              // else — a cold rebuild is half an hour of not watching. Short
              // ones stay silent; a notification per `stop` is just noise.
              if (event.kind.type === "completed" && Date.now() - op.startedAt >= WALKED_AWAY_MS) {
                void notify(
                  "operations",
                  `${label} finished`,
                  `Took ${formatElapsed(Date.now() - op.startedAt)}.`,
                );
                this.noteAttention(this.projectOfOpKey(project), `${label} finished`);
              }
              break;
            case "fixAvailable": {
              const repair = event.kind.repair;
              // Re-emissions of the same repair (retries) collapse to one button.
              if (!op.fixes.some((f) => f.repair.id === repair.id && f.repair.arg === repair.arg)) {
                op.fixes.push({ repair, project: event.kind.project });
              }
              break;
            }
            case "failed":
              op.terminal = "failed";
              op.error = event.kind.error;
              this.pushActivity(`✗ ${label} failed: ${event.kind.error}`, true);
              void notify("operations", `${label} failed`, event.kind.error);
              this.noteAttention(this.projectOfOpKey(project), `${label} failed`);
              break;
            default:
              break;
          }
        });
        const op = current();
        if (op) op.id = id;
      } catch (e) {
        if (current()) delete this.operations[project];
        this.error = e instanceof Error ? e.message : String(e);
      }
    },

    /** Auto-start one command, after whatever it waits for has come up.
     * Manual Run deliberately ignores `after` — asking for a command by hand
     * means now, not eventually. */
    async autoStartCommand(project: ProjectId, cmd: ProjectCommand): Promise<void> {
      const key = commandKey(project, cmd.name);
      if (this.operations[key]?.terminal === null) return; // already running
      const after = cmd.after?.trim();
      if (after) {
        const up = await this.waitForCommandReady(project, after);
        if (!up) {
          this.pushActivity(`✗ ${cmd.name} not started — ${after} never came up`, true);
          return;
        }
      }
      await this.runLifecycle(key, cmd.name, {
        type: "runProjectCommand",
        id: project,
        name: cmd.name,
      });
    },

    /** Resolve once `name` has finished starting, or false if it never does.
     *
     * A dev server never exits, so "finished" is the wrong question — what a
     * dependent needs is "up". Three ways to know, in order of how much they
     * can be trusted: the command exited cleanly (a one-shot really is done),
     * its declared `readyWhen` text appeared, or it printed something and
     * then went quiet, which is what a server does once it has finished
     * booting. The last is a guess, which is why the first two exist. */
    async waitForCommandReady(project: ProjectId, name: string): Promise<boolean> {
      const key = commandKey(project, name);
      const readyWhen = this.projects
        .find((p) => p.id === project)
        ?.commands?.find((c) => c.name === name)
        ?.readyWhen?.trim();
      const until = Date.now() + READY_TIMEOUT_MS;
      let seen = -1;
      let quietSince = Date.now();
      while (Date.now() < until) {
        const op = this.operations[key];
        // Absent is "not yet", not "never": the command it waits for is very
        // likely being dispatched in the same tick. The timeout is what stops
        // this waiting forever on a command nobody is going to start.
        if (op?.terminal === "completed") return true;
        if (op?.terminal) return false;
        if (op) {
          if (readyWhen) {
            // Matched against the visible text — the banner worth waiting for
            // is exactly the sort of line a tool prints in colour.
            if (op.lines.some((l) => stripAnsi(l.line).includes(readyWhen))) return true;
          } else if (op.received !== seen) {
            seen = op.received;
            quietSince = Date.now();
          } else if (seen > 0 && Date.now() - quietSince >= READY_SETTLE_MS) {
            return true;
          }
        }
        await new Promise((resolve) => setTimeout(resolve, READY_POLL_MS));
      }
      return false;
    },

    /** The project behind an operations-map key — the key is either a
     * project id or a `<project>:cmd:`/`:share`/`:domain` slot; workspace and
     * `new:` keys belong to no project and come back null. */
    projectOfOpKey(key: string): ProjectId | null {
      return this.projects.find((p) => p.id === key || key.startsWith(`${p.id}:`))?.id ?? null;
    },

    /** Remember that something notification-worthy happened to a project the
     * user was not looking at. Looking means: window visible AND that project
     * selected — a toast fired at a visible but different pane is exactly the
     * kind that gets missed. */
    noteAttention(project: ProjectId | null, title: string) {
      if (project === null) return;
      // `document` is absent under the node test runner, where nobody is
      // looking by definition.
      const visible = typeof document !== "undefined" && !document.hidden;
      const looking = visible && this.selection.kind === "project" && this.selection.id === project;
      if (looking) return;
      const titles = this.attention[project] ?? [];
      titles.push(title);
      // The dot says "something happened"; three titles are plenty of what.
      this.attention[project] = titles.slice(-3);
    },

    /** Visiting the project is what clears its marker — not dismissing a
     * toast, which may never have been seen. */
    clearAttention(project: ProjectId) {
      delete this.attention[project];
    },

    /** Drop a finished operation so its card stops being shown. */
    dismissOperation(key: string) {
      delete this.operations[key];
    },

    async cancelLifecycle(project: ProjectId): Promise<void> {
      const op = this.operations[project];
      if (!op || op.terminal || op.id < 0) return;
      try {
        await cancelOperation(op.id);
      } catch (e) {
        this.error = e instanceof Error ? e.message : String(e);
      }
    },

    async openLogs(project: ProjectId, service: string): Promise<void> {
      await this.closeLogs();
      const token = ++nextToken;
      this.logs = { token, project, service, handle: null, lines: [] };
      const current = (): LogView | null =>
        this.logs && this.logs.token === token ? this.logs : null;
      try {
        const handle = await streamServiceLogs(project, service, 100, (line) => {
          const view = current();
          if (view) pushBounded(view.lines, line, LOG_LINE_CAP);
        });
        const view = current();
        if (view) view.handle = handle;
        else await stopLogStream(handle); // panel closed while the stream opened
      } catch (e) {
        this.error = e instanceof Error ? e.message : String(e);
        if (current()) this.logs = null;
      }
    },

    async fetchEnvReport(project: ProjectId) {
      try {
        return await envReport(project);
      } catch (e) {
        this.error = e instanceof Error ? e.message : String(e);
        return null;
      }
    },

    async closeLogs(): Promise<void> {
      const current = this.logs;
      this.logs = null;
      if (current?.handle != null) {
        try {
          await stopLogStream(current.handle);
        } catch {
          // Stream teardown is best-effort.
        }
      }
    },
  },
});

// Without this, editing this file during `vp dev` hot-replaces the components
// that use the store but leaves the store instance on its old definition, so
// newly added actions are missing until a full reload.
if (import.meta.hot) {
  import.meta.hot.accept(acceptHMRUpdate(useEngineStore, import.meta.hot));
}
