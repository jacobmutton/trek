use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;

use crate::commands::require_workspace;
use crate::config::Config;
use crate::output::{OutputCtx, emit_ok};
use crate::state::StateRoot;
use crate::ticket;

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
    // Best-effort registry: if we can't load state (no XDG dir yet, etc.),
    // we still classify via path heuristics.
    let known_tickets: Vec<(String, Vec<(String, Option<String>)>)> =
        match StateRoot::for_workspace(ws.config.workspace.id) {
            Ok(state) => ticket::list_all(&state)
                .unwrap_or_default()
                .into_iter()
                .map(|t| {
                    let pairs = t
                        .branches
                        .iter()
                        .map(|b| (b.repo.clone(), b.suffix.clone()))
                        .collect();
                    (t.id, pairs)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
    let (ticket, suffix, repo, location) = classify(&ws.config, &cwd, &known_tickets);
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
/// preprod|worktree) `cwd` falls inside. Consults the ticket registry to
/// disambiguate `<ticket><join><suffix>` vs `<ticket>` when the join string
/// also appears inside ticket ids (e.g. "FUN-1234").
fn classify(
    cfg: &Config,
    cwd: &Path,
    known: &[(String, Vec<(String, Option<String>)>)],
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
                    let (ticket, suffix) = split_leaf(&leaf, &cfg.branch.suffix_join, &r.name, known);
                    return (Some(ticket), suffix, Some(r.name.clone()), Some("worktree"));
                }
            }
            return (None, None, Some(r.name.clone()), Some("worktree"));
        }
    }
    (None, None, None, None)
}

/// Match `leaf` against the known (ticket, suffix) pairs for this repo. Tries
/// the longest matching ticket prefix first so `FUN-1234-migration` beats
/// `FUN-1234` when both exist.
fn split_leaf(
    leaf: &str,
    join: &str,
    repo: &str,
    known: &[(String, Vec<(String, Option<String>)>)],
) -> (String, Option<String>) {
    let mut candidates: Vec<(&str, Option<&str>)> = Vec::new();
    for (id, pairs) in known {
        for (r, suf) in pairs {
            if r != repo {
                continue;
            }
            let expected_leaf = match suf {
                Some(s) => format!("{id}{join}{s}"),
                None => id.clone(),
            };
            if expected_leaf == leaf {
                candidates.push((id.as_str(), suf.as_deref()));
            }
        }
    }
    // Prefer the most specific match (longer ticket id, then with suffix).
    candidates.sort_by_key(|(id, suf)| (std::cmp::Reverse(id.len()), suf.is_none()));
    if let Some((id, suf)) = candidates.first() {
        return ((*id).to_string(), suf.map(|s| s.to_string()));
    }
    // Fall back to the joiner heuristic if we have no registry hit.
    if let Some(idx) = leaf.rfind(join) {
        if leaf.matches(join).count() >= 2 {
            let (id, suffix) = leaf.split_at(idx);
            return (id.to_string(), Some(suffix[join.len()..].to_string()));
        }
    }
    (leaf.to_string(), None)
}

fn path_starts_with(p: &Path, prefix: &Path) -> bool {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    p.starts_with(&prefix)
}
