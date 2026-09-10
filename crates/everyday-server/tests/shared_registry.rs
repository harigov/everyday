//! Sharing and MCP, over one `Registry`, on one `devices.json`.
//!
//! The desktop app runs both features in one process, and both touch the
//! same device list -- a paired phone and an issued MCP token are rows in
//! the same file. This is the test for the invariant `Registry`'s own doc
//! states: a process that starts more than one thing over that file must
//! open it once and hand every user of it the same `Arc<Registry>`, built
//! here the way the app builds it -- [`everyday_server::prepare_with`] for
//! sharing, [`everyday_server::mcp::start`] for MCP, both given the one
//! registry opened above them. What is checked is the failure a second,
//! independent `Registry::open` of the same file would reintroduce: a
//! device-list write from one feature silently erasing what the other one
//! just wrote, because each copy would then save back only what it itself
//! had in memory.

use everyday_server::auth::Registry;
use everyday_server::mcp;
use everyday_service::{PROTOCOL, Scope, Service};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn a_device_list_write_from_sharing_does_not_erase_an_mcp_token() {
    everyday_server::install_crypto_provider();

    let vault_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let devices_path = config_dir.path().join(everyday_server::DEVICES_FILE);

    let vault = everyday_vault::create(
        vault_dir.path(),
        everyday_core::VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: Default::default(),
            auto_lock_seconds: 900,
            forget_key_seconds: 0,
        },
    )
    .unwrap();
    vault.save_journal(&everyday_core::Journal::new("Journal")).unwrap();

    let service = Arc::new(Service::new());
    service.set(vault);

    // The one registry. Opened exactly once, and handed to both features
    // below -- never opened a second time over the same path.
    let registry = Arc::new(Registry::open(&devices_path).unwrap());

    // Sharing, over TLS, on its own port.
    let share_config = everyday_server::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        enabled: true,
        allow_remote_unlock: true,
        no_tls: false,
    };
    let parts =
        everyday_server::prepare_with(config_dir.path(), &share_config, registry.clone()).unwrap();
    service.set_events(parts.broadcaster.clone());
    let cert_pem = parts.identity.as_ref().unwrap().cert_pem.clone();
    let sharing =
        everyday_server::start(service.clone(), parts, &share_config, "Test".into()).await.unwrap();
    let tls_client = everyday_server::client::pinned_client(&cert_pem).unwrap();
    let share_base = format!("https://localhost:{}", sharing.address.port());

    // MCP, on its own port, over the *same* registry -- not a second
    // `Registry::open`.
    let mcp_config = mcp::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        enabled: true,
        allow_destructive: false,
        token: None,
        device_id: None,
    };
    let mcp_running = mcp::start(service.clone(), registry.clone(), &mcp_config).await.unwrap();
    let mcp_base = format!("http://127.0.0.1:{}", mcp_running.address.port());

    // Issue an MCP token: a write to the device list, through `registry`.
    let token = mcp::issue_token(&registry, config_dir.path(), vec![Scope::All]).unwrap();

    // Now pair a phone through sharing -- a *second*, independent write to
    // the device list, through the same `Arc<Registry>`. This is the write
    // that, through a second `Registry` over the same file, would save a
    // snapshot with no MCP row in it and silently erase what `issue_token`
    // above just wrote.
    let code = registry.new_pairing_code();
    let paired = tls_client
        .post(format!("{share_base}/v1/pair"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .json(&json!({ "code": code, "deviceName": "Phone", "scopes": [] }))
        .send()
        .await
        .unwrap();
    assert!(paired.status().is_success(), "pairing failed: {}", paired.status());

    // The MCP row is still on disk, alongside the phone that just paired --
    // not overwritten by the pairing write.
    let reread = Registry::open(&devices_path).unwrap();
    let names: Vec<String> = reread.devices().into_iter().map(|d| d.name).collect();
    assert!(names.iter().any(|n| n == "MCP client"), "MCP row missing: {names:?}");
    assert!(names.iter().any(|n| n == "Phone"), "paired phone missing: {names:?}");

    // ...and the token still authenticates, against the *live* listener --
    // not a copy that fell out of step with what is on disk.
    let plain_client = reqwest::Client::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": everyday_mcp::MODERN,
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    });
    let response = plain_client
        .post(format!("{mcp_base}/mcp"))
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-protocol-version", everyday_mcp::MODERN)
        .header("mcp-method", "tools/list")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: serde_json::Value = response.json().await.unwrap();
    assert!(value.get("error").is_none(), "the MCP token no longer authenticates: {value:?}");
}
