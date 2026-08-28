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
    pub step_timeout_secs: Option<u64>,
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
    pub step_timeout_secs: u64,
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
            step_timeout_secs: 1800,
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

/// Environment variables a repo-authored config may not set. These decide
/// *which* program runs (`PATH`), or inject attacker code into one that does
/// (the loader and interpreter hooks). A repo may still set ordinary build
/// variables like `NODE_ENV` or `RUST_LOG`.
fn repo_env_allowed(name: &str) -> bool {
    const DENY: &[&str] = &[
        "PATH",
        "SHELL",
        "IFS",
        "HOME",
        "ENV",
        "BASH_ENV",
        "NODE_OPTIONS",
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "PERL5OPT",
        "PERL5LIB",
        "RUBYOPT",
        "RUBYLIB",
        "GEM_HOME",
        "GEM_PATH",
        "PAGER",
        "EDITOR",
        "VISUAL",
    ];
    const DENY_PREFIX: &[&str] = &["LD_", "DYLD_", "GIT_SSH", "GIT_EXTERNAL_", "GIT_PROXY"];
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    !DENY.contains(&upper.as_str()) && !DENY_PREFIX.iter().any(|p| upper.starts_with(p))
}

fn extend_unique(into: &mut Vec<String>, from: Vec<String>) {
    for item in from {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

impl Config {
    /// Full merge for a run.
    ///
    /// SECURITY: both repo layers are read from the **source checkout** — the
    /// working copy the user already has — and never from `target`. The target
    /// is a freshly checked-out branch whose tracked files are controlled by
    /// whoever wrote it; letting it configure the run would let a branch
    /// reconfigure the tool that is setting it up. A config file that exists
    /// only in the branch is reported as ignored rather than silently dropped.
    pub fn load(config_dir: &Path, source: &Path, target: &Path) -> Result<Config> {
        let mut cfg = Config::default();
        cfg.merge_file(Origin::User, &config_dir.join(USER_FILE))?;
        cfg.merge_file(Origin::Repo, &source.join(REPO_FILE))?;
        cfg.merge_file(Origin::RepoLocal, &source.join(REPO_LOCAL_FILE))?;
        cfg.note_ignored_branch_config(source, target);
        Ok(cfg)
    }

    /// Warn when the branch carries a config the source checkout does not, or
    /// carries a different one — otherwise "my config did nothing" is baffling.
    fn note_ignored_branch_config(&mut self, source: &Path, target: &Path) {
        for file in [REPO_FILE, REPO_LOCAL_FILE] {
            let in_target = target.join(file);
            if !in_target.is_file() {
                continue;
            }
            let same = match (std::fs::read(&in_target), std::fs::read(source.join(file))) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if !same {
                self.warnings.push(format!(
                    "ignored {file} from the worktree: configuration is read from {} (the branch does not get to configure its own setup)",
                    source.display()
                ));
            }
        }
    }

    /// User layer only — enough for the hook (placement/focus) before a target exists.
    pub fn load_user(config_dir: &Path) -> Result<Config> {
        let mut cfg = Config::default();
        cfg.merge_file(Origin::User, &config_dir.join(USER_FILE))?;
        Ok(cfg)
    }

    fn merge_file(&mut self, origin: Origin, path: &Path) -> Result<()> {
        match read_layer(path) {
            Ok(Some(layer)) => {
                self.layers.push((origin, path.to_path_buf()));
                self.merge(layer, origin);
                Ok(())
            }
            Ok(None) => Ok(()),
            // The user's own config is theirs to fix, so a bad one is fatal.
            // A repo layer is only advisory: report it and carry on, so a typo
            // (or a file written for a newer version) cannot block setup.
            Err(err) if origin.is_repo() => {
                self.warnings
                    .push(format!("ignored {}: {err:#}", path.display()));
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn merge(&mut self, layer: ConfigLayer, origin: Origin) {
        let repo = origin.is_repo();
        let label = origin.label();
        let mut warnings: Vec<String> = Vec::new();
        let mut refuse = |name: &str, why: &str| {
            warnings.push(format!("{label}: `{name}` {why}; ignored"));
        };
        /// Belongs to the person running herdr, not to a repository.
        macro_rules! host_scalar {
            ($field:ident) => {
                if let Some(v) = layer.$field {
                    if repo {
                        refuse(stringify!($field), "can only be set in the user config");
                    } else {
                        self.$field = v;
                    }
                }
            };
        }
        /// A repo may switch it OFF but never back ON: configuration must not
        /// be able to re-enable execution that the user turned off.
        macro_rules! off_only_scalar {
            ($field:ident) => {
                if let Some(v) = layer.$field {
                    if repo && v {
                        refuse(stringify!($field), "can only be disabled by a repo config");
                    } else {
                        self.$field = v;
                    }
                }
            };
        }
        /// A repo may lower a limit but never raise it.
        macro_rules! lower_only_scalar {
            ($field:ident) => {
                if let Some(v) = layer.$field {
                    if repo && v > self.$field {
                        refuse(stringify!($field), "can only be lowered by a repo config");
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
        host_scalar!(mode);
        host_scalar!(auto_close_secs);
        off_only_scalar!(install);
        off_only_scalar!(mise_trust);
        off_only_scalar!(direnv_allow);
        off_only_scalar!(use_mise);
        off_only_scalar!(use_direnv);
        lower_only_scalar!(copy_size_cap_mb);
        lower_only_scalar!(total_size_cap_mb);
        lower_only_scalar!(step_timeout_secs);
        extend_unique(&mut self.copy, layer.copy);
        extend_unique(&mut self.clone, layer.clone);
        // A symlink aims the worktree back at the source checkout, so writes in
        // the worktree mutate the main one. That is the opposite of what this
        // tool is for, and it is a choice only the user may make.
        if repo && !layer.symlink.is_empty() {
            refuse("symlink", "can only be set in the user config");
        } else {
            extend_unique(&mut self.symlink, layer.symlink);
        }
        extend_unique(&mut self.exclude, layer.exclude);
        for (name, value) in layer.env {
            if repo && !repo_env_allowed(&name) {
                refuse(
                    &format!("env.{name}"),
                    "selects or injects into the program that runs",
                );
                continue;
            }
            self.env.insert(name, value);
        }
        for mut step in layer.steps {
            step.origin = origin;
            self.steps.push(step);
        }
        self.warnings.extend(warnings);
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

    struct Fixture {
        _tmp: tempfile::TempDir,
        cfg_dir: PathBuf,
        source: PathBuf,
        target: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("cfg");
        let source = tmp.path().join("src");
        let target = tmp.path().join("wt");
        for d in [&cfg_dir, &source, &target] {
            std::fs::create_dir_all(d).unwrap();
        }
        Fixture {
            _tmp: tmp,
            cfg_dir,
            source,
            target,
        }
    }

    impl Fixture {
        fn user(&self, body: &str) -> &Self {
            std::fs::write(self.cfg_dir.join(USER_FILE), body).unwrap();
            self
        }
        fn repo(&self, body: &str) -> &Self {
            std::fs::write(self.source.join(REPO_FILE), body).unwrap();
            self
        }
        /// A config committed on the branch being set up.
        fn branch(&self, body: &str) -> &Self {
            std::fs::write(self.target.join(REPO_FILE), body).unwrap();
            self
        }
        fn load(&self) -> Config {
            Config::load(&self.cfg_dir, &self.source, &self.target).unwrap()
        }
    }

    #[test]
    fn layers_merge_in_order_and_lists_extend() {
        let f = fixture();
        f.user("auto_close_secs = 9\ncopy = ['.secrets/']\n");
        f.repo("install = false\nclone = ['.pio/']\n[[steps]]\nname = 'migrate'\nrun = 'pnpm db:migrate'\nif = 'prisma/schema.prisma'\n");
        std::fs::write(
            f.source.join(REPO_LOCAL_FILE),
            "total_size_cap_mb = 100\n[env]\nFOO = 'bar'\n",
        )
        .unwrap();

        let cfg = f.load();
        assert_eq!(cfg.auto_close_secs, 9, "user layer applies");
        assert!(!cfg.install, "repo layer may disable install");
        assert_eq!(cfg.total_size_cap_mb, 100, "repo-local may lower a cap");
        assert!(cfg.copy.iter().any(|p| p == ".secrets/"));
        assert!(cfg.copy.iter().any(|p| p == ".env"), "defaults kept");
        assert!(cfg.clone.iter().any(|p| p == ".pio/"));
        assert_eq!(cfg.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(cfg.steps.len(), 1);
        assert_eq!(cfg.steps[0].origin, Origin::Repo);
        assert_eq!(cfg.layers.len(), 3);
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    }

    /// The branch being set up does not get to configure the tool setting it up.
    #[test]
    fn a_config_committed_on_the_branch_is_ignored_and_reported() {
        let f = fixture();
        f.branch("install = false\nauto_close_secs = 0\n[[steps]]\nname = 'pwn'\nrun = 'id'\n");
        let cfg = f.load();
        assert!(cfg.install, "branch config must not take effect");
        assert_eq!(cfg.auto_close_secs, 5);
        assert!(cfg.steps.is_empty(), "branch steps must not be collected");
        assert!(cfg.layers.is_empty(), "branch file is not a config layer");
        assert_eq!(cfg.warnings.len(), 1);
        assert!(cfg.warnings[0].contains("ignored .herdr-worktree.toml from the worktree"));
    }

    #[test]
    fn an_identical_branch_copy_is_not_reported() {
        let f = fixture();
        f.repo("install = false\n");
        std::fs::write(f.target.join(REPO_FILE), "install = false\n").unwrap();
        let cfg = f.load();
        assert!(!cfg.install);
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    }

    /// A repo may restrict the run; it may never widen it.
    #[test]
    fn a_repo_layer_can_only_restrict() {
        let f = fixture();
        f.user(
            "install = false\nmise_trust = false\ndirenv_allow = false\nuse_mise = false\nuse_direnv = false\ncopy_size_cap_mb = 10\ntotal_size_cap_mb = 20\nstep_timeout_secs = 60\n",
        );
        f.repo(
            "install = true\nmise_trust = true\ndirenv_allow = true\nuse_mise = true\nuse_direnv = true\ncopy_size_cap_mb = 99999\ntotal_size_cap_mb = 99999\nstep_timeout_secs = 99999\nmode = 'copy'\nfocus = false\nplacement = 'tab'\ndirection = 'right'\ncolor = false\ntrust_repo_steps = false\nsymlink = ['node_modules/']\n",
        );
        let cfg = f.load();
        assert!(!cfg.install, "install stays off");
        assert!(
            !cfg.mise_trust && !cfg.direnv_allow,
            "trust grants stay off"
        );
        assert!(!cfg.use_mise && !cfg.use_direnv);
        assert_eq!(cfg.copy_size_cap_mb, 10, "cap not raised");
        assert_eq!(cfg.total_size_cap_mb, 20, "cap not raised");
        assert_eq!(cfg.step_timeout_secs, 60, "timeout not raised");
        assert_eq!(cfg.mode, CloneMode::Reflink, "mode is host-only");
        assert!(cfg.focus, "focus is host-only");
        assert_eq!(cfg.placement, Placement::Split);
        assert_eq!(cfg.direction, Direction::Down);
        assert!(cfg.color);
        assert!(cfg.trust_repo_steps, "a repo cannot change its own trust");
        assert!(
            cfg.symlink.is_empty(),
            "a repo cannot alias the worktree at the source"
        );
        for key in [
            "install",
            "mise_trust",
            "direnv_allow",
            "use_mise",
            "use_direnv",
            "copy_size_cap_mb",
            "total_size_cap_mb",
            "step_timeout_secs",
            "mode",
            "focus",
            "placement",
            "direction",
            "color",
            "trust_repo_steps",
            "symlink",
        ] {
            assert!(
                cfg.warnings.iter().any(|w| w.contains(&format!("`{key}`"))),
                "no warning for {key} in {:?}",
                cfg.warnings
            );
        }
    }

    #[test]
    fn a_repo_layer_may_lower_a_cap_and_turn_things_off() {
        let f = fixture();
        f.user("copy_size_cap_mb = 4096\n");
        f.repo("copy_size_cap_mb = 64\nuse_mise = false\n");
        let cfg = f.load();
        assert_eq!(cfg.copy_size_cap_mb, 64);
        assert!(!cfg.use_mise);
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    }

    /// `[env]` decides which program actually runs.
    #[test]
    fn a_repo_layer_cannot_set_loader_or_lookup_variables() {
        let f = fixture();
        f.repo(
            "[env]\nPATH = 'hack'\nLD_PRELOAD = '/tmp/x.so'\nDYLD_INSERT_LIBRARIES = '/tmp/x.dylib'\nNODE_OPTIONS = '--require /tmp/x.js'\nBASH_ENV = '/tmp/x.sh'\nGIT_SSH_COMMAND = 'sh -c id'\nSHELL = '/tmp/sh'\nNODE_ENV = 'test'\n",
        );
        let cfg = f.load();
        assert_eq!(cfg.env.get("NODE_ENV").map(String::as_str), Some("test"));
        for denied in [
            "PATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "NODE_OPTIONS",
            "BASH_ENV",
            "GIT_SSH_COMMAND",
            "SHELL",
        ] {
            assert!(!cfg.env.contains_key(denied), "{denied} was accepted");
            assert!(cfg
                .warnings
                .iter()
                .any(|w| w.contains(&format!("env.{denied}"))));
        }
    }

    #[test]
    fn the_user_layer_may_set_anything() {
        let f = fixture();
        f.user("install = true\nmise_trust = true\nmode = 'copy'\ncopy_size_cap_mb = 99999\nsymlink = ['datasets/']\n[env]\nPATH = '/custom/bin'\n");
        let cfg = f.load();
        assert!(cfg.install && cfg.mise_trust);
        assert_eq!(cfg.mode, CloneMode::Copy);
        assert_eq!(cfg.copy_size_cap_mb, 99999);
        assert_eq!(cfg.symlink, vec!["datasets/".to_string()]);
        assert_eq!(cfg.env.get("PATH").map(String::as_str), Some("/custom/bin"));
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    }

    /// A malformed repo config must not take the whole run down with it.
    #[test]
    fn a_broken_repo_layer_warns_instead_of_failing() {
        let f = fixture();
        f.repo("this is not toml = = =\n");
        let cfg = f.load();
        assert!(cfg.install, "defaults still apply");
        assert_eq!(cfg.warnings.len(), 1);
        assert!(cfg.warnings[0].contains("ignored"));
        assert!(cfg.warnings[0].contains(REPO_FILE));
    }

    #[test]
    fn an_unknown_key_in_a_repo_layer_is_only_a_warning() {
        let f = fixture();
        f.repo("future_key_from_a_newer_version = 1\n");
        let cfg = f.load();
        assert_eq!(cfg.warnings.len(), 1);
        assert!(cfg.warnings[0].contains("future_key_from_a_newer_version"));
    }

    #[test]
    fn unknown_keys_are_errors_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(USER_FILE), "bogus = 1\n").unwrap();
        let err = Config::load_user(dir.path()).unwrap_err();
        assert!(err.to_string().contains(USER_FILE));
    }
}
