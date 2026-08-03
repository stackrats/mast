import { describe, expect, it } from "vite-plus/test";

import type {
  EnginePatch,
  EngineSnapshot,
  ProjectStatus,
  ProjectSummary,
  SubscriptionItem,
} from "../bindings";
import { EngineSync, type EngineStateSink, type PatchTransport } from "./engineSync";
import { applyPatchEvent } from "./projects";
import { pushBounded } from "./ring";

function project(id: string, name: string, status: ProjectStatus = "stopped"): ProjectSummary {
  return {
    id,
    name,
    path: `/code/${name}`,
    status,
    composeProjectName: name,
    isSail: false,
    services: [],
    resolutionError: null,
    warnings: [],
  };
}

function snap(seq: number): EngineSnapshot {
  return {
    protocolVersion: 1,
    seq,
    readOnly: false,
    docker: { available: true, contextName: "default", endpoint: "unix:///x", error: null },
    integrations: { terminal: null, editor: null },
    watchedDirectories: [],
    discovered: [],
    projects: [project("p1", "fake-sail-app")],
    workspaces: [],
  };
}

function statusPatch(seq: number, status: ProjectStatus = "running"): EnginePatch {
  return { seq, event: { type: "projectStatusChanged", id: "p1", status } };
}

function patchItem(patch: EnginePatch): SubscriptionItem {
  return { type: "patch", patch };
}

class FakeTransport implements PatchTransport {
  snapshotCalls = 0;
  streams: Array<{ id: number; afterSeq: number | null }> = [];
  /** Fired inside startPatchStream — lets tests emit concurrent items. */
  onStart?: (id: number) => void;
  constructor(private snapshots: EngineSnapshot[]) {}

  async startPatchStream(id: number, afterSeq: number | null): Promise<void> {
    this.streams.push({ id, afterSeq });
    this.onStart?.(id);
  }

  async snapshot(): Promise<EngineSnapshot> {
    this.snapshotCalls += 1;
    return this.snapshots[Math.min(this.snapshotCalls - 1, this.snapshots.length - 1)];
  }
}

class RecordingSink implements EngineStateSink {
  resets: number[] = [];
  applied: number[] = [];
  reset(s: EngineSnapshot): void {
    this.resets.push(s.seq);
  }
  apply(p: EnginePatch): void {
    this.applied.push(p.seq);
  }
}

describe("EngineSync protocol", () => {
  it("subscribes first, then discards buffered patches folded into the snapshot", async () => {
    const transport = new FakeTransport([snap(10)]);
    const sink = new RecordingSink();
    const sync = new EngineSync(transport, sink);
    // Patches racing in between subscribe and snapshot: 9, 10 are already in
    // the snapshot; 11 is genuinely new.
    transport.onStart = (id) => {
      sync.handleItem(id, patchItem(statusPatch(9)));
      sync.handleItem(id, patchItem(statusPatch(10)));
      sync.handleItem(id, patchItem(statusPatch(11)));
    };
    await sync.connect();

    expect(transport.streams).toEqual([{ id: 1, afterSeq: null }]);
    expect(sink.resets).toEqual([10]);
    expect(sink.applied).toEqual([11]);
    expect(sync.seq).toBe(11);
    expect(sync.phase).toBe("live");
    expect(sync.resyncs).toBe(0);
  });

  it("treats a live seq gap as a forced resync", async () => {
    const transport = new FakeTransport([snap(10), snap(12)]);
    const sink = new RecordingSink();
    const sync = new EngineSync(transport, sink);
    await sync.connect();

    sync.handleItem(1, patchItem(statusPatch(12))); // gap: expected 11
    await Promise.resolve(); // let the queued resync run
    await Promise.resolve();

    expect(sync.resyncs).toBe(1);
    expect(transport.snapshotCalls).toBe(2);
    expect(transport.streams.map((s) => s.id)).toEqual([1, 2]);
    expect(sink.resets).toEqual([10, 12]);
    expect(sink.applied).toEqual([]); // the gap patch was never applied
    expect(sync.seq).toBe(12);
  });

  it("resyncs on an explicit ResyncRequired item", async () => {
    const transport = new FakeTransport([snap(5), snap(8)]);
    const sink = new RecordingSink();
    const sync = new EngineSync(transport, sink);
    await sync.connect();

    sync.handleItem(1, { type: "resyncRequired" });
    await Promise.resolve();
    await Promise.resolve();

    expect(sync.resyncs).toBe(1);
    expect(sink.resets).toEqual([5, 8]);
    expect(sync.seq).toBe(8);
    expect(sync.phase).toBe("live");
  });

  it("ignores duplicates and items from superseded stream generations", async () => {
    const transport = new FakeTransport([snap(10)]);
    const sink = new RecordingSink();
    const sync = new EngineSync(transport, sink);
    await sync.connect();

    sync.handleItem(1, patchItem(statusPatch(10))); // duplicate: ignored
    sync.handleItem(99, patchItem(statusPatch(11))); // wrong generation: ignored
    expect(sink.applied).toEqual([]);
    expect(sync.seq).toBe(10);
    expect(sync.resyncs).toBe(0);

    sync.handleItem(1, patchItem(statusPatch(11))); // contiguous: applied
    expect(sink.applied).toEqual([11]);
    expect(sync.seq).toBe(11);
  });
});

describe("applyPatchEvent reducer", () => {
  const base = [project("p1", "alpha")];

  it("adds, updates fully, changes status, and removes", () => {
    const added = applyPatchEvent(base, { type: "projectAdded", project: project("p2", "beta") });
    expect(added.map((p) => p.name)).toEqual(["alpha", "beta"]);

    const updated = applyPatchEvent(added, {
      type: "projectUpdated",
      project: {
        ...project("p2", "beta", "running"),
        services: [{ name: "app", containerId: "cid", state: "running", health: "healthy" }],
      },
    });
    const beta = updated.find((p) => p.id === "p2")!;
    expect(beta.status).toBe("running");
    expect(beta.services).toHaveLength(1);

    const statusOnly = applyPatchEvent(updated, {
      type: "projectStatusChanged",
      id: "p1",
      status: "degraded",
    });
    expect(statusOnly.find((p) => p.id === "p1")?.status).toBe("degraded");

    const removed = applyPatchEvent(statusOnly, { type: "projectRemoved", id: "p1" });
    expect(removed.map((p) => p.id)).toEqual(["p2"]);
  });

  it("re-add replaces and keeps name ordering", () => {
    const readded = applyPatchEvent(base, {
      type: "projectAdded",
      project: project("p1", "renamed", "running"),
    });
    expect(readded).toHaveLength(1);
    expect(readded[0].name).toBe("renamed");
  });

  it("leaves the list untouched for non-project events", () => {
    const result = applyPatchEvent(base, {
      type: "dockerStatusChanged",
      status: { available: false, contextName: null, endpoint: null, error: "gone" },
    });
    expect(result).toEqual(base);
  });
});

describe("pushBounded ring buffer", () => {
  it("drops the oldest entries beyond the cap", () => {
    const items: number[] = [];
    for (let i = 0; i < 7; i += 1) pushBounded(items, i, 5);
    expect(items).toEqual([2, 3, 4, 5, 6]);
  });
});
