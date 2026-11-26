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
//! `X-KarmaYoga-Protocol` always; a mismatch is refused rather than attempted.
//! `X-KarmaYoga-Request` on a write, which is what makes a retry safe.
//!
//! The two screen-time routes are the exception: a heartbeats token and
//! nothing else, because the collectors that call them are not built from
//! this workspace. See `crate::screentime`.

use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use karmayoga_core::BlobId;
use karmayoga_core::id::{FileId, MailMessageId, StoryId};
use karmayoga_service::ctx::{Caller, Ctx, Scope};
use karmayoga_service::error::CommandError;
use karmayoga_service::{PROTOCOL, Service};
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

/// Most a body may be buffered to before anybody has proved who they are.
///
/// The router used to set its default limit to the attachment ceiling, half a
/// gigabyte, and `call`, `stream` and `put_blob` took `Bytes` -- which axum
/// buffers before a handler runs -- so anybody who could reach the port could
/// make the server hold that much per request without ever showing a token.
/// Those three now take the raw body and read it themselves once the caller
/// is known (see [`read_body`]). The one body still read beforehand is a
/// pairing request: a code, a device name and a scope list.
const UNAUTHENTICATED_BODY_BYTES: usize = 64 * 1024;

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
    /// The browsers signed in, for a server answering `Transport::Web`.
    /// `None` everywhere else, where nothing could be signed in.
    sessions: Option<Arc<crate::web::session::Sessions>>,
    /// Set when the listener answering for this server stops (`start`): what
    /// a connection that outlives its request -- a voice session -- watches,
    /// since the listener's graceful shutdown does not reach an upgraded one.
    pub(crate) closing: tokio::sync::watch::Sender<bool>,
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
            sessions: None,
            closing: tokio::sync::watch::channel(false).0,
        })
    }

    /// The server behind web access: the same routes, answering signed-in
    /// browsers from `sessions` instead of paired devices from tokens. No
    /// fingerprint -- a browser pins nothing -- and remote unlock is web
    /// access's own setting, decided at its login, not here.
    pub fn for_web(
        service: Arc<Service>,
        registry: Arc<Registry>,
        broadcaster: Arc<Broadcaster>,
        name: String,
        sessions: Arc<crate::web::session::Sessions>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            registry,
            broadcaster,
            name,
            fingerprint: String::new(),
            allow_remote_unlock: std::sync::atomic::AtomicBool::new(false),
            in_flight: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT * 4)),
            sessions: Some(sessions),
            closing: tokio::sync::watch::channel(false).0,
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
    /// A browser, through web access's gatekeeper -- see
    /// `crate::web::routes`. The session it found is in two headers only
    /// the gatekeeper sets.
    Web,
}

pub fn router(server: Arc<Server>, transport: Transport) -> Router {
    use axum::extract::DefaultBodyLimit;
    let mut router = Router::new();
    // A browser does not pair, has its own handshake (`/web/hello`) and its
    // own event stream (`/web/events`, which ends with the session), and is
    // not a screen-time collector. Everything else is the same for it.
    if transport != Transport::Web {
        router = router
            .route("/v1/hello", get(hello))
            // Its own limit, not only the router's below, so that widening the
            // default for some later route can never widen what a stranger with
            // no token may make this server buffer.
            .route("/v1/pair", post(pair).layer(DefaultBodyLimit::max(UNAUTHENTICATED_BODY_BYTES)))
            .route("/v1/events", get(events))
            // The two routes collectors speak to, for a machine with no copy of
            // this application running: a heartbeats token, no protocol header,
            // and a body that must name its device. Their handlers are shared
            // with the shell's loopback listener -- see `crate::screentime`.
            .route("/v1/screentime/hello", get(crate::screentime::shared_hello))
            .route("/v1/screentime/heartbeats", post(crate::screentime::shared_heartbeats));
    }
    router
        // The phone's Health Auto Export pushes, on every transport -- web
        // access is the one a phone can reach. Bearer-token authenticated
        // even there; see `crate::health`.
        .route("/v1/health/hae/{source}", post(crate::health::push))
        .route("/v1/call/{name}", post(call))
        .route("/v1/stream/{name}", post(stream))
        // Talking to the assistant from another device: see `crate::voice`.
        .route(crate::voice::ROUTE, get(voice))
        .route("/v1/blob", post(put_blob))
        .route("/v1/blob/{id}", get(get_blob))
        .route("/v1/mail/body/{id}", get(get_mail_body))
        .route("/v1/mail/part/{message_id}/{identifier}", get(get_mail_part))
        .route("/v1/mail/img/{message_id}/{token}", get(get_mail_image))
        // The image address's first shape, with the message id in a query
        // parameter instead of the path -- kept so a body sanitised before
        // `karmayoga-mail::sanitize` started writing the new shape still
        // resolves. See `get_mail_image_legacy`'s own docs.
        .route("/v1/mail/img/{token}", get(get_mail_image_legacy))
        .route("/v1/news/body/{story}", get(get_news_body))
        .route("/v1/news/img/{story}/{token}", get(get_news_image))
        // A file in the files catalog, with ranges, and one chunk of an upload
        // to it. The chunk's body is raw bytes read by the handler after
        // `authenticate`, like `put_blob`'s, so no default limit governs it.
        .route("/v1/files/file/{id}", get(get_file))
        .route("/v1/files/upload/{upload}", post(upload_file_chunk))
        // What an extractor that buffers -- `Json`, `Bytes` -- may take when a
        // route says nothing. It does not govern a raw `Body`, which is why
        // `call`, `stream` and `put_blob` can still take up to their own
        // limits: they read through `read_body`, after `authenticate`.
        .layer(DefaultBodyLimit::max(UNAUTHENTICATED_BODY_BYTES))
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
        (status_for(&self.0.code), Json(self.0)).into_response()
    }
}

/// The status a command error's code is answered with.
///
/// Its own function rather than the body of [`Failure`]'s `into_response`
/// so the screen-time ingest routes (see `crate::screentime`) answer every
/// code the way every other route does, and differ only where their
/// contract says so.
pub(crate) fn status_for(code: &str) -> StatusCode {
    use karmayoga_service::error::codes;
    match code {
        // This server's own codes, from `auth.rs` and `client.rs` --
        // `bad_password` joins `unauthorized` here because both say the
        // same thing about a credential: it did not check out.
        "unauthorized" | codes::BAD_PASSWORD => StatusCode::UNAUTHORIZED,
        codes::FORBIDDEN => StatusCode::FORBIDDEN,
        // Concurrency limits, not authentication: `busy` is `karmayoga-service`'s
        // own transfer-slot ceiling, the same shape as a pairing code's
        // attempt limit. `rate_limited` joins them: the assistant or an
        // MCP client asked for more mail ops than its per-turn or
        // per-minute budget allows, the same "slow down" answer as the
        // other two.
        "too_many_attempts" | codes::BUSY | codes::RATE_LIMITED => StatusCode::TOO_MANY_REQUESTS,
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
        codes::UNKNOWN_COMMAND | codes::UNKNOWN_TOOL | codes::NOT_FOUND => StatusCode::NOT_FOUND,
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
        // `no_location` says the same thing about `weather` (the
        // command) that `invalid_grant` says about an account: a
        // specific, known step -- adding a location in Settings → About
        // You -- fixes it.
        codes::LOCKED
        | codes::NO_VAULT
        | codes::UNSUPPORTED
        | codes::CONFIRM_REQUIRED
        | codes::NOT_AN_IMAGE
        | codes::NO_LOCATION
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
        codes::NETWORK
        | codes::UNREACHABLE
        | codes::AGENT
        | codes::QUICK
        | codes::UNREADABLE
        | codes::PROVIDER => StatusCode::BAD_GATEWAY,
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
    body: axum::body::Body,
) -> Answer<Response> {
    check_protocol(&headers)?;
    let mut ctx = authenticate(&server, transport, &headers)?;
    ctx.request_id = header_str(&headers, "x-karmayoga-request");
    let body = read_body(body, MAX_JSON_BYTES).await?;

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
    // Every command that does it is here -- see `derives_a_key` for the list
    // and why each is on it. Whatever else they do, each is a guess at the
    // same secret for the same price, so each goes through the same gate and
    // counts against the same lockout. A command that was cheaper to guess
    // through than `unlock` would simply become the way in.
    if derives_a_key(&name) {
        // ...but they are not the same *policy*. `--no-remote-unlock` says a
        // sealed vault is opened at the machine holding it and nowhere else.
        // Applied to `verify_password` as well, it locked people out of a
        // vault that was already open: a remote window whose own screen timed
        // out could never answer for itself again, and there was nothing to do
        // at the host either, because the vault it holds was never locked.
        //
        // So the refusal is `unlock`'s, plus the one case where the others
        // would be a way around it -- a sealed vault, where an answer of "yes,
        // that is the password" is exactly the thing the flag is keeping on
        // the other machine. `change_password` gives that answer too, and
        // replaces the password on a right guess besides. With the vault open
        // there is no such answer to buy: the caller is already holding a
        // token that reads it.
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
        // `change_password` can fail before it has guessed at anything -- a
        // new password too short to accept, a vault opened read-only -- and
        // somebody who mistyped the *new* password has not guessed at the old
        // one. Only a verdict on `current` counts for or against them.
        let not_a_guess = name == "change_password"
            && outcome
                .as_ref()
                .is_err_and(|e| e.code != karmayoga_service::error::codes::BAD_PASSWORD);
        if !not_a_guess {
            gate.record(outcome.is_ok());
        }
        return Ok(Json(outcome?).into_response());
    }

    let _permit = server.in_flight.clone().acquire_owned().await.ok();
    let value = server.service.call(ctx, &name, args).await?;
    Ok(Json(value).into_response())
}

/// The commands that derive a key from a password the caller sent, and so go
/// through [`Registry::unlock_gate`].
///
/// One list, so the gate and the refusal in [`call`] cannot disagree about it.
/// `unlock` opens the vault. `verify_password` opens nothing -- it is what a
/// client asks when its own screen locked and the vault behind it never
/// closed -- but costs the same derivation. `change_password` derives a key
/// from `current` to unwrap the vault's own, whether or not the vault is open,
/// which made it an ungated way to ask "is this the password?" that also
/// replaced the password on a right answer.
fn derives_a_key(name: &str) -> bool {
    matches!(name, "unlock" | "verify_password" | "change_password")
}

/// Read a request's body, refusing it once it passes `limit`.
///
/// For the three routes that take a body worth having -- `call`, `stream` and
/// `put_blob` -- and only ever called after [`authenticate`], which is the
/// point: see [`UNAUTHENTICATED_BODY_BYTES`]. They take the raw `Body`, which
/// no `DefaultBodyLimit` governs, so this is the only limit on it.
///
/// Its own loop rather than `axum::body::to_bytes`, whose error says whether
/// the limit was hit or the client went away only through a type from
/// `http-body-util`, which this crate does not depend on -- and the two want
/// different answers.
///
/// Answers a [`CommandError`] rather than a [`Failure`] so the screen-time
/// ingest routes in `crate::screentime`, which answer some codes their own
/// way, read their bodies through this too; `?` turns it into a `Failure`
/// everywhere else.
pub(crate) async fn read_body(
    body: axum::body::Body,
    limit: usize,
) -> Result<Vec<u8>, CommandError> {
    use futures::StreamExt;
    let too_large = || CommandError::new("too_large", "that request is too large");
    // A declared length past the limit is refused without reading a byte.
    let declared = axum::body::HttpBody::size_hint(&body).lower();
    if declared > limit as u64 {
        return Err(too_large());
    }
    let mut out = Vec::with_capacity(declared as usize);
    let mut chunks = body.into_data_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|e| {
            CommandError::new("invalid", format!("the request did not arrive whole: {e}"))
        })?;
        // ...and an undeclared one at the chunk that would carry it past.
        if out.len() + chunk.len() > limit {
            return Err(too_large());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// A voice session for a paired device: its microphone in, this computer's
/// models and assistant, its speaker out, over one WebSocket. See
/// `crate::voice`.
///
/// The token and the agent scope are checked before the upgrade, so a device
/// that may not talk to the assistant is refused with the same `{ code,
/// message }` as any command, not a socket that opens and then closes.
async fn voice(
    State((server, transport)): Ctxt,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> Answer<Response> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    ctx.require(Scope::Agent)?;
    let service = server.service.clone();
    let device = match &ctx.caller {
        Caller::Device(id) => Some(id.clone()),
        _ => None,
    };
    Ok(upgrade.on_upgrade(move |socket| async move {
        let (frames, out) = crate::voice::axum_socket(socket);
        let frames = crate::voice::until(frames, disallowed(server, device));
        crate::voice::serve(service, ctx, frames, out).await;
    }))
}

/// Finishes when a voice session stops being allowed: its device revoked, or
/// sharing switched off. The token was looked at once, before the upgrade,
/// and a session lasts as long as somebody keeps talking -- so the device is
/// looked for again every few seconds.
async fn disallowed(server: Arc<Server>, device: Option<String>) {
    let mut closing = server.closing.subscribe();
    let mut every = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = closing.wait_for(|closing| *closing) => return,
            _ = every.tick() => {
                if let Some(id) = &device
                    && !server.registry.holds(id)
                {
                    return;
                }
            }
        }
    }
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
    body: axum::body::Body,
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
    // A turn's arguments are a prompt and references -- pictures arrive
    // through `put_blob` first -- so a command's limit is plenty.
    let body = read_body(body, MAX_JSON_BYTES).await?;
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
        let sink: karmayoga_service::agent::Sink = Arc::new(move |event| {
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
    body: axum::body::Body,
) -> Answer<Json<String>> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    let bytes = read_body(body, Service::MAX_ATTACHMENT_BYTES).await?;
    Ok(Json(server.service.put_blob(&ctx, bytes).await?))
}

#[derive(Deserialize)]
struct BlobQuery {}

/// Does this caller hold only scopes that write and never read?
fn write_only(ctx: &Ctx) -> bool {
    !ctx.scopes.is_empty()
        && ctx.scopes.iter().all(|s| matches!(s, Scope::Heartbeats | Scope::HealthIngest))
}

/// Turn a write-only caller away from a route that only reads.
fn refuse_write_only(ctx: &Ctx) -> Result<(), Failure> {
    if write_only(ctx) {
        return Err(CommandError::new(
            karmayoga_service::error::codes::FORBIDDEN,
            "this token cannot read",
        )
        .into());
    }
    Ok(())
}

/// What `Cache-Control` a response carrying decrypted vault bytes is sent
/// with, given the one `desktop` would use.
///
/// A browser reached over the network or through web access keeps what it is
/// told it may, in a cache on its own disk that outlives the session, the
/// pairing and the vault's lock. A passport scan or a lab report opened on a
/// borrowed laptop would stay there for the year the blob and file routes
/// used to allow, readable by whoever sat down next. So for those two
/// transports nothing the vault decrypted is kept: `no-store`, and no
/// `max-age` or `immutable` beside it, because that is what lets a browser
/// answer without asking again.
///
/// The local socket is the same user's own process -- the command line, not
/// a browser -- and keeps what the route would have said. The desktop's own
/// `karmayoga://` handler does not come through here at all.
fn vault_cache_control(transport: Transport, desktop: &'static str) -> &'static str {
    match transport {
        Transport::Network | Transport::Web => "no-store",
        Transport::Socket => desktop,
    }
}

/// Serve part of an attachment, honouring `Range`.
///
/// The same arithmetic the desktop's `karmayoga://` handler uses, from the same
/// module, so a video seeks over a network exactly as it seeks off a disk: two
/// chunks decrypted rather than a whole file downloaded.
async fn get_blob(
    State((server, transport)): Ctxt,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(_q): Query<BlobQuery>,
) -> Answer<Response> {
    let ctx = authenticate_media(&server, transport, &headers)?;
    // A write-only token -- a collector's heartbeats, a phone's health push
    // -- reads nothing, and the blob store holds every sealed file and what
    // a connector sent for a clinical document as well as pictures. It is
    // the token most likely to leak.
    refuse_write_only(&ctx)?;
    let id = BlobId::parse(&id).map_err(|_| CommandError::new("invalid", "not a blob address"))?;

    let total = server.service.blob_len(id)?;
    let plan = karmayoga_core::media::plan(
        total,
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
    );
    let bytes = server.service.blob_range(id, plan.start, plan.len)?;

    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, karmayoga_core::media::sniff_mime(&bytes))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        // Blobs are content-addressed, so the bytes behind a URL can never
        // change and a client on this machine may cache them for ever -- but
        // see `vault_cache_control` for why one that is a browser may not.
        .header(
            header::CACHE_CONTROL,
            vault_cache_control(transport, "private, max-age=31536000, immutable"),
        );
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
// The same three things `karmayoga-app/src/protocol.rs`'s `karmayoga://mail/…`
// routes answer, over HTTP instead -- both transports call straight into
// `karmayoga_service::mail::view`, which is where the actual logic (and its
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
    let ctx = authenticate_media(&server, transport, &headers)?;
    ctx.require(Scope::Mail)?;
    let id =
        MailMessageId::parse(&id).map_err(|_| CommandError::new("invalid", "not a message id"))?;

    let vault = server.service.require()?;
    let one_off = server.service.remote_images_allowed_once(id);
    let allow_remote = karmayoga_service::mail::view::remote_images_allowed(&vault, id, one_off)?;
    let doc = karmayoga_service::mail::view::body_document(&vault, id, allow_remote)?;

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
    let ctx = authenticate_media(&server, transport, &headers)?;
    ctx.require(Scope::Mail)?;
    let message_id = MailMessageId::parse(&message_id)
        .map_err(|_| CommandError::new("invalid", "not a message id"))?;

    let vault = server.service.require()?;
    let served = karmayoga_service::mail::view::part(&vault, message_id, &identifier)?;
    mail_bytes_response(
        served,
        vault_cache_control(transport, "private, max-age=31536000, immutable"),
    )
}

/// `/v1/mail/img/{message_id}/{token}` -- the shape
/// `karmayoga-mail::sanitize::sanitize` writes into a message's HTML today.
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
    /// The message this image belongs to -- `karmayoga://mail/img/{token}?m={msg}`'s
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
/// synced before this change. See `crates/karmayoga-app/src/protocol.rs`'s
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
    let ctx = authenticate_media(server, transport, headers)?;
    ctx.require(Scope::Mail)?;
    let message_id = MailMessageId::parse(message_id)
        .map_err(|_| CommandError::new("invalid", "not a message id"))?;

    let vault = server.service.require()?;
    let one_off = server.service.remote_images_allowed_once(message_id);
    let client = karmayoga_service::http::public_client()?;
    let served =
        karmayoga_service::mail::view::remote_image(&vault, client, message_id, token, one_off)
            .await?;
    // Never cached: a placeholder answered before permission was granted
    // must not shadow the real picture once it is.
    mail_bytes_response(served, "no-store")
}

fn mail_bytes_response(
    served: karmayoga_service::mail::view::PartResponse,
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

// ---- news: a rendered story, a picture ------------------------------------
//
// The twins of `karmayoga-app/src/protocol.rs`'s `karmayoga://news/…` routes,
// over HTTP, calling straight into `karmayoga_service::news::view`. Like
// mail's they check their scope explicitly rather than relying on a command
// table, because they are not commands: a story's body is written by a
// stranger, and the pictures inside it are fetched on a client's behalf.

#[derive(Deserialize)]
struct NewsPart {
    /// `?part=notes|transcript|summary`: which of the story's documents a
    /// pane's tab draws. Absent, or a word this does not know, is the whole
    /// body -- see [`karmayoga_service::news::view::BodyPart`].
    part: Option<String>,
}

async fn get_news_body(
    State((server, transport)): Ctxt,
    Path(story): Path<String>,
    headers: HeaderMap,
    Query(q): Query<NewsPart>,
) -> Answer<Response> {
    let ctx = authenticate_media(&server, transport, &headers)?;
    ctx.require(Scope::News)?;
    let story =
        StoryId::parse(&story).map_err(|_| CommandError::new("invalid", "not a story id"))?;
    let part = karmayoga_service::news::view::BodyPart::parse(q.part.as_deref().unwrap_or(""));

    let vault = server.service.require()?;
    // A newsletter's body is a message's, drawn by mail, and what is kept of it
    // in Files -- its summary -- is made from that message. A client that may
    // read the Feed is not for that reason one that may read mail.
    let mail_backed = {
        let vault = vault.clone();
        karmayoga_service::blocking(move || Ok(vault.story(story)?.mail_message.is_some())).await?
    };
    if mail_backed {
        ctx.require(Scope::Mail)?;
    }
    let doc = karmayoga_service::blocking(move || {
        karmayoga_service::news::view::body_document(&vault, story, part)
    })
    .await?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        // Decided fresh every time, as mail's is: whether pictures are hidden
        // changes when a person switches them on.
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        // Read by the interface's `fetch()` of this address, never by anything
        // inside the sandboxed frame. See `protocol.rs`'s `respond_news_body`.
        .header("x-news-images-hidden", if doc.images_hidden { "1" } else { "0" })
        .body(axum::body::Body::from(doc.html))
        .map_err(|e| CommandError::new("internal", e.to_string()).into())
}

/// `/v1/news/img/{story}/{token}`: a picture from a story's body, from the
/// vault when it was fetched before and from the public internet when it was
/// not -- never from a private address.
async fn get_news_image(
    State((server, transport)): Ctxt,
    Path((story, token)): Path<(String, String)>,
    headers: HeaderMap,
) -> Answer<Response> {
    let ctx = authenticate_media(&server, transport, &headers)?;
    ctx.require(Scope::News)?;
    let story =
        StoryId::parse(&story).map_err(|_| CommandError::new("invalid", "not a story id"))?;

    let vault = server.service.require()?;
    let client = karmayoga_service::http::public_client()?;
    let served = karmayoga_service::news::view::image(&vault, client, story, &token).await?;
    // Never cached, for the reason `get_mail_image` gives.
    mail_bytes_response(served, "no-store")
}

// ---- files: a file's bytes, one chunk of an upload -------------------------
//
// The twins of `karmayoga-app/src/protocol.rs`'s `karmayoga://files/file/{id}`
// and of the shell's `upload_chunk` command, over HTTP. Unlike a blob, a file
// is not content-addressed: its bytes can change under the same id, and its
// type is the catalog's word for it rather than anything sniffed from the
// bytes -- so what comes out of here is held to a stricter rule than a blob.

/// Most one chunk of an upload may carry. The shells send four megabytes; twice
/// that leaves room for a client that rounds up and keeps a chunk from being a
/// way to make the server hold a great deal before it has read a byte of it.
const MAX_UPLOAD_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// The largest file answered whole when nothing asked for a part of it.
///
/// [`karmayoga_core::media::plan`] answers a request with no `Range` -- an
/// `<img>`, a `fetch()` -- with the first eight megabytes and a `206`, which a
/// film's player asks for and goes back for the rest of, but which a picture
/// does not: it draws what it was sent. A twenty-five megabyte photograph would
/// show as the top third of one. So up to this size the answer is the file; past
/// it, which only a film or an archive is, the planner decides. Not larger,
/// because a response is a buffer in memory, here and in the shell.
pub const WHOLE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// What a file is answered with when it is opened as a document rather than
/// drawn: the policy `karmayoga-app/src/protocol.rs` sends every `karmayoga:`
/// response with. A catalogued file is whatever bytes somebody put in the
/// folder -- an HTML page a person saved, a mail attachment -- and the type it
/// is served as is the catalog's, so one that is `text/html` would, opened at
/// this address, run its script as part of the page that served it. `sandbox`
/// gives it an opaque origin and no script; as an `<img>` or a `<video>`, which
/// is all the interface asks for, the policy does nothing at all.
const FILE_DOCUMENT_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// What to send for a file of `total` bytes and an optional `Range`: the
/// whole of it when it is small enough and nothing asked for less (see
/// [`WHOLE_FILE_BYTES`]), and what [`karmayoga_core::media::plan`] says
/// otherwise. One function for the server and the desktop shell, so a range
/// means the same thing in both.
pub fn file_plan(total: u64, range: Option<&str>) -> karmayoga_core::media::ResponsePlan {
    let asked = range.is_some_and(|r| karmayoga_core::media::parse_range(r, total).is_some());
    if !asked && total <= WHOLE_FILE_BYTES {
        return karmayoga_core::media::ResponsePlan { start: 0, len: total, total, partial: false };
    }
    karmayoga_core::media::plan(total, range)
}

/// A file's type as its response header, from the catalog's word for it: kept
/// if it is a plausible header value, `application/octet-stream` if not. The
/// catalog is not sniffed, but nor is it trusted to hold nothing that would
/// make a response unbuildable.
pub fn file_content_type(mime: &str) -> &str {
    if !mime.is_empty() && mime.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        mime
    } else {
        "application/octet-stream"
    }
}

/// How long a webview may keep a file's bytes. A year for an address whose
/// `?v=` is the version the file has right now
/// ([`karmayoga_core::Vault::file_version_is_current`]) -- so a changed file
/// has another address -- and never otherwise, because a file's bytes can
/// change under the same id, and under the same `?v=` until the catalog
/// catches up. Never `immutable`, as a blob's are.
pub fn file_cache_control(current: bool) -> &'static str {
    if current { "private, max-age=31536000" } else { "no-cache" }
}

/// What a request for a file asked for, read: the bytes, and what a response
/// made from them needs to say.
pub struct FileSlice {
    /// The catalog's type for the file.
    pub mime: String,
    pub plan: karmayoga_core::media::ResponsePlan,
    pub bytes: Vec<u8>,
    /// The request named the version the file has now, so the answer may be
    /// kept ([`file_cache_control`]).
    pub current: bool,
}

/// Read what a request for a file asked for. Blocking; callers run it off the
/// async runtime.
///
/// The length comes from the disk and the type from the catalog, and the bytes
/// are read after both, so a file that changed in between answers an error to
/// try again rather than a body that disagrees with its own headers.
pub fn read_file_slice(
    vault: &karmayoga_core::Vault,
    id: FileId,
    range: Option<&str>,
    version: Option<&str>,
) -> Result<FileSlice, CommandError> {
    let mime = vault.file(id)?.mime;
    let current = match version.filter(|v| !v.is_empty()) {
        Some(v) => vault.file_version_is_current(id, v)?,
        None => false,
    };
    let total = vault.file_len(id)?;
    let plan = file_plan(total, range);
    let bytes = vault.read_file_range(id, plan.start, plan.len)?;
    if bytes.len() as u64 != plan.len {
        return Err(CommandError::new("io", "that file changed while it was being read"));
    }
    Ok(FileSlice { mime, plan, bytes, current })
}

#[derive(Deserialize)]
struct FileVersion {
    /// `?v=`: what the interface versions an address by. Cached only when it
    /// is the version the file has now -- see [`file_cache_control`].
    v: Option<String>,
}

/// Serve part of a file, honouring `Range`.
///
/// The scope is checked here, not left to a command table, because this is not
/// a command: a paired device holding no `Files` scope may not read the folder
/// through the one route that does not go through [`call`].
async fn get_file(
    State((server, transport)): Ctxt,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<FileVersion>,
) -> Answer<Response> {
    let ctx = authenticate_media(&server, transport, &headers)?;
    ctx.require(Scope::Files)?;
    let id = FileId::parse(&id).map_err(|_| CommandError::new("invalid", "not a file address"))?;

    let vault = server.service.require_unlocked()?;
    // One of Health's -- a clinical document's page, a meal's photograph --
    // is Health's to show as much as Files': a device holding only `Files`
    // reads the folder, not somebody's lab reports.
    let origin = {
        let vault = vault.clone();
        karmayoga_service::blocking(move || Ok(vault.file(id)?.origin)).await?
    };
    if origin == karmayoga_core::files::FileOrigin::Health {
        ctx.require(Scope::Health)?;
    }
    let range = header_str(&headers, "range");
    let version = q.v;
    let FileSlice { mime, plan, bytes, current } = karmayoga_service::blocking(move || {
        read_file_slice(&vault, id, range.as_deref(), version.as_deref())
    })
    .await?;

    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, file_content_type(&mime))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(header::CACHE_CONTROL, vault_cache_control(transport, file_cache_control(current)))
        .header("x-content-type-options", "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, FILE_DOCUMENT_POLICY);
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

#[derive(Deserialize)]
struct UploadOffset {
    /// Where in the file this chunk begins. Read as text and parsed after
    /// authentication, so a caller with no token is refused as such rather
    /// than told what is wrong with its query.
    offset: Option<String>,
}

/// One chunk of an upload begun with the `begin_upload` command, as raw bytes,
/// at `?offset=`. Answers the bytes received so far.
///
/// The same door the desktop shell's `upload_chunk` command is: both call
/// `domains::files::upload_chunk`, which decides what an offset that is not
/// where the upload has got to means. Bytes, not JSON, so it is not a command.
async fn upload_file_chunk(
    State((server, transport)): Ctxt,
    Path(upload): Path<String>,
    Query(q): Query<UploadOffset>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Answer<Json<u64>> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    ctx.require(Scope::Files)?;
    let offset = q
        .offset
        .as_deref()
        .and_then(|o| o.parse::<u64>().ok())
        .ok_or_else(|| CommandError::new("invalid", "an upload chunk needs an ?offset="))?;
    let bytes = read_body(body, MAX_UPLOAD_CHUNK_BYTES).await?;
    let received = karmayoga_service::domains::files::upload_chunk(
        server.service.clone(),
        ctx,
        upload,
        offset,
        bytes,
    )
    .await?;
    Ok(Json(received))
}

// ---- events --------------------------------------------------------------

/// Everything the service says, as it says it.
///
/// One stream per connected client. A client is told about writes it did not
/// make -- the origin is compared here rather than in the interface, so a
/// window never reloads under its own cursor.
///
/// And only about what it may read. A new-mail notice carries a sender, a
/// subject and the first words of the message; a reminder names a medicine; a
/// meeting offer carries a join link. A token that holds only
/// [`Scope::Heartbeats`] or [`Scope::HealthIngest`] is documented as one that
/// "can read nothing", and sits in a browser profile or an app on a phone, so
/// it is refused the stream outright -- the same refusal [`get_blob`] gives it
/// -- and any other token's stream is filtered by its scopes (see
/// `crate::sse::is_for`).
async fn events(
    State((server, transport)): Ctxt,
    headers: HeaderMap,
) -> Answer<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + use<>>> {
    check_protocol(&headers)?;
    let ctx = authenticate(&server, transport, &headers)?;
    refuse_write_only(&ctx)?;
    let me = ctx.caller.origin().unwrap_or("").to_string();
    Ok(server.broadcaster.stream(me, ctx.scopes))
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
    // A copy of the app from before the rename says it under the old name,
    // and is told which side to update rather than that it said nothing.
    let Some(raw) = header_str(headers, "x-karmayoga-protocol")
        .or_else(|| header_str(headers, karmayoga_core::legacy::PROTOCOL_HEADER))
    else {
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
                "that copy of Karma Yoga speaks protocol {n} and this one speaks {PROTOCOL}; \
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
///
/// # Not an MCP token
///
/// An MCP client's token is a row in the same device list a paired phone's
/// is -- one list, so revoking either is the same act -- but it is a key to
/// the MCP endpoint and nothing else. That endpoint is where its limits live:
/// the switch for destructive tools, each account's own MCP access, a tool
/// catalogue filtered to match. Accepted here, the same token reached the
/// whole command table with none of them. So it is refused by kind, from the
/// identity [`Registry::authenticate`] stamped on it, not from anything the
/// request says about itself. A device paired before rows were marked counts
/// as MCP too -- see `auth::Device::mcp` -- and pairs again, hence the message.
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
            let ctx = server.registry.authenticate(&token)?;
            if let Caller::Device(id) = &ctx.caller
                && karmayoga_core::agent::tools::is_mcp_device_id(id)
            {
                return Err(CommandError::new(
                    "forbidden",
                    "this token was issued for an MCP client, and those only work on the MCP \
                     endpoint. If this is a device you paired, pair it again.",
                )
                .into());
            }
            Ok(ctx)
        }
        Transport::Web => web_session(server, headers, false),
    }
}

/// [`authenticate`], for the routes that answer media -- an attachment, a
/// message body, a picture inside one, a story.
///
/// The same for a paired device or the socket. A browser may reach these
/// with the media token instead of the cookie (see `crate::web::session`
/// for why the token exists), and without the protocol header, which an
/// `<img>` cannot send. These routes only read.
fn authenticate_media(
    server: &Arc<Server>,
    transport: Transport,
    headers: &HeaderMap,
) -> Result<Ctx, Failure> {
    if transport == Transport::Web {
        return web_session(server, headers, true);
    }
    check_protocol(headers)?;
    authenticate(server, transport, headers)
}

/// The session web access's gatekeeper found for this request.
///
/// Only ever from the two headers the gatekeeper sets -- it removes any copy
/// a client sent before it looks. `media` says whether the media token is
/// enough, or the cookie is needed.
///
/// A vault that is no longer unlocked ends every session here too, not only
/// through `crate::web::WebSink`: a lock that some path forgot to announce
/// must not leave a browser holding a session into the next unlock.
fn web_session(server: &Arc<Server>, headers: &HeaderMap, media: bool) -> Result<Ctx, Failure> {
    use crate::web::routes::{ACCESS_HEADER, SESSION_HEADER};
    let sessions = server
        .sessions
        .as_ref()
        .ok_or_else(|| CommandError::new("internal", "this server has no browser sessions"))?;
    let signed_out = || CommandError::new("unauthorized", "this browser is not signed in");
    let id = header_str(headers, SESSION_HEADER).ok_or_else(signed_out)?;
    let access = header_str(headers, ACCESS_HEADER).unwrap_or_default();
    if access != "full" && !(media && access == "media") {
        return Err(signed_out().into());
    }
    let Some(vault) = server.service.get().filter(|v| v.is_unlocked()) else {
        sessions.close_all();
        return Err(signed_out().into());
    };
    // A session opened on the vault that was open before this one -- the
    // desktop opening or creating another underneath a running server -- is
    // no key to this one. `belongs_to` ends every such session.
    if !sessions.belongs_to(vault.path()) {
        return Err(signed_out().into());
    }
    let session = sessions.by_id(&id).ok_or_else(signed_out)?;
    Ok(Ctx {
        caller: Caller::Device(crate::web::routes::origin_of(&session.id, headers)),
        scopes: vec![Scope::All],
        proved_at: Some(session.proved_at),
        request_id: None,
    })
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
    use karmayoga_service::error::codes;

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

    /// Every code `karmayoga-service` can put on a `CommandError` -- every
    /// literal inside that crate, and everything `karmayoga_core::Error::code`
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
            (codes::UNREACHABLE, StatusCode::BAD_GATEWAY),
            (codes::NOT_AN_IMAGE, StatusCode::UNPROCESSABLE_ENTITY),
            (codes::NO_LOCATION, StatusCode::UNPROCESSABLE_ENTITY),
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
    use karmayoga_core::VaultConfig;
    use karmayoga_core::id::{AccountId, PackId, ThreadId};
    use karmayoga_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags, RemoteImage,
    };
    use karmayoga_core::packstore::PackRef;
    use karmayoga_core::store::mail::IngestMessage;
    use tower::ServiceExt;

    /// A vault with one message from `sender`, and a `Body` sealed from a
    /// real [`karmayoga_mail::sanitize::sanitize`] call -- so the address
    /// this test drives a request against is exactly what the sync engine
    /// would have written, not a hand-built stand-in for it.
    fn seeded(
        sender: &str,
        html: &str,
    ) -> (tempfile::TempDir, Arc<Service>, MailMessageId, String) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(dir.path(), VaultConfig::default()).unwrap();

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
            category_source: CategorySource::Rules,
            invite: None,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
        };
        vault
            .ingest_mail(account, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
            .unwrap();

        let out = karmayoga_mail::sanitize::sanitize(
            html,
            &karmayoga_mail::sanitize::Rewrite::new(message_id.to_string()),
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

    /// Pulls the first `karmayoga://...` address out of a sanitised
    /// document's `src="..."` and turns it into the path
    /// `karmayoga-server`'s own routes answer -- `karmayoga://mail/...`
    /// becomes `/v1/mail/...`, the same address family under a different
    /// transport. See `karmayoga-app/src/protocol.rs`'s own tests for the
    /// desktop half of this.
    fn request_path_from(html: &str) -> String {
        let start = html.find("src=\"karmayoga://mail").expect("a rewritten src") + "src=\"".len();
        let end = html[start..].find('"').expect("a closing quote") + start;
        format!("/v1/mail{}", &html[start..end]["karmayoga://mail".len()..])
    }

    async fn get(router: Router, path: &str) -> Response {
        router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-karmayoga-protocol", PROTOCOL.to_string())
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

/// The Feed's two routes, driven through the real [`Router`] the way
/// `mail_route_tests` drives mail's: a routing bug -- a path shape that does
/// not match the table, an extractor reading the wrong segment -- is what
/// they would catch.
#[cfg(test)]
mod news_route_tests {
    use super::*;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::news::{BodyOrigin, NewsImage, NewsSource, SourceKind, Story, StoryBody};
    use tower::ServiceExt;

    /// A vault with one web source with pictures switched `images`, and one
    /// story under it with a body naming a picture.
    fn seeded(images: bool) -> (tempfile::TempDir, Arc<Service>, StoryId) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(dir.path(), VaultConfig::default()).unwrap();

        let mut source =
            NewsSource::new(SourceKind::Web { url: "https://blog.example/feed".into() }, "Blog");
        source.images = images;
        vault.save_news_source(&source).unwrap();

        let mut story = Story::new(
            source.id,
            vault.news_key("https://blog.example/post").unwrap(),
            "A post",
            jiff::Timestamp::now(),
        );
        story.summary = "The summary".into();
        let body = StoryBody {
            story_id: story.id,
            html: "<p>Hello</p><img src=\"karmayoga://news/img/s/tok\">".into(),
            text: String::new(),
            origin: BodyOrigin::Feed,
            images: vec![NewsImage {
                token: "tok".into(),
                url: "https://cdn.example.com/a.png".into(),
                cached_blob: None,
            }],
        };
        let id = story.id;
        vault.insert_stories(&[(story, Some(body))]).unwrap();

        let service = Arc::new(Service::new());
        service.set(vault);
        (dir, service, id)
    }

    fn socket_router(service: Arc<Service>) -> (Router, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let broadcaster = Arc::new(Broadcaster::new());
        let server =
            Server::new(service, registry, broadcaster, "Test".into(), String::new(), false);
        (router(server, Transport::Socket), config_dir)
    }

    async fn get(router: Router, path: &str) -> Response {
        router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-karmayoga-protocol", PROTOCOL.to_string())
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn header_of<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn the_body_route_serves_the_document_and_says_whether_pictures_are_hidden() {
        for (images, hidden) in [(true, "0"), (false, "1")] {
            let (_dir, service, story) = seeded(images);
            let (router, _config_dir) = socket_router(service);
            let response = get(router, &format!("/v1/news/body/{story}")).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(header_of(&response, "content-type"), Some("text/html; charset=utf-8"));
            assert_eq!(header_of(&response, "x-news-images-hidden"), Some(hidden));
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(html.contains("<p>Hello</p>"), "{html}");
            assert!(html.contains("Content-Security-Policy"));
        }
    }

    #[tokio::test]
    async fn a_part_asks_for_one_of_a_stories_documents_and_anything_else_is_the_whole() {
        let (_dir, service, story) = seeded(true);
        // An episode: the article written from its transcript, the feed's notes
        // below the rule `article.rs` keeps between them.
        service
            .require()
            .unwrap()
            .save_story_body(&StoryBody {
                story_id: story,
                html: "<p>Written.</p><hr data-show-notes><h2>Show notes</h2><p>Guest</p>".into(),
                text: String::new(),
                origin: BodyOrigin::Transcript,
                images: Vec::new(),
            })
            .unwrap();

        // (the query, a word it has, a word it has not)
        let asked = [
            ("?part=notes", "Guest", "Written."),
            ("?part=summary", "Written.", "Guest"),
            ("", "Written.", "never in a body"),
            ("?part=nonsense", "Guest", "never in a body"),
        ];
        for (query, has, hasnt) in asked {
            let (router, _config_dir) = socket_router(service.clone());
            let response = get(router, &format!("/v1/news/body/{story}{query}")).await;
            assert_eq!(response.status(), StatusCode::OK, "{query}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(html.contains(has), "{query}: {html}");
            assert!(!html.contains(hasnt), "{query}: {html}");
        }
        // The whole is both.
        let (router, _config_dir) = socket_router(service.clone());
        let response = get(router, &format!("/v1/news/body/{story}?part=nonsense")).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(bytes.to_vec()).unwrap().contains("Written."));

        // No transcript was kept: that tab has nothing, which is a 404.
        let (router, _config_dir) = socket_router(service);
        let response = get(router, &format!("/v1/news/body/{story}?part=transcript")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_picture_the_source_hides_is_the_placeholder_pixel() {
        let (_dir, service, story) = seeded(false);
        let (router, _config_dir) = socket_router(service);
        let response = get(router, &format!("/v1/news/img/{story}/tok")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(&response, "content-type"), Some("image/png"));
        assert_eq!(header_of(&response, "cache-control"), Some("no-store"));
    }

    #[tokio::test]
    async fn a_story_that_is_not_an_id_or_not_there_is_refused() {
        let (_dir, service, _story) = seeded(true);
        let (router, _config_dir) = socket_router(service.clone());
        let response = get(router, "/v1/news/body/not-an-id").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let (router, _config_dir) = socket_router(service);
        let response = get(router, &format!("/v1/news/body/{}", StoryId::new())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

/// What is refused before a command runs: a body from somebody who has not
/// said who they are, an MCP token on this listener, and a password change
/// that would be a free guess. Driven through the real [`Router`], as
/// `mail_route_tests` is, because each of these is about the order in which
/// a handler does things -- which only the router can show.
#[cfg(test)]
mod refusal_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;

    /// No vault at all: everything these tests check happens before a
    /// command would need one.
    fn server(allow_remote_unlock: bool) -> (Arc<Server>, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let server = Server::new(
            Arc::new(Service::new()),
            registry,
            Arc::new(Broadcaster::new()),
            "Test".into(),
            String::new(),
            allow_remote_unlock,
        );
        (server, config_dir)
    }

    async fn post(
        app: Router,
        path: &str,
        token: Option<&str>,
        body: axum::body::Body,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("x-karmayoga-protocol", PROTOCOL.to_string());
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn json_body(value: Value) -> axum::body::Body {
        axum::body::Body::from(serde_json::to_vec(&value).unwrap())
    }

    /// A body that notes whether anybody ever asked it for a byte.
    fn watched_body(touched: Arc<AtomicBool>) -> axum::body::Body {
        axum::body::Body::from_stream(futures::stream::once(async move {
            touched.store(true, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![b'x'; 1024]))
        }))
    }

    #[tokio::test]
    async fn a_body_is_not_read_for_somebody_with_no_token() {
        let (server, _dir) = server(true);
        for path in [
            "/v1/call/list_journals",
            "/v1/stream/send_message",
            "/v1/blob",
            "/v1/files/upload/abc?offset=0",
        ] {
            let touched = Arc::new(AtomicBool::new(false));
            let app = router(server.clone(), Transport::Network);
            let (status, _) = post(app, path, None, watched_body(touched.clone())).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert!(!touched.load(Ordering::SeqCst), "{path} read a body before authenticating");
        }
    }

    #[tokio::test]
    async fn a_command_past_its_limit_is_too_large_and_one_under_it_is_not() {
        let (server, _dir) = server(true);

        // Streamed with no declared length, so it is the running count that
        // has to catch it rather than the size hint.
        let chunk = axum::body::Bytes::from(vec![b' '; 1024 * 1024]);
        let chunks = MAX_JSON_BYTES / chunk.len() + 1;
        let body = axum::body::Body::from_stream(futures::stream::iter(
            std::iter::repeat_n(chunk, chunks).map(Ok::<_, std::convert::Infallible>),
        ));
        let app = router(server.clone(), Transport::Socket);
        let (status, answer) = post(app, "/v1/call/list_journals", None, body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(answer["code"], "too_large");

        // Well past the router's small default, which a raw body is not held
        // to: this one reaches the service, which has no vault to answer with.
        let padded = json_body(json!({ "pad": "x".repeat(3 * 1024 * 1024) }));
        let app = router(server, Transport::Socket);
        let (status, answer) = post(app, "/v1/call/list_journals", None, padded).await;
        assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
    }

    #[tokio::test]
    async fn an_mcp_token_is_refused_on_this_listener() {
        let (server, _dir) = server(true);
        let (mcp, _) = server.registry.issue("Claude Code", vec![Scope::All], true).unwrap();
        let (paired, _) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();

        let app = router(server.clone(), Transport::Network);
        let (status, answer) =
            post(app, "/v1/call/list_journals", Some(&mcp), json_body(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["code"], "forbidden");

        // A device paired the ordinary way, holding the same scope, gets past
        // authentication to the service.
        let app = router(server, Transport::Network);
        let (status, answer) =
            post(app, "/v1/call/list_journals", Some(&paired), json_body(json!({}))).await;
        assert!(
            status != StatusCode::FORBIDDEN && status != StatusCode::UNAUTHORIZED,
            "{status} {answer}"
        );
    }

    #[tokio::test]
    async fn every_command_that_derives_a_key_is_refused_when_remote_unlock_is_off() {
        // No vault is a sealed one, which is the case the refusal is for.
        let (server, _dir) = server(false);
        let (token, _) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();
        for name in ["unlock", "verify_password", "change_password"] {
            let args = json!({ "password": "a guess", "current": "a guess", "next": "a new one!" });
            let app = router(server.clone(), Transport::Network);
            let (status, answer) =
                post(app, &format!("/v1/call/{name}"), Some(&token), json_body(args)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {answer}");
        }
    }

    #[tokio::test]
    async fn changing_the_password_counts_against_the_same_lockout_as_unlocking() {
        let (server, _dir) = server(true);
        let (token, id) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();

        // Spend this device's attempts the way wrong guesses at `unlock` would.
        let caller = Caller::Device(id);
        for _ in 0..100 {
            let Ok(gate) = server.registry.unlock_gate(&caller).await else { break };
            gate.record(false);
        }

        let args = json!({ "current": "one more guess", "next": "a new password" });
        let app = router(server, Transport::Network);
        let (status, answer) =
            post(app, "/v1/call/change_password", Some(&token), json_body(args)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{answer}");
        assert_eq!(answer["code"], "too_many_attempts");
    }

    #[tokio::test]
    async fn an_upload_chunk_needs_the_files_scope_and_an_offset() {
        let (server, _dir) = server(true);
        let (narrow, _) = server.registry.issue("Phone", vec![Scope::Journals], false).unwrap();
        let (wide, _) = server.registry.issue("Laptop", vec![Scope::Files], false).unwrap();

        let app = router(server.clone(), Transport::Network);
        let (status, answer) = post(
            app,
            "/v1/files/upload/abc?offset=0",
            Some(&narrow),
            axum::body::Body::from(vec![0u8; 16]),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["code"], "forbidden");

        // Past the scope check, a chunk with nowhere to go is a bad request,
        // not a guess at offset zero.
        let app = router(server, Transport::Network);
        let (status, answer) = post(
            app,
            "/v1/files/upload/abc?offset=soon",
            Some(&wide),
            axum::body::Body::from(vec![0u8; 16]),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    }

    #[tokio::test]
    async fn a_chunk_past_its_limit_is_too_large() {
        let (server, _dir) = server(true);
        let (token, _) = server.registry.issue("Laptop", vec![Scope::Files], false).unwrap();
        let app = router(server, Transport::Network);
        let body = axum::body::Body::from(vec![0u8; MAX_UPLOAD_CHUNK_BYTES + 1]);
        let (status, answer) = post(app, "/v1/files/upload/abc?offset=0", Some(&token), body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
    }

    async fn open_events(
        server: &Arc<Server>,
        transport: Transport,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .uri("/v1/events")
            .header("x-karmayoga-protocol", PROTOCOL.to_string());
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = router(server.clone(), transport)
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if status == StatusCode::OK {
            // An open stream has no end to read to.
            return (status, Value::Null);
        }
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// A browser extension's heartbeats token and a phone's health-push token
    /// are documented as ones that can read nothing, and the event stream is
    /// where a new-mail notice, a dose reminder and a meeting's join link are
    /// said. Neither may open it; a token that can read something may.
    #[tokio::test]
    async fn a_write_only_token_cannot_open_the_event_stream() {
        let (server, _dir) = server(true);
        for scopes in [
            vec![Scope::Heartbeats],
            vec![Scope::HealthIngest],
            vec![Scope::Heartbeats, Scope::HealthIngest],
        ] {
            let (token, _) = server.registry.issue("Collector", scopes.clone(), false).unwrap();
            let (status, answer) = open_events(&server, Transport::Network, Some(&token)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{scopes:?}: {answer}");
            assert_eq!(answer["code"], "forbidden", "{scopes:?}");
        }

        let (none, _) = open_events(&server, Transport::Network, None).await;
        assert_eq!(none, StatusCode::UNAUTHORIZED);

        // Write-only *and* something readable is not write-only: it asked for
        // the stream with a scope that may hear part of it.
        let (mixed, _) = server
            .registry
            .issue("Phone", vec![Scope::HealthIngest, Scope::Health], false)
            .unwrap();
        let (status, _) = open_events(&server, Transport::Network, Some(&mixed)).await;
        assert_eq!(status, StatusCode::OK);
        let (all, _) = server.registry.issue("Laptop", vec![Scope::All], false).unwrap();
        let (status, _) = open_events(&server, Transport::Network, Some(&all)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = open_events(&server, Transport::Socket, None).await;
        assert_eq!(status, StatusCode::OK);
    }
}

/// What a file is answered with, apart from where its bytes come from -- which
/// is the facade's, and tested there.
#[cfg(test)]
mod file_route_tests {
    use super::*;
    use karmayoga_core::media::{ResponsePlan, plan};
    use tower::ServiceExt;

    #[test]
    fn a_small_file_asked_for_whole_is_answered_whole() {
        // Where `media::plan` alone would send eight megabytes and a 206, and
        // a picture would be drawn from the top of it.
        let total = 20 * 1024 * 1024;
        assert!(plan(total, None).partial);
        assert_eq!(
            file_plan(total, None),
            ResponsePlan { start: 0, len: total, total, partial: false }
        );
        assert_eq!(
            file_plan(0, None),
            ResponsePlan { start: 0, len: 0, total: 0, partial: false },
            "an empty file is an empty answer"
        );
    }

    #[test]
    fn a_range_is_the_planners_to_decide() {
        let total = 20 * 1024 * 1024;
        for range in ["bytes=0-", "bytes=1000-1999", "bytes=-500"] {
            assert_eq!(file_plan(total, Some(range)), plan(total, Some(range)), "{range}");
        }
        // Past what a response may be, nothing asked for less still gets a part.
        let film = WHOLE_FILE_BYTES + 1;
        assert_eq!(file_plan(film, None), plan(film, None));
        assert!(file_plan(film, None).partial);
        // A range that is nonsense is no range at all.
        assert_eq!(file_plan(100, Some("bytes=oops")).len, 100);
    }

    #[test]
    fn a_file_is_only_cached_under_an_address_that_names_its_version() {
        assert_eq!(file_cache_control(true), "private, max-age=31536000");
        assert_eq!(file_cache_control(false), "no-cache");
        // Never what a blob's are: the bytes behind a file's address can change.
        assert!(!file_cache_control(true).contains("immutable"));
    }

    #[test]
    fn a_type_that_is_not_a_header_value_is_an_octet_stream() {
        assert_eq!(file_content_type("image/jpeg"), "image/jpeg");
        assert_eq!(file_content_type("text/plain; charset=utf-8"), "text/plain; charset=utf-8");
        for bad in ["", "image/jp\neg", "text/\u{7f}", "caf\u{e9}/x"] {
            assert_eq!(file_content_type(bad), "application/octet-stream", "{bad:?}");
        }
    }

    /// Nothing to read from: this is about who may ask, which is decided
    /// before the vault is looked at.
    #[tokio::test]
    async fn a_file_is_not_served_to_a_device_without_the_files_scope() {
        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let server = Server::new(
            Arc::new(Service::new()),
            registry,
            Arc::new(Broadcaster::new()),
            "Test".into(),
            String::new(),
            true,
        );
        let (narrow, _) = server.registry.issue("Phone", vec![Scope::Mail], false).unwrap();
        let uri = format!("/v1/files/file/{}?v=abc", FileId::new());
        for (token, want) in
            [(Some(narrow), StatusCode::FORBIDDEN), (None, StatusCode::UNAUTHORIZED)]
        {
            let mut request =
                Request::builder().uri(&uri).header("x-karmayoga-protocol", PROTOCOL.to_string());
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let response = router(server.clone(), Transport::Network)
                .oneshot(request.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), want);
        }
    }
}

/// What a browser is told it may keep of the vault's decrypted bytes.
#[cfg(test)]
mod cache_route_tests {
    use super::*;
    use karmayoga_core::VaultConfig;
    use tower::ServiceExt;

    #[test]
    fn a_browser_is_never_told_to_keep_what_the_vault_decrypted() {
        for desktop in
            ["private, max-age=31536000, immutable", "private, max-age=31536000", "no-cache"]
        {
            for transport in [Transport::Network, Transport::Web] {
                assert_eq!(vault_cache_control(transport, desktop), "no-store", "{desktop}");
            }
            // The local socket is the owner's own process, not a browser.
            assert_eq!(vault_cache_control(Transport::Socket, desktop), desktop);
        }
    }

    /// The header on the wire, from a real blob, on each transport that can
    /// answer one without a browser session: a paired device over the
    /// network says `no-store` with nothing beside it; the socket keeps its
    /// year.
    #[tokio::test]
    async fn a_blob_over_the_network_is_not_kept_and_over_the_socket_is() {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(dir.path(), VaultConfig::default()).unwrap();
        let service = Arc::new(Service::new());
        service.set(vault);
        let id = service.put_blob(&Ctx::local(), b"a passport scan".to_vec()).await.unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
        let (token, _) = registry.issue("Laptop", vec![Scope::All], false).unwrap();
        let server = Server::new(
            service,
            registry,
            Arc::new(Broadcaster::new()),
            "Test".into(),
            String::new(),
            false,
        );

        let ask = |transport: Transport, token: Option<String>| {
            let app = router(server.clone(), transport);
            let uri = format!("/v1/blob/{id}");
            async move {
                let mut request = Request::builder()
                    .uri(uri)
                    .header("x-karmayoga-protocol", PROTOCOL.to_string());
                if let Some(token) = token {
                    request = request.header("authorization", format!("Bearer {token}"));
                }
                app.oneshot(request.body(axum::body::Body::empty()).unwrap()).await.unwrap()
            }
        };

        let network = ask(Transport::Network, Some(token)).await;
        assert_eq!(network.status(), StatusCode::OK);
        assert_eq!(network.headers()[header::CACHE_CONTROL], "no-store");

        let socket = ask(Transport::Socket, None).await;
        assert_eq!(socket.status(), StatusCode::OK);
        assert_eq!(socket.headers()[header::CACHE_CONTROL], "private, max-age=31536000, immutable");
    }
}
