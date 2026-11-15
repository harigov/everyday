//! Streamable HTTP: every message is a POST to one URL, and the answer comes
//! back either as one JSON body or as a stream of server-sent events that
//! ends with it.
//!
//! What this keeps between requests is small: the session id the server
//! minted at `initialize` (legacy era only), the agreed protocol revision,
//! and the bearer token, which a sign-in may replace while the connection
//! is open.
//!
//! Nothing here puts a header value, a token or the address's query in an
//! error or a log line: a URL can carry a key, and a header always does.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, WWW_AUTHENTICATE,
};
use serde_json::Value;

use super::{Failure, lock};
use crate::jsonrpc::{self, Incoming};
use crate::{ConnectError, sse};

/// The most of an answer read before giving up on it: a tool result is cut
/// to 40,000 characters long after this, so this is only a guard against a
/// server that never stops.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// How much of an error page is read to say what went wrong.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// How much of an error page is shown.
const SNIPPET_CHARS: usize = 200;

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const METHOD_HEADER: &str = "mcp-method";
const NAME_HEADER: &str = "mcp-name";

#[derive(Default)]
struct Session {
    id: Option<String>,
    protocol: Option<String>,
}

pub struct Http {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    bearer: Mutex<Option<String>>,
    session: Mutex<Session>,
    /// The stateless era: no session, and `Mcp-Method` and `Mcp-Name` on
    /// every request.
    modern: AtomicBool,
    next_id: AtomicI64,
    tools_changed: AtomicBool,
    closed: AtomicBool,
}

impl Http {
    pub fn new(
        url: String,
        headers: Vec<(String, String)>,
        bearer: Option<String>,
        connect_timeout: Duration,
    ) -> Result<Self, ConnectError> {
        let parsed = url::Url::parse(&url).map_err(|_| {
            ConnectError::Transport("the server's address is not a valid URL".into())
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ConnectError::Transport(
                "the server's address must start with http:// or https://".into(),
            ));
        }
        // No total timeout: a reply may be a stream that stays open. The
        // client bounds each request itself. Redirects are not followed: a
        // POST that is sent somewhere else would take the custom headers
        // with it, and the address a person typed is the one they trust.
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("karmayoga-connect/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ConnectError::Transport("could not set up the HTTP client".into()))?;
        Ok(Self {
            client,
            url,
            headers,
            bearer: Mutex::new(bearer),
            session: Mutex::new(Session::default()),
            modern: AtomicBool::new(false),
            next_id: AtomicI64::new(0),
            tools_changed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    pub fn use_protocol(&self, version: &str, modern: bool) {
        lock(&self.session).protocol = Some(version.to_string());
        self.modern.store(modern, Ordering::Relaxed);
    }

    pub fn session_id(&self) -> Option<String> {
        lock(&self.session).id.clone()
    }

    pub fn reset_session(&self) {
        *lock(&self.session) = Session::default();
    }

    pub fn set_bearer(&self, token: Option<String>) {
        *lock(&self.bearer) = token;
    }

    pub fn tools_changed(&self) -> bool {
        self.tools_changed.load(Ordering::Relaxed)
    }

    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::Relaxed)
    }

    pub fn is_alive(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }

    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let tool = (method == "tools/call")
            .then(|| {
                params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .flatten();
        let body = jsonrpc::request(id, method, params);
        let mut response = self.post(&body, method, tool.as_deref()).await?;

        if method == "initialize"
            && let Some(session) =
                response.headers().get(SESSION_HEADER).and_then(|v| v.to_str().ok())
        {
            lock(&self.session).id = Some(session.to_string());
        }

        let streamed = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.to_ascii_lowercase().contains("text/event-stream"));
        if streamed {
            return self.read_stream(&mut response, id).await;
        }

        let (bytes, truncated) = read_body(&mut response, MAX_BODY).await?;
        if truncated {
            return Err(
                ConnectError::Protocol("the server's answer was far too large".into()).into()
            );
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ConnectError::Protocol("the server's answer was not valid JSON".into()))?;
        let messages = match value {
            Value::Array(batch) => batch,
            one => vec![one],
        };
        self.settle(messages, id).unwrap_or_else(|| {
            Err(ConnectError::Protocol(
                "the server's answer held no response to the request".into(),
            )
            .into())
        })
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), Failure> {
        let body = jsonrpc::notification(method, params);
        // The answer is 202 with no body; anything else that is a success is
        // not worth reading either.
        self.post(&body, method, None).await.map(|_| ())
    }

    /// End a legacy session. A server may refuse (405) or have forgotten it;
    /// either way the client is done.
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::Relaxed) || self.modern.load(Ordering::Relaxed) {
            return;
        }
        let Some(session) = self.session_id() else { return };
        let Ok(mut headers) = self.headers_for(None, None, false) else { return };
        if let Ok(value) = HeaderValue::from_str(&session) {
            headers.insert(HeaderName::from_static(SESSION_HEADER), value);
        }
        let delete = self.client.delete(&self.url).headers(headers).send();
        let _ = tokio::time::timeout(Duration::from_secs(5), delete).await;
    }

    /// POST one message and check the status. Returns the response only when
    /// it is a success.
    async fn post(
        &self,
        body: &Value,
        method: &str,
        tool: Option<&str>,
    ) -> Result<reqwest::Response, Failure> {
        let headers = self.headers_for(Some(method), tool, true)?;
        let sent_session = lock(&self.session).id.clone();
        let bytes = serde_json::to_vec(body)
            .map_err(|_| ConnectError::Protocol("could not encode a request".into()))?;
        let mut response = self
            .client
            .post(&self.url)
            .headers(headers)
            .body(bytes)
            .send()
            .await
            .map_err(|e| ConnectError::Transport(describe(e)))?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let authenticate = response
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ");
        if status.as_u16() == 401
            || (status.as_u16() == 403 && authenticate.contains("insufficient_scope"))
        {
            let (resource_metadata, scope) = crate::oauth::parse_www_authenticate(&authenticate);
            return Err(ConnectError::Unauthorized { resource_metadata, scope }.into());
        }
        if status.as_u16() == 404
            && let Some(session) = sent_session
        {
            return Err(Failure::SessionExpired(session));
        }
        if status.is_redirection() {
            return Err(ConnectError::Transport(format!(
                "the server answered {status}: it redirects elsewhere, so use the address it redirects to"
            ))
            .into());
        }

        // A modern server refuses a version it does not speak with HTTP 400
        // and a JSON-RPC error in the body, which is an answer, not noise.
        let (bytes, _) = read_body(&mut response, MAX_ERROR_BODY).await.unwrap_or_default();
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && let Ok(Incoming::Response { outcome: Err(error), .. }) = jsonrpc::parse(value)
        {
            return Err(ConnectError::from(error).into());
        }
        let snippet =
            String::from_utf8_lossy(&bytes).split_whitespace().collect::<Vec<_>>().join(" ");
        let snippet: String = snippet.chars().take(SNIPPET_CHARS).collect();
        let message = if snippet.is_empty() {
            format!("the server answered {status}")
        } else {
            format!("the server answered {status}: {snippet}")
        };
        Err(ConnectError::Transport(message).into())
    }

    /// The headers every request carries. `json_body` is false for the
    /// body-less DELETE.
    fn headers_for(
        &self,
        method: Option<&str>,
        tool: Option<&str>,
        json_body: bool,
    ) -> Result<HeaderMap, ConnectError> {
        let invalid = || ConnectError::Transport("a header name or value is not valid".into());
        let mut map = HeaderMap::new();
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
            let mut value = HeaderValue::from_str(value).map_err(|_| invalid())?;
            value.set_sensitive(true);
            map.insert(name, value);
        }
        map.insert(ACCEPT, HeaderValue::from_static("application/json, text/event-stream"));
        if json_body {
            map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if let Some(token) = lock(&self.bearer).as_deref() {
            let mut value =
                HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| invalid())?;
            value.set_sensitive(true);
            map.insert(AUTHORIZATION, value);
        }
        {
            let session = lock(&self.session);
            if let Some(id) = session.id.as_deref().and_then(|id| HeaderValue::from_str(id).ok()) {
                map.insert(HeaderName::from_static(SESSION_HEADER), id);
            }
            if let Some(version) =
                session.protocol.as_deref().and_then(|v| HeaderValue::from_str(v).ok())
            {
                map.insert(HeaderName::from_static(PROTOCOL_HEADER), version);
            }
        }
        if self.modern.load(Ordering::Relaxed) {
            if let Some(method) = method.and_then(|m| HeaderValue::from_str(m).ok()) {
                map.insert(HeaderName::from_static(METHOD_HEADER), method);
            }
            if let Some(name) = tool.and_then(|t| HeaderValue::from_str(&header_safe(t)).ok()) {
                map.insert(HeaderName::from_static(NAME_HEADER), name);
            }
        }
        Ok(map)
    }

    /// Read events until the response to request `id` arrives.
    async fn read_stream(
        &self,
        response: &mut reqwest::Response,
        id: i64,
    ) -> Result<Value, Failure> {
        let mut parser = sse::Parser::new();
        let mut total = 0usize;
        loop {
            let chunk = response.chunk().await.map_err(|e| ConnectError::Transport(describe(e)))?;
            let (events, ended) = match chunk {
                Some(bytes) => {
                    total += bytes.len();
                    if total > MAX_BODY {
                        return Err(ConnectError::Protocol(
                            "the server's answer was far too large".into(),
                        )
                        .into());
                    }
                    (parser.push(&bytes), false)
                }
                None => (parser.finish().into_iter().collect(), true),
            };
            for event in events {
                if event.name.as_deref().is_some_and(|n| n != "message")
                    || event.data.trim().is_empty()
                {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                    tracing::debug!("a server-sent event was not JSON");
                    continue;
                };
                let messages = match value {
                    Value::Array(batch) => batch,
                    one => vec![one],
                };
                if let Some(outcome) = self.settle(messages, id) {
                    return outcome;
                }
            }
            if ended {
                return Err(ConnectError::Protocol(
                    "the server's stream ended without answering".into(),
                )
                .into());
            }
        }
    }

    /// Go through what arrived: the answer to `id` if it is there, and
    /// meanwhile note a tools-changed announcement. A request the server
    /// makes mid-stream is not answered -- there is nowhere to POST the
    /// answer that the server could pair with it -- and is left alone.
    fn settle(&self, messages: Vec<Value>, id: i64) -> Option<Result<Value, Failure>> {
        for message in messages {
            match jsonrpc::parse(message) {
                Ok(Incoming::Response { id: answered, outcome }) => {
                    if jsonrpc::id_matches(&answered, id) {
                        return Some(outcome.map_err(|e| Failure::Error(e.into())));
                    }
                    tracing::debug!("ignoring an answer to a request that was not asked");
                }
                Ok(Incoming::Notification { method }) => {
                    if method == "notifications/tools/list_changed" {
                        self.tools_changed.store(true, Ordering::Relaxed);
                    }
                }
                Ok(Incoming::Request { method, .. }) => {
                    tracing::debug!(method, "ignoring a request the server made in a stream");
                }
                Err(_) => tracing::debug!("ignoring a message that was not JSON-RPC"),
            }
        }
        None
    }
}

/// A tool name as an `Mcp-Name` header value: as it is when it is made of
/// visible ASCII, else wrapped in Base64 so that it survives as a header.
fn header_safe(name: &str) -> String {
    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic()) {
        name.to_string()
    } else {
        format!("=?base64?{}?=", STANDARD.encode(name))
    }
}

/// Read a body up to `max` bytes. The flag says it was cut.
async fn read_body(
    response: &mut reqwest::Response,
    max: usize,
) -> Result<(Vec<u8>, bool), ConnectError> {
    let mut body = Vec::new();
    while let Some(chunk) =
        response.chunk().await.map_err(|e| ConnectError::Transport(describe(e)))?
    {
        let room = max.saturating_sub(body.len());
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

/// What went wrong with a request, in words, and nothing that could be
/// secret: not the address (its query may carry a key), not a header.
fn describe(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut chain = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(&error);
    while let Some(e) = source {
        chain.push_str(&e.to_string().to_ascii_lowercase());
        chain.push(' ');
        source = e.source();
    }
    if error.is_timeout() {
        "the server did not answer in time".into()
    } else if chain.contains("certificate") || chain.contains("tls") || chain.contains("ssl") {
        "could not make a secure connection to the server (a TLS or certificate problem)".into()
    } else if error.is_connect() {
        "could not connect to the server (the connection was refused, or the address could not be reached)".into()
    } else if error.is_builder() {
        "the server's address or a header is not valid".into()
    } else if error.is_body() || error.is_decode() {
        "the connection to the server was interrupted while it was answering".into()
    } else {
        "the request to the server failed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_go_in_as_they_are() {
        assert_eq!(header_safe("search_issues"), "search_issues");
        assert_eq!(header_safe("a.b-c"), "a.b-c");
    }

    #[test]
    fn other_names_are_wrapped() {
        assert_eq!(
            header_safe("caf\u{e9}"),
            format!("=?base64?{}?=", STANDARD.encode("caf\u{e9}"))
        );
        assert!(header_safe("two words").starts_with("=?base64?"));
    }

    #[test]
    fn an_address_that_is_not_http_is_refused() {
        let refused =
            |url: &str| Http::new(url.into(), vec![], None, Duration::from_secs(1)).is_err();
        assert!(refused("not a url"));
        assert!(refused("ftp://example.com/mcp"));
        assert!(!refused("https://example.com/mcp"));
    }
}
