use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::process::{checked_output, checked_status};

const DEFAULT_PLUGIN_ID: &str = "olivergilan.herdr-jj";

#[derive(Deserialize)]
pub struct InvocationContext {
    pub workspace_id: Option<String>,
    pub workspace_cwd: Option<PathBuf>,
    pub focused_pane_cwd: Option<PathBuf>,
}

impl InvocationContext {
    pub fn from_env() -> Result<Self> {
        let raw = env::var("HERDR_PLUGIN_CONTEXT_JSON")
            .context("HERDR_PLUGIN_CONTEXT_JSON is missing")?;
        serde_json::from_str(&raw).context("invalid HerdR plugin context")
    }

    pub fn source_cwd(&self) -> Option<&Path> {
        self.focused_pane_cwd
            .as_deref()
            .or(self.workspace_cwd.as_deref())
    }

    pub fn required_source(&self) -> Result<(&Path, &str)> {
        let cwd = self
            .source_cwd()
            .context("focused HerdR workspace has no working directory")?;
        let workspace_id = self
            .workspace_id
            .as_deref()
            .context("focused HerdR workspace has no ID")?;
        if cwd.as_os_str().is_empty() || workspace_id.is_empty() {
            bail!("focused HerdR workspace context is incomplete");
        }
        Ok((cwd, workspace_id))
    }
}

pub struct Herdr {
    binary: OsString,
    plugin_id: String,
}

pub struct CreatedWorkspace {
    pub workspace_id: String,
    pub root_pane_id: String,
}

#[derive(Deserialize)]
pub struct Snapshot {
    pub workspaces: Vec<SnapshotWorkspace>,
    pub panes: Vec<SnapshotPane>,
}

#[derive(Deserialize)]
pub struct SnapshotWorkspace {
    pub workspace_id: String,
    pub active_tab_id: String,
}

#[derive(Deserialize)]
pub struct SnapshotPane {
    pub workspace_id: String,
    pub tab_id: String,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub focused: bool,
}

impl Snapshot {
    pub fn cwd_for_workspace(&self, workspace: &SnapshotWorkspace) -> Option<&Path> {
        self.panes
            .iter()
            .filter(|pane| {
                pane.workspace_id == workspace.workspace_id
                    && pane.tab_id == workspace.active_tab_id
            })
            .find(|pane| pane.focused)
            .or_else(|| {
                self.panes.iter().find(|pane| {
                    pane.workspace_id == workspace.workspace_id
                        && pane.tab_id == workspace.active_tab_id
                })
            })
            .and_then(|pane| pane.cwd.as_deref())
    }

    pub fn workspace_for_root(&self, root: &Path) -> Option<String> {
        let expected = canonical_or_original(root);
        self.workspaces.iter().find_map(|workspace| {
            self.panes
                .iter()
                .filter(|pane| {
                    pane.workspace_id == workspace.workspace_id
                        && pane.tab_id == workspace.active_tab_id
                })
                .filter_map(|pane| pane.cwd.as_deref())
                .any(|cwd| canonical_or_original(cwd) == expected)
                .then(|| workspace.workspace_id.clone())
        })
    }
}

impl Herdr {
    pub fn from_env() -> Self {
        Self {
            binary: env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr")),
            plugin_id: env::var("HERDR_PLUGIN_ID")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_PLUGIN_ID.to_owned()),
        }
    }

    pub fn open_popup(&self, entrypoint: &str, values: &[(&str, &OsStr)]) -> Result<()> {
        let mut command = Command::new(&self.binary);
        command.args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            &self.plugin_id,
            "--entrypoint",
            entrypoint,
        ]);
        for (key, value) in values {
            let mut assignment = OsString::from(key);
            assignment.push("=");
            assignment.push(value);
            command.arg("--env").arg(assignment);
        }
        command.arg("--focus");
        checked_status(&mut command, "open HerdR plugin popup")
    }

    pub fn create_workspace(
        &self,
        cwd: &Path,
        label: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<CreatedWorkspace> {
        let mut command = Command::new(&self.binary);
        command
            .args(["workspace", "create", "--cwd"])
            .arg(cwd)
            .args(["--label", label, "--focus"]);
        for (key, value) in env {
            command.arg("--env").arg(format!("{key}={value}"));
        }
        let output = checked_output(&mut command, "create HerdR workspace")?;
        let envelope: Envelope<WorkspaceCreateResult> =
            serde_json::from_str(&output).context("invalid HerdR workspace response")?;
        Ok(CreatedWorkspace {
            workspace_id: envelope.result.workspace.workspace_id,
            root_pane_id: envelope.result.root_pane.pane_id,
        })
    }

    pub fn focus_workspace(&self, workspace_id: &str) -> Result<()> {
        let mut command = Command::new(&self.binary);
        command.args(["workspace", "focus", workspace_id]);
        checked_status(&mut command, "focus HerdR workspace")
    }

    pub fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        let mut command = Command::new(&self.binary);
        command.args(["workspace", "close", workspace_id]);
        checked_status(&mut command, "close HerdR workspace")
    }

    pub fn run_in_pane(&self, pane_id: &str, command_text: &str) -> Result<()> {
        let mut command = Command::new(&self.binary);
        command.args(["pane", "run", pane_id, command_text]);
        checked_status(&mut command, "submit post-create command")
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        let mut command = Command::new(&self.binary);
        command.args(["api", "snapshot"]);
        let output = checked_output(&mut command, "read HerdR session snapshot")?;
        let envelope: Envelope<SnapshotResult> =
            serde_json::from_str(&output).context("invalid HerdR snapshot response")?;
        Ok(envelope.result.snapshot)
    }

    pub fn report_status(
        &self,
        workspace_id: &str,
        change: Option<&str>,
        status: Option<&str>,
    ) -> Result<()> {
        let mut command = Command::new(&self.binary);
        command.args([
            "workspace",
            "report-metadata",
            workspace_id,
            "--source",
            DEFAULT_PLUGIN_ID,
        ]);
        match change {
            Some(value) => {
                command.arg("--token").arg(format!("jj_change={value}"));
            }
            None => {
                command.args(["--clear-token", "jj_change"]);
            }
        }
        match status {
            Some(value) if !value.is_empty() => {
                command.arg("--token").arg(format!("jj_status={value}"));
            }
            _ => {
                command.args(["--clear-token", "jj_status"]);
            }
        }
        checked_status(&mut command, "report JJ workspace metadata")
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Deserialize)]
struct WorkspaceCreateResult {
    workspace: WorkspaceId,
    root_pane: PaneId,
}

#[derive(Deserialize)]
struct WorkspaceId {
    workspace_id: String,
}

#[derive(Deserialize)]
struct PaneId {
    pane_id: String,
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: T,
}

#[derive(Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}
