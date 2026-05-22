//! Post-snapshot resolution of cross-resource references.
//!
//! Three shapes are recognised in env and `display` values:
//!   `${apps.KEY.env.VAR}`              — fetch from app's live env
//!   `${addons.KEY.env.VAR}`            — fetch from addon's env endpoint
//!   `${addons.KEY.addon.dot.path}`     — fetch from the provider-specific
//!                                        v4 metadata endpoint and walk into
//!                                        the JSON tree
//!
//! These can't be resolved at load time because they depend on live Clever
//! state. The first interpolation pass leaves them as `${...}` literals;
//! this module re-walks the project's env values + `display` block, looks
//! up the live values, and substitutes them in place.
//!
//! Missing data (referenced resource not deployed yet, path not in JSON,
//! var name typo, etc.) is treated as a warning + empty substitution —
//! matches the "deploy once, then redeploy with the value populated"
//! workflow. Only env values and `display` are rescanned; refs in `name:`,
//! `domains:`, etc. would survive as literals.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use indexmap::IndexMap;
use tracing::warn;

use crate::clever::Clever;
use crate::commands::live::LiveSnapshot;
use crate::interpolate::{CrossRef, CrossRefKind, cross_ref_regex, parse_cross_ref};
use crate::model::Project;

/// Walk every env value of every project app + every `display` value,
/// substitute every `${apps.X.env.Y}` / `${addons.X.env.Y}` /
/// `${addons.X.addon.path}` ref with its live value, and return the list
/// of warnings emitted (so callers can include them in summary output).
pub fn resolve_in_project(
    clever: &Clever,
    org: &str,
    project: &mut Project,
    live: &LiveSnapshot,
) -> Result<Vec<String>> {
    let (env_refs, meta_refs) = collect_all_refs(project);
    if env_refs.is_empty() && meta_refs.is_empty() {
        return Ok(Vec::new());
    }

    let mut warnings: Vec<String> = Vec::new();

    let env_cache = fetch_env_caches(clever, org, project, live, &env_refs, &mut warnings)?;
    let meta_cache = fetch_meta_caches(clever, project, live, &meta_refs, &mut warnings)?;

    let app_keys: Vec<String> = project.apps.keys().cloned().collect();
    for key in app_keys {
        let env_clone: IndexMap<String, String> = project.apps[&key].env.clone();
        let mut new_env: IndexMap<String, String> = IndexMap::new();
        for (k, v) in env_clone {
            let resolved = substitute_in(&v, &env_cache, &meta_cache, project, &mut warnings);
            new_env.insert(k, resolved);
        }
        project.apps.get_mut(&key).unwrap().env = new_env;
    }
    let display_clone: IndexMap<String, String> = project.display.clone();
    let mut new_display: IndexMap<String, String> = IndexMap::new();
    for (k, v) in display_clone {
        let resolved = substitute_in(&v, &env_cache, &meta_cache, project, &mut warnings);
        new_display.insert(k, resolved);
    }
    project.display = new_display;

    Ok(warnings)
}

fn collect_all_refs(project: &Project) -> (HashSet<(CrossRefKind, String)>, HashSet<String>) {
    let mut env_refs: HashSet<(CrossRefKind, String)> = HashSet::new();
    let mut meta_refs: HashSet<String> = HashSet::new();
    for app in project.apps.values() {
        for value in app.env.values() {
            collect_refs(value, &mut env_refs, &mut meta_refs);
        }
    }
    for value in project.display.values() {
        collect_refs(value, &mut env_refs, &mut meta_refs);
    }
    (env_refs, meta_refs)
}

fn collect_refs(
    value: &str,
    env_out: &mut HashSet<(CrossRefKind, String)>,
    meta_out: &mut HashSet<String>,
) {
    for caps in cross_ref_regex().captures_iter(value) {
        // Skip function-call form (group 2 captured).
        if caps.get(2).is_some() {
            continue;
        }
        match parse_cross_ref(&caps[1]) {
            Some(CrossRef::AppEnv { key, .. }) => {
                env_out.insert((CrossRefKind::App, key));
            }
            Some(CrossRef::AddonEnv { key, .. }) => {
                env_out.insert((CrossRefKind::Addon, key));
            }
            Some(CrossRef::AddonMeta { key, .. }) => {
                meta_out.insert(key);
            }
            None => {}
        }
    }
}

fn fetch_env_caches(
    clever: &Clever,
    org: &str,
    project: &Project,
    live: &LiveSnapshot,
    env_refs: &HashSet<(CrossRefKind, String)>,
    warnings: &mut Vec<String>,
) -> Result<HashMap<(CrossRefKind, String), IndexMap<String, String>>> {
    let mut cache: HashMap<(CrossRefKind, String), IndexMap<String, String>> = HashMap::new();
    for (kind, key) in env_refs {
        match fetch_env_for(clever, org, project, live, *kind, key) {
            Ok(Some(env)) => {
                cache.insert((*kind, key.clone()), env);
            }
            Ok(None) => {
                let label = match kind {
                    CrossRefKind::App => "app",
                    CrossRefKind::Addon => "addon",
                };
                let msg = format!(
                    "cross-ref `${{{}.env.*}}` refers to {label} `{key}` but it isn't deployed yet — substituting empty values; re-apply after the source resource is created",
                    cross_ref_prefix(*kind, key)
                );
                warn!("{msg}");
                warnings.push(msg);
                cache.insert((*kind, key.clone()), IndexMap::new());
            }
            Err(e) => {
                let msg = format!(
                    "failed to read env of {kind:?} `{key}` for cross-ref resolution: {e:#}"
                );
                warn!("{msg}");
                warnings.push(msg);
                cache.insert((*kind, key.clone()), IndexMap::new());
            }
        }
    }
    Ok(cache)
}

fn fetch_meta_caches(
    clever: &Clever,
    project: &Project,
    live: &LiveSnapshot,
    meta_refs: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut cache: HashMap<String, serde_json::Value> = HashMap::new();
    for key in meta_refs {
        match fetch_meta_for(clever, project, live, key) {
            Ok(Some(json)) => {
                cache.insert(key.clone(), json);
            }
            Ok(None) => {
                let msg = format!(
                    "cross-ref `${{addons.{key}.addon.*}}` refers to addon `{key}` but it isn't deployed yet — substituting empty values; re-apply after the source resource is created",
                );
                warn!("{msg}");
                warnings.push(msg);
                cache.insert(key.clone(), serde_json::Value::Null);
            }
            Err(e) => {
                let msg = format!(
                    "failed to read addon metadata for `{key}`: {e:#} — substituting empty values"
                );
                warn!("{msg}");
                warnings.push(msg);
                cache.insert(key.clone(), serde_json::Value::Null);
            }
        }
    }
    Ok(cache)
}

fn cross_ref_prefix(kind: CrossRefKind, key: &str) -> String {
    match kind {
        CrossRefKind::App => format!("apps.{key}"),
        CrossRefKind::Addon => format!("addons.{key}"),
    }
}

/// Returns `Ok(Some(env))` if the source resource exists, `Ok(None)` if it
/// doesn't (yet), or `Err` on a real API failure.
fn fetch_env_for(
    clever: &Clever,
    org: &str,
    project: &Project,
    live: &LiveSnapshot,
    kind: CrossRefKind,
    key: &str,
) -> Result<Option<IndexMap<String, String>>> {
    match kind {
        CrossRefKind::App => {
            let Some(app) = project.apps.get(key) else {
                return Ok(None);
            };
            let Some(app_id) = live.app_id_by_name.get(&app.name) else {
                return Ok(None);
            };
            let env = clever
                .get_env_full(app_id)
                .with_context(|| format!("reading full env of app `{}`", app.name))?
                .merged();
            Ok(Some(env))
        }
        CrossRefKind::Addon => {
            let Some(addon) = project.addons.get(key) else {
                return Ok(None);
            };
            let Some(lookup) = live.addon_lookup_by_name.get(&addon.name) else {
                return Ok(None);
            };
            let env = clever
                .get_addon_env(org, &lookup.addon_id)
                .with_context(|| format!("reading addon env of `{}`", addon.name))?;
            Ok(Some(env))
        }
    }
}

fn fetch_meta_for(
    clever: &Clever,
    project: &Project,
    live: &LiveSnapshot,
    key: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(addon) = project.addons.get(key) else {
        return Ok(None);
    };
    let Some(lookup) = live.addon_lookup_by_name.get(&addon.name) else {
        return Ok(None);
    };
    let json = clever
        .get_addon_meta(&lookup.provider_id, &lookup.real_id)
        .with_context(|| {
            format!(
                "reading addon metadata for `{}` ({} / {})",
                addon.name, lookup.provider_id, lookup.real_id
            )
        })?;
    Ok(Some(json))
}

fn substitute_in(
    value: &str,
    env_cache: &HashMap<(CrossRefKind, String), IndexMap<String, String>>,
    meta_cache: &HashMap<String, serde_json::Value>,
    project: &Project,
    warnings: &mut Vec<String>,
) -> String {
    let re = cross_ref_regex();
    re.replace_all(value, |caps: &regex::Captures| {
        if caps.get(2).is_some() {
            return caps[0].to_string();
        }
        let name = &caps[1];
        let Some(parsed) = parse_cross_ref(name) else {
            return caps[0].to_string();
        };
        match parsed {
            CrossRef::AppEnv { key, var } => {
                substitute_env(CrossRefKind::App, &key, &var, env_cache, project, warnings)
            }
            CrossRef::AddonEnv { key, var } => substitute_env(
                CrossRefKind::Addon,
                &key,
                &var,
                env_cache,
                project,
                warnings,
            ),
            CrossRef::AddonMeta { key, path } => {
                substitute_meta(&key, &path, meta_cache, project, warnings)
            }
        }
    })
    .into_owned()
}

fn substitute_env(
    kind: CrossRefKind,
    key: &str,
    var: &str,
    env_cache: &HashMap<(CrossRefKind, String), IndexMap<String, String>>,
    project: &Project,
    warnings: &mut Vec<String>,
) -> String {
    let exists_in_project = match kind {
        CrossRefKind::App => project.apps.contains_key(key),
        CrossRefKind::Addon => project.addons.contains_key(key),
    };
    if !exists_in_project {
        push_warning(
            warnings,
            format!(
                "cross-ref `${{{}.env.{var}}}` points at unknown project key — substituting empty",
                cross_ref_prefix(kind, key)
            ),
        );
        return String::new();
    }
    let Some(env) = env_cache.get(&(kind, key.to_string())) else {
        return String::new();
    };
    match env.get(var) {
        Some(v) => v.clone(),
        None => {
            push_warning(
                warnings,
                format!(
                    "cross-ref `${{{}.env.{var}}}` not found in source env — substituting empty",
                    cross_ref_prefix(kind, key)
                ),
            );
            String::new()
        }
    }
}

fn substitute_meta(
    key: &str,
    path: &[String],
    meta_cache: &HashMap<String, serde_json::Value>,
    project: &Project,
    warnings: &mut Vec<String>,
) -> String {
    if !project.addons.contains_key(key) {
        push_warning(
            warnings,
            format!(
                "cross-ref `${{addons.{key}.addon.{}}}` points at unknown project key — substituting empty",
                path.join(".")
            ),
        );
        return String::new();
    }
    let Some(json) = meta_cache.get(key) else {
        return String::new();
    };
    if json.is_null() {
        // already-warned source resource
        return String::new();
    }
    match walk_path(json, path) {
        Some(scalar) => scalar,
        None => {
            push_warning(
                warnings,
                format!(
                    "cross-ref `${{addons.{key}.addon.{}}}` not found in addon metadata — substituting empty",
                    path.join(".")
                ),
            );
            String::new()
        }
    }
}

/// Walk a dot-path into a JSON tree. Returns `Some(scalar)` if the path
/// resolves to a string / number / bool. Returns `None` if any segment is
/// missing or if the terminal value is a complex type (object / array /
/// null) we can't render as a string.
fn walk_path(root: &serde_json::Value, path: &[String]) -> Option<String> {
    let mut cur = root;
    for seg in path {
        cur = match cur {
            serde_json::Value::Object(m) => m.get(seg)?,
            _ => return None,
        };
    }
    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn push_warning(warnings: &mut Vec<String>, msg: String) {
    warn!("{msg}");
    if !warnings.iter().any(|w| w == &msg) {
        warnings.push(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::live::{AddonLookup, LiveSnapshot};
    use crate::model::{App, Project};
    use indexmap::IndexMap;
    use std::collections::{BTreeSet, HashMap, HashSet};

    fn empty_live() -> LiveSnapshot {
        LiveSnapshot {
            apps: IndexMap::new(),
            addons: IndexMap::new(),
            network_groups: IndexMap::new(),
            default_region: "par".into(),
            live_app_names: BTreeSet::new(),
            live_addon_names: BTreeSet::new(),
            live_ng_names: BTreeSet::new(),
            app_id_by_name: HashMap::new(),
            addon_lookup_by_name: HashMap::new(),
        }
    }

    fn empty_project() -> Project {
        Project {
            name: "p".into(),
            description: None,
            org: "o".into(),
            region: "par".into(),
            variables: IndexMap::new(),
            apps: IndexMap::new(),
            addons: IndexMap::new(),
            network_groups: IndexMap::new(),
            hooks: None,
            display: IndexMap::new(),
        }
    }

    fn empty_app(name: &str) -> App {
        App {
            name: name.into(),
            kind: "node".into(),
            region: None,
            source: None,
            domains: vec![],
            scalability: None,
            build: None,
            dependencies: vec![],
            config: IndexMap::new(),
            env: IndexMap::new(),
            hooks: None,
        }
    }

    #[test]
    fn collect_refs_finds_apps_and_addons() {
        let mut env = HashSet::new();
        let mut meta = HashSet::new();
        collect_refs(
            "host=${apps.api.env.HOST} pwd=${addons.db.env.PG_PASSWORD}",
            &mut env,
            &mut meta,
        );
        assert!(env.contains(&(CrossRefKind::App, "api".to_string())));
        assert!(env.contains(&(CrossRefKind::Addon, "db".to_string())));
        assert!(meta.is_empty());
    }

    #[test]
    fn collect_refs_finds_addon_metadata() {
        let mut env = HashSet::new();
        let mut meta = HashSet::new();
        collect_refs(
            "u=${addons.otoroshi.addon.initialCredentials.user} api=${addons.otoroshi.addon.api.url}",
            &mut env,
            &mut meta,
        );
        assert!(env.is_empty());
        assert_eq!(meta.len(), 1);
        assert!(meta.contains("otoroshi"));
    }

    #[test]
    fn collect_refs_ignores_plain_vars_and_functions() {
        let mut env = HashSet::new();
        let mut meta = HashSet::new();
        collect_refs("${foo} ${ulid()} ${bar.baz}", &mut env, &mut meta);
        assert!(env.is_empty());
        assert!(meta.is_empty());
    }

    #[test]
    fn substitute_uses_cached_env_when_present() {
        let mut project = empty_project();
        project.apps.insert("api".into(), empty_app("prod-api"));
        let mut env = IndexMap::new();
        env.insert("HOST".into(), "10.0.0.1".into());
        let env_cache = HashMap::from([((CrossRefKind::App, "api".to_string()), env)]);
        let meta_cache = HashMap::new();
        let mut warnings = Vec::new();
        let out = substitute_in(
            "url=${apps.api.env.HOST}/",
            &env_cache,
            &meta_cache,
            &project,
            &mut warnings,
        );
        assert_eq!(out, "url=10.0.0.1/");
        assert!(warnings.is_empty());
    }

    #[test]
    fn substitute_warns_and_blanks_unknown_var() {
        let mut project = empty_project();
        project.apps.insert("api".into(), empty_app("prod-api"));
        let env_cache = HashMap::from([((CrossRefKind::App, "api".to_string()), IndexMap::new())]);
        let meta_cache = HashMap::new();
        let mut warnings = Vec::new();
        let out = substitute_in(
            "x=${apps.api.env.MISSING}",
            &env_cache,
            &meta_cache,
            &project,
            &mut warnings,
        );
        assert_eq!(out, "x=");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not found in source env"));
    }

    #[test]
    fn substitute_meta_walks_json_path() {
        let mut project = empty_project();
        project.addons.insert(
            "oto".into(),
            crate::model::Addon {
                name: "oto-addon".into(),
                kind: "addon-otoroshi".into(),
                size: None,
                crypted: false,
                region: None,
                version: None,
                backup_path: None,
            },
        );
        let json = serde_json::json!({
            "initialCredentials": {
                "user": "cc-admin",
                "password": "p@ss"
            },
            "api": { "url": "https://api.example" }
        });
        let env_cache = HashMap::new();
        let meta_cache = HashMap::from([("oto".to_string(), json)]);
        let mut warnings = Vec::new();
        let out = substitute_in(
            "u=${addons.oto.addon.initialCredentials.user} url=${addons.oto.addon.api.url}",
            &env_cache,
            &meta_cache,
            &project,
            &mut warnings,
        );
        assert_eq!(out, "u=cc-admin url=https://api.example");
        assert!(warnings.is_empty());
    }

    #[test]
    fn substitute_meta_warns_on_unknown_path() {
        let mut project = empty_project();
        project.addons.insert(
            "oto".into(),
            crate::model::Addon {
                name: "oto-addon".into(),
                kind: "addon-otoroshi".into(),
                size: None,
                crypted: false,
                region: None,
                version: None,
                backup_path: None,
            },
        );
        let json = serde_json::json!({ "api": { "url": "x" } });
        let env_cache = HashMap::new();
        let meta_cache = HashMap::from([("oto".to_string(), json)]);
        let mut warnings = Vec::new();
        let out = substitute_in(
            "${addons.oto.addon.nope.deep.path}",
            &env_cache,
            &meta_cache,
            &project,
            &mut warnings,
        );
        assert_eq!(out, "");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not found in addon metadata"));
    }

    #[test]
    fn substitute_meta_rejects_complex_terminal() {
        // Path that resolves to an OBJECT (not a scalar) — can't stringify.
        let mut project = empty_project();
        project.addons.insert(
            "oto".into(),
            crate::model::Addon {
                name: "oto-addon".into(),
                kind: "addon-otoroshi".into(),
                size: None,
                crypted: false,
                region: None,
                version: None,
                backup_path: None,
            },
        );
        let json = serde_json::json!({ "api": { "url": "x" } });
        let meta_cache = HashMap::from([("oto".to_string(), json)]);
        let env_cache = HashMap::new();
        let mut warnings = Vec::new();
        let out = substitute_in(
            "${addons.oto.addon.api}", // resolves to object
            &env_cache,
            &meta_cache,
            &project,
            &mut warnings,
        );
        assert_eq!(out, "");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("not found in addon metadata"))
        );
    }

    #[test]
    fn collect_refs_handles_hyphenated_project_keys() {
        let mut env = HashSet::new();
        let mut meta = HashSet::new();
        collect_refs(
            "${apps.n8n-test-pg.env.POSTGRESQL_ADDON_HOST}",
            &mut env,
            &mut meta,
        );
        assert!(env.contains(&(CrossRefKind::App, "n8n-test-pg".to_string())));
    }

    #[test]
    fn resolve_in_project_is_a_noop_when_no_refs_present() {
        let mut project = empty_project();
        let mut app = empty_app("prod-api");
        app.env.insert("PORT".into(), "8080".into());
        project.apps.insert("api".into(), app);
        let live = empty_live();
        let clever = Clever::new();
        if let Ok(c) = clever {
            let warnings = resolve_in_project(&c, "orga_x", &mut project, &live).unwrap();
            assert!(warnings.is_empty());
            assert_eq!(project.apps["api"].env["PORT"], "8080");
        }
    }

    // Touch AddonLookup so its fields are exercised at least once in tests
    // (silences dead-code warnings if no other test reaches them).
    #[test]
    fn addon_lookup_holds_three_ids() {
        let l = AddonLookup {
            addon_id: "addon_x".into(),
            real_id: "redis_x".into(),
            provider_id: "redis-addon".into(),
        };
        assert_eq!(l.addon_id, "addon_x");
        assert_eq!(l.real_id, "redis_x");
        assert_eq!(l.provider_id, "redis-addon");
    }
}
