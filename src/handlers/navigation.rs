//! Handles virtual filesystem navigation using the /vfs/ uri encoding.
use abap_lsp::{
    config::{DestinationId, ProjectRoot, load_local_project, load_system_configuration},
    context::{ContextError, SystemView},
    uri::{ResourcePath, ResourceUri, UriError},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_lsp_server::jsonrpc::{Error, Result};
use zvfs::{Node, NodeKind};

use crate::server::{NamespaceDelimiter, Server};

/// Parameters for a system project request. Only the system root is needed
/// which is already known based on the client initialization sequence.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSystemsParams {}

/// The [`ProjectSystem`] instances of a project root for the client to present.
#[derive(Debug, Serialize)]
pub struct ProjectSystemsResult {
    pub systems: Vec<ProjectSystem>,
}

/// A single sap system to that is part of a project.
///
/// This is the wire format of the internal [`abap_lsp::config::ProjectSystemConfig`].
#[derive(Debug, Serialize)]
pub struct ProjectSystem {
    /// The name of the folder that serves as portal into this system.
    /// Defaults to the destination ID if no folder was configured.
    pub folder: String,

    /// The URI of the system to redirect to when opening the folder.
    pub uri: String,
}

impl Server {
    /// Request the server to return all [`ProjectSystem`] instances after
    /// loading the `ziege.toml` from the project root of the client.
    pub async fn project_systems(&self, _: ProjectSystemsParams) -> Result<ProjectSystemsResult> {
        let root = self.project_root()?;
        let project = load_local_project(root.as_path()).await.map_err(to_msg)?;

        // Convert the systems into the wire format and resolve the root uris
        let systems = project
            .systems
            .into_iter()
            .map(|(id, config)| ProjectSystem {
                folder: config.folder.unwrap_or_else(|| id.as_str().to_owned()),
                uri: ResourceUri {
                    system: id,
                    path: ResourcePath::Vfs(Vec::new()),
                }
                .to_string(),
            })
            .collect();

        Ok(ProjectSystemsResult { systems })
    }
}

/// Request the server to read the directory entry at the specified URI.
///
/// This is more stable than a [`zvfs::NodeId`] because it can be constructed
/// by the client without persistent server provided state. Its possible that
/// the directory had already been read before and a cached result is returned
/// instead. The `refresh` flag can be used to reload the directory regardless.
///
/// Any resource URI provided here must have a path beginning with `/vfs/`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadDirectoryParams {
    /// A [`ResourceUri`] naming a directory in the project view.
    /// For example, `abap://DEV/vfs/Flight/Classes/`.
    pub uri: String,

    // Whether to reload the directory if it had been loaded before
    #[serde(default)]
    pub refresh: bool,
}

/// The entries resolved from a [`ReadDirectoryParams`] request.
#[derive(Debug, Serialize)]
pub struct ReadDirectoryResult {
    pub entries: Vec<DirectoryEntry>,
}

/// The wire format for a vfs directory entry to return to the client.
#[derive(Debug, Serialize)]
pub struct DirectoryEntry {
    /// The URI that identifies this entry, used for further navigation. This may
    /// drift visually from the client path due to namespace display preferences.
    pub uri: String,
    /// The label of the folder, i.e. the display name.
    pub name: String,
    /// What kind of entry. This helps the client decide what happens on navigation
    pub kind: EntryKind,
}

impl DirectoryEntry {
    fn assemble(
        system: DestinationId,
        path: &[String],
        node: Node,
        delimiter: NamespaceDelimiter,
    ) -> Self {
        // The node label can just be appendend to the parent path.
        let mut path = path.to_vec();
        path.push(node.label.clone());
        let path = ResourcePath::Vfs(path);

        let uri = ResourceUri { system, path }.to_string();

        // translate the kind and whether the node name is a technical object / package
        // name from the system or a user provided label
        let (kind, is_technical_name) = match &node.kind {
            NodeKind::Package { package, .. } => (EntryKind::Package, *package == node.label),
            NodeKind::Object { .. } => (EntryKind::Object, true),
            _ => (EntryKind::Folder, false),
        };

        let name = is_technical_name
            .then(|| delimiter.format_name(&node.label))
            .unwrap_or(node.label);
        Self { uri, name, kind }
    }
}

/// Directory kinds understood by editor clients.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    Folder,
    Package,
    Object,
}

impl Server {
    /// Reads the directory at the [`ResourcePath`] specified in the [`ReadDirectoryParams`].
    ///
    /// If the root is refreshed or no [`SystemView`] exists for this project yet,
    /// the project settings are read for the view mounts.
    pub async fn read_directory(&self, params: ReadDirectoryParams) -> Result<ReadDirectoryResult> {
        let uri: ResourceUri = params
            .uri
            .parse()
            .map_err(|err: UriError| Error::invalid_params(err.to_string()))?;

        // We expect a /vfs/ URI here, an object URI should not be used for navigation.
        let ResourcePath::Vfs(path) = &uri.path else {
            return Err(Error::invalid_params("readDirectory requires a VFS URI"));
        };

        // Find the view to navigate on. This may create a new view for us to use.
        // Notice that the `refresh` is only passed on to the tree if theres no
        // navigation path, i.e we are inside the root directory of the view.
        // This prevents issues where we are currently navigating, for example,
        // `abap://DEV/vfs/ZPACKAGE/Source%20Library/Classes` and then refresh
        // the mount configuration, which could then lead to that view getting
        // dropped and subsequent navigation failing to resolve the path
        let view = self
            .repository_view(
                self.project_root()?,
                &uri.system,
                params.refresh && path.is_empty(),
            )
            .await
            .map_err(to_msg)?;

        let parent = view.tree.resolve_path(path).await.map_err(to_msg)?;

        // The invisible `ZVFS` root has static mount children, so a refresh
        // cant take place on the root from `ZVFS`s perspective.
        let nodes = if view.tree.node(parent).is_some_and(|n| !n.is_directory()) {
            Ok(Vec::new())
        } else if params.refresh && parent != view.tree.root() {
            view.tree.refresh(parent).await
        } else {
            view.tree.children(parent).await
        }
        .map_err(to_msg)?;

        let options = self.options.get().ok_or_else(|| Error {
            message: "Options were not initialized".into(),
            ..Error::invalid_request()
        })?;
        let delimiter = options.presentation.namespace_delimiter;

        let entries: Vec<_> = nodes
            .into_iter()
            .map(|node| DirectoryEntry::assemble(uri.system.clone(), path, node, delimiter))
            .collect();

        Ok(ReadDirectoryResult { entries })
    }

    /// Resolves the project's selected view without configuration I/O. A missing
    /// view or explicit root refresh loads configuration and selects a backend.
    /// This also allows a saved VFS path to rebuild its view after a daemon restart.
    async fn repository_view(
        &self,
        root: &ProjectRoot,
        id: &DestinationId,
        refresh: bool,
    ) -> std::result::Result<Arc<SystemView>, ContextError> {
        if !refresh
            && let Some(context) = self.contexts.get(id).await
            && let Some(view) = context.view(root).await
        {
            return Ok(view);
        }

        let system = load_system_configuration(root, id, &self.user_config_path).await?;
        let context = self.contexts.upsert(id, &system.destination).await?;
        context.upsert_view(root, &system.config.mounts).await
    }
}

fn to_msg(error: impl std::fmt::Display) -> Error {
    let mut rpc = Error::new(tower_lsp_server::jsonrpc::ErrorCode::ServerError(-32803));
    rpc.message = error.to_string().into();
    rpc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_package_labels_preserve_their_spelling_and_uri() {
        let node: Node = serde_json::from_value(serde_json::json!({
            "id": {"scope": "00000000-0000-0000-0000-000000000000", "index": 1},
            "parent": null,
            "label": "/Examples/Flight",
            "kind": "package",
            "package": "/DMO/FLIGHT",
            "uri": "/sap/bc/adt/packages/%2fdmo%2fflight",
            "objectCount": null
        }))
        .unwrap();

        for delimiter in [NamespaceDelimiter::Parentheses, NamespaceDelimiter::Slash] {
            let entry =
                DirectoryEntry::assemble("DEV".to_owned().into(), &[], node.clone(), delimiter);
            assert_eq!(entry.name, "/Examples/Flight");
            assert_eq!(entry.uri, "abap://DEV/vfs/%2FExamples%2FFlight/");
            assert!(matches!(entry.kind, EntryKind::Package));
        }
    }
}
