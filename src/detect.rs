//! Zero-config detection of what a checkout needs: package manager installs,
//! mise config files, direnv.
//!
//! SECURITY: everything here reads files from the *worktree*, whose contents
//! are controlled by whoever wrote the branch. Nothing read here may ever be
//! interpolated into a command line — install commands are built only from
//! fixed allowlists, and are executed as argv (never through a shell).

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallCmd {
    /// Program name. Always a literal from this file — never repo-derived text.
    pub tool: String,
    /// Full argv, executed directly. No shell is involved.
    pub argv: Vec<String>,
    /// Display label.
    pub label: String,
}

fn cmd(tool: &'static str, args: &[&'static str]) -> InstallCmd {
    let mut argv = vec![tool.to_string()];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    InstallCmd {
        tool: tool.to_string(),
        label: argv.join(" "),
        argv,
    }
}

/// The Node package managers we know how to invoke. A `packageManager` value
/// outside this set is ignored: that field is attacker-controlled in any branch
/// a user might check out, so it selects a command, it never *becomes* one.
const NODE_MANAGERS: &[(&str, InstallKind)] = &[
    ("pnpm", InstallKind::Pnpm),
    ("bun", InstallKind::Bun),
    ("yarn", InstallKind::Yarn),
    ("npm", InstallKind::Npm),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallKind {
    Pnpm,
    Bun,
    Yarn,
    Npm,
}

impl InstallKind {
    fn command(self) -> InstallCmd {
        match self {
            InstallKind::Pnpm => cmd("pnpm", &["install"]),
            InstallKind::Bun => cmd("bun", &["install"]),
            InstallKind::Yarn => cmd("yarn", &["install"]),
            InstallKind::Npm => cmd("npm", &["install"]),
        }
    }
}

/// The `packageManager` name declared in package.json, if it names a manager we
/// support. Unsupported or malformed values yield `None` (we then fall back to
/// lockfile detection), so no part of the field ever reaches a process.
fn declared_node_manager(target: &Path) -> Option<InstallKind> {
    let raw = std::fs::read_to_string(target.join("package.json")).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    let field = json.get("packageManager")?.as_str()?;
    let name = field.split('@').next()?.trim();
    NODE_MANAGERS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, kind)| *kind)
}

fn node_install(target: &Path) -> Option<InstallCmd> {
    if !target.join("package.json").is_file() {
        return None;
    }
    let kind = declared_node_manager(target).or_else(|| {
        [
            ("pnpm-lock.yaml", InstallKind::Pnpm),
            ("bun.lock", InstallKind::Bun),
            ("bun.lockb", InstallKind::Bun),
            ("yarn.lock", InstallKind::Yarn),
            ("package-lock.json", InstallKind::Npm),
            ("npm-shrinkwrap.json", InstallKind::Npm),
        ]
        .iter()
        .find(|(lock, _)| target.join(lock).is_file())
        .map(|(_, kind)| *kind)
    })?;
    Some(kind.command())
}

/// One install command per ecosystem present at the checkout root.
pub fn detect_installs(target: &Path) -> Vec<InstallCmd> {
    let mut out = Vec::new();
    if let Some(node) = node_install(target) {
        out.push(node);
    } else if target.join("package.json").is_file() {
        out.push(InstallKind::Npm.command());
    }
    let has = |f: &str| target.join(f).is_file();
    if has("uv.lock") {
        out.push(cmd("uv", &["sync"]));
    } else if has("poetry.lock") {
        out.push(cmd("poetry", &["install"]));
    } else if has("Pipfile.lock") {
        out.push(cmd("pipenv", &["install"]));
    }
    if has("Gemfile.lock") {
        out.push(cmd("bundle", &["install"]));
    }
    if has("go.sum") {
        out.push(cmd("go", &["mod", "download"]));
    }
    if has("mix.lock") {
        out.push(cmd("mix", &["deps.get"]));
    }
    if has("composer.lock") {
        out.push(cmd("composer", &["install"]));
    }
    if has("Cargo.lock") {
        out.push(cmd("cargo", &["fetch"]));
    }
    out
}

pub fn mise_config_files(target: &Path) -> Vec<PathBuf> {
    [
        ".mise.toml",
        "mise.toml",
        ".mise.local.toml",
        "mise.local.toml",
        ".mise/config.toml",
        "mise/config.toml",
        ".config/mise.toml",
        ".config/mise/config.toml",
        ".tool-versions",
    ]
    .iter()
    .map(|f| target.join(f))
    .filter(|p| p.is_file())
    .collect()
}

pub fn has_envrc(target: &Path) -> bool {
    target.join(".envrc").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn labels(target: &Path) -> Vec<String> {
        detect_installs(target)
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    #[test]
    fn package_manager_field_wins_over_lockfiles() {
        let tmp = tempfile::tempdir().unwrap();
        touch(
            tmp.path(),
            "package.json",
            r#"{"packageManager":"pnpm@9.1.0"}"#,
        );
        touch(tmp.path(), "yarn.lock", "");
        assert_eq!(labels(tmp.path()), vec!["pnpm install"]);
    }

    /// The `packageManager` field is attacker-controlled: it must only ever
    /// select from the allowlist, never contribute text to a command.
    #[test]
    fn hostile_package_manager_fields_never_reach_a_command() {
        for hostile in [
            r#"{"packageManager":"x; touch /tmp/pwned; echo y@1"}"#,
            r#"{"packageManager":"./evil@1"}"#,
            r#"{"packageManager":"/bin/sh@1"}"#,
            r#"{"packageManager":"../../../../bin/sh@1"}"#,
            r#"{"packageManager":"npm; id@1"}"#,
            r#"{"packageManager":"$(id)@1"}"#,
            r#"{"packageManager":"`id`@1"}"#,
            r#"{"packageManager":"npm\ninstall@1"}"#,
            r#"{"packageManager":"NPM@1"}"#,
            r#"{"packageManager":" npm @1"}"#,
            r#"{"packageManager":"@1"}"#,
            r#"{"packageManager":123}"#,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            touch(tmp.path(), "package.json", hostile);
            let cmds = detect_installs(tmp.path());
            // With no lockfile the fallback is plain npm; nothing else may appear.
            assert_eq!(cmds.len(), 1, "{hostile}");
            assert_eq!(cmds[0].argv, vec!["npm", "install"], "{hostile}");
            assert_eq!(cmds[0].tool, "npm", "{hostile}");
        }
    }

    #[test]
    fn hostile_field_falls_back_to_the_lockfile_not_the_field() {
        let tmp = tempfile::tempdir().unwrap();
        touch(
            tmp.path(),
            "package.json",
            r#"{"packageManager":"evil; id@1"}"#,
        );
        touch(tmp.path(), "pnpm-lock.yaml", "");
        assert_eq!(labels(tmp.path()), vec!["pnpm install"]);
    }

    #[test]
    fn every_install_argv_is_a_bare_program_plus_literal_args() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "package.json",
            "uv.lock",
            "Gemfile.lock",
            "go.sum",
            "mix.lock",
            "composer.lock",
            "Cargo.lock",
        ] {
            touch(tmp.path(), f, "{}");
        }
        let cmds = detect_installs(tmp.path());
        assert!(cmds.len() >= 7);
        for c in cmds {
            assert!(!c.tool.is_empty());
            assert!(
                c.tool
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
                "tool `{}` is not a bare program name",
                c.tool
            );
            assert_eq!(c.argv[0], c.tool);
            for arg in &c.argv {
                assert!(
                    !arg.chars()
                        .any(|ch| " \t\n;|&$`<>(){}[]*?!\\\"'".contains(ch)),
                    "argv element `{arg}` contains a shell metacharacter"
                );
            }
        }
    }

    #[test]
    fn lockfile_detection_and_multiple_ecosystems() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "package.json", "{}");
        touch(tmp.path(), "bun.lock", "");
        touch(tmp.path(), "uv.lock", "");
        touch(tmp.path(), "Cargo.lock", "");
        assert_eq!(
            labels(tmp.path()),
            vec!["bun install", "uv sync", "cargo fetch"]
        );
    }

    #[test]
    fn package_json_without_lockfile_uses_npm_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "package.json", "{}");
        let cmds = detect_installs(tmp.path());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].tool, "npm");
        assert!(detect_installs(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn mise_and_direnv_presence() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(mise_config_files(tmp.path()).is_empty());
        assert!(!has_envrc(tmp.path()));
        touch(tmp.path(), "mise.toml", "");
        touch(tmp.path(), ".tool-versions", "");
        touch(tmp.path(), ".envrc", "");
        assert_eq!(mise_config_files(tmp.path()).len(), 2);
        assert!(has_envrc(tmp.path()));
    }
}
