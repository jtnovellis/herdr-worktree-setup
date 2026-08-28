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
    /// Refused for safety — e.g. the destination path crosses a symlink, or the
    /// source is not a regular file. Never a transient error.
    Refused(String),
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

/// Join `rel` under `target` without ever traversing a symlink.
///
/// SECURITY: `target` is a freshly checked-out branch, and git stores a symlink
/// as an ordinary tracked file. `target.join(rel)` is string concatenation, and
/// every syscall that follows (`create_dir_all`, `clonefile`, `fs::copy`)
/// resolves intermediate components — so a committed `packages -> /elsewhere`
/// would redirect the whole copy, secrets included, outside the worktree.
/// Walking the components ourselves keeps every write inside it.
pub fn contained_dst(target: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut cur = target.to_path_buf();
    let mut parts = rel.split('/').filter(|p| !p.is_empty()).peekable();
    if parts.peek().is_none() {
        return Err("empty relative path".to_string());
    }
    while let Some(part) = parts.next() {
        if part == "." || part == ".." {
            return Err(format!("`{rel}` contains a `{part}` component"));
        }
        cur.push(part);
        // The last component may legitimately be a symlink — we then skip the
        // item as "already present". An intermediate one must never be followed.
        if parts.peek().is_some()
            && std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(format!(
                "{} is a symlink; refusing to write through it",
                cur.display()
            ));
        }
    }
    Ok(cur)
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
        let dst = match contained_dst(&self.target, &item.rel) {
            Ok(dst) => dst,
            Err(err) => return Outcome::Refused(err),
        };
        if std::fs::symlink_metadata(&dst).is_ok() {
            return Outcome::Exists;
        }
        if let Err(err) = self.ensure_parent(&item.rel) {
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
                    // A remainder containing `..` names a different file once
                    // re-rooted, so leave such a link exactly as it was.
                    if rest
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        return link_target.to_path_buf();
                    }
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

    /// `Some(TooLarge)` when materialising `bytes` real bytes would breach a
    /// cap. Reflinked/cloned bytes cost no disk and are never charged.
    fn cap_exceeded(&self, bytes: u64) -> Option<Outcome> {
        if bytes > self.per_dir_cap || self.plain_copied.saturating_add(bytes) > self.total_cap {
            Some(Outcome::TooLarge { bytes })
        } else {
            None
        }
    }

    fn copy_file(&mut self, src: &Path, dst: &Path) -> Outcome {
        let meta = match std::fs::metadata(src) {
            Ok(meta) => meta,
            Err(err) => return Outcome::Failed(format!("stat: {err}")),
        };
        // Anything that is not a regular file — a FIFO above all — would block
        // `open(2)` forever. `git ls-files` never reports one, but the
        // one-level descend into an excluded directory reads the filesystem.
        if !meta.is_file() {
            return Outcome::Refused("not a regular file".to_string());
        }
        let bytes = meta.len();
        if self.mode == CloneMode::Reflink {
            match reflink_copy::reflink(src, dst) {
                Ok(()) => {
                    return Outcome::Done {
                        method: Method::Reflink,
                        files: 1,
                        bytes,
                    }
                }
                Err(err) if !is_unsupported(&err) => {
                    return Outcome::Failed(format!("copy: {err}"))
                }
                Err(_) => {}
            }
        }
        if let Some(too_large) = self.cap_exceeded(bytes) {
            return too_large;
        }
        match std::fs::copy(src, dst) {
            Ok(n) => {
                self.plain_copied += n;
                Outcome::Done {
                    method: Method::Copy,
                    files: 1,
                    bytes: n,
                }
            }
            Err(err) => Outcome::Failed(format!("copy: {err}")),
        }
    }

    /// Create the parents of `rel` inside the target, mirroring each source
    /// directory's mode. Without that, a `drwx------` directory holding secrets
    /// is recreated as `drwxr-xr-x` by the umask.
    fn ensure_parent(&self, rel: &str) -> io::Result<()> {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let mut dir = self.target.clone();
        let mut src_dir = self.source.clone();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            dir.push(part);
            src_dir.push(part);
            if !dir.exists() {
                std::fs::create_dir(&dir)?;
                if let Ok(meta) = std::fs::metadata(&src_dir) {
                    let _ = std::fs::set_permissions(&dir, meta.permissions());
                }
            }
        }
        Ok(())
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
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp = parent.join(format!(".hws-probe-{}-{nonce:08x}", std::process::id()));
        // Unlink only when the reflink succeeded — i.e. only a file we created.
        match reflink_copy::reflink(probe_file, &tmp) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                true
            }
            Err(_) => false,
        }
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
                        Err(_) => {
                            // Reflink worked for the probe but not for this
                            // file: these are real bytes, so charge and cap them.
                            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            if self.cap_exceeded(size).is_some() {
                                return Ok(Outcome::TooLarge { bytes: size });
                            }
                            let n = std::fs::copy(entry.path(), &out)?;
                            self.plain_copied += n;
                            n
                        }
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
        // No depth bound: `exclude` is user- and repo-extensible, and an exclude
        // that silently stops applying below some depth is worse than none at
        // all. Pruned subtrees are skipped, so the walk stays cheap.
        let mut walker = walkdir::WalkDir::new(dst)
            .follow_links(false)
            .min_depth(1)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{list_ignored, testrepo};
    use crate::planner::{CopyPlan, Group, ItemState, PlanItem};

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

    /// A branch may commit a symlink at any path component. Following it would
    /// place the copy — secrets included — wherever the branch chooses.
    #[test]
    fn refuses_to_write_through_a_symlinked_component() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let target = tmp.path().join("wt");
        let outside = tmp.path().join("OUTSIDE");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // Source has gitignored state under packages/ui/.
        testrepo::write(&repo, "packages/ui/.env.local", "STRIPE=sk_live_x\n");
        testrepo::write(&repo, "packages/ui/node_modules/dep/i.js", "x\n");
        // The branch checked out `packages` as a symlink pointing elsewhere.
        std::os::unix::fs::symlink(&outside, target.join("packages")).unwrap();

        let cfg = Config::default();
        let rules = Rules::from_config(&cfg).unwrap();
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let plan = CopyPlan::build(&repo, &target, cands, &rules);
        let mut copier = Copier::new(&repo, &target, &cfg, rules);

        let blocked: Vec<&str> = plan
            .items
            .iter()
            .filter(|i| matches!(i.state, ItemState::Blocked(_)))
            .map(|i| i.rel.as_str())
            .collect();
        assert!(
            blocked.contains(&"packages/ui/.env.local"),
            "plan did not mark the escape: {blocked:?}"
        );
        assert!(blocked.contains(&"packages/ui/node_modules"));

        // Even applied directly, the copier refuses.
        for rel in ["packages/ui/.env.local", "packages/ui/node_modules"] {
            let item = plan.items.iter().find(|i| i.rel == rel).unwrap();
            match copier.apply(item) {
                Outcome::Refused(reason) => assert!(reason.contains("symlink"), "{reason}"),
                other => panic!("{rel} was not refused: {other:?}"),
            }
        }
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "data landed outside the worktree"
        );
        // Everything the plan can reach safely still copies.
        for item in plan.pending_in(Group::State) {
            assert!(
                matches!(copier.apply(item), Outcome::Done { .. }),
                "{}",
                item.rel
            );
        }
        assert!(target.join(".env").is_file());
    }

    /// `git ls-files` never reports a FIFO, but the one-level descend into an
    /// excluded directory reads the filesystem — and `open`ing a FIFO blocks
    /// forever, which would wedge the pane with no way out.
    #[test]
    fn refuses_a_fifo_instead_of_blocking_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let target = tmp.path().join("wt");
        std::fs::create_dir_all(&target).unwrap();
        let fifo = repo.join("dist/.env.local");
        std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);

        let cfg = Config::default();
        let rules = Rules::from_config(&cfg).unwrap();
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let plan = CopyPlan::build(&repo, &target, cands, &rules);
        // The planner drops it outright…
        assert!(
            plan.items.iter().all(|i| i.rel != "dist/.env.local"),
            "a FIFO was planned"
        );
        // …and the copier would refuse it even if something else planned it.
        let mut copier = Copier::new(&repo, &target, &cfg, rules);
        let item = PlanItem {
            rel: "dist/.env.local".into(),
            is_dir: false,
            is_symlink: false,
            action: Some(crate::planner::PlanAction::Copy),
            state: ItemState::Pending,
        };
        assert!(matches!(copier.apply(&item), Outcome::Refused(_)));
    }

    /// `exclude` is the only "never copy this" control there is; it must hold
    /// at every depth, including under the whole-tree clone fast path.
    #[test]
    fn exclude_applies_at_any_depth_under_a_cloned_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let target = tmp.path().join("wt");
        std::fs::create_dir_all(&target).unwrap();
        testrepo::write(&repo, "node_modules/a/b/c/d/.turbo/daemon/pid", "1\n");
        testrepo::write(&repo, "node_modules/a/b/c/d/keep.js", "x\n");

        let cfg = Config::default();
        let rules = Rules::from_config(&cfg).unwrap();
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let plan = CopyPlan::build(&repo, &target, cands, &rules);
        let mut copier = Copier::new(&repo, &target, &cfg, rules);
        let nm = plan.items.iter().find(|i| i.rel == "node_modules").unwrap();
        assert!(matches!(copier.apply(nm), Outcome::Done { .. }));
        assert!(target.join("node_modules/a/b/c/d/keep.js").is_file());
        assert!(
            !target.join("node_modules/a/b/c/d/.turbo/daemon").exists(),
            "a deep exclude was not applied"
        );
    }

    /// A directory holding secrets must not be recreated world-readable.
    #[test]
    fn created_parent_directories_keep_the_source_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let repo = testrepo::fixture(tmp.path());
        let target = tmp.path().join("wt");
        std::fs::create_dir_all(&target).unwrap();
        testrepo::write(&repo, "secrets/deep/.env.local", "K=v\n");
        std::fs::set_permissions(repo.join("secrets"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(
            repo.join("secrets/deep"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        testrepo::git(&repo, &["check-ignore", "-q", "secrets/deep/.env.local"]);

        let cfg = Config::default();
        let rules = Rules::from_config(&cfg).unwrap();
        let cands = list_ignored(Path::new("git"), &repo).unwrap();
        let plan = CopyPlan::build(&repo, &target, cands, &rules);
        let mut copier = Copier::new(&repo, &target, &cfg, rules);
        let item = plan
            .items
            .iter()
            .find(|i| i.rel == "secrets/deep/.env.local")
            .expect("planned");
        assert!(matches!(copier.apply(item), Outcome::Done { .. }));
        for rel in ["secrets", "secrets/deep"] {
            let mode = std::fs::metadata(target.join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{rel} was widened to {mode:o}");
        }
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
