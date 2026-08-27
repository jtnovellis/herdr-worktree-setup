//! Layered configuration: built-in defaults ← user config dir ← repo
//! `.herdr-worktree.toml` (target) ← `.herdr-worktree.local.toml` (source).
//! Everything is optional; the defaults alone give a working plugin.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Split,
    Tab,
    Overlay,
    Zoomed,
}

impl Placement {
    pub fn as_str(self) -> &'static str {
        match self {
            Placement::Split => "split",
            Placement::Tab => "tab",
            Placement::Overlay => "overlay",
            Placement::Zoomed => "zoomed",
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Down,
    Right,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Down => "down",
            Direction::Right => "right",
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CloneMode {
    /// Try clonefile / reflink first, fall back to a byte copy (size-capped).
    Reflink,
    /// Always byte-copy (size-capped).
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    Default,
    User,
    Repo,
    RepoLocal,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Default => "defaults",
            Origin::User => "user config",
            Origin::Repo => ".herdr-worktree.toml",
            Origin::RepoLocal => ".herdr-worktree.local.toml",
        }
    }
    pub fn is_repo(self) -> bool {
        matches!(self, Origin::Repo | Origin::RepoLocal)
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StepDef {
    pub name: String,
    pub run: String,
    /// Path (relative to the worktree) that must exist for the step to run.
    #[serde(default, rename = "if")]
    pub if_path: Option<String>,
    #[serde(default)]
    pub continue_on_error: bool,
    #[serde(skip)]
    pub origin: Origin,
}

/// One config file, every field optional.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct ConfigLayer {
    pub auto_close_secs: Option<u64>,
    pub focus: Option<bool>,
    pub placement: Option<Placement>,
    pub direction: Option<Direction>,
    pub install: Option<bool>,
    pub mode: Option<CloneMode>,
    pub copy_size_cap_mb: Option<u64>,
    pub total_size_cap_mb: Option<u64>,
    pub color: Option<bool>,
    pub mise_trust: Option<bool>,
    pub direnv_allow: Option<bool>,
    pub use_mise: Option<bool>,
    pub use_direnv: Option<bool>,
    pub trust_repo_steps: Option<bool>,
    pub copy: Vec<String>,
    pub clone: Vec<String>,
    pub symlink: Vec<String>,
    pub exclude: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub steps: Vec<StepDef>,
}

/// Fully resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub auto_close_secs: u64,
    pub focus: bool,
    pub placement: Placement,
    pub direction: Direction,
    pub install: bool,
    pub mode: CloneMode,
    pub copy_size_cap_mb: u64,
    pub total_size_cap_mb: u64,
    pub color: bool,
    pub mise_trust: bool,
    pub direnv_allow: bool,
    pub use_mise: bool,
    pub use_direnv: bool,
    pub trust_repo_steps: bool,
    pub copy: Vec<String>,
    pub clone: Vec<String>,
    pub symlink: Vec<String>,
    pub exclude: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub steps: Vec<StepDef>,
    /// Config files that were actually read, in merge order.
    pub layers: Vec<(Origin, PathBuf)>,
    pub warnings: Vec<String>,
}

pub const DEFAULT_COPY: &[&str] = &[
    ".env",
    ".env.*",
    ".envrc",
    ".envrc.local",
    ".dev.vars",
    ".flaskenv",
    ".mise.local.toml",
    "mise.local.toml",
    ".tool-versions",
    ".npmrc",
    "*.local.json",
    "*.local.toml",
    "*.local.yaml",
    "*.local.yml",
    "docker-compose.override.yml",
    "docker-compose.override.yaml",
    ".vercel/",
    ".wrangler/",
    ".claude/settings.local.json",
    ".vscode/",
    ".idea/",
    "local.properties",
    ".herdr-worktree.local.toml",
];

pub const DEFAULT_CLONE: &[&str] = &[
    "node_modules/",
    ".venv/",
    "venv/",
    "target/",
    ".next/cache/",
    ".turbo/",
    ".cache/",
    ".parcel-cache/",
    ".yarn/cache/",
    ".gradle/",
    "vendor/bundle/",
    "vendor/",
    "_build/",
    "deps/",
    ".dart_tool/",
    "Pods/",
    ".build/",
    ".zig-cache/",
    ".mypy_cache/",
    ".ruff_cache/",
];

pub const DEFAULT_EXCLUDE: &[&str] = &[
    ".git/",
    ".direnv/",
    ".turbo/daemon/",
    "dist/",
    "build/",
    "out/",
    "coverage/",
    "tmp/",
    ".next/",
    ".env.example",
    ".env.sample",
    ".env.template",
    ".env.dist",
    ".env.*.example",
    ".env.*.sample",
    "*.log",
    "*.pid",
    "*.sock",
    ".DS_Store",
];

impl Default for Config {
    fn default() -> Self {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Config {
            auto_close_secs: 5,
            focus: true,
            placement: Placement::Split,
            direction: Direction::Down,
            install: true,
            mode: CloneMode::Reflink,
            copy_size_cap_mb: 2048,
            total_size_cap_mb: 8192,
            color: true,
            mise_trust: true,
            direnv_allow: true,
            use_mise: true,
            use_direnv: true,
            trust_repo_steps: true,
            copy: owned(DEFAULT_COPY),
            clone: owned(DEFAULT_CLONE),
            symlink: Vec::new(),
            exclude: owned(DEFAULT_EXCLUDE),
            env: BTreeMap::new(),
            steps: Vec::new(),
            layers: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

pub const USER_FILE: &str = "config.toml";
pub const REPO_FILE: &str = ".herdr-worktree.toml";
pub const REPO_LOCAL_FILE: &str = ".herdr-worktree.local.toml";

fn read_layer(path: &Path) -> Result<Option<ConfigLayer>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let layer: ConfigLayer =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(layer))
}

fn extend_unique(into: &mut Vec<String>, from: Vec<String>) {
    for item in from {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

impl Config {
    /// Full merge for a run against `target` (its `.herdr-worktree.toml`) with
    /// the ignored `.local` layer read from `source`, where it actually lives.
    pub fn load(config_dir: &Path, source: &Path, target: &Path) -> Result<Config> {
        let mut cfg = Config::default();
        cfg.merge_file(Origin::User, &config_dir.join(USER_FILE))?;
        cfg.merge_file(Origin::Repo, &target.join(REPO_FILE))?;
        // The local layer may already have been copied into the target by an
        // earlier run; prefer the target copy, else the source original.
        let local_target = target.join(REPO_LOCAL_FILE);
        if local_target.is_file() {
            cfg.merge_file(Origin::RepoLocal, &local_target)?;
        } else {
            cfg.merge_file(Origin::RepoLocal, &source.join(REPO_LOCAL_FILE))?;
        }
        Ok(cfg)
    }

    /// User layer only — enough for the hook (placement/focus) before a target exists.
    pub fn load_user(config_dir: &Path) -> Result<Config> {
        let mut cfg = Config::default();
        cfg.merge_file(Origin::User, &config_dir.join(USER_FILE))?;
        Ok(cfg)
    }

    fn merge_file(&mut self, origin: Origin, path: &Path) -> Result<()> {
        if let Some(layer) = read_layer(path)? {
            self.layers.push((origin, path.to_path_buf()));
            self.merge(layer, origin);
        }
        Ok(())
    }

    pub fn merge(&mut self, layer: ConfigLayer, origin: Origin) {
        let mut host_only = |name: &str| {
            self.warnings.push(format!(
                "{}: `{name}` can only be set in the user config; ignored",
                origin.label()
            ));
        };
        macro_rules! scalar {
            ($field:ident) => {
                if let Some(v) = layer.$field {
                    self.$field = v;
                }
            };
        }
        macro_rules! host_scalar {
            ($field:ident) => {
                if let Some(v) = layer.$field {
                    if origin.is_repo() {
                        host_only(stringify!($field));
                    } else {
                        self.$field = v;
                    }
                }
            };
        }
        host_scalar!(focus);
        host_scalar!(placement);
        host_scalar!(direction);
        host_scalar!(trust_repo_steps);
        host_scalar!(color);
        scalar!(auto_close_secs);
        scalar!(install);
        scalar!(mode);
        scalar!(copy_size_cap_mb);
        scalar!(total_size_cap_mb);
        scalar!(mise_trust);
        scalar!(direnv_allow);
        scalar!(use_mise);
        scalar!(use_direnv);
        extend_unique(&mut self.copy, layer.copy);
        extend_unique(&mut self.clone, layer.clone);
        extend_unique(&mut self.symlink, layer.symlink);
        extend_unique(&mut self.exclude, layer.exclude);
        self.env.extend(layer.env);
        for mut step in layer.steps {
            step.origin = origin;
            self.steps.push(step);
        }
    }

    pub fn copy_size_cap_bytes(&self) -> u64 {
        self.copy_size_cap_mb.saturating_mul(1024 * 1024)
    }

    pub fn total_size_cap_bytes(&self) -> u64 {
        self.total_size_cap_mb.saturating_mul(1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.auto_close_secs, 5);
        assert!(cfg.focus && cfg.install);
        assert_eq!(cfg.placement, Placement::Split);
        assert!(cfg.copy.iter().any(|p| p == ".env.*"));
        assert!(cfg.clone.iter().any(|p| p == "node_modules/"));
        assert!(cfg.exclude.iter().any(|p| p == ".env.*.example"));
    }

    #[test]
    fn layers_merge_in_order_and_lists_extend() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_dir = dir.path().join("cfg");
        let source = dir.path().join("src");
        let target = dir.path().join("wt");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            cfg_dir.join(USER_FILE),
            "auto_close_secs = 9\nfocus = false\ncopy = ['.secrets/']\n",
        )
        .unwrap();
        std::fs::write(
            target.join(REPO_FILE),
            "auto_close_secs = 0\nfocus = true\ninstall = false\nclone = ['.pio/']\n[[steps]]\nname = 'migrate'\nrun = 'pnpm db:migrate'\nif = 'prisma/schema.prisma'\n",
        )
        .unwrap();
        std::fs::write(
            source.join(REPO_LOCAL_FILE),
            "install = true\n[env]\nFOO = 'bar'\n",
        )
        .unwrap();

        let cfg = Config::load(&cfg_dir, &source, &target).unwrap();
        assert_eq!(cfg.auto_close_secs, 0, "repo layer overrides user");
        assert!(!cfg.focus, "repo layer may not set focus");
        assert_eq!(cfg.warnings.len(), 1);
        assert!(cfg.install, "local layer overrides repo");
        assert!(cfg.copy.iter().any(|p| p == ".secrets/"));
        assert!(cfg.copy.iter().any(|p| p == ".env"), "defaults kept");
        assert!(cfg.clone.iter().any(|p| p == ".pio/"));
        assert_eq!(cfg.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(cfg.steps.len(), 1);
        assert_eq!(cfg.steps[0].origin, Origin::Repo);
        assert_eq!(
            cfg.steps[0].if_path.as_deref(),
            Some("prisma/schema.prisma")
        );
        assert_eq!(cfg.layers.len(), 3);
    }

    #[test]
    fn unknown_keys_are_errors_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(USER_FILE), "bogus = 1\n").unwrap();
        let err = Config::load_user(dir.path()).unwrap_err();
        assert!(err.to_string().contains(USER_FILE));
    }
}
