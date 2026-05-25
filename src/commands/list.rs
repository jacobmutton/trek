use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use crate::commands::{emit_internal, require_workspace};
use crate::output::{OutputCtx, emit_ok};
use crate::staged::Staged;
use crate::state::StateRoot;
use crate::ticket;

#[derive(Serialize)]
struct ListData<'a> {
    staged: Option<&'a str>,
    tickets: Vec<TicketSummary>,
}

#[derive(Serialize)]
struct TicketSummary {
    id: String,
    branches: usize,
    repos: Vec<String>,
}

pub fn run(ctx: OutputCtx, workspace: Option<&Path>) -> ExitCode {
    let ws = match require_workspace(workspace, ctx, "list") {
        Ok(w) => w,
        Err(code) => return code,
    };
    let state = match StateRoot::for_workspace(ws.config.workspace.id) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "list", &e.to_string()),
    };
    let tickets = match ticket::list_all(&state) {
        Ok(t) => t,
        Err(e) => return emit_internal(ctx, "list", &e.to_string()),
    };
    let staged = match Staged::load(&state) {
        Ok(s) => s,
        Err(e) => return emit_internal(ctx, "list", &e.to_string()),
    };
    let staged_id = staged.as_ref().map(|s| s.ticket.clone());
    let summaries: Vec<TicketSummary> = tickets
        .iter()
        .map(|t| {
            let mut repos: Vec<String> = t
                .branches
                .iter()
                .map(|b| b.repo.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            repos.sort();
            TicketSummary {
                id: t.id.clone(),
                branches: t.branches.len(),
                repos,
            }
        })
        .collect();

    let summaries_for_human = summaries
        .iter()
        .map(|s| (s.id.clone(), s.branches, s.repos.join(",")))
        .collect::<Vec<_>>();
    let staged_for_human = staged_id.clone();

    emit_ok(
        ctx,
        "list",
        ListData {
            staged: staged_id.as_deref(),
            tickets: summaries,
        },
        || {
            if let Some(s) = &staged_for_human {
                eprintln!("staged: {s}");
            } else {
                eprintln!("staged: (none)");
            }
            if summaries_for_human.is_empty() {
                eprintln!("no tickets tracked");
                return;
            }
            for (id, branches, repos) in &summaries_for_human {
                eprintln!("  {id}  ({branches} branch(es), repos: {repos})");
            }
        },
    )
}
