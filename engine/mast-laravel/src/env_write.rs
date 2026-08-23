//! Env write transaction: the compose write path's smaller sibling — read +
//! hash, mutate through the lossless model, recheck for external edits,
//! atomic write preserving mode, timestamped backup. No compose validation
//! step (env schema is free-form); resolution/reconcile picks up behavioral
//! changes (e.g. COMPOSE_FILE) via the file watcher.

use std::path::{Path, PathBuf};

use crate::env::{EnvError, EnvFile};

#[derive(Debug, thiserror::Error)]
pub enum EnvWriteError {
    #[error(transparent)]
    Env(#[from] EnvError),
    #[error("refusing to edit symlink {0}")]
    SymlinkRefused(PathBuf),
    #[error(".env changed on disk while editing — reload and retry")]
    Conflict,
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> EnvWriteError + '_ {
    move |source| EnvWriteError::Io { path: path.to_path_buf(), source }
}

/// Apply `mutate` to the env file at `path` (created if absent). Returns the
/// backup path when a backup directory is provided and the file existed.
pub fn edit_env_file(
    path: &Path,
    backup_dir: Option<&Path>,
    mutate: impl FnOnce(&mut EnvFile) -> Result<(), EnvError>,
) -> Result<Option<PathBuf>, EnvWriteError> {
    let existed = path.exists();
    let (original, mode) = if existed {
        let meta = std::fs::symlink_metadata(path).map_err(io_err(path))?;
        if meta.file_type().is_symlink() {
            return Err(EnvWriteError::SymlinkRefused(path.to_path_buf()));
        }
        (std::fs::read(path).map_err(io_err(path))?, Some(meta.permissions()))
    } else {
        (Vec::new(), None)
    };
    let source = String::from_utf8_lossy(&original).into_owned();

    let mut file = EnvFile::parse(&source);
    mutate(&mut file)?;
    let edited = file.to_string();

    // External-edit guard immediately before commit.
    if existed {
        let current = std::fs::read(path).map_err(io_err(path))?;
        if current != original {
            return Err(EnvWriteError::Conflict);
        }
    }

    let backup = match (backup_dir, existed) {
        (Some(dir), true) => {
            std::fs::create_dir_all(dir).map_err(io_err(dir))?;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup_path = dir.join(format!(
                "{}.{ts}.bak",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            std::fs::write(&backup_path, &original).map_err(io_err(&backup_path))?;
            Some(backup_path)
        }
        _ => None,
    };

    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!(
        ".{}.mast-write-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(edited.as_bytes()).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
    }
    if let Some(mode) = mode {
        std::fs::set_permissions(&tmp, mode).map_err(io_err(&tmp))?;
    }
    std::fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(backup)
}

/// Rewrite CRLF line endings to LF, same discipline as [`edit_env_file`]
/// (symlink refusal, backup, atomic write, mode preserved). Returns the
/// backup path, or `None` as a no-op when the file has no CRLF.
///
/// Why this matters at all: the sail script `source`s `.env` with bash, so
/// CRLF appends an invisible `\r` to every value — `APP_SERVICE` becomes
/// `laravel.test\r` and compose reports `service "laravel.test\r" is not
/// running`. Only the `\r` of a CRLF pair is touched; a stray `\r` inside a
/// value is left exactly where it was.
pub fn normalize_env_line_endings(
    path: &Path,
    backup_dir: Option<&Path>,
) -> Result<Option<PathBuf>, EnvWriteError> {
    let meta = std::fs::symlink_metadata(path).map_err(io_err(path))?;
    if meta.file_type().is_symlink() {
        return Err(EnvWriteError::SymlinkRefused(path.to_path_buf()));
    }
    let original = std::fs::read(path).map_err(io_err(path))?;
    if !original.windows(2).any(|w| w == b"\r\n") {
        return Ok(None);
    }
    let mut edited = Vec::with_capacity(original.len());
    let mut i = 0;
    while i < original.len() {
        if original[i] == b'\r' && original.get(i + 1) == Some(&b'\n') {
            i += 1; // drop the \r, keep the \n
            continue;
        }
        edited.push(original[i]);
        i += 1;
    }

    let backup = match backup_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(io_err(dir))?;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup_path = dir.join(format!(
                "{}.{ts}.bak",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            std::fs::write(&backup_path, &original).map_err(io_err(&backup_path))?;
            Some(backup_path)
        }
        None => None,
    };

    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!(
        ".{}.mast-write-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(&edited).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
    }
    std::fs::set_permissions(&tmp, meta.permissions()).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_are_atomic_with_backup_and_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join(".env");
        std::fs::write(&env, "A=1\nB=2 # keep\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let backups = tmp.path().join("backups");
        let backup = edit_env_file(&env, Some(&backups), |f| f.set("B", "changed"))
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "A=1\nB=changed # keep\n");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "A=1\nB=2 # keep\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&env).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn crlf_normalizes_with_backup_and_values_stay_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join(".env");
        std::fs::write(&env, "APP_SERVICE=laravel.test\r\nDB_HOST=mysql\r\n").unwrap();
        let backups = tmp.path().join("backups");
        let backup = normalize_env_line_endings(&env, Some(&backups)).unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(&env).unwrap(),
            "APP_SERVICE=laravel.test\nDB_HOST=mysql\n"
        );
        assert!(std::fs::read_to_string(backup).unwrap().contains("\r\n"));

        // Already clean: explicit no-op, no backup churn.
        assert!(normalize_env_line_endings(&env, Some(&backups)).unwrap().is_none());
    }

    #[test]
    fn creates_missing_file_and_refuses_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join(".env");
        edit_env_file(&env, None, |f| f.set("NEW", "1")).unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "NEW=1\n");

        std::fs::write(&env, "D=1\nD=2\n").unwrap();
        let result = edit_env_file(&env, None, |f| f.set("D", "x"));
        assert!(matches!(result, Err(EnvWriteError::Env(EnvError::DuplicateKey(_)))));
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "D=1\nD=2\n", "file untouched");
    }
}
