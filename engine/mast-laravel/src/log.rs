//! `storage/logs/laravel.log` parsing: Monolog's line format grouped back
//! into entries, so a stack trace reads as one error instead of two hundred
//! lines interleaved with the next request's noise.
//!
//! An entry starts with `[timestamp] environment.LEVEL: message` and owns
//! every following line that is not itself a header — `[stacktrace]`,
//! `[previous exception]` and `#0 …` frames all fold into the entry that
//! raised them.

/// One grouped log entry, header fields split out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub timestamp: String,
    pub environment: String,
    /// Monolog level, uppercase (`ERROR`, `WARNING`, …).
    pub level: String,
    /// The header line's message, context JSON included.
    pub message: String,
    /// Continuation lines (stack trace and friends), when any.
    pub detail: Option<String>,
}

const LEVELS: [&str; 8] =
    ["DEBUG", "INFO", "NOTICE", "WARNING", "ERROR", "CRITICAL", "ALERT", "EMERGENCY"];

/// Parse a log body into entries, oldest first. Anything before the first
/// header (a tail read that starts mid-entry) is dropped rather than
/// misattributed.
pub fn parse_log(body: &str) -> Vec<LogEntry> {
    let mut entries: Vec<LogEntry> = Vec::new();
    for line in body.lines() {
        if let Some(entry) = parse_header(line) {
            entries.push(entry);
        } else if let Some(last) = entries.last_mut() {
            match &mut last.detail {
                Some(detail) => {
                    detail.push('\n');
                    detail.push_str(line);
                }
                None => last.detail = Some(line.to_string()),
            }
        }
    }
    for entry in &mut entries {
        if let Some(detail) = &mut entry.detail {
            let trimmed = detail.trim();
            if trimmed.is_empty() {
                entry.detail = None;
            } else if trimmed.len() != detail.len() {
                *detail = trimmed.to_string();
            }
        }
    }
    entries
}

/// `[2026-08-23 10:15:30] local.ERROR: message` → an entry; anything that
/// does not carry a plausible timestamp and a known Monolog level is a
/// continuation line (`[stacktrace]` and `[previous exception]` both start
/// with `[` and must not become entries).
fn parse_header(line: &str) -> Option<LogEntry> {
    let rest = line.strip_prefix('[')?;
    let (timestamp, rest) = rest.split_once("] ")?;
    let plausible_ts = timestamp.len() >= 10
        && timestamp.as_bytes()[..4].iter().all(u8::is_ascii_digit)
        && timestamp.as_bytes()[4] == b'-';
    if !plausible_ts {
        return None;
    }
    let (tag, message) = rest.split_once(':')?;
    let (environment, level) = tag.rsplit_once('.')?;
    if environment.is_empty() || environment.contains(' ') || !LEVELS.contains(&level) {
        return None;
    }
    Some(LogEntry {
        timestamp: timestamp.to_string(),
        environment: environment.to_string(),
        level: level.to_string(),
        message: message.trim_start().to_string(),
        detail: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_group_their_stack_traces() {
        let body = "\
[2026-08-23 10:15:30] local.ERROR: Call to undefined method App\\Models\\User::foo() {\"userId\":1}
[stacktrace]
#0 /var/www/html/app/Http/Controllers/HomeController.php(12): App\\Models\\User->foo()
#1 {main}
\"}
[2026-08-23 10:16:02] local.INFO: user logged in
[2026-08-23 10:16:03] production.WARNING: slow query: 2.3s
";
        let entries = parse_log(body);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].level, "ERROR");
        assert_eq!(entries[0].environment, "local");
        assert_eq!(entries[0].timestamp, "2026-08-23 10:15:30");
        assert!(entries[0].message.starts_with("Call to undefined method"));
        let detail = entries[0].detail.as_deref().unwrap();
        assert!(detail.starts_with("[stacktrace]"), "stacktrace folds into its entry");
        assert!(detail.contains("#1 {main}"));
        assert_eq!(entries[1].level, "INFO");
        assert_eq!(entries[1].detail, None);
        assert_eq!(entries[2].environment, "production");
    }

    #[test]
    fn previous_exception_blocks_stay_inside_the_entry() {
        let body = "\
[2026-08-23 09:00:00] local.ERROR: outer
[previous exception] [object] (RuntimeException(code: 0): inner at /app/x.php:3)
[stacktrace]
#0 {main}
";
        let entries = parse_log(body);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].detail.as_deref().unwrap().contains("[previous exception]"));
    }

    #[test]
    fn a_tail_read_starting_mid_entry_drops_the_orphan_lines() {
        let body = "\
#4 /var/www/html/vendor/laravel/framework/src/Kernel.php(20): handle()
#5 {main}
[2026-08-23 11:00:00] local.DEBUG: fresh entry
";
        let entries = parse_log(body);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "fresh entry");
    }

    #[test]
    fn dotted_environments_and_colons_in_messages_parse() {
        let entries =
            parse_log("[2026-01-02 03:04:05] staging.eu.CRITICAL: db: connection refused\n");
        assert_eq!(entries[0].environment, "staging.eu");
        assert_eq!(entries[0].level, "CRITICAL");
        assert_eq!(entries[0].message, "db: connection refused");
    }

    #[test]
    fn garbage_is_not_an_entry() {
        assert!(parse_log("not a log line\nанother\n").is_empty());
        assert!(parse_log("[stacktrace]\n#0 {main}\n").is_empty());
        // Unknown level: whole thing is noise, not a header.
        assert!(parse_log("[2026-08-23 10:00:00] local.SHOUTING: hi\n").is_empty());
    }
}
