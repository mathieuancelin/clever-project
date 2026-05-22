use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Result, anyhow, bail};
use indexmap::IndexMap;
use rand::Rng;
use regex::Regex;
use serde_yaml::Value;

use crate::issues::{Issue, IssueSink};

pub const RESERVED: &[&str] = &["env", "org", "region"];

/// Upper bound on the size argument of `random_alphanumeric*` functions.
/// Prevents accidentally allocating gigabytes via a typo.
const MAX_RANDOM_SIZE: usize = 1024;

static VAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Three forms:
    //   ${name}                       — variable lookup (name may be dot-namespaced).
    //   ${name(args)}                 — function call (single identifier, no dots).
    //   ${apps.KEY.env.VAR}           — cross-resource ref, resolved in a
    //   ${addons.KEY.env.VAR}           second pass against live Clever state.
    //
    // The args capture group is `Some` only for the function form, even when
    // empty (`${ulid()}`), which is how we distinguish call vs lookup. Hyphens
    // are allowed inside identifier segments because project keys can have
    // them (e.g. `n8n-test-pg`).
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_-]*(?:\.[A-Za-z_][A-Za-z0-9_-]*)*)(?:\(([^)]*)\))?\}")
        .unwrap()
});

/// A cross-resource reference, deferred to the post-snapshot resolution
/// pass. Three shapes are supported:
///   `${apps.KEY.env.VAR}`            → `AppEnv`
///   `${addons.KEY.env.VAR}`          → `AddonEnv`
///   `${addons.KEY.addon.<dot.path>}` → `AddonMeta` (fetched from the v4
///                                       provider-specific endpoint)
pub fn parse_cross_ref(name: &str) -> Option<CrossRef> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 4 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    match (parts[0], parts[2]) {
        ("apps", "env") if parts.len() == 4 => Some(CrossRef::AppEnv {
            key: parts[1].to_string(),
            var: parts[3].to_string(),
        }),
        ("addons", "env") if parts.len() == 4 => Some(CrossRef::AddonEnv {
            key: parts[1].to_string(),
            var: parts[3].to_string(),
        }),
        ("addons", "addon") => Some(CrossRef::AddonMeta {
            key: parts[1].to_string(),
            path: parts[3..].iter().map(|s| s.to_string()).collect(),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CrossRef {
    AppEnv { key: String, var: String },
    AddonEnv { key: String, var: String },
    AddonMeta { key: String, path: Vec<String> },
}

/// Coarse classification used by the env-fetching cache: an `AppEnv` /
/// `AddonEnv` pair share a cache slot per `(kind, key)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossRefKind {
    App,
    Addon,
}

/// The same regex `Resolver` uses, exported so the post-snapshot pass can
/// re-scan strings for deferred refs without duplicating the pattern.
pub fn cross_ref_regex() -> &'static Regex {
    &VAR_RE
}

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
        // Pre-evaluate generator functions inside variable values so the
        // common pattern of declaring `slug: ${ulid_lowercase()}` works as
        // expected — the function fires ONCE at build time and every
        // `${slug}` reference gets the same value. Variable cross-refs and
        // function-name typos are left as literals here; they'll surface
        // again when the main resolver runs.
        for v in vars.values_mut() {
            *v = resolve_functions_only(v);
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

    #[allow(dead_code)]
    pub fn resolve_string(&self, s: &str) -> Result<String> {
        let (out, errors) = self.resolve_string_inner(s);
        if let Some(msg) = errors.into_iter().next() {
            return Err(anyhow!("{msg} (in `{s}`)"));
        }
        Ok(out)
    }

    /// Same as resolve_string but pushes one issue per problem encountered
    /// (undefined variable, unknown function, bad function args) and returns
    /// the partially-resolved string (with offending references left empty).
    /// Use this when the caller wants to surface every problem in one pass
    /// instead of bailing on the first one.
    pub fn resolve_string_collecting(&self, s: &str, issues: &mut Vec<Issue>) -> String {
        let (out, errors) = self.resolve_string_inner(s);
        for msg in errors {
            issues.push_issue(format!("{msg} (in `{s}`)"));
        }
        out
    }

    /// Returns `(resolved, error_messages)`. The resolved string has
    /// offending references replaced by empty so the walk can continue
    /// downstream. Each error message is already formatted to be
    /// human-readable (e.g. `undefined variable \`foo\``).
    fn resolve_string_inner(&self, s: &str) -> (String, Vec<String>) {
        let mut errors: Vec<String> = Vec::new();
        let mut record = |msg: String| {
            if !errors.iter().any(|m| m == &msg) {
                errors.push(msg);
            }
        };
        let result = VAR_RE.replace_all(s, |caps: &regex::Captures| {
            let name = &caps[1];
            // A captured args group means it's a function call (parens were
            // present in the source, even if empty: `${ulid()}`).
            if let Some(args_match) = caps.get(2) {
                let args = args_match.as_str();
                match call_function(name, args) {
                    Ok(v) => v,
                    Err(msg) => {
                        record(msg);
                        String::new()
                    }
                }
            } else if parse_cross_ref(name).is_some() {
                // Deferred — leave the literal `${apps.X.env.Y}` in place so
                // the post-snapshot pass can resolve it against live state.
                caps[0].to_string()
            } else {
                match self.vars.get(name) {
                    Some(v) => v.clone(),
                    None => {
                        record(format!("undefined variable `{name}`"));
                        String::new()
                    }
                }
            }
        });
        (result.into_owned(), errors)
    }

    /// Walk a YAML value and replace `${name}` inside every string. Mapping
    /// keys are left untouched. Missing references are recorded in `issues`
    /// and replaced by empty strings; the walk never aborts.
    pub fn resolve_value(&self, v: &mut Value, issues: &mut Vec<Issue>) {
        match v {
            Value::String(s) => {
                let resolved = self.resolve_string_collecting(s, issues);
                *s = resolved;
            }
            Value::Sequence(seq) => {
                for item in seq {
                    self.resolve_value(item, issues);
                }
            }
            Value::Mapping(map) => {
                for (_, val) in map.iter_mut() {
                    self.resolve_value(val, issues);
                }
            }
            _ => {}
        }
    }
}

/// Run only the function-call branch of the interpolator on `s`. Plain
/// variable lookups and deferred cross-refs are echoed back untouched —
/// the regular resolver (in `resolve_string_inner`) handles those at the
/// usage site. Used at resolver-build time to give variables a usable
/// "one-shot evaluation" semantic for generator functions.
fn resolve_functions_only(s: &str) -> String {
    VAR_RE
        .replace_all(s, |caps: &regex::Captures| {
            if let Some(args_match) = caps.get(2) {
                let name = &caps[1];
                match call_function(name, args_match.as_str()) {
                    Ok(v) => v,
                    // Echo the literal on error; the main resolver will
                    // report unknown-function / bad-args later when this
                    // variable is referenced.
                    Err(_) => caps[0].to_string(),
                }
            } else {
                caps[0].to_string()
            }
        })
        .into_owned()
}

/// Dispatch table for `${name(args)}` function calls. Each invocation
/// generates fresh values, so re-running `apply` on the same project file
/// will produce drift on every run — these are best used as one-shot
/// bootstrap helpers (initial secret, unique resource id) that you then
/// pin via `read` once they're in place.
fn call_function(name: &str, args: &str) -> std::result::Result<String, String> {
    match name {
        "ulid" => Ok(ulid::Ulid::new().to_string()),
        "ulid_lowercase" => Ok(ulid::Ulid::new().to_string().to_lowercase()),
        "uuid" => Ok(uuid::Uuid::new_v4().to_string().to_uppercase()),
        "uuid_lowercase" => Ok(uuid::Uuid::new_v4().to_string()),
        "random_alphanumeric" => {
            let size = parse_size(name, args)?;
            Ok(random_string(
                size,
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            ))
        }
        "random_alphanumeric_lowercase" => {
            let size = parse_size(name, args)?;
            Ok(random_string(size, b"abcdefghijklmnopqrstuvwxyz0123456789"))
        }
        _ => Err(format!("unknown interpolation function `{name}`")),
    }
}

fn parse_size(name: &str, args: &str) -> std::result::Result<usize, String> {
    let trimmed = args.trim();
    let size: usize = trimmed
        .parse()
        .map_err(|_| format!("function `{name}` expects a non-negative integer, got `{args}`"))?;
    if size > MAX_RANDOM_SIZE {
        return Err(format!(
            "function `{name}` size `{size}` exceeds the max of {MAX_RANDOM_SIZE}"
        ));
    }
    Ok(size)
}

fn random_string(size: usize, charset: &[u8]) -> String {
    let mut rng = rand::rng();
    (0..size)
        .map(|_| charset[rng.random_range(0..charset.len())] as char)
        .collect()
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
        let file: IndexMap<String, String> = [("foo".to_string(), "fromfile".to_string())]
            .into_iter()
            .collect();
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
        let mut v: Value =
            serde_yaml::from_str("greet: hello ${name}\nlist:\n  - ${name}\n  - other\n").unwrap();
        let mut issues = Vec::new();
        r.resolve_value(&mut v, &mut issues);
        assert!(issues.is_empty());
        let s = serde_yaml::to_string(&v).unwrap();
        assert!(s.contains("hello world"));
        assert!(s.contains("- world"));
    }

    #[test]
    fn resolve_value_accumulates_missing_vars() {
        let r = r(&[], "o", "par");
        let mut v: Value = serde_yaml::from_str("a: ${x}\nb:\n  - ${y}\n  - ${z}\n").unwrap();
        let mut issues = Vec::new();
        r.resolve_value(&mut v, &mut issues);
        assert_eq!(issues.len(), 3, "got: {issues:#?}");
    }

    #[test]
    fn ulid_function_returns_26_char_uppercase() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("id=${ulid()}").unwrap();
        let id = v.strip_prefix("id=").unwrap();
        assert_eq!(id.len(), 26);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "got `{id}`"
        );
    }

    #[test]
    fn ulid_lowercase_function_returns_26_char_lowercase() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("${ulid_lowercase()}").unwrap();
        assert_eq!(v.len(), 26);
        assert!(
            v.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "got `{v}`"
        );
    }

    #[test]
    fn uuid_function_returns_uppercase_hyphenated() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("${uuid()}").unwrap();
        assert_eq!(v.len(), 36); // 32 hex + 4 hyphens
        assert!(v.chars().filter(|c| *c == '-').count() == 4);
        assert!(
            v.chars()
                .all(|c| c == '-' || c.is_ascii_uppercase() || c.is_ascii_digit()),
            "got `{v}`"
        );
    }

    #[test]
    fn uuid_lowercase_function_returns_lowercase_hyphenated() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("${uuid_lowercase()}").unwrap();
        assert_eq!(v.len(), 36);
        assert!(
            v.chars()
                .all(|c| c == '-' || c.is_ascii_lowercase() || c.is_ascii_digit()),
            "got `{v}`"
        );
    }

    #[test]
    fn random_alphanumeric_respects_size_and_mixed_case() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("${random_alphanumeric(32)}").unwrap();
        assert_eq!(v.len(), 32);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric()), "got `{v}`");
    }

    #[test]
    fn random_alphanumeric_lowercase_respects_size_and_no_uppercase() {
        let r = r(&[], "o", "par");
        let v = r
            .resolve_string("${random_alphanumeric_lowercase(40)}")
            .unwrap();
        assert_eq!(v.len(), 40);
        assert!(
            v.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "got `{v}`"
        );
    }

    #[test]
    fn each_call_yields_a_fresh_value() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("${uuid()}/${uuid()}").unwrap();
        let (a, b) = v.split_once('/').unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn unknown_function_is_reported() {
        let r = r(&[], "o", "par");
        let err = r.resolve_string("${nope()}").unwrap_err().to_string();
        assert!(err.contains("unknown interpolation function"));
        assert!(err.contains("nope"));
    }

    #[test]
    fn bad_random_size_is_reported() {
        let r = r(&[], "o", "par");
        let err = r
            .resolve_string("${random_alphanumeric_lowercase(abc)}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("expects a non-negative integer"));
    }

    #[test]
    fn random_size_zero_is_empty() {
        let r = r(&[], "o", "par");
        let v = r.resolve_string("[${random_alphanumeric(0)}]").unwrap();
        assert_eq!(v, "[]");
    }

    #[test]
    fn random_size_over_max_is_rejected() {
        let r = r(&[], "o", "par");
        let err = r
            .resolve_string("${random_alphanumeric(100000)}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("exceeds the max"));
    }

    #[test]
    fn function_without_parens_falls_back_to_variable_lookup() {
        // `${ulid}` (no parens) is just a variable lookup, not a function
        // call. If the variable isn't defined, that's an undefined-var
        // error — NOT an unknown-function error.
        let r = r(&[], "o", "par");
        let err = r.resolve_string("${ulid}").unwrap_err().to_string();
        assert!(err.contains("undefined variable"));
    }

    #[test]
    fn function_in_variable_value_is_evaluated_once() {
        // Mimics the user's pattern: `slug: ${ulid_lowercase()}` declared
        // in `variables`, then referenced via `${slug}` in two env vars.
        // Both references must get the SAME resolved value (shared slug),
        // not the literal `${ulid_lowercase()}`.
        let r = r(&[("slug", "${ulid_lowercase()}")], "o", "par");
        let a = r.resolve_string("${slug}").unwrap();
        let b = r.resolve_string("${slug}").unwrap();
        assert_eq!(a.len(), 26, "slug not resolved, got `{a}`");
        assert!(!a.contains("${"));
        assert_eq!(a, b, "two references should share the same value");
    }

    #[test]
    fn function_inside_a_complex_string_with_hyphens_and_dots() {
        let r = r(&[], "o", "par");
        let v = r
            .resolve_string("n8n-workshop-iana-2026-${ulid_lowercase()}.cleverapps.io")
            .unwrap();
        assert!(v.starts_with("n8n-workshop-iana-2026-"));
        assert!(v.ends_with(".cleverapps.io"));
        // The ulid is 26 chars; total = "n8n-workshop-iana-2026-".len() + 26 + ".cleverapps.io".len()
        assert_eq!(
            v.len(),
            "n8n-workshop-iana-2026-".len() + 26 + ".cleverapps.io".len()
        );
    }

    #[test]
    fn function_via_resolve_value_walks_yaml() {
        let r = r(&[], "o", "par");
        let mut v: Value = serde_yaml::from_str(
            "env:\n  N8N_ENCRYPTION_KEY: ${random_alphanumeric(32)}\n  N8N_HOST: prefix-${ulid_lowercase()}.cleverapps.io\n"
        ).unwrap();
        let mut issues = Vec::new();
        r.resolve_value(&mut v, &mut issues);
        assert!(issues.is_empty(), "got issues: {issues:#?}");
        let s = serde_yaml::to_string(&v).unwrap();
        assert!(
            !s.contains("${random_alphanumeric"),
            "random not substituted, got:\n{s}"
        );
        assert!(
            !s.contains("${ulid_lowercase"),
            "ulid not substituted, got:\n{s}"
        );
    }

    #[test]
    fn variables_and_functions_can_share_a_string() {
        let r = r(&[("foo", "bar")], "o", "par");
        let v = r.resolve_string("${foo}-${ulid_lowercase()}").unwrap();
        assert!(v.starts_with("bar-"));
        assert_eq!(v.len(), "bar-".len() + 26);
    }

    #[test]
    fn resolve_value_replaces_missing_with_empty() {
        let r = r(&[("known", "ok")], "o", "par");
        let mut v: Value =
            serde_yaml::from_str("a: prefix-${unknown}-suffix\nb: ${known}").unwrap();
        let mut issues = Vec::new();
        r.resolve_value(&mut v, &mut issues);
        assert_eq!(issues.len(), 1);
        let s = serde_yaml::to_string(&v).unwrap();
        assert!(s.contains("prefix--suffix"));
        assert!(s.contains("ok"));
    }
}
