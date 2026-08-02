use std::{
    collections::BTreeMap,
    env,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use zadt::{RepositoryFacet, RepositoryPreselection};
use zvfs::{FacetLevel, FacetPolicy, Mount};

const PROJECT_FILE: &str = ".ziege";
const USER_CONFIG_ENV: &str = "ZIEGE_CONFIG";

#[derive(Clone, Debug, Deserialize)]
pub struct ProjectConfig {
    #[serde(default = "config_version")]
    version: u32,
    pub systems: BTreeMap<String, ProjectSystemConfig>,
}

#[derive(Clone, Debug, Deserialize, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSystemConfig {
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
}

#[derive(Clone, Debug, Deserialize, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MountConfig {
    SystemLibrary {
        #[serde(default = "default_system_library_label")]
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

#[derive(Clone, Debug, Deserialize, Hash, Serialize)]
#[serde(untagged)]
pub enum FacetConfig {
    Always(String),
    Detailed {
        facet: String,
        #[serde(default, rename = "minimumObjects")]
        minimum_objects: Option<u32>,
    },
}

#[derive(Clone, Debug, Deserialize, Hash, Serialize)]
pub struct FilterConfig {
    pub facet: String,
    pub values: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Clone, Deserialize)]
pub struct UserConfig {
    #[serde(default = "config_version")]
    version: u32,
    destinations: BTreeMap<String, DestinationConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct DestinationConfig {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    url_env: Option<String>,
    #[serde(default)]
    client: Option<String>,
    #[serde(default)]
    client_env: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    language_env: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    username_env: Option<String>,
    password_env: String,
}

#[derive(Clone, Hash)]
pub struct ResolvedDestination {
    pub url: String,
    pub client: String,
    pub language: String,
    pub username: String,
    pub password: String,
}

pub struct LoadedSystem {
    pub project_root: PathBuf,
    pub system_id: String,
    pub config: ProjectSystemConfig,
    pub destination: ResolvedDestination,
}

impl LoadedSystem {
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.config.hash(&mut hasher);
        self.destination.hash(&mut hasher);
        hasher.finish()
    }

    pub fn mounts(&self) -> Result<Vec<Mount>, ConfigError> {
        if self.config.mounts.is_empty() {
            return Ok(vec![Mount::system_library(default_system_library_label())]);
        }
        self.config.mounts.iter().map(MountConfig::build).collect()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid project URI `{0}`")]
    InvalidProjectUri(String),
    #[error("project URI must use the file scheme: `{0}`")]
    UnsupportedProjectUri(String),
    #[error("cannot read `{path}`: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse `{path}`: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("unsupported {kind} configuration version {version}")]
    UnsupportedVersion { kind: &'static str, version: u32 },
    #[error("system `{system}` is not configured in `{path}`")]
    UnknownSystem { system: String, path: PathBuf },
    #[error("destination `{destination}` is not configured in `{path}`")]
    UnknownDestination { destination: String, path: PathBuf },
    #[error("destination `{destination}` must configure exactly one of `{field}` or `{field}_env`")]
    InvalidDestinationField {
        destination: String,
        field: &'static str,
    },
    #[error("environment variable `{variable}` required by destination `{destination}` is not set")]
    MissingEnvironment {
        destination: String,
        variable: String,
    },
    #[error("destination `{destination}` has an empty `{field}` value")]
    EmptyDestinationField {
        destination: String,
        field: &'static str,
    },
    #[error("mount labels must not be empty")]
    EmptyMountLabel,
    #[error("package mount names must not be empty")]
    EmptyPackage,
    #[error("selection mount `{label}` requires at least one filter value")]
    EmptyFilter { label: String },
}

pub async fn load_project(project_uri: &str) -> Result<(PathBuf, ProjectConfig), ConfigError> {
    let root = project_root(project_uri).await?;
    let path = root.join(PROJECT_FILE);
    let contents = tokio::fs::read_to_string(&path)
        .await
        .map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
    let config: ProjectConfig =
        serde_yaml_ng::from_str(&contents).map_err(|error| ConfigError::Parse {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if config.version != config_version() {
        return Err(ConfigError::UnsupportedVersion {
            kind: "project",
            version: config.version,
        });
    }
    Ok((root, config))
}

pub async fn load_system(project_uri: &str, system_id: &str) -> Result<LoadedSystem, ConfigError> {
    let (project_root, project) = load_project(project_uri).await?;
    let project_path = project_root.join(PROJECT_FILE);
    let config =
        project
            .systems
            .get(system_id)
            .cloned()
            .ok_or_else(|| ConfigError::UnknownSystem {
                system: system_id.to_owned(),
                path: project_path,
            })?;
    let destination_id = config.destination.as_deref().unwrap_or(system_id);
    let user_path = user_config_path()?;
    let contents = tokio::fs::read_to_string(&user_path)
        .await
        .map_err(|source| ConfigError::Read {
            path: user_path.clone(),
            source,
        })?;
    let user: UserConfig = toml::from_str(&contents).map_err(|error| ConfigError::Parse {
        path: user_path.clone(),
        message: error.to_string(),
    })?;
    if user.version != config_version() {
        return Err(ConfigError::UnsupportedVersion {
            kind: "user",
            version: user.version,
        });
    }
    let destination = user
        .destinations
        .get(destination_id)
        .ok_or_else(|| ConfigError::UnknownDestination {
            destination: destination_id.to_owned(),
            path: user_path,
        })?
        .resolve(destination_id)?;

    Ok(LoadedSystem {
        project_root,
        system_id: system_id.to_owned(),
        config,
        destination,
    })
}

pub fn user_config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os(USER_CONFIG_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME").ok_or_else(|| ConfigError::MissingEnvironment {
        destination: "global configuration".to_owned(),
        variable: "HOME".to_owned(),
    })?;
    Ok(PathBuf::from(home).join(".ziegerc"))
}

async fn project_root(project_uri: &str) -> Result<PathBuf, ConfigError> {
    let uri = Url::parse(project_uri)
        .map_err(|_| ConfigError::InvalidProjectUri(project_uri.to_owned()))?;
    let path = uri
        .to_file_path()
        .map_err(|_| ConfigError::UnsupportedProjectUri(project_uri.to_owned()))?;
    tokio::fs::canonicalize(&path)
        .await
        .map_err(|source| ConfigError::Read { path, source })
}

impl DestinationConfig {
    fn resolve(&self, destination: &str) -> Result<ResolvedDestination, ConfigError> {
        Ok(ResolvedDestination {
            url: resolve_value(destination, "url", &self.url, &self.url_env, None)?,
            client: resolve_value(destination, "client", &self.client, &self.client_env, None)?,
            language: resolve_value(
                destination,
                "language",
                &self.language,
                &self.language_env,
                Some("EN"),
            )?,
            username: resolve_value(
                destination,
                "username",
                &self.username,
                &self.username_env,
                None,
            )?,
            password: environment_value(destination, &self.password_env)?,
        })
    }
}

fn resolve_value(
    destination: &str,
    field: &'static str,
    literal: &Option<String>,
    variable: &Option<String>,
    default: Option<&str>,
) -> Result<String, ConfigError> {
    match (literal, variable) {
        (Some(_), Some(_)) => Err(ConfigError::InvalidDestinationField {
            destination: destination.to_owned(),
            field,
        }),
        (Some(value), None) => nonempty(destination, field, value.clone()),
        (None, Some(variable)) => environment_value(destination, variable),
        (None, None) => {
            default
                .map(str::to_owned)
                .ok_or_else(|| ConfigError::InvalidDestinationField {
                    destination: destination.to_owned(),
                    field,
                })
        }
    }
}

fn environment_value(destination: &str, variable: &str) -> Result<String, ConfigError> {
    let value = env::var(variable).map_err(|_| ConfigError::MissingEnvironment {
        destination: destination.to_owned(),
        variable: variable.to_owned(),
    })?;
    nonempty(destination, "environment variable", value)
}

fn nonempty(destination: &str, field: &'static str, value: String) -> Result<String, ConfigError> {
    if value.is_empty() {
        Err(ConfigError::EmptyDestinationField {
            destination: destination.to_owned(),
            field,
        })
    } else {
        Ok(value)
    }
}

impl MountConfig {
    fn build(&self) -> Result<Mount, ConfigError> {
        let (mount, facets) = match self {
            Self::SystemLibrary { label, facets } => {
                validate_label(label)?;
                (Mount::system_library(label), facets)
            }
            Self::Package {
                package,
                label,
                facets,
            } => {
                if package.is_empty() {
                    return Err(ConfigError::EmptyPackage);
                }
                let mount = if let Some(label) = label {
                    validate_label(label)?;
                    Mount::named_package(label, package)
                } else {
                    Mount::package(package)
                };
                (mount, facets)
            }
            Self::Selection {
                label,
                filters,
                facets,
            } => {
                validate_label(label)?;
                let mut preselections = Vec::with_capacity(filters.len());
                for filter in filters {
                    let Some((first, rest)) = filter.values.split_first() else {
                        return Err(ConfigError::EmptyFilter {
                            label: label.clone(),
                        });
                    };
                    let mut preselection =
                        RepositoryPreselection::new(filter.facet.as_str(), first.as_str());
                    for value in rest {
                        preselection = preselection.include(value.as_str());
                    }
                    for value in &filter.exclude {
                        preselection = preselection.exclude(value.as_str());
                    }
                    preselections.push(preselection);
                }
                (Mount::selection(label, preselections), facets)
            }
        };
        Ok(match facets {
            None => mount,
            Some(facets) => mount.facet_policy(facet_policy(facets)),
        })
    }
}

fn facet_policy(facets: &[FacetConfig]) -> FacetPolicy {
    FacetPolicy::new(facets.iter().map(|facet| match facet {
        FacetConfig::Always(facet) => FacetLevel::always(RepositoryFacet::from(facet.as_str())),
        FacetConfig::Detailed {
            facet,
            minimum_objects: Some(minimum_objects),
        } => FacetLevel::adaptive(RepositoryFacet::from(facet.as_str()), *minimum_objects),
        FacetConfig::Detailed {
            facet,
            minimum_objects: None,
        } => FacetLevel::always(RepositoryFacet::from(facet.as_str())),
    }))
}

fn validate_label(label: &str) -> Result<(), ConfigError> {
    if label.is_empty() {
        Err(ConfigError::EmptyMountLabel)
    } else {
        Ok(())
    }
}

const fn config_version() -> u32 {
    1
}

fn default_system_library_label() -> String {
    "System Library".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_mounts() {
        let config: ProjectConfig = serde_yaml_ng::from_str(
            r#"
version: 1
systems:
  S4:
    folder: SAP-DEV
    destination: DEV
    mounts:
      - kind: package
        label: Flight
        package: /DMO/FLIGHT
        facets: [GROUP, TYPE]
      - kind: selection
        label: Local Objects
        filters:
          - facet: PACKAGE
            values: ["..$TMP"]
        facets:
          - facet: OWNER
          - facet: TYPE
            minimumObjects: 10
"#,
        )
        .unwrap();
        let system = &config.systems["S4"];
        assert_eq!(system.folder.as_deref(), Some("SAP-DEV"));
        assert_eq!(system.destination.as_deref(), Some("DEV"));
        assert_eq!(system.mounts.len(), 2);
        assert_eq!(system.mounts[0].build().unwrap().label(), "Flight");
    }

    #[test]
    fn parses_environment_backed_destination() {
        let config: UserConfig = toml::from_str(
            r#"
version = 1
[destinations.DEV]
url_env = "SAP_DESTINATION"
client_env = "SAP_CLIENT"
language = "EN"
username_env = "SAP_USERNAME"
password_env = "SAP_PASSWORD"
"#,
        )
        .unwrap();
        assert_eq!(config.version, 1);
        assert!(config.destinations.contains_key("DEV"));
    }

    #[tokio::test]
    async fn loads_project_configuration_from_a_file_uri() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(PROJECT_FILE),
            "version: 1\nsystems:\n  S4:\n    destination: DEV\n",
        )
        .unwrap();
        let uri = Url::from_directory_path(directory.path()).unwrap();

        let (root, config) = load_project(uri.as_str()).await.unwrap();

        assert_eq!(root, directory.path().canonicalize().unwrap());
        assert_eq!(config.systems["S4"].destination.as_deref(), Some("DEV"));
    }
}
