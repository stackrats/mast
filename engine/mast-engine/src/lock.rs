//! Per-user engine ownership lock (plan §1): one engine instance owns
//! mutation; any other instance runs read-only (observation still converges).
//! Backed by `flock`, which the kernel releases automatically when the owner
//! dies — so stale-lock recovery is inherent; the stored PID is advisory,
//! for display.

use std::io::{Read, Seek, Write};
use std::path::PathBuf;

pub struct OwnershipLock {
    // Held open for the process lifetime; dropping releases the flock.
    _file: std::fs::File,
}

pub enum Ownership {
    Owned(OwnershipLock),
    ReadOnly { owner_pid: Option<u32> },
}

impl Ownership {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Ownership::ReadOnly { .. })
    }

    pub fn owner_pid(&self) -> Option<u32> {
        match self {
            Ownership::ReadOnly { owner_pid } => *owner_pid,
            Ownership::Owned(_) => None,
        }
    }
}

fn default_lock_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(runtime);
        if dir.is_dir() {
            return dir.join("mast");
        }
    }
    #[cfg(unix)]
    let suffix = unsafe { libc::getuid() }.to_string();
    #[cfg(not(unix))]
    let suffix = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    std::env::temp_dir().join(format!("mast-{suffix}"))
}

/// Try to become the mutation owner. `dir` overrides the lock location
/// (tests); production passes `None` for the XDG runtime dir.
pub fn acquire_ownership(dir: Option<PathBuf>) -> Ownership {
    let dir = dir.unwrap_or_else(default_lock_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("cannot create lock dir {}: {e}; assuming sole instance", dir.display());
    }
    let path = dir.join("engine.lock");
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!("cannot open {}: {e}; assuming sole instance", path.display());
            // Fail open: a broken lock dir must not brick the only instance.
            let placeholder = std::env::temp_dir().join("mast-lock-placeholder");
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(placeholder)
                .expect("temp dir must be writable");
            return Ownership::Owned(OwnershipLock { _file: file });
        }
    };

    #[cfg(unix)]
    let locked = {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    };
    // Windows adapter TODO: LockFileEx. Fail open — single-instance use.
    #[cfg(not(unix))]
    let locked = true;
    if locked {
        let _ = file.set_len(0);
        let _ = file.rewind();
        let _ = write!(file, "{}", std::process::id());
        let _ = file.flush();
        Ownership::Owned(OwnershipLock { _file: file })
    } else {
        let mut pid_text = String::new();
        let _ = file.rewind();
        let _ = file.read_to_string(&mut pid_text);
        Ownership::ReadOnly { owner_pid: pid_text.trim().parse().ok() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_read_only_until_owner_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Some(tmp.path().to_path_buf());

        let first = acquire_ownership(dir.clone());
        assert!(!first.is_read_only());

        // flock semantics are unix-only; Windows' documented behavior is
        // fail-open until the LockFileEx port lands (platform-pass TODO) —
        // pinned below so this test flips the day it does.
        #[cfg(unix)]
        {
            let second = acquire_ownership(dir.clone());
            assert!(second.is_read_only());
            assert_eq!(second.owner_pid(), Some(std::process::id()));
        }
        #[cfg(not(unix))]
        {
            let second = acquire_ownership(dir.clone());
            assert!(
                !second.is_read_only(),
                "fail-open is the documented Windows behavior until LockFileEx"
            );
        }

        drop(first);
        let third = acquire_ownership(dir);
        assert!(!third.is_read_only(), "the lock must release when the owner drops");
    }
}
