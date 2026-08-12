use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub workspace_root: PathBuf,
    pub create_bookmark: bool,
    pub post_create: Option<String>,
    pub status_remote: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    workspace_root: Option<String>,
    create_bookmark: Option<bool>,
    post_create: Option<String>,
    status_remote: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = env::var_os("HOME").map(PathBuf::from);
        let Some(config_dir) = env::var_os("HERDR_PLUGIN_CONFIG_DIR") else {
            return Self::from_raw(RawConfig::default(), home.as_deref()).validate();
        };
        let path = PathBuf::from(config_dir).join("config.toml");
        if !path.exists() {
            return Self::from_raw(RawConfig::default(), home.as_deref()).validate();
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        Self::parse(&contents, home.as_deref())
            .with_context(|| format!("invalid plugin config at {}", path.display()))
    }

    fn parse(contents: &str, home: Option<&Path>) -> Result<Self> {
        let raw = toml::from_str(contents)?;
        Self::from_raw(raw, home).validate()
    }

    fn validate(self) -> Result<Self> {
        if !self.workspace_root.is_absolute() {
            anyhow::bail!(
                "workspace_root must be an absolute path or start with '~/': {}",
                self.workspace_root.display()
            );
        }
        Ok(self)
    }

    fn from_raw(raw: RawConfig, home: Option<&Path>) -> Self {
        let workspace_root = raw
            .workspace_root
            .unwrap_or_else(|| "~/.herdr/jj-workspaces".to_owned());
        let workspace_root = expand_tilde(&workspace_root, home);
        let post_create = raw
            .post_create
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let status_remote = raw
            .status_remote
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "origin".to_owned());
        Self {
            workspace_root,
            create_bookmark: raw.create_bookmark.unwrap_or(false),
            post_create,
            status_remote,
        }
    }
}

fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_jj_native() {
        let config = Config::parse("", Some(Path::new("/home/test"))).unwrap();

        assert_eq!(
            config.workspace_root,
            PathBuf::from("/home/test/.herdr/jj-workspaces")
        );
        assert!(!config.create_bookmark);
        assert_eq!(config.status_remote, "origin");
        assert_eq!(config.post_create, None);
    }

    #[test]
    fn parses_user_options() {
        let config = Config::parse(
            r#"
workspace_root = "/workspaces"
create_bookmark = true
post_create = "npm install"
status_remote = "upstream"
"#,
            None,
        )
        .unwrap();

        assert_eq!(config.workspace_root, PathBuf::from("/workspaces"));
        assert!(config.create_bookmark);
        assert_eq!(config.post_create.as_deref(), Some("npm install"));
        assert_eq!(config.status_remote, "upstream");
    }

    #[test]
    fn rejects_relative_workspace_roots() {
        let error = Config::parse("workspace_root = 'workspaces'", None).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("workspace_root must be an absolute path")
        );
    }
}
