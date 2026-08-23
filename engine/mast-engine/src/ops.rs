//! Cancellable-operation machinery (carried over from M1): every mutation
//! runs as a tracked operation with a replayable event history, and the
//! generic streamed-subprocess runner feeds command output into that history.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use mast_contract::{ErrorInfo, OperationEvent, OperationEventKind, OperationId};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{Engine, Redactor};

pub(crate) struct OpHandle {
    pub(crate) cancel: CancellationToken,
    /// Full history so late subscribers replay from the first event.
    pub(crate) events: Mutex<Vec<OperationEvent>>,
    pub(crate) events_tx: broadcast::Sender<(usize, OperationEvent)>,
    /// Error signatures spotted in this operation's output (first-seen
    /// order, deduped). A failing operation ends with their explanations, so
    /// the known failure waves — GPG outages, port squatters, version-locked
    /// volumes — read as sentences instead of scrollback.
    pub(crate) signatures: Mutex<Vec<&'static mast_diagnostics::ErrorSignature>>,
}

impl Engine {
    pub(crate) fn new_operation(&self) -> (OperationId, Arc<OpHandle>) {
        let id = OperationId(self.inner.next_op.fetch_add(1, Ordering::Relaxed) + 1);
        let (events_tx, _) = broadcast::channel(64);
        let handle = Arc::new(OpHandle {
            cancel: CancellationToken::new(),
            events: Mutex::new(Vec::new()),
            events_tx,
            signatures: Mutex::new(Vec::new()),
        });
        self.inner.ops.lock().unwrap().insert(id.0, handle.clone());
        (id, handle)
    }

    pub(crate) fn emit_op(&self, handle: &OpHandle, id: OperationId, kind: OperationEventKind) {
        if let OperationEventKind::Output { line, .. } = &kind
            && let Some(sig) = mast_diagnostics::classify_line(line)
        {
            let mut seen = handle.signatures.lock().unwrap();
            if !seen.iter().any(|s| s.id == sig.id) {
                seen.push(sig);
            }
        }
        let event = OperationEvent { operation: id, kind };
        // Push + broadcast under the history lock so index order is total.
        let mut events = handle.events.lock().unwrap();
        let index = events.len();
        events.push(event.clone());
        let _ = handle.events_tx.send((index, event));
    }

    /// Run `work` as a tracked operation: Started → (work) → Completed/Failed.
    ///
    /// Commands spawned inside `work` are attributed to the action that
    /// dispatched it (M9 history) via the context registered in `dispatch`.
    pub(crate) fn spawn_operation<F>(&self, id: OperationId, handle: Arc<OpHandle>, work: F)
    where
        F: std::future::Future<Output = Result<(), ErrorInfo>> + Send + 'static,
    {
        let engine = self.clone();
        let context = self.inner.op_contexts.lock().unwrap().remove(&id.0);
        tokio::spawn(async move {
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let work = async move {
                match context {
                    Some(context) => crate::history::with_context(context, work).await,
                    None => work.await,
                }
            };
            match work.await {
                Ok(()) => engine.emit_op(&handle, id, OperationEventKind::Completed),
                // A cancelled command comes back as an error. Report it as the
                // cancellation it was — otherwise every cancel reads as
                // "internal error: cancelled".
                Err(_) if handle.cancel.is_cancelled() => {
                    engine.emit_op(&handle, id, OperationEventKind::Cancelled)
                }
                Err(e) => {
                    engine.flush_signature_explanations(&handle, id);
                    engine.emit_op(&handle, id, OperationEventKind::Failed { error: e.to_string() })
                }
            }
        });
    }

    /// Emit the explanations owed for this operation's matched error
    /// signatures (see [`Self::emit_op`]) — called just before a Failed
    /// terminal event, from every path that emits one.
    pub(crate) fn flush_signature_explanations(&self, handle: &OpHandle, id: OperationId) {
        let matched: Vec<_> = handle.signatures.lock().unwrap().iter().take(3).copied().collect();
        for sig in matched {
            self.emit_op(
                handle,
                id,
                OperationEventKind::Output {
                    line: format!("likely cause: {}", sig.explanation),
                    stderr: false,
                },
            );
            self.emit_op(
                handle,
                id,
                OperationEventKind::Output { line: format!("  fix: {}", sig.advice), stderr: false },
            );
        }
    }

    /// Progress/cancellation exerciser; touches no state.
    pub(crate) fn run_fake_operation(&self, id: OperationId, handle: Arc<OpHandle>) {
        let engine = self.clone();
        tokio::spawn(async move {
            engine.emit_op(&handle, id, OperationEventKind::Started);
            for percent in (0..=100u8).step_by(10) {
                tokio::select! {
                    _ = handle.cancel.cancelled() => {
                        engine.emit_op(&handle, id, OperationEventKind::Cancelled);
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(120)) => {
                        engine.emit_op(&handle, id, OperationEventKind::Progress {
                            percent,
                            message: format!("fake startup {percent}%"),
                        });
                    }
                }
            }
            engine.emit_op(&handle, id, OperationEventKind::Completed);
        });
    }

    /// Replays the operation's history from its first event, then follows live
    /// until a terminal event.
    pub fn operation_events(
        &self,
        id: OperationId,
    ) -> Result<BoxStream<'static, OperationEvent>, ErrorInfo> {
        let handle = self
            .inner
            .ops
            .lock()
            .unwrap()
            .get(&id.0)
            .cloned()
            .ok_or(ErrorInfo::NotFound { what: format!("operation {}", id.0) })?;
        let live_rx = handle.events_tx.subscribe();
        let past: Vec<OperationEvent> = handle.events.lock().unwrap().clone();

        let (tx, out_rx) = mpsc::channel::<OperationEvent>(64);
        tokio::spawn(async move {
            let skip = past.len();
            let mut done = false;
            for event in past {
                done = event.kind.is_terminal();
                if tx.send(event).await.is_err() {
                    return;
                }
            }
            if done {
                return;
            }
            let mut live_rx = live_rx;
            loop {
                match live_rx.recv().await {
                    Ok((index, event)) => {
                        if index < skip {
                            continue; // already replayed from history
                        }
                        let terminal = event.kind.is_terminal();
                        if tx.send(event).await.is_err() {
                            return;
                        }
                        if terminal {
                            return;
                        }
                    }
                    // Op event volumes are tiny; treat lag/close as end-of-stream.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        Ok(futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }

    pub fn cancel(&self, id: OperationId) -> Result<(), ErrorInfo> {
        let ops = self.inner.ops.lock().unwrap();
        let handle =
            ops.get(&id.0).ok_or(ErrorInfo::NotFound { what: format!("operation {}", id.0) })?;
        handle.cancel.cancel();
        Ok(())
    }

    /// Run one subprocess streamed into the operation's output, honoring
    /// cancellation (same process-group semantics as lifecycle ops). Shared by
    /// repairs, project commands, app processes, and project creation.
    pub(crate) async fn run_streamed_command(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        argv: &[String],
        cwd: Option<&Path>,
        redactor: &Redactor,
        timeout: Duration,
    ) -> Result<(), ErrorInfo> {
        self.run_streamed_command_env(handle, op, argv, cwd, &[], redactor, timeout).await
    }

    /// [`Self::run_streamed_command`] with an env overlay on the child —
    /// for invocations that need e.g. `SAIL_SKIP_CHECKS=1` without putting
    /// it on the argv.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_streamed_command_env(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        argv: &[String],
        cwd: Option<&Path>,
        env_overlay: &[(String, String)],
        redactor: &Redactor,
        timeout: Duration,
    ) -> Result<(), ErrorInfo> {
        let (line_tx, mut line_rx) = mpsc::channel::<mast_docker::OutputLine>(256);
        let forwarder = {
            let engine = self.clone();
            let handle = handle.clone();
            let redactor = redactor.clone();
            tokio::spawn(async move {
                while let Some(line) = line_rx.recv().await {
                    engine.emit_op(
                        &handle,
                        op,
                        OperationEventKind::Output {
                            line: redactor.redact(&line.line),
                            stderr: line.stderr,
                        },
                    );
                }
            })
        };
        let result = mast_docker::run_streaming(
            argv,
            cwd,
            env_overlay,
            line_tx,
            handle.cancel.clone(),
            timeout,
            Duration::from_secs(8),
        )
        .await;
        let _ = forwarder.await;
        match result {
            Ok(mast_docker::CommandOutcome::Exited(0)) => Ok(()),
            Ok(mast_docker::CommandOutcome::Exited(code)) => Err(ErrorInfo::Internal {
                message: format!(
                    "{} exited with status {code}",
                    argv.first().cloned().unwrap_or_default()
                ),
            }),
            Ok(mast_docker::CommandOutcome::Cancelled) => {
                Err(ErrorInfo::Internal { message: "cancelled".into() })
            }
            Err(e) => Err(ErrorInfo::Internal { message: redactor.redact(&e.to_string()) }),
        }
    }
}
