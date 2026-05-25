//! Hook execution. Hooks are configured under `[hooks]` in trek.toml and run
//! AFTER the command they're attached to has finished mutating state. They
//! are best-effort: a failing hook does NOT change the command's exit code,
//! but it does emit a stderr warning so the agent / user notices.
//!
//! Hooks are invoked via `sh -c "<command-line>"` with this environment:
//!  - TREK_COMMAND          the trek subcommand (start, stage, ...)
//!  - TREK_EXIT_CODE        the command's exit code (decimal string)
//!  - TREK_WORKSPACE        the workspace dir (where trek.toml lives)
//!  - TREK_WORKSPACE_ID     the workspace UUID
//!  - TREK_TICKET           the ticket id if the command involves one, else empty
//!  - TREK_SUFFIX           the suffix if any, else empty

use std::process::{Command, Stdio};

use crate::config::Hooks;
use crate::workspace::Workspace;

pub struct HookEnv<'a> {
    pub command: &'a str,
    pub exit_code: u8,
    pub ticket: Option<&'a str>,
    pub suffix: Option<&'a str>,
}

/// Run the configured hook (if any) for the given command. Errors are logged
/// to stderr; never returned. Hooks run with stdout/stderr inherited so the
/// user sees their output.
pub fn run(ws: &Workspace, hooks: &Hooks, env: &HookEnv<'_>) {
    let Some(cmd) = hooks.for_command(env.command) else {
        return;
    };
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }
    let result = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("TREK_COMMAND", env.command)
        .env("TREK_EXIT_CODE", env.exit_code.to_string())
        .env("TREK_WORKSPACE", &ws.dir)
        .env("TREK_WORKSPACE_ID", ws.config.workspace.id.to_string())
        .env("TREK_TICKET", env.ticket.unwrap_or(""))
        .env("TREK_SUFFIX", env.suffix.unwrap_or(""))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match result {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "trek {0}: post_{0} hook exited {1}",
                env.command,
                s.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            eprintln!("trek {}: post_{} hook failed to spawn: {e}", env.command, env.command);
        }
    }
}
