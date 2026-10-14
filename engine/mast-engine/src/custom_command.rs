//! Ownership for executable Sail commands. Killing the host Docker client
//! does not stop its container process, so each run records its command's
//! PID inside the container and marks the process and descendants with a
//! unique environment value, and cleanup stops only that family.

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
    let run = format!("{}-{}-{nonce}", std::process::id(), op.0);
    let marker = format!("MAST_COMMAND_RUN={run}");
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
    argv.extend([
        "sh".into(),
        "-c".into(),
        RUN_WRAPPER.into(),
        "mast-run".into(),
        format!("{RUN_DIR}/{run}"),
    ]);
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
    stop_argv.extend(["sh".into(), "-c".into(), stop_script(&run)]);
    Ok(Some(ManagedCommand { argv, stop_argv }))
}

/// Where each run records its command's PID, inside the container. In /tmp,
/// so the record goes with the container — as does every process it names.
const RUN_DIR: &str = "/tmp/mast-runs";

/// Record the command's own PID and start time, then become the command.
///
/// The environment marker alone cannot find every process. Sail's PHP binary
/// carries a file capability (`cap_net_bind_service`, so `artisan serve` can
/// bind port 80), which makes every PHP process non-dumpable, and the environ
/// of a non-dumpable process is readable by no one without CAP_SYS_PTRACE —
/// not even its own user. `exec` keeps the PID, so the record names the
/// command itself; the start time tells it apart from an unrelated process
/// that later reuses the number. The wrapper's own comm is `sh`, so field 22
/// of its stat is the start time without parsing around a spaced name.
const RUN_WRAPPER: &str = r#"mkdir -p "${1%/*}" 2>/dev/null; echo "$$ $(cut -d' ' -f22 /proc/$$/stat)" > "$1" 2>/dev/null; shift; exec "$@""#;

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

fn stop_script(run: &str) -> String {
    // The run id is generated above, never supplied by a user. A run's
    // processes are the command the wrapper recorded (if that PID still has
    // the recorded start time), anything carrying the run's exact marker, and
    // every descendant of either — found through /proc/<pid>/stat, which
    // anyone can read, because the PHP processes under a command can be read
    // through nothing else. Matching the entire environment entry excludes
    // similar command lines and other Mast runs. Re-scan during shutdown to
    // catch descendants spawned while the first signals were being delivered.
    assert!(run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    format!(
        r#"marker='MAST_COMMAND_RUN={run}'; record='{RUN_DIR}/{run}'; tracked=''; stopped=''; remaining=10
if {{ read -r pid start < "$record" && read -r stat < "/proc/$pid/stat"; }} 2>/dev/null; then
    rest=${{stat##*) }}; set -- $rest
    [ "${{20}}" = "$start" ] && tracked=$pid
fi
while :; do
    table=''
    for path in /proc/[0-9]*/stat; do
        pid=${{path#/proc/}}; pid=${{pid%/stat}}
        [ "$pid" = "$$" ] && continue
        {{ read -r stat < "$path"; }} 2>/dev/null || continue
        rest=${{stat##*) }}; set -- $rest
        case "$1" in Z|X) continue;; esac
        table="$table$pid $2
"
        case " $tracked " in *" $pid "*) continue;; esac
        tr '\0' '\n' 2>/dev/null < "/proc/$pid/environ" | grep -Fx "$marker" >/dev/null && tracked="$tracked $pid"
    done
    grown=1
    while [ -n "$grown" ]; do
        grown=''
        while read -r pid ppid; do
            [ -n "$pid" ] || continue
            case " $tracked " in *" $pid "*) continue;; esac
            case " $tracked " in *" $ppid "*) tracked="$tracked $pid"; grown=1;; esac
        done <<EOF
$table
EOF
    done
    alive=''
    for pid in $tracked; do
        case "
$table" in *"
$pid "*) ;; *) continue;; esac
        alive="$alive $pid"
        case " $stopped " in *" $pid "*) ;; *) kill -TERM "$pid" 2>/dev/null; stopped="$stopped $pid";; esac
    done
    tracked=$alive
    if [ -z "$alive" ]; then rm -f "$record"; exit 0; fi
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
        let run_id = run.argv[6].strip_prefix("MAST_COMMAND_RUN=").unwrap();
        let record = format!("/tmp/mast-runs/{run_id}");
        assert_eq!(
            &run.argv[7..],
            [
                "app",
                "sh",
                "-c",
                RUN_WRAPPER,
                "mast-run",
                &record,
                "npm",
                "run",
                "dev"
            ]
        );
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
        assert!(run.stop_argv[7].contains(&record));
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
        let script = stop_script(&token);
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

    /// Start a prepared run in a plain container the way `sail exec` would:
    /// the run's user, marker and recording wrapper, with `command` standing
    /// in for the Sail executable.
    async fn start_in(container: &str, run: &ManagedCommand, command: &[&str]) {
        let service = run.argv.iter().position(|arg| arg == "app").unwrap();
        let mut argv: Vec<&str> = vec!["exec", "-d"];
        argv.extend(run.argv[3..service].iter().map(String::as_str));
        argv.push(container);
        argv.extend(
            run.argv[service + 1..service + 6]
                .iter()
                .map(String::as_str),
        );
        argv.extend(command);
        let exec = docker(&argv).await;
        assert!(exec.success(), "{}", exec.stderr);
    }

    // Sail's PHP processes run as `sail` and are non-dumpable — the PHP binary
    // carries a file capability — so their environment is readable by neither
    // root nor `sail`, and the marker cannot find them. A setgid copy of
    // busybox stands in for that binary: running it makes a process
    // non-dumpable the same way. (It must keep the name `busybox`, which is
    // how busybox knows to take the applet from its first argument.) Both runs start through their real argv,
    // wrapper included, and one is stopped through its real stop argv.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_reaches_command_processes_no_one_can_read() {
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
        let setgid = docker(&[
            "exec",
            &name,
            "sh",
            "-c",
            "mkdir /tmp/setgid && cp /bin/busybox /tmp/setgid/busybox \\
             && chgrp daemon /tmp/setgid/busybox && chmod 2755 /tmp/setgid/busybox",
        ])
        .await;
        assert!(setgid.success(), "{}", setgid.stderr);

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_USER=1000\n").unwrap();
        let prepared = |op| {
            prepare(
                dir.path(),
                None,
                "app",
                Some(&name),
                &tail(&["npm", "run", "dev"]),
                OperationId(op),
            )
            .unwrap()
            .unwrap()
        };
        let ours = prepared(1);
        let theirs = prepared(2);
        // A command with a child, as `artisan dev` has, beside another run.
        start_in(
            &name,
            &ours,
            &[
                "/tmp/setgid/busybox",
                "sh",
                "-c",
                "/tmp/setgid/busybox sleep 300 & wait",
            ],
        )
        .await;
        start_in(&name, &theirs, &["/tmp/setgid/busybox", "sleep", "301"]).await;
        let record =
            ours.argv[ours.argv.iter().position(|arg| arg == "mast-run").unwrap() + 1].clone();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let ps = docker(&["exec", &name, "ps", "-o", "args"]).await;
            let recorded = docker(&["exec", &name, "test", "-s", &record]).await;
            if ps.stdout.contains("busybox sleep 300")
                && ps.stdout.contains("sleep 301")
                && recorded.success()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "runs did not start:\n{}",
                ps.stdout
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // The case this guards: the command's own user cannot read its
        // environment, so only the record and the process tree can find it.
        let unreadable = docker(&[
            "exec",
            "-u",
            "1000",
            &name,
            "sh",
            "-c",
            &format!("read -r pid start < {record} && ! cat /proc/$pid/environ >/dev/null 2>&1"),
        ])
        .await;
        assert!(
            unreadable.success(),
            "the command's environment should be unreadable"
        );

        let stopped = mast_docker::run_command(
            &ours.stop_argv,
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
            "the command or its child is still running after a successful stop:\n{}",
            ps.stdout
        );
        assert!(
            ps.stdout.contains("sleep 301"),
            "another run's process was stopped:\n{}",
            ps.stdout
        );
        let kept = docker(&["exec", &name, "test", "-e", &record]).await;
        assert!(!kept.success(), "a completed stop removes the run's record");
    }
}
