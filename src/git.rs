use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::process::{checked_output, checked_status};

/// Move a linked Git worktree so that Git's metadata follows the checkout.
/// Returns false when Git refuses, for example for a worktree that contains
/// submodules.
pub fn move_worktree(repository: &Path, from: &Path, to: &Path) -> bool {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .args(["worktree", "move"])
        .arg(from)
        .arg(to);
    checked_status(&mut command, "move Git worktree").is_ok()
}

/// The lock of a linked Git worktree: `Some` with the reason, possibly empty,
/// when `git worktree lock` was used on it. Git keeps the lock as a `locked`
/// file in the worktree's administrative directory.
pub fn lock_reason(checkout: &Path) -> Option<String> {
    let admin_dir = rev_parse_path(checkout, "--git-dir")?;
    fs::read_to_string(admin_dir.join("locked"))
        .ok()
        .map(|reason| reason.trim().to_owned())
}

/// Delete a staged checkout. A checkout that is still a linked Git worktree
/// is removed through Git so that its metadata goes with it. If Git refuses,
/// for example for a worktree with submodules, the files are deleted directly
/// and Git's stale entry is pruned.
pub fn remove_checkout(path: &Path) -> Result<()> {
    let common_dir = path
        .join(".git")
        .is_file()
        .then(|| rev_parse_path(path, "--git-common-dir"))
        .flatten();
    if let Some(common_dir) = &common_dir {
        let mut remove = git_command(common_dir);
        remove.args(["worktree", "remove", "--force"]).arg(path);
        if checked_status(&mut remove, "remove Git worktree").is_ok() {
            return Ok(());
        }
    }
    fs::remove_dir_all(path)
        .with_context(|| format!("could not delete staged checkout {}", path.display()))?;
    if let Some(common_dir) = &common_dir {
        let mut prune = git_command(common_dir);
        prune.args(["worktree", "prune"]);
        let _ = checked_status(&mut prune, "prune Git worktrees");
    }
    Ok(())
}

fn rev_parse_path(checkout: &Path, option: &str) -> Option<PathBuf> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--path-format=absolute", option]);
    checked_output(&mut command, "resolve Git directory")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn git_command(common_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("--git-dir").arg(common_dir);
    command
}
