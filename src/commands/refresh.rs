use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::audit;
use crate::baseline;
use crate::commands::{emit_internal, require_workspace};
use crate::config::Repo;
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
    baseline: String,
    action: &'static str, // pulled | already_on | failed
    error: Option<String>,
}

#[derive(Serialize)]
struct RefreshData {
    repos: Vec<PerRepo>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    repos_filter: Option<&[String]>,
    non_interactive: bool,
    argv: &[String],
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "refresh") {
        Ok(w) => w,
        Err(code) => return code,
    };
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "refresh", &e.to_string()),
    };
    let _guard = match lock::acquire(&state.lock_file(), non_interactive) {
        Ok(g) => g,
        Err(e) => return emit_err(ctx, "refresh", e.code(), &e.to_string(), None),
    };

    // Refuse to refresh while staged — the design says explicit cleanup first.
    if let Ok(Some(s)) = Staged::load(&state) {
        return emit_err(
            ctx,
            "refresh",
            ErrorCode::AlreadyStaged,
            &format!("ticket `{}` is staged; cleanup or unstage first", s.ticket),
            Some(json!({ "ticket": s.ticket })),
        );
    }

    let chosen: Vec<&Repo> = match repos_filter {
        Some(names) => {
            let mut out = Vec::new();
            for n in names {
                match ws.config.repo(n) {
                    Some(r) => out.push(r),
                    None => {
                        return emit_err(
                            ctx,
                            "refresh",
                            ErrorCode::RepoNotInWorkspace,
                            &format!("repo `{n}` not in workspace"),
                            Some(json!({ "repo": n })),
                        );
                    }
                }
            }
            out
        }
        None => ws.config.repos.iter().collect(),
    };

    // Preflight: every repo must be clean (no need to forbid detached HEAD;
    // we'll fail naturally on checkout if HEAD is weird).
    let opts = Opts {
        require_clean: true,
        forbid_detached: false,
        forbid_mid_op: true,
        forbid_unpushed: false,
    };
    let mut issues = Vec::new();
    for r in &chosen {
        issues.extend(preflight::check(r, &opts));
    }
    if !issues.is_empty() {
        let first = &issues[0];
        return emit_err(
            ctx,
            "refresh",
            first.code,
            &first.message,
            Some(serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null)),
        );
    }

    let mut results = Vec::with_capacity(chosen.len());
    let mut any_failed = false;
    for repo in &chosen {
        let baseline_name = match baseline::resolve(repo) {
            Ok(b) => b,
            Err(e) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    baseline: String::new(),
                    action: "failed",
                    error: Some(e.to_string()),
                });
                continue;
            }
        };
        // Make sure preprod is *on* baseline before pulling. If it's on
        // something else, switch it back first.
        let r = match read::open(&repo.path) {
            Ok(r) => r,
            Err(e) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    baseline: baseline_name.clone(),
                    action: "failed",
                    error: Some(format!("open: {e}")),
                });
                continue;
            }
        };
        let cur = read::current_branch(&r).ok().flatten();
        if cur.as_deref() != Some(baseline_name.as_str()) {
            if let Err(e) = exec::checkout(&repo.path, &baseline_name) {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    baseline: baseline_name.clone(),
                    action: "failed",
                    error: Some(format!("checkout baseline: {e}")),
                });
                continue;
            }
        }
        if let Err(e) = exec::fetch(&repo.path) {
            any_failed = true;
            results.push(PerRepo {
                repo: repo.name.clone(),
                ok: false,
                baseline: baseline_name.clone(),
                action: "failed",
                error: Some(format!("fetch: {e}")),
            });
            continue;
        }
        match exec::pull_ff_only(&repo.path) {
            Ok(_) => results.push(PerRepo {
                repo: repo.name.clone(),
                ok: true,
                baseline: baseline_name,
                action: "pulled",
                error: None,
            }),
            Err(e) => {
                any_failed = true;
                results.push(PerRepo {
                    repo: repo.name.clone(),
                    ok: false,
                    baseline: baseline_name,
                    action: "failed",
                    error: Some(format!("pull: {e}")),
                });
            }
        }
    }

    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    audit::record(&state.audit_file(), "refresh", argv, exit);

    if any_failed && ctx.json {
        let body = RefreshData { repos: results };
        return emit_err(
            ctx,
            "refresh",
            ErrorCode::PartialFailure,
            "one or more repos failed to refresh",
            Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
        );
    }
    if any_failed {
        for r in &results {
            if r.ok {
                eprintln!("[{}] {} ({})", r.repo, r.action, r.baseline);
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

    let lines: Vec<(String, &'static str, String)> = results
        .iter()
        .map(|r| (r.repo.clone(), r.action, r.baseline.clone()))
        .collect();
    emit_ok(
        ctx,
        "refresh",
        RefreshData { repos: results },
        || {
            for (repo, action, baseline) in &lines {
                eprintln!("[{repo}] {action} ({baseline})");
            }
        },
    )
}
