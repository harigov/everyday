//! The MCP route, driven over a real socket in both eras.
//!
//! Not a unit test of `everyday-mcp` -- that crate's own `tests/protocol.rs`
//! already covers the state machine in microseconds with no I/O. What is
//! checked here is the part that cannot be checked there: that this binding
//! wires the right headers to the right checks in the right order, that a
//! real tool call against a real vault actually writes, and that the things
//! which must be refused before a tool ever runs -- a bad `Origin`, a
//! missing token, a header that disagrees with the body -- are refused.

use everyday_server::auth::Registry;
use everyday_server::mcp;
use everyday_service::{Scope, Service};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const LEGACY: &str = "2025-11-25";

struct Harness {
    _dir: tempfile::TempDir,
    _config_dir: tempfile::TempDir,
    config_dir: std::path::PathBuf,
    running: mcp::Running,
    registry: Arc<Registry>,
    base: String,
    http: reqwest::Client,
}

impl Harness {
    async fn start() -> Harness {
        Harness::start_with(None, false).await
    }

    /// `password` makes the vault a *locked* one -- the only way to exercise
    /// the empty-list behaviour, since an unencrypted vault is never locked
    /// to begin with.
    async fn start_with(password: Option<&str>, allow_destructive: bool) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            dir.path(),
            everyday_core::VaultConfig {
                name: "Test".into(),
                backend: "sqlite".into(),
                settings: Default::default(),
                password: password.map(str::to_string),
                kdf: Default::default(),
                auto_lock_seconds: 900,
                forget_key_seconds: 0,
            },
        )
        .unwrap();
        vault.save_journal(&everyday_core::Journal::new("Journal")).unwrap();
        if password.is_some() {
            vault.lock();
        }

        let service = Arc::new(Service::new());
        service.set(vault);

        let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());

        let config = mcp::Config {
            // Port 0: the operating system picks, so tests never collide.
            listen: "127.0.0.1:0".parse().unwrap(),
            enabled: true,
            allow_destructive,
            token: None,
            device_id: None,
        };
        let running = mcp::start(service, registry.clone(), &config).await.unwrap();
        let base = format!("http://127.0.0.1:{}", running.address.port());

        Harness {
            _dir: dir,
            config_dir: config_dir.path().to_path_buf(),
            _config_dir: config_dir,
            running,
            registry,
            base,
            http: reqwest::Client::new(),
        }
    }

    fn url(&self) -> String {
        format!("{}/mcp", self.base)
    }

    /// Mint a token the way `everyday mcp` (phase 3) or the settings panel
    /// (phase 4) would: through the registry, recorded in `mcp.json`.
    fn token(&self) -> String {
        mcp::issue_token(&self.registry, &self.config_dir, vec![Scope::All]).unwrap()
    }

    async fn post(&self, token: &str, headers: &[(&str, &str)], body: &Value) -> reqwest::Response {
        let mut req = self.http.post(self.url()).header("authorization", format!("Bearer {token}"));
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        req.json(body).send().await.unwrap()
    }

    /// A modern (`2026-07-28`) request, with the header mirroring a
    /// well-behaved client sends.
    async fn modern(
        &self,
        token: &str,
        id: i64,
        method: &str,
        params: Value,
    ) -> (StatusCode, Value) {
        let body = modern_body(id, method, params);
        let mut headers = vec![
            ("mcp-protocol-version", everyday_mcp::MODERN.to_string()),
            ("mcp-method", method.to_string()),
        ];
        if let Some(name) = body["params"]["name"].as_str() {
            headers.push(("mcp-name", name.to_string()));
        }
        let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let response = self.post(token, &headers, &body).await;
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        (status, value)
    }

    /// A legacy (`2025-11-25`) request. No header mirroring: legacy has no
    /// per-request `_meta` for a header to mirror.
    async fn legacy(
        &self,
        token: &str,
        id: i64,
        method: &str,
        params: Value,
    ) -> (StatusCode, Value) {
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let response = self.post(token, &[], &body).await;
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        (status, value)
    }
}

type StatusCode = reqwest::StatusCode;

fn modern_body(id: i64, method: &str, mut params: Value) -> Value {
    params.as_object_mut().expect("every test in this file passes an object for params").insert(
        "_meta".to_string(),
        json!({
            "io.modelcontextprotocol/protocolVersion": everyday_mcp::MODERN,
            "io.modelcontextprotocol/clientCapabilities": {},
        }),
    );
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

// ---- discovery and listing, in both eras ----------------------------------

#[tokio::test]
async fn server_discover_names_this_vault_and_its_capabilities() {
    let h = Harness::start().await;
    let token = h.token();
    let (status, body) = h.modern(&token, 1, "server/discover", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["resultType"], "complete");
    assert!(
        body["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!(everyday_mcp::MODERN))
    );
    assert_eq!(body["result"]["capabilities"]["tools"]["listChanged"], true);
    assert!(body["result"]["instructions"].as_str().unwrap().contains("locked"));
}

#[tokio::test]
async fn legacy_initialize_answers_with_no_result_type() {
    let h = Harness::start().await;
    let token = h.token();
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": LEGACY, "capabilities": {} },
    });
    let response = h.post(&token, &[], &body).await;
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["result"]["protocolVersion"], LEGACY);
    assert_eq!(value["result"]["serverInfo"]["name"], "Every Day");
    assert!(value["result"].get("resultType").is_none());
}

#[tokio::test]
async fn tools_list_returns_the_catalogue_in_both_eras() {
    let h = Harness::start().await;
    let token = h.token();

    let (status, modern) = h.modern(&token, 1, "tools/list", json!({})).await;
    assert_eq!(status, 200);
    let modern_tools = modern["result"]["tools"].as_array().unwrap();
    assert!(modern_tools.iter().any(|t| t["name"] == "create_task"));
    assert!(modern_tools.iter().all(|t| t["annotations"]["destructiveHint"] == false));

    let (status, legacy) = h.legacy(&token, 1, "tools/list", json!({})).await;
    assert_eq!(status, 200);
    let legacy_tools = legacy["result"]["tools"].as_array().unwrap();
    assert_eq!(legacy_tools.len(), modern_tools.len());
}

#[tokio::test]
async fn tools_call_creates_a_real_task_that_a_read_tool_can_then_see() {
    let h = Harness::start().await;
    let token = h.token();

    let (status, created) = h
        .modern(
            &token,
            1,
            "tools/call",
            json!({ "name": "create_task", "arguments": { "title": "Buy milk" } }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(created["result"]["isError"], false);
    assert_eq!(created["result"]["structuredContent"]["ok"], true);
    assert_eq!(created["result"]["structuredContent"]["name"], "Buy milk");

    let (_, listed) =
        h.modern(&token, 2, "tools/call", json!({ "name": "list_tasks", "arguments": {} })).await;
    let tasks = listed["result"]["structuredContent"]["tasks"].as_array().unwrap();
    assert!(tasks.iter().any(|t| t["title"] == "Buy milk"), "{tasks:?}");
}

#[tokio::test]
async fn tools_call_also_writes_in_the_legacy_era() {
    let h = Harness::start().await;
    let token = h.token();

    let (status, created) = h
        .legacy(
            &token,
            1,
            "tools/call",
            json!({ "name": "create_task", "arguments": { "title": "Legacy task" } }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(created["result"].get("resultType").is_none());
    assert_eq!(created["result"]["structuredContent"]["name"], "Legacy task");
}

// ---- what a locked vault, and this server's own switches, allow -----------

#[tokio::test]
async fn a_locked_vault_offers_no_tools_at_all() {
    let h = Harness::start_with(Some("hunter2"), false).await;
    let token = h.token();
    let (status, body) = h.modern(&token, 1, "tools/list", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_destructive_tool_is_absent_when_the_switch_is_off() {
    let h = Harness::start_with(None, false).await;
    let token = h.token();
    let (_, body) = h.modern(&token, 1, "tools/list", json!({})).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(!tools.iter().any(|t| t["name"] == "delete_task"), "{tools:?}");
    assert!(tools.iter().any(|t| t["name"] == "create_task"));

    // Not merely unlisted -- naming it directly is refused too, with the
    // answer a model can read and not just a closed door.
    let (status, call) = h
        .modern(
            &token,
            2,
            "tools/call",
            json!({ "name": "delete_task", "arguments": { "task_id": "nonexistent" } }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(call["result"]["isError"], true);
}

#[tokio::test]
async fn a_destructive_tool_is_present_and_runs_when_the_switch_is_on() {
    let h = Harness::start_with(None, true).await;
    let token = h.token();

    let (_, body) = h.modern(&token, 1, "tools/list", json!({})).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "delete_task"), "{tools:?}");

    let (_, created) = h
        .modern(
            &token,
            2,
            "tools/call",
            json!({ "name": "create_task", "arguments": { "title": "Delete me" } }),
        )
        .await;
    let id = created["result"]["structuredContent"]["id"].as_str().unwrap().to_string();

    let (status, deleted) = h
        .modern(
            &token,
            3,
            "tools/call",
            json!({ "name": "delete_task", "arguments": { "task_id": id } }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(deleted["result"]["isError"], false);

    let (_, listed) =
        h.modern(&token, 4, "tools/call", json!({ "name": "list_tasks", "arguments": {} })).await;
    let tasks = listed["result"]["structuredContent"]["tasks"].as_array().unwrap();
    assert!(!tasks.iter().any(|t| t["title"] == "Delete me"), "{tasks:?}");
}

// ---- refusals that happen before a tool ever runs -------------------------

#[tokio::test]
async fn a_present_but_non_local_origin_is_refused_with_403() {
    let h = Harness::start().await;
    let token = h.token();
    let body = modern_body(1, "tools/list", json!({}));
    let response = h
        .http
        .post(h.url())
        .header("authorization", format!("Bearer {token}"))
        .header("origin", "http://evil.example.com")
        .header("mcp-protocol-version", everyday_mcp::MODERN)
        .header("mcp-method", "tools/list")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn a_local_origin_is_accepted() {
    let h = Harness::start().await;
    let token = h.token();
    let body = modern_body(1, "tools/list", json!({}));
    let response = h
        .http
        .post(h.url())
        .header("authorization", format!("Bearer {token}"))
        .header("origin", "http://localhost:9999")
        .header("mcp-protocol-version", everyday_mcp::MODERN)
        .header("mcp-method", "tools/list")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_request_with_no_token_is_refused_with_401() {
    let h = Harness::start().await;
    let body = modern_body(1, "server/discover", json!({}));
    let response = h
        .http
        .post(h.url())
        .header("mcp-protocol-version", everyday_mcp::MODERN)
        .header("mcp-method", "server/discover")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_request_with_a_bad_token_is_refused_with_401() {
    let h = Harness::start().await;
    let body = modern_body(1, "server/discover", json!({}));
    let response = h
        .http
        .post(h.url())
        .header("authorization", "Bearer not-a-real-token")
        .header("mcp-protocol-version", everyday_mcp::MODERN)
        .header("mcp-method", "server/discover")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_header_that_disagrees_with_the_body_is_400_with_dash_32020() {
    let h = Harness::start().await;
    let token = h.token();
    let body = modern_body(1, "tools/list", json!({}));
    let response = h
        .http
        .post(h.url())
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-protocol-version", everyday_mcp::MODERN)
        // Wrong on purpose: the body says "tools/list".
        .header("mcp-method", "tools/call")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["error"]["code"], -32020);
}

#[tokio::test]
async fn a_body_that_is_not_json_is_a_parse_error() {
    let h = Harness::start().await;
    let token = h.token();
    let response = h
        .http
        .post(h.url())
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body("not json at all")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["error"]["code"], -32700);
}

#[tokio::test]
async fn an_unknown_method_is_404_with_dash_32601() {
    let h = Harness::start().await;
    let token = h.token();
    let (status, body) = h.modern(&token, 1, "resources/read", json!({})).await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], -32601);
}

// ---- the two streaming shapes ----------------------------------------------

#[tokio::test]
async fn get_opens_a_legacy_stream_that_learns_of_an_unlock() {
    let h = Harness::start().await;
    let token = h.token();
    let mut response = h
        .http
        .get(h.url())
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );

    // Nothing has happened yet: the legacy stream has no acknowledgement to
    // send, unlike `subscriptions/listen`'s. Fire the same event the vault
    // itself raises on unlock and see it arrive.
    h.running.server.lock_state(false);

    let chunk = tokio::time::timeout(Duration::from_secs(2), response.chunk())
        .await
        .expect("a notification arrived before the timeout")
        .unwrap()
        .expect("the stream is still open");
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("notifications/tools/list_changed"), "{text}");
}

#[tokio::test]
async fn delete_is_not_allowed() {
    let h = Harness::start().await;
    let response = h.http.delete(h.url()).send().await.unwrap();
    assert_eq!(response.status(), 405);
}
