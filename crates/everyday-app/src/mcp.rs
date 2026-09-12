//! Letting somebody else's program use this vault's tools, over MCP.
//!
//! The desktop app's half of the MCP server. Everything about *how* a
//! request is answered lives in [`everyday_server::mcp`]; what is here is
//! the part that only makes sense with a window in front of it -- turning
//! it on from Settings, minting a token and showing it once, and switching
//! destructive tools on or off. Modelled closely on [`crate::sharing`];
//! read that module first, since most of the shape here is copied from it
//! rather than invented.
//!
//! # One runtime, one service
//!
//! Same reasoning as `sharing.rs`: the listener runs on Tauri's own runtime,
//! over the *same* [`Service`] the window uses, so a write an external agent
//! makes and a write typed in the window are the one vault's one writer,
//! never two.
//!
//! # Composing with sharing
//!
//! [`crate::sharing::Sharing`] is the other independent switch over this
//! service, and both may be on together. Neither module reaches into the
//! other's state: `start`, `stop_and_remember` and `set_destructive` below
//! only start and stop this listener, and it is
//! [`crate::state::AppState::recompose_events`] -- called by `commands.rs`
//! right after any of them returns -- that asks both switches for their
//! current sink and folds them together through [`crate::fanout::compose`].
//! See that module's doc for what a call site forgetting to do this used to
//! cost.
//!
//! # Restarting rebinds the port
//!
//! `start` calls [`stop_and_wait`](Mcp::stop_and_wait) before it binds
//! again, for the exact reason `Sharing::start` does: a bind that follows a
//! bare [`stop`](Mcp::stop) can lose the race against the old listener
//! actually releasing the socket, and land on "address already in use" --
//! which once meant a switch that looked like it failed to turn on but had
//! in fact turned itself off. `set_destructive` goes through `start` for
//! the same reason a config change that only takes effect on the next bind
//! has to: unlike sharing's remote-unlock switch, there is no atomic setter
//! on [`everyday_server::mcp::Server`] for this, because that server has no
//! per-request mutable state to hold one in the way sharing's routes do.
//! Going through `start` costs a rebind, but never that bug, because the
//! wait is already there.
//!
//! # One registry, handed in
//!
//! [`crate::sharing::Sharing`] is also a second, independent switch over
//! the same *device list* -- a paired phone and an issued MCP token are
//! rows in the one `devices.json`. `start` and `issue_token` below both
//! take an `&Registry` or `Arc<Registry>` from their caller rather than
//! opening the file themselves; `commands.rs` gets that handle from
//! [`crate::state::AppState::registry`], which opens it once for the whole
//! process. See [`everyday_server::Registry`]'s own doc for why a second
//! `Registry::open` of the same file is a bug and not just untidiness.

use everyday_server::Registry;
use everyday_server::mcp::{Config, issue_token};
use everyday_service::Scope;
use everyday_service::error::CommandResult;
use everyday_service::events::EventSink;
use serde::Serialize;
use std::sync::Arc;

use crate::listener::Listener;

/// What the settings pane draws.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    /// Where it is actually answering, which is not what was asked for when
    /// the listen address named every interface.
    pub address: Option<String>,
    /// Addresses this machine can be reached at, for the picker. Loopback
    /// is not in this list -- it is the default, offered by the interface
    /// itself rather than read out of here -- so this is the same "beyond
    /// this machine" list sharing offers, for the same reason.
    pub addresses: Vec<String>,
    pub port: u16,
    pub allow_destructive: bool,
    /// Whether a token has ever been issued. The token itself is shown
    /// exactly once, at the moment it is minted, and never again.
    pub has_token: bool,
    pub device_id: Option<String>,
}

#[derive(Default)]
pub struct Mcp {
    listener: Listener<Arc<dyn EventSink>>,
}

impl Mcp {
    /// Where the configuration and the token live -- the application's own
    /// configuration directory, never the vault. Same reasoning as
    /// `Sharing::dir`: a Postgres vault can be served from several machines,
    /// each with its own MCP switch, and `everyday backup` copies the vault,
    /// so a bearer token kept there would end up in every backup somebody
    /// ever made.
    fn dir() -> std::path::PathBuf {
        everyday_vault::config_dir()
    }

    pub fn config() -> Config {
        Config::load(&Self::dir())
    }

    /// Is a listener actually answering right now?
    pub fn is_running(&self) -> bool {
        self.listener.is_running()
    }

    /// This switch's own contribution to the event fan-out, if it is on.
    ///
    /// `None` when off, matching [`crate::sharing::Sharing`]'s pattern, so a
    /// caller composing the whole fan-out never has to ask twice whether
    /// this switch is running.
    pub fn sink(&self) -> Option<Arc<dyn EventSink>> {
        self.listener.with(|running| running.map(|r| r.server.clone()))
    }

    /// Start answering.
    ///
    /// See the module doc's "Restarting rebinds the port" for why
    /// `stop_and_wait` comes first. `registry` is the process's one
    /// [`Registry`] over `devices.json` -- see this module's doc's "One
    /// registry, handed in" -- and is handed straight to
    /// [`everyday_server::mcp::start`] rather than opened again here. The
    /// caller -- `commands.rs` -- points the service's events at the window
    /// and every other listener once this returns, by calling
    /// [`crate::state::AppState::recompose_events`]; see the module doc's
    /// "Composing with sharing".
    pub async fn start(
        &self,
        service: Arc<everyday_service::Service>,
        mut config: Config,
        registry: Arc<Registry>,
    ) -> CommandResult<McpStatus> {
        self.stop_and_wait().await;

        let dir = Self::dir();
        let running = everyday_server::mcp::start(service, registry, &config).await?;

        config.enabled = true;
        config.save(&dir)?;
        self.listener.set(running);
        Ok(self.status())
    }

    /// Stop answering.
    pub fn stop(&self) {
        self.listener.stop();
    }

    /// Stop answering, and wait for the port to be released.
    ///
    /// For a caller about to bind the same address. Signalling a shutdown
    /// is not the same moment as the socket actually being free.
    pub async fn stop_and_wait(&self) {
        self.listener.stop_and_wait().await;
    }

    /// Stop answering, and remember not to start next time.
    ///
    /// As with `start`, the caller recomposes the event fan-out once this
    /// returns; nothing here touches `service.set_events` any more.
    pub fn stop_and_remember(&self) -> CommandResult<McpStatus> {
        self.stop();
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.enabled = false;
        config.save(&dir)?;
        Ok(self.status())
    }

    /// Change whether a destructive tool is offered at all.
    ///
    /// A restart, unlike `Sharing::set_remote_unlock` -- see the module
    /// doc's "Restarting rebinds the port" for why that is the honest
    /// choice here rather than a bug re-introduced. When the listener is
    /// off this only writes the file, exactly as `set_remote_unlock` does
    /// in the same case.
    pub async fn set_destructive(
        &self,
        service: Arc<everyday_service::Service>,
        allow: bool,
        registry: Arc<Registry>,
    ) -> CommandResult<McpStatus> {
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.allow_destructive = allow;
        if self.is_running() {
            return self.start(service, config, registry).await;
        }
        config.save(&dir)?;
        Ok(self.status())
    }

    /// Mint a token for an MCP client, and hand it back once.
    ///
    /// `registry` is the process's one [`Registry`], the same `Arc` a
    /// running listener is authenticating against if the switch happens to
    /// be on -- see this module's doc's "One registry, handed in" -- so a
    /// token minted while it is on authenticates immediately, with no
    /// restart in between, and one minted while it is off is still in the
    /// list a start moments later will use.
    pub fn issue_token(&self, registry: &Registry, scopes: Vec<Scope>) -> CommandResult<String> {
        let dir = Self::dir();
        issue_token(registry, &dir, scopes)
    }

    pub fn status(&self) -> McpStatus {
        self.listener.with(|running| {
            let config = Self::config();
            McpStatus {
                running: running.is_some(),
                address: running.map(|r| r.address.to_string()),
                addresses: crate::sharing::advertisable_addresses(),
                port: config.listen.port(),
                allow_destructive: config.allow_destructive,
                has_token: config.token.is_some(),
                device_id: config.device_id,
            }
        })
    }
}
