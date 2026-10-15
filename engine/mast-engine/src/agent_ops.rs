//! The Agents view's actions (ADR-0012): stopping what agent sessions run
//! or left running on this machine, clearing away the stacks worktrees
//! started, and removing or pruning the worktrees themselves.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mast_contract::{ErrorInfo, OperationEventKind, OperationId, ProjectId, Worktree};

use crate::ops::OpHandle;
use crate::{Engine, internal_err};

/// How long a host process tree gets to exit on TERM before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// Stop one host process tree, off the async runtime.
async fn stop_tree(pid: u32, start_ticks: u64) -> Result<usize, ErrorInfo> {
    tokio::task::spawn_blocking(move || crate::agents::stop_host_tree(pid, start_ticks, STOP_GRACE))
        .await
        .map_err(internal_err)?
        .map_err(|message| ErrorInfo::Internal { message: format!("stop failed: {message}") })
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

impl Engine {
    fn say(&self, handle: &OpHandle, op: OperationId, line: impl Into<String>) {
        self.emit_op(handle, op, OperationEventKind::Output { line: line.into(), stderr: false });
    }

    /// Run a command, pass its output on, and fail with its last words.
    async fn run_reported(
        &self,
        handle: &OpHandle,
        op: OperationId,
        argv: &[String],
        dir: Option<&Path>,
    ) -> Result<(), ErrorInfo> {
        let out = mast_docker::run_command(argv, dir, &[], Duration::from_secs(120), 256 * 1024)
            .await
            .map_err(internal_err)?;
        for (text, stderr) in [(&out.stdout, false), (&out.stderr, true)] {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output { line: line.to_string(), stderr },
                );
            }
        }
        if out.success() {
            return Ok(());
        }
        let detail = out.stderr.trim().lines().last().unwrap_or("").to_string();
        Err(ErrorInfo::Internal {
            message: if detail.is_empty() {
                format!("{} exited with status {}", argv[0], out.status)
            } else {
                detail
            },
        })
    }

    /// The project's checkout, the worktree listed at `path`, and the
    /// compose names other stacks may not be taken down by — every
    /// project's own.
    fn worktree_of(
        &self,
        project: &ProjectId,
        path: &str,
    ) -> Result<(PathBuf, Worktree, HashSet<String>), ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let worktree = entry
            .summary
            .worktrees
            .iter()
            .find(|w| w.path == path)
            .cloned()
            .ok_or_else(|| ErrorInfo::InvalidInput {
                message: format!("{path} is not a worktree of {}", entry.summary.name),
            })?;
        let taken = st
            .projects
            .values()
            .filter_map(|e| e.summary.compose_project_name.clone())
            .collect();
        Ok((entry.record.path.clone(), worktree, taken))
    }

    /// Where the project's work happens, for a fresh look at the process
    /// table: its checkout and every worktree.
    fn places_of(&self, project: &ProjectId) -> Vec<crate::agents::Place> {
        let st = self.inner.state.lock().unwrap();
        let Some(entry) = st.projects.get(&project.0) else { return Vec::new() };
        std::iter::once(crate::agents::Place {
            project: project.0.clone(),
            dir: entry.record.path.clone(),
            worktree: None,
        })
        .chain(entry.summary.worktrees.iter().map(|w| crate::agents::Place {
            project: project.0.clone(),
            dir: PathBuf::from(&w.path),
            worktree: Some(PathBuf::from(&w.path)),
        }))
        .collect()
    }

    /// Stop one process tree the Agents view lists for this project — only
    /// one it lists, so this is never a way to signal arbitrary processes.
    pub(crate) async fn stop_host_run(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        pid: u32,
        start_ticks: u64,
    ) -> Result<(), ErrorInfo> {
        let listed = {
            let st = self.inner.state.lock().unwrap();
            st.projects.get(&project.0).and_then(|e| {
                e.summary
                    .host_runs
                    .iter()
                    .find(|r| r.pid == pid && r.start_ticks == start_ticks)
                    .cloned()
            })
        }
        .ok_or_else(|| ErrorInfo::InvalidInput {
            message: format!("process {pid} is not listed for this project any more"),
        })?;
        self.say(
            handle,
            op,
            format!(
                "stopping {} ({} process{}) and everything it started",
                listed.title,
                listed.process_count,
                if listed.process_count == 1 { "" } else { "es" }
            ),
        );
        let result = stop_tree(pid, start_ticks).await;
        self.hint();
        let signalled = result?;
        self.say(handle, op, if signalled == 0 { "it had already exited" } else { "stopped" });
        Ok(())
    }

    /// `docker stop` the containers started from one worktree.
    pub(crate) async fn stop_worktree_containers(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        path: &str,
    ) -> Result<(), ErrorInfo> {
        let (_, worktree, _) = self.worktree_of(project, path)?;
        let running: Vec<&str> =
            worktree.containers.iter().filter(|c| c.running).map(|c| c.id.as_str()).collect();
        if running.is_empty() {
            self.say(handle, op, format!("nothing started from {} is running", worktree.name));
            return Ok(());
        }
        let mut words = vec!["docker", "stop"];
        words.extend(running);
        let result = self.run_reported(handle, op, &argv(&words), None).await;
        self.hint();
        result
    }

    /// Remove the containers started from one worktree.
    pub(crate) async fn remove_worktree_containers(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        path: &str,
    ) -> Result<(), ErrorInfo> {
        let (_, worktree, taken) = self.worktree_of(project, path)?;
        if worktree.containers.is_empty() {
            self.say(handle, op, format!("nothing was started from {}", worktree.name));
            return Ok(());
        }
        let result = self.remove_containers_of(handle, op, &worktree, &taken).await;
        self.hint();
        result
    }

    /// `docker rm -f` a worktree's containers — by id, never by compose
    /// project, since a worktree whose `.env` copied a project's compose
    /// name shares it with that project. A compose project the worktree
    /// started alone also loses its networks; named volumes are kept.
    async fn remove_containers_of(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        worktree: &Worktree,
        taken: &HashSet<String>,
    ) -> Result<(), ErrorInfo> {
        if worktree.containers.is_empty() {
            return Ok(());
        }
        self.say(
            handle,
            op,
            format!(
                "removing the {} container{} started from {} (named volumes are kept)",
                worktree.containers.len(),
                if worktree.containers.len() == 1 { "" } else { "s" },
                worktree.name
            ),
        );
        let mut words = vec!["docker", "rm", "-f"];
        words.extend(worktree.containers.iter().map(|c| c.id.as_str()));
        self.run_reported(handle, op, &argv(&words), None).await?;
        let own: BTreeSet<&str> = worktree
            .containers
            .iter()
            .map(|c| c.compose_project.as_str())
            .filter(|name| !taken.contains(*name))
            .collect();
        for name in own {
            let label = format!("label=com.docker.compose.project={name}");
            let list = argv(&["docker", "network", "ls", "-q", "--filter", &label]);
            let Ok(out) =
                mast_docker::run_command(&list, None, &[], Duration::from_secs(20), 64 * 1024).await
            else {
                continue;
            };
            let networks: Vec<&str> = out.stdout.split_whitespace().collect();
            if out.success() && !networks.is_empty() {
                let mut words = vec!["docker", "network", "rm"];
                words.extend(networks);
                // Best effort: a network something else joined stays.
                let _ = self.run_reported(handle, op, &argv(&words), None).await;
            }
        }
        Ok(())
    }

    /// Remove one worktree: stop what works in it, remove its containers,
    /// then `git worktree remove`. Refused while an agent session works
    /// there, while it is locked, and — unless `force` — while removing it
    /// would lose uncommitted files or commits no branch keeps.
    pub(crate) async fn remove_worktree(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        path: &str,
        force: bool,
    ) -> Result<(), ErrorInfo> {
        let (repo, worktree, taken) = self.worktree_of(project, path)?;
        let name = worktree.name.clone();
        if worktree.locked {
            return Err(ErrorInfo::InvalidInput {
                message: format!(
                    "{name} is locked — unlock it first with git worktree unlock {}",
                    worktree.path
                ),
            });
        }
        if worktree.missing {
            self.say(handle, op, format!("{name}'s directory is already gone; forgetting it"));
            let result = self.prune(handle, op, &repo).await;
            self.hint();
            return result;
        }
        if !force {
            if let Some(changed) = worktree.changed_files.filter(|n| *n > 0) {
                return Err(ErrorInfo::InvalidInput {
                    message: format!(
                        "{name} has {changed} uncommitted change{} that removing it would lose",
                        if changed == 1 { "" } else { "s" }
                    ),
                });
            }
            if worktree.branch.is_none()
                && let Some(commits) = worktree.unmerged_commits.filter(|n| *n > 0)
            {
                return Err(ErrorInfo::InvalidInput {
                    message: format!(
                        "{name} is on a detached HEAD with {commits} commit{} no branch keeps",
                        if commits == 1 { "" } else { "s" }
                    ),
                });
            }
        }

        let places = self.places_of(project);
        let dir = PathBuf::from(&worktree.path);
        let project_id = project.0.clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (runs, sessions) = tokio::task::spawn_blocking(move || {
            crate::agents::scan_host()
                .map(|scan| crate::agents::work_in(&scan, &project_id, &places, &dir, now))
                .unwrap_or_default()
        })
        .await
        .map_err(internal_err)?;
        if let Some(session) = sessions.first() {
            return Err(ErrorInfo::Conflict {
                message: format!(
                    "{} (pid {}) is working in {name} — let it finish or exit that session first",
                    session.agent, session.pid
                ),
            });
        }
        for run in &runs {
            self.say(handle, op, format!("stopping {} ({})", run.title, run.pid));
            stop_tree(run.pid, run.start_ticks).await?;
        }
        self.remove_containers_of(handle, op, &worktree, &taken).await?;

        let mut words = vec!["git", "-C"];
        let repo_text = repo.to_string_lossy().into_owned();
        words.push(&repo_text);
        words.extend(["worktree", "remove"]);
        if force {
            words.push("--force");
        }
        words.push(&worktree.path);
        let result = self.run_reported(handle, op, &argv(&words), None).await;
        self.hint();
        result?;
        self.say(
            handle,
            op,
            match &worktree.branch {
                Some(branch) => format!("removed {name}; its branch {branch} is kept"),
                None => format!("removed {name}"),
            },
        );
        Ok(())
    }

    /// `git worktree prune`: forget every worktree whose directory is gone.
    pub(crate) async fn prune_worktrees(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
    ) -> Result<(), ErrorInfo> {
        let repo = {
            let st = self.inner.state.lock().unwrap();
            st.projects
                .get(&project.0)
                .map(|e| e.record.path.clone())
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?
        };
        let result = self.prune(handle, op, &repo).await;
        self.hint();
        result
    }

    async fn prune(&self, handle: &OpHandle, op: OperationId, repo: &Path) -> Result<(), ErrorInfo> {
        let repo = repo.to_string_lossy();
        self.run_reported(handle, op, &argv(&["git", "-C", &repo, "worktree", "prune", "--verbose"]), None)
            .await
    }
}
