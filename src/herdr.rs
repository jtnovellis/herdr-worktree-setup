//! Thin, best-effort wrapper around the `herdr` CLI (JSON output).

use crate::config::{Direction, Placement};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Herdr {
    bin: PathBuf,
}

pub struct PaneOpen {
    pub plugin: String,
    pub entrypoint: String,
    pub workspace: Option<String>,
    pub target_pane: Option<String>,
    pub placement: Placement,
    pub direction: Direction,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub focus: bool,
}

impl Herdr {
    pub fn from_env() -> Herdr {
        let bin = std::env::var_os("HERDR_BIN_PATH")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("herdr"));
        Herdr { bin }
    }

    /// Run `herdr <args>` and return the parsed `result`.
    pub fn call(&self, args: &[String]) -> Result<Value> {
        let out = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {} {}", self.bin.display(), args.join(" ")))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // herdr prints error envelopes to stderr with a non-zero status.
        let json: Value = match serde_json::from_str(stdout.trim())
            .or_else(|_| serde_json::from_str(stderr.trim()))
        {
            Ok(v) => v,
            Err(_) if out.status.success() => Value::Null,
            Err(_) => {
                return Err(anyhow!(
                    "herdr {} failed: {}",
                    args.join(" "),
                    if stderr.trim().is_empty() {
                        stdout.trim()
                    } else {
                        stderr.trim()
                    }
                ))
            }
        };
        if let Some(err) = json.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
            return Err(anyhow!("herdr {}: {code}: {msg}", args.join(" ")));
        }
        if !out.status.success() {
            return Err(anyhow!("herdr {} exited {}", args.join(" "), out.status));
        }
        Ok(json.get("result").cloned().unwrap_or(json))
    }

    /// `(pane_id, focused)` for every pane of a workspace.
    pub fn workspace_panes(&self, workspace: &str) -> Result<Vec<(String, bool)>> {
        let result = self.call(&[
            "pane".into(),
            "list".into(),
            "--workspace".into(),
            workspace.into(),
        ])?;
        let panes = result
            .get("panes")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("pane list: no panes array"))?;
        Ok(panes
            .iter()
            .filter_map(|p| {
                let id = p.get("pane_id")?.as_str()?.to_string();
                let focused = p.get("focused").and_then(Value::as_bool).unwrap_or(false);
                Some((id, focused))
            })
            .collect())
    }

    /// Open a plugin pane; returns its pane id when herdr reports one.
    pub fn plugin_pane_open(&self, open: &PaneOpen) -> Result<Option<String>> {
        let mut args: Vec<String> = vec![
            "plugin".into(),
            "pane".into(),
            "open".into(),
            "--plugin".into(),
            open.plugin.clone(),
            "--entrypoint".into(),
            open.entrypoint.clone(),
            "--placement".into(),
            open.placement.as_str().into(),
            "--cwd".into(),
            open.cwd.display().to_string(),
        ];
        // herdr's rules: split/zoomed take *only* a target pane; tab takes a
        // workspace; overlay/popup take neither (they use the active pane).
        match open.placement {
            Placement::Split | Placement::Zoomed => {
                if let Some(pane) = &open.target_pane {
                    args.push("--target-pane".into());
                    args.push(pane.clone());
                }
                if open.placement == Placement::Split {
                    args.push("--direction".into());
                    args.push(open.direction.as_str().into());
                }
            }
            Placement::Tab => {
                if let Some(ws) = &open.workspace {
                    args.push("--workspace".into());
                    args.push(ws.clone());
                }
            }
            Placement::Overlay => {}
        }
        for (k, v) in &open.env {
            args.push("--env".into());
            args.push(format!("{k}={v}"));
        }
        args.push(if open.focus {
            "--focus".into()
        } else {
            "--no-focus".into()
        });
        let result = self.call(&args)?;
        Ok(result
            .pointer("/plugin_pane/pane/pane_id")
            .or_else(|| result.pointer("/pane/pane_id"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    pub fn plugin_pane_focus(&self, pane_id: &str) -> Result<()> {
        self.call(&[
            "plugin".into(),
            "pane".into(),
            "focus".into(),
            pane_id.into(),
        ])?;
        Ok(())
    }

    /// Sidebar title for the pane; errors are ignored.
    pub fn set_title(&self, pane_id: &str, title: &str) {
        let _ = self.call(&[
            "pane".into(),
            "report-metadata".into(),
            pane_id.into(),
            "--source".into(),
            "plugin:worktree-setup".into(),
            "--title".into(),
            title.into(),
        ]);
    }

    /// Toast (honours the user's `ui.toast` setting); errors are ignored.
    pub fn notify(&self, title: &str, body: Option<&str>, sound: &str) {
        let mut args: Vec<String> = vec!["notification".into(), "show".into(), title.into()];
        if let Some(body) = body {
            args.push("--body".into());
            args.push(body.into());
        }
        args.push("--sound".into());
        args.push(sound.into());
        let _ = self.call(&args);
    }
}
