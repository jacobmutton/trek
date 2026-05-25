use std::process::ExitCode;

mod audit;
mod baseline;
mod cli;
mod commands;
mod config;
mod error;
mod git;
mod hooks;
mod lock;
mod naming;
mod output;
mod preflight;
mod staged;
mod state;
mod ticket;
mod workspace;

fn main() -> ExitCode {
    cli::run()
}
