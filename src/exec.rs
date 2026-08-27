//! Run one step command with the resolved environment, streaming merged
//! stdout/stderr line by line. Plain pipes, no pty (see README: pty is a
//! possible later feature behind the same interface).

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct StepCmd {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

pub type PidSlot = Arc<Mutex<Option<u32>>>;

/// `[direnv exec <target>] [mise exec --] /bin/sh -c <cmd>`
pub fn shell_argv(cmd: &str, target: &Path, use_direnv: bool, use_mise: bool) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if use_direnv {
        argv.extend([
            "direnv".to_string(),
            "exec".to_string(),
            target.display().to_string(),
        ]);
    }
    if use_mise {
        argv.extend(["mise".to_string(), "exec".to_string(), "--".to_string()]);
    }
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

/// Spawn, stream, wait. Returns the exit code (128+signal when killed).
pub fn run_streaming(
    cmd: &StepCmd,
    pid_slot: &PidSlot,
    on_line: &mut dyn FnMut(String),
) -> io::Result<i32> {
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
        .spawn()?;
    *pid_slot.lock().unwrap() = Some(child.id());

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
    for line in rx {
        on_line(line);
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child.wait()?;
    *pid_slot.lock().unwrap() = None;
    Ok(status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(1)
    }))
}

pub fn kill_pid(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(OsString, OsString)> {
        vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))]
    }

    #[test]
    fn streams_stdout_and_stderr_lines_and_exit_code() {
        let cmd = StepCmd {
            argv: shell_argv(
                "echo one; echo two >&2; printf 'a\\rb\\r\\nc'; exit 3",
                Path::new("/"),
                false,
                false,
            ),
            cwd: PathBuf::from("/"),
            env: env(),
        };
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let mut lines = Vec::new();
        let code = run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap();
        assert_eq!(code, 3);
        lines.sort();
        assert_eq!(lines, vec!["a", "b", "c", "one", "two"]);
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn wraps_with_direnv_and_mise() {
        let argv = shell_argv("pnpm install", Path::new("/wt"), true, true);
        assert_eq!(
            argv,
            vec![
                "direnv",
                "exec",
                "/wt",
                "mise",
                "exec",
                "--",
                "/bin/sh",
                "-c",
                "pnpm install"
            ]
        );
        assert_eq!(
            display(&argv),
            "direnv exec /wt mise exec -- /bin/sh -c 'pnpm install'"
        );
    }

    #[test]
    fn long_lines_are_chunked() {
        let cmd = StepCmd {
            argv: shell_argv(
                "head -c 9000 /dev/zero | tr '\\0' x",
                Path::new("/"),
                false,
                false,
            ),
            cwd: PathBuf::from("/"),
            env: env(),
        };
        let slot: PidSlot = Arc::new(Mutex::new(None));
        let mut lines = Vec::new();
        run_streaming(&cmd, &slot, &mut |l| lines.push(l)).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.len() <= MAX_LINE));
    }
}
