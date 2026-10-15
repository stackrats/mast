// What coding agents are doing around each project, in the words the Agents
// view uses (ADR-0012). The engine finds the worktrees, the agent sessions
// and the processes on this machine; this decides how they read, what counts
// as left behind, and what "Clean up" offers to stop.

import type {
  Action,
  AgentSession,
  ContainerRun,
  HostRun,
  HostRunOwner,
  ProjectId,
  ProjectSummary,
  Worktree,
} from "../bindings";
import { formatElapsed } from "./elapsed";
import { formatBytes } from "./usage";

/** Whether host processes are watched on this platform at all — the
 * engine reads them from `/proc`, so Linux only for now. */
export function hostProcessesWatched(): boolean {
  const platform = globalThis.navigator?.platform ?? globalThis.navigator?.userAgent ?? "";
  return /linux/i.test(platform) && !/android/i.test(platform);
}

/** Container runs Mast did not start — the ones worth showing beside what
 * agents run on the host. */
export function foreignContainerRuns(project: ProjectSummary): ContainerRun[] {
  return (project.containerRuns ?? []).filter((run) => run.owner.type !== "mast");
}

/** Whether the Agents view has anything to say about a project. */
export function hasActivity(project: ProjectSummary): boolean {
  return (
    (project.agentSessions ?? []).length > 0 ||
    (project.hostRuns ?? []).length > 0 ||
    (project.worktrees ?? []).length > 0 ||
    foreignContainerRuns(project).length > 0
  );
}

/** Whether agents have anything going in a project beyond its app
 * container — what its card adds an Agents section for, since the card
 * already lists the container's process trees itself. */
export function hasAgentWork(project: ProjectSummary): boolean {
  return (
    (project.agentSessions ?? []).length > 0 ||
    (project.hostRuns ?? []).length > 0 ||
    (project.worktrees ?? []).length > 0
  );
}

/** The agent sessions working in a worktree (by path) or the checkout (null). */
export function sessionsIn(project: ProjectSummary, worktree: string | null): AgentSession[] {
  return (project.agentSessions ?? []).filter((s) => (s.worktree ?? null) === worktree);
}

/** The host runs a session started. */
export function runsOf(project: ProjectSummary, session: AgentSession): HostRun[] {
  return (project.hostRuns ?? []).filter(
    (run) => run.owner.type === "agent" && run.owner.pid === session.pid,
  );
}

/** Host runs not under one of the project's listed sessions: orphaned ones,
 * and those an agent working elsewhere started here. */
export function unattachedRuns(project: ProjectSummary): HostRun[] {
  const sessions = new Set((project.agentSessions ?? []).map((s) => s.pid));
  return (project.hostRuns ?? []).filter(
    (run) => run.owner.type !== "agent" || !sessions.has(run.owner.pid),
  );
}

/** Host runs working in one worktree. */
export function runsInWorktree(project: ProjectSummary, path: string): HostRun[] {
  return (project.hostRuns ?? []).filter((run) => run.worktree === path);
}

/** "the checkout" or the worktree's name. */
export function placeLabel(project: ProjectSummary, worktree: string | null | undefined): string {
  if (!worktree) return "the checkout";
  const match = (project.worktrees ?? []).find((w) => w.path === worktree);
  return match ? match.name : worktree.split(/[\\/]/).pop() || worktree;
}

/** Who a host run belongs to, short enough for a row. */
export function hostOwnerLabel(owner: HostRunOwner): string {
  return owner.type === "agent" ? owner.agent : "orphaned";
}

/** Who a host run belongs to and what that means, for its details. */
export function hostOwnerSentence(run: HostRun): string {
  if (run.owner.type === "agent") {
    return (
      `${run.owner.agent} (pid ${run.owner.pid}) started this and is still running. ` +
      `It keeps running after that session ends unless the session stops it.`
    );
  }
  return (
    "Whatever started this has exited — an agent session that finished, or a terminal " +
    "that closed — and the system adopted it. It runs until someone stops it."
  );
}

/** Left behind, rather than in use: orphaned on the host, or in the app
 * container with no live starter. */
export function isLeftover(run: HostRun | ContainerRun): boolean {
  return run.owner.type === "orphaned" || run.owner.type === "earlierMast";
}

/** How long it has been going, e.g. `6h 12m`. */
export function upFor(startedAtUnix: number, nowMs: number): string {
  return formatElapsed(nowMs - startedAtUnix * 1000);
}

/** How long ago something was made, coarse enough for a worktree: `40m`,
 * `5h`, `7d`. */
export function madeAgo(createdAtUnix: number, nowMs: number): string {
  const secs = Math.max(0, Math.floor(nowMs / 1000 - createdAtUnix));
  if (secs < 3600) return `${Math.max(1, Math.floor(secs / 60))}m`;
  if (secs < 48 * 3600) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86_400)}d`;
}

/** One line of facts for a host run. */
export function hostRunFacts(run: HostRun, nowMs: number): string {
  const processes = run.processCount === 1 ? "1 process" : `${run.processCount} processes`;
  return `${processes} · ${formatBytes(run.memoryBytes)} · up ${upFor(run.startedAtUnix, nowMs)}`;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** What a worktree's state comes to, one short fact each, worst first. */
export function worktreeFacts(worktree: Worktree): { text: string; tone: "warn" | "muted" }[] {
  const facts: { text: string; tone: "warn" | "muted" }[] = [];
  if (worktree.missing) facts.push({ text: "directory gone", tone: "warn" });
  if (worktree.locked) facts.push({ text: "locked", tone: "muted" });
  if (worktree.changedFiles)
    facts.push({
      text: plural(worktree.changedFiles, "uncommitted change", "uncommitted changes"),
      tone: "warn",
    });
  if (worktree.unmergedCommits)
    facts.push({
      text: `${plural(worktree.unmergedCommits, "commit", "commits")} not in the checkout`,
      tone: "muted",
    });
  if (!worktree.missing && worktree.changedFiles === 0 && !worktree.unmergedCommits)
    facts.push({ text: "nothing unmerged", tone: "muted" });
  return facts;
}

/** The worktree's containers, summarised: "2/3 running · ports 80, 5173". */
export function stackSummary(worktree: Worktree): string | null {
  const containers = worktree.containers ?? [];
  if (containers.length === 0) return null;
  const running = containers.filter((c) => c.running).length;
  const ports = [...new Set(containers.flatMap((c) => c.ports))].sort((a, b) => a - b);
  const names = [...new Set(containers.map((c) => c.composeProject))].join(", ");
  return (
    `${names}: ${running}/${containers.length} running` +
    (ports.length ? ` · ports ${ports.join(", ")}` : "")
  );
}

/** What removing a worktree would cost, in sentences, and whether the
 * engine will need `force` to do it. */
export function removalCost(worktree: Worktree): { force: boolean; lines: string[] } {
  const lines: string[] = [];
  let force = false;
  if (worktree.changedFiles) {
    force = true;
    lines.push(
      `${plural(worktree.changedFiles, "uncommitted change", "uncommitted changes")} ` +
        `will be lost — they exist only in this worktree.`,
    );
  }
  if (!worktree.branch && worktree.unmergedCommits) {
    force = true;
    lines.push(
      `HEAD is detached with ${plural(worktree.unmergedCommits, "commit", "commits")} ` +
        `no branch keeps; they become unreachable.`,
    );
  } else if (worktree.branch) {
    lines.push(
      `The branch ${worktree.branch} is kept` +
        (worktree.unmergedCommits
          ? `, with its ${plural(worktree.unmergedCommits, "commit", "commits")}.`
          : "."),
    );
  }
  return { force, lines };
}

/** The tool whose location a worktree is in, as a fact about the location. */
export function toolHint(worktree: Worktree): string | null {
  return worktree.tool ? `in ${worktree.tool}'s worktree location` : null;
}

export interface Overview {
  sessions: number;
  worktrees: number;
  missingWorktrees: number;
  worktreeStacks: number;
  leftovers: number;
  leftoverBytes: number;
}

/** Totals across every project, for the tiles and the sidebar badge. */
export function overview(projects: ProjectSummary[]): Overview {
  const totals: Overview = {
    sessions: 0,
    worktrees: 0,
    missingWorktrees: 0,
    worktreeStacks: 0,
    leftovers: 0,
    leftoverBytes: 0,
  };
  for (const p of projects) {
    totals.sessions += (p.agentSessions ?? []).length;
    for (const w of p.worktrees ?? []) {
      totals.worktrees += 1;
      if (w.missing) totals.missingWorktrees += 1;
      if ((w.containers ?? []).some((c) => c.running)) totals.worktreeStacks += 1;
    }
    for (const run of [...(p.hostRuns ?? []), ...(p.containerRuns ?? [])]) {
      if (!isLeftover(run)) continue;
      totals.leftovers += 1;
      totals.leftoverBytes += run.memoryBytes;
    }
  }
  return totals;
}

export interface CleanupItem {
  key: string;
  project: ProjectId;
  projectName: string;
  label: string;
  detail: string;
  /** Memory it frees, when that is known. */
  bytes: number;
  /** Ticked when the list opens: what nothing live is using. */
  suggested: boolean;
  action: Action;
}

/**
 * Everything "Clean up" can offer, each with whether it starts ticked.
 *
 * Ticked: what nothing live is using — orphaned processes, container trees
 * whose starter has gone, the containers of a worktree no agent is working
 * in, worktrees whose directory is gone. Offered but not ticked: what a live
 * session started, and container trees started outside Mast, which may be
 * someone's terminal. Removing a worktree itself is never on the list — that
 * is a decision about its work, not a cleanup.
 */
export function cleanupPlan(projects: ProjectSummary[], nowMs: number): CleanupItem[] {
  const all: CleanupItem[] = [];
  for (const p of projects) {
    const items: CleanupItem[] = [];
    for (const run of p.hostRuns ?? []) {
      const where = placeLabel(p, run.worktree);
      items.push({
        key: `host:${p.id}:${run.pid}:${run.startTicks}`,
        project: p.id,
        projectName: p.name,
        label: run.title,
        detail:
          `${run.owner.type === "agent" ? `${run.owner.agent} is still running` : "orphaned"} · ` +
          `in ${where} · ${hostRunFacts(run, nowMs)}`,
        bytes: run.memoryBytes,
        suggested: run.owner.type === "orphaned",
        action: { type: "stopHostRun", id: p.id, pid: run.pid, startTicks: run.startTicks },
      });
    }
    for (const run of foreignContainerRuns(p)) {
      const owner =
        run.owner.type === "earlierMast"
          ? "left by an earlier Mast session"
          : run.owner.type === "orphaned"
            ? "orphaned"
            : "started outside Mast";
      items.push({
        key: `container:${p.id}:${run.pid}:${run.startTicks}`,
        project: p.id,
        projectName: p.name,
        label: run.title,
        detail: `in the app container · ${owner} · ${formatBytes(run.memoryBytes)} · up ${upFor(run.startedAtUnix, nowMs)}`,
        bytes: run.memoryBytes,
        suggested: isLeftover(run),
        action: { type: "stopContainerRun", id: p.id, pid: run.pid, startTicks: run.startTicks },
      });
    }
    for (const w of p.worktrees ?? []) {
      const summary = stackSummary(w);
      if (!summary) continue;
      items.push({
        key: `stack:${p.id}:${w.path}`,
        project: p.id,
        projectName: p.name,
        label: `Containers started from ${w.name}`,
        detail: `${summary} · removed, named volumes kept`,
        bytes: 0,
        suggested: sessionsIn(p, w.path).length === 0,
        action: { type: "removeWorktreeContainers", id: p.id, path: w.path },
      });
    }
    const missing = (p.worktrees ?? []).filter((w) => w.missing && !w.locked);
    if (missing.length) {
      items.push({
        key: `prune:${p.id}`,
        project: p.id,
        projectName: p.name,
        label: `Forget ${plural(missing.length, "worktree", "worktrees")} whose directory is gone`,
        detail: `git worktree prune · ${missing.map((w) => w.name).join(", ")}`,
        bytes: 0,
        suggested: true,
        action: { type: "pruneWorktrees", id: p.id },
      });
    }
    // Ticked first, so what the list proposes reads as one block.
    all.push(...items.filter((item) => item.suggested), ...items.filter((item) => !item.suggested));
  }
  return all;
}
