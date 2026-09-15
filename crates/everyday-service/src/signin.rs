//! Driving one OAuth sign-in from the browser's side: bind the loopback,
//! build the authorization URL, wait for the redirect, exchange the code --
//! and hold the result for `domains::signin`'s commands to hand onward.
//!
//! # The hand-off this leaves for the accounts work
//!
//! This module never writes to the vault. It cannot: it has no `Account`
//! record to write, because that record is being built alongside this in a
//! separate change (see `docs/plans/mail.md`'s Phase 1). What it does
//! instead is hold a finished sign-in's [`Tokens`] in memory, keyed by the
//! `sign_in_id` the interface already has, for exactly as long as it takes
//! `save_account` to come and claim them.
//!
//! **TODO(accounts): the claim step.** Once `Account` and `save_account`
//! exist, `save_account` must call [`SignIns::claim_tokens`] with the
//! `sign_in_id` the interface passed alongside the rest of the new
//! account's fields (the same id `await_oauth_sign_in` answered
//! `tokens_saved_under` with), take the [`Tokens`] it returns, and seal the
//! refresh token (and client secret, if the provider issued one) into the
//! vault's `AccountSecret` row through `SecretStore` -- never anywhere
//! else, and never logged; see [`Tokens`]'s own `Debug` impl for why that is
//! enforced by the type rather than by convention. `claim_tokens` removes
//! the entry as it returns it, so calling it twice for the same
//! `sign_in_id` is *not* how a caller should expect to recover from a
//! failed save -- a save that fails after claiming has to ask the person to
//! sign in again, the same as an expired sign-in does, rather than assume a
//! second claim will still find something there.
//!
//! # Why this holds state at all, rather than everything living in the
//! command bodies
//!
//! `begin_oauth_sign_in` has to return in milliseconds -- it only binds a
//! port and builds a URL -- while the actual wait for the browser can take
//! up to [`SIGN_IN_TIMEOUT`]. Those cannot be the same call, so the wait
//! runs in a task [`SignIns::begin`] spawns and forgets, and
//! `await_oauth_sign_in` is a second call that reads whatever that task
//! eventually decided. [`SignIns`] is where both calls meet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use everyday_mail::oauth::{Loopback, LoopbackError, OAuthClient, OAuthError, Tokens};
use tokio::sync::watch;

use crate::error::{CommandError, CommandResult, codes};

/// How long a sign-in has, from `begin_oauth_sign_in` to a browser coming
/// back to the loopback. Ten minutes is generous for "open this link,
/// possibly switch to a browser that is not already running, sign in,
/// grant consent" -- and short enough that a sign-in someone abandoned does
/// not sit in memory for the rest of the session.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Extra time a finished flow, and the tokens it produced, are kept
/// reachable after [`SIGN_IN_TIMEOUT`] would otherwise have elapsed --
/// covering the exchange itself and an interface that was a little slow to
/// call `await_oauth_sign_in` right as the browser landed. Swept, not
/// enforced strictly: see [`SignIns::sweep`].
const GRACE: Duration = Duration::from_secs(60);

/// What [`SignIns::begin`] hands back to `begin_oauth_sign_in`.
pub struct Begun {
    pub sign_in_id: String,
    pub url: String,
}

#[derive(Clone)]
enum Outcome {
    Success,
    Failed(CommandError),
}

struct Flow {
    cancel: watch::Sender<bool>,
    outcome: watch::Receiver<Option<Outcome>>,
    started_at: jiff::Timestamp,
}

/// Sign-ins in flight, and the tokens a finished one is waiting to be
/// claimed under. See the module doc.
#[derive(Default)]
pub struct SignIns {
    flows: Mutex<HashMap<String, Flow>>,
    completed: Mutex<HashMap<String, (Tokens, jiff::Timestamp)>>,
}

impl SignIns {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a sign-in: bind the loopback, mint the authorization URL, and
    /// spawn the wait-then-exchange that `await_oauth_sign_in` reads the
    /// result of.
    ///
    /// `client.redirect` is overwritten with the loopback's own
    /// `http://127.0.0.1:{port}/callback` before anything is built from it
    /// -- the caller does not, and should not, know the port in advance.
    pub async fn begin(
        self: &Arc<Self>,
        mut client: OAuthClient,
        login_hint: Option<&str>,
    ) -> CommandResult<Begun> {
        self.sweep();

        let loopback = Loopback::bind().await.map_err(|e| {
            CommandError::new(codes::IO, format!("could not open the loopback: {e}"))
        })?;
        client.redirect = loopback.redirect_uri().to_string();
        let authorization = client.begin(login_hint).map_err(command_error_from_oauth)?;

        let sign_in_id = uuid::Uuid::now_v7().to_string();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (outcome_tx, outcome_rx) = watch::channel(None);
        self.flows.lock().unwrap().insert(
            sign_in_id.clone(),
            Flow { cancel: cancel_tx, outcome: outcome_rx, started_at: jiff::Timestamp::now() },
        );

        let this = self.clone();
        let id = sign_in_id.clone();
        let state = authorization.state.clone();
        let verifier = authorization.pkce_verifier.clone();
        // Spawned and forgotten: nothing awaits this handle, because
        // `SignIns` is not itself the thing waiting -- `wait`, called
        // separately and possibly more than once, is. The outcome channel
        // is this task's only way of being heard; not removing the `Flow`
        // once it fires (see `sweep` instead) is what lets a caller who has
        // not made its first `await_oauth_sign_in` call yet still find it.
        tokio::spawn(async move {
            let outcome = match Self::run(&client, loopback, &state, &verifier, cancel_rx).await {
                Ok(tokens) => {
                    this.completed.lock().unwrap().insert(id, (tokens, jiff::Timestamp::now()));
                    Outcome::Success
                }
                Err(err) => Outcome::Failed(err),
            };
            let _ = outcome_tx.send(Some(outcome));
        });

        Ok(Begun { sign_in_id, url: authorization.url })
    }

    /// The loopback wait and the token exchange that follows it, as one
    /// fallible step -- kept out of the spawned closure above only so its
    /// `?`-heavy body reads as ordinary async code rather than a `match`
    /// nested inside a `match`.
    async fn run(
        client: &OAuthClient,
        loopback: Loopback,
        state: &str,
        verifier: &str,
        cancel_rx: watch::Receiver<bool>,
    ) -> CommandResult<Tokens> {
        let code = loopback
            .wait(state, SIGN_IN_TIMEOUT, cancel_rx)
            .await
            .map_err(command_error_from_loopback)?;
        client.exchange(&code, verifier).await.map_err(command_error_from_oauth)
    }

    /// Wait for `sign_in_id`'s flow to finish, and answer with the handle
    /// `claim_tokens` reads -- the `sign_in_id` itself. Safe to call more
    /// than once, and from more than one caller at a time: this only reads
    /// the flow's outcome, it never consumes it.
    pub async fn wait(&self, sign_in_id: &str) -> CommandResult<String> {
        let mut outcome_rx = {
            let flows = self.flows.lock().unwrap();
            let flow = flows.get(sign_in_id).ok_or_else(|| {
                CommandError::new(codes::NOT_FOUND, "no sign-in is waiting under that id")
            })?;
            flow.outcome.clone()
        };
        loop {
            if let Some(outcome) = outcome_rx.borrow_and_update().clone() {
                return match outcome {
                    Outcome::Success => Ok(sign_in_id.to_string()),
                    Outcome::Failed(err) => Err(err),
                };
            }
            if outcome_rx.changed().await.is_err() {
                // The sender was dropped without ever sending -- only
                // possible if the spawned task panicked before `finish`.
                return Err(CommandError::new(
                    codes::INTERNAL,
                    "the sign-in ended without an answer",
                ));
            }
        }
    }

    /// Withdraw a sign-in: signal its loopback to stop waiting, and drop
    /// whatever it had already produced. Silently does nothing for an id
    /// that has already finished, already been claimed, or never existed --
    /// cancelling something that is already over is not a caller's mistake.
    pub fn cancel(&self, sign_in_id: &str) {
        if let Some(flow) = self.flows.lock().unwrap().remove(sign_in_id) {
            let _ = flow.cancel.send(true);
        }
        self.completed.lock().unwrap().remove(sign_in_id);
    }

    /// Take a finished sign-in's tokens, if any are still waiting under
    /// this id. See the module doc's TODO: this is the one call
    /// `save_account` is meant to make, and it removes what it returns.
    pub fn claim_tokens(&self, sign_in_id: &str) -> Option<Tokens> {
        self.completed.lock().unwrap().remove(sign_in_id).map(|(tokens, _)| tokens)
    }

    /// Drop every flow and every unclaimed token. Called when the vault
    /// locks or closes: a locked vault has no key for `save_account` to
    /// write with, so a sign-in nobody can finish claiming is a secret
    /// sitting in memory for no further purpose.
    pub fn clear(&self) {
        for (_, flow) in self.flows.lock().unwrap().drain() {
            let _ = flow.cancel.send(true);
        }
        self.completed.lock().unwrap().clear();
    }

    /// Drop flows and unclaimed tokens old enough that [`SIGN_IN_TIMEOUT`]
    /// guarantees they are finished one way or another. Run at the start of
    /// [`Self::begin`] rather than on a timer of its own -- this process
    /// already has a scheduler and a supervisor for things that must run
    /// whether or not anyone is signing in; a sign-in that never happens
    /// again needs no clock of its own, only to not leak the one before it.
    fn sweep(&self) {
        let Ok(cutoff) = jiff::Timestamp::now().checked_sub(jiff::SignedDuration::from_secs(
            (SIGN_IN_TIMEOUT + GRACE).as_secs() as i64,
        )) else {
            return;
        };
        self.flows.lock().unwrap().retain(|_, f| f.started_at > cutoff);
        self.completed.lock().unwrap().retain(|_, (_, at)| *at > cutoff);
    }
}

/// [`OAuthError`] carries no secret -- see its own doc -- so every variant
/// is safe to fold straight into a [`CommandError`]'s message.
fn command_error_from_oauth(e: OAuthError) -> CommandError {
    match e {
        OAuthError::InvalidGrant { description } => {
            CommandError::new(codes::INVALID_GRANT, description)
        }
        OAuthError::InvalidClient { description } => {
            CommandError::new(codes::INVALID_CLIENT, description)
        }
        OAuthError::Transient { message } => CommandError::new(codes::NETWORK, message),
        OAuthError::Provider { code, description } => {
            CommandError::new(codes::PROVIDER, format!("{code}: {description}"))
        }
    }
}

fn command_error_from_loopback(e: LoopbackError) -> CommandError {
    match e {
        LoopbackError::TimedOut => CommandError::new(codes::TIMED_OUT, e.to_string()),
        LoopbackError::Cancelled => CommandError::new(codes::CANCELLED, e.to_string()),
        LoopbackError::StateMismatch => CommandError::new(codes::FORBIDDEN, e.to_string()),
        LoopbackError::Provider { code, description } => {
            CommandError::new(codes::PROVIDER, format!("{code}: {description}"))
        }
        LoopbackError::NoCode => CommandError::new(codes::INVALID, e.to_string()),
        LoopbackError::Io(err) => CommandError::new(codes::IO, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn client(token_url: String) -> OAuthClient {
        OAuthClient {
            client_id: "id".to_string(),
            client_secret: None,
            auth_url: "https://example.test/auth".to_string(),
            token_url,
            scopes: vec!["mail.read".to_string()],
            redirect: String::new(),
        }
    }

    /// Drive a browser through the loopback by hand: fetch `url`, follow
    /// nothing (there is nothing to follow), just let the GET land.
    async fn visit(url: &str) {
        let parsed = url::Url::parse(url).unwrap();
        let mut stream =
            tokio::net::TcpStream::connect((parsed.host_str().unwrap(), parsed.port().unwrap()))
                .await
                .unwrap();
        let request = format!(
            "GET {}?{} HTTP/1.1\r\nConnection: close\r\n\r\n",
            parsed.path(),
            parsed.query().unwrap_or("")
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let _ = reader.read_line(&mut line).await;
    }

    /// A one-request mock token endpoint answering the same body every
    /// time it is asked, so these tests exercise `SignIns` itself rather
    /// than repeat `oauth::client`'s own coverage of the exchange.
    async fn mock_token_endpoint(access_token: &'static str) -> String {
        use axum::Json;
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/token",
            post(move || async move {
                Json(serde_json::json!({
                    "access_token": access_token,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://127.0.0.1:{port}/token")
    }

    #[tokio::test]
    async fn a_full_sign_in_lands_its_tokens_under_the_id() {
        let token_url = mock_token_endpoint("at-1").await;
        let sign_ins = Arc::new(SignIns::new());

        let begun = sign_ins.begin(client(token_url), None).await.unwrap();
        assert!(begun.url.starts_with("https://example.test/auth?"));

        // Extract the state the way a browser would carry it back, and
        // answer the loopback the URL already points at.
        let parsed = url::Url::parse(&begun.url).unwrap();
        let state = parsed.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();
        let redirect = parsed.query_pairs().find(|(k, _)| k == "redirect_uri");
        let redirect_uri = redirect.map(|(_, v)| v.into_owned()).unwrap();
        visit(&format!("{redirect_uri}?code=abc&state={state}")).await;

        let handle = sign_ins.wait(&begun.sign_in_id).await.unwrap();
        assert_eq!(handle, begun.sign_in_id);

        let tokens = sign_ins.claim_tokens(&begun.sign_in_id).unwrap();
        assert_eq!(tokens.access_token, "at-1");
        // Claimed once: a second claim finds nothing left.
        assert!(sign_ins.claim_tokens(&begun.sign_in_id).is_none());
    }

    #[tokio::test]
    async fn cancelling_makes_the_wait_fail_promptly() {
        let sign_ins = Arc::new(SignIns::new());
        let begun =
            sign_ins.begin(client("http://127.0.0.1:1/token".to_string()), None).await.unwrap();

        sign_ins.cancel(&begun.sign_in_id);
        // The flow was removed outright by `cancel`, so nothing is left to
        // wait on -- exactly like an id that never existed.
        let err = sign_ins.wait(&begun.sign_in_id).await.unwrap_err();
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn waiting_on_an_unknown_id_is_not_found() {
        let sign_ins = SignIns::new();
        let err = sign_ins.wait("no-such-id").await.unwrap_err();
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn clear_cancels_every_flow_and_drops_unclaimed_tokens() {
        let token_url = mock_token_endpoint("at-1").await;
        let sign_ins = Arc::new(SignIns::new());
        let begun = sign_ins.begin(client(token_url), None).await.unwrap();

        sign_ins.clear();
        let err = sign_ins.wait(&begun.sign_in_id).await.unwrap_err();
        assert_eq!(err.code, codes::NOT_FOUND);
    }
}
