//! gitignore-flavoured pattern sets and the classification order
//! exclude → symlink → clone → copy.
//!
//! Pattern semantics (documented in the README): a trailing `/` matches
//! directories only; a leading `/` anchors at the checkout root; otherwise the
//! pattern matches at any depth. `*` never crosses a `/`.

use crate::config::Config;
use anyhow::{Context, Result};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Exclude,
    Symlink,
    Clone,
    Copy,
}

#[derive(Debug, Clone)]
struct PatternSet {
    set: GlobSet,
    dir_only: Vec<bool>,
}

/// `(glob, dir_only)` for a user pattern, or `None` for blank/comment lines.
pub fn normalize(pattern: &str) -> Option<(String, bool)> {
    let mut p = pattern.trim();
    if p.is_empty() || p.starts_with('#') {
        return None;
    }
    let dir_only = p.ends_with('/');
    p = p.trim_end_matches('/');
    if p.is_empty() {
        return None;
    }
    let glob = if let Some(anchored) = p.strip_prefix('/') {
        anchored.to_string()
    } else if p.starts_with("**/") {
        p.to_string()
    } else {
        format!("**/{p}")
    };
    Some((glob, dir_only))
}

fn build_glob(glob: &str) -> Result<Glob> {
    GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid pattern `{glob}`"))
}

impl PatternSet {
    fn build(patterns: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        let mut dir_only = Vec::new();
        for pattern in patterns {
            if let Some((glob, dir)) = normalize(pattern) {
                builder.add(build_glob(&glob)?);
                dir_only.push(dir);
            }
        }
        Ok(PatternSet {
            set: builder.build()?,
            dir_only,
        })
    }

    fn matches(&self, rel: &str, is_dir: bool) -> bool {
        self.set
            .matches(rel)
            .into_iter()
            .any(|i| is_dir || !self.dir_only[i])
    }
}

#[derive(Debug, Clone)]
pub struct Rules {
    exclude: PatternSet,
    symlink: PatternSet,
    clone: PatternSet,
    copy: PatternSet,
    /// Multi-component exclude patterns (`.turbo/daemon/`) — the only ones
    /// applied *inside* a cloned directory, so `dist/` never prunes a package's
    /// own `dist` folder under `node_modules`.
    prune: PatternSet,
}

impl Rules {
    pub fn from_config(cfg: &Config) -> Result<Rules> {
        Rules::new(&cfg.exclude, &cfg.symlink, &cfg.clone, &cfg.copy)
    }

    pub fn new(
        exclude: &[String],
        symlink: &[String],
        clone: &[String],
        copy: &[String],
    ) -> Result<Rules> {
        let prune: Vec<String> = exclude
            .iter()
            .filter(|p| {
                let core = p.trim().trim_start_matches('/').trim_end_matches('/');
                core.contains('/')
            })
            .cloned()
            .collect();
        Ok(Rules {
            exclude: PatternSet::build(exclude)?,
            symlink: PatternSet::build(symlink)?,
            clone: PatternSet::build(clone)?,
            copy: PatternSet::build(copy)?,
            prune: PatternSet::build(&prune)?,
        })
    }

    /// Classify a checkout-relative path (no leading `./`, no trailing `/`).
    pub fn classify(&self, rel: &str, is_dir: bool) -> Option<Action> {
        let rel = rel.trim_start_matches("./").trim_end_matches('/');
        if self.exclude.matches(rel, is_dir) {
            Some(Action::Exclude)
        } else if self.symlink.matches(rel, is_dir) {
            Some(Action::Symlink)
        } else if self.clone.matches(rel, is_dir) {
            Some(Action::Clone)
        } else if self.copy.matches(rel, is_dir) {
            Some(Action::Copy)
        } else {
            None
        }
    }

    /// Should this path *inside* a cloned tree be removed after cloning?
    pub fn should_prune(&self, rel: &str, is_dir: bool) -> bool {
        self.prune.matches(rel.trim_end_matches('/'), is_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules::from_config(&Config::default()).unwrap()
    }

    #[test]
    fn normalizes_like_gitignore() {
        assert_eq!(
            normalize("node_modules/"),
            Some(("**/node_modules".into(), true))
        );
        assert_eq!(normalize("/.env"), Some((".env".into(), false)));
        assert_eq!(normalize(".env.*"), Some(("**/.env.*".into(), false)));
        assert_eq!(normalize("  # comment"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn env_files_copy_but_examples_are_excluded() {
        let r = rules();
        assert_eq!(r.classify(".env", false), Some(Action::Copy));
        assert_eq!(r.classify(".env.dev", false), Some(Action::Copy));
        assert_eq!(r.classify("apps/web/.env.local", false), Some(Action::Copy));
        assert_eq!(r.classify(".env.example", false), Some(Action::Exclude));
        assert_eq!(r.classify(".env.dev.example", false), Some(Action::Exclude));
        assert_eq!(r.classify(".envrc", false), Some(Action::Copy));
    }

    #[test]
    fn caches_clone_at_any_depth_and_dir_only() {
        let r = rules();
        assert_eq!(r.classify("node_modules", true), Some(Action::Clone));
        assert_eq!(
            r.classify("packages/ui/node_modules", true),
            Some(Action::Clone)
        );
        assert_eq!(
            r.classify("node_modules", false),
            None,
            "file named node_modules"
        );
        assert_eq!(r.classify(".next/cache", true), Some(Action::Clone));
        assert_eq!(r.classify(".next", true), Some(Action::Exclude));
        assert_eq!(r.classify("dist", true), Some(Action::Exclude));
    }

    #[test]
    fn prune_only_applies_multi_component_excludes() {
        let r = rules();
        assert!(r.should_prune(".turbo/daemon", true));
        assert!(r.should_prune("apps/x/.turbo/daemon", true));
        assert!(!r.should_prune("node_modules/pkg/dist", true));
        assert!(!r.should_prune("node_modules/pkg/build", true));
    }

    #[test]
    fn explicit_symlink_wins_over_clone_but_not_exclude() {
        let r = Rules::new(
            &["big.bin".into()],
            &["datasets/".into(), "big.bin".into()],
            &["datasets/".into()],
            &[],
        )
        .unwrap();
        assert_eq!(r.classify("datasets", true), Some(Action::Symlink));
        assert_eq!(r.classify("big.bin", false), Some(Action::Exclude));
    }

    #[test]
    fn misc_defaults() {
        let r = rules();
        assert_eq!(
            r.classify(".claude/settings.local.json", false),
            Some(Action::Copy)
        );
        assert_eq!(
            r.classify("config/app.local.toml", false),
            Some(Action::Copy)
        );
        assert_eq!(r.classify(".vercel", true), Some(Action::Copy));
        assert_eq!(r.classify("server.log", false), Some(Action::Exclude));
        assert_eq!(r.classify(".direnv", true), Some(Action::Exclude));
        assert_eq!(r.classify("random.txt", false), None);
    }
}
