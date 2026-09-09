//! The shell's side of being a client.
//!
//! A [`Session`] is either a vault this process holds or a connection to one it
//! does not, and everything above this point takes whichever it is handed. That
//! is the claim the whole feature rests on: a remote client is the same
//! application, with its own window, its own tray and its own keyboard, and the
//! only thing that differs is which machine the commands run on.
//!
//! # The events task
//!
//! A connected client keeps one stream open and turns what arrives into the
//! same window events a local vault raises. It reconnects with backoff, and
//! after a few failures says so as a *condition* rather than a toast -- the
//! vault being unreachable is true until it is not, which is what
//! `Notices.svelte` is for.

use everyday_server::client::{Connection, RemoteClient, ServerEvent};
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::EventSink;
use everyday_service::{Ctx, Service};
use std::sync::Arc;
use std::time::Duration;

/// What this window is looking at.
pub enum Session {
    /// A vault in this process.
    Local(Arc<Service>),
    /// A vault on another machine.
    Remote(Arc<Remote>),
}

/// A connection, and the task watching it.
pub struct Remote {
    pub client: Arc<RemoteClient>,
    /// Cancels the events task when the session is replaced.
    stop: tokio::sync::watch::Sender<bool>,
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.client.clear_cache();
    }
}

/// How long to wait before trying the stream again, and how far that grows.
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Failures in a row before the window is told the vault is unreachable.
///
/// Not the first one. A laptop closing its lid, a phone changing cell, a
/// Tailscale route settling -- all of those drop a stream and all of them come
/// back within a second, and a banner for each would be a banner most of the
/// time.
const QUIET_FAILURES: u32 = 3;

impl Remote {
    /// Connect, and start listening for what the server says.
    pub fn start(client: RemoteClient, sink: Arc<dyn EventSink>) -> Arc<Self> {
        let client = Arc::new(client);
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let remote = Arc::new(Self { client: client.clone(), stop });

        tauri::async_runtime::spawn(async move {
            let mut delay = RECONNECT_MIN;
            let mut failures = 0u32;
            let mut told = false;
            loop {
                if *rx.borrow() {
                    return;
                }
                let sink_for_stream = sink.clone();
                let cache_owner = client.clone();
                let stream = client.events(move |event| match event {
                    ServerEvent::Notify(notification) => sink_for_stream.notify(notification),
                    ServerEvent::Changed(change) => sink_for_stream.changed(change),
                    ServerEvent::LockState { locked } => {
                        if locked {
                            // Every cached byte belongs to a vault that is no
                            // longer open. This is the one place a client holds
                            // plaintext, and the lock screen must mean it holds
                            // none.
                            cache_owner.clear_cache();
                        }
                        sink_for_stream.lock_state(locked);
                    }
                });

                // Raced against the stop signal, not merely checked before it.
                //
                // The stream is a request with a day-long timeout, so awaiting
                // it on its own means a session that has been replaced keeps
                // pumping another machine's events into this window until that
                // day is up -- lists reloading for a vault nobody is looking
                // at, and a lock screen appearing over a local one.
                let outcome = tokio::select! {
                    outcome = stream => outcome,
                    _ = rx.changed() => return,
                };
                if *rx.borrow() {
                    return;
                }

                // Both endings are the same thing from here. `Ok` means the
                // request succeeded and the body later finished, which for a
                // stream that is meant to stay open all day means the server
                // went away; `Err` means it never opened. Neither is
                // distinguishable from the other by anything this can see, so
                // both widen the delay.
                match &outcome {
                    Ok(()) => tracing::debug!("the event stream ended"),
                    Err(e) => tracing::debug!(error = %e, "the event stream failed"),
                }
                failures += 1;

                if failures >= QUIET_FAILURES && !told {
                    told = true;
                    sink.notify(
                        everyday_service::Notification::warning("Not connected")
                            .body(
                                "This vault is on another computer and it is not answering. \
                                 Your work is safe there; nothing can be saved until it is back.",
                            )
                            .for_user()
                            .key("remote:unreachable"),
                    );
                }

                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = rx.changed() => return,
                }
                delay = (delay * 2).min(RECONNECT_MAX);

                // Whether the *next* attempt gets a fresh budget is decided by
                // whether it works, not by anything here. `hello` is a short
                // request against the same connection, so it answers quickly
                // either way, and a server that is back resets both the delay
                // and the banner -- which the old code could not do, because it
                // reset them only when `failures` was zero and nothing ever set
                // it back to zero.
                if client.reachable().await {
                    delay = RECONNECT_MIN;
                    failures = 0;
                    if told {
                        told = false;
                        sink.notify(
                            everyday_service::Notification::new(
                                everyday_service::Level::Success,
                                "Connected again",
                            )
                            .for_user()
                            .key("remote:unreachable"),
                        );
                    }
                }
            }
        });

        remote
    }
}

impl Session {
    /// Run a command, wherever the vault is.
    pub async fn call(
        &self,
        ctx: Ctx,
        name: &str,
        args: serde_json::Value,
    ) -> CommandResult<serde_json::Value> {
        match self {
            Session::Local(service) => service.call(ctx, name, args).await,
            Session::Remote(remote) => remote.client.call(&ctx, name, args).await,
        }
    }

    pub async fn put_blob(&self, ctx: &Ctx, bytes: Vec<u8>) -> CommandResult<String> {
        match self {
            Session::Local(service) => service.put_blob(ctx, bytes).await,
            Session::Remote(remote) => remote.client.put_blob(bytes).await,
        }
    }

    /// A turn of the assistant.
    pub async fn send_message(
        &self,
        ctx: Ctx,
        args: serde_json::Value,
        sink: everyday_service::agent::Sink,
    ) -> CommandResult<()> {
        match self {
            Session::Local(service) => service.send_message(ctx, args, sink).await,
            Session::Remote(remote) => remote.client.send_message(args, sink).await,
        }
    }

    /// What the interface should draw. `None` means no vault at all.
    pub async fn status(&self) -> Option<everyday_core::VaultStatus> {
        match self {
            Session::Local(service) => service.get().map(|v| v.status()),
            Session::Remote(remote) => {
                let ctx = Ctx::local();
                let value = remote.client.call(&ctx, "status", serde_json::json!({})).await.ok()?;
                serde_json::from_value(value).ok()
            }
        }
    }

    /// The connection this session is over, if it is a remote one.
    pub fn connection(&self) -> Option<Connection> {
        match self {
            Session::Local(_) => None,
            Session::Remote(remote) => Some(remote.client.connection().clone()),
        }
    }
}

/// Reconnect to a server this copy has paired with before.
pub async fn resume(id: &str) -> CommandResult<RemoteClient> {
    let connection = crate::remotes::list()
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| CommandError::new("not_found", "there is no such connection"))?;
    let token = crate::remotes::token(&connection.id)?;
    RemoteClient::resume(connection, token).await
}
