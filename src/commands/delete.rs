use std::collections::HashMap;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::cli::DeleteArgs;
use crate::clever::Clever;
use crate::model::Project;

/// Delete every app and addon listed in the project file, looked up by `name`
/// inside the target organisation. Apps are deleted first so any service
/// links from app → addon are released before we touch the addons.
pub fn run(args: DeleteArgs) -> Result<()> {
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

    let clever = Clever::new()?.with_dry_run(args.dry_run);
    if clever.is_dry_run() {
        info!("[dry-run] no mutations will be sent to Clever Cloud");
    }

    let existing_apps = clever
        .list_apps(&project.org)
        .with_context(|| format!("listing applications in org `{}`", project.org))?;
    let app_by_name: HashMap<String, String> = existing_apps
        .into_iter()
        .map(|a| (a.name, a.app_id))
        .collect();

    let mut failures = 0usize;
    for (key, app) in &project.apps {
        match app_by_name.get(&app.name) {
            Some(id) => {
                info!("deleting app `{}` ({}) [project key: {key}]", app.name, id);
                if let Err(e) = clever.delete_app(id) {
                    warn!("failed to delete app `{}`: {e:#} — continuing", app.name);
                    failures += 1;
                }
            }
            None => warn!(
                "app `{}` not found in org `{}` — skipping",
                app.name, project.org
            ),
        }
    }

    let existing_addons = clever
        .list_addons(&project.org)
        .with_context(|| format!("listing addons in org `{}`", project.org))?;
    let addon_by_name: HashMap<String, String> = existing_addons
        .into_iter()
        .map(|a| (a.name, a.addon_id))
        .collect();

    for (key, addon) in &project.addons {
        match addon_by_name.get(&addon.name) {
            Some(id) => {
                info!(
                    "deleting addon `{}` ({}) [project key: {key}]",
                    addon.name, id
                );
                if let Err(e) = clever.delete_addon(id, &project.org) {
                    warn!("failed to delete addon `{}`: {e:#} — continuing", addon.name);
                    failures += 1;
                }
            }
            None => warn!(
                "addon `{}` not found in org `{}` — skipping",
                addon.name, project.org
            ),
        }
    }

    if failures > 0 {
        warn!("delete finished with {failures} failure(s); see warnings above");
    } else {
        info!("delete complete");
    }
    Ok(())
}
