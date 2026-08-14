// Presentation helpers for log captures (the logs panel's Captures tab).
// A capture is read after the fact, so unlike the live log stream every line
// carries Docker's own timestamp — which is the only reason the tab can show
// when something happened rather than when Mast noticed.

import type { CaptureReason, LogCapture } from "../bindings";

/** Short right-aligned status text, next to the capture's time. */
export function reasonLabel(reason: CaptureReason): string {
  switch (reason.type) {
    case "teardown":
      return `before ${reason.verb}`;
    case "exited":
      return reason.status === null ? "exited" : `exited ${reason.status}`;
    case "unhealthy":
      return "unhealthy";
    case "readyTimeout":
      return "not ready";
    case "manual":
      return "captured";
  }
}

/** Whether this capture is a post-mortem of something that went wrong, as
 * opposed to a routine or user-requested read. Drives the status dot and the
 * colour of the reason — but never of the timestamp beside it, which stays
 * neutral so the two read as separate fields. */
export function isPostMortem(reason: CaptureReason): boolean {
  return reason.type === "exited" || reason.type === "unhealthy";
}

/** A capture Mast took on its own initiative, rather than one the user asked
 * for or one that merely preceded a deliberate teardown. */
export function isUnprompted(reason: CaptureReason): boolean {
  return isPostMortem(reason) || reason.type === "readyTimeout";
}

/** The per-line time, at second resolution. Docker's stamp is RFC3339 with
 * nanoseconds, which `Date` parses natively — no formatting library needed.
 * Falls back to the capture's own time for a line that arrived without one. */
export function lineTime(at: string | null, fallbackUnixMs: number): string {
  const ms = at === null ? fallbackUnixMs : Date.parse(at);
  return new Date(Number.isNaN(ms) ? fallbackUnixMs : ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** One-line summary under the title: how much was captured, and over what. */
export function captureSummary(capture: LogCapture): string {
  const count = `${capture.lines.length} line${capture.lines.length === 1 ? "" : "s"}`;
  const window = `last ${capture.windowSecs}s`;
  return capture.truncated ? `${count} · ${window} · oldest dropped` : `${count} · ${window}`;
}

/** What the copy button puts on the clipboard: the lines as they were, with
 * a header naming what died so a pasted capture is self-explanatory. Already
 * redacted by the engine — captures are persisted, so secrets never reached
 * the record in the first place. */
export function copyableCapture(capture: LogCapture): string {
  const header = `# ${capture.projectName} · ${capture.service} — ${reasonLabel(capture.reason)}`;
  const lines = capture.lines.map((l) => (l.at === null ? l.message : `${l.at} ${l.message}`));
  return [header, ...lines].join("\n");
}
