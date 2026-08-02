use std::{
    fmt,
    sync::{Arc, OnceLock},
};

use abap_lsp::{
    config::load_project,
    context::{ContextStore, SystemContext},
    protocol::{
        EntryType, FileSystemEntry, InitializationOptions, NamespaceDelimiter,
        ObjectCreationOptionsParams, ObjectCreationOptionsResult, ProjectSystem,
        ProjectSystemsParams, ProjectSystemsResult, ReadDirectoryParams, ReadDirectoryResult,
        ReadFileParams, ReadFileResult, RefreshTransportsParams, RefreshTransportsResult,
    },
};
use serde_json::json;
use tower_lsp_server::{
    Client as LspClient, LanguageServer,
    jsonrpc::{Error, Result},
    ls_types::{
        InitializeParams, InitializeResult, InitializedParams, MessageType, ServerCapabilities,
    },
};
use zadt::{ClassSourceComponent, Operation};
use zaff::{FileComponent, ObjectFormat, SourceComponent, resolve_file_name};
use zvfs::{Node, NodeId, NodeKind};

const NODE_ID_PREFIX: &str = "node:";
const SOURCE_ID_PREFIX: &str = "source:";

pub struct Backend {
    client: LspClient,
    contexts: Arc<ContextStore>,
    initialization_options: OnceLock<InitializationOptions>,
}

impl fmt::Debug for Backend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Backend {
    pub fn new(client: LspClient, contexts: Arc<ContextStore>) -> Self {
        Self {
            client,
            contexts,
            initialization_options: OnceLock::new(),
        }
    }

    pub async fn project_systems(
        &self,
        params: ProjectSystemsParams,
    ) -> Result<ProjectSystemsResult> {
        let (_, project) = load_project(&params.project_uri).await.map_err(rpc_error)?;
        let systems = project
            .systems
            .into_iter()
            .map(|(name, config)| ProjectSystem {
                folder: config.folder.clone().unwrap_or_else(|| name.clone()),
                name,
                provider: config.provider,
                destination: config.destination,
            })
            .collect();
        Ok(ProjectSystemsResult { systems })
    }

    pub async fn read_directory(&self, params: ReadDirectoryParams) -> Result<ReadDirectoryResult> {
        let context = self
            .contexts
            .get_or_create(&params.project_uri, &params.system)
            .await
            .map_err(rpc_error)?;
        let parent = params
            .parent_id
            .as_deref()
            .map(decode_node_id)
            .transpose()?
            .unwrap_or_else(|| context.tree.root());
        let nodes = context.tree.children(parent).await.map_err(rpc_error)?;
        let entries = nodes
            .into_iter()
            .filter_map(|node| {
                project_node(
                    &context,
                    node,
                    self.initialization_options
                        .get()
                        .copied()
                        .unwrap_or_default()
                        .presentation
                        .namespace_delimiter,
                )
            })
            .collect();
        Ok(ReadDirectoryResult {
            entries,
            modifiable: false,
        })
    }

    pub async fn read_file(&self, params: ReadFileParams) -> Result<ReadFileResult> {
        let context = self
            .contexts
            .get_or_create(&params.project_uri, &params.system)
            .await
            .map_err(rpc_error)?;
        let id = decode_source_id(&params.resource_id)?;
        let entry = context.tree.object_entry(id).map_err(rpc_error)?;
        let (file_name, _) = main_source_file(&entry).map_err(rpc_error)?;
        let source = resolve_file_name(&file_name)
            .and_then(|resolved| resolved.source_ref(&entry))
            .map_err(rpc_error)?;
        let source = source
            .query()
            .execute(&context.client)
            .await
            .map_err(rpc_error)?;
        Ok(ReadFileResult {
            content: source.content,
            language_id: "abap",
            revision: source.etag.map(|etag| etag.as_str().to_owned()),
            writable: false,
        })
    }

    pub async fn object_creation_options(
        &self,
        _: ObjectCreationOptionsParams,
    ) -> Result<ObjectCreationOptionsResult> {
        Ok(ObjectCreationOptionsResult {
            supported: false,
            message: Some(
                "Object creation is unavailable until the ADT lifecycle APIs are implemented"
                    .to_owned(),
            ),
            context_token: None,
            revision: None,
            forms: Vec::new(),
            transports: Vec::new(),
        })
    }

    pub async fn refresh_transports(
        &self,
        _: RefreshTransportsParams,
    ) -> Result<RefreshTransportsResult> {
        Ok(RefreshTransportsResult {
            supported: false,
            message: Some(
                "Transport discovery is unavailable until the ADT CTS APIs are implemented"
                    .to_owned(),
            ),
            context_token: None,
            revision: None,
            transports: Vec::new(),
        })
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let options = match params.initialization_options {
            None | Some(serde_json::Value::Null) => InitializationOptions::default(),
            Some(value) => serde_json::from_value(value)
                .map_err(|error| Error::invalid_params(error.to_string()))?,
        };
        self.initialization_options
            .set(options)
            .map_err(|_| Error::invalid_request())?;
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                experimental: Some(json!({
                    "ziege": {
                        "protocolVersion": 1,
                        "projectConfiguration": true,
                        "virtualFileSystem": true,
                        "namespacePresentation": true,
                        "objectCreation": {
                            "options": true,
                            "refreshTransports": true,
                            "create": false
                        }
                    }
                })),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Ziege language server initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

fn project_node(
    context: &SystemContext,
    node: Node,
    namespace_delimiter: NamespaceDelimiter,
) -> Option<FileSystemEntry> {
    if matches!(node.kind, NodeKind::Object { .. }) {
        let entry = context.tree.object_entry(node.id).ok()?;
        let (name, extension) = main_source_file(&entry).ok()?;
        return Some(FileSystemEntry {
            id: encode_id(SOURCE_ID_PREFIX, node.id),
            name: present_source_name(name, namespace_delimiter),
            entry_type: EntryType::File,
            extension: Some(extension.to_owned()),
            virtual_folder: false,
        });
    }
    let virtual_folder = matches!(node.kind, NodeKind::Mount { .. } | NodeKind::Facet { .. });
    Some(FileSystemEntry {
        id: encode_id(NODE_ID_PREFIX, node.id),
        name: node.label,
        entry_type: EntryType::Directory,
        extension: None,
        virtual_folder,
    })
}

fn present_source_name(name: String, namespace_delimiter: NamespaceDelimiter) -> String {
    if namespace_delimiter == NamespaceDelimiter::Slash
        && let Some(namespaced) = name.strip_prefix('(')
        && let Some((namespace, local_name)) = namespaced.split_once(')')
    {
        return format!("/{namespace}/{local_name}");
    }
    name
}

fn main_source_file(
    entry: &zadt::RepositoryObjectEntry,
) -> std::result::Result<(String, &'static str), zaff::ProjectionError> {
    let format = ObjectFormat::try_from(entry)?;
    let component = match format {
        ObjectFormat::Program => FileComponent::Source(SourceComponent::Program),
        ObjectFormat::Class => {
            FileComponent::Source(SourceComponent::Class(ClassSourceComponent::Main))
        }
    };
    let specification = format
        .files()
        .iter()
        .find(|specification| specification.component == component)
        .expect("every supported object format has a main source file");
    Ok((
        specification.file_name(&entry.name, None)?,
        match format {
            ObjectFormat::Program => "prog.abap",
            ObjectFormat::Class => "clas.abap",
        },
    ))
}

fn encode_id(prefix: &str, id: NodeId) -> String {
    format!(
        "{prefix}{}",
        serde_json::to_string(&id).expect("NodeId is serializable")
    )
}

fn decode_node_id(id: &str) -> Result<NodeId> {
    decode_id(id, NODE_ID_PREFIX)
}

fn decode_source_id(id: &str) -> Result<NodeId> {
    decode_id(id, SOURCE_ID_PREFIX)
}

fn decode_id(id: &str, prefix: &str) -> Result<NodeId> {
    let encoded = id
        .strip_prefix(prefix)
        .ok_or_else(|| Error::invalid_params("resource ID has the wrong kind"))?;
    serde_json::from_str(encoded).map_err(|_| Error::invalid_params("invalid resource ID"))
}

fn rpc_error(error: impl fmt::Display) -> Error {
    let mut rpc = Error::internal_error();
    rpc.message = error.to_string().into();
    rpc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_round_trip_without_exposing_identity_semantics() {
        let id: NodeId =
            serde_json::from_str(r#"{"scope":"52f2732a-9c0d-4d90-a6ff-f6aa55759c06","index":42}"#)
                .unwrap();
        assert_eq!(decode_node_id(&encode_id(NODE_ID_PREFIX, id)).unwrap(), id);
        assert!(decode_source_id(&encode_id(NODE_ID_PREFIX, id)).is_err());
    }

    #[test]
    fn presentation_changes_namespaces_without_changing_plain_names() {
        assert_eq!(
            "/dmo/tatfta.clas.abap",
            present_source_name(
                "(dmo)tatfta.clas.abap".to_owned(),
                NamespaceDelimiter::Slash
            )
        );
        assert_eq!(
            "(dmo)tatfta.clas.abap",
            present_source_name(
                "(dmo)tatfta.clas.abap".to_owned(),
                NamespaceDelimiter::Parentheses
            )
        );
        assert_eq!(
            "zexample.prog.abap",
            present_source_name("zexample.prog.abap".to_owned(), NamespaceDelimiter::Slash)
        );
    }
}
