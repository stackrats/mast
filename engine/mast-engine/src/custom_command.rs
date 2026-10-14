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

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum CleanupTarget {
    Command(String),
    Process(String),
}

impl crate::Engine {
    /// Returns whether a remembered cleanup was completed. Command names and
    /// built-in process names have separate identities even when both match.
    pub(crate) async fn retry_failed_cleanup(
        &self,
        project: &ProjectId,
        target: CleanupTarget,
    ) -> Result<bool, ErrorInfo> {
        let key = (project.0.clone(), target);
        let pending = self.inner.failed_cleanup.lock().unwrap().get(&key).cloned();
        let Some(pending) = pending else {
            return Ok(false);
        };
        crate::project_ops::run_process_stop(&pending.argv, &pending.dir).await?;
        let mut failed = self.inner.failed_cleanup.lock().unwrap();
        if failed.get(&key) == Some(&pending) {
            failed.remove(&key);
            if let Some(handle) = self.inner.ops.lock().unwrap().get(&pending.operation.0) {
                handle.cancel_failed.store(false, Ordering::Relaxed);
            }
        }
        Ok(true)
    }

    /// Removal also retries a previous failed Stop, including commands whose
    /// definitions have since changed or disappeared from the manifest.
    pub(crate) async fn retry_project_cleanup(&self, project: &ProjectId) -> Result<(), ErrorInfo> {
        let targets = self
            .inner
            .failed_cleanup
            .lock()
            .unwrap()
            .keys()
            .filter(|(id, _)| id == &project.0)
            .map(|(_, target)| target.clone())
            .collect::<Vec<_>>();
        for target in targets {
            self.retry_failed_cleanup(project, target).await?;
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
    // Stop as the user the command ran as, never as root. Reading another
    // user's /proc/<pid>/environ takes CAP_SYS_PTRACE, which Docker's default
    // capability set leaves out — so root sees none of the command's
    // processes, finds no marker, and reports a clean stop over a command that
    // is still running. The command's own user can read and signal all of it.
    let mut stop_argv = match container {
        // Keep the original identity if the project directory moves or the
        // Compose configuration is edited while the command is running.
        Some(container) => vec![
            "docker".into(),
            "exec".into(),
            "-u".into(),
            app_user.clone(),
            container.into(),
        ],
        None => exec_prefix(dir, invocation, &service, &app_user, None)?,
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
                "developer",
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

    async fn docker(args: &[&str]) -> mast_docker::CommandOutput {
        let argv: Vec<String> = std::iter::once("docker")
            .chain(args.iter().copied())
            .map(String::from)
            .collect();
        mast_docker::run_command(
            &argv,
            None,
            &[],
            std::time::Duration::from_secs(120),
            64 * 1024,
        )
        .await
        .expect("docker ran")
    }

    async fn docker_usable() -> bool {
        if std::env::var_os("MAST_SKIP_DOCKER_TESTS").is_some() {
            return false;
        }
        mast_docker::run_command(
            &[
                "docker".into(),
                "info".into(),
                "--format".into(),
                "{{.OSType}}".into(),
            ],
            None,
            &[],
            std::time::Duration::from_secs(10),
            16 * 1024,
        )
        .await
        .map(|out| out.success() && out.stdout.trim() == "linux")
        .unwrap_or(false)
    }

    struct RemoveContainer(String);

    impl Drop for RemoveContainer {
        fn drop(&mut self) {
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", &self.0])
                .output();
        }
    }

    // The test above runs the stop script as the user that owns the
    // processes, which is the case that always worked. Sail's are owned by
    // `sail`, inside a container where root lacks CAP_SYS_PTRACE and cannot
    // read their environment — so this drives the real stop argv against a
    // real container, with the command owned by a non-root user.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_reaches_a_non_root_command_inside_a_container() {
        if !docker_usable().await {
            eprintln!("skipping: no usable docker daemon");
            return;
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("mast-it-stop-{nonce}");
        let _cleanup = RemoveContainer(name.clone());
        let started = docker(&[
            "run",
            "-d",
            "--name",
            &name,
            "alpine:latest",
            "sleep",
            "600",
        ])
        .await;
        assert!(started.success(), "{}", started.stderr);

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_USER=1000\n").unwrap();
        let run = prepare(
            dir.path(),
            None,
            "app",
            Some(&name),
            &tail(&["npm", "run", "dev"]),
            OperationId(1),
        )
        .unwrap()
        .unwrap();
        let marker = run
            .argv
            .iter()
            .find(|arg| arg.starts_with("MAST_COMMAND_RUN="))
            .unwrap()
            .clone();
        // What `sail exec -T -u <user> -e <marker>` leaves running, plus a
        // neighbour from another run that must survive.
        let other = format!("{marker}-other");
        for (tag, seconds) in [(&marker, "300"), (&other, "301")] {
            let exec = docker(&[
                "exec", "-d", "-u", "1000", "-e", tag, &name, "sleep", seconds,
            ])
            .await;
            assert!(exec.success(), "{}", exec.stderr);
        }

        let stopped = mast_docker::run_command(
            &run.stop_argv,
            None,
            &[],
            std::time::Duration::from_secs(30),
            64 * 1024,
        )
        .await
        .unwrap();
        assert!(stopped.success(), "{}", stopped.stderr);
        let ps = docker(&["exec", &name, "ps", "-o", "args"]).await;
        assert!(
            !ps.stdout.contains("sleep 300"),
            "the command is still running after a successful stop:\n{}",
            ps.stdout
        );
        assert!(
            ps.stdout.contains("sleep 301"),
            "another run's process was stopped:\n{}",
            ps.stdout
        );
    }
}
