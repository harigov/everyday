//! The client against small servers of its own making: an axum one on a
//! loopback port that speaks both eras of Streamable HTTP, and a Python one
//! on stdio. Nothing here leaves 127.0.0.1.

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use karmayoga_connect::{Client, ClientOptions, ConnectError, Endpoint, MODERN_VERSION};
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// The HTTP server

/// What the server saw of one POST.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    params: Value,
    session: Option<String>,
    protocol: Option<String>,
    mcp_method: Option<String>,
    mcp_name: Option<String>,
}

#[derive(Default)]
struct Server {
    seen: Mutex<Vec<Seen>>,
    deletes: Mutex<Vec<String>>,
    /// The number of the session currently good: `sess-<generation>`.
    generation: AtomicUsize,
}

impl Server {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn seen_method(&self, method: &str) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.method == method).collect()
    }

    /// Forget every session handed out, without telling any client.
    fn expire(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Flavor {
    /// Legacy era, JSON replies.
    Json,
    /// Legacy era, every reply a stream of events.
    Sse,
    /// Modern era; refuses `initialize`.
    Modern,
    /// Legacy era, JSON replies, but only to `Authorization: Bearer good`.
    Bearer,
}

fn header_of(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn json_response(status: StatusCode, body: &Value, extra: &[(&str, String)]) -> Response {
    let mut builder =
        Response::builder().status(status).header(header::CONTENT_TYPE, "application/json");
    for (name, value) in extra {
        builder = builder.header(*name, value.as_str());
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn sse_response(chunks: Vec<String>, extra: &[(&str, String)]) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream");
    for (name, value) in extra {
        builder = builder.header(*name, value.as_str());
    }
    let stream = futures::stream::iter(chunks.into_iter().map(Ok::<_, Infallible>));
    builder.body(Body::from_stream(stream)).unwrap()
}

fn envelope(id: &Value, outcome: Result<Value, (i64, String)>) -> Value {
    match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    }
}

fn tools_page(cursor: Option<&str>) -> Value {
    match cursor {
        None => json!({
            "tools": [{
                "name": "alpha",
                "title": "Alpha",
                "description": "The first tool",
                "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } },
                "annotations": { "readOnlyHint": true, "openWorldHint": false }
            }],
            "nextCursor": "page-2"
        }),
        Some(_) => json!({
            "tools": [{ "name": "beta", "description": "The second", "inputSchema": null }]
        }),
    }
}

fn call_result(name: &str, arguments: &Value) -> Result<Value, (i64, String)> {
    match name {
        "mixed" => Ok(json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "image", "data": "AAAA", "mimeType": "image/png" },
                { "type": "text", "text": "bye" }
            ]
        })),
        "long" => Ok(json!({ "content": [{ "type": "text", "text": "x".repeat(50_000) }] })),
        "broken" => {
            Ok(json!({ "isError": true, "content": [{ "type": "text", "text": "it failed" }] }))
        }
        "structured" => Ok(json!({ "content": [], "structuredContent": { "n": 1 } })),
        "echo" => Ok(json!({ "content": [{ "type": "text", "text": arguments.to_string() }] })),
        "alpha" => Ok(json!({ "content": [{ "type": "text", "text": "ok" }] })),
        other => Err((-32602, format!("unknown tool {other}"))),
    }
}

fn handle(server: &Server, flavor: Flavor, headers: &HeaderMap, body: &str) -> Response {
    let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let method = message["method"].as_str().unwrap_or_default().to_string();
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    server.seen.lock().unwrap().push(Seen {
        method: method.clone(),
        params: params.clone(),
        session: header_of(headers, "mcp-session-id"),
        protocol: header_of(headers, "mcp-protocol-version"),
        mcp_method: header_of(headers, "mcp-method"),
        mcp_name: header_of(headers, "mcp-name"),
    });

    if flavor == Flavor::Bearer
        && header_of(headers, "authorization").as_deref() != Some("Bearer good")
    {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, "Bearer realm=\"test\"")
            .body(Body::empty())
            .unwrap();
    }
    if method.starts_with("notifications/") {
        return Response::builder().status(StatusCode::ACCEPTED).body(Body::empty()).unwrap();
    }

    if flavor == Flavor::Modern {
        return handle_modern(&method, &id, &params, headers);
    }

    let generation = server.generation.load(Ordering::SeqCst);
    if method != "initialize" {
        match header_of(headers, "mcp-session-id") {
            Some(session) if session == format!("sess-{generation}") => {}
            Some(_) => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap();
            }
            None => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from("no session"))
                    .unwrap();
            }
        }
    }

    let mut extra = Vec::new();
    let mut streamed_first = Vec::new();
    let outcome = match method.as_str() {
        "initialize" => {
            let next = server.generation.fetch_add(1, Ordering::SeqCst) + 1;
            extra.push(("mcp-session-id", format!("sess-{next}")));
            Ok(json!({
                "protocolVersion": params["protocolVersion"],
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": "legacy-fake", "version": "0.3" },
                "instructions": "hello"
            }))
        }
        "tools/list" => Ok(tools_page(params.get("cursor").and_then(Value::as_str))),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            if name == "mixed" {
                streamed_first.push(
                    json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }),
                );
            }
            call_result(name, &params["arguments"])
        }
        other => Err((-32601, format!("no method {other}"))),
    };
    let answer = envelope(&id, outcome);

    if flavor == Flavor::Sse {
        // A comment, any notifications, then the answer cut mid-line.
        let mut chunks = vec![": warming up\n\n".to_string()];
        for note in streamed_first {
            chunks.push(format!("event: message\ndata: {note}\n\n"));
        }
        let event = format!("event: message\ndata: {answer}\n\n");
        let (head, tail) = event.split_at(event.len() / 2);
        chunks.push(head.to_string());
        chunks.push(tail.to_string());
        sse_response(chunks, &extra)
    } else {
        json_response(StatusCode::OK, &answer, &extra)
    }
}

fn handle_modern(method: &str, id: &Value, params: &Value, headers: &HeaderMap) -> Response {
    let refuse = |status: StatusCode, code: i64, message: &str, data: Value| {
        let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message, "data": data } });
        json_response(status, &body, &[])
    };
    if method == "initialize" {
        return refuse(
            StatusCode::BAD_REQUEST,
            -32022,
            "Unsupported protocol version",
            json!({ "supported": ["2026-07-28", "2025-11-25"], "requested": "2025-11-25" }),
        );
    }
    let meta = &params["_meta"];
    if meta["io.modelcontextprotocol/protocolVersion"] != json!(MODERN_VERSION)
        || meta["io.modelcontextprotocol/clientCapabilities"].is_null()
    {
        return refuse(StatusCode::BAD_REQUEST, -32602, "missing _meta", json!({}));
    }
    if header_of(headers, "mcp-method").as_deref() != Some(method) {
        return refuse(StatusCode::BAD_REQUEST, -32020, "Mcp-Method does not match", json!({}));
    }
    let outcome = match method {
        "server/discover" => Ok(json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": { "tools": {} },
            "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "modern-fake", "version": "9" } },
            "instructions": "be nice"
        })),
        "tools/list" => Ok(
            json!({ "resultType": "complete", "tools": [{ "name": "alpha", "inputSchema": { "type": "object" } }] }),
        ),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            if header_of(headers, "mcp-name").as_deref() != Some(name) {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    -32020,
                    "Mcp-Name does not match",
                    json!({}),
                );
            }
            call_result(name, &params["arguments"]).map(|mut result| {
                result["resultType"] = json!("complete");
                result
            })
        }
        other => Err((-32601, format!("no method {other}"))),
    };
    json_response(StatusCode::OK, &envelope(id, outcome), &[])
}

async fn legacy_json(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle(&server, Flavor::Json, &headers, &body)
}

async fn legacy_sse(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle(&server, Flavor::Sse, &headers, &body)
}

async fn modern(State(server): State<Arc<Server>>, headers: HeaderMap, body: String) -> Response {
    handle(&server, Flavor::Modern, &headers, &body)
}

async fn bearer(State(server): State<Arc<Server>>, headers: HeaderMap, body: String) -> Response {
    handle(&server, Flavor::Bearer, &headers, &body)
}

async fn needs_sign_in() -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            header::WWW_AUTHENTICATE,
            "Bearer resource_metadata=\"http://127.0.0.1/meta\", scope=\"read write\"",
        )
        .body(Body::empty())
        .unwrap()
}

async fn end_session(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    if let Some(session) = header_of(&headers, "mcp-session-id") {
        server.deletes.lock().unwrap().push(session);
    }
    Response::builder().status(StatusCode::OK).body(Body::empty()).unwrap()
}

/// Start the server; returns its base address.
async fn start() -> (String, Arc<Server>) {
    let server = Arc::new(Server::default());
    let app = Router::new()
        .route("/json", post(legacy_json).delete(end_session))
        .route("/sse", post(legacy_sse).delete(end_session))
        .route("/modern", post(modern))
        .route("/bearer", post(bearer))
        .route("/auth", post(needs_sign_in))
        .with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), server)
}

fn options() -> ClientOptions {
    ClientOptions {
        name: "Test Client".into(),
        version: "1.2.3".into(),
        connect_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(10),
    }
}

fn http(url: String, bearer: Option<&str>) -> Endpoint {
    Endpoint::Http {
        url,
        headers: vec![("X-Test".into(), "yes".into())],
        bearer: bearer.map(str::to_string),
    }
}

// ---------------------------------------------------------------------
// Streamable HTTP, legacy era

#[tokio::test]
async fn legacy_handshake_session_and_paging() {
    let (base, server) = start().await;
    let client = Client::connect(http(format!("{base}/json"), None), options()).await.unwrap();

    let info = client.server();
    assert_eq!(info.name, "legacy-fake");
    assert_eq!(info.version, "0.3");
    assert_eq!(info.protocol_version, "2025-11-25");
    assert_eq!(info.instructions.as_deref(), Some("hello"));

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
    assert_eq!(tools[0].title.as_deref(), Some("Alpha"));
    assert_eq!(tools[0].description, "The first tool");
    assert_eq!(tools[0].annotations.read_only, Some(true));
    assert_eq!(tools[0].annotations.open_world, Some(false));
    assert_eq!(tools[0].annotations.destructive, None);
    assert_eq!(tools[1].input_schema, json!({ "type": "object" }));

    let initialize = &server.seen_method("initialize")[0];
    assert_eq!(initialize.session, None);
    assert_eq!(initialize.protocol, None);
    assert_eq!(initialize.params["protocolVersion"], json!("2025-11-25"));
    assert_eq!(initialize.params["clientInfo"]["name"], json!("Test Client"));

    let initialized = &server.seen_method("notifications/initialized")[0];
    assert_eq!(initialized.session.as_deref(), Some("sess-1"));

    let listings = server.seen_method("tools/list");
    assert_eq!(listings.len(), 2);
    assert_eq!(listings[1].params["cursor"], json!("page-2"));
    for listing in &listings {
        assert_eq!(listing.session.as_deref(), Some("sess-1"));
        assert_eq!(listing.protocol.as_deref(), Some("2025-11-25"));
        assert_eq!(listing.mcp_method, None);
    }

    assert!(client.is_alive());
    client.close().await;
    assert!(!client.is_alive());
    assert_eq!(*server.deletes.lock().unwrap(), ["sess-1"]);
}

#[tokio::test]
async fn results_are_rendered_over_json_and_over_a_stream() {
    let (base, _server) = start().await;
    for route in ["json", "sse"] {
        let client =
            Client::connect(http(format!("{base}/{route}"), None), options()).await.unwrap();

        let mixed = client.call_tool("mixed", json!({})).await.unwrap();
        assert!(!mixed.is_error, "{route}");
        assert_eq!(mixed.text, "hello\n\n[image: image/png, 1 KB]\n\nbye", "{route}");

        let long = client.call_tool("long", json!({})).await.unwrap();
        let (kept, note) = long.text.split_once('\n').unwrap();
        assert_eq!(kept.chars().count(), karmayoga_connect::MAX_RESULT_CHARS, "{route}");
        assert_eq!(note, "[\u{2026} cut: the result was 50000 characters long]", "{route}");

        let broken = client.call_tool("broken", json!({})).await.unwrap();
        assert!(broken.is_error, "{route}");
        assert_eq!(broken.text, "it failed");

        let structured = client.call_tool("structured", json!({})).await.unwrap();
        assert_eq!(structured.structured, Some(json!({ "n": 1 })));
        assert_eq!(structured.text, "{\n  \"n\": 1\n}");

        // A null becomes an empty object; an object goes through as it is.
        assert_eq!(client.call_tool("echo", Value::Null).await.unwrap().text, "{}");
        assert_eq!(
            client.call_tool("echo", json!({ "q": "x" })).await.unwrap().text,
            "{\"q\":\"x\"}"
        );

        match client.call_tool("nope", json!({})).await {
            Err(ConnectError::Rpc { code, .. }) => assert_eq!(code, -32602),
            other => panic!("expected an RPC error, got {other:?}"),
        }
        assert!(client.call_tool("echo", json!([1])).await.is_err());
    }
}

#[tokio::test]
async fn a_tools_changed_notice_in_a_stream_sets_the_flag_until_the_next_listing() {
    let (base, _server) = start().await;
    let client = Client::connect(http(format!("{base}/sse"), None), options()).await.unwrap();
    assert!(!client.tools_changed());
    client.call_tool("mixed", json!({})).await.unwrap();
    assert!(client.tools_changed());
    client.list_tools().await.unwrap();
    assert!(!client.tools_changed());
}

#[tokio::test]
async fn a_401_is_unauthorized_with_what_the_server_named() {
    let (base, _server) = start().await;
    match Client::connect(http(format!("{base}/auth"), None), options()).await {
        Err(ConnectError::Unauthorized { resource_metadata, scope }) => {
            assert_eq!(resource_metadata.as_deref(), Some("http://127.0.0.1/meta"));
            assert_eq!(scope.as_deref(), Some("read write"));
        }
        other => panic!("expected Unauthorized, got {:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn the_bearer_is_sent_and_can_be_replaced() {
    let (base, _server) = start().await;
    let url = format!("{base}/bearer");
    assert!(matches!(
        Client::connect(http(url.clone(), None), options()).await.map(|_| ()),
        Err(ConnectError::Unauthorized { .. })
    ));

    let client = Client::connect(http(url, Some("good")), options()).await.unwrap();
    assert_eq!(client.list_tools().await.unwrap().len(), 2);

    client.set_bearer(Some("expired".into()));
    assert!(matches!(client.list_tools().await, Err(ConnectError::Unauthorized { .. })));
    client.set_bearer(Some("good".into()));
    assert_eq!(client.list_tools().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_lost_session_is_replaced_once_and_the_request_retried() {
    let (base, server) = start().await;
    let client = Client::connect(http(format!("{base}/json"), None), options()).await.unwrap();
    assert_eq!(server.seen_method("initialize").len(), 1);

    server.expire();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 2);

    assert_eq!(server.seen_method("initialize").len(), 2);
    let after = server.seen_method("tools/list");
    // Stale session (refused), then the new one, then the second page.
    assert_eq!(after.first().unwrap().session.as_deref(), Some("sess-1"));
    assert_eq!(after.last().unwrap().session.as_deref(), Some("sess-3"));
}

#[tokio::test]
async fn a_server_that_is_not_there_is_a_transport_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    match Client::connect(http(format!("http://127.0.0.1:{port}/mcp?key=secret"), None), options())
        .await
    {
        Err(ConnectError::Transport(message)) => assert!(!message.contains("secret"), "{message}"),
        other => panic!("expected a transport error, got {:?}", other.map(|_| ())),
    }
}

// ---------------------------------------------------------------------
// Streamable HTTP, modern era

#[tokio::test]
async fn a_server_that_refuses_initialize_is_spoken_to_in_the_modern_era() {
    let (base, server) = start().await;
    let client = Client::connect(http(format!("{base}/modern"), None), options()).await.unwrap();

    let info = client.server();
    assert_eq!(info.name, "modern-fake");
    assert_eq!(info.version, "9");
    assert_eq!(info.protocol_version, MODERN_VERSION);
    assert_eq!(info.instructions.as_deref(), Some("be nice"));

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    let result = client.call_tool("alpha", json!({ "a": 1 })).await.unwrap();
    assert_eq!(result.text, "ok");

    assert!(server.seen_method("notifications/initialized").is_empty());

    let discover = &server.seen_method("server/discover")[0];
    assert_eq!(discover.session, None);
    assert_eq!(discover.mcp_method.as_deref(), Some("server/discover"));
    assert_eq!(discover.protocol.as_deref(), Some(MODERN_VERSION));
    let meta = &discover.params["_meta"];
    assert_eq!(meta["io.modelcontextprotocol/protocolVersion"], json!(MODERN_VERSION));
    assert_eq!(meta["io.modelcontextprotocol/clientInfo"]["name"], json!("Test Client"));
    assert_eq!(meta["io.modelcontextprotocol/clientInfo"]["version"], json!("1.2.3"));
    assert_eq!(meta["io.modelcontextprotocol/clientCapabilities"], json!({}));

    let listing = &server.seen_method("tools/list")[0];
    assert_eq!(listing.mcp_method.as_deref(), Some("tools/list"));
    assert_eq!(listing.mcp_name, None);
    let call = &server.seen_method("tools/call")[0];
    assert_eq!(call.mcp_method.as_deref(), Some("tools/call"));
    assert_eq!(call.mcp_name.as_deref(), Some("alpha"));
    assert_eq!(
        call.params["_meta"]["io.modelcontextprotocol/protocolVersion"],
        json!(MODERN_VERSION)
    );
    assert_eq!(call.session, None);
}

// ---------------------------------------------------------------------
// stdio

fn have_python() -> bool {
    std::process::Command::new("python3").arg("--version").output().is_ok()
}

const FAKE_SERVER: &str = r##"
import sys, json

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def text(mid, value):
    send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": value}]}})

print("a banner that is not JSON")
sys.stdout.flush()

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    mid = message.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": True}},
            "serverInfo": {"name": "pyfake", "version": "0.1"}}})
    elif method == "notifications/initialized":
        sys.stderr.write("got initialized\n")
        sys.stderr.flush()
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [
            {"name": "echo", "description": "Echo", "inputSchema": {"type": "object"}}]}})
    elif method == "tools/call":
        name = message["params"]["name"]
        args = message["params"]["arguments"]
        if name == "ask_ping":
            send({"jsonrpc": "2.0", "id": 900, "method": "ping"})
            reply = json.loads(sys.stdin.readline())
            text(mid, "pong " + json.dumps(reply.get("result")))
        elif name == "changed":
            send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            text(mid, "ok")
        elif name == "die":
            sys.stderr.write("fatal: bad configuration\n")
            sys.stderr.flush()
            sys.exit(3)
        else:
            text(mid, json.dumps(args, sort_keys=True))
"##;

fn python() -> Endpoint {
    Endpoint::Stdio {
        command: "python3".into(),
        args: vec!["-c".into(), FAKE_SERVER.into()],
        env: vec![],
        cwd: None,
    }
}

#[tokio::test]
async fn stdio_handshake_listing_calls_and_a_server_ping() {
    if !have_python() {
        eprintln!("python3 is not on PATH; skipping the stdio test");
        return;
    }
    let client = Client::connect(python(), options()).await.unwrap();
    let info = client.server();
    assert_eq!(info.name, "pyfake");
    // The server chose another revision the client speaks, and it is used.
    assert_eq!(info.protocol_version, "2025-06-18");

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let echoed = client.call_tool("echo", json!({ "b": 2, "a": 1 })).await.unwrap();
    assert_eq!(echoed.text, "{\"a\": 1, \"b\": 2}");

    // The server asks `ping` mid-call and only then answers.
    let pinged = client.call_tool("ask_ping", json!({})).await.unwrap();
    assert_eq!(pinged.text, "pong {}");

    assert!(!client.tools_changed());
    client.call_tool("changed", json!({})).await.unwrap();
    assert!(client.tools_changed());

    // Several at once, answered by id whatever the order.
    let (a, b, c) = tokio::join!(
        client.call_tool("echo", json!({ "n": 1 })),
        client.call_tool("echo", json!({ "n": 2 })),
        client.call_tool("echo", json!({ "n": 3 })),
    );
    assert_eq!(a.unwrap().text, "{\"n\": 1}");
    assert_eq!(b.unwrap().text, "{\"n\": 2}");
    assert_eq!(c.unwrap().text, "{\"n\": 3}");

    assert!(client.is_alive());
    client.close().await;
    assert!(!client.is_alive());
}

#[tokio::test]
async fn stdio_exit_reports_the_end_of_stderr() {
    if !have_python() {
        eprintln!("python3 is not on PATH; skipping the stdio test");
        return;
    }
    let client = Client::connect(python(), options()).await.unwrap();
    match client.call_tool("die", json!({})).await {
        Err(ConnectError::Closed { detail: Some(detail) }) => {
            assert!(detail.contains("bad configuration"), "{detail}");
        }
        other => panic!("expected Closed with a detail, got {other:?}"),
    }
    assert!(!client.is_alive());
    assert!(matches!(client.list_tools().await, Err(ConnectError::Closed { .. })));
}

#[tokio::test]
async fn stdio_program_that_is_not_there_is_a_spawn_error() {
    let missing = Endpoint::Stdio {
        command: "karmayoga-no-such-program".into(),
        args: vec![],
        env: vec![],
        cwd: None,
    };
    match Client::connect(missing, options()).await {
        Err(ConnectError::Spawn(message)) => {
            assert!(message.contains("karmayoga-no-such-program"), "{message}")
        }
        other => panic!("expected a spawn error, got {:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn stdio_connect_gives_up_on_a_silent_program() {
    if !have_python() {
        eprintln!("python3 is not on PATH; skipping the stdio test");
        return;
    }
    let silent = Endpoint::Stdio {
        command: "python3".into(),
        args: vec!["-c".into(), "import time; time.sleep(30)".into()],
        env: vec![],
        cwd: None,
    };
    let quick = ClientOptions { connect_timeout: Duration::from_millis(300), ..options() };
    assert!(matches!(Client::connect(silent, quick).await.map(|_| ()), Err(ConnectError::Timeout)));
}
