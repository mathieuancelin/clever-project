use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::interpolate::Resolver;

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

        let file_vars: IndexMap<String, String> = match map.get(Value::String("variables".into()))
        {
            Some(Value::Mapping(m)) => {
                let mut out = IndexMap::new();
                for (k, v) in m {
                    let key = k
                        .as_str()
                        .ok_or_else(|| anyhow!("variable keys must be strings"))?
                        .to_string();
                    let val = match v {
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => b.to_string(),
                        Value::Number(n) => n.to_string(),
                        _ => bail!("variable `{key}` must be a scalar (string/number/bool)"),
                    };
                    out.insert(key, val);
                }
                out
            }
            Some(Value::Null) | None => IndexMap::new(),
            Some(_) => bail!("`variables` must be a mapping"),
        };

        let resolver = Resolver::build(&file_vars, cli_vars, org.clone(), region.clone())?;

        // Apply CLI overrides into the value tree so the deserialized Project
        // reflects them.
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
            Project::load_and_resolve(&p, None, None, &[]).expect("load failed");
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
        let err = Project::load_and_resolve(&p, None, None, &[]).unwrap_err();
        assert!(err.to_string().contains("missing"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn json_format_works_too() {
        let json = r#"{"name":"P","org":"o","region":"par","apps":{"a":{"name":"${env}-app","kind":"node"}}}"#;
        let p = write_tmp("json", json);
        let (project, _r) = Project::load_and_resolve(&p, None, None, &[]).unwrap();
        assert_eq!(project.apps.get("a").unwrap().name, "prod-app");
        std::fs::remove_file(&p).ok();
    }
}
