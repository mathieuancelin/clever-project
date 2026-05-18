use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{info, warn};

use crate::cli::ApplyArgs;
use crate::clever::{Clever, CreateAddon, CreateApp, ListedAddon, ListedApp};
use crate::model::{App, Project, Source};

pub fn run(args: ApplyArgs) -> Result<()> {
    let mut variables = args.variables;
    if let Some(env) = args.env {
        variables.push(("env".to_string(), env));
    }
    let (project, _resolver) = Project::load_and_resolve(
        &args.file,
        args.org,
        args.region,
        &variables,
    )
    .with_context(|| format!("loading project `{}`", args.file.display()))?;

    let clever = Clever::new()?;

    // Snapshot existing state in the org.
    let mut existing_apps: HashMap<String, ListedApp> = clever
        .list_apps(&project.org)?
        .into_iter()
        .map(|a| (a.name.clone(), a))
        .collect();
    let mut existing_addons: HashMap<String, ListedAddon> = clever
        .list_addons(&project.org)?
        .into_iter()
        .map(|a| (a.name.clone(), a))
        .collect();

    // Phase 1 — addons. Create missing ones, warn (no update) on existing.
    let mut addon_id_by_key: HashMap<String, String> = HashMap::new();
    for (key, addon) in &project.addons {
        match existing_addons.remove(&addon.name) {
            Some(found) => {
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
                addon_id_by_key.insert(key.clone(), found.addon_id);
            }
            None => {
                let region = addon.region.as_deref().unwrap_or(&project.region);
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
                addon_id_by_key.insert(key.clone(), id);
            }
        }
    }

    // Phase 2 — apps. Create missing, update existing iff kind+source match.
    let mut app_id_by_key: HashMap<String, String> = HashMap::new();
    let mut apps_to_link: Vec<(String, &App)> = Vec::new();

    for (key, app) in &project.apps {
        let region = app.region.as_deref().unwrap_or(&project.region);

        let app_id = match existing_apps.remove(&app.name) {
            Some(found) => {
                if !kinds_match(&found.kind, &app.kind) {
                    warn!(
                        "app `{}` exists with kind `{}` but project declares `{}` — skipping update",
                        app.name, found.kind, app.kind
                    );
                    found.app_id
                } else if !source_matches(found.deploy_url.as_deref(), app.source.as_ref()) {
                    warn!(
                        "app `{}` source diverges (clever: {:?}, project: {:?}) — skipping update",
                        app.name, found.deploy_url, app.source
                    );
                    found.app_id
                } else {
                    info!(
                        "updating app `{}` ({}) [project key: {key}]",
                        app.name, found.app_id
                    );
                    update_app(&clever, &found.app_id, app)?;
                    found.app_id
                }
            }
            None => {
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
                update_app(&clever, &id, app)?;
                id
            }
        };

        app_id_by_key.insert(key.clone(), app_id);
        apps_to_link.push((key.clone(), app));
    }

    // Phase 3 — service links. Resolve project keys -> ids, diff against
    // currently linked services, link/unlink to converge.
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

    info!("apply complete");
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
    // Use the original-case substring of the same length so the returned
    // `owner/repo` keeps GitHub's casing.
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
    // env — full replace.
    if !app.env.is_empty() {
        clever.env_replace(app_id, &app.env)?;
    } else {
        // Even an empty desired env should clear existing variables.
        clever.env_replace(app_id, &app.env)?;
    }

    // domains — diff against current.
    let current: HashSet<String> = clever
        .get_domains(app_id)?
        .into_iter()
        .map(|d| d.hostname)
        .collect();
    let desired: HashSet<String> = app.domains.iter().cloned().collect();
    for d in desired.difference(&current) {
        clever.domain_add(app_id, d)?;
    }
    for d in current.difference(&desired) {
        // Skip default *.cleverapps.io domains — they're auto-managed by Clever
        // and trying to remove them would fail.
        if d.ends_with(".cleverapps.io") {
            continue;
        }
        clever.domain_rm(app_id, d)?;
    }

    // scalability.
    if let Some(scale) = &app.scalability {
        clever.scale(app_id, scale)?;
    }

    Ok(())
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

    let services = clever.get_services(app_id)?;
    let current_apps: HashSet<String> = services
        .applications
        .iter()
        .map(|s| s.id.clone())
        .collect();
    let current_addons: HashSet<String> = services
        .addons
        .iter()
        .map(|s| s.id.clone())
        .collect();

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
