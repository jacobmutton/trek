use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

use crate::error::ErrorCode;

/// Holds an exclusive flock on the workspace lock file. Released on drop.
pub struct WorkspaceLock {
    file: File,
}

impl WorkspaceLock {
    /// Try to acquire the lock. If `block` is true, wait for it; otherwise
    /// return `Err(LOCKED)` on contention. Used so that `--non-interactive`
    /// can fail fast.
    pub fn acquire(path: &Path, block: bool) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(LockError::Io)?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(LockError::Io)?;
        if block {
            file.lock_exclusive().map_err(LockError::Io)?;
        } else {
            match file.try_lock_exclusive() {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(LockError::Contended);
                }
                Err(e) => return Err(LockError::Io(e)),
            }
        }
        Ok(Self { file })
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("workspace already locked by another process")]
    Contended,
    #[error("io: {0}")]
    Io(#[source] std::io::Error),
}

impl LockError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Contended => ErrorCode::Locked,
            Self::Io(_) => ErrorCode::Locked,
        }
    }
}

/// Convenience: open the workspace lock with the right blocking policy.
pub fn acquire(path: &Path, non_interactive: bool) -> Result<WorkspaceLock, LockError> {
    WorkspaceLock::acquire(path, !non_interactive)
}

#[allow(dead_code)]
pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).with_context(|| format!("create dir {}", p.display()))?;
    }
    Ok(())
}
