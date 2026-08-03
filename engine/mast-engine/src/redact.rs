//! Secret redaction (plan M4, release-gating): values of secret-looking keys
//! from a project's `.env` must never reach operation events, patches,
//! resolution errors, or persistence. The redactor is rebuilt from `.env` on
//! every reconcile, so newly added secrets are covered without restart.
//!
//! Container log streams are NOT redacted: they render the developer's own
//! application output transiently and are never persisted by Mast.

use std::path::Path;

const SECRET_KEY_MARKERS: [&str; 7] =
    ["PASSWORD", "SECRET", "TOKEN", "_KEY", "APIKEY", "API_KEY", "PRIVATE"];

/// Minimum secret length we redact — shorter values (e.g. "1", "true") would
/// mangle ordinary output far more than they protect.
const MIN_SECRET_LEN: usize = 4;

/// Length at which a value is distinctive enough to redact wherever it appears
/// *inside* a larger token. Below it, a match is far likelier to be a
/// coincidence — a path segment, a flag name — than the secret itself.
const DISTINCTIVE_SECRET_LEN: usize = 12;

/// Values Laravel and Sail ship as documented defaults. They are printed in
/// public documentation and committed to `compose.yaml`, so redacting them
/// protects nothing — while `sail` in particular (Sail's own
/// `AWS_ACCESS_KEY_ID`) would corrupt every `vendor/bin/sail` path Mast shows.
const PLACEHOLDER_VALUES: [&str; 8] =
    ["sail", "password", "secret", "null", "root", "admin", "homestead", "changeme"];

pub const REDACTED: &str = "•••redacted•••";

fn is_placeholder(value: &str) -> bool {
    PLACEHOLDER_VALUES.iter().any(|known| value.eq_ignore_ascii_case(known))
}

#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Secret values, longest first so substring secrets don't leave tails.
    values: Vec<String>,
}

impl Redactor {
    pub fn from_env_file(path: &Path) -> Self {
        let vars = mast_compose::parse_env_file(path);
        let mut values: Vec<String> = vars
            .into_iter()
            .filter(|(key, value)| {
                let key = key.to_ascii_uppercase();
                value.len() >= MIN_SECRET_LEN
                    && !is_placeholder(value)
                    && SECRET_KEY_MARKERS.iter().any(|marker| key.contains(marker))
            })
            .map(|(_, value)| value)
            .collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        values.dedup();
        Self { values }
    }

    /// One redactor covering every project's secrets. History records commands
    /// that belong to no single project, so per-project redactors are not
    /// enough to keep another project's password out of a shared log line.
    pub fn union<'a>(redactors: impl Iterator<Item = &'a Redactor>) -> Self {
        let mut values: Vec<String> = redactors.flat_map(|r| r.values.iter().cloned()).collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        values.dedup();
        Self { values }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for value in &self.values {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), REDACTED);
            }
        }
        out
    }

    /// Redact a single token — one argv element or one env value — rather than
    /// free-form text. A token either *is* a secret, carries one after `=`, or
    /// (when the secret is distinctive) contains one. Blind substring
    /// replacement is wrong here: `/app/vendor/bin/sail` is a path that happens
    /// to contain a value, not a secret, and a history entry whose copy button
    /// yields a mangled path is worse than useless.
    pub fn redact_token(&self, token: &str) -> String {
        if self.values.iter().any(|value| token == value) {
            return REDACTED.to_string();
        }
        let mut out = token.to_string();
        for value in &self.values {
            let assigned = format!("={value}");
            if out.contains(&assigned) {
                out = out.replace(&assigned, &format!("={REDACTED}"));
            } else if value.len() >= DISTINCTIVE_SECRET_LEN && out.contains(value.as_str()) {
                out = out.replace(value.as_str(), REDACTED);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_values_and_leaves_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join(".env");
        std::fs::write(
            &env,
            "APP_NAME=demo\nDB_PASSWORD=hunter2secret\nAPP_KEY=base64:abc123def456\nAPP_DEBUG=true\nMAIL_PORT=1025\nSHORT_TOKEN=ab\n",
        )
        .unwrap();
        let redactor = Redactor::from_env_file(&env);

        let line = "mysql: using password hunter2secret with key base64:abc123def456 on demo:1025";
        let redacted = redactor.redact(line);
        assert!(!redacted.contains("hunter2secret"));
        assert!(!redacted.contains("base64:abc123def456"));
        assert!(redacted.contains("demo:1025"), "{redacted}");
        // Too-short secret values are deliberately left alone.
        assert_eq!(redactor.redact("ab"), "ab");
    }

    /// Sail's own MinIO defaults set `AWS_ACCESS_KEY_ID=sail`, which matches
    /// the `_KEY` marker. Treating that as a secret redacted the `sail` in
    /// every `vendor/bin/sail` path Mast printed.
    #[test]
    fn documented_sail_defaults_are_not_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join(".env");
        std::fs::write(
            &env,
            "AWS_ACCESS_KEY_ID=sail\nAWS_SECRET_ACCESS_KEY=password\nREDIS_PASSWORD=null\n",
        )
        .unwrap();
        let redactor = Redactor::from_env_file(&env);
        let sail = "/home/dev/test-app/vendor/bin/sail";
        assert_eq!(redactor.redact_token(sail), sail);
        assert_eq!(redactor.redact("mysql: connected with password"), "mysql: connected with password");
    }

    #[test]
    fn a_token_is_redacted_whole_or_after_an_equals_but_not_inside_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join(".env");
        // "beta" is a real secret here, and also a plausible path segment.
        std::fs::write(&env, "DB_PASSWORD=beta\nAPI_TOKEN=s3cr3tvaluelong\n").unwrap();
        let redactor = Redactor::from_env_file(&env);

        assert_eq!(redactor.redact_token("beta"), REDACTED);
        assert_eq!(redactor.redact_token("--password=beta"), format!("--password={REDACTED}"));
        // The whole point: a short secret must not eat a path that contains it.
        assert_eq!(redactor.redact_token("/srv/beta/vendor/bin/sail"), "/srv/beta/vendor/bin/sail");
        // A distinctive secret is redacted wherever it hides.
        assert_eq!(
            redactor.redact_token("--url=https://x/s3cr3tvaluelong/y"),
            format!("--url=https://x/{REDACTED}/y")
        );
    }

    #[test]
    fn missing_env_file_redacts_nothing() {
        let redactor = Redactor::from_env_file(Path::new("/nonexistent/.env"));
        assert_eq!(redactor.redact("anything"), "anything");
    }
}
