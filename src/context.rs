//! Shared backend connections, cache layers and repository view state.
//!
//! If the language server only had one client per server, bound to each others
//! lifetimes, system context could simply be stored on each `Server` worker.
//!
//! But because our server can outlive a client session that subsequently reconnects
//! and handle any number of clients connected to the same process, we can use a
//! common context layer for all clients. That way two instances of, for example,
//! Neovim, can both connect to system `A4H` from different roots and benefit from
//! a shared object cache layer and a shared ADT [`Client`].
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

use tokio::sync::Mutex;
use zadt::{Client, Discovery, ReqwestTransport};
use zvfs::{Mount, VirtualRepositoryTree};

use crate::config::{ConfigError, DestinationConfig, DestinationId, MountConfig, ProjectRoot};

/// Shared slots indexed by some identity. A mutex per entry allows for
/// quick insertion without keeping the map locked. The initialization
/// can then take place without locking the full context.
pub type ContextMap<K, V> = Mutex<HashMap<K, Arc<Mutex<Option<V>>>>>;

/// Shared state for one configured backend.
///
/// Because the server runs as a daemon, multiple clients may connect to the
/// same backend system and use this common context layer. They use the same
/// ADT [`Client`] to communicate with the backend.
///
/// A fingerprint is used to ensure that changing connection configuration causes
/// the context to become incompatible.
pub struct SystemContext {
    /// Shared discovered client for communicating with the backend.
    pub client: Client<Discovery>,
    /// Fingerprint of the configuration at the time of insertion
    fingerprint: u64,
    /// Projects of the same root (same mount configuration) can share the
    /// repository tree. Otherwise they can exist in parallel for one connection
    views: ContextMap<ProjectRoot, Arc<SystemView>>,
}

impl SystemContext {
    /// Gets the [`SystemView`] for a specific [`ProjectRoot`], if one exists.
    pub async fn view(&self, project: &ProjectRoot) -> Option<Arc<SystemView>> {
        let slot = self.views.lock().await.get(project)?.clone();
        slot.lock().await.clone()
    }

    /// Inserts or updates a [`SystemView`] at the provided [`ProjectRoot`] with
    /// a specified set of [`MountConfig`]. If a view with the exact same root
    /// project and mount configuration already exists, it is simply returned.
    ///
    /// If neither the project root nor the mounts have actually changed, the
    /// instance is left untouched. This is useful when a refresh was issued
    /// and its uncertain whether the mount configuration is still recent.
    pub async fn upsert_view(
        &self,
        project: &ProjectRoot,
        mounts: &[MountConfig],
    ) -> Result<Arc<SystemView>, ContextError> {
        let slot = self
            .views
            .lock()
            .await
            .entry(project.clone())
            .or_default()
            .clone();

        let mut current = slot.lock().await;
        // Only if the mount configuration has not changed is this still
        // compatible. This is overly sensitive regarding labels, but thats ok.
        if let Some(view) = current.as_ref()
            && view.mounts == mounts
        {
            return Ok(view.clone());
        }

        // Resolve the mounts from our config format into the zvfs format
        // TODO: Maybe a wrapper struct around a vec of mounts?
        let built_mounts = if mounts.is_empty() {
            vec![Mount::system_library("System Library")]
        } else {
            mounts
                .iter()
                .map(MountConfig::build)
                .collect::<Result<Vec<_>, _>>()?
        };

        let tree = VirtualRepositoryTree::builder(self.client.clone())
            .mounts(built_mounts)
            .build()
            .await?;

        let view = Arc::new(SystemView {
            mounts: mounts.to_vec(),
            tree,
        });
        *current = Some(view.clone());
        Ok(view)
    }
}

/// Process-wide backend contexts indexed only by the configured [`DestinationId`].
///
/// Configuration revisions replace the selected context rather than adding keys
/// or retaining a history of backend connections.
#[derive(Default)]
pub struct SystemContextStore {
    contexts: ContextMap<DestinationId, Arc<SystemContext>>,
}

impl SystemContextStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieves a [`SystemContext`] for the given [`DestinationId`] if existent
    pub async fn get(&self, id: &DestinationId) -> Option<Arc<SystemContext>> {
        let slot = self.contexts.lock().await.get(id)?.clone();
        slot.lock().await.clone()
    }

    /// Inserts or updates a [`SystemContext`] based on whether the provided
    /// [`DestinationConfig`] still has the same fingerprint as the stored system.
    ///
    /// If a system with the same configuration already exists, it is simply returned
    /// without being touched. This is convenient when a refresh leaves us uncertain
    /// whether the configuration is still recent.
    pub async fn upsert(
        &self,
        id: &DestinationId,
        config: &DestinationConfig,
    ) -> Result<Arc<SystemContext>, ContextError> {
        let mut hasher = DefaultHasher::new();
        config.hash(&mut hasher);
        let fingerprint = hasher.finish();

        let slot = self
            .contexts
            .lock()
            .await
            .entry(id.clone())
            .or_default()
            .clone();

        let mut current = slot.lock().await;
        // Only if the connection configuration has not changed is this still compatible.
        if let Some(context) = current.as_ref()
            && context.fingerprint == fingerprint
        {
            return Ok(context.clone());
        }

        // TODO: This is a pretty important part, especially when connection
        // configuration gets more complex, best moved somewhere more coherent.
        let transport = ReqwestTransport::builder()
            .destination(&config.url)
            .sap_client(&config.client)
            .language(&config.language)
            .basic_auth(config.username.as_str(), &config.password)
            .build()?;

        let context = Arc::new(SystemContext {
            client: Client::new(transport).discover().await?,
            views: Mutex::new(HashMap::new()),
            fingerprint,
        });

        *current = Some(context.clone());
        Ok(context)
    }
}

/// A view on a system - currently represented only by the [`VirtualRepositoryTree`].
///
/// Clients browsing the same project and destination share this context.
pub struct SystemView {
    pub mounts: Vec<MountConfig>,
    pub tree: VirtualRepositoryTree,
}

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Transport(#[from] zadt::ReqwestTransportBuildError),

    #[error(transparent)]
    Operation(#[from] zadt::OperationError),

    #[error(transparent)]
    Vfs(#[from] zvfs::VfsError),

    #[error("Timed out while connecting to the repository or building its view")]
    BuildTimeout,
}
