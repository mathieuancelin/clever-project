use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "clever-project", version, about = "Sync a project description with Clever Cloud", long_about = None)]
pub struct Cli {
    /// Verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read resources from a Clever Cloud org into a project file
    Read(ReadArgs),
    /// Create or update resources from a project file
    Apply(ApplyArgs),
    /// Delete resources listed in a project file
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// Target organisation (overrides project file)
    #[arg(long)]
    pub org: String,

    /// App name to read (can be repeated)
    #[arg(long = "app")]
    pub apps: Vec<String>,

    /// Addon name to read (can be repeated)
    #[arg(long = "addon")]
    pub addons: Vec<String>,

    /// Read every app and addon in the org
    #[arg(long, conflicts_with_all = ["apps", "addons"])]
    pub all: bool,

    /// Output file path (.yaml/.yml/.json)
    #[arg(short = 'o', long)]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// Project file path (.yaml/.yml/.json)
    pub file: PathBuf,

    /// Override the organisation defined in the project file
    #[arg(long)]
    pub org: Option<String>,

    /// Override the default region defined in the project file
    #[arg(long)]
    pub region: Option<String>,

    /// Value for the special variable `${env}` (default `prod`)
    #[arg(long)]
    pub env: Option<String>,

    /// Set a variable (key=value). Overrides values from the project file.
    #[arg(long = "variable", value_parser = parse_kv)]
    pub variables: Vec<(String, String)>,

    /// Plan only: read current state and log what would change without
    /// mutating anything on Clever Cloud.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Project file path (.yaml/.yml/.json)
    pub file: PathBuf,

    /// Override the organisation defined in the project file
    #[arg(long)]
    pub org: Option<String>,

    /// Override the default region defined in the project file
    #[arg(long)]
    pub region: Option<String>,

    /// Value for the special variable `${env}` (default `prod`)
    #[arg(long)]
    pub env: Option<String>,

    /// Set a variable (key=value). Overrides values from the project file.
    #[arg(long = "variable", value_parser = parse_kv)]
    pub variables: Vec<(String, String)>,

    /// Plan only: log what would be deleted without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected key=value, got `{s}`"))?;
    if k.is_empty() {
        return Err(format!("empty key in `{s}`"));
    }
    Ok((k.to_string(), v.to_string()))
}
