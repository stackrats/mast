//! Workspace orchestration (M6): shared-network wiring, membership
//! mutation, ordered start/stop over the dependency graph with the
//! readiness ladder between layers.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use mast_compose::ComposeInvocation;
use mast_contract::{
    CaptureReason, ErrorInfo, FileEditPreview, OperationEventKind, OperationId, PatchEvent,
    ProjectId, ProjectStatus, ServiceHealth, WorkspaceId,
};
use mast_docker::CommandOutcome;
use mast_project::WorkspaceRecord;

use crate::lifecycle::LifecycleVerb;
use crate::ops::OpHandle;
use crate::{Engine, internal_err, workspace, workspace_summaries};

impl Engine {
    pub(crate) fn network_attach_context(
        &self,
        workspace: &WorkspaceId,
        project: &ProjectId,
    ) -> Result<(String, ComposeInvocation, PathBuf), ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let record = st
            .workspaces
            .iter()
            .find(|w| w.id == workspace.0)
            .ok_or(ErrorInfo::NotFound { what: format!("workspace {}", workspace.0) })?;
        if !record.members.iter().any(|m| m.project_id == project.0) {
            return Err(ErrorInfo::InvalidInput {
                message: "project is not a member of this workspace".into(),
            });
        }
        let network = mast_compose::workspace_network_name(&record.name);
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let invocation = entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
            message: "project not resolved yet".into(),
        })?;
        let file = invocation
            .files
            .first()
            .map(|f| f.path.clone())
            .ok_or(ErrorInfo::Internal { message: "invocation has no files".into() })?;
        Ok((network, invocation, file))
    }

    /// Preview attaching a member's services to the workspace network:
    /// whole-file before/after through the same planner the apply path uses.
    pub async fn network_attach_preview(
        &self,
        workspace: &WorkspaceId,
        project: &ProjectId,
    ) -> Result<FileEditPreview, ErrorInfo> {
        let (network, _invocation, file) = self.network_attach_context(workspace, project)?;
        tokio::task::spawn_blocking(move || {
            let before = std::fs::read_to_string(&file)
                .map_err(internal_err)?;
            let plan = mast_compose::plan_network_attach(&before, &network)
                .map_err(|message| ErrorInfo::InvalidInput { message })?;
            let mut summary: Vec<String> = plan
                .attached_services
                .iter()
                .map(|s| format!("attach service {s} to {network}"))
                .collect();
            for done in &plan.already_attached {
                summary.push(format!("{done} is already attached"));
            }
            for (svc, reason) in &plan.skipped {
                summary.push(format!("skipping {svc}: {reason}"));
            }
            let no_op = plan.edits.is_empty();
            let after = if no_op {
                before.clone()
            } else {
                mast_yaml_edit::apply_all(&before, &plan.edits)
                    .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?
            };
            Ok(FileEditPreview {
                file: file.to_string_lossy().into_owned(),
                before,
                after,
                summary,
                no_op,
            })
        })
        .await
        .map_err(internal_err)?
    }

    pub(crate) fn mutate_workspaces(
        &self,
        mutate: impl FnOnce(&mut Vec<WorkspaceRecord>),
    ) -> Result<(), ErrorInfo> {
        let records = self.with_state(|st, events| {
            mutate(&mut st.workspaces);
            events.push(PatchEvent::WorkspacesChanged { workspaces: workspace_summaries(st) });
            st.workspaces.clone()
        });
        self.inner
            .deps
            .store
            .save_workspaces(&records)
            .map_err(internal_err)
    }

    /// Ordered workspace lifecycle: layered topo for Up (waiting for
    /// readiness between layers; a failed member blocks its dependents),
    /// reverse order for Stop. Holds every member's operation lock for the
    /// duration.
    pub(crate) async fn run_workspace(
        &self,
        handle: &Arc<OpHandle>,
        op_id: OperationId,
        ws_id: &WorkspaceId,
        verb: LifecycleVerb,
    ) -> Result<(), ErrorInfo> {
        let (record, graph) = {
            let st = self.inner.state.lock().unwrap();
            let record = st
                .workspaces
                .iter()
                .find(|w| w.id == ws_id.0)
                .cloned()
                .ok_or(ErrorInfo::NotFound { what: format!("workspace {}", ws_id.0) })?;
            let graph: Vec<(String, Vec<String>)> = record
                .members
                .iter()
                .map(|m| (m.project_id.clone(), m.depends_on.clone()))
                .collect();
            (record, graph)
        };
        let mut layers = workspace::topo_layers(&graph)
            .map_err(|e| ErrorInfo::InvalidInput { message: e })?;
        if verb == LifecycleVerb::Stop {
            layers.reverse();
        }

        // The shared network is external:true in compose files — Mast owns
        // its lifecycle. Create it (idempotently) before members come up.
        if verb == LifecycleVerb::Up {
            let network = mast_compose::workspace_network_name(&record.name);
            let argv: Vec<String> =
                ["docker", "network", "create", "--driver", "bridge", network.as_str()]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            match mast_docker::run_command(&argv, None, &[], Duration::from_secs(20), 64 * 1024)
                .await
            {
                Ok(out) if out.success() => {
                    self.emit_op(
                        handle,
                        op_id,
                        OperationEventKind::Output {
                            line: format!("created shared network {network}"),
                            stderr: false,
                        },
                    );
                }
                Ok(out) if out.stderr.contains("already exists") => {}
                Ok(out) => tracing::warn!("network create: {}", out.stderr.trim()),
                Err(e) => tracing::warn!("network create failed: {e}"),
            }
        }

        // Take every member's lock up front — all or Conflict.
        let member_ids: Vec<String> =
            record.members.iter().map(|m| m.project_id.clone()).collect();
        {
            let mut busy = self.inner.busy_projects.lock().unwrap();
            if let Some(taken) = member_ids.iter().find(|m| busy.contains(*m)) {
                return Err(ErrorInfo::Conflict {
                    message: format!("an operation is already running on {taken}"),
                });
            }
            for member in &member_ids {
                busy.insert(member.clone());
            }
        }
        let release = |engine: &Engine| {
            let mut busy = engine.inner.busy_projects.lock().unwrap();
            for member in &member_ids {
                busy.remove(member);
            }
        };

        let result = self.run_workspace_layers(handle, op_id, &layers, verb).await;
        release(self);
        self.hint();
        result
    }

    async fn run_workspace_layers(
        &self,
        handle: &Arc<OpHandle>,
        op_id: OperationId,
        layers: &[Vec<String>],
        verb: LifecycleVerb,
    ) -> Result<(), ErrorInfo> {
        let display_name = |engine: &Engine, id: &str| -> String {
            let st = engine.inner.state.lock().unwrap();
            st.projects.get(id).map(|e| e.summary.name.clone()).unwrap_or_else(|| id.to_string())
        };
        for (layer_index, layer) in layers.iter().enumerate() {
            for member in layer {
                let name = display_name(self, member);
                self.emit_op(
                    handle,
                    op_id,
                    OperationEventKind::Output {
                        line: format!("[{name}] {}…", verb.label()),
                        stderr: false,
                    },
                );
                let (invocation, redactor) = {
                    let st = self.inner.state.lock().unwrap();
                    let entry = st.projects.get(member).ok_or(ErrorInfo::NotFound {
                        what: format!("project {member}"),
                    })?;
                    let invocation =
                        entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
                            message: format!("{name} is not resolved"),
                        })?;
                    (invocation, entry.redactor.clone())
                };
                // Layer by layer, so an earlier member's ports are already
                // bound by the time a later one checks — which is exactly
                // how two members that publish the same port get separated.
                if verb == LifecycleVerb::Up {
                    self.preflight_ports(
                        handle,
                        op_id,
                        &ProjectId(member.clone()),
                        &format!("[{name}] "),
                    )
                    .await;
                }
                // Stopping a whole workspace is the same evidence-destroying
                // moment as stopping one project, and members go down in
                // dependency order — so the one that failed first is often
                // the one whose log matters.
                if verb == LifecycleVerb::Stop {
                    let requests = self.capture_requests_for_project(
                        &ProjectId(member.clone()),
                        CaptureReason::Teardown { verb: verb.label().to_string() },
                    );
                    for request in requests {
                        self.run_capture(request).await;
                    }
                }
                let (line_tx, mut line_rx) = mpsc::channel::<mast_docker::OutputLine>(256);
                let forwarder = {
                    let engine = self.clone();
                    let handle = handle.clone();
                    let redactor = redactor.clone();
                    let prefix = name.clone();
                    tokio::spawn(async move {
                        while let Some(line) = line_rx.recv().await {
                            engine.emit_op(
                                &handle,
                                op_id,
                                OperationEventKind::Output {
                                    line: format!("[{prefix}] {}", redactor.redact(&line.line)),
                                    stderr: line.stderr,
                                },
                            );
                        }
                    })
                };
                let result = self
                    .inner
                    .deps
                    .runner
                    .run(&invocation, verb, None, line_tx, handle.cancel.clone())
                    .await;
                let _ = forwarder.await;
                match result {
                    Ok(CommandOutcome::Exited(0)) => {}
                    Ok(CommandOutcome::Exited(code)) => {
                        return Err(ErrorInfo::Internal {
                            message: format!(
                                "{name} {} failed (exit {code}) — dependents blocked",
                                verb.label()
                            ),
                        });
                    }
                    Ok(CommandOutcome::Cancelled) => {
                        return Err(ErrorInfo::Internal {
                            message: format!("cancelled during {name}"),
                        });
                    }
                    Err(e) => {
                        return Err(ErrorInfo::Internal {
                            message: format!("{name}: {}", redactor.redact(&e)),
                        });
                    }
                }
                self.hint();
            }

            // Between layers on the way up: wait for the whole layer to be
            // Ready before dependents start.
            if verb == LifecycleVerb::Up && layer_index < layers.len() - 1 {
                for member in layer {
                    let name = display_name(self, member);
                    self.wait_ready(member, &name, handle, op_id).await.map_err(|e| {
                        ErrorInfo::Internal {
                            message: format!("{name}: {e} — dependents blocked"),
                        }
                    })?;
                    self.emit_op(
                        handle,
                        op_id,
                        OperationEventKind::Output {
                            line: format!("[{name}] ready"),
                            stderr: false,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    /// Readiness ladder (plan §7): compose healthcheck when present → Laravel
    /// HTTP `/up` on the host-published APP_PORT → stable-running grace.
    /// (Explicit per-project overrides and TCP-on-selected-port arrive with
    /// probe configuration.)
    async fn wait_ready(
        &self,
        project_id: &str,
        name: &str,
        handle: &OpHandle,
        op_id: OperationId,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + self.inner.config.ready_timeout;
        let grace = self.inner.config.ready_grace;
        let mut announced: Option<&'static str> = None;
        let mut announce = |engine: &Engine, mode: &'static str| {
            if announced != Some(mode) {
                announced = Some(mode);
                engine.emit_op(
                    handle,
                    op_id,
                    OperationEventKind::Output {
                        line: format!("[{name}] waiting for readiness ({mode})…"),
                        stderr: false,
                    },
                );
            }
        };
        let mut http_absent = false;
        let mut running_since: Option<std::time::Instant> = None;

        loop {
            if handle.cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let (running, healthchecked, healthy, app_port) = {
                let st = self.inner.state.lock().unwrap();
                match st.projects.get(project_id) {
                    Some(e) => (
                        e.summary.status == ProjectStatus::Running,
                        e.summary.services.iter().any(|s| s.health != ServiceHealth::Unknown),
                        !e.summary.services.iter().any(|s| {
                            matches!(s.health, ServiceHealth::Unhealthy | ServiceHealth::Starting)
                        }),
                        e.app_port,
                    ),
                    None => return Err("project disappeared".into()),
                }
            };

            if running {
                if healthchecked {
                    announce(self, "compose healthcheck");
                    if healthy {
                        return Ok(());
                    }
                } else if let Some(port) = app_port
                    && !http_absent
                {
                    announce(self, "http /up");
                    match probe_http_up(port).await {
                        UpProbe::Ready => return Ok(()),
                        UpProbe::NotReady => {}
                        UpProbe::EndpointAbsent => {
                            http_absent = true;
                            self.emit_op(
                                handle,
                                op_id,
                                OperationEventKind::Output {
                                    line: format!(
                                        "[{name}] /up not served — falling back to \
                                         stable-running grace"
                                    ),
                                    stderr: false,
                                },
                            );
                        }
                    }
                } else {
                    announce(self, "stable-running grace");
                    match running_since {
                        Some(since) if since.elapsed() >= grace => return Ok(()),
                        Some(_) => {}
                        None => running_since = Some(std::time::Instant::now()),
                    }
                }
            } else {
                running_since = None;
            }

            if std::time::Instant::now() >= deadline {
                // The whole workspace start is about to fail on this member,
                // and the reason is in its output. Nobody was tailing it —
                // that is precisely why the timeout is so opaque today.
                self.capture_stalled_services(project_id).await;
                return Err("did not become ready in time".into());
            }
            self.hint();
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

enum UpProbe {
    Ready,
    NotReady,
    /// 404/405 — no health endpoint on this app; fall back to grace.
    EndpointAbsent,
}

/// Minimal HTTP GET /up against the host-published port. No HTTP client
/// dependency — we control both ends of a two-line request.
async fn probe_http_up(port: u16) -> UpProbe {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(Ok(mut stream)) = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    else {
        return UpProbe::NotReady;
    };
    let request =
        format!("GET /up HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return UpProbe::NotReady;
    }
    let mut buf = [0u8; 128];
    let n = match tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => n,
        _ => return UpProbe::NotReady,
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    match head.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()) {
        Some(status) if (200..300).contains(&status) => UpProbe::Ready,
        Some(404 | 405) => UpProbe::EndpointAbsent,
        _ => UpProbe::NotReady,
    }
}
