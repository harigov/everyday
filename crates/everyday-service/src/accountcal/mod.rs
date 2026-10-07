//! Calendars that sign in: reading the calendars an account already has,
//! over CalDAV or the provider's own API, rather than a feed URL pasted
//! into [`AddCalendar`](../../../../ui/src/components/AddCalendar.svelte).
//!
//! `docs/plans/mail.md`'s phase 6 in one paragraph: `CalendarOrigin::Account`
//! (`everyday-core`) names which account and which of the account's own
//! calendars a [`Calendar`](everyday_core::calendar::Calendar) mirrors; this
//! module is everything that reaches the network to fill it in.
//! `domains::calendars` calls [`discover`], to list what an account offers
//! and let somebody tick the ones they want, [`sync`], to bring one
//! already-subscribed account calendar's events up to date, and the four
//! writes -- [`create_event`], [`load_event`], [`update_event`] and
//! [`delete_event`] -- and knows nothing about CalDAV, the Google Calendar
//! API or Microsoft Graph beyond that split.
//!
//! # Writing
//!
//! A write goes to the server and stops there. Nothing here edits an
//! [`Event`] row to match: once the server has said yes, the calendar's own
//! [`sync`] runs and brings the change back exactly the way it would bring
//! back one made on a phone -- one path into the vault for every change,
//! whoever made it, which is what keeps the incremental sync's bookkeeping
//! (sync tokens, etags, deterministic ids) honest. Each source's writes live
//! in a child module of its own (`google_write.rs`, `graph_write.rs`,
//! `caldav_write.rs`), beside the reading half whose helpers they share.
//!
//! # Three sources, one shape
//!
//! | Source | Discovery | Sync | Auth |
//! |---|---|---|---|
//! | [`caldav`] | principal + calendar-home, `.well-known` fallback | etag diff, or RFC 6578 sync-collection where advertised | Basic (app password) or Bearer |
//! | [`google`] | `calendarList.list` | `events.list` with `syncToken`, full resync on 410 | Bearer |
//! | [`graph`] | `GET /me/calendars` | `calendarView/delta` for the primary calendar, windowed `calendarView` for the rest | Bearer, Graph resource |
//!
//! Every source ends up producing the same two things a caller needs: a
//! list of [`RemoteCalendar`] for discovery, and a
//! `(upsert: Vec<Event>, remove: Vec<EventId>, cursor: AccountSyncCursor)`
//! triple for a sync, hitting [`everyday_core::vault::Vault::sync_account_calendar`]
//! exactly once each. That shared write path is what makes "read-only, like
//! a feed" true for all three sources at once -- see
//! [`everyday_core::store::calendars::CalendarStore::upsert_events`]'s own
//! doc for why the write is incremental rather than a wholesale replace.
//!
//! # Why Google is read over its own API, not CalDAV
//!
//! Google *does* publish a CalDAV endpoint (`apidata.googleusercontent.com`)
//! and `caldav.rs` can reach it -- iCloud, Fastmail, Custom and Google's own
//! CalDAV endpoint all go through the same adapter, which is what the table
//! above means by "CalDAV" covering Google too. But Google's CalDAV
//! principal and calendar-home discovery is unusually brittle in practice
//! (pimsync and several other open clients carry Google-specific
//! workarounds for it), while `calendarList.list` and `events.list` are a
//! stable, documented REST API this application already has a client for.
//! `google.rs` is offered as the *default* path for a Google account; CalDAV
//! remains reachable through the same `caldav.rs` adapter for anyone who
//! would rather use it (Google's calendar settings do publish the address),
//! but `accountcal::discover` and `accountcal::sync` pick the API for a
//! `Provider::Google` account rather than asking `caldav.rs` to fight
//! Google's own discovery quirks.
//!
//! # Where `chrono` is allowed to exist
//!
//! `everyday-core` -- and every other app in this tree -- reads and writes
//! time through `jiff`. `calcard`, the iCalendar parser this module uses
//! for CalDAV (see [`caldav`]'s own doc), is built on `chrono`. Per the
//! plan's settled decision ("chrono comes in with calcard ...; jiff stays
//! where it is. Two time libraries is accepted; conversions happen at the
//! calendar edge"), `chrono` is named only inside this crate's `accountcal`
//! module -- nowhere in `everyday-core`, nowhere in the UI, and nowhere else
//! in `everyday-service` imports it. [`caldav::to_jiff`] is the one function
//! that converts a `chrono::DateTime` into a `jiff::Timestamp`, and every
//! `Event` this module produces, whichever source built it, is already a
//! plain `jiff`-timed [`everyday_core::calendar::Event`] by the time it
//! reaches the vault.

pub mod caldav;
pub mod google;
pub mod graph;
pub mod tokens;

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use everyday_core::Vault;
use everyday_core::account::{Account, Provider};
use everyday_core::calendar::{
    AccountCalendarSource, Calendar, CalendarOrigin, EditableEvent, Event, EventDraft, EventScope,
    SyncReport,
};
use everyday_core::id::{CalendarId, EventId};
use everyday_core::store::calendars::EventQuery;
use serde::{Deserialize, Serialize};

use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

/// One calendar an account offers, before anyone has decided to subscribe
/// to it. What [`discover`] answers with.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCalendar {
    /// The server's own id for this calendar: an href for CalDAV, a
    /// calendar id for Google or Graph. Stable across a resync, which is
    /// what lets `list_account_calendars` say which of these this vault
    /// already has.
    pub remote_id: String,
    pub name: String,
    /// `#rrggbb`, when the provider names one for this calendar. Google and
    /// Graph both do; a bare CalDAV collection often does not, and the
    /// interface's own default palette fills in for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub source: AccountCalendarSource,
    /// Whether this account may put events on it: Google's `accessRole` of
    /// `owner` or `writer`, Graph's `canEdit`. A CalDAV collection says
    /// nothing either way that is cheap to ask, and answers true -- the
    /// server's refusal of the first write is what tells the truth there.
    #[serde(default = "yes")]
    pub writable: bool,
}

fn yes() -> bool {
    true
}

/// A boxed, `Send` future -- hand-rolled the same way
/// `crate::meeting::transcribe::BoxFuture` is, so a trait object can return
/// an `async fn`'s future without `#[async_trait]` or naming it.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One calendar source behind a common interface, shaped like
/// [`crate::meeting::transcribe::Transcriber`]: [`discover`] and [`sync`]
/// used to `match` on [`Provider`] and [`AccountCalendarSource`] directly
/// and call straight into `caldav`, `google` or `graph`; this trait is that
/// match, given a name, so [`provider_for`] is the one place the three
/// sources are chosen between.
pub trait CalendarProvider: Send + Sync {
    fn discover<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
    ) -> BoxFuture<'a, CommandResult<Vec<RemoteCalendar>>>;

    fn sync<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
    ) -> BoxFuture<'a, CommandResult<SyncReport>>;

    /// Put a new event -- repeat rule, guests and all -- on `calendar`.
    /// Guests are invited by the server itself, the way that server
    /// invites anybody: Google and Graph by their own mail, a CalDAV server
    /// that schedules implicitly (iCloud, Fastmail) by its.
    fn create<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        draft: &'a EventDraft,
    ) -> BoxFuture<'a, CommandResult<()>>;

    /// Read `event` back from the server as it stands now, with what
    /// changing it needs -- the full description, every guest by address,
    /// the series' rule. See [`EditableEvent`].
    fn load<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        event: &'a Event,
    ) -> BoxFuture<'a, CommandResult<EditableEvent>>;

    /// Change `event` -- just this occurrence, or its whole series -- to
    /// match `draft`. Guests added are invited, guests removed are told, and
    /// the rest hear about the change, all by the server.
    #[allow(clippy::too_many_arguments)]
    fn update<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        event: &'a Event,
        draft: &'a EventDraft,
        scope: EventScope,
    ) -> BoxFuture<'a, CommandResult<()>>;

    /// Delete `event`, or its whole series. Its guests are told it was
    /// cancelled, by the server.
    fn delete<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
        event: &'a Event,
        scope: EventScope,
    ) -> BoxFuture<'a, CommandResult<()>>;
}

/// The [`CalendarProvider`] for one of the three sources -- the single
/// factory [`discover`] and [`sync`] both go through, in place of the
/// `match` each used to have of its own.
fn provider_for(source: AccountCalendarSource) -> Box<dyn CalendarProvider> {
    match source {
        AccountCalendarSource::CalDav => Box::new(caldav::CalDavProvider),
        AccountCalendarSource::Google => Box::new(google::GoogleProvider),
        AccountCalendarSource::Graph => Box::new(graph::GraphProvider),
    }
}

/// Which source an account's own calendars come from, before any of them
/// has been subscribed to and so has no [`AccountCalendarSource`] of its
/// own yet on a [`CalendarOrigin::Account`] -- see the module doc's table.
fn source_for_provider(provider: Provider) -> AccountCalendarSource {
    match provider {
        Provider::Google => AccountCalendarSource::Google,
        Provider::Microsoft => AccountCalendarSource::Graph,
        Provider::ICloud | Provider::Fastmail | Provider::Yahoo | Provider::Custom => {
            AccountCalendarSource::CalDav
        }
    }
}

/// Discover the calendars `account` offers, over whichever source its
/// provider and CalDAV address say to use.
///
/// Returns an empty list, rather than an error, for an account whose
/// calendar service is off -- `domains::calendars::list_account_calendars`
/// is what decides whether to call this at all, but a caller that does not
/// check first should see "nothing here" rather than a confusing failure.
pub async fn discover(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<Vec<RemoteCalendar>> {
    if !account.services.calendar {
        return Ok(Vec::new());
    }
    let source = source_for_provider(account.provider);
    let result = provider_for(source).discover(svc, vault, account).await;
    note_if_credential_is_bad(svc, vault, account, &result).await;
    result
}

/// A `FORBIDDEN` result means the credential itself was refused -- by a
/// token endpoint for OAuth (already handled inside
/// [`tokens::access_token`], which is what turns `invalid_grant` into this
/// code in the first place) or by the server itself on a plain CalDAV
/// request with a password ([`tokens::credential`] cannot validate an app
/// password before it is used, so a 401 on the actual request is the first
/// this application learns one was revoked). Either way, the account moves
/// to `NeedsSignIn` here, once, regardless of which of [`discover`] or
/// [`sync`] noticed it -- calling this a second time for the same failure
/// (an OAuth account's discovery calling it after `tokens::access_token`
/// already did) is harmless: it writes the same status again.
async fn note_if_credential_is_bad<T>(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    result: &CommandResult<T>,
) {
    if let Err(e) = result
        && e.code == codes::FORBIDDEN
    {
        tokens::mark_needs_sign_in(vault, account.id, &e.message, svc.now()).await;
    }
}

/// Bring one already-subscribed account calendar's events up to date.
///
/// `calendar.origin` must be [`CalendarOrigin::Account`]; anything else is a
/// programming error in the caller, not a condition worth a soft failure
/// for. See the module doc's table for which source handles which account.
pub async fn sync(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    calendar: &Calendar,
) -> CommandResult<SyncReport> {
    let CalendarOrigin::Account { account_id, source, .. } = &calendar.origin else {
        return Err(CommandError::new(
            codes::INVALID,
            "accountcal::sync was asked to sync a calendar that is not an account's",
        ));
    };
    let account_id = *account_id;
    let source = *source;
    let vault_for_account = vault.clone();
    let account = blocking(move || Ok(vault_for_account.account(account_id)?)).await?;

    let result = provider_for(source).sync(svc, vault, &account, calendar).await;
    note_if_credential_is_bad(svc, vault, &account, &result).await;

    // A provider that learned in passing whether this account may write here
    // -- Google says so on every page of `events.list` -- keeps the calendar
    // picker honest without a discovery call of its own.
    if let Ok(SyncReport { writable: Some(writable), .. }) = &result {
        let vault = vault.clone();
        let id = calendar.id;
        let read_only = !*writable;
        if let Err(e) = blocking(move || Ok(vault.set_calendar_read_only(id, read_only)?)).await {
            tracing::warn!(%id, error = %e, "could not record whether a calendar is writable");
        }
    }

    // Only a genuine, permission-shaped failure (`FORBIDDEN`, already
    // handled above by moving the account to `NeedsSignIn`) skips the
    // calendar's own `last_error`: the account view already says so, in a
    // place that covers every calendar the account has rather than
    // repeating the same sentence under each one. Every other failure --
    // a timeout, a 5xx, a malformed response -- is recorded on the
    // calendar exactly the way a feed's is, per the plan.
    if let Err(e) = &result
        && e.code != codes::FORBIDDEN
    {
        record_failure(vault, calendar.id, &e.message).await;
    }
    result
}

/// The account a calendar reads from, and the calendar itself, once both
/// have been checked fit to write to -- the gate every write below passes
/// first, so none of the three sources has to repeat it.
async fn writable_target(
    vault: &Arc<Vault>,
    calendar_id: CalendarId,
) -> CommandResult<(Calendar, Account, AccountCalendarSource)> {
    let vault = vault.clone();
    let (calendar, account) = blocking(move || {
        let calendar = vault.calendar(calendar_id)?;
        let account = match calendar.origin.account_id() {
            Some(id) => Some(vault.account(id)?),
            None => None,
        };
        Ok((calendar, account))
    })
    .await?;
    let (Some(account), CalendarOrigin::Account { source, .. }) = (account, &calendar.origin)
    else {
        return Err(CommandError::new(
            codes::INVALID,
            format!(
                "{} is a subscribed calendar, and nothing can be added to one from here",
                calendar.name
            ),
        ));
    };
    let source = *source;
    if calendar.read_only {
        return Err(CommandError::new(
            codes::INVALID,
            format!("{} does not let {} change it", calendar.name, account.address),
        ));
    }
    if !account.services.calendar {
        return Err(CommandError::new(
            codes::INVALID,
            format!("calendar is switched off for {}", account.address),
        ));
    }
    if !account.can_write_calendars() {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            format!(
                "{} was signed in to read its calendars only. Sign in to it again from Settings \
                 \u{2192} Accounts to let Every Day add and change events.",
                account.address
            ),
        ));
    }
    Ok((calendar, account, source))
}

/// Bring a calendar back in line after a write landed. A failure here is
/// logged rather than returned: the write itself worked, and the next
/// background sync will pick it up regardless.
async fn resync_after_write(svc: &Arc<Service>, vault: &Arc<Vault>, calendar_id: CalendarId) {
    let latest = {
        let vault = vault.clone();
        blocking(move || Ok(vault.calendar(calendar_id)?)).await
    };
    match latest {
        Ok(calendar) => {
            if let Err(e) = sync(svc, vault, &calendar).await {
                tracing::info!(%calendar_id, error = %e, "a calendar did not resync after a write");
            }
        }
        Err(e) => {
            tracing::warn!(%calendar_id, error = %e, "a written calendar could not be read back")
        }
    }
}

/// A write's failure, with the account moved to `NeedsSignIn` when the
/// server refused the credential outright. A 403 on a write is *not* that
/// -- reading may still work perfectly well -- so the sources answer one
/// with another code, and only a refused credential lands here as
/// `FORBIDDEN`. (The scope check in [`writable_target`] answers `FORBIDDEN`
/// too, but before any request is made, so it never reaches this.)
async fn after_write<T>(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    result: CommandResult<T>,
) -> CommandResult<T> {
    note_if_credential_is_bad(svc, vault, account, &result).await;
    result
}

/// Put `draft` on `calendar_id`, then sync it so the new event is on the
/// grid. Answers the new event's first occurrence when the sync found it --
/// `None` when the server took it but the sync has not brought it back yet,
/// which the caller treats as "refresh later", not as a failure.
pub async fn create_event(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    calendar_id: CalendarId,
    draft: &EventDraft,
) -> CommandResult<Option<Event>> {
    let mut draft = draft.clone();
    draft.validate()?;
    let (calendar, account, source) = writable_target(vault, calendar_id).await?;
    let result = provider_for(source).create(svc, vault, &account, &calendar, &draft).await;
    after_write(svc, vault, &account, result).await?;
    resync_after_write(svc, vault, calendar_id).await;

    let vault = vault.clone();
    let day = draft.start_date();
    blocking(move || {
        let found = vault.events(&EventQuery {
            calendar_id: Some(calendar_id),
            from: Some(day),
            to: Some(day),
            ..Default::default()
        })?;
        Ok(found.into_iter().find(|e| e.start == draft.start && e.title == draft.title))
    })
    .await
}

/// Read one event fresh from its server, ready to be changed.
pub async fn load_event(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    event: &Event,
) -> CommandResult<EditableEvent> {
    let (calendar, account, source) = writable_target(vault, event.calendar_id).await?;
    let result = provider_for(source).load(svc, vault, &account, &calendar, event).await;
    after_write(svc, vault, &account, result).await
}

/// Change one event, or its whole series, then sync its calendar.
pub async fn update_event(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
) -> CommandResult<()> {
    let mut draft = draft.clone();
    draft.validate()?;
    let (calendar, account, source) = writable_target(vault, event.calendar_id).await?;
    let result =
        provider_for(source).update(svc, vault, &account, &calendar, event, &draft, scope).await;
    after_write(svc, vault, &account, result).await?;
    resync_after_write(svc, vault, event.calendar_id).await;
    Ok(())
}

/// Delete one event, or its whole series, then sync its calendar.
pub async fn delete_event(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    event: &Event,
    scope: EventScope,
) -> CommandResult<()> {
    let (calendar, account, source) = writable_target(vault, event.calendar_id).await?;
    let result = provider_for(source).delete(svc, vault, &account, &calendar, event, scope).await;
    after_write(svc, vault, &account, result).await?;
    resync_after_write(svc, vault, event.calendar_id).await;
    Ok(())
}

async fn record_failure(vault: &Arc<Vault>, id: CalendarId, why: &str) {
    let vault = vault.clone();
    let why = why.to_string();
    if let Err(e) = blocking(move || Ok(vault.mark_calendar_failed(id, &why)?)).await {
        tracing::warn!(%id, error = %e, "could not record an account calendar sync failure");
    }
}

/// A stable [`everyday_core::id::EventId`] for one occurrence of an account
/// calendar's event, derived from the calendar it is on and the source's
/// own uid rather than minted fresh.
///
/// Feed events (`everyday_core::ics`) get a new random id on every sync,
/// because a feed sync always replaces the calendar's entire event set at
/// once -- see [`everyday_core::id`]'s own doc on why an `EventId` "is an
/// addressing detail rather than a durable name". An account calendar's
/// sync is incremental instead
/// ([`everyday_core::store::calendars::CalendarStore::upsert_events`]), and
/// an incremental write needs to name the *same* row again when nothing
/// about that occurrence changed -- a deterministic id is what lets it do
/// that without reading the calendar back first: reparsing the same
/// occurrence on the next sync reproduces the same id, so upserting it is a
/// no-op write rather than a duplicate row, and the id of a vanished
/// occurrence can be recomputed from its old uid to delete exactly that row.
///
/// UUID v5 over a fixed, arbitrary namespace: deterministic, and it is not
/// meant to be guessed from the outside -- nothing about an `EventId`
/// crosses a trust boundary where that would matter, but there is no reason
/// to use the calendar's own id as the namespace either, since that would
/// make two different accounts' ids collide if they ever shared a
/// coincidental `uid`.
pub(crate) fn deterministic_event_id(
    calendar_id: CalendarId,
    uid: &str,
) -> everyday_core::id::EventId {
    const NAMESPACE: uuid::Uuid = uuid::uuid!("6b6f9f3e-9b9a-4b0a-9f0a-acc0ca1000e1");
    let name = format!("{calendar_id}:{uid}");
    everyday_core::id::EventId(uuid::Uuid::new_v5(&NAMESPACE, name.as_bytes()))
}

/// The days an account calendar's sync materialises recurring events into --
/// the same window a feed uses, so "a year back, two years forward" means
/// one thing everywhere in the calendar app. See [`crate::feeds::sync_window`].
pub(crate) fn sync_window() -> (jiff::civil::Date, jiff::civil::Date) {
    crate::feeds::sync_window(everyday_core::model::today_local())
}

/// Every event this vault already stored for `calendar_id`, inside
/// `window`, that `kept` does not name -- the deletions a *full* (tokenless)
/// resync has to compute for itself.
///
/// Google's `events.list` without a `syncToken` never returns a cancelled
/// item at all (`showDeleted` only has an effect on an incremental page),
/// and Graph's `calendarView/delta` only reports `"@removed"` entries
/// relative to the token it was given -- `None`, on a first sync or after a
/// 410, reports none. Both providers' *incremental* sync tells this crate
/// directly what vanished; only the full, window-bounded fallback needs
/// this -- read back what the window used to hold, throw away everything
/// the fresh list still names, and whatever is left is gone. Restricted to
/// `window` rather than every event this vault has for the calendar, so an
/// event an earlier *incremental* sync wrote from outside today's window
/// (Google's own sync is not itself window-bounded) is left alone: a full
/// resync only speaks for the days it actually asked about.
pub(crate) async fn missing_from_full_resync(
    vault: &Arc<Vault>,
    calendar_id: CalendarId,
    window: (jiff::civil::Date, jiff::civil::Date),
    kept: &HashSet<EventId>,
) -> CommandResult<Vec<EventId>> {
    let vault = vault.clone();
    let kept = kept.clone();
    blocking(move || {
        let existing = vault.events(&EventQuery {
            calendar_id: Some(calendar_id),
            from: Some(window.0),
            to: Some(window.1),
            ..Default::default()
        })?;
        Ok(existing.into_iter().filter(|e| !kept.contains(&e.id)).map(|e| e.id).collect())
    })
    .await
}

/// `Retry-After`, read as a plain count of seconds -- the only form Google
/// and Graph are ever seen to send it in on the responses this crate
/// retries (429s and rate-limited 403s), never the HTTP-date form the
/// header also allows.
pub(crate) fn retry_after_delay(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: u64 = raw.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// A short, capped backoff between retries of one rate-limited HTTP call,
/// used when the server names no `Retry-After` of its own.
///
/// Deliberately much shorter than the supervisor's own task-restart backoff
/// (`crate::supervisor`'s `BACKOFF_BASE`/`BACKOFF_CAP`, minutes wide): that
/// one waits out a whole task being restarted; this waits out a single
/// request inside one sync that is still in progress, and a provider's rate
/// limit typically clears in well under a second. No jitter, for the same
/// reason: jitter earns its keep when many accounts might retry in the same
/// instant and stampede a server together, which is the supervisor's
/// situation, not one calendar's one in-flight request.
pub(crate) fn short_backoff(attempt: u32) -> Duration {
    const BASE: Duration = Duration::from_millis(200);
    const CAP: Duration = Duration::from_secs(2);
    let exponent = attempt.saturating_sub(1).min(4);
    BASE.saturating_mul(1u32 << exponent).min(CAP)
}

/// This module's own schedule for a rate-limited HTTP retry, as a
/// [`crate::retry::RetryPolicy`] -- wraps [`short_backoff`] exactly, and is
/// the one place `google.rs`'s and `graph.rs`'s HTTP loops now read their
/// give-up point from. Both used to carry their own constant for it
/// (`google.rs`'s `MAX_RATE_LIMIT_ATTEMPTS`, `graph.rs`'s
/// `MAX_RETRY_ATTEMPTS`) with a comment on each saying the other was "the
/// same number, for the same reason" -- four attempts, generous enough that
/// a brief burst clears inside one sync, small enough that a sustained
/// limit does not hold up a background poll for minutes.
pub(crate) fn calendar_after_retry_after() -> crate::retry::RetryPolicy {
    crate::retry::RetryPolicy::from_fn(short_backoff, Some(4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::account::{Account, AccountStatus, Provider};
    use everyday_core::calendar::{Calendar, Event, EventStatus};

    /// A vault and a service around it, in a directory nobody has to clean
    /// up -- see `google.rs`'s own `test_vault` for why this is
    /// reproduced here rather than shared with `tests/support/vault.rs`.
    fn test_vault() -> (Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = everyday_core::VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 900,
            forget_key_seconds: 0,
        };
        let vault = Arc::new(everyday_vault::create(dir.path(), config).unwrap());
        (vault, dir)
    }

    fn bare_event(calendar_id: CalendarId, uid: &str, day: jiff::civil::Date) -> Event {
        Event {
            id: EventId::new(),
            calendar_id,
            uid: uid.to_string(),
            title: "Untitled".into(),
            description: String::new(),
            location: String::new(),
            start: jiff::Timestamp::now(),
            end: jiff::Timestamp::now(),
            local_date: day,
            end_date: day,
            tz: "UTC".into(),
            all_day: true,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: Vec::new(),
            url: String::new(),
            busy: true,
            series: None,
            updated_at: jiff::Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn missing_from_full_resync_only_reports_events_inside_the_window_that_were_not_kept() {
        let (vault, _dir) = test_vault();
        let mut account = Account::new(Provider::Google, "person@example.com");
        account.status = AccountStatus::Ok;
        vault.save_account(&account).unwrap();
        let calendar = Calendar::from_account(
            account.id,
            account.provider,
            AccountCalendarSource::Google,
            "cal-1",
            "Work",
        );
        vault.save_calendar(&calendar).unwrap();

        let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31));
        let inside_kept = bare_event(calendar.id, "kept", jiff::civil::date(2026, 6, 1));
        let inside_stale = bare_event(calendar.id, "stale", jiff::civil::date(2026, 6, 2));
        let outside_window = bare_event(calendar.id, "far-future", jiff::civil::date(2028, 1, 1));
        vault
            .sync_account_calendar(
                calendar.id,
                &[inside_kept.clone(), inside_stale.clone(), outside_window.clone()],
                &[],
                Default::default(),
            )
            .unwrap();

        let kept: HashSet<_> = [inside_kept.id].into_iter().collect();
        let missing = missing_from_full_resync(&vault, calendar.id, window, &kept).await.unwrap();

        assert_eq!(missing, vec![inside_stale.id], "only the in-window, un-kept event is reported");
        assert!(
            !missing.contains(&outside_window.id),
            "an event outside the synced window must not be reported as a deletion, per the \
             module doc's 'restricted to window'"
        );
    }

    // ---- retry math, pinned before phase 9.3 touches it ------------------

    /// [`short_backoff`]'s exact sequence, fixed before it is expressed as
    /// a `RetryPolicy`. 200ms base, doubling, capped at 2s.
    #[test]
    fn short_backoff_sequence_is_pinned() {
        let expected = [
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(800),
            Duration::from_millis(1600),
            Duration::from_millis(2000), // capped
            Duration::from_millis(2000),
            Duration::from_millis(2000),
        ];
        for (i, want) in expected.iter().enumerate() {
            let attempt = (i + 1) as u32;
            assert_eq!(short_backoff(attempt), *want, "attempt {attempt}");
        }
    }

    /// [`calendar_after_retry_after`] must answer exactly what
    /// [`short_backoff`] does, and give both `google.rs` and `graph.rs` the
    /// same four-attempt give-up point they each used to name with their
    /// own constant.
    #[test]
    fn calendar_after_retry_after_matches_short_backoff_exactly() {
        let policy = calendar_after_retry_after();
        for attempt in 1..=7u32 {
            assert_eq!(policy.delay_for(attempt), short_backoff(attempt), "attempt {attempt}");
        }
        assert_eq!(policy.max_attempts(), Some(4));
    }

    #[test]
    fn retry_after_delay_reads_a_plain_second_count() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after_delay(&headers), Some(Duration::from_secs(7)));
    }

    #[test]
    fn retry_after_delay_is_none_when_absent_or_not_a_plain_second_count() {
        assert_eq!(retry_after_delay(&reqwest::header::HeaderMap::new()), None, "no header at all");

        let mut headers = reqwest::header::HeaderMap::new();
        headers
            .insert(reqwest::header::RETRY_AFTER, "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(
            retry_after_delay(&headers),
            None,
            "the HTTP-date form is never sent by Google or Graph and is not parsed"
        );
    }

    // ---- finding 2: only a credential problem moves the account ---------

    #[tokio::test]
    async fn a_forbidden_result_moves_the_account_to_needs_sign_in() {
        let (vault, _dir) = test_vault();
        let mut account = Account::new(Provider::Google, "person@example.com");
        account.status = AccountStatus::Ok;
        vault.save_account(&account).unwrap();

        let result: CommandResult<()> = Err(CommandError::new(codes::FORBIDDEN, "bad credential"));
        note_if_credential_is_bad(&Arc::new(Service::new()), &vault, &account, &result).await;

        let reloaded = vault.account(account.id).unwrap();
        assert!(
            matches!(reloaded.status, AccountStatus::NeedsSignIn { .. }),
            "a 401-shaped FORBIDDEN must move the account to NeedsSignIn: {:?}",
            reloaded.status
        );
    }

    #[tokio::test]
    async fn a_rate_limited_or_ordinary_failure_leaves_the_account_exactly_as_it_was() {
        let (vault, _dir) = test_vault();
        for code in [codes::RATE_LIMITED, codes::NETWORK] {
            let mut account = Account::new(Provider::Google, "person@example.com");
            account.status = AccountStatus::Ok;
            vault.save_account(&account).unwrap();

            let result: CommandResult<()> = Err(CommandError::new(code, "try again later"));
            note_if_credential_is_bad(&Arc::new(Service::new()), &vault, &account, &result).await;

            let reloaded = vault.account(account.id).unwrap();
            assert_eq!(
                reloaded.status,
                AccountStatus::Ok,
                "{code} must never be read as a credential problem"
            );
        }
    }

    #[tokio::test]
    async fn a_calendar_level_failure_touches_only_that_calendar_not_the_account() {
        // What a Graph 403 on one calendar -- an ordinary, non-FORBIDDEN
        // failure -- does end to end: `record_failure` is `sync`'s own
        // handler for exactly this, called whenever a result's code is not
        // `FORBIDDEN` (see `sync`'s own body, just above in this file).
        let (vault, _dir) = test_vault();
        let mut account = Account::new(Provider::Microsoft, "person@example.com");
        account.status = AccountStatus::Ok;
        vault.save_account(&account).unwrap();
        let calendar = Calendar::from_account(
            account.id,
            account.provider,
            AccountCalendarSource::Graph,
            "cal-1",
            "Work",
        );
        vault.save_calendar(&calendar).unwrap();

        record_failure(&vault, calendar.id, "Microsoft Graph refused this request").await;

        let reloaded_account = vault.account(account.id).unwrap();
        assert_eq!(reloaded_account.status, AccountStatus::Ok, "the account must be untouched");
        let reloaded_calendar = vault.calendar(calendar.id).unwrap();
        assert_eq!(
            reloaded_calendar.last_error.as_deref(),
            Some("Microsoft Graph refused this request"),
            "the calendar itself must carry the failure"
        );
    }
}
