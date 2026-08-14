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
    /// Parsed from the status text of an exited container: `Exited (1) …`.
    /// Only the log-capture path reads it — a crash worth capturing is worth
    /// labelling with the code it died on.
    pub exit_code: Option<i32>,
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

/// One line read out of a container's retained log, for a capture. Unlike
/// [`LogChunk`] it carries Docker's own timestamp: a capture is read after the
/// fact, so "when did this happen" is not answerable from arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedLine {
    /// Docker's RFC3339 stamp, verbatim. `None` if the line arrived without
    /// one — clients render the capture's own time instead.
    pub at: Option<String>,
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
    /// Read a bounded slice of what Docker still holds for a container, newest
    /// `max_lines` since `since_unix`. Does not follow: this is the
    /// post-mortem read, and for an exited container it returns its final
    /// life. The evidence survives until the container is *removed*, which is
    /// why a capture taken before `up -d --force-recreate` must be taken
    /// before the command, not after.
    async fn container_log_tail(
        &self,
        container_id: &str,
        since_unix: i64,
        max_lines: u32,
    ) -> Result<Vec<CapturedLine>, DockerError>;
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

/// `Exited (137) 3 minutes ago` → `137`. Anything else yields `None`, which
/// reads as "it stopped and Docker did not say why".
fn parse_exit_code(status_text: &str) -> Option<i32> {
    let rest = status_text.trim().strip_prefix("Exited (")?;
    let (code, _) = rest.split_once(')')?;
    code.trim().parse().ok()
}

/// Split Docker's `--timestamps` prefix off a log line. The stamp is kept as
/// an opaque string: clients parse RFC3339 natively, so the engine gains
/// nothing from a date crate here.
fn split_timestamp(line: &str) -> (Option<String>, &str) {
    match line.split_once(' ') {
        // A stamp, not a first word that happens to contain a dash.
        Some((stamp, rest)) if stamp.len() >= 20 && stamp.ends_with('Z') && stamp.contains('T') => {
            (Some(stamp.to_string()), rest)
        }
        _ => (None, line),
    }
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
                exit_code: parse_exit_code(&status_text),
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

    async fn container_log_tail(
        &self,
        container_id: &str,
        since_unix: i64,
        max_lines: u32,
    ) -> Result<Vec<CapturedLine>, DockerError> {
        let options = bollard::query_parameters::LogsOptionsBuilder::default()
            .follow(false)
            .stdout(true)
            .stderr(true)
            .timestamps(true)
            .since(since_unix as i32)
            .tail(&max_lines.to_string())
            .build();
        let mut stream = self.docker.logs(container_id, Some(options));
        let mut lines = Vec::new();
        while let Some(item) = stream.next().await {
            // A capture is best-effort: half a post-mortem beats none, so a
            // mid-stream API error keeps what was already read.
            let Ok(output) = item else { break };
            let stderr = matches!(output, bollard::container::LogOutput::StdErr { .. });
            let raw = String::from_utf8_lossy(&output.into_bytes()).into_owned();
            let (at, message) = split_timestamp(raw.trim_end_matches(['\r', '\n']));
            lines.push(CapturedLine { at, message: message.to_string(), stderr });
        }
        Ok(lines)
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

    #[test]
    fn exit_code_parses_from_status_text() {
        assert_eq!(parse_exit_code("Exited (0) 3 minutes ago"), Some(0));
        assert_eq!(parse_exit_code("Exited (137) 2 seconds ago"), Some(137));
        assert_eq!(parse_exit_code("Up 5 seconds (healthy)"), None);
        assert_eq!(parse_exit_code("Created"), None);
    }

    #[test]
    fn timestamps_split_off_without_eating_message_text() {
        let (at, message) = split_timestamp("2026-08-12T14:22:03.123456789Z FATAL: role missing");
        assert_eq!(at.as_deref(), Some("2026-08-12T14:22:03.123456789Z"));
        assert_eq!(message, "FATAL: role missing");

        // A line Docker handed us without a stamp keeps every byte.
        let (at, message) = split_timestamp("plain line with spaces");
        assert_eq!(at, None);
        assert_eq!(message, "plain line with spaces");

        // A leading word that merely looks date-ish is not a stamp.
        let (at, message) = split_timestamp("2026-08-12 something happened");
        assert_eq!(at, None);
        assert_eq!(message, "2026-08-12 something happened");
    }
}
