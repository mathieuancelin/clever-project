use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::clever::Clever;
use crate::cli::CheckArgs;
use crate::commands::apply::{validate_addons, validate_app_scaling};
use crate::commands::resolve_project_file;
use crate::issues::{self, Issue, IssueSink};
use crate::model::Project;

/// Validate a project file end-to-end without contacting Clever Cloud for
/// any mutation. By default we still call the read-only Clever API to
/// validate addon kinds/sizes and app flavor names; `--offline` skips that
/// step (useful in CI without `clever login`).
pub fn run(args: CheckArgs) -> Result<()> {
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

    // 1. Load + resolve, but in collecting mode so that even when variables
    //    or secrets are missing we still get a Project we can run the
    //    cross-resource validators against.
    let file = resolve_project_file(args.file, &std::env::current_dir()?)?;
    let (mut project, mut issues) = Project::load_collecting(
        &file,
        args.org,
        args.region,
        &variables,
        args.secrets_path.as_deref(),
    )
    .with_context(|| format!("loading project `{}`", file.display()))?;

    // 2. Static cross-resource checks.
    validate_dependencies(&project, &mut issues);
    validate_network_groups(&project, &mut issues);
    validate_unique_names(&project, &mut issues);

    // 3. Optional live API validation (addons catalog + app flavors).
    if args.offline {
        info!("--offline set: skipping live API validation");
    } else {
        let clever = Clever::new()?;
        if !project.addons.is_empty() {
            let providers = clever
                .list_addon_providers(&project.org)
                .with_context(|| {
                    format!(
                        "fetching addon providers for org `{}` (used to validate addon kinds and sizes)",
                        project.org
                    )
                })?;
            validate_addons(&mut project.addons, &providers, &mut issues);
        }
        if project.apps.values().any(|a| {
            a.scalability
                .as_ref()
                .and_then(|s| s.instances.as_ref())
                .is_some_and(|i| i.min_size.is_some() || i.max_size.is_some())
        }) {
            let instances = clever.list_app_instances(&project.org).with_context(|| {
                format!(
                    "fetching app instances for org `{}` (used to validate app scaling sizes)",
                    project.org
                )
            })?;
            validate_app_scaling(&mut project.apps, &instances, &mut issues);
        }
    }

    if !issues.is_empty() {
        bail!("{}", issues::render(&issues));
    }

    info!(
        "{} apps, {} addons — project file is valid",
        project.apps.len(),
        project.addons.len()
    );
    Ok(())
}

/// Every `dependencies` entry of every app must reference an existing
/// project key (in `apps:` or `addons:`). Also rejects self-dependencies.
fn validate_dependencies(project: &Project, issues: &mut Vec<Issue>) {
    for (app_key, app) in &project.apps {
        for dep_key in &app.dependencies {
            if dep_key == app_key {
                issues.push_issue(format!("app `{app_key}` lists itself as a dependency"));
                continue;
            }
            let in_apps = project.apps.contains_key(dep_key);
            let in_addons = project.addons.contains_key(dep_key);
            if !in_apps && !in_addons {
                issues.push_issue(format!(
                    "app `{app_key}` has unknown dependency `{dep_key}`: not a project key in `apps:` or `addons:`"
                ));
            }
        }
    }
}

/// Every `link:` entry in a network group must reference an existing
/// project key (in `apps:` or `addons:`).
fn validate_network_groups(project: &Project, issues: &mut Vec<Issue>) {
    for (ng_key, ng) in &project.network_groups {
        for dep_key in &ng.link {
            let in_apps = project.apps.contains_key(dep_key);
            let in_addons = project.addons.contains_key(dep_key);
            if !in_apps && !in_addons {
                issues.push_issue(format!(
                    "network group `{ng_key}` links unknown project key `{dep_key}`: not in `apps:` or `addons:`"
                ));
            }
        }
    }
}

/// Resource names must be unique among apps, unique among addons, and unique
/// among network groups. An app, an addon and an NG can share a name across
/// types (they live in different Clever namespaces), but two of the same
/// type sharing a name would clash on `name → id` lookups.
fn validate_unique_names(project: &Project, issues: &mut Vec<Issue>) {
    let mut seen_apps: HashMap<&str, &str> = HashMap::new();
    for (key, app) in &project.apps {
        if app.name.trim().is_empty() {
            issues.push_issue(format!("app `{key}` has an empty `name`"));
            continue;
        }
        if let Some(prev) = seen_apps.insert(&app.name, key) {
            issues.push_issue(format!(
                "apps `{prev}` and `{key}` both resolve to the same name `{}`",
                app.name
            ));
        }
    }
    let mut seen_addons: HashMap<&str, &str> = HashMap::new();
    for (key, addon) in &project.addons {
        if addon.name.trim().is_empty() {
            issues.push_issue(format!("addon `{key}` has an empty `name`"));
            continue;
        }
        if let Some(prev) = seen_addons.insert(&addon.name, key) {
            issues.push_issue(format!(
                "addons `{prev}` and `{key}` both resolve to the same name `{}`",
                addon.name
            ));
        }
    }
    let mut seen_ngs: HashMap<&str, &str> = HashMap::new();
    for (key, ng) in &project.network_groups {
        if ng.name.trim().is_empty() {
            issues.push_issue(format!("network group `{key}` has an empty `name`"));
            continue;
        }
        if let Some(prev) = seen_ngs.insert(&ng.name, key) {
            issues.push_issue(format!(
                "network groups `{prev}` and `{key}` both resolve to the same name `{}`",
                ng.name
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addon, App, NetworkGroup};
    use indexmap::IndexMap;

    fn empty_ng(name: &str, links: &[&str]) -> NetworkGroup {
        NetworkGroup {
            name: name.to_string(),
            description: None,
            link: links.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn empty_app(name: &str, kind: &str) -> App {
        App {
            name: name.to_string(),
            kind: kind.to_string(),
            region: None,
            source: None,
            domains: vec![],
            scalability: None,
            dependencies: vec![],
            config: IndexMap::new(),
            env: IndexMap::new(),
        }
    }

    fn empty_addon(name: &str, kind: &str) -> Addon {
        Addon {
            name: name.to_string(),
            kind: kind.to_string(),
            size: None,
            crypted: false,
            region: None,
            version: None,
            backup_path: None,
        }
    }

    fn make_project() -> Project {
        Project {
            name: "p".into(),
            description: None,
            org: "o".into(),
            region: "par".into(),
            variables: IndexMap::new(),
            apps: IndexMap::new(),
            addons: IndexMap::new(),
            network_groups: IndexMap::new(),
        }
    }

    fn run_validator<F>(project: &Project, f: F) -> Vec<Issue>
    where
        F: FnOnce(&Project, &mut Vec<Issue>),
    {
        let mut issues = Vec::new();
        f(project, &mut issues);
        issues
    }

    #[test]
    fn dependencies_must_reference_known_keys() {
        let mut project = make_project();
        let mut app = empty_app("api", "node");
        app.dependencies = vec!["does-not-exist".to_string()];
        project.apps.insert("api".to_string(), app);
        let issues = run_validator(&project, validate_dependencies);
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0]
                .message
                .contains("unknown dependency `does-not-exist`")
        );
    }

    #[test]
    fn self_dependency_is_rejected() {
        let mut project = make_project();
        let mut app = empty_app("api", "node");
        app.dependencies = vec!["api".to_string()];
        project.apps.insert("api".to_string(), app);
        let issues = run_validator(&project, validate_dependencies);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("itself"));
    }

    #[test]
    fn valid_dependencies_pass() {
        let mut project = make_project();
        let mut api = empty_app("api", "node");
        api.dependencies = vec!["db".to_string(), "worker".to_string()];
        project.apps.insert("api".to_string(), api);
        project
            .apps
            .insert("worker".to_string(), empty_app("worker", "node"));
        project
            .addons
            .insert("db".to_string(), empty_addon("db", "postgresql"));
        let issues = run_validator(&project, validate_dependencies);
        assert!(issues.is_empty());
    }

    #[test]
    fn duplicate_app_names_rejected() {
        let mut project = make_project();
        project
            .apps
            .insert("a".to_string(), empty_app("same-name", "node"));
        project
            .apps
            .insert("b".to_string(), empty_app("same-name", "node"));
        let issues = run_validator(&project, validate_unique_names);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("same-name"));
    }

    #[test]
    fn duplicate_addon_names_rejected() {
        let mut project = make_project();
        project
            .addons
            .insert("a".to_string(), empty_addon("the-db", "postgresql"));
        project
            .addons
            .insert("b".to_string(), empty_addon("the-db", "postgresql"));
        let issues = run_validator(&project, validate_unique_names);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("the-db"));
    }

    #[test]
    fn app_and_addon_can_share_a_name() {
        let mut project = make_project();
        project
            .apps
            .insert("a".to_string(), empty_app("dual", "node"));
        project
            .addons
            .insert("d".to_string(), empty_addon("dual", "postgresql"));
        let issues = run_validator(&project, validate_unique_names);
        assert!(issues.is_empty());
    }

    #[test]
    fn ng_link_must_reference_known_key() {
        let mut project = make_project();
        project
            .apps
            .insert("api".to_string(), empty_app("api", "node"));
        project
            .network_groups
            .insert("net".to_string(), empty_ng("vpn", &["api", "ghost"]));
        let issues = run_validator(&project, validate_network_groups);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("ghost"));
    }

    #[test]
    fn ng_with_only_known_links_passes() {
        let mut project = make_project();
        project
            .apps
            .insert("api".to_string(), empty_app("api", "node"));
        project
            .addons
            .insert("db".to_string(), empty_addon("db", "postgresql"));
        project
            .network_groups
            .insert("net".to_string(), empty_ng("vpn", &["api", "db"]));
        let issues = run_validator(&project, validate_network_groups);
        assert!(issues.is_empty());
    }

    #[test]
    fn duplicate_ng_names_rejected() {
        let mut project = make_project();
        project
            .network_groups
            .insert("a".to_string(), empty_ng("dup", &[]));
        project
            .network_groups
            .insert("b".to_string(), empty_ng("dup", &[]));
        let issues = run_validator(&project, validate_unique_names);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("dup"));
    }

    #[test]
    fn empty_app_name_rejected() {
        let mut project = make_project();
        project.apps.insert("a".to_string(), empty_app("", "node"));
        let issues = run_validator(&project, validate_unique_names);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("empty"));
    }

    #[test]
    fn multiple_issues_accumulate_in_one_pass() {
        let mut project = make_project();
        // app with a bad dep AND a self-dep
        let mut api = empty_app("api", "node");
        api.dependencies = vec!["api".to_string(), "ghost".to_string()];
        project.apps.insert("api".to_string(), api);
        // duplicate app name on a second app
        project
            .apps
            .insert("api2".to_string(), empty_app("api", "node"));
        // NG linking a non-existent key
        project
            .network_groups
            .insert("net".to_string(), empty_ng("vpn", &["nowhere"]));

        let mut issues = Vec::new();
        validate_dependencies(&project, &mut issues);
        validate_network_groups(&project, &mut issues);
        validate_unique_names(&project, &mut issues);
        // self-dep + unknown dep `ghost` + duplicate `api` name + NG bad link = 4
        assert_eq!(issues.len(), 4, "got: {issues:#?}");
    }
}
