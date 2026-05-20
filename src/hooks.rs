//! Pre / post hooks for `apply` and `delete`. Hooks are shell commands
//! declared in the project file (project-wide and per-app); they let users
//! orchestrate external steps (builds, migrations, notifications) without a
//! custom wrapper.
//!
//! Pre-hook failure aborts the run before any mutation. Post-hook failure
//! exits non-zero but doesn't undo what already happened (no rollback).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Pre,
    Post,
}

impl HookPhase {
    fn as_str(self) -> &'static str {
        match self {
            HookPhase::Pre => "pre",
            HookPhase::Post => "post",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOperation {
    Apply,
    Delete,
}

impl HookOperation {
    fn as_str(self) -> &'static str {
        match self {
            HookOperation::Apply => "apply",
            HookOperation::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HookContext<'a> {
    pub operation: HookOperation,
    pub phase: HookPhase,
    pub project_path: &'a Path,
    pub org: &'a str,
    pub region: &'a str,
    pub env: &'a str,
    pub app: Option<HookAppContext<'a>>,
}

#[derive(Debug, Clone)]
pub struct HookAppContext<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
}

/// Run a hook command. Skipped (with a log line) when `dry_run` or `skip`
/// are true. Stdout/stderr are inherited so the user sees output live.
pub fn run_hook(command: &str, ctx: &HookContext, dry_run: bool, skip: bool) -> Result<()> {
    let label = hook_label(ctx);
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if dry_run {
        info!("[dry-run] would run {label}: `{trimmed}`");
        return Ok(());
    }
    if skip {
        warn!("--skip-hooks set, skipping {label}: `{trimmed}`");
        return Ok(());
    }
    info!("running {label}: `{trimmed}`");

    let cwd = ctx
        .project_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let mut cmd = spawn_command(trimmed);
    cmd.current_dir(&cwd);
    populate_env(&mut cmd, ctx);

    let status = cmd
        .status()
        .with_context(|| format!("spawning hook for {label}: `{trimmed}`"))?;
    if !status.success() {
        bail!(
            "hook {label} `{trimmed}` failed (exit {})",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }
    Ok(())
}

fn hook_label(ctx: &HookContext) -> String {
    match &ctx.app {
        Some(a) => format!(
            "{}_{} hook for app `{}`",
            ctx.phase.as_str(),
            ctx.operation.as_str(),
            a.key
        ),
        None => format!(
            "project {}_{} hook",
            ctx.phase.as_str(),
            ctx.operation.as_str()
        ),
    }
}

#[cfg(not(windows))]
fn spawn_command(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

#[cfg(windows)]
fn spawn_command(cmd: &str) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(cmd);
    c
}

fn populate_env(cmd: &mut Command, ctx: &HookContext) {
    cmd.env("CLEVER_PROJECT_FILE", ctx.project_path);
    cmd.env("CLEVER_PROJECT_ORG", ctx.org);
    cmd.env("CLEVER_PROJECT_REGION", ctx.region);
    cmd.env("CLEVER_PROJECT_ENV", ctx.env);
    cmd.env("CLEVER_PROJECT_OPERATION", ctx.operation.as_str());
    cmd.env("CLEVER_PROJECT_PHASE", ctx.phase.as_str());
    if let Some(app) = &ctx.app {
        cmd.env("CLEVER_PROJECT_APP_KEY", app.key);
        cmd.env("CLEVER_PROJECT_APP_NAME", app.name);
        cmd.env("CLEVER_PROJECT_APP_KIND", app.kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(project_path: &'a Path, app: Option<HookAppContext<'a>>) -> HookContext<'a> {
        HookContext {
            operation: HookOperation::Apply,
            phase: HookPhase::Pre,
            project_path,
            org: "orga_x",
            region: "par",
            env: "prod",
            app,
        }
    }

    #[test]
    fn empty_command_is_a_noop() {
        let p = std::path::PathBuf::from("/tmp/proj.yaml");
        run_hook("", &ctx(&p, None), false, false).unwrap();
        run_hook("   \t  ", &ctx(&p, None), false, false).unwrap();
    }

    #[test]
    fn dry_run_does_not_execute() {
        let p = std::path::PathBuf::from("/tmp/proj.yaml");
        run_hook("exit 1", &ctx(&p, None), true, false).unwrap();
    }

    #[test]
    fn skip_flag_does_not_execute() {
        let p = std::path::PathBuf::from("/tmp/proj.yaml");
        run_hook("exit 1", &ctx(&p, None), false, true).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn successful_command_returns_ok() {
        let p = std::path::PathBuf::from("/tmp/proj.yaml");
        run_hook("true", &ctx(&p, None), false, false).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn failed_command_bails() {
        let p = std::path::PathBuf::from("/tmp/proj.yaml");
        let err = run_hook("false", &ctx(&p, None), false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("failed"), "got: {msg}");
    }

    #[cfg(not(windows))]
    #[test]
    fn env_vars_are_exposed_to_the_command() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};
        let mut out = std::env::temp_dir();
        out.push(format!(
            "clever-project-hook-env-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = std::path::PathBuf::from("/tmp/proj.yaml");
        let app = HookAppContext {
            key: "api",
            name: "prod-api",
            kind: "node",
        };
        let cmd = format!(
            "printf '%s|%s|%s|%s|%s|%s\\n' \"$CLEVER_PROJECT_OPERATION\" \"$CLEVER_PROJECT_PHASE\" \"$CLEVER_PROJECT_ORG\" \"$CLEVER_PROJECT_APP_KEY\" \"$CLEVER_PROJECT_APP_NAME\" \"$CLEVER_PROJECT_APP_KIND\" > {}",
            out.display()
        );
        run_hook(&cmd, &ctx(&p, Some(app)), false, false).unwrap();
        let body = fs::read_to_string(&out).unwrap();
        assert_eq!(body.trim(), "apply|pre|orga_x|api|prod-api|node");
        fs::remove_file(&out).ok();
    }

    #[cfg(not(windows))]
    #[test]
    fn cwd_is_project_directory() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "clever-project-hook-cwd-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let project = dir.join("project.clever.yaml");
        let marker = dir.join("marker.txt");
        let cmd = format!("pwd > {}", marker.display());
        run_hook(&cmd, &ctx(&project, None), false, false).unwrap();
        let pwd = fs::read_to_string(&marker).unwrap();
        // macOS' /tmp is a symlink to /private/tmp — accept either prefix.
        assert!(
            pwd.trim() == dir.to_string_lossy()
                || pwd
                    .trim()
                    .ends_with(dir.file_name().unwrap().to_str().unwrap()),
            "got pwd `{}`, expected something matching `{}`",
            pwd.trim(),
            dir.display()
        );
        fs::remove_dir_all(&dir).ok();
    }
}
