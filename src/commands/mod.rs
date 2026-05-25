pub mod cleanup;
pub mod init;
pub mod list;
pub mod path;
pub mod refresh;
pub mod run;
pub mod stage;
pub mod start;
pub mod status;
pub mod unstage;
pub mod r#where;

use std::path::Path;
use std::process::ExitCode;

use serde_json::json;

use crate::error::ErrorCode;
use crate::output::{OutputCtx, emit_err};
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
