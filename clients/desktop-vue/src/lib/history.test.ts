import { describe, expect, it } from "vite-plus/test";

import type { HistoryEntry } from "../bindings";
import { copyableCommand, isFailure, outcomeLabel, shellQuote } from "./history";

function command(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    id: 1,
    atUnixMs: 0,
    label: "Start acme",
    project: null,
    operation: null,
    origin: "user",
    detail: {
      type: "command",
      argv: ["docker", "compose", "up", "-d"],
      cwd: "/home/dev/acme",
      env: [],
      streaming: true,
    },
    outcome: { type: "exited", status: 0 },
    durationMs: 1200,
    output: [],
    ...overrides,
  };
}

describe("shellQuote", () => {
  it("leaves shell-safe arguments alone", () => {
    expect(shellQuote(["docker", "compose", "-f", "/home/dev/a/compose.yaml"])).toBe(
      "docker compose -f /home/dev/a/compose.yaml",
    );
  });

  it("quotes anything a shell would reinterpret", () => {
    expect(shellQuote(["sh", "-c", "echo hi; rm -rf $HOME"])).toBe(`sh -c 'echo hi; rm -rf $HOME'`);
    expect(shellQuote([""])).toBe(`''`);
  });

  it("survives embedded single quotes", () => {
    // The POSIX escape: close the quote, emit a literal one, reopen. The
    // copied line has to be pasteable verbatim, quotes and all.
    expect(shellQuote([`it's`])).toBe("'it'\\''s'");
  });
});

describe("copyableCommand", () => {
  it("includes the working directory so the command reproduces", () => {
    expect(copyableCommand(command())).toBe("cd /home/dev/acme && docker compose up -d");
  });

  it("keeps masked env values masked — a copy must not leak a secret", () => {
    const entry = command({
      detail: {
        type: "command",
        argv: ["php", "artisan", "migrate"],
        cwd: null,
        env: [{ key: "DB_PASSWORD", value: "•••redacted•••", masked: true }],
        streaming: false,
      },
    });
    expect(copyableCommand(entry)).toContain("DB_PASSWORD='•••redacted•••'");
    expect(copyableCommand(entry)).not.toContain("hunter2");
  });

  it("falls back to the path for a file write", () => {
    const entry = command({
      detail: { type: "fileWrite", path: "/home/dev/acme/.env", summary: ["APP_PORT=8123"] },
    });
    expect(copyableCommand(entry)).toBe("/home/dev/acme/.env");
  });
});

describe("outcomes", () => {
  it("treats any non-zero exit as a failure", () => {
    expect(isFailure({ type: "exited", status: 0 })).toBe(false);
    expect(isFailure({ type: "exited", status: 1 })).toBe(true);
    expect(isFailure({ type: "failed", error: "spawn failed" })).toBe(true);
    // Cancelling is a choice the user made, not a failure to report.
    expect(isFailure({ type: "cancelled" })).toBe(false);
  });

  it("labels an exit status rather than hiding it", () => {
    expect(outcomeLabel(command())).toBe("ok");
    expect(outcomeLabel(command({ outcome: { type: "exited", status: 137 } }))).toBe("exit 137");
    expect(outcomeLabel(command({ outcome: { type: "running" } }))).toBe("running…");
  });
});
