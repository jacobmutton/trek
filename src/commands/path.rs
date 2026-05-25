use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::commands::require_workspace;
use crate::error::ErrorCode;
use crate::naming;
use crate::output::{OutputCtx, emit_err, emit_ok};

#[derive(Serialize)]
struct PathData<'a> {
    repo: &'a str,
    ticket: &'a str,
    suffix: Option<&'a str>,
    location: &'static str,
    path: std::path::PathBuf,
}

pub fn run(
    ctx: OutputCtx,
    workspace: Option<&Path>,
    ticket: &str,
    repo: &str,
    suffix: Option<&str>,
    preprod: bool,
) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "path") {
        Ok(w) => w,
        Err(code) => return code,
    };
    if let Err(e) = naming::validate_ticket(&ws.config, ticket) {
        return emit_err(ctx, "path", ErrorCode::InvalidTicketId, &e.to_string(), None);
    }
    let Some(r) = ws.config.repo(repo) else {
        return emit_err(
            ctx,
            "path",
            ErrorCode::RepoNotInWorkspace,
            &format!("repo `{repo}` not in workspace"),
            Some(json!({ "repo": repo })),
        );
    };
    let path = if preprod {
        r.path.clone()
    } else {
        match naming::resolve(&ws.config, r, ticket, suffix) {
            Ok(n) => n.worktree,
            Err(e) => {
                return emit_err(ctx, "path", ErrorCode::Internal, &e.to_string(), None);
            }
        }
    };
    let location = if preprod { "preprod" } else { "worktree" };
    let path_out = path.clone();
    emit_ok(
        ctx,
        "path",
        PathData {
            repo: &r.name,
            ticket,
            suffix,
            location,
            path: path.clone(),
        },
        || {
            println!("{}", path_out.display());
        },
    )
}
