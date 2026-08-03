//! Laravel app processes (Reverb, Horizon, queue worker, scheduler): long-
//! running artisan commands that live INSIDE the app container, invisible to
//! container-level observation. Detection is composer.json/.env driven; the
//! running check scans `/proc/*/cmdline` in the container (portable to
//! busybox/dash — `ps`/`pgrep` are absent from many php images).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessDef {
    pub id: &'static str,
    pub title: &'static str,
    /// The artisan subcommand + args (run as `php artisan …`).
    pub artisan: &'static [&'static str],
    /// Substring that identifies the process in a cmdline scan. Chosen to be
    /// unambiguous against the artisan invocation itself.
    pub pattern: &'static str,
}

pub const PROCESSES: &[ProcessDef] = &[
    ProcessDef {
        id: "reverb",
        title: "Reverb",
        artisan: &["reverb:start"],
        pattern: "reverb:start",
    },
    ProcessDef {
        id: "horizon",
        title: "Horizon",
        artisan: &["horizon"],
        pattern: "artisan horizon",
    },
    ProcessDef {
        id: "queue",
        title: "Queue worker",
        artisan: &["queue:work"],
        pattern: "queue:work",
    },
    ProcessDef {
        id: "schedule",
        title: "Scheduler",
        artisan: &["schedule:work"],
        pattern: "schedule:work",
    },
];

pub fn process_def(id: &str) -> Option<&'static ProcessDef> {
    PROCESSES.iter().find(|def| def.id == id)
}

/// Which processes make sense for this project, from composer.json and the
/// decoded `.env`. Queue workers only when a non-sync queue is configured
/// and Horizon (its supervisor) is not installed.
pub fn detect_processes(
    composer_json: Option<&str>,
    env: &std::collections::HashMap<String, String>,
) -> Vec<&'static ProcessDef> {
    let composer = composer_json.unwrap_or("");
    let is_laravel = composer.contains("laravel/framework");
    let has_horizon = composer.contains("laravel/horizon");
    let queue = env.get("QUEUE_CONNECTION").map(String::as_str).unwrap_or("sync");
    PROCESSES
        .iter()
        .filter(|def| match def.id {
            "reverb" => {
                composer.contains("laravel/reverb")
                    || env.get("BROADCAST_CONNECTION").map(String::as_str) == Some("reverb")
            }
            "horizon" => has_horizon,
            "queue" => is_laravel && queue != "sync" && !has_horizon,
            "schedule" => is_laravel,
            _ => false,
        })
        .collect()
}

/// POSIX-sh scan printing one line per process cmdline (NULs → spaces).
/// Works under dash/busybox; no procps required.
pub fn scan_script() -> &'static str {
    r#"for p in /proc/[0-9]*/cmdline; do tr '\0' ' ' < "$p" 2>/dev/null; echo; done"#
}

/// POSIX-sh kill of every process whose cmdline matches `pattern`
/// (SIGTERM — artisan processes shut down cleanly on it). The killer's own
/// cmdline CONTAINS the pattern, so `$$` must be excluded or the script
/// SIGTERMs itself mid-loop (the pkill footgun). Always exits 0: stopping
/// an already-stopped process is a no-op, not a failure.
pub fn kill_script(pattern: &str) -> String {
    // The pattern lands inside a case glob; escape glob/quote metacharacters.
    let safe: String = pattern
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | ' ' | '.'))
        .collect();
    format!(
        r#"for p in /proc/[0-9]*; do pid="${{p#/proc/}}"; [ "$pid" = "$$" ] && continue; case "$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null)" in *{safe}*) kill "$pid" 2>/dev/null;; esac; done; exit 0"#
    )
}

/// Does a cmdline scan (from [`scan_script`]) show `pattern` running?
/// The scan's own shell lines are ignored (they contain the pattern too).
pub fn scan_shows(scan_output: &str, pattern: &str) -> bool {
    scan_output
        .lines()
        .filter(|line| !line.contains("/proc/[0-9]*"))
        .any(|line| line.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn detection_follows_composer_and_env() {
        let composer = r#"{"require":{"laravel/framework":"^12","laravel/reverb":"^1"}}"#;
        let detected = detect_processes(Some(composer), &env(&[("QUEUE_CONNECTION", "redis")]));
        let ids: Vec<_> = detected.iter().map(|d| d.id).collect();
        assert_eq!(ids, ["reverb", "queue", "schedule"]);

        // Horizon supersedes the plain queue worker.
        let composer = r#"{"require":{"laravel/framework":"^12","laravel/horizon":"^5"}}"#;
        let detected = detect_processes(Some(composer), &env(&[("QUEUE_CONNECTION", "redis")]));
        let ids: Vec<_> = detected.iter().map(|d| d.id).collect();
        assert_eq!(ids, ["horizon", "schedule"]);

        // Sync queue → no worker; not Laravel → nothing.
        let composer = r#"{"require":{"laravel/framework":"^12"}}"#;
        let detected = detect_processes(Some(composer), &env(&[]));
        assert_eq!(detected.iter().map(|d| d.id).collect::<Vec<_>>(), ["schedule"]);
        assert!(detect_processes(None, &env(&[])).is_empty());
    }

    #[test]
    fn reverb_detected_from_env_even_without_composer_entry() {
        let detected = detect_processes(
            Some(r#"{"require":{"laravel/framework":"^12"}}"#),
            &env(&[("BROADCAST_CONNECTION", "reverb")]),
        );
        assert!(detected.iter().any(|d| d.id == "reverb"));
    }

    #[test]
    fn scan_matching_ignores_the_scanner_itself() {
        let output = "sh -c for p in /proc/[0-9]*/cmdline; do tr reverb:start horizon \n\
                      php artisan reverb:start --debug \n\
                      /usr/local/bin/php artisan schedule:work \n";
        assert!(scan_shows(output, "reverb:start"));
        assert!(scan_shows(output, "schedule:work"));
        assert!(!scan_shows(output, "queue:work"));
        assert!(!scan_shows(output, "artisan horizon"));
    }

    #[test]
    fn kill_script_strips_dangerous_characters() {
        let script = kill_script("reverb:start\"; $(rm -rf /)");
        let glob = script.split("in *").nth(1).and_then(|s| s.split("*)").next()).unwrap();
        // Quotes, semicolons, slashes and subshell syntax never reach the glob.
        assert_eq!(glob, "reverb:start rm -rf ");
        assert!(kill_script("queue:work").contains("*queue:work*"));
    }

    #[test]
    fn kill_script_excludes_itself_and_always_exits_zero() {
        let script = kill_script("reverb:start");
        // Self-exclusion: the killer's own cmdline matches the pattern.
        assert!(script.contains(r#"[ "$pid" = "$$" ] && continue"#), "{script}");
        assert!(script.ends_with("exit 0"), "{script}");
    }

    /// The real thing, on the host: a decoy process matching the pattern is
    /// killed while the killer survives to exit 0.
    #[test]
    fn kill_script_kills_the_target_not_itself() {
        // No exec: sh must stay alive with the marker in its cmdline ($0).
        let mut decoy = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .arg("fake-reverb:start-marker")
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let status = std::process::Command::new("sh")
            .args(["-c", &kill_script("fake-reverb:start-marker")])
            .status()
            .unwrap();
        assert!(status.success(), "killer must exit 0, got {status:?}");
        // The decoy dies (SIGTERM) shortly after.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match decoy.try_wait().unwrap() {
                Some(_) => break,
                None if std::time::Instant::now() > deadline => {
                    let _ = decoy.kill();
                    panic!("decoy survived the kill script");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }
}
