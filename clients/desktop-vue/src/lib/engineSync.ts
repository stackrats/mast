// Client half of the snapshot/subscribe protocol (plan §2):
// subscribe FIRST (buffering), fetch snapshot SECOND, discard buffered
// patches with seq <= snapshot.seq, then follow live patches. Any seq gap
// or an explicit ResyncRequired ends the generation and starts a new one.
//
// Transport-injected and framework-free so the protocol is unit-testable
// without Tauri; the Pinia store supplies the sink.

import type { EnginePatch, EngineSnapshot, SubscriptionItem } from "../bindings";

export type SyncPhase = "idle" | "syncing" | "live" | "error";

export interface PatchTransport {
  snapshot(): Promise<EngineSnapshot>;
  /** Ask the backend to (re)start the patch stream for this generation. */
  startPatchStream(streamId: number, afterSeq: number | null): Promise<void>;
}

export interface EngineStateSink {
  reset(snapshot: EngineSnapshot): void;
  apply(patch: EnginePatch): void;
  phase?(phase: SyncPhase): void;
  error?(error: unknown): void;
}

export class EngineSync {
  seq = 0;
  phase: SyncPhase = "idle";
  /** Number of resynchronizations since connect — surfaced for debugging. */
  resyncs = 0;

  private streamId = 0;
  private buffer: EnginePatch[] = [];
  private syncing = false;
  private resyncPending = false;

  constructor(
    private transport: PatchTransport,
    private sink: EngineStateSink,
  ) {}

  /** Feed every incoming PatchStreamItem event here, tagged with its stream. */
  handleItem(streamId: number, item: SubscriptionItem): void {
    if (streamId !== this.streamId || this.phase === "error") return; // superseded generation
    if (item.type === "resyncRequired") {
      if (this.syncing) this.resyncPending = true;
      else this.resyncInBackground();
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
    this.resyncPending = false;
    this.streamId += 1;
    let snapshot: EngineSnapshot;
    try {
      await this.transport.startPatchStream(this.streamId, null);
      snapshot = await this.transport.snapshot();
    } catch (error) {
      this.syncing = false;
      this.buffer = [];
      this.setPhase("error");
      this.sink.error?.(error);
      throw error;
    }
    // A stream can overflow while the snapshot is in flight. Its snapshot
    // need not contain the lost changes, and the backend has closed that
    // stream, so publishing it as live would leave the UI permanently stale.
    if (this.resyncPending) {
      this.syncing = false;
      await this.resync();
      return;
    }
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
      this.resyncInBackground();
      return false;
    }
    this.sink.apply(patch);
    this.seq = patch.seq;
    return true;
  }

  private resyncInBackground(): void {
    // connect reports errors to the sink; consume the rejected promise from
    // event-driven retries rather than leaving an unhandled rejection.
    void this.resync().catch(() => {});
  }

  private setPhase(phase: SyncPhase): void {
    this.phase = phase;
    this.sink.phase?.(phase);
  }
}
