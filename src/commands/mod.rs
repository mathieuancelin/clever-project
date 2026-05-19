pub mod apply;
pub mod check;
pub mod delete;
pub mod read;

use std::collections::HashMap;

use anyhow::Result;
use tracing::info;

use crate::clever::{Clever, ListedAddon, ListedApp};

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
