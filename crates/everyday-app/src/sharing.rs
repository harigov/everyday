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
//! service, and it wants a say in the same fan-out. Rather than either
//! module reaching into the other's state, `start` and `stop_and_remember`
//! below take an `other` list of whatever the other switch is currently
//! contributing, supplied by the caller in `commands.rs`, and fold it in
//! through [`crate::fanout::compose`].

use everyday_server::{Broadcaster, Config, Running};
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::EventSink;
use serde::Serialize;
use std::sync::{Arc, Mutex};

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
    running: Mutex<Option<Running>>,
    /// Kept across a stop and start so a device does not have to pair again
    /// when somebody toggles the switch.
    parts: Mutex<Option<SharedParts>>,
    invitation: Mutex<Option<everyday_server::pairing::Invitation>>,
}

struct SharedParts {
    registry: Arc<everyday_server::Registry>,
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
    pub fn set_remote_unlock(&self, allow: bool) -> CommandResult<ShareStatus> {
        if let Some(running) = self.running.lock().unwrap().as_ref() {
            running.server.set_allow_remote_unlock(allow);
        }
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.allow_remote_unlock = allow;
        config.save(&dir)?;
        Ok(self.status())
    }

    /// Start answering, and point the service's events at the window, every
    /// connected device, and whatever else (MCP) is already listening.
    pub async fn start(
        &self,
        service: Arc<everyday_service::Service>,
        window_sink: Arc<dyn EventSink>,
        other: Vec<Arc<dyn EventSink>>,
        mut config: Config,
    ) -> CommandResult<ShareStatus> {
        // Awaited, not merely signalled: what follows binds an address, and
        // the old listener may still be holding it.
        self.stop_and_wait().await;
        everyday_server::install_crypto_provider();

        let dir = Self::dir();
        let parts = everyday_server::prepare(&dir, &config)?;
        let registry = parts.registry.clone();
        let broadcaster = parts.broadcaster.clone();

        // The window still needs its events, so does every paired device,
        // and so does MCP if it is on. None of the three can be told to
        // look at either of the others'.
        let mut sinks: Vec<Arc<dyn EventSink>> = vec![broadcaster.clone()];
        sinks.extend(other);
        service.set_events(crate::fanout::compose(window_sink, sinks));

        let name = service.get().map(|v| v.status().name).unwrap_or_else(|| "Every Day".into());
        let running = everyday_server::start(service, parts, &config, name).await?;

        config.enabled = true;
        config.save(&dir)?;
        *self.parts.lock().unwrap() = Some(SharedParts { registry, broadcaster });
        *self.running.lock().unwrap() = Some(running);
        Ok(self.status())
    }

    /// Stop answering. Paired devices are remembered.
    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            running.stop();
        }
        *self.invitation.lock().unwrap() = None;
    }

    /// Stop answering, and wait for the socket to be released.
    ///
    /// For a caller that is about to bind the same address. Signalling a
    /// shutdown is not the same as having shut down.
    pub async fn stop_and_wait(&self) {
        let previous = self.running.lock().unwrap().take();
        *self.invitation.lock().unwrap() = None;
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
    ) -> CommandResult<ShareStatus> {
        self.stop();
        // Back to the window and whatever else (MCP) is still listening.
        // Without this the fan-out would keep a broadcaster alive that
        // nothing is reading from, or -- the bug this exists to avoid --
        // would drop MCP's stream on the floor because sharing stopped.
        service.set_events(crate::fanout::compose(window_sink, other));
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.enabled = false;
        config.save(&dir)?;
        Ok(self.status())
    }

    /// Offer to pair, for the next five minutes.
    pub fn invite(&self) -> CommandResult<ShareStatus> {
        let invitation = {
            let running = self.running.lock().unwrap();
            let running = running
                .as_ref()
                .ok_or_else(|| CommandError::new("not_sharing", "turn sharing on first"))?;
            let code = running.server.registry.new_pairing_code();
            everyday_server::pairing::invitation(
                &advertised_host(running),
                running.fingerprint(),
                &code,
                &running.server.name,
            )
        };
        *self.invitation.lock().unwrap() = Some(invitation);
        // `status` takes the same lock, so the borrow above has to be over.
        Ok(self.status())
    }

    /// Withdraw the offer.
    pub fn cancel_invite(&self) -> ShareStatus {
        if let Some(running) = self.running.lock().unwrap().as_ref() {
            running.server.registry.clear_pairing_code();
        }
        *self.invitation.lock().unwrap() = None;
        self.status()
    }

    pub fn revoke(&self, id: &str) -> CommandResult<ShareStatus> {
        if let Some(parts) = self.parts.lock().unwrap().as_ref() {
            parts.registry.revoke(id)?;
        } else {
            // Sharing is off, but the list is still on disk and somebody is
            // looking at it. Open it just to take the row out.
            everyday_server::Registry::open(Self::dir().join(everyday_server::DEVICES_FILE))?
                .revoke(id)?;
        }
        Ok(self.status())
    }

    /// Is a server actually listening right now?
    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
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

    pub fn status(&self) -> ShareStatus {
        let running = self.running.lock().unwrap();
        let config = Self::config();
        let devices = match self.parts.lock().unwrap().as_ref() {
            Some(parts) => parts.registry.devices(),
            None => {
                everyday_server::Registry::open(Self::dir().join(everyday_server::DEVICES_FILE))
                    .map(|r| r.devices())
                    .unwrap_or_default()
            }
        };
        ShareStatus {
            sharing: running.is_some(),
            address: running.as_ref().map(|r| r.address.to_string()),
            addresses: advertisable_addresses(),
            port: config.listen.port(),
            // From the running server when there is one, because it is live
            // there and the file is only where it is remembered.
            allow_remote_unlock: running
                .as_ref()
                .map(|r| r.server.allow_remote_unlock())
                .unwrap_or(config.allow_remote_unlock),
            devices,
            invitation: self.invitation.lock().unwrap().clone(),
            listeners: self
                .parts
                .lock()
                .unwrap()
                .as_ref()
                .map(|p| p.broadcaster.listeners())
                .unwrap_or(0),
        }
    }
}

/// The address to put in a pairing link.
///
/// The listening address when it names one, and otherwise the first address
/// this machine actually has -- because `0.0.0.0:7397` is the right thing to
/// *listen* on and a useless thing to hand somebody.
fn advertised_host(running: &Running) -> String {
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
