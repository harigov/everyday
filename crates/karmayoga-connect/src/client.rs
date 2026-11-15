//! One open connection to one MCP server.
//!
//! See `docs/plans/connections.md`'s "Protocol". The client speaks the
//! legacy era (an `initialize` handshake, and over HTTP a session) unless a
//! server refuses that and names the modern one, in which case every request
//! after a `server/discover` carries its own `_meta` instead.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::transport::{Failure, Transport};
use crate::{
    ClientOptions, ConnectError, Endpoint, LEGACY_VERSION, MAX_RESULT_CHARS, MODERN_VERSION,
    SUPPORTED_VERSIONS,
};

/// The most pages of `tools/list` followed: a server that never stops
/// paging is broken, and this is how that is told from a long list.
const MAX_PAGES: usize = 50;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Who answered, as the server named itself in its handshake.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// The revision agreed on: the legacy one `initialize` settled, or
    /// [`crate::MODERN_VERSION`].
    pub protocol_version: String,
    pub instructions: Option<String>,
}

/// One tool, as `tools/list` described it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    /// The `inputSchema` exactly as sent; `{"type":"object"}` when absent.
    pub input_schema: Value,
    pub annotations: ToolAnnotations,
}

/// A tool's `annotations` (and the `title` inside them, folded into
/// [`Tool::title`] when the tool has none of its own).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

/// What `tools/call` answered, rendered for a model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// The server's `isError`: the tool ran and failed, and `text` says why.
    pub is_error: bool,
    /// Every content block rendered as the plan says, joined by blank lines,
    /// cut at [`crate::MAX_RESULT_CHARS`] with a note when longer.
    pub text: String,
    /// `structuredContent`, if the server sent any.
    pub structured: Option<Value>,
}

/// An open connection. Cheap to share behind an `Arc`: every method takes
/// `&self`, and requests may be in flight concurrently.
pub struct Client {
    server: ServerInfo,
    transport: Transport,
    options: ClientOptions,
    /// The stateless era: every request carries `_meta`.
    modern: bool,
    /// Held while a lost session is replaced, so that requests which all
    /// found it gone start one new session between them, not one each.
    renewing: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl Client {
    /// Open the connection and complete the handshake: spawn the program or
    /// reach the URL, `initialize` (falling forward to the modern era when
    /// the server asks for it), `notifications/initialized`. Fails with
    /// [`ConnectError::Unauthorized`] when an HTTP server wants a sign-in.
    pub async fn connect(
        endpoint: Endpoint,
        options: ClientOptions,
    ) -> Result<Client, ConnectError> {
        // The timeout covers spawning and the handshake alike; a program
        // that was started and then abandoned is killed with its handle.
        let opened = tokio::time::timeout(options.connect_timeout, async {
            let transport = Transport::open(endpoint, &options)?;
            let (server, modern) = handshake(&transport, &options).await?;
            Ok::<_, ConnectError>((transport, server, modern))
        })
        .await
        .map_err(|_| ConnectError::Timeout)?;
        let (transport, server, modern) = opened?;
        Ok(Client {
            server,
            transport,
            options,
            modern,
            renewing: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        })
    }

    /// What the server said about itself.
    pub fn server(&self) -> &ServerInfo {
        &self.server
    }

    /// Every tool, following `nextCursor` to the end. Clears
    /// [`Self::tools_changed`].
    pub async fn list_tools(&self) -> Result<Vec<Tool>, ConnectError> {
        // Cleared first, so that a change announced while listing is not
        // lost: the list may already be stale when it arrives.
        self.transport.take_tools_changed();
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.rpc("tools/list", params).await?;
            if let Some(listed) = page.get("tools").and_then(Value::as_array) {
                tools.extend(listed.iter().filter_map(tool_from_json));
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        tracing::warn!(pages = MAX_PAGES, "a server's tool list did not end; the rest is ignored");
        Ok(tools)
    }

    /// Call `name` with `arguments` (a JSON object). A tool that ran and
    /// failed is `Ok` with [`ToolResult::is_error`]; `Err` is reserved for a
    /// call that never got an answer or was refused as a request.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolResult, ConnectError> {
        let arguments = match arguments {
            Value::Null => json!({}),
            object @ Value::Object(_) => object,
            _ => {
                return Err(ConnectError::Protocol(
                    "a tool's arguments must be a JSON object".into(),
                ));
            }
        };
        let result =
            self.rpc("tools/call", json!({ "name": name, "arguments": arguments })).await?;
        Ok(render_result(&result))
    }

    /// Whether the server said its tools changed since the last
    /// [`Self::list_tools`].
    pub fn tools_changed(&self) -> bool {
        self.transport.tools_changed()
    }

    /// Whether the connection can still carry a request: false once a stdio
    /// program has exited or the client was closed. An HTTP client is alive
    /// until closed; a dead session is found by the next request.
    pub fn is_alive(&self) -> bool {
        !self.closed.load(Ordering::Relaxed) && self.transport.is_alive()
    }

    /// Replace the bearer token sent from now on -- a refreshed one -- for an
    /// HTTP connection. A no-op for stdio.
    pub fn set_bearer(&self, token: Option<String>) {
        self.transport.set_bearer(token);
    }

    /// End the connection: terminate a legacy HTTP session with `DELETE`
    /// (ignoring the answer), or close a program's stdin and kill it if it
    /// has not exited within two seconds.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.transport.close().await;
    }

    /// One request, bounded by the call timeout, with `_meta` added in the
    /// modern era.
    async fn rpc(&self, method: &str, mut params: Value) -> Result<Value, ConnectError> {
        if self.modern
            && let Some(map) = params.as_object_mut()
        {
            map.insert("_meta".into(), meta(&self.options));
        }
        tokio::time::timeout(self.options.call_timeout, self.send(method, params))
            .await
            .map_err(|_| ConnectError::Timeout)?
    }

    /// Send, and if the server has lost the session this request carried,
    /// start another and send once more.
    async fn send(&self, method: &str, params: Value) -> Result<Value, ConnectError> {
        match self.transport.request(method, Some(params.clone())).await {
            Err(Failure::SessionExpired(lost)) => {
                self.renew(&lost).await?;
                self.transport.request(method, Some(params)).await.map_err(Failure::into_error)
            }
            other => other.map_err(Failure::into_error),
        }
    }

    /// Replace the session `lost`, unless a request that found it gone
    /// first has already.
    async fn renew(&self, lost: &str) -> Result<(), ConnectError> {
        let _turn = self.renewing.lock().await;
        if self.transport.session_id().as_deref() != Some(lost) {
            return Ok(());
        }
        self.transport.reset_session();
        let (_, modern) = handshake(&self.transport, &self.options).await?;
        if modern {
            // Not a session at all any more; a fresh connection starts over
            // in the era the server now speaks.
            return Err(ConnectError::Protocol(
                "the server changed protocol revision; reconnect".into(),
            ));
        }
        Ok(())
    }
}

/// `initialize`, and the modern fallback when the server will not.
async fn handshake(
    transport: &Transport,
    options: &ClientOptions,
) -> Result<(ServerInfo, bool), ConnectError> {
    let params = json!({
        "protocolVersion": LEGACY_VERSION,
        "capabilities": {},
        "clientInfo": client_info(options),
    });
    match transport.request("initialize", Some(params)).await.map_err(Failure::into_error) {
        Ok(result) => {
            let version = result
                .get("protocolVersion")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ConnectError::Protocol(
                        "the server's answer to initialize named no protocol version".into(),
                    )
                })?
                .to_string();
            // The modern revision has no `initialize`; a server naming it
            // here is confused, and so would this client be.
            if !SUPPORTED_VERSIONS[1..].contains(&version.as_str()) {
                return Err(ConnectError::Protocol(format!(
                    "the server chose protocol revision {version}, which this client does not speak"
                )));
            }
            transport.use_protocol(&version, false);
            transport.notify("notifications/initialized", None).await?;
            let server = server_info(&result, version);
            Ok((server, false))
        }
        Err(ConnectError::Rpc { code: -32022, data, .. }) if wants_modern(data.as_ref()) => {
            transport.use_protocol(MODERN_VERSION, true);
            let found = transport
                .request("server/discover", Some(json!({ "_meta": meta(options) })))
                .await
                .map_err(Failure::into_error)?;
            let server = server_info(&found, MODERN_VERSION.to_string());
            Ok((server, true))
        }
        Err(e) => Err(e),
    }
}

/// Whether the `data` of a `-32022` lists a revision at or after the modern
/// one. Revisions are dates, so comparing them as text orders them.
fn wants_modern(data: Option<&Value>) -> bool {
    data.and_then(|d| d.get("supported")).and_then(Value::as_array).is_some_and(|versions| {
        versions
            .iter()
            .filter_map(Value::as_str)
            .any(|v| v.as_bytes().first().is_some_and(u8::is_ascii_digit) && v >= MODERN_VERSION)
    })
}

fn client_info(options: &ClientOptions) -> Value {
    json!({ "name": options.name, "version": options.version })
}

/// What a modern request carries in place of a session.
fn meta(options: &ClientOptions) -> Value {
    json!({
        META_VERSION: MODERN_VERSION,
        META_CLIENT_INFO: client_info(options),
        META_CAPABILITIES: {},
    })
}

/// The server's self-description from either era's answer: legacy has
/// `serverInfo` in the result, modern in `_meta`. `instructions` is in the
/// result in both.
fn server_info(result: &Value, protocol_version: String) -> ServerInfo {
    let info = result
        .get("serverInfo")
        .or_else(|| result.get("_meta").and_then(|m| m.get(META_SERVER_INFO)));
    let text = |key: &str| {
        info.and_then(|i| i.get(key)).and_then(Value::as_str).unwrap_or_default().to_string()
    };
    ServerInfo {
        name: text("name"),
        version: text("version"),
        protocol_version,
        instructions: result
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

/// One entry of `tools/list`, or `None` if it has no name to call it by.
fn tool_from_json(value: &Value) -> Option<Tool> {
    let name = value.get("name").and_then(Value::as_str).filter(|n| !n.is_empty())?.to_string();
    let annotations = value.get("annotations");
    let hint = |key: &str| annotations.and_then(|a| a.get(key)).and_then(Value::as_bool);
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| annotations.and_then(|a| a.get("title")).and_then(Value::as_str))
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let input_schema = match value.get("inputSchema") {
        Some(schema) if schema.is_object() => schema.clone(),
        _ => json!({ "type": "object" }),
    };
    Some(Tool {
        name,
        title,
        description: value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_schema,
        annotations: ToolAnnotations {
            read_only: hint("readOnlyHint"),
            destructive: hint("destructiveHint"),
            idempotent: hint("idempotentHint"),
            open_world: hint("openWorldHint"),
        },
    })
}

/// A `tools/call` result as text for a model.
fn render_result(result: &Value) -> ToolResult {
    let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let structured = result.get("structuredContent").filter(|s| !s.is_null()).cloned();

    let mut parts: Vec<String> = Vec::new();
    let mut has_text = false;
    let blocks =
        result.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for block in blocks {
        let field = |key: &str| block.get(key).and_then(Value::as_str);
        match field("type") {
            Some("text") => {
                if let Some(text) = field("text").filter(|t| !t.is_empty()) {
                    has_text = true;
                    parts.push(text.to_string());
                }
            }
            Some("resource") => {
                let Some(resource) = block.get("resource") else { continue };
                let inner = |key: &str| resource.get(key).and_then(Value::as_str);
                let uri = inner("uri").unwrap_or_default();
                if let Some(text) = inner("text") {
                    has_text = true;
                    parts.push(if uri.is_empty() {
                        text.to_string()
                    } else {
                        format!("{uri}\n{text}")
                    });
                } else if let Some(blob) = inner("blob") {
                    let mime = inner("mimeType").unwrap_or("unknown type");
                    parts.push(format!("[resource: {uri}, {mime}, {} KB]", kilobytes(blob)));
                }
            }
            Some("resource_link") => {
                let uri = field("uri").unwrap_or_default();
                let label = field("title")
                    .filter(|t| !t.is_empty())
                    .or_else(|| field("name"))
                    .unwrap_or_default();
                let line = format!("{label} {uri}");
                parts.push(line.trim().to_string());
            }
            Some(kind @ ("image" | "audio")) => {
                let mime = field("mimeType").unwrap_or("unknown type");
                let size = kilobytes(field("data").unwrap_or_default());
                parts.push(format!("[{kind}: {mime}, {size} KB]"));
            }
            _ => {}
        }
    }
    if !has_text && let Some(structured) = &structured {
        parts.push(serde_json::to_string_pretty(structured).unwrap_or_default());
    }

    ToolResult { is_error, text: cut(parts.join("\n\n")), structured }
}

/// The size of the bytes a Base64 string stands for, in whole kilobytes,
/// rounded up so that something is never "0 KB".
fn kilobytes(base64: &str) -> usize {
    (base64.len().saturating_mul(3) / 4).div_ceil(1024).max(1)
}

/// Cut `text` to [`MAX_RESULT_CHARS`] characters, with a note when it was
/// longer.
fn cut(text: String) -> String {
    let Some((end, _)) = text.char_indices().nth(MAX_RESULT_CHARS) else { return text };
    let total = text.chars().count();
    format!("{}\n[\u{2026} cut: the result was {total} characters long]", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_each_kind_of_block_in_order() {
        let result = json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "image", "data": "A".repeat(46_400), "mimeType": "image/png" },
                { "type": "resource", "resource": { "uri": "file:///a.txt", "text": "body" } },
                { "type": "resource", "resource": { "uri": "file:///b.bin", "mimeType": "application/zip", "blob": "AAAA" } },
                { "type": "resource_link", "uri": "https://x/y", "name": "n", "title": "The Title" },
                { "type": "audio", "data": "AAAA", "mimeType": "audio/wav" },
                { "type": "mystery" },
            ]
        });
        let rendered = render_result(&result);
        assert!(!rendered.is_error);
        assert_eq!(
            rendered.text,
            "hello\n\n[image: image/png, 34 KB]\n\nfile:///a.txt\nbody\n\n[resource: file:///b.bin, application/zip, 1 KB]\n\nThe Title https://x/y\n\n[audio: audio/wav, 1 KB]"
        );
    }

    #[test]
    fn structured_content_stands_in_only_when_there_is_no_text() {
        let only = render_result(&json!({ "content": [], "structuredContent": { "a": 1 } }));
        assert_eq!(only.text, "{\n  \"a\": 1\n}");
        assert_eq!(only.structured, Some(json!({ "a": 1 })));

        let both = render_result(
            &json!({ "content": [{ "type": "text", "text": "t" }], "structuredContent": { "a": 1 } }),
        );
        assert_eq!(both.text, "t");
        assert!(both.structured.is_some());
    }

    #[test]
    fn is_error_is_carried() {
        let failed = render_result(
            &json!({ "isError": true, "content": [{ "type": "text", "text": "nope" }] }),
        );
        assert!(failed.is_error);
        assert_eq!(failed.text, "nope");
    }

    #[test]
    fn long_results_are_cut_on_a_character_boundary() {
        let long = "\u{e9}".repeat(MAX_RESULT_CHARS + 25);
        let rendered = render_result(&json!({ "content": [{ "type": "text", "text": long }] }));
        let (kept, note) = rendered.text.split_once('\n').unwrap();
        assert_eq!(kept.chars().count(), MAX_RESULT_CHARS);
        assert_eq!(
            note,
            format!("[\u{2026} cut: the result was {} characters long]", MAX_RESULT_CHARS + 25)
        );

        let exact = "x".repeat(MAX_RESULT_CHARS);
        assert_eq!(cut(exact.clone()), exact);
    }

    #[test]
    fn tools_are_read_with_their_defaults() {
        let tool = tool_from_json(&json!({
            "name": "search",
            "annotations": { "title": "Search it", "readOnlyHint": true, "destructiveHint": false }
        }))
        .unwrap();
        assert_eq!(tool.title.as_deref(), Some("Search it"));
        assert_eq!(tool.description, "");
        assert_eq!(tool.input_schema, json!({ "type": "object" }));
        assert_eq!(tool.annotations.read_only, Some(true));
        assert_eq!(tool.annotations.destructive, Some(false));
        assert_eq!(tool.annotations.idempotent, None);

        let own_title = tool_from_json(&json!({ "name": "t", "title": "Own", "inputSchema": null, "annotations": { "title": "Other" } })).unwrap();
        assert_eq!(own_title.title.as_deref(), Some("Own"));
        assert_eq!(own_title.input_schema, json!({ "type": "object" }));

        assert!(tool_from_json(&json!({ "description": "no name" })).is_none());
    }

    #[test]
    fn modern_is_asked_for_by_a_listed_revision() {
        assert!(wants_modern(Some(&json!({ "supported": ["2026-07-28", "2025-11-25"] }))));
        assert!(wants_modern(Some(&json!({ "supported": ["2027-01-01"] }))));
        assert!(!wants_modern(Some(&json!({ "supported": ["2025-11-25"] }))));
        assert!(!wants_modern(Some(&json!({ "supported": ["draft"] }))));
        assert!(!wants_modern(Some(&json!({}))));
        assert!(!wants_modern(None));
    }

    #[test]
    fn server_info_reads_either_era() {
        let legacy =
            json!({ "serverInfo": { "name": "s", "version": "1" }, "instructions": "use me" });
        let info = server_info(&legacy, "2025-11-25".into());
        assert_eq!((info.name.as_str(), info.version.as_str()), ("s", "1"));
        assert_eq!(info.instructions.as_deref(), Some("use me"));

        let modern = json!({ "_meta": { META_SERVER_INFO: { "name": "m", "version": "2" } } });
        let info = server_info(&modern, MODERN_VERSION.into());
        assert_eq!(info.name, "m");
        assert_eq!(info.instructions, None);
    }
}
