use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use uuid::Uuid;

use crate::error::ErrorCode;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::workspace::TREK_TOML;

#[derive(Serialize)]
struct InitData<'a> {
    workspace_id: Uuid,
    path: &'a Path,
    created: bool,
}

const SCAFFOLD: &str = r#"# trek workspace config. Add one [[repos]] table per repo.
#
# See `trek --help` and the design doc for the full schema.

[workspace]
name = "__NAME__"
id   = "__UUID__"

[branch]
pattern     = "{user}-{ticket}"
suffix_join = "-"
# ticket_regex = "^[A-Z]+-\\d+$"

[stage]
orphan_repos = "baseline"   # baseline | leave | fail

# [[repos]]
# name         = "api"
# path         = "~/code/api"
# worktree_dir = "~/worktrees/api"
# baseline     = "main"        # optional; defaults to origin/HEAD
"#;

pub fn run(ctx: OutputCtx) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            return emit_err(
                ctx,
                "init",
                ErrorCode::Internal,
                &format!("cwd: {e}"),
                None,
            );
        }
    };
    let target = cwd.join(TREK_TOML);
    if target.exists() {
        return emit_err(
            ctx,
            "init",
            ErrorCode::Internal,
            &format!("{} already exists", target.display()),
            None,
        );
    }
    let id = Uuid::new_v4();
    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    let body = SCAFFOLD
        .replace("__NAME__", &name)
        .replace("__UUID__", &id.to_string());
    if let Err(e) = std::fs::write(&target, body) {
        return emit_err(
            ctx,
            "init",
            ErrorCode::Internal,
            &format!("write {}: {e}", target.display()),
            None,
        );
    }
    emit_ok(
        ctx,
        "init",
        InitData {
            workspace_id: id,
            path: &target,
            created: true,
        },
        || {
            eprintln!("trek init: wrote {} (workspace id {id})", target.display());
        },
    )
}
