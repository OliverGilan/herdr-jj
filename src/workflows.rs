use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::herdr::{Herdr, InvocationContext, Snapshot, required_source};
use crate::jj::JjRepository;
use crate::ui::{
    CreateDialog, OpenEntry, create_dialog, generated_name, open_dialog, remove_dialog,
};

const SOURCE_CWD: &str = "HERDR_JJ_SOURCE_CWD";
const SOURCE_WORKSPACE_ID: &str = "HERDR_JJ_SOURCE_WORKSPACE_ID";

pub fn open_action(action: &str) -> Result<()> {
    if !matches!(action, "create" | "open" | "remove") {
        anyhow::bail!("unknown action: {action}");
    }
    let context = InvocationContext::from_env()?;
    let (cwd, workspace_id) = required_source(&context)?;
    let herdr = Herdr::from_env();
    herdr.open_popup(
        action,
        &[
            (SOURCE_CWD, cwd.as_os_str()),
            (SOURCE_WORKSPACE_ID, OsStr::new(workspace_id)),
        ],
    )
}

pub fn run_pane(pane: &str) -> Result<()> {
    match pane {
        "create" => create_workspace(),
        "open" => open_workspace(),
        "remove" => remove_workspace(),
        _ => anyhow::bail!("unknown pane workflow: {pane}"),
    }
}

pub fn refresh_status() -> Result<()> {
    let config = Config::load()?;
    let herdr = Herdr::from_env();
    let event = env::var("HERDR_PLUGIN_EVENT").unwrap_or_default();

    if matches!(event.as_str(), "workspace.created" | "workspace.focused")
        && let Ok(context) = InvocationContext::from_env()
        && let (Some(workspace_id), Some(cwd)) =
            (context.workspace_id.as_deref(), context.source_cwd())
    {
        return report_one(&herdr, workspace_id, cwd, &config.status_remote);
    }

    let snapshot = herdr.snapshot()?;
    for workspace in &snapshot.workspaces {
        let cwd = cwd_for_workspace(&snapshot, &workspace.workspace_id, &workspace.active_tab_id);
        let Some(cwd) = cwd else {
            continue;
        };
        if let Err(error) = report_one(&herdr, &workspace.workspace_id, cwd, &config.status_remote)
        {
            eprintln!(
                "warning: could not refresh {}: {error:#}",
                workspace.workspace_id
            );
        }
    }
    Ok(())
}

fn create_workspace() -> Result<()> {
    let config = Config::load()?;
    let source = required_env_path(SOURCE_CWD)?;
    let repository = JjRepository::discover(&source)?;
    let parent = repository.snapshot_current()?;
    let Some(choice) = create_dialog(CreateDialog {
        initial_name: generated_name(seed()),
        create_bookmark: config.create_bookmark,
    })?
    else {
        return Ok(());
    };

    let created = repository.create_workspace(
        &config.workspace_root,
        &choice.name,
        &parent.commit_id,
        choice.create_bookmark,
    )?;
    let herdr = Herdr::from_env();
    let herdr_workspace =
        match herdr.create_workspace(&created.root, &created.name, &repository.workspace_env()) {
            Ok(workspace) => workspace,
            Err(error) => {
                let rollback = repository.rollback_workspace(&created);
                return match rollback {
                    Ok(()) => Err(error.context("JJ workspace was rolled back")),
                    Err(rollback) => Err(error.context(format!(
                        "JJ rollback also failed: {rollback:#}; checkout remains at {}",
                        created.root.display()
                    ))),
                };
            }
        };

    if let Some(command) = config.post_create.as_deref() {
        herdr
            .run_in_pane(&herdr_workspace.root_pane_id, command)
            .with_context(|| {
                format!(
                    "workspace {} was created, but its post-create command was not submitted",
                    herdr_workspace.workspace_id
                )
            })?;
    }
    Ok(())
}

fn open_workspace() -> Result<()> {
    let source = required_env_path(SOURCE_CWD)?;
    let repository = JjRepository::discover(&source)?;
    let herdr = Herdr::from_env();
    let snapshot = herdr.snapshot()?;
    let entries = repository
        .list_workspaces()?
        .into_iter()
        .map(|workspace| OpenEntry {
            open_workspace_id: workspace
                .available
                .then(|| herdr.find_workspace_for_root(&snapshot, &workspace.root))
                .flatten(),
            workspace,
        })
        .collect();
    let Some(choice) = open_dialog(entries)? else {
        return Ok(());
    };

    if let Some(workspace_id) = choice.open_workspace_id {
        herdr.focus_workspace(&workspace_id)
    } else {
        herdr
            .create_workspace(
                &choice.workspace.root,
                &choice.workspace.name,
                &repository.workspace_env(),
            )
            .map(|_| ())
    }
}

fn remove_workspace() -> Result<()> {
    let source = required_env_path(SOURCE_CWD)?;
    let workspace_id =
        env::var(SOURCE_WORKSPACE_ID).context("missing source HerdR workspace ID")?;
    let repository = JjRepository::discover(&source)?;
    if repository.is_main_workspace() {
        anyhow::bail!("refusing to remove the main JJ workspace");
    }
    let workspace_name = repository.current_workspace_name()?;
    if !remove_dialog()? {
        return Ok(());
    }

    let staged_checkout = repository.stage_current_workspace_removal(&workspace_name)?;
    spawn_cleanup(&staged_checkout)?;
    Herdr::from_env()
        .close_workspace(&workspace_id)
        .context("JJ workspace was removed, but its HerdR workspace could not be closed")
}

pub fn cleanup(path: &Path) -> Result<()> {
    let config = Config::load()?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !path.is_absolute()
        || !path.starts_with(&config.workspace_root)
        || !name.starts_with('.')
        || !name.contains(".herdr-jj-removing-")
    {
        anyhow::bail!(
            "refusing to delete invalid cleanup path: {}",
            path.display()
        );
    }
    std::fs::remove_dir_all(path)
        .with_context(|| format!("could not delete staged checkout {}", path.display()))
}

fn spawn_cleanup(path: &Path) -> Result<()> {
    let executable = env::current_exe().context("could not locate the plugin executable")?;
    let mut command = Command::new(executable);
    command
        .arg("cleanup")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .context("could not start background checkout cleanup")?;
    Ok(())
}

fn report_one(herdr: &Herdr, workspace_id: &str, cwd: &Path, remote: &str) -> Result<()> {
    let status =
        JjRepository::discover(cwd).and_then(|repository| repository.change_status(cwd, remote));
    match status {
        Ok(status) => herdr.report_status(
            workspace_id,
            Some(&status.change_token()),
            Some(&status.status_token()),
        ),
        Err(_) => herdr.report_status(workspace_id, None, None),
    }
}

fn cwd_for_workspace<'a>(
    snapshot: &'a Snapshot,
    workspace_id: &str,
    active_tab_id: &str,
) -> Option<&'a Path> {
    snapshot
        .panes
        .iter()
        .filter(|pane| pane.workspace_id == workspace_id && pane.tab_id == active_tab_id)
        .find(|pane| pane.focused)
        .or_else(|| {
            snapshot
                .panes
                .iter()
                .find(|pane| pane.workspace_id == workspace_id && pane.tab_id == active_tab_id)
        })
        .and_then(|pane| pane.cwd.as_deref())
}

fn required_env_path(name: &str) -> Result<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .with_context(|| format!("missing {name}"))
}

fn seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{SnapshotPane, SnapshotWorkspace};

    #[test]
    fn status_refresh_prefers_the_active_tabs_focused_pane() {
        let snapshot = Snapshot {
            workspaces: vec![SnapshotWorkspace {
                workspace_id: "w1".to_owned(),
                active_tab_id: "w1:t2".to_owned(),
            }],
            panes: vec![
                SnapshotPane {
                    workspace_id: "w1".to_owned(),
                    tab_id: "w1:t1".to_owned(),
                    cwd: Some(PathBuf::from("/old")),
                    focused: false,
                },
                SnapshotPane {
                    workspace_id: "w1".to_owned(),
                    tab_id: "w1:t2".to_owned(),
                    cwd: Some(PathBuf::from("/active")),
                    focused: true,
                },
            ],
        };

        assert_eq!(
            cwd_for_workspace(&snapshot, "w1", "w1:t2"),
            Some(Path::new("/active"))
        );
    }
}
