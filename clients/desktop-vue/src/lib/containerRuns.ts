// What is running inside a project's app container, in the words the card
// uses. The engine groups the container's processes into trees and says who
// started each (see the engine's `inventory` module); this decides how that
// reads, and which of them deserve a second look.

import type { ContainerRun, ContainerRunOwner } from "../bindings";
import { formatElapsed } from "./elapsed";
import { formatBytes } from "./usage";

/** Who started it, short enough for a chip. */
export function ownerLabel(owner: ContainerRunOwner): string {
  switch (owner.type) {
    case "mast":
      return "Mast";
    case "earlierMast":
      return "earlier Mast session";
    case "outside":
      return "outside Mast";
    case "orphaned":
      return "orphaned";
  }
}

/** Who started it and what that means, for the chip's menu. */
export function ownerSentence(owner: ContainerRunOwner): string {
  switch (owner.type) {
    case "mast":
      return `Mast is running this as ${owner.command}.`;
    case "earlierMast":
      return (
        `An earlier Mast session started this as ${owner.command} and no longer tracks it — ` +
        `quitting Mast never stops what it started inside a container. Running ` +
        `${owner.command} again replaces it.`
      );
    case "outside":
      return (
        "Started in the container by something other than Mast: sail artisan in a " +
        "terminal, an agent, or another tool."
      );
    case "orphaned":
      return (
        "Whatever started this has exited and the container adopted it. It runs until " +
        "it is stopped or the container restarts."
      );
  }
}

/** Anything Mast is not running is worth a look: it holds memory, and a
 * second copy of a daemon does its work twice. */
export function needsAttention(run: ContainerRun): boolean {
  return run.owner.type !== "mast";
}

/** How long it has been running, e.g. `6h 12m`. */
export function runAge(run: ContainerRun, nowMs: number): string {
  return formatElapsed(nowMs - run.startedAtUnix * 1000);
}

/** One line of facts for a chip's tooltip and menu. */
export function runFacts(run: ContainerRun, nowMs: number): string {
  const processes = run.processCount === 1 ? "1 process" : `${run.processCount} processes`;
  return `${processes} · ${formatBytes(run.memoryBytes)} · up ${runAge(run, nowMs)}`;
}

/** The runs to list: everything, Mast's own first, then the oldest
 * leftovers — the ones most likely forgotten. */
export function orderedRuns(runs: ContainerRun[]): ContainerRun[] {
  return [...runs].sort((a, b) => {
    const rank = (run: ContainerRun) => (needsAttention(run) ? 1 : 0);
    return rank(a) - rank(b) || a.startedAtUnix - b.startedAtUnix;
  });
}

/** The run an advisory's Stop action names, to show how old it is — two
 * copies of one stack carry the same title, and their age is what tells
 * them apart. */
export function runAt(
  runs: ContainerRun[],
  pid: number,
  startTicks: number,
): ContainerRun | undefined {
  return runs.find((run) => run.pid === pid && run.startTicks === startTicks);
}
