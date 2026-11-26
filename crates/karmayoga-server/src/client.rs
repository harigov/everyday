//! The other end: a copy of the application talking to a vault it does not
//! hold.
//!
//! This is what makes a remote client the *same app* rather than a viewer. It
//! offers the same three entry points [`karmayoga_service::Service`] does --
//! `call`, the blob calls, and `send_message` -- so the shell above it can
//! hold either and the interface never learns which.
//!
//! # Why the pinning is here and not in the webview
//!
//! Two reasons, and either alone would be enough. A webview will not accept a
//! self-signed certificate and JavaScript cannot pin one. And the content
//! security policy in `tauri.conf.json` allows `connect-src 'self' ipc:` and
//! nothing else, which the application treats as load-bearing -- nothing it
//! renders can cause a request of its own. Forwarding from Rust keeps both
//! true: the bearer token never enters the webview, and the only outbound
//! connection is one this file makes.
//!
//! # What is cached, and what is not
//!
//! Attachment bytes, in memory, bounded. Blobs are content-addressed, so the
//! bytes behind an address can never change and a cache over them needs no
//! invalidation at all -- which is the whole reason it is safe to have one in an
//! arrangement built around not keeping vault content on the client. It is
//! dropped when the connection is, never written to disk, and covers the case
//! it exists for: scrubbing a video, and scrolling a shelf of forty covers.
//!
//! Nothing else is cached. There is no offline mode, and a client that could
//! serve a list from memory while disconnected would be one edit away from
//! being one.

use karmayoga_service::agent::{AgentEvent, Sink};
use karmayoga_service::error::{CommandError, CommandResult};
use karmayoga_service::news::view::BodyPart;
use karmayoga_service::{Ctx, PROTOCOL};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::pairing::Invite;

/// Longest a single command may take.
///
/// Generous, because a calendar sync and a turn of the assistant both happen
/// behind one of these, and finite, because a request that will never answer
/// should not hold a spinner for ever.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Longest a picture may take. A model on the server's own processor is
/// given ten minutes by the service (`images::local`), and OpenAI two; this
/// covers either and the trip, so the answer is not cut off while the server
/// is still painting -- or still paying for -- the picture.
const PICTURE_TIMEOUT: Duration = Duration::from_secs(12 * 60);

/// How long `name` may take, where [`CALL_TIMEOUT`] would give up on an
/// answer the server is still working on.
fn call_timeout(name: &str) -> Duration {
    match name {
        "generate_image" => PICTURE_TIMEOUT,
        _ => CALL_TIMEOUT,
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest one turn of the assistant may take, start to finish.
///
/// Its own figure because the timeout covers the whole response body and a
/// turn's body is a stream that stays open while the model thinks and runs
/// tools. Generous rather than absent: a stream that will never end should
/// still stop holding a spinner eventually.
const TURN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Most attachment bytes to keep in memory.
///
/// Two hundred megabytes is a scrubbed video and a shelf of covers, and is
/// small beside what a webview holds for the same content anyway.
const BLOB_CACHE_BYTES: usize = 200 * 1024 * 1024;

/// A server this client has paired with, as remembered between sessions.
///
/// The token is deliberately absent: it is a bearer credential to an unlocked
/// vault and belongs in the operating system's keychain, not in a file beside
/// the application's settings. See the shell's `remotes` module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    /// What the vault calls itself, for the picker.
    pub name: String,
    /// `host:port`, as dialled.
    pub host: String,
    /// The certificate's SHA-256, hex. Empty when TLS is somebody else's.
    pub fingerprint: String,
    /// The certificate itself, so a reconnection pins without asking again.
    ///
    /// Empty when nothing was pinned -- a link with no fingerprint -- and the
    /// server is checked the ordinary way instead. See `client_for`.
    pub cert_pem: String,
    pub paired: jiff::Timestamp,
}

/// What a server says about itself before anybody has paired.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub name: String,
    pub protocol: u32,
    pub fingerprint: String,
    pub locked: bool,
    #[serde(default)]
    pub pairing: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Paired {
    token: String,
    device_id: String,
}

pub struct RemoteClient {
    http: reqwest::Client,
    base: String,
    token: String,
    connection: Connection,
    blobs: Mutex<BlobCache>,
}

/// What [`RemoteClient::mail_body`] answers with -- see
/// [`karmayoga_service::mailview::BodyDocument`], which this mirrors across
/// the wire.
pub struct MailBody {
    pub html: String,
    pub images_hidden: bool,
}

/// What [`RemoteClient::mail_part`] and [`RemoteClient::mail_image`] answer
/// with -- see [`karmayoga_service::mailview::PartResponse`], which this
/// mirrors across the wire.
pub struct MailBytes {
    pub content_type: String,
    pub attachment: bool,
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
}

/// What [`RemoteClient::file_range`] answers with: some of a file's bytes, and
/// what a response made from them needs to say about them.
pub struct FileBytes {
    /// The catalog's type for the file, as the server sent it -- never
    /// sniffed from the bytes.
    pub content_type: String,
    /// The whole file's length, not this part's.
    pub total: u64,
    /// Where `bytes` begins in the file.
    pub start: u64,
    /// Whether the server sent less than the whole file (a `206`), however it
    /// came to: a `Range` it was asked for, or a file too big to send at once.
    pub partial: bool,
    pub bytes: Vec<u8>,
}

impl RemoteClient {
    /// Pair with the server a link points at, and connect.
    ///
    /// Three things happen in order, and the order is the security of it. The
    /// certificate is fetched and its fingerprint compared with the one in the
    /// link, which the person carried from the other screen; only then is a
    /// client built that trusts that certificate and no other; only then is the
    /// one-time code spent. A mismatch stops before anything is sent.
    ///
    /// A link with no fingerprint is one for a server behind somebody else's
    /// TLS -- see `Config::no_tls` -- and there is nothing the person carried
    /// to compare a certificate with. So nothing is pinned, and the server has
    /// to pass the check any HTTPS client makes: a certificate a public
    /// authority vouches for, naming the host dialled. This used to pin
    /// whatever answered, which vouched for nothing -- anybody between the two
    /// machines could answer, be pinned, and be handed the code.
    pub async fn pair(invite: &Invite, device_name: &str) -> CommandResult<(Self, String)> {
        let pinned = !invite.fingerprint.is_empty();
        let cert_pem = if pinned {
            let cert_pem = fetch_certificate(&invite.host).await?;
            let der = crate::tls::pem_to_der(&cert_pem)?;
            let seen = crate::tls::fingerprint_of(&der);
            if !seen.eq_ignore_ascii_case(&invite.fingerprint) {
                return Err(CommandError::new(
                    "fingerprint",
                    "that computer answered with a different certificate from the one in the \
                     link. Either the link is old, or something is between you and it.",
                ));
            }
            cert_pem
        } else {
            String::new()
        };

        let http = client_for(&cert_pem)?;
        let base = format!("https://{}", invite.host);
        // A failed certificate check reads as "could not reach" by the time
        // `transport` has had it, which is no help to somebody who trimmed a
        // link for a computer serving its own certificate.
        let hello = hello_with(&http, &base).await.map_err(|e| {
            if pinned {
                e
            } else {
                CommandError::new(
                    &e.code,
                    format!(
                        "{} This link has no certificate fingerprint in it, so that computer \
                         has to show a certificate a public authority issued. If it serves its \
                         own, copy a fresh link from it.",
                        e.message
                    ),
                )
            }
        })?;
        check_protocol(hello.protocol)?;

        let paired: Paired = http
            .post(format!("{base}/v1/pair"))
            .header("x-karmayoga-protocol", PROTOCOL.to_string())
            .header(karmayoga_core::legacy::PROTOCOL_HEADER, PROTOCOL.to_string())
            .json(&serde_json::json!({
                "code": invite.code,
                "deviceName": device_name,
                "scopes": [],
            }))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?
            .json()
            .await
            .map_err(transport)?;

        let connection = Connection {
            id: paired.device_id,
            name: if hello.name.is_empty() { invite.name.clone() } else { hello.name },
            host: invite.host.clone(),
            // What the server says of itself is only worth recording when it
            // is what was pinned.
            fingerprint: if pinned { hello.fingerprint } else { String::new() },
            cert_pem,
            paired: jiff::Timestamp::now(),
        };
        let token = paired.token.clone();
        Ok((Self::new(http, base, paired.token, connection), token))
    }

    /// Reconnect to a server this client has paired with before.
    pub async fn resume(connection: Connection, token: String) -> CommandResult<Self> {
        let http = client_for(&connection.cert_pem)?;
        let base = format!("https://{}", connection.host);
        let hello = hello_with(&http, &base).await?;
        check_protocol(hello.protocol)?;
        Ok(Self::new(http, base, token, connection))
    }

    fn new(http: reqwest::Client, base: String, token: String, connection: Connection) -> Self {
        Self { http, base, token, connection, blobs: Mutex::new(BlobCache::default()) }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header("x-karmayoga-protocol", PROTOCOL.to_string())
            // Said under the old name as well, so a server from before the
            // rename answers with which side to update.
            .header(karmayoga_core::legacy::PROTOCOL_HEADER, PROTOCOL.to_string())
            .header("authorization", format!("Bearer {}", self.token))
            .header("x-karmayoga-client", &self.connection.id)
    }

    /// Run a command on the machine holding the vault.
    pub async fn call(&self, ctx: &Ctx, name: &str, args: Value) -> CommandResult<Value> {
        let mut request = self
            .request(reqwest::Method::POST, &format!("/v1/call/{name}"))
            .timeout(call_timeout(name));
        if let Some(id) = &ctx.request_id {
            request = request.header("x-karmayoga-request", id);
        }
        let response = request.json(&args).send().await.map_err(transport)?;
        response.error_for_body().await?.json().await.map_err(transport)
    }

    pub async fn put_blob(&self, bytes: Vec<u8>) -> CommandResult<String> {
        let response = self
            .request(reqwest::Method::POST, "/v1/blob")
            .body(bytes)
            .send()
            .await
            .map_err(transport)?;
        response.error_for_body().await?.json().await.map_err(transport)
    }

    /// How long an attachment is.
    ///
    /// A `Range` request for one byte rather than a route of its own: the reply
    /// carries `Content-Range`, which is where the total lives, and the byte
    /// comes back cached for the read that is about to follow.
    pub async fn blob_len(&self, id: &str) -> CommandResult<u64> {
        if let Some(len) = self.blobs.lock().unwrap().len_of(id) {
            return Ok(len);
        }
        let response = self
            .request(reqwest::Method::GET, &format!("/v1/blob/{id}"))
            .header("range", "bytes=0-0")
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;

        let total = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<u64>().ok())
            // A server that answered 200 rather than 206 sent the whole thing,
            // and its length is the length.
            .or_else(|| response.content_length())
            .ok_or_else(|| CommandError::new("network", "that attachment had no length"))?;
        self.blobs.lock().unwrap().note_len(id, total);
        Ok(total)
    }

    /// Part of an attachment, from memory when it has been read before.
    pub async fn blob_range(&self, id: &str, offset: u64, len: u64) -> CommandResult<Vec<u8>> {
        if let Some(bytes) = self.blobs.lock().unwrap().get(id, offset, len) {
            return Ok(bytes);
        }
        let end = offset + len.saturating_sub(1);
        let bytes = self
            .request(reqwest::Method::GET, &format!("/v1/blob/{id}"))
            .header("range", format!("bytes={offset}-{end}"))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?
            .bytes()
            .await
            .map_err(transport)?
            .to_vec();
        self.blobs.lock().unwrap().put(id, offset, bytes.clone());
        Ok(bytes)
    }

    /// Part of a file in the files catalog -- or all of it, for a small one --
    /// with its type and where it sits in the whole. `range` is a `Range`
    /// header's value, passed on as the webview sent it, so the server's
    /// planner (`routes::file_plan`) decides what a range means for a file
    /// exactly as it does for a window on the vault's own machine, and a film
    /// seeks over a network as it seeks off a disk.
    ///
    /// Not cached, unlike [`Self::blob_range`]: a file's bytes can change under
    /// the same id -- something else may edit it in the folder -- so a cache
    /// over them would need exactly the invalidation this client has no way to
    /// hear about.
    pub async fn file_range(&self, id: &str, range: Option<&str>) -> CommandResult<FileBytes> {
        let mut request = self.request(reqwest::Method::GET, &format!("/v1/files/file/{id}"));
        if let Some(range) = range {
            request = request.header("range", range);
        }
        let response = request.send().await.map_err(transport)?.error_for_body().await?;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let partial = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let placed = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);
        let bytes = response.bytes().await.map_err(transport)?.to_vec();
        let (start, total) = match (partial, placed) {
            (true, Some(placed)) => placed,
            (true, None) => return Err(CommandError::new("network", "that file had no length")),
            // A `200` is the whole file.
            (false, _) => (0, bytes.len() as u64),
        };
        Ok(FileBytes { content_type, total, start, partial, bytes })
    }

    /// One chunk of an upload begun with `begin_upload`, at `offset`. Answers
    /// the bytes the server has received so far, which is how a caller knows
    /// the chunk landed where it meant it to.
    ///
    /// Bytes, not JSON, so its own route rather than a command -- see
    /// `karmayoga_service::domains::files::upload_chunk`.
    pub async fn upload_chunk(
        &self,
        upload: &str,
        offset: u64,
        bytes: Vec<u8>,
    ) -> CommandResult<u64> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/v1/files/upload/{}?offset={offset}", encode_segment(upload)),
            )
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(transport)?;
        response.error_for_body().await?.json().await.map_err(transport)
    }

    /// A rendered message body, and whether it hid remote images -- the
    /// remote counterpart of [`karmayoga_service::mailview::BodyDocument`],
    /// fetched from `/v1/mail/body/{id}` rather than built locally. Read by
    /// `karmayoga-app/src/protocol.rs`'s `mail/body` route when this window
    /// is looking at a vault on another machine.
    pub async fn mail_body(&self, id: &str) -> CommandResult<MailBody> {
        let response = self
            .request(reqwest::Method::GET, &format!("/v1/mail/body/{id}"))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;
        // Read before the body is consumed: `x-mail-images-hidden` is a
        // header, not part of the HTML, exactly so the interface can learn
        // it without parsing the document -- see the server route's own
        // docs for why.
        let images_hidden = response
            .headers()
            .get("x-mail-images-hidden")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "true");
        let html = response.text().await.map_err(transport)?;
        Ok(MailBody { html, images_hidden })
    }

    /// One part of a message -- an attachment, or an inline image a `cid:`
    /// reference points at. The remote counterpart of
    /// [`karmayoga_service::mailview::part`].
    pub async fn mail_part(&self, message_id: &str, identifier: &str) -> CommandResult<MailBytes> {
        let response = self
            .request(reqwest::Method::GET, &format!("/v1/mail/part/{message_id}/{identifier}"))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;
        Self::mail_bytes_from(response).await
    }

    /// A remote image, fetched (or refused, or served from the cache) on
    /// the machine holding the vault -- never by this process, which is the
    /// whole point: see `karmayoga_service::mailview::remote_image`'s docs.
    ///
    /// Requests `/v1/mail/img/{message_id}/{token}`, matching the path
    /// shape `karmayoga-mail::sanitize::sanitize` writes today (message id
    /// first, the same order `mail_part` already uses) -- never the older
    /// `?m=` query form, even when the address a caller resolved this
    /// `token` and `message_id` from was itself written under that older
    /// shape: `karmayoga-server`'s route answers a given `(message_id,
    /// token)` pair identically no matter which shape asked for it, so
    /// there is nothing for this client to preserve by choosing one over
    /// the other. See `routes.rs`'s `get_mail_image_legacy` for why the
    /// server still keeps the older route at all.
    pub async fn mail_image(&self, token: &str, message_id: &str) -> CommandResult<MailBytes> {
        let response = self
            .request(reqwest::Method::GET, &format!("/v1/mail/img/{message_id}/{token}"))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;
        Self::mail_bytes_from(response).await
    }

    /// A story's rendered body and whether its source holds pictures back:
    /// the remote counterpart of `karmayoga_service::news::view::body_document`,
    /// from `/v1/news/body/{story}`, or of one of the story's documents --
    /// `?part=` -- for a pane's tab. The same pair a mail body is, so it comes
    /// back as one.
    pub async fn news_body(&self, story: &str, part: BodyPart) -> CommandResult<MailBody> {
        let path = match part.query() {
            Some(part) => format!("/v1/news/body/{story}?part={part}"),
            None => format!("/v1/news/body/{story}"),
        };
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;
        let images_hidden = response
            .headers()
            .get("x-news-images-hidden")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "1");
        let html = response.text().await.map_err(transport)?;
        Ok(MailBody { html, images_hidden })
    }

    /// One picture from a story's body, from `/v1/news/img/{story}/{token}`.
    pub async fn news_image(&self, story: &str, token: &str) -> CommandResult<MailBytes> {
        let response = self
            .request(reqwest::Method::GET, &format!("/v1/news/img/{story}/{token}"))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;
        Self::mail_bytes_from(response).await
    }

    async fn mail_bytes_from(response: reqwest::Response) -> CommandResult<MailBytes> {
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let disposition = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok());
        let attachment = disposition.is_some_and(|v| v.starts_with("attachment"));
        let filename = disposition
            .and_then(|v| v.split("filename=\"").nth(1))
            .and_then(|v| v.strip_suffix('"'))
            .map(str::to_string);
        let bytes = response.bytes().await.map_err(transport)?.to_vec();
        Ok(MailBytes { content_type, attachment, filename, bytes })
    }

    /// Forget every cached byte. Called when the vault locks, and when this
    /// connection is dropped.
    pub fn clear_cache(&self) {
        self.blobs.lock().unwrap().clear();
    }

    /// Is the machine holding this vault answering?
    ///
    /// One short request, on the connection already open, so a reconnect loop
    /// can find out whether it is worth resetting its backoff without waiting
    /// on a stream that stays open all day either way.
    pub async fn reachable(&self) -> bool {
        self.request(reqwest::Method::GET, "/v1/hello")
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    /// A turn of the assistant, read a line at a time.
    pub async fn send_message(&self, args: Value, sink: Sink) -> CommandResult<()> {
        use futures::StreamExt;

        let response = self
            .request(reqwest::Method::POST, "/v1/stream/send_message")
            // Not the client's 120-second command timeout. A turn runs tools
            // and can genuinely take minutes, and the timeout applies to the
            // *whole* response -- so inheriting it truncated a long turn
            // mid-sentence and left the panel with a stream that stopped for
            // no reason it could report.
            .timeout(TURN_TIMEOUT)
            .json(&args)
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;

        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk.map_err(transport)?);
            // A frame is a line. Anything after the last newline is the start of
            // the next one and stays in the buffer -- a chunk boundary falls
            // wherever the network put it, not where the JSON ends.
            while let Some(at) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=at).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_slice::<AgentEvent>(line) {
                    Ok(event) => sink(event),
                    Err(e) => tracing::debug!(error = %e, "an assistant event did not parse"),
                }
            }
        }
        Ok(())
    }

    /// Talk to the assistant on the computer at the other end: a voice
    /// session there, on `conversation`, whose [`VoiceLink`] is handed back
    /// to be driven exactly as one on this computer would be -- microphone
    /// audio in, audio to play and events out. See `crate::voice`.
    ///
    /// Answers once the session is open, or with why it could not be (no
    /// voice chosen there, the vault locked, a device without the agent
    /// scope), so the caller can say so as it would for a session here.
    pub async fn voice_open(
        &self,
        conversation: karmayoga_core::ConversationId,
        on_screen: Option<Value>,
    ) -> CommandResult<karmayoga_service::voice::VoiceLink> {
        use crate::voice::{ClientMessage, Frame, ServerMessage};
        use base64::Engine;

        let http = voice_client(&self.connection.cert_pem)?;
        // The whole of opening, bounded: a computer that took the request and
        // then went quiet -- asleep, the network gone mid-way -- would leave
        // the voice slot starting for ever. A minute, because the other end
        // may load its voice models first.
        let opening = async {
            let key = base64::engine::general_purpose::STANDARD.encode(rand::random::<[u8; 16]>());
            let response = http
                .get(format!("{}{}", self.base, crate::voice::ROUTE))
                .header("x-karmayoga-protocol", PROTOCOL.to_string())
                .header(karmayoga_core::legacy::PROTOCOL_HEADER, PROTOCOL.to_string())
                .bearer_auth(&self.token)
                .header(reqwest::header::CONNECTION, "Upgrade")
                .header(reqwest::header::UPGRADE, "websocket")
                .header(reqwest::header::SEC_WEBSOCKET_VERSION, "13")
                .header(reqwest::header::SEC_WEBSOCKET_KEY, key)
                .send()
                .await
                .map_err(transport)?;
            if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
                // The server's own `{ code, message }`, when it refused.
                response.error_for_body().await?;
                return Err(CommandError::new(
                    "network",
                    "that computer did not open a voice session",
                ));
            }
            let upgraded = response.upgrade().await.map_err(transport)?;
            let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                upgraded,
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                None,
            )
            .await;
            let (mut frames, out) = crate::voice::tungstenite_socket(socket);

            let open = ClientMessage::Open { conversation_id: conversation, on_screen };
            let _ = out.send(Frame::Text(serde_json::to_string(&open).unwrap_or_default()));
            match frames.recv().await {
                Some(Frame::Text(raw)) => match serde_json::from_str::<ServerMessage>(&raw) {
                    Ok(ServerMessage::Opened) => Ok(crate::voice::link_over(frames, out)),
                    Ok(ServerMessage::Failed { code, message }) => {
                        Err(CommandError { code, message })
                    }
                    _ => {
                        Err(CommandError::new("network", "that computer said something unexpected"))
                    }
                },
                _ => Err(CommandError::new("network", crate::voice::LOST)),
            }
        };
        tokio::time::timeout(Duration::from_secs(60), opening).await.unwrap_or_else(|_| {
            Err(CommandError::new("network", "your computer did not start talking in time"))
        })
    }

    /// Subscribe to what the server says, calling `on_event` for each.
    ///
    /// Returns when the stream ends, so the caller decides whether to try
    /// again. Reconnection belongs to whoever knows what a disconnection should
    /// look like on screen.
    pub async fn events(&self, on_event: impl Fn(ServerEvent)) -> CommandResult<()> {
        use futures::StreamExt;

        let response = self
            .request(reqwest::Method::GET, "/v1/events")
            .timeout(Duration::from_secs(60 * 60 * 24))
            .send()
            .await
            .map_err(transport)?
            .error_for_body()
            .await?;

        let mut stream = response.bytes_stream();
        // Bytes, not a string.
        //
        // Decoding each chunk as it arrives corrupts any character whose bytes
        // straddle a chunk boundary -- `from_utf8_lossy` turns the half it can
        // see into a replacement character -- and the frame then fails to
        // parse and is dropped with a debug line. An entry title with an
        // em-dash or a name with an accent in it is enough, and the symptom is
        // a change event that goes missing about one time in a thousand.
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk.map_err(transport)?);
            // Server-sent events are separated by a blank line, and a field is
            // `name: value`. Only `data` is read: the event name is in the
            // payload's own `type`, so nothing depends on parsing both.
            while let Some(at) = find(&buffer, b"\n\n") {
                let frame: Vec<u8> = buffer.drain(..at + 2).collect();
                for line in frame.split(|b| *b == b'\n') {
                    let Some(data) = line.strip_prefix(b"data:") else { continue };
                    match serde_json::from_slice::<ServerEvent>(trim(data)) {
                        Ok(event) => on_event(event),
                        Err(e) => tracing::debug!(error = %e, "a server event did not parse"),
                    }
                }
            }
        }
        Ok(())
    }
}

/// One thing a server said, unprompted.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerEvent {
    Notify(karmayoga_service::events::Notification),
    Changed(karmayoga_service::events::Change),
    LockState { locked: bool },
    MeetingOffer(karmayoga_service::events::MeetingOffer),
}

// ---- the cache -----------------------------------------------------------

#[derive(Default)]
struct BlobCache {
    /// Keyed by address and range, because a range is what a video asks for and
    /// the same range is what it asks for again when it seeks back.
    ranges: HashMap<(String, u64, u64), Vec<u8>>,
    /// Insertion order, for eviction. Oldest first, which is the only ordering
    /// that means anything for bytes that never change.
    order: Vec<(String, u64, u64)>,
    lengths: HashMap<String, u64>,
    bytes: usize,
}

impl BlobCache {
    fn get(&self, id: &str, offset: u64, len: u64) -> Option<Vec<u8>> {
        self.ranges.get(&(id.to_string(), offset, len)).cloned()
    }

    fn put(&mut self, id: &str, offset: u64, bytes: Vec<u8>) {
        // A single range larger than the whole budget would evict everything
        // and then not fit. Not worth caching at all.
        if bytes.len() > BLOB_CACHE_BYTES {
            return;
        }
        let key = (id.to_string(), offset, bytes.len() as u64);
        self.bytes += bytes.len();
        if self.ranges.insert(key.clone(), bytes).is_none() {
            self.order.push(key);
        }
        while self.bytes > BLOB_CACHE_BYTES && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            if let Some(gone) = self.ranges.remove(&oldest) {
                self.bytes -= gone.len();
            }
        }
    }

    fn len_of(&self, id: &str) -> Option<u64> {
        self.lengths.get(id).copied()
    }

    fn note_len(&mut self, id: &str, len: u64) {
        self.lengths.insert(id.to_string(), len);
    }

    fn clear(&mut self) {
        self.ranges.clear();
        self.order.clear();
        self.lengths.clear();
        self.bytes = 0;
    }
}

// ---- getting a connection off the ground ---------------------------------

/// Fetch a server's certificate without trusting anything.
///
/// The one moment a client talks to a machine it has no reason to believe. It
/// sends nothing -- no token, no code, not even a request -- beyond what a TLS
/// handshake requires, and what it takes away is the certificate, whose
/// fingerprint is then compared with the one the person carried. A mismatch
/// means the exchange stops before a secret exists.
async fn fetch_certificate(host: &str) -> CommandResult<String> {
    let (name, port) = split_host(host)?;
    let stream = tokio::net::TcpStream::connect((name.as_str(), port))
        .await
        .map_err(|e| CommandError::new("network", format!("could not reach {host}: {e}")))?;

    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| CommandError::new("internal", e.to_string()))?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(CollectCertificate))
    .with_no_client_auth();

    let server_name = rustls::pki_types::ServerName::try_from(name.clone())
        .map_err(|_| CommandError::new("invalid", format!("{name} is not a usable host name")))?
        .to_owned();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tls = connector.connect(server_name, stream).await.map_err(|e| {
        CommandError::new("network", format!("could not start TLS with {host}: {e}"))
    })?;

    let (_, connection) = tls.get_ref();
    let der = connection
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or_else(|| CommandError::new("network", "that computer sent no certificate"))?;
    Ok(crate::tls::der_to_pem(der))
}

/// Accepts any certificate, and is only ever used to *look* at one.
///
/// Named for what it is. The security of pairing is the fingerprint comparison
/// that follows, not this handshake, and the connection it makes is thrown away
/// without a byte of application data crossing it.
#[derive(Debug)]
struct CollectCertificate;

impl rustls::client::danger::ServerCertVerifier for CollectCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A client that trusts one certificate and nothing else.
///
/// Two settings, and both are load-bearing.
///
/// `tls_certs_only` rather than `add_root_certificate`: the latter trusts this
/// certificate *and* every public authority, which is strictly weaker than the
/// pinning was meant to be. Here there is exactly one root, and it is the one
/// the person carried across from the other screen. No authority anywhere can
/// issue a certificate this client would accept.
///
/// And the host *name* is not checked, which reads alarming and is the correct
/// model here. What identifies the far end is the certificate itself, byte for
/// byte; the name in it adds nothing on top of that. Checking it actively hurt:
/// a certificate has to name every address it might be reached at, a machine's
/// addresses change when it joins a different network, and regenerating the
/// certificate to cover a new one changes its fingerprint -- which unpairs
/// every device that pinned it. So the names are stable and ignored, and the
/// identity is the pin. See `tls::local_names`.
pub fn pinned_client(cert_pem: &str) -> CommandResult<reqwest::Client> {
    let certificate = reqwest::Certificate::from_pem(cert_pem.as_bytes())
        .map_err(|e| CommandError::new("invalid", format!("that certificate is unusable: {e}")))?;
    reqwest::Client::builder()
        .tls_certs_only([certificate])
        .tls_danger_accept_invalid_hostnames(true)
        .timeout(CALL_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("KarmaYoga/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CommandError::new("network", format!("could not start the client: {e}")))
}

/// [`client_for`], for a voice session: HTTP/1.1 only, because a WebSocket is
/// an HTTP/1.1 upgrade and an HTTP/2 connection cannot be upgraded; and with
/// no overall timeout, because a conversation lasts as long as it lasts.
fn voice_client(cert_pem: &str) -> CommandResult<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .http1_only()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("KarmaYoga/", env!("CARGO_PKG_VERSION")));
    let builder = if cert_pem.is_empty() {
        builder.https_only(true)
    } else {
        let certificate = reqwest::Certificate::from_pem(cert_pem.as_bytes()).map_err(|e| {
            CommandError::new("invalid", format!("that certificate is unusable: {e}"))
        })?;
        builder.tls_certs_only([certificate]).tls_danger_accept_invalid_hostnames(true)
    };
    builder
        .build()
        .map_err(|e| CommandError::new("network", format!("could not start the client: {e}")))
}

/// The client for a connection: pinned to `cert_pem` when there is one, and
/// checked the ordinary way when there is not.
///
/// An empty pin is what a link without a fingerprint leaves behind -- see
/// [`RemoteClient::pair`] -- and means the server is behind somebody else's
/// TLS, a reverse proxy holding a publicly issued certificate. What vouches
/// for it then is that authority, through the operating system's own trust
/// store (`reqwest`'s default verifier, `rustls-platform-verifier`), with the
/// host name checked as a browser would. `https_only` so that nothing -- a
/// redirect included -- can move a request onto plain HTTP, where nothing at
/// all would vouch for the other end.
fn client_for(cert_pem: &str) -> CommandResult<reqwest::Client> {
    if !cert_pem.is_empty() {
        return pinned_client(cert_pem);
    }
    reqwest::Client::builder()
        .https_only(true)
        .timeout(CALL_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("KarmaYoga/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CommandError::new("network", format!("could not start the client: {e}")))
}

/// Ask a server what it is, before trusting it with anything.
pub async fn hello(host: &str, cert_pem: &str) -> CommandResult<Hello> {
    let http = client_for(cert_pem)?;
    hello_with(&http, &format!("https://{host}")).await
}

async fn hello_with(http: &reqwest::Client, base: &str) -> CommandResult<Hello> {
    http.get(format!("{base}/v1/hello"))
        .send()
        .await
        .map_err(transport)?
        .error_for_body()
        .await?
        .json()
        .await
        .map_err(transport)
}

fn check_protocol(theirs: u32) -> CommandResult<()> {
    if theirs == PROTOCOL {
        return Ok(());
    }
    let older = if theirs < PROTOCOL { "that computer" } else { "this one" };
    Err(CommandError::new(
        "protocol",
        format!(
            "that copy of Karma Yoga speaks protocol {theirs} and this one speaks {PROTOCOL}. \
             Update {older}."
        ),
    ))
}

/// Where `needle` first appears in `haystack`.
/// `(start, total)` from a `Content-Range` value, `bytes 5-9/20`. `None` for a
/// range of unknown length (`bytes 5-9/*`) and for anything that is not one:
/// a server that sends no length has told this client nothing it can plan
/// the rest of a download by.
fn parse_content_range(value: &str) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = spec.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// One path segment, percent-encoded. An upload's name is opaque to this
/// client -- the service decides what one looks like -- so it is not trusted
/// to be free of anything that would end the segment or the path.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// ASCII whitespace off both ends, without decoding.
fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |n| n + 1);
    &bytes[start..end]
}

fn split_host(host: &str) -> CommandResult<(String, u16)> {
    // `[::1]:7397` as well as `10.0.0.1:7397`, because an address bar that
    // refused an IPv6 literal would be one people report as broken.
    if let Some(rest) = host.strip_prefix('[') {
        let (name, port) = rest
            .split_once("]:")
            .ok_or_else(|| CommandError::new("invalid", format!("{host} has no port")))?;
        let port = port
            .parse()
            .map_err(|_| CommandError::new("invalid", format!("{port} is not a port")))?;
        return Ok((name.to_string(), port));
    }
    match host.rsplit_once(':') {
        Some((name, port)) => {
            let port = port
                .parse()
                .map_err(|_| CommandError::new("invalid", format!("{port} is not a port")))?;
            Ok((name.to_string(), port))
        }
        None => Ok((host.to_string(), crate::DEFAULT_PORT)),
    }
}

/// A transport failure, with the address taken out of it.
///
/// `reqwest` interpolates the request URL into most of its `Display` output.
/// That matters less here than it does for a calendar URL -- the address is the
/// user's own machine -- but an error string that quotes a URL is one that ends
/// up in a log, and this application has a rule about that.
fn transport(e: reqwest::Error) -> CommandError {
    let message = karmayoga_service::http::strip_url(&e.to_string());
    if e.is_timeout() {
        CommandError::new("network", format!("that computer did not answer: {message}"))
    } else if e.is_connect() {
        CommandError::new(
            "unreachable",
            "could not reach the computer holding this vault. It may be asleep, or off this \
             network.",
        )
    } else {
        CommandError::new("network", message)
    }
}

/// Turn a non-2xx response into the `{ code, message }` the server sent.
///
/// The point of the whole error type crossing the wire: a conflict raised on
/// the other machine arrives here as `conflict` and reaches the editor's own
/// banner, rather than being flattened into "the network went wrong".
trait ErrorForBody: Sized {
    async fn error_for_body(self) -> CommandResult<Self>;
}

impl ErrorForBody for reqwest::Response {
    async fn error_for_body(self) -> CommandResult<Self> {
        if self.status().is_success() {
            return Ok(self);
        }
        let status = self.status();
        let body = self.text().await.unwrap_or_default();
        if let Ok(error) = serde_json::from_str::<CommandError>(&body) {
            return Err(error);
        }
        Err(CommandError::new(
            "network",
            format!("that computer answered {}", status.canonical_reason().unwrap_or("badly")),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_may_carry_a_port_or_not() {
        assert_eq!(split_host("10.0.0.1:7397").unwrap(), ("10.0.0.1".into(), 7397));
        assert_eq!(
            split_host("laptop.local").unwrap(),
            ("laptop.local".into(), crate::DEFAULT_PORT)
        );
        assert_eq!(split_host("[::1]:7397").unwrap(), ("::1".into(), 7397));
        assert!(split_host("10.0.0.1:not-a-port").is_err());
    }

    #[test]
    fn a_picture_is_waited_for_longer_than_it_can_take_at_the_server() {
        assert!(call_timeout("generate_image") > Duration::from_secs(10 * 60));
        assert_eq!(call_timeout("list_notes"), CALL_TIMEOUT);
    }

    #[test]
    fn a_version_mismatch_says_which_side_is_behind() {
        let older = check_protocol(PROTOCOL + 1).unwrap_err();
        assert!(older.message.contains("this one"), "{}", older.message);
        let newer = check_protocol(PROTOCOL - 1).unwrap_err();
        assert!(newer.message.contains("that computer"), "{}", newer.message);
    }

    #[test]
    fn the_cache_answers_the_range_it_was_given() {
        let mut cache = BlobCache::default();
        cache.put("abc", 100, vec![1, 2, 3, 4]);
        assert_eq!(cache.get("abc", 100, 4), Some(vec![1, 2, 3, 4]));
        // A different range of the same blob is a different entry, because a
        // partial answer to a range request is a wrong answer.
        assert_eq!(cache.get("abc", 100, 2), None);
        assert_eq!(cache.get("abc", 0, 4), None);
    }

    #[test]
    fn the_cache_stays_within_its_budget() {
        let mut cache = BlobCache::default();
        let chunk = vec![0u8; 64 * 1024 * 1024];
        for n in 0..8 {
            cache.put("v", n * 1000, chunk.clone());
        }
        assert!(cache.bytes <= BLOB_CACHE_BYTES, "{} bytes held", cache.bytes);
        // ...and the newest is still there, which is what a seek just asked for.
        assert!(cache.get("v", 7000, chunk.len() as u64).is_some());
    }

    /// A link with no fingerprint is not a licence to trust whatever answers.
    /// This server serves its own certificate, as every one not behind a
    /// proxy does, so the ordinary check refuses it -- and refuses it before
    /// the code is spent, which is what the old behaviour got wrong: it
    /// pinned the certificate and paired.
    #[tokio::test]
    async fn a_link_with_no_fingerprint_does_not_trust_whatever_answers() {
        crate::install_crypto_provider();
        let config_dir = tempfile::tempdir().unwrap();
        let config = crate::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            enabled: true,
            allow_remote_unlock: true,
            no_tls: false,
        };
        let parts = crate::prepare(config_dir.path(), &config).unwrap();
        let service = Arc::new(karmayoga_service::Service::new());
        let running = crate::start(service, parts, &config, "Test".into()).await.unwrap();

        let invite = Invite {
            host: format!("localhost:{}", running.address.port()),
            fingerprint: String::new(),
            code: running.server.registry.new_pairing_code(),
            name: "Test".into(),
        };
        assert!(
            RemoteClient::pair(&invite, "Laptop").await.is_err(),
            "a certificate nobody vouched for was trusted with no fingerprint to check"
        );
        assert!(running.server.registry.pairing_open(), "the code was spent anyway");
    }

    #[test]
    fn locking_forgets_every_byte() {
        let mut cache = BlobCache::default();
        cache.put("abc", 0, vec![9; 32]);
        cache.note_len("abc", 32);
        cache.clear();
        assert_eq!(cache.get("abc", 0, 32), None);
        assert_eq!(cache.len_of("abc"), None);
        assert_eq!(cache.bytes, 0);
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn a_content_range_says_where_a_part_starts_and_how_long_the_file_is() {
        assert_eq!(parse_content_range("bytes 0-0/1234"), Some((0, 1234)));
        assert_eq!(
            parse_content_range("bytes 4194304-8388607/20000000"),
            Some((4_194_304, 20_000_000))
        );
        // No length is no plan.
        assert_eq!(parse_content_range("bytes 0-9/*"), None);
        assert_eq!(parse_content_range("items 0-9/20"), None);
        assert_eq!(parse_content_range(""), None);
    }

    #[test]
    fn an_upload_name_cannot_end_its_segment() {
        assert_eq!(encode_segment("0192-abc_DEF.~"), "0192-abc_DEF.~");
        assert_eq!(encode_segment("a/b?c=d e"), "a%2Fb%3Fc%3Dd%20e");
        assert_eq!(encode_segment("../x"), "..%2Fx");
    }
}
