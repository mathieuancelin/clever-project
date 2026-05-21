//! Wrapper around the `clever` CLI.
//!
//! Every command is invoked as a child process. We always pass
//! `--format json` (or `-F json`) where the subcommand supports it and parse
//! stdout. Stderr is captured and surfaced on failure.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::model::{Instances, Scalability};

pub fn ensure_available() -> Result<PathBuf> {
    which::which("clever")
        .map_err(|_| anyhow!("`clever` not found in PATH. Install it via `npm i -g clever-tools`."))
}

const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_BACKOFF_BASE_MS: u64 = 500;

#[derive(Debug)]
pub struct Clever {
    program: PathBuf,
    dry_run: bool,
    max_attempts: u32,
    backoff_base_ms: u64,
}

impl Clever {
    pub fn new() -> Result<Self> {
        Ok(Self {
            program: ensure_available()?,
            dry_run: false,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
        })
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    #[allow(dead_code)]
    pub fn with_retry(mut self, max_attempts: u32, backoff_base_ms: u64) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.backoff_base_ms = backoff_base_ms;
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
        let label = args.join(" ");
        self.retry(&label, || self.run_capture_once(args))
    }

    fn run_capture_once(&self, args: &[&str]) -> Result<Vec<u8>> {
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

    fn run_with_stdin(&self, args: &[&str], stdin_payload: &[u8]) -> Result<Vec<u8>> {
        let label = args.join(" ");
        self.retry(&label, || self.run_with_stdin_once(args, stdin_payload))
    }

    fn run_with_stdin_once(&self, args: &[&str], stdin_payload: &[u8]) -> Result<Vec<u8>> {
        info!("clever {}", args.join(" "));
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning `clever {}`", args.join(" ")))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("no stdin handle on `clever {}`", args.join(" ")))?
            .write_all(stdin_payload)
            .with_context(|| format!("writing stdin to `clever {}`", args.join(" ")))?;
        let output = child
            .wait_with_output()
            .with_context(|| format!("waiting for `clever {}`", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "`clever {}` failed (status {}):\nstderr: {}\nstdout: {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
        Ok(output.stdout)
    }

    fn retry<F, T>(&self, label: &str, mut op: F) -> Result<T>
    where
        F: FnMut() -> Result<T>,
    {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match op() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if attempt >= self.max_attempts || !is_transient_error(&msg) {
                        return Err(e);
                    }
                    let delay = backoff_delay(self.backoff_base_ms, attempt);
                    warn!(
                        "transient error on `clever {label}` (attempt {attempt}/{}): {e:#} — retrying in {:?}",
                        self.max_attempts, delay
                    );
                    std::thread::sleep(delay);
                }
            }
        }
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
        let out = self.get_env_full(app)?;
        Ok(out.env)
    }

    /// Like `get_env` but also returns the addon- and dependency-injected
    /// vars that Clever merges into the app's runtime env (so callers can
    /// resolve cross-refs like `${apps.X.env.POSTGRESQL_ADDON_HOST}` where
    /// the var is auto-populated by a linked addon).
    pub fn get_env_full(&self, app: &str) -> Result<EnvFull> {
        let json = self.run_json(&["env", "--app", app, "--format", "json"])?;
        serde_json::from_value(json).context("decoding `env` output")
    }

    /// Fetch the env vars an addon would inject into any linked app
    /// (POSTGRESQL_ADDON_HOST, REDIS_HOST, etc.). Uses the v2 API endpoint
    /// rather than `clever addon env` because the latter does not return
    /// structured JSON we can rely on.
    pub fn get_addon_env(&self, org: &str, addon_id: &str) -> Result<IndexMap<String, String>> {
        let url =
            format!("https://api.clever-cloud.com/v2/organisations/{org}/addons/{addon_id}/env");
        let json = self.run_json(&["curl", &url])?;
        let entries: Vec<EnvVar> =
            serde_json::from_value(json).context("decoding addon env output")?;
        Ok(entries.into_iter().map(|v| (v.name, v.value)).collect())
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

    /// Fetch the live env vars, domains, scalability and build config of an
    /// application in a single request. Replaces the env + domain + scale
    /// trio for `read` and `status` (apply keeps its independent calls so it
    /// can diff env/domains against the very latest state right before a
    /// mutation).
    pub fn get_app_details(&self, org: &str, app_id: &str) -> Result<AppDetailsView> {
        let url =
            format!("https://api.clever-cloud.com/v2/organisations/{org}/applications/{app_id}");
        let json = self.run_json(&["curl", &url])?;
        let details: AppDetails =
            serde_json::from_value(json).context("decoding application details output")?;
        Ok(app_details_view_from(details))
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
        // A name conflict here means either a true race, or a retry whose
        // previous attempt succeeded server-side but lost the response. In
        // both cases the resource exists and the lookup below resolves it.
        if let Err(e) = self.run(&args) {
            if !is_name_conflict(&format!("{e:#}")) {
                return Err(e);
            }
            warn!(
                "`clever create` for app `{}` reported a name conflict — resolving via listing",
                params.name
            );
        }

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

    /// Create an addon. Returns the new addon id and its underlying realId,
    /// both looked up by name after creation.
    pub fn create_addon(&self, params: &CreateAddon<'_>) -> Result<CreatedAddon> {
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
            let id = dry_run_id("addon", params.name);
            return Ok(CreatedAddon {
                addon_id: id.clone(),
                real_id: format!("{id}-real"),
            });
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

        if let Err(e) = self.run(&args) {
            if !is_name_conflict(&format!("{e:#}")) {
                return Err(e);
            }
            warn!(
                "`clever addon create` for `{}` reported a name conflict — resolving via listing",
                params.name
            );
        }

        let addons = self.list_addons(params.org)?;
        addons
            .into_iter()
            .find(|a| a.name == params.name)
            .map(|a| CreatedAddon {
                addon_id: a.addon_id,
                real_id: a.real_id,
            })
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
        self.run_with_stdin(&["env", "import", "--app", app, "--json"], &body)?;
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

    /// Set the build-instance flavor. `clever scale --build-flavor <name>`
    /// switches the dedicated build instance to `<name>`, or to `disabled`
    /// to turn it off entirely.
    pub fn set_build_flavor(&self, app: &str, flavor: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would set build flavor of `{app}` to `{flavor}`");
            return Ok(());
        }
        self.run(&["scale", "--app", app, "--build-flavor", flavor])
    }

    /// Set the git branch Clever pulls from on the next deploy. No first-
    /// party `clever-tools` subcommand exposes this, so we hit the v2 API
    /// directly via `clever curl -X PUT`.
    pub fn set_branch(&self, org: &str, app_id: &str, branch: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would set branch of `{app_id}` to `{branch}`");
            return Ok(());
        }
        let url = format!(
            "https://api.clever-cloud.com/v2/organisations/{org}/applications/{app_id}/branch"
        );
        let body = serde_json::json!({ "branch": branch }).to_string();
        self.run(&[
            "curl",
            "-X",
            "PUT",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            &url,
        ])
    }

    /// Restart (or kick off the first deployment of) an application.
    ///
    /// Fire-and-forget: the underlying `clever restart` is spawned and
    /// detached, so apply doesn't block waiting for the deployment to
    /// start. The HTTP call that triggers the restart is sent in the
    /// child's first few hundred ms; if apply exits before that finishes
    /// the child is reparented to init/launchd on Unix and runs to
    /// completion on its own. Stdout/stderr are silenced so they don't
    /// race with the rest of apply's output.
    pub fn restart(&self, app: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would restart app `{app}`");
            return Ok(());
        }
        info!("kicking off restart of `{app}` (running in background)");
        let _child = Command::new(&self.program)
            .args(["restart", "--app", app, "--quiet"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning `clever restart --app {app}`"))?;
        // `_child` is dropped without `wait()`. On Unix that leaves a
        // zombie until either we reap it (we don't) or the apply process
        // exits — at which point the running child gets reparented to
        // init/launchd and finishes on its own.
        Ok(())
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

    // -------- network groups --------

    /// List all network groups in the given organisation along with their
    /// currently linked members.
    pub fn list_network_groups(&self, org: &str) -> Result<Vec<ListedNetworkGroup>> {
        let json = self.run_json(&["ng", "--format", "json", "--org", org])?;
        serde_json::from_value(json).context("decoding `ng list` output")
    }

    /// Create a network group. Returns its new `ng_xxx` id (looked up by
    /// label after creation).
    pub fn create_network_group(&self, params: &CreateNetworkGroup<'_>) -> Result<String> {
        if self.dry_run {
            info!(
                "[dry-run] would create network group `{}` (description={:?}, members={:?})",
                params.label, params.description, params.members
            );
            return Ok(dry_run_id("ng", params.label));
        }
        let mut args: Vec<&str> = vec!["ng", "create", params.label, "--org", params.org];
        if let Some(d) = params.description {
            args.push("--description");
            args.push(d);
        }
        let members_csv;
        if !params.members.is_empty() {
            members_csv = params.members.join(",");
            args.push("--link");
            args.push(&members_csv);
        }
        if let Err(e) = self.run(&args) {
            if !is_name_conflict(&format!("{e:#}")) {
                return Err(e);
            }
            warn!(
                "`clever ng create` for `{}` reported a name conflict — resolving via listing",
                params.label
            );
        }
        let ngs = self.list_network_groups(params.org)?;
        ngs.into_iter()
            .find(|n| n.label == params.label)
            .map(|n| n.id)
            .ok_or_else(|| anyhow!("network group `{}` not found after creation", params.label))
    }

    pub fn delete_network_group(&self, id_or_label: &str, org: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would delete network group `{id_or_label}`");
            return Ok(());
        }
        self.run(&["ng", "delete", id_or_label, "--org", org])
    }

    pub fn ng_link(&self, member_id: &str, ng_id_or_label: &str, org: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would link `{member_id}` to network group `{ng_id_or_label}`");
            return Ok(());
        }
        self.run(&["ng", "link", member_id, ng_id_or_label, "--org", org])
    }

    pub fn ng_unlink(&self, member_id: &str, ng_id_or_label: &str, org: &str) -> Result<()> {
        if self.dry_run {
            info!("[dry-run] would unlink `{member_id}` from network group `{ng_id_or_label}`");
            return Ok(());
        }
        self.run(&["ng", "unlink", member_id, ng_id_or_label, "--org", org])
    }
}

fn is_transient_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    const PATTERNS: &[&str] = &[
        "etimedout",
        "econnreset",
        "econnrefused",
        "ehostunreach",
        "enotfound",
        "eai_again",
        "socket hang up",
        "network error",
        "fetch failed",
        "request timeout",
        "rate limit",
        "too many requests",
        " 429",
        " 500",
        " 502",
        " 503",
        " 504",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
        "internal server error",
    ];
    PATTERNS.iter().any(|p| m.contains(p))
}

fn is_name_conflict(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("already exists") || m.contains("already taken") || m.contains("name is taken")
}

fn backoff_delay(base_ms: u64, attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(5);
    let exp = base_ms.saturating_mul(1u64 << shift);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() % 200) as u64)
        .unwrap_or(0);
    Duration::from_millis(exp.saturating_add(jitter))
}

#[derive(Debug)]
pub struct CreatedAddon {
    pub addon_id: String,
    pub real_id: String,
}

#[derive(Debug)]
pub struct CreateNetworkGroup<'a> {
    pub label: &'a str,
    pub org: &'a str,
    pub description: Option<&'a str>,
    pub members: &'a [String],
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
    /// Underlying provider-specific id used by `clever ng link`
    /// (e.g. `postgresql_xxx`, `redis_xxx`).
    #[serde(rename = "realId")]
    pub real_id: String,
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
pub struct ListedNetworkGroup {
    pub id: String,
    pub label: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub description: Option<String>,
    #[serde(default)]
    pub members: Vec<NetworkGroupMember>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkGroupMember {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EnvFull {
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default, deserialize_with = "deserialize_injected_env")]
    pub from_addons: Vec<EnvVar>,
    #[serde(default, deserialize_with = "deserialize_injected_env")]
    pub from_dependencies: Vec<EnvVar>,
}

impl EnvFull {
    /// Flatten the three categories into one map. Order of precedence:
    /// dependencies < addons < user, so user-set vars always win on a name
    /// collision. Mirrors the runtime view an app process actually sees.
    pub fn merged(&self) -> IndexMap<String, String> {
        let mut out = IndexMap::new();
        for v in &self.from_dependencies {
            out.insert(v.name.clone(), v.value.clone());
        }
        for v in &self.from_addons {
            out.insert(v.name.clone(), v.value.clone());
        }
        for v in &self.env {
            out.insert(v.name.clone(), v.value.clone());
        }
        out
    }
}

/// `clever env` returns `from_addons` / `from_dependencies` either as a
/// flat list of `{name, value}` or as a nested map keyed by source name
/// (the shape depends on the clever-tools version). This deserializer
/// flattens both forms into a single `Vec<EnvVar>`.
fn deserialize_injected_env<'de, D>(de: D) -> std::result::Result<Vec<EnvVar>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: serde_json::Value = serde::Deserialize::deserialize(de)?;
    let mut out: Vec<EnvVar> = Vec::new();
    match value {
        serde_json::Value::Null => {}
        serde_json::Value::Array(items) => {
            for item in items {
                if let Ok(v) = serde_json::from_value::<EnvVar>(item) {
                    out.push(v);
                }
            }
        }
        serde_json::Value::Object(map) => {
            // Either `{ "name": "VALUE" }` or `{ "source": [{name, value}] }`.
            for (k, v) in map {
                match v {
                    serde_json::Value::String(s) => out.push(EnvVar { name: k, value: s }),
                    serde_json::Value::Array(items) => {
                        for item in items {
                            if let Ok(v) = serde_json::from_value::<EnvVar>(item) {
                                out.push(v);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Ok(out)
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

#[derive(Debug, Deserialize)]
pub struct AppDetails {
    pub instance: AppDetailsInstance,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub vhosts: Vec<AppDetailsVhost>,
    #[serde(rename = "buildFlavor", default)]
    pub build_flavor: Option<AppFlavor>,
    #[serde(rename = "separateBuild", default)]
    pub separate_build: bool,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppDetailsInstance {
    #[serde(rename = "minInstances")]
    pub min_instances: u32,
    #[serde(rename = "maxInstances")]
    pub max_instances: u32,
    #[serde(rename = "minFlavor")]
    pub min_flavor: AppFlavor,
    #[serde(rename = "maxFlavor")]
    pub max_flavor: AppFlavor,
}

#[derive(Debug, Deserialize)]
pub struct AppDetailsVhost {
    pub fqdn: String,
}

/// Decoded view of the per-app details endpoint. The five fields that
/// `read` / `status` need from a single round-trip, instead of the
/// previous env + domain + scale trio.
#[derive(Debug, Clone)]
pub struct AppDetailsView {
    pub scalability: Scalability,
    pub env: Vec<EnvVar>,
    /// Domain names with the trailing slash the API attaches to vhost
    /// fqdns stripped off. `.cleverapps.io` entries are *not* filtered here
    /// — callers decide what to do with them.
    pub vhosts: Vec<String>,
    pub build: Option<crate::model::Build>,
    /// Currently configured git branch (Clever pulls from this when the
    /// user pushes to the deploy remote). `None` if the API doesn't return
    /// one (e.g. apps with no source).
    pub branch: Option<String>,
}

fn app_details_view_from(d: AppDetails) -> AppDetailsView {
    let scalability = scalability_from_details(&d);
    let vhosts: Vec<String> = d
        .vhosts
        .into_iter()
        .map(|v| v.fqdn.trim_end_matches('/').to_string())
        .collect();
    let build = d.build_flavor.map(|f| crate::model::Build {
        separate: d.separate_build,
        flavor: Some(f.name),
    });
    AppDetailsView {
        scalability,
        env: d.env,
        vhosts,
        build,
        branch: d.branch,
    }
}

fn scalability_from_details(d: &AppDetails) -> Scalability {
    let inst = &d.instance;
    let is_auto =
        inst.min_instances != inst.max_instances || inst.min_flavor.name != inst.max_flavor.name;
    let instances = if is_auto {
        Instances {
            min_number: Some(inst.min_instances),
            max_number: Some(inst.max_instances),
            min_size: Some(inst.min_flavor.name.clone()),
            max_size: Some(inst.max_flavor.name.clone()),
        }
    } else {
        Instances {
            min_number: Some(inst.min_instances),
            max_number: None,
            min_size: Some(inst.min_flavor.name.clone()),
            max_size: None,
        }
    };
    Scalability {
        auto: is_auto,
        instances: Some(instances),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_patterns_detected() {
        assert!(is_transient_error("ETIMEDOUT"));
        assert!(is_transient_error("connection failed: ECONNRESET"));
        assert!(is_transient_error("fetch failed"));
        assert!(is_transient_error("socket hang up"));
        assert!(is_transient_error("HTTP 503 Service Unavailable"));
        assert!(is_transient_error("got 429 Too Many Requests"));
        assert!(is_transient_error("Internal Server Error"));
        assert!(is_transient_error("Bad Gateway"));
    }

    #[test]
    fn permanent_patterns_not_retried() {
        assert!(!is_transient_error("not found"));
        assert!(!is_transient_error("404"));
        assert!(!is_transient_error("invalid plan"));
        assert!(!is_transient_error("already exists"));
        assert!(!is_transient_error("forbidden"));
        assert!(!is_transient_error("validation failed"));
    }

    #[test]
    fn name_conflict_detected() {
        assert!(is_name_conflict("an app with name `foo` already exists"));
        assert!(is_name_conflict("Name is already taken"));
        assert!(is_name_conflict("This name is taken"));
    }

    #[test]
    fn name_conflict_does_not_false_positive() {
        assert!(!is_name_conflict("not found"));
        assert!(!is_name_conflict("ETIMEDOUT"));
        assert!(!is_name_conflict("invalid plan"));
    }

    #[test]
    fn backoff_grows_with_attempts() {
        let d1 = backoff_delay(100, 1);
        let d2 = backoff_delay(100, 2);
        let d3 = backoff_delay(100, 3);
        // d1 ~ 100ms, d2 ~ 200ms, d3 ~ 400ms (plus jitter < 200ms each)
        assert!(d1.as_millis() >= 100 && d1.as_millis() < 300);
        assert!(d2.as_millis() >= 200 && d2.as_millis() < 400);
        assert!(d3.as_millis() >= 400 && d3.as_millis() < 600);
    }

    #[test]
    fn backoff_caps_shift() {
        // Even at large attempts, the shift is capped so we don't overflow.
        let _ = backoff_delay(100, 100);
    }

    #[test]
    fn scalability_from_fixed_deployment() {
        // minInstances == maxInstances and same flavor → fixed (auto=false),
        // only min populated.
        let raw = r#"{
            "instance": {
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS"},
                "maxFlavor": {"name": "XS"}
            }
        }"#;
        let details: AppDetails = serde_json::from_str(raw).unwrap();
        let s = scalability_from_details(&details);
        assert!(!s.auto);
        let inst = s.instances.as_ref().unwrap();
        assert_eq!(inst.min_number, Some(1));
        assert_eq!(inst.max_number, None);
        assert_eq!(inst.min_size.as_deref(), Some("XS"));
        assert_eq!(inst.max_size, None);
    }

    #[test]
    fn scalability_from_autoscaled_range() {
        // Range on either count or flavor → auto=true, min+max populated.
        let raw = r#"{
            "instance": {
                "minInstances": 2,
                "maxInstances": 8,
                "minFlavor": {"name": "S"},
                "maxFlavor": {"name": "M"}
            }
        }"#;
        let details: AppDetails = serde_json::from_str(raw).unwrap();
        let s = scalability_from_details(&details);
        assert!(s.auto);
        let inst = s.instances.as_ref().unwrap();
        assert_eq!(inst.min_number, Some(2));
        assert_eq!(inst.max_number, Some(8));
        assert_eq!(inst.min_size.as_deref(), Some("S"));
        assert_eq!(inst.max_size.as_deref(), Some("M"));
    }

    #[test]
    fn scalability_count_range_but_same_flavor_is_auto() {
        let raw = r#"{
            "instance": {
                "minInstances": 1,
                "maxInstances": 4,
                "minFlavor": {"name": "M"},
                "maxFlavor": {"name": "M"}
            }
        }"#;
        let details: AppDetails = serde_json::from_str(raw).unwrap();
        let s = scalability_from_details(&details);
        assert!(s.auto);
        let inst = s.instances.as_ref().unwrap();
        assert_eq!(inst.min_number, Some(1));
        assert_eq!(inst.max_number, Some(4));
    }

    #[test]
    fn app_details_view_extracts_env_vhosts_and_build() {
        let raw = r#"{
            "id": "app_xx",
            "instance": {
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS"},
                "maxFlavor": {"name": "XS"}
            },
            "env": [
                {"name": "PORT", "value": "8080"},
                {"name": "DEBUG", "value": "1"}
            ],
            "vhosts": [
                {"fqdn": "x.cleverapps.io/"},
                {"fqdn": "api.example.com/"}
            ],
            "buildFlavor": {"name": "M"},
            "separateBuild": false
        }"#;
        let d: AppDetails = serde_json::from_str(raw).unwrap();
        let view = app_details_view_from(d);
        // env passthrough
        assert_eq!(view.env.len(), 2);
        assert_eq!(view.env[0].name, "PORT");
        assert_eq!(view.env[0].value, "8080");
        // trailing slash stripped, no filtering on cleverapps.io here
        assert_eq!(
            view.vhosts,
            vec!["x.cleverapps.io".to_string(), "api.example.com".to_string()]
        );
        // build flavor preserved, separate=false
        let build = view.build.unwrap();
        assert!(!build.separate);
        assert_eq!(build.flavor.as_deref(), Some("M"));
    }

    #[test]
    fn app_details_view_handles_separate_build_true() {
        let raw = r#"{
            "instance": {
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS"},
                "maxFlavor": {"name": "XS"}
            },
            "buildFlavor": {"name": "L"},
            "separateBuild": true
        }"#;
        let d: AppDetails = serde_json::from_str(raw).unwrap();
        let view = app_details_view_from(d);
        let build = view.build.unwrap();
        assert!(build.separate);
        assert_eq!(build.flavor.as_deref(), Some("L"));
    }

    #[test]
    fn app_details_view_extracts_branch() {
        let raw = r#"{
            "instance": {
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS"},
                "maxFlavor": {"name": "XS"}
            },
            "branch": "main"
        }"#;
        let d: AppDetails = serde_json::from_str(raw).unwrap();
        let view = app_details_view_from(d);
        assert_eq!(view.branch.as_deref(), Some("main"));
    }

    #[test]
    fn app_details_view_missing_branch_is_none() {
        let raw = r#"{
            "instance": {
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS"},
                "maxFlavor": {"name": "XS"}
            }
        }"#;
        let d: AppDetails = serde_json::from_str(raw).unwrap();
        let view = app_details_view_from(d);
        assert!(view.branch.is_none());
    }

    #[test]
    fn app_details_view_no_build_flavor_means_no_build_block() {
        let raw = r#"{
            "instance": {
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS"},
                "maxFlavor": {"name": "XS"}
            }
        }"#;
        let d: AppDetails = serde_json::from_str(raw).unwrap();
        let view = app_details_view_from(d);
        assert!(view.build.is_none());
        assert!(view.env.is_empty());
        assert!(view.vhosts.is_empty());
    }

    #[test]
    fn app_details_ignores_unknown_fields() {
        // The real endpoint returns dozens of fields. We only deserialize a
        // handful; the rest must not cause failure.
        let raw = r#"{
            "id": "app_xx",
            "name": "x",
            "instance": {
                "type": "node",
                "minInstances": 1,
                "maxInstances": 1,
                "minFlavor": {"name": "XS", "mem": 1152, "cpus": 1, "extra": "ignored"},
                "maxFlavor": {"name": "XS"},
                "flavors": [{"name": "pico"}, {"name": "XS"}]
            },
            "vhosts": [{"fqdn": "x.cleverapps.io/"}],
            "env": [{"name": "K", "value": "v"}]
        }"#;
        let details: AppDetails = serde_json::from_str(raw).unwrap();
        assert_eq!(details.instance.min_flavor.name, "XS");
    }
}
