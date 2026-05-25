use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::commands::{emit_internal, require_workspace};
use crate::error::ErrorCode;
use crate::output::{OutputCtx, emit_err, emit_ok};
use crate::staged::Staged;
use crate::state::StateRoot;
use crate::ticket::{self, Ticket};

#[derive(Serialize)]
struct StatusData {
    ticket: Ticket,
    staged: bool,
    suffix: Option<String>,
}

pub fn run(ctx: OutputCtx, workspace: Option<&Path>, ticket_arg: Option<&str>) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "status") {
        Ok(w) => w,
        Err(code) => return code,
    };
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "status", &e.to_string()),
    };
    let staged = match Staged::load(&state) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "status", &e.to_string()),
    };
    // If the caller didn't name a ticket, default to the staged one.
    let target = ticket_arg.map(String::from).or_else(|| staged.as_ref().map(|s| s.ticket.clone()));
    let Some(id) = target else {
        return emit_err(
            ctx,
            "status",
            ErrorCode::NotStaged,
            "no ticket given and nothing is staged",
            None,
        );
    };
    let t = match ticket::Ticket::load(&state, &id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return emit_err(
                ctx,
                "status",
                ErrorCode::TicketNotFound,
                &format!("ticket `{id}` not found"),
                Some(json!({ "ticket": id })),
            );
        }
        Err(e) => return emit_internal(ctx, "status", &e.to_string()),
    };
    let is_staged = staged.as_ref().map(|s| s.ticket == id).unwrap_or(false);
    let suffix = staged.as_ref().and_then(|s| s.suffix.clone());
    let branches_for_human: Vec<_> = t
        .branches
        .iter()
        .map(|b| {
            (
                b.repo.clone(),
                b.suffix.clone(),
                b.name.clone(),
                b.from.clone(),
            )
        })
        .collect();
    let id_for_human = t.id.clone();
    emit_ok(
        ctx,
        "status",
        StatusData {
            ticket: t,
            staged: is_staged,
            suffix,
        },
        || {
            eprintln!("ticket {id_for_human}{}", if is_staged { "  [STAGED]" } else { "" });
            for (repo, suffix, name, from) in &branches_for_human {
                let s = suffix
                    .as_deref()
                    .map(|s| format!(" @{s}"))
                    .unwrap_or_default();
                eprintln!("  {repo}{s}: {name} (from {from})");
            }
        },
    )
}
