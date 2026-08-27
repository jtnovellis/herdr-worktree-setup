//! Zero-config detection of what a checkout needs: package manager installs,
//! mise config files, direnv.

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallCmd {
    /// Executable that must resolve (unless mise provides it).
    pub tool: String,
    /// Shell command line.
    pub command: String,
    pub label: String,
}

fn cmd(tool: &str, command: &str) -> InstallCmd {
    InstallCmd {
        tool: tool.to_string(),
        command: command.to_string(),
        label: command.to_string(),
    }
}

fn package_manager_field(target: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(target.join("package.json")).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    let field = json.get("packageManager")?.as_str()?;
    let name = field.split('@').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn node_install(target: &Path) -> Option<InstallCmd> {
    if !target.join("package.json").is_file() {
        return None;
    }
    let by_field = package_manager_field(target);
    let name = by_field.as_deref().or_else(|| {
        [
            ("pnpm-lock.yaml", "pnpm"),
            ("bun.lock", "bun"),
            ("bun.lockb", "bun"),
            ("yarn.lock", "yarn"),
            ("package-lock.json", "npm"),
            ("npm-shrinkwrap.json", "npm"),
        ]
        .iter()
        .find(|(lock, _)| target.join(lock).is_file())
        .map(|(_, pm)| *pm)
    })?;
    Some(match name {
        "pnpm" => cmd("pnpm", "pnpm install"),
        "bun" => cmd("bun", "bun install"),
        "yarn" => cmd("yarn", "yarn install"),
        "npm" => cmd("npm", "npm install"),
        other => cmd(other, &format!("{other} install")),
    })
}

/// One install command per ecosystem present at the checkout root.
pub fn detect_installs(target: &Path) -> Vec<InstallCmd> {
    let mut out = Vec::new();
    if let Some(node) = node_install(target) {
        out.push(node);
    } else if target.join("package.json").is_file() {
        out.push(cmd("npm", "npm install"));
    }
    let has = |f: &str| target.join(f).is_file();
    if has("uv.lock") {
        out.push(cmd("uv", "uv sync"));
    } else if has("poetry.lock") {
        out.push(cmd("poetry", "poetry install"));
    } else if has("Pipfile.lock") {
        out.push(cmd("pipenv", "pipenv install"));
    }
    if has("Gemfile.lock") {
        out.push(cmd("bundle", "bundle install"));
    }
    if has("go.sum") {
        out.push(cmd("go", "go mod download"));
    }
    if has("mix.lock") {
        out.push(cmd("mix", "mix deps.get"));
    }
    if has("composer.lock") {
        out.push(cmd("composer", "composer install"));
    }
    if has("Cargo.lock") {
        out.push(cmd("cargo", "cargo fetch"));
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

    #[test]
    fn package_manager_field_wins_over_lockfiles() {
        let tmp = tempfile::tempdir().unwrap();
        touch(
            tmp.path(),
            "package.json",
            r#"{"packageManager":"pnpm@9.1.0"}"#,
        );
        touch(tmp.path(), "yarn.lock", "");
        let cmds = detect_installs(tmp.path());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command, "pnpm install");
    }

    #[test]
    fn lockfile_detection_and_multiple_ecosystems() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "package.json", "{}");
        touch(tmp.path(), "bun.lock", "");
        touch(tmp.path(), "uv.lock", "");
        touch(tmp.path(), "Cargo.lock", "");
        let cmds: Vec<String> = detect_installs(tmp.path())
            .into_iter()
            .map(|c| c.command)
            .collect();
        assert_eq!(cmds, vec!["bun install", "uv sync", "cargo fetch"]);
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
