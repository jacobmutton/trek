use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::audit;
use crate::baseline;
use crate::commands::{emit_internal, require_workspace};
use crate::config::{OrphanRepos, Repo};
use crate::error::ErrorCode;
use crate::git::{exec, read};
use crate::lock;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::preflight::{self, Opts};
use crate::staged::{Snapshot, Staged};
use crate::state::StateRoot;
use crate::ticket;

#[derive(Serialize)]
struct PerRepo {
    repo: String,
    ok: bool,
    target_branch: Option<String>,
    action: &'static str, // switched | already_on | baseline | left | failed
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct StageData<'a> {
    ticket: &'a str,
    suffix: Option<&'a str>,
    repos: Vec<PerRepo>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    ticket_id: &str,
    suffix: Option<&str>,
    keep_others: bool,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "stage") {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket_id) {
        return emit_err(ctx, "stage", ErrorCode::InvalidTicketId, &e.to_string(), None);
    }
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "stage", &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => return emit_err(ctx, "stage", e.code(), &e.to_string(), None),
    };

    // ALREADY_STAGED unless it's the same ticket — that's a no-op.
    if let Ok(Some(prev)) = Staged::load(&state) {
        if prev.ticket != ticket_id {
            return emit_err(
                ctx,
                "stage",
                ErrorCode::AlreadyStaged,
                &format!("ticket `{}` is currently staged", prev.ticket),
                Some(json!({ "ticket": prev.ticket })),
            );
        }
        // Same ticket — idempotent no-op success.
        let body = StageData {
            ticket: ticket_id,
            suffix,
            repos: vec![],
        };
        audit::record(&state.audit_file(), "stage", argv, 0);
        return emit_ok(ctx, "stage", body, || {
            eprintln!("stage: already staged for {ticket_id}");
        });
    }

    let ticket_rec = match ticket::Ticket::load(&state, ticket_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return emit_err(
                ctx,
                "stage",
                ErrorCode::TicketNotFound,
                &format!("ticket `{ticket_id}` not tracked; run `trek start` first"),
                Some(json!({ "ticket": ticket_id })),
            );
        }
        Err(e) => return emit_internal(ctx, "stage", &e.to_string()),
    };

    // Resolve per-repo target branch up-front. Missing branch → BRANCH_NOT_FOUND.
    struct Plan<'a> {
        repo: &'a Repo,
        target: Option<String>, // None → orphan (treated per orphan_repos)
    }
    let mut plans: Vec<Plan> = Vec::new();
    for repo in &ws.config.repos {
        let target = ticket_rec.find(&repo.name, suffix).map(|b| b.name.clone());
        plans.push(Plan { repo, target });
    }

    // Preflight every repo we'll touch (declared OR orphan-baseline). Skip
    // orphan repos when keep_others or orphan_repos=leave. We require strict
    // start state: any prior trek stage clears via stage/cleanup, so detached
    // HEAD here means the user touched preprod manually.
    let touch_orphan = !keep_others && ws.config.stage.orphan_repos != OrphanRepos::Leave;
    let opts = Opts::strict();
    let mut issues = Vec::new();
    for p in &plans {
        if p.target.is_none() && !touch_orphan {
            continue;
        }
        issues.extend(preflight::check(p.repo, &opts));
    }
    if !issues.is_empty() {
        // Map the *first* issue's code as the top-level code; full list in
        // details.
        let first = &issues[0];
        return emit_err(
            ctx,
            "stage",
            first.code,
            &first.message,
            Some(serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null)),
        );
    }

    // If orphan_repos = fail and there's any orphan, error out.
    if !keep_others && ws.config.stage.orphan_repos == OrphanRepos::Fail {
        let orphans: Vec<&str> = plans
            .iter()
            .filter(|p| p.target.is_none())
            .map(|p| p.repo.name.as_str())
            .collect();
        if !orphans.is_empty() {
            return emit_err(
                ctx,
                "stage",
                ErrorCode::BranchNotFound,
                &format!(
                    "ticket `{ticket_id}` is missing branches for: {}",
                    orphans.join(", ")
                ),
                Some(json!({ "orphans": orphans })),
            );
        }
    }

    // Verify every target branch exists.
    for p in &plans {
        let Some(t) = &p.target else { continue };
        let r = match read::open(&p.repo.path) {
            Ok(r) => r,
            Err(e) => {
                return emit_err(
                    ctx,
                    "stage",
                    ErrorCode::Internal,
                    &format!("open repo `{}`: {e}", p.repo.name),
                    None,
                );
            }
        };
        match read::branch_exists(&r, t) {
            Ok(true) => {}
            Ok(false) => {
                return emit_err(
                    ctx,
                    "stage",
                    ErrorCode::BranchNotFound,
                    &format!("branch `{t}` does not exist in repo `{}`", p.repo.name),
                    Some(json!({ "repo": p.repo.name, "branch": t })),
                );
            }
            Err(e) => {
                return emit_err(
                    ctx,
                    "stage",
                    ErrorCode::Internal,
                    &format!("branch check: {e}"),
                    None,
                );
            }
        }
    }

    // Snapshot first (before we touch anything).
    let mut snapshots = Vec::new();
    for p in &plans {
        if p.target.is_none() && !touch_orphan {
            continue;
        }
        let s = match snapshot_repo(p.repo) {
            Ok(s) => s,
            Err(e) => {
                return emit_err(
                    ctx,
                    "stage",
                    ErrorCode::Internal,
                    &format!("snapshot `{}`: {e}", p.repo.name),
                    None,
                );
            }
        };
        snapshots.push(s);
    }

    // Now do checkouts. Per-repo result aggregated.
    let mut results: Vec<PerRepo> = Vec::with_capacity(plans.len());
    let mut any_failed = false;
    for p in &plans {
        match (&p.target, touch_orphan) {
            (Some(target), _) => {
                // The ticket branch is owned by a worktree, so we can't
                // check it out by name. Detach to the branch's SHA instead.
                let target_sha = match exec::rev_parse(&p.repo.path, target) {
                    Ok(s) => s,
                    Err(e) => {
                        any_failed = true;
                        results.push(PerRepo {
                            repo: p.repo.name.clone(),
                            ok: false,
                            target_branch: Some(target.clone()),
                            action: "failed",
                            error_code: Some(e.code()),
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                };
                let r = read::open(&p.repo.path);
                let cur_sha = r.as_ref().ok().and_then(|r| read::head_sha(r).ok());
                if cur_sha.as_deref() == Some(target_sha.as_str()) {
                    results.push(PerRepo {
                        repo: p.repo.name.clone(),
                        ok: true,
                        target_branch: Some(target.clone()),
                        action: "already_on",
                        error: None,
                        error_code: None,
                    });
                    continue;
                }
                match exec::checkout_detach(&p.repo.path, &target_sha) {
                    Ok(_) => results.push(PerRepo {
                        repo: p.repo.name.clone(),
                        ok: true,
                        target_branch: Some(target.clone()),
                        action: "switched",
                        error: None,
                        error_code: None,
                    }),
                    Err(e) => {
                        any_failed = true;
                        results.push(PerRepo {
                            repo: p.repo.name.clone(),
                            ok: false,
                            target_branch: Some(target.clone()),
                            action: "failed",
                            error_code: Some(e.code()),
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
            (None, false) => {
                results.push(PerRepo {
                    repo: p.repo.name.clone(),
                    ok: true,
                    target_branch: None,
                    action: "left",
                    error: None,
                    error_code: None,
                });
            }
            (None, true) => {
                // Switch orphan to baseline.
                let baseline_name = match baseline::resolve(p.repo) {
                    Ok(b) => b,
                    Err(e) => {
                        any_failed = true;
                        results.push(PerRepo {
                            repo: p.repo.name.clone(),
                            ok: false,
                            target_branch: None,
                            action: "failed",
                            error_code: Some(ErrorCode::BranchNotFound),
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                };
                let r = read::open(&p.repo.path);
                let cur = r
                    .as_ref()
                    .ok()
                    .and_then(|r| read::current_branch(r).ok().flatten());
                if cur.as_deref() == Some(&baseline_name) {
                    results.push(PerRepo {
                        repo: p.repo.name.clone(),
                        ok: true,
                        target_branch: Some(baseline_name),
                        action: "already_on",
                        error: None,
                        error_code: None,
                    });
                    continue;
                }
                match exec::checkout(&p.repo.path, &baseline_name) {
                    Ok(_) => results.push(PerRepo {
                        repo: p.repo.name.clone(),
                        ok: true,
                        target_branch: Some(baseline_name),
                        action: "baseline",
                        error: None,
                        error_code: None,
                    }),
                    Err(e) => {
                        any_failed = true;
                        results.push(PerRepo {
                            repo: p.repo.name.clone(),
                            ok: false,
                            target_branch: Some(baseline_name),
                            action: "failed",
                            error_code: Some(e.code()),
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
        }
    }

    // Commit staged record (snapshot taken from *before* we touched anything).
    let staged = Staged {
        ticket: ticket_id.to_string(),
        suffix: suffix.map(String::from),
        snapshots,
        partial: any_failed,
    };
    if let Err(e) = staged.save(&state) {
        return emit_internal(ctx, "stage", &format!("save staged record: {e}"));
    }

    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    audit::record(&state.audit_file(), "stage", argv, exit);

    let body = StageData {
        ticket: ticket_id,
        suffix,
        repos: results,
    };
    if any_failed {
        if ctx.json {
            return emit_err(
                ctx,
                "stage",
                ErrorCode::PartialFailure,
                "one or more repos failed during stage",
                Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
            );
        }
        for r in &body.repos {
            human_line(r);
        }
        return ExitCode::from(exit);
    }
    let lines: Vec<_> = body.repos.iter().map(line_tuple).collect();
    emit_ok(ctx, "stage", body, || {
        for l in &lines {
            eprintln!("[{}] {} ({})", l.0, l.1, l.2);
        }
    })
}

fn line_tuple(r: &PerRepo) -> (String, &'static str, String) {
    (
        r.repo.clone(),
        r.action,
        r.target_branch.clone().unwrap_or_else(|| "-".into()),
    )
}
fn human_line(r: &PerRepo) {
    if r.ok {
        eprintln!(
            "[{}] {} ({})",
            r.repo,
            r.action,
            r.target_branch.as_deref().unwrap_or("-")
        );
    } else {
        eprintln!(
            "[{}] failed: {}",
            r.repo,
            r.error.as_deref().unwrap_or("unknown")
        );
    }
}

fn snapshot_repo(repo: &Repo) -> anyhow::Result<Snapshot> {
    let st = read::state(&repo.path)?;
    Ok(Snapshot {
        repo: repo.name.clone(),
        branch: st.branch,
        head_sha: st.head_sha,
        clean: !st.dirty,
    })
}

