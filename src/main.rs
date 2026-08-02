mod backend;

use std::{env, error::Error, sync::Arc, time::Duration};

use abap_lsp::context::ContextStore;
use backend::Backend;
use tokio::{
    net::TcpListener,
    sync::mpsc,
    time::{Instant, sleep_until},
};
use tower_lsp_server::{LspService, Server};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_ADDRESS: &str = "127.0.0.1:9257";
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let address = env::var("ZIEGE_LSP_ADDRESS").unwrap_or_else(|_| DEFAULT_ADDRESS.to_owned());
    let idle_timeout = env::var("ZIEGE_IDLE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_IDLE_TIMEOUT);
    let listener = TcpListener::bind(&address).await?;
    info!(%address, "Ziege language server listening");

    let contexts = Arc::new(ContextStore::default());
    let (disconnected_tx, mut disconnected_rx) = mpsc::unbounded_channel();
    let mut active_connections = 0usize;
    let mut idle_deadline = Some(Instant::now() + idle_timeout);

    loop {
        if let Some(deadline) = idle_deadline {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => {
                            warn!(%error, "failed to accept LSP connection");
                            continue;
                        }
                    };
                    active_connections += 1;
                    idle_deadline = None;
                    spawn_connection(stream, peer, contexts.clone(), disconnected_tx.clone());
                }
                Some(()) = disconnected_rx.recv() => {
                    active_connections = active_connections.saturating_sub(1);
                    if active_connections == 0 {
                        idle_deadline = Some(Instant::now() + idle_timeout);
                    }
                }
                _ = sleep_until(deadline) => {
                    if active_connections == 0 {
                        info!("idle timeout elapsed; exiting");
                        break;
                    }
                    idle_deadline = None;
                }
            }
        } else {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => {
                            warn!(%error, "failed to accept LSP connection");
                            continue;
                        }
                    };
                    active_connections += 1;
                    spawn_connection(stream, peer, contexts.clone(), disconnected_tx.clone());
                }
                Some(()) = disconnected_rx.recv() => {
                    active_connections = active_connections.saturating_sub(1);
                    if active_connections == 0 {
                        idle_deadline = Some(Instant::now() + idle_timeout);
                    }
                }
            }
        }
    }
    Ok(())
}

fn spawn_connection(
    stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
    contexts: Arc<ContextStore>,
    disconnected: mpsc::UnboundedSender<()>,
) {
    tokio::spawn(async move {
        let (read, write) = tokio::io::split(stream);
        let (service, socket) = LspService::build(|client| Backend::new(client, contexts))
            .custom_method("ziege/project/systems", Backend::project_systems)
            .custom_method("ziege/fileSystem/readDirectory", Backend::read_directory)
            .custom_method("ziege/fileSystem/readFile", Backend::read_file)
            .custom_method(
                "ziege/objectCreation/options",
                Backend::object_creation_options,
            )
            .custom_method(
                "ziege/objectCreation/refreshTransports",
                Backend::refresh_transports,
            )
            .finish();
        info!(%peer, "LSP client connected");
        Server::new(read, write, socket).serve(service).await;
        info!(%peer, "LSP client disconnected");
        if disconnected.send(()).is_err() {
            error!("connection lifecycle receiver stopped");
        }
    });
}
