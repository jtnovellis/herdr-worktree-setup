//! ratatui front end: a steps table, the selected step's live output, a
//! footer with keys and the auto-close countdown.

pub mod keys;
pub mod plain;
pub mod state;
pub mod view;

use crate::config::Config;
use crate::herdr::Herdr;
use crate::job::{self, Job, Lock};
use crate::pipeline::{Worker, WorkerEvent};
use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use state::App;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

const TICK: Duration = Duration::from_millis(50);

/// Run the pipeline for `job` with the TUI. Returns the process exit code.
pub fn run(job: Job, cfg: Config) -> Result<i32> {
    let pane_id = std::env::var("HERDR_PANE_ID").ok();
    let lock = job.lock_path(&job::state_dir());
    let _ = Lock::write(&lock, pane_id.clone());
    let herdr = Herdr::from_env();

    let worker = Worker::spawn(job, cfg.clone());
    let mut app = App::new(&cfg);
    let mut terminal = ratatui::init();
    let total_steps = |app: &App| app.steps.len();

    let result = (|| -> Result<()> {
        loop {
            loop {
                match worker.rx.try_recv() {
                    Ok(ev) => {
                        side_effects(&herdr, pane_id.as_deref(), &app, &ev, total_steps(&app));
                        app.apply(ev);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if app.done.is_none() && app.fatal.is_none() {
                            app.fatal = Some("worker stopped unexpectedly".into());
                        }
                        break;
                    }
                }
            }
            app.tick();
            if app.quit {
                break;
            }
            terminal.draw(|frame| view::render(frame, &mut app))?;
            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        keys::handle(&mut app, key, &worker)
                    }
                    Event::Mouse(m) => keys::handle_mouse(&mut app, m),
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    ratatui::restore();
    worker.abort();
    Lock::remove(&lock);
    result?;
    Ok(app.exit_code())
}

/// Sidebar title + toast, best effort, on step boundaries.
fn side_effects(herdr: &Herdr, pane_id: Option<&str>, app: &App, ev: &WorkerEvent, total: usize) {
    let Some(pane) = pane_id else { return };
    match ev {
        WorkerEvent::Finished { idx, .. } => {
            herdr.set_title(pane, &format!("Setup {}/{}", idx + 1, total.max(1)));
        }
        WorkerEvent::Done { ok } => {
            let branch = app
                .header
                .as_ref()
                .and_then(|h| h.branch.clone())
                .unwrap_or_else(|| "worktree".into());
            if *ok {
                herdr.set_title(pane, "Setup ✓");
                if !app.dry_run {
                    herdr.notify(
                        &format!("{branch} is ready"),
                        Some("Worktree Setup finished"),
                        "done",
                    );
                }
            } else {
                herdr.set_title(pane, "Setup ✗");
                herdr.notify(
                    &format!("{branch}: setup needs attention"),
                    Some("A Worktree Setup step failed — see the setup pane"),
                    "request",
                );
            }
        }
        WorkerEvent::Fatal(msg) => {
            herdr.set_title(pane, "Setup ✗");
            herdr.notify("Worktree Setup failed", Some(msg), "request");
        }
        _ => {}
    }
}

/// Show a fatal message full-screen until a key is pressed. Exit code 2.
pub fn show_fatal(message: &str) -> i32 {
    let mut app = App::new(&Config::default());
    app.fatal = Some(message.to_string());
    let mut terminal = ratatui::init();
    loop {
        if terminal
            .draw(|frame| view::render(frame, &mut app))
            .is_err()
        {
            break;
        }
        if let Ok(true) = event::poll(Duration::from_millis(100)) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press {
                    break;
                }
            }
        }
    }
    ratatui::restore();
    2
}
