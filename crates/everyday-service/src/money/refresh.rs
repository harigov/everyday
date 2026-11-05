//! Keeping linked banks current: one link's sync, and the supervised loop.
//!
//! # A sync
//!
//! [`sync_link`] is one link's turn, whether the loop or a person asked:
//!
//! - **One at a time per link**: it holds `svc.money.sync_turn(id)` for the
//!   whole sync, and reads the link's state only once it is held, so "Sync
//!   now" and the refresher never race a cursor.
//! - It skips a paused link, a link whose state says `needs_login`, and a
//!   link whose environment differs from the credentials' -- that one is
//!   told so in `last_error`, and Plaid is never called with it, because a
//!   sandbox token sent to production (or the reverse) is a credential sent
//!   somewhere it does not belong.
//! - It pages `/transactions/sync` from the stored cursor until `has_more`
//!   is false, accumulating; on `TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION`
//!   it starts again from the stored cursor (at most three times). The
//!   whole accumulation goes to `Vault::apply_bank_sync` (added and modified
//!   as upserts, the last word on each transaction winning; removed), and
//!   *then* the new cursor is saved -- so a crash in between re-applies an
//!   idempotent delta rather than skipping one.
//! - Next due: six hours after a success -- or ten minutes, for a link less
//!   than a day old whose history Plaid has not finished sending, so a bank
//!   linked a moment ago shows its transactions within minutes rather than
//!   after its first six hours. After a failure, a backoff of 15 minutes ×
//!   2^failures, capped at a day. An error that means the person must sign
//!   in again sets `needs_login` and stops the link until they have.
//! - A failure is said once per outage (`svc.money.failed`/`recovered`) with
//!   a notification that names the bank -- the person's name for it -- and
//!   Plaid's words, never a token.
//!
//! Every read and write of the vault goes through [`blocking`], so nothing
//! here holds the vault's write lock across an await. The reads and writes
//! are behind `Ledger`, which the vault implements; the tests implement it
//! too, which is how "the cursor is stored only after the delta is" is
//! checked without a database that can be made to fail on cue.
//!
//! # The loop
//!
//! [`run`] syncs every due link (`Vault::due_bank_links`), raises
//! `Change::new(Kind::Txn, Op::Updated)` if any sync changed anything, and
//! sleeps until the next one is due -- at least a minute, at most half an
//! hour, so a link added from another window is not long unnoticed --
//! racing `stop` as `calendar_refresh::run` does. A locked vault waits.
//! Without Plaid credentials nothing is synced and nothing is said: a vault
//! that has never had keys is not failing, and one whose keys were removed
//! was emptied of them on purpose.

use super::SyncReport;
use super::plaid::{Host, MUTATION_DURING_PAGINATION, Plaid, PlaidError};
use crate::error::{CommandResult, codes};
use crate::events::{Change, Kind, Notification, Op};
use crate::service::{Service, blocking};
use crate::supervisor::{Outcome, TaskResult};
use everyday_core::Vault;
use everyday_core::id::BankLinkId;
use everyday_core::money::{
    AccessToken, BankLink, LinkProvider, LinkState, PlaidCredentials, SyncDelta, SyncOutcome,
    SyncedAccount, SyncedTxn,
};
use jiff::{SignedDuration, Timestamp};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How long after a success a link is due again. Plaid itself refreshes a
/// bank's transactions a few times a day; asking oftener finds nothing.
const SUCCESS_INTERVAL: SignedDuration = SignedDuration::from_hours(6);

/// How soon a young link whose history is still arriving is asked again.
const FILLING_INTERVAL: SignedDuration = SignedDuration::from_mins(10);

/// How long a link counts as young, for [`FILLING_INTERVAL`]. A link whose
/// history never finishes arriving falls back to [`SUCCESS_INTERVAL`] after
/// this, rather than being asked every ten minutes for ever.
const FILLING_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// The first wait after a failure, doubled for each one after it.
const BACKOFF_FIRST_MINUTES: i64 = 15;

/// The longest wait after failures.
const BACKOFF_MOST_MINUTES: i64 = 24 * 60;

/// When the loop looks again at a link that wants its sign-in, in case it
/// was relinked from somewhere that did not clear the flag. Relinking here
/// makes it due at once.
const LOGIN_RECHECK: SignedDuration = SignedDuration::from_hours(24);

/// When the loop looks again at a link from the other environment, in case
/// the person switched their keys back.
const MISMATCH_RECHECK: SignedDuration = SignedDuration::from_hours(1);

/// How many times one sync starts its paging again after Plaid says the data
/// changed under it.
const MAX_RESTARTS: u32 = 3;

/// The most pages one sync reads: 200,000 transactions, far beyond any
/// person's two years, and a stop for a server that says "more" for ever.
const MAX_PAGES: usize = 400;

/// The loop's longest sleep: a link added from another window, or keys set
/// for the first time, are noticed within this.
const LONGEST_SLEEP: Duration = Duration::from_secs(30 * 60);

/// The loop's shortest sleep, so a link due a moment from now is not asked
/// for in a hurry.
const SHORTEST_SLEEP: Duration = Duration::from_secs(60);

/// What a sync says when there are no keys to sync with.
const NO_KEYS: &str = "No Plaid keys are set; add them in Settings → Money.";

/// Everything a sync reads from the vault and writes to it, as one seam.
///
/// The vault is the only implementation outside the tests. Every method is
/// called on the blocking pool, through [`blocking`], never on the async
/// side. `credentials` and `token` are the vault's two doors (see
/// `Vault::plaid_access` and `Vault::bank_link_access`), opened only when a
/// request to Plaid is about to be made.
pub(crate) trait Ledger: Send + Sync + 'static {
    /// May this process write? A second copy of the application that holds
    /// no write claim does not sync: fetching to throw the answer away is not
    /// a useful thing to do on a timer.
    fn writable(&self) -> bool;
    fn link(&self, id: BankLinkId) -> everyday_core::Result<BankLink>;
    fn state(&self, id: BankLinkId) -> everyday_core::Result<Option<LinkState>>;
    /// Are keys set? Asked without opening the door.
    fn has_credentials(&self) -> everyday_core::Result<bool>;
    /// The keys, or `None` when none are set.
    fn credentials(&self) -> everyday_core::Result<Option<PlaidCredentials>>;
    fn token(&self, id: BankLinkId) -> everyday_core::Result<AccessToken>;
    fn apply(&self, id: BankLinkId, delta: &SyncDelta) -> everyday_core::Result<SyncOutcome>;
    fn save_state(&self, state: &LinkState) -> everyday_core::Result<()>;
    /// Links whose state is due by `now`, or that have none.
    fn due(&self, now: Timestamp) -> everyday_core::Result<Vec<BankLinkId>>;
    /// Every link that is not paused.
    fn active(&self) -> everyday_core::Result<Vec<BankLinkId>>;
    fn next_due(&self) -> everyday_core::Result<Option<Timestamp>>;
}

impl Ledger for Vault {
    fn writable(&self) -> bool {
        self.is_writable()
    }

    fn link(&self, id: BankLinkId) -> everyday_core::Result<BankLink> {
        self.bank_link(id)
    }

    fn state(&self, id: BankLinkId) -> everyday_core::Result<Option<LinkState>> {
        self.bank_link_state(id)
    }

    fn has_credentials(&self) -> everyday_core::Result<bool> {
        Ok(self.plaid_setup()?.configured)
    }

    fn credentials(&self) -> everyday_core::Result<Option<PlaidCredentials>> {
        // `plaid_access` cannot tell "none set" from "set and unreadable"
        // by its error; `plaid_setup` can, and hands nothing secret back.
        if !self.plaid_setup()?.configured {
            return Ok(None);
        }
        self.plaid_access().map(Some)
    }

    fn token(&self, id: BankLinkId) -> everyday_core::Result<AccessToken> {
        self.bank_link_access(id)
    }

    fn apply(&self, id: BankLinkId, delta: &SyncDelta) -> everyday_core::Result<SyncOutcome> {
        self.apply_bank_sync(id, delta)
    }

    fn save_state(&self, state: &LinkState) -> everyday_core::Result<()> {
        self.save_bank_link_state(state)
    }

    fn due(&self, now: Timestamp) -> everyday_core::Result<Vec<BankLinkId>> {
        self.due_bank_links(now)
    }

    fn active(&self) -> everyday_core::Result<Vec<BankLinkId>> {
        Ok(self.bank_links()?.into_iter().filter(|link| !link.paused).map(|link| link.id).collect())
    }

    fn next_due(&self) -> everyday_core::Result<Option<Timestamp>> {
        self.next_bank_link_due()
    }
}

/// What one link's turn did: the report, and whether the link's health --
/// its error, or whether it wants a sign-in -- moved, which the sidebar
/// draws and so is worth a reload even when no transaction changed.
pub(crate) struct Turn {
    pub(crate) report: SyncReport,
    pub(crate) health_changed: bool,
}

impl Turn {
    fn quiet(report: SyncReport) -> Self {
        Self { report, health_changed: false }
    }
}

/// A link's health before a turn, to compare with after.
struct Health {
    error: Option<String>,
    needs_login: bool,
}

impl Health {
    fn of(state: &LinkState) -> Self {
        Self { error: state.last_error.clone(), needs_login: state.needs_login }
    }

    fn changed(&self, state: &LinkState) -> bool {
        self.error != state.last_error || self.needs_login != state.needs_login
    }
}

// ── The loop ─────────────────────────────────────────────────────────────

/// The supervised loop. Returns when `stop` turns `true`.
pub async fn run(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    mut stop: watch::Receiver<bool>,
) -> TaskResult {
    let host = Host::Plaid;
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        // Raced with the signal rather than left to finish: a bank that has
        // stopped answering must not hold up a lock. Whatever a sync stores
        // goes through the blocking pool, which sees its write through
        // whether or not anyone is still waiting for it, and a cursor that
        // was not saved only means the same delta is applied again.
        let sleep = tokio::select! {
            pass = pass(&svc, &vault, &host) => match pass {
                Ok(sleep) => sleep,
                // Locked between one turn and the next: nothing to read and
                // nothing to write. The supervisor stops this task on the
                // lock in any case; until it does, wait.
                Err(e) if e.code == codes::LOCKED => LONGEST_SLEEP,
                Err(e) => return Err(e.into()),
            },
            changed = stop.changed() => {
                if changed.is_err() {
                    return Ok(Outcome::Done);
                }
                continue;
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            changed = stop.changed() => {
                // The sender is gone: nobody is left to ask this to stop, or
                // to want what it does.
                if changed.is_err() {
                    return Ok(Outcome::Done);
                }
            }
        }
    }
}

/// One turn of [`run`]: every due link, one change if anything moved, and how
/// long to sleep.
async fn pass<L: Ledger>(
    svc: &Arc<Service>,
    ledger: &Arc<L>,
    host: &Host,
) -> CommandResult<Duration> {
    if !ledger.writable() {
        return Ok(LONGEST_SLEEP);
    }
    let turns = sync_many(svc, ledger, host, false, true).await?;
    if turns.iter().any(|turn| turn.report.changed() || turn.health_changed) {
        svc.events().changed(Change::new(Kind::Txn, Op::Updated));
    }
    let next = blocking({
        let ledger = ledger.clone();
        move || Ok(ledger.next_due()?)
    })
    .await?;
    Ok(sleep_for(next, svc.now()))
}

/// How long to sleep for a link due at `next`.
///
/// A link already due after a pass that has just done all it could is one
/// with no keys to sync it, or one whose state could not be saved; neither
/// is helped by asking again in a minute, and the second would be asking
/// Plaid again every minute. So that is the longest sleep, not the
/// shortest.
fn sleep_for(next: Option<Timestamp>, now: Timestamp) -> Duration {
    let Some(next) = next else { return LONGEST_SLEEP };
    let millis = next.as_millisecond() - now.as_millisecond();
    if millis <= 0 {
        return LONGEST_SLEEP;
    }
    Duration::from_millis(millis as u64).clamp(SHORTEST_SLEEP, LONGEST_SLEEP)
}

// ── Asking ───────────────────────────────────────────────────────────────

/// Sync one link now. Never fails: a failure is in the report (and the
/// link's state).
pub async fn sync_link(svc: &Arc<Service>, vault: &Arc<Vault>, id: BankLinkId) -> SyncReport {
    sync_one(svc, vault, &Host::Plaid, id, false).await.report
}

/// Sync every link that is due (or every link that is not paused, with
/// `all` -- "Sync now"), in turn.
///
/// Without keys, a due pass is quietly empty. Asked for `all`, each link's
/// report says there are no keys, since a person pressed something and is
/// owed an answer.
pub async fn sync_due(svc: &Arc<Service>, vault: &Arc<Vault>, all: bool) -> Vec<SyncReport> {
    match sync_many(svc, vault, &Host::Plaid, all, false).await {
        Ok(turns) => turns.into_iter().map(|turn| turn.report).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not list the banks to sync");
            Vec::new()
        }
    }
}

/// [`sync_due`], for any ledger and host, keeping each turn's health, and
/// failing when the links could not even be listed (a locked vault, which
/// the loop waits out).
pub(crate) async fn sync_many<L: Ledger>(
    svc: &Arc<Service>,
    ledger: &Arc<L>,
    host: &Host,
    all: bool,
    background: bool,
) -> CommandResult<Vec<Turn>> {
    let now = svc.now();
    let ids = blocking({
        let ledger = ledger.clone();
        move || {
            if all {
                return Ok(ledger.active()?);
            }
            if !ledger.has_credentials()? {
                return Ok(Vec::new());
            }
            Ok(ledger.due(now)?)
        }
    })
    .await?;
    let mut turns = Vec::with_capacity(ids.len());
    // One after another: a person has at most a handful of banks, and Plaid
    // is kinder to a client that does not ask about all of them at once.
    for id in ids {
        turns.push(sync_one(svc, ledger, host, id, background).await);
    }
    Ok(turns)
}

// ── One link ─────────────────────────────────────────────────────────────

/// One link's sync, against `host`, recording what it learned in the link's
/// state. `background` is true when nobody asked -- the loop's own turn --
/// which is when a failure is worth reaching a person who is not looking at
/// Money, and when a skipped link is pushed back so the loop does not wake
/// for it every minute.
pub(crate) async fn sync_one<L: Ledger>(
    svc: &Arc<Service>,
    ledger: &Arc<L>,
    host: &Host,
    id: BankLinkId,
    background: bool,
) -> Turn {
    let turn = svc.money.sync_turn(id);
    let _held = turn.lock().await;

    // Read only now the turn is held: a state read before it could be one
    // the sync ahead in the queue is about to replace.
    let loaded = blocking({
        let ledger = ledger.clone();
        move || {
            let link = ledger.link(id)?;
            let state = ledger.state(id)?.unwrap_or_else(|| LinkState::new(id));
            let creds = ledger.credentials()?;
            Ok((link, state, creds))
        }
    })
    .await;
    let (link, mut state, creds) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => return Turn::quiet(SyncReport::failed(id, e.message)),
    };
    let before = Health::of(&state);
    let now = svc.now();

    if link.paused {
        if background {
            state.next_at = now + SUCCESS_INTERVAL;
            save(ledger, &state).await;
        }
        return Turn::quiet(SyncReport::failed(id, format!("Syncing {} is paused.", link.name)));
    }
    if state.needs_login {
        if background {
            state.next_at = now + LOGIN_RECHECK;
            save(ledger, &state).await;
        }
        let message = state.last_error.clone().unwrap_or_else(|| sign_in_again(&link));
        return Turn::quiet(SyncReport { needs_login: true, ..SyncReport::failed(id, message) });
    }
    let Some(creds) = creds else {
        return Turn::quiet(SyncReport::failed(id, NO_KEYS));
    };
    if let Some(problem) = environment_problem(&link, &creds) {
        // Not a failure of the bank's, so no backoff and no notice: the
        // person changed their keys, and the sidebar says what to do.
        state.last_error = Some(problem.clone());
        state.next_at = now + MISMATCH_RECHECK;
        save(ledger, &state).await;
        return Turn {
            report: SyncReport::failed(id, problem),
            health_changed: before.changed(&state),
        };
    }

    let token = blocking({
        let ledger = ledger.clone();
        move || Ok(ledger.token(id)?)
    })
    .await;
    let token = match token {
        Ok(token) => token,
        Err(e) => return failed(svc, ledger, &link, state, before, e.message, background).await,
    };
    let plaid = Plaid::at(host, creds);
    let collected = collect(&plaid, &token, &state.cursor).await;
    let collected = match collected {
        Ok(collected) => collected,
        Err(e) if e.needs_login() => {
            return login_lost(svc, ledger, &link, state, before, e, background).await;
        }
        Err(e) => return failed(svc, ledger, &link, state, before, e.message, background).await,
    };

    let Collected { delta, next_cursor, historical_done } = collected;
    let applied = blocking({
        let ledger = ledger.clone();
        move || Ok(ledger.apply(id, &delta)?)
    })
    .await;
    let outcome = match applied {
        Ok(outcome) => outcome,
        Err(e) => {
            // Disconnected while it synced: there is no state to keep, and
            // saving one would leave a row behind for a link that is gone.
            if e.code == codes::NOT_FOUND && gone(ledger, id).await {
                return Turn::quiet(SyncReport::failed(
                    id,
                    "This bank was disconnected while it synced.",
                ));
            }
            let message = format!("What {} sent could not be stored: {}", link.name, e.message);
            return failed(svc, ledger, &link, state, before, message, background).await;
        }
    };

    // Only now that the delta is stored: a cursor saved ahead of it would
    // skip whatever the delta held if the write had failed.
    let finished = svc.now();
    state.cursor = next_cursor;
    state.historical_done |= historical_done;
    state.last_ok = Some(finished);
    state.last_error = None;
    state.needs_login = false;
    state.failures = 0;
    state.next_at = next_after_success(&link, &state, finished);
    save(ledger, &state).await;
    svc.money.recovered(id);
    Turn {
        report: SyncReport {
            link_id: id,
            added: outcome.added,
            updated: outcome.updated,
            removed: outcome.removed,
            needs_login: false,
            error: None,
        },
        health_changed: before.changed(&state),
    }
}

/// Why `link` must not be sent to Plaid with `creds`, if it must not: it was
/// made in the other environment. A sandbox token is meaningless to
/// production and the reverse, and sending one there is sending a
/// credential somewhere it does not belong.
pub(crate) fn environment_problem(link: &BankLink, creds: &PlaidCredentials) -> Option<String> {
    // Exhaustive on purpose: a second provider must decide here what it
    // means, rather than fall through to Plaid.
    let linked = match &link.provider {
        LinkProvider::Plaid { environment, .. } => *environment,
    };
    (linked != creds.environment).then(|| {
        format!(
            "This bank was linked with {} keys; switch back to them or link it again.",
            linked.as_str()
        )
    })
}

/// What a sync gathered across every page, before any of it is stored.
struct Collected {
    delta: SyncDelta,
    next_cursor: String,
    historical_done: bool,
}

/// Page `/transactions/sync` from `from` until there is no more, starting
/// again from `from` -- discarding what was gathered -- when Plaid says the
/// data changed during paging, at most [`MAX_RESTARTS`] times.
async fn collect(plaid: &Plaid, token: &AccessToken, from: &str) -> Result<Collected, PlaidError> {
    let mut restarts = 0;
    'pass: loop {
        let mut accounts: Vec<SyncedAccount> = Vec::new();
        let mut upserts: Vec<SyncedTxn> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        let mut cursor = from.to_string();
        for _ in 0..MAX_PAGES {
            let page = match plaid.sync_page(token, &cursor).await {
                Ok(page) => page,
                Err(e) if e.code == MUTATION_DURING_PAGINATION && restarts < MAX_RESTARTS => {
                    restarts += 1;
                    continue 'pass;
                }
                Err(e) => return Err(e),
            };
            // Every page lists every account; the last list is the newest.
            if !page.accounts.is_empty() {
                accounts = page.accounts;
            }
            upserts.extend(page.added);
            upserts.extend(page.modified);
            removed.extend(page.removed);
            if !page.has_more {
                return Ok(Collected {
                    delta: delta_of(accounts, upserts, removed),
                    next_cursor: page.next_cursor,
                    historical_done: page.historical_done,
                });
            }
            if page.next_cursor == cursor {
                return Err(PlaidError::local("Plaid kept sending the same page"));
            }
            cursor = page.next_cursor;
        }
        return Err(PlaidError::local("Plaid sent more pages than one sync reads"));
    }
}

/// Pages gathered into one delta: each transaction once, as the last page
/// that mentioned it had it (added on one page, modified on a later one, is
/// the modification), and none that a page withdrew.
fn delta_of(
    accounts: Vec<SyncedAccount>,
    upserts: Vec<SyncedTxn>,
    removed: Vec<String>,
) -> SyncDelta {
    let upserts = {
        let gone: HashSet<&str> = removed.iter().map(String::as_str).collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut kept: Vec<SyncedTxn> = upserts
            .into_iter()
            .rev()
            .filter(|txn| {
                !gone.contains(txn.provider_id.as_str()) && seen.insert(txn.provider_id.clone())
            })
            .collect();
        kept.reverse();
        kept
    };
    SyncDelta { accounts, upserts, removed }
}

/// When a link that just synced is due again.
fn next_after_success(link: &BankLink, state: &LinkState, now: Timestamp) -> Timestamp {
    let young = now.duration_since(link.created_at) < FILLING_WINDOW;
    if young && !state.historical_done { now + FILLING_INTERVAL } else { now + SUCCESS_INTERVAL }
}

/// How long to wait after a failure, given how many came before it: 15
/// minutes, then 30, an hour, two... capped at a day.
fn backoff(failures_before: u32) -> SignedDuration {
    let minutes = BACKOFF_FIRST_MINUTES.saturating_mul(1i64 << failures_before.min(16));
    SignedDuration::from_mins(minutes.min(BACKOFF_MOST_MINUTES))
}

/// Record a failure: back off, keep Plaid's words, say so once per outage.
async fn failed<L: Ledger>(
    svc: &Arc<Service>,
    ledger: &Arc<L>,
    link: &BankLink,
    mut state: LinkState,
    before: Health,
    message: String,
    background: bool,
) -> Turn {
    let now = svc.now();
    state.next_at = now + backoff(state.failures);
    state.failures = state.failures.saturating_add(1);
    state.last_error = Some(message.clone());
    save(ledger, &state).await;
    // The link's id and nothing it says: a failure's words often name the
    // bank, which the store keeps out of every clear column, and a log is
    // one.
    tracing::info!(link = %link.id, "a bank could not be synced");
    if svc.money.failed(link.id) {
        let notice = Notification::warning(format!("{} could not be synced", link.name))
            .body(message.clone())
            .key(notice_key(link.id));
        notify(svc, background, notice);
    }
    Turn { report: SyncReport::failed(link.id, message), health_changed: before.changed(&state) }
}

/// Record that the bank wants the person to sign in again, and stop asking
/// it until they have.
async fn login_lost<L: Ledger>(
    svc: &Arc<Service>,
    ledger: &Arc<L>,
    link: &BankLink,
    mut state: LinkState,
    before: Health,
    error: PlaidError,
    background: bool,
) -> Turn {
    let now = svc.now();
    state.needs_login = true;
    state.last_error = Some(error.message.clone());
    state.next_at = now + LOGIN_RECHECK;
    save(ledger, &state).await;
    tracing::info!(link = %link.id, code = %error.code, "a bank wants its sign-in again");
    if svc.money.failed(link.id) {
        let notice = Notification::warning(sign_in_again(link))
            .body(
                "Choose Sign in again beside it in Money → Accounts, and syncing will carry on \
                 from where it stopped.",
            )
            .key(notice_key(link.id));
        notify(svc, background, notice);
    }
    Turn {
        report: SyncReport { needs_login: true, ..SyncReport::failed(link.id, error.message) },
        health_changed: before.changed(&state),
    }
}

fn sign_in_again(link: &BankLink) -> String {
    format!("{} needs you to sign in again", link.name)
}

/// One key per link, so a fault that is still a fault replaces its own
/// message rather than stacking a second.
fn notice_key(id: BankLinkId) -> String {
    format!("money:{id}")
}

/// Raise a notification. One a background pass raises must reach a person
/// who may have Money closed; one raised by somebody pressing "Sync now" is
/// for the window they are looking at.
fn notify(svc: &Service, background: bool, notice: Notification) {
    svc.events().notify(if background { notice.for_user() } else { notice });
}

/// Store a link's state, saying so in the log if it could not be: the next
/// sync starts from the cursor before, which is safe.
async fn save<L: Ledger>(ledger: &Arc<L>, state: &LinkState) {
    let saved = blocking({
        let ledger = ledger.clone();
        let state = state.clone();
        move || Ok(ledger.save_state(&state)?)
    })
    .await;
    if let Err(e) = saved {
        tracing::warn!(link = %state.link_id, error = %e, "could not record what a bank sync learned");
    }
}

/// Has `id` been disconnected?
async fn gone<L: Ledger>(ledger: &Arc<L>, id: BankLinkId) -> bool {
    let found = blocking({
        let ledger = ledger.clone();
        move || Ok(ledger.link(id)?)
    })
    .await;
    matches!(found, Err(e) if e.code == codes::NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventSink, Reach};
    use crate::money::plaid::fake::{self, SECRET, creds, ok, page, refuse, txn};
    use everyday_core::money::PlaidEnvironment;
    use serde_json::Value;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TOKEN: &str = "access-sandbox-0000-1111-2222-3333";

    /// One link's worth of vault, recording every write in order.
    struct Book {
        link: Mutex<BankLink>,
        state: Mutex<Option<LinkState>>,
        creds: Option<PlaidCredentials>,
        fail_apply: AtomicBool,
        log: Mutex<Vec<String>>,
        applied: Mutex<Vec<SyncDelta>>,
    }

    impl Book {
        fn new(environment: PlaidEnvironment) -> Arc<Self> {
            let provider = LinkProvider::Plaid {
                item_id: "item-1".into(),
                institution_id: "ins_1".into(),
                environment,
            };
            Arc::new(Self {
                link: Mutex::new(BankLink::new(provider, "First Platypus Bank")),
                state: Mutex::new(None),
                creds: Some(creds()),
                fail_apply: AtomicBool::new(false),
                log: Mutex::default(),
                applied: Mutex::default(),
            })
        }

        fn without_keys() -> Arc<Self> {
            let book = Self::new(PlaidEnvironment::Sandbox);
            let mut book = Arc::try_unwrap(book).ok().unwrap();
            book.creds = None;
            Arc::new(book)
        }

        fn id(&self) -> BankLinkId {
            self.link.lock().unwrap().id
        }

        fn current(&self) -> LinkState {
            self.state.lock().unwrap().clone().unwrap_or_else(|| LinkState::new(self.id()))
        }

        fn set_cursor(&self, cursor: &str) {
            let mut state = LinkState::new(self.id());
            state.cursor = cursor.into();
            *self.state.lock().unwrap() = Some(state);
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Ledger for Book {
        fn writable(&self) -> bool {
            true
        }
        fn link(&self, _id: BankLinkId) -> everyday_core::Result<BankLink> {
            Ok(self.link.lock().unwrap().clone())
        }
        fn state(&self, _id: BankLinkId) -> everyday_core::Result<Option<LinkState>> {
            Ok(self.state.lock().unwrap().clone())
        }
        fn has_credentials(&self) -> everyday_core::Result<bool> {
            Ok(self.creds.is_some())
        }
        fn credentials(&self) -> everyday_core::Result<Option<PlaidCredentials>> {
            Ok(self.creds.clone())
        }
        fn token(&self, _id: BankLinkId) -> everyday_core::Result<AccessToken> {
            self.log.lock().unwrap().push("token".into());
            Ok(AccessToken(TOKEN.into()))
        }
        fn apply(&self, _id: BankLinkId, delta: &SyncDelta) -> everyday_core::Result<SyncOutcome> {
            if self.fail_apply.load(Ordering::SeqCst) {
                self.log.lock().unwrap().push("apply failed".into());
                return Err(everyday_core::Error::Invalid("the disk is full".into()));
            }
            self.log.lock().unwrap().push("apply".into());
            self.applied.lock().unwrap().push(delta.clone());
            Ok(SyncOutcome {
                added: delta.upserts.len() as u32,
                updated: 0,
                removed: delta.removed.len() as u32,
                new_accounts: 0,
            })
        }
        fn save_state(&self, state: &LinkState) -> everyday_core::Result<()> {
            self.log.lock().unwrap().push(format!("save cursor={}", state.cursor));
            *self.state.lock().unwrap() = Some(state.clone());
            Ok(())
        }
        fn due(&self, now: Timestamp) -> everyday_core::Result<Vec<BankLinkId>> {
            let due = self.state.lock().unwrap().as_ref().is_none_or(|state| state.next_at <= now);
            Ok(if due { vec![self.id()] } else { Vec::new() })
        }
        fn active(&self) -> everyday_core::Result<Vec<BankLinkId>> {
            let link = self.link.lock().unwrap();
            Ok(if link.paused { Vec::new() } else { vec![link.id] })
        }
        fn next_due(&self) -> everyday_core::Result<Option<Timestamp>> {
            Ok(self.state.lock().unwrap().as_ref().map(|state| state.next_at))
        }
    }

    /// Every notification and change a service raised.
    #[derive(Default)]
    struct Heard {
        notices: Mutex<Vec<crate::events::Notification>>,
        changes: Mutex<Vec<Change>>,
    }

    impl EventSink for Heard {
        fn notify(&self, notification: crate::events::Notification) {
            self.notices.lock().unwrap().push(notification);
        }
        fn changed(&self, change: Change) {
            self.changes.lock().unwrap().push(change);
        }
    }

    impl Heard {
        fn txn_changes(&self) -> usize {
            self.changes.lock().unwrap().iter().filter(|c| c.kind == Kind::Txn).count()
        }
    }

    fn cursor_of(body: &Value) -> Option<String> {
        body.get("cursor").and_then(Value::as_str).map(str::to_string)
    }

    #[tokio::test]
    async fn pages_are_gathered_into_one_delta_and_the_cursor_is_stored_only_after_it() {
        let plaid = fake::serve(|_, body| match cursor_of(body).as_deref() {
            None => ok(page(vec![txn("t1", 4.5)], vec![], &[], "c1", true)),
            Some("c1") => {
                ok(page(vec![txn("t2", -100.0)], vec![txn("t1", 5.0)], &["t0"], "c2", false))
            }
            _ => refuse(400, "UNEXPECTED", "no such cursor"),
        })
        .await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Sandbox);
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;

        assert_eq!(turn.report.error, None);
        assert_eq!((turn.report.added, turn.report.removed), (2, 1));
        assert_eq!(book.log(), vec!["token", "apply", "save cursor=c2"]);
        let applied = book.applied.lock().unwrap();
        assert_eq!(applied.len(), 1, "one delta for the whole sync");
        let delta = &applied[0];
        let ids: Vec<&str> = delta.upserts.iter().map(|t| t.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["t2", "t1"]);
        let t1 = delta.upserts.iter().find(|t| t.provider_id == "t1").unwrap();
        assert_eq!(t1.amount, -500, "the later page's modification wins");
        assert_eq!(delta.removed, vec!["t0".to_string()]);
        assert_eq!(delta.accounts.len(), 1);

        let state = book.current();
        assert_eq!(state.cursor, "c2");
        assert!(state.historical_done && state.last_ok.is_some() && state.last_error.is_none());
        let wait = state.next_at.duration_since(svc.now());
        assert!(
            wait > SignedDuration::from_hours(5),
            "six hours after a finished history: {wait:?}"
        );
    }

    #[tokio::test]
    async fn a_delta_that_could_not_be_stored_leaves_the_cursor_where_it_was() {
        let plaid =
            fake::serve(|_, _| ok(page(vec![txn("t1", 4.5)], vec![], &[], "c-next", false))).await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Sandbox);
        book.set_cursor("c-before");
        book.fail_apply.store(true, Ordering::SeqCst);
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;

        assert!(turn.report.error.as_deref().is_some_and(|e| e.contains("could not be stored")));
        assert_eq!(book.log(), vec!["token", "apply failed", "save cursor=c-before"]);
        let state = book.current();
        assert_eq!(state.cursor, "c-before");
        assert_eq!(state.failures, 1);
        assert!(state.last_error.is_some());
        assert!(turn.health_changed);
    }

    #[tokio::test]
    async fn a_mutation_during_pagination_starts_again_from_the_stored_cursor() {
        let tripped = Arc::new(AtomicBool::new(false));
        let trip = tripped.clone();
        let plaid = fake::serve(move |_, body| {
            let again = trip.load(Ordering::SeqCst);
            match (cursor_of(body).as_deref(), again) {
                (Some("c0"), false) => ok(page(vec![txn("t1", 1.0)], vec![], &[], "c1", true)),
                (Some("c1"), _) => {
                    trip.store(true, Ordering::SeqCst);
                    refuse(400, MUTATION_DURING_PAGINATION, "Underlying transaction data changed")
                }
                (Some("c0"), true) => ok(page(vec![txn("t1", 1.0)], vec![], &[], "c1b", true)),
                (Some("c1b"), _) => ok(page(vec![txn("t2", 2.0)], vec![], &[], "c2", false)),
                _ => refuse(400, "UNEXPECTED", "no such cursor"),
            }
        })
        .await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Sandbox);
        book.set_cursor("c0");
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;

        assert_eq!(turn.report.error, None);
        let asked: Vec<Option<String>> = plaid.seen().iter().map(|s| cursor_of(&s.body)).collect();
        assert_eq!(
            asked,
            vec![Some("c0".into()), Some("c1".into()), Some("c0".into()), Some("c1b".into())]
        );
        let applied = book.applied.lock().unwrap();
        let ids: Vec<&str> = applied[0].upserts.iter().map(|t| t.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["t1", "t2"], "what came before the restart is not kept twice");
        assert_eq!(book.current().cursor, "c2");
        assert!(tripped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn mutations_without_end_give_up_after_three_restarts() {
        let plaid = fake::serve(|_, _| {
            refuse(400, MUTATION_DURING_PAGINATION, "Underlying transaction data changed")
        })
        .await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Sandbox);
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;
        assert!(turn.report.error.is_some());
        assert_eq!(plaid.total(), 4, "the first try and three restarts");
        assert!(!book.log().contains(&"apply".to_string()));
    }

    #[tokio::test]
    async fn a_bank_that_wants_its_login_again_is_flagged_and_left_alone() {
        let plaid = fake::serve(|_, _| {
            refuse(400, "ITEM_LOGIN_REQUIRED", "the login details of this item have changed")
        })
        .await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Sandbox);
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), true).await;

        assert!(turn.report.needs_login);
        let state = book.current();
        assert!(state.needs_login);
        assert_eq!(state.failures, 0, "not a failure to back off from: it waits for the person");
        assert!(!book.log().contains(&"apply".to_string()));

        let again = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;
        assert!(again.report.needs_login);
        assert_eq!(plaid.total(), 1, "a link waiting for its sign-in is not asked again");
    }

    #[tokio::test]
    async fn a_link_from_the_other_environment_is_never_sent_to_plaid() {
        let plaid = fake::serve(|_, _| ok(page(vec![], vec![], &[], "c1", false))).await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Production);
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;

        let error = turn.report.error.unwrap();
        assert_eq!(
            error,
            "This bank was linked with production keys; switch back to them or link it again."
        );
        assert_eq!(plaid.total(), 0);
        assert!(!book.log().contains(&"token".to_string()), "its token is not even read");
        assert_eq!(book.current().last_error.as_deref(), Some(error.as_str()));
    }

    #[tokio::test]
    async fn a_failure_is_said_once_per_outage_and_names_the_bank_not_the_token() {
        let healthy = Arc::new(AtomicBool::new(false));
        let health = healthy.clone();
        let plaid = fake::serve(move |_, body| {
            if health.load(Ordering::SeqCst) {
                ok(page(vec![], vec![], &[], "c1", false))
            } else {
                // Echoes the token, as a careless server might.
                let echoed = format!("internal error for {}", body["access_token"]);
                refuse(500, "INTERNAL_SERVER_ERROR", &echoed)
            }
        })
        .await;
        let heard = Arc::new(Heard::default());
        let svc = Arc::new(Service::new());
        svc.set_events(heard.clone());
        let book = Book::new(PlaidEnvironment::Sandbox);

        sync_one(&svc, &book, &plaid.host(), book.id(), true).await;
        sync_one(&svc, &book, &plaid.host(), book.id(), true).await;
        assert_eq!(heard.notices.lock().unwrap().len(), 1, "one notice for one outage");
        assert_eq!(book.current().failures, 2);

        healthy.store(true, Ordering::SeqCst);
        sync_one(&svc, &book, &plaid.host(), book.id(), true).await;
        assert_eq!(book.current().failures, 0);
        healthy.store(false, Ordering::SeqCst);
        sync_one(&svc, &book, &plaid.host(), book.id(), true).await;

        let notices = heard.notices.lock().unwrap();
        assert_eq!(notices.len(), 2, "a new outage is news again");
        for notice in notices.iter() {
            assert!(notice.title.contains("First Platypus Bank"), "{}", notice.title);
            let said = format!("{} {}", notice.title, notice.body.clone().unwrap_or_default());
            assert!(!said.contains(TOKEN) && !said.contains(SECRET), "{said}");
            assert!(
                matches!(notice.reach, Reach::User),
                "a background failure must reach the person"
            );
            assert_eq!(notice.key.as_deref(), Some(format!("money:{}", book.id()).as_str()));
        }
    }

    #[tokio::test]
    async fn without_plaid_keys_nothing_is_asked_and_nothing_is_written() {
        let plaid = fake::serve(|_, _| ok(page(vec![], vec![], &[], "c1", false))).await;
        let svc = Arc::new(Service::new());
        let book = Book::without_keys();
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;
        assert_eq!(turn.report.error.as_deref(), Some(NO_KEYS));
        let quiet = sync_many(&svc, &book, &plaid.host(), false, true).await.unwrap();
        assert!(quiet.is_empty(), "a due pass without keys syncs nothing and says nothing");
        assert_eq!(plaid.total(), 0);
        assert!(book.log().is_empty());
    }

    #[tokio::test]
    async fn a_paused_link_is_not_synced() {
        let plaid = fake::serve(|_, _| ok(page(vec![], vec![], &[], "c1", false))).await;
        let svc = Arc::new(Service::new());
        let book = Book::new(PlaidEnvironment::Sandbox);
        book.link.lock().unwrap().paused = true;
        let turn = sync_one(&svc, &book, &plaid.host(), book.id(), false).await;
        assert!(turn.report.error.as_deref().is_some_and(|e| e.contains("paused")));
        assert_eq!(plaid.total(), 0);
        assert!(sync_many(&svc, &book, &plaid.host(), true, false).await.unwrap().is_empty());
        // The loop pushes it back rather than waking for it every minute.
        sync_one(&svc, &book, &plaid.host(), book.id(), true).await;
        assert!(book.current().next_at > svc.now() + SignedDuration::from_hours(5));
        assert_eq!(plaid.total(), 0);
    }

    #[tokio::test]
    async fn a_pass_announces_transactions_only_when_something_changed() {
        let plaid =
            fake::serve(|_, _| ok(page(vec![txn("t1", 4.5)], vec![], &[], "c1", false))).await;
        let heard = Arc::new(Heard::default());
        let svc = Arc::new(Service::new());
        svc.set_events(heard.clone());
        let book = Book::new(PlaidEnvironment::Sandbox);

        let sleep = pass(&svc, &book, &plaid.host()).await.unwrap();
        assert_eq!(heard.txn_changes(), 1);
        assert_eq!(sleep, LONGEST_SLEEP, "six hours away, clamped to half an hour");

        pass(&svc, &book, &plaid.host()).await.unwrap();
        assert_eq!(heard.txn_changes(), 1, "nothing was due, so nothing was said");
        assert_eq!(plaid.total(), 1);
    }

    #[test]
    fn backoff_doubles_from_fifteen_minutes_to_a_day() {
        assert_eq!(backoff(0), SignedDuration::from_mins(15));
        assert_eq!(backoff(1), SignedDuration::from_mins(30));
        assert_eq!(backoff(2), SignedDuration::from_mins(60));
        assert_eq!(backoff(6), SignedDuration::from_mins(960));
        assert_eq!(backoff(7), SignedDuration::from_hours(24));
        assert_eq!(backoff(u32::MAX), SignedDuration::from_hours(24));
    }

    #[test]
    fn sleep_is_clamped_between_a_minute_and_half_an_hour() {
        let now = Timestamp::now();
        assert_eq!(sleep_for(None, now), LONGEST_SLEEP);
        assert_eq!(sleep_for(Some(now + SignedDuration::from_secs(5)), now), SHORTEST_SLEEP);
        assert_eq!(
            sleep_for(Some(now + SignedDuration::from_mins(10)), now),
            Duration::from_secs(600)
        );
        assert_eq!(sleep_for(Some(now + SignedDuration::from_hours(6)), now), LONGEST_SLEEP);
        assert_eq!(sleep_for(Some(now - SignedDuration::from_mins(1)), now), LONGEST_SLEEP);
    }

    #[test]
    fn a_young_link_still_filling_its_history_is_asked_again_soon() {
        let provider = LinkProvider::Plaid {
            item_id: "i".into(),
            institution_id: String::new(),
            environment: PlaidEnvironment::Sandbox,
        };
        let link = BankLink::new(provider, "Bank");
        let mut state = LinkState::new(link.id);
        let now = link.created_at + SignedDuration::from_mins(1);
        assert_eq!(next_after_success(&link, &state, now), now + FILLING_INTERVAL);
        state.historical_done = true;
        assert_eq!(next_after_success(&link, &state, now), now + SUCCESS_INTERVAL);
        state.historical_done = false;
        let later = link.created_at + SignedDuration::from_hours(25);
        assert_eq!(next_after_success(&link, &state, later), later + SUCCESS_INTERVAL);
    }

    #[test]
    fn a_withdrawn_transaction_is_not_upserted_in_the_same_delta() {
        let synced = |id: &str| SyncedTxn {
            provider_account_id: "acc-1".into(),
            provider_id: id.into(),
            replaces_pending: None,
            date: jiff::civil::Date::constant(2026, 11, 1),
            amount: -100,
            currency: "USD".into(),
            payee: String::new(),
            description: String::new(),
            pending: false,
            bank_category: String::new(),
        };
        let delta = delta_of(vec![], vec![synced("a"), synced("b"), synced("a")], vec!["b".into()]);
        let ids: Vec<&str> = delta.upserts.iter().map(|t| t.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
        assert_eq!(delta.removed, vec!["b".to_string()]);
    }

    #[tokio::test]
    async fn a_linked_bank_syncs_into_the_vault_end_to_end() {
        let plaid = fake::serve(|_, _| {
            ok(page(vec![txn("t1", 4.5), txn("t2", -1200.0)], vec![], &[], "c-first", false))
        })
        .await;
        let (svc, vault, _dir) = fake::test_vault();
        vault.set_plaid_credentials(&creds()).unwrap();
        let provider = LinkProvider::Plaid {
            item_id: "item-1".into(),
            institution_id: "ins_1".into(),
            environment: PlaidEnvironment::Sandbox,
        };
        let link = BankLink::new(provider, "First Platypus Bank");
        vault.add_bank_link(&link, &AccessToken(TOKEN.into())).unwrap();

        let turn = sync_one(&svc, &vault, &plaid.host(), link.id, false).await;
        assert_eq!(turn.report.error, None);
        assert_eq!(turn.report.added, 2);
        assert_eq!(plaid.seen()[0].body["access_token"], TOKEN);
        let state = vault.bank_link_state(link.id).unwrap().unwrap();
        assert_eq!(state.cursor, "c-first");
        let accounts = vault.bank_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].balance, Some(11_025));
    }

    #[tokio::test]
    async fn the_loop_stops_when_asked() {
        let (svc, vault, _dir) = fake::test_vault();
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(run(svc.clone(), vault.clone(), rx));
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(true).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the loop answers its stop")
            .expect("the task did not panic");
        assert!(matches!(outcome, Ok(Outcome::Done)));
    }
}
