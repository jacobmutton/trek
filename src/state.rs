use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::workspace::ensure_dir;

/// On-disk state root: `$XDG_STATE_HOME/trek/<workspace-id>/`.
pub struct StateRoot {
    pub root: PathBuf,
}

impl StateRoot {
    pub fn for_workspace(id: Uuid) -> Result<Self> {
        let base = xdg_state_base()?;
        let root = base.join("trek").join(id.to_string());
        ensure_dir(&root)?;
        ensure_dir(&root.join("tickets"))?;
        Ok(Self { root })
    }

    pub fn tickets_dir(&self) -> PathBuf {
        self.root.join("tickets")
    }
    pub fn ticket_file(&self, ticket: &str) -> PathBuf {
        self.tickets_dir().join(format!("{ticket}.json"))
    }
    pub fn staged_file(&self) -> PathBuf {
        self.root.join("staged.json")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.root.join("lock")
    }
    pub fn audit_file(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }
}

fn xdg_state_base() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().context("cannot resolve $HOME")?;
    Ok(home.join(".local").join("state"))
}

/// Atomically write `bytes` to `path` via a sibling tmp file + rename.
/// Caller is responsible for the parent dir existing.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("no parent for {}", path.display()))?;
    let file_name = path
        .file_name()
        .with_context(|| format!("no file name for {}", path.display()))?;
    let mut tmp = parent.to_path_buf();
    tmp.push(format!(".{}.tmp", file_name.to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
