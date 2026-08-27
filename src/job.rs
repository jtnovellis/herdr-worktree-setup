//! The unit of work: which source checkout feeds which new worktree, plus the
//! on-disk job/lock files under the plugin state directory.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Job {
    /// Main checkout (`repo_root`) the state is copied from.
    pub source: PathBuf,
    /// The freshly created worktree.
    pub target: PathBuf,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub repo_name: String,
    #[serde(default)]
    pub dry_run: bool,
    /// Whether the new workspace was focused when the event fired; a setup
    /// pane never steals focus from a worktree created in the background.
    #[serde(default = "default_true")]
    pub workspace_focused: bool,
    /// Where this job was read from; the lock lives beside it so the hook and
    /// the pane agree on the path regardless of their environments.
    #[serde(skip)]
    pub job_file: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

/// Result of deriving a job from herdr's JSON: either something to do, or a
/// reason this invocation should quietly do nothing.
#[derive(Debug)]
pub enum JobSource {
    Ready(Job),
    Skip(String),
}

// --- herdr event envelope (worktree.created) --------------------------------

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    #[allow(dead_code)]
    event: String,
    data: EventData,
}

#[derive(Deserialize)]
struct EventData {
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    kind: String,
    workspace: Option<WorkspaceInfo>,
    worktree: Option<WorktreeInfo>,
}

#[derive(Deserialize)]
struct WorkspaceInfo {
    workspace_id: String,
    #[serde(default = "default_true")]
    focused: bool,
    worktree: Option<WorkspaceWorktreeInfo>,
}

#[derive(Deserialize)]
struct WorkspaceWorktreeInfo {
    repo_root: String,
    checkout_path: String,
    #[serde(default)]
    repo_name: String,
    #[serde(default)]
    is_linked_worktree: bool,
}

#[derive(Deserialize)]
struct WorktreeInfo {
    path: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    open_workspace_id: Option<String>,
    #[serde(default)]
    is_linked_worktree: bool,
}

// --- herdr invocation context (actions) --------------------------------------

#[derive(Deserialize)]
struct InvocationContext {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    worktree: Option<WorkspaceWorktreeInfo>,
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn repo_name_or_basename(name: &str, source: &Path) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string())
}

impl Job {
    /// Parse `HERDR_PLUGIN_EVENT_JSON` (full envelope, or bare `data`).
    pub fn from_event_json(raw: &str) -> Result<JobSource> {
        let data: EventData = match serde_json::from_str::<Envelope>(raw) {
            Ok(env) => env.data,
            Err(_) => serde_json::from_str::<EventData>(raw)
                .context("HERDR_PLUGIN_EVENT_JSON is not a worktree event")?,
        };
        let ws = data.workspace;
        let wt = data.worktree;
        let (target, branch, linked_wt) = match &wt {
            Some(w) => (
                Some(PathBuf::from(&w.path)),
                w.branch.clone(),
                w.is_linked_worktree,
            ),
            None => (None, None, false),
        };
        let wsi = ws.as_ref().and_then(|w| w.worktree.as_ref());
        let target = target
            .or_else(|| wsi.map(|i| PathBuf::from(&i.checkout_path)))
            .ok_or_else(|| anyhow!("event carries no worktree path"))?;
        let Some(info) = wsi else {
            return Ok(JobSource::Skip(
                "event has no workspace.worktree.repo_root (not a git worktree workspace)".into(),
            ));
        };
        let source = PathBuf::from(&info.repo_root);
        if !(info.is_linked_worktree || linked_wt) || same_path(&source, &target) {
            return Ok(JobSource::Skip(format!(
                "{} is the main checkout, nothing to set up",
                target.display()
            )));
        }
        let workspace_id = ws
            .as_ref()
            .map(|w| w.workspace_id.clone())
            .or_else(|| wt.as_ref().and_then(|w| w.open_workspace_id.clone()));
        let workspace_focused = ws.as_ref().map(|w| w.focused).unwrap_or(true);
        Ok(JobSource::Ready(Job {
            repo_name: repo_name_or_basename(&info.repo_name, &source),
            source,
            target,
            branch,
            workspace_id,
            dry_run: false,
            workspace_focused,
            job_file: None,
        }))
    }

    /// Parse `HERDR_PLUGIN_CONTEXT_JSON` (action invocations).
    pub fn from_context_json(raw: &str) -> Result<JobSource> {
        let ctx: InvocationContext =
            serde_json::from_str(raw).context("HERDR_PLUGIN_CONTEXT_JSON is not valid")?;
        let Some(info) = ctx.worktree else {
            return Ok(JobSource::Skip(
                "this workspace is not a git worktree workspace (no worktree in context)".into(),
            ));
        };
        let source = PathBuf::from(&info.repo_root);
        let target = PathBuf::from(&info.checkout_path);
        if !info.is_linked_worktree || same_path(&source, &target) {
            return Ok(JobSource::Skip(format!(
                "{} is the main checkout, nothing to set up",
                target.display()
            )));
        }
        Ok(JobSource::Ready(Job {
            repo_name: repo_name_or_basename(&info.repo_name, &source),
            source,
            target,
            branch: None,
            workspace_id: ctx.workspace_id,
            dry_run: false,
            workspace_focused: true,
            job_file: None,
        }))
    }

    /// Job for a `ui` process: `HWS_JOB` file first, then the invocation context.
    pub fn from_env() -> Result<Job> {
        if let Ok(path) = std::env::var("HWS_JOB") {
            return Job::read(Path::new(&path));
        }
        if let Ok(raw) = std::env::var("HERDR_PLUGIN_CONTEXT_JSON") {
            return match Job::from_context_json(&raw)? {
                JobSource::Ready(job) => Ok(job),
                JobSource::Skip(reason) => Err(anyhow!(reason)),
            };
        }
        Err(anyhow!(
            "no job: set HWS_JOB=<job.json> or run inside a herdr plugin pane"
        ))
    }

    pub fn fill_branch(&mut self, git: &Path) {
        if self.branch.is_some() {
            return;
        }
        let out = std::process::Command::new(git)
            .args(["-C"])
            .arg(&self.target)
            .args(["symbolic-ref", "--short", "-q", "HEAD"])
            .output();
        if let Ok(out) = out {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !s.is_empty() {
                self.branch = Some(s);
            }
        }
    }

    /// Stable file-name key for job/lock files.
    pub fn key(&self) -> String {
        let raw = self
            .workspace_id
            .clone()
            .unwrap_or_else(|| self.target.display().to_string());
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    pub fn write(&self, state_dir: &Path) -> Result<PathBuf> {
        let dir = state_dir.join("jobs");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("{}.json", self.key()));
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    pub fn read(path: &Path) -> Result<Job> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading job file {}", path.display()))?;
        let mut job: Job = serde_json::from_str(&raw)
            .with_context(|| format!("parsing job file {}", path.display()))?;
        job.job_file = Some(path.to_path_buf());
        Ok(job)
    }

    /// `<jobs>/<key>.pid`, beside the job file when the job came from one.
    pub fn lock_path(&self, state_dir: &Path) -> PathBuf {
        match &self.job_file {
            Some(file) => file.with_extension("pid"),
            None => state_dir.join("jobs").join(format!("{}.pid", self.key())),
        }
    }
}

// --- state / config directories ---------------------------------------------

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state/herdr-worktree-setup"))
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config/herdr-worktree-setup"))
}

/// Remove job/lock files older than a week (best effort).
pub fn sweep_old(state_dir: &Path) {
    let dir = state_dir.join("jobs");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = Duration::from_secs(7 * 24 * 3600);
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if SystemTime::now()
            .duration_since(modified)
            .map(|age| age > cutoff)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// --- live-process lock --------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Lock {
    pub pid: u32,
    #[serde(default)]
    pub pane_id: Option<String>,
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // kill(pid, 0) probes existence; EPERM still means "exists".
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

impl Lock {
    /// The lock at `path` if it names a process that is still running.
    pub fn read_live(path: &Path) -> Option<Lock> {
        let raw = std::fs::read_to_string(path).ok()?;
        let lock: Lock = serde_json::from_str(&raw).ok()?;
        if pid_alive(lock.pid) {
            Some(lock)
        } else {
            let _ = std::fs::remove_file(path);
            None
        }
    }

    pub fn write(path: &Path, pane_id: Option<String>) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = Lock {
            pid: std::process::id(),
            pane_id,
        };
        std::fs::write(path, serde_json::to_string(&lock)?)?;
        Ok(())
    }

    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = include_str!("../tests/fixtures/event.json");

    #[test]
    fn parses_worktree_created_envelope() {
        let JobSource::Ready(job) = Job::from_event_json(EVENT).unwrap() else {
            panic!("expected a job");
        };
        assert_eq!(job.source, PathBuf::from("/Users/me/developer/clyras"));
        assert_eq!(
            job.target,
            PathBuf::from("/Users/me/.herdr/worktrees/clyras/feature-x")
        );
        assert_eq!(job.branch.as_deref(), Some("feature/x"));
        assert_eq!(job.workspace_id.as_deref(), Some("w9"));
        assert_eq!(job.repo_name, "clyras");
        assert!(!job.dry_run);
        assert!(job.workspace_focused);
        let unfocused = EVENT.replace("\"focused\": true", "\"focused\": false");
        let JobSource::Ready(job) = Job::from_event_json(&unfocused).unwrap() else {
            panic!()
        };
        assert!(!job.workspace_focused);
    }

    #[test]
    fn main_checkout_is_skipped() {
        let raw = EVENT
            .replace(
                "/Users/me/.herdr/worktrees/clyras/feature-x",
                "/Users/me/developer/clyras",
            )
            .replace(
                "\"is_linked_worktree\": true",
                "\"is_linked_worktree\": false",
            );
        assert!(matches!(
            Job::from_event_json(&raw).unwrap(),
            JobSource::Skip(_)
        ));
    }

    #[test]
    fn context_without_worktree_is_skipped() {
        let raw = r#"{"workspace_id":"w1","workspace_cwd":"/tmp/x"}"#;
        assert!(matches!(
            Job::from_context_json(raw).unwrap(),
            JobSource::Skip(_)
        ));
    }

    #[test]
    fn context_with_linked_worktree_is_ready() {
        let raw = r#"{"workspace_id":"w9","worktree":{"repo_root":"/r","checkout_path":"/w","repo_name":"r","repo_key":"/r/.git","is_linked_worktree":true}}"#;
        let JobSource::Ready(job) = Job::from_context_json(raw).unwrap() else {
            panic!("expected a job");
        };
        assert_eq!(job.source, PathBuf::from("/r"));
        assert_eq!(job.target, PathBuf::from("/w"));
        assert_eq!(job.key(), "w9");
    }

    #[test]
    fn job_roundtrip_and_lock() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job {
            source: "/a".into(),
            target: "/b".into(),
            branch: None,
            workspace_id: Some("w1:x".into()),
            repo_name: "a".into(),
            dry_run: true,
            workspace_focused: false,
            job_file: None,
        };
        let path = job.write(dir.path()).unwrap();
        assert!(path.ends_with("jobs/w1_x.json"));
        let read_back = Job::read(&path).unwrap();
        assert_eq!(read_back.job_file.as_deref(), Some(path.as_path()));
        assert_eq!(
            read_back.lock_path(Path::new("/elsewhere")),
            path.with_extension("pid")
        );
        assert_eq!(
            Job {
                job_file: None,
                ..read_back
            },
            job
        );
        let lock = job.lock_path(dir.path());
        Lock::write(&lock, Some("w1:p1".into())).unwrap();
        let live = Lock::read_live(&lock).expect("own pid is alive");
        assert_eq!(live.pid, std::process::id());
        std::fs::write(&lock, r#"{"pid":999999999,"pane_id":null}"#).unwrap();
        assert!(Lock::read_live(&lock).is_none());
        assert!(!lock.exists(), "dead lock is removed");
    }
}
