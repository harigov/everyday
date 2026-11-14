//! Subscribed calendars and the events read out of them.
//!
//! Note the division of labour, which is the same one the rest of the
//! application makes and is worth spelling out because this is the only
//! feature that touches a network -- two features, now that phase 6 has
//! landed:
//!
//!   this file      what a URL or an account is, when to fetch it, what to do on a 403
//!   feeds.rs       turning a URL into bytes -- a subscription's own socket
//!   accountcal/    turning an account's calendars into bytes -- CalDAV, Google, Graph
//!   karmayoga-core  everything that happens to those bytes afterwards
//!
//! The last of those is the part with the difficult logic in it -- RFC 5545,
//! recurrence, time zones -- and it is testable offline precisely because it
//! never learns that a network exists.

use crate::accountcal::{self, RemoteCalendar};
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::Notification;
use crate::feeds;
use crate::service::{Service, blocking};
use futures::StreamExt;
use karmayoga_core::account::{Account, AccountStatus};
use karmayoga_core::calendar::{
    Calendar, CalendarAccess, CalendarOrigin, CalendarProvider, EditableEvent, Event, EventDraft,
    EventScope, SyncReport,
};
use karmayoga_core::model::{system_tz, today_local};
use karmayoga_core::store::calendars::EventQuery;
use karmayoga_core::{AccountId, CalendarId, EventId, Vault};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

use super::Nothing;

/// A calendar and how much is in it.
///
/// A named struct rather than widening `Calendar` itself: the count is a fact
/// about storage at this instant, not a property of the subscription, and
/// putting it on the record would mean every sync had to remember to update it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarInfo {
    #[serde(flatten)]
    pub calendar: Calendar,
    pub events: u64,
    /// Whether a new event can go on it -- a fact about the calendar *and*
    /// its account's sign-in together, worked out once here rather than by
    /// every picker that offers it.
    pub access: CalendarAccess,
}

/// Whether new events can go on `calendar`, given the accounts it might
/// belong to.
pub(crate) fn access_of(calendar: &Calendar, accounts: &[Account]) -> CalendarAccess {
    let CalendarOrigin::Account { account_id, .. } = &calendar.origin else {
        return CalendarAccess::ReadOnly;
    };
    if calendar.read_only {
        return CalendarAccess::ReadOnly;
    }
    match accounts.iter().find(|a| a.id == *account_id) {
        Some(account) if !account.can_write_calendars() => CalendarAccess::NeedsSignIn,
        Some(_) => CalendarAccess::Writable,
        None => CalendarAccess::ReadOnly,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    /// Where, in that product, the secret address is found.
    pub hint: String,
}

impl ProviderInfo {
    fn of(p: CalendarProvider) -> Self {
        let (label, hint) = match p {
            CalendarProvider::Google => (
                "Google Calendar",
                "Settings \u{2192} your calendar \u{2192} Integrate calendar \u{2192} \
                 Secret address in iCal format.",
            ),
            CalendarProvider::Outlook => (
                "Outlook",
                "Settings \u{2192} Calendar \u{2192} Shared calendars \u{2192} \
                 Publish a calendar, then copy the ICS link.",
            ),
            CalendarProvider::Apple => (
                "Apple Calendar",
                "iCloud.com \u{2192} Calendar \u{2192} the share icon beside a calendar \
                 \u{2192} Public Calendar, then copy the link.",
            ),
            CalendarProvider::Other => (
                "Another calendar",
                "Any address publishing an iCalendar (.ics) feed \u{2014} a team calendar, \
                 a fixture list, your country\u{2019}s public holidays.",
            ),
        };
        Self { id: p.as_str().to_string(), label: label.into(), hint: hint.into() }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCalendar {
    pub calendar: Calendar,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarRef {
    pub id: CalendarId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Events {
    pub query: EventQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRef {
    pub id: EventId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncDue {
    pub force: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultCalendar {
    /// `None` sends new events to this computer's own time blocks.
    #[serde(default)]
    pub id: Option<CalendarId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThisComputer {
    /// Whether this computer is a calendar new events can go to.
    pub on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEvent {
    pub calendar_id: CalendarId,
    pub draft: EventDraft,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEvent {
    pub id: EventId,
    pub draft: EventDraft,
    #[serde(default)]
    pub scope: EventScope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteEvent {
    pub id: EventId,
    #[serde(default)]
    pub scope: EventScope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscribe {
    pub name: String,
    pub url: String,
    pub color: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Import {
    pub name: String,
    pub label: String,
    pub color: String,
    pub ics: String,
}

async fn list_calendars(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<CalendarInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        let calendars = vault.calendars()?;
        // Only read when some calendar is an account's -- a vault of feeds
        // alone has no accounts to decrypt, and a backend without the
        // accounts domain at all must still list its calendars.
        let accounts = if calendars.iter().any(|c| c.origin.account_id().is_some()) {
            vault.accounts().unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut out = Vec::new();
        for calendar in calendars {
            // The count is a `COUNT(*)` over a clear index column, so listing
            // four calendars decrypts four records and nothing else.
            let events = vault.event_count(calendar.id).unwrap_or(0);
            let access = access_of(&calendar, &accounts);
            out.push(CalendarInfo { calendar, events, access });
        }
        Ok(out)
    })
    .await
}

/// Choose where new events go: one of an account's calendars, or -- with no
/// id -- this computer's own time blocks.
async fn set_default_calendar(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: DefaultCalendar,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.set_default_calendar(args.id)).await
}

/// Switch this computer on or off as a calendar new events can go to. Off
/// only while they go to an account's calendar -- see
/// `Vault::set_this_computer`.
async fn set_this_computer(svc: Arc<Service>, _ctx: Ctx, args: ThisComputer) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.set_this_computer(args.on)).await
}

/// Put a new event on one of an account's calendars, and sync it back.
///
/// Answers the event as the sync stored it, so the interface can select it;
/// `None` when the server took it but the sync has not brought it back yet.
async fn create_event(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: CreateEvent,
) -> CommandResult<Option<Event>> {
    let vault = svc.require()?;
    accountcal::create_event(&svc, &vault, args.calendar_id, &args.draft).await
}

/// Read an account calendar's event fresh from its server, to be changed.
async fn load_event(svc: Arc<Service>, _ctx: Ctx, args: EventRef) -> CommandResult<EditableEvent> {
    let vault = svc.require()?;
    let event = stored_event(&vault, args.id).await?;
    accountcal::load_event(&svc, &vault, &event).await
}

async fn update_event(svc: Arc<Service>, _ctx: Ctx, args: UpdateEvent) -> CommandResult<()> {
    let vault = svc.require()?;
    let event = stored_event(&vault, args.id).await?;
    accountcal::update_event(&svc, &vault, &event, &args.draft, args.scope).await
}

async fn delete_event(svc: Arc<Service>, _ctx: Ctx, args: DeleteEvent) -> CommandResult<()> {
    let vault = svc.require()?;
    let event = stored_event(&vault, args.id).await?;
    accountcal::delete_event(&svc, &vault, &event, args.scope).await
}

async fn stored_event(vault: &Arc<Vault>, id: EventId) -> CommandResult<Event> {
    let vault = vault.clone();
    blocking(move || Ok(vault.event(id)?)).await
}

/// The assistant's way to an account's calendar -- [`karmayoga_core::agent::
/// tools::CalendarWriter`], over exactly the functions the four commands
/// above call, so `create_event` from chat and a click in the calendar run
/// the same code.
///
/// A tool body runs on the blocking pool (see `agent::run_tool` and
/// `meta::run_tool`), and these are async, so each call blocks this thread
/// on the runtime that dispatched it. Errors become the core's own type on
/// the way out, the way `domains::mail::respond_to_invite_for_tool`'s do,
/// because the hook answers inside the core.
pub(crate) struct ToolCalendarWriter {
    svc: Arc<Service>,
    runtime: tokio::runtime::Handle,
}

impl ToolCalendarWriter {
    /// Built on the blocking thread a tool is about to run on, which has the
    /// runtime's context entered -- `Handle::current` finds it there.
    pub(crate) fn new(svc: Arc<Service>) -> Self {
        Self { svc, runtime: tokio::runtime::Handle::current() }
    }

    fn vault(&self) -> karmayoga_core::error::Result<Arc<Vault>> {
        self.svc.require().map_err(to_core_error)
    }

    fn event(&self, id: EventId) -> karmayoga_core::error::Result<(Arc<Vault>, Event)> {
        let vault = self.vault()?;
        let event = vault.event(id)?;
        Ok((vault, event))
    }

    /// Tell every open window the calendar moved, the way the commands'
    /// own `change:` rows would -- a tool call has no command row of its
    /// own to raise one. `written` in `agent.rs` raises the event change;
    /// this is the calendar list, whose counts moved too.
    fn changed(&self, id: Option<EventId>) {
        self.svc.events().changed(crate::events::Change {
            kind: crate::events::Kind::Event,
            op: crate::events::Op::Updated,
            id: id.map(|id| id.to_string()),
            ids: Vec::new(),
            origin: None,
        });
    }
}

fn to_core_error(e: CommandError) -> karmayoga_core::error::Error {
    karmayoga_core::error::Error::Invalid(e.message)
}

impl karmayoga_core::agent::tools::CalendarWriter for ToolCalendarWriter {
    fn create(
        &self,
        calendar: CalendarId,
        draft: &EventDraft,
    ) -> karmayoga_core::error::Result<Option<Event>> {
        let vault = self.vault()?;
        let created = self
            .runtime
            .block_on(accountcal::create_event(&self.svc, &vault, calendar, draft))
            .map_err(to_core_error)?;
        self.changed(created.as_ref().map(|e| e.id));
        Ok(created)
    }

    fn load(&self, id: EventId) -> karmayoga_core::error::Result<EditableEvent> {
        let (vault, event) = self.event(id)?;
        self.runtime
            .block_on(accountcal::load_event(&self.svc, &vault, &event))
            .map_err(to_core_error)
    }

    fn update(
        &self,
        id: EventId,
        draft: &EventDraft,
        scope: EventScope,
    ) -> karmayoga_core::error::Result<()> {
        let (vault, event) = self.event(id)?;
        self.runtime
            .block_on(accountcal::update_event(&self.svc, &vault, &event, draft, scope))
            .map_err(to_core_error)?;
        self.changed(Some(id));
        Ok(())
    }

    fn delete(&self, id: EventId, scope: EventScope) -> karmayoga_core::error::Result<()> {
        let (vault, event) = self.event(id)?;
        self.runtime
            .block_on(accountcal::delete_event(&self.svc, &vault, &event, scope))
            .map_err(to_core_error)?;
        self.changed(Some(id));
        Ok(())
    }
}

async fn save_calendar(svc: Arc<Service>, _ctx: Ctx, args: SaveCalendar) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_calendar(&args.calendar)).await
}

/// Unsubscribe: the calendar and every event that came from it.
async fn delete_calendar(svc: Arc<Service>, _ctx: Ctx, args: CalendarRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_calendar(args.id)).await
}

async fn list_events(svc: Arc<Service>, _ctx: Ctx, args: Events) -> CommandResult<Vec<Event>> {
    svc.on_vault(move |vault| vault.events(&args.query)).await
}

async fn get_event(svc: Arc<Service>, _ctx: Ctx, args: EventRef) -> CommandResult<Event> {
    svc.on_vault(move |vault| vault.event(args.id)).await
}

/// Fetch one calendar's feed and replace its events with what came back.
///
/// The failure path is as deliberate as the success one. A feed that cannot be
/// fetched, or that answers with something that is not a calendar, leaves the
/// events already stored exactly where they are and records *why* on the
/// subscription -- because the alternative, a calendar that empties itself when
/// the wifi drops, is the one failure of an automatic sync that people notice
/// and never forgive.
async fn sync_calendar(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: CalendarRef,
) -> CommandResult<SyncReport> {
    let vault = svc.require()?;
    let report = sync_telling_about_new_events(&svc, &vault, args.id).await?;
    // A hand-driven refresh that works ends the outage as much as a background
    // one does, so the next failure is news again. Without this, a feed fixed
    // from the sidebar would never notify a second time.
    svc.feed_recovered(args.id);
    Ok(report)
}

/// Refresh what is due, or with `force` everything: the Refresh button. The
/// background refresher does the same a calendar at a time, on its own
/// schedule -- see [`crate::calendar_refresh`].
async fn sync_due_calendars(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SyncDue,
) -> CommandResult<Vec<SyncReport>> {
    let vault = svc.require()?;
    Ok(sync_due(&svc, &vault, args.force).await?.reports)
}

/// What one pass over the calendars came to.
#[derive(Default)]
pub(crate) struct DuePass {
    pub reports: Vec<SyncReport>,
    /// Somebody looking at the calendar would see something different now: an
    /// event came or went, or one of them started or stopped failing.
    pub changed: bool,
}

/// How many calendars sync at once. Each waits on its own server.
pub(crate) const SYNC_AT_ONCE: usize = 4;

/// How recently a calendar with an alert coming up must have synced for it to
/// be left be. See [`due_calendars`]'s `soon`.
const CHECKED_WITHIN_SECS: i64 = 2 * 60;

/// A calendar about to sync, and what is needed to say anything about it after.
pub(crate) struct DueCalendar {
    pub id: CalendarId,
    name: String,
    account: Option<AccountId>,
    error_before: Option<String>,
    /// Asked for by hand: nothing is notified about it, since whoever asked is
    /// looking at the calendar.
    force: bool,
}

/// The subscriptions -- a feed's URL or an account's own calendars -- to sync
/// now: every one with `force`, and otherwise each whose refresh interval has
/// elapsed, and each in `soon` -- with a reminder or a meeting coming round in
/// the next few minutes, see [`Service::calendars_alerting_soon`] -- that has
/// not synced in the last [`CHECKED_WITHIN_SECS`], so a meeting moved since
/// the last refresh is reminded at its new time rather than its old one.
/// Neither for a calendar refreshed only by hand, nor one that failed less
/// than a few minutes ago (see [`Service::feed_retry_due`]), nor one whose
/// account waits for somebody to sign in again: asking on its behalf would
/// send the dead token to the provider for as long as nobody did, and signing
/// in moves it straight back to `Ok`, so nothing is missed.
///
/// `skip` are calendars already syncing. Only the writable vault holder syncs:
/// a read-only vault cannot store what a sync fetches, nor record a failure.
pub(crate) fn due_calendars(
    svc: &Service,
    vault: &Vault,
    force: bool,
    soon: &HashSet<CalendarId>,
    skip: &HashSet<CalendarId>,
) -> karmayoga_core::error::Result<Vec<DueCalendar>> {
    if !vault.is_writable() {
        return Ok(Vec::new());
    }
    let now = svc.now();
    let wanted = |c: &Calendar| {
        if force {
            return true;
        }
        if c.refresh_minutes == 0 || !svc.feed_retry_due(c.id, now) {
            return false;
        }
        let checked = c
            .last_synced_at
            .is_some_and(|at| now.as_second() - at.as_second() < CHECKED_WITHIN_SECS);
        (soon.contains(&c.id) && !checked) || c.is_due(now)
    };
    let candidates: Vec<Calendar> = vault
        .calendars()?
        .into_iter()
        .filter(|c| c.origin.is_syncable() && !skip.contains(&c.id) && wanted(c))
        .collect();
    // Each account read once, and only for a calendar that is otherwise due.
    let mut waiting: std::collections::HashMap<AccountId, bool> = Default::default();
    let mut due: Vec<DueCalendar> = candidates
        .into_iter()
        .filter(|c| {
            force
                || c.origin.account_id().is_none_or(|id| {
                    !*waiting.entry(id).or_insert_with(|| {
                        vault
                            .account(id)
                            .is_ok_and(|a| matches!(a.status, AccountStatus::NeedsSignIn { .. }))
                    })
                })
        })
        .map(|c| DueCalendar {
            id: c.id,
            account: c.origin.account_id(),
            name: c.name,
            error_before: c.last_error,
            force,
        })
        .collect();
    // A reminder's calendar first, when only so many may sync at once.
    due.sort_by_key(|c| !soon.contains(&c.id));
    Ok(due)
}

/// Sync one calendar a pass chose. See [`settle`] for what is said after.
pub(crate) async fn sync_chosen(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    calendar: DueCalendar,
) -> (DueCalendar, CommandResult<SyncReport>) {
    let result = sync_telling_about_new_events(&svc, &vault, calendar.id).await;
    (calendar, result)
}

/// What one calendar's sync comes to, after it: whether somebody looking at
/// the calendar would now see something different -- an event came or went,
/// or it started or stopped failing.
///
/// A failure is already recorded on the subscription, and the calendar
/// sidebar shows it there, beside the calendar it belongs to. That is the right
/// place for it -- but only if you are looking at the calendar. This runs on a
/// timer whoever is using the app, and what it brings in is what reminders are
/// sent from, so a calendar that has quietly stopped is a meeting reminded at
/// the time it used to be. So once its outage has lasted (see
/// [`Service::feed_failed`]) it is said -- never for a refresh asked for by
/// hand, whose asker has just seen it in the sidebar, nor for an account that
/// needs signing in, which [`crate::trouble`] has said already, once.
pub(crate) fn settle(
    svc: &Service,
    vault: &Vault,
    calendar: &DueCalendar,
    result: &CommandResult<SyncReport>,
) -> bool {
    match result {
        Ok(report) => {
            svc.feed_recovered(calendar.id);
            if let Some(account) = calendar.account {
                crate::trouble::calendar_synced(svc, account);
            }
            report.events > 0 || report.removed > 0 || calendar.error_before.is_some()
        }
        Err(e) => {
            // Locked, or removed while this sync was out: not a calendar that
            // has stopped refreshing, and nothing to say about it.
            let Ok(after) = vault.calendar(calendar.id) else { return false };
            tracing::info!(id = %calendar.id, error = %e, "a calendar could not be refreshed");
            let unreachable = e.code == codes::UNREACHABLE;
            if !calendar.force
                && e.code != codes::FORBIDDEN
                && svc.feed_failed(calendar.id, svc.now(), unreachable)
            {
                tell_stopped(svc, vault, calendar);
            }
            after.last_error != calendar.error_before
        }
    }
}

/// Every calendar due now -- see [`due_calendars`] -- synced, a few at once,
/// and what came of them: the Refresh button, with `force`, and older paired
/// windows that still poll.
pub(crate) async fn sync_due(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    force: bool,
) -> CommandResult<DuePass> {
    let due = {
        let (svc, vault) = (svc.clone(), vault.clone());
        blocking(move || Ok(due_calendars(&svc, &vault, force, &HashSet::new(), &HashSet::new())?))
            .await?
    };
    let synced: Vec<_> = futures::stream::iter(due)
        .map(|calendar| sync_chosen(svc.clone(), vault.clone(), calendar))
        .buffer_unordered(SYNC_AT_ONCE)
        .collect()
        .await;
    let mut out = DuePass::default();
    for (calendar, result) in synced {
        out.changed |= settle(svc, vault, &calendar, &result);
        if let Ok(report) = result {
            out.reports.push(report);
        }
    }
    Ok(out)
}

/// Say that `stopped` has stopped refreshing. An account's calendars share one
/// notification, naming every one of them whose outage has been said and is
/// still going on, so a second calendar of the same account going down
/// replaces the first's warning with one that names both rather than one that
/// forgets the first. A feed has its own.
fn tell_stopped(svc: &Service, vault: &Vault, stopped: &DueCalendar) {
    let (title, key, many) = match stopped.account {
        None => {
            (format!("{} is not refreshing", stopped.name), format!("feed:{}", stopped.id), false)
        }
        Some(account) => {
            let said: HashSet<CalendarId> =
                svc.sync_trouble().calendars().said_keys().into_iter().collect();
            let mut names: Vec<String> = vault
                .calendars()
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.id != stopped.id && said.contains(&c.id))
                .filter(|c| c.origin.account_id() == Some(account))
                .map(|c| c.name)
                .collect();
            names.push(stopped.name.clone());
            let title = match &names[..] {
                [one] => format!("{one} is not refreshing"),
                many => {
                    let from = vault.account(account).map(|a| a.address).unwrap_or_default();
                    format!("{} calendars from {from} are not refreshing", many.len())
                }
            };
            (title, format!("calendars:{account}"), names.len() > 1)
        }
    };
    let whose = if many { "Their" } else { "Its" };
    svc.events().notify(
        Notification::warning(title)
            // Deliberately not the error's own words: a feed URL is a bearer
            // credential and error text from a fetch can quote it. The
            // interface shows the recorded detail beside the calendar, where
            // the person reading it already has the address.
            .body(format!(
                "{whose} events may be out of date, so a meeting moved since may still be \
                 reminded at its old time. Open the calendar for details."
            ))
            .for_user()
            .key(key),
    );
    svc.feed_said(stopped.id);
}

/// Sync one calendar, and say so when the sync added an event the calendar
/// did not already hold.
///
/// The first sync of a subscription is its history -- every event would
/// look new -- and a hidden calendar is one the person has asked not to be
/// shown. Both stay quiet. A snapshot that cannot be read stays quiet too:
/// announcing against an empty "before" would be the first-sync flood again.
///
/// The snapshot is taken before the network round-trip, so an event written
/// from here while the sync is out (or brought back by a write's own
/// resync) is missing from it. `arrivals::select_fresh` leaves the
/// account's own events out for exactly that reason.
///
/// The calendar is read after waiting for the turn, not before: a sync that
/// had to wait for another has just moved this calendar's sync token.
async fn sync_telling_about_new_events(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    id: CalendarId,
) -> CommandResult<SyncReport> {
    let turn = svc.calendar_sync_turn(id);
    let _turn = turn.lock().await;
    let calendar = &vault.calendar(id)?;
    let before = if calendar.visible && calendar.last_synced_at.is_some() {
        let (svc, vault, calendar) = (svc.clone(), vault.clone(), calendar.clone());
        blocking(move || Ok(crate::arrivals::event_identities(&vault, &calendar, svc.now())?))
            .await
            .ok()
    } else {
        None
    };
    let report = sync_dispatch(svc, vault, calendar).await?;
    if let Some(before) = before {
        let svc = svc.clone();
        let vault = vault.clone();
        let calendar = calendar.clone();
        let _ = blocking(move || {
            crate::arrivals::announce_fresh_events(&svc, &vault, &calendar, &before);
            Ok(())
        })
        .await;
    }
    Ok(report)
}

/// One calendar's sync, whichever kind of subscription it is.
///
/// The split this dispatches over is exactly [`CalendarOrigin`]'s: a `Url`
/// goes through `feeds.rs`, an `Account` through `accountcal::sync`, and a
/// `File` is never `is_due`, so it never reaches here at all -- both
/// [`sync_calendar`] and [`sync_due`] go through
/// [`sync_telling_about_new_events`], which calls this, so the choice is
/// made in exactly one place.
async fn sync_dispatch(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    calendar: &Calendar,
) -> CommandResult<SyncReport> {
    match &calendar.origin {
        CalendarOrigin::Url { .. } => sync_one(vault, calendar.id).await,
        CalendarOrigin::Account { .. } => accountcal::sync(svc, vault, calendar).await,
        CalendarOrigin::File { .. } => {
            Err(CommandError::new(codes::INVALID, "a file calendar has nothing to sync"))
        }
    }
}

async fn sync_one(vault: &Arc<Vault>, id: CalendarId) -> CommandResult<SyncReport> {
    let url = vault.calendar(id)?.fetch_url()?;
    let text = match feeds::fetch(&url).await {
        Ok(text) => text,
        Err(e) => {
            record_failure(vault, id, &e.message);
            return Err(e);
        }
    };
    apply_feed(vault, id, text).await
}

/// Hand fetched text to the core, on the blocking pool.
///
/// Parsing a year of a busy calendar, expanding its recurrences and sealing a
/// few thousand events is real CPU work, and doing it on an async worker would
/// stall every other command for the duration of a sync that is meant to be
/// invisible.
async fn apply_feed(vault: &Arc<Vault>, id: CalendarId, text: String) -> CommandResult<SyncReport> {
    let window = feeds::sync_window(today_local());
    let tz = system_tz();
    let v = vault.clone();
    let outcome = blocking(move || Ok(v.sync_calendar_from_ics(id, &text, window, &tz))).await?;
    match outcome {
        Ok(report) => Ok(report),
        Err(e) => {
            let message = e.to_string();
            record_failure(vault, id, &message);
            Err(CommandError::from(e))
        }
    }
}

/// Note on the subscription why the last attempt did not work.
fn record_failure(vault: &Arc<Vault>, id: CalendarId, why: &str) {
    if let Err(e) = vault.mark_calendar_failed(id, why) {
        tracing::warn!(%id, error = %e, "could not record the calendar sync failure");
    }
}

/// Subscribe to a feed and fetch it once, so the calendar appears with its
/// events already in it rather than empty and pending.
///
/// One command rather than three round trips, because the three are not
/// independent: a subscription that saved and then failed to fetch would leave
/// a calendar in the sidebar that the user has to work out how to remove, and
/// the honest answer to "this address is not a calendar" is to have added
/// nothing at all.
async fn subscribe_calendar(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Subscribe,
) -> CommandResult<CalendarInfo> {
    let vault = svc.require()?;
    let url = karmayoga_core::calendar::normalize_feed_url(&args.url)?;
    let calendar = Calendar::subscribed(args.name.trim(), &url).with_color(args.color);
    let text = feeds::fetch(&url).await?;
    add_calendar(&vault, calendar, text, "Calendar").await
}

/// Add a calendar from a `.ics` file the user chose.
///
/// The text arrives from the caller, which read it with the browser's own file
/// API. No path crosses the boundary and nothing on disk is opened by this
/// process, so "import a calendar" cannot be talked into reading a file the
/// user did not pick.
async fn import_calendar(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Import,
) -> CommandResult<CalendarInfo> {
    let vault = svc.require()?;
    let calendar = Calendar::imported(args.name.trim(), args.label).with_color(args.color);
    add_calendar(&vault, calendar, args.ics, "Imported calendar").await
}

/// Save a new calendar, fill it from `text`, and add nothing at all if that
/// does not work.
///
/// Both ways in -- a subscription and an imported file -- do exactly this once
/// they have the iCalendar text in hand, and they differ only in where the text
/// came from and what to call the result when the document does not name
/// itself. Keeping the shared half here is what makes the all-or-nothing
/// guarantee in `subscribe_calendar`'s doc comment one piece of code rather
/// than two that have to be kept in agreement.
async fn add_calendar(
    vault: &Arc<Vault>,
    mut calendar: Calendar,
    text: String,
    fallback_name: &str,
) -> CommandResult<CalendarInfo> {
    // Name it after the publisher when the person adding it did not: Google,
    // Outlook and Apple all set `X-WR-CALNAME`, and "Priya - Work" is a better
    // name than anything a text field would have got out of someone in a hurry.
    if calendar.name.is_empty() {
        calendar.name = karmayoga_core::ics::parse(&text)
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| fallback_name.to_string());
    }
    let id = calendar.id;
    {
        let vault = vault.clone();
        let calendar = calendar.clone();
        blocking(move || Ok(vault.save_calendar(&calendar)?)).await?;
    }

    match apply_feed(vault, id, text).await {
        Ok(report) => Ok(info_after_adding(vault, id, report.events)?),
        Err(e) => {
            // Nothing added: see `subscribe_calendar`. Undoing the save is safe
            // because nothing else can have pointed at it yet.
            let _ = vault.delete_calendar(id);
            Err(e)
        }
    }
}

/// One calendar an account offers, and whether this vault already has it.
///
/// `calendar_id` is `Some` exactly when `subscribed` is true: the interface
/// needs it to draw the unsubscribe action against the calendar that is
/// already here, rather than the remote listing that produced it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCalendarInfo {
    #[serde(flatten)]
    pub remote: RemoteCalendar,
    pub subscribed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar_id: Option<CalendarId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRef {
    pub account: AccountId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeAccountCalendar {
    pub account: AccountId,
    pub remote_id: String,
}

/// List the calendars `account` offers, marking which ones this vault
/// already subscribes to.
///
/// Discovery happens every time this is called rather than being cached --
/// the "add a calendar → from an account" sheet is opened rarely enough
/// that a fresh CalDAV PROPFIND or an API list is cheap next to the
/// confusion of showing a calendar the account no longer has, or hiding one
/// it just gained.
async fn list_account_calendars(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AccountRef,
) -> CommandResult<Vec<RemoteCalendarInfo>> {
    let vault = svc.require()?;
    let account_id = args.account;
    let account = {
        let vault = vault.clone();
        blocking(move || Ok(vault.account(account_id)?)).await?
    };
    let remotes = accountcal::discover(&svc, &vault, &account).await?;
    let existing = {
        let vault = vault.clone();
        blocking(move || Ok(vault.account_calendars(account_id)?)).await?
    };
    // Discovery is also the cheapest moment to learn whether each calendar
    // already here can still be written to -- a shared calendar whose owner
    // took away edit rights since it was subscribed to.
    {
        let updates: Vec<(CalendarId, bool)> = existing
            .iter()
            .filter_map(|c| match &c.origin {
                CalendarOrigin::Account { remote_id, .. } => remotes
                    .iter()
                    .find(|r| r.remote_id == *remote_id)
                    .filter(|r| r.writable == c.read_only)
                    .map(|r| (c.id, !r.writable)),
                _ => None,
            })
            .collect();
        if !updates.is_empty() && vault.is_writable() {
            let vault = vault.clone();
            blocking(move || {
                for (id, read_only) in updates {
                    vault.set_calendar_read_only(id, read_only)?;
                }
                Ok(())
            })
            .await?;
        }
    }
    Ok(remotes
        .into_iter()
        .map(|remote| {
            let matched = existing.iter().find(|c| match &c.origin {
                CalendarOrigin::Account { remote_id, .. } => *remote_id == remote.remote_id,
                _ => false,
            });
            RemoteCalendarInfo {
                remote,
                subscribed: matched.is_some(),
                calendar_id: matched.map(|c| c.id),
            }
        })
        .collect())
}

/// Subscribe to one of an account's calendars and fetch it once, on the same
/// all-or-nothing terms [`subscribe_calendar`] promises for a feed: a
/// calendar that fails its first sync is not left behind half-added.
async fn subscribe_account_calendar(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SubscribeAccountCalendar,
) -> CommandResult<CalendarInfo> {
    let vault = svc.require()?;
    let account_id = args.account;
    let account = {
        let vault = vault.clone();
        blocking(move || Ok(vault.account(account_id)?)).await?
    };
    let remote = accountcal::discover(&svc, &vault, &account)
        .await?
        .into_iter()
        .find(|r| r.remote_id == args.remote_id)
        .ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "that calendar is no longer offered by this account",
            )
        })?;

    let mut calendar = Calendar::from_account(
        account.id,
        account.provider,
        remote.source,
        remote.remote_id.clone(),
        remote.name.clone(),
    );
    if let Some(color) = remote.color {
        calendar.color = color;
    }
    let id = calendar.id;
    {
        let vault = vault.clone();
        let calendar = calendar.clone();
        let read_only = !remote.writable;
        blocking(move || {
            vault.save_calendar(&calendar)?;
            Ok(vault.set_calendar_read_only(calendar.id, read_only)?)
        })
        .await?;
    }

    // Saved a moment ago and never synced, so already due as far as the
    // background pass can see.
    let turn = svc.calendar_sync_turn(id);
    let _turn = turn.lock().await;
    match accountcal::sync(&svc, &vault, &calendar).await {
        Ok(report) => Ok(info_after_adding(&vault, id, report.events)?),
        Err(e) => {
            // See `subscribe_calendar`: nothing added is the honest answer
            // to a first sync that did not work, and undoing the save is
            // safe because nothing else can have pointed at this calendar
            // yet.
            let _ = vault.delete_calendar(id);
            Err(e)
        }
    }
}

/// A calendar just added, as `list_calendars` would describe it.
fn info_after_adding(
    vault: &Arc<Vault>,
    id: CalendarId,
    events: u64,
) -> CommandResult<CalendarInfo> {
    let calendar = vault.calendar(id)?;
    let accounts = match calendar.origin.account_id() {
        Some(account) => vec![vault.account(account)?],
        None => Vec::new(),
    };
    let access = access_of(&calendar, &accounts);
    Ok(CalendarInfo { calendar, events, access })
}

/// The providers the "add a calendar" sheet offers, with where to find the
/// address for each.
///
/// Held in Rust rather than hard-coded in a client because the guess that picks
/// a provider from a pasted URL lives here too, and the two have to agree about
/// what the set is.
async fn calendar_providers(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<ProviderInfo>> {
    let _ = svc.require()?;
    Ok(CalendarProvider::ALL.iter().map(|p| ProviderInfo::of(*p)).collect())
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_calendars", scope: Calendars, effect: Read,
        args: Nothing, returns: "CalendarInfo[]", signature: &[],
        run: list_calendars,
    },
    command! {
        name: "save_calendar", scope: Calendars, effect: Write,
        change: Calendar / Updated,
        id: |a: &SaveCalendar| Some(a.calendar.id.to_string()),
        args: SaveCalendar, returns: "void",
        signature: &[("calendar", "Calendar", true)],
        run: save_calendar,
    },
    command! {
        name: "delete_calendar", scope: Calendars, effect: Destructive,
        change: Calendar / Deleted,
        id: |a: &CalendarRef| Some(a.id.to_string()),
        args: CalendarRef, returns: "void",
        signature: &[("id", "CalendarId", true)],
        run: delete_calendar,
    },
    command! {
        name: "set_default_calendar", scope: Calendars, effect: Write,
        change: Calendar / Updated,
        id: |a: &DefaultCalendar| a.id.map(|id| id.to_string()),
        args: DefaultCalendar, returns: "void",
        signature: &[("id", "CalendarId | null", false)],
        run: set_default_calendar,
    },
    command! {
        name: "set_this_computer", scope: Calendars, effect: Write,
        change: Calendar / Updated,
        args: ThisComputer, returns: "void",
        signature: &[("on", "boolean", true)],
        run: set_this_computer,
    },
    command! {
        name: "create_event", scope: Calendars, effect: Write,
        change: Event / Created,
        args: CreateEvent, returns: "CalendarEvent | null",
        signature: &[("calendarId", "CalendarId", true), ("draft", "EventDraft", true)],
        run: create_event,
    },
    command! {
        name: "load_event", scope: Calendars, effect: Read,
        args: EventRef, returns: "EditableEvent",
        signature: &[("id", "EventId", true)],
        run: load_event,
    },
    command! {
        name: "update_event", scope: Calendars, effect: Write,
        change: Event / Updated,
        id: |a: &UpdateEvent| Some(a.id.to_string()),
        args: UpdateEvent, returns: "void",
        signature: &[
            ("id", "EventId", true),
            ("draft", "EventDraft", true),
            ("scope", "EventScope", false),
        ],
        run: update_event,
    },
    command! {
        name: "delete_event", scope: Calendars, effect: Destructive,
        change: Event / Deleted,
        id: |a: &DeleteEvent| Some(a.id.to_string()),
        args: DeleteEvent, returns: "void",
        signature: &[("id", "EventId", true), ("scope", "EventScope", false)],
        run: delete_event,
    },
    command! {
        name: "list_events", scope: Calendars, effect: Read,
        args: Events, returns: "CalendarEvent[]",
        signature: &[("query", "EventQuery", true)],
        run: list_events,
    },
    command! {
        name: "get_event", scope: Calendars, effect: Read,
        args: EventRef, returns: "CalendarEvent",
        signature: &[("id", "EventId", true)],
        run: get_event,
    },
    command! {
        name: "sync_calendar", scope: Calendars, effect: Write,
        change: Event / Updated,
        args: CalendarRef, returns: "SyncReport",
        signature: &[("id", "CalendarId", true)],
        run: sync_calendar,
    },
    command! {
        name: "sync_due_calendars", scope: Calendars, effect: Write,
        change: Event / Updated,
        args: SyncDue, returns: "SyncReport[]",
        signature: &[("force", "boolean", true)],
        run: sync_due_calendars,
    },
    command! {
        name: "subscribe_calendar", scope: Calendars, effect: Write,
        change: Calendar / Created,
        args: Subscribe, returns: "CalendarInfo",
        signature: &[("name", "string", true), ("url", "string", true), ("color", "string", true)],
        run: subscribe_calendar,
    },
    command! {
        name: "import_calendar", scope: Calendars, effect: Write,
        change: Calendar / Created,
        args: Import, returns: "CalendarInfo",
        signature: &[
            ("name", "string", true),
            ("label", "string", true),
            ("color", "string", true),
            ("ics", "string", true),
        ],
        run: import_calendar,
    },
    command! {
        name: "calendar_providers", scope: Calendars, effect: Read,
        args: Nothing, returns: "ProviderInfo[]", signature: &[],
        run: calendar_providers,
    },
    command! {
        name: "list_account_calendars", scope: Calendars, effect: Read,
        args: AccountRef, returns: "RemoteCalendarInfo[]",
        signature: &[("account", "AccountId", true)],
        run: list_account_calendars,
    },
    command! {
        name: "subscribe_account_calendar", scope: Calendars, effect: Write,
        change: Calendar / Created,
        args: SubscribeAccountCalendar, returns: "CalendarInfo",
        signature: &[("account", "AccountId", true), ("remoteId", "string", true)],
        run: subscribe_account_calendar,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventSink, Notification};
    use jiff::Timestamp;
    use karmayoga_core::account::Provider;
    use karmayoga_core::calendar::AccountCalendarSource;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Told(Mutex<Vec<Notification>>);

    impl EventSink for Told {
        fn notify(&self, notification: Notification) {
            self.0.lock().unwrap().push(notification);
        }
    }

    struct Hands(Mutex<Timestamp>);

    impl crate::clock::Clock for Hands {
        fn now(&self) -> Timestamp {
            *self.0.lock().unwrap()
        }
        fn instant(&self) -> std::time::Instant {
            std::time::Instant::now()
        }
    }

    fn env() -> (Arc<Service>, Arc<Vault>, Arc<Told>, Arc<Hands>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(
            dir.path(),
            karmayoga_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        let told = Arc::new(Told::default());
        svc.set_events(told.clone());
        let hands = Arc::new(Hands(Mutex::new(Timestamp::now())));
        svc.set_clock(hands.clone());
        (svc, vault, told, hands, dir)
    }

    fn account_calendar(vault: &Vault, account: AccountId, name: &str) -> Calendar {
        let calendar = Calendar::from_account(
            account,
            Provider::Google,
            AccountCalendarSource::Google,
            name,
            name,
        );
        vault.save_calendar(&calendar).unwrap();
        calendar
    }

    fn chosen(calendar: &Calendar) -> DueCalendar {
        DueCalendar {
            id: calendar.id,
            name: calendar.name.clone(),
            account: calendar.origin.account_id(),
            error_before: None,
            force: false,
        }
    }

    fn refused() -> CommandResult<SyncReport> {
        Err(CommandError::new(codes::INVALID, "the server said no"))
    }

    /// Two calendars of one account that stop share one warning, which names
    /// both: the second does not replace the first's with one that forgets it.
    #[test]
    fn an_accounts_stopped_calendars_are_named_together() {
        let (svc, vault, told, hands, _dir) = env();
        let account = karmayoga_core::account::Account::new(Provider::Google, "x@example.com");
        vault.save_account(&account).unwrap();
        let work = account_calendar(&vault, account.id, "Work");
        let home = account_calendar(&vault, account.id, "Home");

        settle(&svc, &vault, &chosen(&work), &refused());
        *hands.0.lock().unwrap() += jiff::SignedDuration::from_mins(5);
        settle(&svc, &vault, &chosen(&home), &refused());
        assert!(told.0.lock().unwrap().is_empty(), "too soon");

        *hands.0.lock().unwrap() += jiff::SignedDuration::from_mins(10);
        settle(&svc, &vault, &chosen(&work), &refused());
        *hands.0.lock().unwrap() += jiff::SignedDuration::from_mins(5);
        settle(&svc, &vault, &chosen(&home), &refused());

        let told = told.0.lock().unwrap();
        let titles: Vec<_> = told.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Work is not refreshing", "2 calendars from x@example.com are not refreshing"]
        );
        assert_eq!(told[0].key, told[1].key, "the second replaces the first");
    }

    /// A calendar refreshed only by hand is not fetched before a reminder,
    /// and one that just failed waits its turn.
    #[test]
    fn a_reminder_does_not_fetch_a_manual_or_just_failed_calendar() {
        let (svc, vault, _told, _hands, _dir) = env();
        let mut account = karmayoga_core::account::Account::new(Provider::Google, "x@example.com");
        account.status = AccountStatus::Ok;
        vault.save_account(&account).unwrap();
        let mut manual = account_calendar(&vault, account.id, "Manual");
        manual.refresh_minutes = 0;
        vault.save_calendar(&manual).unwrap();
        let failing = account_calendar(&vault, account.id, "Failing");
        let fine = account_calendar(&vault, account.id, "Fine");
        settle(&svc, &vault, &chosen(&failing), &refused());

        let soon = HashSet::from([manual.id, failing.id, fine.id]);
        let due = due_calendars(&svc, &vault, false, &soon, &HashSet::new()).unwrap();
        let ids: Vec<_> = due.iter().map(|c| c.id).collect();
        assert_eq!(ids, [fine.id]);
    }
}
