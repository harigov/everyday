//! Signing a connection in with OAuth: discovery, registration, the browser,
//! the code exchange, and the grant sealed into the connection's secret.
//!
//! See `docs/plans/connections.md`'s "Sign-in". The shape follows
//! [`crate::signin`], the mail accounts' sign-in: `begin` returns in
//! moments with an address to open, the wait for the browser runs in a task,
//! and `wait` -- which may be called more than once -- reads what that task
//! decided. The difference is where the result goes: a mail sign-in leaves
//! its tokens in memory for an account to claim, while a connection already
//! exists, so the task seals the grant into its secret itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use karmayoga_connect::oauth::{self, ClientCreds, OAuthError, Pkce, TokenAuth};
use karmayoga_core::connection::{AuthMethod, OAuthGrant, Transport};
use karmayoga_core::{ConnectionId, Vault};
use karmayoga_mail::oauth::{Loopback, LoopbackError};
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use tokio::sync::watch;

use super::{HubError, core_token_auth, http_client, hub, read_connection, vault_task};
use crate::error::codes;

/// How long the browser has to come back.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long a sign-in is remembered after it began, so `wait` can be asked
/// again after a dropped connection. A little longer than the wait itself.
const KEEP: Duration = Duration::from_secs(15 * 60);

/// What the task decided, once it has: the connection signed in, or why not.
type Outcome = Option<Result<ConnectionId, HubError>>;

/// A sign-in begun: the address to open, and the id to wait on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Begun {
    pub sign_in_id: String,
    pub url: String,
}

struct Pending {
    connection: ConnectionId,
    /// Turned true to withdraw the sign-in. The task holds the receiving end.
    cancel: watch::Sender<bool>,
    outcome: watch::Receiver<Outcome>,
    began: Instant,
}

/// The sign-ins in flight. Each holds a PKCE verifier and a loopback
/// listener, for at most ten minutes.
#[derive(Default)]
pub struct ConnectionSignIns {
    pending: Mutex<HashMap<String, Pending>>,
}

impl ConnectionSignIns {
    /// Start signing connection `id` in: discover its authorization server,
    /// use the connection's own client or register one, bind the loopback
    /// (on the connection's `redirect_port`, if set), and answer the address
    /// to open. The wait for the browser starts at once in the background.
    pub async fn begin(&self, vault: &Arc<Vault>, id: ConnectionId) -> Result<Begun, HubError> {
        self.sweep();
        let (connection, secret) = read_connection(vault, id).await?;
        let (Transport::Http { url, .. }, AuthMethod::OAuth { client_id, scopes, redirect_port }) =
            (&connection.transport, &connection.auth)
        else {
            return Err(HubError::new("this connection does not sign in with a browser"));
        };

        // A second click on "Sign in" must not find the first one's port
        // still held (Slack's is fixed), so the earlier attempt is ended and
        // given a moment to let go of it.
        self.end_for(id).await;

        let http = http_client();
        let (resource_metadata, challenge_scope) = challenge(&http, url).await;
        let discovery =
            oauth::discover(&http, url, resource_metadata.as_deref()).await.map_err(oauth_error)?;

        let port = redirect_port.unwrap_or(0);
        let loopback = Loopback::bind_port(port).await.map_err(|e| {
            if port == 0 {
                HubError::coded(codes::IO, format!("could not open a port for the sign-in: {e}"))
            } else {
                HubError::coded(
                    codes::IO,
                    format!(
                        "could not listen on port {port} for the sign-in ({e}); is another \
                         program using it?"
                    ),
                )
            }
        })?;
        let redirect_uri = loopback.redirect_uri().to_string();

        let creds = match client_id.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(own) => {
                let client_secret = secret.client_secret.clone().filter(|s| !s.is_empty());
                let methods = &discovery.server.token_endpoint_auth_methods;
                let lists = |m: &str| methods.iter().any(|x| x == m);
                let auth = match &client_secret {
                    None => TokenAuth::None,
                    Some(_) if methods.is_empty() || lists("client_secret_post") => {
                        TokenAuth::ClientSecretPost
                    }
                    Some(_) if lists("client_secret_basic") => TokenAuth::ClientSecretBasic,
                    Some(_) => TokenAuth::ClientSecretPost,
                };
                ClientCreds { client_id: own.to_string(), client_secret, auth }
            }
            None => oauth::register(&http, &discovery.server, &redirect_uri, "Karma Yoga")
                .await
                .map_err(oauth_error)?,
        };

        let scopes = oauth::choose_scopes(scopes, challenge_scope.as_deref(), &discovery.resource);
        let pkce = Pkce::new();
        let state = oauth::random_state();
        let resource = discovery.resource.resource.clone();
        let url = oauth::authorize_url(
            &discovery.server,
            &creds.client_id,
            &redirect_uri,
            &scopes,
            &state,
            &pkce,
            Some(resource.as_str()),
        );

        let sign_in_id = uuid::Uuid::new_v4().to_string();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (outcome_tx, outcome_rx) = watch::channel::<Outcome>(None);
        self.pending.lock().unwrap().insert(
            sign_in_id.clone(),
            Pending {
                connection: id,
                cancel: cancel_tx,
                outcome: outcome_rx,
                began: Instant::now(),
            },
        );

        let token_endpoint = discovery.server.token_endpoint.clone();
        let vault = Arc::clone(vault);
        // Spawned and left: `wait`, called separately, is what listens. The
        // outcome channel is this task's only voice, and the entry stays in
        // the map after it speaks, so a caller late to ask still hears it.
        tokio::spawn(async move {
            let result = finish(
                &vault,
                id,
                loopback,
                &state,
                cancel_rx,
                &http,
                &token_endpoint,
                &creds,
                &pkce,
                &redirect_uri,
                &resource,
            )
            .await;
            outcome_tx.send_replace(Some(result));
        });

        Ok(Begun { sign_in_id, url })
    }

    /// Wait for sign-in `sign_in_id` to finish: the browser came back, the
    /// code was exchanged, and the grant is sealed into the connection's
    /// secret (and its state no longer says it needs a sign-in). Safe to
    /// call again after a dropped connection.
    pub async fn wait(
        &self,
        vault: &Arc<Vault>,
        sign_in_id: &str,
    ) -> Result<ConnectionId, HubError> {
        // Nothing to read from the vault: the task wrote what there was.
        let _ = vault;
        self.sweep();
        let mut outcome =
            self.pending.lock().unwrap().get(sign_in_id).map(|p| p.outcome.clone()).ok_or_else(
                || {
                    HubError::coded(
                        codes::NOT_FOUND,
                        "that sign-in is not in progress; it may have expired. Start it again.",
                    )
                },
            )?;
        let decided = outcome
            .wait_for(|o| o.is_some())
            .await
            .map_err(|_| HubError::coded(codes::CANCELLED, "the sign-in was cancelled"))?
            .clone();
        decided
            .unwrap_or_else(|| Err(HubError::coded(codes::CANCELLED, "the sign-in was cancelled")))
    }

    /// Withdraw a sign-in nobody is going to finish. A no-op for one that is
    /// unknown or already over.
    pub fn cancel(&self, sign_in_id: &str) {
        if let Some(p) = self.pending.lock().unwrap().remove(sign_in_id) {
            p.cancel.send_replace(true);
        }
    }

    /// Withdraw every sign-in of one connection: it was removed.
    pub fn cancel_connection(&self, id: ConnectionId) {
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, p| {
            let ours = p.connection == id;
            if ours {
                p.cancel.send_replace(true);
            }
            !ours
        });
    }

    /// Withdraw them all: the vault locked.
    pub fn clear(&self) {
        for (_, p) in self.pending.lock().unwrap().drain() {
            p.cancel.send_replace(true);
        }
    }

    /// Forget sign-ins that began too long ago to be asked about.
    fn sweep(&self) {
        self.pending.lock().unwrap().retain(|_, p| {
            let keep = p.began.elapsed() < KEEP;
            if !keep {
                p.cancel.send_replace(true);
            }
            keep
        });
    }

    /// End the connection's earlier, unfinished sign-ins, and wait (briefly)
    /// for each to give up its port.
    async fn end_for(&self, id: ConnectionId) {
        let mut waiting = Vec::new();
        {
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, p| {
                if p.connection != id {
                    return true;
                }
                if p.outcome.borrow().is_none() {
                    p.cancel.send_replace(true);
                    waiting.push(p.outcome.clone());
                }
                false
            });
        }
        for mut outcome in waiting {
            let _ = tokio::time::timeout(Duration::from_secs(2), outcome.wait_for(|o| o.is_some()))
                .await;
        }
    }
}

/// Ask the server what it wants, the way a client with no token would: an
/// `initialize` it will answer with 401 and a `WWW-Authenticate` naming its
/// resource metadata and the scope to ask for. Anything else -- no answer, a
/// different status -- is "no hint", and discovery falls back to the
/// well-known addresses.
async fn challenge(http: &reqwest::Client, url: &str) -> (Option<String>, Option<String>) {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": karmayoga_connect::LEGACY_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "Karma Yoga", "version": env!("CARGO_PKG_VERSION") },
        },
    })
    .to_string();
    let answer = http
        .post(url)
        .timeout(Duration::from_secs(15))
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .body(body)
        .send()
        .await;
    let Ok(answer) = answer else { return (None, None) };
    if answer.status() != reqwest::StatusCode::UNAUTHORIZED {
        return (None, None);
    }
    answer
        .headers()
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(oauth::parse_www_authenticate)
        .find(|(metadata, scope)| metadata.is_some() || scope.is_some())
        .unwrap_or((None, None))
}

/// Everything after the browser is opened: wait for it to come back,
/// exchange the code, seal the grant, and drop whatever client was open
/// with the old one.
#[allow(clippy::too_many_arguments)]
async fn finish(
    vault: &Arc<Vault>,
    id: ConnectionId,
    loopback: Loopback,
    state: &str,
    cancel: watch::Receiver<bool>,
    http: &reqwest::Client,
    token_endpoint: &str,
    creds: &ClientCreds,
    pkce: &Pkce,
    redirect_uri: &str,
    resource: &str,
) -> Result<ConnectionId, HubError> {
    let withdrawn = cancel.clone();
    let code = loopback.wait(state, SIGN_IN_TIMEOUT, cancel).await.map_err(loopback_error)?;
    let grant = oauth::exchange(
        http,
        token_endpoint,
        creds,
        &code,
        &pkce.verifier,
        redirect_uri,
        Some(resource),
    )
    .await
    .map_err(oauth_error)?;
    // Withdrawn (or the vault locked) while the exchange was in flight: the
    // token is thrown away rather than sealed into a vault nobody asked.
    if *withdrawn.borrow() {
        return Err(HubError::coded(codes::CANCELLED, "the sign-in was cancelled"));
    }

    let sealed = OAuthGrant {
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
        token_endpoint: token_endpoint.to_string(),
        token_auth: core_token_auth(creds.auth),
        resource: Some(resource.to_string()),
        access_token: grant.access_token,
        refresh_token: grant.refresh_token,
        expires_at: grant.expires_at,
        scope: grant.scope,
    };
    let writing = Arc::clone(vault);
    vault_task(move || {
        let mut secret = writing.connection_secret(id)?;
        secret.grant = Some(sealed);
        writing.set_connection_secret(id, &secret)?;
        writing.update_connection_state(id, |state| {
            state.needs_sign_in = false;
            state.last_error = None;
        })
    })
    .await?;
    hub().forget(id);
    Ok(id)
}

fn loopback_error(e: LoopbackError) -> HubError {
    match e {
        LoopbackError::TimedOut => HubError::coded(
            codes::TIMED_OUT,
            "nobody came back from the browser in ten minutes; sign in again",
        ),
        LoopbackError::Cancelled => HubError::coded(codes::CANCELLED, "the sign-in was cancelled"),
        other => HubError::coded(codes::PROVIDER, other.to_string()),
    }
}

fn oauth_error(e: OAuthError) -> HubError {
    match e {
        OAuthError::NoRegistration => HubError::coded(
            codes::INVALID_CLIENT,
            "This service needs a client ID of your own: add one under Sign-in, then sign in again.",
        ),
        OAuthError::InvalidClient(_) => HubError::coded(codes::INVALID_CLIENT, e.to_string()),
        OAuthError::InvalidGrant => HubError::coded(codes::INVALID_GRANT, e.to_string()),
        other => HubError::coded(codes::PROVIDER, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_failures_carry_the_code_a_client_routes_on() {
        assert_eq!(oauth_error(OAuthError::NoRegistration).code, Some(codes::INVALID_CLIENT));
        assert_eq!(oauth_error(OAuthError::InvalidGrant).code, Some(codes::INVALID_GRANT));
        assert_eq!(
            oauth_error(OAuthError::InvalidClient("bad".into())).code,
            Some(codes::INVALID_CLIENT)
        );
        assert_eq!(oauth_error(OAuthError::Http("down".into())).code, Some(codes::PROVIDER));
        assert!(oauth_error(OAuthError::NoRegistration).message.contains("client ID of your own"));
    }

    #[test]
    fn loopback_failures_carry_their_codes() {
        assert_eq!(loopback_error(LoopbackError::TimedOut).code, Some(codes::TIMED_OUT));
        assert_eq!(loopback_error(LoopbackError::Cancelled).code, Some(codes::CANCELLED));
        assert_eq!(loopback_error(LoopbackError::NoCode).code, Some(codes::PROVIDER));
    }

    #[tokio::test]
    async fn waiting_on_an_unknown_sign_in_is_not_found() {
        let sign_ins = ConnectionSignIns::default();
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(
            karmayoga_vault::create(
                dir.path(),
                karmayoga_core::VaultConfig {
                    name: "Test".into(),
                    backend: "sqlite".into(),
                    settings: Default::default(),
                    password: None,
                    kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
                    auto_lock_seconds: 900,
                    forget_key_seconds: 0,
                },
            )
            .unwrap(),
        );
        let e = sign_ins.wait(&vault, "nope").await.unwrap_err();
        assert_eq!(e.code, Some(codes::NOT_FOUND));
    }
}
