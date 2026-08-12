use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::process::{checked_output, checked_status};

const DEFAULT_PLUGIN_ID: &str = "olivergilan.herdr-jj";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct InvocationContext {
    pub workspace_id: Option<String>,
    pub workspace_label: Option<String>,
    pub workspace_cwd: Option<PathBuf>,
    pub tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
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
}

#[derive(Clone, Debug)]
pub struct Herdr {
    binary: OsString,
    plugin_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedWorkspace {
    pub workspace_id: String,
    pub root_pane_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Snapshot {
    pub workspaces: Vec<SnapshotWorkspace>,
    pub panes: Vec<SnapshotPane>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SnapshotWorkspace {
    pub workspace_id: String,
    pub active_tab_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SnapshotPane {
    pub workspace_id: String,
    pub tab_id: String,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub focused: bool,
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

    #[cfg(test)]
    pub fn with_binary(binary: impl Into<OsString>) -> Self {
        Self {
            binary: binary.into(),
            plugin_id: DEFAULT_PLUGIN_ID.to_owned(),
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
        let envelope: WorkspaceCreateEnvelope =
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
        let envelope: SnapshotEnvelope =
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

    pub fn find_workspace_for_root(&self, snapshot: &Snapshot, root: &Path) -> Option<String> {
        let expected = canonical_or_original(root);
        snapshot.workspaces.iter().find_map(|workspace| {
            snapshot
                .panes
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

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Deserialize)]
struct WorkspaceCreateEnvelope {
    result: WorkspaceCreateResult,
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
struct SnapshotEnvelope {
    result: SnapshotResult,
}

#[derive(Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}

pub fn required_source(context: &InvocationContext) -> Result<(&Path, &str)> {
    let cwd = context
        .source_cwd()
        .context("focused HerdR workspace has no working directory")?;
    let workspace_id = context
        .workspace_id
        .as_deref()
        .context("focused HerdR workspace has no ID")?;
    if cwd.as_os_str().is_empty() || workspace_id.is_empty() {
        bail!("focused HerdR workspace context is incomplete");
    }
    Ok((cwd, workspace_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn context_prefers_focused_pane_cwd() {
        let context: InvocationContext = serde_json::from_str(
            r#"{
                "workspace_id":"w1",
                "workspace_cwd":"/repo",
                "focused_pane_cwd":"/repo/subdir"
            }"#,
        )
        .unwrap();

        assert_eq!(context.source_cwd(), Some(Path::new("/repo/subdir")));
    }

    #[test]
    fn finds_open_workspace_by_active_tab_cwd() {
        let herdr = Herdr::with_binary("herdr");
        let snapshot = Snapshot {
            workspaces: vec![SnapshotWorkspace {
                workspace_id: "w2".to_owned(),
                active_tab_id: "w2:t1".to_owned(),
            }],
            panes: vec![SnapshotPane {
                workspace_id: "w2".to_owned(),
                tab_id: "w2:t1".to_owned(),
                cwd: Some(PathBuf::from("/repo.feature")),
                focused: false,
            }],
        };

        assert_eq!(
            herdr.find_workspace_for_root(&snapshot, Path::new("/repo.feature")),
            Some("w2".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn creates_a_workspace_with_env_and_parses_container_ids() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("herdr");
        let log = temp.path().join("args.log");
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '%s\\n' '{{\"result\":{{\"workspace\":{{\"workspace_id\":\"w9\"}},\"root_pane\":{{\"pane_id\":\"w9:p1\"}}}}}}'\n",
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).unwrap();
        let herdr = Herdr::with_binary(&binary);
        let env = BTreeMap::from([("GH_REPO".to_owned(), "owner/repo".to_owned())]);

        let created = herdr
            .create_workspace(Path::new("/tmp/repo.feature"), "feature", &env)
            .unwrap();

        assert_eq!(created.workspace_id, "w9");
        assert_eq!(created.root_pane_id, "w9:p1");
        let args = fs::read_to_string(log).unwrap();
        assert!(args.contains("workspace create --cwd /tmp/repo.feature"));
        assert!(args.contains("--env GH_REPO=owner/repo"));
    }
}
