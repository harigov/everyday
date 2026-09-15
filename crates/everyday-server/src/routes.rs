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
use everyday_core::id::MailMessageId;
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
        .route("/v1/mail/body/{id}", get(get_mail_body))
        .route("/v1/mail/part/{message_id}/{identifier}", get(get_mail_part))
        .route("/v1/mail/img/{message_id}/{token}", get(get_mail_image))
        // The image address's first shape, with the message id in a query
        // parameter instead of the path -- kept so a body sanitised before
        // `everyday-mail::sanitize` started writing the new shape still
        // resolves. See `get_mail_image_legacy`'s own docs.
        .route("/v1/mail/img/{token}", get(get_mail_image_legacy))
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
        use everyday_service::error::codes;
        let status = match self.0.code.as_str() {
            // This server's own codes, from `auth.rs` and `client.rs` --
            // `bad_password` joins `unauthorized` here because both say the
            // same thing about a credential: it did not check out.
            "unauthorized" | codes::BAD_PASSWORD => StatusCode::UNAUTHORIZED,
            codes::FORBIDDEN => StatusCode::FORBIDDEN,
            // Concurrency limits, not authentication: `busy` is `everyday-service`'s
            // own transfer-slot ceiling, the same shape as a pairing code's
            // attempt limit. `rate_limited` joins them: the assistant or an
            // MCP client asked for more mail ops than its per-turn or
            // per-minute budget allows, the same "slow down" answer as the
            // other two.
            "too_many_attempts" | codes::BUSY | codes::RATE_LIMITED => {
                StatusCode::TOO_MANY_REQUESTS
            }
            // Malformed input, whether the shape came from JSON that would
            // not deserialise or from a vault descriptor naming a backend or
            // a cipher this build has never heard of.
            // `invalid_client` joins them: a wrong client id or secret is a
            // malformed request in the same sense a bad backend name is --
            // the fix is the caller's, not a retry.
            "bad_code"
            | codes::INVALID
            | codes::UNKNOWN_BACKEND
            | codes::UNKNOWN_CIPHER
            | codes::INVALID_CLIENT => StatusCode::BAD_REQUEST,
            codes::UNKNOWN_COMMAND | codes::UNKNOWN_TOOL | codes::NOT_FOUND => {
                StatusCode::NOT_FOUND
            }
            // `already_initialised` is a conflict for the same reason a
            // stale save is: the thing this call assumed did not exist,
            // does. `retry` joins it because the answer is the same one a
            // conflict gets -- ask again, deliberately, with a fresh id.
            codes::CONFLICT | codes::ALREADY_INITIALISED | codes::RETRY => StatusCode::CONFLICT,
            codes::TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
            // A version older or newer than this build understands.
            // `unsupported_version` is a vault's on-disk format saying the
            // same thing `protocol` says about the wire: upgrade the build
            // rather than retry the call.
            "protocol" | codes::UNSUPPORTED_VERSION => StatusCode::UPGRADE_REQUIRED,
            // Understood, but this vault or this input cannot honour it --
            // `not_an_image` is the same shape as `unsupported`: a caller
            // that gave a well-formed request pointed at the wrong thing.
            // `invalid_grant` says the same thing about an account that
            // `locked` says about a vault: understood, cannot be honoured
            // as it stands, and there is a specific known step -- sign in
            // again -- that fixes it.
            codes::LOCKED
            | codes::NO_VAULT
            | codes::UNSUPPORTED
            | codes::CONFIRM_REQUIRED
            | codes::NOT_AN_IMAGE
            | codes::INVALID_GRANT => StatusCode::UNPROCESSABLE_ENTITY,
            // Another copy of this process holds the write claim. Distinct
            // from `conflict`'s optimistic-concurrency meaning -- nothing
            // about the record changed, the vault itself is spoken for --
            // which is what the WebDAV status name actually means.
            codes::VAULT_IN_USE => StatusCode::LOCKED,
            // A request this server could not carry out because something
            // beyond it did not answer, or answered with something that
            // could not be used: the web, or the model behind the assistant
            // and the quick model, whichever endpoint a person configured.
            // `provider` joins them: the OAuth endpoint on the other end of
            // the socket answered with something other than a token or one
            // of the two named failures above, which is exactly the shape
            // of "something beyond this server did not behave".
            codes::NETWORK | codes::AGENT | codes::QUICK | codes::UNREADABLE | codes::PROVIDER => {
                StatusCode::BAD_GATEWAY
            }
            // Nobody's browser came back inside the sign-in's own window.
            codes::TIMED_OUT => StatusCode::REQUEST_TIMEOUT,
            // The flow this call named was withdrawn -- `cancel_oauth_sign_in`
            // -- and, unlike `not_found`, once existed and will not answer
            // again under the same id.
            codes::CANCELLED => StatusCode::GONE,
            // Everything left is this side's own failure to make sense of
            // its own data or finish its own work: `decrypt_failed` is
            // ciphertext that does not check out against the key that
            // opened it, which is a corruption question rather than a
            // caller's mistake; `internal`, `panic`, `io`, `serde` and
            // `backend` are this process failing at something with no
            // caller-facing shape at all.
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
        // ...but they are not the same *policy*. `--no-remote-unlock` says a
        // sealed vault is opened at the machine holding it and nowhere else.
        // Applied to `verify_password` as well, it locked people out of a
        // vault that was already open: a remote window whose own screen timed
        // out could never answer for itself again, and there was nothing to do
        // at the host either, because the vault it holds was never locked.
        //
        // So the refusal is `unlock`'s, plus the one case where verifying
        // would be a way around it -- a sealed vault, where an answer of "yes,
        // that is the password" is exactly the thing the flag is keeping on
        // the other machine. With the vault open there is no such answer to
        // buy: the caller is already holding a token that reads it.
        let sealed = server.service.get().is_none_or(|v| !v.is_unlocked());
        if !server.allow_remote_unlock()
            && transport == Transport::Network
            && (name == "unlock" || sealed)
        {
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
    let plan =
        everyday_core::media::plan(total, headers.get(header::RANGE).and_then(|v| v.to_str().ok()));
    let bytes = server.service.blob_range(id, plan.start, plan.len)?;

    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, everyday_core::media::sniff_mime(&bytes))
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

// ---- mail: a rendered body, a part, a remote image -----------------------
//
// The same three things `everyday-app/src/protocol.rs`'s `everyday://mail/…`
// routes answer, over HTTP instead -- both transports call straight into
// `everyday_service::mailview`, which is where the actual logic (and its
// tests) live. See that module's docs for why none of this is a JSON
// command: a rendered body is bytes, not a result `Service::call` returns.
//
// Unlike `get_blob`, which is content-addressed and open to anything a
// paired device can reach, these three check `Scope::Mail` explicitly: a
// `MailMessageId` is not an opaque hash, and mail is the one domain whose
// contents are written by strangers -- see `ctx::Scope::Mail`'s own docs.

async fn get_mail_body(
    State((server, transport)): Ctxt,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer<Response> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    ctx.require(Scope::Mail)?;
    let id =
        MailMessageId::parse(&id).map_err(|_| CommandError::new("invalid", "not a message id"))?;

    let vault = server.service.require()?;
    let one_off = server.service.remote_images_allowed_once(id);
    let allow_remote = everyday_service::mailview::remote_images_allowed(&vault, id, one_off)?;
    let doc = everyday_service::mailview::body_document(&vault, id, allow_remote)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        // A rendered body is decided fresh every time -- whether images are
        // hidden can change between two requests for the same id -- so it
        // must never be believed from a cache. See `protocol.rs`'s own
        // `mail/body` route for the identical header.
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        // Read by the interface's `fetch()` of this address -- never by
        // anything inside the sandboxed frame itself -- to show "images
        // hidden" without parsing the document. See
        // `ui/src/lib/mailview.ts`.
        .header("x-mail-images-hidden", doc.images_hidden.to_string())
        .body(axum::body::Body::from(doc.html))
        .map_err(|e| CommandError::new("internal", e.to_string()).into())
}

async fn get_mail_part(
    State((server, transport)): Ctxt,
    Path((message_id, identifier)): Path<(String, String)>,
    headers: HeaderMap,
) -> Answer<Response> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    ctx.require(Scope::Mail)?;
    let message_id = MailMessageId::parse(&message_id)
        .map_err(|_| CommandError::new("invalid", "not a message id"))?;

    let vault = server.service.require()?;
    let served = everyday_service::mailview::part(&vault, message_id, &identifier)?;
    mail_bytes_response(served, "private, max-age=31536000, immutable")
}

/// `/v1/mail/img/{message_id}/{token}` -- the shape
/// `everyday-mail::sanitize::sanitize` writes into a message's HTML today.
/// The message id comes first, matching `/v1/mail/part/{message_id}/{identifier}`'s
/// own order, so both routes share one shape.
async fn get_mail_image(
    State((server, transport)): Ctxt,
    Path((message_id, token)): Path<(String, String)>,
    headers: HeaderMap,
) -> Answer<Response> {
    mail_image_response(&server, transport, &headers, &message_id, &token).await
}

#[derive(Deserialize)]
struct MailImageQuery {
    /// The message this image belongs to -- `everyday://mail/img/{token}?m={msg}`'s
    /// own query parameter, named to match.
    m: String,
}

/// `/v1/mail/img/{token}?m={message_id}` -- the image address's first
/// shape, with the message id in a query parameter rather than the path.
/// Still answered, and will be for as long as a vault might hold a body
/// sanitised before `sanitize::sanitize` started writing the message id
/// into the path: a stored body is sealed at sync time and never
/// re-sanitised to migrate it to the new shape, so a route that stopped
/// understanding the old one would break every remote image in a message
/// synced before this change. See `crates/everyday-app/src/protocol.rs`'s
/// identical fallback for the desktop transport.
async fn get_mail_image_legacy(
    State((server, transport)): Ctxt,
    Path(token): Path<String>,
    headers: HeaderMap,
    Query(q): Query<MailImageQuery>,
) -> Answer<Response> {
    mail_image_response(&server, transport, &headers, &q.m, &token).await
}

/// What both `get_mail_image` and `get_mail_image_legacy` do once they have
/// a message id and a token, whichever shape of address it arrived in.
async fn mail_image_response(
    server: &Arc<Server>,
    transport: Transport,
    headers: &HeaderMap,
    message_id: &str,
    token: &str,
) -> Answer<Response> {
    check_protocol(headers)?;
    let ctx = authenticate(server, transport, headers)?;
    ctx.require(Scope::Mail)?;
    let message_id = MailMessageId::parse(message_id)
        .map_err(|_| CommandError::new("invalid", "not a message id"))?;

    let vault = server.service.require()?;
    let one_off = server.service.remote_images_allowed_once(message_id);
    let client = everyday_service::http::public_client()?;
    let served =
        everyday_service::mailview::remote_image(&vault, client, message_id, token, one_off)
            .await?;
    // Never cached: a placeholder answered before permission was granted
    // must not shadow the real picture once it is.
    mail_bytes_response(served, "no-store")
}

fn mail_bytes_response(
    served: everyday_service::mailview::PartResponse,
    cache_control: &str,
) -> Answer<Response> {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, served.content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .header("x-content-type-options", "nosniff");
    if served.attachment {
        let filename = served.filename.as_deref().unwrap_or("attachment");
        response = response.header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename.replace('"', "'")),
        );
    }
    response
        .body(axum::body::Body::from(served.bytes))
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

#[cfg(test)]
mod status_tests {
    use super::*;
    use everyday_service::error::codes;

    fn status_of(code: &str) -> StatusCode {
        Failure(CommandError::new(code, "test")).into_response().status()
    }

    /// Every code this server's own `CommandError::new` calls can carry --
    /// see `auth.rs` and `client.rs` -- mapped somewhere other than the
    /// default 500, so a status new to this list is a choice rather than an
    /// accident.
    #[test]
    fn this_servers_own_codes_are_covered() {
        assert_eq!(status_of("unauthorized"), StatusCode::UNAUTHORIZED);
        assert_eq!(status_of("too_many_attempts"), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(status_of("bad_code"), StatusCode::BAD_REQUEST);
        assert_eq!(status_of("protocol"), StatusCode::UPGRADE_REQUIRED);
    }

    /// Every code `everyday-service` can put on a `CommandError` -- every
    /// literal inside that crate, and everything `everyday_core::Error::code`
    /// produces -- maps to a status somebody chose on purpose. Before
    /// `codes::ALL` existed, seven of these fell through to the default 500
    /// because nothing here had been taught the word: `bad_password`,
    /// `vault_in_use`, `decrypt_failed`, `unsupported_version`, `network`,
    /// `quick` and `agent`. This is what stops an eighth arriving the same
    /// way -- a code missing from this match is still answered, just not
    /// with the status this test was told to expect, so a new one added to
    /// `codes::ALL` without a line here fails this rather than shipping
    /// silently as a 500.
    #[test]
    fn every_code_this_server_can_receive_maps_to_a_deliberate_status() {
        let expected: &[(&str, StatusCode)] = &[
            (codes::LOCKED, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::BAD_PASSWORD, StatusCode::UNAUTHORIZED),
            (codes::ALREADY_INITIALISED, StatusCode::CONFLICT),
            (codes::NO_VAULT, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::UNSUPPORTED_VERSION, StatusCode::UPGRADE_REQUIRED),
            (codes::UNKNOWN_BACKEND, StatusCode::BAD_REQUEST),
            (codes::UNKNOWN_CIPHER, StatusCode::BAD_REQUEST),
            (codes::NOT_FOUND, StatusCode::NOT_FOUND),
            (codes::DECRYPT_FAILED, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::UNSUPPORTED, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::VAULT_IN_USE, StatusCode::LOCKED),
            (codes::CONFLICT, StatusCode::CONFLICT),
            (codes::INVALID, StatusCode::BAD_REQUEST),
            (codes::IO, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::SERDE, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::BACKEND, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::AGENT, StatusCode::BAD_GATEWAY),
            (codes::BUSY, StatusCode::TOO_MANY_REQUESTS),
            (codes::CONFIRM_REQUIRED, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::FORBIDDEN, StatusCode::FORBIDDEN),
            (codes::INTERNAL, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::NETWORK, StatusCode::BAD_GATEWAY),
            (codes::NOT_AN_IMAGE, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::PANIC, StatusCode::INTERNAL_SERVER_ERROR),
            (codes::QUICK, StatusCode::BAD_GATEWAY),
            (codes::RATE_LIMITED, StatusCode::TOO_MANY_REQUESTS),
            (codes::RETRY, StatusCode::CONFLICT),
            (codes::TOO_LARGE, StatusCode::PAYLOAD_TOO_LARGE),
            (codes::UNKNOWN_COMMAND, StatusCode::NOT_FOUND),
            (codes::UNKNOWN_TOOL, StatusCode::NOT_FOUND),
            (codes::UNREADABLE, StatusCode::BAD_GATEWAY),
            (codes::INVALID_GRANT, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::INVALID_CLIENT, StatusCode::BAD_REQUEST),
            (codes::PROVIDER, StatusCode::BAD_GATEWAY),
            (codes::TIMED_OUT, StatusCode::REQUEST_TIMEOUT),
            (codes::CANCELLED, StatusCode::GONE),
        ];
        // Every constant is in the table above, and the table has nothing
        // beyond the constants -- so a code added to `codes::ALL` without a
        // line here is caught, and so is a line here for a code that no
        // longer exists.
        assert_eq!(
            expected.len(),
            codes::ALL.len(),
            "this table and `codes::ALL` have drifted apart"
        );
        for code in codes::ALL {
            let (_, want) = expected
                .iter()
                .find(|(c, _)| c == code)
                .unwrap_or_else(|| panic!("{code} is in `codes::ALL` but not in this table"));
            assert_eq!(status_of(code), *want, "{code} did not map to the status this expects");
        }
    }
}

/// The image route, driven through the real axum [`Router`] with
/// [`tower::ServiceExt::oneshot`] rather than called as a plain function --
/// what a routing bug (a path shape that does not match the route table, an
/// extractor reading the wrong segment) would actually break. `Transport::Socket`
/// needs no token, which is what lets this drive the router directly rather
/// than standing up the TLS harness `tests/serve.rs` uses for the rest of
/// this crate's HTTP-level tests -- see `authenticate`'s own docs for why a
/// socket transport asks for none.
#[cfg(test)]
mod mail_route_tests {
    use super::*;
    use everyday_core::VaultConfig;
    use everyday_core::id::{AccountId, PackId, ThreadId};
    use everyday_core::mail::{
        Address, Body, Mailbox, MailboxRole, Message, MessageFlags, RemoteImage,
    };
    use everyday_core::packstore::PackRef;
    use everyday_core::store::mail::IngestMessage;
    use tower::ServiceExt;

    /// A vault with one message from `sender`, and a `Body` sealed from a
    /// real [`everyday_mail::sanitize::sanitize`] call -- so the address
    /// this test drives a request against is exactly what the sync engine
    /// would have written, not a hand-built stand-in for it.
    fn seeded(
        sender: &str,
        html: &str,
    ) -> (tempfile::TempDir, Arc<Service>, MailMessageId, String) {
        let dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(dir.path(), VaultConfig::default()).unwrap();

        let account = AccountId::new();
        let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();

        let message_id = MailMessageId::new();
        let message = Message {
            id: message_id,
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{message_id}@routes.example>"),
            date: jiff::Timestamp::now(),
            from: Address::bare(sender),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "hi".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            invite: None,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
        };
        vault
            .ingest_mail(account, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
            .unwrap();

        let out = everyday_mail::sanitize::sanitize(
            html,
            &everyday_mail::sanitize::Rewrite::new(message_id.to_string()),
        );
        let body = Body {
            message_id,
            html_sanitised: out.html.clone(),
            text: String::new(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: out
                .remote_images
                .iter()
                .map(|r| RemoteImage {
                    original_url: r.original_url.clone(),
                    token: r.token.clone(),
                    cached_blob: None,
                })
                .collect(),
        };
        vault.save_body(&body).unwrap();

        let service = Arc::new(Service::new());
        service.set(vault);
        (dir, service, message_id, out.html)
    }

    /// Also hands back the registry's temporary directory: it has to outlive
    /// the router, even though nothing in these tests touches it again
    /// after `Registry::open` has read it.
    fn socket_router(service: Arc<Service>) -> (Router, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let broadcaster = Arc::new(Broadcaster::new());
        let server =
            Server::new(service, registry, broadcaster, "Test".into(), String::new(), false);
        (router(server, Transport::Socket), config_dir)
    }

    /// Pulls the first `everyday://...` address out of a sanitised
    /// document's `src="..."` and turns it into the path
    /// `everyday-server`'s own routes answer -- `everyday://mail/...`
    /// becomes `/v1/mail/...`, the same address family under a different
    /// transport. See `everyday-app/src/protocol.rs`'s own tests for the
    /// desktop half of this.
    fn request_path_from(html: &str) -> String {
        let start = html.find("src=\"everyday://mail").expect("a rewritten src") + "src=\"".len();
        let end = html[start..].find('"').expect("a closing quote") + start;
        format!("/v1/mail{}", &html[start..end]["everyday://mail".len()..])
    }

    async fn get(router: Router, path: &str) -> Response {
        router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-everyday-protocol", PROTOCOL.to_string())
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_new_path_shaped_image_route_serves_the_placeholder_for_an_unlisted_sender() {
        let (_dir, service, _id, html) =
            seeded("news@marketing.example", r#"<img src="https://cdn.example.com/logo.png">"#);
        let path = request_path_from(&html);
        assert!(!path.contains("?m="), "{path}");

        let (router, _config_dir) = socket_router(service);
        let response = get(router, &path).await;
        // The sender is not on the allow-list, so this is the placeholder
        // pixel, not a fetch -- what matters here is the 200: a route that
        // mis-parsed the message id or the token would have answered
        // `not_found` instead, since `remote_images_allowed` could not have
        // found the message or `remote_image` could not have found the
        // token.
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
    }

    /// The image address's first shape -- the message id in `?m=`, not the
    /// path -- still resolves: a body sanitised before that shape existed
    /// is sealed in the vault exactly as it was, and this route is what
    /// keeps its remote images working. See `routes.rs`'s own docs on
    /// `get_mail_image_legacy`.
    #[tokio::test]
    async fn the_old_query_parameter_shaped_image_route_still_resolves() {
        let (_dir, service, id, _html) =
            seeded("news@marketing.example", r#"<img src="https://cdn.example.com/logo.png">"#);
        // Reconstructed by hand into the address's first shape -- this is
        // exactly what a body sanitised under an older build still carries,
        // which is the case this test exists to keep working.
        let path = format!("/v1/mail/img/sometoken?m={id}");

        let (router, _config_dir) = socket_router(service);
        let response = get(router, &path).await;
        // Same answer as the new shape above, for the same reason: the
        // sender is not on the allow-list, so this is the placeholder --
        // what matters is that it is 200 at all, which only happens once
        // `q.m` has been read and `MailMessageId::parse`d into the message
        // this vault actually holds.
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
    }
}
