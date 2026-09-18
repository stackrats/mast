// How a project or workspace status is rendered. Both maps were duplicated
// across components; a status has to look the same everywhere or the colour
// stops carrying meaning.

import type { Action, ProjectStatus } from "../bindings";

export type StatusBadgeVariant = "secondary" | "warning" | "success" | "destructive";

/** Badge colour for a status, where there is room for a word. */
export const statusBadgeVariant: Record<ProjectStatus, StatusBadgeVariant> = {
  stopped: "secondary",
  starting: "warning",
  running: "success",
  degraded: "warning",
  failed: "destructive",
};

/** Dot colour for a status, where there is only room for a dot. */
export const statusDot: Record<ProjectStatus, string> = {
  stopped: "bg-slate-300 dark:bg-slate-600",
  starting: "bg-amber-400",
  running: "bg-emerald-500",
  degraded: "bg-orange-400",
  failed: "bg-red-500",
};

export type ProcessStatus = "stopped" | "starting" | "stopping" | "running" | "failed";

/** A daemon's launch operation lasts as long as the daemon. Observation,
 * rather than operation completion, tells us when to stop its spinner. */
export function processStatus(
  running: boolean,
  operation?: {
    actionType?: Action["type"];
    terminal: "completed" | "failed" | "cancelled" | null;
    cancelling?: boolean;
  },
): ProcessStatus {
  if (operation?.terminal === null) {
    if (operation.cancelling || operation.actionType === "stopProcess") return "stopping";
    if (!running) return "starting";
  }
  // A failed launch/stop must not hide a daemon observed running externally.
  if (running) return "running";
  return operation?.terminal === "failed" ? "failed" : "stopped";
}
