mod clever;
mod cli;
mod commands;
mod interpolate;
mod issues;
mod lock;
mod model;
mod state;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let args = cli::Cli::parse();
    let format = command_format(&args.command);
    init_tracing(args.verbose, format);

    match args.command {
        cli::Command::Read(args) => commands::read::run(args),
        cli::Command::Apply(args) => commands::apply::run(args),
        cli::Command::Delete(args) => commands::delete::run(args),
        cli::Command::Check(args) => commands::check::run(args),
        cli::Command::Status(args) => commands::status::run(args),
        cli::Command::Init(args) => commands::init::run(args),
        cli::Command::Unlock(args) => commands::unlock::run(args),
        cli::Command::Completions(args) => commands::completions::run(args),
    }
}

fn command_format(cmd: &cli::Command) -> cli::OutputFormat {
    match cmd {
        cli::Command::Read(a) => a.format,
        cli::Command::Apply(a) => a.format,
        cli::Command::Delete(a) => a.format,
        cli::Command::Check(a) => a.format,
        cli::Command::Status(a) => a.format,
        cli::Command::Init(a) => a.format,
        cli::Command::Unlock(a) => a.format,
        // The completions command writes a raw shell script to stdout; keep
        // logs quiet by treating it as JSON-mode (warn level only).
        cli::Command::Completions(_) => cli::OutputFormat::Json,
    }
}

fn init_tracing(verbose: bool, format: cli::OutputFormat) {
    // In JSON mode, suppress info logs by default so they don't compete with
    // the structured payload on stdout. `RUST_LOG` still overrides if the
    // user wants traces for debugging.
    let default_level = if verbose {
        "debug"
    } else if format.is_json() {
        "warn"
    } else {
        "info"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    // Always emit logs to stderr so the data stream on stdout stays clean
    // (clean piping into `jq` and friends).
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();
}
