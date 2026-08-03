// Presentation helpers for the effect history (the logs panel's History tab).
// The engine records argv arrays because Mast never runs a shell string; the
// copy affordance is what turns one back into something a developer can paste.

import type { HistoryEntry, HistoryOutcome } from "../bindings";

/** Characters that are safe unquoted in every POSIX shell. */
const BARE = /^[A-Za-z0-9_@%+=:,./-]+$/;

/** Quote one argument so a shell sees exactly the string Mast passed. */
export function shellQuoteArg(arg: string): string {
  if (arg.length > 0 && BARE.test(arg)) return arg;
  // Single quotes are literal in POSIX shells except for the quote itself.
  return `'${arg.replaceAll("'", `'\\''`)}'`;
}

export function shellQuote(argv: string[]): string {
  return argv.map(shellQuoteArg).join(" ");
}

/** The command as displayed: argv joined, quoted only where it matters. */
export function commandLine(entry: HistoryEntry): string {
  return entry.detail.type === "command" ? shellQuote(entry.detail.argv) : entry.detail.path;
}

/** What the copy button puts on the clipboard: a line that reproduces the
 * command in a terminal, including the directory it ran in and any env
 * overlay. Masked env values are left masked — a copy must not hand out a
 * secret the UI refused to show. */
export function copyableCommand(entry: HistoryEntry): string {
  if (entry.detail.type !== "command") return entry.detail.path;
  const parts: string[] = [];
  if (entry.detail.cwd) parts.push(`cd ${shellQuoteArg(entry.detail.cwd)}`);
  const env = entry.detail.env.map((v) => `${v.key}=${shellQuoteArg(v.value)}`).join(" ");
  parts.push(env ? `${env} ${shellQuote(entry.detail.argv)}` : shellQuote(entry.detail.argv));
  return parts.join(" && ");
}

export function isFailure(outcome: HistoryOutcome): boolean {
  if (outcome.type === "failed") return true;
  return outcome.type === "exited" && outcome.status !== 0;
}

export function isRunning(outcome: HistoryOutcome): boolean {
  return outcome.type === "running";
}

/** Short right-aligned status text: "exit 1", "3.2s", "cancelled". */
export function outcomeLabel(entry: HistoryEntry): string {
  switch (entry.outcome.type) {
    case "running":
      return "running…";
    case "cancelled":
      return "cancelled";
    case "detached":
      return "launched";
    case "applied":
      return "applied";
    case "failed":
      return "failed";
    case "exited":
      return entry.outcome.status === 0 ? "ok" : `exit ${entry.outcome.status}`;
  }
}

export function outcomeDetail(entry: HistoryEntry): string | null {
  return entry.outcome.type === "failed" ? entry.outcome.error : null;
}

export function formatDuration(ms: number | null): string {
  if (ms === null) return "";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  return `${minutes}m ${Math.round((ms % 60_000) / 1000)}s`;
}

export function formatTime(atUnixMs: number): string {
  return new Date(atUnixMs).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
