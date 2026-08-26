import { computed } from "vue";
import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { LogCapture, OperationEvent } from "../bindings";
import { useEngineStore } from "./engine";

// The store must work against Pinia's reactive proxies — the regression this
// suite guards (op/log views compared by object identity dropped every event).
vi.mock("../lib/notify", () => ({ notify: vi.fn(async () => {}) }));

vi.mock("../lib/transport", () => ({
  tauriPatchTransport: {
    snapshot: vi.fn(),
    startPatchStream: vi.fn(),
  },
  onPatchStreamItem: vi.fn(async () => () => {}),
  dispatchAction: vi.fn(),
  cancelOperation: vi.fn(async () => {}),
  envReport: vi.fn(async () => null),
  networkAttachPreview: vi.fn(async () => null),
  listSnapshots: vi.fn(async () => []),
  snapshotReport: vi.fn(async () => null),
  runDiagnostics: vi.fn(async () => null),
  repairPreview: vi.fn(async () => null),
  diagnosticsHistory: vi.fn(async () => null),
  catalog: vi.fn(async () => []),
  catalogPreview: vi.fn(async () => null),
  serviceRemovePreview: vi.fn(async () => null),
  customServicePreview: vi.fn(async () => null),
  streamServiceLogs: vi.fn(),
  stopLogStream: vi.fn(async () => {}),
  historyRecent: vi.fn(async () => []),
  startHistoryStream: vi.fn(async () => {}),
  logCaptures: vi.fn(async () => []),
  startCaptureStream: vi.fn(async () => {}),
  appVersion: vi.fn(async () => "0.2.0"),
  startUsageStream: vi.fn(async () => {}),
  stopUsageStream: vi.fn(async () => {}),
}));

import { dispatchAction, streamServiceLogs, stopLogStream } from "../lib/transport";
import { notify } from "../lib/notify";

const dispatchMock = vi.mocked(dispatchAction);
const streamLogsMock = vi.mocked(streamServiceLogs);
const stopLogsMock = vi.mocked(stopLogStream);

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
});

describe("runLifecycle", () => {
  it("records output and terminal events on the reactive view", async () => {
    let emit: ((event: OperationEvent) => void) | null = null;
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emit = onEvent;
      return 42;
    });

    const store = useEngineStore();
    await store.runLifecycle("p1", "stop", { type: "stopProject", id: "p1" });
    expect(store.operations["p1"].id).toBe(42);

    emit!({ operation: 42, kind: { type: "started" } });
    emit!({ operation: 42, kind: { type: "output", line: "Stopping api", stderr: true } });
    emit!({ operation: 42, kind: { type: "completed" } });

    const op = store.operations["p1"];
    expect(op.lines).toEqual([{ line: "Stopping api", stderr: true }]);
    expect(op.terminal).toBe("completed");
  });

  it("leaves the logs panel however the user left it", async () => {
    dispatchMock.mockImplementation(async () => 1);

    const store = useEngineStore();
    // Closed stays closed: an operation must not steal vertical space or undo
    // a deliberate close.
    store.logsOpen = false;
    await store.runLifecycle("p1", "start", { type: "startProject", id: "p1" });
    expect(store.logsOpen).toBe(false);
    // Output still accumulates behind the closed panel.
    expect(store.activity.at(-1)?.line).toBe("▶ start");

    store.logsOpen = true;
    await store.runLifecycle("p2", "start", { type: "startProject", id: "p2" });
    expect(store.logsOpen).toBe(true);
  });

  it("handles terminal events that arrive before dispatch resolves", async () => {
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      // Channel events can beat the invoke reply (fast-failing ops).
      onEvent({ operation: 7, kind: { type: "failed", error: "exit 1" } });
      return 7;
    });

    const store = useEngineStore();
    await store.runLifecycle("p1", "start", { type: "startProject", id: "p1" });
    expect(store.operations["p1"].terminal).toBe("failed");
    expect(store.operations["p1"].error).toBe("exit 1");
  });

  it("ignores events from a superseded operation", async () => {
    const emits: ((event: OperationEvent) => void)[] = [];
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emits.push(onEvent);
      return emits.length;
    });

    const store = useEngineStore();
    await store.runLifecycle("p1", "start", { type: "startProject", id: "p1" });
    await store.runLifecycle("p1", "stop", { type: "stopProject", id: "p1" });

    emits[0]({ operation: 1, kind: { type: "completed" } }); // stale generation
    expect(store.operations["p1"].terminal).toBeNull();
    emits[1]({ operation: 2, kind: { type: "completed" } });
    expect(store.operations["p1"].terminal).toBe("completed");
  });
});

describe("hasRunningOp", () => {
  it("is true only between dispatch and the terminal event", async () => {
    let emit: ((event: OperationEvent) => void) | null = null;
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emit = onEvent;
      return 7;
    });

    const store = useEngineStore();
    expect(store.hasRunningOp("p1")).toBe(false);

    await store.runLifecycle("p1", "rebuild", { type: "rebuildProject", id: "p1" });
    expect(store.hasRunningOp("p1")).toBe(true);
    // A sibling project is untouched — the lock is per key, not global.
    expect(store.hasRunningOp("p2")).toBe(false);

    emit!({ operation: 7, kind: { type: "completed" } });
    expect(store.hasRunningOp("p1")).toBe(false);
  });

  it("stays true through a failure that has not landed yet, then clears", async () => {
    let emit: ((event: OperationEvent) => void) | null = null;
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emit = onEvent;
      return 8;
    });

    const store = useEngineStore();
    await store.runLifecycle("p1", "rebuild", { type: "rebuildProject", id: "p1" });
    emit!({ operation: 8, kind: { type: "output", line: "#10 ...", stderr: false } });
    expect(store.hasRunningOp("p1")).toBe(true);

    emit!({ operation: 8, kind: { type: "failed", error: "boom" } });
    expect(store.hasRunningOp("p1")).toBe(false);
  });
});

describe("completion notifications", () => {
  // The point is to catch the rebuild you walked away from, not to ping on
  // every `stop` the user is sitting and watching.
  it("stays quiet for an operation that finished promptly", async () => {
    let emit: ((event: OperationEvent) => void) | null = null;
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emit = onEvent;
      return 9;
    });

    const store = useEngineStore();
    await store.runLifecycle("p1", "stop", { type: "stopProject", id: "p1" });
    emit!({ operation: 9, kind: { type: "completed" } });
    expect(vi.mocked(notify)).not.toHaveBeenCalled();
  });

  it("announces one that outlasted the user's attention", async () => {
    let emit: ((event: OperationEvent) => void) | null = null;
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emit = onEvent;
      return 10;
    });

    const store = useEngineStore();
    await store.runLifecycle("p1", "rebuild", { type: "rebuildProject", id: "p1" });
    // Backdate the start rather than waiting out a real minute.
    store.operations["p1"].startedAt -= 20 * 60_000;
    emit!({ operation: 10, kind: { type: "completed" } });

    expect(vi.mocked(notify)).toHaveBeenCalledWith(
      "operations",
      "rebuild finished",
      "Took 20m 00s.",
    );
  });
});

describe("command ordering", () => {
  function project(commands: unknown[]) {
    return {
      id: "p1",
      name: "p1",
      path: "/p1",
      status: "running",
      services: [],
      commands,
      processes: [],
      warnings: [],
    };
  }

  it("waits for the dependency's ready line before starting a dependent", async () => {
    const emitters = new Map<string, (event: OperationEvent) => void>();
    let next = 1;
    dispatchMock.mockImplementation(async (action, onEvent) => {
      emitters.set((action as { name: string }).name, onEvent);
      return next++;
    });

    const store = useEngineStore();
    store.projects = [
      project([
        { name: "api", command: "sail artisan dev", autoStart: true, readyWhen: "Server running" },
        { name: "web", command: "vp dev", autoStart: true, after: "api" },
      ]),
    ] as never;
    const [api, web] = store.projects[0].commands!;

    void store.autoStartCommand("p1", api);
    void store.autoStartCommand("p1", web);
    await vi.waitFor(() => expect(emitters.has("api")).toBe(true));

    // The banner has not arrived, so nothing may have started `web` yet.
    emitters.get("api")!({
      operation: 1,
      kind: { type: "output", line: "booting", stderr: false },
    });
    await new Promise((r) => setTimeout(r, 50));
    expect(emitters.has("web")).toBe(false);

    emitters.get("api")!({
      operation: 1,
      // Colour is exactly how a tool prints its banner; the match must see
      // through it.
      kind: { type: "output", line: "\x1b[32mServer running\x1b[39m on :80", stderr: false },
    });
    await vi.waitFor(() => expect(emitters.has("web")).toBe(true));
  });

  it("does not start a dependent whose dependency failed", async () => {
    const emitters = new Map<string, (event: OperationEvent) => void>();
    dispatchMock.mockImplementation(async (action, onEvent) => {
      emitters.set((action as { name: string }).name, onEvent);
      return 1;
    });

    const store = useEngineStore();
    store.projects = [
      project([
        { name: "api", command: "sail artisan dev", autoStart: true, readyWhen: "up" },
        { name: "web", command: "vp dev", autoStart: true, after: "api" },
      ]),
    ] as never;
    const [api, web] = store.projects[0].commands!;

    void store.autoStartCommand("p1", api);
    const dependent = store.autoStartCommand("p1", web);
    await vi.waitFor(() => expect(emitters.has("api")).toBe(true));

    emitters.get("api")!({ operation: 1, kind: { type: "failed", error: "boom" } });
    await dependent;
    expect(emitters.has("web")).toBe(false);
    // And it says so, rather than leaving a chip mysteriously grey.
    expect(store.activity.some((a) => a.line.includes("never came up"))).toBe(true);
  });

  it("treats a one-shot that exited cleanly as up", async () => {
    const emitters = new Map<string, (event: OperationEvent) => void>();
    dispatchMock.mockImplementation(async (action, onEvent) => {
      emitters.set((action as { name: string }).name, onEvent);
      return 1;
    });

    const store = useEngineStore();
    store.projects = [
      project([
        { name: "build", command: "vp build", autoStart: true },
        { name: "serve", command: "vp preview", autoStart: true, after: "build" },
      ]),
    ] as never;
    const [build, serve] = store.projects[0].commands!;

    void store.autoStartCommand("p1", build);
    void store.autoStartCommand("p1", serve);
    await vi.waitFor(() => expect(emitters.has("build")).toBe(true));

    emitters.get("build")!({ operation: 1, kind: { type: "completed" } });
    await vi.waitFor(() => expect(emitters.has("serve")).toBe(true));
  });

  // The ring caps at OP_LINE_CAP; a command noisier than that must not read
  // as having gone quiet just because the buffer stopped growing.
  it("keeps waiting on a command that is chattering past the line cap", async () => {
    const emitters = new Map<string, (event: OperationEvent) => void>();
    dispatchMock.mockImplementation(async (action, onEvent) => {
      emitters.set((action as { name: string }).name, onEvent);
      return 1;
    });

    const store = useEngineStore();
    store.projects = [
      project([
        { name: "api", command: "vp build", autoStart: true },
        { name: "web", command: "vp dev", autoStart: true, after: "api" },
      ]),
    ] as never;
    const [api, web] = store.projects[0].commands!;

    void store.autoStartCommand("p1", api);
    void store.autoStartCommand("p1", web);
    await vi.waitFor(() => expect(emitters.has("api")).toBe(true));

    const emit = emitters.get("api")!;
    for (let i = 0; i < 260; i++) {
      emit({ operation: 1, kind: { type: "output", line: `step ${i}`, stderr: false } });
    }
    const op = store.operations["p1:cmd:api"];
    expect(op.lines.length).toBe(200); // the ring is full and stuck there
    expect(op.received).toBe(260); // but the count still moves

    // Keep it talking for longer than a whole settle window — the buffer
    // stays pinned at the cap throughout, which is the trap.
    for (let n = 0; n < 9; n++) {
      await new Promise((r) => setTimeout(r, 500));
      emit({ operation: 1, kind: { type: "output", line: `still working ${n}`, stderr: false } });
    }
    expect(emitters.has("web")).toBe(false);
  });

  it("starts a command with no dependency immediately", async () => {
    dispatchMock.mockImplementation(async () => 1);
    const store = useEngineStore();
    store.projects = [project([{ name: "dev", command: "vp dev", autoStart: true }])] as never;
    await store.autoStartCommand("p1", store.projects[0].commands![0]);
    expect(dispatchMock).toHaveBeenCalled();
  });
});

describe("logs", () => {
  it("appends streamed lines to the reactive view", async () => {
    let emit: ((line: { service: string; message: string; stderr: boolean }) => void) | null = null;
    streamLogsMock.mockImplementation(async (_p, _s, _tail, onLine) => {
      emit = onLine;
      return 9;
    });

    const store = useEngineStore();
    await store.openLogs("p1", "api");
    expect(store.logs?.handle).toBe(9);

    emit!({ service: "api", message: "listening", stderr: false });
    emit!({ service: "api", message: "warn", stderr: true });
    expect(store.logs?.lines.map((l) => l.message)).toEqual(["listening", "warn"]);

    await store.closeLogs();
    expect(store.logs).toBeNull();
    expect(stopLogsMock).toHaveBeenCalledWith(9);
  });

  it("stops the stream if the panel closed while it was opening", async () => {
    let resolveOpen: ((handle: number) => void) | null = null;
    streamLogsMock.mockImplementation(
      () => new Promise<number>((resolve) => (resolveOpen = resolve)),
    );

    const store = useEngineStore();
    const opening = store.openLogs("p1", "api");
    await vi.waitFor(() => {
      if (!store.logs) throw new Error("panel not registered yet");
    });
    await store.closeLogs(); // user closes before the stream handle arrives
    resolveOpen!(5);
    await opening;
    expect(store.logs).toBeNull();
    expect(stopLogsMock).toHaveBeenCalledWith(5);
  });
});

describe("applyPatch auto-start", () => {
  const makeProject = (status: "stopped" | "running") => ({
    id: "p1",
    name: "app",
    path: "/x/app",
    status,
    composeProjectName: "app",
    isSail: true,
    services: [],
    resolutionError: null,
    warnings: [],
    commands: [{ name: "dev", command: "sail npm run dev", autoStart: true }],
    processes: [],
    gitBranch: null,
    gitDirty: null,
  });

  it("fires only on an observed transition into running", async () => {
    dispatchMock.mockResolvedValue(9);
    const store = useEngineStore();
    store.projects = [makeProject("stopped")];

    store.applyPatch({
      seq: 1,
      event: { type: "projectStatusChanged", id: "p1", status: "running" },
    });
    await vi.waitFor(() => expect(dispatchMock).toHaveBeenCalledTimes(1));
    expect(dispatchMock.mock.calls[0][0]).toEqual({
      type: "runProjectCommand",
      id: "p1",
      name: "dev",
    });

    // Already running → no re-fire.
    store.applyPatch({
      seq: 2,
      event: { type: "projectStatusChanged", id: "p1", status: "running" },
    });
    expect(dispatchMock).toHaveBeenCalledTimes(1);
  });

  it("does not fire for a project that APPEARS already running", () => {
    dispatchMock.mockResolvedValue(9);
    const store = useEngineStore();
    store.applyPatch({ seq: 1, event: { type: "projectAdded", project: makeProject("running") } });
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});

describe("dismissOperation", () => {
  it("removes a cancelled scaffold so its card stops being rendered", async () => {
    let emit: ((event: OperationEvent) => void) | null = null;
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      emit = onEvent;
      return 9;
    });

    const store = useEngineStore();
    await store.runLifecycle("new:that-thang", "create that-thang", {
      type: "createProject",
      parent: "/tmp",
      name: "that-thang",
      php: "85",
      services: [],
    });
    emit!({ operation: 9, kind: { type: "cancelled" } });
    expect(store.operations["new:that-thang"].terminal).toBe("cancelled");

    store.dismissOperation("new:that-thang");
    expect(store.operations["new:that-thang"]).toBeUndefined();
    expect(Object.keys(store.operations)).not.toContain("new:that-thang");
  });
});

describe("operations reactivity", () => {
  it("a computed over the operations map recomputes when a key is deleted", async () => {
    dispatchMock.mockImplementation(async () => 11);
    const store = useEngineStore();
    await store.runLifecycle("new:x", "create x", {
      type: "createProject",
      parent: "/tmp",
      name: "x",
      php: "85",
      services: [],
    });

    // Mirrors HomePane's `scaffolding` computed.
    const keys = computed(() => Object.keys(store.operations));
    expect(keys.value).toContain("new:x");

    store.dismissOperation("new:x");
    expect(keys.value).not.toContain("new:x");
  });
});

describe("log captures", () => {
  const capture = (id: number, overrides: Partial<LogCapture> = {}): LogCapture => ({
    id,
    atUnixMs: 1_700_000_000_000 + id,
    project: "p1",
    projectName: "acme",
    service: "queue",
    containerId: `cid-${id}`,
    reason: { type: "exited", status: 1 },
    windowSecs: 60,
    lines: [{ at: null, message: "boom", stderr: true }],
    truncated: false,
    ...overrides,
  });

  it("keeps captures newest first however they arrive", () => {
    const store = useEngineStore();
    // The live stream and the backlog fetch overlap, so order is not given.
    store.addCapture(capture(2));
    store.addCapture(capture(5));
    store.addCapture(capture(3));

    expect(store.captures.map((c) => c.id)).toEqual([5, 3, 2]);
  });

  it("ignores a capture already held, so an overlapping backlog is harmless", () => {
    const store = useEngineStore();
    store.addCapture(capture(4));
    store.addCapture(capture(4));

    expect(store.captures).toHaveLength(1);
  });

  it("drops the oldest past the cap", () => {
    const store = useEngineStore();
    for (let id = 1; id <= 60; id += 1) store.addCapture(capture(id));

    expect(store.captures).toHaveLength(50);
    expect(store.captures[0].id).toBe(60);
    expect(store.captures.at(-1)!.id).toBe(11);
  });

  it("badges only what the user has not seen", () => {
    const store = useEngineStore();
    store.addCapture(capture(1));
    store.addCapture(capture(2));
    expect(store.unseenCaptureCount).toBe(2);

    store.markCapturesSeen();
    expect(store.unseenCaptureCount).toBe(0);

    // A capture that lands afterwards badges again.
    store.addCapture(capture(3));
    expect(store.unseenCaptureCount).toBe(1);
  });

  it("clearing empties the view and asks the engine to delete the store", async () => {
    dispatchMock.mockImplementation(async (_action, onEvent) => {
      onEvent({ operation: 1, kind: { type: "completed" } });
      return 1;
    });
    const store = useEngineStore();
    store.addCapture(capture(1));

    await store.clearCaptures();

    expect(store.captures).toHaveLength(0);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "clearLogCaptures" }, expect.any(Function));
  });
});
