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
    /// Validate a project file (syntax, variables, dependencies, sizing,
    /// kinds, regions). Doesn't modify anything.
    Check(CheckArgs),
    /// Compare a project file against the live Clever Cloud org and report
    /// any drift. Read-only; doesn't modify anything.
    Status(StatusArgs),
    /// Scaffold a new project file by asking a few questions (or via flags
    /// in non-interactive mode).
    Init(InitArgs),
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
    /// Project file path (.yaml/.yml/.json). If omitted, looks for
    /// `project.clever.yaml`, `.yml` or `.json` in the current directory.
    pub file: Option<PathBuf>,

    /// Override the organisation defined in the project file
    #[arg(long)]
    pub org: Option<String>,

    /// Override the default region defined in the project file
    #[arg(long)]
    pub region: Option<String>,

    /// Value for the special variable `${env}` (default `prod`)
    #[arg(long)]
    pub env: Option<String>,

    /// Set a variable (key=value). Overrides values from the project file
    /// and from --variable-path.
    #[arg(long = "variable", value_parser = parse_kv)]
    pub variables: Vec<(String, String)>,

    /// Load variable overrides from a YAML/JSON file (flat key/value
    /// mapping). Can be repeated; later files override earlier ones, and
    /// --variable beats anything from these files.
    #[arg(long = "variable-path")]
    pub variable_paths: Vec<PathBuf>,

    /// Explicit path to a secrets file. When omitted, secrets are
    /// auto-discovered next to the project file (`<stem>.secrets` and
    /// `<stem>.<env>.secrets`).
    #[arg(long)]
    pub secrets_path: Option<PathBuf>,

    /// Plan only: read current state and log what would change without
    /// mutating anything on Clever Cloud.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive confirmation prompt. Required when stdin is not
    /// a TTY (CI environments, piped invocations).
    #[arg(long, alias = "auto-approve")]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Project file path (.yaml/.yml/.json). If omitted, looks for
    /// `project.clever.yaml`, `.yml` or `.json` in the current directory.
    pub file: Option<PathBuf>,

    /// Override the organisation defined in the project file
    #[arg(long)]
    pub org: Option<String>,

    /// Override the default region defined in the project file
    #[arg(long)]
    pub region: Option<String>,

    /// Value for the special variable `${env}` (default `prod`)
    #[arg(long)]
    pub env: Option<String>,

    /// Set a variable (key=value). Overrides values from the project file
    /// and from --variable-path.
    #[arg(long = "variable", value_parser = parse_kv)]
    pub variables: Vec<(String, String)>,

    /// Load variable overrides from a YAML/JSON file (repeatable).
    #[arg(long = "variable-path")]
    pub variable_paths: Vec<PathBuf>,

    /// Explicit path to a secrets file.
    #[arg(long)]
    pub secrets_path: Option<PathBuf>,

    /// Skip live validation against Clever's API (addon catalog, app
    /// instance flavors). Useful in CI environments without `clever login`.
    /// Static validation (syntax, variables, kinds, regions, dependencies,
    /// uniqueness) is always performed.
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Project file path (.yaml/.yml/.json). If omitted, looks for
    /// `project.clever.yaml`, `.yml` or `.json` in the current directory.
    pub file: Option<PathBuf>,

    /// Override the organisation defined in the project file
    #[arg(long)]
    pub org: Option<String>,

    /// Override the default region defined in the project file
    #[arg(long)]
    pub region: Option<String>,

    /// Value for the special variable `${env}` (default `prod`)
    #[arg(long)]
    pub env: Option<String>,

    /// Set a variable (key=value). Overrides values from the project file
    /// and from --variable-path.
    #[arg(long = "variable", value_parser = parse_kv)]
    pub variables: Vec<(String, String)>,

    /// Load variable overrides from a YAML/JSON file (repeatable).
    #[arg(long = "variable-path")]
    pub variable_paths: Vec<PathBuf>,

    /// Explicit path to a secrets file.
    #[arg(long)]
    pub secrets_path: Option<PathBuf>,

    /// Hide resources that are perfectly in sync; only show drift.
    #[arg(long)]
    pub brief: bool,

    /// Exit with code 1 if any drift, orphan, or pending creation is found.
    /// Useful in CI checks.
    #[arg(long)]
    pub exit_on_drift: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Project file path (.yaml/.yml/.json). If omitted, looks for
    /// `project.clever.yaml`, `.yml` or `.json` in the current directory.
    pub file: Option<PathBuf>,

    /// Override the organisation defined in the project file
    #[arg(long)]
    pub org: Option<String>,

    /// Override the default region defined in the project file
    #[arg(long)]
    pub region: Option<String>,

    /// Value for the special variable `${env}` (default `prod`)
    #[arg(long)]
    pub env: Option<String>,

    /// Set a variable (key=value). Overrides values from the project file
    /// and from --variable-path.
    #[arg(long = "variable", value_parser = parse_kv)]
    pub variables: Vec<(String, String)>,

    /// Load variable overrides from a YAML/JSON file (flat key/value
    /// mapping). Can be repeated; later files override earlier ones, and
    /// --variable beats anything from these files.
    #[arg(long = "variable-path")]
    pub variable_paths: Vec<PathBuf>,

    /// Explicit path to a secrets file. When omitted, secrets are
    /// auto-discovered next to the project file (`<stem>.secrets` and
    /// `<stem>.<env>.secrets`).
    #[arg(long)]
    pub secrets_path: Option<PathBuf>,

    /// Plan only: log what would be deleted without mutating anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive confirmation prompt. Required when stdin is not
    /// a TTY (CI environments, piped invocations).
    #[arg(long, alias = "auto-approve")]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Project name (free-form). Asked interactively if omitted.
    #[arg(long)]
    pub name: Option<String>,

    /// Clever Cloud organisation id (`orga_xxx`).
    #[arg(long)]
    pub org: Option<String>,

    /// Default region. Defaults to `par`.
    #[arg(long)]
    pub region: Option<String>,

    /// App kind (`node`, `docker`, `python`, `jar`, ...).
    #[arg(long)]
    pub kind: Option<String>,

    /// GitHub source. Accepts `owner/repo`, `github.com/owner/repo`, or a
    /// full URL. Omit (or pass `--no-source` in non-interactive mode) for
    /// no source.
    #[arg(long)]
    pub source: Option<String>,

    /// Explicitly create the project with no source, even in interactive
    /// mode. Useful for `--non-interactive` runs.
    #[arg(long, conflicts_with = "source")]
    pub no_source: bool,

    /// Addon kind to provision alongside the app (repeatable). E.g.
    /// `--addon postgresql --addon redis`.
    #[arg(long = "addon")]
    pub addons: Vec<String>,

    /// Output path. Default `project.clever.yaml`.
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Don't prompt for anything. Fail if a required field wasn't passed.
    #[arg(long)]
    pub non_interactive: bool,

    /// Overwrite the output file if it already exists.
    #[arg(long)]
    pub force: bool,
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
