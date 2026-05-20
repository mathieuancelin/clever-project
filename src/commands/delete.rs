use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tracing::{info, warn};

use crate::clever::Clever;
use crate::cli::DeleteArgs;
use crate::commands::prompt;
use crate::commands::targets::{self as targets_mod, TargetKind, Targets};
use crate::commands::{OrgCache, resolve_project_file};
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
                .with_context(|| format!("loading --variables-file-path `{}`", path.display()))?,
        );
    }
    variables.extend(args.variables);
    if let Some(env) = args.env {
        variables.push(("env".to_string(), env));
    }
    let file = resolve_project_file(args.file, &std::env::current_dir()?)?;
    let (project, _resolver) = Project::load_and_resolve(
        &file,
        args.org,
        args.region,
        &variables,
        args.secrets_path.as_deref(),
        &args.secrets,
    )
    .with_context(|| format!("loading project `{}`", file.display()))?;

    let clever = Clever::new()?.with_dry_run(args.dry_run);
    if clever.is_dry_run() {
        info!("[dry-run] no mutations will be sent to Clever Cloud");
    }

    let targets = targets_mod::build(&args.targets, &project)
        .with_context(|| "validating --target flags".to_string())?;

    let total = count_targets(&project, &targets);

    if args.format.is_json() {
        let payload = json_plan(&project, &targets);
        let out = serde_json::to_string_pretty(&payload).context("serializing JSON plan")?;
        println!("{out}");
    } else {
        print!("{}", render_delete_plan(&project, &targets));
    }

    if args.dry_run {
        info!("dry-run: {total} resource(s) would be deleted");
        return Ok(());
    }
    if total == 0 {
        info!("nothing to delete — project file has no resources matching the current targets");
        return Ok(());
    }
    if !args.yes {
        if args.format.is_json() {
            bail!("--format json requires --yes (no prompts in JSON mode)");
        }
        if !prompt::stdin_is_tty() {
            bail!(
                "stdin is not a TTY and --yes was not given; pass --yes (or --auto-approve) to run delete non-interactively"
            );
        }
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        let approved =
            prompt::ask_yes_no("\nDestroy these resources", false, &mut stdin, &mut stdout)?;
        if !approved {
            bail!("aborted by user");
        }
    }

    let mut state = State::load(&file)?;

    let _lock_guard = if args.no_lock {
        warn!("--no-lock passed — running without a state lock");
        None
    } else {
        Some(crate::lock::acquire(state.path(), "delete", &file)?)
    };

    let effective_env = variables
        .iter()
        .rev()
        .find(|(k, _)| k == "env")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "prod".to_string());

    run_pre_delete_hooks(&project, &targets, &file, &effective_env, args.skip_hooks)?;

    let mut cache = OrgCache::new();
    let mut failures = 0usize;

    for (key, ng) in &project.network_groups {
        if !targets.is_targeted(TargetKind::NetworkGroup, key) {
            continue;
        }
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
        if !targets.is_targeted(TargetKind::App, key) {
            continue;
        }
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
        if !targets.is_targeted(TargetKind::Addon, key) {
            continue;
        }
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

    run_post_delete_hooks(&project, &targets, &file, &effective_env, args.skip_hooks)?;

    if failures > 0 {
        warn!("delete finished with {failures} failure(s); see warnings above");
    } else {
        info!("delete complete");
    }
    Ok(())
}

fn run_pre_delete_hooks(
    project: &Project,
    targets: &Targets,
    project_path: &std::path::Path,
    env: &str,
    skip: bool,
) -> Result<()> {
    use crate::hooks::{HookAppContext, HookContext, HookOperation, HookPhase, run_hook};
    if let Some(cmd) = project.hooks.as_ref().and_then(|h| h.pre_delete.as_deref()) {
        let ctx = HookContext {
            operation: HookOperation::Delete,
            phase: HookPhase::Pre,
            project_path,
            org: &project.org,
            region: &project.region,
            env,
            app: None,
        };
        run_hook(cmd, &ctx, false, skip)?;
    }
    for (key, app) in &project.apps {
        if !targets.is_targeted(TargetKind::App, key) {
            continue;
        }
        let Some(cmd) = app.hooks.as_ref().and_then(|h| h.pre_delete.as_deref()) else {
            continue;
        };
        let ctx = HookContext {
            operation: HookOperation::Delete,
            phase: HookPhase::Pre,
            project_path,
            org: &project.org,
            region: &project.region,
            env,
            app: Some(HookAppContext {
                key,
                name: &app.name,
                kind: &app.kind,
            }),
        };
        run_hook(cmd, &ctx, false, skip)?;
    }
    Ok(())
}

fn run_post_delete_hooks(
    project: &Project,
    targets: &Targets,
    project_path: &std::path::Path,
    env: &str,
    skip: bool,
) -> Result<()> {
    use crate::hooks::{HookAppContext, HookContext, HookOperation, HookPhase, run_hook};
    for (key, app) in &project.apps {
        if !targets.is_targeted(TargetKind::App, key) {
            continue;
        }
        let Some(cmd) = app.hooks.as_ref().and_then(|h| h.post_delete.as_deref()) else {
            continue;
        };
        let ctx = HookContext {
            operation: HookOperation::Delete,
            phase: HookPhase::Post,
            project_path,
            org: &project.org,
            region: &project.region,
            env,
            app: Some(HookAppContext {
                key,
                name: &app.name,
                kind: &app.kind,
            }),
        };
        run_hook(cmd, &ctx, false, skip)?;
    }
    if let Some(cmd) = project
        .hooks
        .as_ref()
        .and_then(|h| h.post_delete.as_deref())
    {
        let ctx = HookContext {
            operation: HookOperation::Delete,
            phase: HookPhase::Post,
            project_path,
            org: &project.org,
            region: &project.region,
            env,
            app: None,
        };
        run_hook(cmd, &ctx, false, skip)?;
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

#[derive(Debug, Serialize)]
struct JsonDeletePlan<'a> {
    project: &'a str,
    org: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    targeting: Vec<String>,
    summary: JsonDeleteSummary,
    network_groups: Vec<&'a str>,
    apps: Vec<&'a str>,
    addons: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
struct JsonDeleteSummary {
    to_destroy: usize,
}

fn json_plan<'a>(project: &'a Project, targets: &'a Targets) -> JsonDeletePlan<'a> {
    let ng_names: Vec<&str> = project
        .network_groups
        .iter()
        .filter(|(k, _)| targets.is_targeted(TargetKind::NetworkGroup, k))
        .map(|(_, n)| n.name.as_str())
        .collect();
    let app_names: Vec<&str> = project
        .apps
        .iter()
        .filter(|(k, _)| targets.is_targeted(TargetKind::App, k))
        .map(|(_, a)| a.name.as_str())
        .collect();
    let addon_names: Vec<&str> = project
        .addons
        .iter()
        .filter(|(k, _)| targets.is_targeted(TargetKind::Addon, k))
        .map(|(_, a)| a.name.as_str())
        .collect();
    let total = ng_names.len() + app_names.len() + addon_names.len();
    let mut targeting: Vec<String> = Vec::new();
    for k in &targets.apps {
        targeting.push(format!("apps.{k}"));
    }
    for k in &targets.addons {
        targeting.push(format!("addons.{k}"));
    }
    for k in &targets.network_groups {
        targeting.push(format!("network_groups.{k}"));
    }
    JsonDeletePlan {
        project: &project.name,
        org: &project.org,
        targeting,
        summary: JsonDeleteSummary { to_destroy: total },
        network_groups: ng_names,
        apps: app_names,
        addons: addon_names,
    }
}

fn count_targets(project: &Project, targets: &Targets) -> usize {
    let ngs = project
        .network_groups
        .keys()
        .filter(|k| targets.is_targeted(TargetKind::NetworkGroup, k))
        .count();
    let apps = project
        .apps
        .keys()
        .filter(|k| targets.is_targeted(TargetKind::App, k))
        .count();
    let addons = project
        .addons
        .keys()
        .filter(|k| targets.is_targeted(TargetKind::Addon, k))
        .count();
    ngs + apps + addons
}

/// Render the list of resources delete will attempt to remove, in the order
/// it will attempt them (NGs first to release members, then apps, then
/// addons). No live API call is made here — delete is best-effort and will
/// skip anything that's already gone with a warning at run time.
fn render_delete_plan(project: &Project, targets: &Targets) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Plan for project `{}` against org `{}`:",
        project.name, project.org
    );
    if !targets.is_empty() {
        let _ = writeln!(out, "  {}", targets.label());
    }

    let ng_keys: Vec<&String> = project
        .network_groups
        .keys()
        .filter(|k| targets.is_targeted(TargetKind::NetworkGroup, k))
        .collect();
    let app_keys: Vec<&String> = project
        .apps
        .keys()
        .filter(|k| targets.is_targeted(TargetKind::App, k))
        .collect();
    let addon_keys: Vec<&String> = project
        .addons
        .keys()
        .filter(|k| targets.is_targeted(TargetKind::Addon, k))
        .collect();
    let total = ng_keys.len() + app_keys.len() + addon_keys.len();

    if total == 0 {
        let _ = writeln!(
            out,
            "  (nothing to delete — project file or current targets match no resources)"
        );
        return out;
    }
    let _ = writeln!(
        out,
        "  {total} to destroy: {} network_group, {} app, {} addon.",
        ng_keys.len(),
        app_keys.len(),
        addon_keys.len()
    );
    let _ = writeln!(out);
    for k in &ng_keys {
        let _ = writeln!(
            out,
            "  - network_group \"{}\"",
            project.network_groups[*k].name
        );
    }
    for k in &app_keys {
        let _ = writeln!(out, "  - app \"{}\"", project.apps[*k].name);
    }
    for k in &addon_keys {
        let _ = writeln!(out, "  - addon \"{}\"", project.addons[*k].name);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addon, App, NetworkGroup};
    use indexmap::IndexMap;

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
            hooks: None,
        }
    }

    fn make_app(name: &str) -> App {
        App {
            name: name.into(),
            kind: "node".into(),
            region: None,
            source: None,
            domains: vec![],
            scalability: None,
            build: None,
            dependencies: vec![],
            config: IndexMap::new(),
            env: IndexMap::new(),
            hooks: None,
        }
    }

    fn make_addon(name: &str) -> Addon {
        Addon {
            name: name.into(),
            kind: "postgresql".into(),
            size: None,
            crypted: false,
            region: None,
            version: None,
            backup_path: None,
        }
    }

    fn make_ng(name: &str) -> NetworkGroup {
        NetworkGroup {
            name: name.into(),
            description: None,
            link: vec![],
        }
    }

    #[test]
    fn empty_project_renders_friendly_message() {
        let project = make_project();
        let s = render_delete_plan(&project, &Targets::default());
        assert!(s.contains("nothing to delete"));
    }

    #[test]
    fn render_orders_ng_app_addon() {
        let mut project = make_project();
        project.apps.insert("a".into(), make_app("prod-api"));
        project.addons.insert("d".into(), make_addon("prod-db"));
        project.network_groups.insert("n".into(), make_ng("vpn"));
        let s = render_delete_plan(&project, &Targets::default());
        // NGs first, apps next, addons last.
        let ng_pos = s.find("vpn").unwrap();
        let app_pos = s.find("prod-api").unwrap();
        let addon_pos = s.find("prod-db").unwrap();
        assert!(ng_pos < app_pos);
        assert!(app_pos < addon_pos);
        assert!(s.contains("3 to destroy"));
    }

    #[test]
    fn render_summary_counts_each_type() {
        let mut project = make_project();
        project.apps.insert("a".into(), make_app("x"));
        project.apps.insert("b".into(), make_app("y"));
        project.addons.insert("d".into(), make_addon("z"));
        let s = render_delete_plan(&project, &Targets::default());
        assert!(s.contains("3 to destroy: 0 network_group, 2 app, 1 addon"));
    }

    #[test]
    fn targets_filter_delete_plan() {
        let mut project = make_project();
        project.apps.insert("a".into(), make_app("prod-api"));
        project.apps.insert("b".into(), make_app("prod-worker"));
        project.addons.insert("d".into(), make_addon("prod-db"));
        let mut targets = Targets::default();
        targets.apps.insert("a".into());

        let s = render_delete_plan(&project, &targets);
        assert!(s.contains("1 to destroy: 0 network_group, 1 app, 0 addon"));
        assert!(s.contains("prod-api"));
        assert!(!s.contains("prod-worker"));
        assert!(!s.contains("prod-db"));
        assert!(s.contains("(targeting: apps.a)"));
    }

    #[test]
    fn targets_with_no_matches_renders_empty_message() {
        // Edge: project has addons but only an app target → nothing matches.
        let mut project = make_project();
        project.addons.insert("d".into(), make_addon("prod-db"));
        let mut targets = Targets::default();
        targets.apps.insert("nonexistent".into());
        let s = render_delete_plan(&project, &targets);
        assert!(s.contains("nothing to delete"));
    }

    #[test]
    fn count_targets_respects_filter() {
        let mut project = make_project();
        project.apps.insert("a".into(), make_app("x"));
        project.apps.insert("b".into(), make_app("y"));
        project.addons.insert("d".into(), make_addon("z"));
        assert_eq!(count_targets(&project, &Targets::default()), 3);
        let mut targets = Targets::default();
        targets.addons.insert("d".into());
        assert_eq!(count_targets(&project, &targets), 1);
    }
}
