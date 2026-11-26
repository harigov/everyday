//! What a browser can ask for, and the gatekeeper in front of all of it.
//!
//! # The gatekeeper runs first, for everything
//!
//! It wraps the whole router rather than being one of its layers, because
//! it has to rewrite the address of a media request before routing sees it
//! -- and because "everything" has to include the static files: a device
//! that may not use this vault has no business being handed even its login
//! page. In order, it:
//!
//! 1. refuses a request that came through a proxy;
//! 2. asks the [`Gate`] whether this peer may connect at all;
//! 3. refuses a write whose `Origin` is not this server;
//! 4. resolves the session -- from `/m/<media token>/...` for a picture or a
//!    body, from the cookie for anything else -- and tells the routes behind
//!    it through two headers of its own ([`SESSION_HEADER`] and
//!    [`ACCESS_HEADER`]), removing any copy a client sent first;
//! 5. adds the headers every answer carries (not HSTS -- see `gatekeeper`).
//!
//! The `/v1` routes are `crate::routes`' own, served with
//! `Transport::Web`, whose `authenticate` reads those two headers and
//! nothing else.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use karmayoga_service::ctx::{Caller, Ctx, Scope};
use karmayoga_service::error::{CommandError, codes};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use super::Config;
use super::assets::Assets;
use super::network::Gate;
use super::session::{self, Sessions};
use crate::auth::Registry;
use crate::routes::Server;
use crate::sse::Broadcaster;
use crate::wire;

/// Set by the gatekeeper, never believed from a client: the id of the
/// session this request belongs to.
pub(crate) const SESSION_HEADER: &str = "x-karmayoga-web-session";
/// Set by the gatekeeper beside [`SESSION_HEADER`]: `full` for a request
/// that carried the cookie, `media` for one that carried the media token.
pub(crate) const ACCESS_HEADER: &str = "x-karmayoga-web-access";
/// Sent by the page: which tab of the browser this is. Every tab shares the
/// one cookie, and so the one session; without this, a write made in one
/// tab was stamped with the very origin the other tab's event stream skips
/// as its own, and the other tab never heard about it. Only names an origin
/// -- it grants nothing, so a client that lies about it fools only itself.
pub(crate) const TAB_HEADER: &str = "x-karmayoga-tab";

/// The origin a request's writes are stamped with and its event stream
/// skips: the session's id, and the tab's when the page named one.
pub(crate) fn origin_of(session: &str, headers: &HeaderMap) -> String {
    let tab: String = headers
        .get(TAB_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    if tab.is_empty() { session.to_string() } else { format!("{session}.{tab}") }
}

/// Headers that say a proxy is in the way. Nothing here is meant to sit
/// behind one, and one that does would make every request look as if it
/// came from the proxy's own address.
const PROXY_HEADERS: &[&str] =
    &["forwarded", "x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "x-real-ip", "via"];

/// What the interface's own pages may do. The desktop window's policy
/// (`tauri.conf.json`) with its custom schemes swapped for this origin, and
/// with `frame-ancestors` so no other page can frame this one.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; media-src 'self' data: blob:; font-src 'self' data:; \
    connect-src 'self'; frame-src 'self'; worker-src 'self' blob:; \
    object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'";

/// What everything under `/v1/` is sent with unless its route said
/// otherwise: the same sandbox the desktop's `karmayoga:` responses carry.
/// A blob's type is sniffed from its bytes, and a picture can be an SVG; a
/// person opening one by its address in this browser would otherwise run
/// its script on this origin, where the session cookie goes with every
/// request it makes.
const API_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

pub(crate) struct WebState {
    pub server: Arc<Server>,
    pub sessions: Arc<Sessions>,
    pub gate: Arc<Gate>,
    pub assets: Assets,
    pub broadcaster: Arc<Broadcaster>,
    pub registry: Arc<Registry>,
    pub config: Config,
    /// `https://name:port` for every name the certificate carries, for the
    /// `Origin` check. Updated by the supervisor when the names change.
    pub origins: RwLock<Vec<String>>,
}

impl WebState {
    fn origin_ok(&self, origin: &HeaderValue) -> bool {
        let Ok(origin) = origin.to_str() else { return false };
        self.origins.read().unwrap().iter().any(|o| o.eq_ignore_ascii_case(origin))
    }
}

/// The router, with the gatekeeper around it -- ready to serve.
pub(crate) fn app(state: Arc<WebState>) -> Router {
    let api = crate::routes::router(state.server.clone(), crate::routes::Transport::Web);
    let web = Router::new()
        .route("/web/hello", get(hello))
        .route("/web/login", post(login))
        .route("/web/logout", post(logout))
        .route("/web/events", get(events))
        .fallback(asset)
        .with_state(state.clone());
    let inner = api.merge(web);
    // Around the whole router, not as one of its layers: see the module doc.
    Router::new()
        .fallback_service(inner)
        .layer(axum::middleware::from_fn_with_state(state, gatekeeper))
}

// ---- the gatekeeper ----------------------------------------------------------

async fn gatekeeper(State(state): State<Arc<WebState>>, mut req: Request, next: Next) -> Response {
    let Some(peer) = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0) else {
        return refuse(StatusCode::FORBIDDEN, "forbidden", "This connection has no address.");
    };
    if PROXY_HEADERS.iter().any(|h| req.headers().contains_key(*h)) {
        return refuse(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This server does not answer requests passed on by a proxy.",
        );
    }
    if let Err(why) = state.gate.admit(peer).await {
        tracing::info!(%peer, %why, "refused a browser");
        return refuse(StatusCode::FORBIDDEN, "forbidden", "This device may not open this vault.");
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD)
        && let Some(origin) = req.headers().get(header::ORIGIN)
        && !state.origin_ok(origin)
    {
        return refuse(
            StatusCode::FORBIDDEN,
            "forbidden",
            "That page may not write to this vault.",
        );
    }

    let ip = peer.ip().to_canonical();
    req.headers_mut().remove(SESSION_HEADER);
    req.headers_mut().remove(ACCESS_HEADER);

    let path = req.uri().path().to_string();
    if let Some(rest) = path.strip_prefix("/m/") {
        let (token, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let tail = format!("/{tail}");
        // Only what a page draws with an `<img>` or a `<video>`: a file's bytes
        // are `/v1/files/file/`, and not `/v1/files/upload/`, which is a POST
        // and goes through the cookie like every other write.
        let media = ["/v1/blob/", "/v1/mail/", "/v1/news/", "/v1/files/file/"]
            .iter()
            .any(|p| tail.starts_with(p));
        if !matches!(*req.method(), Method::GET | Method::HEAD) || !media {
            return refuse(StatusCode::NOT_FOUND, codes::NOT_FOUND, "Nothing is here.");
        }
        let Some(session) = state.sessions.by_media(token, ip) else {
            return refuse(StatusCode::UNAUTHORIZED, "unauthorized", "Sign in again.");
        };
        let rewritten = match req.uri().query() {
            Some(q) => format!("{tail}?{q}"),
            None => tail,
        };
        match rewritten.parse::<Uri>() {
            Ok(uri) => *req.uri_mut() = uri,
            Err(_) => return refuse(StatusCode::BAD_REQUEST, codes::INVALID, "Not an address."),
        }
        mark(req.headers_mut(), &session.id, "media");
    } else if let Some(cookie) = session::cookie_from(req.headers())
        && let Some(session) = state.sessions.by_cookie(&cookie, ip)
    {
        mark(req.headers_mut(), &session.id, "full");
    }

    let api = req.uri().path().starts_with("/v1/");
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    if api && !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(API_POLICY));
    }
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    // No Strict-Transport-Security: it binds the whole host name, every port,
    // and this machine's tailnet name may well serve something else over
    // plain HTTP (a dev server, a dashboard) that it would break for a year.
    // There is no plain-HTTP listener here for it to protect anyway.
    response
}

fn mark(headers: &mut HeaderMap, id: &str, access: &'static str) {
    if let Ok(value) = HeaderValue::from_str(id) {
        headers.insert(SESSION_HEADER, value);
        headers.insert(ACCESS_HEADER, HeaderValue::from_static(access));
    }
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "code": code, "message": message }))).into_response()
}

fn failure(e: CommandError) -> Response {
    (crate::routes::status_for(&e.code), Json(e)).into_response()
}

/// The session the gatekeeper found for a request that needs the cookie.
fn signed_in(state: &WebState, headers: &HeaderMap) -> Option<session::SessionInfo> {
    let id = headers.get(SESSION_HEADER)?.to_str().ok()?;
    if headers.get(ACCESS_HEADER).and_then(|v| v.to_str().ok()) != Some("full") {
        return None;
    }
    // Opened on the vault that is open now, or not at all: see
    // `Sessions::belongs_to`.
    let vault = state.server.service.get()?;
    if !state.sessions.belongs_to(vault.path()) {
        return None;
    }
    state.sessions.by_id(id)
}

// ---- /web/hello ---------------------------------------------------------------

/// What the page asks first: what this vault is called, whether this browser
/// is signed in, and -- when it is -- the vault's status and a media token.
///
/// A browser that is not signed in is told the vault is locked, whatever
/// the machine holding it thinks: from where it stands, it is. The status it
/// is given then carries the name and nothing else of the vault's -- not its
/// path, not its counts.
async fn hello(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let vault = state.server.service.get();
    let unlocked = vault.as_ref().is_some_and(|v| v.is_unlocked());
    let media = match signed_in(&state, &headers) {
        Some(_) if unlocked => session::cookie_from(&headers)
            .and_then(|cookie| state.sessions.media_token(&cookie, peer.ip().to_canonical())),
        _ => None,
    };
    let status = match (&vault, &media) {
        (Some(v), Some(_)) => serde_json::to_value(v.status()).unwrap_or(Value::Null),
        (Some(v), None) => {
            let full = v.status();
            json!({
                "name": full.name,
                "backend": full.backend,
                "unlocked": false,
                "encrypted": full.encrypted,
                "autoLockSeconds": full.auto_lock_seconds,
                "forgetKeySeconds": 0,
                "path": "",
                "writable": full.writable,
            })
        }
        (None, _) => Value::Null,
    };
    Json(json!({
        // The vault open now, not the one open when the switch was thrown --
        // the desktop can open another underneath a running server.
        "name": vault.as_ref().map(|v| v.status().name).unwrap_or_else(|| state.server.name.clone()),
        "protocol": karmayoga_service::PROTOCOL,
        "signedIn": media.is_some(),
        "mediaToken": media,
        "status": status,
        "allowUnlock": state.config.allow_unlock,
    }))
    .into_response()
}

// ---- /web/login -----------------------------------------------------------------

#[derive(Deserialize)]
struct Login {
    password: String,
}

/// The password, once: unlock a locked vault, or check it against an open
/// one, and either way open a session.
///
/// Every attempt goes through the same gate a paired device's `unlock` does
/// -- one Argon2 derivation at a time, server-wide, and a lockout per
/// address after five wrong guesses -- because a browser is not a cheaper
/// way to guess than a paired device is.
async fn login(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Response {
    // A header no form and no cross-site script can add without a preflight,
    // which nothing here answers.
    if headers.get(wire::headers::WEB).is_none() {
        return refuse(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This request did not come from the page.",
        );
    }
    let Some(vault) = state.server.service.get() else {
        return failure(CommandError::new(codes::NO_VAULT, "No vault is open on that computer."));
    };
    if !vault.status().encrypted {
        return failure(CommandError::new(
            codes::FORBIDDEN,
            "This vault has no password, so it cannot be opened from a browser. Give it one in \
             Settings -> Vault on the computer that holds it.",
        ));
    }
    let ip = peer.ip().to_canonical();
    let unlocked = vault.is_unlocked();
    if !unlocked && !state.config.allow_unlock {
        return failure(CommandError::new(
            codes::FORBIDDEN,
            "This vault is locked, and may only be unlocked on the computer that holds it.",
        ));
    }

    let guesser = Caller::Device(format!("web-peer:{ip}"));
    let gate = match state.registry.unlock_gate(&guesser).await {
        Ok(gate) => gate,
        Err(e) => return failure(e),
    };
    let ctx = Ctx { caller: guesser, scopes: vec![Scope::All], proved_at: None, request_id: None };
    let name = if unlocked { "verify_password" } else { "unlock" };
    let outcome = state.server.service.call(ctx, name, json!({ "password": body.password })).await;
    gate.record(outcome.is_ok());
    drop(gate);
    if let Err(e) = outcome {
        return failure(e);
    }

    let opened = state.sessions.open(ip, vault.path());
    tracing::info!(%peer, session = %opened.id, "a browser signed in");
    let status = serde_json::to_value(vault.status()).unwrap_or(Value::Null);
    let mut response =
        Json(json!({ "mediaToken": opened.media, "status": status })).into_response();
    if let Ok(cookie) = HeaderValue::from_str(&session::set_cookie(&opened.cookie)) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

// ---- /web/logout ----------------------------------------------------------------

/// End this browser's session. The vault stays as it is.
async fn logout(headers: HeaderMap, State(state): State<Arc<WebState>>) -> Response {
    if headers.get(wire::headers::WEB).is_none() {
        return refuse(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This request did not come from the page.",
        );
    }
    if let Some(cookie) = session::cookie_from(&headers) {
        state.sessions.close(&cookie);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(cookie) = HeaderValue::from_str(&session::clear_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

// ---- /web/events ----------------------------------------------------------------

/// Everything the service says, for as long as this session lasts.
///
/// `crate::routes`' own `/v1/events` with one difference that matters: the
/// stream ends when the session does -- the vault locking, the browser
/// signing out -- rather than carrying on telling a signed-out page what
/// changed.
async fn events(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    let Some(session) = signed_in(&state, &headers) else {
        return refuse(StatusCode::UNAUTHORIZED, "unauthorized", "Sign in again.");
    };
    if headers.get(wire::headers::PROTOCOL).and_then(|v| v.to_str().ok())
        != Some(karmayoga_service::PROTOCOL.to_string().as_str())
    {
        return refuse(
            StatusCode::UPGRADE_REQUIRED,
            "protocol",
            "This page is older than the server. Reload it.",
        );
    }
    let ended = state.sessions.ended(&session.id);
    // A browser's session holds everything, as `crate::routes::web_session`
    // says; its stream is filtered by that and so hears all there is.
    state
        .broadcaster
        .stream_until(origin_of(&session.id, &headers), vec![Scope::All], ended)
        .into_response()
}

// ---- the interface's own files -------------------------------------------------

async fn asset(State(state): State<Arc<WebState>>, method: Method, uri: Uri) -> Response {
    if !matches!(method, Method::GET | Method::HEAD) {
        return refuse(StatusCode::NOT_FOUND, codes::NOT_FOUND, "Nothing is here.");
    }
    let path = uri.path().trim_start_matches('/');
    // An address under one of the API's own prefixes that no route matched
    // is a mistake, not a page of the interface.
    if path.starts_with("v1/") || path.starts_with("web/") || path.starts_with("m/") {
        return refuse(StatusCode::NOT_FOUND, codes::NOT_FOUND, "Nothing is here.");
    }
    let path = if path.is_empty() { "index.html" } else { path };
    let found = state.assets.get(path).map(|a| (path, a));
    // The interface is one page; any other address without a file extension
    // is a place in it, and gets the page.
    let found = found.or_else(|| {
        let last = path.rsplit('/').next().unwrap_or(path);
        (!last.contains('.')).then(|| state.assets.get("index.html").map(|a| ("index.html", a)))?
    });
    let Some((served, asset)) = found else {
        // Every browser asks for this whether the page names one or not, and
        // the interface has none: an empty answer rather than an error in
        // the console of every page load.
        if path == "favicon.ico" {
            return StatusCode::NO_CONTENT.into_response();
        }
        return refuse(StatusCode::NOT_FOUND, codes::NOT_FOUND, "Nothing is here.");
    };
    let html = asset.mime.starts_with("text/html");
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset.mime.as_str())
        .header(
            header::CACHE_CONTROL,
            // Vite names every built file after its contents, so anything
            // under `assets/` can be kept for ever; the page itself is asked
            // for fresh, so an upgrade is picked up on the next load.
            if served.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            },
        );
    if html {
        response = response.header(header::CONTENT_SECURITY_POLICY, CSP);
    }
    response
        .body(axum::body::Body::from(asset.bytes.into_owned()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The gatekeeper, the login and the session rules, driven through the real
/// router with [`tower::ServiceExt::oneshot`]. The one thing not real is
/// who-is: [`Gate::skip_whois`] admits any address in Tailscale's range
/// without asking a tailscaled this machine may not run. The range check
/// itself still applies -- that is what the first test is about.
#[cfg(test)]
mod tests;
