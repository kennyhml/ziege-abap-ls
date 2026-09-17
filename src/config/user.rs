//! User-wide editing preferences from `config.toml`.
//!
//! Path discovery honors `ZIEGE_CONFIG`, then an absolute `XDG_CONFIG_HOME`,
//! falling back to `~/.config/ziege/config.toml`. No destinations are loaded here.

use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;

use super::{CONFIG_VERSION, ConfigError, read_config};

const USER_CONFIG_ENV: &str = "ZIEGE_CONFIG";

/// General preferences from the user-wide `config.toml`, loaded when opening a system.
#[serde_inline_default]
#[derive(Clone, Deserialize)]
pub struct UserConfig {
    /// Configuration schema version; currently 1.
    #[serde_inline_default(1)]
    version: u32,
    /// Locking preferences reserved for future editing support.
    #[serde(default)]
    editing: EditingConfig,
}

impl UserConfig {
    /// Combines user locking preferences with the projects read-only setting.
    pub(super) fn editing_policy(&self, readonly: bool) -> EditingPolicy {
        EditingPolicy {
            readonly,
            locking: self.editing.locking,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct EditingConfig {
    /// The mode to lock objects for editing
    #[serde(default)]
    locking: LockingMode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LockingMode {
    /// The file must be locked explicitly before editing is possible
    #[default]
    Explicit,
    /// The file can be written to right away, locking takes place in the background
    Implicit,
    /// The file is not locked. Optimistic concurrency control is used.
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditingPolicy {
    /// Whether files can be edited. Comes from the project configuration
    pub readonly: bool,
    /// How files are to be locked, if at all, for editing
    pub locking: LockingMode,
}

/// Loads the [`UserConfig`] at the provided path
pub(super) async fn load_user_configuration(path: &Path) -> Result<UserConfig, ConfigError> {
    let config: UserConfig = read_config(path).await?;
    if config.version != CONFIG_VERSION {
        return Err(ConfigError::UnsupportedVersion {
            kind: "user",
            version: config.version,
        });
    }
    Ok(config)
}

/// Resolves `ZIEGE_CONFIG`, then XDG, then the HOME-based default.
///
/// `destinations.toml` is always a sibling of this file, even with overrides.
pub fn user_config_path() -> Result<PathBuf, ConfigError> {
    resolve_user_config_path(
        env::var_os(USER_CONFIG_ENV),
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

/// Internal helper for the actual path resolving. This keeps the
/// possible paths clean and obvious at the callsite.
fn resolve_user_config_path(
    override_path: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, ConfigError> {
    if let Some(path) = override_path {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = xdg_config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path.join("ziege/config.toml"));
    }
    let home = home.ok_or_else(|| ConfigError::MissingEnvironment {
        destination: "global configuration".to_owned(),
        variable: "HOME".to_owned(),
    })?;
    Ok(PathBuf::from(home).join(".config/ziege/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectConfig;

    #[test]
    fn user_config_path_respects_override_xdg_and_home_fallback() {
        assert_eq!(
            resolve_user_config_path(
                Some("/custom/config.toml".into()),
                Some("/xdg".into()),
                None
            )
            .unwrap(),
            PathBuf::from("/custom/config.toml")
        );
        assert_eq!(
            resolve_user_config_path(None, Some("/xdg".into()), None).unwrap(),
            PathBuf::from("/xdg/ziege/config.toml")
        );
        for xdg in [None, Some("".into()), Some("relative".into())] {
            assert_eq!(
                resolve_user_config_path(None, xdg, Some("/home/user".into())).unwrap(),
                PathBuf::from("/home/user/.config/ziege/config.toml")
            );
        }
        assert!(
            matches!(resolve_user_config_path(None, None, None), Err(ConfigError::MissingEnvironment { variable, .. }) if variable == "HOME")
        );
    }

    #[test]
    fn parses_user_preferences() {
        let config: UserConfig = toml::from_str(
            r#"
version = 1
[editing]
locking = "implicit"
"#,
        )
        .unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.editing.locking, LockingMode::Implicit);
    }

    #[test]
    fn editing_defaults_to_explicit_without_changing_project_readonly() {
        let user: UserConfig =
            toml::from_str("version = 1\n[editing]\nlocking = 'implicit'\n").unwrap();
        let project: ProjectConfig = toml::from_str("version = 1\n[systems.DEV]\n").unwrap();
        let policy = user.editing_policy(project.systems["DEV"].readonly);
        assert_eq!(policy.locking, LockingMode::Implicit);
        assert!(!policy.readonly);
        let default: UserConfig = toml::from_str("version = 1").unwrap();
        assert_eq!(default.editing.locking, LockingMode::Explicit);
    }
}
