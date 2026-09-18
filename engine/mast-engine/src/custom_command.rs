//! Ownership for executable Sail commands. Killing the host Docker client
//! does not stop its container process, so mark the process and descendants
//! with a unique environment value and clean up only that marked family.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use mast_compose::ComposeInvocation;
use mast_contract::{ErrorInfo, OperationId, ProjectId};

pub(crate) struct ManagedCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) stop_argv: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FailedCleanup {
    pub(crate) argv: Vec<String>,
    pub(crate) dir: PathBuf,
    pub(crate) operation: OperationId,
}

impl crate::Engine {
    pub(crate) async fn retry_failed_command_cleanup(
        &self,
        project: &ProjectId,
        name: &str,
    ) -> Result<(), ErrorInfo> {
        let key = (project.0.clone(), name.to_string());
        let pending = self
            .inner
            .failed_command_cleanup
            .lock()
            .unwrap()
            .get(&key)
            .cloned();
        let Some(pending) = pending else {
            return Ok(());
        };
        crate::project_ops::run_process_stop(&pending.argv, &pending.dir).await?;
        let mut failed = self.inner.failed_command_cleanup.lock().unwrap();
        if failed.get(&key) == Some(&pending) {
            failed.remove(&key);
            if let Some(handle) = self.inner.ops.lock().unwrap().get(&pending.operation.0) {
                handle.cancel_failed.store(false, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Removal also retries a previous failed Stop, including commands whose
    /// definitions have since changed or disappeared from the manifest.
    pub(crate) async fn retry_project_command_cleanup(
        &self,
        project: &ProjectId,
    ) -> Result<(), ErrorInfo> {
        let names = self
            .inner
            .failed_command_cleanup
            .lock()
            .unwrap()
            .keys()
            .filter(|(id, _)| id == &project.0)
            .map(|(_, name)| name.clone())
            .collect::<Vec<_>>();
        for name in names {
            self.retry_failed_command_cleanup(project, &name).await?;
        }
        Ok(())
    }
}

/// Return the container executable behind the Sail verbs with predictable
/// dispatch. Compose verbs and arbitrary shell/docker commands stay intact.
fn executable(tail: &[String]) -> Option<Vec<String>> {
    let (verb, rest) = tail.split_first()?;
    let prefix: Vec<String> = match verb.as_str() {
        "artisan" | "art" | "a" => vec!["php".into(), "artisan".into()],
        "tinker" | "test" => vec!["php".into(), "artisan".into(), verb.clone()],
        "php" | "composer" | "node" | "npm" | "npx" | "pnpm" | "pnpx" | "yarn" | "bun" | "bunx" => {
            vec![verb.clone()]
        }
        _ => return None,
    };
    Some(prefix.into_iter().chain(rest.iter().cloned()).collect())
}

pub(crate) fn prepare(
    dir: &Path,
    invocation: Option<&ComposeInvocation>,
    app_service: &str,
    container: Option<&str>,
    tail: &[String],
    op: OperationId,
) -> Result<Option<ManagedCommand>, ErrorInfo> {
    let Some(command) = executable(tail) else {
        return Ok(None);
    };
    // The operation number alone can repeat after an engine restart. Include
    // this process and timestamp so an old orphan can never claim a new run.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let marker = format!("MAST_COMMAND_RUN={}-{}-{nonce}", std::process::id(), op.0);
    let env = sail_environment(dir, std::env::var("APP_ENV").ok().as_deref());
    let app_user = env
        .get("APP_USER")
        .cloned()
        .or_else(|| std::env::var("APP_USER").ok())
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| "sail".into());
    let service = env
        .get("APP_SERVICE")
        .cloned()
        .or_else(|| std::env::var("APP_SERVICE").ok())
        .filter(|service| !service.is_empty())
        .unwrap_or_else(|| app_service.to_string());
    let container = container.filter(|_| service == app_service);

    let mut argv = exec_prefix(dir, invocation, &service, &app_user, Some(&marker))?;
    argv.extend(command);
    let mut stop_argv = match container {
        // Keep the original identity if the project directory moves or the
        // Compose configuration is edited while the command is running.
        Some(container) => vec![
            "docker".into(),
            "exec".into(),
            "-u".into(),
            "root".into(),
            container.into(),
        ],
        None => exec_prefix(dir, invocation, &service, "root", None)?,
    };
    stop_argv.extend(["sh".into(), "-c".into(), stop_script(&marker)]);
    Ok(Some(ManagedCommand { argv, stop_argv }))
}

fn sail_environment(
    dir: &Path,
    app_env: Option<&str>,
) -> std::collections::HashMap<String, String> {
    let path = app_env
        .filter(|value| !value.is_empty())
        .map(|value| dir.join(format!(".env.{value}")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| dir.join(".env"));
    mast_compose::parse_env_file(&path)
}

fn exec_prefix(
    dir: &Path,
    invocation: Option<&ComposeInvocation>,
    service: &str,
    user: &str,
    marker: Option<&str>,
) -> Result<Vec<String>, ErrorInfo> {
    // Keep Unix Sail's Compose-file selection and environment loading; only
    // translate its executable alias into the equivalent explicit exec.
    #[cfg(unix)]
    let mut argv = {
        let _ = invocation;
        vec![
            dir.join("vendor/bin/sail").to_string_lossy().into_owned(),
            "exec".into(),
            "-T".into(),
        ]
    };
    #[cfg(not(unix))]
    let mut argv = {
        let _ = dir;
        let invocation = invocation.ok_or_else(|| ErrorInfo::InvalidInput {
            message: "project not resolved yet".into(),
        })?;
        let mut argv = crate::project_ops::compose_exec_argv(invocation, service, &[]);
        argv.pop();
        argv
    };
    argv.extend(["-u".into(), user.into()]);
    if let Some(marker) = marker {
        argv.extend(["-e".into(), marker.into()]);
    }
    argv.push(service.into());
    Ok(argv)
}

fn stop_script(marker: &str) -> String {
    // This value is generated above, never supplied by a user. Matching the
    // entire environment entry excludes similar command lines and other Mast
    // runs. Re-scan during shutdown to catch descendants spawned by a parent
    // while its first termination signal was being delivered.
    assert!(
        marker
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '='))
    );
    format!(
        r#"marker='{marker}'; stopped=''; remaining=10
while :; do
    alive=''
    for path in /proc/[0-9]*/environ; do
        pid=${{path#/proc/}}; pid=${{pid%/environ}}
        [ "$pid" = "$$" ] && continue
        tr '\0' '\n' 2>/dev/null < "$path" | grep -Fx "$marker" >/dev/null || continue
        alive="$alive $pid"
        case " $stopped " in *" $pid "*) ;; *) kill -TERM "$pid" 2>/dev/null; stopped="$stopped $pid";; esac
    done
    [ -z "$alive" ] && exit 0
    [ "$remaining" -gt 0 ] || break
    remaining=$((remaining - 1)); sleep 1
done
echo "command processes did not stop within 10 seconds:$alive" >&2
exit 1"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn sail_environment_selects_the_same_file_as_the_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_USER=default\n").unwrap();
        std::fs::write(
            dir.path().join(".env.testing"),
            "APP_USER=tester\nAPP_SERVICE=testing\n",
        )
        .unwrap();
        assert_eq!(
            sail_environment(dir.path(), Some("testing"))["APP_USER"],
            "tester"
        );
        assert_eq!(
            sail_environment(dir.path(), Some("missing"))["APP_USER"],
            "default"
        );
        assert_eq!(sail_environment(dir.path(), None)["APP_USER"], "default");
    }

    #[test]
    fn only_known_sail_executables_are_wrapped() {
        assert_eq!(
            executable(&tail(&["artisan", "queue:work"])),
            Some(tail(&["php", "artisan", "queue:work"]))
        );
        assert_eq!(
            executable(&tail(&["npm", "run", "dev"])),
            Some(tail(&["npm", "run", "dev"]))
        );
        assert!(executable(&tail(&["up", "-d"])).is_none());
        assert!(executable(&tail(&["exec", "app", "sh"])).is_none());
    }

    #[cfg(not(unix))]
    #[test]
    fn windows_exec_places_ownership_options_before_the_service() {
        let dir = tempfile::tempdir().unwrap();
        let compose = dir.path().join("compose.yaml");
        std::fs::write(&compose, "services: {}\n").unwrap();
        let invocation = mast_compose::resolve_invocation(dir.path(), &Default::default()).unwrap();
        let argv = exec_prefix(
            dir.path(),
            Some(&invocation),
            "app",
            "developer",
            Some("MAST_COMMAND_RUN=test"),
        )
        .unwrap();
        assert_eq!(&argv[..3], ["docker", "compose", "-f"]);
        assert_eq!(argv[3], invocation.files[0].path.to_string_lossy());
        assert_eq!(
            &argv[4..],
            [
                "exec",
                "-T",
                "-u",
                "developer",
                "-e",
                "MAST_COMMAND_RUN=test",
                "app"
            ]
        );
        assert!(exec_prefix(dir.path(), None, "app", "developer", None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sail_user_and_arguments_are_preserved_and_cleanup_keeps_container_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_USER=developer\n").unwrap();
        let run = prepare(
            dir.path(),
            None,
            "app",
            Some("original-container"),
            &tail(&["npm", "run", "dev"]),
            OperationId(7),
        )
        .unwrap()
        .unwrap();
        assert_eq!(&run.argv[1..6], ["exec", "-T", "-u", "developer", "-e"]);
        assert!(run.argv[6].starts_with("MAST_COMMAND_RUN="));
        assert_eq!(&run.argv[7..], ["app", "npm", "run", "dev"]);
        assert_eq!(
            &run.stop_argv[..7],
            [
                "docker",
                "exec",
                "-u",
                "root",
                "original-container",
                "sh",
                "-c"
            ]
        );
        assert!(run.stop_argv[7].contains(&run.argv[6]));
        let later = prepare(
            dir.path(),
            None,
            "app",
            Some("original-container"),
            &tail(&["npm", "run", "dev"]),
            OperationId(7),
        )
        .unwrap()
        .unwrap();
        assert_ne!(run.argv[6], later.argv[6]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cleanup_only_stops_the_exact_owned_environment_tag() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let token = format!("test-{}-{nonce}", std::process::id());
        let mut owned = tokio::process::Command::new("sleep")
            .arg("60")
            .env("MAST_COMMAND_RUN", &token)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut unrelated = tokio::process::Command::new("sleep")
            .arg("60")
            .env("MAST_COMMAND_RUN", format!("{token}-other"))
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let script = stop_script(&format!("MAST_COMMAND_RUN={token}"));
        let output = tokio::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(owned.wait().await.unwrap().code().is_none());
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "other command runs must not be signalled"
        );
        unrelated.kill().await.unwrap();
    }
}
