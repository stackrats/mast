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
/// Works under dash/busybox; no procps required. stderr is silenced BEFORE
/// the input redirect: redirections apply left to right, and a process
/// exiting between the glob and the read otherwise prints "cannot open"
/// noise the null device was meant to swallow.
pub fn scan_script() -> &'static str {
    r#"for p in /proc/[0-9]*/cmdline; do tr '\0' ' ' 2>/dev/null < "$p"; echo; done"#
}

/// Cmdline patterns of the frontend dev stack — what `composer run dev`-style
/// scripts (multiplex, concurrently, `npm run dev`, vite, the serve/pail
/// panes) leave behind in the app container when the host-side client dies:
/// cancelling a `docker exec` never kills what it started, so the next start
/// finds its ports held by the previous copy. The duplicate-stack repair
/// SIGTERMs anything matching one of these. Kept to patterns the kill-script
/// sanitizer passes through unchanged (no `/`), and specific enough not to
/// catch lookalikes — `vite.js`/`vite-plus`, never bare `vite`, which is a
/// substring of a vitest cmdline; `php artisan serve` spelled contiguously,
/// because Sail's SUPERVISED app server is also artisan serve but with the
/// interpreter path and a `-d` flag in between (`/usr/bin/php -d
/// variables_order=EGPCS /var/www/html/artisan serve`) — the one serve this
/// must never touch. `npm run dev` also covers pnpm ("pnpm run dev" contains
/// it). The artisan daemons (horizon, reverb, queue, schedule) are
/// deliberately absent: they have their own start/stop chips, and a SIGTERM
/// to the stack's runner takes its panes down with it.
pub const DEV_STACK_PATTERNS: &[&str] = &[
    "multiplex",
    "concurrently",
    "npm run dev",
    "vite.js",
    "vite-plus",
    "php artisan serve",
    "php artisan pail",
];

/// POSIX-sh kill of every process whose cmdline matches `pattern`
/// (SIGTERM — artisan processes shut down cleanly on it). The killer's own
/// cmdline CONTAINS the pattern, so `$$` must be excluded or the script
/// SIGTERMs itself mid-loop (the pkill footgun). Always exits 0: stopping
/// an already-stopped process is a no-op, not a failure.
pub fn kill_script(pattern: &str) -> String {
    kill_script_matching(&[pattern])
}

/// [`kill_script`] over several patterns in one pass — a single /proc walk
/// with the globs joined as `case` alternatives.
pub fn kill_script_matching(patterns: &[&str]) -> String {
    let globs: Vec<String> = patterns
        .iter()
        .map(|pattern| {
            // The pattern lands inside a case glob; escape glob/quote
            // metacharacters.
            let safe: String = pattern
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | ' ' | '.'))
                .collect();
            // The literal half is quoted because a space in an unquoted case
            // pattern is a word break — `*npm run dev*` is a syntax error in
            // dash, and "artisan horizon" hits the same.
            format!("*\"{safe}\"*")
        })
        .collect();
    let globs = globs.join("|");
    // As in [`scan_script`], stderr goes to the null device BEFORE the input
    // redirect — killing is exactly what makes processes vanish mid-walk.
    format!(
        r#"for p in /proc/[0-9]*; do pid="${{p#/proc/}}"; [ "$pid" = "$$" ] && continue; case "$(tr '\0' ' ' 2>/dev/null < "$p/cmdline")" in {globs}) kill "$pid" 2>/dev/null;; esac; done; exit 0"#
    )
}

/// The scan lines (from [`scan_script`]) matching any of `patterns` — the
/// exact cmdlines a [`kill_script_matching`] over the same patterns would
/// signal, for previews that name what they are about to stop. The scanner's
/// own shell line is excluded, as in [`scan_shows`].
pub fn scan_matches(scan_output: &str, patterns: &[&str]) -> Vec<String> {
    scan_output
        .lines()
        .filter(|line| !line.contains("/proc/[0-9]*"))
        .map(str::trim)
        .filter(|line| patterns.iter().any(|pattern| line.contains(pattern)))
        .map(String::from)
        .collect()
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
        let glob = script.split("in *\"").nth(1).and_then(|s| s.split("\"*)").next()).unwrap();
        // Quotes, semicolons, slashes and subshell syntax never reach the glob.
        assert_eq!(glob, "reverb:start rm -rf ");
        assert!(kill_script("queue:work").contains("*\"queue:work\"*"));
    }

    /// Every published dev-stack pattern must pass the kill-script sanitizer
    /// unchanged — a silently mangled glob is a kill that never matches.
    #[test]
    fn dev_stack_patterns_survive_the_sanitizer_and_share_one_case() {
        let script = kill_script_matching(DEV_STACK_PATTERNS);
        for pattern in DEV_STACK_PATTERNS {
            assert!(script.contains(&format!("*\"{pattern}\"*")), "{pattern} mangled: {script}");
        }
        // One /proc walk, alternatives joined in a single case glob.
        assert!(script.contains("*\"multiplex\"*|*\"concurrently\"*"), "{script}");
        assert!(script.ends_with("exit 0"), "{script}");
    }

    /// The cmdlines from the real incident (three multiplex stacks stacked in
    /// one app container): the stack's processes match, the daemons and
    /// lookalikes do not, and the scanner's own line is ignored.
    #[test]
    fn scan_matches_finds_the_dev_stack_and_ignores_lookalikes() {
        let scan = "sh -c for p in /proc/[0-9]*/cmdline; do tr multiplex vite.js \n\
             sh -c pnpm dlx @laravel/multiplex@0.4 --title 'artisan dev' 'vite,pnpm run dev' \n\
             node /home/sail/.cache/pnpm/dlx/252c/node_modules/@laravel/multiplex/dist/cli.js \n\
             node /usr/bin/pnpm run dev \n\
             sh -c vite \n\
             node /var/www/html/node_modules/.bin/../vite/bin/vite.js \n\
             node /var/www/html/node_modules/@voidzero-dev/vite-plus/bin/vp.js dev \n\
             node /var/www/html/node_modules/vitest/vitest.mjs --watch \n\
             php artisan serve \n\
             /usr/bin/php -d variables_order=EGPCS /var/www/html/artisan serve --host=0.0.0.0 --port=80 \n\
             php artisan horizon \n\
             /usr/local/bin/php artisan reverb:start \n";
        let matched = scan_matches(scan, DEV_STACK_PATTERNS);
        // `sh -c vite` matches nothing (bare "vite" is not a pattern — it
        // would catch vitest), and neither does Sail's SUPERVISED app server,
        // whose artisan serve is spelled through the interpreter path.
        assert_eq!(matched.len(), 6, "{matched:#?}");
        assert!(matched.iter().all(|l| !l.contains("vitest") && !l.contains("horizon")));
        assert!(matched.iter().all(|l| !l.contains("reverb")), "{matched:#?}");
        assert!(matched.iter().all(|l| !l.contains("variables_order")), "{matched:#?}");
        assert!(matched.iter().any(|l| l.contains("multiplex/dist/cli.js")));
        assert!(matched.iter().any(|l| l.contains("vite/bin/vite.js")));
        assert!(matched.iter().any(|l| l.contains("vp.js dev")));
    }

    #[test]
    fn kill_script_excludes_itself_and_always_exits_zero() {
        let script = kill_script("reverb:start");
        // Self-exclusion: the killer's own cmdline matches the pattern.
        assert!(script.contains(r#"[ "$pid" = "$$" ] && continue"#), "{script}");
        assert!(script.ends_with("exit 0"), "{script}");
    }

    /// The real thing, on the host: a decoy process matching the pattern is
    /// killed while the killer survives to exit 0. The marker keeps a space
    /// in it, so the quoted-glob fix stays exercised — an unquoted space in
    /// a case pattern is a dash syntax error, which is how "artisan horizon"
    /// and the multi-word dev-stack patterns used to fail.
    /// linux-only: the kill script walks /proc (as it does inside the
    /// container in production); macOS has no /proc to exercise it on.
    #[cfg(target_os = "linux")]
    #[test]
    fn kill_script_kills_the_target_not_itself() {
        // No exec: sh must stay alive with the marker in its cmdline ($0).
        let mut decoy = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .arg("fake reverb:start marker")
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let status = std::process::Command::new("sh")
            .args(["-c", &kill_script("fake reverb:start marker")])
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
