//! The screen-time ingest routes, over real sockets: the shell's loopback
//! listener, and the sharing server's TLS router.
//!
//! The order a request is checked in -- `Host`, `Origin`, token, scope,
//! body -- is tested against the loopback router itself in
//! `src/screentime.rs`. What is checked here is what only a socket shows:
//! that the listener refuses to listen beyond this machine, that an
//! extension's request over a real connection gets its answer and its CORS
//! headers, and that the sharing server's copy of the routes reaches a real
//! vault under the token's own scope -- with no protocol header, since the
//! extensions are not built from this workspace.

use futures::future::BoxFuture;
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::screentime::{CollectorConfig, IngestReport};
use karmayoga_server::auth::Registry;
use karmayoga_server::screentime::{self, Forward, HeartbeatBody};
use karmayoga_service::{CommandResult, Scope, Service};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

// ---- the loopback listener ---------------------------------------------------

/// A shell that writes down what it was handed.
#[derive(Default)]
struct Fake {
    bodies: Mutex<Vec<HeartbeatBody>>,
    heard: Mutex<Vec<String>>,
}

impl Forward for Fake {
    fn device(&self) -> String {
        "laptop".into()
    }

    fn record(&self, body: HeartbeatBody) -> BoxFuture<'static, CommandResult<IngestReport>> {
        let accepted = body.beats.len() as u32;
        self.bodies.lock().unwrap().push(body);
        Box::pin(async move { Ok(IngestReport { accepted, rejected: 0, spans: 1 }) })
    }

    fn config(&self) -> BoxFuture<'static, CommandResult<CollectorConfig>> {
        Box::pin(async { Ok(CollectorConfig { idle_minutes: 5, keep_titles: false }) })
    }

    fn heard(&self, collector: &str) {
        self.heard.lock().unwrap().push(collector.to_string());
    }
}

fn registry(dir: &tempfile::TempDir) -> Arc<Registry> {
    Arc::new(Registry::open(dir.path().join("devices.json")).unwrap())
}

/// One run on a site, a couple of minutes ago.
fn beat() -> Value {
    let start = Timestamp::now() - SignedDuration::from_secs(120);
    let end = start + SignedDuration::from_secs(30);
    json!({
        "kind": "site", "app": "Firefox", "host": "github.com",
        "start": start.to_string(), "end": end.to_string(),
    })
}

#[tokio::test]
async fn the_listener_will_not_listen_beyond_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:7399"] {
        let outcome =
            screentime::start(Arc::new(Fake::default()), registry(&dir), address.parse().unwrap())
                .await;
        let e = outcome.err().unwrap_or_else(|| panic!("listened on {address}"));
        assert_eq!(e.code, "invalid", "{address}");
    }
}

#[tokio::test]
async fn over_a_real_socket_an_extension_says_hello_and_delivers() {
    let dir = tempfile::tempdir().unwrap();
    let registry = registry(&dir);
    let (token, _) = registry.issue("Firefox", vec![Scope::Heartbeats], false).unwrap();
    let fake = Arc::new(Fake::default());
    let running =
        screentime::start(fake.clone(), registry, "127.0.0.1:0".parse().unwrap()).await.unwrap();
    let base = format!("http://127.0.0.1:{}", running.address.port());
    let http = reqwest::Client::new();
    let origin = "chrome-extension://abcdefghijklmnop";

    let response = http
        .get(format!("{base}/v1/screentime/hello"))
        .header("authorization", format!("Bearer {token}"))
        .header("origin", origin)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["access-control-allow-origin"], origin);
    let hello: Value = response.json().await.unwrap();
    assert_eq!(hello["device"], "laptop");
    assert_eq!(hello["idleMinutes"], 5);
    assert_eq!(hello["keepTitles"], false);

    let response = http
        .post(format!("{base}/v1/screentime/heartbeats"))
        .header("authorization", format!("Bearer {token}"))
        .header("origin", origin)
        .json(&json!({ "collector": "chrome", "beats": [beat(), beat()] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let report: Value = response.json().await.unwrap();
    assert_eq!(report, json!({ "accepted": 2, "rejected": 0, "spans": 1 }));
    assert_eq!(fake.bodies.lock().unwrap()[0].device.as_deref(), Some("laptop"));
    assert_eq!(*fake.heard.lock().unwrap(), vec!["chrome".to_string()]);

    // A page that rebinds its own name to 127.0.0.1 still sends its name.
    let response = http
        .post(format!("{base}/v1/screentime/heartbeats"))
        .header("host", "evil.example")
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "collector": "chrome", "beats": [beat()] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(fake.bodies.lock().unwrap().len(), 1);

    running.stop_and_wait().await;
}

// ---- the sharing server --------------------------------------------------------

struct Sharing {
    _dir: tempfile::TempDir,
    _config_dir: tempfile::TempDir,
    running: karmayoga_server::Running,
    base: String,
    http: reqwest::Client,
}

impl Sharing {
    /// A sharing server over TLS, as `tests/serve.rs` stands one up. With a
    /// password, the vault is locked when it starts.
    async fn start(password: Option<&str>) -> Sharing {
        karmayoga_server::install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(
            dir.path(),
            karmayoga_core::VaultConfig {
                name: "Test".into(),
                backend: "sqlite".into(),
                settings: Default::default(),
                password: password.map(str::to_string),
                kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
                auto_lock_seconds: 900,
                forget_key_seconds: 0,
            },
        )
        .unwrap();
        vault.save_journal(&karmayoga_core::Journal::new("Journal")).unwrap();
        if password.is_some() {
            vault.lock();
        }
        let service = Arc::new(Service::new());
        service.set(vault);

        let config = karmayoga_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            enabled: true,
            allow_remote_unlock: true,
            no_tls: false,
        };
        let parts = karmayoga_server::prepare(config_dir.path(), &config).unwrap();
        service.set_events(parts.broadcaster.clone());
        let cert_pem = parts.identity.as_ref().unwrap().cert_pem.clone();
        let running =
            karmayoga_server::start(service, parts, &config, "Test".into()).await.unwrap();
        let http = karmayoga_server::client::pinned_client(&cert_pem).unwrap();
        let base = format!("https://localhost:{}", running.address.port());
        Sharing { _dir: dir, _config_dir: config_dir, running, base, http }
    }

    fn token(&self, scopes: Vec<Scope>, mcp: bool) -> String {
        self.running.server.registry.issue("Phone", scopes, mcp).unwrap().0
    }

    /// No `X-KarmaYoga-Protocol`: a collector does not send one.
    async fn post(&self, token: Option<&str>, body: Value) -> (u16, Value) {
        let mut request = self.http.post(format!("{}/v1/screentime/heartbeats", self.base));
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = request.json(&body).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn hello(&self, token: &str) -> (u16, Value) {
        let response = self
            .http
            .get(format!("{}/v1/screentime/hello", self.base))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn the_sharing_server_takes_heartbeats_that_name_their_device() {
    let s = Sharing::start(None).await;
    let token = s.token(vec![Scope::Heartbeats], false);

    let (status, hello) = s.hello(&token).await;
    assert_eq!(status, 200, "{hello}");
    assert_eq!(hello["device"], "", "the server is not the collector's machine");
    assert_eq!(hello["idleMinutes"], 3);
    assert_eq!(hello["keepTitles"], true);

    let (status, report) = s
        .post(Some(&token), json!({ "device": "desk", "collector": "firefox", "beats": [beat()] }))
        .await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report, json!({ "accepted": 1, "rejected": 0, "spans": 1 }));

    // It cannot tell which machine sent a batch that does not say.
    for body in [
        json!({ "collector": "firefox", "beats": [beat()] }),
        json!({ "device": "  ", "collector": "firefox", "beats": [beat()] }),
    ] {
        let (status, answer) = s.post(Some(&token), body).await;
        assert_eq!(status, 400, "{answer}");
        assert_eq!(answer["code"], "invalid");
    }
}

#[tokio::test]
async fn the_sharing_server_wants_a_token_that_may_send_heartbeats() {
    let s = Sharing::start(None).await;
    let body = json!({ "device": "desk", "collector": "firefox", "beats": [beat()] });

    let (status, answer) = s.post(None, body.clone()).await;
    assert_eq!(status, 401, "{answer}");
    assert_eq!(answer["code"], "unauthorized");
    let (status, _) = s.post(Some("not-a-token"), body.clone()).await;
    assert_eq!(status, 401);

    for token in [
        s.token(vec![Scope::Library], false),
        s.token(vec![Scope::ScreenTime], false),
        s.token(vec![Scope::All], true),
    ] {
        let (status, answer) = s.post(Some(&token), body.clone()).await;
        assert_eq!(status, 403, "{answer}");
        assert_eq!(answer["code"], "forbidden");
    }

    // A paired copy of this application holds `All`, and may.
    let paired = s.token(vec![Scope::All], false);
    let (status, answer) = s.post(Some(&paired), body).await;
    assert_eq!(status, 200, "{answer}");
}

#[tokio::test]
async fn a_locked_vault_answers_a_collector_with_a_conflict() {
    let s = Sharing::start(Some("correct horse battery")).await;
    let token = s.token(vec![Scope::Heartbeats], false);
    let (status, answer) = s
        .post(Some(&token), json!({ "device": "desk", "collector": "firefox", "beats": [beat()] }))
        .await;
    assert_eq!(status, 409, "{answer}");
    assert_eq!(answer["code"], "locked");
    let (status, answer) = s.hello(&token).await;
    assert_eq!(status, 409, "{answer}");
    assert_eq!(answer["code"], "locked");
}
