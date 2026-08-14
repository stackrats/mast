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
  ProjectId,
  ProjectSummary,
  WorkspaceSummary,
} from "../bindings";
import { EngineSync, type SyncPhase } from "../lib/engineSync";
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
  lines: { line: string; stderr: boolean }[];
  terminal: "completed" | "failed" | "cancelled" | null;
  error: string | null;
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

/** Operations-map key for a user-defined project command (M7.5). */
export function commandKey(project: string, name: string): string {
  return `${project}:cmd:${name}`;
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
    integrations: { terminal: null, editor: null, autoPortRemap: true } as IntegrationSettings,
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
    logsTab: "output" as "output" | "history" | "captures",
    logsOpen: loadLogsOpen(),
    logs: null as LogView | null,
    selection: { kind: "home" } as Selection,
    busy: 0,
    error: null as string | null,
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
            } else if (p.status === "running" && wasUnhealthy.get(p.id) === true) {
              void notify("health", `${p.name} recovered`, "All services healthy again.");
            }
            if (p.status !== "running" || wasRunning.get(p.id) !== false) continue;
            for (const cmd of (p.commands ?? []).filter((c) => c.autoStart)) {
              const key = commandKey(p.id, cmd.name);
              const existing = this.operations[key];
              if (existing && existing.terminal === null) continue;
              void this.runLifecycle(key, cmd.name, {
                type: "runProjectCommand",
                id: p.id,
                name: cmd.name,
              });
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
      this.operations[project] = { token, id: -1, label, lines: [], terminal: null, error: null };
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
              this.pushActivity(event.kind.line, event.kind.stderr);
              break;
            case "completed":
            case "cancelled":
              op.terminal = event.kind.type;
              this.pushActivity(
                event.kind.type === "completed" ? `✓ ${label} completed` : `⏹ ${label} cancelled`,
                false,
              );
              break;
            case "failed":
              op.terminal = "failed";
              op.error = event.kind.error;
              this.pushActivity(`✗ ${label} failed: ${event.kind.error}`, true);
              void notify("operations", `${label} failed`, event.kind.error);
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
