use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{info, warn};

use crate::cli::ApplyArgs;
use crate::clever::{Clever, CreateAddon, CreateApp, ListedAddon, ListedApp};
use crate::commands::OrgCache;
use crate::model::{App, Project, Source};
use crate::state::{ResourceKind, State, StateResource};

pub fn run(args: ApplyArgs) -> Result<()> {
    let mut variables: Vec<(String, String)> = Vec::new();
    for path in &args.variable_paths {
        variables.extend(
            crate::model::load_variables_file(path)
                .with_context(|| format!("loading --variable-path `{}`", path.display()))?,
        );
    }
    variables.extend(args.variables);
    if let Some(env) = args.env {
        variables.push(("env".to_string(), env));
    }
    let (project, _resolver) = Project::load_and_resolve(
        &args.file,
        args.org,
        args.region,
        &variables,
        args.secrets_path.as_deref(),
    )
    .with_context(|| format!("loading project `{}`", args.file.display()))?;

    let clever = Clever::new()?.with_dry_run(args.dry_run);
    if clever.is_dry_run() {
        info!("[dry-run] no mutations will be sent to Clever Cloud");
    }

    let mut state = State::load(&args.file)?;
    let effective_env = variables
        .iter()
        .rev()
        .find(|(k, _)| k == "env")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "prod".to_string());

    let mut cache = OrgCache::new();

    // Phase 1 — addons.
    let mut addon_id_by_key: HashMap<String, String> = HashMap::new();
    for (key, addon) in &project.addons {
        let id = handle_addon(
            &clever,
            &mut state,
            &mut cache,
            &project,
            &effective_env,
            key,
            addon,
        )?;
        addon_id_by_key.insert(key.clone(), id);
    }

    // Phase 2 — apps.
    let mut app_id_by_key: HashMap<String, String> = HashMap::new();
    let mut apps_to_link: Vec<(String, &App)> = Vec::new();

    for (key, app) in &project.apps {
        let id = handle_app(
            &clever,
            &mut state,
            &mut cache,
            &project,
            &effective_env,
            key,
            app,
        )?;
        app_id_by_key.insert(key.clone(), id);
        apps_to_link.push((key.clone(), app));
    }

    // Phase 3 — service links. Wrapped in a one-shot retry: if anything
    // fails (likely due to a stale id pulled from state), refresh state
    // against fresh listings, rebuild the dep maps, and try again.
    let phase3 = || -> Result<()> {
        for (key, app) in &apps_to_link {
            let app_id = &app_id_by_key[key];
            sync_dependencies(
                &clever,
                app_id,
                &app.dependencies,
                &app_id_by_key,
                &addon_id_by_key,
                &project,
            )
            .with_context(|| format!("syncing dependencies of app `{}`", app.name))?;
        }
        Ok(())
    };

    if let Err(e) = phase3() {
        warn!(
            "phase 3 (service links) failed: {e:#} — refreshing state against clever and retrying once"
        );
        refresh_dep_maps(
            &clever,
            &mut state,
            &mut cache,
            &project,
            &effective_env,
            &mut app_id_by_key,
            &mut addon_id_by_key,
        )?;
        // Persist whatever the refresh learned, so the next run starts clean
        // even if the retry below still fails.
        if !clever.is_dry_run() {
            state
                .save()
                .with_context(|| format!("saving state file `{}`", state.path().display()))?;
        }
        for (key, app) in &apps_to_link {
            let app_id = &app_id_by_key[key];
            sync_dependencies(
                &clever,
                app_id,
                &app.dependencies,
                &app_id_by_key,
                &addon_id_by_key,
                &project,
            )
            .with_context(|| format!("syncing dependencies of app `{}` (retry)", app.name))?;
        }
    }

    if !clever.is_dry_run() {
        state
            .save()
            .with_context(|| format!("saving state file `{}`", state.path().display()))?;
    }

    info!("apply complete");
    Ok(())
}

fn handle_addon(
    clever: &Clever,
    state: &mut State,
    cache: &mut OrgCache,
    project: &Project,
    env: &str,
    key: &str,
    addon: &crate::model::Addon,
) -> Result<String> {
    // State first. Addons aren't updated, so there's no operation we could
    // use to validate the entry — staleness will surface in phase 3 if
    // someone tries to link this addon, and we retry there.
    if let Some(r) = state.find(ResourceKind::Addon, &addon.name, &project.org) {
        info!(
            "addon `{}` known from state ({}), leaving untouched [project key: {key}]",
            addon.name, r.id
        );
        return Ok(r.id.clone());
    }

    // Listing path.
    let region = addon.region.as_deref().unwrap_or(&project.region);
    let listed = cache.addons(clever, &project.org)?;
    if let Some(found) = listed.get(&addon.name).cloned() {
        let resolved = resolve_provider(&addon.kind);
        if !found.provider_id.eq_ignore_ascii_case(resolved)
            && !found.kind.eq_ignore_ascii_case(&addon.kind)
        {
            warn!(
                "addon `{}` exists with provider `{}` but project declares `{}` — leaving as-is",
                addon.name, found.provider_id, addon.kind
            );
        }
        info!(
            "addon `{}` already exists ({}), leaving untouched [project key: {key}]",
            addon.name, found.addon_id
        );
        if !clever.is_dry_run() {
            state.upsert(StateResource {
                kind: ResourceKind::Addon,
                id: found.addon_id.clone(),
                org_id: project.org.clone(),
                region: found.region.clone(),
                env: env.to_string(),
                name: addon.name.clone(),
            });
        }
        return Ok(found.addon_id);
    }

    // Create.
    let version_string = addon
        .version
        .as_ref()
        .map(yaml_scalar_to_string)
        .transpose()?;
    let provider = resolve_provider(&addon.kind);
    info!("creating addon `{}` [project key: {key}]", addon.name);
    let id = clever.create_addon(&CreateAddon {
        provider,
        name: &addon.name,
        org: &project.org,
        region,
        plan: addon.size.as_deref(),
        version: version_string.as_deref(),
        crypted: addon.crypted,
    })?;
    if !clever.is_dry_run() {
        state.upsert(StateResource {
            kind: ResourceKind::Addon,
            id: id.clone(),
            org_id: project.org.clone(),
            region: region.to_string(),
            env: env.to_string(),
            name: addon.name.clone(),
        });
    }
    Ok(id)
}

fn handle_app(
    clever: &Clever,
    state: &mut State,
    cache: &mut OrgCache,
    project: &Project,
    env: &str,
    key: &str,
    app: &App,
) -> Result<String> {
    // State first. We validate the entry by running update_app — if any
    // call there fails (typically because the id no longer exists), drop
    // the state entry, invalidate the cache, and fall through to the
    // listing path.
    if let Some(r) = state.find(ResourceKind::App, &app.name, &project.org) {
        let id = r.id.clone();
        info!(
            "updating app `{}` (from state, {id}) [project key: {key}]",
            app.name
        );
        match update_app(clever, &id, app) {
            Ok(()) => return Ok(id),
            Err(e) => {
                warn!(
                    "state hit for app `{}` (id={id}) but update failed: {e:#} — dropping stale state entry and refreshing from clever",
                    app.name
                );
                state.remove_by_id(&id);
                cache.invalidate();
            }
        }
    }

    let region = app.region.as_deref().unwrap_or(&project.region);
    let listed = cache.apps(clever, &project.org)?;
    if let Some(found) = listed.get(&app.name).cloned() {
        if !kinds_match(&found.kind, &app.kind) {
            warn!(
                "app `{}` exists with kind `{}` but project declares `{}` — skipping update",
                app.name, found.kind, app.kind
            );
            return Ok(found.app_id);
        }
        if !source_matches(found.deploy_url.as_deref(), app.source.as_ref()) {
            warn!(
                "app `{}` source diverges (clever: {:?}, project: {:?}) — skipping update",
                app.name, found.deploy_url, app.source
            );
            return Ok(found.app_id);
        }
        info!(
            "updating app `{}` ({}) [project key: {key}]",
            app.name, found.app_id
        );
        if !clever.is_dry_run() {
            state.upsert(StateResource {
                kind: ResourceKind::App,
                id: found.app_id.clone(),
                org_id: project.org.clone(),
                region: found.zone.clone(),
                env: env.to_string(),
                name: app.name.clone(),
            });
        }
        update_app(clever, &found.app_id, app)?;
        return Ok(found.app_id);
    }

    // Create.
    info!("creating app `{}` [project key: {key}]", app.name);
    let github = app
        .source
        .as_ref()
        .map(|s| parse_github(&s.from))
        .transpose()?
        .flatten();
    let id = clever.create_app(&CreateApp {
        name: &app.name,
        kind: &app.kind,
        org: &project.org,
        region,
        github: github.as_deref(),
    })?;
    if app.source.is_some() && github.is_none() {
        warn!(
            "app `{}` source is not a github URL — app created empty, you'll need to deploy the code manually",
            app.name
        );
    }
    if !clever.is_dry_run() {
        state.upsert(StateResource {
            kind: ResourceKind::App,
            id: id.clone(),
            org_id: project.org.clone(),
            region: region.to_string(),
            env: env.to_string(),
            name: app.name.clone(),
        });
    }
    update_app(clever, &id, app)?;
    Ok(id)
}

/// Force-refresh state against fresh clever listings: every project resource
/// known to state is verified to actually exist; stale entries are removed
/// and replaced by whatever the listing reports under the same `name`. Dep
/// maps are rebuilt from the corrected state.
fn refresh_dep_maps(
    clever: &Clever,
    state: &mut State,
    cache: &mut OrgCache,
    project: &Project,
    env: &str,
    app_id_by_key: &mut HashMap<String, String>,
    addon_id_by_key: &mut HashMap<String, String>,
) -> Result<()> {
    cache.invalidate();

    // Materialize the listings up front so we can hold mutable refs to state.
    let live_apps: HashMap<String, ListedApp> = cache.apps(clever, &project.org)?.clone();
    let live_addons: HashMap<String, ListedAddon> = cache.addons(clever, &project.org)?.clone();

    for (key, addon) in &project.addons {
        let prev_id = state
            .find(ResourceKind::Addon, &addon.name, &project.org)
            .map(|r| r.id.clone());
        match live_addons.get(&addon.name) {
            Some(found) => {
                if prev_id.as_deref() != Some(&found.addon_id) {
                    if let Some(id) = &prev_id {
                        state.remove_by_id(id);
                    }
                    state.upsert(StateResource {
                        kind: ResourceKind::Addon,
                        id: found.addon_id.clone(),
                        org_id: project.org.clone(),
                        region: found.region.clone(),
                        env: env.to_string(),
                        name: addon.name.clone(),
                    });
                }
                addon_id_by_key.insert(key.clone(), found.addon_id.clone());
            }
            None => {
                if let Some(id) = prev_id {
                    state.remove_by_id(&id);
                }
                addon_id_by_key.remove(key);
                warn!(
                    "addon `{}` referenced by project key `{key}` not found in org `{}` after refresh",
                    addon.name, project.org
                );
            }
        }
    }

    for (key, app) in &project.apps {
        let prev_id = state
            .find(ResourceKind::App, &app.name, &project.org)
            .map(|r| r.id.clone());
        match live_apps.get(&app.name) {
            Some(found) => {
                if prev_id.as_deref() != Some(&found.app_id) {
                    if let Some(id) = &prev_id {
                        state.remove_by_id(id);
                    }
                    state.upsert(StateResource {
                        kind: ResourceKind::App,
                        id: found.app_id.clone(),
                        org_id: project.org.clone(),
                        region: found.zone.clone(),
                        env: env.to_string(),
                        name: app.name.clone(),
                    });
                }
                app_id_by_key.insert(key.clone(), found.app_id.clone());
            }
            None => {
                if let Some(id) = prev_id {
                    state.remove_by_id(&id);
                }
                app_id_by_key.remove(key);
                warn!(
                    "app `{}` referenced by project key `{key}` not found in org `{}` after refresh",
                    app.name, project.org
                );
            }
        }
    }

    Ok(())
}

fn kinds_match(clever_kind: &str, project_kind: &str) -> bool {
    let a = clever_kind.to_lowercase();
    let b = project_kind.to_lowercase();
    if a == b {
        return true;
    }
    // Clever lists Java apps as `jar`; users commonly write `java`.
    matches!((a.as_str(), b.as_str()), ("jar", "java") | ("java", "jar"))
}

fn source_matches(deploy_url: Option<&str>, source: Option<&Source>) -> bool {
    match (deploy_url, source) {
        (_, None) => true, // project doesn't pin a source -> never blocks updates
        (None, Some(_)) => false,
        (Some(d), Some(s)) => normalize_git_url(d) == normalize_git_url(&s.from),
    }
}

fn normalize_git_url(url: &str) -> String {
    let lower = url.trim().trim_end_matches('/').to_lowercase();
    lower.strip_suffix(".git").unwrap_or(&lower).to_string()
}

/// Extract `owner/repo` from a GitHub URL. Returns `None` for non-github URLs.
fn parse_github(url: &str) -> Result<Option<String>> {
    let s = url.trim();
    let lower = s.to_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://github.com/") {
        r
    } else if let Some(r) = lower.strip_prefix("git@github.com:") {
        r
    } else {
        return Ok(None);
    };
    let offset = s.len() - rest.len();
    let original = &s[offset..];
    let trimmed = original
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(original.trim_end_matches('/'));
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!("can't extract owner/repo from github URL `{url}`");
    }
    Ok(Some(format!("{}/{}", parts[0], parts[1])))
}

fn update_app(clever: &Clever, app_id: &str, app: &App) -> Result<()> {
    // env — full replace (also clears variables when env is empty).
    clever.env_replace(app_id, &app.env)?;

    // domains — diff against current state when we have a real app id;
    // for a freshly-created app (real or dry-run) just add the desired set.
    let desired: HashSet<String> = app.domains.iter().cloned().collect();
    if is_synthetic(app_id) {
        for d in &desired {
            clever.domain_add(app_id, d)?;
        }
    } else {
        let current: HashSet<String> = clever
            .get_domains(app_id)?
            .into_iter()
            .map(|d| d.hostname)
            .collect();
        for d in desired.difference(&current) {
            clever.domain_add(app_id, d)?;
        }
        for d in current.difference(&desired) {
            // Auto-managed *.cleverapps.io domains can't be removed.
            if d.ends_with(".cleverapps.io") {
                continue;
            }
            clever.domain_rm(app_id, d)?;
        }
    }

    // scalability.
    if let Some(scale) = &app.scalability {
        clever.scale(app_id, scale)?;
    }

    Ok(())
}

fn is_synthetic(id: &str) -> bool {
    id.starts_with("dry-run::")
}

fn sync_dependencies(
    clever: &Clever,
    app_id: &str,
    dependencies: &[String],
    app_id_by_key: &HashMap<String, String>,
    addon_id_by_key: &HashMap<String, String>,
    project: &Project,
) -> Result<()> {
    let mut desired_apps: HashSet<String> = HashSet::new();
    let mut desired_addons: HashSet<String> = HashSet::new();
    for dep_key in dependencies {
        if let Some(id) = app_id_by_key.get(dep_key) {
            if id != app_id {
                desired_apps.insert(id.clone());
            }
        } else if let Some(id) = addon_id_by_key.get(dep_key) {
            desired_addons.insert(id.clone());
        } else {
            return Err(anyhow!(
                "dependency `{dep_key}` references neither an app nor an addon in the project"
            ));
        }
    }
    // Special: dependency name may refer to a resource by *name* rather than
    // project key. Fall back to looking it up there too.
    for dep_key in dependencies {
        if app_id_by_key.contains_key(dep_key) || addon_id_by_key.contains_key(dep_key) {
            continue;
        }
        if let Some(app) = project.apps.values().find(|a| a.name == *dep_key) {
            warn!("dependency `{dep_key}` matched by name on app `{}`", app.name);
        }
    }

    let (current_apps, current_addons): (HashSet<String>, HashSet<String>) = if is_synthetic(app_id)
    {
        // Freshly created in dry-run: no existing links to read.
        (HashSet::new(), HashSet::new())
    } else {
        let services = clever.get_services(app_id)?;
        (
            services.applications.iter().map(|s| s.id.clone()).collect(),
            services.addons.iter().map(|s| s.id.clone()).collect(),
        )
    };

    for id in desired_addons.difference(&current_addons) {
        clever.link_addon(app_id, id)?;
    }
    for id in current_addons.difference(&desired_addons) {
        clever.unlink_addon(app_id, id)?;
    }
    for id in desired_apps.difference(&current_apps) {
        clever.link_app(app_id, id)?;
    }
    for id in current_apps.difference(&desired_apps) {
        clever.unlink_app(app_id, id)?;
    }

    Ok(())
}

/// Map a user-friendly `kind` from the project file to the provider id
/// expected by `clever addon create`. Values not in the table pass through
/// unchanged (so users can also write the full `xxx-addon` form directly).
fn resolve_provider(kind: &str) -> &str {
    match kind.to_lowercase().as_str() {
        "postgresql" | "postgres" | "pg" => "postgresql-addon",
        "mysql" => "mysql-addon",
        "redis" => "redis-addon",
        "mongodb" | "mongo" => "mongodb-addon",
        "elasticsearch" | "es" => "es-addon",
        "cellar" | "s3" => "cellar-addon",
        "matomo" => "addon-matomo",
        "pulsar" => "addon-pulsar",
        _ => kind,
    }
}

fn yaml_scalar_to_string(v: &serde_yaml::Value) -> Result<String> {
    match v {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        _ => bail!("expected a scalar (string/number/bool), got `{v:?}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_https() {
        assert_eq!(
            parse_github("https://github.com/MAIF/otoroshi.git").unwrap(),
            Some("MAIF/otoroshi".to_string())
        );
        assert_eq!(
            parse_github("https://github.com/cloud-apim/X").unwrap(),
            Some("cloud-apim/X".to_string())
        );
    }

    #[test]
    fn parses_github_ssh() {
        assert_eq!(
            parse_github("git@github.com:Foo/Bar.git").unwrap(),
            Some("Foo/Bar".to_string())
        );
    }

    #[test]
    fn returns_none_for_non_github() {
        assert_eq!(parse_github("https://gitlab.com/x/y.git").unwrap(), None);
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_git_url("https://github.com/MAIF/otoroshi.git"),
            normalize_git_url("https://github.com/maif/otoroshi/")
        );
    }

    #[test]
    fn kinds_match_jar_java() {
        assert!(kinds_match("jar", "java"));
        assert!(kinds_match("java", "jar"));
        assert!(kinds_match("node", "node"));
        assert!(!kinds_match("node", "java"));
    }

    #[test]
    fn provider_mapping() {
        assert_eq!(resolve_provider("postgresql"), "postgresql-addon");
        assert_eq!(resolve_provider("cellar"), "cellar-addon");
        assert_eq!(resolve_provider("matomo"), "addon-matomo");
        assert_eq!(resolve_provider("kv"), "kv");
        assert_eq!(resolve_provider("cellar-addon"), "cellar-addon");
    }
}
