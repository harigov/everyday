//! The phone's Health Auto Export route, `POST /v1/health/hae/{source}`: on
//! the sharing server over TLS, and on web access's router, where the
//! gatekeeper's session must count for nothing.
//!
//! What is checked here is the order a request is judged in -- token, scope,
//! source, body -- that a push reaches a real vault under the token's own
//! scope, and that a token is good for the one source it was minted for.
//! What a push *means* (units, deduplication, the automation's status) is
//! the vault's, tested beside it in `karmayoga-core`.

use axum::body::Body;
use axum::http::Request;
use jiff::Timestamp;
use karmayoga_core::health::{Connector, HealthSource};
use karmayoga_core::id::HealthSourceId;
use karmayoga_server::health::MAX_BODY_BYTES;
use karmayoga_server::web::session::Sessions;
use karmayoga_server::{Broadcaster, Server, Transport};
use karmayoga_service::{Ctx, Scope, Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

/// A push of one day's steps, as Health Auto Export's "Health Metrics"
/// automation sends them (Export Version 2, Summarize Data off).
fn push_body() -> Value {
    json!({
        "data": {
            "metrics": [{
                "name": "step_count",
                "units": "count",
                "data": [{
                    "date": "2026-11-14 08:00:00 -0800",
                    "qty": 120,
                    "source": "Hari's iPhone",
                }],
            }],
        },
    })
}

struct Sharing {
    _dir: tempfile::TempDir,
    _config_dir: tempfile::TempDir,
    running: karmayoga_server::Running,
    base: String,
    http: reqwest::Client,
}

impl Sharing {
    /// A sharing server over TLS, as `tests/screentime.rs` stands one up.
    /// With a password, the vault is locked when it starts.
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

    /// A phone's token: write-only, for health, and not yet any source's.
    fn phone(&self) -> String {
        self.token(vec![Scope::HealthIngest], false)
    }

    /// A phone's token as `health_push_setup` makes one: bound to `source`,
    /// the way the shell tells the vault, and good for no other.
    async fn phone_for(&self, source: HealthSourceId) -> String {
        let (token, id) = self
            .running
            .server
            .registry
            .issue("Health Auto Export", vec![Scope::HealthIngest], false)
            .unwrap();
        self.bind(source, &id).await;
        token
    }

    /// Tell the vault which registry token may push to `source` (none, for
    /// an empty one), through the command the shell calls.
    async fn bind(&self, source: HealthSourceId, token_id: &str) {
        let args = json!({ "sourceId": source.to_string(), "tokenId": token_id });
        self.running
            .server
            .service
            .call(Ctx::local(), "set_health_source_token", args)
            .await
            .unwrap();
    }

    /// A Health Auto Export source in the vault, as Settings makes one.
    fn source(&self, paused: bool) -> HealthSourceId {
        let id = HealthSourceId::new();
        let now = Timestamp::now();
        self.running
            .server
            .service
            .get()
            .unwrap()
            .save_health_source(&HealthSource {
                id,
                connector: Connector::HealthAutoExport,
                name: "Hari's iPhone".into(),
                paused,
                status: Default::default(),
                token_id: String::new(),
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        id
    }

    /// What the phone sends: a token, the automation's headers, a JSON body.
    /// No `X-KarmaYoga-Protocol`: Health Auto Export does not send one.
    async fn post(&self, source: &str, token: Option<&str>, body: &Value) -> (u16, Value) {
        let mut request = self
            .http
            .post(format!("{}/v1/health/hae/{source}", self.base))
            .header("automation-id", "A1")
            .header("automation-name", "Hourly metrics")
            .header("automation-period", "Since Last Sync")
            .json(body);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// Web access's router over this server's own service and registry, and
    /// a browser signed in to it.
    fn web(&self) -> (axum::Router, String) {
        let sessions = Arc::new(Sessions::new());
        let opened = sessions.open("127.0.0.1".parse().unwrap(), self._dir.path());
        let server = Server::for_web(
            self.running.server.service.clone(),
            self.running.server.registry.clone(),
            Arc::new(Broadcaster::new()),
            "Test".into(),
            sessions,
        );
        (karmayoga_server::routes::router(server, Transport::Web), opened.id)
    }
}

/// One request to web access's router, answered as status and JSON.
async fn send(router: &axum::Router, request: Request<Body>) -> (u16, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn web_push(source: &str, token: Option<&str>, session: Option<&str>, body: Body) -> Request<Body> {
    let mut request = Request::builder().method("POST").uri(format!("/v1/health/hae/{source}"));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    // The two headers the gatekeeper sets once a cookie checks out; a client
    // that sent them itself would have had them removed before routing.
    if let Some(session) = session {
        request = request
            .header("x-karmayoga-web-session", session)
            .header("x-karmayoga-web-access", "full");
    }
    request.body(body).unwrap()
}

#[tokio::test]
async fn a_push_reaches_the_vault_under_the_phones_own_token() {
    let s = Sharing::start(None).await;
    let source = s.source(false);
    let phone = s.phone_for(source).await;

    let (status, report) = s.post(&source.to_string(), Some(&phone), &push_body()).await;
    assert_eq!(status, 200, "{report}");
    assert!(report["samplesAdded"].is_u64(), "an ingest report: {report}");

    // The automation's headers travelled with it.
    let vault = s.running.server.service.get().unwrap();
    let seen = vault.health_source(source).unwrap().status.automations;
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].id, "A1");
    assert_eq!(seen[0].period, "Since Last Sync");
}

#[tokio::test]
async fn a_token_is_good_for_the_one_source_it_was_minted_for() {
    let s = Sharing::start(None).await;
    let (mine, theirs) = (s.source(false), s.source(false));
    let phone = s.phone_for(mine).await;

    let (status, answer) = s.post(&mine.to_string(), Some(&phone), &push_body()).await;
    assert_eq!(status, 200, "{answer}");
    // The same token, another source: refused, and nothing is written there.
    let (status, answer) = s.post(&theirs.to_string(), Some(&phone), &push_body()).await;
    assert_eq!(status, 403, "{answer}");
    assert_eq!(answer["code"], "forbidden");
    let vault = s.running.server.service.get().unwrap();
    assert!(vault.health_source(theirs).unwrap().status.automations.is_empty());

    // A token that holds `All` is not a phone's either: only the one the
    // vault was told of may push.
    let paired = s.token(vec![Scope::All], false);
    let (status, answer) = s.post(&mine.to_string(), Some(&paired), &push_body()).await;
    assert_eq!(status, 403, "{answer}");

    // Revoking clears the binding, and the token with it.
    s.bind(mine, "").await;
    let (status, answer) = s.post(&mine.to_string(), Some(&phone), &push_body()).await;
    assert_eq!(status, 403, "{answer}");
}

#[tokio::test]
async fn a_push_wants_a_token_that_may_push_health() {
    let s = Sharing::start(None).await;
    let source = s.source(false).to_string();

    let (status, answer) = s.post(&source, None, &push_body()).await;
    assert_eq!(status, 401, "{answer}");
    assert_eq!(answer["code"], "unauthorized");
    let (status, _) = s.post(&source, Some("not-a-token"), &push_body()).await;
    assert_eq!(status, 401);

    for token in [
        s.token(vec![Scope::Library], false),
        // Reading health is not pushing it, nor the other way about.
        s.token(vec![Scope::Health], false),
        s.token(vec![Scope::Heartbeats], false),
        // A token minted for somebody else's model is `All`, and is not a phone.
        s.token(vec![Scope::All], true),
    ] {
        let (status, answer) = s.post(&source, Some(&token), &push_body()).await;
        assert_eq!(status, 403, "{answer}");
        assert_eq!(answer["code"], "forbidden");
    }
}

#[tokio::test]
async fn a_body_that_is_not_a_push_is_refused_after_the_token() {
    let s = Sharing::start(None).await;
    let source = s.source(false).to_string();
    let phone = s.phone();

    let response = s
        .http
        .post(format!("{}/v1/health/hae/{source}", s.base))
        .header("authorization", format!("Bearer {phone}"))
        .body("this is not json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["code"], "invalid");

    // JSON, but not an object.
    let (status, answer) = s.post(&source, Some(&phone), &json!([1, 2, 3])).await;
    assert_eq!(status, 400, "{answer}");

    // An address that names no source of any vault.
    let (status, answer) = s.post("not-an-id", Some(&phone), &push_body()).await;
    assert_eq!(status, 400, "{answer}");
    assert_eq!(answer["code"], "invalid");

    // A source this vault has never heard of.
    let (status, answer) =
        s.post(&HealthSourceId::new().to_string(), Some(&phone), &push_body()).await;
    assert!((400..500).contains(&status), "{status}: {answer}");
}

#[tokio::test]
async fn a_paused_source_is_not_written_to() {
    let s = Sharing::start(None).await;
    let source = s.source(true);
    let phone = s.phone_for(source).await;
    let (status, answer) = s.post(&source.to_string(), Some(&phone), &push_body()).await;
    assert_eq!(status, 400, "{answer}");
    assert!(answer["message"].as_str().unwrap().contains("paused"), "{answer}");
}

#[tokio::test]
async fn a_locked_vault_answers_the_phone_with_a_conflict() {
    let s = Sharing::start(Some("correct horse battery")).await;
    let source = HealthSourceId::new().to_string();
    let (status, answer) = s.post(&source, Some(&s.phone()), &push_body()).await;
    assert_eq!(status, 409, "{answer}");
    assert_eq!(answer["code"], "locked");
}

// ---- web access ------------------------------------------------------------

#[tokio::test]
async fn on_web_access_the_bearer_token_is_what_counts_and_a_session_is_not_enough() {
    let s = Sharing::start(None).await;
    let id = s.source(false);
    let source = id.to_string();
    let phone = s.phone_for(id).await;
    let (router, session) = s.web();
    let body = || Body::from(serde_json::to_vec(&push_body()).unwrap());

    // The phone: a token, no cookie, no session.
    let (status, report) = send(&router, web_push(&source, Some(&phone), None, body())).await;
    assert_eq!(status, 200, "{report}");
    assert!(report["samplesAdded"].is_u64(), "{report}");

    // A signed-in browser's page is not the phone.
    let (status, answer) = send(&router, web_push(&source, None, Some(&session), body())).await;
    assert_eq!(status, 401, "{answer}");
    assert_eq!(answer["code"], "unauthorized");

    // Nor does a session make a token that may not push into one that may.
    let reader = s.token(vec![Scope::Health], false);
    let (status, answer) =
        send(&router, web_push(&source, Some(&reader), Some(&session), body())).await;
    assert_eq!(status, 403, "{answer}");
}

#[tokio::test]
async fn a_push_past_the_limit_is_refused_before_it_is_read() {
    let s = Sharing::start(None).await;
    let source = s.source(false).to_string();
    let (router, _) = s.web();
    let too_big = || Body::from(vec![0u8; MAX_BODY_BYTES + 1]);

    // Judged by its token first, as every body is: a stranger is not
    // answered with how large a push may be.
    let (status, answer) = send(&router, web_push(&source, None, None, too_big())).await;
    assert_eq!(status, 401, "{answer}");

    let (status, answer) =
        send(&router, web_push(&source, Some(&s.phone()), None, too_big())).await;
    assert_eq!(status, 413, "{answer}");
    assert_eq!(answer["code"], "too_large");
}
