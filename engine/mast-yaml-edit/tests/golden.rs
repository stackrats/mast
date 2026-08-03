//! Golden tests over `fixtures/compose-corpus` (M5 verify criterion):
//! every corpus file parses error-free, and targeted edits are byte-exact
//! outside their splice. The corpus traps CRLF, missing trailing newlines,
//! block-scalar chomping, comments-everywhere, quoted keys, and sail's
//! 4-space style — reformatters must never touch it (see its README).

use std::path::PathBuf;

use mast_yaml_edit::{Edit, YamlEditError, apply, index, key};

fn corpus(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/compose-corpus").join(name);
    std::fs::read_to_string(path).expect("corpus file")
}

/// Byte-exactness outside the splice, by construction check: prefix and
/// suffix around the OLD range must be identical.
fn assert_spliced(original: &str, applied: &mast_yaml_edit::AppliedEdit) {
    let (start, end) = applied.old_range;
    assert_eq!(&applied.new_source[..start], &original[..start], "prefix changed");
    let tail = original.len() - end;
    assert_eq!(
        &applied.new_source[applied.new_source.len() - tail..],
        &original[end..],
        "suffix changed"
    );
}

#[test]
fn sail_stock_edits_are_byte_exact() {
    let src = corpus("sail-stock.yaml");
    for edit in [
        Edit::SetScalar {
            path: vec![key("services"), key("laravel.test"), key("environment"), key("XDEBUG_MODE")],
            value: "'develop,debug'".into(),
        },
        Edit::SetScalar {
            path: vec![key("services"), key("redis"), key("image")],
            value: "'valkey/valkey:alpine'".into(),
        },
        Edit::InsertMapKey {
            path: vec![key("services"), key("mysql"), key("environment")],
            key: "MYSQL_ALLOW_EMPTY_PASSWORD".into(),
            value: "'yes'".into(),
        },
        Edit::InsertSeqItem {
            path: vec![key("services"), key("laravel.test"), key("depends_on")],
            value: "meilisearch".into(),
        },
        Edit::RemoveKey {
            path: vec![key("services"), key("mysql"), key("environment"), key("MYSQL_DATABASE")],
        },
        Edit::RemoveSeqItem {
            path: vec![key("services"), key("laravel.test"), key("ports"), index(1)],
        },
    ] {
        let applied = apply(&src, &edit).unwrap_or_else(|e| panic!("{edit:?}: {e}"));
        assert_spliced(&src, &applied);
    }
}

#[test]
fn comments_everywhere_edits_keep_comments() {
    let src = corpus("comments-everywhere.yaml");
    let applied = apply(
        &src,
        &Edit::SetScalar {
            path: vec![key("services"), key("app"), key("environment"), key("FOO")],
            value: "barbar".into(),
        },
    )
    .unwrap();
    assert_spliced(&src, &applied);
    assert!(applied.new_source.contains("barbar # after scalar"));

    let applied = apply(
        &src,
        &Edit::InsertMapKey {
            path: vec![key("services"), key("app"), key("environment")],
            key: "NEWKEY".into(),
            value: "newval".into(),
        },
    )
    .unwrap();
    // The last pair's trailing comment survives, insertion lands after it.
    assert!(applied.new_source.contains("BAZ: qux # last pair has a comment too"));
    assert!(applied.new_source.contains("      NEWKEY: newval\n"));

    // Removing a commented pair takes its whole line, nothing else.
    let applied = apply(
        &src,
        &Edit::RemoveKey { path: vec![key("services"), key("app"), key("environment"), key("FOO")] },
    )
    .unwrap();
    assert!(!applied.new_source.contains("FOO: bar"));
    assert!(applied.new_source.contains("# between pairs"));
}

#[test]
fn crlf_corpus_stays_crlf() {
    let src = corpus("crlf.yaml");
    assert!(src.contains("\r\n"));
    let applied = apply(
        &src,
        &Edit::InsertMapKey {
            path: vec![key("services"), key("app"), key("environment")],
            key: "CRLFKEY".into(),
            value: "v1".into(),
        },
    )
    .unwrap();
    assert!(applied.new_source.contains("CRLFKEY: v1\r\n"));
    assert!(!applied.new_source.replace("\r\n", "").contains('\n'));
}

#[test]
fn no_trailing_newline_corpus_handled() {
    let src = corpus("no-trailing-newline.yaml");
    assert!(!src.ends_with('\n'));
    let applied = apply(
        &src,
        &Edit::SetScalar {
            path: vec![key("services"), key("app"), key("image")],
            value: "repo/app:2".into(),
        },
    )
    .unwrap();
    assert_spliced(&src, &applied);
    let applied = apply(
        &src,
        &Edit::InsertMapKey {
            path: vec![key("services"), key("app")],
            key: "NEWK".into(),
            value: "v".into(),
        },
    )
    .unwrap();
    assert!(applied.new_source.contains("    NEWK: v"));
}

#[test]
fn quoted_and_weird_keys_resolve() {
    let src = corpus("weird-keys.yaml");
    let applied = apply(
        &src,
        &Edit::SetScalar {
            path: vec![key("services"), key("my.service-name"), key("labels"), key("com.example/slash")],
            value: "changed".into(),
        },
    )
    .unwrap();
    assert_spliced(&src, &applied);
    assert!(applied.new_source.contains("\"com.example/slash\": changed"));
}

#[test]
fn block_scalar_replacement_spares_the_neighbours() {
    let src = corpus("block-scalars.yaml");
    let applied = apply(
        &src,
        &Edit::SetScalar {
            path: vec![key("services"), key("app"), key("environment"), key("FOLDED")],
            value: "plain-now".into(),
        },
    )
    .unwrap();
    assert_spliced(&src, &applied);
    assert!(applied.new_source.contains("AFTER: plain"));
}

#[test]
fn anchors_merge_edits_inside_anchor_refused() {
    let src = corpus("anchors-merge.yaml");
    let result = apply(
        &src,
        &Edit::SetScalar { path: vec![key("x-base"), key("restart")], value: "always".into() },
    );
    assert!(matches!(result, Err(YamlEditError::VerifyFailed(_))), "got {result:?}");
    // Editing OUTSIDE the anchored region works fine.
    let applied = apply(
        &src,
        &Edit::SetScalar {
            path: vec![key("services"), key("worker"), key("image")],
            value: "repo/worker:2".into(),
        },
    )
    .unwrap();
    assert_spliced(&src, &applied);
}

#[test]
fn every_corpus_file_is_error_free_for_the_editor() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/compose-corpus");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.path().extension().is_some_and(|e| e == "yaml") {
            let src = std::fs::read_to_string(entry.path()).unwrap();
            // A no-op probe: resolving a bogus path must yield PathNotFound,
            // never Parse — i.e. the editor can parse every corpus file.
            match apply(
                &src,
                &Edit::SetScalar { path: vec![key("zz-definitely-missing")], value: "x".into() },
            ) {
                Err(YamlEditError::PathNotFound(_)) => checked += 1,
                Err(YamlEditError::Unsupported(_)) => checked += 1, // scalar-root docs
                other => panic!("{}: unexpected {other:?}", entry.path().display()),
            }
        }
    }
    assert!(checked >= 12, "expected the full corpus, checked {checked}");
}
