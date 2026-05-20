//! Mutex over the local state file. A `<state>.lock` sentinel created
//! atomically (`O_CREAT|O_EXCL` semantics via `create_new`) prevents two
//! concurrent `apply` / `delete` runs from racing on the same project.
//!
//! The guard releases the file on `Drop` (normal returns, errors, regular
//! panics). A hard kill (SIGKILL, power loss, plain SIGINT) leaves the file
//! behind — the `unlock` command is the manual override.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub id: String,
    pub operation: String,
    pub pid: u32,
    #[serde(default)]
    pub user: Option<String>,
    pub project_path: String,
    pub acquired_at_unix: u64,
}

#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    released: bool,
}

impl LockGuard {
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Release the lock file. Called automatically on `Drop`; this method is
    /// only here so callers can release early and surface IO errors instead
    /// of swallowing them (Drop must be infallible).
    #[allow(dead_code)]
    pub fn release(mut self) -> Result<()> {
        self.released = true;
        std::fs::remove_file(&self.path)
            .with_context(|| format!("removing lock file `{}`", self.path.display()))
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Err(e) = std::fs::remove_file(&self.path) {
            warn!(
                "failed to remove lock file `{}`: {e} — run `clever-project unlock` to clean up",
                self.path.display()
            );
        }
    }
}

pub fn lock_path_for(state_path: &Path) -> PathBuf {
    let mut s = state_path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

pub fn acquire(state_path: &Path, operation: &str, project_path: &Path) -> Result<LockGuard> {
    let path = lock_path_for(state_path);

    let id = generate_id();
    let info = LockInfo {
        id,
        operation: operation.to_string(),
        pid: std::process::id(),
        user: current_user(),
        project_path: project_path.display().to_string(),
        acquired_at_unix: now_unix(),
    };

    let body = serde_json::to_string_pretty(&info).context("serializing lock info")?;

    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            f.write_all(body.as_bytes())
                .with_context(|| format!("writing lock file `{}`", path.display()))?;
            debug!("acquired lock `{}` (id={})", path.display(), info.id);
            Ok(LockGuard {
                path,
                released: false,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = peek(&path).unwrap_or(None);
            bail!(
                "{}",
                lock_held_message(&path, existing.as_ref(), project_path)
            );
        }
        Err(e) => Err(e).with_context(|| format!("creating lock file `{}`", path.display()))?,
    }
}

pub fn peek(lock_path: &Path) -> Result<Option<LockInfo>> {
    if !lock_path.exists() {
        return Ok(None);
    }
    let mut f = File::open(lock_path)
        .with_context(|| format!("opening lock file `{}`", lock_path.display()))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)
        .with_context(|| format!("reading lock file `{}`", lock_path.display()))?;
    if buf.trim().is_empty() {
        return Ok(None);
    }
    let info: LockInfo = serde_json::from_str(&buf)
        .with_context(|| format!("parsing lock file `{}`", lock_path.display()))?;
    Ok(Some(info))
}

pub fn force_remove(lock_path: &Path) -> Result<bool> {
    if !lock_path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(lock_path)
        .with_context(|| format!("removing lock file `{}`", lock_path.display()))?;
    Ok(true)
}

fn lock_held_message(path: &Path, existing: Option<&LockInfo>, project_path: &Path) -> String {
    let mut s = format!(
        "another `clever-project` run is holding the lock on this project (file: `{}`)",
        path.display()
    );
    if let Some(info) = existing {
        let age = age_pretty(info.acquired_at_unix);
        let user = info.user.as_deref().unwrap_or("?");
        s.push_str(&format!(
            "\n  operation:    {}\n  pid:          {}\n  user:         {}\n  project:      {}\n  acquired:     {age}\n  lock id:      {}",
            info.operation, info.pid, user, info.project_path, info.id
        ));
        if info.project_path != project_path.display().to_string() {
            s.push_str(&format!(
                "\n\nnote: the lock was acquired against a different project path (`{}`). If that run finished without cleaning up (Ctrl+C, crash), run `clever-project unlock` against this project to release it.",
                info.project_path
            ));
        } else {
            s.push_str(
                "\n\nif you're sure no run is in progress (Ctrl+C, crash), run `clever-project unlock` to release it.",
            );
        }
    } else {
        s.push_str(
            "\n(could not read lock metadata — run `clever-project unlock` to release if you're sure no run is in progress)",
        );
    }
    s
}

fn generate_id() -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let secs = now_unix();
    format!("{pid:08x}-{secs:x}-{nanos:x}")
}

fn current_user() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn age_pretty(acquired_at_unix: u64) -> String {
    let now = now_unix();
    if now <= acquired_at_unix {
        return "just now".to_string();
    }
    let secs = now - acquired_at_unix;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m {}s ago", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h ago", secs / 86_400, (secs % 86_400) / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_state_path() -> PathBuf {
        let seq = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!(
            "clever-project-lock-{}-{nanos}-{seq}.state",
            std::process::id()
        ));
        p
    }

    #[test]
    fn lock_path_appends_suffix() {
        let p = lock_path_for(Path::new("/tmp/foo.state"));
        assert_eq!(p, PathBuf::from("/tmp/foo.state.lock"));
    }

    #[test]
    fn acquire_creates_file_and_drop_releases_it() {
        let state = tmp_state_path();
        let lock = lock_path_for(&state);
        {
            let _guard = acquire(&state, "apply", Path::new("/tmp/proj.yaml")).unwrap();
            assert!(lock.exists(), "lock file should exist while guard is alive");
        }
        assert!(!lock.exists(), "lock file should be gone after drop");
    }

    #[test]
    fn second_acquire_fails_with_holder_metadata() {
        let state = tmp_state_path();
        let lock = lock_path_for(&state);
        let _g1 = acquire(&state, "apply", Path::new("/tmp/proj.yaml")).unwrap();

        let err = acquire(&state, "apply", Path::new("/tmp/proj.yaml")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("holding the lock"), "got: {msg}");
        assert!(msg.contains("operation:"), "got: {msg}");
        assert!(msg.contains("pid:"), "got: {msg}");

        // _g1 drops → lock released. Manual cleanup for safety.
        drop(_g1);
        let _ = std::fs::remove_file(&lock);
    }

    #[test]
    fn explicit_release_removes_lock() {
        let state = tmp_state_path();
        let lock = lock_path_for(&state);
        let g = acquire(&state, "apply", Path::new("/tmp/proj.yaml")).unwrap();
        g.release().unwrap();
        assert!(!lock.exists());
    }

    #[test]
    fn peek_returns_info_for_held_lock() {
        let state = tmp_state_path();
        let lock = lock_path_for(&state);
        let _g = acquire(&state, "delete", Path::new("/tmp/p.yaml")).unwrap();
        let info = peek(&lock).unwrap().expect("peek should return info");
        assert_eq!(info.operation, "delete");
        assert_eq!(info.project_path, "/tmp/p.yaml");
        assert_eq!(info.pid, std::process::id());
    }

    #[test]
    fn peek_returns_none_when_absent() {
        let state = tmp_state_path();
        let lock = lock_path_for(&state);
        assert!(peek(&lock).unwrap().is_none());
    }

    #[test]
    fn force_remove_clears_lock() {
        let state = tmp_state_path();
        let lock = lock_path_for(&state);
        std::mem::forget(acquire(&state, "apply", Path::new("/tmp/p.yaml")).unwrap());
        assert!(lock.exists());
        let removed = force_remove(&lock).unwrap();
        assert!(removed);
        assert!(!lock.exists());
        let removed_again = force_remove(&lock).unwrap();
        assert!(!removed_again);
    }

    #[test]
    fn age_pretty_formats() {
        let now = now_unix();
        assert_eq!(age_pretty(now), "just now");
        assert!(age_pretty(now.saturating_sub(10)).ends_with("s ago"));
        assert!(age_pretty(now.saturating_sub(120)).contains('m'));
        assert!(age_pretty(now.saturating_sub(7_200)).contains('h'));
        assert!(age_pretty(now.saturating_sub(200_000)).contains('d'));
    }
}
