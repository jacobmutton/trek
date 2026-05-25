use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use crate::audit;
use crate::baseline;
use crate::commands::{emit_internal, require_workspace};
use crate::error::ErrorCode;
use crate::git::{exec, read};
use crate::lock;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::preflight::{self, Opts};
use crate::staged::Staged;
use crate::state::StateRoot;

#[derive(Serialize)]
struct PerRepo {
    repo: String,
    ok: bool,
    target_branch: String,
    action: &'static str,
    error: Option<String>,
    error_code: Option<ErrorCode>,
}

#[derive(Serialize)]
struct UnstageData {
    ticket: String,
    repos: Vec<PerRepo>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    to_baseline: bool,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "unstage") {
        Ok(w) => w,
        Err(code) => return code,
    };
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "unstage", &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => return emit_err(ctx, "unstage", e.code(), &e.to_string(), None),
    };

    let Some(staged) = (match Staged::load(&state) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "unstage", &e.to_string()),
    }) else {
        return emit_err(
            ctx,
            "unstage",
            ErrorCode::NotStaged,
            "nothing is currently staged",
            None,
        );
    };

    // Preflight: every repo we'll touch must be clean / not mid-op.
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
            "unstage",
            first.code,
            &first.message,
            Some(serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null)),
        );
    }

    let mut results: Vec<PerRepo> = Vec::with_capacity(staged.snapshots.len());
    let mut any_failed = false;

    for snap in &staged.snapshots {
        let Some(repo) = ws.config.repo(&snap.repo) else {
            continue;
        };
        let target = if to_baseline {
            match baseline::resolve(repo) {
                Ok(b) => b,
                Err(e) => {
                    any_failed = true;
                    results.push(PerRepo {
                        repo: snap.repo.clone(),
                        ok: false,
                        target_branch: String::new(),
                        action: "failed",
                        error: Some(e.to_string()),
                        error_code: Some(ErrorCode::BranchNotFound),
                    });
                    continue;
                }
            }
        } else {
            let Some(b) = snap.branch.clone() else {
                any_failed = true;
                results.push(PerRepo {
                    repo: snap.repo.clone(),
                    ok: false,
                    target_branch: String::new(),
                    action: "failed",
                    error: Some("snapshot has no branch (was detached); use --to-baseline".into()),
                    error_code: Some(ErrorCode::DriftDetected),
                });
                continue;
            };
            b
        };

        // Drift check (snapshot mode only): if the current branch is set
        // (not detached) and differs from both the snapshot branch *and*
        // every ticket branch, the user has manually switched preprod
        // somewhere else — refuse with DRIFT_DETECTED.
        if !to_baseline {
            let r = read::open(&repo.path).ok();
            if let Some(rr) = r.as_ref() {
                let cur = read::current_branch(rr).ok().flatten();
                if cur.as_deref() == Some(target.as_str()) {
                    results.push(PerRepo {
                        repo: snap.repo.clone(),
                        ok: true,
                        target_branch: target.clone(),
                        action: "already_on",
                        error: None,
                        error_code: None,
                    });
                    continue;
                }
                if let Some(c) = &cur {
                    // Detached HEAD (None) is expected — that's what trek
                    // leaves preprod in. A *different* named branch means
                    // the user moved it.
                    any_failed = true;
                    results.push(PerRepo {
                        repo: snap.repo.clone(),
                        ok: false,
                        target_branch: target.clone(),
                        action: "failed",
                        error: Some(format!(
                            "preprod is on `{c}`, expected `{target}` or detached HEAD; use --to-baseline to force"
                        )),
                        error_code: Some(ErrorCode::DriftDetected),
                    });
                    continue;
                }
            }
        }

        match exec::checkout(&repo.path, &target) {
            Ok(_) => results.push(PerRepo {
                repo: snap.repo.clone(),
                ok: true,
                target_branch: target,
                action: if to_baseline { "baseline" } else { "restored" },
                error: None,
                error_code: None,
            }),
            Err(e) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: snap.repo.clone(),
                    ok: false,
                    target_branch: target,
                    action: "failed",
                    error: Some(e.to_string()),
                    error_code: Some(e.code()),
                });
            }
        }
    }

    if !any_failed {
        if let Err(e) = Staged::clear(&state) {
            return emit_internal(ctx, "unstage", &format!("clear staged: {e}"));
        }
    }
    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    audit::record(&state.audit_file(), "unstage", argv, exit);

    let body = UnstageData {
        ticket: staged.ticket.clone(),
        repos: results,
    };
    if any_failed && ctx.json {
        return emit_err(
            ctx,
            "unstage",
            ErrorCode::PartialFailure,
            "one or more repos failed during unstage",
            Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
        );
    }
    if any_failed {
        for r in &body.repos {
            if r.ok {
                eprintln!("[{}] {} ({})", r.repo, r.action, r.target_branch);
            } else {
                eprintln!(
                    "[{}] failed: {}",
                    r.repo,
                    r.error.as_deref().unwrap_or("unknown")
                );
            }
        }
        return ExitCode::from(exit);
    }
    let lines: Vec<_> = body
        .repos
        .iter()
        .map(|r| (r.repo.clone(), r.action, r.target_branch.clone()))
        .collect();
    emit_ok(ctx, "unstage", body, || {
        for (repo, action, target) in &lines {
            eprintln!("[{repo}] {action} ({target})");
        }
    })
}
