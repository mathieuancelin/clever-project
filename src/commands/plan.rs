//! What `apply` will do, computed once at the top of the run.
//!
//! Mirrors the apply pipeline so the categories match what the phases will
//! actually mutate:
//!
//! - **App create**: app missing from the live org; the plan lists the kind,
//!   region, source, and the initial env / domains / dependencies.
//! - **App update**: app exists; the plan only lists fields apply will touch
//!   on existing apps — env, domains, dependencies. Other drift (`kind`,
//!   `region`, `source.from`) is surfaced as informational warnings since
//!   apply won't auto-update those.
//! - **Addon create**: addon missing; the plan shows the provider, size, and
//!   region.
//! - **Addon present**: phase 1 never updates existing addons, so any drift
//!   is informational only.
//! - **NG create**: network group missing; the plan shows initial members.
//! - **NG update**: members synced against the file.
//!
//! `delete`'s plan isn't covered yet — this is `apply`-side only for v1.

use std::fmt::Write;

use indexmap::IndexMap;

use crate::commands::diff::{
    DiffBody, FieldDiff, diff_map, diff_set, kinds_equivalent, quote_escape, sizes_equivalent,
};
use crate::commands::live::LiveSnapshot;
use crate::commands::targets::{TargetKind, Targets};
use crate::model::{Addon, App, Project};

#[derive(Debug, Default)]
pub struct Plan {
    pub apps: Vec<AppOp>,
    pub addons: Vec<AddonOp>,
    pub network_groups: Vec<NgOp>,
}

#[derive(Debug)]
pub struct AppOp {
    pub name: String,
    pub kind: AppOpKind,
}

#[derive(Debug)]
pub enum AppOpKind {
    Create {
        kind: String,
        region: String,
        source: Option<String>,
        env: IndexMap<String, String>,
        domains: Vec<String>,
        dependencies: Vec<String>,
    },
    /// `mutations`: things apply will change (env / domains / dependencies).
    /// `non_mutable_drift`: drift on fields apply won't touch (kind, region,
    /// source). Both empty means no-op.
    Existing {
        mutations: Vec<FieldDiff>,
        non_mutable_drift: Vec<FieldDiff>,
    },
}

#[derive(Debug)]
pub struct AddonOp {
    pub name: String,
    pub kind: AddonOpKind,
}

#[derive(Debug)]
pub enum AddonOpKind {
    Create {
        provider: String,
        size: Option<String>,
        region: String,
    },
    /// Apply doesn't update existing addons; any drift on them is purely
    /// informational.
    Existing { drift: Vec<FieldDiff> },
}

#[derive(Debug)]
pub struct NgOp {
    pub name: String,
    pub kind: NgOpKind,
}

#[derive(Debug)]
pub enum NgOpKind {
    Create { members: Vec<String> },
    Existing { mutations: Vec<FieldDiff> },
}

impl Plan {
    /// Number of resources apply will actually mutate (create or update).
    pub fn mutation_count(&self) -> usize {
        let app_changes = self
            .apps
            .iter()
            .filter(|o| match &o.kind {
                AppOpKind::Create { .. } => true,
                AppOpKind::Existing { mutations, .. } => !mutations.is_empty(),
            })
            .count();
        let addon_changes = self
            .addons
            .iter()
            .filter(|o| matches!(o.kind, AddonOpKind::Create { .. }))
            .count();
        let ng_changes = self
            .network_groups
            .iter()
            .filter(|o| match &o.kind {
                NgOpKind::Create { .. } => true,
                NgOpKind::Existing { mutations } => !mutations.is_empty(),
            })
            .count();
        app_changes + addon_changes + ng_changes
    }
}

pub fn compute(project: &Project, live: &LiveSnapshot, targets: &Targets) -> Plan {
    let mut plan = Plan::default();

    // Addons first — apply's phase 1 order, and apps may reference them.
    for (key, addon) in &project.addons {
        if !targets.is_targeted(TargetKind::Addon, key) {
            continue;
        }
        match live.addons.get(addon.name.as_str()) {
            None => plan.addons.push(AddonOp {
                name: addon.name.clone(),
                kind: AddonOpKind::Create {
                    provider: addon.kind.clone(),
                    size: addon.size.clone(),
                    region: addon
                        .region
                        .clone()
                        .unwrap_or_else(|| project.region.clone()),
                },
            }),
            Some(live_addon) => {
                let drift =
                    diff_addon_info(addon, live_addon, &project.region, &live.default_region);
                plan.addons.push(AddonOp {
                    name: addon.name.clone(),
                    kind: AddonOpKind::Existing { drift },
                });
            }
        }
    }

    // Apps.
    for (key, app) in &project.apps {
        if !targets.is_targeted(TargetKind::App, key) {
            continue;
        }
        match live.apps.get(app.name.as_str()) {
            None => plan.apps.push(AppOp {
                name: app.name.clone(),
                kind: AppOpKind::Create {
                    kind: app.kind.clone(),
                    region: app.region.clone().unwrap_or_else(|| project.region.clone()),
                    source: app.source.as_ref().map(|s| s.from.clone()),
                    env: app.env.clone(),
                    domains: app.domains.clone(),
                    dependencies: app.dependencies.clone(),
                },
            }),
            Some(live_app) => {
                let mutations = diff_app_mutable(app, live_app);
                let non_mutable_drift =
                    diff_app_non_mutable(app, live_app, &project.region, &live.default_region);
                plan.apps.push(AppOp {
                    name: app.name.clone(),
                    kind: AppOpKind::Existing {
                        mutations,
                        non_mutable_drift,
                    },
                });
            }
        }
    }

    // Network groups.
    for (key, ng) in &project.network_groups {
        if !targets.is_targeted(TargetKind::NetworkGroup, key) {
            continue;
        }
        match live.network_groups.get(ng.name.as_str()) {
            None => plan.network_groups.push(NgOp {
                name: ng.name.clone(),
                kind: NgOpKind::Create {
                    members: ng.link.clone(),
                },
            }),
            Some(live_ng) => {
                let mut mutations = Vec::new();
                if let Some(d) = diff_set("members", &ng.link, &live_ng.link) {
                    mutations.push(d);
                }
                plan.network_groups.push(NgOp {
                    name: ng.name.clone(),
                    kind: NgOpKind::Existing { mutations },
                });
            }
        }
    }

    plan
}

/// Fields apply will actually rewrite on existing apps: env, domains,
/// dependencies. Domains served by `*.cleverapps.io` are filtered out of the
/// live set since apply never removes them (Clever auto-manages them).
fn diff_app_mutable(file: &App, live: &App) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    // Domains: drop cleverapps.io entries from the live side before diffing,
    // matching apply's behaviour (it never removes them).
    let live_user_domains: Vec<String> = live
        .domains
        .iter()
        .filter(|d| !d.ends_with(".cleverapps.io"))
        .cloned()
        .collect();
    if let Some(d) = diff_set("domains", &file.domains, &live_user_domains) {
        diffs.push(d);
    }
    if let Some(d) = diff_set("dependencies", &file.dependencies, &live.dependencies) {
        diffs.push(d);
    }
    if let Some(d) = diff_map("env", &file.env, &live.env) {
        diffs.push(d);
    }
    diffs
}

fn diff_app_non_mutable(
    file: &App,
    live: &App,
    file_default: &str,
    live_default: &str,
) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    if file.kind != live.kind {
        diffs.push(FieldDiff {
            field: "kind".into(),
            body: DiffBody::Scalar {
                file: file.kind.clone(),
                live: live.kind.clone(),
            },
        });
    }
    let file_region = file.region.clone().unwrap_or_else(|| file_default.into());
    let live_region = live.region.clone().unwrap_or_else(|| live_default.into());
    if file_region != live_region {
        diffs.push(FieldDiff {
            field: "region".into(),
            body: DiffBody::Scalar {
                file: file_region,
                live: live_region,
            },
        });
    }
    let file_source = file.source.as_ref().map(|s| s.from.as_str()).unwrap_or("");
    let live_source = live.source.as_ref().map(|s| s.from.as_str()).unwrap_or("");
    if file_source != live_source {
        diffs.push(FieldDiff {
            field: "source.from".into(),
            body: DiffBody::Scalar {
                file: file_source.to_string(),
                live: live_source.to_string(),
            },
        });
    }
    diffs
}

fn diff_addon_info(
    file: &Addon,
    live: &Addon,
    file_default: &str,
    live_default: &str,
) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    if !kinds_equivalent(&file.kind, &live.kind) {
        diffs.push(FieldDiff {
            field: "kind".into(),
            body: DiffBody::Scalar {
                file: file.kind.clone(),
                live: live.kind.clone(),
            },
        });
    }
    let file_region = file.region.clone().unwrap_or_else(|| file_default.into());
    let live_region = live.region.clone().unwrap_or_else(|| live_default.into());
    if file_region != live_region {
        diffs.push(FieldDiff {
            field: "region".into(),
            body: DiffBody::Scalar {
                file: file_region,
                live: live_region,
            },
        });
    }
    if let (Some(fs), Some(ls)) = (file.size.as_deref(), live.size.as_deref())
        && !sizes_equivalent(fs, ls)
    {
        diffs.push(FieldDiff {
            field: "size".into(),
            body: DiffBody::Scalar {
                file: fs.to_string(),
                live: ls.to_string(),
            },
        });
    }
    diffs
}

pub fn render(plan: &Plan, project: &Project, targets: &Targets) -> String {
    let mut out = String::new();

    let mut to_create = 0;
    let mut to_update = 0;
    let mut unchanged = 0;
    for o in &plan.apps {
        match &o.kind {
            AppOpKind::Create { .. } => to_create += 1,
            AppOpKind::Existing { mutations, .. } if !mutations.is_empty() => to_update += 1,
            AppOpKind::Existing { .. } => unchanged += 1,
        }
    }
    for o in &plan.addons {
        match &o.kind {
            AddonOpKind::Create { .. } => to_create += 1,
            AddonOpKind::Existing { .. } => unchanged += 1,
        }
    }
    for o in &plan.network_groups {
        match &o.kind {
            NgOpKind::Create { .. } => to_create += 1,
            NgOpKind::Existing { mutations } if !mutations.is_empty() => to_update += 1,
            NgOpKind::Existing { .. } => unchanged += 1,
        }
    }

    let _ = writeln!(
        out,
        "Plan for project `{}` against org `{}` (default region `{}`):",
        project.name, project.org, project.region
    );
    if !targets.is_empty() {
        let _ = writeln!(out, "  {}", targets.label());
    }
    let _ = writeln!(
        out,
        "  {to_create} to create, {to_update} to update, {unchanged} unchanged."
    );
    let _ = writeln!(out);

    for o in &plan.addons {
        render_addon(&mut out, o);
    }
    for o in &plan.apps {
        render_app(&mut out, o);
    }
    for o in &plan.network_groups {
        render_ng(&mut out, o);
    }
    if plan.apps.is_empty() && plan.addons.is_empty() && plan.network_groups.is_empty() {
        let _ = writeln!(out, "  (project file is empty)");
    }

    out
}

fn render_app(out: &mut String, op: &AppOp) {
    match &op.kind {
        AppOpKind::Create {
            kind,
            region,
            source,
            env,
            domains,
            dependencies,
        } => {
            let src_hint = match source {
                Some(s) => format!(", github={s}"),
                None => String::new(),
            };
            let _ = writeln!(
                out,
                "  + app \"{}\" ({kind}, region={region}{src_hint})",
                op.name
            );
            if !env.is_empty() {
                let _ = writeln!(out, "      env:");
                for (k, v) in env {
                    let _ = writeln!(out, "        + {k} = \"{}\"", quote_escape(v));
                }
            }
            if !domains.is_empty() {
                let _ = writeln!(out, "      domains:");
                for d in domains {
                    let _ = writeln!(out, "        + {d}");
                }
            }
            if !dependencies.is_empty() {
                let _ = writeln!(out, "      dependencies:");
                for d in dependencies {
                    let _ = writeln!(out, "        + {d}");
                }
            }
        }
        AppOpKind::Existing {
            mutations,
            non_mutable_drift,
        } => {
            if mutations.is_empty() && non_mutable_drift.is_empty() {
                let _ = writeln!(out, "  = app \"{}\"", op.name);
                return;
            }
            if !mutations.is_empty() {
                let _ = writeln!(out, "  ~ app \"{}\"", op.name);
                for d in mutations {
                    render_field(out, d);
                }
            } else {
                let _ = writeln!(out, "  = app \"{}\"", op.name);
            }
            if !non_mutable_drift.is_empty() {
                let _ = writeln!(
                    out,
                    "      ! drift on fields apply won't auto-update (recreate manually if needed):"
                );
                for d in non_mutable_drift {
                    render_field_info(out, d);
                }
            }
        }
    }
}

fn render_addon(out: &mut String, op: &AddonOp) {
    match &op.kind {
        AddonOpKind::Create {
            provider,
            size,
            region,
        } => {
            let size_hint = match size {
                Some(s) => format!(", {s}"),
                None => String::new(),
            };
            let _ = writeln!(
                out,
                "  + addon \"{}\" ({provider}{size_hint}, region={region})",
                op.name
            );
        }
        AddonOpKind::Existing { drift } => {
            if drift.is_empty() {
                let _ = writeln!(out, "  = addon \"{}\"", op.name);
            } else {
                let _ = writeln!(out, "  = addon \"{}\"", op.name);
                let _ = writeln!(
                    out,
                    "      ! drift detected (apply never updates existing addons):"
                );
                for d in drift {
                    render_field_info(out, d);
                }
            }
        }
    }
}

fn render_ng(out: &mut String, op: &NgOp) {
    match &op.kind {
        NgOpKind::Create { members } => {
            let _ = writeln!(out, "  + network_group \"{}\"", op.name);
            if !members.is_empty() {
                let _ = writeln!(out, "      members:");
                for m in members {
                    let _ = writeln!(out, "        + {m}");
                }
            }
        }
        NgOpKind::Existing { mutations } if !mutations.is_empty() => {
            let _ = writeln!(out, "  ~ network_group \"{}\"", op.name);
            for d in mutations {
                render_field(out, d);
            }
        }
        NgOpKind::Existing { .. } => {
            let _ = writeln!(out, "  = network_group \"{}\"", op.name);
        }
    }
}

fn render_field(out: &mut String, diff: &FieldDiff) {
    match &diff.body {
        DiffBody::Scalar { file, live } => {
            let _ = writeln!(
                out,
                "      {}: \"{}\" → \"{}\"",
                diff.field,
                quote_escape(live),
                quote_escape(file)
            );
        }
        DiffBody::Set(entries) => {
            let _ = writeln!(out, "      {}:", diff.field);
            for e in entries {
                let _ = writeln!(out, "        {} {}", e.op, e.value);
            }
        }
        DiffBody::Map(entries) => {
            let _ = writeln!(out, "      {}:", diff.field);
            for e in entries {
                match e.op {
                    '+' => {
                        let _ = writeln!(
                            out,
                            "        + {} = \"{}\"",
                            e.key,
                            quote_escape(e.file.as_deref().unwrap_or(""))
                        );
                    }
                    '-' => {
                        let _ = writeln!(
                            out,
                            "        - {} = \"{}\"",
                            e.key,
                            quote_escape(e.live.as_deref().unwrap_or(""))
                        );
                    }
                    '~' => {
                        let _ = writeln!(
                            out,
                            "        ~ {}: \"{}\" → \"{}\"",
                            e.key,
                            quote_escape(e.live.as_deref().unwrap_or("")),
                            quote_escape(e.file.as_deref().unwrap_or(""))
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render_field_info(out: &mut String, diff: &FieldDiff) {
    if let DiffBody::Scalar { file, live } = &diff.body {
        let _ = writeln!(
            out,
            "        {}: live=\"{}\" file=\"{}\"",
            diff.field,
            quote_escape(live),
            quote_escape(file)
        );
    } else {
        render_field(out, diff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addon, App, NetworkGroup, Project, Source};

    fn make_app(name: &str, kind: &str) -> App {
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

    fn make_addon(name: &str, kind: &str, size: Option<&str>) -> Addon {
        Addon {
            name: name.to_string(),
            kind: kind.to_string(),
            size: size.map(str::to_string),
            crypted: false,
            region: None,
            version: None,
            backup_path: None,
        }
    }

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
        }
    }

    fn empty_live() -> LiveSnapshot {
        LiveSnapshot {
            apps: IndexMap::new(),
            addons: IndexMap::new(),
            network_groups: IndexMap::new(),
            default_region: "par".into(),
        }
    }

    #[test]
    fn empty_plan_is_empty() {
        let p = compute(&empty_project(), &empty_live(), &Targets::default());
        assert_eq!(p.mutation_count(), 0);
    }

    #[test]
    fn missing_app_is_create() {
        let mut project = empty_project();
        let mut api = make_app("prod-api", "node");
        api.source = Some(Source {
            from: "https://github.com/me/api.git".into(),
            branch: None,
        });
        api.env.insert("PORT".into(), "8080".into());
        api.domains.push("api.example.com".into());
        project.apps.insert("api".into(), api);

        let plan = compute(&project, &empty_live(), &Targets::default());
        assert_eq!(plan.apps.len(), 1);
        match &plan.apps[0].kind {
            AppOpKind::Create {
                kind,
                source,
                env,
                domains,
                ..
            } => {
                assert_eq!(kind, "node");
                assert_eq!(source.as_deref(), Some("https://github.com/me/api.git"));
                assert_eq!(env.get("PORT").map(String::as_str), Some("8080"));
                assert_eq!(domains, &["api.example.com".to_string()]);
            }
            _ => panic!("expected Create"),
        }
        assert_eq!(plan.mutation_count(), 1);
    }

    #[test]
    fn existing_synced_app_is_noop() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), make_app("prod-api", "node"));
        let mut live = empty_live();
        live.apps
            .insert("prod-api".into(), make_app("prod-api", "node"));
        let plan = compute(&project, &live, &Targets::default());
        match &plan.apps[0].kind {
            AppOpKind::Existing {
                mutations,
                non_mutable_drift,
            } => {
                assert!(mutations.is_empty());
                assert!(non_mutable_drift.is_empty());
            }
            _ => panic!("expected Existing"),
        }
        assert_eq!(plan.mutation_count(), 0);
    }

    #[test]
    fn env_drift_is_update() {
        let mut project = empty_project();
        let mut api = make_app("prod-api", "node");
        api.env.insert("PORT".into(), "3000".into());
        api.env.insert("NEW".into(), "yes".into());
        project.apps.insert("api".into(), api);
        let mut live = empty_live();
        let mut live_api = make_app("prod-api", "node");
        live_api.env.insert("PORT".into(), "8080".into());
        live_api.env.insert("OLD".into(), "true".into());
        live.apps.insert("prod-api".into(), live_api);

        let plan = compute(&project, &live, &Targets::default());
        match &plan.apps[0].kind {
            AppOpKind::Existing { mutations, .. } => {
                assert!(mutations.iter().any(|d| d.field == "env"));
            }
            _ => panic!(),
        }
        assert_eq!(plan.mutation_count(), 1);
    }

    #[test]
    fn cleverapps_domain_is_not_treated_as_drift() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), make_app("prod-api", "node"));
        let mut live = empty_live();
        let mut live_api = make_app("prod-api", "node");
        live_api.domains.push("prod-api.cleverapps.io".into());
        live.apps.insert("prod-api".into(), live_api);

        let plan = compute(&project, &live, &Targets::default());
        match &plan.apps[0].kind {
            AppOpKind::Existing { mutations, .. } => {
                assert!(
                    mutations.is_empty(),
                    "cleverapps.io domain leaked into diff"
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn kind_drift_is_non_mutable_warning() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), make_app("prod-api", "python"));
        let mut live = empty_live();
        live.apps
            .insert("prod-api".into(), make_app("prod-api", "node"));

        let plan = compute(&project, &live, &Targets::default());
        match &plan.apps[0].kind {
            AppOpKind::Existing {
                mutations,
                non_mutable_drift,
            } => {
                assert!(mutations.is_empty());
                assert!(non_mutable_drift.iter().any(|d| d.field == "kind"));
            }
            _ => panic!(),
        }
        // Non-mutable drift alone doesn't count as a mutation.
        assert_eq!(plan.mutation_count(), 0);
    }

    #[test]
    fn missing_addon_is_create() {
        let mut project = empty_project();
        project.addons.insert(
            "db".into(),
            make_addon("prod-db", "postgresql", Some("xs_sml")),
        );
        let plan = compute(&project, &empty_live(), &Targets::default());
        assert_eq!(plan.addons.len(), 1);
        match &plan.addons[0].kind {
            AddonOpKind::Create {
                provider,
                size,
                region,
            } => {
                assert_eq!(provider, "postgresql");
                assert_eq!(size.as_deref(), Some("xs_sml"));
                assert_eq!(region, "par");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn existing_addon_drift_is_informational() {
        let mut project = empty_project();
        project.addons.insert(
            "db".into(),
            make_addon("prod-db", "postgresql", Some("s_sml")),
        );
        let mut live = empty_live();
        live.addons.insert(
            "prod-db".into(),
            make_addon("prod-db", "postgresql", Some("xs_sml")),
        );
        let plan = compute(&project, &live, &Targets::default());
        match &plan.addons[0].kind {
            AddonOpKind::Existing { drift } => {
                assert!(drift.iter().any(|d| d.field == "size"));
            }
            _ => panic!(),
        }
        // Apply doesn't update addons, so this doesn't count as a mutation.
        assert_eq!(plan.mutation_count(), 0);
    }

    #[test]
    fn ng_member_drift_is_update() {
        let mut project = empty_project();
        project.network_groups.insert(
            "vpn".into(),
            NetworkGroup {
                name: "vpn".into(),
                description: None,
                link: vec!["api".into(), "db".into()],
            },
        );
        let mut live = empty_live();
        live.network_groups.insert(
            "vpn".into(),
            NetworkGroup {
                name: "vpn".into(),
                description: None,
                link: vec!["api".into()],
            },
        );
        let plan = compute(&project, &live, &Targets::default());
        match &plan.network_groups[0].kind {
            NgOpKind::Existing { mutations } => {
                assert!(mutations.iter().any(|d| d.field == "members"));
            }
            _ => panic!(),
        }
        assert_eq!(plan.mutation_count(), 1);
    }

    #[test]
    fn targeting_filters_to_one_app() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), make_app("prod-api", "node"));
        project
            .apps
            .insert("worker".into(), make_app("prod-worker", "node"));
        project
            .addons
            .insert("db".into(), make_addon("prod-db", "postgresql", None));
        let live = empty_live();
        let mut targets = Targets::default();
        targets.apps.insert("api".into());

        let plan = compute(&project, &live, &targets);
        assert_eq!(plan.apps.len(), 1);
        assert_eq!(plan.apps[0].name, "prod-api");
        assert!(plan.addons.is_empty());
    }

    #[test]
    fn targeting_addon_and_app_includes_both() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), make_app("prod-api", "node"));
        project
            .addons
            .insert("db".into(), make_addon("prod-db", "postgresql", None));
        project
            .addons
            .insert("cache".into(), make_addon("prod-cache", "redis", None));
        let live = empty_live();
        let mut targets = Targets::default();
        targets.apps.insert("api".into());
        targets.addons.insert("db".into());

        let plan = compute(&project, &live, &targets);
        assert_eq!(plan.addons.len(), 1);
        assert_eq!(plan.addons[0].name, "prod-db");
        assert_eq!(plan.apps.len(), 1);
    }

    #[test]
    fn render_with_targets_shows_label() {
        let mut project = empty_project();
        project
            .apps
            .insert("api".into(), make_app("prod-api", "node"));
        let mut targets = Targets::default();
        targets.apps.insert("api".into());
        let plan = compute(&project, &empty_live(), &targets);
        let s = render(&plan, &project, &targets);
        assert!(s.contains("(targeting: apps.api)"));
    }

    #[test]
    fn render_smoke_test() {
        let mut project = empty_project();
        let mut api = make_app("prod-api", "node");
        api.env.insert("PORT".into(), "3000".into());
        project.apps.insert("api".into(), api);
        project.addons.insert(
            "db".into(),
            make_addon("prod-db", "postgresql", Some("xs_sml")),
        );

        let mut live = empty_live();
        let mut live_db = make_addon("prod-db", "postgresql", Some("xs_sml"));
        live_db.size = Some("xs_sml".into());
        live.addons.insert("prod-db".into(), live_db);

        let plan = compute(&project, &live, &Targets::default());
        let s = render(&plan, &project, &Targets::default());
        // 1 app to create + 0 addon (synced) + 0 NG = 1 create, 0 update, 1 unchanged.
        assert!(s.contains("1 to create, 0 to update, 1 unchanged"));
        assert!(s.contains("+ app \"prod-api\""));
        assert!(s.contains("= addon \"prod-db\""));
    }
}
