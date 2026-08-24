//! Per-project operations: env reporting, import, the new-project wizard,
//! lifecycle verbs, user commands, app processes, and log streams.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use mast_compose::ComposeInvocation;
use mast_contract::{
    CaptureReason, ContainerState, EnvEntryView, EnvFinding, EnvReport, ErrorInfo,
    FindingSeverity, LogLine, OperationEventKind, OperationId, PatchEvent, ProjectId,
    ProjectStatus,
};
use tokio::sync::mpsc;

use mast_docker::CommandOutcome;

use crate::diagnostics::COMPOSER_IMAGE;
use crate::lifecycle::LifecycleVerb;
use crate::ops::OpHandle;

/// The app service name Sail scaffolds into every project.
const SAIL_APP_SERVICE: &str = "laravel.test";
use crate::{Engine, ProjectEntry, Redactor, initial_summary, internal_err};

impl Engine {
    /// The tail of `storage/logs/laravel.log`, parsed into grouped entries
    /// (newest first). On demand only, like the env report — log bodies
    /// routinely carry user data. Reads at most the last 256 KiB, so a
    /// months-old multi-gigabyte log answers as fast as a fresh one.
    pub async fn laravel_log(
        &self,
        project: &ProjectId,
    ) -> Result<mast_contract::LaravelLogReport, ErrorInfo> {
        const WINDOW: u64 = 256 * 1024;
        const MAX_ENTRIES: usize = 300;
        let path = {
            let st = self.inner.state.lock().unwrap();
            st.projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?
                .record
                .path
                .clone()
        };
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let log_path = path.join("storage/logs/laravel.log");
            let Ok(mut file) = std::fs::File::open(&log_path) else {
                return Ok(mast_contract::LaravelLogReport {
                    exists: false,
                    entries: Vec::new(),
                    truncated: false,
                });
            };
            let len = file.metadata().map_err(crate::internal_err)?.len();
            let mut truncated = len > WINDOW;
            if truncated {
                file.seek(SeekFrom::End(-(WINDOW as i64))).map_err(crate::internal_err)?;
            }
            let mut body = String::new();
            // Not read_to_string: a seek can land mid-UTF-8-sequence.
            let mut bytes = Vec::with_capacity(WINDOW as usize);
            file.read_to_end(&mut bytes).map_err(crate::internal_err)?;
            body.push_str(&String::from_utf8_lossy(&bytes));
            let mut parsed = mast_laravel::log::parse_log(&body);
            if parsed.len() > MAX_ENTRIES {
                truncated = true;
                parsed.drain(..parsed.len() - MAX_ENTRIES);
            }
            parsed.reverse();
            let entries = parsed
                .into_iter()
                .map(|e| mast_contract::LaravelLogEntry {
                    timestamp: e.timestamp,
                    environment: e.environment,
                    level: e.level,
                    message: e.message,
                    detail: e.detail,
                })
                .collect();
            Ok(mast_contract::LaravelLogReport { exists: true, entries, truncated })
        })
        .await
        .map_err(crate::internal_err)?
    }

    /// Build the env editor payload: entries with secret flags, the
    /// .env/.env.example diff, and validation findings against the resolved
    /// service names. On demand only — never in snapshots/patches.
    pub async fn env_report(&self, project: &ProjectId) -> Result<EnvReport, ErrorInfo> {
        let (path, service_names) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            // Hostname checks must accept every DNS name a service answers
            // to, not just its key (container_name, network aliases).
            let services = entry
                .model
                .as_ref()
                .map(|m| {
                    m.services
                        .iter()
                        .flat_map(|s| {
                            std::iter::once(s.name.clone()).chain(s.aliases.iter().cloned())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (entry.record.path.clone(), services)
        };
        tokio::task::spawn_blocking(move || {
            let env_path = path.join(".env");
            let example_path = path.join(".env.example");
            let env_exists = env_path.is_file();
            let example_exists = example_path.is_file();
            let env_src = std::fs::read_to_string(&env_path).unwrap_or_default();
            let example_src = std::fs::read_to_string(&example_path).unwrap_or_default();
            let env = mast_laravel::EnvFile::parse(&env_src);
            let example = mast_laravel::EnvFile::parse(&example_src);
            let example_keys: Vec<String> =
                example.entries().map(|e| e.key.clone()).collect();

            let entries: Vec<EnvEntryView> = env
                .entries()
                .map(|e| EnvEntryView {
                    secret: mast_laravel::is_secret_key(&e.key),
                    in_example: example_keys.contains(&e.key),
                    key: e.key.clone(),
                    value: e.value.clone(),
                })
                .collect();
            let missing_from_env: Vec<String> = example_keys
                .iter()
                .filter(|k| env.get(k).is_none())
                .cloned()
                .collect();

            let pairs: Vec<(String, String)> =
                entries.iter().map(|e| (e.key.clone(), e.value.clone())).collect();
            let findings = mast_laravel::validate(&pairs, &service_names)
                .into_iter()
                .map(|f| EnvFinding {
                    severity: match f.severity {
                        mast_laravel::Severity::Warning => FindingSeverity::Warning,
                        mast_laravel::Severity::Error => FindingSeverity::Error,
                    },
                    key: f.key,
                    message: f.message,
                })
                .collect();

            Ok(EnvReport { env_exists, example_exists, entries, missing_from_env, findings })
        })
        .await
        .map_err(internal_err)?
    }

    pub(crate) fn project_path(&self, project: &ProjectId) -> Result<PathBuf, ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        st.projects
            .get(&project.0)
            .map(|e| e.record.path.clone())
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })
    }

    pub(crate) fn project_launch_context(
        &self,
        project: &ProjectId,
    ) -> Result<(PathBuf, Option<String>), ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        Ok((entry.record.path.clone(), st.integrations.terminal.clone()))
    }

    /// Lifecycle verbs: validated + project-locked synchronously, then run as
    /// a cancellable operation with streamed output.
    /// Import the project at `path` into the metadata store + live state
    /// (no-op if already imported).
    pub(crate) async fn import_project_at(&self, path: PathBuf) -> Result<(), ErrorInfo> {
        let engine2 = self.clone();
        let record =
            tokio::task::spawn_blocking(move || engine2.inner.deps.store.import_project(&path))
                .await
                .map_err(internal_err)?
                .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
        self.with_state(|st, events| {
            if !st.projects.contains_key(&record.id) {
                let entry = ProjectEntry {
                    summary: initial_summary(&record),
                    record,
                    invocation: None,
                    model: None,
                    redactor: Redactor::default(),
                    app_port: None,
                    host_ports: Vec::new(),
                    compose_fingerprint: None,
                };
                events.push(PatchEvent::ProjectAdded { project: entry.summary.clone() });
                st.projects.insert(entry.record.id.clone(), entry);
            }
        });
        self.hint();
        Ok(())
    }

    /// New-project wizard (M7): the flow the Sail docs describe —
    /// `composer create-project`, `composer require laravel/sail --dev`, then
    /// `php artisan sail:install --php=…`, each run in the official
    /// `composer` image so no PHP is needed on the host. Sail writes the
    /// compose file itself, so every runtime it ships (through 8.5) is
    /// reachable; the retired laravel.build endpoint stopped at 8.4.
    pub(crate) async fn create_project(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        parent: &str,
        name: &str,
        php: &str,
        services: &[String],
    ) -> Result<(), ErrorInfo> {
        // `$services` in Sail's InteractsWithDockerComposeServices.
        const ALLOWED_SERVICES: [&str; 15] = [
            "mysql", "pgsql", "mariadb", "mongodb", "redis", "valkey", "memcached",
            "meilisearch", "typesense", "minio", "rustfs", "mailpit", "rabbitmq", "selenium",
            "soketi",
        ];
        let valid_name = !name.is_empty()
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !valid_name {
            return Err(ErrorInfo::InvalidInput {
                message: "project name must be lowercase letters, digits, - or _".into(),
            });
        }
        // Runtimes Sail ships in vendor/laravel/sail/runtimes.
        if !["80", "81", "82", "83", "84", "85"].contains(&php) {
            return Err(ErrorInfo::InvalidInput { message: format!("unsupported PHP series {php}") });
        }
        // sail:install takes a dotted version ("8.5"), not our wire form.
        let php_dotted = format!("{}.{}", &php[0..1], &php[1..2]);
        let mut build_services: Vec<&String> = Vec::new();
        for service in services {
            if ALLOWED_SERVICES.contains(&service.as_str()) {
                build_services.push(service);
            } else {
                return Err(ErrorInfo::InvalidInput {
                    message: format!("unknown Sail service \"{service}\""),
                });
            }
        }
        let parent_dir = PathBuf::from(parent);
        if !parent_dir.is_dir() {
            return Err(ErrorInfo::InvalidInput {
                message: format!("{parent} is not a directory"),
            });
        }
        let target = parent_dir.join(name);
        if target.exists() {
            return Err(ErrorInfo::InvalidInput {
                message: format!("{} already exists", target.display()),
            });
        }
        if !self.inner.state.lock().unwrap().docker.available {
            return Err(ErrorInfo::InvalidInput {
                message: "docker is not reachable — the installer runs in a container".into(),
            });
        }

        let redactor = Redactor::default();
        let (uid, gid) = crate::diagnostics::uid_gid();
        // Composer writes its cache to $COMPOSER_HOME; the default is
        // unwritable when the container runs as the host user.
        let composer_run = |mount: &Path, entrypoint: Option<&str>| -> Vec<String> {
            let mut argv: Vec<String> = ["docker", "run", "--rm", "-u"].map(String::from).into();
            argv.push(format!("{uid}:{gid}"));
            argv.extend(["-e".into(), "COMPOSER_HOME=/tmp".into(), "-v".into()]);
            argv.push(format!("{}:/app", mount.display()));
            argv.extend(["-w".into(), "/app".into()]);
            if let Some(entrypoint) = entrypoint {
                argv.extend(["--entrypoint".into(), entrypoint.into()]);
            }
            argv.push(COMPOSER_IMAGE.into());
            argv
        };
        // Long: the first run pulls the image and the whole framework.
        let long = Duration::from_secs(30 * 60);
        let app_service = format!("{name}.test");

        // Everything that writes into `target` runs here so one failure — a
        // cancel included — can undo the lot. Cleanup is only safe because the
        // existence check above refused to start unless the path was free, so
        // nothing under it predates this operation.
        let scaffold: Result<(), ErrorInfo> = async {
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!(
                        "creating {name} in {parent} (first run pulls the composer image)"
                    ),
                    stderr: false,
                },
            );
            let mut create = composer_run(&parent_dir, None);
            create.extend(
                ["create-project", "laravel/laravel", name, "--no-interaction"].map(String::from),
            );
            self.run_streamed_command(handle, op, &create, None, &redactor, long).await?;

            if !target.is_dir() {
                return Err(ErrorInfo::Internal {
                    message: format!("composer finished but {} was not created", target.display()),
                });
            }

            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: "requiring laravel/sail".into(),
                    stderr: false,
                },
            );
            let mut require = composer_run(&target, None);
            require
                .extend(["require", "laravel/sail", "--dev", "--no-interaction"].map(String::from));
            self.run_streamed_command(handle, op, &require, None, &redactor, long).await?;

            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!("sail:install --php={php_dotted}"),
                    stderr: false,
                },
            );
            let mut install = composer_run(&target, Some("php"));
            install.extend(["artisan", "sail:install"].map(String::from));
            if !build_services.is_empty() {
                let with = build_services.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
                install.push(format!("--with={with}"));
            }
            install.push(format!("--php={php_dotted}"));
            install.push("--no-interaction".into());
            self.run_streamed_command(handle, op, &install, None, &redactor, long).await?;

            // Sail names the app service `laravel.test` in every project it
            // scaffolds, which is ambiguous the moment you run more than one.
            // Rename it after the app itself, and point APP_SERVICE at the new
            // name so the `sail` script still finds the container.
            //
            // WWWUSER/WWWGROUP are exported by that same `sail` script rather
            // than by compose. Mast drives `docker compose` directly, so without
            // them the build arg and container env interpolate empty and files
            // written inside the container land with the wrong owner on Linux.
            // Written before the rename so compose validation sees them set.
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!(
                        "setting APP_SERVICE={app_service}, WWWUSER={uid}, WWWGROUP={gid}"
                    ),
                    stderr: false,
                },
            );
            let env_path = target.join(".env");
            let backups = self.inner.deps.store.backups_dir();
            tokio::task::spawn_blocking({
                let app_service = app_service.clone();
                move || {
                    mast_laravel::edit_env_file(&env_path, Some(&backups), |f| {
                        f.set("APP_SERVICE", &app_service)?;
                        f.set("WWWUSER", &uid.to_string())?;
                        f.set("WWWGROUP", &gid.to_string())
                    })
                }
            })
            .await
            .map_err(internal_err)?
            .map_err(crate::env_write_error)?;

            self.rename_service(handle, op, &target, SAIL_APP_SERVICE, &app_service).await
        }
        .await;
        self.discard_scaffold_on_error(handle, op, &target, scaffold).await?;

        self.import_project_at(target.clone()).await?;

        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("{name} created and imported"),
                stderr: false,
            },
        );
        Ok(())
    }

    /// Undo a failed or cancelled scaffold by removing the directory it was
    /// building.
    ///
    /// Only ever called on a path [`create_project`] refused to touch unless it
    /// was free, so the whole tree was written by this operation — there is no
    /// pre-existing work to lose. A cleanup failure is reported but never
    /// replaces the error that caused it.
    async fn discard_scaffold_on_error(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        target: &Path,
        result: Result<(), ErrorInfo>,
    ) -> Result<(), ErrorInfo> {
        let Err(err) = result else { return Ok(()) };
        if target.is_dir() {
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output {
                    line: format!("removing the partly-created {}", target.display()),
                    stderr: false,
                },
            );
            let path = target.to_path_buf();
            let removed =
                tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await;
            if let Ok(Err(e)) = removed {
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("could not remove {}: {e}", target.display()),
                        stderr: true,
                    },
                );
            }
        }
        Err(err)
    }

    /// Rename a compose service through the full write transaction (validated
    /// by `docker compose`, backed up, refused on an external edit). Used on
    /// the freshly scaffolded project, which is not imported yet, so the
    /// invocation is resolved straight from the directory.
    async fn rename_service(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<(), ErrorInfo> {
        let env = self.inner.deps.process_env.clone();
        let invocation = tokio::task::spawn_blocking({
            let dir = dir.to_path_buf();
            move || mast_compose::resolve_invocation(&dir, &env)
        })
        .await
        .map_err(internal_err)?
        .map_err(internal_err)?;
        let file = invocation
            .files
            .first()
            .map(|f| f.path.clone())
            .ok_or(ErrorInfo::Internal { message: "invocation has no files".into() })?;

        let edits = [mast_yaml_edit::Edit::RenameKey {
            path: vec![mast_yaml_edit::key("services"), mast_yaml_edit::key(from)],
            to: to.to_string(),
        }];
        let backups = self.inner.deps.store.backups_dir();
        mast_compose::apply_compose_edit(&invocation, &file, &edits, Some(&backups))
            .await
            .map_err(|e| match e {
                mast_compose::ComposeEditError::ConflictExternalEdit => {
                    ErrorInfo::Conflict { message: e.to_string() }
                }
                other => ErrorInfo::InvalidInput { message: other.to_string() },
            })?;
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("renamed service {from} to {to} in {}", file.display()),
                stderr: false,
            },
        );
        Ok(())
    }

    pub(crate) fn process_context(
        &self,
        project: &ProjectId,
    ) -> Result<(ComposeInvocation, PathBuf, Redactor), ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let invocation = entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
            message: "project not resolved yet".into(),
        })?;
        Ok((invocation, entry.record.path.clone(), entry.redactor.clone()))
    }

    /// Start a Laravel app process: `sail artisan …` (terminal parity) for
    /// sail projects, `docker compose exec -T <app> php artisan …` otherwise.
    /// Streams until the process exits or the operation is cancelled.
    pub(crate) async fn start_process(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        process: &str,
    ) -> Result<(), ErrorInfo> {
        let def = mast_laravel::processes::process_def(process).ok_or_else(|| {
            ErrorInfo::InvalidInput { message: format!("unknown process {process}") }
        })?;
        let (invocation, dir, redactor) = self.process_context(project)?;
        let app_service = app_service_of(&dir);
        let argv = match &invocation.runner {
            mast_compose::Runner::Sail { script } => {
                let mut argv = vec![script.to_string_lossy().into_owned(), "artisan".into()];
                argv.extend(def.artisan.iter().map(|s| s.to_string()));
                argv
            }
            mast_compose::Runner::DockerCompose => {
                let mut tail = vec!["php".to_string(), "artisan".to_string()];
                tail.extend(def.artisan.iter().map(|s| s.to_string()));
                compose_exec_argv(&invocation, &app_service, &tail)
            }
        };
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output { line: format!("$ {}", argv.join(" ")), stderr: false },
        );
        // Processes run until stopped; a week ≈ unbounded.
        self.run_streamed_command(handle, op, &argv, Some(&dir), &redactor, Duration::from_secs(7 * 24 * 3600))
            .await?;
        self.hint();
        Ok(())
    }

    /// Stop a Laravel app process by cmdline match inside the app container —
    /// SIGTERM, catching terminal-started instances too (killing a
    /// `docker exec` client never kills the in-container process).
    pub(crate) async fn stop_process(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        process: &str,
    ) -> Result<(), ErrorInfo> {
        let def = mast_laravel::processes::process_def(process).ok_or_else(|| {
            ErrorInfo::InvalidInput { message: format!("unknown process {process}") }
        })?;
        let (invocation, dir, _redactor) = self.process_context(project)?;
        let app_service = app_service_of(&dir);
        let tail = vec![
            "sh".to_string(),
            "-c".to_string(),
            mast_laravel::processes::kill_script(def.pattern),
        ];
        let argv = compose_exec_argv(&invocation, &app_service, &tail);
        let out = mast_docker::run_command(&argv, Some(&dir), &[], Duration::from_secs(15), 64 * 1024)
            .await
            .map_err(internal_err)?;
        if !out.success() {
            let detail = if out.stderr.trim().is_empty() {
                format!("exec exited with status {}", out.status)
            } else {
                out.stderr.trim().to_string()
            };
            return Err(ErrorInfo::Internal { message: format!("stop failed: {detail}") });
        }
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("{} signalled to stop", def.title),
                stderr: false,
            },
        );
        self.hint();
        Ok(())
    }

    /// Run one user-defined command (M7.5): whitespace-split argv, no shell;
    /// `sail` resolves to vendor/bin/sail; cwd = project dir; streamed +
    /// cancellable (dev servers run until stopped).
    pub(crate) async fn run_project_command(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        name: &str,
    ) -> Result<(), ErrorInfo> {
        let (path, redactor, command, cwd) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            let cmd = entry
                .record
                .commands
                .iter()
                .find(|c| c.name == name)
                .ok_or(ErrorInfo::NotFound { what: format!("command \"{name}\"") })?;
            (
                entry.record.path.clone(),
                entry.redactor.clone(),
                cmd.command.clone(),
                cmd.cwd.clone(),
            )
        };
        let mut argv: Vec<String> = command.split_whitespace().map(String::from).collect();
        if argv.is_empty() {
            return Err(ErrorInfo::InvalidInput { message: "empty command".into() });
        }
        // A relative override walks from the project; absolute stands alone.
        // Resolved at run time, not save time — the sibling repo may be
        // cloned after the command is.
        let run_dir = match &cwd {
            Some(dir) => {
                let resolved = path.join(dir);
                let resolved = resolved.canonicalize().map_err(|_| ErrorInfo::InvalidInput {
                    message: format!(
                        "working directory {} does not exist (from \"{dir}\")",
                        resolved.display()
                    ),
                })?;
                if !resolved.is_dir() {
                    return Err(ErrorInfo::InvalidInput {
                        message: format!("{} is not a directory", resolved.display()),
                    });
                }
                resolved
            }
            None => path.clone(),
        };
        if argv[0] == "sail" {
            if cwd.is_some() {
                return Err(ErrorInfo::InvalidInput {
                    message: "sail commands only work from the project root — leave the \
                              working directory empty"
                        .into(),
                });
            }
            let script = path.join("vendor/bin/sail");
            if !script.is_file() {
                return Err(ErrorInfo::InvalidInput {
                    message: "vendor/bin/sail not found — bootstrap the project first \
                              (Diagnostics offers a containerized composer install)"
                        .into(),
                });
            }
            argv[0] = script.to_string_lossy().into_owned();
        }
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: if run_dir == path {
                    format!("$ {command}")
                } else {
                    format!("$ {command}  (in {})", run_dir.display())
                },
                stderr: false,
            },
        );
        // A week ≈ unbounded: dev servers run until the user stops them.
        self.run_streamed_command(handle, op, &argv, Some(&run_dir), &redactor, Duration::from_secs(7 * 24 * 3600))
            .await
    }

    pub(crate) fn dispatch_lifecycle(
        &self,
        project: ProjectId,
        verb: LifecycleVerb,
        service: Option<String>,
    ) -> Result<OperationId, ErrorInfo> {
        let (invocation, redactor, project_name, orphan_container, orphan_running) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            let invocation = entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
                message: entry
                    .summary
                    .resolution_error
                    .clone()
                    .unwrap_or_else(|| "project not resolved yet".into()),
            })?;
            // An orphaned service (observed container, gone from the compose
            // file) cannot be addressed through compose — its verb goes
            // straight to the docker CLI against the container.
            let orphan_container = service.as_deref().and_then(|name| {
                entry
                    .summary
                    .services
                    .iter()
                    .find(|s| s.orphaned && s.name == name)
                    .and_then(|s| s.container_id.clone())
            });
            // A whole-project stop must reach the leftovers too, or "Stop"
            // leaves half the project running (the post-git-pull trap).
            let orphan_running: Vec<(String, String)> =
                if service.is_none() && verb == LifecycleVerb::Stop {
                    entry
                        .summary
                        .services
                        .iter()
                        .filter(|s| {
                            s.orphaned
                                && matches!(
                                    s.state,
                                    Some(
                                        ContainerState::Running
                                            | ContainerState::Restarting
                                            | ContainerState::Paused
                                    )
                                )
                        })
                        .filter_map(|s| s.container_id.clone().map(|id| (s.name.clone(), id)))
                        .collect()
                } else {
                    Vec::new()
                };
            (
                invocation,
                entry.redactor.clone(),
                entry.summary.name.clone(),
                orphan_container,
                orphan_running,
            )
        };
        if orphan_container.is_some() && verb == LifecycleVerb::Rebuild {
            return Err(ErrorInfo::InvalidInput {
                message: format!(
                    "{} is no longer in the compose file — Rebuild the whole project to \
                     replace it",
                    service.as_deref().unwrap_or_default()
                ),
            });
        }
        // A deliberate new operation supersedes any crash notice.
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
            // Journaled for crash recovery; removed on any terminal event.
            // (Effect context — small sync writes to the metadata store.)
            let _ = engine.inner.deps.store.journal_push(mast_project::OperationJournalEntry {
                operation: id.0,
                project_id: project.0.clone(),
                verb: match &service {
                    Some(service) => format!("{} {service}", verb.label()),
                    None => verb.label().to_string(),
                },
                started_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            engine.emit_op(&handle, id, OperationEventKind::Started);
            // Whole-project starts flip status optimistically; per-service
            // verbs let observation settle the service's state instead. A
            // rebuild ends with everything up, and its port preflight matters
            // most of all — a stale config is exactly when ports moved.
            if service.is_none()
                && matches!(
                    verb,
                    LifecycleVerb::Up | LifecycleVerb::Restart | LifecycleVerb::Rebuild
                )
            {
                engine.with_state(|st, events| {
                    if let Some(entry) = st.projects.get_mut(&project.0)
                        && entry.summary.status != ProjectStatus::Starting
                    {
                        entry.summary.status = ProjectStatus::Starting;
                        events.push(PatchEvent::ProjectStatusChanged {
                            id: project.clone(),
                            status: ProjectStatus::Starting,
                        });
                    }
                });
                engine.preflight_ports(&handle, id, &project, "").await;
            }

            // Capture before the command, not after: `restart` reuses the
            // container but `rebuild` recreates it, and a removed container
            // takes its log with it. `Up` is excluded — there is nothing to
            // post-mortem about a container that is about to start.
            if matches!(verb, LifecycleVerb::Stop | LifecycleVerb::Restart | LifecycleVerb::Rebuild)
            {
                let reason = CaptureReason::Teardown { verb: verb.label().to_string() };
                let requests = match &service {
                    Some(service) => {
                        engine.capture_request(&project, service, reason).into_iter().collect()
                    }
                    None => engine.capture_requests_for_project(&project, reason),
                };
                for request in requests {
                    engine.run_capture(request).await;
                }
            }

            let (line_tx, mut line_rx) = mpsc::channel::<mast_docker::OutputLine>(256);
            let forwarder = {
                let engine = engine.clone();
                let handle = handle.clone();
                let redactor = redactor.clone();
                tokio::spawn(async move {
                    while let Some(line) = line_rx.recv().await {
                        engine.emit_op(
                            &handle,
                            id,
                            OperationEventKind::Output {
                                line: redactor.redact(&line.line),
                                stderr: line.stderr,
                            },
                        );
                    }
                })
            };

            let label = match &service {
                Some(service) => {
                    format!("{} {project_name} ({service})", capitalize(verb.label()))
                }
                None => format!("{} {project_name}", capitalize(verb.label())),
            };
            let result = crate::history::with_context(
                crate::history::CommandContext {
                    label,
                    project: Some(project.clone()),
                    operation: Some(id),
                },
                async {
                    if let Some(container_id) = &orphan_container {
                        // The file no longer knows this service — drive the
                        // container itself.
                        return engine
                            .inner
                            .deps
                            .runner
                            .run_container(
                                verb,
                                container_id,
                                line_tx.clone(),
                                handle.cancel.clone(),
                            )
                            .await;
                    }
                    let mut outcome = engine
                        .inner
                        .deps
                        .runner
                        .run(
                            &invocation,
                            verb,
                            service.as_deref(),
                            line_tx.clone(),
                            handle.cancel.clone(),
                        )
                        .await;
                    for (name, container_id) in &orphan_running {
                        let _ = line_tx
                            .send(mast_docker::OutputLine {
                                line: format!("stopping {name} (no longer in the compose file)"),
                                stderr: false,
                            })
                            .await;
                        match engine
                            .inner
                            .deps
                            .runner
                            .run_container(
                                LifecycleVerb::Stop,
                                container_id,
                                line_tx.clone(),
                                handle.cancel.clone(),
                            )
                            .await
                        {
                            Ok(CommandOutcome::Exited(0)) => {}
                            Ok(CommandOutcome::Cancelled) => {
                                outcome = Ok(CommandOutcome::Cancelled);
                                break;
                            }
                            // The compose verb's own failure stays the
                            // headline; a leftover that would not stop only
                            // fails an otherwise-clean operation.
                            Ok(CommandOutcome::Exited(code)) => {
                                if matches!(outcome, Ok(CommandOutcome::Exited(0))) {
                                    outcome = Err(format!(
                                        "failed to stop leftover container {name} (exit {code})"
                                    ));
                                }
                            }
                            Err(e) => {
                                if matches!(outcome, Ok(CommandOutcome::Exited(0))) {
                                    outcome =
                                        Err(format!("failed to stop leftover container {name}: {e}"));
                                }
                            }
                        }
                    }
                    outcome
                },
            )
            .await;
            drop(line_tx);
            let _ = forwarder.await;

            let kind = match result {
                Ok(CommandOutcome::Exited(0)) => OperationEventKind::Completed,
                Ok(CommandOutcome::Exited(code)) => OperationEventKind::Failed {
                    error: format!("{} exited with status {code}", verb.label()),
                },
                Ok(CommandOutcome::Cancelled) => OperationEventKind::Cancelled,
                Err(e) => OperationEventKind::Failed { error: redactor.redact(&e) },
            };
            // A failing verb owes its explanation (and Fix button, when a
            // signature maps to a repair) before the terminal event.
            if matches!(kind, OperationEventKind::Failed { .. }) {
                engine.flush_signature_explanations(&handle, id, Some(&project));
            }
            // Order matters: journal cleared and lock released BEFORE the
            // terminal event — once a client sees the terminal, dispatching a
            // follow-up verb must succeed.
            let _ = engine.inner.deps.store.journal_remove(id.0);
            engine.inner.busy_projects.lock().unwrap().remove(&project.0);
            engine.emit_op(&handle, id, kind);
            // Whatever happened, inspection is truth: reconcile settles state.
            engine.hint();
        });
        Ok(id)
    }

    /// Follow a service's container logs (plan §3: dedicated channel, never
    /// through the patch store).
    pub async fn service_logs(
        &self,
        project: &ProjectId,
        service: &str,
        tail: u32,
    ) -> Result<BoxStream<'static, LogLine>, ErrorInfo> {
        let container_id = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            entry
                .summary
                .services
                .iter()
                .find(|s| s.name == service)
                .and_then(|s| s.container_id.clone())
                .ok_or(ErrorInfo::NotFound {
                    what: format!("no container for service {service}"),
                })?
        };
        let adapter = self
            .inner
            .adapter
            .lock()
            .unwrap()
            .clone()
            .ok_or(ErrorInfo::Internal { message: "docker unavailable".into() })?;
        let stream = adapter
            .container_logs(&container_id, tail)
            .await
            .map_err(internal_err)?;
        let service = service.to_string();
        Ok(stream
            .map(move |chunk| LogLine {
                service: service.clone(),
                message: chunk.message,
                stderr: chunk.stderr,
            })
            .boxed())
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Sail's own app-service selector: `.env` `APP_SERVICE`, default
/// `laravel.test`.
/// The classic limits everyone tunes, in display order.
const PHP_INI_KEYS: [&str; 6] = [
    "memory_limit",
    "max_execution_time",
    "upload_max_filesize",
    "post_max_size",
    "max_input_vars",
    "opcache.enable",
];

impl crate::Engine {
    /// The PHP runtime as it actually is: `php -m` and the common limits via
    /// `ini_get`, both from the RUNNING app container — what the runtime
    /// loaded beats what any file promises — plus the vendored runtime files
    /// that change them, for the dialog's edit buttons.
    pub async fn php_runtime(
        &self,
        project: &ProjectId,
    ) -> Result<mast_contract::PhpRuntimeReport, ErrorInfo> {
        let (path, services) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            (entry.record.path.clone(), entry.summary.services.clone())
        };
        let app = app_service_of(&path);
        let container = services
            .iter()
            .find(|s| s.name == app && s.state == Some(mast_contract::ContainerState::Running))
            .and_then(|s| s.container_id.clone())
            .ok_or_else(|| ErrorInfo::InvalidInput {
                message: format!("{app} is not running — start the project first"),
            })?;

        let exec = |tail: Vec<String>| {
            let container = container.clone();
            async move {
                let mut argv: Vec<String> =
                    ["docker", "exec", container.as_str()].map(String::from).into();
                let label = tail.join(" ");
                argv.extend(tail);
                let out = mast_docker::run_command(
                    &argv,
                    None,
                    &[],
                    crate::diagnostics::PROBE_TIMEOUT,
                    crate::diagnostics::PROBE_CAP,
                )
                .await
                .map_err(crate::internal_err)?;
                if !out.success() {
                    return Err(ErrorInfo::Internal {
                        message: format!("{label} failed: {}", out.stderr.trim()),
                    });
                }
                Ok(out.stdout)
            }
        };

        let modules = exec(["php", "-m"].map(String::from).into()).await?;
        let mut extensions: Vec<String> = modules
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('['))
            .map(str::to_string)
            .collect();
        extensions.sort_by_key(|e| e.to_ascii_lowercase());
        extensions.dedup();

        let script = format!(
            "foreach ([{}] as $k) echo $k, '=', ini_get($k), PHP_EOL;",
            PHP_INI_KEYS.map(|k| format!("'{k}'")).join(",")
        );
        let raw =
            exec(vec!["php".into(), "-r".into(), script]).await?;
        let reported: std::collections::HashMap<&str, &str> =
            raw.lines().filter_map(|l| l.split_once('=')).collect();
        // Fixed order from the key list, not hash order — the dialog reads
        // top-down the same way every time.
        let ini = PHP_INI_KEYS
            .iter()
            .map(|key| mast_contract::PhpIniValue {
                key: (*key).to_string(),
                value: reported.get(key).unwrap_or(&"").trim().to_string(),
            })
            .collect();

        // The vendored runtime's own files, when the standard layout holds —
        // the dialog's edit buttons point straight at them.
        let runtime_file = |name: &str| {
            crate::php::runtime_context(&path)
                .map(|context| format!("{}/{name}", context.trim_end_matches('/')))
                .filter(|rel| path.join(rel).is_file())
        };
        Ok(mast_contract::PhpRuntimeReport {
            extensions,
            ini,
            ini_file: runtime_file("php.ini"),
            dockerfile: runtime_file("Dockerfile"),
        })
    }
}

pub(crate) fn app_service_of(dir: &Path) -> String {
    mast_compose::parse_env_file(&dir.join(".env"))
        .get("APP_SERVICE")
        .cloned()
        .unwrap_or_else(|| "laravel.test".to_string())
}

/// `docker compose <files/profiles> exec -T <service> <tail…>` — the exact
/// resolved invocation, non-interactive.
pub(crate) fn compose_exec_argv(
    invocation: &ComposeInvocation,
    service: &str,
    tail: &[String],
) -> Vec<String> {
    let mut argv = vec!["docker".to_string(), "compose".to_string()];
    for file in &invocation.files {
        argv.push("-f".into());
        argv.push(file.path.to_string_lossy().into_owned());
    }
    for profile in &invocation.profiles {
        argv.push("--profile".into());
        argv.push(profile.clone());
    }
    argv.extend(["exec".to_string(), "-T".to_string(), service.to_string()]);
    argv.extend(tail.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_service_comes_from_env_with_sail_default() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(app_service_of(tmp.path()), "laravel.test");
        std::fs::write(tmp.path().join(".env"), "APP_SERVICE=api\n").unwrap();
        assert_eq!(app_service_of(tmp.path()), "api");
    }

    #[test]
    fn compose_exec_argv_carries_files_profiles_and_tty_off() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("compose.yaml"), "services: {}\n").unwrap();
        std::fs::write(tmp.path().join(".env"), "COMPOSE_PROFILES=debug\n").unwrap();
        let inv =
            mast_compose::resolve_invocation(tmp.path(), &std::collections::HashMap::new())
                .unwrap();
        let argv = compose_exec_argv(&inv, "app", &["php".into(), "artisan".into()]);
        assert_eq!(argv[..2], ["docker", "compose"]);
        assert!(argv.contains(&"--profile".to_string()));
        let exec_at = argv.iter().position(|a| a == "exec").unwrap();
        assert_eq!(&argv[exec_at..], ["exec", "-T", "app", "php", "artisan"]);
    }
}
