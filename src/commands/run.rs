//! `trek run --in {worktree|preprod} -t <ticket> [...] -- <cmd...>`.
//!
//! Sequential streams each per-repo command, line-prefixed with `[<repo>] `.
//! `--parallel` captures per-repo output and prints it in repo order after
//! every command finishes. JSON mode always captures and emits per-repo
//! `exit_code` / `stdout` / `stderr` / `duration_ms`.
//!
//! Aggregate exit code is `0` iff every per-repo run is `0`; otherwise `3`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use crate::cli::RunLocation;
use crate::commands::{emit_internal, require_workspace};
use crate::config::Repo;
use crate::error::ErrorCode;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::staged::Staged;
use crate::state::StateRoot;
use crate::ticket::Ticket;

#[derive(Serialize)]
struct PerRepo {
    repo: String,
    path: PathBuf,
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u128,
    error: Option<String>,
}

#[derive(Serialize)]
struct RunData<'a> {
    ticket: &'a str,
    suffix: Option<&'a str>,
    location: &'static str,
    repos: Vec<PerRepo>,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    location: RunLocation,
    ticket: &str,
    suffix: Option<&str>,
    repos_filter: Option<&[String]>,
    parallel: bool,
    cmd: &[String],
) -> ExitCode {
    if cmd.is_empty() {
        return emit_err(ctx, "run", ErrorCode::Internal, "command is empty", None);
    }
    let ws = match require_workspace(workspace, ctx, "run") {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket) {
        return emit_err(ctx, "run", ErrorCode::InvalidTicketId, &e.to_string(), None);
    }
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "run", &e.to_string()),
    };

    // --in preprod refuses unless the staged ticket matches -t.
    if matches!(location, RunLocation::Preprod) {
        let staged = match Staged::load(&state) {
            Ok(s) => s,
            Err(e) => return emit_internal(ctx, "run", &e.to_string()),
        };
        let Some(s) = staged else {
            return emit_err(
                ctx,
                "run",
                ErrorCode::NotStaged,
                "nothing is staged; `trek stage` first",
                None,
            );
        };
        if s.ticket != ticket {
            return emit_err(
                ctx,
                "run",
                ErrorCode::NotStaged,
                &format!(
                    "ticket `{ticket}` is not staged (currently staged: `{}`); --in preprod requires the staged ticket to match -t",
                    s.ticket
                ),
                Some(json!({ "staged": s.ticket, "requested": ticket })),
            );
        }
    }

    let ticket_rec = match Ticket::load(&state, ticket) {
        Ok(t) => t,
        Err(e) => return emit_internal(ctx, "run", &e.to_string()),
    };

    // Resolve (repo_name, working_dir) per target.
    let targets = match location {
        RunLocation::Worktree => match resolve_worktree_targets(&ws.config, &ticket_rec, ticket, suffix, repos_filter) {
            Ok(t) => t,
            Err(code) => return code(ctx),
        },
        RunLocation::Preprod => match resolve_preprod_targets(&ws.config, repos_filter) {
            Ok(t) => t,
            Err(code) => return code(ctx),
        },
    };

    let cmd_owned: Vec<String> = cmd.to_vec();
    let mut results: Vec<PerRepo> = Vec::with_capacity(targets.len());

    if parallel {
        // Parallel: capture per-repo, then print in repo order.
        let mut handles = Vec::with_capacity(targets.len());
        for (repo, path) in targets {
            let cmd = cmd_owned.clone();
            let h = std::thread::spawn(move || (repo, path.clone(), run_capture(&path, &cmd)));
            handles.push(h);
        }
        for h in handles {
            let (repo, path, r) = h.join().expect("run thread panicked");
            results.push(into_per_repo(repo, path, r));
        }
        if !ctx.json {
            for r in &results {
                print_repo_block(r);
            }
        }
    } else if ctx.json {
        for (repo, path) in targets {
            let r = run_capture(&path, &cmd_owned);
            results.push(into_per_repo(repo, path, r));
        }
    } else {
        for (repo, path) in targets {
            let prefix = format!("[{repo}] ");
            let r = run_stream(&path, &cmd_owned, &prefix);
            results.push(into_per_repo(repo, path, r));
        }
    }

    let any_failed = results
        .iter()
        .any(|r| r.exit_code != 0 || r.error.is_some());
    let exit = if any_failed {
        ErrorCode::PartialFailure.exit_code()
    } else {
        0
    };
    let body = RunData {
        ticket,
        suffix,
        location: match location {
            RunLocation::Worktree => "worktree",
            RunLocation::Preprod => "preprod",
        },
        repos: results,
    };
    if ctx.json {
        if any_failed {
            return emit_err(
                ctx,
                "run",
                ErrorCode::PartialFailure,
                "one or more repos exited non-zero",
                Some(serde_json::to_value(&body).unwrap_or(serde_json::Value::Null)),
            );
        }
        return emit_ok(ctx, "run", body, || {});
    }
    if any_failed {
        return ExitCode::from(exit);
    }
    ExitCode::SUCCESS
}

type ErrFn = Box<dyn FnOnce(OutputCtx) -> ExitCode>;

fn resolve_worktree_targets(
    cfg: &crate::config::Config,
    ticket_rec: &Option<Ticket>,
    ticket: &str,
    suffix: Option<&str>,
    repos_filter: Option<&[String]>,
) -> Result<Vec<(String, PathBuf)>, ErrFn> {
    let Some(rec) = ticket_rec else {
        let t = ticket.to_string();
        return Err(Box::new(move |ctx| {
            emit_err(
                ctx,
                "run",
                ErrorCode::TicketNotFound,
                &format!("ticket `{t}` not tracked"),
                Some(json!({ "ticket": t })),
            )
        }));
    };
    let matched: Vec<_> = rec
        .branches
        .iter()
        .filter(|b| b.suffix.as_deref() == suffix)
        .collect();

    let mut out = Vec::new();
    if let Some(names) = repos_filter {
        for n in names {
            if cfg.repo(n).is_none() {
                let n = n.clone();
                return Err(Box::new(move |ctx| {
                    emit_err(
                        ctx,
                        "run",
                        ErrorCode::RepoNotInWorkspace,
                        &format!("repo `{n}` not in workspace"),
                        Some(json!({ "repo": n })),
                    )
                }));
            }
            let Some(b) = matched.iter().find(|b| &b.repo == n) else {
                let n = n.clone();
                let s = suffix.map(String::from);
                let ticket = ticket.to_string();
                return Err(Box::new(move |ctx| {
                    let s_human = s.as_deref().map(|s| format!(" with suffix `{s}`")).unwrap_or_default();
                    emit_err(
                        ctx,
                        "run",
                        ErrorCode::SuffixNotFound,
                        &format!("ticket `{ticket}` has no branch for repo `{n}`{s_human}"),
                        Some(json!({ "repo": n, "suffix": s })),
                    )
                }));
            };
            out.push((b.repo.clone(), b.worktree.clone()));
        }
    } else {
        for b in matched {
            out.push((b.repo.clone(), b.worktree.clone()));
        }
        if out.is_empty() {
            let s = suffix.map(String::from);
            let ticket = ticket.to_string();
            return Err(Box::new(move |ctx| {
                let s_human = s.as_deref().map(|s| format!(" with suffix `{s}`")).unwrap_or_default();
                emit_err(
                    ctx,
                    "run",
                    ErrorCode::SuffixNotFound,
                    &format!("ticket `{ticket}` has no branches{s_human}"),
                    Some(json!({ "ticket": ticket, "suffix": s })),
                )
            }));
        }
    }
    Ok(out)
}

fn resolve_preprod_targets(
    cfg: &crate::config::Config,
    repos_filter: Option<&[String]>,
) -> Result<Vec<(String, PathBuf)>, ErrFn> {
    let chosen: Vec<&Repo> = match repos_filter {
        Some(names) => {
            let mut out = Vec::new();
            for n in names {
                match cfg.repo(n) {
                    Some(r) => out.push(r),
                    None => {
                        let n = n.clone();
                        return Err(Box::new(move |ctx| {
                            emit_err(
                                ctx,
                                "run",
                                ErrorCode::RepoNotInWorkspace,
                                &format!("repo `{n}` not in workspace"),
                                Some(json!({ "repo": n })),
                            )
                        }));
                    }
                }
            }
            out
        }
        None => cfg.repos.iter().collect(),
    };
    Ok(chosen
        .into_iter()
        .map(|r| (r.name.clone(), r.path.clone()))
        .collect())
}

struct RunOutcome {
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration: Duration,
    error: Option<String>,
}

fn run_capture(path: &Path, cmd: &[String]) -> RunOutcome {
    let start = Instant::now();
    let r = Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(path)
        .output();
    let duration = start.elapsed();
    match r {
        Ok(out) => RunOutcome {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            duration,
            error: None,
        },
        Err(e) => RunOutcome {
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            duration,
            error: Some(e.to_string()),
        },
    }
}

fn run_stream(path: &Path, cmd: &[String], prefix: &str) -> RunOutcome {
    let start = Instant::now();
    let spawned = Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return RunOutcome {
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration: start.elapsed(),
                error: Some(e.to_string()),
            };
        }
    };
    let child_stdout = child.stdout.take().expect("piped stdout");
    let child_stderr = child.stderr.take().expect("piped stderr");
    let p_out = prefix.to_string();
    let p_err = prefix.to_string();
    let t_out = std::thread::spawn(move || {
        let stdout = std::io::stdout();
        for line in BufReader::new(child_stdout).lines().map_while(Result::ok) {
            let mut lock = stdout.lock();
            let _ = writeln!(lock, "{p_out}{line}");
        }
    });
    let t_err = std::thread::spawn(move || {
        let stderr = std::io::stderr();
        for line in BufReader::new(child_stderr).lines().map_while(Result::ok) {
            let mut lock = stderr.lock();
            let _ = writeln!(lock, "{p_err}{line}");
        }
    });
    let status = child.wait();
    let _ = t_out.join();
    let _ = t_err.join();
    let duration = start.elapsed();
    match status {
        Ok(s) => RunOutcome {
            exit_code: s.code().unwrap_or(-1),
            stdout: String::new(),
            stderr: String::new(),
            duration,
            error: None,
        },
        Err(e) => RunOutcome {
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            duration,
            error: Some(e.to_string()),
        },
    }
}

fn into_per_repo(repo: String, path: PathBuf, r: RunOutcome) -> PerRepo {
    PerRepo {
        repo,
        path,
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
        duration_ms: r.duration.as_millis(),
        error: r.error,
    }
}

fn print_repo_block(r: &PerRepo) {
    let prefix = format!("[{}] ", r.repo);
    for line in r.stdout.lines() {
        println!("{prefix}{line}");
    }
    for line in r.stderr.lines() {
        eprintln!("{prefix}{line}");
    }
    if let Some(err) = &r.error {
        eprintln!("{prefix}error: {err}");
    }
}
