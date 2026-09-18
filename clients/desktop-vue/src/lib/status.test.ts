import { describe, expect, it } from "vite-plus/test";

import { processStatus } from "./status";

describe("Laravel process indicators", () => {
  it("stops showing startup progress once the daemon is observed running", () => {
    const launch = { actionType: "startProcess", terminal: null } as const;
    expect(processStatus(false, launch)).toBe("starting");
    // The operation stays open for the daemon's entire lifetime.
    expect(processStatus(true, launch)).toBe("running");
    // A code-change restart can briefly remove it from the process scan.
    expect(processStatus(false, launch)).toBe("starting");
    expect(processStatus(true, launch)).toBe("running");
  });

  it("keeps stop progress amber until the stop operation finishes", () => {
    const stop = { actionType: "stopProcess", terminal: null } as const;
    expect(processStatus(true, stop)).toBe("stopping");
    expect(processStatus(false, stop)).toBe("stopping");
    expect(processStatus(false, { ...stop, terminal: "completed" })).toBe("stopped");
  });

  it("shows cancellation as stopping instead of a healthy running daemon", () => {
    expect(processStatus(true, { terminal: null, cancelling: true })).toBe("stopping");
    expect(processStatus(false, { terminal: "cancelled" })).toBe("stopped");
  });

  it("shows failures without contradicting an externally running process", () => {
    expect(processStatus(false, { terminal: "failed" })).toBe("failed");
    expect(processStatus(true, { terminal: "failed" })).toBe("running");
    expect(processStatus(true)).toBe("running");
    expect(processStatus(false)).toBe("stopped");
  });
});
