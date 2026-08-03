//! Env validation rules (plan §6, M5): service-name/hostname cross-checks,
//! host-port forward conflicts, queue/cache/session coherence. Pure functions
//! over decoded entries + the project's resolved service names.

pub const SECRET_KEY_MARKERS: [&str; 7] =
    ["PASSWORD", "SECRET", "TOKEN", "_KEY", "APIKEY", "API_KEY", "PRIVATE"];

pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_KEY_MARKERS.iter().any(|marker| upper.contains(marker))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub key: Option<String>,
    pub message: String,
}

fn warn(key: &str, message: String) -> Finding {
    Finding { severity: Severity::Warning, key: Some(key.to_string()), message }
}

/// Keys whose values name a host that, inside containers, should be a
/// compose service name.
const HOST_KEYS: [&str; 6] =
    ["DB_HOST", "REDIS_HOST", "MAIL_HOST", "MEMCACHED_HOST", "MEILISEARCH_HOST", "MINIO_HOST"];

const LOCAL_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "host.docker.internal"];

pub fn validate(entries: &[(String, String)], service_names: &[String]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());

    // 1. Hostname ↔ service-name cross-check.
    for host_key in HOST_KEYS {
        if let Some(value) = get(host_key) {
            if value.is_empty() || value.contains('$') {
                continue; // interpolated or unset — nothing to check
            }
            if LOCAL_HOSTS.contains(&value) {
                findings.push(warn(
                    host_key,
                    format!(
                        "{value} points at the host, not a container — inside containers use \
                         the compose service name"
                    ),
                ));
            } else if !service_names.is_empty()
                && !service_names.iter().any(|s| s == value)
            {
                findings.push(warn(
                    host_key,
                    format!(
                        "\"{value}\" is not a service in this project (services: {})",
                        service_names.join(", ")
                    ),
                ));
            }
        }
    }

    // 2. Host-side port-forward conflicts.
    let mut seen_ports: Vec<(&str, &str)> = Vec::new();
    for (key, value) in entries {
        let is_forward = key == "APP_PORT" || key == "VITE_PORT"
            || (key.starts_with("FORWARD_") && key.ends_with("_PORT"));
        if is_forward && !value.is_empty() && !value.contains('$') {
            if let Some((other, _)) = seen_ports.iter().find(|(_, v)| v == value) {
                findings.push(warn(
                    key,
                    format!("host port {value} is also used by {other}"),
                ));
            }
            seen_ports.push((key, value));
        }
    }

    // 3. Queue/cache/session coherence: redis-backed drivers need a redis
    //    host that exists.
    let redis_ok = get("REDIS_HOST").is_some_and(|host| {
        !host.is_empty()
            && (service_names.is_empty()
                || service_names.iter().any(|s| s == host)
                || host.contains('$')
                || LOCAL_HOSTS.contains(&host))
    });
    for driver_key in ["QUEUE_CONNECTION", "CACHE_DRIVER", "CACHE_STORE", "SESSION_DRIVER"] {
        if get(driver_key) == Some("redis") && !redis_ok {
            findings.push(warn(
                driver_key,
                "set to redis, but REDIS_HOST is missing or does not name a service in this \
                 project"
                    .into(),
            ));
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn host_cross_check() {
        let services = vec!["mysql".to_string(), "redis".to_string()];
        let findings = validate(
            &entries(&[("DB_HOST", "mariadb"), ("REDIS_HOST", "redis"), ("MAIL_HOST", "localhost")]),
            &services,
        );
        assert!(findings.iter().any(|f| f.key.as_deref() == Some("DB_HOST")
            && f.message.contains("not a service")));
        assert!(findings.iter().any(|f| f.key.as_deref() == Some("MAIL_HOST")
            && f.message.contains("points at the host")));
        assert!(!findings.iter().any(|f| f.key.as_deref() == Some("REDIS_HOST")));
    }

    #[test]
    fn port_conflicts() {
        let findings = validate(
            &entries(&[
                ("APP_PORT", "8080"),
                ("FORWARD_DB_PORT", "3306"),
                ("VITE_PORT", "8080"),
                ("FORWARD_REDIS_PORT", "${X:-6379}"),
            ]),
            &[],
        );
        assert_eq!(
            findings.iter().filter(|f| f.message.contains("also used by")).count(),
            1
        );
        assert!(findings.iter().any(|f| f.key.as_deref() == Some("VITE_PORT")));
    }

    #[test]
    fn redis_coherence() {
        let services = vec!["app".to_string()]; // no redis service
        let findings = validate(
            &entries(&[("QUEUE_CONNECTION", "redis"), ("REDIS_HOST", "redis")]),
            &services,
        );
        assert!(findings.iter().any(|f| f.key.as_deref() == Some("QUEUE_CONNECTION")));

        let ok = validate(
            &entries(&[("QUEUE_CONNECTION", "redis"), ("REDIS_HOST", "redis")]),
            &["app".to_string(), "redis".to_string()],
        );
        assert!(!ok.iter().any(|f| f.key.as_deref() == Some("QUEUE_CONNECTION")));
    }

    #[test]
    fn secret_key_detection() {
        assert!(is_secret_key("DB_PASSWORD"));
        assert!(is_secret_key("APP_KEY"));
        assert!(!is_secret_key("APP_PORT"));
    }
}
