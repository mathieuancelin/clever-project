//! Wrapper around the `clever` CLI.
//!
//! Every command is invoked as a child process. We always pass
//! `--format json` (or `-F json`) where the subcommand supports it and parse
//! stdout. Stderr is captured and surfaced on failure.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use serde::Deserialize;
use tracing::{debug, info};

use crate::model::Scalability;

pub fn ensure_available() -> Result<PathBuf> {
    which::which("clever")
        .map_err(|_| anyhow!("`clever` not found in PATH. Install it via `npm i -g clever-tools`."))
}

#[derive(Debug)]
pub struct Clever {
    program: PathBuf,
    dry_run: bool,
}

impl Clever {
    pub fn new() -> Result<Self> {
        Ok(Self {
            program: ensure_available()?,
            dry_run: false,
        })
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Run a `clever` subcommand and return its parsed JSON output.
    pub fn run_json(&self, args: &[&str]) -> Result<serde_json::Value> {
        let stdout = self.run_capture(args)?;
        serde_json::from_slice(&stdout)
            .with_context(|| format!("parsing JSON output of `clever {}`", args.join(" ")))
    }

    /// Run a `clever` subcommand for its side effect (no parsed output).
    /// stdout is logged at debug level, stderr only on failure.
    pub fn run(&self, args: &[&str]) -> Result<()> {
        self.run_capture(args).map(|_| ())
    }

    fn run_capture(&self, args: &[&str]) -> Result<Vec<u8>> {
        info!("clever {}", args.join(" "));
        let output = Command::new(&self.program)
            .args(args)
            .output()
            .with_context(|| format!("spawning `clever {}`", args.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!(
                "`clever {}` failed (status {}):\nstderr: {}\nstdout: {}",
                args.join(" "),
                output.status,
                stderr.trim(),
                stdout.trim()
            );
        }
        debug!("stdout: {}", String::from_utf8_lossy(&output.stdout).trim());
        Ok(output.stdout)
    }

    // -------- read side --------

    /// List all applications in an organisation.
    pub fn list_apps(&self, org: &str) -> Result<Vec<ListedApp>> {
        #[derive(Deserialize)]
        struct OrgWrapper {
            applications: Vec<ListedApp>,
        }
        let json = self.run_json(&["applications", "list", "--format", "json", "--org", org])?;
        let wrappers: Vec<OrgWrapper> =
            serde_json::from_value(json).context("decoding `applications list` output")?;
        Ok(wrappers.into_iter().flat_map(|w| w.applications).collect())
    }

    pub fn list_addons(&self, org: &str) -> Result<Vec<ListedAddon>> {
        let json = self.run_json(&["addon", "list", "--format", "json", "--org", org])?;
        serde_json::from_value(json).context("decoding `addon list` output")
    }

    pub fn get_env(&self, app: &str) -> Result<Vec<EnvVar>> {
        #[derive(Deserialize)]
        struct EnvOut {
            env: Vec<EnvVar>,
            #[allow(dead_code)]
            #[serde(default)]
            from_addons: serde_json::Value,
            #[allow(dead_code)]
            #[serde(default)]
            from_dependencies: serde_json::Value,
        }
        let json = self.run_json(&["env", "--app", app, "--format", "json"])?;
        let out: EnvOut = serde_json::from_value(json).context("decoding `env` output")?;
        Ok(out.env)
    }

    pub fn get_domains(&self, app: &str) -> Result<Vec<Domain>> {
        let json = self.run_json(&["domain", "--app", app, "--format", "json"])?;
        serde_json::from_value(json).context("decoding `domain` output")
    }

    pub fn get_services(&self, app: &str) -> Result<Services> {
        let json = self.run_json(&["service", "--app", app, "--format", "json"])?;
        serde_json::from_value(json).context("decoding `service` output")
    }

    /// List every add-on provider available to the given organisation,
    /// along with their plans and supported regions. Used to validate
    /// project-file addon specs (kind, size) before sending anything to
    /// Clever.
    pub fn list_addon_providers(&self, org: &str) -> Result<Vec<AddonProvider>> {
        let url = format!("https://api.clever-cloud.com/v2/products/addonproviders?orgaId={org}");
        let json = self.run_json(&["curl", &url])?;
        serde_json::from_value(json).context("decoding addon providers output")
    }

    /// List the application instance types available to the given org —
    /// one entry per `variant.slug` (`jar`, `node`, `static-apache`, ...)
    /// with the corresponding flavors. Used to validate project-file app
    /// scaling sizes against the live catalog.
    pub fn list_app_instances(&self, org: &str) -> Result<Vec<AppInstance>> {
        let url = format!("https://api.clever-cloud.com/v2/products/instances?for={org}");
        let json = self.run_json(&["curl", &url])?;
        serde_json::from_value(json).context("decoding app instances output")
    }

    // -------- write side --------

    /// Create an application. Returns the new app id (looked up by name after
    /// creation since `clever create`'s JSON output shape is not relied upon).
    pub fn create_app(&self, params: &CreateApp<'_>) -> Result<String> {
        if self.dry_run {
            info!(
                "[dry-run] would create app `{}` (type={}, region={}, github={:?})",
                params.name, params.kind, params.region, params.github
            );
            return Ok(dry_run_id("app", params.name));
        }

        let mut args: Vec<&str> = vec![
            "create",
            params.name,
            "--type",
            params.kind,
            "--org",
            params.org,
            "--region",
            params.region,
        ];
        if let Some(g) = params.github {
            args.push("--github");
            args.push(g);
        }
        self.run(&args)?;

        let apps = self.list_apps(params.org)?;
        apps.into_iter()
            .find(|a| a.name == params.name)
            .map(|a| a.app_id)
            .ok_or_else(|| anyhow!("app `{}` not found after creation", params.name))
    }

    pub fn delete_app(&self, app: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would delete app `{app}`");
            return Ok(());
        }
        self.run(&["delete", "--app", app, "--yes"])
    }

    /// Create an addon. Returns the new addon id (looked up by name).
    pub fn create_addon(&self, params: &CreateAddon<'_>) -> Result<String> {
        if self.dry_run {
            info!(
                "[dry-run] would create addon `{}` (provider={}, region={}, plan={:?}, version={:?}, crypted={})",
                params.name,
                params.provider,
                params.region,
                params.plan,
                params.version,
                params.crypted
            );
            return Ok(dry_run_id("addon", params.name));
        }

        let mut args: Vec<&str> = vec![
            "addon",
            "create",
            params.provider,
            params.name,
            "--org",
            params.org,
            "--region",
            params.region,
            "--yes",
        ];
        if let Some(p) = params.plan {
            args.push("--plan");
            args.push(p);
        }
        if let Some(v) = params.version {
            args.push("--addon-version");
            args.push(v);
        }
        // `crypted: true` -> encryption-at-rest. Option name is best-effort
        // and may need adjusting per provider.
        let opt = if params.crypted {
            Some("encryption=true".to_string())
        } else {
            None
        };
        if let Some(ref o) = opt {
            args.push("--option");
            args.push(o);
        }

        self.run(&args)?;

        let addons = self.list_addons(params.org)?;
        addons
            .into_iter()
            .find(|a| a.name == params.name)
            .map(|a| a.addon_id)
            .ok_or_else(|| anyhow!("addon `{}` not found after creation", params.name))
    }

    pub fn delete_addon(&self, addon: &str, org: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would delete addon `{addon}`");
            return Ok(());
        }
        self.run(&["addon", "delete", addon, "--org", org, "--yes"])
    }

    /// Replace the entire environment for an app. Uses `clever env import
    /// --json` which deletes any pre-existing variables.
    pub fn env_replace(&self, app: &str, env: &IndexMap<String, String>) -> Result<()> {
        if self.dry_run {
            let keys: Vec<&str> = env.keys().map(String::as_str).collect();
            info!(
                "[dry-run] would replace env on `{app}` ({} vars: {})",
                env.len(),
                keys.join(", ")
            );
            return Ok(());
        }

        #[derive(serde::Serialize)]
        struct Pair<'a> {
            name: &'a str,
            value: &'a str,
        }
        let payload: Vec<Pair<'_>> = env
            .iter()
            .map(|(k, v)| Pair { name: k, value: v })
            .collect();
        let body = serde_json::to_vec(&payload).context("serializing env payload")?;

        info!(
            "clever env import --app {} --json (replace, {} vars)",
            app,
            env.len()
        );
        let mut child = Command::new(&self.program)
            .args(["env", "import", "--app", app, "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawning `clever env import`")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("no stdin handle on `clever env import`"))?
            .write_all(&body)
            .context("writing env payload to `clever env import` stdin")?;
        let output = child
            .wait_with_output()
            .context("waiting for `clever env import`")?;
        if !output.status.success() {
            bail!(
                "`clever env import` failed (status {}):\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub fn domain_add(&self, app: &str, hostname: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would add domain `{hostname}` to `{app}`");
            return Ok(());
        }
        self.run(&["domain", "add", hostname, "--app", app])
    }

    pub fn domain_rm(&self, app: &str, hostname: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would remove domain `{hostname}` from `{app}`");
            return Ok(());
        }
        self.run(&["domain", "rm", hostname, "--app", app])
    }

    pub fn scale(&self, app: &str, scalability: &Scalability) -> Result<()> {
        if self.dry_run {
            info!(
                "[dry-run] would scale `{app}` (auto={}, instances={:?})",
                scalability.auto, scalability.instances
            );
            return Ok(());
        }
        let mut args: Vec<String> = vec!["scale".into(), "--app".into(), app.into()];
        let inst = scalability.instances.as_ref();
        if scalability.auto {
            if let Some(n) = inst.and_then(|i| i.min_number) {
                args.push("--min-instances".into());
                args.push(n.to_string());
            }
            if let Some(n) = inst
                .and_then(|i| i.max_number)
                .or_else(|| inst.and_then(|i| i.min_number))
            {
                args.push("--max-instances".into());
                args.push(n.to_string());
            }
            if let Some(s) = inst.and_then(|i| i.min_size.clone()) {
                args.push("--min-flavor".into());
                args.push(s);
            }
            if let Some(s) = inst
                .and_then(|i| i.max_size.clone())
                .or_else(|| inst.and_then(|i| i.min_size.clone()))
            {
                args.push("--max-flavor".into());
                args.push(s);
            }
        } else {
            let n = inst.and_then(|i| i.min_number).unwrap_or(1);
            args.push("--instances".into());
            args.push(n.to_string());
            if let Some(s) = inst.and_then(|i| i.min_size.clone()) {
                args.push("--flavor".into());
                args.push(s);
            }
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&refs)
    }

    /// Restart (or kick off the first deployment of) an application.
    /// Uses `--quiet` so we don't pollute the output with deployment logs.
    pub fn restart(&self, app: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would restart app `{app}`");
            return Ok(());
        }
        self.run(&["restart", "--app", app, "--quiet"])
    }

    pub fn link_addon(&self, app: &str, addon: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would link addon `{addon}` to `{app}`");
            return Ok(());
        }
        self.run(&["service", "link-addon", addon, "--app", app])
    }

    pub fn unlink_addon(&self, app: &str, addon: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would unlink addon `{addon}` from `{app}`");
            return Ok(());
        }
        self.run(&["service", "unlink-addon", addon, "--app", app])
    }

    pub fn link_app(&self, app: &str, dep: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would link app `{dep}` to `{app}`");
            return Ok(());
        }
        self.run(&["service", "link-app", dep, "--app", app])
    }

    pub fn unlink_app(&self, app: &str, dep: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would unlink app `{dep}` from `{app}`");
            return Ok(());
        }
        self.run(&["service", "unlink-app", dep, "--app", app])
    }
}

/// Synthetic id used in dry-run mode so dependency resolution still has
/// something stable to reference. Never sent to `clever`.
fn dry_run_id(kind: &str, name: &str) -> String {
    format!("dry-run::{kind}::{name}")
}

#[derive(Debug)]
pub struct CreateApp<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub org: &'a str,
    pub region: &'a str,
    pub github: Option<&'a str>,
}

#[derive(Debug)]
pub struct CreateAddon<'a> {
    pub provider: &'a str,
    pub name: &'a str,
    pub org: &'a str,
    pub region: &'a str,
    pub plan: Option<&'a str>,
    pub version: Option<&'a str>,
    pub crypted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListedApp {
    #[serde(rename = "app_id")]
    pub app_id: String,
    #[serde(rename = "org_id")]
    #[allow(dead_code)]
    pub org_id: String,
    pub name: String,
    /// Clever region (e.g. `par`).
    pub zone: String,
    /// Instance kind (e.g. `node`, `jar`).
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub deploy_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListedAddon {
    #[serde(rename = "addonId")]
    pub addon_id: String,
    pub name: String,
    #[serde(rename = "planName")]
    #[allow(dead_code)]
    pub plan_name: String,
    #[serde(rename = "planSlug")]
    pub plan_slug: String,
    /// e.g. `postgresql-addon`, `redis-addon`.
    #[serde(rename = "providerId")]
    pub provider_id: String,
    pub region: String,
    /// Human-readable provider name (e.g. `PostgreSQL`).
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Domain {
    pub hostname: String,
    #[serde(rename = "isFavourite", default)]
    #[allow(dead_code)]
    pub is_favourite: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Services {
    #[serde(default)]
    pub applications: Vec<ServiceRef>,
    #[serde(default)]
    pub addons: Vec<ServiceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceRef {
    pub id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddonProvider {
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub regions: Vec<String>,
    #[serde(default)]
    pub plans: Vec<AddonPlan>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddonPlan {
    pub slug: String,
    #[allow(dead_code)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppInstance {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    pub type_: String,
    pub variant: AppInstanceVariant,
    #[serde(default)]
    pub flavors: Vec<AppFlavor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppInstanceVariant {
    pub slug: String,
    #[allow(dead_code)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppFlavor {
    pub name: String,
}
