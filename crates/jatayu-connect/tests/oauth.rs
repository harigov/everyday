//! The MCP authorization flow against servers that answer the way real ones
//! do: path-inserted metadata, OIDC-only metadata, GitHub's form-encoded
//! token answer, Slack's nested one. Every server here is a local axum
//! router on `127.0.0.1`; nothing reaches the network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::Json as JsonBody;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use jatayu_connect::oauth::{
    self, AuthServer, ClientCreds, OAuthError, Pkce, ProtectedResource, TokenAuth, challenge_for,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// A client that ignores proxy settings in the environment, so a test run
/// behind one still reaches 127.0.0.1.
fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// Bind first, so the router can be built knowing its own address, then serve
/// it. Returns the base address, `http://127.0.0.1:PORT`.
async fn serve(build: impl FnOnce(&str) -> Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = build(&base);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    base
}

/// A route that answers 200 with `body`.
fn json_route(body: Value) -> axum::routing::MethodRouter {
    get(move || {
        let body = body.clone();
        async move { axum::Json(body) }
    })
}

/// One request as the fake server saw it: its `Authorization` header and body.
type Request = (Option<String>, String);

/// What a token endpoint was sent: the `Authorization` header (if any) and
/// the body, as written.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<Request>>>);

impl Seen {
    fn only(&self) -> (Option<String>, String) {
        let all = self.0.lock().unwrap();
        assert_eq!(all.len(), 1, "expected one request");
        all[0].clone()
    }

    /// The one request's body as a map of form fields.
    fn form(&self) -> HashMap<String, String> {
        let (_, body) = self.only();
        url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
    }
}

/// A token endpoint that always answers `status` with `body`, recording what
/// it was sent. Returns its address.
async fn token_server(status: u16, content_type: &'static str, body: &str) -> (String, Seen) {
    let seen = Seen::default();
    let reply = body.to_string();
    let recorded = seen.clone();
    let base = serve(move |_| {
        Router::new().route(
            "/token",
            post(move |headers: HeaderMap, sent: String| {
                let recorded = recorded.clone();
                let reply = reply.clone();
                async move {
                    let authorization = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    recorded.0.lock().unwrap().push((authorization, sent));
                    (StatusCode::from_u16(status).unwrap(), [(CONTENT_TYPE, content_type)], reply)
                }
            }),
        )
    })
    .await;
    (format!("{base}/token"), seen)
}

fn provider_code(error: OAuthError) -> String {
    match error {
        OAuthError::Provider { code, .. } => code,
        other => panic!("wrong error: {other:?}"),
    }
}

fn client(auth: TokenAuth, secret: Option<&str>) -> ClientCreds {
    ClientCreds { client_id: "the-client".into(), client_secret: secret.map(str::to_string), auth }
}

fn server_with(methods: &[&str], registration: Option<&str>) -> AuthServer {
    AuthServer {
        issuer: "https://as.test".into(),
        authorization_endpoint: "https://as.test/authorize".into(),
        token_endpoint: "https://as.test/token".into(),
        registration_endpoint: registration.map(str::to_string),
        code_challenge_methods: vec!["S256".into()],
        token_endpoint_auth_methods: methods.iter().map(|m| m.to_string()).collect(),
        scopes_supported: Vec::new(),
    }
}

// --- discovery -------------------------------------------------------------

fn as_metadata(base: &str, issuer: &str) -> Value {
    json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post"],
        "scopes_supported": ["read", "write"],
    })
}

#[tokio::test]
async fn discovery_follows_the_resource_metadata_a_401_named() {
    let base = serve(|base| {
        Router::new()
            .route(
                "/named/metadata",
                json_route(json!({
                    "resource": format!("{base}/mcp"),
                    "authorization_servers": [base],
                    "scopes_supported": ["read"],
                })),
            )
            .route("/.well-known/oauth-authorization-server", json_route(as_metadata(base, base)))
    })
    .await;

    let named = format!("{base}/named/metadata");
    let found = oauth::discover(&http(), &format!("{base}/mcp"), Some(&named)).await.unwrap();
    assert_eq!(found.resource.resource, format!("{base}/mcp"));
    assert_eq!(found.resource.authorization_servers, vec![base.clone()]);
    assert_eq!(found.resource.scopes_supported, vec!["read".to_string()]);
    assert_eq!(found.server.authorization_endpoint, format!("{base}/authorize"));
    assert_eq!(found.server.token_endpoint, format!("{base}/token"));
    assert_eq!(found.server.registration_endpoint, Some(format!("{base}/register")));
    assert_eq!(found.server.token_endpoint_auth_methods, vec!["none", "client_secret_post"]);
    assert_eq!(found.server.code_challenge_methods, vec!["S256"]);
}

#[tokio::test]
async fn a_stale_resource_metadata_address_falls_back_to_the_well_known_places() {
    let base = serve(|base| {
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                json_route(
                    json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]}),
                ),
            )
            .route("/.well-known/oauth-authorization-server", json_route(as_metadata(base, base)))
    })
    .await;
    let gone = format!("{base}/gone");
    let found = oauth::discover(&http(), &format!("{base}/mcp"), Some(&gone)).await.unwrap();
    assert_eq!(found.resource.authorization_servers, vec![base]);
}

#[tokio::test]
async fn discovery_inserts_the_path_into_the_protected_resource_well_known() {
    // Only the path-inserted address answers, as Linear's and Datadog's do;
    // the root one is a 404. The resource is kept as the metadata wrote it,
    // trailing slash and all, as GitHub's has.
    let base = serve(|base| {
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource/v1/mcp",
                json_route(json!({
                    "resource": format!("{base}/v1/mcp/"),
                    "authorization_servers": [base],
                })),
            )
            .route("/.well-known/oauth-authorization-server", json_route(as_metadata(base, base)))
    })
    .await;
    let found = oauth::discover(&http(), &format!("{base}/v1/mcp"), None).await.unwrap();
    assert_eq!(found.resource.resource, format!("{base}/v1/mcp/"));
    assert!(found.resource.scopes_supported.is_empty());
}

#[tokio::test]
async fn discovery_reads_the_root_protected_resource_well_known_last() {
    let base = serve(|base| {
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                json_route(json!({"resource": base, "authorization_servers": [base]})),
            )
            .route("/.well-known/oauth-authorization-server", json_route(as_metadata(base, base)))
    })
    .await;
    let found = oauth::discover(&http(), &format!("{base}/some/path"), None).await.unwrap();
    assert_eq!(found.resource.resource, base);
}

#[tokio::test]
async fn authorization_server_metadata_has_the_issuer_path_inserted() {
    // Datadog's and GitHub's issuers have paths; the metadata lives between
    // the host and the path (RFC 8414 §3.1).
    let base = serve(|base| {
        let issuer = format!("{base}/login/oauth");
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                json_route(
                    json!({"resource": format!("{base}/mcp"), "authorization_servers": [issuer]}),
                ),
            )
            .route(
                "/.well-known/oauth-authorization-server/login/oauth",
                json_route(as_metadata(base, &issuer)),
            )
    })
    .await;
    let found = oauth::discover(&http(), &format!("{base}/mcp"), None).await.unwrap();
    assert_eq!(found.server.issuer, format!("{base}/login/oauth"));
    assert_eq!(found.server.token_endpoint, format!("{base}/token"));
}

#[tokio::test]
async fn authorization_server_metadata_falls_back_to_openid_configuration() {
    // Google's issuer has only OIDC metadata, and no registration.
    let base = serve(|base| {
        let mut oidc = as_metadata(base, base);
        oidc.as_object_mut().unwrap().remove("registration_endpoint");
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                json_route(json!({"resource": format!("{base}/mcp"), "authorization_servers": [format!("{base}/")]})),
            )
            .route("/.well-known/openid-configuration", json_route(oidc))
    })
    .await;
    let found = oauth::discover(&http(), &format!("{base}/mcp"), None).await.unwrap();
    assert_eq!(found.server.registration_endpoint, None);
    assert_eq!(found.server.authorization_endpoint, format!("{base}/authorize"));
}

#[tokio::test]
async fn openid_configuration_is_tried_with_the_issuer_path_too() {
    let base = serve(|base| {
        let issuer = format!("{base}/tenant");
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                json_route(
                    json!({"resource": format!("{base}/mcp"), "authorization_servers": [issuer]}),
                ),
            )
            .route(
                "/tenant/.well-known/openid-configuration",
                json_route(as_metadata(base, &issuer)),
            )
    })
    .await;
    let found = oauth::discover(&http(), &format!("{base}/mcp"), None).await.unwrap();
    assert_eq!(found.server.issuer, format!("{base}/tenant"));
}

#[tokio::test]
async fn a_server_with_no_resource_metadata_is_its_own_authorization_server() {
    let base = serve(|base| {
        Router::new()
            .route("/.well-known/oauth-authorization-server", json_route(as_metadata(base, base)))
    })
    .await;
    // A trailing slash on the address does not end up in the resource.
    let found = oauth::discover(&http(), &format!("{base}/mcp/"), None).await.unwrap();
    assert_eq!(found.resource.resource, format!("{base}/mcp"));
    assert_eq!(found.resource.authorization_servers, vec![base.clone()]);
    assert!(found.resource.scopes_supported.is_empty());
    assert_eq!(found.server.token_endpoint, format!("{base}/token"));
}

#[tokio::test]
async fn discovery_fails_readably_when_nothing_answers() {
    let base = serve(|_| Router::new()).await;
    let error = oauth::discover(&http(), &format!("{base}/mcp"), None).await.unwrap_err();
    let reason = match error {
        OAuthError::Discovery(reason) => reason,
        other => panic!("wrong error: {other:?}"),
    };
    assert!(reason.contains("HTTP 404"), "{reason}");
}

#[tokio::test]
async fn metadata_without_the_endpoints_is_not_accepted() {
    let base = serve(|base| {
        Router::new()
            .route("/.well-known/oauth-authorization-server", json_route(json!({"issuer": base})))
    })
    .await;
    let error = oauth::discover(&http(), &format!("{base}/mcp"), None).await.unwrap_err();
    assert!(matches!(error, OAuthError::Discovery(_)), "{error:?}");
}

#[tokio::test]
async fn an_address_that_is_not_a_web_address_is_refused() {
    let error = oauth::discover(&http(), "not a url", None).await.unwrap_err();
    assert!(matches!(error, OAuthError::Discovery(_)));
    let error = oauth::discover(&http(), "file:///etc/passwd", None).await.unwrap_err();
    assert!(matches!(error, OAuthError::Discovery(_)));
}

// --- registration ----------------------------------------------------------

/// A registration endpoint answering `status` with `body`, remembering the
/// JSON it was sent.
async fn registration_server(status: u16, body: Value) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let recorded = seen.clone();
    let base = serve(move |_| {
        Router::new().route(
            "/register",
            post(move |JsonBody(sent): JsonBody<Value>| {
                let recorded = recorded.clone();
                let body = body.clone();
                async move {
                    recorded.lock().unwrap().push(sent);
                    (StatusCode::from_u16(status).unwrap(), axum::Json(body))
                }
            }),
        )
    })
    .await;
    (format!("{base}/register"), seen)
}

#[tokio::test]
async fn registration_asks_for_a_public_client_and_reads_the_answer() {
    let (endpoint, seen) = registration_server(201, json!({"client_id": "abc123"})).await;
    let server = server_with(&["none", "client_secret_basic"], Some(&endpoint));
    let creds = oauth::register(&http(), &server, "http://127.0.0.1:5555/callback", "Jatayu")
        .await
        .unwrap();
    assert_eq!(creds, client_with("abc123", None, TokenAuth::None));

    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["client_name"], "Jatayu");
    assert_eq!(sent["redirect_uris"], json!(["http://127.0.0.1:5555/callback"]));
    assert_eq!(sent["grant_types"], json!(["authorization_code", "refresh_token"]));
    assert_eq!(sent["response_types"], json!(["code"]));
    assert_eq!(sent["token_endpoint_auth_method"], "none");
}

fn client_with(id: &str, secret: Option<&str>, auth: TokenAuth) -> ClientCreds {
    ClientCreds { client_id: id.into(), client_secret: secret.map(str::to_string), auth }
}

#[tokio::test]
async fn registration_asks_for_none_when_the_server_lists_nothing() {
    let (endpoint, seen) = registration_server(200, json!({"client_id": "abc"})).await;
    let server = server_with(&[], Some(&endpoint));
    oauth::register(&http(), &server, "http://127.0.0.1:1/callback", "x").await.unwrap();
    assert_eq!(seen.lock().unwrap()[0]["token_endpoint_auth_method"], "none");
}

#[tokio::test]
async fn registration_asks_for_the_secret_method_a_server_lists() {
    let (endpoint, seen) = registration_server(
        201,
        json!({"client_id": "abc", "client_secret": "s3cret", "token_endpoint_auth_method": "client_secret_post"}),
    )
    .await;
    let server = server_with(
        &["private_key_jwt", "client_secret_post", "client_secret_basic"],
        Some(&endpoint),
    );
    let creds =
        oauth::register(&http(), &server, "http://127.0.0.1:1/callback", "x").await.unwrap();
    assert_eq!(seen.lock().unwrap()[0]["token_endpoint_auth_method"], "client_secret_post");
    assert_eq!(creds, client_with("abc", Some("s3cret"), TokenAuth::ClientSecretPost));
}

#[tokio::test]
async fn a_secret_with_no_method_means_basic() {
    let (endpoint, _) =
        registration_server(201, json!({"client_id": "abc", "client_secret": "s3cret"})).await;
    let server = server_with(&["client_secret_basic"], Some(&endpoint));
    let creds =
        oauth::register(&http(), &server, "http://127.0.0.1:1/callback", "x").await.unwrap();
    assert_eq!(creds.auth, TokenAuth::ClientSecretBasic);
    assert_eq!(creds.client_secret.as_deref(), Some("s3cret"));
}

#[tokio::test]
async fn registration_without_an_endpoint_needs_a_client_of_your_own() {
    let server = server_with(&["none"], None);
    let error =
        oauth::register(&http(), &server, "http://127.0.0.1:1/callback", "x").await.unwrap_err();
    assert_eq!(error, OAuthError::NoRegistration);
}

#[tokio::test]
async fn registration_errors_are_the_servers_own() {
    let (endpoint, _) = registration_server(
        400,
        json!({"error": "invalid_redirect_uri", "error_description": "loopback not allowed"}),
    )
    .await;
    let server = server_with(&[], Some(&endpoint));
    let error =
        oauth::register(&http(), &server, "http://127.0.0.1:1/callback", "x").await.unwrap_err();
    assert_eq!(
        error,
        OAuthError::Provider {
            code: "invalid_redirect_uri".into(),
            description: "loopback not allowed".into()
        }
    );
}

// --- PKCE, state, the address to open --------------------------------------

#[test]
fn the_challenge_is_the_unpadded_base64url_sha256_of_the_verifier() {
    // Worked out independently: base64url(sha256(verifier)), no padding.
    assert_eq!(
        challenge_for("dBjftJeZ4CVP-mJ92K9XyhqRdBmQBHdEAPH1oEPOJaE"),
        "ECOwuI51niW8u0oCew8yqVk4sYVDPMjDxZDQPHiFhrg"
    );
}

#[test]
fn a_fresh_pair_is_43_url_safe_characters_and_never_repeats() {
    let a = Pkce::new();
    let b = Pkce::new();
    assert_eq!(a.verifier.len(), 43);
    assert!(a.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    assert_eq!(a.challenge, challenge_for(&a.verifier));
    assert_ne!(a.verifier, b.verifier);

    let s = oauth::random_state();
    assert!(s.len() >= 16);
    assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    assert_ne!(s, oauth::random_state());
}

fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

#[test]
fn the_authorize_address_carries_everything_the_provider_needs() {
    let server = server_with(&["none"], None);
    let pkce = Pkce::new();
    let scopes = vec!["read".to_string(), "write".to_string()];
    let url = oauth::authorize_url(
        &server,
        "cid",
        "http://127.0.0.1:5555/callback",
        &scopes,
        "st4te",
        &pkce,
        Some("https://mcp.test/mcp"),
    );
    assert!(url.starts_with("https://as.test/authorize?"), "{url}");
    let q = query_of(&url);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:5555/callback");
    assert_eq!(q["scope"], "read write");
    assert_eq!(q["state"], "st4te");
    assert_eq!(q["code_challenge"], pkce.challenge);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["resource"], "https://mcp.test/mcp");
    assert!(!q.contains_key("access_type"));
    assert!(!q.contains_key("prompt"));
}

#[test]
fn scope_and_resource_are_left_out_when_there_are_none() {
    let q = query_of(&oauth::authorize_url(
        &server_with(&[], None),
        "cid",
        "http://x/cb",
        &[],
        "s",
        &Pkce::new(),
        None,
    ));
    assert!(!q.contains_key("scope"));
    assert!(!q.contains_key("resource"));
}

#[test]
fn google_gets_offline_access_and_the_consent_screen() {
    let mut server = server_with(&[], None);
    server.authorization_endpoint = "https://accounts.google.com/o/oauth2/v2/auth".into();
    let q = query_of(&oauth::authorize_url(
        &server,
        "cid",
        "http://x/cb",
        &[],
        "s",
        &Pkce::new(),
        None,
    ));
    assert_eq!(q["access_type"], "offline");
    assert_eq!(q["prompt"], "consent");
}

#[test]
fn an_existing_query_on_the_endpoint_survives() {
    let mut server = server_with(&[], None);
    server.authorization_endpoint = "https://as.test/authorize?tenant=acme".into();
    let q = query_of(&oauth::authorize_url(
        &server,
        "cid",
        "http://x/cb",
        &[],
        "s",
        &Pkce::new(),
        None,
    ));
    assert_eq!(q["tenant"], "acme");
    assert_eq!(q["response_type"], "code");
}

// --- the token endpoint ----------------------------------------------------

async fn exchange_with(
    endpoint: &str,
    creds: &ClientCreds,
    resource: Option<&str>,
) -> Result<oauth::Grant, OAuthError> {
    oauth::exchange(
        &http(),
        endpoint,
        creds,
        "the-code",
        "the-verifier",
        "http://127.0.0.1:5555/callback",
        resource,
    )
    .await
}

#[tokio::test]
async fn a_json_answer_becomes_a_grant() {
    let (endpoint, seen) = token_server(
        200,
        "application/json",
        r#"{"access_token":"at","refresh_token":"rt","expires_in":3600,"scope":"read write","token_type":"Bearer"}"#,
    )
    .await;
    let before = jiff::Timestamp::now();
    let grant =
        exchange_with(&endpoint, &client(TokenAuth::None, None), Some("https://mcp.test/mcp"))
            .await
            .unwrap();
    assert_eq!(grant.access_token, "at");
    assert_eq!(grant.refresh_token.as_deref(), Some("rt"));
    assert_eq!(grant.scope.as_deref(), Some("read write"));
    let expires = grant.expires_at.unwrap();
    let in_seconds = expires.as_second() - before.as_second();
    assert!((3590..=3610).contains(&in_seconds), "{in_seconds}");

    let form = seen.form();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "the-code");
    assert_eq!(form["code_verifier"], "the-verifier");
    assert_eq!(form["redirect_uri"], "http://127.0.0.1:5555/callback");
    assert_eq!(form["resource"], "https://mcp.test/mcp");
    assert_eq!(form["client_id"], "the-client");
    assert!(!form.contains_key("client_secret"));
    assert_eq!(seen.only().0, None);
}

#[tokio::test]
async fn no_resource_is_sent_when_none_is_given() {
    let (endpoint, seen) = token_server(200, "application/json", r#"{"access_token":"at"}"#).await;
    let grant = exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap();
    assert_eq!(grant.refresh_token, None);
    assert_eq!(grant.expires_at, None);
    assert!(!seen.form().contains_key("resource"));
}

#[tokio::test]
async fn expires_in_may_be_a_numeric_string() {
    let (endpoint, _) =
        token_server(200, "application/json", r#"{"access_token":"at","expires_in":"7200"}"#).await;
    let before = jiff::Timestamp::now().as_second();
    let grant = exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap();
    let in_seconds = grant.expires_at.unwrap().as_second() - before;
    assert!((7190..=7210).contains(&in_seconds), "{in_seconds}");
}

#[tokio::test]
async fn a_form_encoded_answer_becomes_a_grant() {
    // GitHub, when it does not honour `Accept: application/json`.
    let (endpoint, _) = token_server(
        200,
        "application/x-www-form-urlencoded; charset=utf-8",
        "access_token=gho_abc%2B1&scope=repo%2Cuser&token_type=bearer",
    )
    .await;
    let grant = exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap();
    assert_eq!(grant.access_token, "gho_abc+1");
    assert_eq!(grant.scope.as_deref(), Some("repo,user"));
    assert_eq!(grant.refresh_token, None);
}

#[tokio::test]
async fn a_form_encoded_error_is_an_error() {
    let (endpoint, _) = token_server(
        200,
        "application/x-www-form-urlencoded",
        "error=bad_verification_code&error_description=The+code+passed+is+incorrect",
    )
    .await;
    let error = exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err();
    assert_eq!(error, OAuthError::InvalidGrant);
}

#[tokio::test]
async fn slack_nests_the_user_token() {
    let (endpoint, _) = token_server(
        200,
        "application/json",
        r#"{"ok":true,"access_token":"xoxb-bot","authed_user":{"id":"U1","access_token":"xoxp-user","refresh_token":"xoxe-1","expires_in":43200,"scope":"search:read.public"}}"#,
    )
    .await;
    let grant = exchange_with(&endpoint, &client(TokenAuth::ClientSecretPost, Some("s")), None)
        .await
        .unwrap();
    assert_eq!(grant.access_token, "xoxp-user");
    assert_eq!(grant.refresh_token.as_deref(), Some("xoxe-1"));
    assert_eq!(grant.scope.as_deref(), Some("search:read.public"));
    assert!(grant.expires_at.is_some());
}

#[tokio::test]
async fn slack_may_answer_with_a_top_level_token() {
    let (endpoint, _) = token_server(
        200,
        "application/json",
        r#"{"ok":true,"access_token":"xoxp-user","expires_in":3600}"#,
    )
    .await;
    let grant = exchange_with(&endpoint, &client(TokenAuth::ClientSecretPost, Some("s")), None)
        .await
        .unwrap();
    assert_eq!(grant.access_token, "xoxp-user");
}

#[tokio::test]
async fn slack_ok_false_maps_its_error_codes() {
    for (code, expected) in [
        ("invalid_code", OAuthError::InvalidGrant),
        ("invalid_refresh_token", OAuthError::InvalidGrant),
        ("token_revoked", OAuthError::InvalidGrant),
        ("token_expired", OAuthError::InvalidGrant),
        ("invalid_client_id", OAuthError::InvalidClient("invalid_client_id".into())),
        ("bad_client_secret", OAuthError::InvalidClient("bad_client_secret".into())),
        (
            "ratelimited",
            OAuthError::Provider {
                code: "ratelimited".into(),
                description: "no reason given".into(),
            },
        ),
    ] {
        let (endpoint, _) =
            token_server(200, "application/json", &json!({"ok": false, "error": code}).to_string())
                .await;
        let error =
            exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err();
        assert_eq!(error, expected, "{code}");
    }
}

#[tokio::test]
async fn standard_oauth_errors_are_sorted_by_what_the_caller_does() {
    let (endpoint, _) = token_server(
        400,
        "application/json",
        r#"{"error":"invalid_grant","error_description":"code was already used"}"#,
    )
    .await;
    assert_eq!(
        exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err(),
        OAuthError::InvalidGrant
    );

    let (endpoint, _) = token_server(
        401,
        "application/json",
        r#"{"error":"invalid_client","error_description":"unknown client"}"#,
    )
    .await;
    assert_eq!(
        exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err(),
        OAuthError::InvalidClient("unknown client".into())
    );

    let (endpoint, _) = token_server(
        400,
        "application/json",
        r#"{"error":"unauthorized_client","error_description":"not allowed to use this grant"}"#,
    )
    .await;
    assert_eq!(
        exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err(),
        OAuthError::InvalidClient("not allowed to use this grant".into())
    );

    let (endpoint, _) = token_server(
        400,
        "application/json",
        r#"{"error":"invalid_scope","error_description":"no such scope"}"#,
    )
    .await;
    assert_eq!(
        exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err(),
        OAuthError::Provider { code: "invalid_scope".into(), description: "no such scope".into() }
    );
}

#[tokio::test]
async fn an_answer_with_no_token_is_an_error_not_an_empty_grant() {
    let (endpoint, _) = token_server(200, "application/json", r#"{"token_type":"Bearer"}"#).await;
    let error = exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err();
    let code = provider_code(error);
    assert_eq!(code, "no_token");

    let (endpoint, _) = token_server(502, "text/html", "<html>Bad gateway</html>").await;
    let error = exchange_with(&endpoint, &client(TokenAuth::None, None), None).await.unwrap_err();
    let code = provider_code(error);
    assert_eq!(code, "http_502");
}

#[tokio::test]
async fn a_server_that_is_not_there_is_an_http_error() {
    // Bind, learn the port, and let go of it again.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let error = exchange_with(
        &format!("http://127.0.0.1:{port}/token"),
        &client(TokenAuth::None, None),
        None,
    )
    .await
    .unwrap_err();
    let reason = match error {
        OAuthError::Http(reason) => reason,
        other => panic!("wrong error: {other:?}"),
    };
    assert!(
        !reason.contains(&port.to_string()),
        "the address should not be in the message: {reason}"
    );
}

#[tokio::test]
async fn basic_auth_sends_the_secret_in_the_header_and_not_the_body() {
    let (endpoint, seen) = token_server(200, "application/json", r#"{"access_token":"at"}"#).await;
    // Both are form-encoded before they are joined (RFC 6749 §2.3.1).
    let creds = ClientCreds {
        client_id: "id with space".into(),
        client_secret: Some("se:cret+/".into()),
        auth: TokenAuth::ClientSecretBasic,
    };
    exchange_with(&endpoint, &creds, None).await.unwrap();
    let (authorization, body) = seen.only();
    use base64::Engine as _;
    let expected =
        base64::engine::general_purpose::STANDARD.encode("id+with+space:se%3Acret%2B%2F");
    assert_eq!(authorization, Some(format!("Basic {expected}")));
    assert!(!body.contains("se%3Acret") && !body.contains("secret"), "{body}");
    assert!(!seen.form().contains_key("client_id"));
    assert!(!seen.form().contains_key("client_secret"));
}

#[tokio::test]
async fn post_auth_sends_the_secret_in_the_body_and_no_header() {
    let (endpoint, seen) = token_server(200, "application/json", r#"{"access_token":"at"}"#).await;
    exchange_with(&endpoint, &client(TokenAuth::ClientSecretPost, Some("s3cret")), None)
        .await
        .unwrap();
    assert_eq!(seen.only().0, None);
    let form = seen.form();
    assert_eq!(form["client_id"], "the-client");
    assert_eq!(form["client_secret"], "s3cret");
}

#[tokio::test]
async fn a_secret_method_with_no_secret_is_sent_as_a_public_client() {
    let (endpoint, seen) = token_server(200, "application/json", r#"{"access_token":"at"}"#).await;
    exchange_with(&endpoint, &client(TokenAuth::ClientSecretBasic, None), None).await.unwrap();
    assert_eq!(seen.only().0, None);
    assert_eq!(seen.form()["client_id"], "the-client");
}

#[tokio::test]
async fn refresh_sends_the_resource_and_returns_a_rotated_token() {
    let (endpoint, seen) = token_server(
        200,
        "application/json",
        r#"{"access_token":"new-at","refresh_token":"new-rt","expires_in":1800}"#,
    )
    .await;
    let grant = oauth::refresh(
        &http(),
        &endpoint,
        &client(TokenAuth::ClientSecretPost, Some("s3cret")),
        "old-rt",
        Some("https://mcp.test/mcp"),
    )
    .await
    .unwrap();
    assert_eq!(grant.access_token, "new-at");
    assert_eq!(grant.refresh_token.as_deref(), Some("new-rt"));
    let form = seen.form();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "old-rt");
    assert_eq!(form["resource"], "https://mcp.test/mcp");
    assert_eq!(form["client_secret"], "s3cret");
    assert!(!form.contains_key("code"));
}

#[tokio::test]
async fn a_refused_refresh_is_invalid_grant() {
    let (endpoint, _) = token_server(400, "application/json", r#"{"error":"invalid_grant"}"#).await;
    let error = oauth::refresh(&http(), &endpoint, &client(TokenAuth::None, None), "old-rt", None)
        .await
        .unwrap_err();
    assert_eq!(error, OAuthError::InvalidGrant);
}

// --- the 401's header, choosing scopes, secrets in Debug ------------------

#[test]
fn linears_challenge_names_its_metadata_and_scope() {
    let (rm, scope) = oauth::parse_www_authenticate(
        r#"Bearer realm="OAuth", resource_metadata="https://mcp.linear.app/.well-known/oauth-protected-resource/mcp", scope="read write""#,
    );
    assert_eq!(
        rm.as_deref(),
        Some("https://mcp.linear.app/.well-known/oauth-protected-resource/mcp")
    );
    assert_eq!(scope.as_deref(), Some("read write"));
}

#[test]
fn githubs_challenge_names_its_metadata_and_no_scope() {
    let (rm, scope) = oauth::parse_www_authenticate(
        r#"Bearer error="invalid_request", error_description="No access token was provided in this request", resource_metadata="https://api.githubcopilot.com/.well-known/oauth-protected-resource/mcp/""#,
    );
    assert_eq!(
        rm.as_deref(),
        Some("https://api.githubcopilot.com/.well-known/oauth-protected-resource/mcp/")
    );
    assert_eq!(scope, None);
}

#[test]
fn the_challenge_is_read_without_regard_to_case_or_quoting() {
    let (rm, scope) =
        oauth::parse_www_authenticate("BEARER Resource_Metadata=https://x.test/m,SCOPE=\"a b\"");
    assert_eq!(rm.as_deref(), Some("https://x.test/m"));
    assert_eq!(scope.as_deref(), Some("a b"));
    assert_eq!(oauth::parse_www_authenticate("Bearer"), (None, None));
}

#[test]
fn scopes_come_from_the_connection_then_the_challenge_then_the_resource() {
    let resource = ProtectedResource {
        resource: "https://mcp.test".into(),
        authorization_servers: vec!["https://as.test".into()],
        scopes_supported: vec!["a".into(), "b".into()],
    };
    let own = vec!["mine".to_string()];
    assert_eq!(oauth::choose_scopes(&own, Some("x y"), &resource), own);
    assert_eq!(oauth::choose_scopes(&[], Some("x  y"), &resource), vec!["x", "y"]);
    assert_eq!(oauth::choose_scopes(&[], Some("  "), &resource), vec!["a", "b"]);
    assert_eq!(oauth::choose_scopes(&[], None, &resource), vec!["a", "b"]);
}

#[test]
fn debug_never_shows_a_secret() {
    let creds = ClientCreds {
        client_id: "visible-id".into(),
        client_secret: Some("hunter2-client-secret".into()),
        auth: TokenAuth::ClientSecretPost,
    };
    let shown = format!("{creds:?}");
    assert!(shown.contains("visible-id"));
    assert!(!shown.contains("hunter2"), "{shown}");

    let grant = oauth::Grant {
        access_token: "access-token-value".into(),
        refresh_token: Some("refresh-token-value".into()),
        expires_at: None,
        scope: Some("read".into()),
    };
    let shown = format!("{grant:?}");
    assert!(
        !shown.contains("access-token-value") && !shown.contains("refresh-token-value"),
        "{shown}"
    );

    let pkce = Pkce::new();
    let shown = format!("{pkce:?}");
    assert!(!shown.contains(&pkce.verifier), "{shown}");
}
