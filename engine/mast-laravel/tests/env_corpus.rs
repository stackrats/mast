//! env-corpus matrix (M5 verify criterion): every corpus file must round-trip
//! byte-exactly, decode per Docker's documented semantics, and — where the
//! compose CLI is available — agree with docker compose's own view of the
//! interpolation environment.

use std::path::PathBuf;
use std::time::Duration;

use mast_laravel::{EnvFile, EnvItem};

fn corpus(name: &str) -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/env-corpus").join(name);
    std::fs::read_to_string(path).expect("corpus file")
}

const FILES: [&str; 7] = [
    "basic.env",
    "quoting.env",
    "multiline.env",
    "interpolation.env",
    "laravel-real.env",
    "weird.env",
    "crlf.env",
];

#[test]
fn every_corpus_file_roundtrips_byte_exactly() {
    for name in FILES {
        let src = corpus(name);
        let parsed = EnvFile::parse(&src);
        assert_eq!(parsed.to_string(), src, "{name} did not round-trip");
    }
}

#[test]
fn decode_matrix() {
    let basic = EnvFile::parse(&corpus("basic.env"));
    assert_eq!(basic.get("APP_PORT").unwrap().value, "8080");
    assert_eq!(basic.get("DB_PORT").unwrap().value, "3306");
    assert_eq!(basic.get("DB_PORT").unwrap().inline_comment.as_deref(), Some(" # default mysql port"));
    assert!(basic.get("CACHE_DRIVER").unwrap().export);
    assert_eq!(basic.get("EMPTY_VALUE").unwrap().value, "");

    let quoting = EnvFile::parse(&corpus("quoting.env"));
    assert_eq!(quoting.get("SQ_LITERAL").unwrap().value, "no $INTERP here");
    assert_eq!(quoting.get("SQ_HASH").unwrap().value, "not # a comment");
    assert_eq!(
        quoting.get("DQ_ESCAPES").unwrap().value,
        "line1\nline2\t\"quoted\" backslash\\ end"
    );
    assert_eq!(quoting.get("DQ_HASH").unwrap().value, "also # not a comment");
    assert_eq!(quoting.get("SPACED_EQ").unwrap().value, "spaced");
    assert_eq!(quoting.get("COLON_STYLE").unwrap().value, "colon-value");

    let multi = EnvFile::parse(&corpus("multiline.env"));
    assert_eq!(multi.get("SQ_MULTI").unwrap().value, "first line\nsecond line\nthird line");
    assert_eq!(multi.get("DQ_MULTI").unwrap().value, "alpha\nbeta");
    assert_eq!(multi.get("AFTER").unwrap().value, "2");

    let interp = EnvFile::parse(&corpus("interpolation.env"));
    // Interpolation stays RAW in the model.
    assert_eq!(interp.get("FULL_URL").unwrap().value, "https://${BASE_HOST}/api");
    assert_eq!(interp.get("SQ_NO_INTERP").unwrap().value, "host is ${BASE_HOST}");

    let weird = EnvFile::parse(&corpus("weird.env"));
    let unknowns: Vec<&str> = weird
        .items()
        .filter_map(|i| match i {
            EnvItem::Unknown(raw) => Some(raw.as_str()),
            _ => None,
        })
        .collect();
    assert!(unknowns.iter().any(|u| u.contains("WEIRD_JUNK")), "{unknowns:?}");
    assert!(unknowns.iter().any(|u| u.contains("1LEADING_DIGIT")), "{unknowns:?}");
    assert_eq!(weird.get("UNICODE").unwrap().value, "café ☕");
    assert_eq!(weird.get("DOTTED.KEY").unwrap().value, "ok");
    assert_eq!(weird.get("NO_FINAL_NEWLINE").unwrap().value, "yes");
}

#[test]
fn edits_touch_only_their_line() {
    let src = corpus("laravel-real.env");
    let mut file = EnvFile::parse(&src);
    file.set("APP_PORT", "9000").unwrap();
    let out = file.to_string();
    // Everything except the APP_PORT line is untouched.
    let changed: Vec<(&str, &str)> = src
        .lines()
        .zip(out.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(changed, vec![("APP_PORT=8099", "APP_PORT=9000")]);
    // The tricky password survived verbatim.
    assert!(out.contains("DB_PASSWORD='secret#with$pecials'"));
}

async fn compose_cli_available() -> bool {
    mast_docker::run_command(
        &["docker".into(), "compose".into(), "version".into()],
        None,
        &[],
        Duration::from_secs(5),
        4096,
    )
    .await
    .map(|o| o.success())
    .unwrap_or(false)
}

/// Cross-check our decoding against docker compose's own dotenv parser via
/// `config --environment` (prints the interpolation environment). Limited to
/// single-line values without `$` (compose expands interpolation; we keep it
/// raw by design).
#[tokio::test(flavor = "multi_thread")]
async fn compose_agrees_with_our_decoding() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI unavailable");
        return;
    }
    for name in ["basic.env", "quoting.env", "laravel-real.env"] {
        let src = corpus(name);
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".env"), &src).unwrap();
        std::fs::write(
            tmp.path().join("compose.yaml"),
            "services:\n  probe:\n    image: alpine\n",
        )
        .unwrap();
        let out = mast_docker::run_command(
            &["docker".into(), "compose".into(), "config".into(), "--environment".into()],
            Some(tmp.path()),
            &[],
            Duration::from_secs(20),
            1024 * 1024,
        )
        .await
        .unwrap();
        assert!(out.success(), "{name}: config --environment failed: {}", out.stderr);

        let compose_view: std::collections::HashMap<&str, &str> = out
            .stdout
            .lines()
            .filter_map(|l| l.split_once('='))
            .collect();
        let ours = EnvFile::parse(&src);
        let mut compared = 0;
        for entry in ours.entries() {
            if entry.value.contains('$') || entry.value.contains('\n') {
                continue;
            }
            let Some(theirs) = compose_view.get(entry.key.as_str()) else {
                continue; // e.g. dotted keys compose may skip
            };
            assert_eq!(
                *theirs, entry.value,
                "{name}: {} decodes differently (compose={theirs:?}, ours={:?})",
                entry.key, entry.value
            );
            compared += 1;
        }
        assert!(compared >= 5, "{name}: only {compared} comparable entries");
    }
}
