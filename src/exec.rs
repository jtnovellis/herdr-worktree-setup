//! Run one step command with the resolved environment, streaming merged
//! stdout/stderr line by line. Plain pipes, no pty (see README: pty is a
//! possible later feature behind the same interface).
//!
//! SECURITY: every child runs in its own process group so that a hung or
//! forking step can be terminated as a unit, and steps are bounded by a
//! deadline. Install steps are executed as argv with no shell involved; only
//! user/repo-authored `[[steps]]` get a shell, which is what they are for.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct StepCmd {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Wall-clock limit; `None` means no limit.
    pub timeout: Option<Duration>,
}

/// The pid of the running child, which is also its process-group id.
pub type PidSlot = Arc<Mutex<Option<u32>>>;

/// `direnv exec <target>` / `mise exec --` prefix, using the absolute paths we
/// already resolved. Bare names are deliberately avoided: a step's PATH can be
/// influenced by configuration, and the tool manager must not be swappable.
pub fn tool_prefix(target: &Path, direnv: Option<&Path>, mise: Option<&Path>) -> Vec<String> {
    let mut argv = Vec::new();
    if let Some(direnv) = direnv {
        argv.extend([
            direnv.display().to_string(),
            "exec".to_string(),
            target.display().to_string(),
        ]);
    }
    if let Some(mise) = mise {
        argv.extend([
            mise.display().to_string(),
            "exec".to_string(),
            "--".to_string(),
        ]);
    }
    argv
}

/// A command run through the tool managers with NO shell: `argv` is executed
/// directly, so its elements can never be reinterpreted as syntax.
pub fn direct_argv(
    argv: &[String],
    target: &Path,
    direnv: Option<&Path>,
    mise: Option<&Path>,
) -> Vec<String> {
    let mut out = tool_prefix(target, direnv, mise);
    out.extend(argv.iter().cloned());
    out
}

/// A shell command line, for user/repo-authored `[[steps]]` that legitimately
/// need shell syntax. Never use this for anything derived from repo *data*.
pub fn shell_argv(
    cmd: &str,
    target: &Path,
    direnv: Option<&Path>,
    mise: Option<&Path>,
) -> Vec<String> {
    let mut argv = tool_prefix(target, direnv, mise);
    argv.extend(["/bin/sh".to_string(), "-c".to_string(), cmd.to_string()]);
    argv
}

pub fn display(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || c == '\'' || c == '"')
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const MAX_LINE: usize = 4096;
/// How long a killed step gets to die politely before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// Split a byte stream on `\n` / `\r` (progress-bar rewrites become lines), cap
/// line length, and hand each line to `emit`.
fn pump(mut reader: impl Read, emit: &mpsc::Sender<String>) {
    let mut buf = [0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    let mut last_cr = false;
    let flush = |line: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(line).into_owned();
        line.clear();
        let _ = emit.send(text);
    };
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &b in &buf[..n] {
            match b {
                b'\n' => {
                    if !last_cr {
                        flush(&mut line);
                    }
                    last_cr = false;
                }
                b'\r' => {
                    flush(&mut line);
                    last_cr = true;
                }
                _ => {
                    last_cr = false;
                    line.push(b);
                    if line.len() >= MAX_LINE {
                        flush(&mut line);
                    }
                }
            }
        }
    }
    if !line.is_empty() {
        flush(&mut line);
    }
}

/// Signal a whole process group. `pgid` is the group leader's pid, which is
/// what `process_group(0)` gives us.
fn signal_group(pgid: u32, sig: libc::c_int) {
    if pgid == 0 {
        return;
    }
    // Negative pid means "the process group"; this reaches grandchildren that a
    // step forked, which signalling the leader alone would leave running.
    unsafe {
        libc::kill(-(pgid as libc::pid_t), sig);
    }
}

pub fn terminate_group(pgid: u32) {
    signal_group(pgid, libc::SIGTERM);
}

/// Spawn, stream, wait. Returns the exit code (128+signal when killed, and
/// `TIMED_OUT` when the deadline was hit).
pub const TIMED_OUT: i32 = -1;

pub fn run_streaming(
    cmd: &StepCmd,
    pid_slot: &PidSlot,
    on_line: &mut dyn FnMut(String),
) -> io::Result<i32> {
    use std::os::unix::process::CommandExt;

    let (program, args) = cmd
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(&cmd.cwd)
        .env_clear()
        .envs(cmd.env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group: a step that forks can be stopped as a unit, and it
        // cannot signal or read from herdr's terminal.
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    *pid_slot.lock().unwrap() = Some(pid);

    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || pump(out, &tx)));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || pump(err, &tx)));
    }
    drop(tx);

    let start = Instant::now();
    let mut timed_out = false;
    let mut killed_at: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => on_line(line),
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(limit) = cmd.timeout {
            if killed_at.is_none() && start.elapsed() > limit {
                timed_out = true;
                killed_at = Some(Instant::now());
                on_line(format!(
                    "[worktree-setup] step exceeded its {}s timeout — terminating",
                    limit.as_secs()
                ));
                signal_group(pid, libc::SIGTERM);
            }
        }
        if let Some(at) = killed_at {
            if at.elapsed() > KILL_GRACE {
                signal_group(pid, libc::SIGKILL);
                killed_at = Some(Instant::now());
            }
        }
    }
    for reader in readers {
        let _ = reader.join();
    }

    // Pipes are closed. Reap without ever blocking while holding the pid lock,
    // and clear the slot in the same critical section that observes the exit so
    // a concurrent abort can never signal a recycled pid.
    let deadline = Instant::now() + KILL_GRACE;
    let status = loop {
        let mut slot = pid_slot.lock().unwrap();
        match child.try_wait() {
            Ok(Some(status)) => {
                *slot = None;
                break status;
            }
            Ok(None) => {}
            // Never return while the slot still names a pid that may already be
            // reaped: a later abort would signal whatever inherits the number.
            Err(err) => {
                *slot = None;
                return Err(err);
            }
        }
        drop(slot);
        if Instant::now() > deadline {
            // Closed its pipes but will not exit.
            signal_group(pid, libc::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if timed_out {
        return Ok(TIMED_OUT);
    }
    Ok(status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(1)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(OsString, OsString)> {
        vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))]
    }

    fn sh(script: &str, timeout: Option<Duration>) -> StepCmd {
        StepCmd {
            argv: shell_argv(script, Path::new("/"), None, None),
            cwd: PathBuf::from("/"),
            env: env(),
            timeout,
        }
    }

    #[test]
    fn streams_stdout_and_stderr_lines_and_exit_code() {
        let cmd = sh(
            "echo one; echo two >&2; printf 'a\\rb\\r\\nc'; exit 3",
            None,
        );
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let mut lines = Vec::new();
        let code = run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap();
        assert_eq!(code, 3);
        lines.sort();
        assert_eq!(lines, vec!["a", "b", "c", "one", "two"]);
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn wraps_with_direnv_and_mise_using_absolute_paths() {
        let argv = shell_argv(
            "pnpm install",
            Path::new("/wt"),
            Some(Path::new("/opt/bin/direnv")),
            Some(Path::new("/opt/bin/mise")),
        );
        assert_eq!(
            argv,
            vec![
                "/opt/bin/direnv",
                "exec",
                "/wt",
                "/opt/bin/mise",
                "exec",
                "--",
                "/bin/sh",
                "-c",
                "pnpm install"
            ]
        );
    }

    #[test]
    fn direct_argv_never_introduces_a_shell() {
        let argv = direct_argv(
            &["pnpm".into(), "install".into()],
            Path::new("/wt"),
            None,
            Some(Path::new("/opt/bin/mise")),
        );
        assert_eq!(argv, vec!["/opt/bin/mise", "exec", "--", "pnpm", "install"]);
        assert!(!argv.iter().any(|a| a.ends_with("/sh") || a == "-c"));
    }

    /// A hostile "tool name" is inert when executed as argv: it is looked up as
    /// one program name, never parsed.
    #[test]
    fn argv_execution_does_not_interpret_metacharacters() {
        let marker = std::env::temp_dir().join("hws-argv-injection-probe");
        let _ = std::fs::remove_file(&marker);
        let cmd = StepCmd {
            argv: vec!["/bin/echo".into(), format!("x; touch {}", marker.display())],
            cwd: PathBuf::from("/"),
            env: env(),
            timeout: None,
        };
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let mut lines = Vec::new();
        assert_eq!(
            run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap(),
            0
        );
        assert!(!marker.exists(), "argv element was interpreted by a shell");
    }

    #[test]
    fn long_lines_are_chunked() {
        let cmd = sh("head -c 9000 /dev/zero | tr '\\0' x", None);
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let mut lines = Vec::new();
        run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.len() <= MAX_LINE));
    }

    /// A step that ignores SIGTERM and forks a child holding the pipes open is
    /// still stopped, and the whole group dies with it.
    #[test]
    fn timeout_kills_a_stubborn_process_group() {
        let cmd = sh(
            "trap '' TERM; sleep 60 & sleep 60; wait",
            Some(Duration::from_millis(300)),
        );
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let start = Instant::now();
        let mut lines = Vec::new();
        let code = run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap();
        assert_eq!(code, TIMED_OUT);
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "took {:?}",
            start.elapsed()
        );
        assert!(lines.iter().any(|l| l.contains("exceeded its")));
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn terminating_the_group_reaches_a_forked_grandchild() {
        let marker = std::env::temp_dir().join(format!("hws-group-kill-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let cmd = sh(
            &format!("(sleep 5; touch {}) & echo started; wait", marker.display()),
            Some(Duration::from_millis(200)),
        );
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let mut lines = Vec::new();
        run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap();
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            !marker.exists(),
            "grandchild survived the group kill and touched {}",
            marker.display()
        );
    }
}
