//! Per-ticket state record. One JSON file per ticket under
//! `$XDG_STATE_HOME/trek/<workspace-id>/tickets/<ticket>.json`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::{StateRoot, atomic_write};

/// Per-(repo, suffix) branch the ticket owns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub repo: String,
    /// `None` for the default branch (no `--suffix`).
    pub suffix: Option<String>,
    pub name: String,
    /// What was passed as `--from` (or the resolved baseline).
    pub from: String,
    pub worktree: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub branches: Vec<Branch>,
}

impl Ticket {
    pub fn new(id: String) -> Self {
        Self { id, branches: Vec::new() }
    }

    pub fn load(state: &StateRoot, id: &str) -> Result<Option<Self>> {
        let path = state.ticket_file(id);
        if !path.is_file() {
            return Ok(None);
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let t: Ticket = serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(Some(t))
    }

    pub fn save(&self, state: &StateRoot) -> Result<()> {
        let path = state.ticket_file(&self.id);
        let body = serde_json::to_vec_pretty(self)?;
        atomic_write(&path, &body)?;
        Ok(())
    }

    pub fn delete(state: &StateRoot, id: &str) -> Result<()> {
        let path = state.ticket_file(id);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(())
    }

    pub fn find(&self, repo: &str, suffix: Option<&str>) -> Option<&Branch> {
        self.branches
            .iter()
            .find(|b| b.repo == repo && b.suffix.as_deref() == suffix)
    }

    /// Insert or replace the branch for (repo, suffix). Returns true if a
    /// pre-existing entry was replaced.
    pub fn upsert(&mut self, b: Branch) -> bool {
        if let Some(pos) = self
            .branches
            .iter()
            .position(|x| x.repo == b.repo && x.suffix == b.suffix)
        {
            self.branches[pos] = b;
            true
        } else {
            self.branches.push(b);
            false
        }
    }
}

/// Enumerate all ticket records in the state dir.
pub fn list_all(state: &StateRoot) -> Result<Vec<Ticket>> {
    let dir = state.tickets_dir();
    if !dir.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("read dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let t: Ticket = serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", path.display()))?;
        out.push(t);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
