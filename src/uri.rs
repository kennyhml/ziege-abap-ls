//! Addresses of repository views and projected files.
//!
//! We currently support two URI schemas, assuming a DEV system ID:
//!
//! - `abap://DEV/zclmyclass.clas.abap`
//! - `abap://DEV/vfs/ZPACKAGE/Source%20Libary%20/Classes/zclmyclass.clas.abap`
//!
//! The first schema is typically used when a document is opened. Its identity
//! is unique in a system context because an object type and name form a unique
//! pair. The VFS (Virtual Filesystem) schema is used for the buffers that represent
//! the navigatable object repository. That way we dont need to rely on the client
//! having a [`zvfs::NodeId`] at hand when navigating objects.
//!
//! The authority identifies a [`DestinationId`]. The project root belongs to the
//! editor connection, so it does not appear in the URI.
use crate::config::DestinationId;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::{fmt, str::FromStr};

/// An address within a configured destination.
///
/// For example, `abap://DEV/vfs/Flight/Classes/` identifies a browser directory.
/// `abap://DEV/zcl_example.clas.abap` identifies a projected file independently of
/// the mounts through which it was found. See [`ResourcePath`] for this distinction.
///
/// ```
/// use abap_lsp::uri::{ResourcePath, ResourceUri};
///
/// let uri: ResourceUri = "abap://DEV/vfs/%2FDMO%2FFLIGHT/".parse().unwrap();
/// assert_eq!(uri.system.as_str(), "DEV");
/// assert_eq!(uri.path, ResourcePath::Vfs(vec!["/DMO/FLIGHT".into()]));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceUri {
    pub system: DestinationId,
    pub path: ResourcePath,
}

/// The part of a [`ResourceUri`] following its destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourcePath {
    /// Labels in the configured [`crate::context::SystemView`]. A label
    /// containing slashes is still one segment, for example `/DMO/FLIGHT`.
    Vfs(Vec<String>),
    /// A canonical projected file name. Source loading is added separately from
    /// repository navigation, but uses the same destination and URI boundary.
    Document(String),
}

impl FromStr for ResourceUri {
    type Err = UriError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (authority, path) = value
            .strip_prefix("abap://")
            .and_then(|value| value.split_once('/'))
            .ok_or(UriError::InvalidAddress)?;

        // Split the original URI before decoding. General URL parsers can remove
        // dot segments, which would change the meaning of repository labels.
        let system = DestinationId::from_uri(decode_component(authority)?);

        let segments = path
            .strip_suffix('/')
            .unwrap_or(path)
            .split('/')
            .map(decode_component)
            .collect::<Result<Vec<_>, _>>()?;

        let path = match segments.as_slice() {
            [first, rest @ ..] if first == "vfs" => ResourcePath::Vfs(rest.to_vec()),
            [name] if !value.ends_with('/') => ResourcePath::Document(name.clone()),
            _ => return Err(UriError::InvalidAddress),
        };

        Ok(Self { system, path })
    }
}

impl fmt::Display for ResourceUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Set of characters that need url encoding. We need to remove certain
        // characters, such as `.`, as its used for component seperators in AFF.
        const CHARSET: &AsciiSet = &NON_ALPHANUMERIC
            .remove(b'-')
            .remove(b'.')
            .remove(b'_')
            .remove(b'~');

        // For VFS path segments, the dot gets a navigation implication and must
        // be encoded when it otherwise causes ambiguity
        const DOT_CHARSET: &AsciiSet = &CHARSET.add(b'.');

        // destination is authority
        write!(
            formatter,
            "abap://{}/",
            utf8_percent_encode(self.system.as_str(), CHARSET)
        )?;

        match &self.path {
            ResourcePath::Vfs(path) => {
                write!(formatter, "vfs/")?;
                for segment in path {
                    let encode_set = match segment.as_str() {
                        "." | ".." => DOT_CHARSET,
                        _ => CHARSET,
                    };
                    write!(formatter, "{}/", utf8_percent_encode(segment, encode_set))?
                }
                Ok(())
            }
            ResourcePath::Document(name) => {
                write!(formatter, "{}", utf8_percent_encode(name, CHARSET))
            }
        }
    }
}

/// Regular URI percent decoding is fairly permissive. We want to be pretty
/// defensive here, so we must add some validation of our own.
fn decode_component(value: &str) -> Result<String, UriError> {
    if value.is_empty() {
        return Err(UriError::InvalidAddress);
    }

    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        // Any % must be followed by two hex digits
        if byte == b'%' {
            if !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
            {
                return Err(UriError::InvalidEncoding);
            }
        // Only characters, letters, or the literally named special characters are allowed
        } else if !(byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=".contains(&byte)) {
            return Err(UriError::InvalidEncoding);
        }
    }

    let decoded = percent_decode_str(value)
        .decode_utf8()
        .map_err(|_| UriError::InvalidEncoding)?;

    // Reject control characters, they can mess with display / logging
    if decoded.chars().any(char::is_control) {
        return Err(UriError::InvalidEncoding);
    }

    Ok(decoded.into_owned())
}

#[derive(Debug, thiserror::Error)]
pub enum UriError {
    #[error("Expected abap://DESTINATION/vfs/path/ or abap://DESTINATION/filename")]
    InvalidAddress,
    #[error("Invalid URI component. Reserved characters must be percent encoded")]
    InvalidEncoding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_preserve_destination_case_and_encoded_segment_boundaries() {
        let uri: ResourceUri =
            "abap://DeV%20%40%3A%2F/vfs/Flight%20%26%20More/%2FDMO%2FFLIGHT/%252F/%2E%2E/"
                .parse()
                .unwrap();
        assert_eq!(uri.system.as_str(), "DeV @:/");
        assert_eq!(
            uri.path,
            ResourcePath::Vfs(vec![
                "Flight & More".into(),
                "/DMO/FLIGHT".into(),
                "%2F".into(),
                "..".into()
            ])
        );
        assert_eq!(uri.to_string().parse::<ResourceUri>().unwrap(), uri);
        assert_ne!(
            "abap://DEV/vfs/".parse::<ResourceUri>().unwrap().system,
            "abap://dev/vfs/".parse::<ResourceUri>().unwrap().system
        );
        let file: ResourceUri = "abap://DEV/%23dmo%23cl_flight.clas.abap".parse().unwrap();
        assert_eq!(
            file.path,
            ResourcePath::Document("#dmo#cl_flight.clas.abap".into())
        );
        assert_eq!(file.to_string(), "abap://DEV/%23dmo%23cl_flight.clas.abap");
    }

    #[test]
    fn malformed_or_ambiguous_addresses_are_rejected() {
        for uri in [
            "file:///tmp/file",
            "abap:/DEV/vfs/",
            "abap:///vfs/",
            "abap://DEV/",
            "abap://DEV@project/vfs/",
            "abap://DEV:42/vfs/",
            "abap://DEV/vfs//Classes/",
            "abap://DEV/vfs/%",
            "abap://DEV/vfs/%FF",
            "abap://DEV/vfs/%00",
            "abap://DEV/vfs/Classes?x=1",
            "abap://DEV/file.abap#fragment",
            "abap://DEV/dir/file.abap",
        ] {
            assert!(uri.parse::<ResourceUri>().is_err(), "{uri}");
        }
    }
}
