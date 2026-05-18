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

    let clever = Clever::new()?;

    let existing_apps = clever
        .list_apps(&project.org)
        .with_context(|| format!("listing applications in org `{}`", project.org))?;
    let app_by_name: HashMap<String, String> = existing_apps
        .into_iter()
        .map(|a| (a.name, a.app_id))
        .collect();

    for (key, app) in &project.apps {
        match app_by_name.get(&app.name) {
            Some(id) => {
                info!("deleting app `{}` ({}) [project key: {key}]", app.name, id);
                clever
                    .delete_app(id)
                    .with_context(|| format!("deleting app `{}`", app.name))?;
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
                clever
                    .delete_addon(id, &project.org)
                    .with_context(|| format!("deleting addon `{}`", addon.name))?;
            }
            None => warn!(
                "addon `{}` not found in org `{}` — skipping",
                addon.name, project.org
            ),
        }
    }

    info!("delete complete");
    Ok(())
}
