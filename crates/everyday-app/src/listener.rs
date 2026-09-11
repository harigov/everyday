//! The lifecycle every switch that starts a listener in this process shares.
//!
//! [`crate::sharing::Sharing`] and [`crate::mcp::Mcp`] each turn a listener on
//! and off from a settings pane, and each used to keep its own copy of the
//! same handful of moves: hold whatever `everyday_server` handed back inside
//! a `Mutex`, take it out to stop it, wait for the port to actually be free
//! before rebinding. The two copies differed only in which
//! `everyday_server::Running<_>` they held -- an `Arc<everyday_server::Server>`
//! for sharing, an `Arc<dyn EventSink>` for MCP -- which is exactly what a
//! generic parameter is for. What each module still keeps for itself is
//! whatever is actually its own: sharing clears an outstanding pairing
//! invitation on the way down, MCP does not, and neither module's `start` looks
//! anything like the other's once the actual binding begins.

use std::sync::Mutex;

/// A listener this process starts and stops, generic over `X`: whatever
/// `everyday_server::Running<X>` carries once bound.
pub(crate) struct Listener<X> {
    running: Mutex<Option<everyday_server::Running<X>>>,
}

impl<X> Default for Listener<X> {
    fn default() -> Self {
        Self { running: Mutex::new(None) }
    }
}

impl<X> Listener<X> {
    /// Is a server actually answering right now?
    pub(crate) fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    /// Look at whatever is running, if anything, without taking it. For
    /// reading something off it -- a fingerprint, a registry -- while it
    /// keeps running.
    pub(crate) fn with<T>(&self, f: impl FnOnce(Option<&everyday_server::Running<X>>) -> T) -> T {
        f(self.running.lock().unwrap().as_ref())
    }

    /// Record that `running` is what is now listening. The caller has
    /// already stopped whatever was there before, or there was nothing to.
    pub(crate) fn set(&self, running: everyday_server::Running<X>) {
        *self.running.lock().unwrap() = Some(running);
    }

    /// Stop answering. Connections in flight are allowed to finish.
    pub(crate) fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            running.stop();
        }
    }

    /// Stop answering, and wait for the socket to be released -- for a
    /// caller about to bind the same address. Signalling a shutdown is not
    /// the same moment as the socket actually being free.
    pub(crate) async fn stop_and_wait(&self) {
        let previous = self.running.lock().unwrap().take();
        if let Some(running) = previous {
            running.stop_and_wait().await;
        }
    }
}
