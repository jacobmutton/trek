//! `trek push <ticket>` — push every branch of a ticket to its remote, one
//! repo at a time. Idempotent (already-up-to-date is success). Refuses if
//! any selected worktree is mid-op.
//!
//! Per design's v2 slice: no force-with-lease for now; if you need it, push
//! manually. We do support --set-upstream so newly-created branches gain a
//! tracking ref on first push.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::commands::{emit_internal, finalize, require_workspace};
use crate::error::ErrorCode;
use crate::git::exec::{self, GitError, GitOutcome};
use crate::lock;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::state::StateRoot;
use crate::ticket::Ticket;

#[derive(Serialize)]
struct PerBranch {
    repo: String,
    suffix: Option<String>,
    branch: String,
    /// pushed | up_to_date | failed
    action: &'static str,
    ok: bool,
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct PushData<'a> {
    ticket: &'a str,
    suffix: Option<&'a str>,
    remote: String,
    branches: Vec<PerBranch>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    ticket: &str,
    suffix: Option<&str>,
    repos_filter: Option<&[String]>,
    remote: &str,
    set_upstream: bool,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "push") {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket) {
        return emit_err(ctx, "push", ErrorCode::InvalidTicketId, &e.to_string(), None);
    }
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "push", &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => return emit_err(ctx, "push", e.code(), &e.to_string(), None),
    };

    let ticket_rec = match Ticket::load(&state, ticket) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return emit_err(
                ctx,
                "push",
                ErrorCode::TicketNotFound,
                &format!("ticket `{ticket}` not tracked"),
                Some(json!({ "ticket": ticket })),
            );
        }
        Err(e) => return emit_internal(ctx, "push", &e.to_string()),
    };

    // Filter to the requested suffix and repos.
    let filter_repos: Option<std::collections::HashSet<&str>> = repos_filter
        .map(|names| names.iter().map(|s| s.as_str()).collect());
    if let Some(names) = repos_filter {
        for n in names {
            if ws.config.repo(n).is_none() {
                return emit_err(
                    ctx,
                    "push",
                    ErrorCode::RepoNotInWorkspace,
                    &format!("repo `{n}` not in workspace"),
                    Some(json!({ "repo": n })),
                );
            }
        }
    }
    let selected: Vec<_> = ticket_rec
        .branches
        .iter()
        .filter(|b| b.suffix.as_deref() == suffix)
        .filter(|b| {
            filter_repos
                .as_ref()
                .map(|s| s.contains(b.repo.as_str()))
                .unwrap_or(true)
        })
        .collect();
    if selected.is_empty() {
        let s_human = suffix.map(|s| format!(" with suffix `{s}`")).unwrap_or_default();
        return emit_err(
            ctx,
            "push",
            ErrorCode::SuffixNotFound,
            &format!("ticket `{ticket}` has no matching branches{s_human}"),
            Some(json!({ "ticket": ticket, "suffix": suffix })),
        );
    }

    let mut results: Vec<PerBranch> = Vec::with_capacity(selected.len());
    let mut any_failed = false;
    for b in &selected {
        let outcome = do_push(&b.worktree, remote, &b.name, set_upstream);
        match outcome {
            Ok((action, _out)) => results.push(PerBranch {
                repo: b.repo.clone(),
                suffix: b.suffix.clone(),
                branch: b.name.clone(),
                action,
                ok: true,
                error: None,
                error_code: None,
            }),
            Err(e) => {
                any_failed = true;
                results.push(PerBranch {
                    repo: b.repo.clone(),
                    suffix: b.suffix.clone(),
                    branch: b.name.clone(),
                    action: "failed",
                    ok: false,
                    error: Some(e.to_string()),
                    error_code: Some(e.code()),
                });
            }
        }
    }

    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    finalize(&ws, &state, "push", argv, exit, Some(ticket), suffix);

    let body = PushData {
        ticket,
        suffix,
        remote: remote.to_string(),
        branches: results,
    };
    if any_failed {
        if ctx.json {
            return emit_err(
                ctx,
                "push",
                ErrorCode::PartialFailure,
                "one or more branches failed to push",
                Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
            );
        }
        for r in &body.branches {
            human_line(r);
        }
        return ExitCode::from(exit);
    }
    let lines: Vec<(String, &'static str, String)> = body
        .branches
        .iter()
        .map(|r| (r.repo.clone(), r.action, r.branch.clone()))
        .collect();
    emit_ok(ctx, "push", body, || {
        for (repo, action, branch) in &lines {
            eprintln!("[{repo}] {action} ({branch})");
        }
    })
}

fn do_push(
    worktree: &Path,
    remote: &str,
    branch: &str,
    set_upstream: bool,
) -> Result<(&'static str, GitOutcome), GitError> {
    let out = exec::push(worktree, remote, branch, set_upstream)?;
    // Git prints "Everything up-to-date" on no-op. We don't get that from
    // stdout (it goes to stderr); detect from stderr text.
    let action = if out.stderr.contains("Everything up-to-date")
        || out.stderr.contains("up to date")
    {
        "up_to_date"
    } else {
        "pushed"
    };
    Ok((action, out))
}

fn human_line(r: &PerBranch) {
    if r.ok {
        eprintln!("[{}] {} ({})", r.repo, r.action, r.branch);
    } else {
        eprintln!("[{}] failed: {}", r.repo, r.error.as_deref().unwrap_or("unknown"));
    }
}
