//! `Action::ShareProject` — sail share as a first-class, debuggable
//! operation. The tracker's verdict on `sail share` is "permanently
//! half-broken", and most of that is invisibility: the expose client's
//! output scrolls in a terminal, the URL is hand-copied, and the classic
//! failure (Vite dev-server asset URLs leaking through the tunnel as
//! loopback fetches, blocked by Chrome's Private Network Access and blamed
//! on CORS) says nothing about its cause. Here the tunnel runs as a
//! streamed operation: preflight names the asset trap BEFORE the link is
//! handed out, the public URL is parsed onto the project summary (for the
//! UI's link + QR code), and cancelling the operation reliably stops the
//! container — which a killed `docker run` client alone does not.

use std::sync::Arc;
use std::time::Duration;

use mast_contract::{ErrorInfo, OperationEventKind, OperationId, PatchEvent, ProjectId};
use mast_docker::run_command;
use mast_laravel::share::{ShareSettings, find_public_url, share_run_argv, share_settings};
use tokio::sync::mpsc;

use crate::diagnostics::{PROBE_CAP, PROBE_TIMEOUT};
use crate::{Engine, internal_err};

pub(crate) fn share_container_name(project_id: &str) -> String {
    format!("mast-share-{project_id}")
}

impl Engine {
    pub(crate) fn dispatch_share(&self, project: ProjectId) -> Result<OperationId, ErrorInfo> {
        let (path, redactor, name, running) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            (
                entry.record.path.clone(),
                entry.redactor.clone(),
                entry.summary.name.clone(),
                entry.summary.status != mast_contract::ProjectStatus::Stopped,
            )
        };
        if !running {
            return Err(ErrorInfo::InvalidInput {
                message: "the tunnel forwards the RUNNING app — start the project first".into(),
            });
        }

        let (id, handle) = self.new_operation();
        let engine = self.clone();
        tokio::spawn(async move {
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let container = share_container_name(&project.0);
            let work = crate::history::with_context(
                crate::history::CommandContext {
                    label: format!("Share {name}"),
                    project: Some(project.clone()),
                    operation: Some(id),
                },
                engine.share_work(&handle, id, &project, &path, &container, &redactor),
            );
            let result = work.await;
            // Whatever happened, leave nothing behind: the tunnel container
            // outlives a killed docker client, and a stale URL on the
            // summary would be a link to nowhere.
            let rm: Vec<String> =
                ["docker", "rm", "-f", container.as_str()].map(String::from).into();
            let _ = run_command(&rm, None, &[], PROBE_TIMEOUT, PROBE_CAP).await;
            engine.set_share_url(&project.0, None);
            let kind = match result {
                Ok(()) => OperationEventKind::Completed,
                Err(_) if handle.cancel.is_cancelled() => {
                    engine.emit_op(
                        &handle,
                        id,
                        OperationEventKind::Output {
                            line: "share tunnel closed".into(),
                            stderr: false,
                        },
                    );
                    OperationEventKind::Cancelled
                }
                Err(e) => {
                    engine.flush_signature_explanations(&handle, id);
                    OperationEventKind::Failed { error: redactor.redact(&e.to_string()) }
                }
            };
            engine.emit_op(&handle, id, kind);
            engine.hint();
        });
        Ok(id)
    }

    fn set_share_url(&self, project: &str, url: Option<String>) {
        self.with_state(|st, events| {
            if let Some(entry) = st.projects.get_mut(project)
                && entry.summary.share_url != url
            {
                entry.summary.share_url = url;
                events.push(PatchEvent::ProjectUpdated { project: entry.summary.clone() });
            }
        });
    }

    async fn share_work(
        &self,
        handle: &Arc<crate::OpHandle>,
        op: OperationId,
        project: &ProjectId,
        path: &std::path::Path,
        container: &str,
        redactor: &crate::Redactor,
    ) -> Result<(), ErrorInfo> {
        let out = |line: String, stderr: bool| {
            self.emit_op(handle, op, OperationEventKind::Output { line, stderr });
        };

        let (settings, hot) = tokio::task::spawn_blocking({
            let path = path.to_path_buf();
            move || {
                let src = std::fs::read_to_string(path.join(".env")).unwrap_or_default();
                let pairs: Vec<(String, String)> = mast_laravel::EnvFile::parse(&src)
                    .entries()
                    .map(|e| (e.key.clone(), e.value.clone()))
                    .collect();
                let hot = std::fs::read_to_string(path.join("public/hot"))
                    .ok()
                    .and_then(|c| mast_laravel::vite::parse_hot_file(&c))
                    .map(|hot| {
                        let listening = crate::diagnostics::dev_server_listening(&hot);
                        (hot, listening)
                    });
                (share_settings(&pairs), hot)
            }
        })
        .await
        .map_err(internal_err)?;

        // Preflight: name the asset trap BEFORE anyone opens the link.
        match &hot {
            Some((hot, Some(true))) => out(
                format!(
                    "WARNING: the Vite dev server is running, so shared pages will point \
                     their assets at {} — visitors cannot reach that address, and Chrome \
                     blocks it as a Private Network Access (\"CORS\") error. Stop the dev \
                     server and run a production build (npm run build) before handing the \
                     link out.",
                    hot.url
                ),
                true,
            ),
            Some((hot, _)) => out(
                format!(
                    "WARNING: public/hot is stale (nothing listens at {}), so pages render \
                     dev-server asset URLs and builds change nothing. Diagnostics offers a \
                     one-click fix, or delete public/hot.",
                    hot.url
                ),
                true,
            ),
            None => {}
        }
        out(
            "if links on shared pages point at localhost, configure trusted proxies \
             (Laravel docs: Sharing Your Site)"
                .into(),
            false,
        );

        // A crashed previous share can leave the named container behind.
        let rm: Vec<String> = ["docker", "rm", "-f", container].map(String::from).into();
        let _ = run_command(&rm, None, &[], PROBE_TIMEOUT, PROBE_CAP).await;

        let argv = share_run_argv(&settings, container);
        out(format!("$ {}", redactor.redact(&argv.join(" "))), false);

        let (line_tx, mut line_rx) = mpsc::channel::<mast_docker::OutputLine>(256);
        let forwarder = {
            let engine = self.clone();
            let handle = handle.clone();
            let redactor = redactor.clone();
            let project = project.clone();
            let settings: ShareSettings = settings.clone();
            tokio::spawn(async move {
                let mut url_seen = false;
                while let Some(line) = line_rx.recv().await {
                    if !url_seen
                        && let Some(url) = find_public_url(&line.line, &settings)
                    {
                        url_seen = true;
                        engine.set_share_url(&project.0, Some(url.clone()));
                        engine.emit_op(
                            &handle,
                            op,
                            OperationEventKind::Output {
                                line: format!("public URL: {url}"),
                                stderr: false,
                            },
                        );
                    }
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
            &argv,
            Some(path),
            &[],
            line_tx,
            handle.cancel.clone(),
            // The tunnel runs until stopped; a week is "indefinitely" with a
            // backstop, same as long-running project commands.
            Duration::from_secs(7 * 24 * 60 * 60),
            Duration::from_secs(8),
        )
        .await;
        let _ = forwarder.await;
        match result {
            Ok(mast_docker::CommandOutcome::Exited(0)) => Ok(()),
            Ok(mast_docker::CommandOutcome::Exited(code)) => Err(ErrorInfo::Internal {
                message: format!("the expose client exited with status {code}"),
            }),
            Ok(mast_docker::CommandOutcome::Cancelled) => {
                Err(ErrorInfo::Internal { message: "cancelled".into() })
            }
            Err(e) => Err(ErrorInfo::Internal { message: redactor.redact(&e.to_string()) }),
        }
    }
}
