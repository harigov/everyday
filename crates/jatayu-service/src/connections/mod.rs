//! The runtime behind connections: one live MCP client per connection, kept
//! warm, with tokens refreshed and what each server offers written back to
//! the vault. See `docs/plans/connections.md`.
//!
//! [`tools`] (the assistant's two tools) and [`crate::domains::connections`]
//! (Settings) are the only callers.
//!
//! Process-wide, like the browser's sessions: a tool the assistant calls has
//! the vault but not the [`crate::service::Service`], and a live connection
//! is the process's, not one command's. [`Hub::close_all`] runs when the
//! vault locks or closes, so no client outlives the key its token came from.
//!
//! # What keeps a client honest
//!
//! A live client remembers a fingerprint of the settings and the secret it
//! was opened with, and is replaced the moment either changes -- whether or
//! not a caller remembered to [`Hub::forget`] it. The fingerprint leaves out
//! an OAuth grant's tokens and expiry: those change on every refresh, and a
//! refreshed token is handed to the client with `set_bearer` rather than by
//! opening a second connection.
//!
//! A stdio server is a child process, so idle ones are stopped after ten
//! minutes and idle HTTP clients after thirty. That is done lazily on every
//! access and by a small task that ticks while anything is open, so a
//! program nobody is using does not stay running until the vault locks.

pub mod signin;
pub mod tools;

use std::collections::HashMap;
use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use jatayu_connect::oauth::{self, ClientCreds, OAuthError};
use jatayu_connect::{Client, ClientOptions, ConnectError, Endpoint, ToolResult};
use jatayu_core::connection::{
    AuthMethod, Connection, ConnectionSecret, ConnectionState, RemoteTool, ServerInfo, TokenAuth,
    Transport,
};
use jatayu_core::{ConnectionId, Vault};
use jiff::Timestamp;
use serde_json::Value;

use crate::error::codes;

/// A stdio server nobody has used for this long is stopped.
const IDLE_STDIO: Duration = Duration::from_secs(10 * 60);
/// An HTTP client nobody has used for this long is dropped.
const IDLE_HTTP: Duration = Duration::from_secs(30 * 60);
/// How often the reaper looks while anything is open.
const REAP_EVERY: Duration = Duration::from_secs(60);
/// An access token due to run out within this many seconds is refreshed
/// before it is used.
const REFRESH_SKEW_SECS: i64 = 60;
/// After a failed listing, how long [`Hub::usable`] leaves a connection alone
/// rather than waiting out its connect timeout on every turn.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// The longest an error quotes of what a program wrote to stderr.
const DETAIL_CHARS: usize = 300;

/// Why the hub could not do what it was asked, in words for a person (and a
/// model), with nothing secret in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubError {
    pub message: String,
    /// The server wants a sign-in, or a new token: the panel's "Sign in"
    /// button, not "try again".
    pub needs_sign_in: bool,
    /// One of [`crate::error::codes`], when the failure has a name a client
    /// can route on: a sign-in that was cancelled, a client ID that is
    /// needed. Most failures have none.
    pub code: Option<&'static str>,
}

impl HubError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), needs_sign_in: false, code: None }
    }

    /// A failure with a code a client can route on.
    pub fn coded(code: &'static str, message: impl Into<String>) -> Self {
        Self { message: message.into(), needs_sign_in: false, code: Some(code) }
    }

    /// A failure only a sign-in (or a new token) can mend.
    pub fn sign_in(message: impl Into<String>) -> Self {
        Self { message: message.into(), needs_sign_in: true, code: None }
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HubError {}

/// Every connection a caller may use, with what is known of each.
#[derive(Debug, Clone, Default)]
pub struct Usable {
    /// Switched on (and, for an unattended caller, allowed unattended), with
    /// a tool list no older than `TOOLS_FRESH_SECS` where it could be had.
    pub connections: Vec<(Connection, ConnectionState)>,
    /// Those whose tools could not be listed: name, and why.
    pub unavailable: Vec<(String, String)>,
}

/// One open client, and what it was opened with.
struct Live {
    client: Arc<Client>,
    last_used: Instant,
    fingerprint: u64,
    stdio: bool,
}

/// The live clients, and the sign-ins in flight.
pub struct Hub {
    sign_ins: signin::ConnectionSignIns,
    live: Mutex<HashMap<ConnectionId, Live>>,
    /// One gate per connection, held while a client is opened, so two
    /// callers arriving together do not start two programs.
    gates: Mutex<HashMap<ConnectionId, Arc<tokio::sync::Mutex<()>>>>,
    /// When each connection last failed to list, for [`RETRY_AFTER`].
    failed: Mutex<HashMap<ConnectionId, Instant>>,
    /// Whether the reaper task is running.
    reaper: AtomicBool,
    /// Bumped by [`Hub::close_all`]. A connect that began before a lock and
    /// finishes after it sees a different number, and throws its client
    /// away rather than keep a token read before the lock.
    generation: AtomicU64,
}

static HUB: LazyLock<Hub> = LazyLock::new(Hub::new);

/// The process's hub.
pub fn hub() -> &'static Hub {
    &HUB
}

/// What OAuth talks to the internet with: discovery, registration, the token
/// endpoint. Each address it fetches was vetted before it was asked for, and
/// a redirect is a second one the far end picks after the check, so a
/// redirect is held to the same rule on the way: a few hops, `https` only,
/// and never to a literal private address -- the one hop the resolver below
/// never sees. Servers that move their metadata to a canonical address with
/// a `301` are common enough that refusing every redirect broke sign-in. A
/// request that takes more than half a minute is not going to finish.
///
/// Two clients, and which is used depends on the address the person typed for
/// the server, since everything else comes from the server's own answers.
/// For a server on the internet they come from [`Reach::PublicOnly`]: names
/// resolve only to public addresses, so a metadata document that sends the
/// next request to `localhost`, a router or a cloud's metadata address
/// finds nothing there. For a server the person typed as an address on this
/// computer or a private network (a program of their own, in a container) they
/// come from [`Reach::Anywhere`], because that server's own authorization
/// server is as likely to be there too.
#[derive(Clone)]
pub(crate) struct OAuthHttp {
    client: reqwest::Client,
    reach: Reach,
}

/// Where an OAuth request may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    PublicOnly,
    Anywhere,
}

impl OAuthHttp {
    /// For a connection whose server is at `server_url`, as typed.
    pub(crate) fn for_server(server_url: &str) -> Self {
        static PUBLIC: LazyLock<reqwest::Client> =
            LazyLock::new(|| oauth_client(Reach::PublicOnly));
        static ANYWHERE: LazyLock<reqwest::Client> =
            LazyLock::new(|| oauth_client(Reach::Anywhere));
        let reach =
            if names_a_private_place(server_url) { Reach::Anywhere } else { Reach::PublicOnly };
        let client = match reach {
            Reach::PublicOnly => PUBLIC.clone(),
            Reach::Anywhere => ANYWHERE.clone(),
        };
        Self { client, reach }
    }

    /// For `connection`, wherever its server is.
    pub(crate) fn for_connection(connection: &Connection) -> Self {
        match &connection.transport {
            Transport::Http { url, .. } => Self::for_server(url),
            Transport::Stdio { .. } => Self::for_server(""),
        }
    }

    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// The check [`oauth::discover_gated`] holds each address a server names
    /// to, before it is fetched.
    pub(crate) fn gate(&self) -> oauth::UrlGate<'static> {
        match self.reach {
            Reach::PublicOnly => &public_address_only,
            Reach::Anywhere => &oauth::any_address,
        }
    }
}

fn oauth_client(reach: Reach) -> reqwest::Client {
    let builder = reqwest::Client::builder().timeout(Duration::from_secs(30)).user_agent("Jatayu");
    let builder = match reach {
        // No proxy: one resolves names itself, and would be a way round the
        // resolver below.
        Reach::PublicOnly => builder
            .no_proxy()
            .dns_resolver(Arc::new(crate::http::PublicOnly))
            .redirect(crate::http::public_redirect_policy(
                OAUTH_REDIRECTS,
                crate::http::RedirectSchemes::HttpsOnly,
            )),
        Reach::Anywhere => builder.redirect(reqwest::redirect::Policy::limited(OAUTH_REDIRECTS)),
    };
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// How many redirects one OAuth request may follow.
const OAUTH_REDIRECTS: usize = 5;

/// Whether a typed server address is on this computer or a private network,
/// by what it says and not by what it resolves to: a name somebody else
/// controls can resolve to anything, and the person's own `localhost` and
/// numeric addresses cannot be made to.
fn names_a_private_place(server_url: &str) -> bool {
    reqwest::Url::parse(server_url.trim()).ok().is_some_and(|url| is_private_target(&url))
}

/// Whether `url`'s host is `localhost` (or under it) or a numeric address on
/// this computer or a private network.
fn is_private_target(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else { return false };
    let host = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    crate::http::literal_ip(host).is_some_and(|ip| crate::http::is_forbidden_address(ip, false))
}

/// The gate for a server on the internet: nothing a server names may be on
/// this computer or a private network. A numeric address is refused here; a
/// name is refused when it resolves, by [`crate::http::PublicOnly`].
fn public_address_only(url: &reqwest::Url) -> Result<(), String> {
    if is_private_target(url) {
        Err("points at this computer or a private network".to_string())
    } else {
        Ok(())
    }
}

/// Run vault work off the async runtime, answering a [`HubError`]. Goes
/// through [`crate::service::blocking`] so what the work touched is
/// announced like any command's.
pub(crate) async fn vault_task<T, F>(f: F) -> Result<T, HubError>
where
    F: FnOnce() -> jatayu_core::Result<T> + Send + 'static,
    T: Send + 'static,
{
    crate::service::blocking(move || Ok(f()?)).await.map_err(|e| HubError::new(e.message))
}

/// A connection and its secret, read together.
pub(crate) async fn read_connection(
    vault: &Arc<Vault>,
    id: ConnectionId,
) -> Result<(Connection, ConnectionSecret), HubError> {
    let vault = Arc::clone(vault);
    vault_task(move || Ok((vault.connection(id)?, vault.connection_secret(id)?))).await
}

pub(crate) fn core_token_auth(auth: oauth::TokenAuth) -> TokenAuth {
    match auth {
        oauth::TokenAuth::None => TokenAuth::None,
        oauth::TokenAuth::ClientSecretPost => TokenAuth::ClientSecretPost,
        oauth::TokenAuth::ClientSecretBasic => TokenAuth::ClientSecretBasic,
    }
}

fn connect_token_auth(auth: TokenAuth) -> oauth::TokenAuth {
    match auth {
        TokenAuth::None => oauth::TokenAuth::None,
        TokenAuth::ClientSecretPost => oauth::TokenAuth::ClientSecretPost,
        TokenAuth::ClientSecretBasic => oauth::TokenAuth::ClientSecretBasic,
    }
}

/// End a client without making the caller wait for it: a program is given
/// two seconds to leave. Outside a runtime there is nothing to run the close
/// on, and dropping the client kills a program anyway.
fn close_later(client: Arc<Client>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move { client.close().await });
        }
        Err(_) => drop(client),
    }
}

impl Hub {
    fn new() -> Self {
        Self {
            sign_ins: signin::ConnectionSignIns::default(),
            live: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashMap::new()),
            reaper: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    /// The OAuth sign-ins this process is driving.
    pub fn sign_ins(&self) -> &signin::ConnectionSignIns {
        &self.sign_ins
    }

    /// The connections `unattended` (or attended) callers may use, listing
    /// the tools again -- concurrently, each bounded by the connect timeout
    /// -- for any whose cached list is missing, stale, or reported changed.
    /// A connection that cannot be listed keeps its old list, if it has one,
    /// and is also named in `unavailable`.
    pub async fn usable(&self, vault: &Arc<Vault>, unattended: bool) -> Usable {
        self.reap();
        let reading = Arc::clone(vault);
        let Ok((settings, states)) =
            vault_task(move || Ok((reading.connection_settings()?, reading.connection_states()?)))
                .await
        else {
            return Usable::default();
        };
        let candidates: Vec<Connection> =
            settings.connections.into_iter().filter(|c| allowed(c, unattended)).collect();

        let now = Timestamp::now();
        let mut stale = Vec::new();
        let mut unavailable = Vec::new();
        for connection in &candidates {
            let state = states.of(connection.id);
            if state.tools_fresh(now) && !self.tools_changed(connection.id) {
                continue;
            }
            // A server that just failed is not tried again on every turn:
            // each try could cost its whole connect timeout.
            if self.recently_failed(connection.id) {
                let why = state.last_error.clone().unwrap_or_else(|| {
                    format!("{} could not be reached a moment ago", connection.name)
                });
                unavailable.push((connection.name.clone(), why));
                continue;
            }
            stale.push(connection.id);
        }

        let results =
            futures::future::join_all(stale.iter().map(|id| self.refresh(vault, *id))).await;
        for (id, result) in stale.iter().zip(results) {
            if let Err(e) = result
                && let Some(c) = candidates.iter().find(|c| c.id == *id)
            {
                unavailable.push((c.name.clone(), e.message));
            }
        }

        // Read again: a refresh just rewrote the lists.
        let reading = Arc::clone(vault);
        let states = vault_task(move || reading.connection_states()).await.unwrap_or(states);
        let connections = candidates
            .into_iter()
            .map(|c| {
                let state = states.of(c.id);
                (c, state)
            })
            .collect();
        Usable { connections, unavailable }
    }

    /// Connect (or reuse the live client), list the tools afresh, and record
    /// what was found -- or why it failed -- as the connection's state.
    pub async fn refresh(
        &self,
        vault: &Arc<Vault>,
        id: ConnectionId,
    ) -> Result<ConnectionState, HubError> {
        let listed = self
            .run(vault, id, Retry::Reconnect, |client: Arc<Client>| async move {
                let tools = client.list_tools().await?;
                Ok::<_, ConnectError>((client.server().clone(), tools))
            })
            .await;
        match listed {
            Ok((server, tools)) => {
                let now = Timestamp::now();
                let state = ConnectionState {
                    server: Some(ServerInfo {
                        name: server.name,
                        version: server.version,
                        protocol_version: server.protocol_version,
                        instructions: server.instructions,
                    }),
                    tools: tools.into_iter().map(remote_tool).collect(),
                    tools_at: Some(now),
                    last_ok: Some(now),
                    last_error: None,
                    needs_sign_in: false,
                };
                let writing = Arc::clone(vault);
                let saved = state.clone();
                vault_task(move || writing.save_connection_state(id, saved)).await?;
                self.failed.lock().unwrap().remove(&id);
                Ok(state)
            }
            Err(e) => {
                // A connection switched off was not tried, so nothing is
                // learned about it.
                if e.code != Some(codes::FORBIDDEN) {
                    self.record_failure(vault, id, &e).await;
                }
                Err(e)
            }
        }
    }

    /// Call `tool` on connection `id`. Records a sign-in failure in the state.
    ///
    /// Never sent twice: a connection that drops mid-call, or a server that
    /// answers 502, may already have done what was asked -- created the
    /// issue, posted the message -- and the person agreed to one. Only a
    /// refused token (which did nothing) is mended and tried again; an
    /// expired session is renewed inside the client, before the call is
    /// sent.
    pub async fn call(
        &self,
        vault: &Arc<Vault>,
        id: ConnectionId,
        tool: &str,
        arguments: Value,
    ) -> Result<ToolResult, HubError> {
        let result = self
            .run(vault, id, Retry::OnlyRefusals, |client: Arc<Client>| {
                let tool = tool.to_string();
                let arguments = arguments.clone();
                async move { client.call_tool(&tool, arguments).await }
            })
            .await;
        if let Err(e) = &result
            && e.needs_sign_in
        {
            self.record_failure(vault, id, e).await;
        }
        result
    }

    /// Drop the live client for `id`, if any -- after its settings or
    /// secrets changed, or it was removed -- so the next use connects anew.
    pub fn forget(&self, id: ConnectionId) {
        self.failed.lock().unwrap().remove(&id);
        if let Some(old) = self.live.lock().unwrap().remove(&id) {
            close_later(old.client);
        }
    }

    /// Whether a live client is open for `id`.
    pub fn is_live(&self, id: ConnectionId) -> bool {
        self.reap();
        self.live.lock().unwrap().get(&id).is_some_and(|l| l.client.is_alive())
    }

    /// Drop every live client and cancel every sign-in: the vault locked or
    /// closed.
    pub fn close_all(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let all: Vec<Live> = self.live.lock().unwrap().drain().map(|(_, l)| l).collect();
        for live in all {
            close_later(live.client);
        }
        self.failed.lock().unwrap().clear();
        self.sign_ins.clear();
    }

    // ---- getting a client -----------------------------------------------

    /// Run `op` on connection `id`'s client. A refusal of the token is
    /// mended once by refreshing it, for a connection that has a refresh
    /// token to refresh it with; with [`Retry::Reconnect`], a dropped
    /// connection is also mended once by connecting afresh.
    async fn run<T, F, Fut>(
        &self,
        vault: &Arc<Vault>,
        id: ConnectionId,
        retry: Retry,
        op: F,
    ) -> Result<T, HubError>
    where
        F: Fn(Arc<Client>) -> Fut,
        Fut: Future<Output = Result<T, ConnectError>>,
    {
        let mut reconnected = false;
        let mut refreshed = false;
        loop {
            let (connection, client) = self.client_for(vault, id, refreshed).await?;
            self.touch(id);
            let failure = match op(client).await {
                Ok(value) => {
                    self.touch(id);
                    return Ok(value);
                }
                Err(e) => e,
            };
            match failure {
                ConnectError::Unauthorized { .. }
                    if !refreshed && matches!(connection.auth, AuthMethod::OAuth { .. }) =>
                {
                    refreshed = true;
                    self.forget(id);
                }
                e if retry == Retry::Reconnect && !reconnected && e.is_retryable() => {
                    reconnected = true;
                    self.forget(id);
                }
                e => return Err(friendly(&connection, &e, false)),
            }
        }
    }

    /// The live client for `id`, opened if there is none (or the one there
    /// was opened with something that has changed). `refresh_token` forces
    /// an OAuth token refresh whether or not it is due.
    async fn client_for(
        &self,
        vault: &Arc<Vault>,
        id: ConnectionId,
        refresh_token: bool,
    ) -> Result<(Connection, Arc<Client>), HubError> {
        self.reap();
        let gate = self.gate(id);
        let _one_at_a_time = gate.lock().await;

        let (connection, mut secret) = read_connection(vault, id).await?;
        if !connection.enabled {
            return Err(HubError::coded(
                codes::FORBIDDEN,
                format!("{} is switched off in Settings \u{2192} Connections", connection.name),
            ));
        }

        let mut force = refresh_token;
        loop {
            self.freshen(vault, &connection, &mut secret, force).await?;
            let fingerprint = fingerprint(&connection, &secret);
            if let Some(client) = self.reusable(id, fingerprint) {
                return Ok((connection, client));
            }
            // Whatever was open was opened with something else, or has died.
            if let Some(old) = self.live.lock().unwrap().remove(&id) {
                close_later(old.client);
            }

            let endpoint = endpoint_for(&connection, &secret)?;
            let stdio = matches!(connection.transport, Transport::Stdio { .. });
            let options = ClientOptions {
                connect_timeout: Duration::from_secs(if stdio { 90 } else { 30 }),
                call_timeout: Duration::from_secs(120),
                ..ClientOptions::default()
            };
            let generation = self.generation.load(Ordering::SeqCst);
            match Client::connect(endpoint, options).await {
                Ok(client) => {
                    let client = Arc::new(client);
                    let mut live = self.live.lock().unwrap();
                    if self.generation.load(Ordering::SeqCst) != generation {
                        drop(live);
                        close_later(client);
                        return Err(HubError::new("the vault locked while connecting"));
                    }
                    live.insert(
                        id,
                        Live {
                            client: Arc::clone(&client),
                            last_used: Instant::now(),
                            fingerprint,
                            stdio,
                        },
                    );
                    drop(live);
                    self.ensure_reaper();
                    return Ok((connection, client));
                }
                // The token may simply have run out without saying when.
                Err(ConnectError::Unauthorized { .. }) if !force && refreshable(&secret) => {
                    force = true;
                }
                Err(e) => return Err(friendly(&connection, &e, true)),
            }
        }
    }

    /// Refresh the OAuth access token if it runs out within a minute (or
    /// `force`), saving the new one -- and a rotated refresh token, if the
    /// server sent one -- and handing it to the live client.
    async fn freshen(
        &self,
        vault: &Arc<Vault>,
        connection: &Connection,
        secret: &mut ConnectionSecret,
        force: bool,
    ) -> Result<(), HubError> {
        if !matches!(connection.auth, AuthMethod::OAuth { .. }) || !refreshable(secret) {
            return Ok(());
        }
        let Some(grant) = secret.grant.clone() else { return Ok(()) };
        let Some(refresh_token) = grant.refresh_token.clone() else { return Ok(()) };
        let due = force
            || grant.expires_at.is_some_and(|at| {
                at.as_second() - Timestamp::now().as_second() <= REFRESH_SKEW_SECS
            });
        if !due {
            return Ok(());
        }

        let creds = ClientCreds {
            client_id: grant.client_id.clone(),
            client_secret: grant.client_secret.clone(),
            auth: connect_token_auth(grant.token_auth),
        };
        let renewed = match oauth::refresh(
            OAuthHttp::for_connection(connection).client(),
            &grant.token_endpoint,
            &creds,
            &refresh_token,
            grant.resource.as_deref(),
        )
        .await
        {
            Ok(renewed) => renewed,
            Err(OAuthError::InvalidGrant) => {
                let e = HubError::sign_in(format!("Sign in to {} again", connection.name));
                self.record_failure(vault, connection.id, &e).await;
                return Err(e);
            }
            Err(e) => {
                return Err(HubError::new(format!(
                    "could not renew {}'s sign-in: {e}",
                    connection.name
                )));
            }
        };

        let mut next = grant;
        next.access_token = renewed.access_token;
        next.expires_at = renewed.expires_at;
        if let Some(rotated) = renewed.refresh_token {
            next.refresh_token = Some(rotated);
        }
        if renewed.scope.is_some() {
            next.scope = renewed.scope;
        }
        secret.grant = Some(next.clone());

        // Only the grant is written, onto whatever the secret holds now, so a
        // token pasted in while this was in flight is not undone.
        let writing = Arc::clone(vault);
        let id = connection.id;
        let saved = next.clone();
        vault_task(move || {
            let mut stored = writing.connection_secret(id)?;
            stored.grant = Some(saved);
            writing.set_connection_secret(id, &stored)
        })
        .await?;

        if let Some(live) = self.live.lock().unwrap().get(&id) {
            live.client.set_bearer(Some(next.access_token));
        }
        Ok(())
    }

    /// The live client for `id`, if it is open, alive, and was opened with
    /// exactly what is current.
    fn reusable(&self, id: ConnectionId, fingerprint: u64) -> Option<Arc<Client>> {
        let mut live = self.live.lock().unwrap();
        match live.get_mut(&id) {
            Some(l) if l.fingerprint == fingerprint && l.client.is_alive() => {
                l.last_used = Instant::now();
                Some(Arc::clone(&l.client))
            }
            _ => None,
        }
    }

    fn gate(&self, id: ConnectionId) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.gates.lock().unwrap().entry(id).or_default())
    }

    fn touch(&self, id: ConnectionId) {
        if let Some(l) = self.live.lock().unwrap().get_mut(&id) {
            l.last_used = Instant::now();
        }
    }

    fn tools_changed(&self, id: ConnectionId) -> bool {
        self.live.lock().unwrap().get(&id).is_some_and(|l| l.client.tools_changed())
    }

    fn recently_failed(&self, id: ConnectionId) -> bool {
        self.failed.lock().unwrap().get(&id).is_some_and(|at| at.elapsed() < RETRY_AFTER)
    }

    /// Write why an attempt failed into the connection's state, leaving what
    /// it learned before (its tools, when it last worked) alone.
    async fn record_failure(&self, vault: &Arc<Vault>, id: ConnectionId, error: &HubError) {
        self.failed.lock().unwrap().insert(id, Instant::now());
        let writing = Arc::clone(vault);
        let message = error.message.clone();
        let sign_in = error.needs_sign_in;
        // The state is a note about the attempt; failing to write it must
        // not hide the failure itself.
        let _ = vault_task(move || {
            writing.update_connection_state(id, |state| {
                state.last_error = Some(message);
                if sign_in {
                    state.needs_sign_in = true;
                }
            })
        })
        .await;
    }

    // ---- idle clients ---------------------------------------------------

    /// Close what has sat idle too long, and what has died.
    fn reap(&self) {
        let now = Instant::now();
        let expired: Vec<Arc<Client>> = {
            let mut live = self.live.lock().unwrap();
            let ids: Vec<ConnectionId> = live
                .iter()
                .filter(|(_, l)| {
                    let limit = if l.stdio { IDLE_STDIO } else { IDLE_HTTP };
                    now.duration_since(l.last_used) > limit || !l.client.is_alive()
                })
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter().filter_map(|id| live.remove(&id)).map(|l| l.client).collect()
        };
        for client in expired {
            close_later(client);
        }
    }

    /// Start the task that reaps while anything is open, if it is not
    /// running. It ends when the map is empty; the next client opened starts
    /// it again.
    fn ensure_reaper(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if self.reaper.swap(true, Ordering::SeqCst) {
            return;
        }
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_EVERY).await;
                let hub = hub();
                hub.reap();
                // Checked and cleared under the map's lock, so a client
                // opened just now either is seen here or starts a new task.
                let empty = {
                    let live = hub.live.lock().unwrap();
                    let empty = live.is_empty();
                    if empty {
                        hub.reaper.store(false, Ordering::SeqCst);
                    }
                    empty
                };
                if empty {
                    return;
                }
            }
        });
    }
}

/// Whether the grant has a refresh token to renew itself with.
fn refreshable(secret: &ConnectionSecret) -> bool {
    secret.grant.as_ref().is_some_and(|g| g.refresh_token.as_ref().is_some_and(|t| !t.is_empty()))
}

/// What a live client was opened with, as one number: the transport, the
/// sign-in method and every secret value, but not the OAuth tokens, which
/// change on each refresh and reach a live client by other means.
fn fingerprint(connection: &Connection, secret: &ConnectionSecret) -> u64 {
    let mut stable = secret.clone();
    if let Some(grant) = stable.grant.as_mut() {
        grant.access_token.clear();
        grant.refresh_token = None;
        grant.expires_at = None;
        grant.scope = None;
    }
    let bytes =
        serde_json::to_vec(&(&connection.transport, &connection.auth, &stable)).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// What went wrong, in words for a person: the connection is named, and
/// nothing that was sent is quoted.
fn friendly(connection: &Connection, error: &ConnectError, connecting: bool) -> HubError {
    let name = &connection.name;
    match error {
        ConnectError::Unauthorized { .. } => {
            let how = match connection.auth {
                AuthMethod::Bearer => "check its token",
                AuthMethod::OAuth { .. } => "sign in again",
                AuthMethod::None => "check its headers",
            };
            HubError::sign_in(format!(
                "{name} refused the credentials; {how} in Settings \u{2192} Connections"
            ))
        }
        ConnectError::Spawn(message) => {
            HubError::new(format!("{name} could not be started: {}", clip(message)))
        }
        ConnectError::Transport(message) => HubError::new(format!("{name}: {}", clip(message))),
        ConnectError::Protocol(message) => HubError::new(format!(
            "{name} answered in a way this app does not understand: {}",
            clip(message)
        )),
        ConnectError::Rpc { message, .. } => {
            HubError::new(format!("{name} refused: {}", clip(message)))
        }
        ConnectError::Timeout if connecting => {
            HubError::new(format!("{name} did not answer when connecting"))
        }
        ConnectError::Timeout => HubError::new(format!("{name} did not answer within two minutes")),
        ConnectError::Closed { detail } => {
            match detail.as_deref().map(clip).filter(|d| !d.is_empty()) {
                Some(detail) => HubError::new(format!("{name} closed the connection: {detail}")),
                None => HubError::new(format!("{name} closed the connection")),
            }
        }
    }
}

/// `text` trimmed, and cut at [`DETAIL_CHARS`].
fn clip(text: &str) -> String {
    clip_to(text, DETAIL_CHARS)
}

/// `text` trimmed, and cut at `max` characters with an ellipsis.
pub(crate) fn clip_to(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('\u{2026}');
    cut
}

/// Whether a turn that is (or is not) `unattended` may use `connection`:
/// switched on, and for a turn with nobody watching, one the person has said
/// routines and jobs may use. The one rule, for what is offered, listed and
/// allowed alike.
pub(crate) fn allowed(connection: &Connection, unattended: bool) -> bool {
    connection.enabled && (!unattended || connection.unattended)
}

/// Whether [`Hub::run`] may connect afresh and try again after a dropped
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Listing tools: asking twice changes nothing.
    Reconnect,
    /// Calling one: a drop may come after the server acted.
    OnlyRefusals,
}

/// The [`Endpoint`] for `connection`, with `secret`'s values filled in: the
/// headers or environment it names (a name with no value is an error saying
/// which), and a bearer token from [`ConnectionSecret::bearer`] or the OAuth
/// grant's access token.
pub fn endpoint_for(
    connection: &Connection,
    secret: &ConnectionSecret,
) -> Result<Endpoint, HubError> {
    let name = &connection.name;
    match &connection.transport {
        Transport::Http { url, headers } => {
            let headers = headers
                .iter()
                .map(|h| value_of(&secret.headers, h).map(|v| (h.clone(), v)))
                .collect::<Result<Vec<_>, _>>()?;
            let bearer = match &connection.auth {
                AuthMethod::None => None,
                AuthMethod::Bearer => {
                    Some(secret.bearer.clone().filter(|t| !t.is_empty()).ok_or_else(|| {
                        HubError::sign_in(format!(
                            "{name} has no token; add one in Settings \u{2192} Connections"
                        ))
                    })?)
                }
                AuthMethod::OAuth { .. } => Some(
                    secret
                        .grant
                        .as_ref()
                        .map(|g| g.access_token.clone())
                        .filter(|t| !t.is_empty())
                        .ok_or_else(|| HubError::sign_in(format!("Sign in to {name} first")))?,
                ),
            };
            Ok(Endpoint::Http { url: url.clone(), headers, bearer })
        }
        Transport::Stdio { command, args, env, cwd } => {
            let env = env
                .iter()
                .map(|e| value_of(&secret.env, e).map(|v| (e.clone(), v)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Endpoint::Stdio {
                command: command.clone(),
                args: args.clone(),
                env,
                cwd: cwd.as_deref().map(expand_home),
            })
        }
    }
}

/// The value kept for `name`, which must be there and not empty.
fn value_of(
    values: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<String, HubError> {
    values.get(name).filter(|v| !v.is_empty()).cloned().ok_or_else(|| {
        HubError::new(format!("{name} has no value; set it in Settings \u{2192} Connections"))
    })
}

/// A directory as typed, with a leading `~` meaning the home directory.
fn expand_home(path: &str) -> PathBuf {
    let rest = if path == "~" { Some("") } else { path.strip_prefix("~/") };
    match (rest, std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// A listed tool in the vault's shape.
pub fn remote_tool(tool: jatayu_connect::Tool) -> RemoteTool {
    RemoteTool {
        name: tool.name,
        title: tool.title,
        description: tool.description,
        input_schema: tool.input_schema,
        hints: jatayu_core::connection::ToolHints {
            read_only: tool.annotations.read_only,
            destructive: tool.annotations.destructive,
            idempotent: tool.annotations.idempotent,
            open_world: tool.annotations.open_world,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::connection::OAuthGrant;

    fn http(headers: &[&str], auth: AuthMethod) -> Connection {
        Connection::new(
            "Work",
            Transport::Http {
                url: "https://example.com/mcp".into(),
                headers: headers.iter().map(|h| h.to_string()).collect(),
            },
            auth,
        )
    }

    fn grant(token: &str) -> OAuthGrant {
        OAuthGrant {
            client_id: "client".into(),
            client_secret: None,
            token_endpoint: "https://example.com/token".into(),
            token_auth: TokenAuth::None,
            resource: None,
            access_token: token.into(),
            refresh_token: Some("refresh".into()),
            expires_at: None,
            scope: None,
        }
    }

    #[test]
    fn http_endpoint_carries_named_headers_and_bearer() {
        let connection = http(&["X-Key"], AuthMethod::Bearer);
        let mut secret = ConnectionSecret::default();
        secret.headers.insert("X-Key".into(), "k".into());
        secret.bearer = Some("tok".into());
        match endpoint_for(&connection, &secret).unwrap() {
            Endpoint::Http { url, headers, bearer } => {
                assert_eq!(url, "https://example.com/mcp");
                assert_eq!(headers, vec![("X-Key".to_string(), "k".to_string())]);
                assert_eq!(bearer.as_deref(), Some("tok"));
            }
            other => panic!("expected http, got {other:?}"),
        }
    }

    #[test]
    fn a_header_without_a_value_names_itself_in_the_error() {
        let connection = http(&["X-Key"], AuthMethod::None);
        let e = endpoint_for(&connection, &ConnectionSecret::default()).unwrap_err();
        assert!(e.message.starts_with("X-Key has no value"), "{}", e.message);
        assert!(!e.needs_sign_in);
    }

    #[test]
    fn bearer_and_oauth_without_a_token_need_a_sign_in() {
        let bearer = http(&[], AuthMethod::Bearer);
        assert!(endpoint_for(&bearer, &ConnectionSecret::default()).unwrap_err().needs_sign_in);
        let oauth =
            http(&[], AuthMethod::OAuth { client_id: None, scopes: vec![], redirect_port: None });
        assert!(endpoint_for(&oauth, &ConnectionSecret::default()).unwrap_err().needs_sign_in);
        let secret = ConnectionSecret { grant: Some(grant("access")), ..Default::default() };
        match endpoint_for(&oauth, &secret).unwrap() {
            Endpoint::Http { bearer, .. } => assert_eq!(bearer.as_deref(), Some("access")),
            other => panic!("expected http, got {other:?}"),
        }
    }

    #[test]
    fn stdio_endpoint_takes_its_environment_and_a_home_relative_directory() {
        let connection = Connection::new(
            "Local",
            Transport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into()],
                env: vec!["API_KEY".into()],
                cwd: Some("~/work".into()),
            },
            AuthMethod::None,
        );
        let mut secret = ConnectionSecret::default();
        secret.env.insert("API_KEY".into(), "v".into());
        match endpoint_for(&connection, &secret).unwrap() {
            Endpoint::Stdio { command, args, env, cwd } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y".to_string()]);
                assert_eq!(env, vec![("API_KEY".to_string(), "v".to_string())]);
                if let Some(home) = std::env::var_os("HOME") {
                    assert_eq!(cwd, Some(PathBuf::from(home).join("work")));
                }
            }
            other => panic!("expected stdio, got {other:?}"),
        }
    }

    #[test]
    fn the_fingerprint_follows_settings_and_secrets_but_not_refreshed_tokens() {
        let connection =
            http(&[], AuthMethod::OAuth { client_id: None, scopes: vec![], redirect_port: None });
        let secret = ConnectionSecret { grant: Some(grant("one")), ..Default::default() };
        let before = fingerprint(&connection, &secret);

        let mut refreshed = secret.clone();
        let g = refreshed.grant.as_mut().unwrap();
        g.access_token = "two".into();
        g.refresh_token = Some("rotated".into());
        g.expires_at = Some(Timestamp::now());
        assert_eq!(before, fingerprint(&connection, &refreshed));

        let mut other_client = secret.clone();
        other_client.grant.as_mut().unwrap().client_id = "someone-else".into();
        assert_ne!(before, fingerprint(&connection, &other_client));

        let mut with_header = secret.clone();
        with_header.headers.insert("X".into(), "y".into());
        assert_ne!(before, fingerprint(&connection, &with_header));
    }

    #[test]
    fn friendly_errors_name_the_connection_and_clip_stderr() {
        let connection = http(&[], AuthMethod::None);
        let long = "x".repeat(1000);
        let e = friendly(&connection, &ConnectError::Closed { detail: Some(long) }, false);
        assert!(e.message.starts_with("Work closed the connection: "));
        assert!(e.message.chars().count() < 400);
        let e = friendly(&connection, &ConnectError::Timeout, false);
        assert_eq!(e.message, "Work did not answer within two minutes");
        let e = friendly(
            &connection,
            &ConnectError::Rpc { code: -1, message: "nope".into(), data: None },
            false,
        );
        assert_eq!(e.message, "Work refused: nope");
        assert!(
            friendly(
                &connection,
                &ConnectError::Unauthorized { resource_metadata: None, scope: None },
                false
            )
            .needs_sign_in
        );
    }

    #[test]
    fn a_server_typed_as_a_private_place_may_name_private_places_and_no_other_may() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        for typed in [
            "http://localhost:8080/mcp",
            "http://app.localhost/mcp",
            "http://127.0.0.1:3000/mcp",
            "http://[::1]:3000/mcp",
            "http://192.168.1.20/mcp",
            "http://10.0.0.5/mcp",
            "http://2130706433/mcp",
        ] {
            assert!(names_a_private_place(typed), "{typed}");
            assert_eq!(OAuthHttp::for_server(typed).reach, Reach::Anywhere, "{typed}");
        }
        for typed in ["https://mcp.linear.app/mcp", "https://8.8.8.8/mcp", "", "nonsense"] {
            assert!(!names_a_private_place(typed), "{typed}");
            assert_eq!(OAuthHttp::for_server(typed).reach, Reach::PublicOnly, "{typed}");
        }

        // What a public server's metadata may point at.
        let gate = OAuthHttp::for_server("https://mcp.linear.app/mcp").gate();
        for refused in [
            "http://127.0.0.1:6379/token",
            "https://localhost/token",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/token",
            "https://[::ffff:10.0.0.1]/token",
            "https://192.168.0.1/token",
            "https://100.64.0.1/token",
            "https://metadata.localhost./token",
        ] {
            assert!(gate(&url(refused)).is_err(), "{refused}");
        }
        for allowed in ["https://auth.linear.app/token", "https://1.1.1.1/token"] {
            assert!(gate(&url(allowed)).is_ok(), "{allowed}");
        }
        // A private server's own may be anywhere.
        let gate = OAuthHttp::for_server("http://127.0.0.1:3000/mcp").gate();
        assert!(gate(&url("http://127.0.0.1:3001/token")).is_ok());
        assert!(gate(&url("https://login.example/token")).is_ok());
    }
}
