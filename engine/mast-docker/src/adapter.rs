//! `RuntimeAdapter`: the observation boundary the engine programs against.
//! `BollardAdapter` implements it over the Engine API; tests use fakes.
//! Association ground truth is the compose label contract proven in ADR-0001.

use std::collections::HashMap;

use bollard::Docker;
use futures::stream::BoxStream;
use futures::StreamExt;

use crate::{DockerEndpoint, DockerError};

pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
pub const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";
pub const COMPOSE_CONFIG_FILES_LABEL: &str = "com.docker.compose.project.config_files";
pub const COMPOSE_WORKING_DIR_LABEL: &str = "com.docker.compose.project.working_dir";
pub const COMPOSE_CONFIG_HASH_LABEL: &str = "com.docker.compose.config-hash";

/// One compose-labelled container as observed from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerObservation {
    pub id: String,
    pub name: String,
    /// `com.docker.compose.project` label.
    pub project: String,
    /// `com.docker.compose.service` label.
    pub service: String,
    pub config_files: Vec<String>,
    pub working_dir: Option<String>,
    /// Raw daemon state string (`running`, `exited`, …).
    pub state: String,
    /// Parsed from the status text: `healthy` / `unhealthy` / `starting`.
    pub health: Option<String>,
    pub config_hash: Option<String>,
}

/// Coarse observation hint: something changed, re-inspect. Inputs are hints,
/// inspection is truth (plan §3) — so events carry no payload the reducer
/// would be tempted to trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeEvent;

/// One chunk of container log output (usually a line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub message: String,
    pub stderr: bool,
}

#[async_trait::async_trait]
pub trait RuntimeAdapter: Send + Sync {
    async fn ping(&self) -> Result<(), DockerError>;
    async fn list_compose_containers(&self) -> Result<Vec<ContainerObservation>, DockerError>;
    /// Stream of change hints. Ends on connection loss; the engine reconnects
    /// with backoff and resyncs.
    async fn events(&self) -> Result<BoxStream<'static, RuntimeEvent>, DockerError>;
    /// Follow a container's log stream (last `tail` lines first). Ends when
    /// the container stops or the connection drops.
    async fn container_logs(
        &self,
        container_id: &str,
        tail: u32,
    ) -> Result<BoxStream<'static, LogChunk>, DockerError>;
}

pub struct BollardAdapter {
    docker: Docker,
}

impl BollardAdapter {
    /// Map the resolved endpoint onto a bollard transport (ADR-0002):
    /// unix + tcp/http are supported for observation; ssh/npipe/fd are not
    /// (documented degradation — CLI lifecycle is unaffected).
    pub fn connect(endpoint: &DockerEndpoint) -> Result<Self, DockerError> {
        let host = endpoint.host.as_str();
        let docker = if host.starts_with("unix://") {
            connect_unix(host)?
        } else if host.starts_with("tcp://") || host.starts_with("http://") {
            Docker::connect_with_http(host, 30, bollard::API_DEFAULT_VERSION)
                .map_err(|e| DockerError::Api(e.to_string()))?
        } else {
            return Err(DockerError::UnsupportedEndpoint(host.to_string()));
        };
        Ok(Self { docker })
    }
}

#[cfg(unix)]
fn connect_unix(host: &str) -> Result<Docker, DockerError> {
    Docker::connect_with_unix(host, 30, bollard::API_DEFAULT_VERSION)
        .map_err(|e| DockerError::Api(e.to_string()))
}

/// Windows adapter TODO: bollard has no unix-socket transport there at all
/// (npipe:// is what Docker Desktop speaks), so a unix endpoint degrades the
/// same documented way ssh/fd do — observation is lost, CLI lifecycle is not.
#[cfg(not(unix))]
fn connect_unix(host: &str) -> Result<Docker, DockerError> {
    Err(DockerError::UnsupportedEndpoint(host.to_string()))
}

fn parse_health(status_text: &str) -> Option<String> {
    let lower = status_text.to_ascii_lowercase();
    if lower.contains("(healthy)") {
        Some("healthy".into())
    } else if lower.contains("(unhealthy)") {
        Some("unhealthy".into())
    } else if lower.contains("health: starting") {
        Some("starting".into())
    } else {
        None
    }
}

#[async_trait::async_trait]
impl RuntimeAdapter for BollardAdapter {
    async fn ping(&self) -> Result<(), DockerError> {
        self.docker.ping().await.map(|_| ()).map_err(|e| DockerError::Api(e.to_string()))
    }

    async fn list_compose_containers(&self) -> Result<Vec<ContainerObservation>, DockerError> {
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert("label".into(), vec![COMPOSE_PROJECT_LABEL.into()]);
        let options = bollard::query_parameters::ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        let summaries = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|e| DockerError::Api(e.to_string()))?;

        let mut observations = Vec::with_capacity(summaries.len());
        for c in summaries {
            let labels = c.labels.unwrap_or_default();
            let Some(project) = labels.get(COMPOSE_PROJECT_LABEL).cloned() else {
                continue;
            };
            let status_text = c.status.unwrap_or_default();
            observations.push(ContainerObservation {
                id: c.id.unwrap_or_default(),
                name: c
                    .names
                    .unwrap_or_default()
                    .first()
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_default(),
                project,
                service: labels.get(COMPOSE_SERVICE_LABEL).cloned().unwrap_or_default(),
                config_files: labels
                    .get(COMPOSE_CONFIG_FILES_LABEL)
                    .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default(),
                working_dir: labels.get(COMPOSE_WORKING_DIR_LABEL).cloned(),
                state: c.state.map(|s| s.to_string().to_ascii_lowercase()).unwrap_or_default(),
                health: parse_health(&status_text),
                config_hash: labels.get(COMPOSE_CONFIG_HASH_LABEL).cloned(),
            });
        }
        Ok(observations)
    }

    async fn events(&self) -> Result<BoxStream<'static, RuntimeEvent>, DockerError> {
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert("type".into(), vec!["container".into()]);
        let options = bollard::query_parameters::EventsOptionsBuilder::default()
            .filters(&filters)
            .build();
        let stream = self
            .docker
            .events(Some(options))
            .take_while(|item| futures::future::ready(item.is_ok()))
            .map(|_| RuntimeEvent)
            .boxed();
        Ok(stream)
    }

    async fn container_logs(
        &self,
        container_id: &str,
        tail: u32,
    ) -> Result<BoxStream<'static, LogChunk>, DockerError> {
        let options = bollard::query_parameters::LogsOptionsBuilder::default()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .tail(&tail.to_string())
            .build();
        let stream = self
            .docker
            .logs(container_id, Some(options))
            .take_while(|item| futures::future::ready(item.is_ok()))
            .filter_map(|item| {
                futures::future::ready(item.ok().map(|output| {
                    let stderr =
                        matches!(output, bollard::container::LogOutput::StdErr { .. });
                    LogChunk {
                        message: String::from_utf8_lossy(&output.into_bytes())
                            .trim_end_matches(['\r', '\n'])
                            .to_string(),
                        stderr,
                    }
                }))
            })
            .boxed();
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_parses_from_status_text() {
        assert_eq!(parse_health("Up 5 seconds (healthy)"), Some("healthy".into()));
        assert_eq!(parse_health("Up 2 minutes (unhealthy)"), Some("unhealthy".into()));
        assert_eq!(parse_health("Up 1 second (health: starting)"), Some("starting".into()));
        assert_eq!(parse_health("Exited (0) 3 minutes ago"), None);
    }
}
