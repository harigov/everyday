//! A server, and a client that drives it over a real socket.
//!
//! Not a unit test of the router: a whole server on a loopback port, with TLS,
//! pinning, pairing and every refusal it is supposed to make. What is checked
//! here is the part that cannot be checked anywhere else -- that the wire works,
//! and that the things which must be refused are refused before a command runs.

use everyday_server::pairing;
use everyday_service::Service;
use serde_json::{Value, json};
use std::sync::Arc;

const PROTOCOL: u32 = everyday_service::PROTOCOL;

struct Harness {
    _dir: tempfile::TempDir,
    _config_dir: tempfile::TempDir,
    running: everyday_server::Running,
    service: Arc<Service>,
    base: String,
    /// A client that trusts this server's certificate and nothing else.
    http: reqwest::Client,
}

impl Harness {
    async fn start() -> Harness {
        Harness::start_with(None).await
    }

    /// `password` makes the vault a *locked* one, which is the only way to
    /// exercise unlocking: an unencrypted vault accepts any password, so a
    /// test that guessed at one would be measuring nothing.
    async fn start_with(password: Option<&str>) -> Harness {
        everyday_server::install_crypto_provider();

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
        // `create` hands back an *unlocked* vault -- it has just been given the
        // password. A server that has been restarted starts locked, which is
        // the state the unlock tests are about.
        if password.is_some() {
            vault.lock();
        }

        let service = Arc::new(Service::new());
        service.set(vault);

        let config = everyday_server::Config {
            // Port 0: the operating system picks, so tests never collide.
            listen: "127.0.0.1:0".parse().unwrap(),
            enabled: true,
            allow_remote_unlock: true,
            no_tls: false,
        };
        let parts = everyday_server::prepare(config_dir.path(), &config).unwrap();
        service.set_events(parts.broadcaster.clone());
        let cert_pem = parts.identity.as_ref().unwrap().cert_pem.clone();
        let running =
            everyday_server::start(service.clone(), parts, &config, "Test".into()).await.unwrap();

        // The pinning a real client does, through the same function it uses.
        let http = everyday_server::client::pinned_client(&cert_pem).unwrap();

        let base = format!("https://localhost:{}", running.address.port());
        Harness { _dir: dir, _config_dir: config_dir, running, service, base, http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn hello(&self) -> Value {
        self.http.get(self.url("/v1/hello")).send().await.unwrap().json().await.unwrap()
    }

    /// Pair, the way a client does: read the invitation, post the code.
    async fn pair(&self, name: &str) -> String {
        let code = self.running.server.registry.new_pairing_code();
        let response = self
            .http
            .post(self.url("/v1/pair"))
            .header("x-everyday-protocol", PROTOCOL.to_string())
            .json(&json!({ "code": code, "deviceName": name, "scopes": [] }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "pairing failed: {}", response.status());
        let body: Value = response.json().await.unwrap();
        body["token"].as_str().unwrap().to_string()
    }

    async fn call(&self, token: &str, name: &str, args: Value) -> reqwest::Response {
        self.http
            .post(self.url(&format!("/v1/call/{name}")))
            .header("x-everyday-protocol", PROTOCOL.to_string())
            .header("authorization", format!("Bearer {token}"))
            .json(&args)
            .send()
            .await
            .unwrap()
    }

    async fn ok(&self, token: &str, name: &str, args: Value) -> Value {
        let response = self.call(token, name, args).await;
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{name}: {status} {body}");
        body
    }
}

#[tokio::test]
async fn hello_says_who_this_is_before_anybody_has_paired() {
    let h = Harness::start().await;
    let hello = h.hello().await;
    assert_eq!(hello["name"], "Test");
    assert_eq!(hello["protocol"], PROTOCOL);
    assert_eq!(hello["fingerprint"], h.running.fingerprint());
    assert_eq!(hello["fingerprint"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn a_paired_device_can_read_the_vault() {
    let h = Harness::start().await;
    let token = h.pair("Laptop").await;
    let journals = h.ok(&token, "list_journals", json!({})).await;
    assert_eq!(journals.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_request_with_no_token_is_refused() {
    let h = Harness::start().await;
    let response = h
        .http
        .post(h.url("/v1/call/list_journals"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_revoked_device_stops_working_at_once() {
    let h = Harness::start().await;
    let token = h.pair("Phone").await;
    let devices = h.running.server.registry.devices();
    h.running.server.registry.revoke(&devices[0].id).unwrap();

    let response = h.call(&token, "list_journals", json!({})).await;
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_wrong_pairing_code_does_not_pair() {
    let h = Harness::start().await;
    h.running.server.registry.new_pairing_code();
    let response = h
        .http
        .post(h.url("/v1/pair"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .json(&json!({ "code": "WRONGONE", "deviceName": "x", "scopes": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn a_client_speaking_another_protocol_is_told_both_numbers() {
    let h = Harness::start().await;
    let token = h.pair("Old build").await;
    let response = h
        .http
        .post(h.url("/v1/call/list_journals"))
        .header("x-everyday-protocol", (PROTOCOL + 7).to_string())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 426);
    let body: Value = response.json().await.unwrap();
    let message = body["message"].as_str().unwrap();
    assert!(message.contains(&(PROTOCOL + 7).to_string()), "{message}");
    assert!(message.contains(&PROTOCOL.to_string()), "{message}");
}

#[tokio::test]
async fn a_scope_the_token_lacks_is_refused() {
    let h = Harness::start().await;
    // Pair with only the library, the way a browser extension would.
    let code = h.running.server.registry.new_pairing_code();
    let body: Value = h
        .http
        .post(h.url("/v1/pair"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .json(&json!({ "code": code, "deviceName": "Extension", "scopes": ["library"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = body["token"].as_str().unwrap();

    assert_eq!(h.call(token, "list_journals", json!({})).await.status(), 403);
    assert!(h.call(token, "list_kinds", json!({})).await.status().is_success());
}

#[tokio::test]
async fn an_attachment_round_trips_and_a_range_reads_the_middle() {
    let h = Harness::start().await;
    let token = h.pair("Laptop").await;

    // Large enough to span more than one sealed chunk, so the range read is a
    // real one rather than a slice of a single block.
    let payload: Vec<u8> = (0..700_000u32).map(|n| (n % 251) as u8).collect();
    let id: String = h
        .http
        .post(h.url("/v1/blob"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .header("authorization", format!("Bearer {token}"))
        .body(payload.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let response = h
        .http
        .get(h.url(&format!("/v1/blob/{id}")))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .header("authorization", format!("Bearer {token}"))
        .header("range", "bytes=300000-300099")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    let bytes = response.bytes().await.unwrap();
    assert_eq!(bytes.len(), 100);
    assert_eq!(&bytes[..], &payload[300_000..300_100]);
}

#[tokio::test]
async fn a_write_reaches_the_other_client_and_not_the_one_that_made_it() {
    use futures::StreamExt;

    let h = Harness::start().await;
    let writer = h.pair("Writer").await;
    let watcher = h.pair("Watcher").await;

    let mut stream = h
        .http
        .get(h.url("/v1/events"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .header("authorization", format!("Bearer {watcher}"))
        .send()
        .await
        .unwrap()
        .bytes_stream();

    let minted = h.ok(&writer, "new_journal", json!({ "name": "From the writer" })).await;
    h.ok(&writer, "save_journal", json!({ "journal": minted })).await;

    // The watcher hears about it.
    let heard = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if text.contains("\"journal\"") {
                return text;
            }
        }
        text
    })
    .await
    .expect("the event stream went quiet");
    assert!(heard.contains("changed"), "{heard}");
    assert!(heard.contains("\"journal\""), "{heard}");

    // And the writer does not hear about its own.
    let mut own = h
        .http
        .get(h.url("/v1/events"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .header("authorization", format!("Bearer {writer}"))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    let again = h.ok(&writer, "new_journal", json!({ "name": "Second" })).await;
    h.ok(&writer, "save_journal", json!({ "journal": again })).await;
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(600), async {
        let mut text = String::new();
        while let Some(chunk) = own.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if text.contains("changed") {
                return text;
            }
        }
        text
    })
    .await;
    assert!(
        quiet.is_err() || !quiet.unwrap().contains("changed"),
        "a client was told about its own write"
    );
}

#[tokio::test]
async fn a_retried_write_is_answered_from_the_first_attempt() {
    let h = Harness::start().await;
    let token = h.pair("Flaky").await;

    let journals = h.ok(&token, "list_journals", json!({})).await;
    let journal_id = journals[0]["id"].clone();
    let mut entry = h.ok(&token, "new_entry", json!({ "journalId": journal_id })).await;
    entry["title"] = json!("first");
    h.ok(&token, "save_entry", json!({ "entry": entry.clone(), "expect": null })).await;

    let loaded = h.ok(&token, "get_entry", json!({ "id": entry["id"] })).await;
    let mut edited = loaded.clone();
    edited["title"] = json!("second");
    edited["updatedAt"] = json!(jiff::Timestamp::now().to_string());
    let args = json!({ "entry": edited, "expect": loaded["updatedAt"] });

    async fn save(h: &Harness, token: &str, args: &Value) -> reqwest::StatusCode {
        h.http
            .post(h.url("/v1/call/save_entry"))
            .header("x-everyday-protocol", PROTOCOL.to_string())
            .header("authorization", format!("Bearer {token}"))
            // The same id both times: this is one write that the client could
            // not tell had landed, not two writes.
            .header("x-everyday-request", "save-1")
            .json(args)
            .send()
            .await
            .unwrap()
            .status()
    }
    assert!(save(&h, &token, &args).await.is_success());
    assert!(
        save(&h, &token, &args).await.is_success(),
        "a retry was refused as a conflict against itself"
    );
}

#[tokio::test]
async fn unlocking_is_serialised_so_it_cannot_be_used_to_burn_the_machine_down() {
    let h = Harness::start_with(Some("correct horse battery staple")).await;
    let token = h.pair("Laptop").await;
    // Each attempt burns 64 MiB of Argon2 by design, which is a fine cost to
    // impose on somebody typing a password and an excellent denial of service
    // to hand a stranger. So a device that keeps guessing is turned away.
    let mut refused = 0;
    for _ in 0..8 {
        let status = h.call(&token, "unlock", json!({ "password": "wrong" })).await.status();
        if status == 429 {
            // ...and the right password still works once the lockout lifts,
            // which is checked separately rather than by waiting ten minutes.
            return;
        }
        refused += 1;
    }
    panic!("{refused} unlock attempts and never a refusal to keep trying");
}

#[cfg(unix)]
#[tokio::test]
async fn the_local_socket_needs_no_token() {
    let h = Harness::start().await;
    let socket = h._config_dir.path().join("test.sock");
    let _stop =
        everyday_server::serve_socket(h.running.server.clone(), socket.clone()).await.unwrap();

    // A raw request over the socket, because no HTTP client here speaks Unix
    // sockets. One request, one response, which is all this needs to prove.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let body = b"{}";
    let request = format!(
        "POST /v1/call/list_journals HTTP/1.1\r\nHost: localhost\r\n\
         x-everyday-protocol: {PROTOCOL}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains("\"name\""), "{answer}");
}

#[cfg(unix)]
#[tokio::test]
async fn pairing_is_refused_over_the_local_socket() {
    let h = Harness::start().await;
    let socket = h._config_dir.path().join("pair.sock");
    let _stop =
        everyday_server::serve_socket(h.running.server.clone(), socket.clone()).await.unwrap();
    let code = h.running.server.registry.new_pairing_code();

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let body = serde_json::to_vec(&json!({
        "code": code, "deviceName": "sneaky", "scopes": []
    }))
    .unwrap();
    let request = format!(
        "POST /v1/pair HTTP/1.1\r\nHost: localhost\r\nx-everyday-protocol: {PROTOCOL}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
}

#[tokio::test]
async fn the_right_password_unlocks_a_vault_over_the_wire() {
    let h = Harness::start_with(Some("correct horse battery staple")).await;
    let token = h.pair("Laptop").await;
    assert!(!h.service.get().unwrap().is_unlocked());

    let status =
        h.ok(&token, "unlock", json!({ "password": "correct horse battery staple" })).await;
    assert_eq!(status["unlocked"], true);
    assert!(h.service.get().unwrap().is_unlocked());
}

#[tokio::test]
async fn a_vault_that_refuses_remote_unlocking_says_so() {
    everyday_server::install_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let vault = everyday_vault::create(
        dir.path(),
        everyday_core::VaultConfig {
            name: "Closed".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: Some("a password nobody will guess".into()),
            kdf: Default::default(),
            auto_lock_seconds: 900,
            forget_key_seconds: 0,
        },
    )
    .unwrap();
    vault.lock();
    let service = Arc::new(Service::new());
    service.set(vault);

    let config = everyday_server::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        enabled: true,
        allow_remote_unlock: false,
        no_tls: false,
    };
    let parts = everyday_server::prepare(config_dir.path(), &config).unwrap();
    let cert_pem = parts.identity.as_ref().unwrap().cert_pem.clone();
    let running = everyday_server::start(service, parts, &config, "Closed".into()).await.unwrap();
    let http = everyday_server::client::pinned_client(&cert_pem).unwrap();
    let base = format!("https://localhost:{}", running.address.port());

    let code = running.server.registry.new_pairing_code();
    let paired: Value = http
        .post(format!("{base}/v1/pair"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .json(&json!({ "code": code, "deviceName": "Phone", "scopes": [] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let response = http
        .post(format!("{base}/v1/call/unlock"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .header("authorization", format!("Bearer {}", paired["token"].as_str().unwrap()))
        .json(&json!({ "password": "a password nobody will guess" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn the_service_is_the_same_one_the_local_window_is_using() {
    // The property that makes the desktop app able to serve its own vault: a
    // write over the wire is visible to whoever holds the service directly.
    let h = Harness::start().await;
    let token = h.pair("Laptop").await;
    let minted = h.ok(&token, "new_journal", json!({ "name": "Over the wire" })).await;
    h.ok(&token, "save_journal", json!({ "journal": minted })).await;

    let local = h.service.get().unwrap().journals().unwrap();
    assert!(local.iter().any(|j| j.name == "Over the wire"));
}

#[test]
fn a_pairing_link_is_what_a_client_reads_back() {
    let invitation = pairing::invitation("100.64.0.12:7397", "abcdef", "PQRS2345", "Journal");
    let invite = pairing::parse(&invitation.url).unwrap();
    assert_eq!(invite.host, "100.64.0.12:7397");
    assert_eq!(invite.fingerprint, "abcdef");
    assert_eq!(invite.code, "PQRS2345");
}

// ---- the client half -----------------------------------------------------
//
// The same `RemoteClient` the desktop shell forwards through, driven against a
// real server. This is what makes phase three's claim -- that a remote client
// is the same application -- checkable rather than asserted.

#[tokio::test]
async fn a_client_pairs_from_a_link_and_then_speaks_for_the_vault() {
    use everyday_server::client::RemoteClient;

    let h = Harness::start().await;
    let code = h.running.server.registry.new_pairing_code();
    let invitation = pairing::invitation(
        &format!("localhost:{}", h.running.address.port()),
        h.running.fingerprint(),
        &code,
        "Test",
    );
    let invite = pairing::parse(&invitation.url).unwrap();

    let (client, _token) = RemoteClient::pair(&invite, "Second laptop").await.unwrap();
    let ctx = everyday_service::Ctx::local();

    let journals = client.call(&ctx, "list_journals", json!({})).await.unwrap();
    assert_eq!(journals.as_array().unwrap().len(), 1);

    // A write from the client is a write to the vault.
    let minted = client.call(&ctx, "new_journal", json!({ "name": "Remote" })).await.unwrap();
    client.call(&ctx, "save_journal", json!({ "journal": minted })).await.unwrap();
    let local = h.service.get().unwrap().journals().unwrap();
    assert!(local.iter().any(|j| j.name == "Remote"));
}

#[tokio::test]
async fn a_link_whose_fingerprint_does_not_match_is_refused_before_anything_is_sent() {
    use everyday_server::client::RemoteClient;

    let h = Harness::start().await;
    let code = h.running.server.registry.new_pairing_code();
    let invite = everyday_server::pairing::Invite {
        host: format!("localhost:{}", h.running.address.port()),
        // A fingerprint for some other certificate entirely.
        fingerprint: "0".repeat(64),
        code: code.clone(),
        name: "Test".into(),
    };
    let e = RemoteClient::pair(&invite, "Impostor").await.err().expect("a wrong fingerprint");
    assert_eq!(e.code, "fingerprint");

    // ...and the one-time code was not spent, so the honest client can still
    // use it. A failed pairing must not cost somebody their code.
    let invitation = pairing::invitation(
        &format!("localhost:{}", h.running.address.port()),
        h.running.fingerprint(),
        &code,
        "Test",
    );
    let good = pairing::parse(&invitation.url).unwrap();
    assert!(RemoteClient::pair(&good, "Laptop").await.is_ok());
}

#[tokio::test]
async fn a_client_reads_an_attachment_and_then_reads_it_from_memory() {
    use everyday_server::client::RemoteClient;

    let h = Harness::start().await;
    let code = h.running.server.registry.new_pairing_code();
    let invitation = pairing::invitation(
        &format!("localhost:{}", h.running.address.port()),
        h.running.fingerprint(),
        &code,
        "Test",
    );
    let (client, _) =
        RemoteClient::pair(&pairing::parse(&invitation.url).unwrap(), "Laptop").await.unwrap();

    let payload: Vec<u8> = (0..500_000u32).map(|n| (n % 251) as u8).collect();
    let id = client.put_blob(payload.clone()).await.unwrap();

    assert_eq!(client.blob_len(&id).await.unwrap(), payload.len() as u64);
    let middle = client.blob_range(&id, 200_000, 128).await.unwrap();
    assert_eq!(middle, &payload[200_000..200_128]);

    // The second read of the same range is the cache's. Blobs are
    // content-addressed, so this can never be a stale answer.
    let again = client.blob_range(&id, 200_000, 128).await.unwrap();
    assert_eq!(again, middle);

    // Locking forgets it, and the bytes are still correct afterwards.
    client.clear_cache();
    assert_eq!(client.blob_range(&id, 200_000, 128).await.unwrap(), middle);
}

#[tokio::test]
async fn a_client_hears_what_the_server_says() {
    use everyday_server::client::{RemoteClient, ServerEvent};
    use std::sync::Mutex;

    let h = Harness::start().await;
    let code = h.running.server.registry.new_pairing_code();
    let invitation = pairing::invitation(
        &format!("localhost:{}", h.running.address.port()),
        h.running.fingerprint(),
        &code,
        "Test",
    );
    let (client, _) =
        RemoteClient::pair(&pairing::parse(&invitation.url).unwrap(), "Watcher").await.unwrap();
    let client = Arc::new(client);

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let (client, seen) = (client.clone(), seen.clone());
        tokio::spawn(async move {
            let _ = client
                .events(|event| {
                    let name = match event {
                        ServerEvent::Changed(change) => {
                            format!("changed:{}", serde_json::to_string(&change.kind).unwrap())
                        }
                        ServerEvent::Notify(_) => "notify".into(),
                        ServerEvent::LockState { locked } => format!("lock:{locked}"),
                    };
                    seen.lock().unwrap().push(name);
                })
                .await;
        });
    }
    // Give the subscription a moment to be registered before the write.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let token = h.pair("Somebody else").await;
    let minted = h.ok(&token, "new_journal", json!({ "name": "Elsewhere" })).await;
    h.ok(&token, "save_journal", json!({ "journal": minted })).await;

    for _ in 0..50 {
        if seen.lock().unwrap().iter().any(|s| s.contains("journal")) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the client heard nothing: {:?}", seen.lock().unwrap());
}

/// A client reaches a server at an address the certificate does not name.
///
/// The property that makes moving networks safe. A certificate cannot list
/// every address a machine might one day have, and regenerating it to add one
/// would change the fingerprint every paired device pinned -- so the client
/// identifies the server by the certificate itself and ignores the name. This
/// serves a certificate naming nothing relevant and connects anyway.
#[tokio::test]
async fn the_certificate_is_the_identity_rather_than_the_address() {
    everyday_server::install_crypto_provider();

    let dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let vault = everyday_vault::create(
        dir.path(),
        everyday_core::VaultConfig {
            name: "Elsewhere".into(),
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

    let config = everyday_server::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        enabled: true,
        allow_remote_unlock: true,
        no_tls: false,
    };
    // A certificate that names somewhere this test will not dial.
    let identity = everyday_server::tls::Identity::load_or_create(
        config_dir.path(),
        &["a-name-nobody-will-dial.invalid".to_string()],
    )
    .unwrap();
    let cert_pem = identity.cert_pem.clone();
    let parts = everyday_server::Parts {
        registry: Arc::new(
            everyday_server::Registry::open(config_dir.path().join("devices.json")).unwrap(),
        ),
        broadcaster: Arc::new(everyday_server::Broadcaster::new()),
        identity: Some(identity),
    };
    let running =
        everyday_server::start(service, parts, &config, "Elsewhere".into()).await.unwrap();

    let http = everyday_server::client::pinned_client(&cert_pem).unwrap();
    let response = http
        .get(format!("https://127.0.0.1:{}/v1/hello", running.address.port()))
        .send()
        .await
        .expect("the pin is the identity, so the name must not matter");
    assert!(response.status().is_success());

    // ...and a certificate that is *not* the pinned one is still refused,
    // which is the half that makes the above safe rather than merely lax.
    let other = everyday_server::tls::Identity::load_or_create(
        &config_dir.path().join("other"),
        &["a-name-nobody-will-dial.invalid".to_string()],
    )
    .unwrap();
    let wrong = everyday_server::client::pinned_client(&other.cert_pem).unwrap();
    assert!(
        wrong
            .get(format!("https://127.0.0.1:{}/v1/hello", running.address.port()))
            .send()
            .await
            .is_err(),
        "a client accepted a certificate it had not pinned"
    );
}

/// The fifth app works over the wire, like the other four.
///
/// The claim the command table is *for*: an app added to this application is a
/// module in the service, and every client gets it without anything being
/// wired up per feature. The Overview's records arrived after server mode did,
/// so this is the test that says so rather than assumes it.
#[tokio::test]
async fn a_client_can_use_the_records_that_arrived_after_it() {
    let h = Harness::start().await;
    let token = h.pair("Laptop").await;

    // Roles and goals, minted and saved from the other machine.
    let seeded = h.ok(&token, "seed_roles", json!({})).await;
    assert!(seeded.as_u64().unwrap() > 0, "a fresh vault offers a starting set");

    let roles = h.ok(&token, "list_roles", json!({})).await;
    let role_id = roles[0]["id"].clone();
    // The counts the sidebar draws come back with each role.
    assert!(roles[0]["goals"].is_number(), "{roles}");

    let goal =
        h.ok(&token, "new_goal", json!({ "roleId": role_id, "title": "Read twelve books" })).await;
    h.ok(&token, "save_goal", json!({ "goal": goal })).await;

    let goals = h.ok(&token, "list_goals", json!({ "query": {} })).await;
    assert!(goals.as_array().unwrap().iter().any(|g| g["title"] == "Read twelve books"), "{goals}");

    // The report, which is the whole point of the app: one grouped scan that
    // opens no ciphertext, over a window a client chose.
    let report =
        h.ok(&token, "time_by_purpose", json!({ "from": "2026-09-07", "to": "2026-09-13" })).await;
    assert!(report["purposes"].is_array(), "{report}");
    assert!(report["events"].is_array(), "{report}");

    // And trackers, which moved out of journals in the same change.
    let tracker = h.ok(&token, "new_tracker", json!({ "name": "Floss", "kind": "check" })).await;
    h.ok(&token, "save_tracker", json!({ "tracker": tracker.clone() })).await;
    let trackers = h.ok(&token, "list_trackers", json!({})).await;
    assert!(trackers.as_array().unwrap().iter().any(|t| t["name"] == "Floss"), "{trackers}");

    let reading = h
        .ok(
            &token,
            "log_reading",
            json!({
                "trackerId": tracker["id"],
                "value": 1.0,
                "date": "2026-09-09",
            }),
        )
        .await;
    assert_eq!(reading["value"], 1.0, "{reading}");

    // The local window sees all of it, because there is one vault and one
    // service behind both.
    let local = h.service.get().unwrap();
    assert!(local.trackers().unwrap().iter().any(|t| t.name == "Floss"));
    assert!(!local.roles().unwrap().is_empty());
}

/// A token that does not hold the purpose scope cannot read the shape of
/// somebody's life.
///
/// Roles and goals are their own scope for a reason: a purpose is a pointer on
/// almost every record, so reading them tells you which roles exist and how
/// much time each takes without reading a single entry.
#[tokio::test]
async fn the_purpose_scope_can_be_withheld_on_its_own() {
    let h = Harness::start().await;
    let code = h.running.server.registry.new_pairing_code();
    let body: Value = h
        .http
        .post(h.url("/v1/pair"))
        .header("x-everyday-protocol", PROTOCOL.to_string())
        .json(&json!({ "code": code, "deviceName": "Shelf reader", "scopes": ["library"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = body["token"].as_str().unwrap();

    assert_eq!(h.call(token, "list_roles", json!({})).await.status(), 403);
    assert_eq!(h.call(token, "list_goals", json!({ "query": {} })).await.status(), 403);
    assert_eq!(
        h.call(token, "time_by_purpose", json!({ "from": "2026-09-07", "to": "2026-09-13" }))
            .await
            .status(),
        403
    );
    // ...and the scope it does hold still works.
    assert!(h.call(token, "list_kinds", json!({})).await.status().is_success());
}

/// `stop_and_wait` must return as soon as the listener is actually closed,
/// not sit out its five-second timeout.
///
/// The bug this guards against: the TLS branch of `start` spawned a serving
/// task that never signalled `stopped` on its way out, so a caller waiting
/// on `stop_and_wait` against a TLS listener always paid the full timeout,
/// whether or not the socket had already been released. `Harness::start`
/// runs with TLS on -- the default in every other test in this file -- so
/// this is the same listener the rest of the suite exercises, not a special
/// case built to dodge the bug.
#[tokio::test]
async fn stop_and_wait_returns_promptly_for_a_tls_listener() {
    let h = Harness::start().await;
    let began = std::time::Instant::now();
    h.running.stop_and_wait().await;
    assert!(
        began.elapsed() < std::time::Duration::from_secs(1),
        "stop_and_wait took {:?}, which means it hit its five-second timeout",
        began.elapsed()
    );
}
