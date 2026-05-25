//! `trek cleanup <ticket>`: undo a ticket's worktrees and branches, and if
//! the ticket is currently staged, restore preprod to baseline along the way.
//!
//! Idempotent: re-running after partial failure tries the same repos again
//! and skips ones already cleaned (absent worktree + absent branch).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::baseline;
use crate::commands::{emit_internal, finalize, require_workspace};
use crate::error::ErrorCode;
use crate::git::{exec, read};
use crate::lock;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::preflight::{self, Opts};
use crate::staged::Staged;
use crate::state::StateRoot;
use crate::ticket::Ticket;

#[derive(Serialize)]
struct PerBranch {
    repo: String,
    suffix: Option<String>,
    branch: String,
    /// removed | absent | failed
    worktree_action: &'static str,
    /// deleted | absent | kept_merged | failed | skipped
    branch_action: &'static str,
    ok: bool,
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct PreprodEntry {
    repo: String,
    /// restored | already_on | failed
    action: &'static str,
    ok: bool,
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct CleanupData<'a> {
    ticket: &'a str,
    /// `true` if the ticket was staged at the start and we restored preprod.
    was_staged: bool,
    preprod: Vec<PreprodEntry>,
    branches: Vec<PerBranch>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    ticket_id: &str,
    keep_merged: bool,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "cleanup") {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket_id) {
        return emit_err(
            ctx,
            "cleanup",
            ErrorCode::InvalidTicketId,
            &e.to_string(),
            None,
        );
    }
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "cleanup", &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => return emit_err(ctx, "cleanup", e.code(), &e.to_string(), None),
    };

    let mut ticket_rec = match Ticket::load(&state, ticket_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return emit_err(
                ctx,
                "cleanup",
                ErrorCode::TicketNotFound,
                &format!("ticket `{ticket_id}` not tracked"),
                Some(json!({ "ticket": ticket_id })),
            );
        }
        Err(e) => return emit_internal(ctx, "cleanup", &e.to_string()),
    };

    let staged = match Staged::load(&state) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "cleanup", &e.to_string()),
    };
    let was_staged = staged.as_ref().map(|s| s.ticket == ticket_id).unwrap_or(false);

    let mut any_failed = false;
    let mut preprod_results: Vec<PreprodEntry> = Vec::new();

    if was_staged {
        let staged = staged.as_ref().unwrap();
        // Preflight: the preprod checkouts we'll touch must be clean and not
        // mid-op. We do *not* forbid detached HEAD — that's the state stage
        // leaves them in.
        let opts = Opts {
            require_clean: true,
            forbid_detached: false,
            forbid_mid_op: true,
            forbid_unpushed: false,
        };
        let mut issues = Vec::new();
        for snap in &staged.snapshots {
            let Some(r) = ws.config.repo(&snap.repo) else {
                continue;
            };
            issues.extend(preflight::check(r, &opts));
        }
        if !issues.is_empty() {
            let first = &issues[0];
            return emit_err(
                ctx,
                "cleanup",
                first.code,
                &first.message,
                Some(serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null)),
            );
        }

        for snap in &staged.snapshots {
            let Some(repo) = ws.config.repo(&snap.repo) else {
                continue;
            };
            let baseline_name = match baseline::resolve(repo) {
                Ok(b) => b,
                Err(e) => {
                    any_failed = true;
                    preprod_results.push(PreprodEntry {
                        repo: snap.repo.clone(),
                        action: "failed",
                        ok: false,
                        error: Some(e.to_string()),
                        error_code: Some(ErrorCode::BranchNotFound),
                    });
                    continue;
                }
            };
            let r = read::open(&repo.path).ok();
            let cur = r
                .as_ref()
                .and_then(|r| read::current_branch(r).ok().flatten());
            if cur.as_deref() == Some(baseline_name.as_str()) {
                preprod_results.push(PreprodEntry {
                    repo: snap.repo.clone(),
                    action: "already_on",
                    ok: true,
                    error: None,
                    error_code: None,
                });
                continue;
            }
            match exec::checkout(&repo.path, &baseline_name) {
                Ok(_) => preprod_results.push(PreprodEntry {
                    repo: snap.repo.clone(),
                    action: "restored",
                    ok: true,
                    error: None,
                    error_code: None,
                }),
                Err(e) => {
                    any_failed = true;
                    preprod_results.push(PreprodEntry {
                        repo: snap.repo.clone(),
                        action: "failed",
                        ok: false,
                        error: Some(e.to_string()),
                        error_code: Some(e.code()),
                    });
                }
            }
        }
    }

    // Per-branch worktree + branch cleanup.
    let mut branch_results: Vec<PerBranch> = Vec::with_capacity(ticket_rec.branches.len());
    let mut cleaned: HashSet<(String, Option<String>)> = HashSet::new();
    let original_branches = ticket_rec.branches.clone();

    // Cache baseline per repo (only needed when --keep-merged).
    let mut baseline_cache: HashMap<String, String> = HashMap::new();

    for b in &original_branches {
        let Some(repo) = ws.config.repo(&b.repo) else {
            any_failed = true;
            branch_results.push(PerBranch {
                repo: b.repo.clone(),
                suffix: b.suffix.clone(),
                branch: b.name.clone(),
                worktree_action: "skipped",
                branch_action: "skipped",
                ok: false,
                error: Some(format!("repo `{}` no longer in workspace", b.repo)),
                error_code: Some(ErrorCode::RepoNotInWorkspace),
            });
            continue;
        };

        // Worktree.
        let worktree_action: &'static str = if b.worktree.exists() {
            match exec::worktree_remove(&repo.path, &b.worktree, false) {
                Ok(_) => "removed",
                Err(e) => {
                    any_failed = true;
                    branch_results.push(PerBranch {
                        repo: b.repo.clone(),
                        suffix: b.suffix.clone(),
                        branch: b.name.clone(),
                        worktree_action: "failed",
                        branch_action: "skipped",
                        ok: false,
                        error: Some(format!("worktree remove: {e}")),
                        error_code: Some(e.code()),
                    });
                    continue;
                }
            }
        } else {
            "absent"
        };
        // Best-effort prune so the .git/worktrees admin dir doesn't accumulate
        // dangling entries.
        let _ = exec::worktree_prune(&repo.path);

        // Branch.
        let r = match read::open(&repo.path) {
            Ok(r) => r,
            Err(e) => {
                any_failed = true;
                branch_results.push(PerBranch {
                    repo: b.repo.clone(),
                    suffix: b.suffix.clone(),
                    branch: b.name.clone(),
                    worktree_action,
                    branch_action: "failed",
                    ok: false,
                    error: Some(format!("open repo: {e}")),
                    error_code: Some(ErrorCode::Internal),
                });
                continue;
            }
        };
        let branch_present = read::branch_exists(&r, &b.name).unwrap_or(false);

        let branch_action: &'static str = if !branch_present {
            "absent"
        } else if keep_merged {
            let baseline_name = match baseline_cache.get(&b.repo) {
                Some(s) => s.clone(),
                None => match baseline::resolve(repo) {
                    Ok(s) => {
                        baseline_cache.insert(b.repo.clone(), s.clone());
                        s
                    }
                    Err(e) => {
                        any_failed = true;
                        branch_results.push(PerBranch {
                            repo: b.repo.clone(),
                            suffix: b.suffix.clone(),
                            branch: b.name.clone(),
                            worktree_action,
                            branch_action: "failed",
                            ok: false,
                            error: Some(format!("resolve baseline: {e}")),
                            error_code: Some(ErrorCode::BranchNotFound),
                        });
                        continue;
                    }
                },
            };
            let merged = read::is_merged_into(&r, &b.name, &baseline_name).unwrap_or(false);
            if merged {
                "kept_merged"
            } else {
                match exec::delete_branch(&repo.path, &b.name, true) {
                    Ok(_) => "deleted",
                    Err(e) => {
                        any_failed = true;
                        branch_results.push(PerBranch {
                            repo: b.repo.clone(),
                            suffix: b.suffix.clone(),
                            branch: b.name.clone(),
                            worktree_action,
                            branch_action: "failed",
                            ok: false,
                            error: Some(format!("branch delete: {e}")),
                            error_code: Some(e.code()),
                        });
                        continue;
                    }
                }
            }
        } else {
            match exec::delete_branch(&repo.path, &b.name, true) {
                Ok(_) => "deleted",
                Err(e) => {
                    any_failed = true;
                    branch_results.push(PerBranch {
                        repo: b.repo.clone(),
                        suffix: b.suffix.clone(),
                        branch: b.name.clone(),
                        worktree_action,
                        branch_action: "failed",
                        ok: false,
                        error: Some(format!("branch delete: {e}")),
                        error_code: Some(e.code()),
                    });
                    continue;
                }
            }
        };

        cleaned.insert((b.repo.clone(), b.suffix.clone()));
        branch_results.push(PerBranch {
            repo: b.repo.clone(),
            suffix: b.suffix.clone(),
            branch: b.name.clone(),
            worktree_action,
            branch_action,
            ok: true,
            error: None,
            error_code: None,
        });
    }

    // Update or delete the ticket record. Successful entries are removed; any
    // failed ones stay so re-running `cleanup` retries just those.
    ticket_rec
        .branches
        .retain(|b| !cleaned.contains(&(b.repo.clone(), b.suffix.clone())));
    if ticket_rec.branches.is_empty() {
        if let Err(e) = Ticket::delete(&state, ticket_id) {
            return emit_internal(ctx, "cleanup", &format!("delete ticket: {e}"));
        }
    } else if let Err(e) = ticket_rec.save(&state) {
        return emit_internal(ctx, "cleanup", &format!("save ticket: {e}"));
    }

    // If we successfully unstaged every preprod, clear the staged record so
    // a future stage can proceed without ALREADY_STAGED.
    if was_staged && !preprod_results.iter().any(|p| !p.ok) {
        if let Err(e) = Staged::clear(&state) {
            return emit_internal(ctx, "cleanup", &format!("clear staged: {e}"));
        }
    }

    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    finalize(&ws, &state, "cleanup", argv, exit, Some(ticket_id), None);

    let body = CleanupData {
        ticket: ticket_id,
        was_staged,
        preprod: preprod_results,
        branches: branch_results,
    };

    if any_failed && ctx.json {
        return emit_err(
            ctx,
            "cleanup",
            ErrorCode::PartialFailure,
            "one or more repos failed during cleanup",
            Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
        );
    }
    if any_failed {
        if was_staged {
            for p in &body.preprod {
                if p.ok {
                    eprintln!("[{}] preprod={}", p.repo, p.action);
                } else {
                    eprintln!(
                        "[{}] preprod failed: {}",
                        p.repo,
                        p.error.as_deref().unwrap_or("unknown")
                    );
                }
            }
        }
        for b in &body.branches {
            if b.ok {
                eprintln!(
                    "[{}] worktree={} branch={}",
                    b.repo, b.worktree_action, b.branch_action
                );
            } else {
                eprintln!(
                    "[{}] failed: {}",
                    b.repo,
                    b.error.as_deref().unwrap_or("unknown")
                );
            }
        }
        return ExitCode::from(exit);
    }

    let preprod_lines: Vec<_> = body
        .preprod
        .iter()
        .map(|p| (p.repo.clone(), p.action))
        .collect();
    let branch_lines: Vec<_> = body
        .branches
        .iter()
        .map(|b| (b.repo.clone(), b.worktree_action, b.branch_action))
        .collect();
    let was_staged_for_human = was_staged;
    emit_ok(ctx, "cleanup", body, || {
        if was_staged_for_human {
            for (repo, action) in &preprod_lines {
                eprintln!("[{repo}] preprod={action}");
            }
        }
        for (repo, wt, br) in &branch_lines {
            eprintln!("[{repo}] worktree={wt} branch={br}");
        }
    })
}
