//! `trek sync <ticket>` — rebase a ticket's branches onto whatever each one
//! was branched from. The design's stacked pattern is:
//!
//!   baseline ← <ticket>-migration ← <ticket>
//!
//! where the upper branch's `from` is the lower one. `sync` walks each
//! branch in topological order (deps first) and runs `git rebase <from>` in
//! its worktree.
//!
//! Conservative: refuses any worktree that is dirty or mid-op. If a rebase
//! fails, we DO NOT auto-abort — the worktree is left in mid-rebase so the
//! user/agent can resolve. The aggregate exit is 3 (PartialFailure).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::commands::{emit_internal, finalize, require_workspace};
use crate::error::ErrorCode;
use crate::git::exec;
use crate::lock;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::preflight::{self, Opts};
use crate::state::StateRoot;
use crate::ticket::{Branch, Ticket};

#[derive(Serialize)]
struct PerBranch {
    repo: String,
    suffix: Option<String>,
    branch: String,
    from: String,
    /// rebased | already_up_to_date | failed | skipped
    action: &'static str,
    ok: bool,
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct SyncData<'a> {
    ticket: &'a str,
    branches: Vec<PerBranch>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    ticket: &str,
    repos_filter: Option<&[String]>,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "sync") {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket) {
        return emit_err(ctx, "sync", ErrorCode::InvalidTicketId, &e.to_string(), None);
    }
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "sync", &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => return emit_err(ctx, "sync", e.code(), &e.to_string(), None),
    };

    let ticket_rec = match Ticket::load(&state, ticket) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return emit_err(
                ctx,
                "sync",
                ErrorCode::TicketNotFound,
                &format!("ticket `{ticket}` not tracked"),
                Some(json!({ "ticket": ticket })),
            );
        }
        Err(e) => return emit_internal(ctx, "sync", &e.to_string()),
    };

    // Optional repo filter.
    let repos_filter_set: Option<HashSet<&str>> =
        repos_filter.map(|v| v.iter().map(|s| s.as_str()).collect());
    if let Some(names) = repos_filter {
        for n in names {
            if ws.config.repo(n).is_none() {
                return emit_err(
                    ctx,
                    "sync",
                    ErrorCode::RepoNotInWorkspace,
                    &format!("repo `{n}` not in workspace"),
                    Some(json!({ "repo": n })),
                );
            }
        }
    }

    let branches: Vec<&Branch> = ticket_rec
        .branches
        .iter()
        .filter(|b| {
            repos_filter_set
                .as_ref()
                .map(|s| s.contains(b.repo.as_str()))
                .unwrap_or(true)
        })
        .collect();

    // Preflight every worktree we'll touch.
    let opts = Opts {
        require_clean: true,
        forbid_detached: false,
        forbid_mid_op: true,
        forbid_unpushed: false,
    };
    let mut issues = Vec::new();
    for b in &branches {
        // Adapt preflight: it expects a Repo. Build a synthetic one pointing
        // at the worktree path (only its `path` and `name` matter to check).
        let synthetic = crate::config::Repo {
            name: format!("{}@{}", b.repo, b.name),
            path: b.worktree.clone(),
            worktree_dir: b.worktree.clone(),
            baseline: None,
            branch_pattern: None,
        };
        if !b.worktree.is_dir() {
            return emit_err(
                ctx,
                "sync",
                ErrorCode::Internal,
                &format!("worktree {} does not exist", b.worktree.display()),
                Some(json!({ "repo": b.repo, "worktree": b.worktree })),
            );
        }
        issues.extend(preflight::check(&synthetic, &opts));
    }
    if !issues.is_empty() {
        let first = &issues[0];
        return emit_err(
            ctx,
            "sync",
            first.code,
            &first.message,
            Some(serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null)),
        );
    }

    // Topological order per (repo, suffix-chain). For a branch B with from F,
    // if F is another branch in this ticket+repo, we sync F first.
    let ordered = topo_order(&branches);

    let mut results: Vec<PerBranch> = Vec::with_capacity(ordered.len());
    let mut any_failed = false;
    for b in &ordered {
        let action: &'static str = match exec::rebase(&b.worktree, &b.from) {
            Ok(out) => {
                if out.stdout.contains("up to date") || out.stdout.contains("up-to-date") {
                    "already_up_to_date"
                } else {
                    "rebased"
                }
            }
            Err(e) => {
                any_failed = true;
                results.push(PerBranch {
                    repo: b.repo.clone(),
                    suffix: b.suffix.clone(),
                    branch: b.name.clone(),
                    from: b.from.clone(),
                    action: "failed",
                    ok: false,
                    error: Some(e.to_string()),
                    error_code: Some(e.code()),
                });
                continue;
            }
        };
        results.push(PerBranch {
            repo: b.repo.clone(),
            suffix: b.suffix.clone(),
            branch: b.name.clone(),
            from: b.from.clone(),
            action,
            ok: true,
            error: None,
            error_code: None,
        });
    }

    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    finalize(&ws, &state, "sync", argv, exit, Some(ticket), None);

    let body = SyncData {
        ticket,
        branches: results,
    };
    if any_failed {
        if ctx.json {
            return emit_err(
                ctx,
                "sync",
                ErrorCode::PartialFailure,
                "one or more rebases failed (worktree left mid-rebase)",
                Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
            );
        }
        for r in &body.branches {
            human_line(r);
        }
        return ExitCode::from(exit);
    }
    let lines: Vec<(String, &'static str, String, String)> = body
        .branches
        .iter()
        .map(|r| (r.repo.clone(), r.action, r.branch.clone(), r.from.clone()))
        .collect();
    emit_ok(ctx, "sync", body, || {
        for (repo, action, branch, from) in &lines {
            eprintln!("[{repo}] {action} ({branch} onto {from})");
        }
    })
}

fn topo_order<'a>(branches: &[&'a Branch]) -> Vec<&'a Branch> {
    // Build name -> branch map keyed by (repo, branch-name). Sync only orders
    // within a single repo (cross-repo branches don't share a rebase
    // relationship).
    let by_name: HashMap<(String, String), &Branch> = branches
        .iter()
        .map(|b| ((b.repo.clone(), b.name.clone()), *b))
        .collect();

    let mut visited: HashSet<(String, String)> = HashSet::new();
    let mut order: Vec<&Branch> = Vec::new();

    fn visit<'a>(
        node: &'a Branch,
        by_name: &HashMap<(String, String), &'a Branch>,
        visited: &mut HashSet<(String, String)>,
        order: &mut Vec<&'a Branch>,
    ) {
        let key = (node.repo.clone(), node.name.clone());
        if !visited.insert(key) {
            return;
        }
        if let Some(dep) = by_name.get(&(node.repo.clone(), node.from.clone())) {
            visit(dep, by_name, visited, order);
        }
        order.push(node);
    }
    for b in branches {
        visit(b, &by_name, &mut visited, &mut order);
    }
    order
}

fn human_line(r: &PerBranch) {
    if r.ok {
        eprintln!("[{}] {} ({} onto {})", r.repo, r.action, r.branch, r.from);
    } else {
        eprintln!("[{}] failed: {}", r.repo, r.error.as_deref().unwrap_or("unknown"));
    }
}
