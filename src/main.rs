mod clever;
mod cli;
mod commands;
mod interpolate;
mod model;
mod state;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let args = cli::Cli::parse();
    init_tracing(args.verbose);

    match args.command {
        cli::Command::Read(args) => commands::read::run(args),
        cli::Command::Apply(args) => commands::apply::run(args),
        cli::Command::Delete(args) => commands::delete::run(args),
        cli::Command::Check(args) => commands::check::run(args),
    }
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose { "debug" } else { "info" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}
