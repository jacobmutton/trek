use serde::Serialize;
use serde_json::Value;
use std::process::ExitCode;

use crate::error::ErrorCode;

pub const SCHEMA: &str = "trek.v1";

/// Visible to tests: rebuild the envelope without going through stdout.
#[cfg(test)]
pub fn envelope_ok_string<T: Serialize>(command: &str, data: T) -> String {
    let env = Envelope {
        schema: SCHEMA,
        command,
        ok: true,
        data: Some(data),
        error: None::<ErrorBody>,
    };
    serde_json::to_string(&env).unwrap()
}

#[cfg(test)]
pub fn envelope_err_string(
    command: &str,
    code: ErrorCode,
    message: &str,
    details: Option<Value>,
) -> String {
    let env: Envelope<()> = Envelope {
        schema: SCHEMA,
        command,
        ok: false,
        data: None,
        error: Some(ErrorBody {
            code,
            message,
            details,
        }),
    };
    serde_json::to_string(&env).unwrap()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct StageData<'a> {
        ticket: &'a str,
        repos: Vec<&'a str>,
    }

    #[test]
    fn ok_envelope_shape() {
        let s = envelope_ok_string(
            "stage",
            StageData {
                ticket: "FUN-1",
                repos: vec!["api", "web"],
            },
        );
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["schema"], "trek.v1");
        assert_eq!(v["command"], "stage");
        assert_eq!(v["ok"], true);
        assert!(v["error"].is_null());
        assert_eq!(v["data"]["ticket"], "FUN-1");
        assert_eq!(v["data"]["repos"][0], "api");
    }

    #[test]
    fn err_envelope_shape() {
        let s = envelope_err_string(
            "stage",
            ErrorCode::DirtyWorktree,
            "repo api has uncommitted changes",
            Some(json!({ "repo": "api", "git_status": " M src/main.rs" })),
        );
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["schema"], "trek.v1");
        assert_eq!(v["ok"], false);
        assert!(v["data"].is_null());
        assert_eq!(v["error"]["code"], "DIRTY_WORKTREE");
        assert_eq!(v["error"]["message"], "repo api has uncommitted changes");
        assert_eq!(v["error"]["details"]["repo"], "api");
    }
}
