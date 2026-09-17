//! Project-local systems, mounts, and repository selection from `ziege.toml`.
//!
//! Loading this file only parses project settings. Destination resolution and
//! user preferences are combined later by [`super::load_system_configuration`].

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use zadt::{RepositoryFacet, RepositoryPreselection};
use zvfs::{FacetLevel, FacetPolicy, Mount};

use super::{CONFIG_VERSION, ConfigError, DestinationId, read_config};

pub(super) const PROJECT_FILE: &str = "ziege.toml";

/// Canonical project path used to identify a repository view across clients.
/// Construction resolves symlinks and relative path components before lookup.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectRoot(PathBuf);

impl ProjectRoot {
    /// Resolves a filesystem path, for example `project/../project`, to its
    /// canonical identity. URI decoding belongs to the protocol boundary.
    pub async fn canonicalize(path: &Path) -> std::io::Result<Self> {
        tokio::fs::canonicalize(path).await.map(Self)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Configuration via the project-local `ziege.toml` file.
#[derive(Clone, Debug, Deserialize)]
pub struct ProjectConfig {
    /// Version of the configuration format
    version: u32,

    /// Per destination configuration. BTree keeps a deterministic order.
    pub systems: BTreeMap<DestinationId, ProjectSystemConfig>,
}

/// Configuration for a system inside a project
#[derive(Clone, Debug, Deserialize, Hash, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSystemConfig {
    /// Name of the folder that presents the portal to the system
    #[serde(default)]
    pub folder: Option<String>,
    /// Editing preference, currently unused by navigation-only browsing.
    #[serde(default)]
    pub readonly: bool,
    /// Root mounts for the repository. Empty means a System Library mount.
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
}

impl ProjectSystemConfig {
    /// Builds configured mounts, using System Library when no mounts are specified.
    pub(super) fn build_mounts(&self) -> Result<Vec<Mount>, ConfigError> {
        if self.mounts.is_empty() {
            return Ok(vec![Mount::system_library("System Library")]);
        }
        self.mounts.iter().map(MountConfig::build).collect()
    }
}

/// A root entry in a real repository, optionally organized by repository facets.
///
/// `ZVFS` explains the concept of mounts in its README.
#[serde_inline_default]
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MountConfig {
    SystemLibrary {
        #[serde_inline_default("System Library".to_owned())]
        label: String,
        #[serde(default)]
        facets: Option<Vec<FacetConfig>>,
    },
    Package {
        package: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        facets: Option<Vec<FacetConfig>>,
    },
    Selection {
        label: String,
        filters: Vec<FilterConfig>,
        #[serde(default)]
        facets: Option<Vec<FacetConfig>>,
    },
}

/// A facet name, or a detailed facet with an adaptive object-count threshold.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FacetConfig {
    Always(String),
    Adaptive { facet: String, minimum_objects: u32 },
}

impl From<&FacetConfig> for FacetLevel {
    fn from(config: &FacetConfig) -> Self {
        match config {
            FacetConfig::Always(facet) => FacetLevel::always(RepositoryFacet::from(facet.as_str())),
            FacetConfig::Adaptive {
                facet,
                minimum_objects,
            } => FacetLevel::adaptive(RepositoryFacet::from(facet.as_str()), *minimum_objects),
        }
    }
}

/// Inclusion and exclusion values for one selection-mount facet.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FilterConfig {
    pub facet: String,
    pub values: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Reads `ziege.toml` from a project directory without opening any ADT connections.
pub async fn load_local_project(project_root: &Path) -> Result<ProjectConfig, ConfigError> {
    let config: ProjectConfig = read_config(&project_root.join(PROJECT_FILE)).await?;
    if config.version != CONFIG_VERSION {
        return Err(ConfigError::UnsupportedVersion {
            kind: "project",
            version: config.version,
        });
    }
    Ok(config)
}

impl MountConfig {
    /// Builds a [`zvfs::Mount`] from the mount configuration.
    pub(crate) fn build(&self) -> Result<Mount, ConfigError> {
        let (mount, facets) = match self {
            Self::SystemLibrary { label, facets } => (Mount::system_library(label), facets),
            Self::Package {
                package,
                label,
                facets,
            } => {
                if let Some(label) = label {
                    (Mount::named_package(label, package), facets)
                } else {
                    (Mount::package(package), facets)
                }
            }
            Self::Selection {
                label,
                filters,
                facets,
            } => {
                let mut preselections = Vec::with_capacity(filters.len());
                for filter in filters {
                    let Some((first, rest)) = filter.values.split_first() else {
                        return Err(ConfigError::EmptyFilter {
                            label: label.clone(),
                        });
                    };
                    preselections.push(
                        RepositoryPreselection::new(filter.facet.as_str(), first.as_str())
                            .extend_include(rest.to_owned())
                            .extend_exclude(filter.exclude.to_owned()),
                    );
                }
                (Mount::selection(label, preselections), facets)
            }
        };

        // We cant render empty folder labels
        if mount.label().is_empty() {
            return Err(ConfigError::EmptyMountLabel);
        }

        Ok(match facets {
            None => mount,
            Some(facets) => mount.facet_policy(FacetPolicy::new(facets.iter().map(Into::into))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_destination_aliases_and_duplicate_systems() {
        for config in [
            "version = 1\n[systems.A]\ndestination = 'DEV'\n",
            "version = 1\n[systems.DEV]\n[systems.DEV]\n",
        ] {
            assert!(toml::from_str::<ProjectConfig>(config).is_err());
        }
    }

    #[test]
    fn parses_project_mounts() {
        let config: ProjectConfig = toml::from_str(
            r#"
version = 1
[systems.DEV]
folder = "SAP-DEV"

[[systems.DEV.mounts]]
kind = "package"
label = "Flight"
package = "/DMO/FLIGHT"
facets = ["GROUP", "TYPE"]

[[systems.DEV.mounts]]
kind = "selection"
label = "Local Objects"
filters = [{ facet = "PACKAGE", values = ["..$TMP"] }]
facets = ["OWNER", { facet = "TYPE", minimum_objects = 10 }]
"#,
        )
        .unwrap();
        let system = &config.systems["DEV"];
        assert_eq!(system.folder.as_deref(), Some("SAP-DEV"));
        assert!(!system.readonly);
        assert_eq!(system.mounts.len(), 2);
        assert_eq!(system.mounts[0].build().unwrap().label(), "Flight");
    }

    #[test]
    fn project_systems_default_to_editable_and_allow_readonly() {
        let default: ProjectConfig = toml::from_str("version = 1\n[systems.DEV]\n").unwrap();
        let readonly: ProjectConfig =
            toml::from_str("version = 1\n[systems.DEV]\nreadonly = true\n").unwrap();
        assert!(!default.systems["DEV"].readonly);
        assert!(readonly.systems["DEV"].readonly);
    }

    #[tokio::test]
    async fn loads_project_configuration_from_a_directory_path() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(PROJECT_FILE),
            "version = 1\n[systems.DEV]\n[systems.dev]\n",
        )
        .unwrap();
        let config = load_local_project(directory.path()).await.unwrap();
        assert_eq!(
            config
                .systems
                .keys()
                .map(DestinationId::as_str)
                .collect::<Vec<_>>(),
            ["DEV", "dev"]
        );
    }
}
