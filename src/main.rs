mod herdr;
mod jj;
mod process;
mod ui;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::herdr::{Herdr, InvocationContext};
use crate::jj::JjRepository;
use crate::ui::{OpenItem, create_dialog, open_dialog, remove_dialog};

const SOURCE_CWD: &str = "HERDR_JJ_SOURCE_CWD";
const SOURCE_WORKSPACE_ID: &str = "HERDR_JJ_SOURCE_WORKSPACE_ID";

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let interactive = args.first().is_some_and(|value| value == "pane");
    let result = match args.as_slice() {
        [command] if command == "refresh" => refresh_status(),
        [command, path] if command == "cleanup" => cleanup(Path::new(path)),
        [command, name] if command == "action" => action(name),
        [command, name] if command == "pane" => pane(name),
        _ => Err(anyhow::anyhow!(
            "usage: herdr-jj <refresh | cleanup <path> | action <create|open|remove> | pane <create|open|remove>>"
        )),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            if interactive {
                eprint!("\nPress Enter to close...");
                let _ = io::stderr().flush();
                let mut line = String::new();
                let _ = io::stdin().read_line(&mut line);
            }
            ExitCode::FAILURE
        }
    }
}

fn action(name: &str) -> Result<()> {
    if !matches!(name, "create" | "open" | "remove") {
        anyhow::bail!("unknown action: {name}");
    }
    let context = InvocationContext::from_env()?;
    let (cwd, workspace_id) = context.required_source()?;
    Herdr::from_env().open_popup(
        name,
        &[
            (SOURCE_CWD, cwd.as_os_str()),
            (SOURCE_WORKSPACE_ID, OsStr::new(workspace_id)),
        ],
    )
}

fn pane(name: &str) -> Result<()> {
    match name {
        "create" => create_workspace(),
        "open" => open_workspace(),
        "remove" => remove_workspace(),
        _ => anyhow::bail!("unknown pane workflow: {name}"),
    }
}

fn refresh_status() -> Result<()> {
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
        let Some(cwd) = snapshot.cwd_for_workspace(workspace) else {
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
    let parent = repository.capture_current_commit()?;
    let Some(choice) = create_dialog(config.create_bookmark)? else {
        return Ok(());
    };

    let created = repository.create_workspace(
        &config.workspace_root,
        &choice.name,
        &parent,
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
    let workspaces = repository.list_workspaces()?;
    let entries = workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| OpenItem {
            index,
            name: &workspace.name,
            change_id: &workspace.change_id,
            description: &workspace.description,
            available: workspace.available,
            open_workspace_id: workspace
                .available
                .then(|| snapshot.workspace_for_root(&workspace.root))
                .flatten(),
        })
        .collect::<Vec<_>>();
    let Some(selected) = open_dialog(&entries)? else {
        return Ok(());
    };
    let workspace = &workspaces[selected];

    if let Some(workspace_id) = entries[selected].open_workspace_id.as_deref() {
        herdr.focus_workspace(workspace_id)
    } else {
        herdr
            .create_workspace(
                &workspace.root,
                &workspace.name,
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

fn cleanup(path: &Path) -> Result<()> {
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
    fs::remove_dir_all(path)
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
    let tokens =
        JjRepository::discover(cwd).and_then(|repository| repository.sidebar_tokens(cwd, remote));
    match tokens {
        Ok(tokens) => herdr.report_status(workspace_id, Some(&tokens.change), Some(&tokens.status)),
        Err(_) => herdr.report_status(workspace_id, None, None),
    }
}

fn required_env_path(name: &str) -> Result<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .with_context(|| format!("missing {name}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_workspace_root")]
    workspace_root: PathBuf,
    #[serde(default)]
    create_bookmark: bool,
    #[serde(default, deserialize_with = "empty_is_none")]
    post_create: Option<String>,
    #[serde(default = "default_remote")]
    status_remote: String,
}

impl Config {
    fn load() -> Result<Self> {
        let path = env::var_os("HERDR_PLUGIN_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("config.toml"));
        let mut config = match path.as_deref() {
            Some(path) if path.exists() => {
                let contents = fs::read_to_string(path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                toml::from_str(&contents)
                    .with_context(|| format!("invalid plugin config at {}", path.display()))?
            }
            _ => Self::default(),
        };
        config.workspace_root = expand_tilde(config.workspace_root);
        config.status_remote = config.status_remote.trim().to_owned();
        if config.status_remote.is_empty() {
            config.status_remote = default_remote();
        }
        if !config.workspace_root.is_absolute() {
            anyhow::bail!(
                "workspace_root must be an absolute path or start with '~/': {}",
                config.workspace_root.display()
            );
        }
        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workspace_root: default_workspace_root(),
            create_bookmark: false,
            post_create: None,
            status_remote: default_remote(),
        }
    }
}

fn default_workspace_root() -> PathBuf {
    PathBuf::from("~/.herdr/jj-workspaces")
}

fn default_remote() -> String {
    "origin".to_owned()
}

fn empty_is_none<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer).map(|value| {
        value
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let Some(path) = path.to_str() else {
        return path;
    };
    match (path.strip_prefix("~/"), env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}
