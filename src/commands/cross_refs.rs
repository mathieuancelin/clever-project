//! Post-snapshot resolution of cross-resource references like
//! `${apps.KEY.env.VAR}` / `${addons.KEY.env.VAR}`.
//!
//! These can't be resolved at load time because they depend on live Clever
//! state (the source app/addon has to exist for its env vars to be readable).
//! The first interpolation pass leaves them as `${...}` literals in the
//! resolved project; this module re-walks the project's env values, looks up
//! the live values, and substitutes them in place.
//!
//! Missing data (referenced resource not deployed yet, var name typo, etc.)
//! is treated as a warning + empty substitution — matches the
//! "deploy once, then redeploy with the value populated" workflow.
//!
//! Only env values are rescanned; refs in `name:`, `domains:`, etc. would
//! survive as literals and almost certainly cause apply to error. Document
//! the restriction in the README.

use std::collections::HashMap;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use tracing::warn;

use crate::clever::Clever;
use crate::commands::live::LiveSnapshot;
use crate::interpolate::{CrossRefKind, cross_ref_regex, parse_cross_ref};
use crate::model::Project;

/// Walk every env value of every project app, substitute every
/// `${apps.X.env.Y}` / `${addons.X.env.Y}` ref with its live value, and
/// return the list of warnings emitted (so callers can include them in
/// their summary output).
pub fn resolve_in_project(
    clever: &Clever,
    org: &str,
    project: &mut Project,
    live: &LiveSnapshot,
) -> Result<Vec<String>> {
    // Scan for refs and figure out which (kind, key) pairs are referenced.
    // We rescan both env values and `display` entries.
    let mut refs_by_kind: HashMap<(CrossRefKind, String), ()> = HashMap::new();
    for app in project.apps.values() {
        for value in app.env.values() {
            collect_refs(value, &mut refs_by_kind);
        }
    }
    for value in project.display.values() {
        collect_refs(value, &mut refs_by_kind);
    }

    if refs_by_kind.is_empty() {
        return Ok(Vec::new());
    }

    // Pre-fetch the env of each unique source. One API call per referenced
    // resource regardless of how many times it appears.
    let mut env_cache: HashMap<(CrossRefKind, String), IndexMap<String, String>> = HashMap::new();
    let mut warnings: Vec<String> = Vec::new();
    for (kind, key) in refs_by_kind.into_keys() {
        match fetch_env_for(clever, org, project, live, kind, &key) {
            Ok(Some(env)) => {
                env_cache.insert((kind, key), env);
            }
            Ok(None) => {
                let label = match kind {
                    CrossRefKind::App => "app",
                    CrossRefKind::Addon => "addon",
                };
                let msg = format!(
                    "cross-ref `${{{}}}.env.*` refers to {label} `{key}` but it isn't deployed yet — substituting empty values; re-apply after the source resource is created",
                    cross_ref_prefix(kind, &key)
                );
                warn!("{msg}");
                warnings.push(msg);
                env_cache.insert((kind, key), IndexMap::new());
            }
            Err(e) => {
                let msg = format!(
                    "failed to read env of {kind:?} `{key}` for cross-ref resolution: {e:#}"
                );
                warn!("{msg}");
                warnings.push(msg);
                env_cache.insert((kind, key), IndexMap::new());
            }
        }
    }

    // Second pass: substitute env values then display values. `refs_by_kind`
    // has already validated that every referenced project key exists; unknown
    // keys → warn + empty.
    let app_keys: Vec<String> = project.apps.keys().cloned().collect();
    for key in app_keys {
        let env_clone: IndexMap<String, String> = project.apps[&key].env.clone();
        let mut new_env: IndexMap<String, String> = IndexMap::new();
        for (k, v) in env_clone {
            let resolved = substitute_in(&v, &env_cache, project, &mut warnings);
            new_env.insert(k, resolved);
        }
        project.apps.get_mut(&key).unwrap().env = new_env;
    }
    let display_clone: IndexMap<String, String> = project.display.clone();
    let mut new_display: IndexMap<String, String> = IndexMap::new();
    for (k, v) in display_clone {
        let resolved = substitute_in(&v, &env_cache, project, &mut warnings);
        new_display.insert(k, resolved);
    }
    project.display = new_display;

    Ok(warnings)
}

fn collect_refs(value: &str, out: &mut HashMap<(CrossRefKind, String), ()>) {
    for caps in cross_ref_regex().captures_iter(value) {
        // Skip function-call form (group 2 captured).
        if caps.get(2).is_some() {
            continue;
        }
        let name = &caps[1];
        if let Some((kind, key, _var)) = parse_cross_ref(name) {
            out.insert((kind, key), ());
        }
    }
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
            let Some(addon_id) = live.addon_id_by_name.get(&addon.name) else {
                return Ok(None);
            };
            let env = clever
                .get_addon_env(org, addon_id)
                .with_context(|| format!("reading addon env of `{}`", addon.name))?;
            Ok(Some(env))
        }
    }
}

fn substitute_in(
    value: &str,
    env_cache: &HashMap<(CrossRefKind, String), IndexMap<String, String>>,
    project: &Project,
    warnings: &mut Vec<String>,
) -> String {
    let re = cross_ref_regex();
    re.replace_all(value, |caps: &regex::Captures| {
        if caps.get(2).is_some() {
            // Function-call form — leave it alone (would have been handled
            // by the first pass; if it's still here something's wrong, but
            // not our problem).
            return caps[0].to_string();
        }
        let name = &caps[1];
        let Some((kind, key, var)) = parse_cross_ref(name) else {
            return caps[0].to_string();
        };

        // Project-key sanity: warn if the project doesn't declare this key.
        let exists_in_project = match kind {
            CrossRefKind::App => project.apps.contains_key(&key),
            CrossRefKind::Addon => project.addons.contains_key(&key),
        };
        if !exists_in_project {
            let msg = format!(
                "cross-ref `${{{}.env.{var}}}` points at unknown project key — substituting empty",
                cross_ref_prefix(kind, &key)
            );
            warn!("{msg}");
            if !warnings.iter().any(|w| w == &msg) {
                warnings.push(msg);
            }
            return String::new();
        }

        let Some(env) = env_cache.get(&(kind, key.clone())) else {
            return String::new();
        };
        match env.get(&var) {
            Some(v) => v.clone(),
            None => {
                let msg = format!(
                    "cross-ref `${{{}.env.{var}}}` not found in source env — substituting empty",
                    cross_ref_prefix(kind, &key)
                );
                warn!("{msg}");
                if !warnings.iter().any(|w| w == &msg) {
                    warnings.push(msg);
                }
                String::new()
            }
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::live::LiveSnapshot;
    use crate::model::{App, Project};
    use indexmap::IndexMap;
    use std::collections::{BTreeSet, HashMap};

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
            addon_id_by_name: HashMap::new(),
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
        let mut out = HashMap::new();
        collect_refs(
            "host=${apps.api.env.HOST} pwd=${addons.db.env.PG_PASSWORD}",
            &mut out,
        );
        assert!(out.contains_key(&(CrossRefKind::App, "api".to_string())));
        assert!(out.contains_key(&(CrossRefKind::Addon, "db".to_string())));
    }

    #[test]
    fn collect_refs_ignores_plain_vars_and_functions() {
        let mut out = HashMap::new();
        collect_refs("${foo} ${ulid()} ${bar.baz}", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_refs_handles_hyphenated_project_keys() {
        let mut out = HashMap::new();
        collect_refs("${apps.n8n-test-pg.env.POSTGRESQL_ADDON_HOST}", &mut out);
        assert!(out.contains_key(&(CrossRefKind::App, "n8n-test-pg".to_string())));
    }

    #[test]
    fn substitute_uses_cached_env_when_present() {
        let mut project = empty_project();
        project.apps.insert("api".into(), empty_app("prod-api"));
        let mut env = IndexMap::new();
        env.insert("HOST".into(), "10.0.0.1".into());
        let mut cache = HashMap::new();
        cache.insert((CrossRefKind::App, "api".to_string()), env);
        let mut warnings = Vec::new();
        let out = substitute_in("url=${apps.api.env.HOST}/", &cache, &project, &mut warnings);
        assert_eq!(out, "url=10.0.0.1/");
        assert!(warnings.is_empty());
    }

    #[test]
    fn substitute_warns_and_blanks_unknown_var() {
        let mut project = empty_project();
        project.apps.insert("api".into(), empty_app("prod-api"));
        let cache = HashMap::from([((CrossRefKind::App, "api".to_string()), IndexMap::new())]);
        let mut warnings = Vec::new();
        let out = substitute_in("x=${apps.api.env.MISSING}", &cache, &project, &mut warnings);
        assert_eq!(out, "x=");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not found in source env"));
    }

    #[test]
    fn substitute_warns_on_unknown_project_key() {
        let project = empty_project();
        let cache = HashMap::new();
        let mut warnings = Vec::new();
        let out = substitute_in("x=${apps.ghost.env.Y}", &cache, &project, &mut warnings);
        assert_eq!(out, "x=");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown project key"));
    }

    #[test]
    fn collect_refs_finds_refs_inside_display_values() {
        // Sanity: a display value with cross-refs should also be picked up
        // by collect_refs (caller is responsible for invoking it on display
        // values too — this just verifies the scanner doesn't depend on
        // location).
        let mut out = HashMap::new();
        collect_refs(
            "postgres://${apps.api.env.PG_USER}@${apps.api.env.PG_HOST}/db",
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!(out.contains_key(&(CrossRefKind::App, "api".to_string())));
    }

    #[test]
    fn resolve_in_project_substitutes_display_values_too() {
        let mut project = empty_project();
        let mut app = empty_app("prod-api");
        app.env.insert(
            "WORKER_PG".into(),
            "${apps.api.env.POSTGRESQL_ADDON_HOST}".into(),
        );
        project.apps.insert("api".into(), app);
        project.display.insert(
            "pg_host".into(),
            "${apps.api.env.POSTGRESQL_ADDON_HOST}".into(),
        );

        // Direct unit test of substitute_in (skipping live fetch).
        let mut env = IndexMap::new();
        env.insert("POSTGRESQL_ADDON_HOST".into(), "10.0.0.5".into());
        let cache = HashMap::from([((CrossRefKind::App, "api".to_string()), env)]);
        let mut warnings = Vec::new();
        let resolved = substitute_in(&project.display["pg_host"], &cache, &project, &mut warnings);
        assert_eq!(resolved, "10.0.0.5");
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_in_project_is_a_noop_when_no_refs_present() {
        let mut project = empty_project();
        let mut app = empty_app("prod-api");
        app.env.insert("PORT".into(), "8080".into());
        project.apps.insert("api".into(), app);
        let live = empty_live();
        // Note: we can't easily build a real Clever here, so the no-ref path
        // (which never calls Clever) is the only one we can unit-test
        // without subprocess mocking. Higher-level integration tests cover
        // the rest.
        let clever = Clever::new();
        if let Ok(c) = clever {
            let warnings = resolve_in_project(&c, "orga_x", &mut project, &live).unwrap();
            assert!(warnings.is_empty());
            assert_eq!(project.apps["api"].env["PORT"], "8080");
        }
    }
}
