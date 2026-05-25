use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;

use crate::commands;
use crate::output::OutputCtx;

#[derive(Parser)]
#[command(
    name = "trek",
    version,
    about = "Multi-repo branch + worktree coordinator for ticket-driven work"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Emit machine-readable JSON on stdout (human output goes to stderr).
    #[arg(long, global = true)]
    pub json: bool,

    /// Refuse to prompt; exit non-zero where a prompt would otherwise appear.
    #[arg(long, global = true)]
    pub non_interactive: bool,

    /// Path to the workspace dir (the dir containing trek.toml).
    #[arg(long, global = true, env = "TREK_WORKSPACE")]
    pub workspace: Option<PathBuf>,

    #[arg(long, global = true)]
    pub quiet: bool,

    #[arg(long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Scaffold trek.toml + workspace UUID in the current directory.
    Init,

    /// Fetch + ff-pull every preprod checkout back to baseline.
    Refresh {
        #[arg(long, value_delimiter = ',')]
        repos: Option<Vec<String>>,
    },

    /// Create branch + worktree per repo for a ticket.
    Start {
        ticket: String,
        #[arg(long, value_delimiter = ',', required = true)]
        repos: Vec<String>,
        #[arg(long)]
        suffix: Option<String>,
        #[arg(long)]
        from: Option<String>,
    },

    /// Like `start`, but reuse a pre-existing branch.
    Adopt {
        ticket: String,
        #[arg(long, value_delimiter = ',', required = true)]
        repos: Vec<String>,
        #[arg(long)]
        suffix: Option<String>,
        #[arg(long)]
        from: Option<String>,
    },

    /// Switch preprod checkouts to a ticket's branches.
    Stage {
        ticket: String,
        #[arg(long)]
        suffix: Option<String>,
        #[arg(long)]
        keep_others: bool,
    },

    /// Restore preprod to the staging snapshot or to baseline.
    Unstage {
        #[arg(long)]
        to_baseline: bool,
    },

    /// Unstage (if needed) + remove all worktrees and branches for a ticket.
    Cleanup {
        ticket: String,
        #[arg(long)]
        keep_merged: bool,
    },

    /// State of a ticket, or the currently-staged ticket if omitted.
    Status { ticket: Option<String> },

    /// All tickets currently tracked in the workspace.
    List,

    /// Print the absolute path to a (ticket, repo) worktree or preprod checkout.
    Path {
        ticket: String,
        repo: String,
        #[arg(long)]
        suffix: Option<String>,
        /// Print the preprod checkout path instead of the worktree.
        #[arg(long)]
        preprod: bool,
    },

    /// Run a command in each repo's worktree or preprod for a ticket.
    Run {
        #[arg(long = "in", value_enum)]
        location: RunLocation,
        #[arg(short = 't', long)]
        ticket: String,
        #[arg(long)]
        suffix: Option<String>,
        #[arg(long, value_delimiter = ',')]
        repos: Option<Vec<String>>,
        #[arg(long)]
        parallel: bool,
        /// Command to run after `--`.
        #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },

    /// Report the workspace and ticket the current directory belongs to.
    Where,
}

#[derive(Copy, Clone, ValueEnum)]
pub enum RunLocation {
    Worktree,
    Preprod,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let ctx = OutputCtx {
        json: cli.json,
        quiet: cli.quiet,
        verbose: cli.verbose,
    };
    let ws = cli.workspace.as_deref();
    let argv: Vec<String> = std::env::args().collect();
    let non_interactive = cli.non_interactive;
    match cli.command {
        Command::Init => commands::init::run(ctx),
        Command::Where => commands::r#where::run(ctx, ws),
        Command::List => commands::list::run(ctx, ws),
        Command::Status { ticket } => commands::status::run(ctx, ws, ticket.as_deref()),
        Command::Path {
            ticket,
            repo,
            suffix,
            preprod,
        } => commands::path::run(ctx, ws, &ticket, &repo, suffix.as_deref(), preprod),
        Command::Start {
            ticket,
            repos,
            suffix,
            from,
        } => commands::start::run(
            ctx,
            ws,
            commands::start::Mode::Start,
            &ticket,
            &repos,
            suffix.as_deref(),
            from.as_deref(),
            non_interactive,
            &argv,
        ),
        Command::Adopt {
            ticket,
            repos,
            suffix,
            from,
        } => commands::start::run(
            ctx,
            ws,
            commands::start::Mode::Adopt,
            &ticket,
            &repos,
            suffix.as_deref(),
            from.as_deref(),
            non_interactive,
            &argv,
        ),
        Command::Refresh { repos } => {
            commands::refresh::run(ctx, ws, repos.as_deref(), non_interactive, &argv)
        }
        Command::Stage {
            ticket,
            suffix,
            keep_others,
        } => commands::stage::run(
            ctx,
            ws,
            &ticket,
            suffix.as_deref(),
            keep_others,
            non_interactive,
            &argv,
        ),
        Command::Unstage { to_baseline } => {
            commands::unstage::run(ctx, ws, to_baseline, non_interactive, &argv)
        }
        Command::Cleanup {
            ticket,
            keep_merged,
        } => commands::cleanup::run(ctx, ws, &ticket, keep_merged, non_interactive, &argv),
        Command::Run {
            location,
            ticket,
            suffix,
            repos,
            parallel,
            cmd,
        } => commands::run::run(
            ctx,
            ws,
            location,
            &ticket,
            suffix.as_deref(),
            repos.as_deref(),
            parallel,
            &cmd,
        ),
    }
}
