//! The other end: a copy of the application talking to a vault it does not
//! hold.
//!
//! This is what makes a remote client the *same app* rather than a viewer. It
//! offers the same three entry points the [`Service`] does -- `call`, the blob
//! pair, and `send_message` -- so the shell above it can hold either and the
//! interface never learns which.
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

use everyday_service::agent::{AgentEvent, Sink};
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::{Ctx, PROTOCOL};
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

impl RemoteClient {
    /// Pair with the server a link points at, and connect.
    ///
    /// Three things happen in order, and the order is the security of it. The
    /// certificate is fetched and its fingerprint compared with the one in the
    /// link, which the person carried from the other screen; only then is a
    /// client built that trusts that certificate and no other; only then is the
    /// one-time code spent. A mismatch stops before anything is sent.
    pub async fn pair(invite: &Invite, device_name: &str) -> CommandResult<(Self, String)> {
        let cert_pem = fetch_certificate(&invite.host).await?;
        if !invite.fingerprint.is_empty() {
            let der = crate::tls::pem_to_der(&cert_pem)?;
            let seen = crate::tls::fingerprint_of(&der);
            if !seen.eq_ignore_ascii_case(&invite.fingerprint) {
                return Err(CommandError::new(
                    "fingerprint",
                    "that computer answered with a different certificate from the one in the \
                     link. Either the link is old, or something is between you and it.",
                ));
            }
        }

        let http = pinned_client(&cert_pem)?;
        let base = format!("https://{}", invite.host);
        let hello = hello_with(&http, &base).await?;
        check_protocol(hello.protocol)?;

        let paired: Paired = http
            .post(format!("{base}/v1/pair"))
            .header("x-everyday-protocol", PROTOCOL.to_string())
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
            fingerprint: hello.fingerprint,
            cert_pem,
            paired: jiff::Timestamp::now(),
        };
        let token = paired.token.clone();
        Ok((Self::new(http, base, paired.token, connection), token))
    }

    /// Reconnect to a server this client has paired with before.
    pub async fn resume(connection: Connection, token: String) -> CommandResult<Self> {
        let http = pinned_client(&connection.cert_pem)?;
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
            .header("x-everyday-protocol", PROTOCOL.to_string())
            .header("authorization", format!("Bearer {}", self.token))
            .header("x-everyday-client", &self.connection.id)
    }

    /// Run a command on the machine holding the vault.
    pub async fn call(&self, ctx: &Ctx, name: &str, args: Value) -> CommandResult<Value> {
        let mut request = self.request(reqwest::Method::POST, &format!("/v1/call/{name}"));
        if let Some(id) = &ctx.request_id {
            request = request.header("x-everyday-request", id);
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
    Notify(everyday_service::events::Notification),
    Changed(everyday_service::events::Change),
    LockState { locked: bool },
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
        .user_agent(concat!("EveryDay/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CommandError::new("network", format!("could not start the client: {e}")))
}

/// Ask a server what it is, before trusting it with anything.
pub async fn hello(host: &str, cert_pem: &str) -> CommandResult<Hello> {
    let http = pinned_client(cert_pem)?;
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
            "that copy of Every Day speaks protocol {theirs} and this one speaks {PROTOCOL}. \
             Update {older}."
        ),
    ))
}

/// Where `needle` first appears in `haystack`.
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
    let message = everyday_service::http::strip_url(&e.to_string());
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
