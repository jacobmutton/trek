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

#[cfg(test)]
mod tests {
    use super::*;

    fn b(repo: &str, suffix: Option<&str>, name: &str) -> Branch {
        Branch {
            repo: repo.into(),
            suffix: suffix.map(String::from),
            name: name.into(),
            from: "main".into(),
            worktree: PathBuf::from("/tmp/wt"),
        }
    }

    #[test]
    fn upsert_inserts_then_replaces() {
        let mut t = Ticket::new("FUN-1".into());
        let inserted = t.upsert(b("api", None, "br1"));
        assert!(!inserted);
        assert_eq!(t.branches.len(), 1);

        // Replace the (api, None) branch
        let replaced = t.upsert(b("api", None, "br2"));
        assert!(replaced);
        assert_eq!(t.branches.len(), 1);
        assert_eq!(t.branches[0].name, "br2");

        // Different suffix is a different entry.
        t.upsert(b("api", Some("migration"), "br3"));
        assert_eq!(t.branches.len(), 2);
    }

    #[test]
    fn find_by_repo_and_suffix() {
        let mut t = Ticket::new("FUN-1".into());
        t.upsert(b("api", None, "default"));
        t.upsert(b("api", Some("migration"), "with-mig"));
        t.upsert(b("web", None, "web-default"));

        assert_eq!(t.find("api", None).unwrap().name, "default");
        assert_eq!(t.find("api", Some("migration")).unwrap().name, "with-mig");
        assert_eq!(t.find("web", None).unwrap().name, "web-default");
        assert!(t.find("web", Some("missing")).is_none());
        assert!(t.find("nope", None).is_none());
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateRoot {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(state.tickets_dir()).unwrap();

        let mut t = Ticket::new("FUN-42".into());
        t.upsert(b("api", None, "br-a"));
        t.upsert(b("web", Some("migration"), "br-w-mig"));
        t.save(&state).unwrap();

        let loaded = Ticket::load(&state, "FUN-42").unwrap().unwrap();
        assert_eq!(loaded.id, "FUN-42");
        assert_eq!(loaded.branches.len(), 2);
        assert_eq!(loaded.find("api", None).unwrap().name, "br-a");

        Ticket::delete(&state, "FUN-42").unwrap();
        assert!(Ticket::load(&state, "FUN-42").unwrap().is_none());
    }

    #[test]
    fn list_all_returns_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateRoot {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(state.tickets_dir()).unwrap();

        for id in &["B-2", "A-1", "C-3"] {
            let mut t = Ticket::new((*id).into());
            t.upsert(b("api", None, "x"));
            t.save(&state).unwrap();
        }
        let ids: Vec<_> = list_all(&state).unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["A-1", "B-2", "C-3"]);
    }

    #[test]
    fn list_all_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateRoot {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(state.tickets_dir()).unwrap();
        assert!(list_all(&state).unwrap().is_empty());
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
