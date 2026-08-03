//! Effective endpoint resolution per ADR-0002: never reimplement the CLI's
//! precedence — run `docker context inspect` (client-side, works offline)
//! with the target environment and parse what it computed. Precedence proven
//! empirically: DOCKER_HOST > named DOCKER_CONTEXT > persisted current >
//! default.

use std::time::Duration;

use crate::DockerError;
use crate::command::run_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSource {
    DockerHostEnv,
    DockerContextEnv,
    PersistedOrDefault,
}

#[derive(Debug, Clone)]
pub struct DockerEndpoint {
    pub context_name: String,
    pub host: String,
    pub source: EndpointSource,
}

/// Resolve the effective endpoint for the current process environment.
pub async fn resolve_endpoint() -> Result<DockerEndpoint, DockerError> {
    let source = if std::env::var_os("DOCKER_HOST").is_some_and(|v| !v.is_empty()) {
        EndpointSource::DockerHostEnv
    } else if std::env::var_os("DOCKER_CONTEXT").is_some_and(|v| !v.is_empty()) {
        EndpointSource::DockerContextEnv
    } else {
        EndpointSource::PersistedOrDefault
    };

    let argv: Vec<String> = ["docker", "context", "inspect", "--format", "{{.Name}}\t{{.Endpoints.docker.Host}}"]
        .into_iter()
        .map(String::from)
        .collect();
    let out = run_command(&argv, None, &[], Duration::from_secs(10), 64 * 1024).await?;
    if !out.success() {
        return Err(DockerError::Cli(format!(
            "docker context inspect failed (exit {}): {}",
            out.status,
            out.stderr.trim()
        )));
    }
    let line = out.stdout.lines().next().unwrap_or_default();
    let (name, host) = line
        .split_once('\t')
        .ok_or_else(|| DockerError::Cli(format!("unexpected context inspect output: {line:?}")))?;
    Ok(DockerEndpoint {
        context_name: name.trim().to_string(),
        host: host.trim().to_string(),
        source,
    })
}
