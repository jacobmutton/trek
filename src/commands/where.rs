use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;

use crate::commands::require_workspace;
use crate::config::Config;
use crate::output::{OutputCtx, emit_ok};

#[derive(Serialize)]
struct WhereData {
    workspace: PathBuf,
    workspace_id: uuid::Uuid,
    ticket: Option<String>,
    suffix: Option<String>,
    repo: Option<String>,
    location: Option<&'static str>,
}

pub fn run(ctx: OutputCtx, workspace: Option<&Path>) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "where") {
        Ok(w) => w,
        Err(code) => return code,
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (ticket, suffix, repo, location) = classify(&ws.config, &cwd);
    emit_ok(
        ctx,
        "where",
        WhereData {
            workspace: ws.dir.clone(),
            workspace_id: ws.config.workspace.id,
            ticket: ticket.clone(),
            suffix: suffix.clone(),
            repo: repo.clone(),
            location,
        },
        || {
            eprintln!("workspace: {}", ws.dir.display());
            eprintln!("  id: {}", ws.config.workspace.id);
            match (&repo, location) {
                (Some(r), Some(loc)) => {
                    let t = ticket.as_deref().unwrap_or("?");
                    let s = suffix
                        .as_deref()
                        .map(|s| format!(" @{s}"))
                        .unwrap_or_default();
                    eprintln!("  in: {loc} of {r} for {t}{s}");
                }
                _ => eprintln!("  in: outside any repo"),
            }
        },
    )
}

/// Walk repos and worktree dirs and figure out which (ticket, suffix, repo,
/// preprod|worktree) `cwd` falls inside.
fn classify(
    cfg: &Config,
    cwd: &Path,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<&'static str>,
) {
    for r in &cfg.repos {
        if path_starts_with(cwd, &r.path) {
            return (None, None, Some(r.name.clone()), Some("preprod"));
        }
        if path_starts_with(cwd, &r.worktree_dir) {
            // The next path component under worktree_dir is `<ticket>` or
            // `<ticket><join><suffix>`.
            let rel = cwd.strip_prefix(&r.worktree_dir).ok();
            if let Some(rel) = rel {
                let leaf = rel
                    .components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy().to_string());
                if let Some(leaf) = leaf {
                    let join = &cfg.branch.suffix_join;
                    let (ticket, suffix) = if let Some(idx) = leaf.find(join.as_str()) {
                        // Heuristic: a join hit means `<ticket><join><suffix>`.
                        // Tickets in the wild often contain dashes too
                        // (FUN-1234), so this is best-effort. We don't have
                        // enough info to disambiguate without consulting the
                        // ticket registry; for now, treat as no-suffix unless
                        // the leaf contains the join more than once.
                        let count = leaf.matches(join.as_str()).count();
                        if count >= 2 {
                            (
                                leaf[..idx + leaf[idx..].find(join.as_str()).unwrap_or(0)].to_string(),
                                Some(leaf[idx + join.len()..].to_string()),
                            )
                        } else {
                            (leaf.clone(), None)
                        }
                    } else {
                        (leaf.clone(), None)
                    };
                    return (Some(ticket), suffix, Some(r.name.clone()), Some("worktree"));
                }
            }
            return (None, None, Some(r.name.clone()), Some("worktree"));
        }
    }
    (None, None, None, None)
}

fn path_starts_with(p: &Path, prefix: &Path) -> bool {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    p.starts_with(&prefix)
}
