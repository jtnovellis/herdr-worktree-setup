//! Line-oriented runner for non-TTY panes, `plan`, and smoke tests.

use crate::config::Config;
use crate::job::Job;
use crate::pipeline::{Outcome, Worker, WorkerEvent};
use std::io::Write;

fn fmt_secs(d: std::time::Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

pub fn run(job: Job, cfg: Config) -> i32 {
    let worker = Worker::spawn(job, cfg);
    let mut names: Vec<String> = Vec::new();
    let mut code = 1;
    let out = std::io::stdout();
    let mut out = out.lock();
    for ev in &worker.rx {
        match ev {
            WorkerEvent::Preparing(what) => {
                let _ = writeln!(out, "… {what}");
            }
            WorkerEvent::Plan { header, steps } => {
                let _ = writeln!(
                    out,
                    "Worktree Setup — {}{} → {}{}",
                    header.repo_name,
                    header
                        .branch
                        .as_ref()
                        .map(|b| format!(" · {b}"))
                        .unwrap_or_default(),
                    header.target.display(),
                    if header.dry_run { "  (dry run)" } else { "" }
                );
                let _ = writeln!(
                    out,
                    "  source: {}  env: {}",
                    header.source.display(),
                    header.env_origin
                );
                for layer in &header.layers {
                    let _ = writeln!(out, "  config: {layer}");
                }
                for warning in &header.warnings {
                    let _ = writeln!(out, "  warning: {warning}");
                }
                names = steps.iter().map(|s| s.name.clone()).collect();
                for (i, step) in steps.iter().enumerate() {
                    let _ = writeln!(out, "  {}. {}  ({})", i + 1, step.name, step.hint);
                }
            }
            WorkerEvent::Started { idx } => {
                let _ = writeln!(out, "\n▶ {}", names.get(idx).cloned().unwrap_or_default());
            }
            WorkerEvent::Line { text, .. } => {
                let _ = writeln!(out, "  {}", strip(&text));
            }
            WorkerEvent::Finished { idx, outcome, took } => {
                let name = names.get(idx).cloned().unwrap_or_default();
                let _ = match outcome {
                    Outcome::Ok(d) => writeln!(out, "✓ {name}: {d}  [{}]", fmt_secs(took)),
                    Outcome::Skipped(d) => writeln!(out, "– {name}: {d}"),
                    Outcome::Failed(d) => writeln!(out, "✗ {name}: {d}  [{}]", fmt_secs(took)),
                };
            }
            WorkerEvent::Done { ok } => {
                let _ = writeln!(
                    out,
                    "\n{}",
                    if ok {
                        "ready ✓"
                    } else {
                        "some steps failed ✗"
                    }
                );
                code = if ok { 0 } else { 1 };
                break;
            }
            WorkerEvent::Fatal(msg) => {
                let _ = writeln!(out, "\nerror: {msg}");
                code = 2;
                break;
            }
        }
        let _ = out.flush();
    }
    let _ = out.flush();
    code
}

fn strip(text: &str) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes::strip(text)).into_owned()
}
