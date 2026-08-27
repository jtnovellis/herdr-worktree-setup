//! Find what the source checkout has that git does not track: the ignored
//! files and (collapsed) ignored directories. Only these are ever copied.

use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Checkout-relative path, no trailing slash.
    pub rel: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// The directory is itself a git repository (has `.git`); never copied.
    pub nested_repo: bool,
}

fn run_git(git: &Path, source: &Path, args: &[&str], pathspecs: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new(git);
    cmd.arg("-C").arg(source).args(args);
    if !pathspecs.is_empty() {
        cmd.arg("--").args(pathspecs);
    }
    let out = cmd
        .output()
        .with_context(|| format!("running {} in {}", git.display(), source.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(
            "git {} failed in {}: {}",
            args.join(" "),
            source.display(),
            if stderr.is_empty() {
                "unknown error".to_string()
            } else {
                stderr
            }
        ));
    }
    Ok(out.stdout)
}

fn split_nul(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
}

/// Ignored, untracked entries of `source`. Ignored directories are collapsed to
/// one entry (git does not descend into them), so this is O(ignored roots).
pub fn list_ignored(git: &Path, source: &Path) -> Result<Vec<Candidate>> {
    let raw = run_git(
        git,
        source,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "--full-name",
            "-z",
        ],
        &[],
    )?;
    let mut candidates = Vec::new();
    for entry in split_nul(&raw) {
        let is_dir_entry = entry.ends_with('/');
        let rel = entry.trim_end_matches('/').to_string();
        if rel.is_empty() || rel == ".git" || rel.starts_with(".git/") {
            continue;
        }
        let abs = source.join(&rel);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(meta) => meta,
            Err(_) => continue, // vanished between listing and now
        };
        let is_symlink = meta.file_type().is_symlink();
        let is_dir = is_dir_entry || (!is_symlink && meta.is_dir());
        let nested_repo = is_dir && !is_symlink && abs.join(".git").exists();
        candidates.push(Candidate {
            rel,
            is_dir,
            is_symlink,
            nested_repo,
        });
    }

    // Defence in depth: `--directory` only lists a directory when nothing under
    // it is tracked, but make that explicit — drop any dir with tracked content.
    let dirs: Vec<&str> = candidates
        .iter()
        .filter(|c| c.is_dir && !c.is_symlink)
        .map(|c| c.rel.as_str())
        .collect();
    if !dirs.is_empty() {
        let tracked = run_git(git, source, &["ls-files", "-z", "--full-name"], &dirs)?;
        let tracked_dirs: HashSet<String> = split_nul(&tracked)
            .filter_map(|file| {
                dirs.iter()
                    .find(|d| file.starts_with(&format!("{d}/")))
                    .map(|d| d.to_string())
            })
            .collect();
        candidates.retain(|c| !(c.is_dir && tracked_dirs.contains(&c.rel)));
    }
    Ok(candidates)
}

#[cfg(test)]
pub(crate) mod testrepo {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    pub fn write(repo: &Path, rel: &str, content: &str) {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A committed repo with a realistic mix of tracked, ignored and untracked state.
    pub fn fixture(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.invalid"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        write(
            &repo,
            ".gitignore",
            "node_modules/\n.env*\n!.env.example\n.turbo/\ndist/\n.next/\n*.log\nvendor-repo/\n.vercel/\n",
        );
        write(&repo, "package.json", "{\"name\":\"x\"}\n");
        write(&repo, ".env.example", "X=1\n");
        write(&repo, "packages/ui/package.json", "{}\n");
        write(&repo, "src/main.ts", "export {}\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        // ignored state
        write(&repo, ".env", "SECRET=1\n");
        write(&repo, ".env.local", "PORT=3000\n");
        write(&repo, "node_modules/.bin/x", "#!/bin/sh\n");
        write(&repo, "node_modules/pkg/dist/index.js", "x\n");
        write(&repo, "node_modules/pkg/package.json", "{}\n");
        write(&repo, "packages/ui/node_modules/dep/index.js", "x\n");
        write(&repo, ".turbo/daemon/pid", "1\n");
        write(&repo, ".turbo/cache/a", "a\n");
        write(&repo, "dist/bundle.js", "x\n");
        write(&repo, ".next/cache/x", "x\n");
        write(&repo, ".next/server/y", "y\n");
        write(&repo, "server.log", "log\n");
        write(&repo, ".vercel/project.json", "{}\n");
        // untracked, NOT ignored — must never be copied
        write(&repo, "wip.txt", "wip\n");
        // ignored nested repo
        write(&repo, "vendor-repo/README", "x\n");
        git(&repo.join("vendor-repo"), &["init", "-q"]);
        // relative symlink among ignored files
        std::os::unix::fs::symlink("../../.env", repo.join("packages/ui/.env.development"))
            .unwrap();
        repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_ignored_entries_collapsed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let rels: Vec<&str> = cands.iter().map(|c| c.rel.as_str()).collect();
        for expected in [
            ".env",
            ".env.local",
            "node_modules",
            "packages/ui/node_modules",
            ".turbo",
            "dist",
            ".next",
            "server.log",
            ".vercel",
            "vendor-repo",
            "packages/ui/.env.development",
        ] {
            assert!(rels.contains(&expected), "missing {expected} in {rels:?}");
        }
        assert!(
            !rels.contains(&"wip.txt"),
            "untracked-but-not-ignored must not appear"
        );
        assert!(
            !rels.contains(&".env.example"),
            "tracked file must not appear"
        );
        assert!(
            !rels.iter().any(|r| r.starts_with("node_modules/")),
            "dirs are collapsed"
        );
        let nm = cands.iter().find(|c| c.rel == "node_modules").unwrap();
        assert!(nm.is_dir && !nm.nested_repo);
        let vendored = cands.iter().find(|c| c.rel == "vendor-repo").unwrap();
        assert!(vendored.nested_repo);
        let link = cands
            .iter()
            .find(|c| c.rel == "packages/ui/.env.development")
            .unwrap();
        assert!(link.is_symlink && !link.is_dir);
    }
}
