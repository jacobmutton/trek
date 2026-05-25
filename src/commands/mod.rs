pub mod cleanup;
pub mod doctor;
pub mod init;
pub mod list;
pub mod path;
pub mod push;
pub mod refresh;
pub mod run;
pub mod stage;
pub mod start;
pub mod status;
pub mod sync;
pub mod unstage;
pub mod r#where;

use std::path::Path;
use std::process::ExitCode;

use serde_json::json;

use crate::audit;
use crate::error::ErrorCode;
use crate::hooks::{self, HookEnv};
use crate::output::{OutputCtx, emit_err};
use crate::state::StateRoot;
use crate::workspace::{Workspace, WorkspaceError, resolve};

/// Resolve the workspace or emit a NO_WORKSPACE error.
pub fn require_workspace(
    explicit: Option<&Path>,
    ctx: OutputCtx,
    command: &str,
) -> Result<Workspace, ExitCode> {
    match resolve(explicit) {
        Ok(ws) => Ok(ws),
        Err(e) => {
            let code = e.code();
            let details = match &e {
                WorkspaceError::Missing(p) => Some(json!({ "path": p })),
                _ => None,
            };
            Err(emit_err(ctx, command, code, &e.to_string(), details))
        }
    }
}

/// For internal-error paths.
pub fn emit_internal(ctx: OutputCtx, command: &str, msg: &str) -> ExitCode {
    emit_err(ctx, command, ErrorCode::Internal, msg, None)
}

/// Record an audit entry and fire the post-command hook (if any). Always call
/// this on the final exit path of a state-changing command.
pub fn finalize(
    ws: &Workspace,
    state: &StateRoot,
    command: &str,
    argv: &[String],
    exit: u8,
    ticket: Option<&str>,
    suffix: Option<&str>,
) {
    audit::record(&state.audit_file(), command, argv, exit);
    hooks::run(
        ws,
        &ws.config.hooks,
        &HookEnv {
            command,
            exit_code: exit,
            ticket,
            suffix,
        },
    );
}
