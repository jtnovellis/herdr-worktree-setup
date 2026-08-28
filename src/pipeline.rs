//! The ordered steps for one job, executed on a worker thread that reports
//! progress over a channel. Both the TUI and the plain runner consume it.

use crate::config::{Config, StepDef};
use crate::detect::{self, InstallCmd};
use crate::discover;
use crate::exec::{self, PidSlot, StepCmd};
use crate::fsops::{human_bytes, Copier, Method, Outcome as FsOutcome};
use crate::job::Job;
use crate::planner::{CopyPlan, Group, ItemState, PlanAction, PlanItem};
use crate::rules::Rules;
use crate::shellenv::ResolvedEnv;
use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Header {
    pub repo_name: String,
    pub branch: Option<String>,
    pub source: PathBuf,
    pub target: PathBuf,
    pub env_origin: String,
    pub dry_run: bool,
    pub layers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StepMeta {
    pub name: String,
    /// What the step is going to do, shown before it runs.
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok(String),
    Skipped(String),
    Failed(String),
}

#[derive(Debug)]
pub enum WorkerEvent {
    Preparing(String),
    Plan {
        header: Header,
        steps: Vec<StepMeta>,
    },
    Started {
        idx: usize,
    },
    Line {
        idx: usize,
        text: String,
    },
    Finished {
        idx: usize,
        outcome: Outcome,
        took: Duration,
    },
    Done {
        ok: bool,
    },
    Fatal(String),
}

#[derive(Debug)]
pub enum UiCommand {
    Retry(Vec<usize>),
    Abort,
}

enum StepKind {
    CopyState,
    CloneCaches,
    MiseTrust(Vec<PathBuf>),
    DirenvAllow,
    Install(InstallCmd),
    Custom(StepDef),
}

struct Step {
    meta: StepMeta,
    kind: StepKind,
}

pub struct Pipeline {
    job: Job,
    cfg: Config,
    env: ResolvedEnv,
    plan: CopyPlan,
    steps: Vec<Step>,
    outcomes: Vec<Option<Outcome>>,
    copier: Copier,
    step_env: Vec<(OsString, OsString)>,
    pid_slot: PidSlot,
    abort: Arc<AtomicBool>,
    use_mise: bool,
    use_direnv: bool,
    mise_bin: Option<PathBuf>,
    direnv_bin: Option<PathBuf>,
    step_timeout: Option<Duration>,
}

fn shorten_home(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && s.starts_with(&home) => format!("~{}", &s[home.len()..]),
        _ => s,
    }
}

impl Pipeline {
    pub fn prepare(
        mut job: Job,
        cfg: Config,
        pid_slot: PidSlot,
        abort: Arc<AtomicBool>,
        tx: &Sender<WorkerEvent>,
    ) -> Result<Pipeline> {
        if !job.target.is_dir() {
            anyhow::bail!("worktree {} does not exist", job.target.display());
        }
        if !job.source.is_dir() {
            anyhow::bail!("source checkout {} does not exist", job.source.display());
        }
        let _ = tx.send(WorkerEvent::Preparing("resolving shell environment".into()));
        let env = ResolvedEnv::resolve();
        let git = env.git();
        job.fill_branch(&git);

        let _ = tx.send(WorkerEvent::Preparing("discovering ignored state".into()));
        let rules = Rules::from_config(&cfg).context("invalid copy/clone/exclude patterns")?;
        let candidates = discover::list_ignored(&git, &job.source)?;
        let plan = CopyPlan::build(&job.source, &job.target, candidates, &rules);
        let copier = Copier::new(&job.source, &job.target, &cfg, rules);

        let mise_files = detect::mise_config_files(&job.target);
        let has_envrc = detect::has_envrc(&job.target);
        let mise_bin = env.find_tool("mise");
        let direnv_bin = env.find_tool("direnv");
        let use_mise = cfg.use_mise && !mise_files.is_empty() && mise_bin.is_some();
        let use_direnv = cfg.use_direnv && has_envrc && direnv_bin.is_some();

        let mut steps = Vec::new();
        let count = |g: Group| plan.pending_in(g).len();
        steps.push(Step {
            meta: StepMeta {
                name: "copy dev state".into(),
                hint: match count(Group::State) {
                    0 => "nothing to copy".into(),
                    n => format!("{n} items"),
                },
            },
            kind: StepKind::CopyState,
        });
        steps.push(Step {
            meta: StepMeta {
                name: "clone caches".into(),
                hint: match count(Group::Caches) {
                    0 => "nothing to clone".into(),
                    n => format!("{n} dirs"),
                },
            },
            kind: StepKind::CloneCaches,
        });
        steps.push(Step {
            meta: StepMeta {
                name: "mise trust".into(),
                hint: if mise_files.is_empty() {
                    "no mise config".into()
                } else {
                    mise_files
                        .iter()
                        .map(|p| {
                            p.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            },
            kind: StepKind::MiseTrust(mise_files),
        });
        steps.push(Step {
            meta: StepMeta {
                name: "direnv allow".into(),
                hint: if has_envrc {
                    ".envrc".into()
                } else {
                    "no .envrc".into()
                },
            },
            kind: StepKind::DirenvAllow,
        });
        for install in detect::detect_installs(&job.target) {
            let mut via = Vec::new();
            if use_direnv {
                via.push("direnv");
            }
            if use_mise {
                via.push("mise");
            }
            let hint = if via.is_empty() {
                install.label.clone()
            } else {
                format!("{} (via {})", install.label, via.join(" + "))
            };
            steps.push(Step {
                meta: StepMeta {
                    name: install.label.clone(),
                    hint,
                },
                kind: StepKind::Install(install),
            });
        }
        for def in &cfg.steps {
            steps.push(Step {
                meta: StepMeta {
                    name: def.name.clone(),
                    hint: def.run.clone(),
                },
                kind: StepKind::Custom(def.clone()),
            });
        }

        let mut step_env: Vec<(OsString, OsString)> = env
            .to_vec()
            .into_iter()
            .filter(|(k, _)| k != "HWS_JOB")
            .collect();
        let mut set = |k: &str, v: String| {
            step_env.retain(|(key, _)| key != k);
            step_env.push((OsString::from(k), OsString::from(v)));
        };
        set("HWS_SOURCE", job.source.display().to_string());
        set("HWS_TARGET", job.target.display().to_string());
        set("HWS_BRANCH", job.branch.clone().unwrap_or_default());
        set(
            "HWS_WORKSPACE_ID",
            job.workspace_id.clone().unwrap_or_default(),
        );
        if cfg.color {
            set("FORCE_COLOR", "1".into());
        }
        for (k, v) in &cfg.env {
            set(k, v.clone());
        }

        let step_timeout =
            (cfg.step_timeout_secs > 0).then(|| Duration::from_secs(cfg.step_timeout_secs));
        let outcomes = vec![None; steps.len()];
        Ok(Pipeline {
            job,
            cfg,
            env,
            plan,
            steps,
            outcomes,
            copier,
            step_env,
            pid_slot,
            abort,
            use_mise,
            use_direnv,
            mise_bin,
            direnv_bin,
            step_timeout,
        })
    }

    pub fn header(&self) -> Header {
        Header {
            repo_name: self.job.repo_name.clone(),
            branch: self.job.branch.clone(),
            source: PathBuf::from(shorten_home(&self.job.source)),
            target: PathBuf::from(shorten_home(&self.job.target)),
            env_origin: self.env.to_string(),
            dry_run: self.job.dry_run,
            layers: self
                .cfg
                .layers
                .iter()
                .map(|(_, p)| shorten_home(p))
                .collect(),
            warnings: self.cfg.warnings.clone(),
        }
    }

    pub fn metas(&self) -> Vec<StepMeta> {
        self.steps.iter().map(|s| s.meta.clone()).collect()
    }

    pub fn all_ok(&self) -> bool {
        !self
            .outcomes
            .iter()
            .any(|o| matches!(o, Some(Outcome::Failed(_))))
    }

    pub fn failed_indices(&self) -> Vec<usize> {
        self.outcomes
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, Some(Outcome::Failed(_))))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn run_all(&mut self, tx: &Sender<WorkerEvent>) {
        let idxs: Vec<usize> = (0..self.steps.len()).collect();
        self.run_indices(&idxs, tx);
    }

    pub fn run_indices(&mut self, idxs: &[usize], tx: &Sender<WorkerEvent>) {
        for &idx in idxs {
            if idx >= self.steps.len() {
                continue;
            }
            let start = Instant::now();
            let _ = tx.send(WorkerEvent::Started { idx });
            let outcome = if self.abort.load(Ordering::SeqCst) {
                Outcome::Skipped("aborted".into())
            } else {
                self.run_step(idx, tx)
            };
            self.outcomes[idx] = Some(outcome.clone());
            let _ = tx.send(WorkerEvent::Finished {
                idx,
                outcome,
                took: start.elapsed(),
            });
        }
        let _ = tx.send(WorkerEvent::Done { ok: self.all_ok() });
    }

    fn line(&self, tx: &Sender<WorkerEvent>, idx: usize, text: impl Into<String>) {
        if tx
            .send(WorkerEvent::Line {
                idx,
                text: text.into(),
            })
            .is_err()
        {
            self.abort.store(true, Ordering::SeqCst);
        }
    }

    fn run_step(&mut self, idx: usize, tx: &Sender<WorkerEvent>) -> Outcome {
        // Take the kind out to avoid borrowing self.steps while mutating self.
        let kind = std::mem::replace(&mut self.steps[idx].kind, StepKind::DirenvAllow);
        let outcome = match &kind {
            StepKind::CopyState => self.run_copy(idx, Group::State, tx),
            StepKind::CloneCaches => self.run_copy(idx, Group::Caches, tx),
            StepKind::MiseTrust(files) => self.run_mise_trust(idx, files, tx),
            StepKind::DirenvAllow => self.run_direnv_allow(idx, tx),
            StepKind::Install(cmd) => self.run_install(idx, cmd, tx),
            StepKind::Custom(def) => self.run_custom(idx, def, tx),
        };
        self.steps[idx].kind = kind;
        outcome
    }

    fn run_copy(&mut self, idx: usize, group: Group, tx: &Sender<WorkerEvent>) -> Outcome {
        let items: Vec<PlanItem> = self.plan.items_in(group).into_iter().cloned().collect();
        let blocked: Vec<(String, String)> = self
            .plan
            .blocked_in(group)
            .into_iter()
            .map(|(item, reason)| (item.rel.clone(), reason.to_string()))
            .collect();
        let pending: Vec<&PlanItem> = items
            .iter()
            .filter(|i| i.state == ItemState::Pending)
            .collect();
        for item in items.iter().filter(|i| i.state == ItemState::Exists) {
            self.line(
                tx,
                idx,
                format!("= {}  (already in worktree, kept)", item.rel),
            );
        }
        for (rel, reason) in &blocked {
            self.line(tx, idx, format!("! {rel}  refused: {reason}"));
        }
        if pending.is_empty() {
            if !blocked.is_empty() {
                return Outcome::Failed(format!("{} refused for safety", blocked.len()));
            }
            return Outcome::Skipped(match group {
                Group::State => "nothing to copy".into(),
                Group::Caches => "nothing to clone".into(),
            });
        }
        if self.job.dry_run {
            for item in &pending {
                let verb = item.action.map(PlanAction::verb).unwrap_or("copy");
                let suffix = if item.is_symlink {
                    " (symlink)"
                } else if item.is_dir {
                    "/"
                } else {
                    ""
                };
                self.line(tx, idx, format!("~ would {verb} {}{suffix}", item.rel));
            }
            if !blocked.is_empty() {
                return Outcome::Failed(format!("{} refused for safety", blocked.len()));
            }
            return Outcome::Ok(format!("{} planned", pending.len()));
        }
        let mut done = 0usize;
        let mut failed = 0usize;
        let mut refused = blocked.len();
        let mut files = 0u64;
        let mut bytes = 0u64;
        let mut methods: Vec<Method> = Vec::new();
        let mut copied: Vec<String> = Vec::new();
        for item in &pending {
            if self.abort.load(Ordering::SeqCst) {
                break;
            }
            let outcome = self.copier.apply(item);
            let text = match &outcome {
                FsOutcome::Done {
                    method,
                    files: f,
                    bytes: b,
                } => {
                    done += 1;
                    files += f;
                    bytes += b;
                    copied.push(item.rel.clone());
                    if !methods.contains(method) {
                        methods.push(*method);
                    }
                    if item.is_dir && !item.is_symlink {
                        let size = if *b > 0 {
                            format!(", {}", human_bytes(*b))
                        } else {
                            String::new()
                        };
                        format!("✓ {}/  ({method}, {f} files{size})", item.rel)
                    } else {
                        format!("✓ {}  ({method})", item.rel)
                    }
                }
                FsOutcome::Exists => format!("= {}  (already in worktree, kept)", item.rel),
                FsOutcome::Refused(reason) => {
                    refused += 1;
                    format!("! {}  refused: {reason}", item.rel)
                }
                FsOutcome::TooLarge { bytes } => format!(
                    "! {}  skipped: {} exceeds the copy cap and this volume cannot reflink",
                    item.rel,
                    human_bytes(*bytes)
                ),
                FsOutcome::Failed(err) => {
                    failed += 1;
                    format!("✗ {}  {err}", item.rel)
                }
            };
            self.line(tx, idx, text);
        }
        if group == Group::State {
            self.warn_about_untracked_secrets(idx, &copied, tx);
        }
        if failed > 0 {
            return Outcome::Failed(format!("{failed} of {} failed", pending.len()));
        }
        if refused > 0 {
            return Outcome::Failed(format!("{refused} refused for safety"));
        }
        let how = methods
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("+");
        Outcome::Ok(match group {
            Group::State => format!("{done} items via {how}"),
            Group::Caches => {
                let size = if bytes > 0 {
                    format!(", {}", human_bytes(bytes))
                } else {
                    String::new()
                };
                format!("{done} dirs via {how} ({files} files{size})")
            }
        })
    }

    /// Whether a file is gitignored in the source is what made it a candidate;
    /// whether it stays ignored in the worktree is decided by the branch. A
    /// branch that drops `.env` from its `.gitignore` turns a routine
    /// `git add -A` into a commit of the user's live credentials, so say so.
    fn warn_about_untracked_secrets(
        &self,
        idx: usize,
        copied: &[String],
        tx: &Sender<WorkerEvent>,
    ) {
        if copied.is_empty() {
            return;
        }
        let mut child = match std::process::Command::new(self.env.git())
            .arg("-C")
            .arg(&self.job.target)
            .args(["--literal-pathspecs", "check-ignore", "--stdin", "-z"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return,
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let payload: Vec<u8> = copied
                .iter()
                .flat_map(|rel| {
                    let mut bytes = rel.as_bytes().to_vec();
                    bytes.push(0);
                    bytes
                })
                .collect();
            let _ = stdin.write_all(&payload);
        }
        let Ok(out) = child.wait_with_output() else {
            return;
        };
        let ignored: std::collections::HashSet<&str> = out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();
        let exposed: Vec<&String> = copied
            .iter()
            .filter(|rel| !ignored.contains(rel.as_str()))
            .collect();
        if exposed.is_empty() {
            return;
        }
        self.line(
            tx,
            idx,
            format!(
                "! {} copied file(s) are NOT gitignored on this branch — `git add -A` would commit them: {}",
                exposed.len(),
                exposed
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    /// Run a command for a step, streaming its output; `Ok(exit_code)`.
    fn run_cmd(
        &self,
        idx: usize,
        argv: Vec<String>,
        tx: &Sender<WorkerEvent>,
    ) -> Result<i32, String> {
        self.line(tx, idx, format!("$ {}", exec::display(&argv)));
        let cmd = StepCmd {
            argv,
            cwd: self.job.target.clone(),
            env: self.step_env.clone(),
            timeout: self.step_timeout,
        };
        let mut emit = |text: String| {
            let _ = tx.send(WorkerEvent::Line { idx, text });
        };
        exec::run_streaming(&cmd, &self.pid_slot, &mut emit).map_err(|e| e.to_string())
    }

    fn run_mise_trust(&self, idx: usize, files: &[PathBuf], tx: &Sender<WorkerEvent>) -> Outcome {
        if files.is_empty() {
            return Outcome::Skipped("no mise config".into());
        }
        if !self.cfg.mise_trust {
            return Outcome::Skipped("disabled (mise_trust = false)".into());
        }
        let Some(mise) = &self.mise_bin else {
            return Outcome::Skipped(format!("mise not found on PATH ({})", self.env));
        };
        if self.job.dry_run {
            return Outcome::Skipped(format!("dry run: mise trust ×{}", files.len()));
        }
        let mut trusted = 0;
        for file in files {
            match self.run_cmd(
                idx,
                vec![
                    mise.display().to_string(),
                    "trust".into(),
                    file.display().to_string(),
                ],
                tx,
            ) {
                Ok(0) => trusted += 1,
                Ok(exec::TIMED_OUT) => return Outcome::Failed(self.timeout_message()),
                Ok(code) => return Outcome::Failed(format!("mise trust exited {code}")),
                Err(err) => return Outcome::Failed(err),
            }
        }
        Outcome::Ok(format!(
            "trusted {trusted} file{}",
            if trusted == 1 { "" } else { "s" }
        ))
    }

    fn run_direnv_allow(&self, idx: usize, tx: &Sender<WorkerEvent>) -> Outcome {
        if !detect::has_envrc(&self.job.target) {
            return Outcome::Skipped("no .envrc".into());
        }
        if !self.cfg.direnv_allow {
            return Outcome::Skipped("disabled (direnv_allow = false)".into());
        }
        let Some(direnv) = &self.direnv_bin else {
            return Outcome::Skipped(format!("direnv not found on PATH ({})", self.env));
        };
        if self.job.dry_run {
            return Outcome::Skipped("dry run: direnv allow".into());
        }
        match self.run_cmd(
            idx,
            vec![
                direnv.display().to_string(),
                "allow".into(),
                self.job.target.display().to_string(),
            ],
            tx,
        ) {
            Ok(0) => Outcome::Ok("allowed".into()),
            Ok(exec::TIMED_OUT) => Outcome::Failed(self.timeout_message()),
            Ok(code) => Outcome::Failed(format!("direnv allow exited {code}")),
            Err(err) => Outcome::Failed(err),
        }
    }

    /// The resolved `direnv`/`mise` binaries, when this repo uses them. Absolute
    /// paths, so a step's environment cannot swap the tool manager itself.
    fn tool_bins(&self) -> (Option<&std::path::Path>, Option<&std::path::Path>) {
        (
            self.use_direnv
                .then_some(self.direnv_bin.as_deref())
                .flatten(),
            self.use_mise.then_some(self.mise_bin.as_deref()).flatten(),
        )
    }

    /// A shell command line — only for user/repo-authored `[[steps]]`.
    fn wrapped(&self, command: &str) -> Vec<String> {
        let (direnv, mise) = self.tool_bins();
        exec::shell_argv(command, &self.job.target, direnv, mise)
    }

    /// An install command: argv, executed directly, never through a shell.
    fn wrapped_argv(&self, argv: &[String]) -> Vec<String> {
        let (direnv, mise) = self.tool_bins();
        exec::direct_argv(argv, &self.job.target, direnv, mise)
    }

    fn timeout_message(&self) -> String {
        match self.step_timeout {
            Some(limit) => format!("timed out after {}s", limit.as_secs()),
            None => "timed out".to_string(),
        }
    }

    fn run_install(&self, idx: usize, install: &InstallCmd, tx: &Sender<WorkerEvent>) -> Outcome {
        if !self.cfg.install {
            return Outcome::Skipped("disabled (install = false)".into());
        }
        if !self.use_mise && self.env.find_tool(&install.tool).is_none() {
            return Outcome::Failed(format!(
                "`{}` not found on PATH (resolved from {})",
                install.tool, self.env
            ));
        }
        let argv = self.wrapped_argv(&install.argv);
        if self.job.dry_run {
            return Outcome::Skipped(format!("dry run: {}", exec::display(&argv)));
        }
        match self.run_cmd(idx, argv, tx) {
            Ok(0) => Outcome::Ok("done".into()),
            Ok(exec::TIMED_OUT) => Outcome::Failed(self.timeout_message()),
            Ok(code) => Outcome::Failed(format!("exited {code}")),
            Err(err) => Outcome::Failed(err),
        }
    }

    fn run_custom(&self, idx: usize, def: &StepDef, tx: &Sender<WorkerEvent>) -> Outcome {
        if def.origin.is_repo() && !self.cfg.trust_repo_steps {
            return Outcome::Skipped("repo step not run (trust_repo_steps = false)".into());
        }
        if let Some(gate) = &def.if_path {
            if !self.job.target.join(gate).exists() {
                return Outcome::Skipped(format!("if: {gate} not present"));
            }
        }
        let argv = self.wrapped(&def.run);
        if self.job.dry_run {
            return Outcome::Skipped(format!("dry run: {}", exec::display(&argv)));
        }
        match self.run_cmd(idx, argv, tx) {
            Ok(0) => Outcome::Ok("done".into()),
            Ok(exec::TIMED_OUT) => Outcome::Failed(self.timeout_message()),
            Ok(code) if def.continue_on_error => {
                Outcome::Skipped(format!("exited {code} (ignored)"))
            }
            Ok(code) => Outcome::Failed(format!("exited {code}")),
            Err(err) => Outcome::Failed(err),
        }
    }
}

/// Handle to the worker thread.
pub struct Worker {
    pub rx: Receiver<WorkerEvent>,
    pub cmd_tx: Sender<UiCommand>,
    pub pid_slot: PidSlot,
    pub abort: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(job: Job, cfg: Config) -> Worker {
        let (tx, rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel::<UiCommand>();
        let pid_slot: PidSlot = Arc::new(Mutex::new(None));
        let abort = Arc::new(AtomicBool::new(false));
        let (slot, flag) = (pid_slot.clone(), abort.clone());
        let handle = std::thread::spawn(move || {
            let tx_panic = tx.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let mut pipeline = match Pipeline::prepare(job, cfg, slot, flag, &tx) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ = tx.send(WorkerEvent::Fatal(format!("{err:#}")));
                        return;
                    }
                };
                let _ = tx.send(WorkerEvent::Plan {
                    header: pipeline.header(),
                    steps: pipeline.metas(),
                });
                pipeline.run_all(&tx);
                // Retry requests until the UI hangs up or asks to abort.
                while let Ok(UiCommand::Retry(idxs)) = cmd_rx.recv() {
                    let idxs = if idxs.is_empty() {
                        pipeline.failed_indices()
                    } else {
                        idxs
                    };
                    pipeline.run_indices(&idxs, &tx);
                }
            }));
            if result.is_err() {
                let _ = tx_panic.send(WorkerEvent::Fatal("worker thread panicked".into()));
            }
        });
        Worker {
            rx,
            cmd_tx,
            pid_slot,
            abort,
            handle: Some(handle),
        }
    }

    /// Stop: no further steps, kill the running child (if any).
    pub fn abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
        if let Some(pid) = *self.pid_slot.lock().unwrap() {
            exec::terminate_group(pid);
        }
        let _ = self.cmd_tx.send(UiCommand::Abort);
    }

    pub fn retry_failed(&self) {
        let _ = self.cmd_tx.send(UiCommand::Retry(Vec::new()));
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.abort();
        if let Some(handle) = self.handle.take() {
            // Give the worker a moment; never block a closing pane for long.
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
