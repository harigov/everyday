//! The browser routes' tests: sign-in, the media token, and what a session may reach.

use super::*;
use crate::web::network::Network;
use axum::body::Body;
use axum::http::Request;
use karmayoga_service::Service;
use tower::ServiceExt;

const PEER: &str = "100.70.65.114:50000";
const OTHER_DEVICE: &str = "100.96.163.56:50000";
const PASSWORD: &str = "correct horse battery staple";
const ORIGIN: &str = "https://box.tail.ts.net:7400";

struct Rig {
    state: Arc<WebState>,
    service: Arc<Service>,
    _dirs: Vec<tempfile::TempDir>,
}

fn rig(password: Option<&str>, config: Config) -> Rig {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = karmayoga_vault::create(
        vault_dir.path(),
        karmayoga_core::VaultConfig {
            name: "Web".into(),
            password: password.map(str::to_string),
            kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        },
    )
    .unwrap();
    let service = Arc::new(Service::new());
    service.set(vault);

    let config_dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::open(config_dir.path().join("devices.json")).unwrap());
    let sessions = Arc::new(Sessions::new());
    let broadcaster = Arc::new(Broadcaster::new());
    service.set_events(Arc::new(crate::web::WebSink {
        broadcaster: broadcaster.clone(),
        sessions: sessions.clone(),
    }));
    let server = Server::for_web(
        service.clone(),
        registry.clone(),
        broadcaster.clone(),
        "Web".into(),
        sessions.clone(),
    );
    let mut gate = Gate::new(Network::default());
    gate.skip_whois = true;

    let ui = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ui.path().join("assets")).unwrap();
    std::fs::write(ui.path().join("index.html"), "<!doctype html><title>t</title>").unwrap();
    std::fs::write(ui.path().join("assets/app-abc.js"), "1").unwrap();

    let state = Arc::new(WebState {
        server,
        sessions,
        gate: Arc::new(gate),
        assets: Assets::folder(ui.path().to_path_buf()),
        broadcaster,
        registry,
        config,
        origins: RwLock::new(vec![ORIGIN.into()]),
    });
    Rig { state, service, _dirs: vec![vault_dir, config_dir, ui] }
}

async fn send(rig: &Rig, mut req: Request<Body>, peer: &str) -> Response {
    req.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    app(rig.state.clone()).oneshot(req).await.unwrap()
}

fn get(path: &str) -> axum::http::request::Builder {
    Request::builder().method(Method::GET).uri(path)
}

fn post(path: &str) -> axum::http::request::Builder {
    Request::builder().method(Method::POST).uri(path)
}

fn protocol() -> String {
    karmayoga_service::PROTOCOL.to_string()
}

async fn json_of(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Sign in from `peer`, answering the session cookie and media token.
async fn sign_in(rig: &Rig, peer: &str) -> (String, String) {
    let response = send(
        rig,
        post("/web/login")
            .header("content-type", "application/json")
            .header("x-karmayoga-web", "1")
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap(),
        peer,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("__Host-karmayoga="), "{cookie}");
    let media = json_of(response).await["mediaToken"].as_str().unwrap().to_string();
    (cookie, media)
}

async fn status_with(rig: &Rig, cookie: &str, peer: &str) -> StatusCode {
    send(
        rig,
        post("/v1/call/status")
            .header("cookie", cookie)
            .header("x-karmayoga-protocol", protocol())
            .body(Body::empty())
            .unwrap(),
        peer,
    )
    .await
    .status()
}

#[tokio::test]
async fn a_device_off_the_tailnet_is_not_even_handed_the_page() {
    let rig = rig(Some(PASSWORD), Config::default());
    for peer in ["192.168.1.5:4000", "127.0.0.1:4000", "172.17.0.2:4000", "[::1]:4000"] {
        let response = send(&rig, get("/").body(Body::empty()).unwrap(), peer).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{peer}");
        let response = send(&rig, get("/web/hello").body(Body::empty()).unwrap(), peer).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{peer}");
    }
}

#[tokio::test]
async fn the_page_and_its_files_are_served_to_a_device_on_the_tailnet() {
    let rig = rig(Some(PASSWORD), Config::default());
    let page = send(&rig, get("/").body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert!(
        page.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'self'")
    );
    assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");

    let script = send(&rig, get("/assets/app-abc.js").body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(script.status(), StatusCode::OK);
    assert!(script.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));

    // A place in the interface is the page; a missing file is not.
    let place = send(&rig, get("/mail/inbox").body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(place.status(), StatusCode::OK);
    let missing = send(&rig, get("/missing.js").body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let api = send(&rig, get("/v1/nothing").body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(api.status(), StatusCode::NOT_FOUND);
    // Pairing is not something a browser does.
    let pair = send(&rig, post("/v1/pair").body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(pair.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_request_passed_on_by_a_proxy_is_refused() {
    let rig = rig(Some(PASSWORD), Config::default());
    for proxy in ["x-forwarded-for", "forwarded", "x-real-ip", "via"] {
        let response =
            send(&rig, get("/").header(proxy, "1.2.3.4").body(Body::empty()).unwrap(), PEER)
                .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{proxy}");
    }
}

#[tokio::test]
async fn the_password_opens_a_session_and_a_wrong_one_does_not() {
    let rig = rig(Some(PASSWORD), Config::default());
    let hello =
        json_of(send(&rig, get("/web/hello").body(Body::empty()).unwrap(), PEER).await).await;
    assert_eq!(hello["signedIn"], false);
    assert_eq!(hello["status"]["unlocked"], false, "not signed in reads as locked");
    assert_eq!(hello["status"]["path"], "", "and gives nothing of the vault away");

    let wrong = send(
        &rig,
        post("/web/login")
            .header("content-type", "application/json")
            .header("x-karmayoga-web", "1")
            .body(Body::from(json!({ "password": "nope" }).to_string()))
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.headers().get(header::SET_COOKIE).is_none());

    // Without the page's own header, as a form on another site would send.
    let bare = send(
        &rig,
        post("/web/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(bare.status(), StatusCode::FORBIDDEN);

    let (cookie, media) = sign_in(&rig, PEER).await;
    assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::OK);

    let hello = json_of(
        send(
            &rig,
            get("/web/hello").header("cookie", &cookie).body(Body::empty()).unwrap(),
            PEER,
        )
        .await,
    )
    .await;
    assert_eq!(hello["signedIn"], true);
    assert_eq!(hello["mediaToken"], media.as_str(), "a reload is handed the same token");
    assert_eq!(hello["status"]["unlocked"], true);

    // The same cookie from another device is nobody.
    assert_eq!(status_with(&rig, &cookie, OTHER_DEVICE).await, StatusCode::UNAUTHORIZED);
    // And a call without the protocol header is refused before it runs.
    let no_protocol = send(
        &rig,
        post("/v1/call/status").header("cookie", &cookie).body(Body::empty()).unwrap(),
        PEER,
    )
    .await;
    assert_eq!(no_protocol.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn a_session_named_by_the_client_is_not_believed() {
    let rig = rig(Some(PASSWORD), Config::default());
    let opened = rig
        .state
        .sessions
        .open(PEER.parse::<SocketAddr>().unwrap().ip(), rig.service.get().unwrap().path());
    let response = send(
        &rig,
        post("/v1/call/status")
            .header(SESSION_HEADER, &opened.id)
            .header(ACCESS_HEADER, "full")
            .header("x-karmayoga-protocol", protocol())
            .body(Body::empty())
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_media_token_reads_media_and_nothing_else() {
    let rig = rig(Some(PASSWORD), Config::default());
    let (cookie, media) = sign_in(&rig, PEER).await;

    let blob: String = json_of(
        send(
            &rig,
            post("/v1/blob")
                .header("cookie", &cookie)
                .header("x-karmayoga-protocol", protocol())
                .body(Body::from(b"a picture".to_vec()))
                .unwrap(),
            PEER,
        )
        .await,
    )
    .await
    .as_str()
    .unwrap()
    .to_string();

    // An `<img>` sends no protocol header and, in a sandboxed frame, no
    // cookie: the token in the address is all it has.
    let picture = send(
        &rig,
        get(&format!("/m/{media}/v1/blob/{blob}")).body(Body::empty()).unwrap(),
        PEER,
    )
    .await;
    assert_eq!(picture.status(), StatusCode::OK);
    // Sandboxed as a document, so a blob that sniffs as an SVG and is
    // opened by its address runs nothing on this origin.
    assert_eq!(picture.headers()[header::CONTENT_SECURITY_POLICY], API_POLICY);
    let bytes = axum::body::to_bytes(picture.into_body(), 1 << 20).await.unwrap();
    assert_eq!(&bytes[..], b"a picture");

    for (method, path) in [
        (Method::GET, format!("/m/{media}/v1/call/status")),
        (Method::POST, format!("/m/{media}/v1/call/status")),
        (Method::POST, format!("/m/{media}/v1/blob")),
        (Method::GET, format!("/m/{media}/web/hello")),
    ] {
        let response = send(
            &rig,
            Request::builder().method(method.clone()).uri(&path).body(Body::empty()).unwrap(),
            PEER,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    let wrong = send(
        &rig,
        get(&format!("/m/not-it/v1/blob/{blob}")).body(Body::empty()).unwrap(),
        PEER,
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let elsewhere = send(
        &rig,
        get(&format!("/m/{media}/v1/blob/{blob}")).body(Body::empty()).unwrap(),
        OTHER_DEVICE,
    )
    .await;
    assert_eq!(elsewhere.status(), StatusCode::UNAUTHORIZED);
    // And the token is not a cookie.
    let as_cookie = format!("__Host-karmayoga={media}");
    assert_eq!(status_with(&rig, &as_cookie, PEER).await, StatusCode::UNAUTHORIZED);
}

/// A file is drawn from the folder the way a blob is -- by an `<img>` that
/// has only the token in its address -- and uploaded to by a POST, which
/// the token must not be able to make.
#[tokio::test]
async fn the_media_token_reaches_a_files_bytes_and_not_its_upload() {
    let rig = rig(Some(PASSWORD), Config::default());
    let (cookie, media) = sign_in(&rig, PEER).await;
    let file = karmayoga_core::FileId::new();

    // Past the gatekeeper, which is all this is about: what the files
    // route then says of an id nobody has is its own business, and is
    // never the gatekeeper's "Nothing is here."
    let admitted = send(
        &rig,
        get(&format!("/m/{media}/v1/files/file/{file}?v=abc")).body(Body::empty()).unwrap(),
        PEER,
    )
    .await;
    let refusal = json_of(admitted).await;
    assert_ne!(refusal["message"], "Nothing is here.", "{refusal}");

    let wrong = send(
        &rig,
        get(&format!("/m/not-it/v1/files/file/{file}")).body(Body::empty()).unwrap(),
        PEER,
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    // An upload is a write: not through the media token, whatever it is
    // posted to...
    let uploaded = send(
        &rig,
        post(&format!("/m/{media}/v1/files/upload/abc?offset=0"))
            .body(Body::from(vec![0u8; 16]))
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(uploaded.status(), StatusCode::NOT_FOUND);

    // ...and through the cookie, the way the page does it, it gets as far
    // as the files route.
    let through_the_session = send(
        &rig,
        post("/v1/files/upload/abc?offset=0")
            .header("cookie", &cookie)
            .header("origin", ORIGIN)
            .header("x-karmayoga-protocol", protocol())
            .body(Body::from(vec![0u8; 16]))
            .unwrap(),
        PEER,
    )
    .await;
    assert_ne!(through_the_session.status(), StatusCode::UNAUTHORIZED);
    let answer = json_of(through_the_session).await;
    assert_ne!(answer["message"], "Nothing is here.", "{answer}");
}

#[tokio::test]
async fn locking_the_vault_signs_every_browser_out() {
    let rig = rig(Some(PASSWORD), Config::default());
    let (cookie, media) = sign_in(&rig, PEER).await;
    let (other, _) = sign_in(&rig, OTHER_DEVICE).await;

    let locked = send(
        &rig,
        post("/v1/call/lock")
            .header("cookie", &cookie)
            .header("x-karmayoga-protocol", protocol())
            .body(Body::empty())
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(locked.status(), StatusCode::OK);
    assert!(!rig.service.get().unwrap().is_unlocked());

    assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status_with(&rig, &other, OTHER_DEVICE).await, StatusCode::UNAUTHORIZED);
    let picture =
        send(&rig, get(&format!("/m/{media}/v1/blob/x")).body(Body::empty()).unwrap(), PEER)
            .await;
    assert_eq!(picture.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(rig.state.sessions.count(), 0);

    // The password unlocks it again from the browser.
    let (cookie, _) = sign_in(&rig, PEER).await;
    assert!(rig.service.get().unwrap().is_unlocked());
    assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::OK);
}

#[tokio::test]
async fn opening_another_vault_signs_the_browsers_of_the_first_out() {
    let rig = rig(Some(PASSWORD), Config::default());
    let (cookie, media) = sign_in(&rig, PEER).await;
    assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::OK);

    // What the desktop's "open" or "create" does underneath a running
    // server: one vault out, another -- here with no password, so open
    // the moment it is set -- in.
    let other = tempfile::tempdir().unwrap();
    let vault = karmayoga_vault::create(
        other.path(),
        karmayoga_core::VaultConfig {
            name: "Other".into(),
            password: None,
            ..Default::default()
        },
    )
    .unwrap();
    rig.service.set(vault);

    assert_eq!(status_with(&rig, &cookie, PEER).await, StatusCode::UNAUTHORIZED);
    let picture =
        send(&rig, get(&format!("/m/{media}/v1/blob/x")).body(Body::empty()).unwrap(), PEER)
            .await;
    assert_eq!(picture.status(), StatusCode::UNAUTHORIZED);
    let hello = json_of(
        send(
            &rig,
            get("/web/hello").header("cookie", &cookie).body(Body::empty()).unwrap(),
            PEER,
        )
        .await,
    )
    .await;
    assert_eq!(hello["signedIn"], false);
    assert_eq!(hello["name"], "Other", "the name is the vault open now");
}

#[test]
fn each_tab_is_its_own_origin_and_cannot_name_one_with_punctuation() {
    let mut headers = HeaderMap::new();
    assert_eq!(origin_of("web-1", &headers), "web-1");
    headers.insert(TAB_HEADER, HeaderValue::from_static("a1b2"));
    assert_eq!(origin_of("web-1", &headers), "web-1.a1b2");
    headers.insert(TAB_HEADER, HeaderValue::from_static("../x y"));
    assert_eq!(origin_of("web-1", &headers), "web-1.xy");
}

#[tokio::test]
async fn a_locked_vault_stays_locked_when_unlocking_from_a_browser_is_off() {
    let rig = rig(Some(PASSWORD), Config { allow_unlock: false, ..Config::default() });
    rig.service.get().unwrap().lock();
    let response = send(
        &rig,
        post("/web/login")
            .header("content-type", "application/json")
            .header("x-karmayoga-web", "1")
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!rig.service.get().unwrap().is_unlocked());
}

#[tokio::test]
async fn a_vault_with_no_password_cannot_be_opened_from_a_browser() {
    let rig = rig(None, Config::default());
    let response = send(
        &rig,
        post("/web/login")
            .header("content-type", "application/json")
            .header("x-karmayoga-web", "1")
            .body(Body::from(json!({ "password": "" }).to_string()))
            .unwrap(),
        PEER,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(rig.state.sessions.count(), 0);
}

#[tokio::test]
async fn a_write_from_another_origin_is_refused() {
    let rig = rig(Some(PASSWORD), Config::default());
    let login = |origin: &str| {
        post("/web/login")
            .header("content-type", "application/json")
            .header("x-karmayoga-web", "1")
            .header(header::ORIGIN, origin)
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap()
    };
    let evil = send(&rig, login("https://evil.example"), PEER).await;
    assert_eq!(evil.status(), StatusCode::FORBIDDEN);
    let ours = send(&rig, login(ORIGIN), PEER).await;
    assert_eq!(ours.status(), StatusCode::OK);
}

#[tokio::test]
async fn too_many_wrong_passwords_lock_that_device_out() {
    let rig = rig(Some(PASSWORD), Config::default());
    let attempt = |password: &str| {
        post("/web/login")
            .header("content-type", "application/json")
            .header("x-karmayoga-web", "1")
            .body(Body::from(json!({ "password": password }).to_string()))
            .unwrap()
    };
    for _ in 0..5 {
        assert_eq!(send(&rig, attempt("wrong"), PEER).await.status(), StatusCode::UNAUTHORIZED);
    }
    // Even the right one is turned away now -- from this device.
    assert_eq!(
        send(&rig, attempt(PASSWORD), PEER).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(send(&rig, attempt(PASSWORD), OTHER_DEVICE).await.status(), StatusCode::OK);
}
