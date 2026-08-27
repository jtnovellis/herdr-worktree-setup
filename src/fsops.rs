//! Bringing files across: copy-on-write wherever the filesystem allows it.
//!
//! Ladder for a directory: APFS `clonefile(2)` of the whole tree (macOS) →
//! per-file reflink walk (btrfs/xfs/APFS) → size-capped byte copy. Symlinks are
//! recreated as symlinks; absolute links into the source checkout are rewritten
//! to point into the target. Nothing that already exists in the target is touched.

use crate::config::{CloneMode, Config};
use crate::planner::{PlanAction, PlanItem};
use crate::rules::Rules;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    ApfsClone,
    Reflink,
    Copy,
    Symlink,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::ApfsClone => "APFS clone",
            Method::Reflink => "reflink",
            Method::Copy => "copy",
            Method::Symlink => "symlink",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done {
        method: Method,
        files: u64,
        bytes: u64,
    },
    Exists,
    TooLarge {
        bytes: u64,
    },
    Failed(String),
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub struct Copier {
    source: PathBuf,
    target: PathBuf,
    source_prefixes: Vec<PathBuf>,
    mode: CloneMode,
    per_dir_cap: u64,
    total_cap: u64,
    plain_copied: u64,
    rules: Rules,
}

#[cfg(target_os = "macos")]
mod apfs {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;

    extern "C" {
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32)
            -> libc::c_int;
    }

    /// Clone a whole directory hierarchy (or a file) atomically on APFS.
    pub fn clone(src: &Path, dst: &Path) -> io::Result<()> {
        let src = CString::new(src.as_os_str().as_bytes())?;
        let dst = CString::new(dst.as_os_str().as_bytes())?;
        let rc = unsafe {
            clonefile(
                src.as_ptr(),
                dst.as_ptr(),
                CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn is_unsupported(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices | io::ErrorKind::InvalidInput
    ) || err
        .raw_os_error()
        // ENOTSUP and EOPNOTSUPP are the same value on Linux, distinct on macOS.
        .is_some_and(|code| {
            [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EXDEV, libc::EINVAL].contains(&code)
        })
}

impl Copier {
    pub fn new(source: &Path, target: &Path, cfg: &Config, rules: Rules) -> Copier {
        let mut source_prefixes = vec![source.to_path_buf()];
        if let Ok(canon) = source.canonicalize() {
            if canon != source {
                source_prefixes.push(canon);
            }
        }
        Copier {
            source: source.to_path_buf(),
            target: target.to_path_buf(),
            source_prefixes,
            mode: cfg.mode,
            per_dir_cap: cfg.copy_size_cap_bytes(),
            total_cap: cfg.total_size_cap_bytes(),
            plain_copied: 0,
            rules,
        }
    }

    pub fn apply(&mut self, item: &PlanItem) -> Outcome {
        let Some(action) = item.action else {
            return Outcome::Failed("no action".into());
        };
        let src = self.source.join(&item.rel);
        let dst = self.target.join(&item.rel);
        if std::fs::symlink_metadata(&dst).is_ok() {
            return Outcome::Exists;
        }
        if let Err(err) = ensure_parent(&dst) {
            return Outcome::Failed(err.to_string());
        }
        match action {
            PlanAction::Symlink => self.make_symlink(&src, &dst),
            _ if item.is_symlink => self.recreate_symlink(&src, &dst),
            PlanAction::Copy if !item.is_dir => self.copy_file(&src, &dst),
            PlanAction::Copy | PlanAction::Clone => self.clone_tree(&item.rel, &src, &dst),
        }
    }

    fn make_symlink(&self, src: &Path, dst: &Path) -> Outcome {
        match std::os::unix::fs::symlink(src, dst) {
            Ok(()) => Outcome::Done {
                method: Method::Symlink,
                files: 1,
                bytes: 0,
            },
            Err(err) => Outcome::Failed(format!("symlink: {err}")),
        }
    }

    fn rewrite_link_target(&self, link_target: &Path) -> PathBuf {
        if link_target.is_absolute() {
            for prefix in &self.source_prefixes {
                if let Ok(rest) = link_target.strip_prefix(prefix) {
                    return self.target.join(rest);
                }
            }
        }
        link_target.to_path_buf()
    }

    fn recreate_symlink(&self, src: &Path, dst: &Path) -> Outcome {
        let link_target = match std::fs::read_link(src) {
            Ok(t) => self.rewrite_link_target(&t),
            Err(err) => return Outcome::Failed(format!("read_link: {err}")),
        };
        match std::os::unix::fs::symlink(&link_target, dst) {
            Ok(()) => Outcome::Done {
                method: Method::Symlink,
                files: 1,
                bytes: 0,
            },
            Err(err) => Outcome::Failed(format!("symlink: {err}")),
        }
    }

    fn copy_file(&mut self, src: &Path, dst: &Path) -> Outcome {
        let bytes = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        match self.mode {
            CloneMode::Reflink => match reflink_copy::reflink_or_copy(src, dst) {
                Ok(None) => Outcome::Done {
                    method: Method::Reflink,
                    files: 1,
                    bytes,
                },
                Ok(Some(n)) => {
                    self.plain_copied += n;
                    Outcome::Done {
                        method: Method::Copy,
                        files: 1,
                        bytes: n,
                    }
                }
                Err(err) => Outcome::Failed(format!("copy: {err}")),
            },
            CloneMode::Copy => match std::fs::copy(src, dst) {
                Ok(n) => {
                    self.plain_copied += n;
                    Outcome::Done {
                        method: Method::Copy,
                        files: 1,
                        bytes: n,
                    }
                }
                Err(err) => Outcome::Failed(format!("copy: {err}")),
            },
        }
    }

    fn clone_tree(&mut self, rel: &str, src: &Path, dst: &Path) -> Outcome {
        #[cfg(target_os = "macos")]
        if self.mode == CloneMode::Reflink {
            match apfs::clone(src, dst) {
                Ok(()) => {
                    self.post_prune(rel, dst);
                    let files = self.sweep_symlinks(dst);
                    return Outcome::Done {
                        method: Method::ApfsClone,
                        files,
                        bytes: 0,
                    };
                }
                Err(err) if err.raw_os_error() == Some(libc::EEXIST) => return Outcome::Exists,
                Err(_) => {
                    // Partial state is impossible (clonefile is atomic) but be safe.
                    let _ = std::fs::remove_dir_all(dst);
                }
            }
        }
        match self.walk_copy(rel, src, dst) {
            Ok(outcome) => outcome,
            Err(err) => {
                let _ = std::fs::remove_dir_all(dst);
                Outcome::Failed(err.to_string())
            }
        }
    }

    /// Probe whether a reflink of one file into `dst`'s parent works.
    fn reflink_works(&self, probe_file: &Path, dst: &Path) -> bool {
        let parent = dst.parent().unwrap_or(&self.target);
        let tmp = parent.join(format!(".hws-probe-{}", std::process::id()));
        let ok = reflink_copy::reflink(probe_file, &tmp).is_ok();
        let _ = std::fs::remove_file(&tmp);
        ok
    }

    fn walk_copy(&mut self, rel: &str, src: &Path, dst: &Path) -> io::Result<Outcome> {
        // Pre-scan: size + a regular file to probe with.
        let mut size = 0u64;
        let mut probe: Option<PathBuf> = None;
        for entry in walkdir::WalkDir::new(src)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() {
                size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                if probe.is_none() {
                    probe = Some(entry.path().to_path_buf());
                }
            }
        }
        let reflink = self.mode == CloneMode::Reflink
            && probe
                .as_deref()
                .map(|p| self.reflink_works(p, dst))
                .unwrap_or(false);
        if !reflink
            && (size > self.per_dir_cap || self.plain_copied.saturating_add(size) > self.total_cap)
        {
            return Ok(Outcome::TooLarge { bytes: size });
        }

        let mut files = 0u64;
        let mut bytes = 0u64;
        let method = if reflink {
            Method::Reflink
        } else {
            Method::Copy
        };
        let mut walker = walkdir::WalkDir::new(src).follow_links(false).into_iter();
        while let Some(entry) = walker.next() {
            let entry = entry?;
            let sub = entry.path().strip_prefix(src).unwrap_or(entry.path());
            let out = dst.join(sub);
            let sub_rel = if sub.as_os_str().is_empty() {
                rel.to_string()
            } else {
                format!("{rel}/{}", sub.to_string_lossy())
            };
            let ft = entry.file_type();
            if entry.depth() > 0 && self.rules.should_prune(&sub_rel, ft.is_dir()) {
                if ft.is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if ft.is_dir() {
                std::fs::create_dir_all(&out)?;
                if let Ok(meta) = entry.metadata() {
                    let _ = std::fs::set_permissions(&out, meta.permissions());
                }
            } else if ft.is_symlink() {
                let link_target = std::fs::read_link(entry.path())?;
                std::os::unix::fs::symlink(self.rewrite_link_target(&link_target), &out)?;
            } else if ft.is_file() {
                let n = if reflink {
                    match reflink_copy::reflink(entry.path(), &out) {
                        Ok(()) => entry.metadata().map(|m| m.len()).unwrap_or(0),
                        Err(err) if !is_unsupported(&err) => return Err(err),
                        Err(_) => std::fs::copy(entry.path(), &out)?,
                    }
                } else {
                    let n = std::fs::copy(entry.path(), &out)?;
                    self.plain_copied += n;
                    n
                };
                files += 1;
                bytes += n;
            }
        }
        Ok(Outcome::Done {
            method,
            files,
            bytes,
        })
    }

    /// After a whole-tree clone, remove the few multi-component excludes
    /// (`.turbo/daemon`) that the atomic clone could not skip. Bounded depth.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn post_prune(&self, rel: &str, dst: &Path) {
        let mut walker = walkdir::WalkDir::new(dst)
            .follow_links(false)
            .min_depth(1)
            .max_depth(3)
            .into_iter();
        while let Some(Ok(entry)) = walker.next() {
            let sub = entry.path().strip_prefix(dst).unwrap_or(entry.path());
            let sub_rel = format!("{rel}/{}", sub.to_string_lossy());
            let is_dir = entry.file_type().is_dir();
            if self.rules.should_prune(&sub_rel, is_dir) {
                if is_dir {
                    let _ = std::fs::remove_dir_all(entry.path());
                    walker.skip_current_dir();
                } else {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    /// readdir-only sweep: count files, rewrite absolute symlinks into the source.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn sweep_symlinks(&self, dst: &Path) -> u64 {
        let mut files = 0u64;
        for entry in walkdir::WalkDir::new(dst)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            let ft = entry.file_type();
            if ft.is_file() {
                files += 1;
            } else if ft.is_symlink() {
                if let Ok(t) = std::fs::read_link(entry.path()) {
                    let rewritten = self.rewrite_link_target(&t);
                    if rewritten != t {
                        let _ = std::fs::remove_file(entry.path());
                        let _ = std::os::unix::fs::symlink(rewritten, entry.path());
                    }
                }
            }
        }
        files
    }
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{list_ignored, testrepo};
    use crate::planner::{CopyPlan, Group, ItemState};

    fn setup(mode: CloneMode) -> (tempfile::TempDir, PathBuf, PathBuf, CopyPlan, Copier) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let target = tmp.path().join("wt");
        std::fs::create_dir_all(&target).unwrap();
        // absolute symlink into the source checkout, inside a cache dir
        std::os::unix::fs::symlink(
            repo.join("node_modules/pkg"),
            repo.join("node_modules/linked"),
        )
        .unwrap();
        let cfg = Config {
            mode,
            ..Default::default()
        };
        let rules = Rules::from_config(&cfg).unwrap();
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let plan = CopyPlan::build(&repo, &target, cands, &rules);
        let copier = Copier::new(&repo, &target, &cfg, rules);
        (tmp, repo, target, plan, copier)
    }

    fn check_result(repo: &Path, target: &Path) {
        assert_eq!(
            std::fs::read_to_string(target.join(".env")).unwrap(),
            "SECRET=1\n"
        );
        assert!(target.join("node_modules/.bin/x").is_file());
        assert!(
            target.join("node_modules/pkg/dist/index.js").is_file(),
            "dist inside node_modules kept"
        );
        assert!(target
            .join("packages/ui/node_modules/dep/index.js")
            .is_file());
        assert!(target.join(".turbo/cache/a").is_file());
        assert!(
            !target.join(".turbo/daemon").exists(),
            "multi-component exclude pruned"
        );
        assert!(target.join(".next/cache/x").is_file());
        assert!(!target.join(".next/server").exists());
        assert!(!target.join("dist").exists());
        assert!(!target.join("server.log").exists());
        assert!(!target.join("vendor-repo").exists());
        assert!(!target.join("wip.txt").exists());
        let rel = std::fs::read_link(target.join("packages/ui/.env.development")).unwrap();
        assert_eq!(
            rel,
            PathBuf::from("../../.env"),
            "relative symlink kept verbatim"
        );
        let abs = std::fs::read_link(target.join("node_modules/linked")).unwrap();
        assert_eq!(
            abs,
            target.join("node_modules/pkg"),
            "absolute link rewritten into target"
        );
        assert!(
            repo.join("node_modules/linked").exists(),
            "source untouched"
        );
    }

    #[test]
    fn applies_full_plan_reflink_mode() {
        let (_tmp, repo, target, plan, mut copier) = setup(CloneMode::Reflink);
        for group in [Group::State, Group::Caches] {
            for item in plan.pending_in(group) {
                let out = copier.apply(item);
                assert!(matches!(out, Outcome::Done { .. }), "{}: {out:?}", item.rel);
            }
        }
        check_result(&repo, &target);
        // second run: everything exists, nothing overwritten
        std::fs::write(target.join(".env"), "CHANGED=1\n").unwrap();
        let env_item = plan.items.iter().find(|i| i.rel == ".env").unwrap();
        assert_eq!(copier.apply(env_item), Outcome::Exists);
        assert_eq!(
            std::fs::read_to_string(target.join(".env")).unwrap(),
            "CHANGED=1\n"
        );
    }

    #[test]
    fn applies_full_plan_copy_mode() {
        let (_tmp, repo, target, plan, mut copier) = setup(CloneMode::Copy);
        for group in [Group::State, Group::Caches] {
            for item in plan.pending_in(group) {
                let out = copier.apply(item);
                assert!(
                    matches!(
                        out,
                        Outcome::Done {
                            method: Method::Copy | Method::Symlink,
                            ..
                        }
                    ),
                    "{}: {out:?}",
                    item.rel
                );
            }
        }
        check_result(&repo, &target);
    }

    #[test]
    fn size_cap_skips_large_dirs_in_copy_mode() {
        let (_tmp, _repo, target, plan, mut copier) = setup(CloneMode::Copy);
        copier.per_dir_cap = 1; // one byte
        let nm = plan.items.iter().find(|i| i.rel == "node_modules").unwrap();
        assert!(matches!(copier.apply(nm), Outcome::TooLarge { .. }));
        assert!(
            !target.join("node_modules").exists(),
            "nothing partial left behind"
        );
        assert!(plan.items.iter().all(|i| i.rel != "wip.txt"));
        assert!(plan.items.iter().any(|i| i.state == ItemState::Pending));
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
