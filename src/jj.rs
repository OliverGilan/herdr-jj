use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::process::{checked_output, checked_output_raw, checked_status};

const STATUS_TEMPLATE: &str = concat!(
    "if(conflict, \"1\", \"\") ++ \"\\x1f\" ++ ",
    "if(empty, \"1\", \"\") ++ \"\\x1f\" ++ ",
    "change_id.shortest(12) ++ \"\\x1f\" ++ ",
    "commit_id ++ \"\\x1f\" ++ ",
    "self.diff().files().len() ++ \"\\x1f\" ++ ",
    "local_bookmarks.map(|b| b.name()).join(\" \" ) ++ \"\\x1f\" ++ ",
    "description.first_line() ++ \"\\n\""
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JjRepository {
    pub current_root: PathBuf,
    pub main_root: PathBuf,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeStatus {
    pub conflicted: bool,
    pub empty: bool,
    pub change_id: String,
    pub commit_id: String,
    pub changed_files: usize,
    pub bookmarks: Vec<String>,
    pub description: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub name: String,
    pub root: PathBuf,
    pub change_id: String,
    pub description: String,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedJjWorkspace {
    pub name: String,
    pub root: PathBuf,
    pub bookmark_created: bool,
}

impl JjRepository {
    pub fn discover(path: &Path) -> Result<Self> {
        let current_root = workspace_root(path)?;
        let current_root = fs::canonicalize(&current_root).unwrap_or(current_root);
        let main_root = resolve_main_root(&current_root)?;
        let name = main_root
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("repository")
            .to_owned();
        Ok(Self {
            current_root,
            main_root,
            name,
        })
    }

    pub fn is_main_workspace(&self) -> bool {
        self.current_root == self.main_root || self.current_root.join(".jj/repo").is_dir()
    }

    pub fn snapshot_current(&self) -> Result<ChangeStatus> {
        let mut status = Command::new("jj");
        status
            .args(["--no-pager", "-R"])
            .arg(&self.current_root)
            .arg("status");
        checked_status(&mut status, "snapshot current JJ workspace")?;
        self.change_status(&self.current_root, "origin")
    }

    pub fn current_workspace_name(&self) -> Result<String> {
        let mut command = self.read_command(&self.current_root);
        command.args([
            "workspace",
            "list",
            "-T",
            "if(target.current_working_copy(), name ++ \"\\n\", \"\")",
        ]);
        let name = checked_output(&mut command, "identify current JJ workspace")?;
        if name.is_empty() {
            bail!("could not identify current JJ workspace");
        }
        Ok(name)
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceEntry>> {
        let mut command = self.read_command(&self.main_root);
        command.args([
            "workspace",
            "list",
            "-T",
            "name ++ \"\\t\" ++ target.change_id().shortest(12) ++ \"\\t\" ++ target.description().first_line() ++ \"\\n\"",
        ]);
        let output = checked_output(&mut command, "list JJ workspaces")?;
        output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut fields = line.splitn(3, '\t');
                let name = fields.next().unwrap_or_default().to_owned();
                let change_id = fields.next().unwrap_or_default().to_owned();
                let description = fields.next().unwrap_or_default().to_owned();
                let root = self.workspace_root_for_name(&name);
                match root {
                    Ok(root) => Ok(WorkspaceEntry {
                        available: root.exists(),
                        name,
                        root,
                        change_id,
                        description,
                    }),
                    Err(_) => Ok(WorkspaceEntry {
                        name,
                        root: PathBuf::new(),
                        change_id,
                        description,
                        available: false,
                    }),
                }
            })
            .collect()
    }

    pub fn workspace_root_for_name(&self, name: &str) -> Result<PathBuf> {
        let mut command = self.read_command(&self.main_root);
        command.args(["workspace", "root", "--name", name]);
        let output = checked_output(&mut command, "resolve JJ workspace root")?;
        if output.is_empty() {
            bail!("JJ workspace {name} has no available root");
        }
        Ok(PathBuf::from(output))
    }

    pub fn create_workspace(
        &self,
        workspace_root: &Path,
        name: &str,
        parent_commit_id: &str,
        create_bookmark: bool,
    ) -> Result<CreatedJjWorkspace> {
        if !valid_workspace_name(name) {
            bail!("workspace name must match [A-Za-z0-9._/-]");
        }
        if self
            .list_workspaces()?
            .iter()
            .any(|workspace| workspace.name == name)
        {
            bail!("JJ workspace already exists: {name}");
        }

        let root = workspace_root.join(&self.name).join(path_slug(name));
        if root.exists() {
            bail!("checkout path already exists: {}", root.display());
        }
        let parent = root
            .parent()
            .context("workspace checkout has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;

        let mut add = Command::new("jj");
        add.args(["--no-pager", "-R"])
            .arg(&self.current_root)
            .args([
                "workspace",
                "add",
                "--name",
                name,
                "--revision",
                parent_commit_id,
            ])
            .arg(&root);
        if let Err(error) = checked_status(&mut add, "create JJ workspace") {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }

        if create_bookmark {
            let mut bookmark = Command::new("jj");
            bookmark.args(["--no-pager", "-R"]).arg(&root).args([
                "bookmark",
                "create",
                name,
                "--revision",
                "@",
            ]);
            if let Err(error) = checked_status(&mut bookmark, "create JJ bookmark") {
                let created = CreatedJjWorkspace {
                    name: name.to_owned(),
                    root: root.clone(),
                    bookmark_created: false,
                };
                let rollback = self.rollback_workspace(&created).err();
                return Err(match rollback {
                    Some(rollback) => error.context(format!("rollback also failed: {rollback:#}")),
                    None => error,
                });
            }
        }

        Ok(CreatedJjWorkspace {
            name: name.to_owned(),
            root,
            bookmark_created: create_bookmark,
        })
    }

    pub fn rollback_workspace(&self, created: &CreatedJjWorkspace) -> Result<()> {
        if created.bookmark_created {
            let mut bookmark = Command::new("jj");
            bookmark
                .args(["--no-pager", "-R"])
                .arg(&self.main_root)
                .args(["bookmark", "forget", &created.name]);
            checked_status(&mut bookmark, "forget transaction-created JJ bookmark")?;
        }
        let mut forget = Command::new("jj");
        forget
            .args(["--no-pager", "-R"])
            .arg(&self.main_root)
            .args(["workspace", "forget", &created.name]);
        checked_status(&mut forget, "roll back JJ workspace registration")?;
        if created.root.exists() {
            fs::remove_dir_all(&created.root)
                .with_context(|| format!("could not remove {}", created.root.display()))?;
        }
        Ok(())
    }

    pub fn remove_current_workspace(&self, workspace_name: &str) -> Result<()> {
        if self.is_main_workspace() {
            bail!("refusing to remove the main JJ workspace");
        }
        let expected = self.workspace_root_for_name(workspace_name)?;
        let expected = fs::canonicalize(&expected).unwrap_or(expected);
        if expected != self.current_root {
            bail!("current path does not match JJ workspace {workspace_name}");
        }
        if self.current_root.parent().is_none() || self.current_root == Path::new("/") {
            bail!(
                "refusing to remove unsafe path: {}",
                self.current_root.display()
            );
        }

        self.snapshot_current()?;

        let tombstone = removal_tombstone(&self.current_root)?;
        fs::rename(&self.current_root, &tombstone).with_context(|| {
            format!(
                "could not stage {} for removal",
                self.current_root.display()
            )
        })?;

        let mut forget = Command::new("jj");
        forget
            .args(["--no-pager", "-R"])
            .arg(&self.main_root)
            .args(["workspace", "forget", workspace_name]);
        if let Err(error) = checked_status(&mut forget, "forget JJ workspace") {
            let restore = fs::rename(&tombstone, &self.current_root);
            return match restore {
                Ok(()) => Err(error),
                Err(restore) => Err(error.context(format!(
                    "could not restore checkout from {}: {restore}",
                    tombstone.display()
                ))),
            };
        }

        fs::remove_dir_all(&tombstone)
            .with_context(|| format!("could not delete {}", tombstone.display()))?;
        Ok(())
    }

    pub fn change_status(&self, root: &Path, remote: &str) -> Result<ChangeStatus> {
        let mut command = self.read_command(root);
        command.args([
            "log",
            "--no-graph",
            "--revisions",
            "@",
            "--template",
            STATUS_TEMPLATE,
        ]);
        let output = checked_output_raw(&mut command, "read JJ change status")?;
        let mut fields = output.splitn(7, '\x1f');
        let conflicted = fields.next().unwrap_or_default() == "1";
        let empty = fields.next().unwrap_or_default() == "1";
        let change_id = fields.next().unwrap_or_default().to_owned();
        let commit_id = fields.next().unwrap_or_default().to_owned();
        let changed_files = fields
            .next()
            .unwrap_or_default()
            .parse::<usize>()
            .unwrap_or_default();
        let bookmarks = fields
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let description = fields.next().unwrap_or_default().trim().to_owned();
        if change_id.is_empty() || commit_id.is_empty() {
            bail!("JJ returned incomplete change status");
        }

        let (ahead, behind) = bookmarks
            .first()
            .filter(|name| valid_remote_ref(name) && valid_remote_ref(remote))
            .map(|name| {
                let ahead = self.revset_count(root, &format!("{name}@{remote}..{name}"));
                let behind = self.revset_count(root, &format!("{name}..{name}@{remote}"));
                (ahead.unwrap_or_default(), behind.unwrap_or_default())
            })
            .unwrap_or_default();

        Ok(ChangeStatus {
            conflicted,
            empty,
            change_id,
            commit_id,
            changed_files,
            bookmarks,
            description,
            ahead,
            behind,
        })
    }

    pub fn workspace_env(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        if let Some(repo) = self.github_repo() {
            env.insert("GH_REPO".to_owned(), repo);
        }
        env
    }

    fn github_repo(&self) -> Option<String> {
        let mut command = self.read_command(&self.main_root);
        command.args(["git", "remote", "list"]);
        let output = checked_output(&mut command, "list JJ Git remotes").ok()?;
        let origin = output.lines().find_map(|line| {
            let (name, url) = line.split_once(' ')?;
            (name == "origin").then_some(url.trim())
        })?;
        github_slug(origin)
    }

    fn revset_count(&self, root: &Path, revset: &str) -> Option<usize> {
        let mut command = self.read_command(root);
        command.args(["log", "--count", "--revisions", revset]);
        checked_output(&mut command, "count JJ revisions")
            .ok()?
            .parse()
            .ok()
    }

    fn read_command(&self, root: &Path) -> Command {
        let mut command = Command::new("jj");
        command
            .args(["--no-pager", "--ignore-working-copy", "-R"])
            .arg(root);
        command
    }
}

impl ChangeStatus {
    pub fn change_token(&self) -> String {
        if self.bookmarks.is_empty() {
            format!("@{}", self.change_id)
        } else {
            self.bookmarks.join(" ")
        }
    }

    pub fn status_token(&self) -> String {
        let mut values = Vec::new();
        if self.conflicted {
            values.push("!".to_owned());
        }
        if self.ahead > 0 || self.behind > 0 {
            let mut remote = String::new();
            if self.ahead > 0 {
                remote.push_str(&format!("+{}", self.ahead));
            }
            if self.behind > 0 {
                remote.push_str(&format!("-{}", self.behind));
            }
            values.push(remote);
        }
        if !self.empty {
            values.push(format!("*{}", self.changed_files));
        }
        values.join(" ")
    }
}

pub fn valid_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-/".contains(character))
}

pub fn path_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator {
            slug.push('-');
            separator = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "workspace".to_owned()
    } else {
        slug.to_owned()
    }
}

fn workspace_root(path: &Path) -> Result<PathBuf> {
    let mut command = Command::new("jj");
    command
        .args(["--no-pager", "--ignore-working-copy", "-R"])
        .arg(path)
        .args(["workspace", "root"]);
    checked_output(&mut command, "resolve JJ workspace root").map(PathBuf::from)
}

fn resolve_main_root(current_root: &Path) -> Result<PathBuf> {
    let repo_pointer = current_root.join(".jj/repo");
    if repo_pointer.is_dir() {
        return Ok(current_root.to_path_buf());
    }
    let pointer = fs::read_to_string(&repo_pointer)
        .with_context(|| format!("could not read {}", repo_pointer.display()))?;
    let store = fs::canonicalize(current_root.join(".jj").join(pointer.trim()))
        .context("could not resolve JJ repository pointer")?;
    let jj_dir = store
        .parent()
        .filter(|path| path.file_name().and_then(|value| value.to_str()) == Some(".jj"))
        .context("JJ repository pointer does not target .jj/repo")?;
    let main_root = jj_dir
        .parent()
        .context("JJ repository pointer has no workspace root")?;
    Ok(main_root.to_path_buf())
}

fn removal_tombstone(root: &Path) -> Result<PathBuf> {
    let parent = root.parent().context("workspace has no parent directory")?;
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    Ok(parent.join(format!(
        ".{name}.herdr-jj-removing-{}-{nonce}",
        std::process::id()
    )))
}

fn valid_remote_ref(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-/".contains(character))
}

fn github_slug(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        None
    } else {
        Some(format!("{owner}/{repo}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validates_workspace_names() {
        assert!(valid_workspace_name("feature/herdr-jj_1.0"));
        assert!(!valid_workspace_name("feature with spaces"));
        assert!(!valid_workspace_name(""));
    }

    #[test]
    fn creates_stable_path_slugs() {
        assert_eq!(path_slug("workspace/Herdr_JJ"), "workspace-herdr-jj");
        assert_eq!(path_slug("///"), "workspace");
    }

    #[test]
    fn parses_github_remote_urls() {
        assert_eq!(
            github_slug("git@github.com:olivergilan/herdr-jj.git"),
            Some("olivergilan/herdr-jj".to_owned())
        );
        assert_eq!(
            github_slug("https://github.com/olivergilan/herdr-jj"),
            Some("olivergilan/herdr-jj".to_owned())
        );
        assert_eq!(github_slug("https://gitlab.com/example/repo"), None);
    }

    #[test]
    fn formats_status_tokens() {
        let status = ChangeStatus {
            conflicted: true,
            empty: false,
            change_id: "abc".to_owned(),
            commit_id: "123".to_owned(),
            changed_files: 2,
            bookmarks: vec!["feature".to_owned()],
            description: String::new(),
            ahead: 3,
            behind: 1,
        };

        assert_eq!(status.change_token(), "feature");
        assert_eq!(status.status_token(), "! +3-1 *2");
    }

    #[test]
    fn creates_a_workspace_on_the_captured_current_change_and_rolls_it_back() {
        let fixture = JjFixture::new();
        fs::write(fixture.main.join("README.md"), "parent change\n").unwrap();
        let repository = JjRepository::discover(&fixture.main).unwrap();
        let parent = repository.snapshot_current().unwrap();

        let created = repository
            .create_workspace(&fixture.workspaces, "feature/api", &parent.commit_id, false)
            .unwrap();

        assert!(created.root.join("README.md").exists());
        let child = JjRepository::discover(&created.root).unwrap();
        assert_eq!(child.main_root, repository.main_root);
        assert_eq!(child.current_workspace_name().unwrap(), "feature/api");
        assert_eq!(
            jj_output(
                &created.root,
                &["log", "-r", "@-", "--no-graph", "-T", "commit_id"]
            ),
            parent.commit_id
        );

        repository.rollback_workspace(&created).unwrap();
        assert!(!created.root.exists());
        assert!(
            repository
                .list_workspaces()
                .unwrap()
                .iter()
                .all(|workspace| workspace.name != "feature/api")
        );
    }

    #[test]
    fn optional_bookmark_is_created_and_removed_with_rollback() {
        let fixture = JjFixture::new();
        let repository = JjRepository::discover(&fixture.main).unwrap();
        let parent = repository.snapshot_current().unwrap();

        let created = repository
            .create_workspace(
                &fixture.workspaces,
                "feature-bookmark",
                &parent.commit_id,
                true,
            )
            .unwrap();

        let status = repository.change_status(&created.root, "origin").unwrap();
        assert_eq!(status.bookmarks, ["feature-bookmark"]);

        repository.rollback_workspace(&created).unwrap();
        let bookmarks = jj_output(&fixture.main, &["bookmark", "list", "feature-bookmark"]);
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn removes_a_changed_unbookmarked_workspace_after_snapshot() {
        let fixture = JjFixture::new();
        fs::write(fixture.main.join(".gitignore"), "ignored.log\n").unwrap();
        let repository = JjRepository::discover(&fixture.main).unwrap();
        let parent = repository.snapshot_current().unwrap();
        let created = repository
            .create_workspace(&fixture.workspaces, "throwaway", &parent.commit_id, false)
            .unwrap();
        fs::write(created.root.join("changed.txt"), "recoverable in JJ\n").unwrap();
        fs::write(created.root.join("ignored.log"), "deleted with checkout\n").unwrap();
        let child = JjRepository::discover(&created.root).unwrap();
        let removed_change = child.snapshot_current().unwrap();

        child.remove_current_workspace("throwaway").unwrap();

        assert!(!created.root.exists());
        assert!(
            repository
                .list_workspaces()
                .unwrap()
                .iter()
                .all(|workspace| workspace.name != "throwaway")
        );
        assert_eq!(
            jj_output(
                &fixture.main,
                &[
                    "log",
                    "-r",
                    &removed_change.commit_id,
                    "--no-graph",
                    "-T",
                    "commit_id"
                ]
            ),
            removed_change.commit_id
        );
    }

    #[test]
    fn refuses_to_remove_the_main_workspace() {
        let fixture = JjFixture::new();
        let repository = JjRepository::discover(&fixture.main).unwrap();

        let error = repository.remove_current_workspace("default").unwrap_err();

        assert!(error.to_string().contains("main JJ workspace"));
        assert!(fixture.main.exists());
    }

    struct JjFixture {
        _temp: TempDir,
        main: PathBuf,
        workspaces: PathBuf,
    }

    impl JjFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let main = temp.path().join("repo");
            let status = Command::new("jj")
                .args(["git", "init", "--no-colocate"])
                .arg(&main)
                .status()
                .unwrap();
            assert!(status.success());
            Self {
                workspaces: temp.path().join("workspaces"),
                _temp: temp,
                main,
            }
        }
    }

    fn jj_output(root: &Path, args: &[&str]) -> String {
        let output = Command::new("jj")
            .args(["--no-pager", "--ignore-working-copy", "-R"])
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "jj failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}
