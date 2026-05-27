//! Resource targeting (`--target apps.api`, etc.).
//!
//! A target points to one entry in the project file's `apps:`, `addons:` or
//! `network_groups:` section, by **project key** (the YAML key under the
//! section), not by the resolved `name:` field.
//!
//! When at least one target is set, apply/delete only mutate the targeted
//! resources; everything else is left alone. Apply still needs the ids of
//! non-targeted resources that are referenced as dependencies of targeted
//! apps — that resolution happens via state/listing in `apply.rs`.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};

use crate::model::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TargetKind {
    App,
    Addon,
    NetworkGroup,
}

#[derive(Debug, Clone, Default)]
pub struct Targets {
    pub apps: BTreeSet<String>,
    pub addons: BTreeSet<String>,
    pub network_groups: BTreeSet<String>,
}

impl Targets {
    /// `true` when no targets were specified at all — i.e. apply/delete
    /// should process every resource in the project file (the default).
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty() && self.addons.is_empty() && self.network_groups.is_empty()
    }

    /// `true` iff the given resource is in the targeted set. Always `true`
    /// when no targets are specified (so existing call sites read as
    /// "process this resource").
    pub fn is_targeted(&self, kind: TargetKind, key: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        match kind {
            TargetKind::App => self.apps.contains(key),
            TargetKind::Addon => self.addons.contains(key),
            TargetKind::NetworkGroup => self.network_groups.contains(key),
        }
    }

    /// Pretty-printed "(targeting: apps.api, addons.db)" string for plan
    /// headers. Empty when no targets.
    pub fn label(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        for k in &self.apps {
            parts.push(format!("apps.{k}"));
        }
        for k in &self.addons {
            parts.push(format!("addons.{k}"));
        }
        for k in &self.network_groups {
            parts.push(format!("network_groups.{k}"));
        }
        format!("(targeting: {})", parts.join(", "))
    }
}

/// Parse a single `--target` value. Accepts:
/// - `apps.<key>` / `app.<key>`
/// - `addons.<key>` / `addon.<key>`
/// - `network_groups.<key>` / `network_group.<key>` / `ngs.<key>` / `ng.<key>`
pub fn parse_target(s: &str) -> Result<(TargetKind, String), String> {
    let (raw_kind, key) = s
        .split_once('.')
        .ok_or_else(|| format!("target `{s}` must be `<type>.<key>`"))?;
    if key.is_empty() {
        return Err(format!("target `{s}` has an empty key after `.`"));
    }
    let kind = match raw_kind.to_lowercase().as_str() {
        "app" | "apps" => TargetKind::App,
        "addon" | "addons" => TargetKind::Addon,
        "network_group" | "network_groups" | "ng" | "ngs" => TargetKind::NetworkGroup,
        other => {
            return Err(format!(
                "unknown target type `{other}` in `{s}` (expected one of: apps, addons, network_groups, ng)"
            ));
        }
    };
    Ok((kind, key.to_string()))
}

/// Build a `Targets` from CLI specs and validate every entry against the
/// loaded project file. Typos that don't resolve to a project key error out
/// here, before any mutation.
pub fn build(specs: &[(TargetKind, String)], project: &Project) -> Result<Targets> {
    let mut t = Targets::default();
    for (kind, key) in specs {
        match kind {
            TargetKind::App => {
                if !project.apps.contains_key(key) {
                    bail!(
                        "target `apps.{key}` doesn't match any project key under `apps:`{}",
                        suggest_keys("apps", project.apps.keys())
                    );
                }
                t.apps.insert(key.clone());
            }
            TargetKind::Addon => {
                if !project.addons.contains_key(key) {
                    bail!(
                        "target `addons.{key}` doesn't match any project key under `addons:`{}",
                        suggest_keys("addons", project.addons.keys())
                    );
                }
                t.addons.insert(key.clone());
            }
            TargetKind::NetworkGroup => {
                if !project.network_groups.contains_key(key) {
                    bail!(
                        "target `network_groups.{key}` doesn't match any project key under `network_groups:`{}",
                        suggest_keys("network_groups", project.network_groups.keys())
                    );
                }
                t.network_groups.insert(key.clone());
            }
        }
    }
    Ok(t)
}

fn suggest_keys<'a, I: IntoIterator<Item = &'a String>>(label: &str, keys: I) -> String {
    let names: Vec<&str> = keys.into_iter().map(String::as_str).collect();
    if names.is_empty() {
        format!(" (no `{label}` section in the project)")
    } else {
        format!(". Available {label} keys: {}", names.join(", "))
    }
}

/// Clap value_parser: parse a `--target` arg into a (kind, key) pair.
pub fn parse_target_arg(s: &str) -> Result<(TargetKind, String), String> {
    parse_target(s)
}

/// Validate that every dependency of every targeted app either points to a
/// targeted resource or to one that's already known by `id_resolver` (state
/// or live). Returns `Ok(())` only when the whole closure is covered. Bails
/// listing the missing piece(s) so the user knows which `--target` flag to
/// add.
pub fn check_targeted_dep_closure<F>(
    project: &Project,
    targets: &Targets,
    mut id_resolver: F,
) -> Result<()>
where
    F: FnMut(&str) -> Option<()>,
{
    if targets.is_empty() {
        return Ok(());
    }
    let mut missing: Vec<String> = Vec::new();
    for (app_key, app) in &project.apps {
        if !targets.apps.contains(app_key) {
            continue;
        }
        for dep_key in &app.dependencies {
            let in_targeted_apps = targets.apps.contains(dep_key);
            let in_targeted_addons = targets.addons.contains(dep_key);
            if in_targeted_apps || in_targeted_addons {
                continue;
            }
            if id_resolver(dep_key).is_some() {
                continue;
            }
            missing.push(format!(
                "  - app `{app_key}` depends on `{dep_key}` which is neither targeted nor already provisioned"
            ));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "targeting leaves dependencies unresolved (add the missing --target flag, or run a full apply first):\n{}",
            missing.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addon, App, NetworkGroup};
    use indexmap::IndexMap;

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

    fn app(name: &str, deps: &[&str]) -> App {
        App {
            name: name.into(),
            kind: "node".into(),
            region: None,
            source: None,
            domains: vec![],
            scalability: None,
            build: None,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            config: IndexMap::new(),
            env: IndexMap::new(),
            hooks: None,
        }
    }

    fn addon(name: &str) -> Addon {
        Addon {
            name: name.into(),
            kind: "postgresql".into(),
            size: None,
            crypted: false,
            region: None,
            version: None,
            backup_path: None,
            env: IndexMap::new(),
            domains: vec![],
        }
    }

    fn ng(name: &str) -> NetworkGroup {
        NetworkGroup {
            name: name.into(),
            description: None,
            link: vec![],
        }
    }

    #[test]
    fn parse_apps_plural_and_singular() {
        assert_eq!(
            parse_target("apps.api").unwrap(),
            (TargetKind::App, "api".into())
        );
        assert_eq!(
            parse_target("app.api").unwrap(),
            (TargetKind::App, "api".into())
        );
        assert_eq!(
            parse_target("APPS.api").unwrap(),
            (TargetKind::App, "api".into())
        );
    }

    #[test]
    fn parse_addons_and_ng_aliases() {
        assert_eq!(
            parse_target("addons.db").unwrap(),
            (TargetKind::Addon, "db".into())
        );
        assert_eq!(
            parse_target("addon.db").unwrap(),
            (TargetKind::Addon, "db".into())
        );
        assert_eq!(
            parse_target("network_groups.vpn").unwrap(),
            (TargetKind::NetworkGroup, "vpn".into())
        );
        assert_eq!(
            parse_target("ng.vpn").unwrap(),
            (TargetKind::NetworkGroup, "vpn".into())
        );
        assert_eq!(
            parse_target("ngs.vpn").unwrap(),
            (TargetKind::NetworkGroup, "vpn".into())
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_target("api").is_err());
        assert!(parse_target("apps.").is_err());
        assert!(parse_target("widgets.foo").is_err());
    }

    #[test]
    fn build_rejects_unknown_key() {
        let mut project = empty_project();
        project.apps.insert("api".into(), app("api", &[]));
        let err = build(&[(TargetKind::App, "ghost".into())], &project).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ghost"));
        assert!(msg.contains("api")); // suggests the existing key
    }

    #[test]
    fn build_no_section_message() {
        let project = empty_project();
        let err = build(&[(TargetKind::Addon, "db".into())], &project).unwrap_err();
        assert!(format!("{err:#}").contains("no `addons` section"));
    }

    #[test]
    fn empty_targets_match_everything() {
        let t = Targets::default();
        assert!(t.is_empty());
        assert!(t.is_targeted(TargetKind::App, "anything"));
    }

    #[test]
    fn is_targeted_filters_to_set() {
        let mut t = Targets::default();
        t.apps.insert("api".into());
        assert!(t.is_targeted(TargetKind::App, "api"));
        assert!(!t.is_targeted(TargetKind::App, "worker"));
        assert!(!t.is_targeted(TargetKind::Addon, "api"));
    }

    #[test]
    fn label_string() {
        let mut t = Targets::default();
        t.apps.insert("api".into());
        t.addons.insert("db".into());
        assert_eq!(t.label(), "(targeting: apps.api, addons.db)");
    }

    #[test]
    fn check_closure_passes_when_dep_targeted() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), app("prod-api", &["db"][..]));
        project.addons.insert("db".into(), addon("prod-db"));
        let mut t = Targets::default();
        t.apps.insert("api".into());
        t.addons.insert("db".into());
        check_targeted_dep_closure(&project, &t, |_| None).unwrap();
    }

    #[test]
    fn check_closure_passes_when_dep_resolvable() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), app("prod-api", &["db"][..]));
        project.addons.insert("db".into(), addon("prod-db"));
        let mut t = Targets::default();
        t.apps.insert("api".into());
        // db is NOT targeted, but the resolver says it already exists.
        check_targeted_dep_closure(
            &project,
            &t,
            |key| if key == "db" { Some(()) } else { None },
        )
        .unwrap();
    }

    #[test]
    fn check_closure_bails_on_missing_dep() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), app("prod-api", &["db"][..]));
        project.addons.insert("db".into(), addon("prod-db"));
        let mut t = Targets::default();
        t.apps.insert("api".into());
        let err = check_targeted_dep_closure(&project, &t, |_| None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`db`"));
        assert!(msg.contains("unresolved"));
    }

    #[test]
    fn ng_listed_in_label() {
        let mut t = Targets::default();
        t.network_groups.insert("vpn".into());
        let _unused = ng("vpn");
        assert!(t.label().contains("network_groups.vpn"));
    }
}
