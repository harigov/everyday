//! Authorization code with PKCE: the URL a browser is sent to, and the two
//! token requests that follow it. See the module doc for why this crate and
//! why `oauth2-reqwest`.

use std::sync::OnceLock;
use std::time::Duration;

use oauth2::basic::{BasicClient, BasicErrorResponseType, BasicTokenResponse};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken as OAuth2RefreshToken,
    RequestTokenError, Scope as OAuthScope, StandardErrorResponse, TokenResponse, TokenUrl,
};
use oauth2_reqwest::ReqwestClient;

/// A client fully configured with both endpoints: the only shape this module
/// ever builds, since [`OAuthClient::configured`] always sets both before
/// handing one back. Naming it once is what keeps [`OAuthClient::begin`],
/// [`OAuthClient::exchange`] and [`OAuthClient::refresh`] from repeating
/// `oauth2`'s five-parameter typestate.
type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// How long a single token request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A provider's OAuth endpoints, a client identity, and the scopes a sign-in
/// asks for. Everything a caller needs to hand over -- no `Account`, no
/// vault, no notion of which provider this is beyond what its own URLs say.
///
/// # Why `redirect` lives here and not on a call
///
/// The token endpoint is required by
/// [RFC 6749 §4.1.3](https://www.rfc-editor.org/rfc/rfc6749#section-4.1.3)
/// to reject a code exchange whose `redirect_uri` does not match, byte for
/// byte, the one the authorization request used. A `redirect_uri` argument
/// repeated on [`OAuthClient::begin`] and [`OAuthClient::exchange`] would be
/// two places that have to agree, called minutes apart by different code --
/// exactly the shape of thing that drifts. Setting it once, when the
/// loopback's port is already known, means there is nothing left to
/// disagree with itself.
#[derive(Clone)]
pub struct OAuthClient {
    pub client_id: String,
    /// Absent for a provider that issues public clients no secret at all.
    /// `oauth2` already does the right thing either way: HTTP Basic auth
    /// when a secret is set, the request body's `client_id` alone when it
    /// is not (see `AuthType`'s default in the `oauth2` crate).
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    /// The loopback's own `http://127.0.0.1:{port}/callback`, or a fixed
    /// URI for a provider that is not driven through [`crate::oauth::Loopback`].
    pub redirect: String,
}

impl std::fmt::Debug for OAuthClient {
    /// Hand-written, not derived, so that `client_secret` cannot start
    /// printing again because a field was added below it and nobody
    /// remembered this type exists. See the module doc.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "[redacted]"))
            .field("auth_url", &self.auth_url)
            .field("token_url", &self.token_url)
            .field("scopes", &self.scopes)
            .field("redirect", &self.redirect)
            .finish()
    }
}

/// What [`OAuthClient::begin`] hands back: the URL to open, the CSRF state
/// [`crate::oauth::Loopback::wait`] checks the redirect against, and the PKCE
/// verifier [`OAuthClient::exchange`] needs a minute or two later.
///
/// Not [`Serialize`](serde::Serialize): every field but `state` has to
/// survive only as long as this process does, in memory, for exactly one
/// pending sign-in -- the moment it can cross a wire is the moment it can be
/// written to a log or a disk this crate does not control.
#[derive(Clone)]
pub struct Authorization {
    pub url: String,
    /// The CSRF token, reflected back by the provider on the redirect.
    /// Unlike the verifier below, its value is meant to be visible -- it
    /// travels in a URL bar and a redirect query string regardless -- so it
    /// is not redacted from `Debug`. Its secrecy was never the point; being
    /// unpredictable and checked is.
    pub state: String,
    pub pkce_verifier: String,
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorization")
            .field("url", &self.url)
            .field("state", &self.state)
            .field("pkce_verifier", &"[redacted]")
            .finish()
    }
}

/// What a token endpoint gave back, read out of `oauth2`'s typed response
/// into the plain shape the rest of this tree passes around.
///
/// Not [`Serialize`](serde::Serialize) either, for the same reason
/// [`Authorization`] is not: this is the value `everyday-service` holds in
/// memory under a `sign_in_id` until `save_account` claims it into the
/// vault's `SecretStore`, and it must not become the sort of value that can
/// be accidentally returned from a command.
#[derive(Clone)]
pub struct Tokens {
    pub access_token: String,
    pub expires_at: jiff::Timestamp,
    /// `Some` only when the provider actually sent one on this response.
    /// Google, and most providers, omit it on an ordinary refresh and send
    /// one only the first time or when they choose to rotate it -- silently
    /// treating an absent field as "the old one is still good" is what lets
    /// rotation (see [`OAuthClient::refresh`]'s doc) work at all.
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "[redacted]"))
            .field("scope", &self.scope)
            .finish()
    }
}

/// Every way a token request can fail, sorted by what a caller must do
/// about it.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    /// The authorization code, or the refresh token, is no longer good.
    /// There is nothing to retry: the account has to sign in again. Its
    /// `description` is the provider's own words, verbatim, for a support
    /// page or a log -- never a token, so it is safe to keep in the clear.
    #[error("this account needs to sign in again: {description}")]
    InvalidGrant { description: String },
    /// The client id or secret is wrong. Shown to the user plainly, because
    /// the fix is theirs: re-paste the client id, or the secret, from the
    /// provider's console.
    #[error("the client id or client secret is wrong: {description}")]
    InvalidClient { description: String },
    /// The request never reached the provider, or the provider's answer
    /// could not be read as HTTP at all -- a DNS failure, a dropped
    /// connection, a timeout. Worth retrying with backoff.
    #[error("could not reach the provider: {message}")]
    Transient { message: String },
    /// Every other error shape: a provider-defined error code this enum has
    /// no variant for, or a response that parsed as something other than a
    /// token or a standard error. `description` is the provider's prose
    /// when it sent any; this crate never invents detail past what the
    /// provider said.
    #[error("{code}: {description}")]
    Provider { code: String, description: String },
}

impl OAuthError {
    /// Turn `oauth2`'s generic request failure into the four-way split
    /// above.
    ///
    /// `RequestTokenError::Parse` carries the *raw response bytes* alongside
    /// the parse failure, in `oauth2`, because a caller debugging a
    /// non-standard provider often wants to see exactly what came back.
    /// This crate throws that half away on purpose: a response that failed
    /// to parse as the token shape we expected can still be a *successful*
    /// grant with an access token sitting in cleartext inside those bytes --
    /// a provider that added an undocumented field would otherwise turn a
    /// parse error into a token leaking into whichever log prints this
    /// error's `Display`.
    fn from_request<RE>(
        err: RequestTokenError<RE, StandardErrorResponse<BasicErrorResponseType>>,
    ) -> Self
    where
        RE: std::error::Error + 'static,
    {
        match err {
            RequestTokenError::ServerResponse(resp) => {
                let description = resp
                    .error_description()
                    .cloned()
                    .unwrap_or_else(|| "the provider did not say why".to_string());
                match resp.error() {
                    BasicErrorResponseType::InvalidGrant => {
                        OAuthError::InvalidGrant { description }
                    }
                    BasicErrorResponseType::InvalidClient => {
                        OAuthError::InvalidClient { description }
                    }
                    other => OAuthError::Provider { code: other.as_ref().to_string(), description },
                }
            }
            RequestTokenError::Request(re) => OAuthError::Transient { message: re.to_string() },
            RequestTokenError::Parse(e, _raw_body_deliberately_discarded) => {
                OAuthError::Provider { code: "parse_error".to_string(), description: e.to_string() }
            }
            RequestTokenError::Other(message) => {
                OAuthError::Provider { code: "other".to_string(), description: message }
            }
        }
    }
}

impl OAuthClient {
    /// Build the fully-configured `oauth2` client this module's three verbs
    /// share. Cheap -- it is field assignment over already-owned `String`s,
    /// no I/O -- so it is rebuilt on every call rather than cached.
    fn configured(&self) -> Result<ConfiguredClient, OAuthError> {
        let auth_url = AuthUrl::new(self.auth_url.clone()).map_err(|e| OAuthError::Provider {
            code: "invalid_auth_url".to_string(),
            description: e.to_string(),
        })?;
        let token_url =
            TokenUrl::new(self.token_url.clone()).map_err(|e| OAuthError::Provider {
                code: "invalid_token_url".to_string(),
                description: e.to_string(),
            })?;
        let redirect_url =
            RedirectUrl::new(self.redirect.clone()).map_err(|e| OAuthError::Provider {
                code: "invalid_redirect_url".to_string(),
                description: e.to_string(),
            })?;
        let mut client = BasicClient::new(ClientId::new(self.client_id.clone()))
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);
        if let Some(secret) = &self.client_secret {
            client = client.set_client_secret(ClientSecret::new(secret.clone()));
        }
        Ok(client)
    }

    /// Google's authorization endpoint, detected from the URL rather than
    /// from a `Provider` enum this crate does not have: `access_type=offline`
    /// is what makes Google send a refresh token on a *first* consent at
    /// all, and `prompt=consent` is what makes it send one again for an
    /// account that already granted this app access once before -- without
    /// it, a re-added account silently gets an access token and no way to
    /// keep it alive. Neither parameter means anything to a provider that
    /// does not define it, so this would be harmless to send everywhere;
    /// it is scoped to Google anyway because a query parameter a provider
    /// has never heard of is still one more thing for somebody auditing a
    /// request to wonder about.
    fn is_google(&self) -> bool {
        url::Url::parse(&self.auth_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case("accounts.google.com")))
            .unwrap_or(false)
    }

    /// Start a sign-in: an S256 PKCE challenge, a fresh CSRF state, and the
    /// URL to open in the system browser.
    ///
    /// `login_hint` pre-fills the account chooser when the caller already
    /// knows which address this sign-in is for -- adding an account a
    /// second time after `invalid_grant`, say -- and is otherwise omitted
    /// rather than sent empty, since an empty `login_hint` is its own small
    /// tell to whoever is watching the request.
    pub fn begin(&self, login_hint: Option<&str>) -> Result<Authorization, OAuthError> {
        let client = self.configured()?;
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request =
            client.authorize_url(CsrfToken::new_random).set_pkce_challenge(pkce_challenge);
        for scope in &self.scopes {
            request = request.add_scope(OAuthScope::new(scope.clone()));
        }
        if self.is_google() {
            request = request
                .add_extra_param("access_type", "offline")
                .add_extra_param("prompt", "consent");
        }
        if let Some(hint) = login_hint
            && !hint.is_empty()
        {
            request = request.add_extra_param("login_hint", hint.to_string());
        }
        let (url, state) = request.url();
        Ok(Authorization {
            url: url.to_string(),
            state: state.secret().clone(),
            pkce_verifier: pkce_verifier.secret().clone(),
        })
    }

    /// Exchange an authorization code -- and the verifier [`Self::begin`]
    /// minted alongside its challenge -- for tokens.
    pub async fn exchange(&self, code: &str, pkce_verifier: &str) -> Result<Tokens, OAuthError> {
        let client = self.configured()?;
        let http = http_client()?;
        let response = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier.to_string()))
            .request_async(http)
            .await
            .map_err(OAuthError::from_request)?;
        Ok(tokens_from(response))
    }

    /// Trade a refresh token for a new access token, and -- when the
    /// provider chose to rotate it -- a new refresh token.
    ///
    /// # Rotation
    ///
    /// [`Tokens::refresh_token`] is `Some` only when *this* response carried
    /// one. The caller is what decides rotation happened: if it is `Some`,
    /// persist it in place of the old one; if it is `None`, the old refresh
    /// token is still the current one and must be kept, not discarded.
    /// `everyday-service`'s `TokenCache` is what makes that swap atomic
    /// under one account's lock -- see `crates/everyday-service/src/token_cache.rs`.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, OAuthError> {
        let client = self.configured()?;
        let http = http_client()?;
        let token = OAuth2RefreshToken::new(refresh_token.to_string());
        let response = client
            .exchange_refresh_token(&token)
            .request_async(http)
            .await
            .map_err(OAuthError::from_request)?;
        Ok(tokens_from(response))
    }

    /// [`Self::refresh`], but for a *different* resource than the one this
    /// client was built to sign in for.
    ///
    /// Microsoft's `common`-tenant refresh token is not scoped to one
    /// resource the way its access tokens are: the same refresh token that
    /// minted an `outlook.office.com` access token for IMAP can be redeemed
    /// again, with a different `scope`, for a `graph.microsoft.com` one --
    /// provided the original consent covered it, which is why the calendar
    /// scope is asked for up front alongside the mail one (see
    /// `Provider::Microsoft`'s preset) rather than negotiated here. Plain
    /// [`Self::refresh`] omits `scope` entirely, which every provider reads
    /// as "the same resource as before"; this is the one call in this crate
    /// that asks for something else with the credential already in hand,
    /// which is what `everyday-service::accountcal::graph` needs and no
    /// other caller does.
    pub async fn refresh_for_scopes(
        &self,
        refresh_token: &str,
        scopes: &[String],
    ) -> Result<Tokens, OAuthError> {
        let client = self.configured()?;
        let http = http_client()?;
        let token = OAuth2RefreshToken::new(refresh_token.to_string());
        let mut request = client.exchange_refresh_token(&token);
        for scope in scopes {
            request = request.add_scope(OAuthScope::new(scope.clone()));
        }
        let response = request.request_async(http).await.map_err(OAuthError::from_request)?;
        Ok(tokens_from(response))
    }
}

fn tokens_from(response: BasicTokenResponse) -> Tokens {
    // A provider that omits `expires_in` (permitted by RFC 6749, and rare in
    // practice) gets treated as already close to expiry rather than as
    // good forever, so `TokenCache` refreshes it again soon instead of
    // trusting a token whose lifetime nobody stated.
    let ttl = response.expires_in().unwrap_or(Duration::from_secs(60));
    let expires_at = jiff::Timestamp::now()
        .checked_add(jiff::SignedDuration::from_secs(ttl.as_secs().min(i64::MAX as u64) as i64))
        .unwrap_or_else(|_| jiff::Timestamp::now());
    Tokens {
        access_token: response.access_token().secret().clone(),
        expires_at,
        refresh_token: response.refresh_token().map(|t| t.secret().clone()),
        scope: response
            .scopes()
            .filter(|s| !s.is_empty())
            .map(|scopes| scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" ")),
    }
}

/// The `reqwest` client every token request shares, built once. Following no
/// redirects is not a style choice: a token endpoint that redirected would
/// be a server this application never agreed to send a client secret or an
/// authorization code to. See `oauth2-reqwest`'s own module doc, which makes
/// the same point about the same policy.
fn http_client() -> Result<&'static ReqwestClient, OAuthError> {
    static CLIENT: OnceLock<Result<ReqwestClient, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("EveryDay/", env!("CARGO_PKG_VERSION")))
                .build()
                .map(ReqwestClient::from)
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| OAuthError::Transient { message: format!("could not start the fetcher: {e}") })
}

/// The initial response for `AUTHENTICATE XOAUTH2`
/// ([Google's xoauth2-protocol](https://developers.google.com/gmail/imap/xoauth2-protocol)),
/// for IMAP and SMTP alike.
///
/// Deliberately **not** base64-encoded. Both consumers in this tree do that
/// themselves: `async-imap`'s [`Authenticator`](https://docs.rs/async-imap/latest/async_imap/trait.Authenticator.html)
/// trait says so explicitly -- "the returned byte-string is base64-encoded
/// and then sent back to the server" -- and `lettre`'s
/// `Mechanism::Xoauth2::response` builds this exact string internally from a
/// plain `Credentials`, encoding it only once it reaches the wire. A helper
/// that encoded here as well would not fail loudly; it would produce a
/// string that is valid base64 of base64, which a server rejects as a
/// malformed credential in a way that looks nothing like the encoding bug
/// that caused it. Returning the raw SASL string is what keeps that
/// encoding a single, findable step.
pub fn xoauth2_sasl(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::extract::Form;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use base64::Engine;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A token endpoint that plays back one canned answer per grant type,
    /// and remembers every request it was sent -- so a test can both drive
    /// a scenario ("the second refresh returns `invalid_grant`") and assert
    /// on exactly what this module sent it (the PKCE verifier, the rotated
    /// refresh token, the client secret's presence).
    #[derive(Default)]
    struct MockProvider {
        /// Queued answers, taken in order; the last one repeats once the
        /// queue is empty, so a test that only cares about one exchange
        /// need not queue more than that.
        answers: Mutex<Vec<Answer>>,
        requests: Mutex<Vec<HashMap<String, String>>>,
    }

    enum Answer {
        Token(Value),
        Error { status: u16, body: Value },
    }

    async fn start(provider: Arc<MockProvider>) -> (String, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new()
            .route(
                "/token",
                post(move |Form(form): Form<HashMap<String, String>>| {
                    let provider = provider.clone();
                    async move {
                        provider.requests.lock().unwrap().push(form);
                        let mut answers = provider.answers.lock().unwrap();
                        let answer = if answers.len() > 1 {
                            answers.remove(0)
                        } else {
                            match answers.first() {
                                Some(Answer::Token(v)) => Answer::Token(v.clone()),
                                Some(Answer::Error { status, body }) => {
                                    Answer::Error { status: *status, body: body.clone() }
                                }
                                None => Answer::Error {
                                    status: 500,
                                    body: json!({"error": "server_error"}),
                                },
                            }
                        };
                        match answer {
                            Answer::Token(body) => (StatusCode::OK, Json(body)).into_response(),
                            Answer::Error { status, body } => {
                                (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
                            }
                        }
                    }
                }),
            )
            .with_state(());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://127.0.0.1:{port}/token"), handle)
    }

    fn client(token_url: String, secret: Option<&str>) -> OAuthClient {
        OAuthClient {
            client_id: "test-client".to_string(),
            client_secret: secret.map(str::to_string),
            auth_url: "https://example.test/auth".to_string(),
            token_url,
            scopes: vec!["mail.read".to_string()],
            redirect: "http://127.0.0.1:9999/callback".to_string(),
        }
    }

    fn token_body(access_token: &str, refresh_token: Option<&str>) -> Value {
        let mut body = json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": 3600,
        });
        if let Some(rt) = refresh_token {
            body["refresh_token"] = json!(rt);
        }
        body
    }

    #[tokio::test]
    async fn exchange_reads_the_token_and_its_expiry() {
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Token(token_body("access-1", Some("refresh-1")))]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider.clone()).await;
        let c = client(token_url, Some("shh"));

        let before = jiff::Timestamp::now();
        let tokens = c.exchange("a-code", "a-verifier").await.unwrap();
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-1"));
        assert!(tokens.expires_at > before);

        let sent = provider.requests.lock().unwrap().clone();
        assert_eq!(sent[0].get("grant_type").map(String::as_str), Some("authorization_code"));
        assert_eq!(sent[0].get("code").map(String::as_str), Some("a-code"));
        assert_eq!(sent[0].get("code_verifier").map(String::as_str), Some("a-verifier"));
        handle.abort();
    }

    #[tokio::test]
    async fn refresh_surfaces_a_rotated_refresh_token() {
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Token(token_body("access-2", Some("refresh-2")))]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider.clone()).await;
        let c = client(token_url, Some("shh"));

        let tokens = c.refresh("refresh-1").await.unwrap();
        assert_eq!(tokens.access_token, "access-2");
        // Rotation: the provider handed back a *different* refresh token,
        // and it is this one -- not the one the caller sent in -- that
        // `Tokens` carries onward.
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-2"));

        let sent = provider.requests.lock().unwrap().clone();
        assert_eq!(sent[0].get("grant_type").map(String::as_str), Some("refresh_token"));
        assert_eq!(sent[0].get("refresh_token").map(String::as_str), Some("refresh-1"));
        handle.abort();
    }

    #[tokio::test]
    async fn refresh_for_scopes_asks_for_the_given_scopes_not_the_clients_own() {
        // `accountcal::graph`'s whole reason for existing: the same refresh
        // token minting a token for a resource other than the one the
        // client was built to sign in for, by naming a different scope on
        // this one call. See this method's own doc.
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Token(token_body("access-graph", None))]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider.clone()).await;
        let c = client(token_url, Some("shh"));

        let tokens = c
            .refresh_for_scopes(
                "refresh-1",
                &["https://graph.microsoft.com/.default".to_string(), "offline_access".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "access-graph");

        let sent = provider.requests.lock().unwrap().clone();
        assert_eq!(sent[0].get("grant_type").map(String::as_str), Some("refresh_token"));
        let scope = sent[0].get("scope").cloned().unwrap_or_default();
        assert!(scope.contains("https://graph.microsoft.com/.default"));
        assert!(scope.contains("offline_access"));
        handle.abort();
    }

    #[tokio::test]
    async fn refresh_without_rotation_carries_no_refresh_token() {
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Token(token_body("access-3", None))]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider.clone()).await;
        let c = client(token_url, Some("shh"));

        let tokens = c.refresh("refresh-1").await.unwrap();
        // Absent, not empty and not the old value: the caller is the one
        // that has to know "nothing changed, keep what you had".
        assert!(tokens.refresh_token.is_none());
        handle.abort();
    }

    #[tokio::test]
    async fn invalid_grant_asks_for_sign_in_again() {
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Error {
                status: 400,
                body: json!({
                    "error": "invalid_grant",
                    "error_description": "Token has been expired or revoked.",
                }),
            }]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider).await;
        let c = client(token_url, Some("shh"));

        let err = c.refresh("stale").await.unwrap_err();
        match err {
            OAuthError::InvalidGrant { description } => {
                assert_eq!(description, "Token has been expired or revoked.");
            }
            other => panic!("expected InvalidGrant, got {other:?}"),
        }
        handle.abort();
    }

    #[tokio::test]
    async fn invalid_client_names_the_credential_that_is_wrong() {
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Error {
                status: 401,
                body: json!({
                    "error": "invalid_client",
                    "error_description": "Unauthorized",
                }),
            }]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider).await;
        let c = client(token_url, Some("wrong-secret"));

        let err = c.exchange("code", "verifier").await.unwrap_err();
        assert!(matches!(err, OAuthError::InvalidClient { .. }));
        assert!(err.to_string().contains("client id or client secret is wrong"));
        handle.abort();
    }

    #[tokio::test]
    async fn an_unreached_endpoint_is_transient() {
        // Nothing is listening on this port: a connection failure, not a
        // provider's answer.
        let c = client("http://127.0.0.1:1/token".to_string(), None);
        let err = c.exchange("code", "verifier").await.unwrap_err();
        assert!(matches!(err, OAuthError::Transient { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn a_parse_failure_never_carries_the_raw_body() {
        // A response that is not a valid token or error shape at all --
        // deliberately including what looks like a live access token, to
        // prove it does not leak into the error this module produces.
        let provider = Arc::new(MockProvider {
            answers: Mutex::new(vec![Answer::Token(json!({
                "access_token": "SECRET-THAT-MUST-NOT-LEAK",
                "not_a_field_this_crate_expects": {"nested": true},
                // No `token_type`: `oauth2` requires it, so this fails to
                // parse as a `BasicTokenResponse` despite looking token-shaped.
            }))]),
            requests: Mutex::new(Vec::new()),
        });
        let (token_url, handle) = start(provider).await;
        let c = client(token_url, None);

        let err = c.exchange("code", "verifier").await.unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains("SECRET-THAT-MUST-NOT-LEAK"), "{rendered}");
        handle.abort();
    }

    #[test]
    fn the_pkce_verifier_matches_the_challenge_in_the_url() {
        let c = client("http://127.0.0.1:9/token".to_string(), None);
        let auth = c.begin(None).unwrap();

        let parsed = url::Url::parse(&auth.url).unwrap();
        let pairs: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("code_challenge_method").map(String::as_str), Some("S256"));
        assert_eq!(pairs.get("state").map(String::as_str), Some(auth.state.as_str()));

        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(auth.pkce_verifier.as_bytes()));
        assert_eq!(pairs.get("code_challenge"), Some(&expected));
    }

    #[test]
    fn begin_adds_googles_offline_params_only_for_google() {
        let mut google = client("http://127.0.0.1:9/token".to_string(), None);
        google.auth_url = "https://accounts.google.com/o/oauth2/v2/auth".to_string();
        let auth = google.begin(None).unwrap();
        assert!(auth.url.contains("access_type=offline"));
        assert!(auth.url.contains("prompt=consent"));

        let other = client("http://127.0.0.1:9/token".to_string(), None);
        let auth = other.begin(None).unwrap();
        assert!(!auth.url.contains("access_type"));
        assert!(!auth.url.contains("prompt="));
    }

    #[test]
    fn begin_includes_a_login_hint_when_given() {
        let c = client("http://127.0.0.1:9/token".to_string(), None);
        let auth = c.begin(Some("person@example.com")).unwrap();
        assert!(auth.url.contains("login_hint=person%40example.com"));

        let without = c.begin(None).unwrap();
        assert!(!without.url.contains("login_hint"));
    }

    #[test]
    fn debug_never_prints_a_secret() {
        let c = client("http://127.0.0.1:9/token".to_string(), Some("top-secret-value"));
        assert!(!format!("{c:?}").contains("top-secret-value"));

        let auth = c.begin(None).unwrap();
        assert!(!format!("{auth:?}").contains(&auth.pkce_verifier));

        let tokens = Tokens {
            access_token: "AT-secret".to_string(),
            expires_at: jiff::Timestamp::now(),
            refresh_token: Some("RT-secret".to_string()),
            scope: Some("mail.read".to_string()),
        };
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains("AT-secret"));
        assert!(!rendered.contains("RT-secret"));
        // `scope` is not a secret and should still be legible -- this is a
        // redaction test, not a "print nothing" test.
        assert!(rendered.contains("mail.read"));
    }

    #[test]
    fn xoauth2_matches_the_documented_wire_format() {
        // The exact vector `lettre` tests its own `Mechanism::Xoauth2`
        // against, so a change here that breaks either library's consumer
        // shows up immediately rather than at a live server.
        let sasl = xoauth2_sasl("username", "vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg==");
        assert_eq!(
            sasl,
            "user=username\x01auth=Bearer vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg==\x01\x01"
        );
        // Not base64: a base64 alphabet never contains `=` outside trailing
        // padding, and this string's `=` sits mid-string after `Bearer`.
        assert!(sasl.contains('\x01'));
    }
}
