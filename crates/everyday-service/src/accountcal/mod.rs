//! Calendars that sign in: reading the calendars an account already has,
//! over CalDAV or the provider's own API, rather than a feed URL pasted
//! into [`AddCalendar`](../../../../ui/src/components/AddCalendar.svelte).
//!
//! `docs/plans/mail.md`'s phase 6 in one paragraph: `CalendarOrigin::Account`
//! (`everyday-core`) names which account and which of the account's own
//! calendars a [`Calendar`](everyday_core::calendar::Calendar) mirrors; this
//! module is everything that reaches the network to fill it in.
//! `domains::calendars` calls exactly two functions here --
//! [`discover`], to list what an account offers and let somebody tick the
//! ones they want, and [`sync`], to bring one already-subscribed account
//! calendar's events up to date -- and knows nothing about CalDAV, the
//! Google Calendar API or Microsoft Graph beyond that split.
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

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, Provider};
use everyday_core::calendar::{AccountCalendarSource, Calendar, CalendarOrigin, SyncReport};
use everyday_core::id::CalendarId;
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
    let result = match account.provider {
        Provider::Google => google::discover(svc, vault, account).await,
        Provider::Microsoft => graph::discover(svc, vault, account).await,
        Provider::ICloud | Provider::Fastmail | Provider::Yahoo | Provider::Custom => {
            caldav::discover(svc, vault, account).await
        }
    };
    note_if_credential_is_bad(vault, account, &result).await;
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
    vault: &Arc<Vault>,
    account: &Account,
    result: &CommandResult<T>,
) {
    if let Err(e) = result
        && e.code == codes::FORBIDDEN
    {
        tokens::mark_needs_sign_in(vault, account.id, &e.message).await;
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

    let result = match source {
        AccountCalendarSource::CalDav => caldav::sync(svc, vault, &account, calendar).await,
        AccountCalendarSource::Google => google::sync(svc, vault, &account, calendar).await,
        AccountCalendarSource::Graph => graph::sync(svc, vault, &account, calendar).await,
    };
    note_if_credential_is_bad(vault, &account, &result).await;

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
