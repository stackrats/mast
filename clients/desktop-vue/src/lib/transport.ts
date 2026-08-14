// The only file that touches Tauri APIs directly. Everything above it
// (EngineSync, stores, components) is transport-agnostic.

import { Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

import {
  commands,
  events,
  type Action,
  type CustomServiceSpec,
  type HistoryEntry,
  type LogCapture,
  type LogLine,
  type OperationEvent,
  type OperationId,
  type ProjectId,
  type Result,
  type SubscriptionItem,
  type UsageSample,
} from "../bindings";
import type { PatchTransport } from "./engineSync";

function unwrap<T>(result: Result<T, string>): T {
  if (result.status === "error") throw new Error(result.error);
  return result.data;
}

export const tauriPatchTransport: PatchTransport = {
  async snapshot() {
    return unwrap(await commands.snapshot());
  },
  async startPatchStream(streamId, afterSeq) {
    unwrap(await commands.startPatchStream(streamId, afterSeq));
  },
};

export function onPatchStreamItem(
  cb: (streamId: number, item: SubscriptionItem) => void,
): Promise<() => void> {
  return events.patchStreamItem.listen((e) => cb(e.payload.streamId, e.payload.item));
}

/** Dispatch an action; `onEvent` receives its full operation event history. */
export async function dispatchAction(
  action: Action,
  onEvent: (event: OperationEvent) => void,
): Promise<OperationId> {
  const channel = new Channel<OperationEvent>();
  channel.onmessage = onEvent;
  return unwrap(await commands.dispatchAction(action, channel));
}

export async function cancelOperation(id: OperationId): Promise<void> {
  unwrap(await commands.cancelOperation(id));
}

export async function envReport(project: ProjectId) {
  return unwrap(await commands.envReport(project));
}

export async function networkAttachPreview(workspace: string, project: ProjectId) {
  return unwrap(await commands.networkAttachPreview(workspace, project));
}

export async function listSnapshots(workspace: string) {
  return unwrap(await commands.listSnapshots(workspace));
}

export async function snapshotReport(snapshotId: string) {
  return unwrap(await commands.snapshotReport(snapshotId));
}

export async function runDiagnostics() {
  return unwrap(await commands.runDiagnostics());
}

export async function repairPreview(repair: string, arg: string | null, project: ProjectId | null) {
  return unwrap(await commands.repairPreview(repair, arg, project));
}

export async function diagnosticsHistory() {
  return unwrap(await commands.diagnosticsHistory());
}

export async function catalog(project: ProjectId) {
  return unwrap(await commands.catalog(project));
}

export async function catalogPreview(project: ProjectId, service: string, remove: boolean) {
  return unwrap(await commands.catalogPreview(project, service, remove));
}

export async function serviceRemovePreview(project: ProjectId, service: string) {
  return unwrap(await commands.serviceRemovePreview(project, service));
}

export async function serviceImagePreview(project: ProjectId, service: string, image: string) {
  return unwrap(await commands.serviceImagePreview(project, service, image));
}

export async function customServicePreview(project: ProjectId, spec: CustomServiceSpec) {
  return unwrap(await commands.customServicePreview(project, spec));
}

/** Follow a service's container logs; returns a handle for stopLogStream. */
export async function streamServiceLogs(
  project: ProjectId,
  service: string,
  tail: number,
  onLine: (line: LogLine) => void,
): Promise<number> {
  const channel = new Channel<LogLine>();
  channel.onmessage = onLine;
  return unwrap(await commands.streamServiceLogs(project, service, tail, channel));
}

export async function stopLogStream(handle: number): Promise<void> {
  unwrap(await commands.stopLogStream(handle));
}

/** Native directory picker. Null when the user cancels. */
export async function pickDirectory(title: string): Promise<string | null> {
  const chosen = await open({ directory: true, multiple: false, title });
  return typeof chosen === "string" ? chosen : null;
}

/** The effect-history backlog: what Mast has run and written, oldest first. */
export async function historyRecent(): Promise<HistoryEntry[]> {
  return unwrap(await commands.historyRecent());
}

/** Follow effect history. Entries arrive on creation and again on completion,
 * so the consumer upserts by id rather than appending. */
export async function startHistoryStream(onEntry: (entry: HistoryEntry) => void): Promise<void> {
  const channel = new Channel<HistoryEntry>();
  channel.onmessage = onEntry;
  unwrap(await commands.startHistoryStream(channel));
}

/** Stored log captures, newest first — read from disk, so this returns
 * captures taken before the app was last closed. */
export async function logCaptures(limit: number): Promise<LogCapture[]> {
  return unwrap(await commands.logCaptures(limit));
}

/** Follow log captures. Append-only: a capture is never revised once written,
 * so the consumer prepends rather than upserting. */
export async function startCaptureStream(onCapture: (capture: LogCapture) => void): Promise<void> {
  const channel = new Channel<LogCapture>();
  channel.onmessage = onCapture;
  unwrap(await commands.startCaptureStream(channel));
}

/** Follow live CPU/memory usage. Calling this is what makes the engine start
 * sampling — it does no work while nobody is subscribed — so the caller is
 * expected to stop the stream whenever the numbers are not being looked at. */
export async function startUsageStream(onSample: (sample: UsageSample) => void): Promise<void> {
  const channel = new Channel<UsageSample>();
  channel.onmessage = onSample;
  unwrap(await commands.startUsageStream(channel));
}

/** Drop the usage subscription, which stops the engine sampling. */
export async function stopUsageStream(): Promise<void> {
  unwrap(await commands.stopUsageStream());
}
