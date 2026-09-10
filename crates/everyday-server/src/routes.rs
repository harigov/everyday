//! The router, served on both transports.
//!
//! Built once and handed to two listeners: TLS over TCP for other machines, and
//! a local socket for processes belonging to the same user. That is not a
//! convenience -- it is what lets a browser extension and the command-line tool
//! reach a vault without either of them holding a token or trusting a
//! certificate, and it is why this file never assumes there is a network under
//! it.
//!
//! # Everything under `/v1`
//!
//! A version in the path as well as [`PROTOCOL`] in a header, because they
//! answer different questions. The path says which shape of API this is; the
//! header says which build of the command table is behind it. A client and a
//! server are the same binary here, so the header is the one that will
//! disagree.
//!
//! # What a request has to carry
//!
//! `Authorization: Bearer <token>` on the TLS transport, and nothing on the
//! socket -- the operating system already said who is on the other end.
//! `X-Everyday-Protocol` always; a mismatch is refused rather than attempted.
//! `X-Everyday-Request` on a write, which is what makes a retry safe.

use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use everyday_core::BlobId;
use everyday_service::ctx::{Caller, Ctx, Scope};
use everyday_service::error::CommandError;
use everyday_service::{PROTOCOL, Service};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::auth::Registry;
use crate::sse::Broadcaster;

/// Largest JSON body a command may arrive in.
///
/// Generous -- a board reorder can carry a few hundred tasks, and an imported
/// calendar is a document -- and finite, which is the point. Attachments do not
/// come this way; they have their own route and their own limit.
const MAX_JSON_BYTES: usize = 32 * 1024 * 1024;

/// How many requests one device may have in flight.
///
/// Each one can be holding a blocking thread, so an unbounded client could
/// exhaust the pool and stall every other device. Eight is more parallelism than
/// a window ever needs and less than a runaway loop wants.
const MAX_IN_FLIGHT: usize = 8;

pub struct Server {
    pub service: Arc<Service>,
    pub registry: Arc<Registry>,
    pub broadcaster: Arc<Broadcaster>,
    /// What this vault is called, for the pairing screen.
    pub name: String,
    /// The certificate a client should pin. Empty when TLS is terminated
    /// somewhere else.
    pub fingerprint: String,
    /// May a client unlock the vault over the wire?
    ///
    /// An atomic rather than a field, so changing it does not mean restarting
    /// the server. Restarting to flip one boolean meant rebinding the port,
    /// which meant racing the old listener's shutdown -- and losing that race
    /// left sharing switched off with an "address already in use" behind it.
    allow_remote_unlock: std::sync::atomic::AtomicBool,
    in_flight: Arc<tokio::sync::Semaphore>,
}

impl Server {
    pub fn new(
        service: Arc<Service>,
        registry: Arc<Registry>,
        broadcaster: Arc<Broadcaster>,
        name: String,
        fingerprint: String,
        allow_remote_unlock: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            registry,
            broadcaster,
            name,
            fingerprint,
            allow_remote_unlock: std::sync::atomic::AtomicBool::new(allow_remote_unlock),
            in_flight: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT * 4)),
        })
    }

    pub fn allow_remote_unlock(&self) -> bool {
        self.allow_remote_unlock.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_allow_remote_unlock(&self, allow: bool) {
        self.allow_remote_unlock.store(allow, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How a request arrived, which decides how it is authenticated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// TLS over TCP. A token is required.
    Network,
    /// A local socket. The operating system vouched for the peer.
    Socket,
}

pub fn router(server: Arc<Server>, transport: Transport) -> Router {
    Router::new()
        .route("/v1/hello", get(hello))
        .route("/v1/pair", post(pair))
        .route("/v1/call/{name}", post(call))
        .route("/v1/stream/{name}", post(stream))
        .route("/v1/blob", post(put_blob))
        .route("/v1/blob/{id}", get(get_blob))
        .route("/v1/events", get(events))
        .layer(axum::extract::DefaultBodyLimit::max(Service::MAX_ATTACHMENT_BYTES))
        .with_state((server, transport))
}

type Ctxt = State<(Arc<Server>, Transport)>;

// ---- errors --------------------------------------------------------------

/// A command error, as a response.
///
/// The status is derived from the code rather than chosen at each call site, so
/// a new error code cannot accidentally arrive as a 200 with a body nobody
/// checks. The body is the same `{ code, message }` the interface already knows
/// how to route on, which is what lets a remote client hand the window the
/// error the server raised rather than flattening everything into "network".
struct Failure(CommandError);

impl From<CommandError> for Failure {
    fn from(e: CommandError) -> Self {
        Failure(e)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status = match self.0.code.as_str() {
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "forbidden" => StatusCode::FORBIDDEN,
            "too_many_attempts" => StatusCode::TOO_MANY_REQUESTS,
            "bad_code" | "invalid" => StatusCode::BAD_REQUEST,
            "unknown_command" | "unknown_tool" | "not_found" => StatusCode::NOT_FOUND,
            "conflict" => StatusCode::CONFLICT,
            "too_large" => StatusCode::PAYLOAD_TOO_LARGE,
            "protocol" => StatusCode::UPGRADE_REQUIRED,
            "locked" | "no_vault" | "unsupported" | "confirm_required" => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0)).into_response()
    }
}

type Answer<T> = Result<T, Failure>;

// ---- the handshake -------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    name: String,
    protocol: u32,
    fingerprint: String,
    locked: bool,
    /// Is there an unused pairing code outstanding? A client that pastes a URL
    /// whose code has expired should be told that rather than "bad code".
    pairing: bool,
}

/// What a client asks before it trusts anything.
///
/// Unauthenticated on purpose: it is the call that finds out whether the two
/// builds can talk at all, and refusing it without a token would mean a version
/// mismatch presenting as an authentication failure.
async fn hello(State((server, _)): Ctxt) -> Json<Hello> {
    Json(Hello {
        name: server.name.clone(),
        protocol: PROTOCOL,
        fingerprint: server.fingerprint.clone(),
        locked: !server.service.get().is_some_and(|v| v.is_unlocked()),
        pairing: server.registry.pairing_open(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    device_name: String,
    #[serde(default)]
    scopes: Vec<Scope>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Paired {
    token: String,
    device_id: String,
    protocol: u32,
}

async fn pair(
    State((server, transport)): Ctxt,
    headers: HeaderMap,
    Json(body): Json<PairRequest>,
) -> Answer<Json<Paired>> {
    check_protocol(&headers)?;
    // Pairing over the local socket would be pointless -- a process that can
    // reach the socket already has everything a token would buy -- and it would
    // be a way to mint a token from a machine that never saw the code.
    if transport == Transport::Socket {
        return Err(CommandError::new("forbidden", "pair over the network, not the socket").into());
    }
    let (token, device_id) = server.registry.pair(&body.code, &body.device_name, body.scopes)?;
    Ok(Json(Paired { token, device_id, protocol: PROTOCOL }))
}

// ---- commands ------------------------------------------------------------

async fn call(
    State((server, transport)): Ctxt,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Answer<Response> {
    check_protocol(&headers)?;
    if body.len() > MAX_JSON_BYTES {
        return Err(CommandError::new("too_large", "that request is too large").into());
    }
    let mut ctx = authenticate(&server, transport, &headers)?;
    ctx.request_id = header_str(&headers, "x-everyday-request");

    let args: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| CommandError::new("invalid", format!("{name}: {e}")))?
    };

    // Deriving a key is the one thing whose *cost* is the attack. Each
    // attempt burns 64 MiB of Argon2 by design, so they run one at a time and
    // a device that keeps guessing is turned away. See `auth`.
    //
    // Both commands that do it are here. `verify_password` is what a client
    // asks when its own screen was locked and the vault behind it never
    // closed; it opens nothing, but it is the same guess at the same secret
    // for the same price, so it goes through the same gate and counts against
    // the same lockout. A screen that was cheaper to guess at than a vault
    // would simply become the way in.
    if name == "unlock" || name == "verify_password" {
        if !server.allow_remote_unlock() && transport == Transport::Network {
            return Err(CommandError::new(
                "forbidden",
                "this vault must be unlocked on the machine holding it",
            )
            .into());
        }
        let gate = server.registry.unlock_gate(&ctx.caller).await?;
        let outcome = server.service.call(ctx, &name, args).await;
        gate.record(outcome.is_ok());
        return Ok(Json(outcome?).into_response());
    }

    let _permit = server.in_flight.clone().acquire_owned().await.ok();
    let value = server.service.call(ctx, &name, args).await?;
    Ok(Json(value).into_response())
}

/// The one command that answers with a stream.
///
/// Newline-delimited JSON rather than server-sent events, and the difference
/// matters: this is the *response to one request*, not a subscription. A client
/// reads lines until the body ends, and the body ending is how it knows the turn
/// is over -- no terminal event to miss, no reconnection to get wrong.
///
/// Kept off `/v1/call` deliberately. A route that sometimes answered with a
/// value and sometimes with a stream would oblige every client to look at the
/// content type before it knew how to read the body.
async fn stream(
    State((server, transport)): Ctxt,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Answer<Response> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    if name != "send_message" {
        return Err(CommandError::new(
            "unknown_command",
            format!("{name} does not answer with a stream"),
        )
        .into());
    }
    let args: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| CommandError::new("invalid", format!("{name}: {e}")))?
    };

    // A generous buffer, and a lossy one by construction: if a client stops
    // reading, the sender fills up and the turn's own progress is what slows.
    // That is the right way round -- the vault's work should never be dropped
    // to keep a stream tidy.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let service = server.service.clone();
    tokio::spawn(async move {
        let sender = tx.clone();
        let sink: everyday_service::agent::Sink = Arc::new(move |event| {
            let _ = sender.send(serde_json::to_value(event).unwrap_or(Value::Null));
        });
        if let Err(e) = service.send_message(ctx, args, sink).await {
            // The turn failed before the harness could say so itself. The
            // client is drawing a spinner, so the last thing down the pipe has
            // to be terminal whatever happened.
            let _ = tx.send(json!({ "type": "failed", "message": e.message }));
        }
    });

    let lines = futures::stream::unfold(rx, |mut rx| async move {
        let value = rx.recv().await?;
        let mut line = serde_json::to_vec(&value).unwrap_or_default();
        line.push(b'\n');
        Some((Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(line)), rx))
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(axum::body::Body::from_stream(lines))
        .map_err(|e| CommandError::new("internal", e.to_string()).into())
}

// ---- attachments ---------------------------------------------------------

async fn put_blob(
    State((server, transport)): Ctxt,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Answer<Json<String>> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    Ok(Json(server.service.put_blob(&ctx, body.to_vec()).await?))
}

#[derive(Deserialize)]
struct BlobQuery {}

/// Serve part of an attachment, honouring `Range`.
///
/// The same arithmetic the desktop's `everyday://` handler uses, from the same
/// module, so a video seeks over a network exactly as it seeks off a disk: two
/// chunks decrypted rather than a whole file downloaded.
async fn get_blob(
    State((server, transport)): Ctxt,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(_q): Query<BlobQuery>,
) -> Answer<Response> {
    check_protocol(&headers)?;
    let _ctx = authenticate(&server, transport, &headers)?;
    let id = BlobId::parse(&id).map_err(|_| CommandError::new("invalid", "not a blob address"))?;

    let total = server.service.blob_len(id)?;
    let plan = everyday_vault::media::plan(
        total,
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
    );
    let bytes = server.service.blob_range(id, plan.start, plan.len)?;

    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, everyday_vault::media::sniff_mime(&bytes))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        // Blobs are content-addressed, so the bytes behind a URL can never
        // change and a client may cache them for ever.
        .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable");
    response = if plan.partial {
        response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, plan.content_range())
    } else {
        response.status(StatusCode::OK)
    };
    response
        .body(axum::body::Body::from(bytes))
        .map_err(|e| CommandError::new("internal", e.to_string()).into())
}

// ---- events --------------------------------------------------------------

/// Everything the service says, as it says it.
///
/// One stream per connected client. A client is told about writes it did not
/// make -- the origin is compared here rather than in the interface, so a
/// window never reloads under its own cursor.
async fn events(
    State((server, transport)): Ctxt,
    headers: HeaderMap,
) -> Answer<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + use<>>> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    let me = ctx.caller.origin().unwrap_or("").to_string();
    Ok(server.broadcaster.stream(me))
}

// ---- the parts every route shares ----------------------------------------

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

/// Refuse a client built against a different command table.
///
/// Named, not guessed: the message carries both numbers, because "update the
/// app" is only useful advice if it says which side is behind.
fn check_protocol(headers: &HeaderMap) -> Result<(), Failure> {
    let Some(raw) = header_str(headers, "x-everyday-protocol") else {
        return Err(CommandError::new(
            "protocol",
            format!(
                "this request did not say which protocol it speaks; this server speaks {PROTOCOL}"
            ),
        )
        .into());
    };
    match raw.parse::<u32>() {
        Ok(n) if n == PROTOCOL => Ok(()),
        Ok(n) => Err(CommandError::new(
            "protocol",
            format!(
                "that copy of Every Day speaks protocol {n} and this one speaks {PROTOCOL}; \
                 update the older of the two"
            ),
        )
        .into()),
        Err(_) => Err(CommandError::new("protocol", "the protocol header was not a number").into()),
    }
}

/// Who is calling.
///
/// Over the socket, nobody had to prove anything: the peer is a process
/// belonging to this user, which is a stronger statement than any token. Over
/// the network, a bearer token is required and is what carries the scopes.
fn authenticate(
    server: &Arc<Server>,
    transport: Transport,
    headers: &HeaderMap,
) -> Result<Ctx, Failure> {
    match transport {
        Transport::Socket => Ok(Ctx {
            caller: Caller::Socket,
            scopes: vec![Scope::All],
            proved_at: None,
            request_id: None,
        }),
        Transport::Network => {
            let token = header_str(headers, "authorization")
                .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
                .ok_or_else(|| {
                    CommandError::new("unauthorized", "this request carried no device token")
                })?;
            Ok(server.registry.authenticate(&token)?)
        }
    }
}

/// The connection's peer, for logs. Not used for authentication -- see
/// [`authenticate`] -- because an address is not an identity.
#[allow(dead_code)]
pub fn peer(request: &Request) -> Option<String> {
    request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.to_string())
}

/// The JSON a client sees when something is refused before a command ran.
#[allow(dead_code)]
pub fn refusal(code: &str, message: &str) -> Value {
    json!({ "code": code, "message": message })
}
