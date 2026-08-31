//! Keeping a user command alive: auto-restart and restart-on-change.
//!
//! A dev server that dies wants relaunching, not reporting — and a queue
//! worker never sees new code until relaunched, which is why
//! `restart_when_changed` exists at all. Both run under ONE operation: the
//! chip stays green across restarts, Stop cancels the whole arrangement, and
//! every child run still lands in the effect history individually.
//!
//! The one thing a supervisor must never do is fight the user: rapid exits
//! stop the loop (a command that cannot stay up needs a person, and five
//! restarts of it are five copies of the same failure), and a plain exit of
//! a watch-only command ends the operation exactly as it would unsupervised.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mast_contract::{ErrorInfo, OperationEventKind, OperationId, ProjectCommand};
use notify::RecursiveMode;
use tokio::sync::mpsc;

use crate::ops::OpHandle;
use crate::{Engine, Redactor};

/// An exit this soon after starting counts toward the crash loop.
const RAPID_EXIT: Duration = Duration::from_secs(30);
/// Rapid exits in a row before the supervisor gives up.
const RAPID_EXITS_TO_STOP: u32 = 5;
/// Editors save in bursts; one restart per burst.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(400);
/// Same "a week ≈ unbounded" budget as unsupervised commands, per child run.
const RUN_BUDGET: Duration = Duration::from_secs(7 * 24 * 3600);

impl Engine {
    /// Run one user command under supervision. Returns like
    /// [`Engine::run_streamed_command`] — the caller cannot tell the two
    /// apart, which is the point.
    pub(crate) async fn supervise_command(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        cmd: &ProjectCommand,
        argv: &[String],
        run_dir: &Path,
        redactor: &Redactor,
    ) -> Result<(), ErrorInfo> {
        // The watcher callback runs on notify's thread; an unbounded channel
        // gives it a sync, never-blocking send. Dropping the sender when no
        // watch is configured permanently disables the select branches below.
        let (change_tx, mut change_rx) = mpsc::unbounded_channel::<String>();
        let _watcher = if cmd.restart_when_changed.is_empty() {
            drop(change_tx);
            None
        } else {
            match watch(run_dir, &cmd.restart_when_changed, change_tx) {
                Ok(watcher) => Some(watcher),
                Err(message) => {
                    // A broken watch degrades to plain auto-restart; silence
                    // here would look like the feature quietly not working.
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: format!("cannot watch for file changes: {message}"),
                            stderr: true,
                        },
                    );
                    None
                }
            }
        };

        let mut rapid_exits: u32 = 0;
        loop {
            let child_cancel = handle.cancel.child_token();
            let started = Instant::now();
            let run = self.stream_child(
                handle,
                op,
                argv,
                Some(run_dir),
                &[],
                redactor,
                RUN_BUDGET,
                child_cancel.clone(),
            );
            tokio::pin!(run);
            enum Next {
                Exited(Result<mast_docker::CommandOutcome, ErrorInfo>),
                Changed(String),
            }
            let next = tokio::select! {
                result = &mut run => Next::Exited(result),
                Some(path) = change_rx.recv() => {
                    child_cancel.cancel();
                    let _ = (&mut run).await;
                    Next::Changed(path)
                }
            };
            match next {
                Next::Changed(path) => {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: format!("↻ {path} changed — restarting"),
                            stderr: false,
                        },
                    );
                    rapid_exits = 0;
                    while change_rx.try_recv().is_ok() {} // one restart per burst
                }
                // A spawn failure (missing binary, bad cwd) is not something a
                // restart can fix; five copies of it would just say so slower.
                Next::Exited(Err(e)) => return Err(e),
                Next::Exited(Ok(mast_docker::CommandOutcome::Cancelled)) => {
                    return Err(ErrorInfo::Internal { message: "cancelled".into() });
                }
                Next::Exited(Ok(mast_docker::CommandOutcome::Exited(status))) => {
                    if !cmd.auto_restart {
                        // Watch-only: the command ended on its own terms,
                        // exactly as it would have unsupervised.
                        return if status == 0 {
                            Ok(())
                        } else {
                            Err(ErrorInfo::Internal {
                                message: format!(
                                    "{} exited with status {status}",
                                    argv.first().cloned().unwrap_or_default()
                                ),
                            })
                        };
                    }
                    let Some(delay) = on_unexpected_exit(&mut rapid_exits, started.elapsed())
                    else {
                        return Err(ErrorInfo::Internal {
                            message: if status == 0 {
                                format!(
                                    "exited cleanly {RAPID_EXITS_TO_STOP} times within seconds \
                                     of starting — auto-restart stopped: a command that \
                                     finishes this fast is a one-shot, not a server, so turn \
                                     auto-restart off for it"
                                )
                            } else {
                                format!(
                                    "crash loop: exited {RAPID_EXITS_TO_STOP} times within \
                                     seconds of starting (last status {status}) — auto-restart \
                                     stopped, the output above is the same failure \
                                     {RAPID_EXITS_TO_STOP} times over"
                                )
                            },
                        });
                    };
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: format!(
                                "↻ exited with status {status} — restarting in {:.1}s",
                                delay.as_secs_f32()
                            ),
                            stderr: status != 0,
                        },
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        // Backoff must not outlive a Stop…
                        _ = handle.cancel.cancelled() => {
                            return Err(ErrorInfo::Internal { message: "cancelled".into() });
                        }
                        // …and a file change during backoff may BE the fix.
                        Some(path) = change_rx.recv() => {
                            self.emit_op(
                                handle,
                                op,
                                OperationEventKind::Output {
                                    line: format!("↻ {path} changed — restarting now"),
                                    stderr: false,
                                },
                            );
                            rapid_exits = 0;
                            while change_rx.try_recv().is_ok() {}
                        }
                    }
                }
            }
        }
    }
}

/// Half a second for a one-off death, then 1s/2s/4s/8s as exits speed up —
/// enough to keep a flapping command from burning a core, short enough that
/// recovery still feels immediate.
fn restart_delay(rapid_exits: u32) -> Duration {
    match rapid_exits {
        0 => Duration::from_millis(500),
        n => Duration::from_secs(1 << (n - 1).min(3)),
    }
}

/// Track one exit the user did not ask for: `Some(delay)` restarts after it,
/// `None` declares the crash loop. A run that stayed up a while resets the
/// count — a server dying nightly is not the same animal as one dying on
/// arrival.
fn on_unexpected_exit(rapid_exits: &mut u32, uptime: Duration) -> Option<Duration> {
    if uptime < RAPID_EXIT {
        *rapid_exits += 1;
    } else {
        *rapid_exits = 0;
    }
    if *rapid_exits >= RAPID_EXITS_TO_STOP { None } else { Some(restart_delay(*rapid_exits)) }
}

/// Watch the pattern roots and forward the first matching relative path per
/// debounce window. The returned debouncer must stay alive for the watch to.
fn watch(
    run_dir: &Path,
    patterns: &[String],
    changes: mpsc::UnboundedSender<String>,
) -> Result<
    notify_debouncer_full::Debouncer<notify::RecommendedWatcher, notify_debouncer_full::RecommendedCache>,
    String,
> {
    let compiled: Vec<glob::Pattern> = patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    if compiled.is_empty() {
        return Err(format!("no valid glob among {patterns:?}"));
    }
    let dir = run_dir.to_path_buf();
    let mut debouncer = notify_debouncer_full::new_debouncer(
        WATCH_DEBOUNCE,
        None,
        move |result: notify_debouncer_full::DebounceEventResult| {
            let Ok(events) = result else { return };
            for event in events {
                for path in &event.paths {
                    let Ok(rel) = path.strip_prefix(&dir) else { continue };
                    if rel.components().any(|c| c.as_os_str() == ".git") {
                        continue;
                    }
                    if compiled.iter().any(|p| p.matches_path(rel)) {
                        let _ = changes.send(rel.to_string_lossy().into_owned());
                        return; // one signal per burst; the loop drains stragglers
                    }
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;
    let roots = watch_roots(run_dir, patterns);
    let mut watching = 0;
    for root in &roots {
        // A root that is not there yet (pattern for a directory the project
        // does not have) is skipped, not fatal — the other roots still work.
        if debouncer.watch(root, RecursiveMode::Recursive).is_ok() {
            watching += 1;
        }
    }
    if watching == 0 {
        return Err(format!(
            "none of the watched paths exist ({})",
            roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(debouncer)
}

/// The literal directory prefix of each pattern, so watches land on `app/`
/// and `config/` rather than the whole tree — a recursive watch at the
/// project root would register vendor/ and node_modules/, tens of thousands
/// of inotify watches for files no pattern can match.
fn watch_roots(run_dir: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        let mut prefix = PathBuf::new();
        for component in Path::new(pattern).components() {
            if component.as_os_str().to_string_lossy().contains(['*', '?', '[', '{']) {
                break;
            }
            prefix.push(component);
        }
        let candidate = if prefix.as_os_str().is_empty() {
            run_dir.to_path_buf()
        } else {
            run_dir.join(&prefix)
        };
        // A fully-literal pattern names a file; watching the file itself
        // misses editors that replace-and-rename, so watch its directory.
        let root = if candidate.is_dir() {
            candidate
        } else {
            candidate.parent().map(Path::to_path_buf).unwrap_or_else(|| run_dir.to_path_buf())
        };
        if !roots.iter().any(|r| root.starts_with(r)) {
            roots.retain(|r| !r.starts_with(&root));
            roots.push(root);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_roots_land_on_pattern_prefixes_not_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/Jobs")).unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        let roots = watch_roots(
            dir.path(),
            &["app/**".into(), "app/Jobs/*.php".into(), "config/queue.php".into()],
        );
        // app/Jobs collapses into app; the literal file pattern watches its
        // parent directory.
        assert_eq!(roots, vec![dir.path().join("app"), dir.path().join("config")]);
    }

    #[test]
    fn a_bare_glob_falls_back_to_the_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(watch_roots(dir.path(), &["*.env".into()]), vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn backoff_starts_gentle_and_caps() {
        assert_eq!(restart_delay(0), Duration::from_millis(500));
        assert_eq!(restart_delay(1), Duration::from_secs(1));
        assert_eq!(restart_delay(3), Duration::from_secs(4));
        assert_eq!(restart_delay(4), Duration::from_secs(8));
        assert_eq!(restart_delay(40), Duration::from_secs(8));
    }

    #[test]
    fn rapid_exits_stop_the_loop_and_a_long_run_forgives_them() {
        let mut rapid = 0;
        let instant = Duration::from_millis(10);
        for _ in 0..RAPID_EXITS_TO_STOP - 1 {
            assert!(on_unexpected_exit(&mut rapid, instant).is_some());
        }
        // One good long run wipes the slate…
        assert!(on_unexpected_exit(&mut rapid, RAPID_EXIT * 2).is_some());
        assert_eq!(rapid, 0);
        // …so it takes the full streak again before the loop gives up.
        for _ in 0..RAPID_EXITS_TO_STOP - 1 {
            assert!(on_unexpected_exit(&mut rapid, instant).is_some());
        }
        assert!(on_unexpected_exit(&mut rapid, instant).is_none());
    }
}
