//! Per-repo state checks: dirty tree, detached HEAD, mid-op, unpushed
//! commits. Used by stage/unstage/refresh to bail out before touching git.

use serde::Serialize;

use crate::config::Repo;
use crate::error::ErrorCode;
use crate::git::read;

/// One repo's offenders. `None` means clean.
#[derive(Debug, Serialize)]
pub struct Issue {
    pub repo: String,
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

pub struct Opts {
    pub require_clean: bool,
    pub forbid_detached: bool,
    pub forbid_mid_op: bool,
    pub forbid_unpushed: bool,
}

impl Opts {
    pub fn strict() -> Self {
        Self {
            require_clean: true,
            forbid_detached: true,
            forbid_mid_op: true,
            forbid_unpushed: true,
        }
    }
}

pub fn check(repo: &Repo, opts: &Opts) -> Vec<Issue> {
    let mut out = Vec::new();
    let r = match read::open(&repo.path) {
        Ok(r) => r,
        Err(e) => {
            out.push(Issue {
                repo: repo.name.clone(),
                code: ErrorCode::Internal,
                message: format!("open repo: {e}"),
                details: None,
            });
            return out;
        }
    };

    if opts.forbid_detached {
        match read::current_branch(&r) {
            Ok(None) => out.push(Issue {
                repo: repo.name.clone(),
                code: ErrorCode::DetachedHead,
                message: "preprod is in a detached HEAD state".into(),
                details: None,
            }),
            Ok(Some(_)) => {}
            Err(e) => out.push(Issue {
                repo: repo.name.clone(),
                code: ErrorCode::Internal,
                message: format!("read HEAD: {e}"),
                details: None,
            }),
        }
    }

    if opts.forbid_mid_op {
        if let Some(op) = read::mid_operation(&r) {
            out.push(Issue {
                repo: repo.name.clone(),
                code: ErrorCode::MidOperation,
                message: format!("preprod is mid-{op}"),
                details: Some(serde_json::json!({ "operation": op })),
            });
        }
    }

    if opts.require_clean {
        match read::is_dirty(&r) {
            Ok(true) => {
                let summary = read::dirty_summary(&r, 10).unwrap_or_default();
                out.push(Issue {
                    repo: repo.name.clone(),
                    code: ErrorCode::DirtyWorktree,
                    message: format!("repo `{}` has uncommitted changes", repo.name),
                    details: Some(serde_json::json!({ "git_status": summary })),
                });
            }
            Ok(false) => {}
            Err(e) => out.push(Issue {
                repo: repo.name.clone(),
                code: ErrorCode::Internal,
                message: format!("status: {e}"),
                details: None,
            }),
        }
    }

    if opts.forbid_unpushed {
        if let Ok(Some(branch)) = read::current_branch(&r) {
            match read::unpushed_count(&r, &branch) {
                Ok(n) if n > 0 => out.push(Issue {
                    repo: repo.name.clone(),
                    code: ErrorCode::UnpushedCommits,
                    message: format!(
                        "branch `{branch}` in repo `{}` has {n} unpushed commits",
                        repo.name
                    ),
                    details: Some(serde_json::json!({ "branch": branch, "ahead": n })),
                }),
                _ => {}
            }
        }
    }
    out
}
