//! The Sail PHP runtime: what the app service builds from, what it could
//! build from, and the version switch as ONE operation. By hand this is
//! four steps users routinely half-do — edit `build.context`, edit the
//! `sail-X.Y/app` tag, `build --no-cache`, recreate — and any missed step
//! leaves the container running a different PHP than everything believes
//! (laravel/sail#442's afternoon-eating shape; ~20 tracker threads).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_contract::{ErrorInfo, OperationEventKind, OperationId, PhpVersionInfo, ProjectId};
use mast_yaml_edit::key;

use crate::{Engine, internal_err};

/// Sail-shaped build services from the project's base compose file.
pub(crate) fn sail_build_facts(dir: &Path) -> Vec<mast_diagnostics::SailBuildFacts> {
    let Some(source) = base_compose_source(dir) else { return Vec::new() };
    mast_compose::sail::sail_builds(&source)
        .into_iter()
        .filter_map(|b| {
            let context = b.context?;
            let context_series = mast_compose::sail::runtime_series(&context)?;
            Some(mast_diagnostics::SailBuildFacts {
                context_exists: dir.join(context.trim_start_matches("./")).is_dir(),
                image_series: b.image.as_deref().and_then(mast_compose::sail::image_series),
                service: b.service,
                context,
                context_series,
            })
        })
        .collect()
}

fn base_compose_source(dir: &Path) -> Option<String> {
    ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"]
        .iter()
        .find_map(|name| std::fs::read_to_string(dir.join(name)).ok())
}

/// PHP series present under `vendor/laravel/sail/runtimes/`.
pub(crate) fn available_runtimes(dir: &Path) -> Vec<String> {
    let mut series: Vec<String> = std::fs::read_dir(dir.join("vendor/laravel/sail/runtimes"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    series.sort();
    series
}

/// Node majors the picker offers — the nodesource release lines the Sail
/// runtime Dockerfile can install (`setup_$NODE_VERSION.x`).
const NODE_CHOICES: [&str; 4] = ["18", "20", "22", "24"];

/// What the runtime pickers show: the first Sail-shaped build service's
/// pinned PHP series and the vendored alternatives, plus the effective Node
/// major — the compose `build.args.NODE_VERSION` override when present,
/// else the runtime Dockerfile's ARG default.
pub(crate) fn php_info(dir: &Path) -> Option<PhpVersionInfo> {
    let source = base_compose_source(dir)?;
    let build = mast_compose::sail::sail_builds(&source).into_iter().find_map(|b| {
        let context = b.context.clone()?;
        let series = mast_compose::sail::runtime_series(&context)?;
        Some((b, context, series))
    });
    let (build, context, current) = build?;
    let dockerfile_node =
        std::fs::read_to_string(dir.join(context.trim_start_matches("./")).join("Dockerfile"))
            .ok()
            .and_then(|dockerfile| {
                dockerfile.lines().find_map(|line| {
                    line.trim().strip_prefix("ARG NODE_VERSION=").map(|v| v.trim().to_string())
                })
            })
            .filter(|v| !v.is_empty());
    let node = build.node_arg.clone().or(dockerfile_node);
    // Only the mapping build form can carry the override; a read-only chip
    // is better than a picker whose apply must refuse.
    let mut node_available: Vec<String> = if build.build_is_mapping {
        NODE_CHOICES.map(String::from).to_vec()
    } else {
        Vec::new()
    };
    if let Some(n) = &node
        && !node_available.is_empty()
        && !node_available.contains(n)
    {
        node_available.push(n.clone());
        node_available.sort_by_key(|v| v.parse::<u32>().unwrap_or(u32::MAX));
    }
    Some(PhpVersionInfo {
        service: build.service,
        current,
        available: available_runtimes(dir),
        node,
        node_available,
    })
}

impl Engine {
    /// `Action::SetPhpVersion`: the four-step switch as a single cancellable
    /// operation under the project's op lock, journaled like a lifecycle verb.
    pub(crate) fn dispatch_php_switch(
        &self,
        project: ProjectId,
        service: String,
        series: String,
    ) -> Result<OperationId, ErrorInfo> {
        if series.is_empty()
            || !series.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            return Err(ErrorInfo::InvalidInput {
                message: format!("\"{series}\" is not a PHP series like 8.4"),
            });
        }
        let (invocation, file) = self.catalog_context(&project)?;
        let (path, redactor, running) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            (
                entry.record.path.clone(),
                entry.redactor.clone(),
                entry.summary.status != mast_contract::ProjectStatus::Stopped,
            )
        };
        self.inner.crash_notices.lock().unwrap().remove(&project.0);
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
                verb: format!("switch PHP to {series}"),
                started_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let result = engine
                .php_switch_work(
                    &handle, id, &invocation, &file, &path, &service, &series, running, &redactor,
                )
                .await;
            let kind = match result {
                Ok(()) => OperationEventKind::Completed,
                Err(_) if handle.cancel.is_cancelled() => OperationEventKind::Cancelled,
                Err(e) => {
                    engine.flush_signature_explanations(&handle, id, Some(&project));
                    OperationEventKind::Failed { error: redactor.redact(&e.to_string()) }
                }
            };
            // Lock and journal cleared BEFORE the terminal event, like every
            // lifecycle op: a client that sees the terminal may immediately
            // dispatch a follow-up.
            let _ = engine.inner.deps.store.journal_remove(id.0);
            engine.inner.busy_projects.lock().unwrap().remove(&project.0);
            engine.emit_op(&handle, id, kind);
            engine.hint();
        });
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn php_switch_work(
        &self,
        handle: &Arc<crate::OpHandle>,
        op: OperationId,
        invocation: &mast_compose::ComposeInvocation,
        file: &Path,
        project_dir: &Path,
        service: &str,
        series: &str,
        running: bool,
        redactor: &crate::Redactor,
    ) -> Result<(), ErrorInfo> {
        let out = |line: String| {
            self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
        };
        let source = tokio::task::spawn_blocking({
            let file = file.to_path_buf();
            move || std::fs::read_to_string(file)
        })
        .await
        .map_err(internal_err)?
        .map_err(internal_err)?;

        let build = mast_compose::sail::sail_builds(&source)
            .into_iter()
            .find(|b| b.service == service)
            .ok_or_else(|| ErrorInfo::InvalidInput {
                message: format!("service \"{service}\" has no build: in this file"),
            })?;
        let context = build.context.ok_or_else(|| ErrorInfo::InvalidInput {
            message: format!("service \"{service}\" has no build context"),
        })?;
        let current = mast_compose::sail::runtime_series(&context).ok_or_else(|| {
            ErrorInfo::InvalidInput {
                message: format!(
                    "\"{service}\" does not build from a Sail runtime shape ({context})"
                ),
            }
        })?;
        if current == series {
            out(format!("{service} already builds PHP {series} — nothing to do"));
            return Ok(());
        }
        let trimmed = context.trim_end_matches('/');
        let base = trimmed.strip_suffix(current.as_str()).unwrap_or(trimmed);
        let new_context = format!("{base}{series}");
        if !project_dir.join(new_context.trim_start_matches("./")).is_dir() {
            let available = available_runtimes(project_dir);
            return Err(ErrorInfo::InvalidInput {
                message: if available.is_empty() {
                    format!("{new_context} does not exist — run composer install first")
                } else {
                    format!(
                        "PHP {series} is not vendored here ({new_context} does not exist) — \
                         available: {}",
                        available.join(", ")
                    )
                },
            });
        }

        // Context and image tag move together — the whole point.
        let build_string_form =
            mast_yaml_edit::get_scalar(&source, &[key("services"), key(service), key("build")])
                .is_some();
        let mut edits = vec![mast_yaml_edit::Edit::SetScalar {
            path: if build_string_form {
                vec![key("services"), key(service), key("build")]
            } else {
                vec![key("services"), key(service), key("build"), key("context")]
            },
            value: format!("'{new_context}'"),
        }];
        let mut summary = vec![format!("{service}: build context -> {new_context}")];
        if build.image.as_deref().and_then(mast_compose::sail::image_series).is_some() {
            edits.push(mast_yaml_edit::Edit::SetScalar {
                path: vec![key("services"), key(service), key("image")],
                value: format!("'sail-{series}/app'"),
            });
            summary.push(format!("{service}: image -> sail-{series}/app"));
        }
        self.write_compose(invocation, file, &edits, summary.clone()).await?;
        for line in summary {
            out(line);
        }

        self.rebuild_runtime_and_verify(
            handle,
            op,
            invocation,
            project_dir,
            service,
            running,
            redactor,
            &["php".into(), "-v".into()],
            &format!("PHP {series}."),
            &format!("PHP {series}"),
        )
        .await
    }

    /// The back half every runtime switch shares: no-cache rebuild, recreate
    /// when the project was running, and the container itself gets the last
    /// word (`verify_argv` output must contain `expect`).
    #[allow(clippy::too_many_arguments)]
    async fn rebuild_runtime_and_verify(
        &self,
        handle: &Arc<crate::OpHandle>,
        op: OperationId,
        invocation: &mast_compose::ComposeInvocation,
        project_dir: &Path,
        service: &str,
        running: bool,
        redactor: &crate::Redactor,
        verify_argv: &[String],
        expect: &str,
        label: &str,
    ) -> Result<(), ErrorInfo> {
        let out = |line: String| {
            self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
        };
        let (argv, env) =
            crate::db_repair::scoped_compose_argv(invocation, &["build", "--no-cache", service]);
        out(format!("$ {}", argv.join(" ")));
        // A cold runtime build fetches the base image and every PPA
        // package — the slowest legitimate operation Mast runs.
        self.run_streamed_command_env(
            handle,
            op,
            &argv,
            Some(project_dir),
            &env,
            redactor,
            Duration::from_secs(45 * 60),
        )
        .await?;

        if !running {
            out(format!("built {label} — start the project to run it"));
            return Ok(());
        }
        let (argv, env) =
            crate::db_repair::scoped_compose_argv(invocation, &["up", "-d", service]);
        out(format!("$ {}", argv.join(" ")));
        self.run_streamed_command_env(
            handle,
            op,
            &argv,
            Some(project_dir),
            &env,
            redactor,
            Duration::from_secs(15 * 60),
        )
        .await?;

        // Trust nothing: the container itself says what it runs.
        let argv =
            crate::project_ops::compose_exec_argv(invocation, service, verify_argv);
        let verified = mast_docker::run_command(
            &argv,
            Some(&invocation.project_dir),
            &[],
            Duration::from_secs(30),
            crate::diagnostics::PROBE_CAP,
        )
        .await
        .ok()
        .filter(|o| o.success())
        .is_some_and(|o| o.stdout.contains(expect));
        if !verified {
            return Err(ErrorInfo::Internal {
                message: format!(
                    "the rebuilt container does not report {label} — check the build output \
                     above"
                ),
            });
        }
        out(format!("verified: {service} runs {label}"));
        Ok(())
    }

    /// `Action::SetNodeVersion`: pin `build.args.NODE_VERSION` (Sail's
    /// documented override), then the same rebuild-and-verify as PHP.
    pub(crate) fn dispatch_node_switch(
        &self,
        project: ProjectId,
        service: String,
        major: String,
    ) -> Result<OperationId, ErrorInfo> {
        let (invocation, file) = self.catalog_context(&project)?;
        let (path, redactor, running) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            (
                entry.record.path.clone(),
                entry.redactor.clone(),
                entry.summary.status != mast_contract::ProjectStatus::Stopped,
            )
        };
        self.inner.crash_notices.lock().unwrap().remove(&project.0);
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
                verb: format!("switch Node to {major}"),
                started_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let result = engine
                .node_switch_work(
                    &handle, id, &invocation, &file, &path, &service, &major, running, &redactor,
                )
                .await;
            let kind = match result {
                Ok(()) => OperationEventKind::Completed,
                Err(_) if handle.cancel.is_cancelled() => OperationEventKind::Cancelled,
                Err(e) => {
                    engine.flush_signature_explanations(&handle, id, Some(&project));
                    OperationEventKind::Failed { error: redactor.redact(&e.to_string()) }
                }
            };
            let _ = engine.inner.deps.store.journal_remove(id.0);
            engine.inner.busy_projects.lock().unwrap().remove(&project.0);
            engine.emit_op(&handle, id, kind);
            engine.hint();
        });
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn node_switch_work(
        &self,
        handle: &Arc<crate::OpHandle>,
        op: OperationId,
        invocation: &mast_compose::ComposeInvocation,
        file: &Path,
        project_dir: &Path,
        service: &str,
        major: &str,
        running: bool,
        redactor: &crate::Redactor,
    ) -> Result<(), ErrorInfo> {
        let source = tokio::task::spawn_blocking({
            let file = file.to_path_buf();
            move || std::fs::read_to_string(file)
        })
        .await
        .map_err(internal_err)?
        .map_err(internal_err)?;
        let already = mast_compose::sail::sail_builds(&source)
            .into_iter()
            .find(|b| b.service == service)
            .is_some_and(|b| b.node_arg.as_deref() == Some(major));
        if already {
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!("{service} already pins Node {major} — nothing to do"),
                    stderr: false,
                },
            );
            return Ok(());
        }
        let (edits, summary) = mast_compose::sail::plan_set_node_version(&source, service, major)
            .map_err(|message| ErrorInfo::InvalidInput { message })?;
        self.write_compose(invocation, file, &edits, summary.clone()).await?;
        for line in summary {
            self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
        }
        self.rebuild_runtime_and_verify(
            handle,
            op,
            invocation,
            project_dir,
            service,
            running,
            redactor,
            &["node".into(), "-v".into()],
            &format!("v{major}."),
            &format!("Node {major}"),
        )
        .await
    }
}
