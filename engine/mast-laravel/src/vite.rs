//! Vite's `public/hot` marker. While the file exists, Blade's `@vite`
//! renders dev-server URLs from its contents instead of built assets — by
//! design while the dev server runs, and a silent trap when it does not:
//! a killed dev server leaves the file behind, pages load without CSS/JS,
//! and `npm run build` changes nothing until the file is deleted. Through a
//! share tunnel even a LIVE dev server breaks: the tunnel forwards only the
//! app port, so visitors' browsers try to fetch assets from the developer's
//! loopback and Chrome blocks it (Private Network Access, reported as CORS).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotFile {
    /// The dev-server URL as written (e.g. `http://[::1]:5173`).
    pub url: String,
    /// Host part, brackets intact for IPv6 (`[::1]`).
    pub host: String,
    pub port: u16,
    /// The host only resolves on the developer's own machine.
    pub loopback: bool,
}

/// Parse `public/hot` contents. `None` for shapes we don't understand —
/// better silent than wrong about a file we would offer to delete.
pub fn parse_hot_file(contents: &str) -> Option<HotFile> {
    let url = contents.lines().next()?.trim();
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let default_port = if url.starts_with("https://") { 443 } else { 80 };
    let authority = rest.split(['/', '?']).next()?;
    let (host, port) = if let Some(bracket_end) = authority.find(']') {
        // IPv6: [::1] or [::1]:5173
        let host = &authority[..=bracket_end];
        let port = match authority[bracket_end + 1..].strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, default_port),
        }
    };
    if host.is_empty() {
        return None;
    }
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "0.0.0.0" | "[::]");
    Some(HotFile { url: url.to_string(), host: host.to_string(), port, loopback })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_file_shapes_parse() {
        let hot = parse_hot_file("http://[::1]:5173\n").unwrap();
        assert_eq!(hot.host, "[::1]");
        assert_eq!(hot.port, 5173);
        assert!(hot.loopback);

        let hot = parse_hot_file("http://localhost:5173").unwrap();
        assert_eq!((hot.host.as_str(), hot.port, hot.loopback), ("localhost", 5173, true));

        let hot = parse_hot_file("https://vite.myapp.test").unwrap();
        assert_eq!((hot.host.as_str(), hot.port, hot.loopback), ("vite.myapp.test", 443, false));

        assert!(parse_hot_file("").is_none());
        assert!(parse_hot_file("not a url").is_none());
        assert!(parse_hot_file("http://:5173").is_none());
    }
}
