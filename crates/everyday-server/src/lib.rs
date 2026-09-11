//! Serve a vault to other copies of Every Day.
//!
//! The remote half of "one vault, many windows". A client here is not a thin
//! viewer -- it is the same application, with its own window, its own tray and
//! its own keyboard, whose Rust side forwards every command to whichever
//! machine holds the vault. That machine keeps the key, the search index, the
//! feeds and the assistant; a client keeps a device token and a certificate
//! fingerprint, and nothing about the data at all.
//!
//! ```text
//!   laptop A (the vault)                laptop B / a phone
//!   ┌──────────────────────┐            ┌──────────────────────┐
//!   │ window ─┐            │            │ window ─┐            │
//!   │         ├─ Service ──┼── TLS ─────┼─ RemoteClient        │
//!   │ server ─┘     │      │            │         (pins a cert)│
//!   │            the vault │            │      no key, no data │
//!   └──────────────────────┘            └──────────────────────┘
//! ```
//!
//! # Two transports, one router
//!
//! TLS over TCP for other machines, and a local socket for processes belonging
//! to the same user. The second is not an afterthought: it is how the
//! command-line tool writes through a running app instead of opening the vault
//! read-only beside it, and how a browser extension will reach a vault without
//! holding a token or trusting a certificate. Anything reachable one way is
//! reachable the other, except pairing.
//!
//! # What this is honest about
//!
//! The server sees plaintext. That is a different trust model from a hosted
//! Postgres vault, which sees only ciphertext, and the interface says so on the
//! screen where somebody turns sharing on. What the network sees is TLS with a
//! certificate the client pinned when it paired, so no authority anywhere can
//! issue one it would accept.
//!
//! There is no offline mode. A client with no route to its server shows the
//! connect screen and nothing else. Caching plaintext on the client is exactly
//! what this arrangement exists to avoid, and a client that could edit offline
//! would be doing it.

pub mod auth;
pub mod client;
pub mod mcp;
pub mod pairing;
pub mod routes;
pub mod sse;
pub mod tls;

use everyday_service::error::{CommandError, CommandResult};
use everyday_service::{EventSink, Service};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use auth::{DeviceInfo, Registry};
pub use routes::{Server, Transport};
pub use sse::{Broadcaster, Fanout};

/// The port a vault is served on by default.
///
/// Unassigned by IANA, memorable enough to type, and outside the range a
/// desktop is likely to have something else on.
pub const DEFAULT_PORT: u16 = 7397;

/// How a server is set up, and what it remembers between runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Where to listen. `0.0.0.0` means every interface, which is what somebody
    /// on a private network wants; a specific address is what somebody with
    /// several networks wants.
    pub listen: SocketAddr,
    /// Whether sharing is on. Persisted, so a machine that was sharing when it
    /// was shut down is sharing when it comes back.
    pub enabled: bool,
    /// May a paired device unlock the vault, or must that happen on the machine
    /// holding it?
    ///
    /// On by default, because a headless server has no other way to be
    /// unlocked, and because the alternative for a laptop under a desk is
    /// keeping the password in an environment file.
    pub allow_remote_unlock: bool,
    /// Terminate TLS somewhere else -- a reverse proxy with a real certificate.
    /// The client then pins nothing and requires `https` from the proxy.
    #[serde(default)]
    pub no_tls: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            enabled: false,
            allow_remote_unlock: true,
            no_tls: false,
        }
    }
}

pub const CONFIG_FILE: &str = "server.json";
pub const DEVICES_FILE: &str = "devices.json";

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
        everyday_core::fsutil::write_atomic(&dir.join(CONFIG_FILE), text.as_bytes(), "server")
            .map_err(|e| CommandError::new("io", e.to_string()))
    }
}

/// A running server, and the handle that stops it.
pub struct Running {
    pub server: Arc<Server>,
    /// Where it actually bound, which is not what was asked for when port 0 was.
    pub address: SocketAddr,
    shutdown: tokio::sync::watch::Sender<bool>,
    /// Set by the serving task on its way out, so a caller that is about to
    /// rebind the same port can wait for the socket to be released.
    stopped: tokio::sync::watch::Sender<bool>,
}

impl Running {
    /// Stop answering. Connections in flight are allowed to finish.
    ///
    /// Returns as soon as the request is *made*. To rebind the same address,
    /// use [`Running::stop_and_wait`]: signalling is not releasing, and a bind
    /// that follows a bare `stop` loses the race often enough to matter.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Stop answering, and wait until the listener is actually closed.
    pub async fn stop_and_wait(&self) {
        self.stop();
        let mut rx = self.stopped.subscribe();
        // Bounded, because a connection that will not finish must not become a
        // switch that will not move. The graceful shutdown itself is capped at
        // three seconds, so this is that plus room.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rx.wait_for(|stopped| *stopped),
        )
        .await;
    }

    pub fn fingerprint(&self) -> &str {
        &self.server.fingerprint
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Everything a server needs that is not the vault.
pub struct Parts {
    pub registry: Arc<Registry>,
    pub broadcaster: Arc<Broadcaster>,
    pub identity: Option<tls::Identity>,
}

/// Read the device list and the certificate out of `dir`.
///
/// `dir` is the application's configuration directory, *not* the vault. A
/// Postgres vault can be served from two machines and each has its own devices,
/// and `everyday backup` copies the vault -- so a private key kept there would
/// end up in every backup the user ever made.
///
/// Opens its own [`Registry`]. Right for a caller -- `everyday serve` is the
/// one today -- that runs nothing else over the same `devices.json` in this
/// process. A caller that does, such as the desktop app running sharing and
/// an MCP listener together, must use [`prepare_with`] instead: see that
/// function's doc for why.
pub fn prepare(dir: &Path, config: &Config) -> CommandResult<Parts> {
    let registry = Arc::new(Registry::open(dir.join(DEVICES_FILE))?);
    prepare_with(dir, config, registry)
}

/// As [`prepare`], but over a [`Registry`] the caller already has open.
///
/// A `Registry` keeps the whole device list in memory and writes all of it
/// back on every change -- see that type's own doc. Two processes may each
/// open `devices.json` safely, because the file itself is the arbiter; two
/// `Registry`s open in the *same* process are not, because neither one's
/// in-memory list knows the other wrote anything, and whichever saves last
/// wins, erasing the other's write. A process that starts more than one
/// thing over the same device list -- sharing and an MCP listener, in the
/// desktop app -- must therefore open the file once and hand every user of
/// it the same `Arc<Registry>`, which is what this function lets it do:
/// `prepare` opens one for you, this takes yours.
pub fn prepare_with(dir: &Path, config: &Config, registry: Arc<Registry>) -> CommandResult<Parts> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CommandError::new("io", format!("{}: {e}", dir.display())))?;
    let identity = if config.no_tls {
        None
    } else {
        Some(tls::Identity::load_or_create(dir, &tls::local_names(&config.listen))?)
    };
    Ok(Parts { registry, broadcaster: Arc::new(Broadcaster::new()), identity })
}

/// Start answering on `config.listen`.
///
/// Must be spawned on the runtime the rest of the application uses. The desktop
/// app has exactly one -- Tauri's -- and starting a second here would double the
/// blocking pool and, worse, would mean the vault's single-writer rule was being
/// kept by two sets of threads that know nothing about each other.
pub async fn start(
    service: Arc<Service>,
    parts: Parts,
    config: &Config,
    vault_name: String,
) -> CommandResult<Running> {
    let Parts { registry, broadcaster, identity } = parts;
    let fingerprint = identity.as_ref().map(|i| i.fingerprint.clone()).unwrap_or_default();

    let server = Server::new(
        service,
        registry,
        broadcaster,
        vault_name,
        fingerprint,
        config.allow_remote_unlock,
    );

    let listener = tokio::net::TcpListener::bind(config.listen).await.map_err(|e| {
        CommandError::new("io", format!("could not listen on {}: {e}", config.listen))
    })?;
    let address = listener.local_addr().map_err(|e| CommandError::new("io", e.to_string()))?;

    let (shutdown, mut rx) = tokio::sync::watch::channel(false);
    let (stopped, _) = tokio::sync::watch::channel(false);
    let router = routes::router(server.clone(), Transport::Network);

    match identity {
        Some(identity) => {
            let tls = tls_config(&identity)?;
            let acceptor = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls));
            let handle = axum_server::Handle::new();
            {
                let handle = handle.clone();
                tokio::spawn(async move {
                    let _ = rx.wait_for(|stop| *stop).await;
                    handle.graceful_shutdown(Some(std::time::Duration::from_secs(3)));
                });
            }
            let std_listener =
                listener.into_std().map_err(|e| CommandError::new("io", e.to_string()))?;
            let done = stopped.clone();
            tokio::spawn(async move {
                if let Err(e) = axum_server::from_tcp_rustls(std_listener, acceptor)
                    .handle(handle)
                    .serve(router.into_make_service())
                    .await
                {
                    tracing::warn!(error = %e, "the server stopped");
                }
                // Without this, nothing on the TLS branch ever sent on
                // `stopped`, so every `stop_and_wait` against a TLS listener
                // ran its shutdown to completion and then blocked for the
                // full five-second timeout anyway, waiting for a signal that
                // was never coming. `Sharing::start` calls `stop_and_wait`
                // before it (re)binds, so that five seconds landed on every
                // share toggle in the desktop app.
                let _ = done.send(true);
            });
        }
        None => {
            let done = stopped.clone();
            tokio::spawn(async move {
                let shutdown = async move {
                    let _ = rx.wait_for(|stop| *stop).await;
                };
                if let Err(e) = axum::serve(listener, router.into_make_service())
                    .with_graceful_shutdown(shutdown)
                    .await
                {
                    tracing::warn!(error = %e, "the server stopped");
                }
                let _ = done.send(true);
            });
        }
    }

    tracing::info!(%address, "serving the vault");
    Ok(Running { server, address, shutdown, stopped })
}

/// Serve the local socket as well, for processes belonging to this user.
///
/// No TLS and no token: the operating system already vouched for the peer, and
/// a socket in the user's own runtime directory is not reachable by anybody
/// else. This is how the command-line tool writes through a running app, and how
/// a browser-extension host will reach a vault that is not shared at all.
#[cfg(unix)]
pub async fn serve_socket(
    server: Arc<Server>,
    path: PathBuf,
) -> CommandResult<tokio::sync::watch::Sender<bool>> {
    // A socket left behind by a process that crashed would refuse to bind. It
    // is ours to remove: the path is derived from this user's runtime directory
    // and the vault, and a live server would still be holding it.
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", parent.display())))?;
    }
    let listener = tokio::net::UnixListener::bind(&path)
        .map_err(|e| CommandError::new("io", format!("could not bind {}: {e}", path.display())))?;
    restrict_socket(&path);

    let (shutdown, mut rx) = tokio::sync::watch::channel(false);
    let router = routes::router(server, Transport::Socket);
    tokio::spawn(async move {
        let shutdown = async move {
            let _ = rx.wait_for(|stop| *stop).await;
        };
        if let Err(e) =
            axum::serve(listener, router.into_make_service()).with_graceful_shutdown(shutdown).await
        {
            tracing::warn!(error = %e, "the local socket stopped");
        }
        let _ = std::fs::remove_file(&path);
    });
    Ok(shutdown)
}

#[cfg(unix)]
fn restrict_socket(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, "could not restrict the local socket's permissions");
    }
}

/// Where this user's local socket for `vault` lives.
///
/// Under the runtime directory where there is one, because that is the
/// directory the operating system already restricts to this user and clears on
/// logout. Named after the vault path's hash so two vaults open at once do not
/// collide.
pub fn socket_path(vault: &Path) -> PathBuf {
    let key = blake3::hash(vault.to_string_lossy().as_bytes()).to_hex();
    let name = format!("everyday-{}.sock", &key[..16]);
    let dir =
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(name)
}

fn tls_config(identity: &tls::Identity) -> CommandResult<rustls::ServerConfig> {
    let certs = rustls_pemfile_certs(&identity.cert_pem)?;
    let key = rustls_pemfile_key(&identity.key_pem)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| CommandError::new("internal", e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| CommandError::new("internal", format!("that certificate is unusable: {e}")))?;
    // Offer HTTP/2, so a client that wants forty covers at once gets forty
    // streams on one connection rather than forty round trips in a queue.
    // Without this, ALPN offers nothing and every client falls back to 1.1.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

fn pem_blocks(pem: &str, tag: &str) -> Vec<Vec<u8>> {
    let begin = format!("-----BEGIN {tag}");
    let end = format!("-----END {tag}");
    let mut out = Vec::new();
    let mut body = String::new();
    let mut inside = false;
    for line in pem.lines() {
        if line.starts_with(&begin) {
            inside = true;
            body.clear();
        } else if line.starts_with(&end) {
            inside = false;
            use base64::Engine;
            if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body.trim()) {
                out.push(der);
            }
        } else if inside {
            body.push_str(line.trim());
        }
    }
    out
}

fn rustls_pemfile_certs(
    pem: &str,
) -> CommandResult<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let certs: Vec<_> = pem_blocks(pem, "CERTIFICATE")
        .into_iter()
        .map(rustls::pki_types::CertificateDer::from)
        .collect();
    if certs.is_empty() {
        return Err(CommandError::new("invalid", "no certificate in that file"));
    }
    Ok(certs)
}

fn rustls_pemfile_key(pem: &str) -> CommandResult<rustls::pki_types::PrivateKeyDer<'static>> {
    // rcgen writes PKCS#8. The other two spellings are accepted anyway, because
    // somebody pointing this at a key from a reverse proxy should not have to
    // convert it first.
    for (tag, wrap) in [("PRIVATE KEY", 0u8), ("RSA PRIVATE KEY", 1), ("EC PRIVATE KEY", 2)] {
        if let Some(der) = pem_blocks(pem, tag).into_iter().next() {
            return Ok(match wrap {
                1 => rustls::pki_types::PrivateKeyDer::Pkcs1(der.into()),
                2 => rustls::pki_types::PrivateKeyDer::Sec1(der.into()),
                _ => rustls::pki_types::PrivateKeyDer::Pkcs8(der.into()),
            });
        }
    }
    Err(CommandError::new("invalid", "no private key in that file"))
}

/// Install the crypto provider this process will use.
///
/// Named explicitly rather than left to `rustls`'s process default, because
/// this binary may already carry another rustls user -- the calendar fetcher
/// does -- and two providers in one process makes "the default" ambiguous
/// enough to panic at handshake time. Calling it twice is harmless.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A sink that sends to the window and to every connected device.
pub fn fanout(local: Arc<dyn EventSink>, broadcaster: Arc<Broadcaster>) -> Arc<dyn EventSink> {
    Arc::new(Fanout::new(vec![local, broadcaster]))
}
