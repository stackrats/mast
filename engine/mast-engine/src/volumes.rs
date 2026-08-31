//! Data snapshots: point-in-time copies of a service's named volumes.
//!
//! The problem is `sail down -v` and its relatives — one flag between a
//! developer and a database they spent a week seeding. The insurance is a
//! copy that is cheap to take and boring to store: each snapshot is a set of
//! labeled docker volumes (`mast-snap-<group>-<key>`), copied cold with the
//! service's container stopped and restarted afterwards. Docker itself is
//! the store — snapshots survive an app-data wipe, need no database of
//! their own, and `docker volume ls` can always audit what Mast made.
//!
//! Restore is the destructive half (wipe the live volume, copy back), so it
//! carries the same posture as high-risk repairs: the client confirms, and
//! offers a fresh snapshot of the current data first.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mast_contract::{
    ContainerState, ErrorInfo, OperationEventKind, OperationId, PatchEvent, ProjectId,
    VolumeSnapshot,
};

use crate::ops::OpHandle;
use crate::{Engine, Redactor};

const L_SNAPSHOT: &str = "mast.snapshot";
const L_PROJECT: &str = "mast.project";
const L_SERVICE: &str = "mast.service";
const L_GROUP: &str = "mast.group";
const L_AT: &str = "mast.at";
/// Compose source name of the copied volume (`sail-mysql`).
const L_SOURCE_KEY: &str = "mast.source-key";
/// Real docker volume name the copy came from — where restore puts it back.
const L_SOURCE: &str = "mast.source";
/// Compose project name, so a restore can recreate a deleted original with
/// the labels compose requires before it will adopt the volume.
const L_COMPOSE_PROJECT: &str = "mast.compose-project";

const LS_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_START_TIMEOUT: Duration = Duration::from_secs(120);
/// A copy streams a whole database; the budget is the idle-tolerant kind.
const COPY_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The throwaway image that carries `cp` and `find`. Pinned to a plain tag —
/// the copy needs POSIX tools, not reproducibility.
const HELPER_IMAGE: &str = "alpine:latest";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Group ids come from the nanosecond clock: unique enough for a per-machine
/// store where snapshot ops on one project serialize behind the op lock.
fn new_group_id() -> String {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}")
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `docker volume ls --format "{{.Name}}\t{{.Labels}}"` rows into
/// (name, labels). Label values Mast writes contain no commas, so the
/// comma-joined form splits cleanly.
fn parse_volume_rows(stdout: &str) -> Vec<(String, BTreeMap<String, String>)> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, labels) = line.split_once('\t')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let labels = labels
                .split(',')
                .filter_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    Some((k.trim().to_string(), v.trim().to_string()))
                })
                .collect();
            Some((name.to_string(), labels))
        })
        .collect()
}

/// Rows → snapshots, one per group, volumes sorted, newest first.
fn group_snapshots(rows: Vec<(String, BTreeMap<String, String>)>) -> Vec<VolumeSnapshot> {
    let mut by_group: BTreeMap<String, VolumeSnapshot> = BTreeMap::new();
    for (_, labels) in rows {
        let Some(group) = labels.get(L_GROUP).cloned() else { continue };
        let entry = by_group.entry(group.clone()).or_insert_with(|| VolumeSnapshot {
            group,
            project: ProjectId(labels.get(L_PROJECT).cloned().unwrap_or_default()),
            service: labels.get(L_SERVICE).cloned().unwrap_or_default(),
            at_unix_ms: labels.get(L_AT).and_then(|a| a.parse().ok()).unwrap_or(0),
            volumes: Vec::new(),
        });
        if let Some(key) = labels.get(L_SOURCE_KEY) {
            entry.volumes.push(key.clone());
        }
    }
    let mut out: Vec<VolumeSnapshot> = by_group.into_values().collect();
    for snapshot in &mut out {
        snapshot.volumes.sort();
    }
    out.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.at_unix_ms));
    out
}

impl Engine {
    /// Stored data snapshots for a project, newest first — read straight off
    /// docker's volume labels, never a side database.
    pub async fn volume_snapshots(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<VolumeSnapshot>, ErrorInfo> {
        let out = mast_docker::run_command(
            &argv(&[
                "docker",
                "volume",
                "ls",
                "--format",
                "{{.Name}}\t{{.Labels}}",
                "--filter",
                &format!("label={L_SNAPSHOT}=1"),
                "--filter",
                &format!("label={L_PROJECT}={}", project.0),
            ]),
            None,
            &[],
            LS_TIMEOUT,
            1024 * 1024,
        )
        .await
        .map_err(|e| ErrorInfo::Internal { message: e.to_string() })?;
        if !out.success() {
            return Err(ErrorInfo::Internal {
                message: format!("docker volume ls failed: {}", out.stderr.trim()),
            });
        }
        Ok(group_snapshots(parse_volume_rows(&out.stdout)))
    }

    /// Volume rows matching one snapshot group (any project — the caller
    /// checks ownership where it matters).
    async fn snapshot_rows(
        &self,
        group: &str,
    ) -> Result<Vec<(String, BTreeMap<String, String>)>, ErrorInfo> {
        let out = mast_docker::run_command(
            &argv(&[
                "docker",
                "volume",
                "ls",
                "--format",
                "{{.Name}}\t{{.Labels}}",
                "--filter",
                &format!("label={L_SNAPSHOT}=1"),
                "--filter",
                &format!("label={L_GROUP}={group}"),
            ]),
            None,
            &[],
            LS_TIMEOUT,
            1024 * 1024,
        )
        .await
        .map_err(|e| ErrorInfo::Internal { message: e.to_string() })?;
        if !out.success() {
            return Err(ErrorInfo::Internal {
                message: format!("docker volume ls failed: {}", out.stderr.trim()),
            });
        }
        Ok(parse_volume_rows(&out.stdout))
    }

    /// The real docker volume behind one compose source name, via the labels
    /// compose stamps on every volume it creates.
    async fn compose_volume_name(
        &self,
        compose_project: &str,
        key: &str,
    ) -> Result<Option<String>, ErrorInfo> {
        let out = mast_docker::run_command(
            &argv(&[
                "docker",
                "volume",
                "ls",
                "--format",
                "{{.Name}}",
                "--filter",
                &format!("label=com.docker.compose.project={compose_project}"),
                "--filter",
                &format!("label=com.docker.compose.volume={key}"),
            ]),
            None,
            &[],
            LS_TIMEOUT,
            64 * 1024,
        )
        .await
        .map_err(|e| ErrorInfo::Internal { message: e.to_string() })?;
        if !out.success() {
            return Err(ErrorInfo::Internal {
                message: format!("docker volume ls failed: {}", out.stderr.trim()),
            });
        }
        Ok(out.stdout.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from))
    }

    pub(crate) fn dispatch_volume_snapshot(
        &self,
        project: ProjectId,
        service: String,
    ) -> Result<OperationId, ErrorInfo> {
        let (compose_project, keys, container, was_running, redactor, project_name) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            let svc = entry
                .summary
                .services
                .iter()
                .find(|s| s.name == service)
                .ok_or(ErrorInfo::NotFound { what: format!("service {service}") })?;
            if svc.data_volumes.is_empty() {
                return Err(ErrorInfo::InvalidInput {
                    message: format!("{service} mounts no named volumes — nothing to snapshot"),
                });
            }
            let compose_project =
                entry.invocation.as_ref().map(|i| i.project_name.clone()).ok_or_else(|| {
                    ErrorInfo::InvalidInput { message: "project not resolved yet".into() }
                })?;
            (
                compose_project,
                svc.data_volumes.clone(),
                svc.container_id.clone(),
                svc.state == Some(ContainerState::Running),
                entry.redactor.clone(),
                entry.summary.name.clone(),
            )
        };
        self.spawn_locked_volume_op(
            project.clone(),
            format!("snapshot {service} data"),
            format!("Snapshot {project_name} ({service}) data"),
            move |engine, handle, id| async move {
                engine
                    .run_volume_snapshot(
                        &handle,
                        id,
                        &project,
                        &compose_project,
                        &service,
                        &keys,
                        container,
                        was_running,
                        &redactor,
                    )
                    .await
            },
        )
    }

    pub(crate) fn dispatch_volume_restore(
        &self,
        project: ProjectId,
        group: String,
    ) -> Result<OperationId, ErrorInfo> {
        let (redactor, project_name) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            (entry.redactor.clone(), entry.summary.name.clone())
        };
        self.spawn_locked_volume_op(
            project.clone(),
            format!("restore data snapshot {group}"),
            format!("Restore {project_name} data snapshot"),
            move |engine, handle, id| async move {
                engine.run_volume_restore(&handle, id, &project, &group, &redactor).await
            },
        )
    }

    /// The shared arrangement for both volume verbs: project op-lock (they
    /// stop and start containers, and must not race a lifecycle op), crash
    /// journal, history context, terminal-event mapping.
    fn spawn_locked_volume_op<F, Fut>(
        &self,
        project: ProjectId,
        journal_verb: String,
        history_label: String,
        work: F,
    ) -> Result<OperationId, ErrorInfo>
    where
        F: FnOnce(Engine, Arc<OpHandle>, OperationId) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), ErrorInfo>> + Send,
    {
        {
            let mut busy = self.inner.busy_projects.lock().unwrap();
            if !busy.insert(project.0.clone()) {
                return Err(ErrorInfo::Conflict {
                    message: format!("an operation is already running on {}", project.0),
                });
            }
        }
        let (id, handle) = self.new_operation();
        let engine = self.clone();
        tokio::spawn(async move {
            let _ = engine.inner.deps.store.journal_push(mast_project::OperationJournalEntry {
                operation: id.0,
                project_id: project.0.clone(),
                verb: journal_verb,
                started_unix: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let result = crate::history::with_context(
                crate::history::CommandContext {
                    label: history_label,
                    project: Some(project.clone()),
                    operation: Some(id),
                },
                work(engine.clone(), handle.clone(), id),
            )
            .await;
            let kind = match result {
                Ok(()) => OperationEventKind::Completed,
                Err(_) if handle.cancel.is_cancelled() => OperationEventKind::Cancelled,
                Err(e) => OperationEventKind::Failed { error: e.to_string() },
            };
            let _ = engine.inner.deps.store.journal_remove(id.0);
            engine.inner.busy_projects.lock().unwrap().remove(&project.0);
            engine.emit_op(&handle, id, kind);
            engine.hint();
        });
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_volume_snapshot(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        compose_project: &str,
        service: &str,
        keys: &[String],
        container: Option<String>,
        was_running: bool,
        redactor: &Redactor,
    ) -> Result<(), ErrorInfo> {
        // Resolve every source volume up front — failing after stopping the
        // container would be a worse version of the same error.
        let mut pairs: Vec<(String, String)> = Vec::new();
        for key in keys {
            match self.compose_volume_name(compose_project, key).await? {
                Some(name) => pairs.push((key.clone(), name)),
                None => {
                    return Err(ErrorInfo::InvalidInput {
                        message: format!(
                            "the volume for {key} does not exist yet — start the project once \
                             so compose creates it"
                        ),
                    });
                }
            }
        }
        let group = new_group_id();
        let at = now_ms();

        self.pause_container(handle, op, service, &container, was_running).await?;
        let mut created: Vec<String> = Vec::new();
        let mut copy = async || -> Result<(), ErrorInfo> {
            for (key, source) in &pairs {
                let dest = format!("mast-snap-{group}-{key}");
                let create = argv(&[
                    "docker",
                    "volume",
                    "create",
                    "--label",
                    &format!("{L_SNAPSHOT}=1"),
                    "--label",
                    &format!("{L_PROJECT}={}", project.0),
                    "--label",
                    &format!("{L_SERVICE}={service}"),
                    "--label",
                    &format!("{L_GROUP}={group}"),
                    "--label",
                    &format!("{L_AT}={at}"),
                    "--label",
                    &format!("{L_SOURCE_KEY}={key}"),
                    "--label",
                    &format!("{L_SOURCE}={source}"),
                    "--label",
                    &format!("{L_COMPOSE_PROJECT}={compose_project}"),
                    &dest,
                ]);
                self.run_streamed_command(handle, op, &create, None, redactor, LS_TIMEOUT)
                    .await?;
                created.push(dest.clone());
                let copy = argv(&[
                    "docker",
                    "run",
                    "--rm",
                    "-v",
                    &format!("{source}:/from:ro"),
                    "-v",
                    &format!("{dest}:/to"),
                    HELPER_IMAGE,
                    "sh",
                    "-c",
                    "cp -a /from/. /to",
                ]);
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("copying {key} ({source})"),
                        stderr: false,
                    },
                );
                self.run_streamed_command(handle, op, &copy, None, redactor, COPY_TIMEOUT)
                    .await?;
            }
            Ok(())
        };
        let result = copy().await;
        // The container comes back whether the copy worked or not…
        self.resume_container(handle, op, service, &container, was_running).await;
        if result.is_err() {
            // …and a failed snapshot leaves no half-snapshot behind to be
            // mistaken for a good one.
            for dest in created {
                let _ = mast_docker::run_command(
                    &argv(&["docker", "volume", "rm", "-f", &dest]),
                    None,
                    &[],
                    LS_TIMEOUT,
                    64 * 1024,
                )
                .await;
            }
        }
        result?;
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!(
                    "snapshot saved ({} volume{}) — restore it any time from the {service} chip",
                    pairs.len(),
                    if pairs.len() == 1 { "" } else { "s" }
                ),
                stderr: false,
            },
        );
        Ok(())
    }

    async fn run_volume_restore(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        group: &str,
        redactor: &Redactor,
    ) -> Result<(), ErrorInfo> {
        let rows = self.snapshot_rows(group).await?;
        if rows.is_empty() {
            return Err(ErrorInfo::NotFound { what: format!("data snapshot {group}") });
        }
        // A group id names volumes; the project check keeps one project's
        // restore from ever touching another's data.
        if rows.iter().any(|(_, l)| l.get(L_PROJECT).map(String::as_str) != Some(&project.0)) {
            return Err(ErrorInfo::InvalidInput {
                message: "that snapshot belongs to a different project".into(),
            });
        }
        let service = rows
            .first()
            .and_then(|(_, l)| l.get(L_SERVICE).cloned())
            .unwrap_or_default();
        let (container, was_running) = {
            let st = self.inner.state.lock().unwrap();
            let svc = st
                .projects
                .get(&project.0)
                .and_then(|e| e.summary.services.iter().find(|s| s.name == service));
            (
                svc.and_then(|s| s.container_id.clone()),
                svc.is_some_and(|s| s.state == Some(ContainerState::Running)),
            )
        };

        self.pause_container(handle, op, &service, &container, was_running).await?;
        let restore = async || -> Result<(), ErrorInfo> {
            for (snap_volume, labels) in &rows {
                let Some(target) = labels.get(L_SOURCE) else { continue };
                let key = labels.get(L_SOURCE_KEY).cloned().unwrap_or_default();
                // A deleted original is recreated with the labels compose
                // stamps itself — without them compose refuses to adopt the
                // volume on the next `up`.
                if let Some(compose_project) = labels.get(L_COMPOSE_PROJECT) {
                    let _ = mast_docker::run_command(
                        &argv(&[
                            "docker",
                            "volume",
                            "create",
                            "--label",
                            &format!("com.docker.compose.project={compose_project}"),
                            "--label",
                            &format!("com.docker.compose.volume={key}"),
                            target,
                        ]),
                        None,
                        &[],
                        LS_TIMEOUT,
                        64 * 1024,
                    )
                    .await; // exists already = harmless no-op
                }
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("restoring {key} into {target}"),
                        stderr: false,
                    },
                );
                let copy = argv(&[
                    "docker",
                    "run",
                    "--rm",
                    "-v",
                    &format!("{snap_volume}:/from:ro"),
                    "-v",
                    &format!("{target}:/to"),
                    HELPER_IMAGE,
                    "sh",
                    "-c",
                    "find /to -mindepth 1 -delete && cp -a /from/. /to",
                ]);
                self.run_streamed_command(handle, op, &copy, None, redactor, COPY_TIMEOUT)
                    .await?;
            }
            Ok(())
        };
        let result = restore().await;
        self.resume_container(handle, op, &service, &container, was_running).await;
        result?;
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("{service} data restored from the snapshot"),
                stderr: false,
            },
        );
        Ok(())
    }

    /// Stop the service's container for a consistent copy. No container (or
    /// a stopped one) needs nothing.
    async fn pause_container(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        service: &str,
        container: &Option<String>,
        was_running: bool,
    ) -> Result<(), ErrorInfo> {
        let (Some(cid), true) = (container, was_running) else { return Ok(()) };
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("stopping {service} for a consistent copy"),
                stderr: false,
            },
        );
        self.run_streamed_command(
            handle,
            op,
            &argv(&["docker", "stop", cid]),
            None,
            &Redactor::default(),
            STOP_START_TIMEOUT,
        )
        .await
    }

    /// Bring the container back — attempted even after a failed copy, and
    /// its own failure only warns: the copy's error is the one to report.
    async fn resume_container(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        service: &str,
        container: &Option<String>,
        was_running: bool,
    ) {
        let (Some(cid), true) = (container, was_running) else { return };
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("starting {service} again"),
                stderr: false,
            },
        );
        let out = mast_docker::run_command(
            &argv(&["docker", "start", cid]),
            None,
            &[],
            STOP_START_TIMEOUT,
            64 * 1024,
        )
        .await;
        let failed = match out {
            Ok(out) if out.success() => None,
            Ok(out) => Some(out.stderr.trim().to_string()),
            Err(e) => Some(e.to_string()),
        };
        if let Some(error) = failed {
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!("could not restart {service}: {error}"),
                    stderr: true,
                },
            );
        }
        self.hint();
    }

    /// Delete one snapshot's volumes. Plain op — no project lock, since it
    /// touches only Mast-made snapshot volumes.
    pub(crate) async fn remove_volume_snapshot(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        group: &str,
    ) -> Result<(), ErrorInfo> {
        let rows = self.snapshot_rows(group).await?;
        if rows.is_empty() {
            return Err(ErrorInfo::NotFound { what: format!("data snapshot {group}") });
        }
        for (name, _) in &rows {
            self.run_streamed_command(
                handle,
                op,
                &argv(&["docker", "volume", "rm", name]),
                None,
                &Redactor::default(),
                LS_TIMEOUT,
            )
            .await?;
        }
        let owner = rows.first().and_then(|(_, l)| l.get(L_PROJECT).cloned()).unwrap_or_default();
        self.with_state(|st, events| {
            if let Some(entry) = st.projects.get(&owner) {
                events.push(PatchEvent::ProjectUpdated { project: entry.summary.clone() });
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_parse_and_group_into_snapshots_newest_first() {
        let stdout = "mast-snap-1a2b-sail-mysql\tmast.at=200,mast.group=1a2b,mast.project=p1,\
                      mast.service=mysql,mast.snapshot=1,mast.source-key=sail-mysql,\
                      mast.source=app_sail-mysql,mast.compose-project=app\n\
                      mast-snap-1a2b-sail-mysql-conf\tmast.at=200,mast.group=1a2b,\
                      mast.project=p1,mast.service=mysql,mast.snapshot=1,\
                      mast.source-key=sail-mysql-conf,mast.source=app_sail-mysql-conf,\
                      mast.compose-project=app\n\
                      mast-snap-9f-sail-redis\tmast.at=900,mast.group=9f,mast.project=p1,\
                      mast.service=redis,mast.snapshot=1,mast.source-key=sail-redis,\
                      mast.source=app_sail-redis,mast.compose-project=app\n";
        let snapshots = group_snapshots(parse_volume_rows(stdout));
        assert_eq!(snapshots.len(), 2);
        // Newest first, multi-volume groups intact.
        assert_eq!(snapshots[0].group, "9f");
        assert_eq!(snapshots[0].service, "redis");
        assert_eq!(snapshots[1].at_unix_ms, 200);
        assert_eq!(snapshots[1].volumes, vec!["sail-mysql", "sail-mysql-conf"]);
        assert_eq!(snapshots[1].project.0, "p1");
    }

    #[test]
    fn garbage_rows_are_skipped_not_fatal() {
        let snapshots = group_snapshots(parse_volume_rows(
            "no-tab-here\nname\t\nweird\tnot-a-pair,mast.group=g1,mast.source-key=k\n",
        ));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].volumes, vec!["k"]);
    }
}
