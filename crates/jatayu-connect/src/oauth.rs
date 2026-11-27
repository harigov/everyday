//! The MCP authorization flow, as plain functions over a `reqwest::Client`.
//!
//! See `docs/plans/connections.md`'s "Sign-in" for the flow this follows. It is
//! deliberately not the `oauth2` crate (which `jatayu_mail::oauth` uses):
//! MCP servers answer in ways a strict client rejects -- Slack nests the user's
//! token under `authed_user` and reports errors with `ok: false` at HTTP 200,
//! GitHub answers a code exchange in form encoding unless asked for JSON --
//! and discovery (RFC 9728, RFC 8414, OIDC) is not something that crate does.
//!
//! The order a sign-in calls these in:
//!
//! 1. [`discover`] from the server's address (and the `resource_metadata` a
//!    401 named, if any): the protected resource and its authorization
//!    server.
//! 2. [`register`] when the connection has no client of its own and the
//!    server offers registration. [`OAuthError::NoRegistration`] otherwise.
//! 3. [`Pkce::new`], [`random_state`], [`authorize_url`]: the address to open.
//! 4. The browser comes back to a loopback with a code (not this module).
//! 5. [`exchange`], then [`refresh`] whenever the token is about to expire.
//!
//! Nothing here logs a token, a secret, a code or a verifier.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::{SignedDuration, Timestamp};
use rand::RngCore;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

/// How long one metadata or registration request may take. Discovery makes
/// several in a row, most of which are expected to be a 404, so a server that
/// simply does not answer must not hold the sign-in for long.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one token request may take; the same as the mail sign-in's.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(20);

/// The most of any one answer this module will read: metadata, a
/// registration, a token. Real ones are a few kilobytes. Every address here
/// was named by a server, and a server that answers with an endless body
/// should cost a failed sign-in, not this process's memory.
const MAX_BODY: usize = 1024 * 1024;

/// Whether an address a server named may be fetched at all, beyond being
/// `https` (or plain `http` to this computer), which is always required.
/// The service decides: it knows whether the person typed an address on this
/// computer or on the internet, and a server on the internet must not be able
/// to send this computer's requests to a printer, a router or a cloud
/// metadata address. `Err` says why not, briefly.
pub type UrlGate<'a> = &'a (dyn Fn(&Url) -> Result<(), String> + Send + Sync);

/// A gate that lets everything through, for a caller that has already
/// decided (and for tests against a server on this computer).
pub fn any_address(_: &Url) -> Result<(), String> {
    Ok(())
}

/// RFC 9728 protected resource metadata, as far as a client needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedResource {
    /// The canonical address the token is for -- sent as `resource`.
    pub resource: String,
    pub authorization_servers: Vec<String>,
    pub scopes_supported: Vec<String>,
}

/// RFC 8414 (or OIDC) authorization server metadata, as far as a client
/// needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthServer {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// `None` when the server does not register clients itself.
    pub registration_endpoint: Option<String>,
    pub code_challenge_methods: Vec<String>,
    pub token_endpoint_auth_methods: Vec<String>,
    pub scopes_supported: Vec<String>,
}

/// What [`discover`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub resource: ProtectedResource,
    pub server: AuthServer,
}

/// How a client proves itself at the token endpoint. Mirrors
/// `jatayu_core::connection::TokenAuth`; the service converts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TokenAuth {
    #[default]
    None,
    ClientSecretPost,
    ClientSecretBasic,
}

/// A client: registered, or the person's own.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientCreds {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth: TokenAuth,
}

impl std::fmt::Debug for ClientCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCreds")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "[redacted]"))
            .field("auth", &self.auth)
            .finish()
    }
}

/// What a token endpoint answered.
#[derive(Clone, PartialEq, Eq)]
pub struct Grant {
    pub access_token: String,
    /// `Some` only when this answer carried one.
    pub refresh_token: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub scope: Option<String>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "[redacted]"))
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish()
    }
}

/// A PKCE pair (S256).
#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    /// The verifier is the secret half; it must not reach a log by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"[redacted]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// A fresh 43-character verifier and its S256 challenge.
    pub fn new() -> Self {
        let verifier = random_token(32);
        let challenge = challenge_for(&verifier);
        Pkce { verifier, challenge }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// The S256 challenge for a verifier: the unpadded base64url of its SHA-256
/// ([RFC 7636 §4.2](https://www.rfc-editor.org/rfc/rfc7636#section-4.2)).
/// Public so the RFC's own worked example can be checked against it.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `n` random bytes from the operating system's generator, as unpadded
/// base64url: 32 bytes make exactly the 43 characters PKCE asks for.
fn random_token(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Why a step of the flow failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    /// The server's metadata could not be found or read.
    #[error("could not find how this server signs in: {0}")]
    Discovery(String),
    /// The server does not register clients itself, and none was given.
    #[error("this service needs a client ID of your own")]
    NoRegistration,
    /// The code or refresh token is no longer good (`invalid_grant`).
    #[error("the sign-in has expired or was revoked; sign in again")]
    InvalidGrant,
    /// The client was refused (`invalid_client`, `unauthorized_client`).
    #[error("the service refused this client: {0}")]
    InvalidClient(String),
    /// Any other error the server named.
    #[error("the service refused: {description} ({code})")]
    Provider { code: String, description: String },
    /// The request itself failed.
    #[error("{0}")]
    Http(String),
}

/// What went wrong with a request, in words a person can read. `reqwest`'s
/// own text names the address, which for a server whose path is a secret
/// (some hosted MCP endpoints are) would put it in front of the user and
/// into any log that keeps the error, so it is taken off.
fn describe(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "the server did not answer in time".to_string()
    } else if error.is_connect() {
        "could not connect to the server".to_string()
    } else {
        error.without_url().to_string()
    }
}

/// Read a response body, refusing to grow past `max` bytes. Both the length
/// the server claims and what actually arrives are checked: the first is a
/// claim, the second a fact.
async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err("the answer was too large".to_string());
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(describe)? {
        if body.len() + chunk.len() > max {
            return Err("the answer was too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Whether an address may carry a code, a verifier, a secret or a token: `https`,
/// or plain `http` to this computer. The rule a connection's own address is
/// held to.
fn is_safe_scheme(url: &Url) -> bool {
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => url.host_str().is_some_and(is_loopback),
        _ => false,
    }
}

/// Refuse `raw` before anything is sent to it: not an address, not `https`
/// (or this computer), or one the service's gate will not have. The one
/// place every request this module makes passes first.
fn guard(raw: &str, gate: UrlGate<'_>) -> Result<(), String> {
    let url = Url::parse(raw.trim()).map_err(|_| "not a web address".to_string())?;
    if !is_safe_scheme(&url) {
        return Err("not a secure address".to_string());
    }
    gate(&url)
}

/// GET `url` and read the body as JSON. `Err` says why not, briefly.
async fn fetch_json(http: &reqwest::Client, url: &str, gate: UrlGate<'_>) -> Result<Value, String> {
    guard(url, gate)?;
    let response = http
        .get(url)
        .header(ACCEPT, "application/json")
        .timeout(DISCOVERY_TIMEOUT)
        .send()
        .await
        .map_err(describe)?;
    let status = response.status();
    if status != StatusCode::OK {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let body = read_capped(response, MAX_BODY).await?;
    serde_json::from_slice::<Value>(&body).map_err(|_| "the answer was not JSON".to_string())
}

/// Whether two addresses are the same place: scheme, host, port and path
/// (a trailing slash does not count) and query. Falls back to comparing the
/// text when either is not an address.
fn same_address(a: &str, b: &str) -> bool {
    match (Url::parse(a.trim()), Url::parse(b.trim())) {
        (Ok(a), Ok(b)) => {
            a.scheme() == b.scheme()
                && a.host_str() == b.host_str()
                && a.port_or_known_default() == b.port_or_known_default()
                && a.path().trim_end_matches('/') == b.path().trim_end_matches('/')
                && a.query() == b.query()
        }
        _ => a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/'),
    }
}

/// Whether protected resource metadata that calls its `resource` `claimed`
/// is about the server at `server`
/// ([RFC 9728 §3.3](https://www.rfc-editor.org/rfc/rfc9728#section-3.3)): the
/// same scheme, host and port, and a path that is the server's own or one of
/// its parents (Slack's is `https://mcp.slack.com` for `.../mcp`). Without
/// this, a server could say its token should be asked for as somebody
/// else's resource, and the token it was given would be good there.
fn resource_covers(server: &Url, claimed: &str) -> bool {
    let Ok(claimed) = Url::parse(claimed.trim()) else { return false };
    if claimed.scheme() != server.scheme()
        || claimed.host_str() != server.host_str()
        || claimed.port_or_known_default() != server.port_or_known_default()
    {
        return false;
    }
    let parent = claimed.path().trim_end_matches('/');
    let own = server.path().trim_end_matches('/');
    own == parent || own.starts_with(&format!("{parent}/"))
}

/// A non-empty string field.
fn text_of<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// A list of strings, empty when the field is missing or of another shape.
fn list_of(map: &Map<String, Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// The server's address in the form a token is bound to
/// ([RFC 8707 §2](https://www.rfc-editor.org/rfc/rfc8707#section-2)):
/// lowercase scheme and host (which `Url` does), no fragment, no credentials,
/// and no trailing slash, so `https://mcp.slack.com/` is `https://mcp.slack.com`.
fn canonical_resource(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let text = url.as_str();
    // A trailing slash on a query string belongs to the query.
    if url.query().is_none() { text.trim_end_matches('/').to_string() } else { text.to_string() }
}

/// Read RFC 9728 metadata; `None` when the two fields a client needs are
/// not there.
fn read_resource(value: &Value) -> Option<ProtectedResource> {
    let map = value.as_object()?;
    let resource = text_of(map, "resource")?.to_string();
    let authorization_servers = list_of(map, "authorization_servers");
    if authorization_servers.is_empty() {
        return None;
    }
    Some(ProtectedResource {
        resource,
        authorization_servers,
        scopes_supported: list_of(map, "scopes_supported"),
    })
}

/// Read RFC 8414 or OIDC metadata; `None` when it names no authorization or
/// token endpoint. `issuer` is what to call the server if it does not say.
fn read_server(value: &Value, issuer: &str) -> Option<AuthServer> {
    let map = value.as_object()?;
    Some(AuthServer {
        issuer: text_of(map, "issuer").unwrap_or(issuer).to_string(),
        authorization_endpoint: text_of(map, "authorization_endpoint")?.to_string(),
        token_endpoint: text_of(map, "token_endpoint")?.to_string(),
        registration_endpoint: text_of(map, "registration_endpoint").map(str::to_string),
        code_challenge_methods: list_of(map, "code_challenge_methods_supported"),
        token_endpoint_auth_methods: list_of(map, "token_endpoint_auth_methods_supported"),
        scopes_supported: list_of(map, "scopes_supported"),
    })
}

/// Find the protected resource and its first authorization server for the
/// MCP server at `server_url`. `resource_metadata` is the address a 401's
/// `WWW-Authenticate` named; without one, try
/// `/.well-known/oauth-protected-resource` with the server's path inserted,
/// then at the root. A server with no resource metadata at all is treated as
/// its own authorization server at its origin (the 2025-03-26 fallback).
/// The authorization server's metadata is read from RFC 8414 with the path
/// inserted, then at the root, then OIDC `openid-configuration` likewise.
///
/// When the 401 named an address and it does not answer, the well-known
/// places are still tried: a stale `resource_metadata` should not be worse
/// than none. When metadata is found, its own `resource` is used as written
/// (GitHub's ends in a slash, Slack's does not), because that is the string
/// the server compares a token's audience against.
pub async fn discover(
    http: &reqwest::Client,
    server_url: &str,
    resource_metadata: Option<&str>,
) -> Result<Discovery, OAuthError> {
    discover_gated(http, server_url, resource_metadata, &any_address).await
}

/// [`discover`], with every address the server's own answers name -- the
/// `resource_metadata` of its 401, its authorization server, that server's
/// endpoints -- held to `gate` as well as to `https`, and checked *before* it
/// is fetched. The address the person typed is not gated: they chose it.
///
/// What the metadata says is checked against what was asked. Its `resource`
/// must be the server's own address (or a parent of it), and an
/// authorization server's `issuer` must be the address it was fetched for,
/// so that a document served from one place cannot speak for another.
pub async fn discover_gated(
    http: &reqwest::Client,
    server_url: &str,
    resource_metadata: Option<&str>,
    gate: UrlGate<'_>,
) -> Result<Discovery, OAuthError> {
    let server = Url::parse(server_url.trim())
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .ok_or_else(|| {
            OAuthError::Discovery("the server's address is not a web address".to_string())
        })?;
    let origin = server.origin().ascii_serialization();

    let mut candidates: Vec<String> = Vec::new();
    if let Some(named) = resource_metadata.map(str::trim).filter(|s| !s.is_empty()) {
        candidates.push(named.to_string());
    }
    let path = server.path().trim_end_matches('/');
    if !path.is_empty() {
        candidates.push(format!("{origin}/.well-known/oauth-protected-resource{path}"));
        if server.path() != path {
            candidates
                .push(format!("{origin}/.well-known/oauth-protected-resource{}", server.path()));
        }
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));

    // The addresses tried are not written into any error: for some hosted
    // servers the path is itself the credential. One the gate refuses is
    // simply not tried, like one that does not answer: the 401 that named it
    // came from the server, and the well-known places below are on the
    // address the person typed.
    let mut resource = None;
    for candidate in &candidates {
        if let Ok(value) = fetch_json(http, candidate, gate).await
            && let Some(found) = read_resource(&value)
        {
            resource = Some(found);
            break;
        }
    }
    let resource = match resource {
        Some(found) => {
            if !resource_covers(&server, &found.resource) {
                return Err(OAuthError::Discovery(
                    "the server's sign-in details are for a different address than the one \
                     it was reached at, so they were not trusted"
                        .to_string(),
                ));
            }
            found
        }
        None => ProtectedResource {
            resource: canonical_resource(&server),
            authorization_servers: vec![origin],
            scopes_supported: Vec::new(),
        },
    };

    let issuer = resource.authorization_servers[0].clone();
    let server = discover_server(http, &issuer, gate).await?;
    check_server_gated(&server, gate)?;
    Ok(Discovery { resource, server })
}

/// Refuse an authorization server that would carry a code, a verifier or a
/// refresh token in the clear, or that cannot do S256 PKCE. Plain HTTP is
/// allowed only to this computer -- the same rule a connection's own address
/// is held to. A server that lists no challenge methods at all is taken at
/// its word that it follows the specification, which requires S256.
#[cfg(test)]
fn check_server(server: &AuthServer) -> Result<(), OAuthError> {
    check_server_gated(server, &any_address)
}

/// [`check_server`], and every endpoint it names passes `gate` too.
fn check_server_gated(server: &AuthServer, gate: UrlGate<'_>) -> Result<(), OAuthError> {
    let endpoints = [
        Some(server.authorization_endpoint.as_str()),
        Some(server.token_endpoint.as_str()),
        server.registration_endpoint.as_deref(),
    ];
    for endpoint in endpoints.into_iter().flatten() {
        let url = Url::parse(endpoint).ok().filter(is_safe_scheme);
        let Some(url) = url else {
            return Err(OAuthError::Discovery(
                "the service signs in over an unencrypted connection, which would send its \
                 tokens in the clear"
                    .to_string(),
            ));
        };
        gate(&url).map_err(|why| {
            OAuthError::Discovery(format!("the service signs in at an address that {why}"))
        })?;
    }
    if !server.code_challenge_methods.is_empty()
        && !server.code_challenge_methods.iter().any(|m| m == "S256")
    {
        return Err(OAuthError::Discovery(
            "the service does not support PKCE with S256, which signing in here requires"
                .to_string(),
        ));
    }
    Ok(())
}

/// `localhost`, `127.x.x.x` or `::1`.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Read an authorization server's metadata from the places RFC 8414 and OIDC
/// put it, for the issuer `issuer`.
///
/// Each address is checked by [`fetch_json`] before it is fetched, and a
/// document whose own `issuer` is not `issuer` is not taken for it.
async fn discover_server(
    http: &reqwest::Client,
    issuer: &str,
    gate: UrlGate<'_>,
) -> Result<AuthServer, OAuthError> {
    let parsed = Url::parse(issuer)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .ok_or_else(|| {
            OAuthError::Discovery(format!(
                "the authorization server \"{issuer}\" is not a web address"
            ))
        })?;
    let origin = parsed.origin().ascii_serialization();
    let path = parsed.path().trim_end_matches('/');

    // RFC 8414 §3.1 inserts the well-known segment between host and path;
    // OIDC Discovery appends it, but servers in the wild do either.
    let mut candidates: Vec<String> = Vec::new();
    if !path.is_empty() {
        candidates.push(format!("{origin}/.well-known/oauth-authorization-server{path}"));
    }
    candidates.push(format!("{origin}/.well-known/oauth-authorization-server"));
    if !path.is_empty() {
        candidates.push(format!("{origin}/.well-known/openid-configuration{path}"));
    }
    candidates.push(format!("{origin}{path}/.well-known/openid-configuration"));

    let mut reasons: Vec<String> = Vec::new();
    for candidate in &candidates {
        match fetch_json(http, candidate, gate).await {
            Ok(value) => match read_server(&value, issuer) {
                Some(found) if same_address(issuer, &found.issuer) => return Ok(found),
                Some(_) => reasons.push(format!("{candidate}: it names a different issuer")),
                None => reasons.push(format!("{candidate}: no authorization or token endpoint")),
            },
            Err(reason) => reasons.push(format!("{candidate}: {reason}")),
        }
    }
    Err(OAuthError::Discovery(format!(
        "{issuer} has no authorization server metadata (tried {})",
        reasons.join("; ")
    )))
}

/// Register a public client (RFC 7591) with `redirect_uri`, preferring
/// `token_endpoint_auth_method: "none"` when the server supports it.
/// [`OAuthError::NoRegistration`] when the server has no registration
/// endpoint.
pub async fn register(
    http: &reqwest::Client,
    server: &AuthServer,
    redirect_uri: &str,
    client_name: &str,
) -> Result<ClientCreds, OAuthError> {
    let Some(endpoint) = server.registration_endpoint.as_deref() else {
        return Err(OAuthError::NoRegistration);
    };
    guard(endpoint, &any_address)
        .map_err(|why| OAuthError::Discovery(format!("the registration address is {why}")))?;
    // A public client is the point of registering on the spot -- there is
    // nowhere safe in a desktop app to keep a secret -- so `none` is asked
    // for whenever the server offers it or does not say what it offers (RFC
    // 8414's default for a missing list is a secret, but servers that omit it
    // are in practice the ones that take no secret). One that lists only
    // secrets gets the first it names.
    let methods = &server.token_endpoint_auth_methods;
    let method = if methods.is_empty() || methods.iter().any(|m| m == "none") {
        "none"
    } else {
        methods
            .iter()
            .map(String::as_str)
            .find(|m| matches!(*m, "client_secret_post" | "client_secret_basic"))
            .unwrap_or("client_secret_basic")
    };
    let body = serde_json::json!({
        "client_name": client_name,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": method,
    });
    let response = http
        .post(endpoint)
        .header(ACCEPT, "application/json")
        .json(&body)
        .timeout(DISCOVERY_TIMEOUT)
        .send()
        .await
        .map_err(|e| OAuthError::Http(describe(e)))?;
    let status = response.status();
    let text = read_text(response).await?;
    let value = serde_json::from_str::<Value>(&text).ok();
    let map = value.as_ref().and_then(Value::as_object);

    if let Some(map) = map
        && let Some(code) = text_of(map, "error")
    {
        return Err(OAuthError::Provider {
            code: code.to_string(),
            description: clip(text_of(map, "error_description").unwrap_or("no reason given")),
        });
    }
    if !status.is_success() {
        return Err(OAuthError::Http(format!(
            "the server refused to register this app (HTTP {})",
            status.as_u16()
        )));
    }
    let Some(map) = map else {
        return Err(OAuthError::Http("the server's registration answer was not JSON".to_string()));
    };
    let Some(client_id) = text_of(map, "client_id") else {
        return Err(OAuthError::Provider {
            code: "invalid_registration".to_string(),
            description: "the server registered the app but gave no client id".to_string(),
        });
    };
    let client_secret = text_of(map, "client_secret").map(str::to_string);
    // RFC 7591 §3.2.1: an answer with no method means `client_secret_basic`
    // when a secret was issued (the RFC 7591 default), a public client otherwise.
    let auth = match text_of(map, "token_endpoint_auth_method") {
        Some("none") => TokenAuth::None,
        Some("client_secret_post") => TokenAuth::ClientSecretPost,
        Some("client_secret_basic") => TokenAuth::ClientSecretBasic,
        _ if client_secret.is_some() => TokenAuth::ClientSecretBasic,
        _ => TokenAuth::None,
    };
    Ok(ClientCreds { client_id: client_id.to_string(), client_secret, auth })
}

/// A random, URL-safe `state`.
pub fn random_state() -> String {
    random_token(24)
}

/// The address to open: `response_type=code`, the client, the redirect,
/// `scope` (space-joined, omitted when empty), `state`, the S256 challenge,
/// and `resource` when given. Google's endpoint also gets
/// `access_type=offline&prompt=consent`.
///
/// Google hands out a refresh token only to a request that asks for offline
/// access and shows the consent screen, and an access token that lasts an
/// hour is no use to an assistant that runs in the background.
pub fn authorize_url(
    server: &AuthServer,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    pkce: &Pkce,
    resource: Option<&str>,
) -> String {
    let Ok(mut url) = Url::parse(&server.authorization_endpoint) else {
        return server.authorization_endpoint.clone();
    };
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect_uri);
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
        query.append_pair("state", state);
        query.append_pair("code_challenge", &pkce.challenge);
        query.append_pair("code_challenge_method", "S256");
        if let Some(resource) = resource {
            query.append_pair("resource", resource);
        }
    }
    if url.host_str() == Some("accounts.google.com") {
        url.query_pairs_mut()
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
    }
    url.into()
}

/// Exchange an authorization code. Reads a JSON answer, a form-encoded one
/// (GitHub without `Accept: application/json`), and Slack's, which nests
/// the user token under `authed_user` and says `ok: false` with an `error`.
#[allow(clippy::too_many_arguments)]
pub async fn exchange(
    http: &reqwest::Client,
    token_endpoint: &str,
    client: &ClientCreds,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    resource: Option<&str>,
) -> Result<Grant, OAuthError> {
    let fields = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    token_request(http, token_endpoint, client, &fields, resource).await
}

/// Refresh an access token.
pub async fn refresh(
    http: &reqwest::Client,
    token_endpoint: &str,
    client: &ClientCreds,
    refresh_token: &str,
    resource: Option<&str>,
) -> Result<Grant, OAuthError> {
    let fields = [("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
    token_request(http, token_endpoint, client, &fields, resource).await
}

/// `application/x-www-form-urlencoded` encoding of one value, which RFC 6749
/// §2.3.1 also requires of a client id and secret before they go into HTTP
/// Basic.
fn form_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// One POST to the token endpoint, and the grant (or the error) it answered.
/// The body is built by hand because `reqwest`'s `form` is an optional
/// feature this crate does not turn on.
async fn token_request(
    http: &reqwest::Client,
    token_endpoint: &str,
    client: &ClientCreds,
    fields: &[(&str, &str)],
    resource: Option<&str>,
) -> Result<Grant, OAuthError> {
    // Built in a block of its own: the serializer holds a `dyn Fn` that is
    // not `Sync`, and one still in scope at the `await` below would make this
    // whole future `!Send` -- unusable from a spawned task.
    // A secret, a code and a verifier go to this address; it must be one they
    // may be sent to even if it did not come through discovery (a grant
    // stored by an older version).
    guard(token_endpoint, &any_address)
        .map_err(|why| OAuthError::Discovery(format!("the token address is {why}")))?;
    let (body, basic) = {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in fields {
            body.append_pair(key, value);
        }
        if let Some(resource) = resource {
            body.append_pair("resource", resource);
        }
        // How the client proves itself. A "secret" method with no secret to send
        // can only be a public client, so it is treated as one.
        let basic = match (client.auth, client.client_secret.as_deref()) {
            (TokenAuth::ClientSecretBasic, Some(secret)) => Some(secret),
            (TokenAuth::ClientSecretPost, Some(secret)) => {
                body.append_pair("client_id", &client.client_id);
                body.append_pair("client_secret", secret);
                None
            }
            _ => {
                body.append_pair("client_id", &client.client_id);
                None
            }
        };
        (body.finish(), basic)
    };
    let mut request = http
        .post(token_endpoint)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .timeout(TOKEN_TIMEOUT)
        .body(body);
    if let Some(secret) = basic {
        request = request.basic_auth(form_encode(&client.client_id), Some(form_encode(secret)));
    }
    let response = request.send().await.map_err(|e| OAuthError::Http(describe(e)))?;
    let status = response.status();
    let text = read_text(response).await?;
    read_grant(status, &text)
}

/// A response body as text, read no further than [`MAX_BODY`].
async fn read_text(response: reqwest::Response) -> Result<String, OAuthError> {
    let bytes = read_capped(response, MAX_BODY).await.map_err(OAuthError::Http)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// At most 300 characters of text a server wrote, so one that answers with a
/// page of HTML cannot fill the screen.
fn clip(text: &str) -> String {
    text.chars().take(300).collect()
}

/// Turn what a server's error code means into the error the caller acts on.
/// The list covers RFC 6749's codes and the ones Slack and GitHub use for
/// the same things.
fn provider_error(code: &str, description: &str) -> OAuthError {
    match code {
        "invalid_grant"
        | "invalid_code"
        | "invalid_refresh_token"
        | "token_revoked"
        | "token_expired"
        | "bad_verification_code" => OAuthError::InvalidGrant,
        "invalid_client" | "unauthorized_client" | "invalid_client_id" | "bad_client_secret" => {
            OAuthError::InvalidClient(if description.is_empty() {
                code.to_string()
            } else {
                clip(description)
            })
        }
        _ => OAuthError::Provider {
            code: code.to_string(),
            description: if description.is_empty() {
                "no reason given".to_string()
            } else {
                clip(description)
            },
        },
    }
}

/// A count of seconds as the number or numeric string servers send for
/// `expires_in`. Zero and below are "no expiry given": some providers use 0
/// for a token that does not lapse.
fn seconds_of(value: &Value) -> Option<i64> {
    let seconds = match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
        }
        _ => None,
    };
    seconds.filter(|n| *n > 0)
}

/// Read a token endpoint's answer: JSON, or failing that form encoding.
fn read_grant(status: StatusCode, text: &str) -> Result<Grant, OAuthError> {
    let map: Map<String, Value> = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map,
        _ if text.contains('=') => url::form_urlencoded::parse(text.trim().as_bytes())
            .map(|(k, v)| (k.into_owned(), Value::String(v.into_owned())))
            .collect(),
        _ => Map::new(),
    };

    // Any `error` is an error, whatever the status: GitHub says 200, Slack
    // says 200 with `ok: false`, and RFC 6749 says 400 or 401.
    if let Some(code) = text_of(&map, "error") {
        return Err(provider_error(code, text_of(&map, "error_description").unwrap_or("")));
    }
    if map.get("ok") == Some(&Value::Bool(false)) {
        return Err(provider_error("unknown_error", ""));
    }

    // Slack's user token, with its own refresh token and lifetime, is nested;
    // the top level of that answer is the bot's. Everything is read from the
    // one object the token came from, so two grants are never mixed.
    let nested = map
        .get("authed_user")
        .and_then(Value::as_object)
        .filter(|o| text_of(o, "access_token").is_some());
    let source = nested.unwrap_or(&map);

    let Some(access_token) = text_of(source, "access_token") else {
        let (code, description) = if status.is_success() {
            ("no_token".to_string(), "the service answered without an access token".to_string())
        } else {
            (format!("http_{}", status.as_u16()), "the service refused the request".to_string())
        };
        return Err(OAuthError::Provider { code, description });
    };
    let expires_at = source
        .get("expires_in")
        .and_then(seconds_of)
        .and_then(|n| Timestamp::now().checked_add(SignedDuration::from_secs(n)).ok());
    Ok(Grant {
        access_token: access_token.to_string(),
        refresh_token: text_of(source, "refresh_token").map(str::to_string),
        expires_at,
        scope: text_of(source, "scope").map(str::to_string),
    })
}

/// The scopes to ask for: `configured` if any, else the challenge's `scope`
/// (space-separated), else the resource's `scopes_supported`.
pub fn choose_scopes(
    configured: &[String],
    challenge_scope: Option<&str>,
    resource: &ProtectedResource,
) -> Vec<String> {
    if !configured.is_empty() {
        return configured.to_vec();
    }
    let from_challenge: Vec<String> = challenge_scope
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if !from_challenge.is_empty() {
        return from_challenge;
    }
    resource.scopes_supported.clone()
}

/// One challenge of a `WWW-Authenticate` header: a scheme and its parameters.
struct Challenge {
    scheme: String,
    params: Vec<(String, String)>,
}

/// A byte that ends a token: whitespace, a comma, or an equals sign.
fn ends_token(byte: u8) -> bool {
    byte == b',' || byte == b'=' || byte.is_ascii_whitespace()
}

/// Split a `WWW-Authenticate` value into challenges
/// ([RFC 9110 §11.6.1](https://www.rfc-editor.org/rfc/rfc9110#section-11.6.1)).
/// A word followed by `=` is a parameter of the challenge before it; any
/// other word starts a new challenge. Every cut falls on an ASCII byte, so
/// slicing the text there is always on a character boundary.
fn parse_challenges(value: &str) -> Vec<Challenge> {
    let bytes = value.as_bytes();
    let mut challenges: Vec<Challenge> = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && (bytes[i] == b',' || bytes[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && !ends_token(bytes[i]) {
            i += 1;
        }
        if i == start {
            // A stray `=`.
            i += 1;
            continue;
        }
        let word = &value[start..i];
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'=' {
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let (text, end) = read_param_value(value, j);
            i = end;
            if let Some(last) = challenges.last_mut() {
                last.params.push((word.to_ascii_lowercase(), text));
            }
        } else {
            challenges.push(Challenge { scheme: word.to_string(), params: Vec::new() });
        }
    }
    challenges
}

/// A parameter's value starting at byte `at`: a quoted string with backslash
/// escapes, or a bare token up to the next comma or space. Returns it with
/// the byte after it.
fn read_param_value(value: &str, at: usize) -> (String, usize) {
    let bytes = value.as_bytes();
    if bytes.get(at) == Some(&b'"') {
        let mut text = String::new();
        let mut escaped = false;
        let mut end = value.len();
        for (offset, ch) in value[at + 1..].char_indices() {
            if escaped {
                text.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                end = at + 1 + offset + 1;
                break;
            } else {
                text.push(ch);
            }
        }
        (text, end)
    } else {
        let mut end = at;
        while end < bytes.len() && bytes[end] != b',' && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        (value[at..end].to_string(), end)
    }
}

/// Read `resource_metadata` and `scope` out of a `WWW-Authenticate: Bearer`
/// header value. Only the Bearer challenge is read, so a header that offers
/// Basic first (or several challenges joined by commas) still works.
pub fn parse_www_authenticate(value: &str) -> (Option<String>, Option<String>) {
    let challenges = parse_challenges(value);
    let Some(bearer) = challenges.iter().find(|c| c.scheme.eq_ignore_ascii_case("bearer")) else {
        return (None, None);
    };
    let get = |name: &str| {
        bearer.params.iter().find(|(k, v)| k == name && !v.is_empty()).map(|(_, v)| v.clone())
    };
    (get("resource_metadata"), get("scope"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(authorize: &str, token: &str, methods: &[&str]) -> AuthServer {
        AuthServer {
            issuer: "https://as.example".into(),
            authorization_endpoint: authorize.into(),
            token_endpoint: token.into(),
            registration_endpoint: None,
            code_challenge_methods: methods.iter().map(|m| m.to_string()).collect(),
            token_endpoint_auth_methods: Vec::new(),
            scopes_supported: Vec::new(),
        }
    }

    #[test]
    fn a_server_that_would_send_tokens_in_the_clear_or_lacks_s256_is_refused() {
        let ok = server("https://as.example/authorize", "https://as.example/token", &["S256"]);
        assert!(check_server(&ok).is_ok());
        let local = server("http://127.0.0.1:9/authorize", "http://localhost:9/token", &[]);
        assert!(check_server(&local).is_ok(), "this computer may use plain HTTP");
        let clear = server("https://as.example/authorize", "http://as.example/token", &["S256"]);
        assert!(matches!(check_server(&clear), Err(OAuthError::Discovery(_))));
        let plain = server("https://as.example/authorize", "https://as.example/token", &["plain"]);
        assert!(matches!(check_server(&plain), Err(OAuthError::Discovery(_))));
    }

    #[test]
    fn quoted_values_keep_commas_and_escapes() {
        let (rm, scope) = parse_www_authenticate(
            r#"Bearer realm="a, b", scope="say \"hi\" now", resource_metadata="https://x.test/m""#,
        );
        assert_eq!(rm.as_deref(), Some("https://x.test/m"));
        assert_eq!(scope.as_deref(), Some(r#"say "hi" now"#));
    }

    #[test]
    fn only_the_bearer_challenge_counts() {
        let (rm, scope) = parse_www_authenticate(
            r#"Basic realm="x", scope="nope", Bearer resource_metadata=https://x.test/m, scope=read"#,
        );
        assert_eq!(rm.as_deref(), Some("https://x.test/m"));
        assert_eq!(scope.as_deref(), Some("read"));
        assert_eq!(parse_www_authenticate(r#"Basic realm="x""#), (None, None));
        assert_eq!(parse_www_authenticate(""), (None, None));
    }

    #[test]
    fn canonical_resource_drops_the_slash_the_fragment_and_the_credentials() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert_eq!(canonical_resource(&url("https://MCP.Slack.com/")), "https://mcp.slack.com");
        assert_eq!(canonical_resource(&url("https://u:p@x.test/mcp/#frag")), "https://x.test/mcp");
        assert_eq!(canonical_resource(&url("https://x.test/mcp?k=v")), "https://x.test/mcp?k=v");
    }

    #[test]
    fn expiry_reads_numbers_and_strings() {
        assert_eq!(seconds_of(&serde_json::json!(3600)), Some(3600));
        assert_eq!(seconds_of(&serde_json::json!("3600")), Some(3600));
        assert_eq!(seconds_of(&serde_json::json!(0)), None);
        assert_eq!(seconds_of(&serde_json::json!("soon")), None);
    }

    /// A fake authorization server on this computer, answering `resource` as
    /// its protected resource metadata and `as_issuer` as the issuer in its
    /// own. Returns its base address.
    async fn fake_metadata(resource: &str, as_issuer: &str) -> String {
        use axum::{Json, Router, routing::get};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let resource = serde_json::json!({
            "resource": resource.replace("BASE", &base),
            "authorization_servers": [base.clone()],
        });
        let server = serde_json::json!({
            "issuer": as_issuer.replace("BASE", &base),
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "code_challenge_methods_supported": ["S256"],
        });
        let router = Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(move || {
                    let resource = resource.clone();
                    async move { Json(resource) }
                }),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                get(move || {
                    let server = server.clone();
                    async move { Json(server) }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        base
    }

    fn local_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn metadata_that_speaks_for_another_resource_or_issuer_is_not_trusted() {
        // Its own address, and a parent of it, are fine.
        for resource in ["BASE/mcp", "BASE/mcp/", "BASE"] {
            let base = fake_metadata(resource, "BASE").await;
            let found = discover(&local_client(), &format!("{base}/mcp"), None).await;
            assert!(found.is_ok(), "{resource}: {found:?}");
        }
        // Another host, another port, another path: not.
        for resource in ["https://elsewhere.example/mcp", "http://127.0.0.1:1/mcp", "BASE/other"] {
            let base = fake_metadata(resource, "BASE").await;
            let error = discover(&local_client(), &format!("{base}/mcp"), None).await.unwrap_err();
            assert!(
                matches!(&error, OAuthError::Discovery(m) if m.contains("different address")),
                "{resource}: {error:?}"
            );
        }
        // An authorization server that says it is somebody else.
        let base = fake_metadata("BASE/mcp", "https://login.example").await;
        let error = discover(&local_client(), &format!("{base}/mcp"), None).await.unwrap_err();
        assert!(
            matches!(&error, OAuthError::Discovery(m) if m.contains("different issuer")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn an_address_the_gate_refuses_is_never_fetched() {
        // Nothing here could be reached if it were tried: the reasons must be
        // the gate's (and the scheme's), not a failure to connect.
        let refuse = |_: &Url| Err::<(), String>("points at a private network".to_string());
        let error = discover_gated(
            &local_client(),
            "https://mcp.invalid/mcp",
            Some("http://169.254.169.254/latest/meta-data"),
            &refuse,
        )
        .await
        .unwrap_err();
        let OAuthError::Discovery(reason) = error else { panic!("wrong error") };
        assert!(reason.contains("points at a private network"), "{reason}");
        assert!(!reason.contains("connect"), "{reason}");
        assert!(!reason.contains("169.254"), "{reason}");
    }

    #[test]
    fn endpoints_are_held_to_the_gate_as_well_as_to_https() {
        let ok = server("https://as.example/authorize", "https://as.example/token", &["S256"]);
        assert!(check_server_gated(&ok, &any_address).is_ok());
        let only_as_example = |u: &Url| {
            if u.host_str() == Some("as.example") {
                Ok(())
            } else {
                Err("is not allowed".to_string())
            }
        };
        assert!(check_server_gated(&ok, &only_as_example).is_ok());
        let elsewhere =
            server("https://as.example/authorize", "https://169.254.169.254/token", &[]);
        assert!(matches!(
            check_server_gated(&elsewhere, &only_as_example),
            Err(OAuthError::Discovery(m)) if m.contains("is not allowed")
        ));
    }

    #[test]
    fn a_resource_must_be_the_server_or_a_parent_of_it() {
        let server = Url::parse("https://mcp.example.com/v1/mcp").unwrap();
        assert!(resource_covers(&server, "https://mcp.example.com/v1/mcp"));
        assert!(resource_covers(&server, "https://MCP.example.com/v1/mcp/"));
        assert!(resource_covers(&server, "https://mcp.example.com/v1"));
        assert!(resource_covers(&server, "https://mcp.example.com"));
        assert!(resource_covers(&server, "https://mcp.example.com:443/"));
        assert!(!resource_covers(&server, "https://mcp.example.com/v1/mcpx"));
        assert!(!resource_covers(&server, "https://mcp.example.com/v2"));
        assert!(!resource_covers(&server, "https://mcp.example.com/v1/mcp/deeper"));
        assert!(!resource_covers(&server, "https://other.example.com/v1/mcp"));
        assert!(!resource_covers(&server, "http://mcp.example.com/v1/mcp"));
        assert!(!resource_covers(&server, "https://mcp.example.com:8443/v1/mcp"));
        assert!(!resource_covers(&server, "not an address"));
    }

    #[test]
    fn an_issuer_is_the_address_it_was_asked_for() {
        assert!(same_address("https://as.example", "https://as.example/"));
        assert!(same_address("https://as.example/t/", "https://AS.example/t"));
        assert!(same_address("https://as.example:443", "https://as.example"));
        assert!(!same_address("https://as.example/t", "https://as.example"));
        assert!(!same_address("https://as.example", "https://evil.example"));
        assert!(!same_address("https://as.example", "http://as.example"));
    }

    #[test]
    fn an_address_must_be_secure_before_the_gate_is_asked() {
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let count = |_: &Url| {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        };
        assert!(guard("https://x.example/m", &count).is_ok());
        assert!(guard("http://127.0.0.1:9/m", &count).is_ok());
        assert!(guard("http://x.example/m", &count).is_err());
        assert!(guard("file:///etc/passwd", &count).is_err());
        assert!(guard("nonsense", &count).is_err());
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
