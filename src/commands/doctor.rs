//! `trek doctor` — diagnostic report of the workspace. Reports per-repo:
//!  - path exists
//!  - is a git repo
//!  - baseline resolves
//!  - worktree_dir exists (creating it is not its responsibility)
//!
//! Also reports state-dir, lock-file, and any stale ticket records that
//! reference repos no longer in trek.toml. Always exits 0; the `data.ok`
//! field in the JSON envelope is the machine-readable verdict.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use crate::baseline;
use crate::commands::{emit_internal, require_workspace};
use crate::git::read;
use crate::output::{OutputCtx, emit_ok};
use crate::state::StateRoot;
use crate::ticket;

#[derive(Serialize)]
struct RepoCheck {
    repo: String,
    path: std::path::PathBuf,
    path_exists: bool,
    is_git_repo: bool,
    baseline: Option<String>,
    baseline_error: Option<String>,
    worktree_dir_exists: bool,
    ok: bool,
}

#[derive(Serialize)]
struct StaleEntry {
    ticket: String,
    repo: String,
}

#[derive(Serialize)]
struct DoctorData {
    workspace: std::path::PathBuf,
    workspace_id: uuid::Uuid,
    state_dir: std::path::PathBuf,
    repos: Vec<RepoCheck>,
    stale_ticket_repos: Vec<StaleEntry>,
    /// Whether a staged record exists. Not a problem on its own; just
    /// reported so the agent knows.
    staged: Option<String>,
    ok: bool,
}

pub fn run(ctx: OutputCtx, workspace: Option<&Path>) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "doctor") {
        Ok(w) => w,
        Err(code) => return code,
    };
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "doctor", &e.to_string()),
    };

    let mut repo_results: Vec<RepoCheck> = Vec::with_capacity(ws.config.repos.len());
    for repo in &ws.config.repos {
        let path_exists = repo.path.is_dir();
        let is_git_repo = path_exists && read::open(&repo.path).is_ok();
        let (baseline, baseline_error) = if is_git_repo {
            match baseline::resolve(repo) {
                Ok(b) => (Some(b), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        let worktree_dir_exists = repo.worktree_dir.is_dir();
        let ok = path_exists && is_git_repo && baseline.is_some();
        repo_results.push(RepoCheck {
            repo: repo.name.clone(),
            path: repo.path.clone(),
            path_exists,
            is_git_repo,
            baseline,
            baseline_error,
            worktree_dir_exists,
            ok,
        });
    }

    // Stale ticket entries: tickets referencing repos not in current config.
    let tickets = ticket::list_all(&state).unwrap_or_default();
    let known: std::collections::HashSet<&str> =
        ws.config.repos.iter().map(|r| r.name.as_str()).collect();
    let mut stale: Vec<StaleEntry> = Vec::new();
    for t in &tickets {
        for b in &t.branches {
            if !known.contains(b.repo.as_str()) {
                stale.push(StaleEntry {
                    ticket: t.id.clone(),
                    repo: b.repo.clone(),
                });
            }
        }
    }

    let staged_id = crate::staged::Staged::load(&state)
        .ok()
        .flatten()
        .map(|s| s.ticket);

    let overall_ok = repo_results.iter().all(|r| r.ok) && stale.is_empty();

    let body = DoctorData {
        workspace: ws.dir.clone(),
        workspace_id: ws.config.workspace.id,
        state_dir: state.root.clone(),
        repos: repo_results,
        stale_ticket_repos: stale,
        staged: staged_id,
        ok: overall_ok,
    };

    // Human report (also captured for emit_ok closure).
    let repo_lines: Vec<(String, bool, bool, Option<String>, Option<String>)> = body
        .repos
        .iter()
        .map(|r| {
            (
                r.repo.clone(),
                r.path_exists,
                r.is_git_repo,
                r.baseline.clone(),
                r.baseline_error.clone(),
            )
        })
        .collect();
    let stale_lines: Vec<(String, String)> = body
        .stale_ticket_repos
        .iter()
        .map(|s| (s.ticket.clone(), s.repo.clone()))
        .collect();
    let staged_for_human = body.staged.clone();
    let ws_dir_for_human = ws.dir.clone();

    let render_human = move || {
        eprintln!("workspace: {}", ws_dir_for_human.display());
        for (repo, path_ok, git_ok, baseline, baseline_err) in &repo_lines {
            let status = if *path_ok && *git_ok && baseline.is_some() {
                "OK"
            } else {
                "FAIL"
            };
            let bline = baseline.as_deref().unwrap_or("?");
            let extra = if !*path_ok {
                "  (path missing)"
            } else if !*git_ok {
                "  (not a git repo)"
            } else if let Some(e) = baseline_err {
                Box::leak(format!("  (baseline: {e})").into_boxed_str())
            } else {
                ""
            };
            eprintln!("  [{repo}] {status}  baseline={bline}{extra}");
        }
        if !stale_lines.is_empty() {
            eprintln!("stale ticket entries (repo gone from trek.toml):");
            for (t, r) in &stale_lines {
                eprintln!("  {t} -> {r}");
            }
        }
        if let Some(s) = &staged_for_human {
            eprintln!("staged: {s}");
        }
    };

    emit_ok(ctx, "doctor", body, render_human)
}
