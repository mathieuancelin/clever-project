use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Result, anyhow, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde_yaml::Value;

pub const RESERVED: &[&str] = &["env", "org", "region"];

static VAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());

/// Resolves `${name}` occurrences against a flat variable map.
///
/// Substitution is single-pass: variable values are treated as literal strings
/// and are not themselves expanded.
#[derive(Debug)]
pub struct Resolver {
    vars: HashMap<String, String>,
}

impl Resolver {
    /// Build a resolver from the project file's `variables` section, CLI
    /// overrides, and the resolved values of the special variables `org` and
    /// `region`. The special variable `env` defaults to `prod` unless
    /// overridden via `--variable env=...`.
    pub fn build(
        file_vars: &IndexMap<String, String>,
        cli_vars: &[(String, String)],
        org: String,
        region: String,
    ) -> Result<Self> {
        for k in file_vars.keys() {
            if RESERVED.contains(&k.as_str()) {
                bail!(
                    "variable `{k}` is reserved and cannot be set in the project file's `variables` section"
                );
            }
        }
        let mut vars: HashMap<String, String> = file_vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in cli_vars {
            vars.insert(k.clone(), v.clone());
        }
        vars.entry("env".to_string())
            .or_insert_with(|| "prod".to_string());
        vars.insert("org".to_string(), org);
        vars.insert("region".to_string(), region);
        Ok(Self { vars })
    }

    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    pub fn resolve_string(&self, s: &str) -> Result<String> {
        let mut missing: Option<String> = None;
        let result = VAR_RE.replace_all(s, |caps: &regex::Captures| {
            let name = &caps[1];
            match self.vars.get(name) {
                Some(v) => v.clone(),
                None => {
                    if missing.is_none() {
                        missing = Some(name.to_string());
                    }
                    String::new()
                }
            }
        });
        if let Some(name) = missing {
            return Err(anyhow!("undefined variable `{name}` (in `{s}`)"));
        }
        Ok(result.into_owned())
    }

    /// Walk a YAML value and replace `${name}` inside every string. Mapping
    /// keys are left untouched.
    pub fn resolve_value(&self, v: &mut Value) -> Result<()> {
        match v {
            Value::String(s) => {
                let resolved = self.resolve_string(s)?;
                *s = resolved;
            }
            Value::Sequence(seq) => {
                for item in seq {
                    self.resolve_value(item)?;
                }
            }
            Value::Mapping(map) => {
                for (_, val) in map.iter_mut() {
                    self.resolve_value(val)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(pairs: &[(&str, &str)], org: &str, region: &str) -> Resolver {
        let file: IndexMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Resolver::build(&file, &[], org.to_string(), region.to_string()).unwrap()
    }

    #[test]
    fn resolves_simple() {
        let r = r(&[("foo", "bar")], "myorg", "par");
        assert_eq!(r.resolve_string("hello ${foo}").unwrap(), "hello bar");
    }

    #[test]
    fn resolves_special_vars() {
        let r = r(&[], "myorg", "par");
        assert_eq!(r.resolve_string("${org}/${region}").unwrap(), "myorg/par");
        assert_eq!(r.resolve_string("${env}").unwrap(), "prod");
    }

    #[test]
    fn env_override_via_cli() {
        let file = IndexMap::new();
        let r = Resolver::build(
            &file,
            &[("env".to_string(), "staging".to_string())],
            "o".to_string(),
            "par".to_string(),
        )
        .unwrap();
        assert_eq!(r.resolve_string("${env}").unwrap(), "staging");
    }

    #[test]
    fn cli_overrides_file() {
        let file: IndexMap<String, String> =
            [("foo".to_string(), "fromfile".to_string())].into_iter().collect();
        let r = Resolver::build(
            &file,
            &[("foo".to_string(), "fromcli".to_string())],
            "o".to_string(),
            "par".to_string(),
        )
        .unwrap();
        assert_eq!(r.resolve_string("${foo}").unwrap(), "fromcli");
    }

    #[test]
    fn rejects_reserved_in_file() {
        let file: IndexMap<String, String> =
            [("org".to_string(), "x".to_string())].into_iter().collect();
        let err = Resolver::build(&file, &[], "o".to_string(), "par".to_string()).unwrap_err();
        assert!(err.to_string().contains("reserved"));
    }

    #[test]
    fn missing_var_errors() {
        let r = r(&[], "o", "par");
        let err = r.resolve_string("hello ${nope}").unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn multiple_in_one_string() {
        let r = r(&[("a", "1"), ("b", "2")], "o", "par");
        assert_eq!(r.resolve_string("${a}-${b}-${a}").unwrap(), "1-2-1");
    }

    #[test]
    fn walks_value_tree() {
        let r = r(&[("name", "world")], "o", "par");
        let mut v: Value = serde_yaml::from_str("greet: hello ${name}\nlist:\n  - ${name}\n  - other\n").unwrap();
        r.resolve_value(&mut v).unwrap();
        let s = serde_yaml::to_string(&v).unwrap();
        assert!(s.contains("hello world"));
        assert!(s.contains("- world"));
    }
}
