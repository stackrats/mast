// Dispatch an action and resolve on its terminal event, forwarding streamed
// output lines — the shared pattern behind every "apply with live output"
// dialog (repairs, project creation, …).

import type { Action } from "../bindings";
import { dispatchAction } from "./transport";

export interface OutputLine {
  line: string;
  stderr: boolean;
}

export function runActionCollecting(
  action: Action,
  onLine: (line: OutputLine) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    dispatchAction(action, (event) => {
      if (event.kind.type === "output") {
        onLine({ line: event.kind.line, stderr: event.kind.stderr });
      } else if (event.kind.type === "completed" || event.kind.type === "cancelled") {
        resolve();
      } else if (event.kind.type === "failed") {
        reject(new Error(event.kind.error));
      }
    }).catch(reject);
  });
}
