use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use tokio::{
    sync::{Mutex, RwLock},
    time::timeout,
};
use zadt::{Client, Ready, ReqwestTransport};
use zvfs::VirtualRepositoryTree;

use crate::config::{ConfigError, LoadedSystem, load_system};

const CONTEXT_BUILD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CONFIGURATION_RETRIES: usize = 3;

#[derive(Clone, Eq, Hash, PartialEq)]
struct ContextKey {
    project_root: PathBuf,
    system_id: String,
    fingerprint: u64,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct ContextIdentity {
    project_root: PathBuf,
    system_id: String,
}

impl ContextKey {
    fn from_loaded(loaded: &LoadedSystem) -> Self {
        Self {
            project_root: loaded.project_root.clone(),
            system_id: loaded.system_id.clone(),
            fingerprint: loaded.fingerprint(),
        }
    }

    fn identity(&self) -> ContextIdentity {
        ContextIdentity {
            project_root: self.project_root.clone(),
            system_id: self.system_id.clone(),
        }
    }
}

pub struct SystemContext {
    pub client: Client<Ready>,
    pub tree: VirtualRepositoryTree,
}

#[derive(Default)]
pub struct ContextStore {
    contexts: RwLock<HashMap<ContextKey, Arc<SystemContext>>>,
    creation_gates: Mutex<HashMap<ContextIdentity, Arc<Mutex<()>>>>,
}

impl ContextStore {
    pub async fn get_or_create(
        &self,
        project_uri: &str,
        system_id: &str,
    ) -> Result<Arc<SystemContext>, ContextError> {
        'identity: loop {
            let loaded = load_system(project_uri, system_id).await?;
            let key = ContextKey::from_loaded(&loaded);
            if let Some(context) = self.contexts.read().await.get(&key).cloned() {
                return Ok(context);
            }

            let identity = key.identity();
            let gate = {
                let mut gates = self.creation_gates.lock().await;
                gates
                    .entry(identity.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            };
            let guard = gate.lock().await;
            let mut loaded = load_system(project_uri, system_id).await?;
            if ContextKey::from_loaded(&loaded).identity() != identity {
                drop(guard);
                continue 'identity;
            }

            for _ in 0..MAX_CONFIGURATION_RETRIES {
                let key = ContextKey::from_loaded(&loaded);
                if let Some(context) = self.contexts.read().await.get(&key).cloned() {
                    return Ok(context);
                }

                let context = timeout(CONTEXT_BUILD_TIMEOUT, build_context(&loaded))
                    .await
                    .map_err(|_| ContextError::BuildTimeout)??;
                let current = load_system(project_uri, system_id).await?;
                let current_key = ContextKey::from_loaded(&current);
                if current_key.identity() != identity {
                    drop(guard);
                    continue 'identity;
                } else if current_key != key {
                    loaded = current;
                    continue;
                }

                let context = Arc::new(context);
                let mut contexts = self.contexts.write().await;
                contexts.retain(|existing, _| {
                    existing.project_root != key.project_root || existing.system_id != key.system_id
                });
                contexts.insert(key, context.clone());
                return Ok(context);
            }
            return Err(ContextError::ConfigurationChanged);
        }
    }
}

async fn build_context(loaded: &LoadedSystem) -> Result<SystemContext, ContextError> {
    let transport = ReqwestTransport::builder()
        .destination(&loaded.destination.url)
        .sap_client(&loaded.destination.client)
        .language(&loaded.destination.language)
        .basic_auth(&loaded.destination.username, &loaded.destination.password)
        .build()?;
    let client = Client::new(transport).discover().await?;
    let tree = VirtualRepositoryTree::builder(client.clone())
        .mounts(loaded.mounts()?)
        .build()
        .await?;
    Ok(SystemContext { client, tree })
}

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("invalid destination configuration: {0}")]
    Transport(#[from] zadt::ReqwestTransportBuildError),
    #[error("ADT request failed: {0}")]
    Operation(#[from] zadt::OperationError),
    #[error("repository tree failed: {0}")]
    Vfs(#[from] zvfs::VfsError),
    #[error("timed out while building the repository context")]
    BuildTimeout,
    #[error("project configuration kept changing while building the repository context")]
    ConfigurationChanged,
}
