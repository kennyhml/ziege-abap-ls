mod handlers;
mod lifecycle;
mod server;

use std::{error::Error, sync::Arc, time::Duration};

use abap_lsp::{config, context::SystemContextStore};
use clap::Parser;
use lifecycle::{ConnectionEvent, ConnectionLifecycle};
use server::Server;
use tokio::{net::TcpListener, sync::mpsc};
use tower_lsp_server::Server as LspServer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// ABAP language server.
#[derive(Debug, Parser)]
struct LaunchArguments {
    /// Keep listening after clients disconnect until the idle timeout.
    #[arg(long)]
    daemon: bool,

    /// TCP address to listen on.
    #[arg(long, env = "ZIEGE_LSP_ADDRESS", default_value = "127.0.0.1:9257")]
    address: String,

    /// Idle timeout in seconds for daemon mode.
    #[arg(long, env = "ZIEGE_IDLE_TIMEOUT_SECONDS", default_value_t = 600)]
    idle_timeout_seconds: u64,
}

impl LaunchArguments {
    fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_seconds)
    }
}

/// The main entry point for clients. We open a TCP Listener at `ZIEGE_LSP_ADDRESS`
/// and wait for clients to connect. [`ConnectionLifecycle`] keeps track of active
/// clients and whether any connection has been accepted to orchestrate server
/// shutdown based on the configuration, such as daemon process mode.
///
/// Each connected client spawns a new task and is served by a dedicated [`LspServer`].
/// They share a [`SystemContextStore`], retaining backend clients
/// and browsed repository views across reconnects while this process is alive.
/// A task reports [`ConnectionEvent::TransportClosed`] after [`LspServer::serve`]
/// returns, so the listener follows the actual transport lifetime.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Process lifetime is selected by the launcher, before any clients connect.
    let process = LaunchArguments::parse();

    // The senders let us know when a connected client initializes or diconnects
    let (connection_tx, mut connection_rx) = mpsc::unbounded_channel();

    let mut lifecycle = ConnectionLifecycle::new(process.daemon, process.idle_timeout());
    let user_config = config::user_config_path()?;
    let contexts = Arc::new(SystemContextStore::new());

    let listener = TcpListener::bind(&process.address).await?;
    info!(address = %process.address, daemon = process.daemon, "Ziege language server listening");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(connection) => connection,
                    Err(error) => {
                        warn!(%error, "failed to LSP connection");
                        continue;
                    }
                };
                lifecycle.connected();

                let contexts = contexts.clone();
                let user_config = user_config.clone();
                let connection_tx = connection_tx.clone();

                tokio::spawn(async move {
                    let (read, write) = tokio::io::split(stream);
                    let (service, socket) =
                        Server::service(contexts, user_config, connection_tx.clone());

                    info!(%peer, "Client connected");
                    LspServer::new(read, write, socket).serve(service).await;

                    info!(%peer, "Client disconnected");
                    let _ = connection_tx.send(ConnectionEvent::TransportClosed);
                });
            }
            Some(event) = connection_rx.recv() => {
                lifecycle.on_event(event);
                if lifecycle.should_exit() {
                    warn!("last connection closed.");
                    break;
                }
            }
            _ = lifecycle.wait_for_idle_timeout() => {
                info!("daemon process reached the idle timeout");
                break;
            }
        }
    }
    Ok(())
}
