use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::clever::Clever;
use crate::cli::DeleteArgs;
use crate::commands::OrgCache;
use crate::model::Project;
use crate::state::{ResourceKind, State};

/// Delete every network group, app and addon listed in the project file.
/// Network groups go first so their members are released before we tear them
/// down; then apps before addons so service links don't dangle. Each lookup
/// tries state first (avoiding an org-wide listing when we already know the
/// id); on `clever delete` failure we assume the state entry is stale, drop
/// it, invalidate the cache, and retry once via a fresh listing.
pub fn run(args: DeleteArgs) -> Result<()> {
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
    let mut cache = OrgCache::new();
    let mut failures = 0usize;

    for (key, ng) in &project.network_groups {
        if let Err(e) = delete_resource(
            &clever,
            &mut state,
            &mut cache,
            &project,
            key,
            &ng.name,
            ResourceKind::NetworkGroup,
        ) {
            warn!(
                "failed to delete network group `{}`: {e:#} — continuing",
                ng.name
            );
            failures += 1;
        }
    }

    for (key, app) in &project.apps {
        if let Err(e) = delete_resource(
            &clever,
            &mut state,
            &mut cache,
            &project,
            key,
            &app.name,
            ResourceKind::App,
        ) {
            warn!("failed to delete app `{}`: {e:#} — continuing", app.name);
            failures += 1;
        }
    }

    for (key, addon) in &project.addons {
        if let Err(e) = delete_resource(
            &clever,
            &mut state,
            &mut cache,
            &project,
            key,
            &addon.name,
            ResourceKind::Addon,
        ) {
            warn!(
                "failed to delete addon `{}`: {e:#} — continuing",
                addon.name
            );
            failures += 1;
        }
    }

    if !clever.is_dry_run() {
        state
            .save()
            .with_context(|| format!("saving state file `{}`", state.path().display()))?;
    }

    if failures > 0 {
        warn!("delete finished with {failures} failure(s); see warnings above");
    } else {
        info!("delete complete");
    }
    Ok(())
}

/// Resolve `name` to an id (state first, then a fresh listing on miss or
/// after a stale-state failure), call `clever delete`, and update state.
fn delete_resource(
    clever: &Clever,
    state: &mut State,
    cache: &mut OrgCache,
    project: &Project,
    key: &str,
    name: &str,
    kind: ResourceKind,
) -> Result<()> {
    let kind_label = match kind {
        ResourceKind::App => "app",
        ResourceKind::Addon => "addon",
        ResourceKind::NetworkGroup => "network group",
    };

    // Try state first.
    if let Some(r) = state.find(kind, name, &project.org) {
        let id = r.id.clone();
        info!("deleting {kind_label} `{name}` ({id}) [project key: {key}, from state]");
        match call_delete(clever, kind, &id, &project.org) {
            Ok(()) => {
                if !clever.is_dry_run() {
                    state.remove_by_id(&id);
                }
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "delete via state-known id `{id}` failed: {e:#} — dropping stale entry and refreshing from clever"
                );
                state.remove_by_id(&id);
                cache.invalidate();
                // fall through to listing path
            }
        }
    }

    // Listing path. For NGs we can pass the label directly (clever ng delete
    // accepts ng-id or ng-label), so no listing call is needed there.
    let fresh_id = match kind {
        ResourceKind::App => cache
            .apps(clever, &project.org)?
            .get(name)
            .map(|a| a.app_id.clone()),
        ResourceKind::Addon => cache
            .addons(clever, &project.org)?
            .get(name)
            .map(|a| a.addon_id.clone()),
        ResourceKind::NetworkGroup => Some(name.to_string()),
    };

    match fresh_id {
        Some(id) => {
            info!("deleting {kind_label} `{name}` ({id}) [project key: {key}, from listing]");
            call_delete(clever, kind, &id, &project.org)?;
            if !clever.is_dry_run() {
                state.remove_by_id(&id);
            }
            Ok(())
        }
        None => {
            warn!(
                "{kind_label} `{name}` not found in state or org `{}` — skipping",
                project.org
            );
            Ok(())
        }
    }
}

fn call_delete(clever: &Clever, kind: ResourceKind, id: &str, org: &str) -> Result<()> {
    match kind {
        ResourceKind::App => clever.delete_app(id),
        ResourceKind::Addon => clever.delete_addon(id, org),
        ResourceKind::NetworkGroup => clever.delete_network_group(id, org),
    }
}
