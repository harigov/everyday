//! Connections' commands, end to end against a real vault.
//!
//! What is here is what only the command layer decides: that no answer,
//! under any command, carries a token, a header's value or a client secret;
//! what each patch to the secrets does; which word the list shows beside a
//! connection; and that one name is one connection. The protocol is the
//! client's own tests'. One test at the bottom runs the whole way against a
//! small MCP server on this computer, so that adding a connection, checking
//! it, listing what it offers and calling a tool are seen to work together.
//!
//! Nothing here leaves the machine: the connections that would connect point
//! at a port nothing listens on, or at the server this file starts.

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use karmayoga_service::connections::hub;
use karmayoga_service::ctx::Ctx;
use karmayoga_service::{Service, error::CommandError};
use serde_json::{Value, json};
use std::sync::Arc;

#[allow(dead_code)]
mod support;

/// Distinctive enough that finding them anywhere means they leaked.
const TOKEN: &str = "bearer-3f2a1b0c-DISTINCTIVE";
const HEADER_VALUE: &str = "header-9d8c7b6a-DISTINCTIVE";
const CLIENT_SECRET: &str = "client-secret-5e4d3c2b-DISTINCTIVE";
const GRANT_TOKEN: &str = "grant-access-1a2b3c4d-DISTINCTIVE";

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A port nothing listens on, so a connection to it fails at once.
fn nowhere() -> &'static str {
    "http://127.0.0.1:1/mcp"
}

fn http(url: &str, headers: &[&str]) -> Value {
    json!({ "type": "http", "url": url, "headers": headers })
}

fn oauth() -> Value {
    json!({ "type": "oAuth" })
}

async fn add(svc: &Arc<Service>, name: &str, transport: Value, auth: Value) -> Value {
    call(svc, "add_connection", json!({ "name": name, "transport": transport, "auth": auth })).await
}

fn id_of(view: &Value) -> Value {
    view["connection"]["id"].clone()
}

fn names(secrets: &Value, key: &str) -> Vec<String> {
    secrets[key].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

// ---- the list --------------------------------------------------------------

#[tokio::test]
async fn a_connection_is_added_edited_and_removed() {
    let (svc, _dir) = support::vault::service(None);
    let empty = call(&svc, "connections", json!({})).await;
    assert_eq!(empty["connections"], json!([]));
    let presets = empty["presets"].as_array().unwrap();
    assert!(presets.iter().any(|p| p["key"] == "slack"));
    assert!(presets.iter().any(|p| p["key"] == "custom-http"));

    let added = call(
        &svc,
        "add_connection",
        json!({
            "preset": "linear",
            "name": "  Linear  ",
            "transport": http("https://mcp.linear.app/mcp", &[]),
            "auth": oauth(),
        }),
    )
    .await;
    assert_eq!(added["connection"]["name"], "Linear");
    assert_eq!(added["connection"]["preset"], "linear");
    assert_eq!(added["connection"]["enabled"], true);
    assert_eq!(added["live"], false);
    let id = id_of(&added);

    let mut edited = added["connection"].clone();
    edited["name"] = json!("Work Linear");
    edited["unattended"] = json!(true);
    let updated = call(&svc, "update_connection", json!({ "connection": edited })).await;
    assert_eq!(updated["connection"]["name"], "Work Linear");
    assert_eq!(updated["connection"]["unattended"], true);
    assert_eq!(id_of(&updated), id);
    assert_eq!(updated["connection"]["createdAt"], added["connection"]["createdAt"]);

    let listed = call(&svc, "connections", json!({})).await;
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert_eq!(listed["connections"][0]["connection"]["name"], "Work Linear");

    let gone = call(&svc, "remove_connection", json!({ "id": id })).await;
    assert_eq!(gone["connections"], json!([]));
    assert_eq!(
        fails(&svc, "update_connection", json!({ "connection": edited })).await.code,
        "not_found"
    );
}

#[tokio::test]
async fn names_are_unique_ignoring_case() {
    let (svc, _dir) = support::vault::service(None);
    add(&svc, "Work", http(nowhere(), &[]), json!({ "type": "none" })).await;
    let e = fails(
        &svc,
        "add_connection",
        json!({
            "name": " work ",
            "transport": http(nowhere(), &[]),
            "auth": { "type": "none" },
        }),
    )
    .await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("already a connection"), "{}", e.message);
    let listed = call(&svc, "connections", json!({})).await;
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_address_that_would_send_a_token_in_the_clear_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(
        &svc,
        "add_connection",
        json!({
            "name": "Elsewhere",
            "transport": http("http://example.com/mcp", &[]),
            "auth": { "type": "bearer" },
        }),
    )
    .await;
    assert_eq!(e.code, "invalid");
}

// ---- secrets ---------------------------------------------------------------

#[tokio::test]
async fn no_answer_from_any_command_carries_a_secret() {
    let (svc, _dir) = support::vault::service(None);
    let added = call(
        &svc,
        "add_connection",
        json!({
            "preset": "github",
            "name": "GitHub",
            "transport": http(nowhere(), &["X-Key"]),
            "auth": { "type": "bearer" },
            "secrets": {
                "bearer": TOKEN,
                "headers": { "X-Key": HEADER_VALUE },
                "clientSecret": CLIENT_SECRET,
            },
        }),
    )
    .await;
    let id = id_of(&added);
    assert_eq!(
        added["secrets"],
        json!({
            "bearer": true,
            "headers": ["X-Key"],
            "env": [],
            "clientSecret": true,
            "signedIn": false,
            "expiresAt": null,
        })
    );

    let mut every = vec![added.clone()];
    every.push(call(&svc, "connections", json!({})).await);
    every.push(call(&svc, "update_connection", json!({ "connection": added["connection"] })).await);
    every.push(call(&svc, "set_connection_secrets", json!({ "id": id, "bearer": TOKEN })).await);
    // Connects to nothing, and fails; the failure is recorded in the state,
    // which is part of the view.
    every.push(call(&svc, "check_connection", json!({ "id": id })).await);
    every.push(
        call(
            &svc,
            "set_connection_tool_policy",
            json!({ "id": id, "tool": "search", "policy": "off" }),
        )
        .await,
    );
    every.push(call(&svc, "sign_out_connection", json!({ "id": id })).await);
    every.push(call(&svc, "remove_connection", json!({ "id": id })).await);
    for answer in &every {
        let text = answer.to_string();
        for secret in [TOKEN, HEADER_VALUE, CLIENT_SECRET] {
            assert!(!text.contains(secret), "a command's answer carried a secret: {text}");
        }
    }

    // And not in what a failing command says either.
    let again = add(&svc, "Again", http(nowhere(), &[]), json!({ "type": "bearer" })).await;
    call(&svc, "set_connection_secrets", json!({ "id": id_of(&again), "bearer": TOKEN })).await;
    let checked = call(&svc, "check_connection", json!({ "id": id_of(&again) })).await;
    assert_eq!(checked["status"], "error", "{checked}");
    assert!(!checked.to_string().contains(TOKEN));
}

#[tokio::test]
async fn a_patch_sets_removes_and_keeps_what_it_does_not_name() {
    let (svc, _dir) = support::vault::service(None);
    let added = add(
        &svc,
        "Local",
        json!({ "type": "stdio", "command": "true", "args": [], "env": ["A", "B"] }),
        json!({ "type": "none" }),
    )
    .await;
    let id = id_of(&added);
    assert_eq!(added["status"], "needsSetup");

    let set =
        call(&svc, "set_connection_secrets", json!({ "id": id, "env": { "A": "1", "B": "2" } }))
            .await;
    assert_eq!(names(&set["secrets"], "env"), ["A", "B"]);
    assert_eq!(set["status"], "unknown");

    // A name with an empty value goes; a name left out stays.
    let removed =
        call(&svc, "set_connection_secrets", json!({ "id": id, "env": { "A": "" } })).await;
    assert_eq!(names(&removed["secrets"], "env"), ["B"]);
    assert_eq!(removed["status"], "needsSetup");

    // A field left out is left alone: a bearer set earlier survives a patch
    // of something else, and an empty one clears it.
    let (svc2, _dir2) = support::vault::service(None);
    let bearer = add(&svc2, "Token", http(nowhere(), &[]), json!({ "type": "bearer" })).await;
    let bid = id_of(&bearer);
    assert_eq!(bearer["status"], "needsSetup");
    let with = call(&svc2, "set_connection_secrets", json!({ "id": bid, "bearer": TOKEN })).await;
    assert_eq!(with["secrets"]["bearer"], true);
    assert_eq!(with["status"], "unknown");
    let kept =
        call(&svc2, "set_connection_secrets", json!({ "id": bid, "clientSecret": CLIENT_SECRET }))
            .await;
    assert_eq!(kept["secrets"]["bearer"], true);
    assert_eq!(kept["secrets"]["clientSecret"], true);
    let cleared = call(&svc2, "set_connection_secrets", json!({ "id": bid, "bearer": "" })).await;
    assert_eq!(cleared["secrets"]["bearer"], false);
    assert_eq!(cleared["secrets"]["clientSecret"], true);
    assert_eq!(cleared["status"], "needsSetup");

    // A name the connection stopped listing is dropped at the next patch.
    let mut edited = removed["connection"].clone();
    edited["transport"]["env"] = json!(["C"]);
    call(&svc, "update_connection", json!({ "connection": edited })).await;
    let tidied = call(&svc, "set_connection_secrets", json!({ "id": id })).await;
    assert_eq!(names(&tidied["secrets"], "env"), Vec::<String>::new());
    assert_eq!(tidied["status"], "needsSetup");
}

// ---- status ----------------------------------------------------------------

#[tokio::test]
async fn the_status_word_says_what_to_do_next() {
    let (svc, _dir) = support::vault::service(None);
    let bearer = add(&svc, "Bearer", http(nowhere(), &[]), json!({ "type": "bearer" })).await;
    assert_eq!(bearer["status"], "needsSetup");
    let sign_in = add(&svc, "OAuth", http(nowhere(), &[]), oauth()).await;
    assert_eq!(sign_in["status"], "needsSignIn");
    let headed = add(&svc, "Headers", http(nowhere(), &["X-Key"]), json!({ "type": "none" })).await;
    assert_eq!(headed["status"], "needsSetup");
    let plain = add(&svc, "Plain", http(nowhere(), &[]), json!({ "type": "none" })).await;
    assert_eq!(plain["status"], "unknown");

    let mut off = plain["connection"].clone();
    off["enabled"] = json!(false);
    let off = call(&svc, "update_connection", json!({ "connection": off })).await;
    assert_eq!(off["status"], "off");

    // A check that cannot connect says so.
    let mut on = off["connection"].clone();
    on["enabled"] = json!(true);
    call(&svc, "update_connection", json!({ "connection": on })).await;
    let failed = call(&svc, "check_connection", json!({ "id": id_of(&plain) })).await;
    assert_eq!(failed["status"], "error");
    assert!(failed["state"]["lastError"].is_string());

    // A check without a token is setup, not a failed sign-in.
    let unset = call(&svc, "check_connection", json!({ "id": id_of(&bearer) })).await;
    assert_eq!(unset["status"], "needsSetup");
}

#[tokio::test]
async fn signing_out_keeps_the_connection_and_forgets_the_grant() {
    let (svc, _dir) = support::vault::service(None);
    let added = add(&svc, "OAuth", http(nowhere(), &[]), oauth()).await;
    let id = id_of(&added);

    // What a finished sign-in leaves, put there as it would have been.
    let vault = svc.get().unwrap();
    let connection_id = id.as_str().unwrap().parse().unwrap();
    let mut secret = vault.connection_secret(connection_id).unwrap();
    secret.grant = Some(karmayoga_core::connection::OAuthGrant {
        client_id: "client".into(),
        client_secret: None,
        token_endpoint: "https://example.com/token".into(),
        token_auth: karmayoga_core::connection::TokenAuth::None,
        resource: None,
        access_token: GRANT_TOKEN.into(),
        refresh_token: Some(GRANT_TOKEN.into()),
        expires_at: None,
        scope: None,
    });
    vault.set_connection_secret(connection_id, &secret).unwrap();

    let listed = call(&svc, "connections", json!({})).await;
    assert_eq!(listed["connections"][0]["secrets"]["signedIn"], true);
    assert_eq!(listed["connections"][0]["status"], "unknown");
    assert!(!listed.to_string().contains(GRANT_TOKEN));

    let out = call(&svc, "sign_out_connection", json!({ "id": id })).await;
    assert_eq!(out["secrets"]["signedIn"], false);
    assert_eq!(out["status"], "needsSignIn");
    assert_eq!(out["connection"]["name"], "OAuth");
    assert!(vault.connection_secret(connection_id).unwrap().grant.is_none());
}

// ---- policy ----------------------------------------------------------------

#[tokio::test]
async fn a_tool_policy_is_set_and_cleared() {
    let (svc, _dir) = support::vault::service(None);
    let added = add(&svc, "Plain", http(nowhere(), &[]), json!({ "type": "none" })).await;
    let id = id_of(&added);

    let off = call(
        &svc,
        "set_connection_tool_policy",
        json!({ "id": id, "tool": "send", "policy": "off" }),
    )
    .await;
    assert_eq!(off["connection"]["policies"], json!({ "send": "off" }));
    let allow = call(
        &svc,
        "set_connection_tool_policy",
        json!({ "id": id, "tool": "read", "policy": "allow" }),
    )
    .await;
    assert_eq!(allow["connection"]["policies"], json!({ "send": "off", "read": "allow" }));
    let cleared = call(
        &svc,
        "set_connection_tool_policy",
        json!({ "id": id, "tool": "send", "policy": null }),
    )
    .await;
    assert_eq!(cleared["connection"]["policies"], json!({ "read": "allow" }));
    let cleared =
        call(&svc, "set_connection_tool_policy", json!({ "id": id, "tool": "read" })).await;
    assert_eq!(cleared["connection"]["policies"], json!({}));

    assert_eq!(
        fails(
            &svc,
            "set_connection_tool_policy",
            json!({ "id": id, "tool": " ", "policy": "off" })
        )
        .await
        .code,
        "invalid"
    );
}

// ---- signing in ------------------------------------------------------------

#[tokio::test]
async fn only_an_oauth_connection_signs_in_with_a_browser() {
    let (svc, _dir) = support::vault::service(None);
    let bearer = add(&svc, "Token", http(nowhere(), &[]), json!({ "type": "bearer" })).await;
    let e = fails(&svc, "begin_connection_sign_in", json!({ "id": id_of(&bearer) })).await;
    assert!(e.message.contains("browser"), "{}", e.message);

    let e = fails(&svc, "await_connection_sign_in", json!({ "signInId": "nope" })).await;
    assert_eq!(e.code, "not_found");
    // Cancelling one nobody started is not an error.
    call(&svc, "cancel_connection_sign_in", json!({ "signInId": "nope" })).await;
}

// ---- a server on this computer ---------------------------------------------

/// The least an MCP server is: `initialize`, the notification that follows
/// it, `tools/list` and one tool.
async fn mcp(Json(body): Json<Value>) -> Response {
    let Some(id) = body.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match body["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": body["params"]["protocolVersion"],
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "fake", "version": "1.2.3" },
        }),
        "tools/list" => json!({
            "tools": [{
                "name": "echo",
                "description": "Say back what you are told.",
                "inputSchema": { "type": "object" },
                "annotations": { "readOnlyHint": true },
            }],
        }),
        "tools/call" => json!({
            "content": [{ "type": "text", "text": "echoed" }],
            "isError": false,
        }),
        other => {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("no {other}") },
            }))
            .into_response();
        }
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

#[tokio::test]
async fn a_server_on_this_computer_is_checked_listed_and_called() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/mcp", post(mcp))).await.unwrap();
    });

    let (svc, _dir) = support::vault::service(None);
    let url = format!("http://127.0.0.1:{port}/mcp");
    let added = add(&svc, "Fake", http(&url, &[]), json!({ "type": "none" })).await;
    let id = id_of(&added);
    assert_eq!(added["status"], "unknown");
    assert_eq!(added["live"], false);

    let checked = call(&svc, "check_connection", json!({ "id": id })).await;
    assert_eq!(checked["status"], "ready", "{checked}");
    assert_eq!(checked["live"], true);
    assert_eq!(checked["state"]["server"]["name"], "fake");
    assert_eq!(checked["state"]["tools"][0]["name"], "echo");
    assert_eq!(checked["state"]["tools"][0]["hints"]["readOnly"], true);
    assert!(checked["state"]["lastError"].is_null());

    let vault = svc.get().unwrap();
    let connection_id: karmayoga_core::ConnectionId = id.as_str().unwrap().parse().unwrap();
    let usable = hub().usable(&vault, false).await;
    assert!(usable.connections.iter().any(|(c, s)| c.id == connection_id && s.tools.len() == 1));
    assert!(usable.unavailable.is_empty(), "{:?}", usable.unavailable);
    // Not for a routine until the connection says it may be.
    assert!(hub().usable(&vault, true).await.connections.is_empty());

    let result = hub().call(&vault, connection_id, "echo", json!({})).await.unwrap();
    assert!(!result.is_error);
    assert!(result.text.contains("echoed"), "{}", result.text);

    // An edit drops the open client, so the next use connects afresh.
    call(&svc, "update_connection", json!({ "connection": checked["connection"] })).await;
    assert!(!hub().is_live(connection_id));
    hub().call(&vault, connection_id, "echo", json!({})).await.unwrap();
    assert!(hub().is_live(connection_id));

    // Switched off, it is refused with the reason.
    let mut off = checked["connection"].clone();
    off["enabled"] = json!(false);
    call(&svc, "update_connection", json!({ "connection": off })).await;
    let e = hub().call(&vault, connection_id, "echo", json!({})).await.unwrap_err();
    assert!(e.message.contains("switched off"), "{}", e.message);

    hub().close_all();
    assert!(!hub().is_live(connection_id));
}

#[tokio::test]
async fn moving_a_connection_elsewhere_leaves_its_token_and_choices_behind() {
    let (svc, _dir) = support::vault::service(None);
    let added =
        add(&svc, "GitHub", http("http://127.0.0.1:1/mcp", &[]), json!({ "type": "bearer" })).await;
    let id = id_of(&added);
    call(&svc, "set_connection_secrets", json!({ "id": id, "bearer": TOKEN })).await;
    call(
        &svc,
        "set_connection_tool_policy",
        json!({ "id": id, "tool": "search", "policy": "allow" }),
    )
    .await;

    // Another path on the same host keeps the token and the choices.
    let mut connection =
        call(&svc, "connections", json!({})).await["connections"][0]["connection"].clone();
    connection["transport"]["url"] = json!("http://127.0.0.1:1/other");
    let same_host = call(&svc, "update_connection", json!({ "connection": connection })).await;
    assert_eq!(same_host["secrets"]["bearer"], json!(true));
    assert_eq!(same_host["connection"]["policies"]["search"], json!("allow"));

    // Another host does not.
    let mut connection = same_host["connection"].clone();
    connection["transport"]["url"] = json!("http://127.0.0.2:1/mcp");
    let moved = call(&svc, "update_connection", json!({ "connection": connection })).await;
    assert_eq!(moved["secrets"]["bearer"], json!(false), "the token stays with the old host");
    assert_eq!(moved["connection"]["policies"], json!({}), "so do the choices about its tools");
    assert_eq!(moved["status"], json!("needsSetup"));
}
