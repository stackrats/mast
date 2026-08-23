//! `sail share` mechanics: the expose tunnel the vendored sail script runs,
//! rebuilt argv-for-argv (same image, same flags, same `.env` knobs and
//! defaults) with two deliberate differences — no TTY, so output arrives as
//! parseable lines, and a `--name`, so stopping the share can actually stop
//! the container (killing an attached `docker run` client does not).

/// The `.env`-driven knobs, with the sail script's own defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSettings {
    pub app_port: u16,
    pub dashboard_port: String,
    pub server_host: String,
    pub server_port: String,
    pub token: String,
    pub server: String,
    pub subdomain: String,
    pub domain: String,
}

pub fn share_settings(entries: &[(String, String)]) -> ShareSettings {
    let get = |key: &str, default: &str| {
        entries
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let server_host = get("SAIL_SHARE_SERVER_HOST", "laravel-sail.site");
    let domain = get("SAIL_SHARE_DOMAIN", &server_host);
    ShareSettings {
        app_port: get("APP_PORT", "80").parse().unwrap_or(80),
        dashboard_port: get("SAIL_SHARE_DASHBOARD", "4040"),
        server_port: get("SAIL_SHARE_SERVER_PORT", "8080"),
        token: get("SAIL_SHARE_TOKEN", ""),
        server: get("SAIL_SHARE_SERVER", ""),
        subdomain: get("SAIL_SHARE_SUBDOMAIN", ""),
        server_host,
        domain,
    }
}

/// The docker invocation, mirroring the sail script's share verb.
pub fn share_run_argv(settings: &ShareSettings, container_name: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["docker", "run", "--init", "--rm", "--name"]
        .map(String::from)
        .to_vec();
    argv.push(container_name.to_string());
    argv.push("--add-host=host.docker.internal:host-gateway".into());
    argv.extend(["-p".into(), format!("{}:4040", settings.dashboard_port)]);
    argv.push("beyondcodegmbh/expose-server:latest".into());
    argv.push("share".into());
    argv.push(format!("http://host.docker.internal:{}", settings.app_port));
    argv.push(format!("--server-host={}", settings.server_host));
    argv.push(format!("--server-port={}", settings.server_port));
    argv.push(format!("--auth={}", settings.token));
    argv.push(format!("--server={}", settings.server));
    argv.push(format!("--subdomain={}", settings.subdomain));
    argv.push(format!("--domain={}", settings.domain));
    argv
}

/// The public tunnel URL, when this output line carries it. Matched by the
/// share domain rather than a label — expose has reworded its banner more
/// than once ("Expose-URL:", "Public HTTP URL:", …).
pub fn find_public_url(line: &str, settings: &ShareSettings) -> Option<String> {
    let needle = format!(".{}", settings.domain);
    line.split_whitespace()
        .map(|token| token.trim_end_matches(['.', ',', '"', '\'', ')']))
        .find(|token| {
            (token.starts_with("http://") || token.starts_with("https://"))
                && token.contains(&needle)
        })
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn settings_default_exactly_like_the_sail_script() {
        let s = share_settings(&[]);
        assert_eq!(s.app_port, 80);
        assert_eq!(s.dashboard_port, "4040");
        assert_eq!(s.server_host, "laravel-sail.site");
        assert_eq!(s.server_port, "8080");
        assert_eq!(s.domain, "laravel-sail.site", "domain defaults to the server host");
        assert_eq!(s.token, "");

        let s = share_settings(&pairs(&[
            ("APP_PORT", "8080"),
            ("SAIL_SHARE_SERVER_HOST", "tunnel.acme.dev"),
            ("SAIL_SHARE_SUBDOMAIN", "demo"),
        ]));
        assert_eq!(s.app_port, 8080);
        assert_eq!(s.domain, "tunnel.acme.dev", "custom host carries into the domain default");
        assert_eq!(s.subdomain, "demo");
    }

    #[test]
    fn run_argv_mirrors_sail_minus_tty_plus_name() {
        let argv = share_run_argv(&share_settings(&[]), "mast-share-abc");
        let joined = argv.join(" ");
        assert!(joined.starts_with("docker run --init --rm --name mast-share-abc"), "{joined}");
        assert!(joined.contains("--add-host=host.docker.internal:host-gateway"), "{joined}");
        assert!(joined.contains("-p 4040:4040"), "{joined}");
        assert!(joined.contains("beyondcodegmbh/expose-server:latest share"), "{joined}");
        assert!(joined.contains("http://host.docker.internal:80"), "{joined}");
        assert!(joined.contains("--server-host=laravel-sail.site"), "{joined}");
        assert!(!argv.contains(&"-t".to_string()), "no TTY — output must be parseable lines");
    }

    #[test]
    fn public_url_found_by_domain_across_banner_wordings() {
        let s = share_settings(&[]);
        for line in [
            "Expose-URL:  http://v5kmmozwni.laravel-sail.site:8080",
            "Public HTTP URL: http://v5kmmozwni.laravel-sail.site:8080.",
            "shared at http://v5kmmozwni.laravel-sail.site:8080, dashboard at http://127.0.0.1:4040",
        ] {
            assert_eq!(
                find_public_url(line, &s).as_deref(),
                Some("http://v5kmmozwni.laravel-sail.site:8080"),
                "{line}"
            );
        }
        // Local and dashboard URLs never match.
        assert_eq!(find_public_url("Local-URL: http://host.docker.internal:80", &s), None);
        assert_eq!(find_public_url("Dashboard: http://127.0.0.1:4040", &s), None);
        assert_eq!(find_public_url("no urls here", &s), None);
    }
}
