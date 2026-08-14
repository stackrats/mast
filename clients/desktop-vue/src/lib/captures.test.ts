import { describe, expect, it } from "vite-plus/test";

import type { LogCapture } from "../bindings";
import { captureSummary, copyableCapture, isPostMortem, lineTime, reasonLabel } from "./captures";

function capture(overrides: Partial<LogCapture> = {}): LogCapture {
  return {
    id: 1,
    atUnixMs: Date.parse("2026-08-12T14:22:10.000Z"),
    project: "p1",
    projectName: "acme",
    service: "queue",
    containerId: "cid",
    reason: { type: "exited", status: 1 },
    windowSecs: 60,
    lines: [
      { at: "2026-08-12T14:22:03.123456789Z", message: "Processing jobs", stderr: false },
      { at: null, message: "connection refused", stderr: true },
    ],
    truncated: false,
    ...overrides,
  };
}

describe("reasonLabel", () => {
  it("names every reason a capture can have", () => {
    expect(reasonLabel({ type: "teardown", verb: "restart" })).toBe("before restart");
    expect(reasonLabel({ type: "exited", status: 137 })).toBe("exited 137");
    expect(reasonLabel({ type: "unhealthy" })).toBe("unhealthy");
    expect(reasonLabel({ type: "readyTimeout" })).toBe("not ready");
    expect(reasonLabel({ type: "manual" })).toBe("captured");
  });

  it("says only that it exited when docker did not report a code", () => {
    expect(reasonLabel({ type: "exited", status: null })).toBe("exited");
  });
});

describe("isPostMortem", () => {
  it("is true only for deaths Mast did not ask for", () => {
    expect(isPostMortem({ type: "exited", status: 1 })).toBe(true);
    expect(isPostMortem({ type: "unhealthy" })).toBe(true);
    // A teardown capture precedes a deliberate stop — nothing went wrong.
    expect(isPostMortem({ type: "teardown", verb: "stop" })).toBe(false);
    expect(isPostMortem({ type: "manual" })).toBe(false);
  });
});

describe("captureSummary", () => {
  it("counts lines and names the window", () => {
    expect(captureSummary(capture())).toBe("2 lines · last 60s");
  });

  it("singularizes one line", () => {
    expect(captureSummary(capture({ lines: [{ at: null, message: "x", stderr: false }] }))).toBe(
      "1 line · last 60s",
    );
  });

  it("says so when the head was dropped", () => {
    expect(captureSummary(capture({ truncated: true }))).toBe(
      "2 lines · last 60s · oldest dropped",
    );
  });
});

describe("lineTime", () => {
  it("reads docker's RFC3339 stamp", () => {
    // Rendered in the viewer's locale/zone, so assert the parse, not the text.
    const at = "2026-08-12T14:22:03.123456789Z";
    expect(lineTime(at, 0)).toBe(
      new Date(Date.parse(at)).toLocaleTimeString(undefined, {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }),
    );
  });

  it("falls back to the capture's own time for a line without one", () => {
    const fallback = Date.parse("2026-08-12T14:22:10.000Z");
    expect(lineTime(null, fallback)).toBe(lineTime("2026-08-12T14:22:10.000Z", 0));
  });

  it("falls back rather than rendering an invalid date", () => {
    const fallback = Date.parse("2026-08-12T14:22:10.000Z");
    expect(lineTime("not a timestamp", fallback)).toBe(lineTime(null, fallback));
  });
});

describe("copyableCapture", () => {
  it("heads the lines with what they came from", () => {
    expect(copyableCapture(capture())).toBe(
      [
        "# acme · queue — exited 1",
        "2026-08-12T14:22:03.123456789Z Processing jobs",
        "connection refused",
      ].join("\n"),
    );
  });
});
