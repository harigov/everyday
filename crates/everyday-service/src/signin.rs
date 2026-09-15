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
//! `attach_oauth_sign_in` to come and claim them.
//!
//! **The claim step** is `attach_oauth_sign_in` in `domains::accounts`: it
//! calls [`SignIns::claim_tokens`] with the id `await_oauth_sign_in`
//! answered, and seals the refresh token, the access token and any client
//! secret into the account's `AccountSecret` -- never anywhere else, and
//! never logged; see [`Tokens`]'s own `Debug` impl for why that is enforced
//! by the type rather than by convention. `claim_tokens` removes the entry as
//! it returns it, so a save that fails after claiming has to ask the person
//! to sign in again, the same as an expired sign-in does, rather than assume
//! a second claim will still find something there.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Flipped by [`SignIns::cancel`], read by this flow's own spawned task
    /// right before it would otherwise insert into `completed`. See the
    /// module doc on why `cancel`'s removal of an already-`completed` entry
    /// is not, by itself, enough: a flow cancelled while its token exchange
    /// is still in flight has nothing in `completed` yet for that removal
    /// to find, and the exchange finishing *after* `cancel` returns is
    /// exactly the ordering a real cancel-during-sign-in produces. This
    /// flag is what the task checks instead of trusting that `cancel` ran
    /// either before or after it, because it cannot know which.
    cancelled: Arc<AtomicBool>,
}

/// Sign-ins in flight, and the tokens a finished one is waiting to be
/// claimed under. See the module doc.
pub struct SignIns {
    flows: Mutex<HashMap<String, Flow>>,
    completed: Mutex<HashMap<String, (Tokens, jiff::Timestamp)>>,
    /// Bumped by [`SignIns::clear`], and captured by every [`Flow`] at the
    /// moment it is spawned. A flow whose captured epoch no longer matches
    /// this one when its task goes to insert into `completed` was cleared
    /// out from under it -- the vault locked or closed mid-exchange -- and
    /// must not write tokens for a vault that is no longer there to receive
    /// them. `cancel` has a `Flow` to flip a flag on; `clear` drops every
    /// `Flow` at once, so it needs one counter instead of one flag per flow
    /// it is about to forget.
    epoch: AtomicU64,
}

impl Default for SignIns {
    fn default() -> Self {
        Self {
            flows: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            epoch: AtomicU64::new(0),
        }
    }
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
        let cancelled = Arc::new(AtomicBool::new(false));
        // Captured now, compared again after the exchange: see `epoch`'s own
        // doc for why a counter, not a flag, is what `clear` needs.
        let born_epoch = self.epoch.load(Ordering::SeqCst);
        self.flows.lock().unwrap().insert(
            sign_in_id.clone(),
            Flow {
                cancel: cancel_tx,
                outcome: outcome_rx,
                started_at: jiff::Timestamp::now(),
                cancelled: Arc::clone(&cancelled),
            },
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
                Ok(mut tokens) => {
                    // The check and the insert happen inside the same lock
                    // acquisition as the write, deliberately: a `cancel` or
                    // a `clear` that lands between "check" and "insert" as
                    // two separate steps would otherwise still let the
                    // tokens through, which is the exact race this closes.
                    // A `cancel` that flips the flag and then finds nothing
                    // yet in `completed` to remove, or a `clear` that bumps
                    // the epoch before this task ever reaches this line,
                    // must both still be visible here -- and because
                    // `cancelled` is set (by `cancel`) strictly before
                    // `cancel` returns, and `epoch` is bumped (by `clear`)
                    // strictly before `clear` drops the flows, either one
                    // happening-before this read is exactly what the
                    // ordinary atomic-ordering guarantee promises will be
                    // observed here.
                    let mut completed = this.completed.lock().unwrap();
                    let still_wanted = !cancelled.load(Ordering::SeqCst)
                        && this.epoch.load(Ordering::SeqCst) == born_epoch;
                    if still_wanted {
                        completed.insert(id, (tokens, jiff::Timestamp::now()));
                        drop(completed);
                        Outcome::Success
                    } else {
                        drop(completed);
                        scrub_tokens(&mut tokens);
                        drop(tokens);
                        Outcome::Failed(CommandError::new(
                            codes::CANCELLED,
                            "the sign-in was cancelled before it finished",
                        ))
                    }
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

    /// Withdraw a sign-in: signal its loopback to stop waiting, flip the
    /// flag its spawned task checks before writing anything to `completed`,
    /// and drop whatever had already landed there. Silently does nothing
    /// for an id that has already finished, already been claimed, or never
    /// existed -- cancelling something that is already over is not a
    /// caller's mistake.
    ///
    /// The flag is flipped *before* this reads `completed`, and the task's
    /// own check-then-insert happens under `completed`'s lock -- see the
    /// spawned task's comment in [`Self::begin`] for why that ordering,
    /// not just this removal, is what actually closes the race: a token
    /// exchange still in flight when this is called has nothing in
    /// `completed` yet for the removal below to find, and finishes strictly
    /// after this function returns.
    pub fn cancel(&self, sign_in_id: &str) {
        if let Some(flow) = self.flows.lock().unwrap().remove(sign_in_id) {
            flow.cancelled.store(true, Ordering::SeqCst);
            let _ = flow.cancel.send(true);
        }
        if let Some((mut tokens, _)) = self.completed.lock().unwrap().remove(sign_in_id) {
            scrub_tokens(&mut tokens);
        }
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
    ///
    /// The epoch is bumped *first*, before anything else here runs, for the
    /// same reason [`Self::cancel`] flips its flag before touching
    /// `completed`: a task whose exchange is still in flight has to see a
    /// mismatched epoch the moment it checks, not a moment after, or it
    /// writes tokens for a vault this call has already decided has nothing
    /// left to write them to. Every flow still in `flows` at this point
    /// captured today's epoch when it was spawned, so bumping it here is
    /// enough to mark all of them, not just the ones this function happens
    /// to still be able to see.
    pub fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        for (_, flow) in self.flows.lock().unwrap().drain() {
            flow.cancelled.store(true, Ordering::SeqCst);
            let _ = flow.cancel.send(true);
        }
        for (_, (mut tokens, _)) in self.completed.lock().unwrap().drain() {
            scrub_tokens(&mut tokens);
        }
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

/// Overwrites a [`Tokens`]'s owned buffers with zero bytes before it is
/// dropped, for every token this module discards rather than hands onward
/// to `save_account` -- a cancelled flow, or one caught by `clear` mid
/// exchange.
///
/// `Tokens` (`everyday_mail::oauth::Tokens`) does not implement
/// [`zeroize::Zeroize`] -- it lives in a crate this fix does not touch, and
/// pulling in a new dependency for one call site here is more than this
/// needs -- so this does by hand exactly what that trait would: walk each
/// secret-carrying `String`'s bytes and overwrite them in place. That is
/// weaker than the crate (nothing here stops the compiler from having
/// already copied the bytes elsewhere on the way to this call, the same
/// caveat `zeroize` itself documents for anything it has not wrapped from
/// the moment of creation), but it is strictly better than the previous
/// behaviour, which was to let `drop` reclaim the allocation with the
/// secret still sitting in it for whatever reused that memory next to read.
fn scrub_tokens(tokens: &mut Tokens) {
    fn zero(s: &mut str) {
        // Safety: every byte is overwritten with `0` (a valid single-byte
        // UTF-8 code point, ASCII NUL) in place; the string's length and
        // allocation are untouched, so the result is still valid UTF-8 of
        // the same byte length `s` already was.
        unsafe {
            for byte in s.as_bytes_mut() {
                *byte = 0;
            }
        }
    }
    zero(&mut tokens.access_token);
    if let Some(refresh_token) = tokens.refresh_token.as_mut() {
        zero(refresh_token);
    }
    if let Some(scope) = tokens.scope.as_mut() {
        zero(scope);
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

    /// Like [`mock_token_endpoint`], but answers only after `delay` -- long
    /// enough that a test can call `cancel` or `clear` while the exchange
    /// this endpoint is answering is still in flight, the ordering the
    /// finding this guards against depends on.
    async fn mock_delayed_token_endpoint(access_token: &'static str, delay: Duration) -> String {
        use axum::Json;
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/token",
            post(move || async move {
                tokio::time::sleep(delay).await;
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

    /// Delivers the redirect the way a real browser would, so the exchange
    /// against `token_url` starts, without waiting for it to finish.
    async fn deliver_redirect(begun: &Begun) {
        let parsed = url::Url::parse(&begun.url).unwrap();
        let state = parsed.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();
        let redirect_uri =
            parsed.query_pairs().find(|(k, _)| k == "redirect_uri").unwrap().1.into_owned();
        visit(&format!("{redirect_uri}?code=abc&state={state}")).await;
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

    /// The race finding 4 closes: cancel a flow *after* the browser has
    /// already delivered its code, while the token exchange is still
    /// running against a deliberately slow mock endpoint. Before the fix,
    /// `cancel`'s removal from `completed` runs before the exchange ever
    /// puts anything there, so the tokens land afterwards regardless and
    /// `claim_tokens` still hands them back; with the fix, the spawned
    /// task's own check-then-insert sees the flag and drops them instead.
    #[tokio::test]
    async fn cancelling_during_the_exchange_drops_the_tokens_it_produces() {
        let token_url = mock_delayed_token_endpoint("at-1", Duration::from_millis(200)).await;
        let sign_ins = Arc::new(SignIns::new());
        let begun = sign_ins.begin(client(token_url), None).await.unwrap();

        deliver_redirect(&begun).await;
        // Give the loopback time to accept the code and start the (slow)
        // exchange before cancelling it mid-flight.
        tokio::time::sleep(Duration::from_millis(50)).await;
        sign_ins.cancel(&begun.sign_in_id);

        // Let the delayed exchange finish and try to land its tokens.
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(sign_ins.claim_tokens(&begun.sign_in_id).is_none());
    }

    /// The same race, triggered by `clear` (a locking or closing vault)
    /// instead of an explicit cancel: the epoch `clear` bumps has to be
    /// what the still-running exchange's task compares against, since
    /// `clear` has already forgotten the `Flow` by the time the exchange
    /// finishes and has nothing left to flip a per-flow flag on.
    #[tokio::test]
    async fn clearing_during_the_exchange_drops_the_tokens_it_produces() {
        let token_url = mock_delayed_token_endpoint("at-1", Duration::from_millis(200)).await;
        let sign_ins = Arc::new(SignIns::new());
        let begun = sign_ins.begin(client(token_url), None).await.unwrap();

        deliver_redirect(&begun).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        sign_ins.clear();

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(sign_ins.claim_tokens(&begun.sign_in_id).is_none());
    }

    /// A later sign-in started after `clear` bumped the epoch must not be
    /// mistaken for one of the flows `clear` just discarded -- the epoch it
    /// captures at `begin` time is the *current* one, so its own tokens
    /// land normally.
    #[tokio::test]
    async fn a_sign_in_begun_after_clear_still_lands_its_tokens() {
        let sign_ins = Arc::new(SignIns::new());
        // Clear with nothing in flight, the way a lock on an already-idle
        // session would.
        sign_ins.clear();

        let token_url = mock_token_endpoint("at-2").await;
        let begun = sign_ins.begin(client(token_url), None).await.unwrap();
        deliver_redirect(&begun).await;

        sign_ins.wait(&begun.sign_in_id).await.unwrap();
        let tokens = sign_ins.claim_tokens(&begun.sign_in_id).unwrap();
        assert_eq!(tokens.access_token, "at-2");
    }
}
