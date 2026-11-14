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
use everyday_service::ctx::{Caller, Ctx, Scope};
use everyday_service::error::{CommandError, codes};
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

/// Set by the gatekeeper, never believed from a client: the id of the
/// session this request belongs to.
pub(crate) const SESSION_HEADER: &str = "x-everyday-web-session";
/// Set by the gatekeeper beside [`SESSION_HEADER`]: `full` for a request
/// that carried the cookie, `media` for one that carried the media token.
pub(crate) const ACCESS_HEADER: &str = "x-everyday-web-access";
/// Sent by the page: which tab of the browser this is. Every tab shares the
/// one cookie, and so the one session; without this, a write made in one
/// tab was stamped with the very origin the other tab's event stream skips
/// as its own, and the other tab never heard about it. Only names an origin
/// -- it grants nothing, so a client that lies about it fools only itself.
pub(crate) const TAB_HEADER: &str = "x-everyday-tab";

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
    connect-src 'self'; frame-src 'self' blob: data:; worker-src 'self' blob:; \
    object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'";

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
        let media = ["/v1/blob/", "/v1/mail/", "/v1/news/"].iter().any(|p| tail.starts_with(p));
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

    let mut response = next.run(req).await;
    let headers = response.headers_mut();
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
        "protocol": everyday_service::PROTOCOL,
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
    if headers.get("x-everyday-web").is_none() {
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
    if headers.get("x-everyday-web").is_none() {
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
    if headers.get("x-everyday-protocol").and_then(|v| v.to_str().ok())
        != Some(everyday_service::PROTOCOL.to_string().as_str())
    {
        return refuse(
            StatusCode::UPGRADE_REQUIRED,
            "protocol",
            "This page is older than the server. Reload it.",
        );
    }
    let ended = state.sessions.ended(&session.id);
    state.broadcaster.stream_until(origin_of(&session.id, &headers), ended).into_response()
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
mod tests {
    use super::*;
    use crate::web::network::Network;
    use axum::body::Body;
    use axum::http::Request;
    use everyday_service::Service;
    use tower::ServiceExt;

    const PEER: &str = "100.70.65.114:50000";
    const OTHER_DEVICE: &str = "100.96.163.56:50000";
    const PASSWORD: &str = "correct horse battery staple";
    const ORIGIN: &str = "https://box.tail.ts.net:7400";

    struct Rig {
        state: Arc<WebState>,
        service: Arc<Service>,
        _dirs: Vec<tempfile::TempDir>,
    }

    fn rig(password: Option<&str>, config: Config) -> Rig {
        let vault_dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            vault_dir.path(),
            everyday_core::VaultConfig {
                name: "Web".into(),
                password: password.map(str::to_string),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let service = Arc::new(Service::new());
        service.set(vault);

        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let sessions = Arc::new(Sessions::new());
        let broadcaster = Arc::new(Broadcaster::new());
        service.set_events(Arc::new(crate::web::WebSink {
            broadcaster: broadcaster.clone(),
            sessions: sessions.clone(),
        }));
        let server = Server::for_web(
            service.clone(),
            registry.clone(),
            broadcaster.clone(),
            "Web".into(),
            sessions.clone(),
        );
        let mut gate = Gate::new(Network::default());
        gate.skip_whois = true;

        let ui = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ui.path().join("assets")).unwrap();
        std::fs::write(ui.path().join("index.html"), "<!doctype html><title>t</title>").unwrap();
        std::fs::write(ui.path().join("assets/app-abc.js"), "1").unwrap();

        let state = Arc::new(WebState {
            server,
            sessions,
            gate: Arc::new(gate),
            assets: Assets::folder(ui.path().to_path_buf()),
            broadcaster,
            registry,
            config,
            origins: RwLock::new(vec![ORIGIN.into()]),
        });
        Rig { state, service, _dirs: vec![vault_dir, config_dir, ui] }
    }

    async fn send(rig: &Rig, mut req: Request<Body>, peer: &str) -> Response {
        req.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        app(rig.state.clone()).oneshot(req).await.unwrap()
    }

    fn get(path: &str) -> axum::http::request::Builder {
        Request::builder().method(Method::GET).uri(path)
    }

    fn post(path: &str) -> axum::http::request::Builder {
        Request::builder().method(Method::POST).uri(path)
    }

    fn protocol() -> String {
        everyday_service::PROTOCOL.to_string()
    }

    async fn json_of(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    /// Sign in from `peer`, answering the session cookie and media token.
    async fn sign_in(rig: &Rig, peer: &str) -> (String, String) {
        let response = send(
            rig,
            post("/web/login")
                .header("content-type", "application/json")
                .header("x-everyday-web", "1")
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap(),
            peer,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap()
            .to_string();
        assert!(cookie.starts_with("__Host-everyday="), "{cookie}");
        let media = json_of(response).await["mediaToken"].as_str().unwrap().to_string();
        (cookie, media)
    }

    async fn status_with(rig: &Rig, cookie: &str, peer: &str) -> StatusCode {
        send(
            rig,
            post("/v1/call/status")
                .header("cookie", cookie)
                .header("x-everyday-protocol", protocol())
                .body(Body::empty())
                .unwrap(),
            peer,
        )
        .await
        .status()
    }

    #[tokio::test]
    async fn a_device_off_the_tailnet_is_not_even_handed_the_page() {
        let rig = rig(Some(PASSWORD), Config::default());
        for peer in ["192.168.1.5:4000", "127.0.0.1:4000", "172.17.0.2:4000", "[::1]:4000"] {
            let response = send(&rig, get("/").body(Body::empty()).unwrap(), peer).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{peer}");
            let response = send(&rig, get("/web/hello").body(Body::empty()).unwrap(), peer).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{peer}");
        }
    }

    #[tokio::test]
    async fn the_page_and_its_files_are_served_to_a_device_on_the_tailnet() {
        let rig = rig(Some(PASSWORD), Config::default());
        let page = send(&rig, get("/").body(Body::empty()).unwrap(), PEER).await;
        assert_eq!(page.status(), StatusCode::OK);
        assert!(
            page.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'self'")
        );
        assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(page.headers()["referrer-policy"], "no-referrer");

        let script = send(&rig, get("/assets/app-abc.js").body(Body::empty()).unwrap(), PEER).await;
        assert_eq!(script.status(), StatusCode::OK);
        assert!(script.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));

        // A place in the interface is the page; a missing file is not.
        let place = send(&rig, get("/mail/inbox").body(Body::empty()).unwrap(), PEER).await;
        assert_eq!(place.status(), StatusCode::OK);
        let missing = send(&rig, get("/missing.js").body(Body::empty()).unwrap(), PEER).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let api = send(&rig, get("/v1/nothing").body(Body::empty()).unwrap(), PEER).await;
        assert_eq!(api.status(), StatusCode::NOT_FOUND);
        // Pairing is not something a browser does.
        let pair = send(&rig, post("/v1/pair").body(Body::empty()).unwrap(), PEER).await;
        assert_eq!(pair.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_request_passed_on_by_a_proxy_is_refused() {
        let rig = rig(Some(PASSWORD), Config::default());
        for proxy in ["x-forwarded-for", "forwarded", "x-real-ip", "via"] {
            let response =
                send(&rig, get("/").header(proxy, "1.2.3.4").body(Body::empty()).unwrap(), PEER)
                    .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{proxy}");
        }
    }

    #[tokio::test]
    async fn the_password_opens_a_session_and_a_wrong_one_does_not() {
        let rig = rig(Some(PASSWORD), Config::default());
        let hello =
            json_of(send(&rig, get("/web/hello").body(Body::empty()).unwrap(), PEER).await).await;
        assert_eq!(hello["signedIn"], false);
        assert_eq!(hello["status"]["unlocked"], false, "not signed in reads as locked");
        assert_eq!(hello["status"]["path"], "", "and gives nothing of the vault away");

        let wrong = send(
            &rig,
            post("/web/login")
                .header("content-type", "application/json")
                .header("x-everyday-web", "1")
                .body(Body::from(json!({ "password": "nope" }).to_string()))
                .unwrap(),
            PEER,
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert!(wrong.headers().get(header::SET_COOKIE).is_none());

        // Without the page's own header, as a form on another site would send.
        let bare = send(
            &rig,
            post("/web/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap(),
            PEER,
        )
        .await;
        assert_eq!(bare.status(), StatusCode::FORBIDDEN);

        let (cookie, media) = sign_in(&rig, PEER).await;
        assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::OK);

        let hello = json_of(
            send(
                &rig,
                get("/web/hello").header("cookie", &cookie).body(Body::empty()).unwrap(),
                PEER,
            )
            .await,
        )
        .await;
        assert_eq!(hello["signedIn"], true);
        assert_eq!(hello["mediaToken"], media.as_str(), "a reload is handed the same token");
        assert_eq!(hello["status"]["unlocked"], true);

        // The same cookie from another device is nobody.
        assert_eq!(status_with(&rig, &cookie, OTHER_DEVICE).await, StatusCode::UNAUTHORIZED);
        // And a call without the protocol header is refused before it runs.
        let no_protocol = send(
            &rig,
            post("/v1/call/status").header("cookie", &cookie).body(Body::empty()).unwrap(),
            PEER,
        )
        .await;
        assert_eq!(no_protocol.status(), StatusCode::UPGRADE_REQUIRED);
    }

    #[tokio::test]
    async fn a_session_named_by_the_client_is_not_believed() {
        let rig = rig(Some(PASSWORD), Config::default());
        let opened = rig
            .state
            .sessions
            .open(PEER.parse::<SocketAddr>().unwrap().ip(), rig.service.get().unwrap().path());
        let response = send(
            &rig,
            post("/v1/call/status")
                .header(SESSION_HEADER, &opened.id)
                .header(ACCESS_HEADER, "full")
                .header("x-everyday-protocol", protocol())
                .body(Body::empty())
                .unwrap(),
            PEER,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_media_token_reads_media_and_nothing_else() {
        let rig = rig(Some(PASSWORD), Config::default());
        let (cookie, media) = sign_in(&rig, PEER).await;

        let blob: String = json_of(
            send(
                &rig,
                post("/v1/blob")
                    .header("cookie", &cookie)
                    .header("x-everyday-protocol", protocol())
                    .body(Body::from(b"a picture".to_vec()))
                    .unwrap(),
                PEER,
            )
            .await,
        )
        .await
        .as_str()
        .unwrap()
        .to_string();

        // An `<img>` sends no protocol header and, in a sandboxed frame, no
        // cookie: the token in the address is all it has.
        let picture = send(
            &rig,
            get(&format!("/m/{media}/v1/blob/{blob}")).body(Body::empty()).unwrap(),
            PEER,
        )
        .await;
        assert_eq!(picture.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(picture.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&bytes[..], b"a picture");

        for (method, path) in [
            (Method::GET, format!("/m/{media}/v1/call/status")),
            (Method::POST, format!("/m/{media}/v1/call/status")),
            (Method::POST, format!("/m/{media}/v1/blob")),
            (Method::GET, format!("/m/{media}/web/hello")),
        ] {
            let response = send(
                &rig,
                Request::builder().method(method.clone()).uri(&path).body(Body::empty()).unwrap(),
                PEER,
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        }
        let wrong = send(
            &rig,
            get(&format!("/m/not-it/v1/blob/{blob}")).body(Body::empty()).unwrap(),
            PEER,
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let elsewhere = send(
            &rig,
            get(&format!("/m/{media}/v1/blob/{blob}")).body(Body::empty()).unwrap(),
            OTHER_DEVICE,
        )
        .await;
        assert_eq!(elsewhere.status(), StatusCode::UNAUTHORIZED);
        // And the token is not a cookie.
        let as_cookie = format!("__Host-everyday={media}");
        assert_eq!(status_with(&rig, &as_cookie, PEER).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn locking_the_vault_signs_every_browser_out() {
        let rig = rig(Some(PASSWORD), Config::default());
        let (cookie, media) = sign_in(&rig, PEER).await;
        let (other, _) = sign_in(&rig, OTHER_DEVICE).await;

        let locked = send(
            &rig,
            post("/v1/call/lock")
                .header("cookie", &cookie)
                .header("x-everyday-protocol", protocol())
                .body(Body::empty())
                .unwrap(),
            PEER,
        )
        .await;
        assert_eq!(locked.status(), StatusCode::OK);
        assert!(!rig.service.get().unwrap().is_unlocked());

        assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status_with(&rig, &other, OTHER_DEVICE).await, StatusCode::UNAUTHORIZED);
        let picture =
            send(&rig, get(&format!("/m/{media}/v1/blob/x")).body(Body::empty()).unwrap(), PEER)
                .await;
        assert_eq!(picture.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rig.state.sessions.count(), 0);

        // The password unlocks it again from the browser.
        let (cookie, _) = sign_in(&rig, PEER).await;
        assert!(rig.service.get().unwrap().is_unlocked());
        assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn opening_another_vault_signs_the_browsers_of_the_first_out() {
        let rig = rig(Some(PASSWORD), Config::default());
        let (cookie, media) = sign_in(&rig, PEER).await;
        assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::OK);

        // What the desktop's "open" or "create" does underneath a running
        // server: one vault out, another -- here with no password, so open
        // the moment it is set -- in.
        let other = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            other.path(),
            everyday_core::VaultConfig {
                name: "Other".into(),
                password: None,
                ..Default::default()
            },
        )
        .unwrap();
        rig.service.set(vault);

        assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::UNAUTHORIZED);
        let picture =
            send(&rig, get(&format!("/m/{media}/v1/blob/x")).body(Body::empty()).unwrap(), PEER)
                .await;
        assert_eq!(picture.status(), StatusCode::UNAUTHORIZED);
        let hello = json_of(
            send(
                &rig,
                get("/web/hello").header("cookie", &cookie).body(Body::empty()).unwrap(),
                PEER,
            )
            .await,
        )
        .await;
        assert_eq!(hello["signedIn"], false);
        assert_eq!(hello["name"], "Other", "the name is the vault open now");
    }

    #[test]
    fn each_tab_is_its_own_origin_and_cannot_name_one_with_punctuation() {
        let mut headers = HeaderMap::new();
        assert_eq!(origin_of("web-1", &headers), "web-1");
        headers.insert(TAB_HEADER, HeaderValue::from_static("a1b2"));
        assert_eq!(origin_of("web-1", &headers), "web-1.a1b2");
        headers.insert(TAB_HEADER, HeaderValue::from_static("../x y"));
        assert_eq!(origin_of("web-1", &headers), "web-1.xy");
    }

    #[tokio::test]
    async fn a_locked_vault_stays_locked_when_unlocking_from_a_browser_is_off() {
        let rig = rig(Some(PASSWORD), Config { allow_unlock: false, ..Config::default() });
        rig.service.get().unwrap().lock();
        let response = send(
            &rig,
            post("/web/login")
                .header("content-type", "application/json")
                .header("x-everyday-web", "1")
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap(),
            PEER,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!rig.service.get().unwrap().is_unlocked());
    }

    #[tokio::test]
    async fn a_vault_with_no_password_cannot_be_opened_from_a_browser() {
        let rig = rig(None, Config::default());
        let response = send(
            &rig,
            post("/web/login")
                .header("content-type", "application/json")
                .header("x-everyday-web", "1")
                .body(Body::from(json!({ "password": "" }).to_string()))
                .unwrap(),
            PEER,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(rig.state.sessions.count(), 0);
    }

    #[tokio::test]
    async fn a_write_from_another_origin_is_refused() {
        let rig = rig(Some(PASSWORD), Config::default());
        let login = |origin: &str| {
            post("/web/login")
                .header("content-type", "application/json")
                .header("x-everyday-web", "1")
                .header(header::ORIGIN, origin)
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap()
        };
        let evil = send(&rig, login("https://evil.example"), PEER).await;
        assert_eq!(evil.status(), StatusCode::FORBIDDEN);
        let ours = send(&rig, login(ORIGIN), PEER).await;
        assert_eq!(ours.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn too_many_wrong_passwords_lock_that_device_out() {
        let rig = rig(Some(PASSWORD), Config::default());
        let attempt = |password: &str| {
            post("/web/login")
                .header("content-type", "application/json")
                .header("x-everyday-web", "1")
                .body(Body::from(json!({ "password": password }).to_string()))
                .unwrap()
        };
        for _ in 0..5 {
            assert_eq!(send(&rig, attempt("wrong"), PEER).await.status(), StatusCode::UNAUTHORIZED);
        }
        // Even the right one is turned away now -- from this device.
        assert_eq!(
            send(&rig, attempt(PASSWORD), PEER).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(send(&rig, attempt(PASSWORD), OTHER_DEVICE).await.status(), StatusCode::OK);
    }
}
