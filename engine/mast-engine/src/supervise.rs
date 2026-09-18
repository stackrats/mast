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

use mast_contract::{ErrorInfo, OperationEventKind, OperationId, ProjectCommand, ProjectId};
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

/// Holding this registration prevents duplicate daemons and lets Stop end
/// the entire file-change supervisor rather than just its current child.
pub(crate) struct ProcessRun {
    engine: Engine,
    key: (String, String),
    op: OperationId,
}

impl ProcessRun {
    pub(crate) fn register(
        engine: &Engine,
        project: &ProjectId,
        process: &str,
        op: OperationId,
    ) -> Result<Self, ErrorInfo> {
        let key = (project.0.clone(), process.to_string());
        let mut runs = engine.inner.process_runs.lock().unwrap();
        if runs.contains_key(&key) {
            return Err(ErrorInfo::InvalidInput {
                message: format!("{process} is already running or stopping"),
            });
        }
        runs.insert(key.clone(), op);
        Ok(Self {
            engine: engine.clone(),
            key,
            op,
        })
    }

    pub(crate) fn stopping(
        engine: &Engine,
        project: &ProjectId,
        process: &str,
        op: OperationId,
    ) -> (Self, Option<OperationId>) {
        let key = (project.0.clone(), process.to_string());
        let active = engine
            .inner
            .process_runs
            .lock()
            .unwrap()
            .insert(key.clone(), op);
        (
            Self {
                engine: engine.clone(),
                key,
                op,
            },
            active,
        )
    }
}

impl Drop for ProcessRun {
    fn drop(&mut self) {
        let mut runs = self.engine.inner.process_runs.lock().unwrap();
        if runs.get(&self.key) == Some(&self.op) {
            runs.remove(&self.key);
        }
    }
}

impl Engine {
    /// Artisan daemons live inside Docker: explicitly stop them there after
    /// disconnecting their host client, before a replacement binds its ports.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn supervise_process(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        def: &mast_laravel::processes::ProcessDef,
        argv: &[String],
        stop_argv: &[String],
        dir: &Path,
        redactor: &Redactor,
    ) -> Result<(), ErrorInfo> {
        let patterns = mast_laravel::processes::RESTART_PATTERNS
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>();
        let (change_tx, mut change_rx) = mpsc::unbounded_channel();
        let mut watcher =
            watch(dir, &patterns, change_tx.clone()).map_err(|message| ErrorInfo::Internal {
                message: format!("cannot watch application code: {message}"),
            })?;
        loop {
            if handle.cancel.is_cancelled() {
                return Err(ErrorInfo::Internal {
                    message: "cancelled".into(),
                });
            }
            // Control both sides of shutdown: cancelling Docker exec alone
            // leaves the in-container daemon running.
            let child_cancel = tokio_util::sync::CancellationToken::new();
            let run = self.stream_child(
                handle,
                op,
                argv,
                Some(dir),
                &[],
                redactor,
                RUN_BUDGET,
                child_cancel.clone(),
            );
            tokio::pin!(run);
            let changed = tokio::select! {
                biased;
                // Poll the child first so cleanup can never accidentally
                // start an unpolled run after the stop command completed.
                outcome = &mut run => {
                    return match outcome? {
                        mast_docker::CommandOutcome::Exited(0) => Ok(()),
                        mast_docker::CommandOutcome::Exited(status) => Err(ErrorInfo::Internal {
                            message: format!("{} exited with status {status}", def.title),
                        }),
                        mast_docker::CommandOutcome::Cancelled => Err(ErrorInfo::Internal { message: "cancelled".into() }),
                    };
                },
                _ = handle.cancel.cancelled() => None,
                Some(path) = change_rx.recv() => Some(path),
            };
            child_cancel.cancel();
            let _ = (&mut run).await;
            let stopped = crate::project_ops::run_process_stop(stop_argv, dir).await;
            if let Err(error) = stopped {
                handle
                    .cancel_failed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: redactor.redact(&error.to_string()),
                        stderr: true,
                    },
                );
                return Err(error);
            }
            let Some(path) = changed.filter(|_| !handle.cancel.is_cancelled()) else {
                return Err(ErrorInfo::Internal {
                    message: "cancelled".into(),
                });
            };
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!("↻ {path} changed — restarting {}", def.title),
                    stderr: false,
                },
            );
            while change_rx.try_recv().is_ok() {}
            // A newly-created directory may now need a recursive watch.
            drop(watcher);
            watcher = watch(dir, &patterns, change_tx.clone())
                .map_err(|message| ErrorInfo::Internal { message })?;
        }
    }

    /// Run one user command under supervision. Returns like
    /// [`Engine::run_streamed_command`] — the caller cannot tell the two
    /// apart, which is the point.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn supervise_command(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        cmd: &ProjectCommand,
        argv: &[String],
        run_dir: &Path,
        redactor: &Redactor,
        stop_argv: Option<&[String]>,
    ) -> Result<(), ErrorInfo> {
        // The watcher callback runs on notify's thread; an unbounded channel
        // gives it a sync, never-blocking send. No watcher means no events.
        let (change_tx, mut change_rx) = mpsc::unbounded_channel::<String>();
        let mut watcher = if cmd.restart_when_changed.is_empty() {
            None
        } else {
            match watch(run_dir, &cmd.restart_when_changed, change_tx.clone()) {
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
            if handle.cancel.is_cancelled() {
                return Err(ErrorInfo::Internal {
                    message: "cancelled".into(),
                });
            }
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
                biased;
                result = &mut run => Next::Exited(result),
                Some(path) = change_rx.recv() => {
                    child_cancel.cancel();
                    let _ = (&mut run).await;
                    Next::Changed(path)
                }
            };
            if let Some(stop_argv) = stop_argv
                && let Err(error) = crate::project_ops::run_process_stop(stop_argv, run_dir).await
            {
                handle
                    .cancel_failed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: redactor.redact(&error.to_string()),
                        stderr: true,
                    },
                );
                return Err(error);
            }
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
                    drop(watcher.take());
                    watcher = Some(
                        watch(run_dir, &cmd.restart_when_changed, change_tx.clone())
                            .map_err(|message| ErrorInfo::Internal { message })?,
                    );
                }
                // A spawn failure (missing binary, bad cwd) is not something a
                // restart can fix; five copies of it would just say so slower.
                Next::Exited(Err(e)) => return Err(e),
                Next::Exited(Ok(mast_docker::CommandOutcome::Cancelled)) => {
                    return Err(ErrorInfo::Internal {
                        message: "cancelled".into(),
                    });
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
    if *rapid_exits >= RAPID_EXITS_TO_STOP {
        None
    } else {
        Some(restart_delay(*rapid_exits))
    }
}

/// Watch the pattern roots and forward the first matching relative path per
/// debounce window. The returned debouncer must stay alive for the watch to.
fn watch(
    run_dir: &Path,
    patterns: &[String],
    changes: mpsc::UnboundedSender<String>,
) -> Result<
    notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    String,
> {
    let compiled: Vec<glob::Pattern> = patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    if compiled.is_empty() {
        return Err(format!("no valid glob among {patterns:?}"));
    }
    // FSEvents reports canonical paths, including /private/var for macOS
    // temporary directories. Register and compare the same root so symlink
    // aliases cannot make every change fall outside the watched directory.
    let dir =
        mast_compose::strip_verbatim(run_dir.canonicalize().map_err(|error| error.to_string())?);
    let roots = watch_roots(&dir, patterns);
    let mut debouncer = notify_debouncer_full::new_debouncer(
        WATCH_DEBOUNCE,
        None,
        move |result: notify_debouncer_full::DebounceEventResult| {
            let Ok(events) = result else { return };
            for event in events {
                // Daemons read their own code during startup. Access events
                // must not make that read look like another code change.
                if event.kind.is_access() {
                    continue;
                }
                for path in &event.paths {
                    let path = mast_compose::strip_verbatim(path.clone());
                    let Ok(rel) = path.strip_prefix(&dir) else {
                        continue;
                    };
                    if rel.as_os_str().is_empty() {
                        continue;
                    }
                    if rel.components().any(|c| c.as_os_str() == ".git") {
                        continue;
                    }
                    if compiled
                        .iter()
                        .any(|p| p.matches_path(rel) || Path::new(p.as_str()).starts_with(rel))
                    {
                        let _ = changes.send(rel.to_string_lossy().into_owned());
                        return; // one signal per burst; the loop drains stragglers
                    }
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;
    let mut watching = 0;
    for (root, mode) in &roots {
        // A root that is not there yet (pattern for a directory the project
        // does not have) is skipped, not fatal — the other roots still work.
        if debouncer.watch(root, *mode).is_ok() {
            watching += 1;
        }
    }
    if watching == 0 {
        return Err(format!(
            "none of the watched paths exist ({})",
            roots
                .iter()
                .map(|(r, _)| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(debouncer)
}

/// The literal directory prefix of each pattern, so watches land on `app/`
/// and `config/` rather than the whole tree — a recursive watch at the
/// project root would register vendor/ and node_modules/, tens of thousands
/// of inotify watches for files no pattern can match.
fn watch_roots(run_dir: &Path, patterns: &[String]) -> Vec<(PathBuf, RecursiveMode)> {
    let mut roots: Vec<(PathBuf, RecursiveMode)> = Vec::new();
    for pattern in patterns {
        let mut prefix = PathBuf::new();
        let mut has_glob = false;
        for component in Path::new(pattern).components() {
            if component
                .as_os_str()
                .to_string_lossy()
                .contains(['*', '?', '[', '{'])
            {
                has_glob = true;
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
        let (mut root, mut mode) = if has_glob && candidate.is_dir() {
            (candidate, RecursiveMode::Recursive)
        } else {
            (
                candidate
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| run_dir.to_path_buf()),
                RecursiveMode::NonRecursive,
            )
        };
        // Watch the nearest existing parent for missing directory creation;
        // the supervisor rebuilds roots before the next child starts.
        while !root.is_dir() && root.starts_with(run_dir) && root != run_dir {
            root.pop();
            mode = RecursiveMode::NonRecursive;
        }
        if !roots.iter().any(|(r, m)| {
            (r == &root && *m == mode) || (*m == RecursiveMode::Recursive && root.starts_with(r))
        }) {
            roots.retain(|(r, _)| mode != RecursiveMode::Recursive || !r.starts_with(&root));
            roots.push((root, mode));
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
            &[
                "app/**".into(),
                "app/Jobs/*.php".into(),
                "config/queue.php".into(),
            ],
        );
        // app/Jobs collapses into app; the literal file pattern watches its
        // parent directory.
        assert_eq!(
            roots,
            vec![
                (dir.path().join("app"), RecursiveMode::Recursive),
                (dir.path().join("config"), RecursiveMode::NonRecursive)
            ]
        );
    }

    #[test]
    fn a_bare_glob_falls_back_to_the_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            watch_roots(dir.path(), &["*.env".into()]),
            vec![(dir.path().to_path_buf(), RecursiveMode::Recursive)]
        );
    }

    #[test]
    fn root_config_files_do_not_expand_code_watches_into_dependency_trees() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        let roots = watch_roots(
            dir.path(),
            &["app/**".into(), ".env".into(), "config/**".into()],
        );
        assert_eq!(
            roots,
            vec![
                (dir.path().join("app"), RecursiveMode::Recursive),
                (dir.path().to_path_buf(), RecursiveMode::NonRecursive),
            ]
        );
    }

    #[tokio::test]
    async fn application_file_changes_reach_the_watcher_on_every_platform() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        let (changes, mut received) = mpsc::unbounded_channel();
        let _watcher = watch(dir.path(), &["app/**".into()], changes).unwrap();
        // The callback forwards one matching path per batch. FSEvents may
        // report the containing directory first; either path must restart
        // the daemon, so do not require the batch to select the filename.
        let mut observed = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                std::fs::write(dir.path().join("app/Job.php"), "<?php // changed").unwrap();
                tokio::select! {
                    changed = received.recv() => {
                        let changed = changed.expect("watch remains active");
                        let matches = Path::new("app/Job.php").starts_with(&changed);
                        observed.push(changed);
                        if matches {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(WATCH_DEBOUNCE * 2) => {}
                }
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "file writes must notify the watcher; received {observed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn code_changes_are_received_when_watching_a_symlink_alias() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("app")).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let (changes, mut received) = mpsc::unbounded_channel();
        let _watcher = watch(&alias, &["app/**".into()], changes).unwrap();
        let mut observed = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                std::fs::write(source.join("app/Job.php"), "<?php // changed").unwrap();
                tokio::select! {
                    changed = received.recv() => {
                        let changed = changed.expect("watch remains active");
                        // A directory notification is a valid code change,
                        // just as in the platform-neutral regression above.
                        let matches = Path::new("app/Job.php").starts_with(&changed);
                        observed.push(changed);
                        if matches {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(WATCH_DEBOUNCE * 2) => {}
                }
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "canonical changes must notify the alias watch; received {observed:?}"
        );
    }

    #[cfg(unix)]
    fn test_engine(dir: &Path) -> Engine {
        Engine::new(
            crate::EngineConfig::default(),
            crate::EngineDeps {
                connector: Arc::new(crate::RealConnector),
                store: mast_project::MetadataStore::open(dir.join("meta")).unwrap(),
                process_env: Default::default(),
                runner: Arc::new(crate::RealLifecycleRunner),
                ownership: crate::acquire_ownership(Some(dir.join("lock"))),
            },
        )
    }

    #[cfg(unix)]
    async fn wait_for_lives(dir: &Path, count: usize) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let log = std::fs::read_to_string(dir.join("lifecycle")).unwrap_or_default();
                if log.lines().filter(|line| *line == "start").count() >= count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("daemon starts within deadline");
    }

    /// Exercise real children and filesystem notifications, with a stand-in
    /// for Docker's in-container stop. Every built-in must stop before its
    /// next start; reads/generated files must not trigger a restart loop.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn all_laravel_daemons_restart_on_code_and_env_changes_and_stop_cleanly() {
        for def in mast_laravel::processes::PROCESSES {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("bootstrap/cache")).unwrap();
            let engine = test_engine(dir.path());
            let (op, handle) = engine.new_operation();
            let cancel = handle.cancel.clone();
            let path = dir.path().to_path_buf();
            let task = tokio::spawn(async move {
                let argv =
                    ["sh", "-c", "echo start >> lifecycle; exec sleep 300"].map(String::from);
                let stop = ["sh", "-c", "echo stop >> lifecycle"].map(String::from);
                engine
                    .supervise_process(&handle, op, def, &argv, &stop, &path, &Redactor::default())
                    .await
            });
            wait_for_lives(dir.path(), 1).await;
            // An initially absent source directory becomes watched too.
            std::fs::create_dir_all(dir.path().join("app/Jobs")).unwrap();
            std::fs::write(dir.path().join("app/Jobs/Job.php"), "<?php // v1").unwrap();
            wait_for_lives(dir.path(), 2).await;
            std::fs::write(dir.path().join("app/Jobs/Job.php"), "<?php // v2").unwrap();
            wait_for_lives(dir.path(), 3).await;
            std::fs::write(dir.path().join(".env"), "QUEUE_CONNECTION=redis\n").unwrap();
            wait_for_lives(dir.path(), 4).await;
            let _ = std::fs::read(dir.path().join("app/Jobs/Job.php")).unwrap();
            std::fs::write(
                dir.path().join("bootstrap/cache/config.php"),
                "<?php return [];",
            )
            .unwrap();
            tokio::time::sleep(WATCH_DEBOUNCE * 3).await;
            cancel.cancel();
            assert!(
                tokio::time::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("lifecycle")).unwrap(),
                "start\nstop\nstart\nstop\nstart\nstop\nstart\nstop\n",
                "{}",
                def.id
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_container_stop_never_launches_a_duplicate_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let engine = test_engine(dir.path());
        let (op, handle) = engine.new_operation();
        let path = dir.path().to_path_buf();
        let task = tokio::spawn(async move {
            let argv = ["sh", "-c", "echo start >> lifecycle; exec sleep 300"].map(String::from);
            let stop = ["sh", "-c", "echo still running >&2; exit 1"].map(String::from);
            engine
                .supervise_process(
                    &handle,
                    op,
                    &mast_laravel::processes::PROCESSES[0],
                    &argv,
                    &stop,
                    &path,
                    &Redactor::default(),
                )
                .await
        });
        wait_for_lives(dir.path(), 1).await;
        std::fs::write(dir.path().join(".env"), "APP_ENV=local\n").unwrap();
        let error = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("still running"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lifecycle")).unwrap(),
            "start\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn failed_remote_cleanup_stays_failed_when_the_operation_was_cancelled() {
        use futures::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let engine = test_engine(dir.path());
        let (op, handle) = engine.new_operation();
        let path = dir.path().to_path_buf();
        let worker = engine.clone();
        engine.spawn_operation(op, handle.clone(), async move {
            let argv = ["sh", "-c", "echo start >> lifecycle; exec sleep 300"].map(String::from);
            let stop = ["sh", "-c", "echo cannot stop >&2; exit 1"].map(String::from);
            worker
                .supervise_process(
                    &handle,
                    op,
                    &mast_laravel::processes::PROCESSES[0],
                    &argv,
                    &stop,
                    &path,
                    &Redactor::default(),
                )
                .await
        });
        wait_for_lives(dir.path(), 1).await;
        engine.cancel(op).unwrap();
        let mut events = engine.operation_events(op).unwrap();
        let terminal = tokio::time::timeout(Duration::from_secs(8), async {
            while let Some(event) = events.next().await {
                if event.kind.is_terminal() {
                    return event.kind;
                }
            }
            panic!("no terminal event")
        })
        .await
        .unwrap();
        assert!(
            matches!(terminal, OperationEventKind::Failed { ref error } if error.contains("cannot stop")),
            "{terminal:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stopping_reserves_the_process_until_cleanup_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let engine = test_engine(dir.path());
        let project = ProjectId("project".into());
        let running = ProcessRun::register(&engine, &project, "queue", OperationId(1)).unwrap();
        assert!(ProcessRun::register(&engine, &project, "queue", OperationId(2)).is_err());
        let (stopping, active) = ProcessRun::stopping(&engine, &project, "queue", OperationId(3));
        assert_eq!(active, Some(OperationId(1)));
        drop(running);
        assert!(ProcessRun::register(&engine, &project, "queue", OperationId(4)).is_err());
        drop(stopping);
        assert!(ProcessRun::register(&engine, &project, "queue", OperationId(5)).is_ok());
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
