//! herdr launches plugin commands with whatever environment its server has —
//! often a minimal PATH. Resolve the user's real shell environment once
//! (interactive login shell, with markers), then run every step with it.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BEGIN: &str = "__HWS_ENV_BEGIN__";
const END: &str = "__HWS_ENV_END__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvOrigin {
    Interactive,
    Login,
    Current,
}

impl fmt::Display for EnvOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EnvOrigin::Interactive => "interactive login shell",
            EnvOrigin::Login => "login shell",
            EnvOrigin::Current => "current process",
        })
    }
}

impl fmt::Display for ResolvedEnv {
    /// e.g. `zsh -lic` / `zsh -lc` / `current process env`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shell = self
            .shell
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sh".into());
        match self.origin {
            EnvOrigin::Interactive => write!(f, "{shell} -lic"),
            EnvOrigin::Login => write!(f, "{shell} -lc"),
            EnvOrigin::Current => f.write_str("current process env"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedEnv {
    pub vars: BTreeMap<OsString, OsString>,
    pub origin: EnvOrigin,
    pub shell: PathBuf,
}

const DROP: &[&str] = &[
    "_",
    "SHLVL",
    "PWD",
    "OLDPWD",
    "HWS_ENV_PROBE",
    "HWS_JOB",
    "COLUMNS",
    "LINES",
    "TERM",
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Directories that commonly hold developer tools but are only added to PATH
/// by rc files or installers; prepended when present on disk.
fn known_tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home() {
        for rel in [
            ".local/bin",
            ".bun/bin",
            ".cargo/bin",
            ".local/share/mise/shims",
            ".volta/bin",
            ".deno/bin",
            ".yarn/bin",
            "Library/pnpm",
            ".local/share/pnpm",
            ".nix-profile/bin",
            "go/bin",
        ] {
            dirs.push(home.join(rel));
        }
    }
    for abs in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ] {
        dirs.push(PathBuf::from(abs));
    }
    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

fn read_with_timeout(mut child: std::process::Child, timeout: Duration) -> Option<Vec<u8>> {
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break,
        }
    }
    reader.join().ok()
}

/// Parse `env -0` output: one `KEY=VALUE` record per NUL. Unambiguous, so a
/// value containing a newline cannot forge a later assignment (notably `PATH`).
fn parse_nul(block: &str) -> Option<BTreeMap<OsString, OsString>> {
    let vars: BTreeMap<OsString, OsString> = block
        .split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            // The marker `printf` leaves a newline before the first record; a
            // key never contains one, so stripping it is unambiguous.
            let (k, v) = record.trim_start_matches('\n').split_once('=')?;
            (!k.is_empty()).then(|| (OsString::from(k), OsString::from(v)))
        })
        .collect();
    (!vars.is_empty()).then_some(vars)
}

fn parse_block(bytes: &[u8]) -> Option<BTreeMap<OsString, OsString>> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find(BEGIN)? + BEGIN.len();
    let end = text[start..].find(END)? + start;
    let block = &text[start..end];
    if block.contains('\0') {
        return parse_nul(block);
    }
    // Fallback for an `env` without `-0`; line-based and therefore ambiguous.
    let mut vars: BTreeMap<OsString, OsString> = BTreeMap::new();
    let mut last_key: Option<String> = None;
    for line in block.lines() {
        let is_assignment = line
            .split_once('=')
            .map(|(k, _)| {
                !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !k.chars().next().unwrap().is_ascii_digit()
            })
            .unwrap_or(false);
        if is_assignment {
            let (k, v) = line.split_once('=').unwrap();
            vars.insert(OsString::from(k), OsString::from(v));
            last_key = Some(k.to_string());
        } else if let Some(k) = &last_key {
            // continuation of a multi-line value
            if let Some(v) = vars.get_mut(OsStr::new(k)) {
                let mut joined = v.clone();
                joined.push("\n");
                joined.push(line);
                *v = joined;
            }
        }
    }
    if vars.is_empty() {
        None
    } else {
        Some(vars)
    }
}

fn probe(shell: &Path, flags: &[&str], timeout: Duration) -> Option<BTreeMap<OsString, OsString>> {
    // `env -0` first: NUL-delimited output cannot be mis-split. An `env` that
    // does not support it exits non-zero before printing, so the plain `env`
    // fallback still produces a parseable block.
    let script = format!(
        "printf '\\n{BEGIN}\\n'; command env -0 2>/dev/null || command env; printf '\\n{END}\\n'"
    );
    let child = Command::new(shell)
        .args(flags)
        .arg(&script)
        .env("TERM", "dumb")
        .env("HWS_ENV_PROBE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    parse_block(&read_with_timeout(child, timeout)?)
}

fn current_env() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().collect()
}

fn split_path(value: &OsStr) -> Vec<PathBuf> {
    std::env::split_paths(value).collect()
}

impl ResolvedEnv {
    pub fn resolve() -> ResolvedEnv {
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|s| s.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        ResolvedEnv::resolve_with(&shell, Duration::from_secs(5))
    }

    pub fn resolve_with(shell: &Path, timeout: Duration) -> ResolvedEnv {
        let (vars, origin) = if let Some(v) = probe(shell, &["-l", "-i", "-c"], timeout) {
            (v, EnvOrigin::Interactive)
        } else if let Some(v) = probe(shell, &["-l", "-c"], timeout) {
            (v, EnvOrigin::Login)
        } else {
            (current_env(), EnvOrigin::Current)
        };
        let mut env = ResolvedEnv {
            vars,
            origin,
            shell: shell.to_path_buf(),
        };
        env.finalize();
        env
    }

    fn finalize(&mut self) {
        for key in DROP {
            self.vars.remove(OsStr::new(key));
        }
        // Keep the herdr plugin context of *this* process, whatever the shell said.
        for (k, v) in current_env() {
            let key = k.to_string_lossy();
            if key.starts_with("HERDR_") || key == "HOME" || key == "USER" || key == "SHELL" {
                self.vars.insert(k, v);
            }
        }
        let term = std::env::var_os("TERM")
            .filter(|t| !t.is_empty() && t != "dumb")
            .unwrap_or_else(|| OsString::from("xterm-256color"));
        self.vars.insert(OsString::from("TERM"), term);

        let mut path = self
            .vars
            .get(OsStr::new("PATH"))
            .map(|p| split_path(p))
            .unwrap_or_default();
        let mut prepend: Vec<PathBuf> = Vec::new();
        for dir in known_tool_dirs() {
            if !path.contains(&dir) && !prepend.contains(&dir) {
                prepend.push(dir);
            }
        }
        // Prepended dirs go *after* the shell's own PATH so user preferences win;
        // they only fill gaps.
        path.extend(prepend);
        for fallback in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            let p = PathBuf::from(fallback);
            if !path.contains(&p) {
                path.push(p);
            }
        }
        if let Ok(joined) = std::env::join_paths(&path) {
            self.vars.insert(OsString::from("PATH"), joined);
        }
    }

    pub fn path_var(&self) -> OsString {
        self.vars
            .get(OsStr::new("PATH"))
            .cloned()
            .unwrap_or_default()
    }

    pub fn find_tool(&self, name: &str) -> Option<PathBuf> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        which::which_in(name, Some(self.path_var()), cwd).ok()
    }

    /// `git` from the resolved PATH, or a plain `git` and let exec fail loudly.
    pub fn git(&self) -> PathBuf {
        self.find_tool("git")
            .unwrap_or_else(|| PathBuf::from("git"))
    }

    pub fn to_vec(&self) -> Vec<(OsString, OsString)> {
        self.vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_records_cannot_forge_an_assignment() {
        let raw = format!("junk\n{BEGIN}\nA=1\0EVIL=x\nPATH=/attacker\0PATH=/real\0{END}\n");
        let vars = parse_block(raw.as_bytes()).unwrap();
        assert_eq!(vars.get(OsStr::new("A")).unwrap(), "1");
        assert_eq!(
            vars.get(OsStr::new("EVIL")).unwrap(),
            "x\nPATH=/attacker",
            "a newline inside a value stays inside it"
        );
        assert_eq!(vars.get(OsStr::new("PATH")).unwrap(), "/real");
    }

    #[test]
    fn a_real_shell_probe_uses_nul_records() {
        let env = ResolvedEnv::resolve_with(Path::new("/bin/sh"), Duration::from_secs(5));
        assert_ne!(env.origin, EnvOrigin::Current);
        // Prove the NUL path is the one taken on this platform.
        let script = format!(
            "printf '\\n{BEGIN}\\n'; command env -0 2>/dev/null || command env; printf '\\n{END}\\n'"
        );
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            out.stdout.contains(&0u8),
            "`env -0` unavailable here; the line-based fallback is in use"
        );
    }

    #[test]
    fn parses_marked_block_with_multiline_values() {
        let raw =
            format!("junk\n{BEGIN}\nA=1\nMULTI=first\nsecond line\nPATH=/x:/y\n{END}\ntrailer");
        let vars = parse_block(raw.as_bytes()).unwrap();
        assert_eq!(vars.get(OsStr::new("A")).unwrap(), "1");
        assert_eq!(vars.get(OsStr::new("MULTI")).unwrap(), "first\nsecond line");
        assert_eq!(vars.get(OsStr::new("PATH")).unwrap(), "/x:/y");
        assert!(parse_block(b"no markers").is_none());
    }

    #[test]
    fn resolves_via_sh_and_keeps_herdr_vars() {
        std::env::set_var("HERDR_TEST_MARKER", "yes");
        let env = ResolvedEnv::resolve_with(Path::new("/bin/sh"), Duration::from_secs(5));
        assert_ne!(env.origin, EnvOrigin::Current, "sh -l -i -c should work");
        assert_eq!(
            env.vars.get(OsStr::new("HERDR_TEST_MARKER")).unwrap(),
            "yes"
        );
        assert!(!env.vars.contains_key(OsStr::new("HWS_ENV_PROBE")));
        let path = env.path_var().to_string_lossy().into_owned();
        assert!(path.contains("/usr/bin"));
        assert!(env.find_tool("sh").is_some());
        assert!(env.git().ends_with("git"));
    }

    #[test]
    fn falls_back_to_current_env_for_a_bogus_shell() {
        let env =
            ResolvedEnv::resolve_with(Path::new("/nonexistent/shell"), Duration::from_secs(1));
        assert_eq!(env.origin, EnvOrigin::Current);
        assert!(env.vars.contains_key(OsStr::new("PATH")));
    }
}
