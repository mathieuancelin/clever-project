use std::collections::HashMap;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use tracing::{info, warn};

use crate::clever::Clever;
use crate::model::{Addon, App, NetworkGroup, Source};

/// In-memory view of what currently lives in an org. Shape parallels
/// `model::Project` so callers (e.g. `status`) can diff project-vs-live
/// without translating between two representations. Fields that aren't
/// exposed by `clever` in JSON mode are left at their defaults (see the
/// notes on `read.rs` for the same caveats around `scalability`, addon
/// `version`, app source branch, etc.).
#[derive(Debug, Clone)]
pub struct LiveSnapshot {
    pub apps: IndexMap<String, App>,
    pub addons: IndexMap<String, Addon>,
    pub network_groups: IndexMap<String, NetworkGroup>,
    /// Region we picked as the "default" for the org based on majority vote
    /// across apps + addons. Used so per-resource regions are only emitted
    /// when they differ from this default — matches the `read` heuristic.
    pub default_region: String,
}

/// Pull the full live snapshot for an org. Three list calls + per-app env,
/// domains and services. NGs come from `list_network_groups`.
pub fn snapshot(clever: &Clever, org: &str) -> Result<LiveSnapshot> {
    info!("listing live resources in org `{org}`");
    let all_apps = clever
        .list_apps(org)
        .with_context(|| format!("listing applications in org `{org}`"))?;
    let all_addons = clever
        .list_addons(org)
        .with_context(|| format!("listing addons in org `{org}`"))?;
    let all_ngs = clever
        .list_network_groups(org)
        .with_context(|| format!("listing network groups in org `{org}`"))?;

    let default_region = pick_default_region(&all_apps, &all_addons);

    let app_name_by_id: HashMap<String, String> = all_apps
        .iter()
        .map(|a| (a.app_id.clone(), a.name.clone()))
        .collect();
    let addon_name_by_id: HashMap<String, String> = all_addons
        .iter()
        .map(|a| (a.addon_id.clone(), a.name.clone()))
        .collect();
    // NGs link by member id which can be an app id or an addon's real_id.
    let addon_name_by_real_id: HashMap<String, String> = all_addons
        .iter()
        .map(|a| (a.real_id.clone(), a.name.clone()))
        .collect();

    let mut apps: IndexMap<String, App> = IndexMap::new();
    for listed in &all_apps {
        let env_vars = clever
            .get_env(&listed.app_id)
            .with_context(|| format!("reading env of app `{}`", listed.name))?;
        let domains = clever
            .get_domains(&listed.app_id)
            .with_context(|| format!("reading domains of app `{}`", listed.name))?;
        let services = clever
            .get_services(&listed.app_id)
            .with_context(|| format!("reading services of app `{}`", listed.name))?;

        let mut dependencies: Vec<String> = Vec::new();
        for s in services.addons {
            match addon_name_by_id.get(&s.id) {
                Some(name) => dependencies.push(name.clone()),
                None => warn!(
                    "addon dependency `{}` of app `{}` not found in org listing",
                    s.id, listed.name
                ),
            }
        }
        for s in services.applications {
            match app_name_by_id.get(&s.id) {
                Some(name) => dependencies.push(name.clone()),
                None => warn!(
                    "app dependency `{}` of app `{}` not found in org listing",
                    s.id, listed.name
                ),
            }
        }

        let env: IndexMap<String, String> =
            env_vars.into_iter().map(|v| (v.name, v.value)).collect();

        let user_domains: Vec<String> = domains
            .into_iter()
            .map(|d| d.hostname)
            .filter(|h| !h.ends_with(".cleverapps.io"))
            .collect();

        let source = listed
            .deploy_url
            .clone()
            .map(|from| Source { from, branch: None });

        apps.insert(
            listed.name.clone(),
            App {
                name: listed.name.clone(),
                kind: listed.kind.clone(),
                region: (listed.zone != default_region).then(|| listed.zone.clone()),
                source,
                domains: user_domains,
                scalability: None,
                dependencies,
                config: IndexMap::new(),
                env,
            },
        );
    }

    let mut addons: IndexMap<String, Addon> = IndexMap::new();
    for listed in &all_addons {
        addons.insert(
            listed.name.clone(),
            Addon {
                name: listed.name.clone(),
                kind: strip_addon_suffix(&listed.provider_id).to_string(),
                size: Some(listed.plan_slug.clone()),
                crypted: false,
                region: (listed.region != default_region).then(|| listed.region.clone()),
                version: None,
                backup_path: None,
            },
        );
    }

    let mut network_groups: IndexMap<String, NetworkGroup> = IndexMap::new();
    for listed in &all_ngs {
        let mut link: Vec<String> = Vec::new();
        for m in &listed.members {
            if let Some(name) = app_name_by_id.get(&m.id) {
                link.push(name.clone());
            } else if let Some(name) = addon_name_by_real_id.get(&m.id) {
                link.push(name.clone());
            } else {
                warn!(
                    "network group `{}` member `{}` not found in org apps/addons",
                    listed.label, m.id
                );
            }
        }
        network_groups.insert(
            listed.label.clone(),
            NetworkGroup {
                name: listed.label.clone(),
                description: None,
                link,
            },
        );
    }

    Ok(LiveSnapshot {
        apps,
        addons,
        network_groups,
        default_region,
    })
}

fn pick_default_region(
    apps: &[crate::clever::ListedApp],
    addons: &[crate::clever::ListedAddon],
) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for a in apps {
        *counts.entry(a.zone.clone()).or_default() += 1;
    }
    for a in addons {
        *counts.entry(a.region.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(k, _)| k)
        .unwrap_or_else(|| "par".to_string())
}

fn strip_addon_suffix(provider_id: &str) -> &str {
    provider_id.strip_suffix("-addon").unwrap_or(provider_id)
}
