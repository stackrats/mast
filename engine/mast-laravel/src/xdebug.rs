//! Sail's Xdebug knobs, as the wrapper and stub actually wire them:
//! `.env`'s SAIL_XDEBUG_MODE feeds the container's XDEBUG_MODE, and
//! SAIL_XDEBUG_CONFIG feeds XDEBUG_CONFIG (default
//! `client_host=host.docker.internal`). Chronic breakage class in the
//! tracker (66 issue bodies): the compose file predates the wiring, the
//! host-gateway mapping is missing on Linux, the runtime image shipped
//! without the extension, or no IDE is listening — and every one of those
//! looks identical from the browser: breakpoints simply never hit.

/// Xdebug as `.env` requests it. `None` when SAIL_XDEBUG_MODE is unset,
/// empty, or `off` — nothing to doctor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdebugEnv {
    /// The requested mode string (`develop,debug`).
    pub mode: String,
    /// Where the container will try to reach the debugger client.
    pub client_host: String,
    pub client_port: u16,
}

impl XdebugEnv {
    /// Step-debugging is requested (the connect-back path matters).
    pub fn wants_debug(&self) -> bool {
        self.mode.split(',').any(|m| m.trim() == "debug")
    }
}

pub fn xdebug_env(entries: &[(String, String)]) -> Option<XdebugEnv> {
    let get = |key: &str| {
        entries
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let mode = get("SAIL_XDEBUG_MODE").filter(|m| m != "off")?;
    // XDEBUG_CONFIG is space-separated key=value pairs; the stub default is
    // client_host=host.docker.internal, client_port falls back to 9003.
    let config = get("SAIL_XDEBUG_CONFIG").unwrap_or_default();
    let setting = |name: &str| {
        config
            .split_whitespace()
            .filter_map(|pair| pair.split_once('='))
            .rev()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    };
    Some(XdebugEnv {
        mode,
        client_host: setting("client_host")
            .unwrap_or_else(|| "host.docker.internal".to_string()),
        client_port: setting("client_port").and_then(|p| p.parse().ok()).unwrap_or(9003),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn off_or_unset_means_nothing_to_doctor() {
        assert!(xdebug_env(&[]).is_none());
        assert!(xdebug_env(&pairs(&[("SAIL_XDEBUG_MODE", "off")])).is_none());
        assert!(xdebug_env(&pairs(&[("SAIL_XDEBUG_MODE", "")])).is_none());
    }

    #[test]
    fn stub_defaults_and_config_overrides_parse() {
        let x = xdebug_env(&pairs(&[("SAIL_XDEBUG_MODE", "develop,debug")])).unwrap();
        assert!(x.wants_debug());
        assert_eq!(x.client_host, "host.docker.internal");
        assert_eq!(x.client_port, 9003);

        let x = xdebug_env(&pairs(&[
            ("SAIL_XDEBUG_MODE", "coverage"),
            ("SAIL_XDEBUG_CONFIG", "client_host=172.17.0.1 client_port=9000 idekey=X"),
        ]))
        .unwrap();
        assert!(!x.wants_debug(), "coverage mode never connects back");
        assert_eq!(x.client_host, "172.17.0.1");
        assert_eq!(x.client_port, 9000);
    }
}
