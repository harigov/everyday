//! Serving this window's vault to other machines.
//!
//! The desktop app's half of server mode. Everything about *how* a vault is
//! served lives in [`everyday_server`]; what is here is the part that only
//! makes sense with a window in front of it -- turning it on from a settings
//! pane, showing a link and a QR code, listing the devices that have paired,
//! and taking one away again.
//!
//! # One runtime, one service
//!
//! The server runs on Tauri's own runtime and over the *same* [`Service`] the
//! window uses. Not a copy, and not a second runtime. A second runtime would
//! double the blocking pool and, worse, would mean the vault's one-writer rule
//! was being kept by two sets of threads that know nothing about each other. The
//! same service is also what makes a write from a phone appear in this window:
//! there is one vault handle and one event sink, fanned out to both.
//!
//! # Composing with MCP
//!
//! [`crate::mcp::Mcp`] is a second, independent switch over the same
//! service, and it wants a say in the same fan-out. Neither module reaches
//! into the other's state to get it: `start`, `stop` and `stop_and_remember`
//! below only start and stop the listener itself, and it is
//! [`crate::state::AppState::recompose_events`] -- called by `commands.rs`
//! right after any of them returns -- that asks both switches for their
//! current sink and folds them together through [`crate::fanout::compose`].
//! That used to be seven call sites in `commands.rs` each remembering to ask
//! the *other* listener for its sink, and forgetting one silently dropped a
//! stream rather than failing loudly.
//!
//! # One registry, handed in
//!
//! [`crate::mcp::Mcp`] is also a second, independent switch over the same
//! *device list* -- a paired phone and an issued MCP token are rows in the
//! one `devices.json`. Every method below that touches it therefore takes
//! an `&Registry` or `Arc<Registry>` from its caller rather than opening
//! the file itself; `commands.rs` gets that handle from
//! [`crate::state::AppState::registry`], which opens it once for the whole
//! process. See [`everyday_server::Registry`]'s own doc for why a second
//! `Registry::open` of the same file is a bug and not just untidiness.

use everyday_server::{Broadcaster, Config, Registry, Server};
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::EventSink;
use serde::Serialize;
use std::sync::{Arc, Mutex};

use crate::listener::Listener;

/// What the settings pane draws.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    pub sharing: bool,
    /// Where it is actually answering, which is not what was asked for when the
    /// listen address named every interface.
    pub address: Option<String>,
    /// Addresses this machine can be reached at, for the picker.
    pub addresses: Vec<String>,
    pub port: u16,
    pub allow_remote_unlock: bool,
    pub devices: Vec<everyday_server::DeviceInfo>,
    /// The outstanding invitation, if somebody pressed "add a device".
    pub invitation: Option<everyday_server::pairing::Invitation>,
    /// How many clients have an event stream open right now.
    pub listeners: usize,
}

#[derive(Default)]
pub struct Sharing {
    listener: Listener<Arc<Server>>,
    /// Kept across a stop and start so the broadcaster's own state --
    /// nothing today, but see `Broadcaster`'s own doc -- does not have to be
    /// rebuilt for no reason when somebody toggles the switch.
    ///
    /// Holds no [`Registry`] of its own: the device list is
    /// [`crate::state::AppState`]'s to open, once, and every method here
    /// that needs it is handed the same `Arc` that a running server
    /// authenticates against, whether or not one happens to be running.
    parts: Mutex<Option<SharedParts>>,
    invitation: Mutex<Option<everyday_server::pairing::Invitation>>,
}

struct SharedParts {
    broadcaster: Arc<Broadcaster>,
}

impl Sharing {
    /// Where the configuration, the certificate and the device list live.
    ///
    /// The application's own configuration directory, never the vault: a
    /// Postgres vault can be served from two machines and each has its own
    /// devices, and `everyday backup` copies a vault, so a private key kept
    /// there would end up in every backup somebody ever made.
    fn dir() -> std::path::PathBuf {
        everyday_vault::config_dir()
    }

    pub fn config() -> Config {
        Config::load(&Self::dir())
    }

    /// Change whether a connected computer may unlock the vault.
    ///
    /// Not a restart. It used to be one, and a restart rebinds the port --
    /// which races the old listener's shutdown, and losing that race left
    /// sharing switched off with "address already in use" behind it. The
    /// server holds it as an atomic and reads it per request.
    pub fn set_remote_unlock(
        &self,
        registry: &Registry,
        allow: bool,
    ) -> CommandResult<ShareStatus> {
        self.listener.with(|running| {
            if let Some(running) = running {
                running.server.set_allow_remote_unlock(allow);
            }
        });
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.allow_remote_unlock = allow;
        config.save(&dir)?;
        Ok(self.status(registry))
    }

    /// Start answering.
    ///
    /// `registry` is the process's one [`Registry`] over `devices.json` --
    /// see this module's doc's "One registry, handed in" -- and is handed
    /// straight to [`everyday_server::prepare_with`] rather than opened
    /// again here. The caller -- `commands.rs` -- is the one that points the
    /// service's events at the window and every other listener, by calling
    /// [`crate::state::AppState::recompose_events`] once this returns; see
    /// the module doc's "Composing with MCP".
    pub async fn start(
        &self,
        service: Arc<everyday_service::Service>,
        mut config: Config,
        registry: Arc<Registry>,
    ) -> CommandResult<ShareStatus> {
        // Awaited, not merely signalled: what follows binds an address, and
        // the old listener may still be holding it.
        self.stop_and_wait().await;
        everyday_server::install_crypto_provider();

        let dir = Self::dir();
        let parts = everyday_server::prepare_with(&dir, &config, registry.clone())?;
        let broadcaster = parts.broadcaster.clone();

        let name = service.get().map(|v| v.status().name).unwrap_or_else(|| "Every Day".into());
        let running = everyday_server::start(service, parts, &config, name).await?;

        config.enabled = true;
        config.save(&dir)?;
        *self.parts.lock().unwrap() = Some(SharedParts { broadcaster });
        self.listener.set(running);
        Ok(self.status(&registry))
    }

    /// Stop answering. Paired devices are remembered.
    pub fn stop(&self) {
        self.listener.stop();
        *self.invitation.lock().unwrap() = None;
    }

    /// Stop answering, and wait for the socket to be released.
    ///
    /// For a caller that is about to bind the same address. Signalling a
    /// shutdown is not the same as having shut down.
    pub async fn stop_and_wait(&self) {
        *self.invitation.lock().unwrap() = None;
        self.listener.stop_and_wait().await;
    }

    /// Stop answering, and remember not to start next time.
    ///
    /// As with `start`, the caller recomposes the event fan-out once this
    /// returns; nothing here touches `service.set_events` any more.
    pub fn stop_and_remember(&self, registry: &Registry) -> CommandResult<ShareStatus> {
        self.stop();
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.enabled = false;
        config.save(&dir)?;
        Ok(self.status(registry))
    }

    /// Offer to pair, for the next five minutes.
    pub fn invite(&self, registry: &Registry) -> CommandResult<ShareStatus> {
        let invitation = self.listener.with(|running| {
            let running =
                running.ok_or_else(|| CommandError::new("not_sharing", "turn sharing on first"))?;
            let code = running.server.registry.new_pairing_code();
            Ok::<_, CommandError>(everyday_server::pairing::invitation(
                &advertised_host(running),
                running.fingerprint(),
                &code,
                &running.server.name,
            ))
        })?;
        *self.invitation.lock().unwrap() = Some(invitation);
        Ok(self.status(registry))
    }

    /// Withdraw the offer.
    pub fn cancel_invite(&self, registry: &Registry) -> ShareStatus {
        self.listener.with(|running| {
            if let Some(running) = running {
                running.server.registry.clear_pairing_code();
            }
        });
        *self.invitation.lock().unwrap() = None;
        self.status(registry)
    }

    pub fn revoke(&self, registry: &Registry, id: &str) -> CommandResult<ShareStatus> {
        registry.revoke(id)?;
        Ok(self.status(registry))
    }

    /// Is a server actually listening right now?
    pub fn is_running(&self) -> bool {
        self.listener.is_running()
    }

    /// This switch's own contribution to the event fan-out, if it is on.
    ///
    /// `None` when off -- not when `parts` merely still holds the last
    /// session's broadcaster, which it does even while stopped (see
    /// `parts`'s own doc), because nothing should be told to fan events out
    /// to a broadcaster nobody is running a server against any more.
    pub fn sink(&self) -> Option<Arc<dyn EventSink>> {
        if !self.is_running() {
            return None;
        }
        self.parts.lock().unwrap().as_ref().map(|p| p.broadcaster.clone() as Arc<dyn EventSink>)
    }

    /// `registry` is read directly rather than through `parts`: it is the
    /// same [`Arc`] whether or not a server happens to be running right
    /// now, so there is no "sharing is off" fallback to open a second copy
    /// of the file for -- see this module's doc's "One registry, handed
    /// in".
    pub fn status(&self, registry: &Registry) -> ShareStatus {
        self.listener.with(|running| {
            let config = Self::config();
            ShareStatus {
                sharing: running.is_some(),
                address: running.map(|r| r.address.to_string()),
                addresses: advertisable_addresses(),
                port: config.listen.port(),
                // From the running server when there is one, because it is
                // live there and the file is only where it is remembered.
                allow_remote_unlock: running
                    .map(|r| r.server.allow_remote_unlock())
                    .unwrap_or(config.allow_remote_unlock),
                devices: registry.devices(),
                invitation: self.invitation.lock().unwrap().clone(),
                listeners: self
                    .parts
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|p| p.broadcaster.listeners())
                    .unwrap_or(0),
            }
        })
    }
}

/// The address to put in a pairing link.
///
/// The listening address when it names one, and otherwise the first address
/// this machine actually has -- because `0.0.0.0:7397` is the right thing to
/// *listen* on and a useless thing to hand somebody.
fn advertised_host(running: &everyday_server::Running<Arc<Server>>) -> String {
    if !running.address.ip().is_unspecified() {
        return running.address.to_string();
    }
    match advertisable_addresses().first() {
        Some(ip) => format!("{ip}:{}", running.address.port()),
        None => format!("127.0.0.1:{}", running.address.port()),
    }
}

/// The addresses worth offering, best first.
///
/// A Tailscale or other CGNAT address comes first when there is one: it is
/// reachable from outside the building, it is already encrypted, and it does
/// not change when somebody joins a different wifi -- which is three reasons why
/// it is the address somebody sharing a vault actually wants.
///
/// `pub(crate)` rather than private: [`crate::mcp::Mcp`] offers the same
/// picker for the same reason, over a listener that defaults to loopback
/// instead of every interface, and there is no second way to rank a
/// Tailscale address above an ordinary LAN one worth writing twice.
pub(crate) fn advertisable_addresses() -> Vec<String> {
    let mut addresses = everyday_server::tls::interface_addresses();
    addresses.sort_by_key(|ip| match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            // 100.64.0.0/10 is the shared address space Tailscale uses.
            if a == 100 && (64..128).contains(&b) { 0 } else { 1 }
        }
        std::net::IpAddr::V6(_) => 2,
    });
    addresses.into_iter().map(|ip| ip.to_string()).collect()
}
