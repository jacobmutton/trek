use serde::Serialize;
use serde_json::Value;
use std::io::IsTerminal;
use std::process::ExitCode;

use crate::error::ErrorCode;

pub const SCHEMA: &str = "trek.v1";

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    schema: &'static str,
    command: &'a str,
    ok: bool,
    data: Option<T>,
    error: Option<ErrorBody<'a>>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: ErrorCode,
    message: &'a str,
    details: Option<Value>,
}

/// Global rendering options pulled from the CLI flags.
#[derive(Clone, Copy)]
pub struct OutputCtx {
    pub json: bool,
    pub quiet: bool,
    #[allow(dead_code)]
    pub verbose: bool,
}

impl OutputCtx {
    pub fn use_color(self) -> bool {
        !self.json && std::io::stderr().is_terminal()
    }
}

/// A successful command result.
pub fn emit_ok<T: Serialize>(
    ctx: OutputCtx,
    command: &str,
    data: T,
    human: impl FnOnce(),
) -> ExitCode {
    if ctx.json {
        let env = Envelope {
            schema: SCHEMA,
            command,
            ok: true,
            data: Some(data),
            error: None,
        };
        match serde_json::to_string(&env) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("trek: failed to serialize result: {e}");
                return ExitCode::from(ErrorCode::Internal.exit_code());
            }
        }
    } else if !ctx.quiet {
        human();
    }
    ExitCode::SUCCESS
}

/// An error result. `human_msg` is printed to stderr in non-JSON mode (and is
/// also used as the `message` field of the JSON envelope).
pub fn emit_err(
    ctx: OutputCtx,
    command: &str,
    code: ErrorCode,
    human_msg: &str,
    details: Option<Value>,
) -> ExitCode {
    if ctx.json {
        let env: Envelope<()> = Envelope {
            schema: SCHEMA,
            command,
            ok: false,
            data: None,
            error: Some(ErrorBody {
                code,
                message: human_msg,
                details,
            }),
        };
        match serde_json::to_string(&env) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("trek: failed to serialize error: {e}");
                return ExitCode::from(ErrorCode::Internal.exit_code());
            }
        }
    } else if !ctx.quiet {
        eprintln!("trek {command}: {human_msg}");
    }
    ExitCode::from(code.exit_code())
}
