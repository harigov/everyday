//! Heartbeats from collectors that are not this application: the browser
//! extension and the GNOME Shell extension.
//!
//! Two doors onto one handler, both speaking the same two routes:
//!
//! ```text
//! GET  /v1/screentime/hello       -> Hello
//! POST /v1/screentime/heartbeats  <- HeartbeatBody   -> IngestReport
//! ```
//!
//! - **The loopback listener** ([`start`]), plain HTTP on `127.0.0.1:7399`,
//!   run by the desktop shell. It forwards through a [`Forward`] the shell
//!   implements, so in a window looking at another machine's vault the
//!   heartbeats go there; and it names the device itself, always this
//!   machine's current name, whatever a body says. A collector on this
//!   machine is this machine: a connection code or a GNOME config written
//!   before a rename would otherwise go on sending the old name, and a
//!   report reconciles a browser's sites with its app only within one
//!   device -- the old name would count every browsing second twice.
//! - **The sharing server**, the same two routes on its TLS router (see
//!   `routes.rs`), for a collector on a machine with no copy of this
//!   application running. There the body must name its device, and the
//!   batch goes through `record_screen_time` under the token's own [`Ctx`].
//!
//! # What a request has to carry
//!
//! `Authorization: Bearer <token>`, for a device in the one [`Registry`]
//! whose scopes hold [`Scope::Heartbeats`] (or `All`). Nothing else: no
//! protocol header, because the extensions are not built from this
//! workspace and must keep working across its releases -- these two routes
//! are the stable surface, and their bodies only ever gain optional fields.
//!
//! A token minted for an MCP client is the one exception to `All`: it
//! reaches these routes only if it names `heartbeats` itself. Such a token
//! is issued with `All` unless its owner narrows it, for somebody else's
//! model; letting it pose as a collector -- and so write made-up usage into
//! the person's days -- would be one more thing it can do that nobody who
//! minted it was asked about. The same rule `crate::mcp` applies to Money
//! and Screen time's own tools.
//!
//! # What the answers are
//!
//! Every refusal is the `{"code", "message"}` every other route answers
//! with, and its status is the one `routes.rs` gives that code, except where
//! the collectors' contract (`docs/plans/screentime.md`, "The ingest
//! routes") says otherwise:
//!
//! | status | when                                              |
//! |--------|---------------------------------------------------|
//! | 401    | no token, or one this registry does not know      |
//! | 403    | a token without the scope; a bad `Host`/`Origin`  |
//! | 400    | a body that is not a batch; no `device` over TLS  |
//! | 413    | a body past [`MAX_BODY_BYTES`], or too many beats |
//! | 409    | `locked` -- and `no_vault`, the same "hold on"    |
//!
//! `409` for both is what lets a collector have one rule: keep the queue
//! and try again later. A vault that is locked and a window that has no
//! vault open yet are the same thing to an extension, and `routes.rs`'s
//! `422` for them would read as "this request is wrong", which tempts a
//! collector to drop what it holds.
//!
//! # The loopback listener is reachable by every web page
//!
//! So, as MCP's: the `Host` must be a loopback name (DNS rebinding), an
//! `Origin`, when present, must be an extension's (`chrome-extension://`,
//! `moz-extension://`, `safari-web-extension://`) -- a web page's is
//! refused -- and only those origins are ever answered with CORS headers.
//! A body is at most [`MAX_BODY_BYTES`], and is read only once the token
//! checked out, so a stranger cannot make this hold two megabytes.
//!
//! None of that applies to the sharing server's copy of the routes: it is
//! TLS to a pinned certificate, not something a page can rebind a name to,
//! and an extension reaching it does so through the tailnet's real
//! certificate, from its own origin, with a token.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use everyday_core::screentime::{CollectorConfig, Heartbeat, HeartbeatBatch, IngestReport};
use everyday_service::error::codes;
use everyday_service::{Caller, CommandError, CommandResult, Ctx, Scope};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Registry;
use crate::routes::{Server, Transport};

/// The port the loopback listener binds by default: two along from sharing,
/// one from MCP.
pub const DEFAULT_SCREENTIME_PORT: u16 = 7399;

/// Largest body either route accepts.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// The origins a browser extension's requests carry. A web page's never
/// starts with one of these, and no page can make its own do so.
const EXTENSION_ORIGINS: &[&str] =
    &["chrome-extension://", "moz-extension://", "safari-web-extension://"];

/// What `POST /v1/screentime/heartbeats` takes. `device` is required on the
/// sharing server and ignored on the loopback listener, which always names
/// this machine (see the module doc).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatBody {
    #[serde(default)]
    pub device: Option<String>,
    pub collector: String,
    pub beats: Vec<Heartbeat>,
}

/// What `GET /v1/screentime/hello` answers.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    /// This machine's name on the loopback listener; empty on the sharing
    /// server, where a collector names its own.
    pub device: String,
    pub idle_minutes: u32,
    pub keep_titles: bool,
    /// This build's version, for an extension to show.
    pub version: String,
}

impl Hello {
    fn new(device: String, config: CollectorConfig) -> Self {
        Self {
            device,
            idle_minutes: config.idle_minutes,
            keep_titles: config.keep_titles,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Where the loopback listener hands what it was sent. The shell's
/// implementation calls `record_screen_time` and `screen_time_collector`
/// through whichever vault its window is looking at.
pub trait Forward: Send + Sync + 'static {
    /// This machine's current name: every loopback delivery is stamped with
    /// it, and [`Hello`] answers it.
    fn device(&self) -> String;
    fn record(&self, body: HeartbeatBody) -> BoxFuture<'static, CommandResult<IngestReport>>;
    fn config(&self) -> BoxFuture<'static, CommandResult<CollectorConfig>>;
    /// Told after every accepted delivery, with the collector's name -- so
    /// the shell can say which extensions it has heard from.
    fn heard(&self, collector: &str);
}

/// The loopback listener's state.
pub struct Listener {
    pub forward: Arc<dyn Forward>,
    pub registry: Arc<Registry>,
}

// ---- what both doors share ------------------------------------------------

/// A refusal, as either door answers it: the status the contract gives its
/// code -- see the module doc's table -- and the `{code, message}` body.
fn refused(e: CommandError) -> Response {
    (status_of(&e.code), Json(e)).into_response()
}

/// `routes.rs`'s status for a code, except the two that mean "not now".
fn status_of(code: &str) -> StatusCode {
    match code {
        codes::LOCKED | codes::NO_VAULT => StatusCode::CONFLICT,
        other => crate::routes::status_for(other),
    }
}

/// Answer `outcome` as JSON, or as the refusal it is.
fn answer<T: Serialize>(outcome: CommandResult<T>) -> Response {
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(e) => refused(e),
    }
}

/// Who is sending heartbeats: the device the bearer token belongs to,
/// holding [`Scope::Heartbeats`]. `401` without a token the registry knows,
/// `403` with one that may not send them -- see the module doc for why a
/// token minted for an MCP client has to name the scope.
pub(crate) fn authenticate(registry: &Arc<Registry>, headers: &HeaderMap) -> CommandResult<Ctx> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| CommandError::new("unauthorized", "this request carried no token"))?;
    let ctx = registry.authenticate(token)?;
    let minted_for_mcp = matches!(
        &ctx.caller,
        Caller::Device(id) if everyday_core::agent::tools::is_mcp_device_id(id)
    );
    let may_send =
        if minted_for_mcp { ctx.names(Scope::Heartbeats) } else { ctx.holds(Scope::Heartbeats) };
    if !may_send {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "this token may not send screen-time heartbeats; issue one for a collector in \
             Settings → Screen time",
        ));
    }
    Ok(ctx)
}

/// Read a batch, at most [`MAX_BODY_BYTES`] of it. Only ever called once
/// the token has checked out.
async fn read_heartbeats(body: Body) -> CommandResult<HeartbeatBody> {
    let bytes = crate::routes::read_body(body, MAX_BODY_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|e| {
        CommandError::new(codes::INVALID, format!("that is not a batch of heartbeats: {e}"))
    })
}

fn internal(e: serde_json::Error) -> CommandError {
    CommandError::new(codes::INTERNAL, e.to_string())
}

// ---- the sharing server's door --------------------------------------------

type Shared = State<(Arc<Server>, Transport)>;

/// Who is calling the sharing server's copy of the routes. Over its local
/// socket nobody has to prove anything, as everywhere else on that router;
/// over the network, a heartbeats token.
fn shared_caller(
    server: &Arc<Server>,
    transport: Transport,
    headers: &HeaderMap,
) -> CommandResult<Ctx> {
    match transport {
        Transport::Socket => Ok(Ctx {
            caller: Caller::Socket,
            scopes: vec![Scope::All],
            proved_at: None,
            request_id: None,
        }),
        Transport::Network => authenticate(&server.registry, headers),
    }
}

/// `GET /v1/screentime/hello` on the sharing server: what the vault tells a
/// collector, and no device -- this machine is not the collector's.
pub(crate) async fn shared_hello(
    State((server, transport)): Shared,
    headers: HeaderMap,
) -> Response {
    answer(shared_config(&server, transport, &headers).await)
}

async fn shared_config(
    server: &Arc<Server>,
    transport: Transport,
    headers: &HeaderMap,
) -> CommandResult<Hello> {
    let ctx = shared_caller(server, transport, headers)?;
    let value = server.service.call(ctx, "screen_time_collector", json!({})).await?;
    let config: CollectorConfig = serde_json::from_value(value).map_err(internal)?;
    Ok(Hello::new(String::new(), config))
}

/// `POST /v1/screentime/heartbeats` on the sharing server: the batch, as
/// `record_screen_time`, under the token's own [`Ctx`].
pub(crate) async fn shared_heartbeats(
    State((server, transport)): Shared,
    headers: HeaderMap,
    body: Body,
) -> Response {
    answer(shared_record(&server, transport, &headers, body).await)
}

async fn shared_record(
    server: &Arc<Server>,
    transport: Transport,
    headers: &HeaderMap,
    body: Body,
) -> CommandResult<Value> {
    let ctx = shared_caller(server, transport, headers)?;
    let body = read_heartbeats(body).await?;
    // Nothing here knows which machine sent this: the server is somebody
    // else's computer, and guessing would file a laptop's day under the
    // server's name.
    let device = body
        .device
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| {
            CommandError::new(
                codes::INVALID,
                "say which device these heartbeats are from: a sharing server cannot tell",
            )
        })?
        .to_string();
    let batch = HeartbeatBatch { device, collector: body.collector, beats: body.beats };
    let args = serde_json::to_value(&batch).map_err(internal)?;
    server.service.call(ctx, "record_screen_time", args).await
}

// ---- the loopback listener ------------------------------------------------

fn router(listener: Arc<Listener>) -> Router {
    Router::new()
        // `options` on both, answered by `guard` before either handler
        // would run: named here so a preflight is a request to a route this
        // router has, whatever axum does with a method a route was not
        // given.
        .route("/v1/screentime/hello", get(loopback_hello).options(preflight))
        .route("/v1/screentime/heartbeats", post(loopback_heartbeats).options(preflight))
        .layer(middleware::from_fn(guard))
        .with_state(listener)
}

/// `GET /v1/screentime/hello` on loopback: this machine's name and what the
/// vault the window is looking at tells a collector.
async fn loopback_hello(State(listener): State<Arc<Listener>>, headers: HeaderMap) -> Response {
    answer(loopback_config(&listener, &headers).await)
}

async fn loopback_config(listener: &Listener, headers: &HeaderMap) -> CommandResult<Hello> {
    authenticate(&listener.registry, headers)?;
    let config = listener.forward.config().await?;
    Ok(Hello::new(listener.forward.device(), config))
}

/// `POST /v1/screentime/heartbeats` on loopback: the batch, with this
/// machine's name when it gave none, handed to the shell.
async fn loopback_heartbeats(
    State(listener): State<Arc<Listener>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    answer(deliver(&listener, &headers, body).await)
}

async fn deliver(
    listener: &Listener,
    headers: &HeaderMap,
    body: Body,
) -> CommandResult<IngestReport> {
    authenticate(&listener.registry, headers)?;
    let mut body = read_heartbeats(body).await?;
    body.device = Some(listener.forward.device());
    let collector = body.collector.trim().to_string();
    let report = listener.forward.record(body).await?;
    // Only once it has landed: "heard from" in Settings means the vault has
    // its heartbeats, not that something knocked.
    listener.forward.heard(&collector);
    Ok(report)
}

/// Never reached -- [`guard`] answers every `OPTIONS` itself -- but what a
/// preflight would get if it were: nothing to say.
async fn preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// What every request to the loopback listener passes before a route sees
/// it: a loopback `Host`, and no `Origin` or an extension's. Answers a
/// preflight itself, and puts the CORS headers on an extension's answers --
/// only an extension's, so a page has nothing to read even where it got a
/// request through.
async fn guard(request: Request, next: Next) -> Response {
    if !host_is_loopback(request.headers(), request.uri()) {
        return refused(CommandError::new(
            codes::FORBIDDEN,
            "this listener answers only requests addressed to this machine's loopback name",
        ));
    }
    let origin = match request.headers().get(header::ORIGIN) {
        None => None,
        Some(value) => match value.to_str() {
            Ok(text) if is_extension_origin(text) => Some(value.clone()),
            _ => {
                return refused(CommandError::new(
                    codes::FORBIDDEN,
                    "only a browser extension may send screen time here, not a web page",
                ));
            }
        },
    };

    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Some(origin) = origin {
            let private_network = request
                .headers()
                .get("access-control-request-private-network")
                .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"true"));
            let headers = response.headers_mut();
            allow_origin(headers, origin);
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST"),
            );
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("Authorization, Content-Type"),
            );
            headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
            // Chrome's Private Network Access asks before a request reaches
            // a loopback address; an extension is let through, but answering
            // costs nothing and keeps a stricter build working.
            if private_network {
                headers.insert(
                    HeaderName::from_static("access-control-allow-private-network"),
                    HeaderValue::from_static("true"),
                );
            }
        }
        return response;
    }

    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        allow_origin(response.headers_mut(), origin);
    }
    response
}

/// Let `origin` -- an extension's, already checked -- read this answer.
fn allow_origin(headers: &mut HeaderMap, origin: HeaderValue) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.append(header::VARY, HeaderValue::from_static("Origin"));
}

/// Is `origin` a browser extension's?
fn is_extension_origin(origin: &str) -> bool {
    EXTENSION_ORIGINS.iter().any(|scheme| origin.len() > scheme.len() && origin.starts_with(scheme))
}

/// Was this request addressed to a loopback name? From `Host`, or the
/// request target's authority where there is no `Host` (HTTP/2). A request
/// with neither is refused: nothing a browser sends lacks both.
fn host_is_loopback(headers: &HeaderMap, uri: &Uri) -> bool {
    match headers.get(header::HOST) {
        Some(value) => value.to_str().is_ok_and(is_loopback_host),
        None => uri.authority().is_some_and(|a| is_loopback_host(a.as_str())),
    }
}

/// `localhost`, a loopback IPv4 address or `[::1]`, with an optional port.
///
/// The check that stops DNS rebinding: a page that points its own name at
/// `127.0.0.1` still sends its own name as the `Host`.
fn is_loopback_host(host: &str) -> bool {
    let (name, port) = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((inside, "")) => (inside, None),
            Some((inside, after)) => match after.strip_prefix(':') {
                Some(port) => (inside, Some(port)),
                None => return false,
            },
            None => return false,
        },
        None => match host.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        },
    };
    if let Some(port) = port
        && (port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Start the loopback listener on `listen` (loopback only: an address that
/// is not is refused).
pub async fn start(
    forward: Arc<dyn Forward>,
    registry: Arc<Registry>,
    listen: SocketAddr,
) -> CommandResult<crate::Running<Arc<Listener>>> {
    // Not a default to be overridden: a heartbeats listener on a reachable
    // address would take made-up usage from the whole network, behind
    // nothing but a token sent in the clear.
    if !listen.ip().is_loopback() {
        return Err(CommandError::new(
            codes::INVALID,
            format!("the screen-time listener only listens on a loopback address, not {listen}"),
        ));
    }
    let state = Arc::new(Listener { forward, registry });
    let socket = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| CommandError::new(codes::IO, format!("could not listen on {listen}: {e}")))?;
    let running = crate::spawn_server(socket, router(state.clone()), None, state)?;
    tracing::info!(address = %running.address, "listening for screen-time heartbeats");
    Ok(running)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loopback_name_or_address_with_or_without_a_port_is_loopback_and_nothing_else_is() {
        for host in [
            "localhost",
            "LOCALHOST:7399",
            "127.0.0.1",
            "127.0.0.1:7399",
            "127.1.2.3:80",
            "[::1]",
            "[::1]:7399",
        ] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in [
            "",
            "evil.example",
            "evil.example:7399",
            "localhost.evil.example",
            "127.0.0.1.nip.io",
            "192.168.1.2:7399",
            "[::1",
            "[::1]x",
            "[::1]:",
            "127.0.0.1:",
            "127.0.0.1:port",
            "127.0.0.1:123456",
            "::1",
            "[fe80::1]:7399",
        ] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn only_an_extensions_origin_is_one() {
        for origin in [
            "chrome-extension://abcdefghijklmnop",
            "moz-extension://0b7e7a5c-2d1f-4a43-9b0a-1c2d3e4f5a6b",
            "safari-web-extension://ABCDEF",
        ] {
            assert!(is_extension_origin(origin), "{origin}");
        }
        for origin in [
            "https://evil.example",
            "http://localhost:7399",
            "null",
            "chrome-extension://",
            "file://",
            "https://chrome-extension.example",
        ] {
            assert!(!is_extension_origin(origin), "{origin}");
        }
    }

    #[test]
    fn locked_and_no_vault_are_conflicts_and_everything_else_is_what_routes_say() {
        assert_eq!(status_of(codes::LOCKED), StatusCode::CONFLICT);
        assert_eq!(status_of(codes::NO_VAULT), StatusCode::CONFLICT);
        assert_eq!(status_of("unauthorized"), StatusCode::UNAUTHORIZED);
        assert_eq!(status_of(codes::FORBIDDEN), StatusCode::FORBIDDEN);
        assert_eq!(status_of(codes::INVALID), StatusCode::BAD_REQUEST);
        assert_eq!(status_of(codes::TOO_LARGE), StatusCode::PAYLOAD_TOO_LARGE);
    }
}

/// The loopback listener's router, driven with [`tower::ServiceExt::oneshot`]
/// against a [`Forward`] that only writes down what it was handed -- what
/// is checked is the order a request is checked in, which only the router
/// can show. The same over a real socket, and the sharing server's copy of
/// the routes against a real vault, are in `tests/screentime.rs`.
#[cfg(test)]
mod router_tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;

    #[derive(Default)]
    struct Fake {
        bodies: Mutex<Vec<HeartbeatBody>>,
        heard: Mutex<Vec<String>>,
        locked: AtomicBool,
    }

    impl Forward for Fake {
        fn device(&self) -> String {
            "laptop".into()
        }

        fn record(&self, body: HeartbeatBody) -> BoxFuture<'static, CommandResult<IngestReport>> {
            if self.locked.load(Ordering::SeqCst) {
                return Box::pin(async { Err(CommandError::new(codes::LOCKED, "locked")) });
            }
            let accepted = body.beats.len() as u32;
            self.bodies.lock().unwrap().push(body);
            Box::pin(async move { Ok(IngestReport { accepted, rejected: 0, spans: 1 }) })
        }

        fn config(&self) -> BoxFuture<'static, CommandResult<CollectorConfig>> {
            if self.locked.load(Ordering::SeqCst) {
                return Box::pin(async { Err(CommandError::new(codes::LOCKED, "locked")) });
            }
            Box::pin(async { Ok(CollectorConfig { idle_minutes: 3, keep_titles: true }) })
        }

        fn heard(&self, collector: &str) {
            self.heard.lock().unwrap().push(collector.to_string());
        }
    }

    struct Rig {
        fake: Arc<Fake>,
        listener: Arc<Listener>,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(dir.path().join("devices.json")).unwrap());
        let fake = Arc::new(Fake::default());
        let listener = Arc::new(Listener { forward: fake.clone(), registry });
        Rig { fake, listener, _dir: dir }
    }

    impl Rig {
        fn token(&self, scopes: Vec<Scope>, mcp: bool) -> String {
            self.listener.registry.issue("Firefox", scopes, mcp).unwrap().0
        }

        fn collector(&self) -> String {
            self.token(vec![Scope::Heartbeats], false)
        }

        async fn send(&self, request: Request) -> (StatusCode, HeaderMap, Value) {
            let response = router(self.listener.clone()).oneshot(request).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
        }

        fn delivered(&self) -> Vec<HeartbeatBody> {
            self.fake.bodies.lock().unwrap().clone()
        }

        fn heard(&self) -> Vec<String> {
            self.fake.heard.lock().unwrap().clone()
        }
    }

    /// A request addressed to the loopback listener, as an extension sends it.
    fn to_loopback(
        method: &str,
        path: &str,
        token: Option<&str>,
        extra: &[(&str, &str)],
        body: Body,
    ) -> Request {
        let mut request =
            Request::builder().method(method).uri(path).header("host", "127.0.0.1:7399");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        request.body(body).unwrap()
    }

    fn batch(device: Option<&str>) -> Body {
        let mut body = json!({
            "collector": "firefox",
            "beats": [{
                "kind": "site", "app": "Firefox", "host": "github.com", "title": "a PR",
                "start": "2026-11-05T17:00:00Z", "end": "2026-11-05T17:00:30Z",
            }],
        });
        if let Some(device) = device {
            body["device"] = json!(device);
        }
        Body::from(serde_json::to_vec(&body).unwrap())
    }

    const BEATS: &str = "/v1/screentime/heartbeats";
    const HELLO: &str = "/v1/screentime/hello";

    #[tokio::test]
    async fn a_batch_is_always_handed_on_with_this_machines_current_name() {
        let rig = rig();
        let token = rig.collector();
        // A name in the body -- an old one, from before a rename -- is not
        // taken: on loopback the collector is this machine.
        let cases = [(None, "laptop"), (Some("   "), "laptop"), (Some("desk"), "laptop")];
        for (device, expected) in cases {
            let request = to_loopback("POST", BEATS, Some(&token), &[], batch(device));
            let (status, _, answer) = rig.send(request).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer, json!({ "accepted": 1, "rejected": 0, "spans": 1 }));
            let last = rig.delivered().pop().unwrap();
            assert_eq!(last.device.as_deref(), Some(expected));
            assert_eq!(last.collector, "firefox");
        }
        assert_eq!(rig.heard(), vec!["firefox"; 3], "told once per delivery");
    }

    #[tokio::test]
    async fn hello_names_this_machine_and_says_what_the_vault_tells_a_collector() {
        let rig = rig();
        let token = rig.collector();
        let (status, _, answer) =
            rig.send(to_loopback("GET", HELLO, Some(&token), &[], Body::empty())).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["device"], "laptop");
        assert_eq!(answer["idleMinutes"], 3);
        assert_eq!(answer["keepTitles"], true);
        assert!(!answer["version"].as_str().unwrap().is_empty());
        assert!(rig.heard().is_empty(), "saying hello delivers nothing");
    }

    #[tokio::test]
    async fn only_a_token_that_may_send_heartbeats_gets_through() {
        let rig = rig();
        let library = rig.token(vec![Scope::Library], false);
        let reader = rig.token(vec![Scope::ScreenTime], false);
        let mcp_everything = rig.token(vec![Scope::All], true);
        let mcp_collector = rig.token(vec![Scope::Heartbeats], true);
        let paired = rig.token(vec![Scope::All], false);

        for (token, status, code) in [
            (None, StatusCode::UNAUTHORIZED, "unauthorized"),
            (Some("not-a-token"), StatusCode::UNAUTHORIZED, "unauthorized"),
            (Some(library.as_str()), StatusCode::FORBIDDEN, "forbidden"),
            (Some(reader.as_str()), StatusCode::FORBIDDEN, "forbidden"),
            (Some(mcp_everything.as_str()), StatusCode::FORBIDDEN, "forbidden"),
        ] {
            let routes = [("POST", BEATS, batch(None)), ("GET", HELLO, Body::empty())];
            for (method, path, body) in routes {
                let (got, _, answer) = rig.send(to_loopback(method, path, token, &[], body)).await;
                assert_eq!(got, status, "{token:?} {path}: {answer}");
                assert_eq!(answer["code"], code, "{token:?} {path}");
                assert!(answer["message"].is_string(), "{answer}");
            }
        }
        assert!(rig.delivered().is_empty(), "nothing refused reached the vault");

        for token in [mcp_collector, paired] {
            let (status, _, answer) =
                rig.send(to_loopback("POST", BEATS, Some(&token), &[], batch(None))).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
        }
        assert_eq!(rig.delivered().len(), 2);
    }

    #[tokio::test]
    async fn a_body_is_not_read_for_somebody_with_no_token() {
        let rig = rig();
        let touched = Arc::new(AtomicBool::new(false));
        let watched = {
            let touched = touched.clone();
            Body::from_stream(futures::stream::once(async move {
                touched.store(true, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![b'x'; 1024]))
            }))
        };
        let (status, _, _) = rig.send(to_loopback("POST", BEATS, None, &[], watched)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!touched.load(Ordering::SeqCst), "the body was read before the token");
    }

    #[tokio::test]
    async fn a_body_past_two_megabytes_is_too_large_and_one_that_is_not_a_batch_is_bad() {
        let rig = rig();
        let token = rig.collector();

        // Streamed with no declared length, so the running count catches it.
        let chunk = axum::body::Bytes::from(vec![b' '; 256 * 1024]);
        let chunks = MAX_BODY_BYTES / chunk.len() + 1;
        let huge = Body::from_stream(futures::stream::iter(
            std::iter::repeat_n(chunk, chunks).map(Ok::<_, std::convert::Infallible>),
        ));
        let (status, _, answer) =
            rig.send(to_loopback("POST", BEATS, Some(&token), &[], huge)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
        assert_eq!(answer["code"], "too_large");

        for bad in ["", "not json", r#"{"collector":"firefox"}"#, r#"{"beats":[]}"#] {
            let request = to_loopback("POST", BEATS, Some(&token), &[], Body::from(bad));
            let (status, _, answer) = rig.send(request).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {answer}");
            assert_eq!(answer["code"], "invalid", "{bad:?}");
        }
        assert!(rig.delivered().is_empty());
    }

    #[tokio::test]
    async fn a_locked_vault_is_a_conflict_and_nobody_is_said_to_have_been_heard() {
        let rig = rig();
        rig.fake.locked.store(true, Ordering::SeqCst);
        let token = rig.collector();
        for (method, path, body) in [("POST", BEATS, batch(None)), ("GET", HELLO, Body::empty())] {
            let (status, _, answer) =
                rig.send(to_loopback(method, path, Some(&token), &[], body)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {answer}");
            assert_eq!(answer["code"], "locked", "{path}");
        }
        assert!(rig.heard().is_empty());
    }

    #[tokio::test]
    async fn a_request_addressed_to_any_other_name_is_refused() {
        let rig = rig();
        let token = rig.collector();
        for host in ["evil.example", "evil.example:7399", "192.168.1.2:7399"] {
            let request = Request::builder()
                .method("POST")
                .uri(BEATS)
                .header("host", host)
                .header("authorization", format!("Bearer {token}"))
                .body(batch(None))
                .unwrap();
            let (status, _, _) = rig.send(request).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{host}");
        }
        // No `Host` at all, and no authority in the target to stand in.
        let request = Request::builder()
            .method("GET")
            .uri(HELLO)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(rig.send(request).await.0, StatusCode::FORBIDDEN);
        for host in ["localhost:7399", "[::1]:7399", "127.0.0.1"] {
            let request = Request::builder()
                .method("GET")
                .uri(HELLO)
                .header("host", host)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap();
            assert_eq!(rig.send(request).await.0, StatusCode::OK, "{host}");
        }
        assert!(rig.delivered().is_empty());
    }

    #[tokio::test]
    async fn a_web_page_is_refused_and_told_nothing() {
        let rig = rig();
        let token = rig.collector();
        for origin in ["https://evil.example", "http://127.0.0.1:7399", "null"] {
            for method in ["POST", "OPTIONS"] {
                let request =
                    to_loopback(method, BEATS, Some(&token), &[("origin", origin)], batch(None));
                let (status, headers, _) = rig.send(request).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} from {origin}");
                assert!(
                    headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
                    "{method} from {origin} was given CORS headers"
                );
            }
        }
        assert!(rig.delivered().is_empty());
    }

    #[tokio::test]
    async fn an_extension_is_answered_with_its_own_origin_and_its_preflight_is_allowed() {
        let rig = rig();
        let token = rig.collector();
        let origin = "moz-extension://0b7e7a5c-2d1f-4a43-9b0a-1c2d3e4f5a6b";

        let (status, headers, answer) = rig
            .send(to_loopback("POST", BEATS, Some(&token), &[("origin", origin)], batch(None)))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], origin);
        assert_eq!(headers[header::VARY], "Origin");

        // A preflight carries no token: the browser asks before it sends one.
        let preflight = to_loopback(
            "OPTIONS",
            BEATS,
            None,
            &[
                ("origin", "chrome-extension://abcdefghijklmnop"),
                ("access-control-request-method", "POST"),
                ("access-control-request-headers", "authorization, content-type"),
                ("access-control-request-private-network", "true"),
            ],
            Body::empty(),
        );
        let (status, headers, _) = rig.send(preflight).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "chrome-extension://abcdefghijklmnop"
        );
        let methods = headers[header::ACCESS_CONTROL_ALLOW_METHODS].to_str().unwrap();
        assert!(methods.contains("POST") && methods.contains("GET"), "{methods}");
        let allowed = headers[header::ACCESS_CONTROL_ALLOW_HEADERS].to_str().unwrap();
        assert!(allowed.contains("Authorization") && allowed.contains("Content-Type"), "{allowed}");
        assert_eq!(headers["access-control-allow-private-network"], "true");

        // No origin at all -- a native collector, the GNOME Shell extension
        // -- is answered without any CORS headers.
        let (status, headers, _) =
            rig.send(to_loopback("POST", BEATS, Some(&token), &[], batch(None))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
        assert_eq!(rig.delivered().len(), 2, "the preflight delivered nothing");
    }
}
