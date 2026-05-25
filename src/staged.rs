//! The "currently staged" record: which ticket (if any) is on preprod, plus
//! a per-repo snapshot of where preprod was before staging, so unstage can
//! restore it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::{StateRoot, atomic_write};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub repo: String,
    /// Branch the preprod checkout was on before staging. `None` for detached
    /// HEAD (which we refuse anyway, but the field is here for completeness).
    pub branch: Option<String>,
    pub head_sha: String,
    pub clean: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Staged {
    pub ticket: String,
    pub suffix: Option<String>,
    pub snapshots: Vec<Snapshot>,
    /// `true` if any per-repo checkout failed; the user must reconcile.
    #[serde(default)]
    pub partial: bool,
}

impl Staged {
    pub fn load(state: &StateRoot) -> Result<Option<Self>> {
        let p = state.staged_file();
        if !p.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let s: Staged =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", p.display()))?;
        Ok(Some(s))
    }

    pub fn save(&self, state: &StateRoot) -> Result<()> {
        let body = serde_json::to_vec_pretty(self)?;
        atomic_write(&state.staged_file(), &body)?;
        Ok(())
    }

    pub fn clear(state: &StateRoot) -> Result<()> {
        let p = state.staged_file();
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
        }
        Ok(())
    }
}
