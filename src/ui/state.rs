//! UI state fed by worker events.

use crate::config::Config;
use crate::pipeline::{Header, Outcome, StepMeta, WorkerEvent};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running(Instant),
    Ok,
    Skipped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct StepRow {
    pub meta: StepMeta,
    pub status: Status,
    pub detail: String,
    pub took: Option<Duration>,
}

pub struct App {
    pub header: Option<Header>,
    pub preparing: String,
    pub steps: Vec<StepRow>,
    pub logs: Vec<Vec<String>>,
    pub selected: usize,
    follow_running: bool,
    pub scroll: usize,
    pub follow_tail: bool,
    pub close_at: Option<Instant>,
    pub done: Option<bool>,
    pub fatal: Option<String>,
    pub quit: bool,
    pub dry_run: bool,
    pub retrying: bool,
    /// Height of the log viewport, updated by the renderer for paging.
    pub log_viewport: usize,
    auto_close: Duration,
}

const MAX_LOG_LINES: usize = 5000;

impl App {
    pub fn new(cfg: &Config) -> App {
        App {
            header: None,
            preparing: "starting".into(),
            steps: Vec::new(),
            logs: Vec::new(),
            selected: 0,
            follow_running: true,
            scroll: 0,
            follow_tail: true,
            close_at: None,
            done: None,
            fatal: None,
            quit: false,
            dry_run: false,
            retrying: false,
            log_viewport: 10,
            auto_close: Duration::from_secs(cfg.auto_close_secs),
        }
    }

    pub fn apply(&mut self, ev: WorkerEvent) {
        match ev {
            WorkerEvent::Preparing(what) => self.preparing = what,
            WorkerEvent::Plan { header, steps } => {
                self.dry_run = header.dry_run;
                self.header = Some(header);
                self.steps = steps
                    .into_iter()
                    .map(|meta| StepRow {
                        detail: meta.hint.clone(),
                        meta,
                        status: Status::Pending,
                        took: None,
                    })
                    .collect();
                self.logs = vec![Vec::new(); self.steps.len()];
            }
            WorkerEvent::Started { idx } => {
                if let Some(row) = self.steps.get_mut(idx) {
                    row.status = Status::Running(Instant::now());
                    row.detail = "running…".into();
                    row.took = None;
                }
                if let Some(log) = self.logs.get_mut(idx) {
                    if self.retrying {
                        log.push("── retry ──".into());
                    }
                }
                if self.follow_running {
                    self.selected = idx;
                    self.scroll = 0;
                    self.follow_tail = true;
                }
                self.done = None;
                self.close_at = None;
            }
            WorkerEvent::Line { idx, text } => {
                if let Some(log) = self.logs.get_mut(idx) {
                    log.push(text);
                    if log.len() > MAX_LOG_LINES {
                        let extra = log.len() - MAX_LOG_LINES;
                        log.drain(0..extra);
                    }
                }
            }
            WorkerEvent::Finished { idx, outcome, took } => {
                if let Some(row) = self.steps.get_mut(idx) {
                    row.took = Some(took);
                    let (status, detail) = match outcome {
                        Outcome::Ok(d) => (Status::Ok, d),
                        Outcome::Skipped(d) => (Status::Skipped, d),
                        Outcome::Failed(d) => (Status::Failed, d),
                    };
                    row.status = status;
                    row.detail = detail;
                }
            }
            WorkerEvent::Done { ok } => {
                self.done = Some(ok);
                self.retrying = false;
                if ok && !self.auto_close.is_zero() {
                    self.close_at = Some(Instant::now() + self.auto_close);
                }
                if !ok {
                    if let Some(first_failed) =
                        self.steps.iter().position(|s| s.status == Status::Failed)
                    {
                        if self.follow_running {
                            self.selected = first_failed;
                            self.scroll = 0;
                            self.follow_tail = true;
                        }
                    }
                }
            }
            WorkerEvent::Fatal(msg) => {
                self.fatal = Some(msg);
                self.done = Some(false);
            }
        }
    }

    pub fn tick(&mut self) {
        if let Some(at) = self.close_at {
            if Instant::now() >= at {
                self.quit = true;
            }
        }
    }

    pub fn cancel_countdown(&mut self) {
        self.close_at = None;
    }

    pub fn remaining_close_secs(&self) -> Option<u64> {
        self.close_at.map(|at| {
            at.saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil() as u64
        })
    }

    pub fn has_failures(&self) -> bool {
        self.steps.iter().any(|s| s.status == Status::Failed)
    }

    pub fn is_running(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.status, Status::Running(_)))
    }

    pub fn exit_code(&self) -> i32 {
        if self.fatal.is_some() {
            2
        } else if self.done == Some(true) {
            0
        } else {
            1
        }
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.steps.len() {
            self.selected = idx;
            self.follow_running = false;
            self.scroll = 0;
            self.follow_tail = true;
        }
    }

    pub fn select_delta(&mut self, delta: isize) {
        if self.steps.is_empty() {
            return;
        }
        let next =
            (self.selected as isize + delta).clamp(0, self.steps.len() as isize - 1) as usize;
        self.select(next);
    }

    pub fn selected_log(&self) -> &[String] {
        self.logs
            .get(self.selected)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn max_scroll(&self) -> usize {
        self.selected_log().len().saturating_sub(self.log_viewport)
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.max_scroll();
        let current = if self.follow_tail { max } else { self.scroll };
        let next = (current as isize + delta).clamp(0, max as isize) as usize;
        self.scroll = next;
        self.follow_tail = next >= max;
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow_tail = self.max_scroll() == 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.follow_tail = true;
        self.scroll = self.max_scroll();
    }

    /// First visible log line index given the current viewport.
    pub fn log_offset(&self) -> usize {
        if self.follow_tail {
            self.max_scroll()
        } else {
            self.scroll.min(self.max_scroll())
        }
    }
}
