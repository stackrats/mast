//! Image tags read from the registry, so "change the MySQL version" offers
//! what can actually be pulled today rather than a list someone typed once.
//!
//! Only Docker Hub is implemented. Every repo Mast's catalog installs lives
//! there, and each registry authenticates differently — a half-supported
//! second registry would fail at the point of use, so an unsupported repo
//! resolves to [`None`] up front and the caller keeps its static fallback.
//!
//! Nothing here touches disk or decides freshness: this module fetches and
//! filters, the caller caches.

use std::time::Duration;

mod filter;
pub use filter::{offered_versions, select_versions, version_sort_key};

/// Public Docker Hub token endpoint — anonymous pulls get a scoped token
/// without credentials.
const AUTH: &str = "https://auth.docker.io/token";
const REGISTRY: &str = "https://registry-1.docker.io";
const USER_AGENT: &str = concat!("mast/", env!("CARGO_PKG_VERSION"));

/// How long a single registry round-trip may take. This runs on a background
/// refresh, never on a path a person is waiting for, so the budget is about
/// not leaking tasks rather than latency.
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("{0} is not a Docker Hub repo")]
    UnsupportedRegistry(String),
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("registry returned {status} for {repo}")]
    Status { repo: String, status: u16 },
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(serde::Deserialize)]
struct TagsResponse {
    #[serde(default)]
    tags: Vec<String>,
}

/// Map a compose `image:` repo onto its Docker Hub path, or [`None`] when it
/// lives somewhere we cannot query.
///
/// A first path segment is a registry host only when it carries a dot or a
/// port — otherwise `mysql/mysql-server` would read as host `mysql`.
pub fn docker_hub_path(repo: &str) -> Option<String> {
    let mut rest = repo;
    if let Some((head, tail)) = repo.split_once('/')
        && (head.contains('.') || head.contains(':') || head == "localhost")
    {
        if !matches!(head, "docker.io" | "index.docker.io" | "registry-1.docker.io") {
            return None;
        }
        rest = tail;
    }
    if rest.is_empty() || rest.contains('/') && rest.matches('/').count() > 1 {
        return None;
    }
    // Official images live under the implicit `library/` namespace.
    Some(if rest.contains('/') { rest.to_string() } else { format!("library/{rest}") })
}

/// Every tag Docker Hub lists for `repo`, unfiltered and unordered.
///
/// The tag list is returned in one response for every repo Mast offers (the
/// largest is ~1400 tags); `Link`-header pagination is therefore not followed.
pub async fn fetch_tags(repo: &str) -> Result<Vec<String>, RegistryError> {
    let path = docker_hub_path(repo)
        .ok_or_else(|| RegistryError::UnsupportedRegistry(repo.to_string()))?;
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()?;

    let token: TokenResponse = client
        .get(AUTH)
        .query(&[("service", "registry.docker.io"), ("scope", &format!("repository:{path}:pull"))])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let response = client
        .get(format!("{REGISTRY}/v2/{path}/tags/list"))
        .bearer_auth(token.token)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RegistryError::Status {
            repo: repo.to_string(),
            status: response.status().as_u16(),
        });
    }
    Ok(response.json::<TagsResponse>().await?.tags)
}

/// Tags worth offering for `repo`: fetched, filtered to release lines, newest
/// first. An empty result means the repo publishes nothing version-shaped
/// (mailpit ships only `latest`) — the caller should then offer no choice.
pub async fn fetch_versions(repo: &str) -> Result<Vec<String>, RegistryError> {
    Ok(select_versions(&fetch_tags(repo).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_images_get_the_library_namespace() {
        assert_eq!(docker_hub_path("mariadb").as_deref(), Some("library/mariadb"));
        assert_eq!(docker_hub_path("redis").as_deref(), Some("library/redis"));
    }

    #[test]
    fn a_leading_segment_is_a_host_only_when_it_looks_like_one() {
        // `mysql` here is a Docker Hub org, not a registry.
        assert_eq!(
            docker_hub_path("mysql/mysql-server").as_deref(),
            Some("mysql/mysql-server")
        );
        assert_eq!(docker_hub_path("docker.io/mariadb").as_deref(), Some("library/mariadb"));
        assert_eq!(
            docker_hub_path("index.docker.io/typesense/typesense").as_deref(),
            Some("typesense/typesense")
        );
    }

    #[test]
    fn other_registries_are_declined_rather_than_guessed() {
        assert_eq!(docker_hub_path("ghcr.io/foo/bar"), None);
        assert_eq!(docker_hub_path("quay.io/prometheus/node-exporter"), None);
        assert_eq!(docker_hub_path("localhost:5000/redis"), None);
        assert_eq!(docker_hub_path(""), None);
    }
}
