use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result};
use indexmap::IndexMap;
use tracing::{info, warn};

use crate::clever::Clever;
use crate::model::{Addon, App, NetworkGroup, Project, Source};

/// In-memory view of what currently lives in an org, scoped to the
/// resources the project file cares about. `apps` / `addons` /
/// `network_groups` only hold detailed entries for resources whose name
/// matches one in the project file — we deliberately avoid fetching
/// env / domains / services for unrelated apps in the same org. The
/// `live_*_names` sets carry just the names of every resource the org
/// returns, so callers that need to detect orphans (e.g. `status`) can
/// still do so cheaply.
#[derive(Debug, Clone)]
pub struct LiveSnapshot {
    pub apps: IndexMap<String, App>,
    pub addons: IndexMap<String, Addon>,
    pub network_groups: IndexMap<String, NetworkGroup>,
    /// Region we picked as the "default" for the org based on majority vote
    /// across apps + addons. Used so per-resource regions are only emitted
    /// when they differ from this default — matches the `read` heuristic.
    pub default_region: String,
    /// Names of every app the org returned (regardless of whether the
    /// project file mentions them). Cheap — populated from the listing.
    pub live_app_names: BTreeSet<String>,
    pub live_addon_names: BTreeSet<String>,
    pub live_ng_names: BTreeSet<String>,
}

/// Snapshot the org, but only fetch detailed env / domains / services for
/// apps (and detailed plans for addons / NGs) whose name appears in the
/// project file. The three org-wide list calls stay — they're cheap and
/// needed to know which project resources already exist.
pub fn snapshot(clever: &Clever, org: &str, project: &Project) -> Result<LiveSnapshot> {
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

    // Resource names mentioned by the project file. These are the only ones
    // worth pulling detailed info for — everything else is noise that costs
    // 3 extra API calls per app.
    let project_app_names: HashSet<&str> = project.apps.values().map(|a| a.name.as_str()).collect();
    let project_addon_names: HashSet<&str> =
        project.addons.values().map(|a| a.name.as_str()).collect();
    let project_ng_names: HashSet<&str> = project
        .network_groups
        .values()
        .map(|n| n.name.as_str())
        .collect();

    // Indexes for dependency resolution inside the apps we *do* fetch in
    // detail. We need ids → names for the full org because a project app
    // may depend on an addon or app outside the file.
    let app_name_by_id: HashMap<String, String> = all_apps
        .iter()
        .map(|a| (a.app_id.clone(), a.name.clone()))
        .collect();
    let addon_name_by_id: HashMap<String, String> = all_addons
        .iter()
        .map(|a| (a.addon_id.clone(), a.name.clone()))
        .collect();
    let addon_name_by_real_id: HashMap<String, String> = all_addons
        .iter()
        .map(|a| (a.real_id.clone(), a.name.clone()))
        .collect();

    // Detailed apps — only for those in the project file. One round-trip
    // pulls env + vhosts + scalability + build at once; services remain a
    // separate call (the per-app details endpoint doesn't expose them).
    let mut apps: IndexMap<String, App> = IndexMap::new();
    for listed in &all_apps {
        if !project_app_names.contains(listed.name.as_str()) {
            continue;
        }
        let details = clever
            .get_app_details(org, &listed.app_id)
            .with_context(|| format!("reading details of app `{}`", listed.name))?;
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
            details.env.into_iter().map(|v| (v.name, v.value)).collect();

        let user_domains: Vec<String> = details
            .vhosts
            .into_iter()
            .filter(|h| !h.ends_with(".cleverapps.io"))
            .collect();

        let source = listed.deploy_url.clone().map(|from| Source {
            from,
            branch: details.branch.clone(),
        });

        apps.insert(
            listed.name.clone(),
            App {
                name: listed.name.clone(),
                kind: listed.kind.clone(),
                region: (listed.zone != default_region).then(|| listed.zone.clone()),
                source,
                domains: user_domains,
                scalability: Some(details.scalability),
                build: details.build,
                dependencies,
                config: IndexMap::new(),
                env,
                hooks: None,
            },
        );
    }

    // Detailed addons — only those in the project file. The listing is
    // already detailed enough to fill in `kind` / `size` / `region`, no
    // extra per-addon call.
    let mut addons: IndexMap<String, Addon> = IndexMap::new();
    for listed in &all_addons {
        if !project_addon_names.contains(listed.name.as_str()) {
            continue;
        }
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

    // Detailed NGs — only those in the project file.
    let mut network_groups: IndexMap<String, NetworkGroup> = IndexMap::new();
    for listed in &all_ngs {
        if !project_ng_names.contains(listed.label.as_str()) {
            continue;
        }
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

    let live_app_names: BTreeSet<String> = all_apps.iter().map(|a| a.name.clone()).collect();
    let live_addon_names: BTreeSet<String> = all_addons.iter().map(|a| a.name.clone()).collect();
    let live_ng_names: BTreeSet<String> = all_ngs.iter().map(|n| n.label.clone()).collect();

    Ok(LiveSnapshot {
        apps,
        addons,
        network_groups,
        default_region,
        live_app_names,
        live_addon_names,
        live_ng_names,
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
