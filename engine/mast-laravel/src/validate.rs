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

    // 4. Database credential traps from the tracker: root as the init user
    //    crash-loops the container (mysql 8.0.23+ forbids MYSQL_USER=root),
    //    and DB_PORT is the IN-NETWORK port — moving it to dodge a host
    //    clash breaks the app's own connection, because the host-side knob
    //    is FORWARD_DB_PORT.
    let connection = get("DB_CONNECTION").unwrap_or_default();
    let db_default_port = match connection {
        "mysql" | "mariadb" => Some("3306"),
        "pgsql" => Some("5432"),
        _ => None,
    };
    if db_default_port.is_some() && get("DB_USERNAME") == Some("root") {
        findings.push(warn(
            "DB_USERNAME",
            "the official database images refuse to initialize with user \"root\" \
             (MYSQL_USER=root) — the container will crash-loop on a fresh volume; use a \
             non-root name (root exists anyway)"
                .into(),
        ));
    }
    if let (Some(default_port), Some(port)) = (db_default_port, get("DB_PORT"))
        && !port.is_empty()
        && !port.contains('$')
        && port != default_port
    {
        findings.push(warn(
            "DB_PORT",
            format!(
                "this is the IN-NETWORK port ({connection} listens on {default_port} inside \
                 the compose network) — to change the port published on your machine, use \
                 FORWARD_DB_PORT instead"
            ),
        ));
    }

    // 5. S3 split-brain (MinIO/RustFS): AWS_ENDPOINT names a compose service
    //    the browser cannot resolve, so browser-facing URLs need AWS_URL —
    //    with the bucket in the path, since these run path-style.
    if get("FILESYSTEM_DISK") == Some("s3")
        && let Some(endpoint) = get("AWS_ENDPOINT")
        && let Some(endpoint_host) = endpoint
            .strip_prefix("http://")
            .or_else(|| endpoint.strip_prefix("https://"))
            .and_then(|rest| rest.split([':', '/']).next())
        && service_names.iter().any(|s| s == endpoint_host)
    {
        if get("AWS_URL").is_none_or(str::is_empty) {
            findings.push(warn(
                "AWS_ENDPOINT",
                format!(
                    "\"{endpoint_host}\" only resolves inside the compose network — browser-\
                     facing file URLs will be dead links. Set AWS_URL to a host-reachable \
                     address including the bucket (e.g. http://localhost:9000/{})",
                    get("AWS_BUCKET").filter(|b| !b.is_empty()).unwrap_or("<bucket>")
                ),
            ));
        }
        if get("AWS_USE_PATH_STYLE_ENDPOINT").is_none_or(|v| v != "true") {
            findings.push(warn(
                "AWS_USE_PATH_STYLE_ENDPOINT",
                "self-hosted S3 (MinIO/RustFS) needs AWS_USE_PATH_STYLE_ENDPOINT=true — \
                 virtual-host addressing tries to resolve <bucket>.<service> and fails"
                    .into(),
            ));
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
    fn db_credential_traps() {
        // root as the init user: the image refuses it.
        let findings = validate(
            &entries(&[("DB_CONNECTION", "mysql"), ("DB_USERNAME", "root")]),
            &["mysql".to_string()],
        );
        assert!(
            findings
                .iter()
                .any(|f| f.key.as_deref() == Some("DB_USERNAME") && f.message.contains("crash-loop"))
        );

        // DB_PORT moved to dodge a host clash: names the real knob.
        let findings = validate(
            &entries(&[("DB_CONNECTION", "mysql"), ("DB_PORT", "3307")]),
            &[],
        );
        let f = findings.iter().find(|f| f.key.as_deref() == Some("DB_PORT")).unwrap();
        assert!(f.message.contains("FORWARD_DB_PORT"), "{}", f.message);

        // The defaults are silent, as is sqlite (no port semantics).
        let quiet = validate(
            &entries(&[
                ("DB_CONNECTION", "pgsql"),
                ("DB_PORT", "5432"),
                ("DB_USERNAME", "sail"),
            ]),
            &[],
        );
        assert!(quiet.iter().all(|f| f.key.as_deref() != Some("DB_PORT")));
        let sqlite = validate(&entries(&[("DB_CONNECTION", "sqlite"), ("DB_PORT", "9999")]), &[]);
        assert!(sqlite.is_empty());
    }

    #[test]
    fn s3_split_brain() {
        let services = vec!["minio".to_string()];
        // In-network endpoint without a browser URL: both halves flagged.
        let findings = validate(
            &entries(&[
                ("FILESYSTEM_DISK", "s3"),
                ("AWS_ENDPOINT", "http://minio:9000"),
                ("AWS_BUCKET", "local"),
            ]),
            &services,
        );
        let url = findings.iter().find(|f| f.key.as_deref() == Some("AWS_ENDPOINT")).unwrap();
        assert!(url.message.contains("http://localhost:9000/local"), "{}", url.message);
        assert!(findings.iter().any(|f| f.key.as_deref() == Some("AWS_USE_PATH_STYLE_ENDPOINT")));

        // Fully configured: silent.
        let quiet = validate(
            &entries(&[
                ("FILESYSTEM_DISK", "s3"),
                ("AWS_ENDPOINT", "http://minio:9000"),
                ("AWS_URL", "http://localhost:9000/local"),
                ("AWS_USE_PATH_STYLE_ENDPOINT", "true"),
            ]),
            &services,
        );
        assert!(quiet.is_empty(), "{quiet:?}");

        // A real AWS endpoint (not a compose service) is none of our business.
        let aws = validate(
            &entries(&[
                ("FILESYSTEM_DISK", "s3"),
                ("AWS_ENDPOINT", "https://s3.us-east-1.amazonaws.com"),
            ]),
            &services,
        );
        assert!(aws.is_empty(), "{aws:?}");
    }

    #[test]
    fn secret_key_detection() {
        assert!(is_secret_key("DB_PASSWORD"));
        assert!(is_secret_key("APP_KEY"));
        assert!(!is_secret_key("APP_PORT"));
    }
}
