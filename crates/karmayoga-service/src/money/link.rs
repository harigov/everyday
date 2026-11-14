//! Linking a bank: Plaid's Hosted Link, opened in the person's browser.
//!
//! ```text
//!   start_bank_link    create a link token (hosted), keep it here under a
//!                      random session id, answer the id and the page's url
//!   (the interface opens the url in the browser; the person signs in)
//!   check_bank_link    ask Plaid how the session stands; when it finished
//!                      with an item, exchange the public token, store the
//!                      link and its access token, start its first sync
//! ```
//!
//! The rules, each a test below:
//!
//! - The link token never leaves this process: the interface gets an opaque
//!   session id (32 random bytes, hex) and the hosted page's url, nothing
//!   else, and the url is Plaid's or it is refused (`plaid::plaid_page`).
//!   Sessions are in `MoneyRuntime::sessions`, found only by their exact id,
//!   cleared at lock, and expire after 30 minutes (`Expired`); at most 4 at
//!   once (start refuses a fifth, before asking Plaid for anything).
//! - A session is finished exactly once: the first `check` that sees it done
//!   removes it, *then* does the exchange, so two windows polling cannot
//!   store a link twice. An exchanged link is stored with
//!   `Vault::add_bank_link` (link + token, one write), named by the
//!   institution, environment from the credentials used, then synced once
//!   immediately (best effort; a failed first sync leaves the link, its state
//!   saying why). A token Plaid issued that the vault could not store is
//!   handed back with `/item/remove`, rather than left counting against the
//!   person's ten banks where nobody can see it.
//! - Update mode (`relink: Some(id)`): the link's own access token goes in
//!   the link token request; finishing clears the link state's
//!   `needs_login`, makes it due now, syncs it, and stores nothing new.
//! - A relink of a link from another environment than the credentials' is
//!   refused before anything is sent: its token means nothing there.
//! - A session remembers the environment its link token was made in; if the
//!   keys change to the other one before it finishes, it fails rather than
//!   asking the wrong Plaid about it.
//! - `user_id` for Plaid: `vault.money_key("plaid-user")` -- stable per
//!   vault, meaningless outside it.
//! - `check` raises no change itself: the command answering `Done` does.

use super::plaid::{self, Host, ItemAdded, LinkProgress, Plaid};
use super::refresh::{self, environment_problem};
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::Vault;
use karmayoga_core::id::BankLinkId;
use karmayoga_core::money::{BankLink, LinkProvider, LinkState, PlaidEnvironment};
use rand::RngCore;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, MutexGuard};

/// How long a session lives. Plaid's own link tokens last longer; a person
/// who has not finished signing in to their bank in half an hour has gone.
const LIFETIME: SignedDuration = SignedDuration::from_mins(30);

/// The most sessions in flight at once. Four windows each linking a bank is
/// already more than anybody does; more is something looping.
const MAX_SESSIONS: usize = 4;

/// A session id's randomness.
const SESSION_BYTES: usize = 32;

/// The longest a link's name may be (`Vault::add_bank_link` refuses more).
const MAX_NAME_CHARS: usize = 200;

/// What `money_key` hashes for Plaid's `client_user_id`.
const PLAID_USER: &str = "plaid-user";

const CLOSED: &str = "The bank's sign-in was closed before it finished.";
const NOTHING_LINKED: &str = "Plaid finished without linking a bank.";
const KEYS_CHANGED: &str =
    "The Plaid keys changed to another environment while the bank was being linked. Start again.";

/// One Hosted Link session in flight.
///
/// Not `Debug`: it holds a link token.
pub struct Session {
    pub(crate) link_token: String,
    /// `Some` in update mode.
    pub(crate) relink: Option<BankLinkId>,
    pub(crate) started: Timestamp,
    /// Which Plaid made the link token, and so which one is asked about it
    /// and which one the new link belongs to.
    pub(crate) environment: PlaidEnvironment,
}

impl Session {
    fn expired(&self, now: Timestamp) -> bool {
        self.started + LIFETIME <= now
    }
}

/// What `start_bank_link` answers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStart {
    /// Opaque; hand it back to `check_bank_link`.
    pub session: String,
    /// Plaid's Hosted Link page. The interface opens it in the browser.
    pub url: String,
}

/// What `check_bank_link` answers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LinkCheck {
    /// Still signing in.
    Waiting,
    /// Linked (or relinked). The interface reloads.
    #[serde(rename_all = "camelCase")]
    Done { link_id: BankLinkId },
    /// The person closed it, or Plaid refused.
    Failed { message: String },
    /// No such session, or it is over thirty minutes old.
    Expired,
}

pub async fn start(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    relink: Option<BankLinkId>,
) -> CommandResult<LinkStart> {
    start_at(svc, vault, &Host::Plaid, relink).await
}

pub async fn check(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    session: &str,
) -> CommandResult<LinkCheck> {
    check_at(svc, vault, &Host::Plaid, session).await
}

/// Forget a session. A no-op for one that is not there.
pub fn cancel(svc: &Service, session: &str) {
    lock_sessions(svc).remove(session);
}

/// [`start`], against `host`.
pub(crate) async fn start_at(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    host: &Host,
    relink: Option<BankLinkId>,
) -> CommandResult<LinkStart> {
    // Asked now rather than when the link is stored: a person should not
    // sign in to their bank only to be told at the end it cannot be kept.
    if !vault.is_writable() {
        return Err(CommandError::new(
            codes::VAULT_IN_USE,
            "Another copy of Karma Yoga is writing to this vault, so a bank cannot be linked from here.",
        ));
    }
    // Before Plaid is asked for anything, and again when the session is
    // stored, since two starts can be in flight together.
    if live_sessions(svc) >= MAX_SESSIONS {
        return Err(too_many());
    }
    let (creds, token, user_id) = blocking({
        let vault = vault.clone();
        move || {
            let creds = vault.plaid_access()?;
            let token = match relink {
                Some(id) => {
                    let link = vault.bank_link(id)?;
                    if let Some(problem) = environment_problem(&link, &creds) {
                        return Err(CommandError::new(codes::INVALID, problem));
                    }
                    Some(vault.bank_link_access(id)?)
                }
                None => None,
            };
            Ok((creds, token, vault.money_key(PLAID_USER)?))
        }
    })
    .await?;
    let environment = creds.environment;
    let made = Plaid::at(host, creds)
        .create_link_token(&user_id, token.as_ref())
        .await
        .map_err(plaid::command_error)?;
    let session = session_id();
    {
        let mut sessions = lock_sessions(svc);
        // Plaid took its time answering, and the vault may have locked while
        // it did -- which cleared every session. A link token stored after
        // that would outlive the lock it was meant to go with.
        if !vault.is_unlocked() {
            return Err(CommandError::new(codes::LOCKED, "the vault locked while Plaid answered"));
        }
        let now = svc.now();
        sessions.retain(|_, s| !s.expired(now));
        if sessions.len() >= MAX_SESSIONS {
            return Err(too_many());
        }
        sessions.insert(
            session.clone(),
            Session { link_token: made.link_token, relink, started: now, environment },
        );
    }
    Ok(LinkStart { session, url: made.hosted_link_url })
}

/// [`check`], against `host`.
///
/// A failure that may pass -- Plaid unreachable, rate-limited, down for
/// maintenance -- answers an error and keeps the session, so the next poll
/// can carry on rather than making the person sign in to their bank again;
/// Plaid refusing the session itself ends it.
pub(crate) async fn check_at(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    host: &Host,
    session: &str,
) -> CommandResult<LinkCheck> {
    let now = svc.now();
    let (link_token, relink, environment) = {
        let mut sessions = lock_sessions(svc);
        let found = sessions
            .get(session)
            .map(|s| (s.expired(now), s.link_token.clone(), s.relink, s.environment));
        match found {
            None => return Ok(LinkCheck::Expired),
            Some((true, ..)) => {
                sessions.remove(session);
                return Ok(LinkCheck::Expired);
            }
            Some((false, link_token, relink, environment)) => (link_token, relink, environment),
        }
    };
    let creds = blocking({
        let vault = vault.clone();
        move || Ok(vault.plaid_access()?)
    })
    .await?;
    if creds.environment != environment {
        lock_sessions(svc).remove(session);
        return Ok(LinkCheck::Failed { message: KEYS_CHANGED.into() });
    }
    let plaid = Plaid::at(host, creds);
    let progress = match plaid.link_progress(&link_token, relink.is_some()).await {
        Ok(progress) => progress,
        Err(e) if e.transient() => return Err(plaid::command_error(e)),
        Err(e) => {
            lock_sessions(svc).remove(session);
            return Ok(LinkCheck::Failed { message: e.message });
        }
    };
    if matches!(progress, LinkProgress::Waiting) {
        return Ok(LinkCheck::Waiting);
    }
    // Finished. Whichever check takes it out of the map finishes it; any
    // other is too late, as it would be had it asked a moment after.
    if lock_sessions(svc).remove(session).is_none() {
        return Ok(LinkCheck::Expired);
    }
    match (progress, relink) {
        (LinkProgress::Exited(words), _) => {
            Ok(LinkCheck::Failed { message: words.unwrap_or_else(|| CLOSED.into()) })
        }
        (_, Some(id)) => relinked(svc, vault, host, id).await,
        (LinkProgress::Added(items), None) => {
            added(svc, vault, host, &plaid, environment, items).await
        }
        (LinkProgress::Updated | LinkProgress::Waiting, None) => {
            Ok(LinkCheck::Failed { message: NOTHING_LINKED.into() })
        }
    }
}

/// Exchange each item's public token, store it as a link, and sync it once.
async fn added(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    host: &Host,
    plaid: &Plaid,
    environment: PlaidEnvironment,
    items: Vec<ItemAdded>,
) -> CommandResult<LinkCheck> {
    let mut first: Option<BankLinkId> = None;
    let mut problem: Option<String> = None;
    for item in items {
        let (token, item_id) = match plaid.exchange(&item.public_token).await {
            Ok(exchanged) => exchanged,
            Err(e) => {
                if problem.is_none() {
                    problem = Some(e.message);
                }
                continue;
            }
        };
        let provider = LinkProvider::Plaid {
            item_id,
            institution_id: item.institution_id.clone(),
            environment,
        };
        let link = BankLink::new(provider, bank_name(&item.institution_name));
        let id = link.id;
        let stored = blocking({
            let vault = vault.clone();
            let token = token.clone();
            move || Ok(vault.add_bank_link(&link, &token)?)
        })
        .await;
        match stored {
            Ok(()) => {
                if first.is_none() {
                    first = Some(id);
                }
                // Best effort: a first sync that fails leaves the link, and
                // its state says why.
                let _ = refresh::sync_one(svc, vault, host, id, false).await;
            }
            Err(e) => {
                if let Err(gone) = plaid.remove_item(&token).await {
                    tracing::warn!(
                        code = %gone.code,
                        "could not hand back a bank that was linked but could not be stored"
                    );
                }
                if problem.is_none() {
                    problem = Some(e.message);
                }
            }
        }
    }
    Ok(match first {
        Some(link_id) => LinkCheck::Done { link_id },
        None => LinkCheck::Failed { message: problem.unwrap_or_else(|| NOTHING_LINKED.into()) },
    })
}

/// Update mode finished: the bank has its sign-in back.
async fn relinked(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    host: &Host,
    id: BankLinkId,
) -> CommandResult<LinkCheck> {
    {
        // Under the link's turn, so a sync in flight cannot save the old
        // `needs_login` back over this.
        let turn = svc.money.sync_turn(id);
        let _held = turn.lock().await;
        let now = svc.now();
        blocking({
            let vault = vault.clone();
            move || {
                // Refused, rather than a state written for nothing, if the
                // bank was disconnected while the person signed in.
                vault.bank_link(id)?;
                let mut state = vault.bank_link_state(id)?.unwrap_or_else(|| LinkState::new(id));
                state.needs_login = false;
                state.last_error = None;
                state.failures = 0;
                state.next_at = now;
                vault.save_bank_link_state(&state)?;
                Ok(())
            }
        })
        .await?;
    }
    svc.money.recovered(id);
    let _ = refresh::sync_one(svc, vault, host, id, false).await;
    Ok(LinkCheck::Done { link_id: id })
}

/// The sessions, whatever a panic elsewhere left the lock in. Never held
/// across an await.
fn lock_sessions(svc: &Service) -> MutexGuard<'_, HashMap<String, Session>> {
    svc.money.sessions.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How many sessions are alive, dropping the ones that are not.
fn live_sessions(svc: &Service) -> usize {
    let now = svc.now();
    let mut sessions = lock_sessions(svc);
    sessions.retain(|_, s| !s.expired(now));
    sessions.len()
}

fn too_many() -> CommandError {
    CommandError::new(
        codes::BUSY,
        "Four banks are already being linked. Finish or close one of those first.",
    )
}

/// 32 random bytes from the thread's CSPRNG, as hex: unguessable, and
/// meaningless to anybody who sees it.
fn session_id() -> String {
    let mut bytes = [0u8; SESSION_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(SESSION_BYTES * 2), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// What a new link is called: the institution's name as Plaid gave it,
/// without control characters, within the length a link's name may be.
fn bank_name(institution: &str) -> String {
    let clean: String = institution.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return "Bank".to_string();
    }
    clean.chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::plaid::fake::{self, creds, ok, page, refuse, txn};
    use karmayoga_core::money::AccessToken;
    use serde_json::{Value, json};
    use std::collections::HashSet;

    const LINK_TOKEN: &str = "link-sandbox-af1a0311-da53-4636";

    fn created() -> Value {
        json!({
            "link_token": LINK_TOKEN,
            "expiration": "2026-11-05T14:00:00Z",
            "hosted_link_url": "https://secure.plaid.com/hl/af1a0311",
            "request_id": "r"
        })
    }

    fn finished_with_an_item() -> Value {
        json!({ "link_token": LINK_TOKEN, "link_sessions": [{
            "link_session_id": "s1",
            "started_at": "2026-11-05T10:00:00Z",
            "finished_at": "2026-11-05T10:02:00Z",
            "exit": null,
            "results": { "item_add_results": [{
                "public_token": "public-sandbox-5c224a01",
                "institution": { "name": "First Platypus Bank", "institution_id": "ins_109508" },
                "accounts": []
            }] }
        }] })
    }

    /// A Plaid that links, exchanges and syncs.
    async fn obliging() -> fake::Fake {
        fake::serve(|path, _| match path {
            "/link/token/create" => ok(created()),
            "/link/token/get" => ok(finished_with_an_item()),
            "/item/public_token/exchange" => {
                ok(json!({ "access_token": "access-sandbox-de3ce8ef", "item_id": "item-1" }))
            }
            "/transactions/sync" => ok(page(vec![txn("t1", 4.5)], vec![], &[], "c1", false)),
            "/item/remove" => ok(json!({ "request_id": "r" })),
            _ => refuse(404, "NOT_FOUND", "no such route"),
        })
        .await
    }

    fn insert(svc: &Service, id: &str, started: Timestamp, relink: Option<BankLinkId>) {
        lock_sessions(svc).insert(
            id.to_string(),
            Session {
                link_token: LINK_TOKEN.into(),
                relink,
                started,
                environment: PlaidEnvironment::Sandbox,
            },
        );
    }

    fn has_session(svc: &Service, id: &str) -> bool {
        lock_sessions(svc).contains_key(id)
    }

    fn plaid_link(name: &str, environment: PlaidEnvironment) -> BankLink {
        let provider = LinkProvider::Plaid {
            item_id: "item-9".into(),
            institution_id: String::new(),
            environment,
        };
        BankLink::new(provider, name)
    }

    #[test]
    fn a_session_id_is_sixty_four_hex_characters_and_never_repeats() {
        let ids: HashSet<String> = (0..1000).map(|_| session_id()).collect();
        assert_eq!(ids.len(), 1000);
        assert!(ids.iter().all(|id| id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit())));
    }

    #[test]
    fn a_banks_name_is_kept_within_bounds() {
        assert_eq!(bank_name("  First Platypus Bank\n"), "First Platypus Bank");
        assert_eq!(bank_name("\u{7}  "), "Bank");
        assert_eq!(bank_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }

    #[tokio::test]
    async fn start_hands_the_interface_an_opaque_session_and_plaids_page_only() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = obliging().await;
        let started = start_at(&svc, &vault, &plaid.host(), None).await.unwrap();

        assert_eq!(started.session.len(), 64);
        assert_eq!(started.url, "https://secure.plaid.com/hl/af1a0311");
        let answered = serde_json::to_string(&started).unwrap();
        assert!(!answered.contains(LINK_TOKEN), "{answered}");
        assert_eq!(
            lock_sessions(&svc).get(&started.session).map(|s| s.link_token.clone()),
            Some(LINK_TOKEN.to_string())
        );
        let body = &plaid.seen()[0].body;
        assert_eq!(body["user"]["client_user_id"], json!(vault.money_key(PLAID_USER).unwrap()));
        assert!(body.get("access_token").is_none());
    }

    #[tokio::test]
    async fn a_fifth_sign_in_at_once_is_refused_without_asking_plaid() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = obliging().await;
        for _ in 0..MAX_SESSIONS {
            start_at(&svc, &vault, &plaid.host(), None).await.unwrap();
        }
        let fifth = start_at(&svc, &vault, &plaid.host(), None).await.unwrap_err();
        assert_eq!(fifth.code, codes::BUSY);
        assert_eq!(plaid.hits("/link/token/create"), MAX_SESSIONS);

        // An expired one no longer counts.
        let oldest = lock_sessions(&svc).keys().next().cloned().unwrap();
        lock_sessions(&svc).get_mut(&oldest).unwrap().started =
            svc.now() - SignedDuration::from_mins(31);
        start_at(&svc, &vault, &plaid.host(), None).await.unwrap();
        assert!(!has_session(&svc, &oldest));
    }

    #[tokio::test]
    async fn a_page_that_is_not_plaids_is_never_handed_to_the_interface() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = fake::serve(|_, _| {
            ok(json!({ "link_token": LINK_TOKEN, "hosted_link_url": "https://evil.example/hl/af1a0311" }))
        })
        .await;
        let err = start_at(&svc, &vault, &plaid.host(), None).await.unwrap_err();
        assert!(!err.message.contains("evil.example"), "{err}");
        assert!(lock_sessions(&svc).is_empty(), "nothing is kept for a refused page");
    }

    #[tokio::test]
    async fn starting_without_keys_says_so_and_asks_nobody() {
        let (svc, vault, _dir) = fake::test_vault();
        let plaid = obliging().await;
        let err = start_at(&svc, &vault, &plaid.host(), None).await.unwrap_err();
        assert!(err.message.contains("Settings"), "{err}");
        assert_eq!(plaid.total(), 0);
    }

    #[tokio::test]
    async fn a_session_over_thirty_minutes_old_is_expired_without_asking_plaid() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = obliging().await;
        let id = session_id();
        insert(&svc, &id, svc.now() - SignedDuration::from_mins(31), None);
        let checked = check_at(&svc, &vault, &plaid.host(), &id).await.unwrap();
        assert!(matches!(checked, LinkCheck::Expired));
        assert!(!has_session(&svc, &id));
        assert_eq!(plaid.total(), 0);
    }

    #[tokio::test]
    async fn a_session_is_found_only_by_its_exact_id() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = fake::serve(|_, _| {
            ok(json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z", "finished_at": null }] }))
        })
        .await;
        let id = session_id();
        insert(&svc, &id, svc.now(), None);
        for near in [id.to_ascii_uppercase(), id[..63].to_string(), format!("{id} "), String::new()]
        {
            let checked = check_at(&svc, &vault, &plaid.host(), &near).await.unwrap();
            assert!(matches!(checked, LinkCheck::Expired), "{near:?} must not find {id}");
        }
        assert_eq!(plaid.total(), 0);
        let checked = check_at(&svc, &vault, &plaid.host(), &id).await.unwrap();
        assert!(matches!(checked, LinkCheck::Waiting));
        assert!(has_session(&svc, &id), "a session still signing in stays");
        assert_eq!(plaid.seen()[0].body["link_token"], LINK_TOKEN);
    }

    #[tokio::test]
    async fn a_finished_session_is_exchanged_exactly_once() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = obliging().await;
        let host = plaid.host();
        let id = session_id();
        insert(&svc, &id, svc.now(), None);

        let (one, other) =
            tokio::join!(check_at(&svc, &vault, &host, &id), check_at(&svc, &vault, &host, &id));
        let answers = [one.unwrap(), other.unwrap()];
        assert_eq!(answers.iter().filter(|a| matches!(a, LinkCheck::Done { .. })).count(), 1);
        assert_eq!(answers.iter().filter(|a| matches!(a, LinkCheck::Expired)).count(), 1);
        assert_eq!(plaid.hits("/item/public_token/exchange"), 1);
        assert!(!has_session(&svc, &id));

        let links = vault.bank_links().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].name, "First Platypus Bank");
        assert_eq!(
            links[0].provider,
            LinkProvider::Plaid {
                item_id: "item-1".into(),
                institution_id: "ins_109508".into(),
                environment: PlaidEnvironment::Sandbox,
            }
        );
        assert_eq!(
            vault.bank_link_access(links[0].id).unwrap().expose(),
            "access-sandbox-de3ce8ef"
        );
        assert_eq!(plaid.hits("/transactions/sync"), 1, "the new link was synced at once");
        assert_eq!(vault.bank_link_state(links[0].id).unwrap().unwrap().cursor, "c1");
    }

    #[tokio::test]
    async fn the_person_closing_the_page_fails_the_session_and_ends_it() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let plaid = fake::serve(|_, _| {
            ok(json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
                "finished_at": "2026-11-05T10:01:00Z", "exit": { "error": null }, "results": null }] }))
        })
        .await;
        let id = session_id();
        insert(&svc, &id, svc.now(), None);
        match check_at(&svc, &vault, &plaid.host(), &id).await.unwrap() {
            LinkCheck::Failed { message } => assert_eq!(message, CLOSED),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!has_session(&svc, &id));
        assert_eq!(plaid.hits("/item/public_token/exchange"), 0);
    }

    #[tokio::test]
    async fn plaid_refusing_the_session_ends_it_and_an_unreachable_plaid_keeps_it() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let refusing =
            fake::serve(|_, _| refuse(400, "INVALID_LINK_TOKEN", "invalid link_token provided"))
                .await;
        let id = session_id();
        insert(&svc, &id, svc.now(), None);
        assert!(matches!(
            check_at(&svc, &vault, &refusing.host(), &id).await.unwrap(),
            LinkCheck::Failed { .. }
        ));
        assert!(!has_session(&svc, &id));

        let id = session_id();
        insert(&svc, &id, svc.now(), None);
        let nowhere = Host::Fake("http://127.0.0.1:1".into());
        assert!(check_at(&svc, &vault, &nowhere, &id).await.is_err());
        assert!(has_session(&svc, &id), "a poll that could not get through keeps the session");

        let limited = fake::serve(|_, _| refuse(429, "RATE_LIMIT", "rate limit exceeded")).await;
        assert!(check_at(&svc, &vault, &limited.host(), &id).await.is_err());
        assert!(has_session(&svc, &id), "a rate-limited poll keeps the session");
    }

    #[tokio::test]
    async fn cancel_forgets_a_session() {
        let svc = Arc::new(Service::new());
        let id = session_id();
        insert(&svc, &id, svc.now(), None);
        cancel(&svc, "not-it");
        assert!(has_session(&svc, &id));
        cancel(&svc, &id);
        assert!(!has_session(&svc, &id));
    }

    #[tokio::test]
    async fn relinking_a_bank_from_the_other_environment_is_refused_without_asking_plaid() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let link = plaid_link("Production Bank", PlaidEnvironment::Production);
        vault.add_bank_link(&link, &AccessToken("access-production-1".into())).unwrap();
        let plaid = obliging().await;
        let err = start_at(&svc, &vault, &plaid.host(), Some(link.id)).await.unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        assert!(err.message.contains("linked with production keys"), "{err}");
        assert_eq!(plaid.total(), 0);
    }

    #[tokio::test]
    async fn relinking_sends_the_links_own_token_and_clears_its_sign_in_flag() {
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let link = plaid_link("First Platypus Bank", PlaidEnvironment::Sandbox);
        vault.add_bank_link(&link, &AccessToken("access-sandbox-old".into())).unwrap();
        let mut state = LinkState::new(link.id);
        state.needs_login = true;
        state.last_error = Some("Sign in again".into());
        state.next_at = svc.now() + SignedDuration::from_hours(24);
        vault.save_bank_link_state(&state).unwrap();

        let plaid = fake::serve(|path, _| match path {
            "/link/token/create" => ok(created()),
            "/link/token/get" => {
                ok(json!({ "link_sessions": [{ "started_at": "2026-11-05T10:00:00Z",
                "finished_at": "2026-11-05T10:01:00Z", "exit": null, "results": null }] }))
            }
            "/transactions/sync" => ok(page(vec![], vec![], &[], "c1", false)),
            _ => refuse(404, "NOT_FOUND", "no such route"),
        })
        .await;
        let started = start_at(&svc, &vault, &plaid.host(), Some(link.id)).await.unwrap();
        let body = &plaid.seen()[0].body;
        assert_eq!(body["access_token"], "access-sandbox-old");
        assert!(body.get("products").is_none());

        match check_at(&svc, &vault, &plaid.host(), &started.session).await.unwrap() {
            LinkCheck::Done { link_id } => assert_eq!(link_id, link.id),
            other => panic!("expected Done, got {other:?}"),
        }
        let state = vault.bank_link_state(link.id).unwrap().unwrap();
        assert!(!state.needs_login && state.last_error.is_none());
        assert_eq!(plaid.hits("/item/public_token/exchange"), 0, "update mode stores nothing new");
        assert_eq!(plaid.hits("/transactions/sync"), 1);
        assert_eq!(vault.bank_links().unwrap().len(), 1);
    }
}
