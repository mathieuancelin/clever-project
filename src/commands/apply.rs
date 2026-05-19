use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{info, warn};

use crate::clever::{AddonProvider, Clever, CreateAddon, CreateApp, ListedAddon, ListedApp};
use crate::cli::ApplyArgs;
use crate::commands::OrgCache;
use crate::model::{Addon, App, Project, Source};
use crate::state::{ResourceKind, State, StateResource};
use indexmap::IndexMap;

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
    let (mut project, _resolver) = Project::load_and_resolve(
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

    // Validate every addon spec against the live list of providers/plans
    // from the Clever API. Catches typos in `kind` and `size` (including
    // case) before any mutation goes out. Skipped if the project has no
    // addons (no need to spend an API call).
    if !project.addons.is_empty() {
        info!("validating addon specs against Clever's provider list");
        let providers = clever.list_addon_providers(&project.org).with_context(|| {
            format!(
                "fetching addon providers for org `{}` (used to validate addon kinds and sizes)",
                project.org
            )
        })?;
        validate_addons(&mut project.addons, &providers)?;
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
    let mut outcomes: HashMap<String, AppOutcome> = HashMap::new();

    for (key, app) in &project.apps {
        let outcome = handle_app(
            &clever,
            &mut state,
            &mut cache,
            &project,
            &effective_env,
            key,
            app,
        )?;
        app_id_by_key.insert(key.clone(), outcome.id.clone());
        outcomes.insert(key.clone(), outcome);
        apps_to_link.push((key.clone(), app));
    }

    // Phase 3 — service links. Per-app `sync_dependencies` returns whether
    // it changed anything; we feed that back into the per-app outcomes so
    // we can decide on restarts in phase 4. Wrapped in a one-shot retry: if
    // anything fails (likely due to a stale id pulled from state), refresh
    // state against fresh listings, rebuild the dep maps, and try again.
    let run_phase3 = |clever: &Clever,
                      apps_to_link: &[(String, &App)],
                      app_id_by_key: &HashMap<String, String>,
                      addon_id_by_key: &HashMap<String, String>,
                      project: &Project|
     -> Result<HashMap<String, bool>> {
        let mut changed = HashMap::new();
        for (key, app) in apps_to_link {
            let app_id = &app_id_by_key[key];
            let deps_changed = sync_dependencies(
                clever,
                app_id,
                &app.dependencies,
                app_id_by_key,
                addon_id_by_key,
                project,
            )
            .with_context(|| format!("syncing dependencies of app `{}`", app.name))?;
            changed.insert(key.clone(), deps_changed);
        }
        Ok(changed)
    };

    let deps_changed_by_key: HashMap<String, bool> = match run_phase3(
        &clever,
        &apps_to_link,
        &app_id_by_key,
        &addon_id_by_key,
        &project,
    ) {
        Ok(m) => m,
        Err(e) => {
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
            if !clever.is_dry_run() {
                state
                    .save()
                    .with_context(|| format!("saving state file `{}`", state.path().display()))?;
            }
            // Sync the per-app outcome ids too — refresh may have rewritten them.
            for (key, outcome) in outcomes.iter_mut() {
                if let Some(new_id) = app_id_by_key.get(key) {
                    if outcome.id != *new_id {
                        outcome.id = new_id.clone();
                    }
                }
            }
            run_phase3(
                &clever,
                &apps_to_link,
                &app_id_by_key,
                &addon_id_by_key,
                &project,
            )?
        }
    };

    // Phase 4 — restart apps where it matters.
    //   - just created with github  → restart triggers the first deploy
    //   - already existed and env or dependencies changed → restart
    //   - created without github    → nothing to deploy yet, skip
    for (key, _app) in &apps_to_link {
        let outcome = &outcomes[key];
        let deps_changed = *deps_changed_by_key.get(key).unwrap_or(&false);
        let restart = if outcome.just_created {
            outcome.created_with_github
        } else {
            outcome.env_changed || deps_changed
        };
        if restart {
            let reason = if outcome.just_created {
                "github source"
            } else if outcome.env_changed && deps_changed {
                "env + dependencies changed"
            } else if outcome.env_changed {
                "env changed"
            } else {
                "dependencies changed"
            };
            info!("restarting app `{}` ({}) — {reason}", _app.name, outcome.id);
            clever
                .restart(&outcome.id)
                .with_context(|| format!("restarting app `{}`", _app.name))?;
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

/// Outcome of bringing one app into the desired state.
struct AppOutcome {
    id: String,
    /// True iff the app was just created in this run.
    just_created: bool,
    /// True iff the create used `--github` (i.e. clever knows where to pull
    /// the code from). Only meaningful when `just_created` is true.
    created_with_github: bool,
    /// True iff the env vars were rewritten during this run.
    env_changed: bool,
}

fn handle_app(
    clever: &Clever,
    state: &mut State,
    cache: &mut OrgCache,
    project: &Project,
    env: &str,
    key: &str,
    app: &App,
) -> Result<AppOutcome> {
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
            Ok(env_changed) => {
                return Ok(AppOutcome {
                    id,
                    just_created: false,
                    created_with_github: false,
                    env_changed,
                });
            }
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
            return Ok(AppOutcome {
                id: found.app_id,
                just_created: false,
                created_with_github: false,
                env_changed: false,
            });
        }
        if !source_matches(found.deploy_url.as_deref(), app.source.as_ref()) {
            warn!(
                "app `{}` source diverges (clever: {:?}, project: {:?}) — skipping update",
                app.name, found.deploy_url, app.source
            );
            return Ok(AppOutcome {
                id: found.app_id,
                just_created: false,
                created_with_github: false,
                env_changed: false,
            });
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
        let env_changed = update_app(clever, &found.app_id, app)?;
        return Ok(AppOutcome {
            id: found.app_id,
            just_created: false,
            created_with_github: false,
            env_changed,
        });
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
    let env_changed = update_app(clever, &id, app)?;
    Ok(AppOutcome {
        id,
        just_created: true,
        created_with_github: github.is_some(),
        env_changed,
    })
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

/// Apply the project's app config to a (real or synthetic) Clever app id.
/// Returns whether the env vars actually changed, so the caller can decide
/// to restart the app afterwards.
fn update_app(clever: &Clever, app_id: &str, app: &App) -> Result<bool> {
    // env — replace only if the desired set differs from what's already
    // there. For freshly created (or synthetic dry-run) apps, the current
    // env is empty/defaults, so any non-empty desired env counts as a
    // change.
    let env_changed = if is_synthetic(app_id) {
        if !app.env.is_empty() {
            clever.env_replace(app_id, &app.env)?;
            true
        } else {
            false
        }
    } else {
        let current: indexmap::IndexMap<String, String> = clever
            .get_env(app_id)?
            .into_iter()
            .map(|v| (v.name, v.value))
            .collect();
        if !maps_equal(&current, &app.env) {
            clever.env_replace(app_id, &app.env)?;
            true
        } else {
            false
        }
    };

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

    Ok(env_changed)
}

fn maps_equal(
    a: &indexmap::IndexMap<String, String>,
    b: &indexmap::IndexMap<String, String>,
) -> bool {
    a.len() == b.len() && a.iter().all(|(k, v)| b.get(k) == Some(v))
}

fn is_synthetic(id: &str) -> bool {
    id.starts_with("dry-run::")
}

/// Sync the linked services for one app. Returns whether at least one link
/// or unlink call was made — so the caller knows whether a restart is in
/// order.
fn sync_dependencies(
    clever: &Clever,
    app_id: &str,
    dependencies: &[String],
    app_id_by_key: &HashMap<String, String>,
    addon_id_by_key: &HashMap<String, String>,
    project: &Project,
) -> Result<bool> {
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
            warn!(
                "dependency `{dep_key}` matched by name on app `{}`",
                app.name
            );
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

    let mut changed = false;
    for id in desired_addons.difference(&current_addons) {
        clever.link_addon(app_id, id)?;
        changed = true;
    }
    for id in current_addons.difference(&desired_addons) {
        clever.unlink_addon(app_id, id)?;
        changed = true;
    }
    for id in desired_apps.difference(&current_apps) {
        clever.link_app(app_id, id)?;
        changed = true;
    }
    for id in current_apps.difference(&desired_apps) {
        clever.unlink_app(app_id, id)?;
        changed = true;
    }

    Ok(changed)
}

/// Validate every addon's `kind` and `size` against the live provider list
/// returned by Clever's API. Normalizes the size casing to match the canonical
/// slug from the API (so users can write `S_BIG` and we send `s_big`, etc.).
fn validate_addons(
    addons: &mut IndexMap<String, Addon>,
    providers: &[AddonProvider],
) -> Result<()> {
    use std::collections::HashMap;
    let provider_by_id: HashMap<&str, &AddonProvider> =
        providers.iter().map(|p| (p.id.as_str(), p)).collect();

    for (key, addon) in addons.iter_mut() {
        let resolved = resolve_provider(&addon.kind);
        let provider = match provider_by_id.get(resolved) {
            Some(p) => *p,
            None => {
                let mut available: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
                available.sort();
                bail!(
                    "addon `{key}` has unknown provider `{}` (resolved to `{resolved}`). Available providers: {}",
                    addon.kind,
                    available.join(", ")
                );
            }
        };

        if let Some(size) = addon.size.clone() {
            let needle = size.to_lowercase();
            let matched = provider
                .plans
                .iter()
                .find(|p| p.slug.to_lowercase() == needle);
            match matched {
                Some(plan) => {
                    if plan.slug != size {
                        addon.size = Some(plan.slug.clone());
                    }
                }
                None => {
                    let mut slugs: Vec<&str> =
                        provider.plans.iter().map(|p| p.slug.as_str()).collect();
                    slugs.sort();
                    bail!(
                        "addon `{key}` has unknown size `{size}` for provider `{}`. Available sizes: {}",
                        provider.id,
                        slugs.join(", ")
                    );
                }
            }
        }

        if let Some(region) = &addon.region {
            if !provider.regions.iter().any(|r| r == region) {
                let mut regs: Vec<&str> = provider.regions.iter().map(String::as_str).collect();
                regs.sort();
                bail!(
                    "addon `{key}` region `{region}` is not supported by provider `{}`. Supported regions: {}",
                    provider.id,
                    regs.join(", ")
                );
            }
        }
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

    use crate::clever::{AddonPlan, AddonProvider};
    use indexmap::IndexMap;

    fn pg_provider() -> AddonProvider {
        AddonProvider {
            id: "postgresql-addon".to_string(),
            name: "PostgreSQL".to_string(),
            regions: vec!["par".to_string(), "rbx".to_string()],
            plans: vec![
                AddonPlan {
                    slug: "xs_sml".to_string(),
                    name: "XS Small Space".to_string(),
                },
                AddonPlan {
                    slug: "s_big".to_string(),
                    name: "S Big Space".to_string(),
                },
            ],
        }
    }

    fn cellar_provider() -> AddonProvider {
        AddonProvider {
            id: "cellar-addon".to_string(),
            name: "Cellar".to_string(),
            regions: vec!["par".to_string()],
            plans: vec![AddonPlan {
                slug: "S".to_string(),
                name: "S".to_string(),
            }],
        }
    }

    fn build_addons(
        entries: &[(&str, &str, Option<&str>, Option<&str>)],
    ) -> IndexMap<String, Addon> {
        let mut out = IndexMap::new();
        for (key, kind, size, region) in entries {
            out.insert(
                key.to_string(),
                Addon {
                    name: format!("{key}-name"),
                    kind: kind.to_string(),
                    size: size.map(str::to_string),
                    crypted: false,
                    region: region.map(str::to_string),
                    version: None,
                    backup_path: None,
                },
            );
        }
        out
    }

    #[test]
    fn validate_addons_unknown_kind() {
        let providers = vec![pg_provider()];
        let mut addons = build_addons(&[("db", "unknownkind", None, None)]);
        let err = validate_addons(&mut addons, &providers).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown provider"));
        assert!(msg.contains("postgresql-addon"));
    }

    #[test]
    fn validate_addons_unknown_size() {
        let providers = vec![pg_provider()];
        let mut addons = build_addons(&[("db", "postgresql", Some("xxl_giant"), None)]);
        let err = validate_addons(&mut addons, &providers).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("xxl_giant"));
        assert!(msg.contains("xs_sml"));
    }

    #[test]
    fn validate_addons_normalises_size_casing() {
        let providers = vec![pg_provider()];
        let mut addons = build_addons(&[("db", "postgresql", Some("S_BIG"), None)]);
        validate_addons(&mut addons, &providers).unwrap();
        assert_eq!(addons.get("db").unwrap().size.as_deref(), Some("s_big"));
    }

    #[test]
    fn validate_addons_preserves_uppercase_when_canonical() {
        let providers = vec![cellar_provider()];
        let mut addons = build_addons(&[("c", "cellar", Some("s"), None)]);
        validate_addons(&mut addons, &providers).unwrap();
        // canonical slug is uppercase "S"
        assert_eq!(addons.get("c").unwrap().size.as_deref(), Some("S"));
    }

    #[test]
    fn validate_addons_rejects_unsupported_region() {
        let providers = vec![pg_provider()];
        let mut addons = build_addons(&[("db", "postgresql", None, Some("syd"))]);
        let err = validate_addons(&mut addons, &providers).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("syd"));
        assert!(msg.contains("Supported regions"));
    }

    #[test]
    fn validate_addons_accepts_size_omitted() {
        let providers = vec![pg_provider()];
        let mut addons = build_addons(&[("db", "postgresql", None, None)]);
        validate_addons(&mut addons, &providers).unwrap();
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
