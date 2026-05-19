//! Local state file (`<project>.state`). A JSON sidecar that records the
//! Clever Cloud resources managed by a given project file, so that
//! subsequent runs can skip the org-wide `clever ... list` calls when the
//! correlation `(name, org_id) → id` is already known.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    App,
    Addon,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateResource {
    pub kind: ResourceKind,
    pub id: String,
    pub org_id: String,
    pub region: String,
    pub env: String,
    pub name: String,
}

#[derive(Debug)]
pub struct State {
    path: PathBuf,
    resources: Vec<StateResource>,
}

impl State {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the state file next to the project file, or return an empty
    /// state if it doesn't exist yet.
    pub fn load(project_path: &Path) -> Result<Self> {
        let path = state_path_for(project_path);
        if !path.exists() {
            debug!("no state file at `{}` — starting empty", path.display());
            return Ok(Self {
                path,
                resources: Vec::new(),
            });
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading state file `{}`", path.display()))?;
        let resources: Vec<StateResource> = if raw.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parsing state file `{}`", path.display()))?
        };
        Ok(Self { path, resources })
    }

    pub fn save(&self) -> Result<()> {
        let body = serde_json::to_string_pretty(&self.resources).context("serializing state")?;
        std::fs::write(&self.path, body)
            .with_context(|| format!("writing state file `{}`", self.path.display()))?;
        Ok(())
    }

    pub fn find(&self, kind: ResourceKind, name: &str, org: &str) -> Option<&StateResource> {
        self.resources
            .iter()
            .find(|r| r.kind == kind && r.name == name && r.org_id == org)
    }

    /// Insert or update a resource. Existing entries are matched by `id`
    /// first, then by `(kind, name, org_id)`.
    pub fn upsert(&mut self, res: StateResource) {
        if let Some(existing) = self.resources.iter_mut().find(|r| r.id == res.id) {
            *existing = res;
            return;
        }
        if let Some(existing) = self
            .resources
            .iter_mut()
            .find(|r| r.kind == res.kind && r.name == res.name && r.org_id == res.org_id)
        {
            *existing = res;
            return;
        }
        self.resources.push(res);
    }

    pub fn remove_by_id(&mut self, id: &str) {
        self.resources.retain(|r| r.id != id);
    }
}

fn state_path_for(project_path: &Path) -> PathBuf {
    project_path.with_extension("state")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_project_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "clever-project-state-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn missing_file_loads_empty() {
        let p = tmp_project_path();
        let s = State::load(&p).unwrap();
        assert!(s.resources.is_empty());
        assert_eq!(s.path().extension().and_then(|e| e.to_str()), Some("state"));
    }

    #[test]
    fn roundtrip_upsert_and_load() {
        let p = tmp_project_path();
        let mut s = State::load(&p).unwrap();
        s.upsert(StateResource {
            kind: ResourceKind::App,
            id: "app_1".into(),
            org_id: "orga_x".into(),
            region: "par".into(),
            env: "prod".into(),
            name: "prod-x".into(),
        });
        s.save().unwrap();

        let s2 = State::load(&p).unwrap();
        let r = s2.find(ResourceKind::App, "prod-x", "orga_x").unwrap();
        assert_eq!(r.id, "app_1");
        std::fs::remove_file(s.path()).ok();
    }

    #[test]
    fn upsert_replaces_by_id() {
        let p = tmp_project_path();
        let mut s = State::load(&p).unwrap();
        s.upsert(StateResource {
            kind: ResourceKind::Addon,
            id: "addon_1".into(),
            org_id: "o".into(),
            region: "par".into(),
            env: "prod".into(),
            name: "n".into(),
        });
        s.upsert(StateResource {
            kind: ResourceKind::Addon,
            id: "addon_1".into(),
            org_id: "o".into(),
            region: "rbx".into(),
            env: "prod".into(),
            name: "n".into(),
        });
        assert_eq!(s.resources.len(), 1);
        assert_eq!(s.resources[0].region, "rbx");
        std::fs::remove_file(s.path()).ok();
    }

    #[test]
    fn remove_by_id_works() {
        let p = tmp_project_path();
        let mut s = State::load(&p).unwrap();
        s.upsert(StateResource {
            kind: ResourceKind::App,
            id: "a".into(),
            org_id: "o".into(),
            region: "par".into(),
            env: "prod".into(),
            name: "x".into(),
        });
        s.remove_by_id("a");
        assert!(s.resources.is_empty());
    }
}
