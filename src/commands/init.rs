use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde::Serialize;
use tracing::info;

use crate::cli::InitArgs;
use crate::model::{
    ALLOWED_APP_KINDS, ALLOWED_REGIONS, Addon, App, Project, Source, normalize_app_kind,
};

pub fn run(args: InitArgs) -> Result<()> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from("project.clever.yaml"));
    if output.exists() && !args.force {
        bail!(
            "`{}` already exists; pass --force to overwrite",
            output.display()
        );
    }
    // JSON mode is for scripted use — interactive prompts would block on
    // stdin and pollute stdout.
    let mut effective = args;
    if effective.format.is_json() {
        effective.non_interactive = true;
    }

    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    let inputs = collect_inputs(&effective, &mut stdin, &mut stdout)?;
    let project = build_project(&inputs);

    project.save(&output)?;
    if effective.format.is_json() {
        #[derive(Serialize)]
        struct InitReport {
            wrote: String,
            project: String,
            org: String,
            apps: Vec<String>,
            addons: Vec<String>,
        }
        let payload = InitReport {
            wrote: output.display().to_string(),
            project: inputs.name.clone(),
            org: inputs.org.clone(),
            apps: project.apps.values().map(|a| a.name.clone()).collect(),
            addons: project.addons.values().map(|a| a.name.clone()).collect(),
        };
        let out = serde_json::to_string_pretty(&payload)
            .map_err(|e| anyhow::anyhow!("serializing JSON report: {e}"))?;
        println!("{out}");
    } else {
        info!("wrote `{}`", output.display());
        eprintln!(
            "\nNext steps:\n  1. Review and edit `{}` (sizes for addons, env vars, scaling, ...).\n  2. Run `clever-project check --offline` to confirm it's valid.\n  3. Run `clever-project apply --env prod` to create the resources.",
            output.display()
        );
    }
    Ok(())
}

#[derive(Debug)]
struct Inputs {
    name: String,
    org: String,
    region: String,
    kind: String,
    source: Option<String>,
    addons: Vec<String>,
}

fn collect_inputs<R: BufRead, W: Write>(
    args: &InitArgs,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<Inputs> {
    let interactive = !args.non_interactive;

    let name = resolve(
        args.name.clone(),
        "Project name",
        None,
        interactive,
        stdin,
        stdout,
        |v| (!v.trim().is_empty()).then(|| v.trim().to_string()),
        "name is required",
    )?;
    let org = resolve(
        args.org.clone(),
        "Org id (orga_...)",
        None,
        interactive,
        stdin,
        stdout,
        |v| (!v.trim().is_empty()).then(|| v.trim().to_string()),
        "org is required",
    )?;
    let region = resolve(
        args.region.clone(),
        "Region",
        Some("par"),
        interactive,
        stdin,
        stdout,
        |v| {
            let v = v.trim();
            ALLOWED_REGIONS.contains(&v).then(|| v.to_string())
        },
        &format!("region must be one of: {}", ALLOWED_REGIONS.join(", ")),
    )?;
    let kind = resolve(
        args.kind.clone(),
        "App kind",
        Some("node"),
        interactive,
        stdin,
        stdout,
        |v| {
            let normalized = normalize_app_kind(v.trim());
            ALLOWED_APP_KINDS
                .contains(&normalized.as_str())
                .then_some(normalized)
        },
        &format!(
            "kind must be one of: {} (or `java` for `jar`)",
            ALLOWED_APP_KINDS.join(", ")
        ),
    )?;

    let source = resolve_source(args, interactive, stdin, stdout)?;
    let addons = resolve_addons(args, interactive, stdin, stdout)?;

    Ok(Inputs {
        name,
        org,
        region,
        kind,
        source,
        addons,
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve<R: BufRead, W: Write, F>(
    given: Option<String>,
    prompt: &str,
    default: Option<&str>,
    interactive: bool,
    stdin: &mut R,
    stdout: &mut W,
    validate: F,
    err_msg: &str,
) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(v) = given {
        if let Some(ok) = validate(&v) {
            return Ok(ok);
        }
        bail!("invalid value for `{prompt}`: {err_msg}");
    }
    if !interactive {
        if let Some(d) = default {
            if let Some(ok) = validate(d) {
                return Ok(ok);
            }
        }
        bail!("missing value for `{prompt}` (non-interactive): {err_msg}");
    }
    loop {
        let raw = ask_line(prompt, default, stdin, stdout)?;
        match validate(&raw) {
            Some(ok) => return Ok(ok),
            None => writeln!(stdout, "  ! {err_msg}")?,
        }
    }
}

fn resolve_source<R: BufRead, W: Write>(
    args: &InitArgs,
    interactive: bool,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<Option<String>> {
    if args.no_source {
        return Ok(None);
    }
    if let Some(s) = args.source.clone() {
        return Ok(Some(normalize_github_url(&s)));
    }
    if !interactive {
        return Ok(None);
    }
    if !ask_yes_no("GitHub source", false, stdin, stdout)? {
        return Ok(None);
    }
    let raw = ask_line("  owner/repo or full URL", None, stdin, stdout)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(normalize_github_url(trimmed)))
}

fn resolve_addons<R: BufRead, W: Write>(
    args: &InitArgs,
    interactive: bool,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<Vec<String>> {
    if !args.addons.is_empty() {
        return Ok(args.addons.iter().map(|s| s.trim().to_string()).collect());
    }
    if !interactive {
        return Ok(Vec::new());
    }
    let raw = ask_line(
        "Addons (comma-separated, empty for none)",
        None,
        stdin,
        stdout,
    )?;
    Ok(raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

fn ask_line<R: BufRead, W: Write>(
    prompt: &str,
    default: Option<&str>,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<String> {
    match default {
        Some(d) => write!(stdout, "{prompt} [{d}]: ")?,
        None => write!(stdout, "{prompt}: ")?,
    }
    stdout.flush()?;
    let mut line = String::new();
    let n = stdin.read_line(&mut line)?;
    if n == 0 {
        // EOF
        if let Some(d) = default {
            return Ok(d.to_string());
        }
        bail!("input closed unexpectedly while prompting for `{prompt}`");
    }
    let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
    if trimmed.is_empty()
        && let Some(d) = default
    {
        return Ok(d.to_string());
    }
    Ok(trimmed)
}

fn ask_yes_no<R: BufRead, W: Write>(
    prompt: &str,
    default: bool,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        write!(stdout, "{prompt}? [{hint}]: ")?;
        stdout.flush()?;
        let mut line = String::new();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            return Ok(default);
        }
        let s = line.trim().to_lowercase();
        if s.is_empty() {
            return Ok(default);
        }
        match s.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => writeln!(stdout, "  ! please answer y or n")?,
        }
    }
}

/// Accept `owner/repo`, `github.com/owner/repo`, `https://github.com/owner/repo`
/// (with or without `.git`) and normalize to `https://github.com/owner/repo.git`.
/// Anything that doesn't look like a GitHub shorthand (e.g. `git@…` or a
/// non-GitHub URL) is passed through unchanged.
pub(crate) fn normalize_github_url(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return s.to_string();
    }
    if s.starts_with("git@") {
        return s.to_string();
    }
    if let Some(rest) = s.strip_prefix("https://github.com/") {
        let rest = rest.trim_end_matches('/');
        let base = rest.strip_suffix(".git").unwrap_or(rest);
        return format!("https://github.com/{base}.git");
    }
    if let Some(rest) = s.strip_prefix("http://github.com/") {
        let rest = rest.trim_end_matches('/');
        let base = rest.strip_suffix(".git").unwrap_or(rest);
        return format!("https://github.com/{base}.git");
    }
    if let Some(rest) = s.strip_prefix("github.com/") {
        let rest = rest.trim_end_matches('/');
        let base = rest.strip_suffix(".git").unwrap_or(rest);
        return format!("https://github.com/{base}.git");
    }
    // owner/repo shorthand: must contain exactly one slash and no scheme.
    if !s.contains("://") && s.matches('/').count() == 1 && !s.contains(' ') {
        let base = s.strip_suffix(".git").unwrap_or(s);
        return format!("https://github.com/{base}.git");
    }
    s.to_string()
}

/// Slugify a free-form string into a safe project key: lowercase, replace
/// runs of non-[a-z0-9] with a single `-`, trim hyphens from both ends.
pub(crate) fn sanitize_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true;
    for ch in raw.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed
    }
}

fn build_project(inputs: &Inputs) -> Project {
    let app_key = sanitize_key(&inputs.name);

    let source = inputs.source.as_ref().map(|from| Source {
        from: from.clone(),
        branch: None,
    });

    let mut apps: IndexMap<String, App> = IndexMap::new();
    let dependencies: Vec<String> = inputs.addons.iter().map(|a| sanitize_key(a)).collect();
    apps.insert(
        app_key.clone(),
        App {
            name: format!("${{env}}-{app_key}"),
            kind: inputs.kind.clone(),
            region: None,
            source,
            domains: vec![],
            scalability: None,
            dependencies,
            config: IndexMap::new(),
            env: IndexMap::new(),
        },
    );

    let mut addons: IndexMap<String, Addon> = IndexMap::new();
    for kind in &inputs.addons {
        let key = sanitize_key(kind);
        addons.insert(
            key.clone(),
            Addon {
                name: format!("${{env}}-{app_key}-{key}"),
                kind: kind.clone(),
                size: None,
                crypted: false,
                region: None,
                version: None,
                backup_path: None,
            },
        );
    }

    Project {
        name: inputs.name.clone(),
        description: None,
        org: inputs.org.clone(),
        region: inputs.region.clone(),
        variables: IndexMap::new(),
        apps,
        addons,
        network_groups: IndexMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_inputs() -> Inputs {
        Inputs {
            name: "My Stack".into(),
            org: "orga_1234".into(),
            region: "par".into(),
            kind: "node".into(),
            source: Some("https://github.com/me/api.git".into()),
            addons: vec!["postgresql".into(), "redis".into()],
        }
    }

    #[test]
    fn build_project_minimal() {
        let inputs = Inputs {
            name: "hello".into(),
            org: "o".into(),
            region: "par".into(),
            kind: "node".into(),
            source: None,
            addons: vec![],
        };
        let p = build_project(&inputs);
        assert_eq!(p.name, "hello");
        assert_eq!(p.org, "o");
        assert_eq!(p.region, "par");
        assert_eq!(p.apps.len(), 1);
        let app = p.apps.get("hello").unwrap();
        assert_eq!(app.name, "${env}-hello");
        assert_eq!(app.kind, "node");
        assert!(app.source.is_none());
        assert!(p.addons.is_empty());
    }

    #[test]
    fn build_project_wires_addons_as_dependencies() {
        let p = build_project(&make_inputs());
        let app = p.apps.get("my-stack").unwrap();
        assert_eq!(app.dependencies, vec!["postgresql", "redis"]);
        assert!(p.addons.contains_key("postgresql"));
        assert!(p.addons.contains_key("redis"));
        let pg = p.addons.get("postgresql").unwrap();
        assert_eq!(pg.name, "${env}-my-stack-postgresql");
        assert_eq!(pg.kind, "postgresql");
        assert!(pg.size.is_none());
    }

    #[test]
    fn build_project_with_github_source() {
        let p = build_project(&make_inputs());
        let app = p.apps.get("my-stack").unwrap();
        let src = app.source.as_ref().unwrap();
        assert_eq!(src.from, "https://github.com/me/api.git");
        assert!(src.branch.is_none());
    }

    #[test]
    fn sanitize_key_handles_garbage() {
        assert_eq!(sanitize_key("Hello World"), "hello-world");
        assert_eq!(sanitize_key("foo--bar  baz"), "foo-bar-baz");
        assert_eq!(sanitize_key("___leading_trailing___"), "leading-trailing");
        assert_eq!(sanitize_key(""), "app");
        assert_eq!(sanitize_key("@@@"), "app");
        assert_eq!(sanitize_key("API"), "api");
        assert_eq!(sanitize_key("server.v2"), "server-v2");
    }

    #[test]
    fn normalize_github_url_owner_repo() {
        assert_eq!(
            normalize_github_url("me/repo"),
            "https://github.com/me/repo.git"
        );
        assert_eq!(
            normalize_github_url("me/repo.git"),
            "https://github.com/me/repo.git"
        );
    }

    #[test]
    fn normalize_github_url_full_url() {
        assert_eq!(
            normalize_github_url("https://github.com/me/repo"),
            "https://github.com/me/repo.git"
        );
        assert_eq!(
            normalize_github_url("https://github.com/me/repo.git"),
            "https://github.com/me/repo.git"
        );
        assert_eq!(
            normalize_github_url("github.com/me/repo"),
            "https://github.com/me/repo.git"
        );
    }

    #[test]
    fn normalize_github_url_passes_ssh_through() {
        assert_eq!(
            normalize_github_url("git@github.com:me/repo.git"),
            "git@github.com:me/repo.git"
        );
    }

    #[test]
    fn normalize_github_url_passes_non_github_through() {
        assert_eq!(
            normalize_github_url("https://gitlab.com/me/repo.git"),
            "https://gitlab.com/me/repo.git"
        );
    }

    #[test]
    fn output_file_passes_check_static_validation() {
        // End-to-end-ish: build a project, save it to a temp file, then
        // re-load it via load_and_resolve (which runs the static
        // validators). Should succeed.
        let inputs = make_inputs();
        let project = build_project(&inputs);

        let path = std::env::temp_dir().join(format!(
            "clever-project-init-test-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        project.save(&path).unwrap();
        let (loaded, _) = Project::load_and_resolve(&path, None, None, &[], None, &[]).unwrap();
        assert_eq!(loaded.name, inputs.name);
        assert_eq!(loaded.org, inputs.org);
        // ${env} unresolved becomes `prod-my-stack` after load_and_resolve
        // runs interpolation with default env.
        assert_eq!(loaded.apps.get("my-stack").unwrap().name, "prod-my-stack");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn ask_line_uses_default_on_empty_input() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        let answer = ask_line("Region", Some("par"), &mut input, &mut output).unwrap();
        assert_eq!(answer, "par");
        let prompt_str = String::from_utf8(output).unwrap();
        assert!(prompt_str.contains("Region [par]:"));
    }

    #[test]
    fn ask_line_returns_user_value() {
        let mut input = Cursor::new(b"myproject\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        let answer = ask_line("Name", None, &mut input, &mut output).unwrap();
        assert_eq!(answer, "myproject");
    }

    #[test]
    fn ask_yes_no_y() {
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(ask_yes_no("GitHub source", false, &mut input, &mut output).unwrap());
    }

    #[test]
    fn ask_yes_no_empty_uses_default() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(ask_yes_no("Pick", true, &mut input, &mut output).unwrap());
        let mut input2 = Cursor::new(b"\n".to_vec());
        let mut output2: Vec<u8> = Vec::new();
        assert!(!ask_yes_no("Pick", false, &mut input2, &mut output2).unwrap());
    }

    #[test]
    fn ask_yes_no_retries_on_garbage() {
        let mut input = Cursor::new(b"maybe\nyes\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(ask_yes_no("Pick", false, &mut input, &mut output).unwrap());
        let s = String::from_utf8(output).unwrap();
        assert!(s.contains("please answer y or n"));
    }

    #[test]
    fn collect_inputs_non_interactive_requires_args() {
        let args = InitArgs {
            name: None,
            org: None,
            region: None,
            kind: None,
            source: None,
            no_source: true,
            addons: vec![],
            output: None,
            non_interactive: true,
            force: false,
            format: Default::default(),
        };
        let mut stdin = Cursor::new(Vec::new());
        let mut stdout: Vec<u8> = Vec::new();
        let err = collect_inputs(&args, &mut stdin, &mut stdout).unwrap_err();
        assert!(format!("{err:#}").contains("Project name"));
    }

    #[test]
    fn collect_inputs_non_interactive_happy_path() {
        let args = InitArgs {
            name: Some("hello".into()),
            org: Some("orga_1".into()),
            region: Some("par".into()),
            kind: Some("node".into()),
            source: None,
            no_source: true,
            addons: vec![],
            output: None,
            non_interactive: true,
            force: false,
            format: Default::default(),
        };
        let mut stdin = Cursor::new(Vec::new());
        let mut stdout: Vec<u8> = Vec::new();
        let inputs = collect_inputs(&args, &mut stdin, &mut stdout).unwrap();
        assert_eq!(inputs.name, "hello");
        assert_eq!(inputs.kind, "node");
        assert!(inputs.source.is_none());
    }

    #[test]
    fn collect_inputs_interactive_prompts_in_order() {
        // Empty input lines fall back to the prompt's default where one
        // exists; required fields without a default get explicit values.
        let scripted = b"My Stack\norga_999\n\n\nn\n\n";
        let mut stdin = Cursor::new(scripted.to_vec());
        let mut stdout: Vec<u8> = Vec::new();
        let args = InitArgs {
            name: None,
            org: None,
            region: None,
            kind: None,
            source: None,
            no_source: false,
            addons: vec![],
            output: None,
            non_interactive: false,
            force: false,
            format: Default::default(),
        };
        let inputs = collect_inputs(&args, &mut stdin, &mut stdout).unwrap();
        assert_eq!(inputs.name, "My Stack");
        assert_eq!(inputs.org, "orga_999");
        assert_eq!(inputs.region, "par");
        assert_eq!(inputs.kind, "node");
        assert!(inputs.source.is_none());
        assert!(inputs.addons.is_empty());
    }

    #[test]
    fn collect_inputs_reprompts_on_invalid_region() {
        let scripted = b"My Stack\norga_1\nzzz\npar\nnode\nn\n\n";
        let mut stdin = Cursor::new(scripted.to_vec());
        let mut stdout: Vec<u8> = Vec::new();
        let args = InitArgs {
            name: None,
            org: None,
            region: None,
            kind: None,
            source: None,
            no_source: false,
            addons: vec![],
            output: None,
            non_interactive: false,
            force: false,
            format: Default::default(),
        };
        let inputs = collect_inputs(&args, &mut stdin, &mut stdout).unwrap();
        assert_eq!(inputs.region, "par");
        let printed = String::from_utf8(stdout).unwrap();
        assert!(printed.contains("region must be one of"));
    }

    #[test]
    fn collect_inputs_interactive_normalizes_github_shorthand() {
        let scripted = b"My App\norga_1\n\n\ny\nme/repo\n\n";
        let mut stdin = Cursor::new(scripted.to_vec());
        let mut stdout: Vec<u8> = Vec::new();
        let args = InitArgs {
            name: None,
            org: None,
            region: None,
            kind: None,
            source: None,
            no_source: false,
            addons: vec![],
            output: None,
            non_interactive: false,
            force: false,
            format: Default::default(),
        };
        let inputs = collect_inputs(&args, &mut stdin, &mut stdout).unwrap();
        assert_eq!(
            inputs.source.as_deref(),
            Some("https://github.com/me/repo.git")
        );
    }
}
