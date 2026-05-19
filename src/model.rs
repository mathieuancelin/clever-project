use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use tracing::debug;

use crate::interpolate::Resolver;

static SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{secrets\.([A-Za-z_][A-Za-z0-9_]*)\}").unwrap()
});

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_groups: Option<serde_yaml::Value>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub config: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env: IndexMap<String, String>,
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
}

#[derive(Debug, Clone, Copy)]
pub enum Format {
    Yaml,
    Json,
}

impl Format {
    pub fn from_path(path: &Path) -> Result<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("yaml") | Some("yml") => Ok(Format::Yaml),
            Some("json") => Ok(Format::Json),
            Some(other) => Err(anyhow!(
                "unsupported file extension `.{other}` (expected .yaml, .yml or .json)"
            )),
            None => Err(anyhow!(
                "missing file extension on `{}` (expected .yaml, .yml or .json)",
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
    /// resolved project and the resolver (the latter can be useful if a caller
    /// later needs to expand additional strings).
    pub fn load_and_resolve(
        path: &Path,
        org_override: Option<String>,
        region_override: Option<String>,
        cli_vars: &[(String, String)],
        secrets_path: Option<&Path>,
    ) -> Result<(Self, Resolver)> {
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

        let secrets = load_secrets(path, &effective_env, secrets_path)?;

        let raw_variables = map
            .remove(Value::String("variables".into()))
            .unwrap_or(Value::Null);
        let file_vars = parse_variables(&raw_variables, &effective_env)?;
        // Allow `${secrets.X}` inside variable values.
        let file_vars = expand_secrets(file_vars, &secrets)?;

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

        resolver.resolve_value(&mut value)?;

        let project: Project = serde_yaml::from_value(value)
            .with_context(|| format!("deserializing project from `{}`", path.display()))?;
        Ok((project, resolver))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let serialized = match Format::from_path(path)? {
            Format::Yaml => serde_yaml::to_string(self).context("serializing to YAML")?,
            Format::Json => serde_json::to_string_pretty(self).context("serializing to JSON")?,
        };
        std::fs::write(path, serialized)
            .with_context(|| format!("writing project file `{}`", path.display()))?;
        Ok(())
    }
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
) -> Result<IndexMap<String, String>> {
    if let Some(p) = explicit {
        return read_secrets_file(p, /* required */ true);
    }

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
    let value: Value = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing secrets file `{}`", path.display()))?;
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

/// Expand `${secrets.X}` references inside the values of the project's
/// variables section, before they're handed to the resolver. References to
/// other variables (`${foo}`) are left untouched here — they're handled by
/// the resolver during the value-tree walk.
fn expand_secrets(
    vars: IndexMap<String, String>,
    secrets: &IndexMap<String, String>,
) -> Result<IndexMap<String, String>> {
    let mut out = IndexMap::with_capacity(vars.len());
    for (k, v) in vars {
        let mut missing: Option<String> = None;
        let resolved = SECRET_RE.replace_all(&v, |caps: &regex::Captures| {
            let name = &caps[1];
            match secrets.get(name) {
                Some(val) => val.clone(),
                None => {
                    if missing.is_none() {
                        missing = Some(name.to_string());
                    }
                    String::new()
                }
            }
        });
        if let Some(name) = missing {
            bail!("undefined secret `{name}` referenced in variable `{k}`");
        }
        out.insert(k, resolved.into_owned());
    }
    Ok(out)
}

/// Load a `--variable-path FILE` as a flat list of `(key, value)` pairs.
/// Accepts YAML or JSON (detected by extension); the file must be a mapping
/// of scalars (matching the shape of `--variable foo=bar` overrides).
pub fn load_variables_file(path: &Path) -> Result<Vec<(String, String)>> {
    let _ = Format::from_path(path)?; // validate extension up front
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading variables file `{}`", path.display()))?;
    let value: Value = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing variables file `{}`", path.display()))?;
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

fn load_value(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading project file `{}`", path.display()))?;
    // JSON is a subset of YAML 1.2, so the YAML parser handles both.
    let _ = Format::from_path(path)?; // validate extension up front
    let value: Value = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing `{}`", path.display()))?;
    Ok(value)
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
    fn loads_and_resolves_spec_sample() {
        let p = write_tmp("yaml", SAMPLE);
        let (project, _r) =
            Project::load_and_resolve(&p, None, None, &[], None).expect("load failed");
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
        let err = Project::load_and_resolve(&p, None, None, &[], None).unwrap_err();
        assert!(err.to_string().contains("missing"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn json_format_works_too() {
        let json = r#"{"name":"P","org":"o","region":"par","apps":{"a":{"name":"${env}-app","kind":"node"}}}"#;
        let p = write_tmp("json", json);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "prod-app");
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
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[], None).unwrap();
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
        )
        .unwrap_err();
        assert!(err.to_string().contains("apikey"));
        std::fs::remove_file(&p).ok();
    }

    fn write_named(name: &str, contents: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("clever-project-test-{}-{}", std::process::id(), rand_suffix()));
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
            Project::load_and_resolve(&project_path, None, None, &[], None).unwrap();
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
        )
        .unwrap();
        assert_eq!(project.apps.get("a").unwrap().env.get("K").unwrap(), "dev-token");
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
            Project::load_and_resolve(&project_path, None, None, &[], None).unwrap();
        assert_eq!(project.apps.get("a").unwrap().env.get("K").unwrap(), "super-secret");
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
            Project::load_and_resolve(&project_path, None, None, &[], Some(&explicit)).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "from-explicit-app");
    }

    #[test]
    fn variables_file_yaml_loads_flat_pairs() {
        let p = write_tmp("yaml", "foo: bar\ncount: 3\nflag: true\n");
        let pairs = load_variables_file(&p).unwrap();
        assert_eq!(pairs, vec![
            ("foo".to_string(), "bar".to_string()),
            ("count".to_string(), "3".to_string()),
            ("flag".to_string(), "true".to_string()),
        ]);
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
    fn missing_secret_errors() {
        let project_path = write_named(
            "z.yaml",
            "name: P\norg: o\nregion: par\napps:\n  a:\n    name: ${secrets.nope}\n    kind: node\n",
        );
        let err = Project::load_and_resolve(&project_path, None, None, &[], None).unwrap_err();
        assert!(err.to_string().contains("secrets.nope") || err.to_string().contains("nope"));
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
        let err = Project::load_and_resolve(&p, None, None, &[], None).unwrap_err();
        assert!(err.to_string().contains("either"));
        std::fs::remove_file(&p).ok();
    }
}
