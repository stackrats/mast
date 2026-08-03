//! Write-transaction tests (M5 verify criteria). Docker-gated pieces skip
//! cleanly when the compose CLI is unavailable; `config` itself is offline
//! (ADR-0001 finding 6) so a daemon is never required.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use mast_compose::{ComposeEditError, apply_compose_edit, resolve_invocation};
use mast_yaml_edit::{Edit, key};

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

const BASE: &str = "services:\n  app:\n    image: alpine:latest # pinned\n    environment:\n      FOO: bar\n      COUNT: '1'\n    ports:\n      - \"8080:80\"\n      - \"9090:90\"\n  db:\n    image: mysql:8\n";

fn setup(dir: &Path) -> std::path::PathBuf {
    let file = dir.join("compose.yaml");
    std::fs::write(&file, BASE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    file
}

#[tokio::test(flavor = "multi_thread")]
async fn happy_path_writes_atomically_with_backup_and_mode() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI unavailable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = setup(tmp.path());
    let invocation = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
    let backups = tmp.path().join("backups");

    let receipt = apply_compose_edit(
        &invocation,
        &file,
        &[Edit::SetScalar {
            path: vec![key("services"), key("app"), key("image")],
            value: "alpine:3.20".into(),
        }],
        Some(&backups),
    )
    .await
    .unwrap();

    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.contains("image: alpine:3.20 # pinned"));
    assert!(after.contains("mysql:8"), "unrelated content untouched");
    // Backup holds the original bytes.
    let backup = receipt.backup.unwrap();
    assert_eq!(std::fs::read_to_string(backup).unwrap(), BASE);
    // Mode preserved through the atomic replace.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_invalid_edit_is_refused_by_compose_validation() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI unavailable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = setup(tmp.path());
    let invocation = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();

    // YAML-valid but compose-schema-invalid: image must be a string.
    let result = apply_compose_edit(
        &invocation,
        &file,
        &[Edit::SetScalar {
            path: vec![key("services"), key("app"), key("image")],
            value: "[1, 2]".into(),
        }],
        None,
    )
    .await;
    assert!(matches!(result, Err(ComposeEditError::ValidationFailed(_))), "got {result:?}");
    // And the file is untouched.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), BASE);
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_files_and_symlinks_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let file = setup(tmp.path());
    let invocation = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();

    let foreign = tmp.path().join("other.yaml");
    std::fs::write(&foreign, "services: {}\n").unwrap();
    let result = apply_compose_edit(&invocation, &foreign, &[], None).await;
    assert!(matches!(result, Err(ComposeEditError::NotAnInvocationFile(_))));

    #[cfg(unix)]
    {
        let link_dir = tempfile::tempdir().unwrap();
        let real = link_dir.path().join("real.yaml");
        std::fs::write(&real, BASE).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&real, &file).unwrap();
        let invocation = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();
        let result = apply_compose_edit(
            &invocation,
            &file,
            &[Edit::SetScalar {
                path: vec![key("services"), key("app"), key("image")],
                value: "x:1".into(),
            }],
            None,
        )
        .await;
        assert!(matches!(result, Err(ComposeEditError::SymlinkRefused(_))), "got {result:?}");
    }
}

/// M5 verify: random edit sequences always leave the file compose-valid.
/// Deterministic LCG so failures reproduce.
#[tokio::test(flavor = "multi_thread")]
async fn property_random_edit_sequences_stay_compose_valid() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI unavailable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = setup(tmp.path());
    let invocation = resolve_invocation(tmp.path(), &HashMap::new()).unwrap();

    let mut state: u64 = 0x5DEECE66D;
    let mut next = |bound: u64| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) % bound
    };

    let mut inserted_env = 0u32;
    let mut applied = 0;
    for round in 0..25 {
        let edit = match next(4) {
            0 => Edit::SetScalar {
                path: vec![key("services"), key("app"), key("environment"), key("COUNT")],
                value: format!("'{round}'"),
            },
            1 => Edit::SetScalar {
                path: vec![key("services"), key("db"), key("image")],
                value: format!("mysql:8.{}", next(30)),
            },
            2 => {
                inserted_env += 1;
                Edit::InsertMapKey {
                    path: vec![key("services"), key("app"), key("environment")],
                    key: format!("GEN_{inserted_env}"),
                    value: format!("'v{round}'"),
                }
            }
            _ => Edit::InsertSeqItem {
                path: vec![key("services"), key("app"), key("ports")],
                value: format!("\"{}:{}\"", 10000 + round, 80 + round),
            },
        };
        apply_compose_edit(&invocation, &file, &[edit], None)
            .await
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        applied += 1;
    }
    assert_eq!(applied, 25);
    // Original comment and quoting style survived 25 transactions.
    let final_content = std::fs::read_to_string(&file).unwrap();
    assert!(final_content.contains("# pinned"));
    assert!(final_content.contains("- \"8080:80\""));
}
