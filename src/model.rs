use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use tracing::debug;

use crate::interpolate::Resolver;
use crate::issues::{self, Issue, IssueSink};

static SECRET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{secrets\.([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());

/// Valid values for `app.kind`, as accepted by `clever create --type`.
/// See https://www.clever.cloud/developers/doc/.
pub const ALLOWED_APP_KINDS: &[&str] = &[
    "docker",
    "dotnet",
    "elixir",
    "frankenphp",
    "go",
    "gradle",
    "haskell",
    "jar",
    "linux",
    "maven",
    "meteor",
    "node",
    "php",
    "play1",
    "play2",
    "python",
    "ruby",
    "rust",
    "sbt",
    "static",
    "static-apache",
    "v",
    "war",
];

/// Lowercase + map common aliases to the canonical kind. `java` becomes
/// `jar` (Clever reports java apps with `type: jar`).
pub fn normalize_app_kind(kind: &str) -> String {
    let lower = kind.to_lowercase();
    match lower.as_str() {
        "java" => "jar".to_string(),
        _ => lower,
    }
}

/// Valid values for `region` (project root, app, or addon).
pub const ALLOWED_REGIONS: &[&str] = &[
    "par", "parhds", "scw", "grahds", "ldn", "mtl", "rbx", "rbxhds", "sgp", "syd", "wsw",
];

/// Addon kinds that expose a `resources.entrypoint` app id in their v4
/// metadata — i.e. managed services that run on a real Clever app under
/// the hood. Only these accept `env:` and `domains:` on the project addon;
/// database-style addons (postgresql, redis, ...) don't have an entrypoint
/// and would silently no-op, so we reject them at load time instead.
///
/// Accepts both the bare canonical name (`otoroshi`) and the `addon-X`
/// prefixed form Clever sometimes returns (`addon-otoroshi`).
pub const MANAGED_ADDON_KINDS: &[&str] = &["otoroshi", "keycloak", "matomo", "metabase", "pulsar"];

/// Whether the given addon `kind` (as written in the project file) is a
/// managed service that supports `env:` and `domains:`. Tolerates the
/// `addon-` prefix and case.
pub fn is_managed_addon_kind(kind: &str) -> bool {
    let lower = kind.to_lowercase();
    let bare = lower.strip_prefix("addon-").unwrap_or(&lower);
    MANAGED_ADDON_KINDS.contains(&bare)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub org: String,
    pub region: String,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub variables: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub apps: IndexMap<String, App>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub addons: IndexMap<String, Addon>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub network_groups: IndexMap<String, NetworkGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,
    /// Key/value pairs surfaced at the end of `apply` (and in `--dry-run`
    /// plan output). Values go through the same interpolation pipeline as
    /// env vars — including cross-resource refs like `${apps.X.env.Y}` —
    /// so you can print resolved URLs, addon credentials, etc.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub display: IndexMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_apply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_apply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_delete: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkGroup {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Project keys (in `apps:` or `addons:`) of the resources to attach to
    /// this network group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalability: Option<Scalability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Build>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub config: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,
}

/// Build-instance config for an app. When `separate: true`, Clever spins up
/// a dedicated instance with `flavor` to compile the code before running it.
/// `flavor` may also be present with `separate: false` — the API tracks
/// both independently, so we mirror that.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Build {
    #[serde(default)]
    pub separate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scalability {
    #[serde(default)]
    pub auto: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Instances>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instances {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Addon {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default)]
    pub crypted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    /// Env vars to push onto the underlying entrypoint app of a managed
    /// addon (otoroshi, keycloak, ...). Rejected at load time when `kind`
    /// is not a managed addon.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env: IndexMap<String, String>,
    /// Custom domains to attach to the underlying entrypoint app of a
    /// managed addon. Rejected at load time when `kind` is not managed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Format {
    Yaml,
    Json,
    Toml,
}

impl Format {
    pub fn from_path(path: &Path) -> Result<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("yaml") | Some("yml") => Ok(Format::Yaml),
            Some("json") => Ok(Format::Json),
            Some("toml") => Ok(Format::Toml),
            Some(other) => Err(anyhow!(
                "unsupported file extension `.{other}` (expected .yaml, .yml, .json or .toml)"
            )),
            None => Err(anyhow!(
                "missing file extension on `{}` (expected .yaml, .yml, .json or .toml)",
                path.display()
            )),
        }
    }
}

impl Project {
    /// Load and parse without interpolation. Used by tests; in production
    /// callers usually want `load_and_resolve`.
    #[allow(dead_code)]
    pub fn load(path: &Path) -> Result<Self> {
        let value = load_value(path)?;
        let project: Project = serde_yaml::from_value(value)
            .with_context(|| format!("deserializing project from `{}`", path.display()))?;
        Ok(project)
    }

    /// Load the project file, run variable interpolation, and return both the
    /// resolved project and the resolver. Fails fast — all accumulated soft
    /// issues (missing vars/secrets, unknown kinds/regions, etc.) are
    /// rendered as a single error.
    pub fn load_and_resolve(
        path: &Path,
        org_override: Option<String>,
        region_override: Option<String>,
        cli_vars: &[(String, String)],
        secrets_path: Option<&Path>,
        cli_secrets: &[(String, String)],
    ) -> Result<(Self, Resolver)> {
        let (project, resolver, issues) = load_inner(
            path,
            org_override,
            region_override,
            cli_vars,
            secrets_path,
            cli_secrets,
        )?;
        if !issues.is_empty() {
            bail!("{}", issues::render(&issues));
        }
        Ok((project, resolver))
    }

    /// Like `load_and_resolve` but does not bail on soft issues. Returns the
    /// (partially resolved) project plus every accumulated issue, so callers
    /// like `check` can run further cross-resource validators on top before
    /// rendering one combined report. Still returns `Err` for fatal failures
    /// that prevent producing a `Project` at all (I/O, syntax, missing
    /// `org`/`region`, mixed variables shape, reserved variable name).
    pub fn load_collecting(
        path: &Path,
        org_override: Option<String>,
        region_override: Option<String>,
        cli_vars: &[(String, String)],
        secrets_path: Option<&Path>,
        cli_secrets: &[(String, String)],
    ) -> Result<(Self, Vec<Issue>)> {
        let (project, _resolver, issues) = load_inner(
            path,
            org_override,
            region_override,
            cli_vars,
            secrets_path,
            cli_secrets,
        )?;
        Ok((project, issues))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let serialized = match Format::from_path(path)? {
            Format::Yaml => serde_yaml::to_string(self).context("serializing to YAML")?,
            Format::Json => serde_json::to_string_pretty(self).context("serializing to JSON")?,
            Format::Toml => toml::to_string_pretty(self).context("serializing to TOML")?,
        };
        std::fs::write(path, serialized)
            .with_context(|| format!("writing project file `{}`", path.display()))?;
        Ok(())
    }
}

/// Shared loading pipeline: parse the file, build the resolver, interpolate,
/// deserialize, run the model-level validators. Soft issues (missing
/// variables, unknown kinds/regions, ...) are collected into the returned
/// `Vec<Issue>`; only truly fatal failures (I/O, syntax, mixed-shape
/// variables, missing `org`/`region`) return `Err`.
fn load_inner(
    path: &Path,
    org_override: Option<String>,
    region_override: Option<String>,
    cli_vars: &[(String, String)],
    secrets_path: Option<&Path>,
    cli_secrets: &[(String, String)],
) -> Result<(Project, Resolver, Vec<Issue>)> {
    let mut issues: Vec<Issue> = Vec::new();

    let mut value = load_value(path)?;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("project file root must be a mapping"))?;

    let org = match org_override {
        Some(v) => v,
        None => map
            .get(Value::String("org".into()))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("missing `org` at project root (and no --org override)"))?,
    };
    let region = match region_override {
        Some(v) => v,
        None => map
            .get(Value::String("region".into()))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow!("missing `region` at project root (and no --region override)")
            })?,
    };

    let effective_env = cli_vars
        .iter()
        .rev()
        .find(|(k, _)| k == "env")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "prod".to_string());

    let secrets = load_secrets(path, &effective_env, secrets_path, cli_secrets)?;

    let raw_variables = map
        .remove(Value::String("variables".into()))
        .unwrap_or(Value::Null);
    let file_vars = parse_variables(&raw_variables, &effective_env)?;
    // Allow `${secrets.X}` inside variable values. Missing secrets become
    // empty strings and an issue.
    let file_vars = expand_secrets(file_vars, &secrets, &mut issues);

    // Build the merged map: file vars first, then secrets exposed under
    // their `secrets.<key>` namespace. Resolver::build will layer
    // cli_vars on top and add env/org/region.
    let mut combined = file_vars;
    for (k, v) in &secrets {
        combined.insert(format!("secrets.{k}"), v.clone());
    }
    let resolver = Resolver::build(&combined, cli_vars, org.clone(), region.clone())?;

    // Apply CLI overrides into the value tree so the deserialized Project
    // reflects them. The `variables` section was removed earlier — the
    // resolver carries the merged values now.
    map.insert(Value::String("org".into()), Value::String(org));
    map.insert(Value::String("region".into()), Value::String(region));

    resolver.resolve_value(&mut value, &mut issues);

    let mut project: Project = serde_yaml::from_value(value)
        .with_context(|| format!("deserializing project from `{}`", path.display()))?;
    validate_and_normalize_app_kinds(&mut project, &mut issues);
    validate_regions(&project, &mut issues);
    validate_addon_managed_features(&project, &mut issues);
    Ok((project, resolver, issues))
}

/// Parse the project file's `variables` section. Two shapes are accepted:
///
/// - **flat**: `Map<String, scalar>` — used as-is.
/// - **per-env**: `Map<String, Map<String, scalar>>` — entries under the
///   special key `common` are always included, then entries under the key
///   matching the resolved `${env}` value are merged on top (overriding
///   common).
///
/// Mixing scalar and mapping values at the top level is rejected.
fn parse_variables(raw: &Value, env: &str) -> Result<IndexMap<String, String>> {
    let mapping = match raw {
        Value::Null => return Ok(IndexMap::new()),
        Value::Mapping(m) => m,
        _ => bail!("`variables` must be a mapping"),
    };
    if mapping.is_empty() {
        return Ok(IndexMap::new());
    }

    let mut saw_scalar = false;
    let mut saw_mapping = false;
    for (_, v) in mapping {
        match v {
            Value::Mapping(_) => saw_mapping = true,
            Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => saw_scalar = true,
            _ => bail!("variable values must be scalars or mappings"),
        }
    }
    if saw_scalar && saw_mapping {
        bail!(
            "`variables` must be either a flat map (key=scalar) or a per-env map (key=mapping), not both"
        );
    }

    if saw_mapping {
        let mut out = IndexMap::new();
        if let Some(Value::Mapping(common)) = mapping.get(Value::String("common".into())) {
            collect_scalar_entries(common, &mut out, "common")?;
        }
        if let Some(Value::Mapping(env_group)) = mapping.get(Value::String(env.into())) {
            collect_scalar_entries(env_group, &mut out, env)?;
        }
        Ok(out)
    } else {
        let mut out = IndexMap::new();
        collect_scalar_entries(mapping, &mut out, "variables")?;
        Ok(out)
    }
}

fn collect_scalar_entries(
    m: &serde_yaml::Mapping,
    out: &mut IndexMap<String, String>,
    where_: &str,
) -> Result<()> {
    for (k, v) in m {
        let key = k
            .as_str()
            .ok_or_else(|| anyhow!("variable keys must be strings (in `{where_}`)"))?
            .to_string();
        let val = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Null => String::new(),
            _ => bail!("variable `{key}` in `{where_}` must be a scalar (string/number/bool)"),
        };
        out.insert(key, val);
    }
    Ok(())
}

/// Load the secrets map for a project.
///
/// - If `explicit` is `Some(path)`, only that file is loaded and it must exist.
/// - Otherwise, both `<project>.secrets` (the env-agnostic defaults) and
///   `<project>.<env>.secrets` (env-specific overrides) are auto-discovered
///   next to the project file. Either or both may be absent — that's fine.
///   When both are present, the env-specific entries override the defaults.
fn load_secrets(
    project_path: &Path,
    env: &str,
    explicit: Option<&Path>,
    cli_secrets: &[(String, String)],
) -> Result<IndexMap<String, String>> {
    let mut out = if let Some(p) = explicit {
        read_secrets_file(p, /* required */ true)?
    } else {
        let mut out = IndexMap::new();
        let stem = project_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let dir = project_path.parent().unwrap_or(Path::new("."));
        if !stem.is_empty() {
            let default_path = dir.join(format!("{stem}.secrets"));
            if default_path.exists() {
                debug!("loading secrets from `{}`", default_path.display());
                for (k, v) in read_secrets_file(&default_path, false)? {
                    out.insert(k, v);
                }
            }
            let env_path = dir.join(format!("{stem}.{env}.secrets"));
            if env_path.exists() {
                debug!("loading env-specific secrets from `{}`", env_path.display());
                for (k, v) in read_secrets_file(&env_path, false)? {
                    out.insert(k, v);
                }
            }
        }
        out
    };

    // `--secret key=value` overrides win over everything loaded from disk,
    // matching `--variable` / `--variables-file-path` semantics.
    for (k, v) in cli_secrets {
        out.insert(k.clone(), v.clone());
    }
    Ok(out)
}

fn read_secrets_file(path: &Path, required: bool) -> Result<IndexMap<String, String>> {
    if !path.exists() {
        if required {
            bail!("secrets file `{}` not found", path.display());
        }
        return Ok(IndexMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading secrets file `{}`", path.display()))?;
    let value = parse_any(&raw).with_context(|| {
        format!(
            "parsing secrets file `{}` (neither YAML, JSON nor TOML)",
            path.display()
        )
    })?;
    let mapping = match value {
        Value::Mapping(m) => m,
        Value::Null => return Ok(IndexMap::new()),
        _ => bail!("secrets file `{}` must be a mapping", path.display()),
    };
    let mut out = IndexMap::new();
    for (k, v) in mapping {
        let key = k
            .as_str()
            .ok_or_else(|| anyhow!("secret keys must be strings (in `{}`)", path.display()))?
            .to_string();
        let val = match v {
            Value::String(s) => s,
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Null => String::new(),
            _ => bail!(
                "secret `{key}` in `{}` must be a scalar (string/number/bool)",
                path.display()
            ),
        };
        out.insert(key, val);
    }
    Ok(out)
}

/// Try parsing `raw` as YAML, then JSON, then TOML; return the first one
/// that produces a mapping (or null). Used for sidecar files (`.secrets`)
/// where the format can't be inferred from the extension.
///
/// YAML is permissive and would happily slurp TOML content into a single
/// scalar string, so each successful parse is filtered to "Mapping or
/// Null" before being returned; anything else falls through to the next
/// parser. The order is YAML → JSON → TOML.
fn parse_any(raw: &str) -> Result<Value> {
    let mut errors: Vec<String> = Vec::new();
    match serde_yaml::from_str::<Value>(raw) {
        Ok(v) if matches!(v, Value::Mapping(_) | Value::Null) => return Ok(v),
        Ok(_) => errors.push("YAML: parsed but root is not a mapping".to_string()),
        Err(e) => errors.push(format!("YAML: {e}")),
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(jv) if jv.is_object() || jv.is_null() => {
            return serde_yaml::to_value(&jv).context("converting parsed JSON to YAML value");
        }
        Ok(_) => errors.push("JSON: parsed but root is not an object".to_string()),
        Err(e) => errors.push(format!("JSON: {e}")),
    }
    match toml::from_str::<toml::Value>(raw) {
        Ok(tv) if tv.is_table() => {
            return serde_yaml::to_value(tv).context("converting parsed TOML to internal value");
        }
        Ok(_) => errors.push("TOML: parsed but root is not a table".to_string()),
        Err(e) => errors.push(format!("TOML: {e}")),
    }
    Err(anyhow!(
        "could not parse as YAML, JSON or TOML (root must be a mapping/object/table)\n  {}",
        errors.join("\n  ")
    ))
}

/// Expand `${secrets.X}` references inside the values of the project's
/// variables section, before they're handed to the resolver. References to
/// other variables (`${foo}`) are left untouched here — they're handled by
/// the resolver during the value-tree walk. Missing secrets are recorded in
/// `issues` and replaced by empty so resolution can continue.
fn expand_secrets(
    vars: IndexMap<String, String>,
    secrets: &IndexMap<String, String>,
    issues: &mut Vec<Issue>,
) -> IndexMap<String, String> {
    let mut out = IndexMap::with_capacity(vars.len());
    for (k, v) in vars {
        let resolved = SECRET_RE.replace_all(&v, |caps: &regex::Captures| {
            let name = &caps[1];
            match secrets.get(name) {
                Some(val) => val.clone(),
                None => {
                    issues.push_issue(format!(
                        "undefined secret `{name}` referenced in variable `{k}`"
                    ));
                    String::new()
                }
            }
        });
        out.insert(k, resolved.into_owned());
    }
    out
}

/// Load a `--variables-file-path FILE` as a flat list of `(key, value)` pairs.
/// Accepts YAML, JSON or TOML (detected by extension); the file must be a
/// mapping of scalars (matching the shape of `--variable foo=bar` overrides).
pub fn load_variables_file(path: &Path) -> Result<Vec<(String, String)>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading variables file `{}`", path.display()))?;
    let value: Value = match Format::from_path(path)? {
        Format::Yaml | Format::Json => serde_yaml::from_str(&raw)
            .with_context(|| format!("parsing variables file `{}`", path.display()))?,
        Format::Toml => {
            let tv: toml::Value = toml::from_str(&raw)
                .with_context(|| format!("parsing TOML variables file `{}`", path.display()))?;
            serde_yaml::to_value(tv).with_context(|| {
                format!(
                    "converting TOML variables file `{}` to internal value",
                    path.display()
                )
            })?
        }
    };
    let mapping = match value {
        Value::Mapping(m) => m,
        Value::Null => return Ok(Vec::new()),
        _ => bail!("variables file `{}` must be a mapping", path.display()),
    };
    let mut out = Vec::new();
    for (k, v) in mapping {
        let key = k
            .as_str()
            .ok_or_else(|| anyhow!("variable keys must be strings (in `{}`)", path.display()))?
            .to_string();
        let val = match v {
            Value::String(s) => s,
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Null => String::new(),
            _ => bail!(
                "variable `{key}` in `{}` must be a scalar (string/number/bool)",
                path.display()
            ),
        };
        out.push((key, val));
    }
    Ok(out)
}

/// Normalize each app's `kind` (lowercase + `java` → `jar`) and record any
/// kind that isn't in `ALLOWED_APP_KINDS`. The mutation happens regardless so
/// downstream code sees the canonical form even when the kind is unknown.
fn validate_and_normalize_app_kinds(project: &mut Project, issues: &mut Vec<Issue>) {
    for (key, app) in project.apps.iter_mut() {
        let normalized = normalize_app_kind(&app.kind);
        if !ALLOWED_APP_KINDS.contains(&normalized.as_str()) {
            issues.push_issue(format!(
                "app `{key}` has unknown kind `{}`. Valid kinds: {} (or `java` as an alias for `jar`)",
                app.kind,
                ALLOWED_APP_KINDS.join(", ")
            ));
        }
        app.kind = normalized;
    }
}

/// Reject any unknown region — root, per-app, or per-addon.
fn validate_regions(project: &Project, issues: &mut Vec<Issue>) {
    check_region("project root", &project.region, issues);
    for (key, app) in &project.apps {
        if let Some(r) = &app.region {
            check_region(&format!("app `{key}`"), r, issues);
        }
    }
    for (key, addon) in &project.addons {
        if let Some(r) = &addon.region {
            check_region(&format!("addon `{key}`"), r, issues);
        }
    }
}

/// Reject `env:` or `domains:` on an addon whose kind is not a known
/// managed service. These fields are pushed onto the addon's entrypoint
/// app — addons without an entrypoint (postgresql, redis, ...) would
/// silently no-op, so we surface it at load time.
fn validate_addon_managed_features(project: &Project, issues: &mut Vec<Issue>) {
    for (key, addon) in &project.addons {
        let has_env = !addon.env.is_empty();
        let has_domains = !addon.domains.is_empty();
        if !has_env && !has_domains {
            continue;
        }
        if !is_managed_addon_kind(&addon.kind) {
            let fields = match (has_env, has_domains) {
                (true, true) => "`env` and `domains`",
                (true, false) => "`env`",
                (false, true) => "`domains`",
                (false, false) => unreachable!(),
            };
            issues.push_issue(format!(
                "addon `{key}` declares {fields} but kind `{}` is not a managed addon. Supported managed kinds: {}",
                addon.kind,
                MANAGED_ADDON_KINDS.join(", ")
            ));
        }
    }
}

fn check_region(where_: &str, value: &str, issues: &mut Vec<Issue>) {
    if !ALLOWED_REGIONS.contains(&value) {
        issues.push_issue(format!(
            "{where_} has unknown region `{value}`. Valid regions: {}",
            ALLOWED_REGIONS.join(", ")
        ));
    }
}

fn load_value(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading project file `{}`", path.display()))?;
    match Format::from_path(path)? {
        // JSON is a subset of YAML 1.2, so the YAML parser handles both.
        Format::Yaml | Format::Json => {
            serde_yaml::from_str(&raw).with_context(|| format!("parsing `{}`", path.display()))
        }
        Format::Toml => {
            let tv: toml::Value = toml::from_str(&raw)
                .with_context(|| format!("parsing TOML `{}`", path.display()))?;
            // Funnel TOML into the same serde_yaml::Value the interpolator
            // walks. `serde_yaml::to_value` accepts any Serialize, so this
            // bridge is lossless for the scalars/arrays/tables we care
            // about (TOML datetimes are converted to strings).
            serde_yaml::to_value(tv)
                .with_context(|| format!("converting TOML `{}` to internal value", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name: My Project
description: "....."
org: orga_orga-clevercloud-cible
region: par
variables:
  foo: bar
  bar: qix
apps:
  app1:
    name: frontend
    kind: java
    region: par
    source:
      from: https://github.com/MAIF/otoroshi.git
      branch: master
    domains:
      - foo.${foo}.qix
    scalability:
      auto: true
      instances:
        minNumber: 1
        maxNumber: 2
        minSize: S
        maxSize: M
    dependencies:
      - addon1
      - addon2
      - app2
    config:
      foo: bar
    env:
      PORT: "8080"
addons:
  addon1:
    name: ${env}-pg
    kind: postgresql
    size: S_BIG
    crypted: true
    region: par
    version: 17
"#;

    fn write_tmp(ext: &str, contents: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "clever-project-test-{}-{}.{ext}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn rand_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }

    #[test]
    fn function_calls_in_env_values_get_resolved() {
        let yaml = "\
name: n8n-test
org: orga_dummy
region: par
apps:
  n8n:
    name: n8n
    kind: node
    domains:
      - n8n-${ulid_lowercase()}.cleverapps.io
    env:
      N8N_ENCRYPTION_KEY: ${random_alphanumeric(32)}
      N8N_HOST: prefix-${ulid_lowercase()}.cleverapps.io
display:
  n8n-url: https://prefix-${ulid_lowercase()}.cleverapps.io/
";
        let p = write_tmp("yaml", yaml);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        let app = project.apps.get("n8n").unwrap();
        let key = app.env.get("N8N_ENCRYPTION_KEY").unwrap();
        assert_eq!(key.len(), 32, "got `{key}`");
        assert!(
            !key.contains("${"),
            "function not substituted, got: `{key}`"
        );
        let host = app.env.get("N8N_HOST").unwrap();
        assert!(!host.contains("${"), "host not substituted: `{host}`");
        assert!(host.starts_with("prefix-"));
        assert!(host.ends_with(".cleverapps.io"));
        assert!(!app.domains[0].contains("${"));
        let url = project.display.get("n8n-url").unwrap();
        assert!(!url.contains("${"), "display not substituted: `{url}`");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn loads_and_resolves_spec_sample() {
        let p = write_tmp("yaml", SAMPLE);
        let (project, _r) =
            Project::load_and_resolve(&p, None, None, &[], None, &[]).expect("load failed");
        assert_eq!(project.name, "My Project");
        assert_eq!(project.org, "orga_orga-clevercloud-cible");
        assert_eq!(project.region, "par");
        let app = project.apps.get("app1").unwrap();
        assert_eq!(app.domains[0], "foo.bar.qix");
        let addon = project.addons.get("addon1").unwrap();
        assert_eq!(addon.name, "prod-pg"); // ${env} -> prod default
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn cli_overrides_apply_to_project() {
        let p = write_tmp("yaml", SAMPLE);
        let (project, _r) = Project::load_and_resolve(
            &p,
            Some("override_org".into()),
            Some("rbx".into()),
            &[("env".to_string(), "staging".to_string())],
            None,
            &[],
        )
        .unwrap();
        assert_eq!(project.org, "override_org");
        assert_eq!(project.region, "rbx");
        assert_eq!(project.addons.get("addon1").unwrap().name, "staging-pg");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn missing_var_propagates() {
        let bad = "name: x\norg: o\nregion: r\napps:\n  a:\n    name: ${missing}\n    kind: node\n";
        let p = write_tmp("yaml", bad);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        assert!(err.to_string().contains("missing"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unknown_root_region_is_rejected() {
        let bad = "name: P\norg: o\nregion: atlantis\napps: {}\n";
        let p = write_tmp("yaml", bad);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("atlantis"));
        assert!(msg.contains("Valid regions"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unknown_app_region_is_rejected() {
        let bad = "name: P\norg: o\nregion: par\napps:\n  a:\n    name: x\n    kind: node\n    region: zzz\n";
        let p = write_tmp("yaml", bad);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("app `a`"));
        assert!(msg.contains("zzz"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn multiple_load_time_issues_are_all_reported() {
        // bad root region + bad app kind + bad app region: 3 problems
        let bad = "name: P\norg: o\nregion: atlantis\napps:\n  a:\n    name: x\n    kind: cobol\n    region: zzz\n";
        let p = write_tmp("yaml", bad);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("3 validation problems"), "got: {msg}");
        assert!(msg.contains("atlantis"));
        assert!(msg.contains("cobol"));
        assert!(msg.contains("zzz"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unknown_addon_region_is_rejected() {
        let bad = "name: P\norg: o\nregion: par\napps: {}\naddons:\n  db:\n    name: x\n    kind: postgresql\n    region: nope\n";
        let p = write_tmp("yaml", bad);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("addon `db`"));
        assert!(msg.contains("nope"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn cli_region_override_is_validated() {
        let yaml = "name: P\norg: o\nregion: par\napps: {}\n";
        let p = write_tmp("yaml", yaml);
        let err = Project::load_and_resolve(&p, None, Some("mars".to_string()), &[], None, &[])
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("mars"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unknown_app_kind_is_rejected() {
        let bad = "name: P\norg: o\nregion: par\napps:\n  a:\n    name: x\n    kind: cobol\n";
        let p = write_tmp("yaml", bad);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cobol"));
        assert!(msg.contains("Valid kinds"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn java_alias_normalises_to_jar() {
        let yaml = "name: P\norg: o\nregion: par\napps:\n  a:\n    name: x\n    kind: java\n";
        let p = write_tmp("yaml", yaml);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().kind, "jar");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn kind_is_lowercased() {
        let yaml = "name: P\norg: o\nregion: par\napps:\n  a:\n    name: x\n    kind: NODE\n";
        let p = write_tmp("yaml", yaml);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().kind, "node");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn json_format_works_too() {
        let json = r#"{"name":"P","org":"o","region":"par","apps":{"a":{"name":"${env}-app","kind":"node"}}}"#;
        let p = write_tmp("json", json);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "prod-app");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn toml_format_works_too() {
        let toml = r#"
name = "P"
org = "o"
region = "par"

[apps.a]
name = "${env}-app"
kind = "node"
domains = ["api.example.com"]

[apps.a.env]
PORT = "8080"

[addons.db]
name = "${env}-db"
kind = "postgresql"
size = "xs_sml"
"#;
        let p = write_tmp("toml", toml);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.name, "P");
        assert_eq!(project.org, "o");
        let app = project.apps.get("a").unwrap();
        assert_eq!(app.name, "prod-app");
        assert_eq!(app.kind, "node");
        assert_eq!(app.domains, vec!["api.example.com".to_string()]);
        assert_eq!(app.env.get("PORT").map(String::as_str), Some("8080"));
        let addon = project.addons.get("db").unwrap();
        assert_eq!(addon.kind, "postgresql");
        assert_eq!(addon.size.as_deref(), Some("xs_sml"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn toml_roundtrip_via_save() {
        // Build a project in memory, save it to .toml, load it back, verify equal.
        let project = Project {
            name: "Round trip".into(),
            description: Some("test".into()),
            org: "o".into(),
            region: "par".into(),
            variables: IndexMap::new(),
            apps: {
                let mut m = IndexMap::new();
                let mut env = IndexMap::new();
                env.insert("PORT".into(), "8080".into());
                m.insert(
                    "api".into(),
                    App {
                        name: "prod-api".into(),
                        kind: "node".into(),
                        region: None,
                        source: Some(Source {
                            from: "https://github.com/me/api.git".into(),
                            branch: None,
                        }),
                        domains: vec!["api.example.com".into()],
                        scalability: None,
                        build: None,
                        dependencies: vec!["db".into()],
                        config: IndexMap::new(),
                        env,
                        hooks: None,
                    },
                );
                m
            },
            addons: {
                let mut m = IndexMap::new();
                m.insert(
                    "db".into(),
                    Addon {
                        name: "prod-db".into(),
                        kind: "postgresql".into(),
                        size: Some("xs_sml".into()),
                        crypted: true,
                        region: None,
                        version: None,
                        backup_path: None,
                        env: IndexMap::new(),
                        domains: vec![],
                    },
                );
                m
            },
            network_groups: IndexMap::new(),
            hooks: None,
            display: IndexMap::new(),
        };

        let mut p = std::env::temp_dir();
        p.push(format!(
            "clever-project-toml-roundtrip-{}-{}.toml",
            std::process::id(),
            rand_suffix()
        ));
        project.save(&p).unwrap();
        // Round-trip through load_and_resolve (it interpolates so use no ${...}).
        let (reloaded, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        assert_eq!(reloaded.name, "Round trip");
        assert_eq!(reloaded.org, "o");
        let api = reloaded.apps.get("api").unwrap();
        assert_eq!(api.name, "prod-api");
        assert_eq!(api.kind, "node");
        assert_eq!(
            api.source.as_ref().unwrap().from,
            "https://github.com/me/api.git"
        );
        assert_eq!(api.dependencies, vec!["db".to_string()]);
        assert_eq!(api.env.get("PORT").map(String::as_str), Some("8080"));
        let db = reloaded.addons.get("db").unwrap();
        assert_eq!(db.size.as_deref(), Some("xs_sml"));
        assert!(db.crypted);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn toml_secrets_file_works() {
        let project_path = write_named(
            "t.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: ${secrets.apikey}-app\n    kind: node\n",
        );
        let dir = project_path.parent().unwrap();
        // The sidecar is named `.secrets` but contains TOML — parse_any
        // should pick it up.
        std::fs::write(dir.join("t.secrets"), "apikey = \"toml-secret\"\n").unwrap();
        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "toml-secret-app");
    }

    #[test]
    fn toml_variables_file_works() {
        let p = write_tmp("toml", "foo = \"bar\"\ncount = 3\nflag = true\n");
        let pairs = load_variables_file(&p).unwrap();
        assert!(pairs.contains(&("foo".to_string(), "bar".to_string())));
        assert!(pairs.contains(&("count".to_string(), "3".to_string())));
        assert!(pairs.contains(&("flag".to_string(), "true".to_string())));
        std::fs::remove_file(&p).ok();
    }

    const PER_ENV: &str = r#"
name: PE
org: o
region: par
variables:
  common:
    domain: foo.bar
  prod:
    apikey: secret_for_prod
  dev:
    apikey: secret_for_dev
    domain: dev.bar
apps:
  a:
    name: ${env}-app
    kind: node
    env:
      DOMAIN: ${domain}
      APIKEY: ${apikey}
"#;

    #[test]
    fn per_env_picks_default_prod_group() {
        let p = write_tmp("yaml", PER_ENV);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        let app = project.apps.get("a").unwrap();
        assert_eq!(app.env.get("DOMAIN").unwrap(), "foo.bar");
        assert_eq!(app.env.get("APIKEY").unwrap(), "secret_for_prod");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn per_env_dev_group_overrides_common() {
        let p = write_tmp("yaml", PER_ENV);
        let (project, _r) = Project::load_and_resolve(
            &p,
            None,
            None,
            &[("env".to_string(), "dev".to_string())],
            None,
            &[],
        )
        .unwrap();
        let app = project.apps.get("a").unwrap();
        assert_eq!(app.env.get("DOMAIN").unwrap(), "dev.bar");
        assert_eq!(app.env.get("APIKEY").unwrap(), "secret_for_dev");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn per_env_unknown_env_uses_common_only_and_errors_on_unknown_ref() {
        let p = write_tmp("yaml", PER_ENV);
        // `staging` doesn't match any per-env group, so only `common` is
        // available. The reference to `${apikey}` in the app's env must error.
        let err = Project::load_and_resolve(
            &p,
            None,
            None,
            &[("env".to_string(), "staging".to_string())],
            None,
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("apikey"));
        std::fs::remove_file(&p).ok();
    }

    fn write_named(name: &str, contents: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "clever-project-test-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.push(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn secrets_auto_discover_default_file() {
        let project_path = write_named(
            "myproj.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: ${secrets.apikey}-app\n    kind: node\n",
        );
        let dir = project_path.parent().unwrap();
        std::fs::write(dir.join("myproj.secrets"), "apikey: deadbeef\n").unwrap();

        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "deadbeef-app");
    }

    #[test]
    fn secrets_env_specific_overrides_default() {
        let project_path = write_named(
            "p.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: x\n    kind: node\n    env:\n      K: ${secrets.token}\n",
        );
        let dir = project_path.parent().unwrap();
        std::fs::write(dir.join("p.secrets"), "token: default-token\n").unwrap();
        std::fs::write(dir.join("p.dev.secrets"), "token: dev-token\n").unwrap();

        let (project, _r) = Project::load_and_resolve(
            &project_path,
            None,
            None,
            &[("env".to_string(), "dev".to_string())],
            None,
            &[],
        )
        .unwrap();
        assert_eq!(
            project.apps.get("a").unwrap().env.get("K").unwrap(),
            "dev-token"
        );
    }

    #[test]
    fn secrets_usable_inside_variables_section() {
        let project_path = write_named(
            "x.yaml",
            "name: P\norg: o\nregion: par\nvariables:\n  apikey: ${secrets.real}\napps:\n  a:\n    name: x\n    kind: node\n    env:\n      K: ${apikey}\n",
        );
        let dir = project_path.parent().unwrap();
        std::fs::write(dir.join("x.secrets"), "real: super-secret\n").unwrap();

        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], None, &[]).unwrap();
        assert_eq!(
            project.apps.get("a").unwrap().env.get("K").unwrap(),
            "super-secret"
        );
    }

    #[test]
    fn cli_secret_overrides_file_secret() {
        let project_path = write_named(
            "y.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: app\n    kind: node\n    env:\n      K: ${secrets.k}\n",
        );
        let dir = project_path.parent().unwrap();
        std::fs::write(dir.join("y.secrets"), "k: from-file\n").unwrap();

        let cli_secrets = [("k".to_string(), "from-cli".to_string())];
        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], None, &cli_secrets).unwrap();
        assert_eq!(
            project.apps.get("a").unwrap().env.get("K").unwrap(),
            "from-cli"
        );
    }

    #[test]
    fn cli_secret_works_without_any_file() {
        let project_path = write_named(
            "y.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: app\n    kind: node\n    env:\n      K: ${secrets.k}\n",
        );
        let cli_secrets = [("k".to_string(), "cli-only".to_string())];
        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], None, &cli_secrets).unwrap();
        assert_eq!(
            project.apps.get("a").unwrap().env.get("K").unwrap(),
            "cli-only"
        );
    }

    #[test]
    fn secrets_explicit_path_overrides_autodiscovery() {
        let project_path = write_named(
            "y.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: ${secrets.k}-app\n    kind: node\n",
        );
        let dir = project_path.parent().unwrap();
        std::fs::write(dir.join("y.secrets"), "k: from-default\n").unwrap();
        let explicit = dir.join("custom.secrets");
        std::fs::write(&explicit, "k: from-explicit\n").unwrap();

        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], Some(&explicit), &[])
                .unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "from-explicit-app");
    }

    #[test]
    fn variables_file_yaml_loads_flat_pairs() {
        let p = write_tmp("yaml", "foo: bar\ncount: 3\nflag: true\n");
        let pairs = load_variables_file(&p).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("foo".to_string(), "bar".to_string()),
                ("count".to_string(), "3".to_string()),
                ("flag".to_string(), "true".to_string()),
            ]
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn variables_file_json_loads_flat_pairs() {
        let p = write_tmp("json", r#"{"foo":"bar","x":"y"}"#);
        let pairs = load_variables_file(&p).unwrap();
        assert!(pairs.contains(&("foo".to_string(), "bar".to_string())));
        assert!(pairs.contains(&("x".to_string(), "y".to_string())));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn variables_file_rejects_nested() {
        let p = write_tmp("yaml", "outer:\n  nested: value\n");
        let err = load_variables_file(&p).unwrap_err();
        assert!(err.to_string().contains("scalar"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn variable_path_overridden_by_explicit_variable() {
        // Simulate how apply/delete merge sources: file vars are pushed
        // first, then --variable, so --variable wins.
        let p = write_tmp("yaml", "foo: from-file\n");
        let mut combined: Vec<(String, String)> = load_variables_file(&p).unwrap();
        combined.push(("foo".to_string(), "from-cli".to_string()));
        // Build a resolver to confirm the last-write-wins behavior.
        let r = crate::interpolate::Resolver::build(
            &IndexMap::new(),
            &combined,
            "o".to_string(),
            "par".to_string(),
        )
        .unwrap();
        assert_eq!(r.resolve_string("${foo}").unwrap(), "from-cli");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn secrets_file_accepts_json_content() {
        let project_path = write_named(
            "j.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: ${secrets.apikey}\n    kind: node\n",
        );
        let dir = project_path.parent().unwrap();
        // The file is named `.secrets` but contains JSON. Should still load.
        std::fs::write(dir.join("j.secrets"), r#"{"apikey":"json-secret"}"#).unwrap();
        let (project, _r) =
            Project::load_and_resolve(&project_path, None, None, &[], None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "json-secret");
    }

    #[test]
    fn secrets_file_invalid_in_both_formats_errors() {
        let project_path = write_named(
            "bad.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: x\n    kind: node\n",
        );
        let dir = project_path.parent().unwrap();
        // Non-mapping at the root would also be rejected — but here let's
        // craft something neither parser will accept (unbalanced braces).
        std::fs::write(
            dir.join("bad.secrets"),
            "{ this is: not valid: in :: any format ]",
        )
        .unwrap();
        let err = Project::load_and_resolve(&project_path, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("YAML") || msg.contains("JSON"));
    }

    #[test]
    fn missing_secret_errors() {
        let project_path = write_named(
            "z.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: ${secrets.nope}\n    kind: node\n",
        );
        let err = Project::load_and_resolve(&project_path, None, None, &[], None, &[]).unwrap_err();
        assert!(err.to_string().contains("secrets.nope") || err.to_string().contains("nope"));
    }

    #[test]
    fn managed_addon_env_is_accepted() {
        let yaml = "name: P\norg: o\nregion: par\naddons:\n  oto:\n    name: my-oto\n    kind: otoroshi\n    env:\n      FOO: bar\n    domains:\n      - oto.example.com\n";
        let p = write_tmp("yaml", yaml);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        let addon = project.addons.get("oto").unwrap();
        assert_eq!(addon.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(addon.domains, vec!["oto.example.com".to_string()]);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn addon_prefixed_kind_is_managed() {
        let yaml = "name: P\norg: o\nregion: par\naddons:\n  pul:\n    name: bus\n    kind: addon-pulsar\n    env:\n      X: y\n";
        let p = write_tmp("yaml", yaml);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        assert!(!project.addons.get("pul").unwrap().env.is_empty());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn addon_env_on_unmanaged_kind_rejected() {
        let yaml = "name: P\norg: o\nregion: par\naddons:\n  db:\n    name: pg\n    kind: postgresql\n    env:\n      FOO: bar\n";
        let p = write_tmp("yaml", yaml);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("addon `db`"));
        assert!(msg.contains("`env`"));
        assert!(msg.contains("postgresql"));
        assert!(msg.contains("Supported managed kinds"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn addon_domains_on_unmanaged_kind_rejected() {
        let yaml = "name: P\norg: o\nregion: par\naddons:\n  db:\n    name: pg\n    kind: redis\n    domains:\n      - x.example.com\n";
        let p = write_tmp("yaml", yaml);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`domains`"));
        assert!(msg.contains("redis"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn addon_without_env_or_domains_skips_managed_check() {
        let yaml =
            "name: P\norg: o\nregion: par\naddons:\n  db:\n    name: pg\n    kind: postgresql\n";
        let p = write_tmp("yaml", yaml);
        // Plain DB addon without env/domains — must load fine.
        Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap();
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn per_env_rejects_mixed_form() {
        let mixed = r#"
name: X
org: o
region: par
variables:
  flat_thing: hello
  group:
    nested: thing
apps: {}
"#;
        let p = write_tmp("yaml", mixed);
        let err = Project::load_and_resolve(&p, None, None, &[], None, &[]).unwrap_err();
        assert!(err.to_string().contains("either"));
        std::fs::remove_file(&p).ok();
    }
}
