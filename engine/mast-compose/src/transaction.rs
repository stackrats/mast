//! The write transaction (plan §6): every config write goes through eight
//! gates, and failure at ANY gate refuses the edit — the worst case is a
//! refused edit, never a mangled file.
//!
//! 1. read bytes + mode + hash + symlink status
//! 2. build splices in memory (each individually verified by mast-yaml-edit)
//! 3. generic YAML re-parse sanity (inside mast-yaml-edit's apply)
//! 4. `docker compose config --quiet` on the EDITED content, using the exact
//!    ComposeInvocation with the target file substituted
//! 5. targeted semantic verification (mast-yaml-edit's delta gate — whole-doc
//!    equality is unsound under anchors/merge keys)
//! 6. recheck the source hash immediately before commit (external-edit race)
//! 7. atomic write preserving mode + line endings (rename over target)
//! 8. timestamped backup for recovery

use std::path::{Path, PathBuf};
use std::time::Duration;

use mast_yaml_edit::Edit;
use sha2::{Digest, Sha256};

use crate::{ComposeInvocation, Runner};

#[derive(Debug, thiserror::Error)]
pub enum ComposeEditError {
    #[error("{0} is not one of this project's compose files")]
    NotAnInvocationFile(PathBuf),
    #[error("refusing to edit symlink {0} (follow it manually if intended)")]
    SymlinkRefused(PathBuf),
    #[error("file is not valid UTF-8: {0}")]
    NotUtf8(PathBuf),
    #[error(transparent)]
    Edit(#[from] mast_yaml_edit::YamlEditError),
    #[error("docker compose rejects the edited file: {0}")]
    ValidationFailed(String),
    #[error("file changed on disk while editing (external edit) — reload and retry")]
    ConflictExternalEdit,
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct EditReceipt {
    pub file: PathBuf,
    pub backup: Option<PathBuf>,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ComposeEditError + '_ {
    move |source| ComposeEditError::Io { path: path.to_path_buf(), source }
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Validate the edited content by pointing `docker compose config --quiet` at
/// a temp copy substituted for the target file, with the project directory
/// pinned so `.env`/interpolation behave identically (ADR-0001). Offline-safe
/// (config never talks to the daemon).
async fn validate_with_compose(
    invocation: &ComposeInvocation,
    target: &Path,
    edited: &str,
) -> Result<(), ComposeEditError> {
    let dir = target.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.mast-validate-{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, edited).map_err(io_err(&tmp))?;

    let mut argv: Vec<String> = vec![
        "docker".into(),
        "compose".into(),
        "--project-directory".into(),
        invocation.project_dir.to_string_lossy().into_owned(),
    ];
    for file in &invocation.files {
        argv.push("-f".into());
        let path = if file.path == target { &tmp } else { &file.path };
        argv.push(path.to_string_lossy().into_owned());
    }
    for profile in &invocation.profiles {
        argv.push("--profile".into());
        argv.push(profile.clone());
    }
    argv.extend(["config".into(), "--quiet".into()]);

    let result = mast_docker::run_command(
        &argv,
        Some(&invocation.project_dir),
        &[],
        Duration::from_secs(20),
        256 * 1024,
    )
    .await;
    let _ = std::fs::remove_file(&tmp);
    let output = result.map_err(|e| ComposeEditError::ValidationFailed(e.to_string()))?;
    if !output.success() {
        return Err(ComposeEditError::ValidationFailed(output.stderr.trim().to_string()));
    }
    Ok(())
}

/// Apply `edits` to `file` under the full write transaction.
pub async fn apply_compose_edit(
    invocation: &ComposeInvocation,
    file: &Path,
    edits: &[Edit],
    backup_dir: Option<&Path>,
) -> Result<EditReceipt, ComposeEditError> {
    // Gate 0: only files that are part of this invocation may be edited.
    let target = invocation
        .files
        .iter()
        .find(|f| f.path == file || f.path.canonicalize().ok().as_deref() == Some(file))
        .map(|f| f.path.clone())
        .ok_or_else(|| ComposeEditError::NotAnInvocationFile(file.to_path_buf()))?;

    // Gate 1: read + mode + hash + symlink status.
    let meta = std::fs::symlink_metadata(&target).map_err(io_err(&target))?;
    if meta.file_type().is_symlink() {
        return Err(ComposeEditError::SymlinkRefused(target));
    }
    let original_bytes = std::fs::read(&target).map_err(io_err(&target))?;
    let original = String::from_utf8(original_bytes.clone())
        .map_err(|_| ComposeEditError::NotUtf8(target.clone()))?;
    let original_hash = hash(&original_bytes);

    // Gates 2+3+5: splice, re-parse, targeted semantic verification.
    let edited = mast_yaml_edit::apply_all(&original, edits)?;

    // Gate 4: compose-level validation of the edited content.
    if !matches!(invocation.runner, Runner::Sail { .. } | Runner::DockerCompose) {
        unreachable!("runner variants covered");
    }
    validate_with_compose(invocation, &target, &edited).await?;

    // Gate 6: the source must not have changed underneath us.
    let current = std::fs::read(&target).map_err(io_err(&target))?;
    if hash(&current) != original_hash {
        return Err(ComposeEditError::ConflictExternalEdit);
    }

    // Gate 8 (before commit so a failed backup blocks the write): backup.
    let backup = if let Some(dir) = backup_dir {
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup_path = dir.join(format!(
            "{}.{ts}.bak",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::write(&backup_path, &original_bytes).map_err(io_err(&backup_path))?;
        Some(backup_path)
    } else {
        None
    };

    // Gate 7: atomic write preserving mode; line endings are preserved by the
    // splice engine itself.
    let dir = target.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.mast-write-{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(edited.as_bytes()).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
    }
    std::fs::set_permissions(&tmp, meta.permissions()).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, &target).map_err(io_err(&target))?;

    Ok(EditReceipt { file: target, backup })
}
