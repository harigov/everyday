//! The interface in a browser, served by the machine holding the vault.
//!
//! See `docs/plans/web-access.md` for the whole design. In one paragraph: a
//! listener bound only to this machine's Tailscale addresses, with a
//! certificate Tailscale issues for its tailnet name, serving the same
//! `ui/` build the desktop window runs; the vault password is the login,
//! and a session lasts until it idles out, the browser signs out, or the
//! vault locks.
//!
//! # The parts
//!
//! * [`network`] -- who may connect, and which addresses to bind.
//! * [`cert`] -- which certificate to show.
//! * [`session`] -- password to cookie and media token.
//! * [`assets`] -- the interface's files.
//! * [`routes`] -- `/web/*`, the static files, and the gatekeeper.
//!
//! [`Network`] and [`Certificate`] are tagged enums in `web.json`; a new
//! kind of either is a new variant and its match arms, and nothing in the
//! sessions, the routes or the interface has to learn about it.
//!
//! # The supervisor
//!
//! Started by [`Web::start`], straight away and then once a minute it ends
//! the sessions that have run out, asks the network where to listen,
//! refetches the certificate when it is due (every twelve hours --
//! tailscaled renews it on its own schedule, so asking that often picks a
//! renewal up long before the old one expires), and opens or closes a
//! listener per address as the addresses change. That is what makes the switch survive
//! Tailscale being slow to come up at boot, a sign-in again, or an address
//! changing: what is wrong is shown in the settings pane, and the next tick
//! tries again.

pub mod assets;
pub mod cert;
pub mod network;
pub mod routes;
pub mod session;
pub mod tailscale;

use everyday_service::Service;
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::{Change, EventSink, MeetingOffer, Notification};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub use assets::{Asset, Assets};
pub use cert::Certificate;
pub use network::{Network, TailnetPeers};

use crate::auth::Registry;
use crate::sse::Broadcaster;
use cert::{Loaded, Resolver};
use network::{Gate, Place};
use routes::WebState;
use session::Sessions;

/// The port web access answers on unless told otherwise. After sharing's
/// 7397, MCP's 7398 and the screen-time collectors' loopback 7399.
pub const DEFAULT_PORT: u16 = 7400;
pub const CONFIG_FILE: &str = "web.json";

/// How often the supervisor looks again.
const TICK: Duration = Duration::from_secs(60);
/// How often the certificate is fetched again once one is in hand.
const CERT_EVERY: Duration = Duration::from_secs(12 * 60 * 60);
/// How soon to try again after a certificate could not be fetched.
const CERT_RETRY: Duration = Duration::from_secs(5 * 60);

/// Web access's settings, in `web.json` in the application's configuration
/// directory -- never in the vault, for the reason `crate::Config` gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Whether it is on. Remembered, so a desktop app that was serving a
    /// browser when it quit serves one again when it starts.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub certificate: Certificate,
    /// May the password typed into a browser unlock a locked vault, or only
    /// prove the person while it is already open?
    #[serde(default = "yes")]
    pub allow_unlock: bool,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            network: Network::default(),
            certificate: Certificate::default(),
            allow_unlock: true,
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> CommandResult<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", dir.display())))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        everyday_core::fsutil::write_atomic(&dir.join(CONFIG_FILE), text.as_bytes(), "web")
            .map_err(|e| CommandError::new("io", e.to_string()))
    }
}

/// What the settings pane draws.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebStatus {
    /// Whether the switch is on -- not whether anything is answering yet;
    /// that is `listening`.
    pub running: bool,
    /// The addresses actually bound.
    pub listening: Vec<String>,
    /// What to open in a browser, best first.
    pub urls: Vec<String>,
    pub port: u16,
    pub network: Network,
    /// What is stopping it, when something is. Shown as it is.
    pub problem: Option<String>,
    pub certificate: Option<Loaded>,
    /// How many browsers are signed in.
    pub sessions: usize,
    pub allow_unlock: bool,
}

impl WebStatus {
    /// What the pane shows when the switch is off.
    pub fn off(config: &Config) -> Self {
        Self {
            running: false,
            listening: Vec::new(),
            urls: Vec::new(),
            port: config.port,
            network: config.network.clone(),
            problem: None,
            certificate: None,
            sessions: 0,
            allow_unlock: config.allow_unlock,
        }
    }
}

/// Fans the service's events out to every signed-in browser, and ends every
/// session when the vault locks.
pub struct WebSink {
    broadcaster: Arc<Broadcaster>,
    sessions: Arc<Sessions>,
}

impl EventSink for WebSink {
    fn notify(&self, notification: Notification) {
        self.broadcaster.notify(notification);
    }

    fn changed(&self, change: Change) {
        self.broadcaster.changed(change);
    }

    fn lock_state(&self, locked: bool) {
        // Said first, so a page whose stream is still open hears why it is
        // about to close; then every session goes, and their streams with
        // them.
        self.broadcaster.lock_state(locked);
        if locked {
            self.sessions.close_all();
        }
    }

    // Meeting offers are not passed on: a browser cannot record, and an
    // offer it could only refuse is noise.
    fn meeting_offer(&self, _offer: MeetingOffer) {}
}

/// One listener, on one address.
struct Listener {
    handle: axum_server::Handle,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Live {
    place: Option<Place>,
    /// What went wrong at each stage of the last tick, kept apart so that
    /// one stage succeeding does not clear another's complaint.
    place_problem: Option<String>,
    cert_problem: Option<String>,
    bind_problem: Option<String>,
    certificate: Option<Loaded>,
    next_certificate: Option<Instant>,
    /// Each address on its own: one that cannot be bound -- IPv6 switched
    /// off, or not assigned yet at boot -- must not keep the others from
    /// answering.
    bound: BTreeMap<SocketAddr, Listener>,
}

struct Shared {
    state: Arc<WebState>,
    resolver: Arc<Resolver>,
    live: Mutex<Live>,
    /// Turns true once the supervisor's first look is over, for a caller
    /// that wants to report how it went -- see [`Web::first_look`].
    looked: tokio::sync::watch::Sender<bool>,
}

/// Web access, switched on. Dropping it switches it off.
pub struct Web {
    shared: Arc<Shared>,
    sink: Arc<WebSink>,
    shutdown: tokio::sync::watch::Sender<bool>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Web {
    /// Switch it on, over `service`.
    ///
    /// Returns at once: the first look -- tailscaled's status, then a
    /// certificate, which the first time can be an ACME round trip of a
    /// minute or more -- happens on the supervisor, as every later one does.
    /// That keeps it off the desktop's startup path, which waits on this.
    /// A caller that wants to say how it went, such as the switch being
    /// thrown in Settings, waits for [`Web::first_look`].
    ///
    /// Must run on the runtime the rest of the process uses, for the reason
    /// `crate::start` gives.
    pub fn start(
        service: Arc<Service>,
        registry: Arc<Registry>,
        assets: Assets,
        config: Config,
        vault_name: String,
    ) -> Web {
        crate::install_crypto_provider();
        let sessions = Arc::new(Sessions::new());
        let broadcaster = Arc::new(Broadcaster::new());
        let server = crate::routes::Server::for_web(
            service,
            registry.clone(),
            broadcaster.clone(),
            vault_name,
            sessions.clone(),
        );
        let state = Arc::new(WebState {
            server,
            sessions: sessions.clone(),
            gate: Arc::new(Gate::new(config.network.clone())),
            assets,
            broadcaster: broadcaster.clone(),
            registry,
            config,
            origins: RwLock::new(Vec::new()),
        });
        let shared = Arc::new(Shared {
            state,
            resolver: Arc::new(Resolver::default()),
            live: Mutex::new(Live::default()),
            looked: tokio::sync::watch::channel(false).0,
        });

        let (shutdown, mut stop) = tokio::sync::watch::channel(false);
        let supervisor = {
            let shared = shared.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shared.tick() => {}
                        _ = stop.wait_for(|s| *s) => break,
                    }
                    shared.looked.send_replace(true);
                    let stopped = tokio::select! {
                        _ = tokio::time::sleep(TICK) => false,
                        _ = stop.wait_for(|s| *s) => true,
                    };
                    if stopped {
                        break;
                    }
                }
            })
        };
        Web {
            sink: Arc::new(WebSink { broadcaster, sessions }),
            shared,
            shutdown,
            supervisor: Mutex::new(Some(supervisor)),
        }
    }

    /// Wait for the first look to be over, or for `limit`, whichever comes
    /// first; then [`Web::status`] says how it went.
    pub async fn first_look(&self, limit: Duration) {
        let mut looked = self.shared.looked.subscribe();
        let _ = tokio::time::timeout(limit, looked.wait_for(|done| *done)).await;
    }

    /// This switch's part of the event fan-out.
    pub fn sink(&self) -> Arc<dyn EventSink> {
        self.sink.clone()
    }

    pub fn status(&self) -> WebStatus {
        let state = &self.shared.state;
        let live = self.shared.live.lock().unwrap();
        let port = state.config.port;
        let urls = match &live.place {
            Some(place) if !live.bound.is_empty() => {
                place.names.iter().map(|n| url_for(n, port)).collect()
            }
            _ => Vec::new(),
        };
        WebStatus {
            running: true,
            listening: live.bound.keys().map(|a| a.to_string()).collect(),
            urls,
            port,
            network: state.config.network.clone(),
            problem: live
                .place_problem
                .clone()
                .or_else(|| live.cert_problem.clone())
                .or_else(|| live.bind_problem.clone()),
            certificate: live.certificate.clone(),
            sessions: state.sessions.count(),
            allow_unlock: state.config.allow_unlock,
        }
    }

    /// Stop answering, sign every browser out, and wait for the port to be
    /// released -- for a caller about to bind it again.
    pub async fn stop_and_wait(&self) {
        let _ = self.shutdown.send(true);
        let supervisor = self.supervisor.lock().unwrap().take();
        if let Some(task) = supervisor {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
        self.shared.state.sessions.close_all();
        let bound = std::mem::take(&mut self.shared.live.lock().unwrap().bound);
        unbind(bound.into_values()).await;
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.shared.state.sessions.close_all();
        for listener in self.shared.live.lock().unwrap().bound.values() {
            listener.handle.graceful_shutdown(Some(Duration::from_secs(3)));
        }
    }
}

fn url_for(name: &str, port: u16) -> String {
    if port == 443 { format!("https://{name}/") } else { format!("https://{name}:{port}/") }
}

impl Shared {
    /// One look: sessions that have run out, where to listen, whether the
    /// certificate is due, and which listeners to open or close.
    async fn tick(&self) {
        let state = &self.state;
        state.sessions.sweep();
        let place = match state.gate.place().await {
            Ok(place) => place,
            Err(e) => {
                self.live.lock().unwrap().place_problem = Some(e.message);
                return;
            }
        };
        let (names_changed, due) = {
            let mut live = self.live.lock().unwrap();
            live.place_problem = None;
            (
                live.place.as_ref().is_none_or(|p| p.names != place.names),
                live.next_certificate.is_none_or(|at| Instant::now() >= at),
            )
        };

        if names_changed || due {
            let loaded = cert::load(&state.config.certificate, &state.gate, &place).await;
            let mut live = self.live.lock().unwrap();
            match loaded {
                Ok((key, loaded)) => {
                    self.resolver.set(key);
                    live.certificate = Some(loaded);
                    live.cert_problem = None;
                    live.next_certificate = Some(Instant::now() + CERT_EVERY);
                }
                Err(e) => {
                    live.cert_problem = Some(e.message);
                    live.next_certificate = Some(Instant::now() + CERT_RETRY);
                }
            }
        }
        // Without a certificate there is nothing to answer with. With an
        // older one, keep answering with it while the new one is late.
        if !self.resolver.is_ready() {
            return;
        }

        *state.origins.write().unwrap() = place
            .names
            .iter()
            .map(|n| url_for(n, state.config.port).trim_end_matches('/').to_string())
            .collect();

        let wanted: Vec<SocketAddr> =
            place.addresses.iter().map(|ip| SocketAddr::new(*ip, state.config.port)).collect();
        // Close what is no longer wanted...
        let gone: Vec<Listener> = {
            let mut live = self.live.lock().unwrap();
            let stale: Vec<SocketAddr> =
                live.bound.keys().filter(|a| !wanted.contains(a)).copied().collect();
            stale.iter().filter_map(|a| live.bound.remove(a)).collect()
        };
        unbind(gone.into_iter()).await;
        // ...and open, one at a time, what is wanted and not open yet. One
        // that fails is tried again on the next tick; the rest answer
        // meanwhile.
        let missing: Vec<SocketAddr> = {
            let live = self.live.lock().unwrap();
            wanted.iter().filter(|a| !live.bound.contains_key(a)).copied().collect()
        };
        let mut failures = Vec::new();
        if !missing.is_empty() {
            match cert::server_config(self.resolver.clone()) {
                Ok(config) => {
                    let tls = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(config));
                    let app = routes::app(self.state.clone());
                    for address in missing {
                        match listen(address, tls.clone(), app.clone()).await {
                            Ok(listener) => {
                                tracing::info!(%address, "web access is answering");
                                self.live.lock().unwrap().bound.insert(address, listener);
                            }
                            Err(e) => failures.push(e.message),
                        }
                    }
                }
                Err(e) => failures.push(e.message),
            }
        }
        let mut live = self.live.lock().unwrap();
        live.bind_problem = (!failures.is_empty()).then(|| failures.join("; "));
        live.place = Some(place);
    }
}

/// Open one listener on `address`.
async fn listen(
    address: SocketAddr,
    tls: axum_server::tls_rustls::RustlsConfig,
    app: axum::Router,
) -> CommandResult<Listener> {
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|e| CommandError::new("io", format!("could not listen on {address}: {e}")))?;
    let std_listener = listener.into_std().map_err(|e| CommandError::new("io", e.to_string()))?;
    let handle = axum_server::Handle::new();
    let serve = axum_server::from_tcp_rustls(std_listener, tls)
        .handle(handle.clone())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>());
    let task = tokio::spawn(async move {
        if let Err(e) = serve.await {
            tracing::warn!(error = %e, "a web listener stopped");
        }
    });
    Ok(Listener { handle, task })
}

async fn unbind(listeners: impl Iterator<Item = Listener>) {
    let listeners: Vec<Listener> = listeners.collect();
    for listener in &listeners {
        listener.handle.graceful_shutdown(Some(Duration::from_secs(3)));
    }
    for listener in listeners {
        let _ = tokio::time::timeout(Duration::from_secs(5), listener.task).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_default_and_an_old_one_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()), Config::default());
        std::fs::write(dir.path().join(CONFIG_FILE), r#"{"enabled": true}"#).unwrap();
        let config = Config::load(dir.path());
        assert!(config.enabled);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.allow_unlock);
        assert_eq!(config.network, Network::default());
    }

    #[test]
    fn the_config_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            enabled: true,
            port: 8443,
            network: Network::Tailscale { peers: TailnetPeers::Logins(vec!["a@b.c".into()]) },
            certificate: Certificate::Tailscale,
            allow_unlock: false,
        };
        config.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()), config);
    }

    #[test]
    fn a_url_leaves_out_the_default_https_port() {
        assert_eq!(url_for("box.tail.ts.net", 443), "https://box.tail.ts.net/");
        assert_eq!(url_for("box.tail.ts.net", 7400), "https://box.tail.ts.net:7400/");
    }
}
