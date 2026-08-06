//! Host-side port forwards, the `.env` half of them.
//!
//! Sail publishes every host port through a variable with a default
//! (`'${APP_PORT:-80}:80'`), so a port is moved by writing one key — the
//! compose file itself never has to change. This module knows which keys
//! those are and how to pick the next port when one is already taken; the
//! probing (what is actually bound right now) is an effect and lives in the
//! engine.

/// Does this `.env` key set a published host port?
///
/// The three shapes Sail uses: `APP_PORT` for the app, `VITE_PORT` for the
/// dev server, and `FORWARD_*_PORT` for every service stub it writes
/// (`FORWARD_DB_PORT`, `FORWARD_REDIS_PORT`, `FORWARD_MAILPIT_PORT`, …).
pub fn is_host_port_key(key: &str) -> bool {
    key == "APP_PORT"
        || key == "VITE_PORT"
        || (key.starts_with("FORWARD_") && key.ends_with("_PORT"))
}

/// How far past the preferred port to look before giving up.
const SCAN_WIDTH: u32 = 256;

/// The port to move to when `preferred` is unavailable, or `None` when the
/// whole scan window is busy.
///
/// A privileged default jumps into the 8000s (80 → 8080, 443 → 8443) the way
/// a developer would write it by hand; anything else counts up from where it
/// is, so 3306 → 3307 stays recognisable as "the database, moved once".
pub fn next_free_port(preferred: u16, is_free: impl Fn(u16) -> bool) -> Option<u16> {
    let start = if preferred < 1024 { preferred as u32 + 8000 } else { preferred as u32 + 1 };
    (start..start + SCAN_WIDTH)
        .take_while(|p| *p <= u16::MAX as u32)
        .map(|p| p as u16)
        .find(|p| is_free(*p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sail_port_keys_are_recognised() {
        for key in ["APP_PORT", "VITE_PORT", "FORWARD_DB_PORT", "FORWARD_MAILPIT_DASHBOARD_PORT"] {
            assert!(is_host_port_key(key), "{key}");
        }
        // Container-side settings that merely mention a port are not forwards.
        for key in ["DB_PORT", "REDIS_PORT", "PORT", "FORWARD_DB_HOST", "APP_PORTAL"] {
            assert!(!is_host_port_key(key), "{key}");
        }
    }

    #[test]
    fn privileged_defaults_move_into_the_8000s() {
        assert_eq!(next_free_port(80, |_| true), Some(8080));
        assert_eq!(next_free_port(443, |_| true), Some(8443));
        // Everything else counts up from itself.
        assert_eq!(next_free_port(3306, |_| true), Some(3307));
        assert_eq!(next_free_port(5173, |_| true), Some(5174));
    }

    #[test]
    fn busy_ports_are_skipped_and_a_full_window_gives_up() {
        let busy = [8080, 8081, 8082];
        assert_eq!(next_free_port(80, |p| !busy.contains(&p)), Some(8083));
        assert_eq!(next_free_port(3306, |_| false), None);
        // No wrap past the end of the port space.
        assert_eq!(next_free_port(u16::MAX, |_| true), None);
    }
}
