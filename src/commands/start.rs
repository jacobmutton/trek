//! `trek start` and `trek adopt`. `start` creates a new branch off `--from`;
//! `adopt` reuses an existing branch. Both are otherwise identical: produce a
//! worktree per repo and write the ticket record.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::baseline;
use crate::commands::{emit_internal, finalize, require_workspace};
use crate::config::Repo;
use crate::error::ErrorCode;
use crate::git::{exec, read};
use crate::lock;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::state::StateRoot;
use crate::ticket::{Branch, Ticket};

pub enum Mode {
    Start,
    Adopt,
}

#[derive(Serialize)]
struct PerRepo {
    repo: String,
    ok: bool,
    branch: String,
    worktree: std::path::PathBuf,
    action: &'static str, // created | adopted | already_present | added_worktree | failed
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct StartData<'a> {
    ticket: &'a str,
    suffix: Option<&'a str>,
    repos: Vec<PerRepo>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    mode: Mode,
    ticket: &str,
    repos: &[String],
    suffix: Option<&str>,
    from: Option<&str>,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let cmd = match mode {
        Mode::Start => "start",
        Mode::Adopt => "adopt",
    };
    let ws = match require_workspace(workspace, ctx, cmd) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket) {
        return emit_err(ctx, cmd, ErrorCode::InvalidTicketId, &e.to_string(), None);
    }
    // Look up every named repo up-front so we fail before touching anything.
    let mut resolved: Vec<&Repo> = Vec::with_capacity(repos.len());
    for name in repos {
        match ws.config.repo(name) {
            Some(r) => resolved.push(r),
            None => {
                return emit_err(
                    ctx,
                    cmd,
                    ErrorCode::RepoNotInWorkspace,
                    &format!("repo `{name}` not in workspace"),
                    Some(json!({ "repo": name })),
                );
            }
        }
    }

    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, cmd, &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => {
            return emit_err(ctx, cmd, e.code(), &e.to_string(), None);
        }
    };

    let mut ticket_record = match Ticket::load(&state, ticket) {
        Ok(Some(t)) => t,
        Ok(None) => Ticket::new(ticket.to_string()),
        Err(e) => return emit_internal(ctx, cmd, &e.to_string()),
    };

    let mut results: Vec<PerRepo> = Vec::with_capacity(resolved.len());
    let mut any_failed = false;

    for repo in &resolved {
        let names = match naming::resolve(&ws.config, repo, ticket, suffix) {
            Ok(n) => n,
            Err(e) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    branch: String::new(),
                    worktree: std::path::PathBuf::new(),
                    action: "failed",
                    error: Some(e.to_string()),
                    error_code: Some(ErrorCode::Internal),
                });
                continue;
            }
        };
        let baseline_name = match baseline::resolve(repo) {
            Ok(b) => b,
            Err(e) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    branch: names.branch.clone(),
                    worktree: names.worktree.clone(),
                    action: "failed",
                    error: Some(e.to_string()),
                    error_code: Some(ErrorCode::BranchNotFound),
                });
                continue;
            }
        };
        let from_ref =
            match naming::resolve_from(&ws.config, repo, ticket, from, &baseline_name) {
                Ok(f) => f,
                Err(e) => {
                    any_failed = true;
                    results.push(PerRepo {
                        repo: repo.name.clone(),
                        ok: false,
                        branch: names.branch.clone(),
                        worktree: names.worktree.clone(),
                        action: "failed",
                        error: Some(e.to_string()),
                        error_code: Some(ErrorCode::Internal),
                    });
                    continue;
                }
            };

        let outcome = do_one_repo(repo, &names.branch, &names.worktree, &from_ref, &mode);
        match outcome {
            Ok(action) => {
                ticket_record.upsert(Branch {
                    repo: repo.name.clone(),
                    suffix: suffix.map(String::from),
                    name: names.branch.clone(),
                    from: from_ref.clone(),
                    worktree: names.worktree.clone(),
                });
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: true,
                    branch: names.branch.clone(),
                    worktree: names.worktree.clone(),
                    action,
                    error: None,
                    error_code: None,
                });
            }
            Err((code, msg)) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    branch: names.branch.clone(),
                    worktree: names.worktree.clone(),
                    action: "failed",
                    error: Some(msg),
                    error_code: Some(code),
                });
            }
        }
    }

    // Best-effort save of whatever did succeed, so a partial run is recoverable.
    if let Err(e) = ticket_record.save(&state) {
        return emit_internal(ctx, cmd, &format!("save ticket record: {e}"));
    }

    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    finalize(&ws, &state, cmd, argv, exit, Some(ticket), suffix);

    if any_failed {
        // Emit one structured error with per-repo details, but exit 3.
        if ctx.json {
            let body = StartData {
                ticket,
                suffix,
                repos: results,
            };
            return emit_err(
                ctx,
                cmd,
                ErrorCode::PartialFailure,
                "one or more repos failed",
                Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
            );
        } else {
            for r in &results {
                if r.ok {
                    eprintln!("[{}] {} ({})", r.repo, r.action, r.branch);
                } else {
                    eprintln!(
                        "[{}] failed: {}",
                        r.repo,
                        r.error.as_deref().unwrap_or("unknown")
                    );
                }
            }
            return std::process::ExitCode::from(exit);
        }
    }

    let lines: Vec<(String, &'static str, String)> = results
        .iter()
        .map(|r| (r.repo.clone(), r.action, r.branch.clone()))
        .collect();
    emit_ok(
        ctx,
        cmd,
        StartData {
            ticket,
            suffix,
            repos: results,
        },
        || {
            for (repo, action, branch) in &lines {
                eprintln!("[{repo}] {action} ({branch})");
            }
        },
    )
}

/// Idempotent per-repo step. Returns the action label on success.
fn do_one_repo(
    repo: &Repo,
    branch: &str,
    worktree: &Path,
    from: &str,
    mode: &Mode,
) -> Result<&'static str, (ErrorCode, String)> {
    let r = read::open(&repo.path)
        .map_err(|e| (ErrorCode::Internal, format!("open repo: {e}")))?;
    let branch_present = read::branch_exists(&r, branch)
        .map_err(|e| (ErrorCode::Internal, format!("branch exists check: {e}")))?;
    let worktree_present = worktree.exists();

    match (mode, branch_present, worktree_present) {
        // already done
        (_, true, true) => Ok("already_present"),

        // start: branch already exists → BRANCH_EXISTS (use adopt instead).
        (Mode::Start, true, false) => {
            // Re-create the worktree off the existing branch (idempotent
            // recovery), as the design specifies for `start`.
            exec::worktree_add(&repo.path, worktree, branch)
                .map(|_| "added_worktree")
                .map_err(|e| (e.code(), e.to_string()))
        }

        (Mode::Start, false, _) => {
            exec::create_branch(&repo.path, branch, from)
                .map_err(|e| (e.code(), e.to_string()))?;
            exec::worktree_add(&repo.path, worktree, branch)
                .map(|_| "created")
                .map_err(|e| (e.code(), e.to_string()))
        }

        (Mode::Adopt, true, false) => exec::worktree_add(&repo.path, worktree, branch)
            .map(|_| "adopted")
            .map_err(|e| (e.code(), e.to_string())),

        (Mode::Adopt, false, _) => Err((
            ErrorCode::BranchNotFound,
            format!("branch `{branch}` does not exist in repo `{}`", repo.name),
        )),
    }
}
