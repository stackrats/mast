//! Lifecycle shell-outs (plan §4 + ADR-0001): terminal parity is a mechanism.
//! Sail projects run `vendor/bin/sail <verb>` WITHOUT `SAIL_SKIP_CHECKS` —
//! the auto-down hygiene on exited projects is sail-normal terminal behavior.
//! Plain projects run `docker compose` with the exact resolved invocation.

use std::time::Duration;

use mast_compose::{ComposeInvocation, Runner};
use mast_docker::{CommandOutcome, OutputLine, run_streaming};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleVerb {
    Up,
    Stop,
    Restart,
    /// Pull and recreate. `restart` reuses the existing container, so it does
    /// not pick up an edited service block — a retagged image needs this.
    /// Scoped to one service it leaves dependencies alone; run project-wide
    /// it also rebuilds images and drops orphans (see [`lifecycle_argv`]).
    Rebuild,
}

impl LifecycleVerb {
    fn args(&self) -> &'static [&'static str] {
        match self {
            LifecycleVerb::Up => &["up", "-d"],
            LifecycleVerb::Stop => &["stop"],
            LifecycleVerb::Restart => &["restart"],
            LifecycleVerb::Rebuild => &["up", "-d", "--force-recreate", "--pull", "always"],
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LifecycleVerb::Up => "start",
            LifecycleVerb::Stop => "stop",
            LifecycleVerb::Restart => "restart",
            LifecycleVerb::Rebuild => "rebuild",
        }
    }
}

/// Exact argv + env overlay for a lifecycle verb on this invocation.
/// `service` scopes the verb to one compose service (trailing argument —
/// identical syntax for sail and compose).
pub fn lifecycle_argv(
    invocation: &ComposeInvocation,
    verb: LifecycleVerb,
    service: Option<&str>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut argv = match &invocation.runner {
        Runner::Sail { script } => vec![script.to_string_lossy().into_owned()],
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
            argv
        }
    };
    argv.extend(verb.args().iter().map(|s| s.to_string()));
    match service {
        Some(service) => {
            // Recreating one service must not drag its dependencies down with
            // it; for the other verbs compose already scopes to the service.
            if verb == LifecycleVerb::Rebuild {
                argv.push("--no-deps".into());
            }
            argv.push(service.to_string());
        }
        // The whole-project rebuild answers a config that changed underneath
        // the project: `--build` refreshes images built from local Dockerfiles
        // and `--remove-orphans` clears containers whose service is gone from
        // the file. Both are wrong for the scoped form — a one-service retag
        // must not rebuild or reap its neighbours.
        None => {
            if verb == LifecycleVerb::Rebuild {
                argv.push("--build".into());
                argv.push("--remove-orphans".into());
            }
        }
    }
    (argv, parity_env(invocation))
}

/// The sail wrapper exports `WWWUSER=${WWWUSER:-$UID}` / `WWWGROUP` before
/// invoking compose (ADR-0001 finding 8), so a Sail-flavored file driven
/// through bare `docker compose` — the vendorless-clone path, and EVERY sail
/// project on Windows — interpolates them to empty strings and builds a
/// container owned by the wrong user (finding 9). Mirror the wrapper: real
/// environment and `.env` win, the uid/gid default only fills the gap
/// (0/0 on Windows, where unix ownership is not a host concern and the
/// bootstrap flow pins the same values into `.env`).
pub(crate) fn parity_env(invocation: &ComposeInvocation) -> Vec<(String, String)> {
    if matches!(invocation.runner, Runner::DockerCompose) {
        let dotenv = mast_compose::parse_env_file(&invocation.project_dir.join(".env"));
        let (uid, gid) = crate::diagnostics::uid_gid();
        return [("WWWUSER", uid), ("WWWGROUP", gid)]
            .into_iter()
            .filter(|(key, _)| {
                std::env::var_os(key).is_none_or(|v| v.is_empty()) && !dotenv.contains_key(*key)
            })
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
    }
    Vec::new()
}

/// Runs lifecycle commands; injected so engine tests need no docker.
#[async_trait::async_trait]
pub trait LifecycleRunner: Send + Sync {
    async fn run(
        &self,
        invocation: &ComposeInvocation,
        verb: LifecycleVerb,
        service: Option<&str>,
        lines: mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String>;

    /// Drive one container directly with the docker CLI — the fallback for an
    /// orphaned service (observed container, no longer declared by the file),
    /// which compose verbs cannot address ("no such service"). Defaulted so
    /// test fakes that never meet an orphan need not implement it.
    async fn run_container(
        &self,
        verb: LifecycleVerb,
        container_id: &str,
        lines: mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        let _ = (verb, container_id, lines, cancel);
        Err("container-direct verbs are not supported by this runner".into())
    }

    /// Remove one container outright (`docker rm -f`) — the disposal path for
    /// orphans. Compose's own `--remove-orphans` stumbles over a leftover in
    /// a half-torn state ("is not connected to the network …"); removal by id
    /// has no such bookkeeping to trip on. Defaulted like [`Self::run_container`].
    async fn remove_container(
        &self,
        container_id: &str,
        lines: mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        let _ = (container_id, lines, cancel);
        Err("container removal is not supported by this runner".into())
    }
}

pub struct RealLifecycleRunner;

#[async_trait::async_trait]
impl LifecycleRunner for RealLifecycleRunner {
    async fn run(
        &self,
        invocation: &ComposeInvocation,
        verb: LifecycleVerb,
        service: Option<&str>,
        lines: mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        let (argv, env) = lifecycle_argv(invocation, verb, service);
        run_streaming(
            &argv,
            Some(&invocation.project_dir),
            &env,
            lines,
            cancel,
            // Generous: first `up` may pull images.
            Duration::from_secs(15 * 60),
            Duration::from_secs(8),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn run_container(
        &self,
        verb: LifecycleVerb,
        container_id: &str,
        lines: mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        let cmd = match verb {
            LifecycleVerb::Up => "start",
            LifecycleVerb::Stop => "stop",
            LifecycleVerb::Restart => "restart",
            // A single orphan has no compose service to rebuild from; the
            // dispatch layer refuses this before it gets here.
            LifecycleVerb::Rebuild => {
                return Err("an orphaned container has no compose service to rebuild".into());
            }
        };
        let argv: Vec<String> = ["docker", cmd, container_id].map(String::from).to_vec();
        run_streaming(
            &argv,
            None,
            &[],
            lines,
            cancel,
            Duration::from_secs(5 * 60),
            Duration::from_secs(8),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn remove_container(
        &self,
        container_id: &str,
        lines: mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        let argv: Vec<String> = ["docker", "rm", "-f", container_id].map(String::from).to_vec();
        run_streaming(
            &argv,
            None,
            &[],
            lines,
            cancel,
            Duration::from_secs(5 * 60),
            Duration::from_secs(8),
        )
        .await
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_compose::resolve_invocation;
    use std::collections::HashMap;

    #[test]
    fn compose_argv_carries_files_and_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("compose.yaml"), "services: {}\n").unwrap();
        std::fs::write(tmp.path().join(".env"), "COMPOSE_PROFILES=debug\n").unwrap();
        let inv = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
        let (argv, env) = lifecycle_argv(&inv, LifecycleVerb::Up, None);
        assert_eq!(argv[0], "docker");
        assert_eq!(argv[1], "compose");
        assert!(argv.contains(&"-f".to_string()));
        assert!(argv.contains(&"--profile".to_string()));
        assert!(argv.contains(&"debug".to_string()));
        assert_eq!(&argv[argv.len() - 2..], ["up", "-d"]);
        // Bare compose gets the wrapper's WWWUSER/WWWGROUP exports mirrored
        // (ADR-0001 finding 9) — nothing pins them here, so both are filled.
        #[cfg(unix)]
        {
            let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(keys, ["WWWUSER", "WWWGROUP"], "{env:?}");
        }
        #[cfg(not(unix))]
        assert!(env.is_empty());
    }

    /// A value the developer pinned in `.env` beats the uid/gid default,
    /// exactly as the sail wrapper's `${WWWUSER:-$UID}` would.
    #[cfg(unix)]
    #[test]
    fn parity_env_defers_to_dotenv_per_key() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("compose.yaml"), "services: {}\n").unwrap();
        std::fs::write(tmp.path().join(".env"), "WWWUSER=1337\n").unwrap();
        let inv = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
        let (_, env) = lifecycle_argv(&inv, LifecycleVerb::Up, None);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["WWWGROUP"], "pinned WWWUSER must not be overridden: {env:?}");
    }

    #[test]
    fn service_scoping_appends_the_trailing_service() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("compose.yaml"), "services: {}\n").unwrap();
        let inv = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
        let (argv, _) = lifecycle_argv(&inv, LifecycleVerb::Up, Some("redis"));
        assert_eq!(&argv[argv.len() - 3..], ["up", "-d", "redis"]);
        let (argv, _) = lifecycle_argv(&inv, LifecycleVerb::Restart, Some("api"));
        assert_eq!(&argv[argv.len() - 2..], ["restart", "api"]);
    }

    /// A retagged image only reaches the container through a pull and a
    /// recreate, and recreating one service must not take its dependencies
    /// down with it.
    #[test]
    fn rebuild_pulls_recreates_and_leaves_dependencies_alone() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("compose.yaml"), "services: {}\n").unwrap();
        let inv = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();

        let (argv, _) = lifecycle_argv(&inv, LifecycleVerb::Rebuild, Some("mysql"));
        assert_eq!(
            &argv[argv.len() - 7..],
            ["up", "-d", "--force-recreate", "--pull", "always", "--no-deps", "mysql"]
        );

        // Whole-project rebuild has no single service to isolate (--no-deps
        // would wrongly skip dependencies); it answers a changed config, so it
        // additionally rebuilds local images and reaps orphaned containers.
        let (argv, _) = lifecycle_argv(&inv, LifecycleVerb::Rebuild, None);
        assert!(!argv.contains(&"--no-deps".to_string()), "{argv:?}");
        assert_eq!(
            &argv[argv.len() - 7..],
            ["up", "-d", "--force-recreate", "--pull", "always", "--build", "--remove-orphans"]
        );
    }

    #[test]
    fn sail_argv_uses_the_script_without_skip_checks() {
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
        let inv = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
        let (argv, env) = lifecycle_argv(&inv, LifecycleVerb::Stop, None);
        assert!(argv[0].ends_with("vendor/bin/sail"));
        assert_eq!(&argv[1..], ["stop"]);
        // Terminal parity: lifecycle must NOT set SAIL_SKIP_CHECKS (ADR-0001).
        assert!(env.iter().all(|(k, _)| k != "SAIL_SKIP_CHECKS"));
    }
}
