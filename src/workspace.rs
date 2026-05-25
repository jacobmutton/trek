use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::error::ErrorCode;

pub const TREK_TOML: &str = "trek.toml";

pub struct Workspace {
    pub dir: PathBuf,
    pub config: Config,
}

impl Workspace {
    pub fn toml_path(&self) -> PathBuf {
        self.dir.join(TREK_TOML)
    }
}

/// Resolve and load the workspace.
///
/// Precedence:
/// 1. `--workspace` flag / `TREK_WORKSPACE` env (clap merges these into one
///    `Option<PathBuf>`).
/// 2. Walk up from cwd looking for `trek.toml`.
/// 3. `NO_WORKSPACE`.
pub fn resolve(explicit: Option<&Path>) -> Result<Workspace, WorkspaceError> {
    let dir = if let Some(p) = explicit {
        let abs = absolutize(p).map_err(WorkspaceError::Io)?;
        if !abs.join(TREK_TOML).is_file() {
            return Err(WorkspaceError::Missing(abs));
        }
        abs
    } else {
        let cwd = std::env::current_dir().map_err(WorkspaceError::Io)?;
        match walk_up_for_toml(&cwd) {
            Some(d) => d,
            None => return Err(WorkspaceError::NotFound),
        }
    };
    let cfg = Config::load(&dir.join(TREK_TOML)).map_err(WorkspaceError::Load)?;
    Ok(Workspace { dir, config: cfg })
}

fn walk_up_for_toml(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(d) = cur {
        if d.join(TREK_TOML).is_file() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

fn absolutize(p: &Path) -> std::io::Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        let cwd = std::env::current_dir()?;
        Ok(cwd.join(p))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("no trek.toml found in cwd or any parent")]
    NotFound,
    #[error("no trek.toml at {0}")]
    Missing(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Load(anyhow::Error),
}

impl WorkspaceError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotFound | Self::Missing(_) => ErrorCode::NoWorkspace,
            Self::Io(_) | Self::Load(_) => ErrorCode::NoWorkspace,
        }
    }
}

/// Where to find a sibling file in the same workspace dir.
pub fn workspace_file(ws: &Workspace, name: &str) -> PathBuf {
    ws.dir.join(name)
}

pub fn ensure_dir(p: &Path) -> Result<()> {
    if !p.exists() {
        std::fs::create_dir_all(p).with_context(|| format!("create dir {}", p.display()))?;
    }
    Ok(())
}
