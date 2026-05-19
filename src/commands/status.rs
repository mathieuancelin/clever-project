use std::collections::BTreeSet;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use tracing::info;

use crate::clever::Clever;
use crate::cli::StatusArgs;
use crate::commands::live::{LiveSnapshot, snapshot as live_snapshot};
use crate::commands::resolve_project_file;
use crate::model::{Addon, App, NetworkGroup, Project};
use crate::state::{ResourceKind, State};

pub fn run(args: StatusArgs) -> Result<()> {
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

    let file = resolve_project_file(args.file, &std::env::current_dir()?)?;
    let (project, _resolver) = Project::load_and_resolve(
        &file,
        args.org,
        args.region,
        &variables,
        args.secrets_path.as_deref(),
    )
    .with_context(|| format!("loading project `{}`", file.display()))?;

    let state = State::load(&file)?;

    let clever = Clever::new()?;
    let live = live_snapshot(&clever, &project.org)
        .with_context(|| format!("reading live snapshot of org `{}`", project.org))?;

    let report = compute_report(&project, &live, &state);

    info!(
        "comparing project `{}` against live org `{}`",
        project.name, project.org
    );
    print!("{}", render(&project, &report, args.brief));

    if args.exit_on_drift && report.has_drift() {
        bail!("drift detected (use `apply` to converge, or remove from project file)");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceTag {
    Synced,
    Drifted,
    OnlyInFile,
    OrphanInOrg,
}

#[derive(Debug)]
struct ResourceVerdict {
    name: String,
    tag: ResourceTag,
    diffs: Vec<FieldDiff>,
}

#[derive(Debug, Clone)]
struct FieldDiff {
    field: String,
    body: DiffBody,
}

#[derive(Debug, Clone)]
enum DiffBody {
    Scalar { file: String, live: String },
    Set(Vec<SetEntry>),
    Map(Vec<MapEntry>),
}

#[derive(Debug, Clone)]
struct SetEntry {
    op: char, // '+', '-'
    value: String,
}

#[derive(Debug, Clone)]
struct MapEntry {
    op: char, // '+', '-', '~'
    key: String,
    file: Option<String>,
    live: Option<String>,
}

#[derive(Debug, Default)]
struct Report {
    apps: Vec<ResourceVerdict>,
    addons: Vec<ResourceVerdict>,
    network_groups: Vec<ResourceVerdict>,
}

impl Report {
    fn has_drift(&self) -> bool {
        let any = |v: &[ResourceVerdict]| v.iter().any(|r| r.tag != ResourceTag::Synced);
        any(&self.apps) || any(&self.addons) || any(&self.network_groups)
    }
}

fn compute_report(project: &Project, live: &LiveSnapshot, state: &State) -> Report {
    let mut out = Report::default();

    // Apps.
    let file_apps: IndexMap<&str, &App> = project
        .apps
        .iter()
        .map(|(_, a)| (a.name.as_str(), a))
        .collect();
    let mut seen_app_names: BTreeSet<String> = BTreeSet::new();
    for (file_name, file_app) in &file_apps {
        seen_app_names.insert(file_name.to_string());
        match live.apps.get(*file_name) {
            Some(live_app) => {
                let diffs = diff_app(file_app, live_app, &project.region, &live.default_region);
                let tag = if diffs.is_empty() {
                    ResourceTag::Synced
                } else {
                    ResourceTag::Drifted
                };
                out.apps.push(ResourceVerdict {
                    name: file_name.to_string(),
                    tag,
                    diffs,
                });
            }
            None => out.apps.push(ResourceVerdict {
                name: file_name.to_string(),
                tag: ResourceTag::OnlyInFile,
                diffs: Vec::new(),
            }),
        }
    }
    // Orphans: live apps that aren't in the file but are tracked in state.
    for live_name in live.apps.keys() {
        if seen_app_names.contains(live_name) {
            continue;
        }
        if state
            .find(ResourceKind::App, live_name, &project.org)
            .is_some()
        {
            out.apps.push(ResourceVerdict {
                name: live_name.clone(),
                tag: ResourceTag::OrphanInOrg,
                diffs: Vec::new(),
            });
        }
    }

    // Addons.
    let file_addons: IndexMap<&str, &Addon> = project
        .addons
        .iter()
        .map(|(_, a)| (a.name.as_str(), a))
        .collect();
    let mut seen_addon_names: BTreeSet<String> = BTreeSet::new();
    for (file_name, file_addon) in &file_addons {
        seen_addon_names.insert(file_name.to_string());
        match live.addons.get(*file_name) {
            Some(live_addon) => {
                let diffs = diff_addon(
                    file_addon,
                    live_addon,
                    &project.region,
                    &live.default_region,
                );
                let tag = if diffs.is_empty() {
                    ResourceTag::Synced
                } else {
                    ResourceTag::Drifted
                };
                out.addons.push(ResourceVerdict {
                    name: file_name.to_string(),
                    tag,
                    diffs,
                });
            }
            None => out.addons.push(ResourceVerdict {
                name: file_name.to_string(),
                tag: ResourceTag::OnlyInFile,
                diffs: Vec::new(),
            }),
        }
    }
    for live_name in live.addons.keys() {
        if seen_addon_names.contains(live_name) {
            continue;
        }
        if state
            .find(ResourceKind::Addon, live_name, &project.org)
            .is_some()
        {
            out.addons.push(ResourceVerdict {
                name: live_name.clone(),
                tag: ResourceTag::OrphanInOrg,
                diffs: Vec::new(),
            });
        }
    }

    // Network groups.
    let file_ngs: IndexMap<&str, &NetworkGroup> = project
        .network_groups
        .iter()
        .map(|(_, n)| (n.name.as_str(), n))
        .collect();
    let mut seen_ng_names: BTreeSet<String> = BTreeSet::new();
    for (file_name, file_ng) in &file_ngs {
        seen_ng_names.insert(file_name.to_string());
        match live.network_groups.get(*file_name) {
            Some(live_ng) => {
                let diffs = diff_ng(file_ng, live_ng);
                let tag = if diffs.is_empty() {
                    ResourceTag::Synced
                } else {
                    ResourceTag::Drifted
                };
                out.network_groups.push(ResourceVerdict {
                    name: file_name.to_string(),
                    tag,
                    diffs,
                });
            }
            None => out.network_groups.push(ResourceVerdict {
                name: file_name.to_string(),
                tag: ResourceTag::OnlyInFile,
                diffs: Vec::new(),
            }),
        }
    }
    for live_name in live.network_groups.keys() {
        if seen_ng_names.contains(live_name) {
            continue;
        }
        if state
            .find(ResourceKind::NetworkGroup, live_name, &project.org)
            .is_some()
        {
            out.network_groups.push(ResourceVerdict {
                name: live_name.clone(),
                tag: ResourceTag::OrphanInOrg,
                diffs: Vec::new(),
            });
        }
    }

    out
}

fn diff_app(file: &App, live: &App, file_default: &str, live_default: &str) -> Vec<FieldDiff> {
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
    if let Some(d) = diff_set("domains", &file.domains, &live.domains) {
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

fn diff_addon(
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
    let file_size = file.size.clone().unwrap_or_default();
    let live_size = live.size.clone().unwrap_or_default();
    if !file_size.is_empty() && !sizes_equivalent(&file_size, &live_size) {
        diffs.push(FieldDiff {
            field: "size".into(),
            body: DiffBody::Scalar {
                file: file_size,
                live: live_size,
            },
        });
    }
    diffs
}

fn diff_ng(file: &NetworkGroup, live: &NetworkGroup) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    if let Some(d) = diff_set("members", &file.link, &live.link) {
        diffs.push(d);
    }
    diffs
}

fn diff_set(field: &str, file: &[String], live: &[String]) -> Option<FieldDiff> {
    let file_set: BTreeSet<&str> = file.iter().map(String::as_str).collect();
    let live_set: BTreeSet<&str> = live.iter().map(String::as_str).collect();
    if file_set == live_set {
        return None;
    }
    let mut entries: Vec<SetEntry> = Vec::new();
    for v in file_set.difference(&live_set) {
        entries.push(SetEntry {
            op: '+',
            value: (*v).to_string(),
        });
    }
    for v in live_set.difference(&file_set) {
        entries.push(SetEntry {
            op: '-',
            value: (*v).to_string(),
        });
    }
    Some(FieldDiff {
        field: field.into(),
        body: DiffBody::Set(entries),
    })
}

fn diff_map(
    field: &str,
    file: &IndexMap<String, String>,
    live: &IndexMap<String, String>,
) -> Option<FieldDiff> {
    let mut entries: Vec<MapEntry> = Vec::new();
    let mut keys: BTreeSet<&str> = file.keys().map(String::as_str).collect();
    keys.extend(live.keys().map(String::as_str));
    for k in keys {
        match (file.get(k), live.get(k)) {
            (Some(fv), Some(lv)) if fv == lv => {}
            (Some(fv), Some(lv)) => entries.push(MapEntry {
                op: '~',
                key: k.into(),
                file: Some(fv.clone()),
                live: Some(lv.clone()),
            }),
            (Some(fv), None) => entries.push(MapEntry {
                op: '+',
                key: k.into(),
                file: Some(fv.clone()),
                live: None,
            }),
            (None, Some(lv)) => entries.push(MapEntry {
                op: '-',
                key: k.into(),
                file: None,
                live: Some(lv.clone()),
            }),
            (None, None) => {}
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(FieldDiff {
        field: field.into(),
        body: DiffBody::Map(entries),
    })
}

/// Loose equivalence between the project file's addon `kind` and the live
/// provider id (stripped of the `-addon` suffix on the live side). Mirrors
/// the aliases recognized at apply time so `postgresql` doesn't read as
/// drifted vs a live `postgresql` (which was `postgresql-addon` upstream).
fn kinds_equivalent(file: &str, live: &str) -> bool {
    if file == live {
        return true;
    }
    let canon = |s: &str| -> String {
        match s.to_lowercase().as_str() {
            "postgres" | "pg" | "postgresql-addon" => "postgresql".into(),
            "mongo" | "mongodb-addon" => "mongodb".into(),
            "es" | "es-addon" => "elasticsearch".into(),
            "s3" | "cellar-addon" => "cellar".into(),
            "mysql-addon" => "mysql".into(),
            "redis-addon" => "redis".into(),
            "addon-matomo" => "matomo".into(),
            "addon-pulsar" => "pulsar".into(),
            other => other.to_string(),
        }
    };
    canon(file) == canon(live)
}

/// Treat `S_BIG` and `s_big` as the same plan; matches the case-normalization
/// done in `apply::validate_addons` so users don't see a spurious size drift.
fn sizes_equivalent(file: &str, live: &str) -> bool {
    file.to_lowercase() == live.to_lowercase()
}

fn render(project: &Project, report: &Report, brief: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Status of project `{}` in org `{}` (default region `{}`):",
        project.name, project.org, project.region
    );
    let _ = writeln!(out);

    let mut counts = Counts::default();
    let mut wrote_any = false;

    for v in &report.apps {
        if render_verdict(&mut out, "app", v, brief) {
            wrote_any = true;
        }
        counts.add(v.tag);
    }
    for v in &report.addons {
        if render_verdict(&mut out, "addon", v, brief) {
            wrote_any = true;
        }
        counts.add(v.tag);
    }
    for v in &report.network_groups {
        if render_verdict(&mut out, "network_group", v, brief) {
            wrote_any = true;
        }
        counts.add(v.tag);
    }

    if !wrote_any && brief {
        let _ = writeln!(out, "  (no drift)");
    }
    if !wrote_any && !brief {
        // empty project, or everything in sync but no resources listed
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Summary: {} drifted, {} to create, {} orphan, {} in sync.",
        counts.drifted, counts.only_in_file, counts.orphan, counts.synced
    );
    out
}

#[derive(Default)]
struct Counts {
    synced: usize,
    drifted: usize,
    only_in_file: usize,
    orphan: usize,
}

impl Counts {
    fn add(&mut self, tag: ResourceTag) {
        match tag {
            ResourceTag::Synced => self.synced += 1,
            ResourceTag::Drifted => self.drifted += 1,
            ResourceTag::OnlyInFile => self.only_in_file += 1,
            ResourceTag::OrphanInOrg => self.orphan += 1,
        }
    }
}

fn render_verdict(out: &mut String, kind: &str, v: &ResourceVerdict, brief: bool) -> bool {
    let marker = match v.tag {
        ResourceTag::Synced => "=",
        ResourceTag::Drifted => "~",
        ResourceTag::OnlyInFile => "+",
        ResourceTag::OrphanInOrg => "-",
    };
    let suffix = match v.tag {
        ResourceTag::Synced => "",
        ResourceTag::Drifted => " drifted",
        ResourceTag::OnlyInFile => " only in file (would be created)",
        ResourceTag::OrphanInOrg => " orphan (managed but missing from file)",
    };
    if brief && v.tag == ResourceTag::Synced {
        return false;
    }
    let _ = writeln!(out, "  {marker} {kind} \"{}\"{}", v.name, suffix);
    for d in &v.diffs {
        render_field_diff(out, d);
    }
    true
}

fn render_field_diff(out: &mut String, diff: &FieldDiff) {
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
                            "        - {} = \"{}\"  (only in org)",
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

fn quote_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Addon, App, Source};

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

    #[test]
    fn diff_app_identical_returns_empty() {
        let a = make_app("api", "node");
        let b = make_app("api", "node");
        assert!(diff_app(&a, &b, "par", "par").is_empty());
    }

    #[test]
    fn diff_app_kind_change_reported() {
        let mut a = make_app("api", "node");
        let b = make_app("api", "python");
        a.kind = "node".into();
        let diffs = diff_app(&a, &b, "par", "par");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "kind");
    }

    #[test]
    fn diff_app_env_set_add_remove_change() {
        let mut a = make_app("api", "node");
        let mut b = make_app("api", "node");
        a.env.insert("ADDED".into(), "new".into());
        a.env.insert("KEPT".into(), "same".into());
        a.env.insert("CHANGED".into(), "new-val".into());
        b.env.insert("REMOVED".into(), "old".into());
        b.env.insert("KEPT".into(), "same".into());
        b.env.insert("CHANGED".into(), "old-val".into());
        let diffs = diff_app(&a, &b, "par", "par");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "env");
        let DiffBody::Map(entries) = &diffs[0].body else {
            panic!("expected map body");
        };
        let ops: BTreeSet<char> = entries.iter().map(|e| e.op).collect();
        assert!(ops.contains(&'+'));
        assert!(ops.contains(&'-'));
        assert!(ops.contains(&'~'));
        // KEPT should not appear
        assert!(entries.iter().all(|e| e.key != "KEPT"));
    }

    #[test]
    fn diff_app_domains_set_diff() {
        let mut a = make_app("api", "node");
        let mut b = make_app("api", "node");
        a.domains = vec!["api.example.com".into(), "shared.example.com".into()];
        b.domains = vec!["legacy.example.com".into(), "shared.example.com".into()];
        let diffs = diff_app(&a, &b, "par", "par");
        assert_eq!(diffs.len(), 1);
        let DiffBody::Set(entries) = &diffs[0].body else {
            panic!("expected set body");
        };
        let added: Vec<&str> = entries
            .iter()
            .filter(|e| e.op == '+')
            .map(|e| e.value.as_str())
            .collect();
        let removed: Vec<&str> = entries
            .iter()
            .filter(|e| e.op == '-')
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(added, ["api.example.com"]);
        assert_eq!(removed, ["legacy.example.com"]);
    }

    #[test]
    fn diff_app_region_uses_default_when_unset() {
        // file says no region, live says no region: both fall back to defaults; should not drift.
        let a = make_app("api", "node");
        let b = make_app("api", "node");
        assert!(diff_app(&a, &b, "par", "par").is_empty());
        // file says explicit "par", live says implicit (default "par"): no drift
        let mut a2 = make_app("api", "node");
        a2.region = Some("par".into());
        assert!(diff_app(&a2, &b, "par", "par").is_empty());
        // file says "rbx", live default is "par": drift
        let mut a3 = make_app("api", "node");
        a3.region = Some("rbx".into());
        let diffs = diff_app(&a3, &b, "par", "par");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "region");
    }

    #[test]
    fn diff_app_source_drift() {
        let mut a = make_app("api", "node");
        let b = make_app("api", "node");
        a.source = Some(Source {
            from: "https://github.com/me/api.git".into(),
            branch: None,
        });
        let diffs = diff_app(&a, &b, "par", "par");
        assert!(diffs.iter().any(|d| d.field == "source.from"));
    }

    #[test]
    fn diff_addon_kind_alias_not_drift() {
        let a = make_addon("db", "postgresql", Some("xs_sml"));
        let b = make_addon("db", "postgresql-addon", Some("xs_sml"));
        assert!(diff_addon(&a, &b, "par", "par").is_empty());
    }

    #[test]
    fn diff_addon_size_case_not_drift() {
        let a = make_addon("db", "postgresql", Some("S_BIG"));
        let b = make_addon("db", "postgresql", Some("s_big"));
        assert!(diff_addon(&a, &b, "par", "par").is_empty());
    }

    #[test]
    fn diff_addon_size_change_reported() {
        let a = make_addon("db", "postgresql", Some("s_sml"));
        let b = make_addon("db", "postgresql", Some("xs_sml"));
        let diffs = diff_addon(&a, &b, "par", "par");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "size");
    }

    #[test]
    fn diff_addon_unset_size_not_compared() {
        // file omits size (will use whatever the org has). Should not drift.
        let a = make_addon("db", "postgresql", None);
        let b = make_addon("db", "postgresql", Some("xs_sml"));
        assert!(diff_addon(&a, &b, "par", "par").is_empty());
    }

    #[test]
    fn diff_ng_member_set() {
        let a = NetworkGroup {
            name: "vpn".into(),
            description: None,
            link: vec!["api".into(), "db".into()],
        };
        let b = NetworkGroup {
            name: "vpn".into(),
            description: None,
            link: vec!["api".into(), "old".into()],
        };
        let diffs = diff_ng(&a, &b);
        assert_eq!(diffs.len(), 1);
        let DiffBody::Set(entries) = &diffs[0].body else {
            panic!("expected set body");
        };
        let added: Vec<&str> = entries
            .iter()
            .filter(|e| e.op == '+')
            .map(|e| e.value.as_str())
            .collect();
        let removed: Vec<&str> = entries
            .iter()
            .filter(|e| e.op == '-')
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(added, ["db"]);
        assert_eq!(removed, ["old"]);
    }

    #[test]
    fn report_counts_categories() {
        // Compose a project + live with one of each tag.
        let mut project = Project {
            name: "P".into(),
            description: None,
            org: "o".into(),
            region: "par".into(),
            variables: IndexMap::new(),
            apps: IndexMap::new(),
            addons: IndexMap::new(),
            network_groups: IndexMap::new(),
        };
        // Synced
        project
            .apps
            .insert("synced".into(), make_app("synced", "node"));
        // Drifted (kind differs)
        let mut drifted_in_file = make_app("drifted", "python");
        drifted_in_file.kind = "python".into();
        project.apps.insert("drifted".into(), drifted_in_file);
        // Only in file
        project
            .apps
            .insert("planned".into(), make_app("planned", "node"));

        let mut live_apps: IndexMap<String, App> = IndexMap::new();
        live_apps.insert("synced".into(), make_app("synced", "node"));
        live_apps.insert("drifted".into(), make_app("drifted", "node"));
        // Orphan: live app not in file, but in state below.
        live_apps.insert("orphan".into(), make_app("orphan", "node"));
        let live = LiveSnapshot {
            apps: live_apps,
            addons: IndexMap::new(),
            network_groups: IndexMap::new(),
            default_region: "par".into(),
        };

        // Hand-built state that tracks the orphan app.
        let state_path = std::env::temp_dir().join(format!(
            "clever-project-status-test-{}-{}.state",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &state_path,
            r#"[{"kind":"app","id":"x","org_id":"o","region":"par","env":"prod","name":"orphan"}]"#,
        )
        .unwrap();
        // State::load looks for `<project>.state` next to a project file, so we
        // pass a sibling .yaml path. The .yaml file doesn't need to exist.
        let project_path = state_path.with_extension("yaml");
        let state = State::load(&project_path).unwrap();

        let report = compute_report(&project, &live, &state);

        let mut tags: Vec<(String, ResourceTag)> = report
            .apps
            .iter()
            .map(|v| (v.name.clone(), v.tag))
            .collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            tags,
            vec![
                ("drifted".to_string(), ResourceTag::Drifted),
                ("orphan".to_string(), ResourceTag::OrphanInOrg),
                ("planned".to_string(), ResourceTag::OnlyInFile),
                ("synced".to_string(), ResourceTag::Synced),
            ]
        );
        assert!(report.has_drift());

        std::fs::remove_file(&state_path).ok();
    }
}
