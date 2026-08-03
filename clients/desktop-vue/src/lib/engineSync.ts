// Client half of the snapshot/subscribe protocol (plan §2):
// subscribe FIRST (buffering), fetch snapshot SECOND, discard buffered
// patches with seq <= snapshot.seq, then follow live patches. Any seq gap
// or an explicit ResyncRequired ends the generation and starts a new one.
//
// Transport-injected and framework-free so the protocol is unit-testable
// without Tauri; the Pinia store supplies the sink.

import type { EnginePatch, EngineSnapshot, SubscriptionItem } from "../bindings";

export type SyncPhase = "idle" | "syncing" | "live";

export interface PatchTransport {
  snapshot(): Promise<EngineSnapshot>;
  /** Ask the backend to (re)start the patch stream for this generation. */
  startPatchStream(streamId: number, afterSeq: number | null): Promise<void>;
}

export interface EngineStateSink {
  reset(snapshot: EngineSnapshot): void;
  apply(patch: EnginePatch): void;
  phase?(phase: SyncPhase): void;
}

export class EngineSync {
  seq = 0;
  phase: SyncPhase = "idle";
  /** Number of resynchronizations since connect — surfaced for debugging. */
  resyncs = 0;

  private streamId = 0;
  private buffer: EnginePatch[] = [];
  private syncing = false;

  constructor(
    private transport: PatchTransport,
    private sink: EngineStateSink,
  ) {}

  /** Feed every incoming PatchStreamItem event here, tagged with its stream. */
  handleItem(streamId: number, item: SubscriptionItem): void {
    if (streamId !== this.streamId) return; // superseded generation
    if (item.type === "resyncRequired") {
      void this.resync();
      return;
    }
    if (this.syncing) {
      this.buffer.push(item.patch);
      return;
    }
    this.applyLive(item.patch);
  }

  async connect(): Promise<void> {
    if (this.syncing) return; // coalesce concurrent (re)connects
    this.syncing = true;
    this.setPhase("syncing");
    this.buffer = [];
    this.streamId += 1;
    await this.transport.startPatchStream(this.streamId, null);
    const snapshot = await this.transport.snapshot();
    this.sink.reset(snapshot);
    this.seq = snapshot.seq;
    const buffered = this.buffer;
    this.buffer = [];
    this.syncing = false;
    for (const patch of buffered) {
      if (!this.applyLive(patch)) break; // gap mid-drain: resync already queued
    }
    if (this.syncing) return; // a drain gap started a new generation
    this.setPhase("live");
  }

  async resync(): Promise<void> {
    if (this.syncing) return;
    this.resyncs += 1;
    await this.connect();
  }

  /** Returns false when the patch triggered a resync. */
  private applyLive(patch: EnginePatch): boolean {
    if (patch.seq <= this.seq) return true; // duplicate/overlap with snapshot
    if (patch.seq !== this.seq + 1) {
      void this.resync();
      return false;
    }
    this.sink.apply(patch);
    this.seq = patch.seq;
    return true;
  }

  private setPhase(phase: SyncPhase): void {
    this.phase = phase;
    this.sink.phase?.(phase);
  }
}
