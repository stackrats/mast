//! The browsable address of a Laravel app, derived from `.env`.
//!
//! Sail ships `APP_URL=http://localhost` and forwards the app container on
//! `APP_PORT`, so the two have to be read together: a non-default `APP_PORT`
//! is where the app actually answers, even though `APP_URL` never mentions
//! it. Anything that is not plain http(s) resolves to `None` — the URL ends
//! up at `xdg-open`, which would happily hand other schemes to a handler.

use std::collections::HashMap;

/// The URL to open for this project, or `None` when `.env` doesn't say.
pub fn app_url(env: &HashMap<String, String>) -> Option<String> {
    let port = env.get("APP_PORT").and_then(|v| v.trim().parse::<u16>().ok());
    match env.get("APP_URL").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        Some(raw) => normalize(raw, port),
        // No APP_URL but a forwarded port: Sail's own default address.
        None => port.map(|p| format!("http://localhost:{p}")),
    }
}

fn normalize(raw: &str, port: Option<u16>) -> Option<String> {
    // Unexpanded interpolation (`${APP_HOST}`) and anything with whitespace
    // or controls is not an address we can open.
    if raw.contains("${") || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
            "http" => ("http", rest),
            "https" => ("https", rest),
            _ => return None,
        },
        // A bare host (`myapp.test`) is a common hand-edit; assume http.
        None => ("http", raw),
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return None;
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (rest, None),
    };
    if !is_host_port(authority) {
        return None;
    }
    // An explicit port in APP_URL wins; otherwise APP_PORT fills it in, but
    // only when it isn't the scheme's default (which needs no port at all).
    let default_port = if scheme == "https" { 443 } else { 80 };
    let authority = match port {
        Some(p) if !authority.contains(':') && p != default_port => format!("{authority}:{p}"),
        _ => authority.to_string(),
    };
    Some(match path {
        Some(path) => format!("{scheme}://{authority}/{path}"),
        None => format!("{scheme}://{authority}"),
    })
}

/// `host` or `host:1234`. Deliberately narrow — it is also what stops a
/// scheme-less `javascript:alert(1)` from being read as a bare host.
fn is_host_port(authority: &str) -> bool {
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)) {
        return false;
    }
    match port {
        Some(port) => !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn app_port_fills_in_the_port_sail_leaves_out() {
        assert_eq!(
            app_url(&env(&[("APP_URL", "http://localhost"), ("APP_PORT", "8080")])),
            Some("http://localhost:8080".into())
        );
        // Default port: the URL is already right.
        assert_eq!(
            app_url(&env(&[("APP_URL", "http://localhost"), ("APP_PORT", "80")])),
            Some("http://localhost".into())
        );
        assert_eq!(
            app_url(&env(&[("APP_URL", "https://app.test"), ("APP_PORT", "443")])),
            Some("https://app.test".into())
        );
        // An explicit port wins over APP_PORT.
        assert_eq!(
            app_url(&env(&[("APP_URL", "http://localhost:3000"), ("APP_PORT", "8080")])),
            Some("http://localhost:3000".into())
        );
    }

    #[test]
    fn either_half_alone_still_yields_an_address() {
        assert_eq!(app_url(&env(&[("APP_PORT", "8080")])), Some("http://localhost:8080".into()));
        assert_eq!(
            app_url(&env(&[("APP_URL", "https://api.thinksolar.local")])),
            Some("https://api.thinksolar.local".into())
        );
        assert_eq!(app_url(&env(&[])), None);
        assert_eq!(app_url(&env(&[("APP_URL", "  ")])), None);
    }

    #[test]
    fn paths_and_bare_hosts() {
        assert_eq!(
            app_url(&env(&[("APP_URL", "http://localhost/admin/"), ("APP_PORT", "8080")])),
            Some("http://localhost:8080/admin".into())
        );
        assert_eq!(app_url(&env(&[("APP_URL", "myapp.test")])), Some("http://myapp.test".into()));
    }

    #[test]
    fn only_http_schemes_are_openable() {
        assert_eq!(app_url(&env(&[("APP_URL", "file:///etc/passwd")])), None);
        assert_eq!(app_url(&env(&[("APP_URL", "javascript:alert(1)")])), None);
        assert_eq!(app_url(&env(&[("APP_URL", "${APP_HOST}")])), None);
        assert_eq!(app_url(&env(&[("APP_URL", "http://")])), None);
    }
}
