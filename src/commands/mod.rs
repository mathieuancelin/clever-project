pub mod apply;
pub mod check;
pub mod delete;
pub mod diff;
pub mod init;
pub mod live;
pub mod plan;
pub mod prompt;
pub mod read;
pub mod status;
pub mod targets;
pub mod unlock;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use tracing::info;

use crate::clever::{Clever, ListedAddon, ListedApp};

/// Resolve which project file to use for an `apply`/`delete`/`check` run.
/// If the user passed one explicitly, that wins. Otherwise we look for the
/// usual conventional file names in `search_dir`, in priority order. If
/// none match, error out with a helpful message.
pub fn resolve_project_file(explicit: Option<PathBuf>, search_dir: &Path) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    const CANDIDATES: &[&str] = &[
        "project.clever.yaml",
        "project.clever.yml",
        "project.clever.toml",
        "project.clever.json",
    ];
    for name in CANDIDATES {
        let p = search_dir.join(name);
        if p.exists() {
            info!("using auto-discovered project file `{}`", p.display());
            return Ok(p);
        }
    }
    bail!(
        "no project file given and none of {} found in `{}`. Pass a path explicitly.",
        CANDIDATES.join(", "),
        search_dir.display()
    )
}

/// Lazy, invalidatable cache of org-wide `clever ... list` results.
///
/// Used by `apply` and `delete` so they can prefer state lookups but still
/// fall back to a fresh listing when state is empty or turns out to be
/// stale (`invalidate()` drops the cache so the next access re-fetches).
pub struct OrgCache {
    apps: Option<HashMap<String, ListedApp>>,
    addons: Option<HashMap<String, ListedAddon>>,
}

impl OrgCache {
    pub fn new() -> Self {
        Self {
            apps: None,
            addons: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.apps = None;
        self.addons = None;
    }

    pub fn apps(&mut self, clever: &Clever, org: &str) -> Result<&HashMap<String, ListedApp>> {
        if self.apps.is_none() {
            info!("listing applications in org `{org}`");
            self.apps = Some(
                clever
                    .list_apps(org)?
                    .into_iter()
                    .map(|a| (a.name.clone(), a))
                    .collect(),
            );
        }
        Ok(self.apps.as_ref().unwrap())
    }

    pub fn addons(&mut self, clever: &Clever, org: &str) -> Result<&HashMap<String, ListedAddon>> {
        if self.addons.is_none() {
            info!("listing addons in org `{org}`");
            self.addons = Some(
                clever
                    .list_addons(org)?
                    .into_iter()
                    .map(|a| (a.name.clone(), a))
                    .collect(),
            );
        }
        Ok(self.addons.as_ref().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_tmp_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "clever-project-resolve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn explicit_path_always_wins() {
        let dir = fresh_tmp_dir();
        let explicit = PathBuf::from("/tmp/anything.yaml");
        let p = resolve_project_file(Some(explicit.clone()), &dir).unwrap();
        assert_eq!(p, explicit);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_yaml_first() {
        let dir = fresh_tmp_dir();
        std::fs::write(dir.join("project.clever.yaml"), "name: x").unwrap();
        std::fs::write(dir.join("project.clever.json"), "{}").unwrap();
        let p = resolve_project_file(None, &dir).unwrap();
        assert_eq!(p.file_name().unwrap(), "project.clever.yaml");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_json() {
        let dir = fresh_tmp_dir();
        std::fs::write(dir.join("project.clever.json"), "{}").unwrap();
        let p = resolve_project_file(None, &dir).unwrap();
        assert_eq!(p.file_name().unwrap(), "project.clever.json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_yml() {
        let dir = fresh_tmp_dir();
        std::fs::write(dir.join("project.clever.yml"), "name: x").unwrap();
        let p = resolve_project_file(None, &dir).unwrap();
        assert_eq!(p.file_name().unwrap(), "project.clever.yml");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_toml() {
        let dir = fresh_tmp_dir();
        std::fs::write(dir.join("project.clever.toml"), "name = \"x\"").unwrap();
        let p = resolve_project_file(None, &dir).unwrap();
        assert_eq!(p.file_name().unwrap(), "project.clever.toml");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toml_preferred_over_json_when_both_present() {
        let dir = fresh_tmp_dir();
        std::fs::write(dir.join("project.clever.toml"), "name = \"x\"").unwrap();
        std::fs::write(dir.join("project.clever.json"), "{}").unwrap();
        let p = resolve_project_file(None, &dir).unwrap();
        assert_eq!(p.file_name().unwrap(), "project.clever.toml");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn errors_when_no_candidate_found() {
        let dir = fresh_tmp_dir();
        let err = resolve_project_file(None, &dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("project.clever.yaml"));
        assert!(msg.contains("Pass a path explicitly"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
