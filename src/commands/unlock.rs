use anyhow::{Context, Result, bail};
use serde::Serialize;
use tracing::{info, warn};

use crate::cli::UnlockArgs;
use crate::commands::prompt;
use crate::commands::resolve_project_file;
use crate::lock;
use crate::state::state_path_for_project;

pub fn run(args: UnlockArgs) -> Result<()> {
    let file = resolve_project_file(args.file, &std::env::current_dir()?)?;
    let state_path = state_path_for_project(&file);
    let lock_path = lock::lock_path_for(&state_path);

    let info = lock::peek(&lock_path).context("inspecting lock file")?;

    if args.format.is_json() {
        let payload = UnlockReport {
            project: file.display().to_string(),
            lock_path: lock_path.display().to_string(),
            held: info.is_some(),
            holder: info.as_ref().map(|i| Holder {
                operation: i.operation.clone(),
                pid: i.pid,
                user: i.user.clone(),
                project_path: i.project_path.clone(),
                acquired_at_unix: i.acquired_at_unix,
                lock_id: i.id.clone(),
            }),
            removed: false,
        };
        if info.is_none() {
            println!("{}", serde_json::to_string_pretty(&payload)?);
            return Ok(());
        }
        if !args.yes {
            bail!("--format json requires --yes to actually remove the lock");
        }
        let removed = lock::force_remove(&lock_path)?;
        let mut p = payload;
        p.removed = removed;
        println!("{}", serde_json::to_string_pretty(&p)?);
        return Ok(());
    }

    match info.as_ref() {
        None => {
            info!(
                "no lock file at `{}` — nothing to release",
                lock_path.display()
            );
            return Ok(());
        }
        Some(i) => {
            println!("Lock file: {}", lock_path.display());
            println!("  operation:    {}", i.operation);
            println!("  pid:          {}", i.pid);
            println!("  user:         {}", i.user.as_deref().unwrap_or("?"));
            println!("  project:      {}", i.project_path);
            println!("  lock id:      {}", i.id);
        }
    }

    if !args.yes {
        if !prompt::stdin_is_tty() {
            bail!(
                "stdin is not a TTY and --yes was not given; pass --yes to release the lock non-interactively"
            );
        }
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        let approved = prompt::ask_yes_no("\nRemove this lock", false, &mut stdin, &mut stdout)?;
        if !approved {
            warn!("aborted by user — lock not removed");
            return Ok(());
        }
    }

    let removed = lock::force_remove(&lock_path)?;
    if removed {
        info!("removed lock file `{}`", lock_path.display());
    } else {
        info!("lock file `{}` was already gone", lock_path.display());
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct UnlockReport {
    project: String,
    lock_path: String,
    held: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    holder: Option<Holder>,
    removed: bool,
}

#[derive(Debug, Serialize)]
struct Holder {
    operation: String,
    pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    project_path: String,
    acquired_at_unix: u64,
    lock_id: String,
}
