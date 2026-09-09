//! Subscribed calendars and the events read out of them.
//!
//! Note the division of labour, which is the same one the rest of the
//! application makes and is worth spelling out because this is the only
//! feature that touches a network:
//!
//!   this file      what a URL is, when to fetch it, what to do on a 403
//!   feeds.rs       turning a URL into bytes -- one of two sockets
//!   everyday-core  everything that happens to those bytes afterwards
//!
//! The last of those is the part with the difficult logic in it -- RFC 5545,
//! recurrence, time zones -- and it is testable offline precisely because it
//! never learns that a network exists.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::events::Notification;
use crate::feeds;
use crate::service::{Service, blocking};
use everyday_core::calendar::{Calendar, CalendarProvider, Event, SyncReport};
use everyday_core::model::{system_tz, today_local};
use everyday_core::store::calendars::EventQuery;
use everyday_core::{CalendarId, EventId, Vault};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::vault::Nothing;

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
    _c: Ctx,
    _a: Nothing,
) -> CommandResult<Vec<CalendarInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        let mut out = Vec::new();
        for calendar in vault.calendars()? {
            // The count is a `COUNT(*)` over a clear index column, so listing
            // four calendars decrypts four records and nothing else.
            let events = vault.event_count(calendar.id).unwrap_or(0);
            out.push(CalendarInfo { calendar, events });
        }
        Ok(out)
    })
    .await
}

async fn save_calendar(svc: Arc<Service>, _c: Ctx, args: SaveCalendar) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_calendar(&args.calendar)?)).await
}

/// Unsubscribe: the calendar and every event that came from it.
async fn delete_calendar(svc: Arc<Service>, _c: Ctx, args: CalendarRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_calendar(args.id)?)).await
}

async fn list_events(svc: Arc<Service>, _c: Ctx, args: Events) -> CommandResult<Vec<Event>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.events(&args.query)?)).await
}

async fn get_event(svc: Arc<Service>, _c: Ctx, args: EventRef) -> CommandResult<Event> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.event(args.id)?)).await
}

/// Fetch one calendar's feed and replace its events with what came back.
///
/// The failure path is as deliberate as the success one. A feed that cannot be
/// fetched, or that answers with something that is not a calendar, leaves the
/// events already stored exactly where they are and records *why* on the
/// subscription -- because the alternative, a calendar that empties itself when
/// the wifi drops, is the one failure of an automatic sync that people notice
/// and never forgive.
async fn sync_calendar(svc: Arc<Service>, _c: Ctx, args: CalendarRef) -> CommandResult<SyncReport> {
    let vault = svc.require()?;
    let report = sync_one(&vault, args.id).await?;
    // A hand-driven refresh that works ends the outage as much as a background
    // one does, so the next failure is news again. Without this, a feed fixed
    // from the sidebar would never notify a second time.
    svc.feed_recovered(args.id);
    Ok(report)
}

/// Fetch every subscription whose refresh interval has elapsed.
///
/// Polled rather than driven by a timer in here, so that a locked vault is
/// never fetched into and a window nobody is looking at is never the reason a
/// laptop wakes its radio. Failures are collected, not raised: one calendar
/// being down must not stop the other three.
async fn sync_due_calendars(
    svc: Arc<Service>,
    _c: Ctx,
    args: SyncDue,
) -> CommandResult<Vec<SyncReport>> {
    let vault = svc.require()?;
    // A read-only vault cannot store what a sync fetches, and `sync_one` would
    // also try to record the failure on the subscription -- another write.
    // Fetching feeds over the network to throw the bytes away is not a useful
    // thing to do on a timer, so the whole pass is skipped.
    if !vault.is_writable() {
        return Ok(Vec::new());
    }
    let now = jiff::Timestamp::now();
    // The name travels with the id because the notification below needs it, and
    // re-reading the subscription after a failed sync to find out what to call
    // it is a second decrypt for a string we already had.
    let due: Vec<(CalendarId, String)> = vault
        .calendars()?
        .into_iter()
        .filter(|c| c.origin.url().is_some() && (args.force || c.is_due(now)))
        .map(|c| (c.id, c.name))
        .collect();

    let mut out = Vec::new();
    for (id, name) in due {
        match sync_one(&vault, id).await {
            Ok(report) => {
                svc.feed_recovered(id);
                out.push(report);
            }
            // The failure itself is already recorded on the subscription by
            // `sync_one`, and the calendar sidebar shows it there, beside the
            // calendar it belongs to. That is the right place for it and it
            // stays -- but it is only the right place if you are looking at the
            // calendar. This pass runs on a timer whoever is using the app, so
            // the case worth notifying is somebody who has spent the week in
            // the journal while a subscription they rely on has been quietly
            // returning nothing.
            //
            // Once per outage, never on a refresh the user asked for by hand --
            // they are looking at the calendar, and the sidebar has just told
            // them.
            Err(e) => {
                tracing::info!(%id, error = %e, "a calendar could not be refreshed");
                if !args.force && svc.feed_failed(id) {
                    svc.events().notify(
                        Notification::warning(format!("{name} is not refreshing"))
                            // Deliberately not `e.message`: a feed URL is a
                            // bearer credential and error text from a fetch can
                            // quote it. The interface shows the recorded detail
                            // beside the calendar, where the person reading it
                            // already has the address.
                            .body("Its events may be out of date. Open the calendar for details.")
                            .for_user()
                            .key(format!("feed:{id}")),
                    );
                }
            }
        }
    }
    Ok(out)
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
    _c: Ctx,
    args: Subscribe,
) -> CommandResult<CalendarInfo> {
    let vault = svc.require()?;
    let url = everyday_core::calendar::normalize_feed_url(&args.url)?;
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
async fn import_calendar(svc: Arc<Service>, _c: Ctx, args: Import) -> CommandResult<CalendarInfo> {
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
        calendar.name = everyday_core::ics::parse(&text)
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
        Ok(report) => Ok(CalendarInfo { calendar: vault.calendar(id)?, events: report.events }),
        Err(e) => {
            // Nothing added: see `subscribe_calendar`. Undoing the save is safe
            // because nothing else can have pointed at it yet.
            let _ = vault.delete_calendar(id);
            Err(e)
        }
    }
}

/// The providers the "add a calendar" sheet offers, with where to find the
/// address for each.
///
/// Held in Rust rather than hard-coded in a client because the guess that picks
/// a provider from a pasted URL lives here too, and the two have to agree about
/// what the set is.
async fn calendar_providers(
    svc: Arc<Service>,
    _c: Ctx,
    _a: Nothing,
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
        args: SaveCalendar, returns: "void",
        signature: &[("calendar", "Calendar", true)],
        run: save_calendar,
    },
    command! {
        name: "delete_calendar", scope: Calendars, effect: Destructive,
        change: Calendar / Deleted,
        args: CalendarRef, returns: "void",
        signature: &[("id", "CalendarId", true)],
        run: delete_calendar,
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
];
