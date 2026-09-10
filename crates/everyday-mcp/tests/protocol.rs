//! Table-driven coverage of [`handle`] against the dual-era wire protocol
//! `docs/plans/mcp-protocol-notes.md` describes, plus the header helpers
//! in `headers.rs` that the HTTP binding will lean on.
//!
//! Every test here drives `handle` directly against a `FakeHost` held
//! entirely in memory: no socket, no clock, no file, and no `tokio`
//! dev-dependency. `block_on` is a dozen lines rather than a runtime, for
//! the same reason `headers.rs` hand-rolls its own base64 decoder — this
//! crate's whole point is to carry as little as possible, and nothing a
//! `FakeHost` does ever actually suspends, so a busy-poll over a no-op
//! waker drives every future here to completion in microseconds.

use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use everyday_mcp::{
    Host, HostError, Implementation, MODERN, Outcome, SUPPORTED_VERSIONS, ToolDef,
    decode_header_value, expected_headers, handle,
};
use serde_json::{Value, json};

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
}

/// A `Host` a test builds by hand: a fixed tool list, in whatever order
/// the test wants, and a table of canned answers for `call`. Any name not
/// in that table comes back `unknown_tool`, which is the right default —
/// it is what a real `Host` says about a name it has never heard of too.
struct FakeHost {
    server_info: Implementation,
    instructions: Option<String>,
    tools: Vec<ToolDef>,
    calls: HashMap<String, Result<Value, HostError>>,
}

impl FakeHost {
    fn new() -> Self {
        FakeHost {
            server_info: Implementation { name: "Every Day".into(), version: "0.1.0".into() },
            instructions: None,
            tools: Vec::new(),
            calls: HashMap::new(),
        }
    }

    fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }

    fn with_call(mut self, name: &str, result: Result<Value, HostError>) -> Self {
        self.calls.insert(name.to_string(), result);
        self
    }
}

impl Host for FakeHost {
    fn server_info(&self) -> Implementation {
        self.server_info.clone()
    }

    fn instructions(&self) -> Option<String> {
        self.instructions.clone()
    }

    async fn tools(&self) -> Result<Vec<ToolDef>, HostError> {
        Ok(self.tools.clone())
    }

    async fn call(&self, name: &str, _arguments: &Value) -> Result<Value, HostError> {
        self.calls.get(name).cloned().unwrap_or_else(|| {
            Err(HostError { code: "unknown_tool".into(), message: format!("no such tool: {name}") })
        })
    }
}

fn tool(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        title: name.to_string(),
        description: format!("{name}, for a test that never reads it"),
        input_schema: json!({ "type": "object", "additionalProperties": false }),
        read_only: true,
        destructive: false,
    }
}

/// A modern (`2026-07-28`) request: `params` plus the `_meta` block that
/// era is defined by.
fn modern_request(id: i64, method: &str, mut params: Value) -> Value {
    params.as_object_mut().expect("params is always an object here").insert(
        "_meta".to_string(),
        json!({
            "io.modelcontextprotocol/protocolVersion": MODERN,
            "io.modelcontextprotocol/clientCapabilities": {},
        }),
    );
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A legacy request: no `_meta` at all, which is exactly what tells
/// `handle` to answer in that era.
fn legacy_request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn reply(outcome: Outcome) -> (u16, Value) {
    match outcome {
        Outcome::Reply { status, body } => (status, body),
        other => panic!("expected Outcome::Reply, got {other:?}"),
    }
}

/// Answer `message` and assert it was refused with `code` and `status`.
///
/// The three lines this replaces were written out in five tests, which is
/// four more places to get the status/code pairing subtly wrong in. The
/// tests keep their own names rather than collapsing into one table: a
/// failure that says which *rule* broke is worth more here than the handful
/// of lines a table would save, and these names are the only place the
/// rules are written down in English.
#[track_caller]
fn refused(host: &FakeHost, message: &Value, status: u16, code: i64) -> Value {
    let (actual_status, body) = reply(block_on(handle(host, message)));
    assert_eq!(actual_status, status, "status, for {body}");
    assert_eq!(body["error"]["code"], code, "error code, for {body}");
    body
}

// ---------------------------------------------------------------------
// Modern (2026-07-28)
// ---------------------------------------------------------------------

#[test]
fn a_well_formed_tool_call_returns_a_complete_result_with_structured_content() {
    let host = FakeHost::new().with_call("list_tasks", Ok(json!({ "tasks": [] })));
    let message = modern_request(1, "tools/call", json!({ "name": "list_tasks", "arguments": {} }));

    let (status, body) = reply(block_on(handle(&host, &message)));

    assert_eq!(status, 200);
    let result = &body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(result["structuredContent"], json!({ "tasks": [] }));
    assert_eq!(result["isError"], false);
}

#[test]
fn tools_list_returns_tools_in_the_hosts_own_order() {
    // Deliberately not alphabetical and not insertion-order-friendly to a
    // `HashMap`'s default hasher -- a regression to one anywhere on this
    // path should scramble this and fail the assertion below.
    let names = ["zebra_tool", "add_task", "mid_list_tool"];
    let host = FakeHost::new().with_tools(names.iter().map(|n| tool(n)).collect());
    let message = modern_request(1, "tools/list", json!({}));

    let (_, body) = reply(block_on(handle(&host, &message)));

    let listed: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(listed, names);
}

#[test]
fn server_discover_reports_versions_capabilities_and_server_identity() {
    let host = FakeHost::new();
    let message = modern_request(1, "server/discover", json!({}));

    let (status, body) = reply(block_on(handle(&host, &message)));

    assert_eq!(status, 200);
    let result = &body["result"];
    assert_eq!(result["supportedVersions"], json!(SUPPORTED_VERSIONS));
    assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"],
        json!({ "name": "Every Day", "version": "0.1.0" })
    );
}

#[test]
fn a_modern_request_missing_the_protocol_version_is_rejected() {
    let host = FakeHost::new();
    let message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": { "_meta": { "io.modelcontextprotocol/clientCapabilities": {} } },
    });

    refused(&host, &message, 400, -32602);
}

#[test]
fn a_modern_request_missing_client_capabilities_is_rejected() {
    let host = FakeHost::new();
    let message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } },
    });

    refused(&host, &message, 400, -32602);
}

#[test]
fn an_unsupported_protocol_version_is_named_in_the_error() {
    let host = FakeHost::new();
    let message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    });

    let body = refused(&host, &message, 400, -32022);
    assert_eq!(body["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(body["error"]["data"]["supported"], json!(SUPPORTED_VERSIONS));
}

#[test]
fn an_unknown_method_is_missing_not_merely_wrong() {
    let host = FakeHost::new();
    let message = modern_request(1, "resources/read", json!({}));

    let (status, body) = reply(block_on(handle(&host, &message)));

    assert_eq!(status, 404, "-32601 MUST be carried on HTTP 404, not 200 or 400");
    assert_eq!(body["error"]["code"], -32601);
}

#[test]
fn subscriptions_listen_opens_a_stream_with_an_acknowledgement() {
    let host = FakeHost::new();
    let message = modern_request(1, "subscriptions/listen", json!({}));

    let outcome = block_on(handle(&host, &message));
    let Outcome::Listen { subscription_id, ack } = outcome else {
        panic!("expected Outcome::Listen, got {outcome:?}");
    };

    assert!(!subscription_id.is_empty());
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
    assert_eq!(ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"], subscription_id);
}

// ---------------------------------------------------------------------
// Legacy (2025-11-25 / 2025-06-18)
// ---------------------------------------------------------------------

#[test]
fn legacy_initialize_answers_with_no_resulttype_at_all() {
    let host = FakeHost::new();
    let message = legacy_request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {} }),
    );

    let (status, body) = reply(block_on(handle(&host, &message)));

    assert_eq!(status, 200);
    let result = &body["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert!(result["capabilities"].is_object());
    assert_eq!(result["serverInfo"], json!({ "name": "Every Day", "version": "0.1.0" }));
    assert!(result.get("resultType").is_none(), "resultType does not exist in this era");
}

#[test]
fn notifications_initialized_is_accepted_without_a_reply() {
    let host = FakeHost::new();
    let message = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });

    assert_eq!(block_on(handle(&host, &message)), Outcome::Accepted);
}

#[test]
fn a_legacy_tool_call_carries_no_resulttype() {
    let host = FakeHost::new().with_call("list_tasks", Ok(json!({ "tasks": [] })));
    let message = legacy_request(1, "tools/call", json!({ "name": "list_tasks", "arguments": {} }));

    let (_, body) = reply(block_on(handle(&host, &message)));

    let result = &body["result"];
    assert_eq!(result["isError"], false);
    assert!(result.get("resultType").is_none(), "resultType does not exist in this era");
}

// ---------------------------------------------------------------------
// Both eras: the error distinction the whole crate exists to get right
// ---------------------------------------------------------------------

#[test]
fn an_unknown_tool_is_a_protocol_error_in_either_era() {
    let host = FakeHost::new(); // no calls registered: everything is unknown

    for (era, message) in [
        (
            "modern",
            modern_request(1, "tools/call", json!({ "name": "no_such_tool", "arguments": {} })),
        ),
        (
            "legacy",
            legacy_request(1, "tools/call", json!({ "name": "no_such_tool", "arguments": {} })),
        ),
    ] {
        let (status, body) = reply(block_on(handle(&host, &message)));
        assert_eq!(status, 400, "{era}");
        assert_eq!(body["error"]["code"], -32602, "{era}");
    }
}

#[test]
fn every_other_host_error_becomes_a_tool_execution_error_the_model_can_read() {
    // `invalid` is a bad argument, `locked` is a vault the caller cannot
    // reach right now, `confirm_required` is a destructive call nobody
    // confirmed -- three different reasons a tool refuses, none of which
    // is the request itself being malformed.
    for code in ["invalid", "locked", "confirm_required"] {
        let message_text = format!("refused: {code}");
        let host = FakeHost::new().with_call(
            "add_task",
            Err(HostError { code: code.to_string(), message: message_text.clone() }),
        );

        for (era, message) in [
            (
                "modern",
                modern_request(1, "tools/call", json!({ "name": "add_task", "arguments": {} })),
            ),
            (
                "legacy",
                legacy_request(1, "tools/call", json!({ "name": "add_task", "arguments": {} })),
            ),
        ] {
            let (status, body) = reply(block_on(handle(&host, &message)));
            assert_eq!(status, 200, "{code}/{era}");
            let result = &body["result"];
            assert_eq!(result["isError"], true, "{code}/{era}");
            assert_eq!(result["content"][0]["type"], "text", "{code}/{era}");
            assert_eq!(result["content"][0]["text"], message_text, "{code}/{era}");
        }
    }
}

#[test]
fn a_malformed_message_is_rejected_before_it_is_routed_anywhere() {
    let host = FakeHost::new();
    let cases: [(&str, Value); 3] = [
        ("missing method", json!({ "jsonrpc": "2.0", "id": 1 })),
        ("wrong jsonrpc", json!({ "jsonrpc": "1.0", "id": 1, "method": "tools/list" })),
        ("null id", json!({ "jsonrpc": "2.0", "id": null, "method": "tools/list" })),
    ];

    for (label, message) in cases {
        let (status, body) = reply(block_on(handle(&host, &message)));
        assert_eq!(status, 400, "{label}");
        assert_eq!(body["error"]["code"], -32600, "{label}");
    }
}

// ---------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------

#[test]
fn expected_headers_reads_what_the_body_already_implies() {
    let call = modern_request(1, "tools/call", json!({ "name": "add_task", "arguments": {} }));
    let expected = expected_headers(&call);
    assert_eq!(expected.method.as_deref(), Some("tools/call"));
    assert_eq!(expected.name.as_deref(), Some("add_task"));
    assert_eq!(expected.protocol_version.as_deref(), Some(MODERN));

    let list = modern_request(1, "tools/list", json!({}));
    let expected = expected_headers(&list);
    assert_eq!(expected.method.as_deref(), Some("tools/list"));
    assert_eq!(expected.name, None, "tools/list does not name a single target");

    let legacy_call = legacy_request(1, "tools/call", json!({ "name": "add_task" }));
    let expected = expected_headers(&legacy_call);
    assert_eq!(expected.name.as_deref(), Some("add_task"));
    assert_eq!(
        expected.protocol_version, None,
        "a legacy body carries no _meta to require a version from"
    );
}

#[test]
fn decode_header_value_round_trips_the_base64_sentinel() {
    // "task list", base64-encoded.
    assert_eq!(decode_header_value("=?base64?dGFzayBsaXN0?="), "task list");

    // A value that is not wrapped at all is returned unchanged.
    assert_eq!(decode_header_value("tools/call"), "tools/call");

    // The interesting case: the *decoded* payload is itself a string that
    // looks exactly like another `=?base64?...?=` sentinel. Decoding must
    // stop after one pass and hand that string back literally, not try to
    // decode it a second time.
    let inner = "=?base64?inner-looks-like-a-sentinel?=";
    let wrapped = "=?base64?PT9iYXNlNjQ/aW5uZXItbG9va3MtbGlrZS1hLXNlbnRpbmVsPz0=?=";
    assert_eq!(decode_header_value(wrapped), inner);
}
