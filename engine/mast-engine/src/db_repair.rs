//! Engine effects for the database credential doctor (`mast_laravel::db`
//! holds the pure half): probing the live container during diagnostics
//! gathering, and the two repairs — a live reconcile when an administrative
//! login still works, and the destructive volume recreate when nothing does.
//!
//! Secrets ride as `docker compose exec -e KEY=VALUE` flags: the value never
//! reaches the container's process list (the client reads it from env), and
//! on the host it is visible only for the probe's lifetime — the same
//! exposure class as `sail mysql` itself. Everything persisted or streamed
//! goes through the project redactor, which knows every `.env` secret.

use std::sync::Arc;
use std::time::Duration;

use mast_compose::{ComposeInvocation, Runner};
use mast_contract::{ErrorInfo, ProjectId, RepairOffer, RepairPlan};
use mast_diagnostics::DbProbeFacts;
use mast_docker::run_command;
use mast_laravel::db::{
    self, AdminLogin, DbCreds, DbKind, admin_probe_tail, admin_query_tail, admin_sql_tail,
    probe_tail,
};

use crate::diagnostics::{PROBE_CAP, PROBE_TIMEOUT};
use crate::{Engine, OperationEventKind, OperationId};

/// A probeable database: the compose service to exec into and the `.env`
/// credentials to test.
pub(crate) struct DbProbeTarget {
    pub service: String,
    pub creds: DbCreds,
}

/// Match `.env` credentials to a compose service — DB_HOST must be a service
/// name or one of its aliases (custom service names answer by alias).
pub(crate) fn resolve_db_target(
    pairs: &[(String, String)],
    services: &[(String, Vec<String>)],
) -> Option<DbProbeTarget> {
    let creds = db::db_creds(pairs)?;
    let service = services
        .iter()
        .find(|(name, aliases)| *name == creds.host || aliases.contains(&creds.host))?
        .0
        .clone();
    Some(DbProbeTarget { service, creds })
}

/// `docker compose … exec -T [-e K=V…] <service> <tail…>` with the
/// invocation's files and profiles. Always bare compose — exec needs no
/// WWWUSER parity and must not trip sail's auto-down hygiene.
pub(crate) fn exec_env_argv(
    invocation: &ComposeInvocation,
    service: &str,
    env: &[(String, String)],
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
    argv.extend(["exec".to_string(), "-T".to_string()]);
    for (key, value) in env {
        argv.push("-e".into());
        argv.push(format!("{key}={value}"));
    }
    argv.push(service.to_string());
    argv.extend(tail.iter().cloned());
    argv
}

/// Compose verb argv + env for the recreate steps (`rm`/`up`), honoring the
/// project's runner. Sail projects go through the sail script for env parity,
/// but WITH `SAIL_SKIP_CHECKS=1` — unlike lifecycle verbs, a surgical repair
/// must not trigger sail's "shut down old processes" auto-`down` mid-surgery
/// (ADR-0001 finding 8.5). Plain projects get the WWWUSER parity overlay.
pub(crate) fn scoped_compose_argv(
    invocation: &ComposeInvocation,
    tail: &[&str],
) -> (Vec<String>, Vec<(String, String)>) {
    match &invocation.runner {
        Runner::Sail { script } => {
            let mut argv = vec![script.to_string_lossy().into_owned()];
            argv.extend(tail.iter().map(|s| s.to_string()));
            (argv, vec![("SAIL_SKIP_CHECKS".into(), "1".into())])
        }
        Runner::DockerCompose => {
            let mut argv = vec!["docker".to_string(), "compose".to_string()];
            for file in &invocation.files {
                argv.push("-f".into());
                argv.push(file.path.to_string_lossy().into_owned());
            }
            for profile in &invocation.profiles {
                argv.push("--profile".into());
                argv.push(profile.clone());
            }
            argv.extend(tail.iter().map(|s| s.to_string()));
            (argv, crate::lifecycle::parity_env(invocation))
        }
    }
}

async fn exec_quiet(
    invocation: &ComposeInvocation,
    service: &str,
    env: &[(String, String)],
    tail: &[String],
) -> Option<mast_docker::CommandOutput> {
    let argv = exec_env_argv(invocation, service, env, tail);
    run_command(&argv, Some(&invocation.project_dir), &[], PROBE_TIMEOUT, PROBE_CAP).await.ok()
}

/// First admin login that can run `SELECT 1` on the initialized volume.
pub(crate) async fn find_admin_login(
    invocation: &ComposeInvocation,
    service: &str,
    creds: &DbCreds,
) -> Option<AdminLogin> {
    for login in db::admin_logins(creds) {
        let (tail, env) = admin_probe_tail(creds.kind, &login);
        if exec_quiet(invocation, service, &env, &tail).await.is_some_and(|o| o.success()) {
            return Some(login);
        }
    }
    None
}

/// Probe the running service with the `.env` credentials. `None` when the
/// outcome says nothing about credentials (service not running, client
/// missing, timeout) — no finding is better than a wrong one.
pub(crate) async fn probe_db(
    invocation: &ComposeInvocation,
    target: &DbProbeTarget,
) -> Option<DbProbeFacts> {
    let creds = &target.creds;
    let facts = |failure, admin_access, migrations_table| DbProbeFacts {
        service: target.service.clone(),
        kind: creds.kind,
        database: creds.database.clone(),
        username: creds.username.clone(),
        failure,
        admin_access,
        migrations_table,
    };
    let (tail, env) = probe_tail(creds);
    let out = exec_quiet(invocation, &target.service, &env, &tail).await?;
    if out.success() {
        // The credentials work — one more question while we are here: has
        // the first `artisan migrate` ever run? A clean `0`/`1` is an
        // answer; anything else (odd shell, permission quirk) is not.
        let (tail, env) = db::migrations_probe_tail(creds);
        let migrations_table = match exec_quiet(invocation, &target.service, &env, &tail).await
        {
            Some(out) if out.success() => match out.stdout.trim() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            },
            _ => None,
        };
        return Some(facts(None, false, migrations_table));
    }
    let failure = db::classify_probe(creds.kind, &out.stderr)?;
    let admin = find_admin_login(invocation, &target.service, creds).await.is_some();
    Some(facts(Some(failure), admin, None))
}

/// Parse `docker volume ls` three-column rows into
/// (daemon volume name, compose project label, compose source label).
pub(crate) fn parse_volume_rows(stdout: &str) -> Vec<(String, String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let name = cols.next()?.trim();
            let project = cols.next()?.trim();
            let source = cols.next()?.trim();
            (!name.is_empty() && !project.is_empty() && !source.is_empty())
                .then(|| (name.to_string(), project.to_string(), source.to_string()))
        })
        .collect()
}

/// Every compose-created volume on the daemon, with its project and source
/// labels — ground truth for mapping model volume sources to real volumes,
/// with no name-derivation guessing.
pub(crate) async fn daemon_volume_rows() -> Vec<(String, String, String)> {
    let argv: Vec<String> = [
        "docker",
        "volume",
        "ls",
        "--format",
        "{{.Name}}\t{{.Label \"com.docker.compose.project\"}}\t{{.Label \"com.docker.compose.volume\"}}",
    ]
    .map(String::from)
    .into();
    match run_command(&argv, None, &[], PROBE_TIMEOUT, PROBE_CAP).await {
        Ok(out) if out.success() => parse_volume_rows(&out.stdout),
        _ => Vec::new(),
    }
}

/// Read a data volume's version marker (`PG_VERSION` for postgres,
/// `mysql_upgrade_info` for the mysql family) through a throwaway read-only
/// container. `None` when unreadable or absent — say nothing rather than
/// guess.
pub(crate) async fn volume_version_marker(volume: &str) -> Option<String> {
    let argv: Vec<String> = [
        "docker",
        "run",
        "--rm",
        "-v",
        &format!("{volume}:/mast-volume:ro"),
        "alpine:latest",
        "sh",
        "-c",
        "cat /mast-volume/PG_VERSION /mast-volume/mysql_upgrade_info 2>/dev/null || true",
    ]
    .map(String::from)
    .into();
    // Generous timeout: the first use may pull alpine (a few MB).
    let out = run_command(&argv, None, &[], Duration::from_secs(60), PROBE_CAP).await.ok()?;
    out.stdout.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from)
}

/// Per-project database-service shapes the diagnostics gather feeds into
/// [`scan_db_versions`], extracted from the resolved model under the state
/// lock.
pub(crate) struct DbServiceMeta {
    pub project_id: String,
    /// Resolved compose project name — the volume label to match.
    pub compose_name: String,
    /// (service, image, named volume sources).
    pub services: Vec<(String, String, Vec<String>)>,
}

/// Compare each database service's pinned image series against what its
/// volume was written by. Only mismatches come back; a fresh volume, an
/// unpinned tag, or an unreadable marker say nothing.
pub(crate) async fn scan_db_versions(
    metas: &[DbServiceMeta],
) -> std::collections::HashMap<String, Vec<mast_diagnostics::DbVersionIssue>> {
    let mut issues: std::collections::HashMap<String, Vec<mast_diagnostics::DbVersionIssue>> =
        Default::default();
    let any_db = metas
        .iter()
        .flat_map(|m| m.services.iter())
        .any(|(_, image, volumes)| !volumes.is_empty() && db::db_image_series(image).is_some());
    if !any_db {
        return issues;
    }
    let rows = daemon_volume_rows().await;
    for meta in metas {
        for (service, image, sources) in &meta.services {
            let Some((kind, image_series)) = db::db_image_series(image) else { continue };
            let Some((volume, _, _)) = rows
                .iter()
                .find(|(_, project, source)| {
                    *project == meta.compose_name && sources.contains(source)
                })
            else {
                continue; // no volume yet — first start initializes cleanly
            };
            let Some(marker) = volume_version_marker(volume).await else { continue };
            let Some(volume_series) = db::parse_volume_marker(&marker) else { continue };
            let Some(verdict) = db::volume_version_verdict(kind, &image_series, &volume_series)
            else {
                continue;
            };
            issues.entry(meta.project_id.clone()).or_default().push(
                mast_diagnostics::DbVersionIssue {
                    service: service.clone(),
                    image: image.clone(),
                    volume_version: db::format_series(&volume_series),
                    verdict,
                },
            );
        }
    }
    issues
}

struct DbRepairCtx {
    invocation: ComposeInvocation,
    model: mast_compose::ResolvedModel,
    path: std::path::PathBuf,
    redactor: crate::Redactor,
}

impl Engine {
    fn db_repair_ctx(&self, project: &ProjectId) -> Result<DbRepairCtx, ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let invocation = entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
            message: "the project's compose invocation is not resolved".into(),
        })?;
        let model = entry.model.clone().ok_or_else(|| ErrorInfo::InvalidInput {
            message: "the project's compose model is not resolved".into(),
        })?;
        Ok(DbRepairCtx {
            invocation,
            model,
            path: entry.record.path.clone(),
            redactor: entry.redactor.clone(),
        })
    }

    /// Fresh-from-disk credentials + service, cross-checked against the
    /// finding's `arg` — the `.env` may have moved on since the report.
    fn db_target_now(&self, ctx: &DbRepairCtx, arg: Option<&str>) -> Result<DbProbeTarget, ErrorInfo> {
        let src = std::fs::read_to_string(ctx.path.join(".env")).map_err(|_| {
            ErrorInfo::InvalidInput { message: "no .env file to reconcile against".into() }
        })?;
        let pairs: Vec<(String, String)> = mast_laravel::EnvFile::parse(&src)
            .entries()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        let services: Vec<(String, Vec<String>)> =
            ctx.model.services.iter().map(|s| (s.name.clone(), s.aliases.clone())).collect();
        let target = resolve_db_target(&pairs, &services).ok_or_else(|| ErrorInfo::Conflict {
            message: ".env no longer declares probeable database credentials — re-run \
                      diagnostics"
                .into(),
        })?;
        if let Some(arg) = arg
            && arg != target.service
        {
            return Err(ErrorInfo::Conflict {
                message: format!(
                    "DB_HOST now points at \"{}\", not \"{arg}\" — re-run diagnostics",
                    target.service
                ),
            });
        }
        Ok(target)
    }

    pub(crate) async fn db_reconcile_preview(
        &self,
        offer: RepairOffer,
        project: &ProjectId,
        arg: Option<&str>,
    ) -> Result<RepairPlan, ErrorInfo> {
        let ctx = self.db_repair_ctx(project)?;
        let target = self.db_target_now(&ctx, arg)?;
        let creds = &target.creds;

        // Statements shown with the password masked; the real SQL is built
        // fresh at apply time.
        let mut masked = creds.clone();
        masked.password = crate::REDACTED.into();
        let statements = match creds.kind {
            DbKind::Mysql | DbKind::Mariadb => db::mysql_reconcile_sql(&masked)
                .map_err(|message| ErrorInfo::InvalidInput { message })?,
            DbKind::Pgsql => format!(
                "{}\n-- then, only if missing:\n{};\nCREATE DATABASE \"testing\" OWNER \"{}\"",
                db::pg_role_sql(&masked).map_err(|message| ErrorInfo::InvalidInput { message })?,
                db::pg_create_database_sql(&masked)
                    .map_err(|message| ErrorInfo::InvalidInput { message })?,
                masked.username,
            ),
        };

        let mut summary = vec![format!(
            "log in as an administrator inside service \"{}\" and apply:",
            target.service
        )];
        summary.extend(statements.lines().map(|l| format!("  {l}")));
        summary.push("no data is modified; the .env credentials are verified afterwards".into());
        Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op: false })
    }

    pub(crate) async fn db_reconcile_apply(
        &self,
        handle: &Arc<crate::OpHandle>,
        op: OperationId,
        project: &ProjectId,
        arg: Option<&str>,
    ) -> Result<(), ErrorInfo> {
        let ctx = self.db_repair_ctx(project)?;
        let target = self.db_target_now(&ctx, arg)?;
        let creds = &target.creds;
        let service = &target.service;
        let out = |line: String| {
            self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
        };

        out(format!("probing administrative access in \"{service}\""));
        let login = find_admin_login(&ctx.invocation, service, creds).await.ok_or_else(|| {
            ErrorInfo::Conflict {
                message: "no administrative login works on this volume any more — the \
                          recreate-volume repair is the remaining option"
                    .into(),
            }
        })?;
        out(format!("administrator: {}", login.rationale));

        match creds.kind {
            DbKind::Mysql | DbKind::Mariadb => {
                let sql = db::mysql_reconcile_sql(creds)
                    .map_err(|message| ErrorInfo::InvalidInput { message })?;
                let (tail, env) = admin_sql_tail(creds.kind, &login, &sql);
                out(format!("applying database/user/grants for \"{}\"", creds.database));
                let argv = exec_env_argv(&ctx.invocation, service, &env, &tail);
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    Some(&ctx.path),
                    &ctx.redactor,
                    PROBE_TIMEOUT,
                )
                .await?;
            }
            DbKind::Pgsql => {
                let role_sql = db::pg_role_sql(creds)
                    .map_err(|message| ErrorInfo::InvalidInput { message })?;
                let (tail, env) = admin_sql_tail(creds.kind, &login, &role_sql);
                out(format!("reconciling role \"{}\"", creds.username));
                let argv = exec_env_argv(&ctx.invocation, service, &env, &tail);
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    Some(&ctx.path),
                    &ctx.redactor,
                    PROBE_TIMEOUT,
                )
                .await?;

                for database in [creds.database.as_str(), "testing"] {
                    let query = db::pg_database_missing_query(database);
                    let (tail, env) = admin_query_tail(creds.kind, &login, &query);
                    let missing = exec_quiet(&ctx.invocation, service, &env, &tail)
                        .await
                        .is_some_and(|o| o.success() && o.stdout.contains("missing"));
                    if missing {
                        let mut for_db = creds.clone();
                        for_db.database = database.to_string();
                        let create = db::pg_create_database_sql(&for_db)
                            .map_err(|message| ErrorInfo::InvalidInput { message })?;
                        let (tail, env) = admin_sql_tail(creds.kind, &login, &create);
                        out(format!("creating database \"{database}\""));
                        let argv = exec_env_argv(&ctx.invocation, service, &env, &tail);
                        self.run_streamed_command(
                            handle,
                            op,
                            &argv,
                            Some(&ctx.path),
                            &ctx.redactor,
                            PROBE_TIMEOUT,
                        )
                        .await?;
                    }
                }
            }
        }

        let (tail, env) = probe_tail(creds);
        let verified =
            exec_quiet(&ctx.invocation, service, &env, &tail).await.is_some_and(|o| o.success());
        if !verified {
            return Err(ErrorInfo::Internal {
                message: "statements applied, but the .env credentials still fail — re-run \
                          diagnostics"
                    .into(),
            });
        }
        out("verified: the .env credentials now work".into());
        self.hint();
        Ok(())
    }

    /// The service's named volumes as they exist on the daemon:
    /// (daemon name, compose source name).
    async fn db_volumes_on_daemon(
        &self,
        ctx: &DbRepairCtx,
        service: &str,
    ) -> Result<Vec<(String, String)>, ErrorInfo> {
        let sources = ctx
            .model
            .services
            .iter()
            .find(|s| s.name == service)
            .map(|s| s.volumes.clone())
            .ok_or_else(|| ErrorInfo::Conflict {
                message: format!("service \"{service}\" is no longer in the compose model"),
            })?;
        if sources.is_empty() {
            return Err(ErrorInfo::InvalidInput {
                message: format!(
                    "service \"{service}\" mounts no named volume — nothing to recreate"
                ),
            });
        }
        Ok(daemon_volume_rows()
            .await
            .into_iter()
            .filter(|(_, project, source)| {
                *project == ctx.model.name && sources.iter().any(|s| s == source)
            })
            .map(|(name, _, source)| (name, source))
            .collect())
    }

    /// What retagging `service` to `new_image` means for its existing data
    /// volume, when both sides are knowable. `None` = nothing to warn about
    /// (not a pinned db image, no volume yet, marker unreadable, same series).
    pub(crate) async fn retag_version_verdict(
        &self,
        project: &ProjectId,
        service: &str,
        new_image: &str,
    ) -> Option<(mast_laravel::db::VersionVerdict, String)> {
        let (kind, image_series) = db::db_image_series(new_image)?;
        let ctx = self.db_repair_ctx(project).ok()?;
        let sources = ctx.model.services.iter().find(|s| s.name == service)?.volumes.clone();
        if sources.is_empty() {
            return None;
        }
        let rows = daemon_volume_rows().await;
        let (volume, _, _) = rows
            .iter()
            .find(|(_, proj, source)| *proj == ctx.model.name && sources.contains(source))?;
        let marker = volume_version_marker(volume).await?;
        let volume_series = db::parse_volume_marker(&marker)?;
        let verdict = db::volume_version_verdict(kind, &image_series, &volume_series)?;
        Some((verdict, db::format_series(&volume_series)))
    }

    pub(crate) async fn db_recreate_preview(
        &self,
        offer: RepairOffer,
        project: &ProjectId,
        arg: Option<&str>,
    ) -> Result<RepairPlan, ErrorInfo> {
        let ctx = self.db_repair_ctx(project)?;
        let service = arg.ok_or_else(|| ErrorInfo::InvalidInput {
            message: "db-recreate-volume needs the service name".into(),
        })?;
        let volumes = self.db_volumes_on_daemon(&ctx, service).await?;

        let mut summary = vec![format!("stop and remove service \"{service}\"")];
        if volumes.is_empty() {
            summary.push(
                "no matching volume exists on the daemon — nothing to delete, the service \
                 will simply re-initialize"
                    .into(),
            );
        }
        for (name, _) in &volumes {
            summary.push(format!("DELETE volume \"{name}\" — every database in it is lost"));
        }
        summary.push(format!(
            "start \"{service}\" again; the image re-initializes user/password/database \
             from the current .env"
        ));
        summary.push("export anything you still need BEFORE applying this".into());
        Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op: false })
    }

    pub(crate) async fn db_recreate_apply(
        &self,
        handle: &Arc<crate::OpHandle>,
        op: OperationId,
        project: &ProjectId,
        arg: Option<&str>,
    ) -> Result<(), ErrorInfo> {
        let ctx = self.db_repair_ctx(project)?;
        let service = arg.ok_or_else(|| ErrorInfo::InvalidInput {
            message: "db-recreate-volume needs the service name".into(),
        })?;
        let volumes = self.db_volumes_on_daemon(&ctx, service).await?;
        let out = |line: String| {
            self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
        };

        let (argv, env) = scoped_compose_argv(&ctx.invocation, &["rm", "-s", "-f", service]);
        out(format!("$ {}", argv.join(" ")));
        self.run_streamed_command_env(
            handle,
            op,
            &argv,
            Some(&ctx.path),
            &env,
            &ctx.redactor,
            Duration::from_secs(5 * 60),
        )
        .await?;

        for (name, _) in &volumes {
            let argv: Vec<String> = ["docker", "volume", "rm", name].map(String::from).into();
            out(format!("$ {}", argv.join(" ")));
            self.run_streamed_command(
                handle,
                op,
                &argv,
                None,
                &ctx.redactor,
                Duration::from_secs(60),
            )
            .await?;
        }

        let (argv, env) = scoped_compose_argv(&ctx.invocation, &["up", "-d", service]);
        out(format!("$ {}", argv.join(" ")));
        self.run_streamed_command_env(
            handle,
            op,
            &argv,
            Some(&ctx.path),
            &env,
            &ctx.redactor,
            Duration::from_secs(15 * 60),
        )
        .await?;

        // The image needs a moment to run its init scripts before the fresh
        // credentials answer; give a bounded confirmation rather than a shrug.
        if let Ok(target) = self.db_target_now(&ctx, Some(service)) {
            out("waiting for the database to initialize from .env".into());
            let (tail, env) = probe_tail(&target.creds);
            for _ in 0..18 {
                if handle.cancel.is_cancelled() {
                    return Err(ErrorInfo::Internal { message: "cancelled".into() });
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                if exec_quiet(&ctx.invocation, service, &env, &tail)
                    .await
                    .is_some_and(|o| o.success())
                {
                    out("verified: the .env credentials work on the fresh volume".into());
                    self.hint();
                    return Ok(());
                }
            }
            out("still initializing — run diagnostics again in a minute to confirm".into());
        }
        self.hint();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn compose_project(dir: &std::path::Path) -> ComposeInvocation {
        std::fs::write(dir.join("compose.yaml"), "services: {}\n").unwrap();
        mast_compose::resolve_invocation(dir, &HashMap::new()).unwrap()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn db_target_resolves_by_service_name_or_alias() {
        let env = pairs(&[
            ("DB_CONNECTION", "mysql"),
            ("DB_HOST", "db.internal"),
            ("DB_DATABASE", "app"),
            ("DB_USERNAME", "sail"),
            ("DB_PASSWORD", "pw"),
        ]);
        // The user's custom-named service answers by alias (thinksolar shape).
        let services =
            vec![("thinksolar-mysql".to_string(), vec!["db.internal".to_string()])];
        let target = resolve_db_target(&env, &services).unwrap();
        assert_eq!(target.service, "thinksolar-mysql");

        // DB_HOST naming nothing in the model → no probe, no finding.
        assert!(resolve_db_target(&env, &[("redis".into(), Vec::new())]).is_none());
    }

    #[test]
    fn exec_argv_carries_env_flags_between_exec_and_service() {
        let tmp = tempfile::tempdir().unwrap();
        let inv = compose_project(tmp.path());
        let argv = exec_env_argv(
            &inv,
            "mysql",
            &[("MYSQL_PWD".into(), "secret".into())],
            &["mysql".into(), "-uroot".into()],
        );
        let exec_at = argv.iter().position(|a| a == "exec").unwrap();
        assert_eq!(
            &argv[exec_at..],
            ["exec", "-T", "-e", "MYSQL_PWD=secret", "mysql", "mysql", "-uroot"]
        );
    }

    #[test]
    fn scoped_verbs_use_sail_with_skip_checks_or_compose_with_parity() {
        let tmp = tempfile::tempdir().unwrap();
        let inv = compose_project(tmp.path());
        let (argv, env) = scoped_compose_argv(&inv, &["rm", "-s", "-f", "mysql"]);
        assert_eq!(argv[..2], ["docker", "compose"]);
        assert_eq!(&argv[argv.len() - 4..], ["rm", "-s", "-f", "mysql"]);
        #[cfg(unix)]
        assert!(env.iter().any(|(k, _)| k == "WWWUSER"), "parity overlay expected: {env:?}");

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        let bin = tmp.path().join("vendor/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("sail"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("sail"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let inv = mast_compose::resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
        let (argv, env) = scoped_compose_argv(&inv, &["up", "-d", "mysql"]);
        assert!(argv[0].ends_with("vendor/bin/sail"), "{argv:?}");
        assert_eq!(&argv[1..], ["up", "-d", "mysql"]);
        // Unlike lifecycle verbs, repairs must not trip sail's auto-down.
        assert!(env.iter().any(|(k, v)| k == "SAIL_SKIP_CHECKS" && v == "1"), "{env:?}");
    }

    #[test]
    fn volume_rows_parse_names_and_labels() {
        let rows = parse_volume_rows(
            "myapp_sail-mysql\tmyapp\tsail-mysql\nmyapp_sail-redis\tmyapp\tsail-redis\n\
             unlabeled\t\t\n\n",
        );
        assert_eq!(
            rows,
            vec![
                ("myapp_sail-mysql".to_string(), "myapp".to_string(), "sail-mysql".to_string()),
                ("myapp_sail-redis".to_string(), "myapp".to_string(), "sail-redis".to_string()),
            ]
        );
    }
}
