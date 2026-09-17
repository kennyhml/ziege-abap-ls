//! Ziege project and language server configurations, either from the environment
//! files or environment variables.
//!
//! There are several locations for Ziege files, all using TOML:
//! - A project-local `ziege.toml` file stores the connections and per-connection
//!   configuration for the local project, such as which system folders to virtualize
//!   and what mounts to use for that connection.
//! - A user-wide `$XDG_CONFIG_HOME/ziege/config.toml` file (defaulting to
//!   `~/.config/ziege/config.toml`) stores editor preferences.
//!   `ZIEGE_CONFIG` overrides this path.
//! - `destinations.toml`, beside the user configuration, stores named SAP connections.
//!
//! The corresponding schemas and loaders live in `project`, `user`, and
//! `destinations`. This module combines them into [`ResolvedSystemConfig`] and exposes
//! the shared configuration API and errors. Process arguments live in `main.rs`.

mod destinations;
mod project;
mod user;

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use thiserror::Error;
use zvfs::Mount;

pub use destinations::{DestinationConfig, DestinationId};
pub use project::{
    FacetConfig, FilterConfig, MountConfig, ProjectConfig, ProjectRoot, ProjectSystemConfig,
    load_local_project,
};
pub use user::{EditingPolicy, LockingMode, UserConfig, user_config_path};

use crate::config::destinations::load_destination;

/// Supported schema version for all configuration files.
const CONFIG_VERSION: u32 = 1;

/// The full configuration for a system to connect to.
///
/// This combines the system specifications from several configuration files
/// and forwards the data relevant to process requests to this system.
///
/// local ziege.toml → systems.DEV      ┐
/// config.toml → editing preferences   ┼> [`ResolvedSystemConfig`]
/// destinations.toml → referenced ADT  ┘
pub struct ResolvedSystemConfig {
    /// The configuration for this system from the local project config
    pub config: ProjectSystemConfig,
    /// Connection settings from `destinations.toml`.
    pub destination: DestinationConfig,
    /// The configured object editing policy for this system
    pub editing: EditingPolicy,
}

impl ResolvedSystemConfig {
    /// Builds root mounts from the resolved projects system configuration.
    pub fn mounts(&self) -> Result<Vec<Mount>, ConfigError> {
        self.config.build_mounts()
    }
}

/// Configuration discovery, parsing, and validation failures.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read `{path}`: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse `{path}`: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("unsupported {kind} configuration version {version}")]
    UnsupportedVersion { kind: &'static str, version: u32 },
    #[error("destination `{destination}` is not configured in `{path}`")]
    UnknownDestination { destination: String, path: PathBuf },
    #[error("environment variable `{variable}` required by destination `{destination}` is not set")]
    MissingEnvironment {
        destination: String,
        variable: String,
    },
    #[error("configuration field `{field}` must not be empty")]
    EmptyField { field: String },
    #[error("mount labels must not be empty")]
    EmptyMountLabel,
    #[error("package mount names must not be empty")]
    EmptyPackage,
    #[error("selection mount `{label}` requires at least one filter value")]
    EmptyFilter { label: String },
}

/// Combines a destination's project settings, user preferences and connection settings.
///
/// The destination must be configured in the project's `systems` table and in
/// `destinations.toml`. The caller supplies the user-config path selected at startup.
pub async fn load_system_configuration(
    project_root: &ProjectRoot,
    id: &DestinationId,
    user_config_path: &Path,
) -> Result<ResolvedSystemConfig, ConfigError> {
    let mut project = load_local_project(project_root.as_path()).await?;
    let config = project
        .systems
        .remove(id)
        .ok_or_else(|| ConfigError::UnknownDestination {
            destination: id.as_str().to_owned(),
            path: project_root.as_path().join(project::PROJECT_FILE),
        })?;
    let user_config = user::load_user_configuration(user_config_path).await?;
    let editing = user_config.editing_policy(config.readonly);

    let destination =
        load_destination(user_config_path.with_file_name("destinations.toml"), id).await?;

    Ok(ResolvedSystemConfig {
        config,
        destination,
        editing,
    })
}

/// Internal helper to read a .toml configuratio at the specified path.
async fn read_config<T: DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
    toml::from_str(&contents).map_err(|error| ConfigError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::project::PROJECT_FILE;
    use super::*;

    #[tokio::test]
    async fn system_configuration_requires_the_sibling_destination_file() {
        let directory = tempfile::tempdir().unwrap();
        let user_path = directory.path().join("custom.toml");
        std::fs::write(&user_path, "version = 1").unwrap();
        std::fs::write(
            directory.path().join(PROJECT_FILE),
            "version = 1\n[systems.DEV]\nreadonly = true\n",
        )
        .unwrap();
        let root = ProjectRoot::canonicalize(directory.path()).await.unwrap();
        let dev = DestinationId::from("DEV".to_owned());
        let destinations = directory.path().join("destinations.toml");
        assert!(
            matches!(load_system_configuration(&root, &dev, &user_path).await,
            Err(ConfigError::Read { path, .. }) if path == destinations)
        );
        std::fs::write(&destinations, "invalid TOML!").unwrap();
        assert!(matches!(
            load_system_configuration(&root, &dev, &user_path).await,
            Err(ConfigError::Parse { path, .. }) if path == destinations
        ));
        std::fs::write(&destinations, "version = 2\n[destinations]").unwrap();
        assert!(matches!(
            load_system_configuration(&root, &dev, &user_path).await,
            Err(ConfigError::UnsupportedVersion {
                kind: "destinations",
                version: 2
            })
        ));
        std::fs::write(&destinations, "version = 1\n[destinations]").unwrap();
        assert!(
            matches!(load_system_configuration(&root, &dev, &user_path).await,
            Err(ConfigError::UnknownDestination { path, destination }) if path == destinations && destination == "DEV")
        );
        std::fs::write(
            &destinations,
            "version = 1\n[destinations.DEV]\nurl = 'https://example.invalid'\nclient = '100'\nusername = 'DEVELOPER'\npassword = 'fixture'\n",
        ).unwrap();
        let loaded = load_system_configuration(&root, &dev, &user_path)
            .await
            .unwrap();
        assert_eq!(loaded.destination.client, "100");
        assert!(loaded.config.readonly);
        assert!(loaded.editing.readonly);

        // A catalog entry alone does not make it available in the project.
        std::fs::write(
            directory.path().join(PROJECT_FILE),
            "version = 1\n[systems.OTHER]\n",
        )
        .unwrap();
        assert!(matches!(
            load_system_configuration(&root, &dev, &user_path).await,
            Err(ConfigError::UnknownDestination { destination, path })
                if destination == "DEV" && path == directory.path().join(PROJECT_FILE)
        ));
    }
}
