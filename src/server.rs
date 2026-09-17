//! Server state, standard LSP lifecycle, and custom handler registration.
//!
//! The TCP listener creates a service for each editor connection and shares the
//! repository context store between them. Custom handlers live in `crate::handlers`
//! and operate on this state.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use abap_lsp::{config::ProjectRoot, context::SystemContextStore};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;
use tower_lsp_server::{
    Client, ClientSocket, LanguageServer, LspService,
    jsonrpc::{Error, Result},
    ls_types::{
        InitializeParams, InitializeResult, InitializedParams, MessageType, ServerCapabilities,
    },
};

use crate::lifecycle::ConnectionEvent;
use url::Url;

/// A server worker handling one editor connection / client.
///
/// All workers share the same [`SystemContextStore`], allowing
/// a reconnect to reuse backend clients and repository views. Each connection
/// supplies its own presentation preferences.
/// The connection also supplies one [`ProjectRoot`] during initialization. A
/// resource URI identifies the destination, while this root selects its view.
///
/// Custom [`InitializationOptions`] can be sent by the client to supply
/// configuratino outside of the language server protocols scope.
pub struct Server {
    /// The client connected to this server worker.
    pub(crate) client: Client,

    /// Context shared across editor connections.
    pub(crate) contexts: Arc<SystemContextStore>,

    /// Configuration location resolved once at process startup. The context store
    /// receives resolved settings and never reads this file itself.
    pub(crate) user_config_path: PathBuf,

    /// Sends connection events to the outer listener loop.
    pub(crate) connection_tx: UnboundedSender<ConnectionEvent>,

    /// Connection specific options set by the initial `initialize` request.
    pub(crate) options: OnceLock<InitializationOptions>,

    /// The project this worker serves. Equivalent paths are canonicalized before
    /// they are used to look up a [`abap_lsp::context::SystemView`].
    pub(crate) project_root: OnceLock<ProjectRoot>,
}

impl Server {
    /// Builds the registered LSP service and its outgoing message socket.
    pub fn service(
        contexts: Arc<SystemContextStore>,
        user_config_path: PathBuf,
        connection_tx: UnboundedSender<ConnectionEvent>,
    ) -> (LspService<Self>, ClientSocket) {
        LspService::build(|client| Self {
            client,
            contexts,
            user_config_path,
            connection_tx,
            options: OnceLock::new(),
            project_root: OnceLock::new(),
        })
        .custom_method("ziege/project/systems", Self::project_systems)
        .custom_method("ziege/fileSystem/readDirectory", Self::read_directory)
        .finish()
    }

    /// Returns the project established by [`Self::initialize`]. All requests on
    /// this connection use the same root, including restored browser buffers.
    pub(crate) fn project_root(&self) -> Result<&ProjectRoot> {
        self.project_root.get().ok_or_else(Error::invalid_request)
    }
}

impl LanguageServer for Server {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let project_root = client_project_root(&params).await?;
        let options = match params.initialization_options {
            None | Some(serde_json::Value::Null) => InitializationOptions::default(),
            Some(value) => serde_json::from_value(value)
                .map_err(|error| Error::invalid_params(error.to_string()))?,
        };

        self.options.set(options).map_err(|_| Error {
            message: "Initialization options have already been set".into(),
            ..Error::invalid_request()
        })?;
        self.project_root.set(project_root).map_err(|_| Error {
            message: "The connection is already bound to a project root".into(),
            ..Error::invalid_request()
        })?;

        let _ = self.connection_tx.send(ConnectionEvent::Initialized);
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                experimental: Some(json!({"ziege": {
                    "protocolVersion": 1,
                    "projectConfiguration": true,
                    "repositoryNavigation": true
                }})),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Ziege initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

/// How technical ABAP namespaces are displayed in repository listings.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum NamespaceDelimiter {
    #[default]
    Parentheses,
    /// This causes issues in alot of editors, but some have workarounds (such as Neovim)
    Slash,
}

impl NamespaceDelimiter {
    /// Formats a technical name for repository listings. Parentheses use
    /// [`zaff::encode_object_name`]. Names outside its supported syntax remain
    /// readable by falling back to their lowercase spelling.
    pub fn format_name(self, name: &str) -> String {
        match self {
            Self::Parentheses => {
                zaff::encode_object_name(name).unwrap_or_else(|_| name.to_ascii_lowercase())
            }
            Self::Slash => name.to_ascii_lowercase(),
        }
    }
}

/// Repository presentation preferences for this editor connection.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationOptions {
    #[serde(default)]
    pub namespace_delimiter: NamespaceDelimiter,
}

/// Custom options supplied with the LSP `initialize` request.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct InitializationOptions {
    #[serde(default)]
    pub presentation: PresentationOptions,
}

/// Selects the single project served by this connection. Older LSP clients can
/// supply `rootUri` instead of `workspaceFolders`. Multiple roots need separate
/// connections so a virtual URI always resolves against an unambiguous view.
#[allow(deprecated)]
async fn client_project_root(params: &InitializeParams) -> Result<ProjectRoot> {
    let uri = match params.workspace_folders.as_deref() {
        Some([folder]) => folder.uri.as_str(),
        None | Some([]) => params
            .root_uri
            .as_ref()
            .map(|uri| uri.as_str())
            .ok_or_else(|| {
                Error::invalid_params("Ziege requires one project root during initialize")
            })?,
        Some(_) => {
            return Err(Error::invalid_params(
                "Use one Ziege connection per project root",
            ));
        }
    };
    canonicalize_project_uri(uri).await
}

/// Converts the clients file URI to a canonical project path at the LSP boundary.
///
/// For example, `file:///home/user/dev/my%20project` becomes the filesystem path
/// `/home/user/dev/my project`. Canonicalization resolves symlinks and `.`/`..`
/// components so equivalent client URIs share the same repository context.
async fn canonicalize_project_uri(project_uri: &str) -> Result<ProjectRoot> {
    let uri = Url::parse(project_uri)
        .map_err(|_| Error::invalid_params(format!("Invalid project URI `{project_uri}`")))?;
    let path = uri.to_file_path().map_err(|_| {
        Error::invalid_params(format!(
            "Project URI must identify a local file directory: `{project_uri}`"
        ))
    })?;

    let root = ProjectRoot::canonicalize(&path).await.map_err(|error| {
        Error::invalid_params(format!(
            "Cannot resolve project directory `{}`: {error}",
            path.display()
        ))
    })?;

    if !root.as_path().is_dir() {
        return Err(Error::invalid_params(
            "Project URI must identify a directory",
        ));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_names_remain_readable_in_repository_listings() {
        for delimiter in [NamespaceDelimiter::Parentheses, NamespaceDelimiter::Slash] {
            assert_eq!(delimiter.format_name("<MEMBER>"), "<member>");
        }
    }

    #[tokio::test]
    async fn project_uri_decodes_spaces_and_canonicalizes_paths() {
        let directory = tempfile::Builder::new()
            .prefix("ziege project ")
            .tempdir()
            .unwrap();
        let uri = Url::from_directory_path(directory.path().join(".")).unwrap();
        assert!(uri.as_str().contains("%20"));
        assert_eq!(
            canonicalize_project_uri(uri.as_str())
                .await
                .unwrap()
                .as_path(),
            directory.path().canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn project_uri_rejects_paths_and_remote_schemes() {
        for uri in ["/home/user/project", "https://example.invalid/project"] {
            let error = canonicalize_project_uri(uri).await.unwrap_err();
            assert_eq!(
                error.code,
                tower_lsp_server::jsonrpc::ErrorCode::InvalidParams
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_uri_resolves_symlinks_to_the_same_root() {
        let directory = tempfile::tempdir().unwrap();
        let alias = directory.path().join("alias");
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let uri = Url::from_directory_path(alias).unwrap();
        assert_eq!(
            canonicalize_project_uri(uri.as_str())
                .await
                .unwrap()
                .as_path(),
            root.canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn initialize_requires_one_project_and_accepts_legacy_root_uri() {
        let directory = tempfile::tempdir().unwrap();
        let uri = Url::from_directory_path(directory.path())
            .unwrap()
            .to_string();
        for params in [
            json!({"capabilities": {}, "workspaceFolders": [{"uri": uri, "name": "project"}]}),
            json!({"capabilities": {}, "rootUri": uri}),
        ] {
            let params = serde_json::from_value(params).unwrap();
            assert_eq!(
                client_project_root(&params).await.unwrap().as_path(),
                directory.path().canonicalize().unwrap()
            );
        }
        for params in [
            json!({"capabilities": {}}),
            json!({"capabilities": {}, "workspaceFolders": [{"uri": uri, "name": "one"}, {"uri": uri, "name": "two"}]}),
        ] {
            let params = serde_json::from_value(params).unwrap();
            assert!(client_project_root(&params).await.is_err());
        }
    }
}
