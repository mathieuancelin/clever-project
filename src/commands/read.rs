use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Serialize;
use tracing::{info, warn};

use crate::clever::{Clever, ListedAddon, ListedApp};
use crate::cli::ReadArgs;
use crate::model::{Addon, App, Project, Source};

pub fn run(args: ReadArgs) -> Result<()> {
    if !args.all && args.apps.is_empty() && args.addons.is_empty() {
        bail!("nothing to read — pass --app/--addon explicitly, or --all");
    }

    let clever = Clever::new()?;

    let all_apps = clever
        .list_apps(&args.org)
        .with_context(|| format!("listing applications in org `{}`", args.org))?;
    let all_addons = clever
        .list_addons(&args.org)
        .with_context(|| format!("listing addons in org `{}`", args.org))?;

    let (selected_apps, selected_addons) = if args.all {
        (all_apps.clone(), all_addons.clone())
    } else {
        let apps: Vec<ListedApp> = args
            .apps
            .iter()
            .map(|needle| {
                all_apps
                    .iter()
                    .find(|a| &a.name == needle || &a.app_id == needle)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!("app `{needle}` not found in org `{}`", args.org)
                    })
            })
            .collect::<Result<_>>()?;
        let addons: Vec<ListedAddon> = args
            .addons
            .iter()
            .map(|needle| {
                all_addons
                    .iter()
                    .find(|a| &a.name == needle || &a.addon_id == needle)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!("addon `{needle}` not found in org `{}`", args.org)
                    })
            })
            .collect::<Result<_>>()?;
        (apps, addons)
    };

    let default_region = pick_default_region(&selected_apps, &selected_addons);

    // Index by id so we can resolve dependency ids -> names later.
    let app_name_by_id: HashMap<String, String> = all_apps
        .iter()
        .map(|a| (a.app_id.clone(), a.name.clone()))
        .collect();
    let addon_name_by_id: HashMap<String, String> = all_addons
        .iter()
        .map(|a| (a.addon_id.clone(), a.name.clone()))
        .collect();

    let mut apps: IndexMap<String, App> = IndexMap::new();
    for listed in &selected_apps {
        info!("reading app `{}` ({})", listed.name, listed.app_id);
        // One call returns env + vhosts + scalability + build. Services still
        // come from a separate endpoint — keep that call.
        let details = clever
            .get_app_details(&args.org, &listed.app_id)
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
                config: IndexMap::new(), // out of scope for the prototype
                env,
                hooks: None,
            },
        );
    }

    let mut addons: IndexMap<String, Addon> = IndexMap::new();
    for listed in selected_addons {
        info!("reading addon `{}` ({})", listed.name, listed.addon_id);
        addons.insert(
            listed.name.clone(),
            Addon {
                name: listed.name.clone(),
                kind: strip_addon_suffix(&listed.provider_id).to_string(),
                size: Some(listed.plan_slug),
                crypted: false, // not exposed by `clever addon list`
                region: (listed.region != default_region).then_some(listed.region),
                version: None, // not exposed by `clever addon list`
                backup_path: None,
                env: IndexMap::new(),
                domains: vec![],
            },
        );
    }

    let project = Project {
        name: format!("imported-from-{}", args.org),
        description: None,
        org: args.org.clone(),
        region: default_region,
        variables: IndexMap::new(),
        apps,
        addons,
        network_groups: IndexMap::new(),
        hooks: None,
        display: IndexMap::new(),
    };

    project
        .save(&args.output)
        .with_context(|| format!("writing project file `{}`", args.output.display()))?;

    if args.format.is_json() {
        #[derive(Serialize)]
        struct ReadReport {
            wrote: String,
            org: String,
            apps: usize,
            addons: usize,
        }
        let payload = ReadReport {
            wrote: args.output.display().to_string(),
            org: args.org.clone(),
            apps: project.apps.len(),
            addons: project.addons.len(),
        };
        let out = serde_json::to_string_pretty(&payload).context("serializing JSON report")?;
        println!("{out}");
    } else {
        info!("wrote project to `{}`", args.output.display());
    }
    Ok(())
}

fn pick_default_region(apps: &[ListedApp], addons: &[ListedAddon]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_addon_suffix() {
        assert_eq!(strip_addon_suffix("postgresql-addon"), "postgresql");
        assert_eq!(strip_addon_suffix("redis-addon"), "redis");
        assert_eq!(strip_addon_suffix("cellar-addon"), "cellar");
        assert_eq!(strip_addon_suffix("kv"), "kv");
    }
}
