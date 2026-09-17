//! Named ADT connections loaded directly from `destinations.toml`.
//!
//! The catalog is loaded on demand beside the resolved user configuration.
//! Connection settings and credentials are literal file values.

use super::{CONFIG_VERSION, ConfigError, read_config};
use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use std::{borrow::Borrow, collections::BTreeMap, path::PathBuf};

/// A destination name read from configuration, either as a catalog key or a
/// project system key. Deserialization does not validate that the destination exists.
/// A [`crate::uri::ResourceUri`] can also supply the name. Its spelling is retained
/// so that destinations such as `DEV` and `dev` remain distinct.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DestinationId(String);

impl DestinationId {
    pub(crate) fn from_uri(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for DestinationId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<String> for DestinationId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Named ADT connections from `destinations.toml`, loaded on demand.
#[derive(Clone, Deserialize)]
struct DestinationsConfig {
    /// Configuration schema version
    version: u32,
    /// Keys used by the project's `systems` table.
    destinations: BTreeMap<DestinationId, DestinationConfig>,
}

/// ADT connection settings read directly from the destinations file.
#[serde_inline_default]
#[derive(Clone, Deserialize, Eq, Hash, PartialEq)]
pub struct DestinationConfig {
    /// Base URL of the ADT system.
    pub url: String,
    /// SAP client number, kept as a string to preserve leading zeroes.
    pub client: String,
    /// Logon language - defaults to English.
    #[serde_inline_default("EN".to_owned())]
    pub language: String,
    /// ADT logon username.
    pub username: String,
    /// ADT logon password.
    pub password: String,
}

/// Reads and validates a named destination only when a real repository is requested.
pub(super) async fn load_destination(
    path: PathBuf,
    id: &DestinationId,
) -> Result<DestinationConfig, ConfigError> {
    let mut config: DestinationsConfig = read_config(&path).await?;

    if config.version != CONFIG_VERSION {
        return Err(ConfigError::UnsupportedVersion {
            kind: "destinations",
            version: config.version,
        });
    }

    let destination =
        config
            .destinations
            .remove(id)
            .ok_or_else(|| ConfigError::UnknownDestination {
                destination: id.as_str().to_owned(),
                path,
            })?;

    for (field, value) in [
        ("url", &destination.url),
        ("client", &destination.client),
        ("language", &destination.language),
        ("username", &destination.username),
        ("password", &destination.password),
    ] {
        if value.is_empty() {
            return Err(ConfigError::EmptyField {
                field: format!("destinations.{}.{field}", id.as_str()),
            });
        }
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loads_literal_destination_and_defaults_language() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("destinations.toml");
        std::fs::write(&path, "version = 1\n[destinations.DEV]\nurl = 'https://example.invalid'\nclient = '001'\nusername = 'DEVELOPER'\npassword = 'example-password'\n").unwrap();
        let destination = load_destination(path, &"DEV".to_owned().into())
            .await
            .unwrap();
        assert_eq!(destination.url, "https://example.invalid");
        assert_eq!(destination.client, "001");
        assert_eq!(destination.language, "EN");
        assert_eq!(destination.username, "DEVELOPER");
        assert_eq!(destination.password, "example-password");
    }
}
