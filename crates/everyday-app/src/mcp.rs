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
//! other's state; `start`, `stop_and_remember` and `set_destructive` below
//! all take an `other` list of whatever sharing is currently contributing,
//! supplied by the caller in `commands.rs`, and fold it in through
//! [`crate::fanout::compose`]. See that module's doc.
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

use everyday_server::Registry;
use everyday_server::mcp::{Config, Running, issue_token};
use everyday_service::Scope;
use everyday_service::error::CommandResult;
use everyday_service::events::EventSink;
use serde::Serialize;
use std::sync::{Arc, Mutex};

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
    running: Mutex<Option<Running>>,
    /// The registry a running listener is authenticating against -- the
    /// exact [`Arc`] passed into [`everyday_server::mcp::start`], not a
    /// second [`Registry::open`] of the same file. Kept so [`Mcp::issue_token`]
    /// mints into the copy the live listener is already checking requests
    /// against: a token minted while the switch is on has to work
    /// immediately, with no restart in between, or "turning it on issues a
    /// token" (see `README.md`) would be a lie the first time somebody
    /// tried it. `None` while the switch is off, when there is no live
    /// listener for a second copy to disagree with -- see `issue_token`.
    registry: Mutex<Option<Arc<Registry>>>,
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
        self.running.lock().unwrap().is_some()
    }

    /// This switch's own contribution to the event fan-out, if it is on.
    ///
    /// `None` when off, matching [`crate::sharing::Sharing`]'s pattern, so a
    /// caller composing the whole fan-out never has to ask twice whether
    /// this switch is running.
    pub fn sink(&self) -> Option<Arc<dyn EventSink>> {
        self.running.lock().unwrap().as_ref().map(|r| r.sink.clone())
    }

    /// Start answering, and point the service's events at the window,
    /// every open MCP stream, and whatever else (sharing) is already
    /// listening.
    ///
    /// See the module doc's "Restarting rebinds the port" for why
    /// `stop_and_wait` comes first, and "Composing with sharing" for `other`.
    pub async fn start(
        &self,
        service: Arc<everyday_service::Service>,
        window_sink: Arc<dyn EventSink>,
        other: Vec<Arc<dyn EventSink>>,
        mut config: Config,
    ) -> CommandResult<McpStatus> {
        self.stop_and_wait().await;

        let dir = Self::dir();
        let registry = Arc::new(Registry::open(dir.join(everyday_server::DEVICES_FILE))?);
        let running =
            everyday_server::mcp::start(service.clone(), registry.clone(), &config).await?;

        // The window still needs its events, so does every open MCP stream
        // -- `running.sink` is what carries `lock_state` to
        // `notifications/tools/list_changed`, see `everyday_server::mcp`'s
        // module doc -- and so does sharing if it is on.
        let mut sinks: Vec<Arc<dyn EventSink>> = vec![running.sink.clone()];
        sinks.extend(other);
        service.set_events(crate::fanout::compose(window_sink, sinks));

        config.enabled = true;
        config.save(&dir)?;
        *self.registry.lock().unwrap() = Some(registry);
        *self.running.lock().unwrap() = Some(running);
        Ok(self.status())
    }

    /// Stop answering.
    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            running.stop();
        }
        *self.registry.lock().unwrap() = None;
    }

    /// Stop answering, and wait for the port to be released.
    ///
    /// For a caller about to bind the same address. Signalling a shutdown
    /// is not the same moment as the socket actually being free.
    pub async fn stop_and_wait(&self) {
        let previous = self.running.lock().unwrap().take();
        *self.registry.lock().unwrap() = None;
        if let Some(running) = previous {
            running.stop_and_wait().await;
        }
    }

    /// Stop answering, and remember not to start next time.
    pub fn stop_and_remember(
        &self,
        window_sink: Arc<dyn EventSink>,
        other: Vec<Arc<dyn EventSink>>,
        service: &everyday_service::Service,
    ) -> CommandResult<McpStatus> {
        self.stop();
        // Back to the window and whatever else (sharing) is still
        // listening. Without this the fan-out would carry a stream nothing
        // is reading from any more, or -- the bug this exists to avoid --
        // would drop sharing's broadcaster because MCP stopped.
        service.set_events(crate::fanout::compose(window_sink, other));
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
        window_sink: Arc<dyn EventSink>,
        other: Vec<Arc<dyn EventSink>>,
        allow: bool,
    ) -> CommandResult<McpStatus> {
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.allow_destructive = allow;
        if self.is_running() {
            return self.start(service, window_sink, other, config).await;
        }
        config.save(&dir)?;
        Ok(self.status())
    }

    /// Mint a token for an MCP client, and hand it back once.
    ///
    /// Reuses the registry a running listener already holds -- see this
    /// struct's own doc on `registry` -- so a token minted while the switch
    /// is on authenticates immediately, against the exact copy the listener
    /// is checking requests against. When the switch is off, opens the
    /// registry file fresh, mints into it, and lets it go: there is no live
    /// listener for a second in-memory copy to fall out of step with.
    pub fn issue_token(&self, scopes: Vec<Scope>) -> CommandResult<String> {
        let dir = Self::dir();
        let live = self.registry.lock().unwrap().clone();
        match live {
            Some(registry) => issue_token(&registry, &dir, scopes),
            None => {
                let registry = Registry::open(dir.join(everyday_server::DEVICES_FILE))?;
                issue_token(&registry, &dir, scopes)
            }
        }
    }

    pub fn status(&self) -> McpStatus {
        let running = self.running.lock().unwrap();
        let config = Self::config();
        McpStatus {
            running: running.is_some(),
            address: running.as_ref().map(|r| r.address.to_string()),
            addresses: crate::sharing::advertisable_addresses(),
            port: config.listen.port(),
            allow_destructive: config.allow_destructive,
            has_token: config.token.is_some(),
            device_id: config.device_id,
        }
    }
}
