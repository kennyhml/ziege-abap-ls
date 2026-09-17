//! Connection counts and process lifetime for the TCP listener.
//!
//! [`ConnectionLifecycle`] owns the state needed to decide when the process can
//! exit. The listener records accepted transports and forwards [`ConnectionEvent`]
//! messages from its workers. Repository and editor state have their own lifetimes.

use std::time::Duration;

use tokio::time::{self, Instant};

/// Internal conncection events. These allow individual connction server
/// workers to communicate with the surrounding framework to inform
/// it of initialization or termination events so that the framework
/// can inititiate clear sequences such as daemon shutdown timers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionEvent {
    Initialized,
    TransportClosed,
}

/// Tracks the connections that keep this server process alive.
///
/// A regular process waits for its first client, then exits when the last
/// transport closes. A daemon instead waits for an idle timeout, both at startup
/// and after its last client disconnects. Any accepted connection cancels that
/// timeout, including probes which never complete LSP initialization.
///
/// The listener uses [`Self::should_exit`] after connection events and waits on
/// [`Self::wait_for_idle_timeout`] alongside incoming connections. Only this type
/// updates the counters and deadline, keeping those decisions together.
pub(crate) struct ConnectionLifecycle {
    active_connections: usize,
    has_accepted_connection: bool,
    idle_deadline: Option<Instant>,
    daemon: bool,
    idle_timeout: Duration,
}

impl ConnectionLifecycle {
    pub(crate) fn new(daemon: bool, idle_timeout: Duration) -> Self {
        Self {
            active_connections: 0,
            has_accepted_connection: false,
            idle_deadline: daemon.then(|| Instant::now() + idle_timeout),
            daemon,
            idle_timeout,
        }
    }

    /// Records an accepted transport before its LSP worker starts serving it.
    pub(crate) fn connected(&mut self) {
        self.active_connections += 1;
        self.has_accepted_connection = true;
        self.idle_deadline = None;
    }

    /// Applies a worker notification. A shutdown acknowledgement alone does not
    /// close a connection. [`ConnectionEvent::TransportClosed`] is sent after
    /// [`tower_lsp_server::Server::serve`] returns.
    pub(crate) fn on_event(&mut self, event: ConnectionEvent) {
        match event {
            ConnectionEvent::Initialized => self.idle_deadline = None,
            ConnectionEvent::TransportClosed => self.disconnected(),
        }
    }

    fn disconnected(&mut self) {
        self.active_connections = self.active_connections.saturating_sub(1);
        if self.daemon && self.active_connections == 0 {
            self.idle_deadline = Some(Instant::now() + self.idle_timeout);
        }
    }

    /// Whether a regular process should exit after its last client disconnects.
    /// Daemon shutdown is driven by [`Self::wait_for_idle_timeout`] instead.
    pub(crate) fn should_exit(&self) -> bool {
        !self.daemon && self.has_accepted_connection && self.active_connections == 0
    }

    /// Completes only when a daemon has remained idle until its deadline.
    ///
    /// The listener creates this wait again after each event. When a connection
    /// is accepted, [`Self::connected`] clears the deadline so the next wait
    /// remains pending until the process becomes idle again.
    pub(crate) async fn wait_for_idle_timeout(&self) {
        match self.idle_deadline {
            Some(deadline) => time::sleep_until(deadline).await,
            None => std::future::pending::<()>().await,
        }
    }
}
