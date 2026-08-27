//! Entry points that run *outside* the pane: the `worktree.created` hook and
//! the `run` / `plan` actions. They derive a job, dedupe against a live setup
//! pane, and ask herdr to open the TUI pane in the new workspace.

use crate::config::{Config, Placement};
use crate::herdr::{Herdr, PaneOpen};
use crate::job::{self, Job, JobSource, Lock};
use anyhow::{Context, Result};

pub const PLUGIN_ID: &str = "worktree-setup";
pub const PANE_ENTRYPOINT: &str = "setup";

/// `worktree.created` hook. Exit 0 whenever there is simply nothing to do.
pub fn hook() -> Result<i32> {
    let Ok(raw) = std::env::var("HERDR_PLUGIN_EVENT_JSON") else {
        eprintln!("worktree-setup: HERDR_PLUGIN_EVENT_JSON is not set (not invoked by a herdr event hook)");
        return Ok(3);
    };
    match Job::from_event_json(&raw)? {
        JobSource::Skip(reason) => {
            println!("worktree-setup: {reason}");
            Ok(0)
        }
        JobSource::Ready(job) => launch(job, false),
    }
}

/// `run` / `plan` actions, invoked from a workspace context.
pub fn action(dry_run: bool) -> Result<i32> {
    let Ok(raw) = std::env::var("HERDR_PLUGIN_CONTEXT_JSON") else {
        eprintln!("worktree-setup: HERDR_PLUGIN_CONTEXT_JSON is not set (invoke this action from inside herdr)");
        return Ok(3);
    };
    match Job::from_context_json(&raw)? {
        JobSource::Skip(reason) => {
            eprintln!("worktree-setup: {reason}");
            Ok(1)
        }
        JobSource::Ready(job) => launch(job, dry_run),
    }
}

fn launch(mut job: Job, dry_run: bool) -> Result<i32> {
    job.dry_run = dry_run;
    let cfg = match Config::load_user(&job::config_dir()) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("worktree-setup: {err:#}; using defaults");
            Config::default()
        }
    };
    let state = job::state_dir();
    job::sweep_old(&state);
    let herdr = Herdr::from_env();

    let lock = job.lock_path(&state);
    if let Some(live) = Lock::read_live(&lock) {
        if let (Some(pane), true) = (&live.pane_id, cfg.focus && job.workspace_focused) {
            let _ = herdr.plugin_pane_focus(pane);
        }
        println!(
            "worktree-setup: setup already running for {} (pid {})",
            job.target.display(),
            live.pid
        );
        return Ok(0);
    }

    let job_file = job.write(&state).context("writing job file")?;

    // Split beside a pane of the *new* workspace, never whatever is focused elsewhere.
    let target_pane = match (&cfg.placement, &job.workspace_id) {
        (Placement::Split | Placement::Zoomed, Some(ws)) => {
            herdr.workspace_panes(ws).ok().and_then(|panes| {
                panes
                    .iter()
                    .find(|(_, focused)| *focused)
                    .or_else(|| panes.first())
                    .map(|(id, _)| id.clone())
            })
        }
        _ => None,
    };

    // A split needs a pane to split; without one, fall back to a tab in the workspace.
    let placement = match cfg.placement {
        Placement::Split | Placement::Zoomed if target_pane.is_none() => Placement::Tab,
        other => other,
    };
    let open = PaneOpen {
        plugin: std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.to_string()),
        entrypoint: PANE_ENTRYPOINT.to_string(),
        workspace: job.workspace_id.clone(),
        target_pane,
        placement,
        direction: cfg.direction,
        cwd: job.target.clone(),
        env: vec![("HWS_JOB".to_string(), job_file.display().to_string())],
        focus: cfg.focus && job.workspace_focused,
    };
    match herdr.plugin_pane_open(&open) {
        Ok(pane) => {
            println!(
                "worktree-setup: opened setup pane {} for {}",
                pane.unwrap_or_else(|| "(popup)".into()),
                job.target.display()
            );
            Ok(0)
        }
        Err(err) => {
            herdr.notify(
                "Worktree Setup could not open its pane",
                Some(&format!("{err:#}")),
                "request",
            );
            Err(err).context("opening the setup pane")
        }
    }
}
