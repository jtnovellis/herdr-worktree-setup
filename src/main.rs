//! herdr-worktree-setup — make a fresh git worktree immediately usable.

mod config;
mod detect;
mod discover;
mod exec;
mod fsops;
mod herdr;
mod hook;
mod job;
mod pipeline;
mod planner;
mod rules;
mod shellenv;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use job::Job;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "herdr-worktree-setup", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// herdr `worktree.created` hook: derive the job and open the setup pane.
    Hook,
    /// Run the pipeline in this pane (needs HWS_JOB or a herdr plugin context).
    Ui {
        /// Line output instead of the TUI (automatic when stdout is not a terminal).
        #[arg(long)]
        plain: bool,
    },
    /// Workspace action: open the setup pane for the current worktree workspace.
    Run {
        /// Only show what would happen.
        #[arg(long)]
        dry_run: bool,
    },
    /// Plan (or apply) outside herdr: what would be brought from SOURCE into TARGET.
    Plan {
        /// Main checkout to copy from.
        #[arg(long)]
        source: PathBuf,
        /// Worktree to set up.
        #[arg(long)]
        target: PathBuf,
        /// Actually run the pipeline instead of only printing the plan.
        #[arg(long)]
        apply: bool,
        /// Use the TUI instead of line output.
        #[arg(long)]
        tui: bool,
    },
}

fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

fn load_config(job: &Job) -> std::result::Result<Config, String> {
    Config::load(&job::config_dir(), &job.source, &job.target).map_err(|e| format!("{e:#}"))
}

fn run_pipeline(job: Job, plain: bool) -> i32 {
    let cfg = match load_config(&job) {
        Ok(cfg) => cfg,
        Err(msg) => {
            if plain {
                eprintln!("worktree-setup: {msg}");
                return 2;
            }
            return ui::show_fatal(&msg);
        }
    };
    if plain {
        ui::plain::run(job, cfg)
    } else {
        match ui::run(job, cfg) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("worktree-setup: {err:#}");
                2
            }
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let code: Result<i32> = match cli.cmd {
        Cmd::Hook => hook::hook(),
        Cmd::Run { dry_run } => hook::action(dry_run),
        Cmd::Ui { plain } => match Job::from_env() {
            Ok(job) => Ok(run_pipeline(job, plain || !stdout_is_tty())),
            Err(err) => {
                eprintln!("worktree-setup: {err:#}");
                Ok(3)
            }
        },
        Cmd::Plan {
            source,
            target,
            apply,
            tui,
        } => {
            let source = source.canonicalize().unwrap_or(source);
            let target = target.canonicalize().unwrap_or(target);
            let repo_name = source
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".into());
            let job = Job {
                source,
                target,
                branch: None,
                workspace_id: None,
                repo_name,
                dry_run: !apply,
                workspace_focused: true,
                job_file: None,
            };
            Ok(run_pipeline(job, !tui || !stdout_is_tty()))
        }
    };
    match code {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("worktree-setup: {err:#}");
            std::process::exit(1);
        }
    }
}
