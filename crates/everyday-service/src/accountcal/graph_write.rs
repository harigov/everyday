//! Microsoft Graph: putting events on an Outlook calendar, changing them, and
//! taking them off again.
//!
//! The reading half is the parent module; this is the half that writes,
//! sharing the parent's Graph token, its time parsing and its zone table.
//! See `accountcal`'s module doc, "Writing", for the shape every source
//! follows: write to the server, and let the calendar's own sync bring the
//! result back. Nothing here touches an [`Event`] row.
//!
//! # Four calls
//!
//! | Write | Request |
//! |---|---|
//! | [`create`] | `POST me/calendar/events`, or `me/calendars/{id}/events` |
//! | [`load`] | `GET me/events/{id}`, and the series master for its pattern |
//! | [`update`] | `PATCH me/events/{id}` -- the occurrence's, or the master's |
//! | [`delete`] | `DELETE me/events/{id}` -- the same choice |
//!
//! Only a create names a calendar: `primary` -- see the parent's `PRIMARY`
//! -- is Graph's own `me/calendar`, anything else is addressed by its id. An
//! event id is unique across the whole mailbox, so everything after that
//! reaches the event directly. There is no `sendUpdates` to ask for: Graph
//! sends the invitations, the updates and the cancellations itself whenever
//! the organiser's copy changes.
//!
//! # Wall clocks and zone names
//!
//! Graph takes a time as a wall-clock reading with no offset --
//! `"2026-10-12T09:00:00"` -- and the name of the zone it is read in, and it
//! expands a series in that zone. Outlook's own names are Windows ones
//! (`"Pacific Standard Time"`), so a draft's IANA zone is turned back into
//! one through the same table the parent reads them with,
//! [`everyday_core::ics::WINDOWS_ZONES`], searched the other way; a zone the
//! table does not have goes as its IANA name, which Graph also accepts.
//! Reading back, the parent's `Prefer: outlook.timezone="UTC"` makes every
//! time a plain UTC instant, and `outlook.body-content-type="text"` makes the
//! description plain text the editor can show -- see [`send`].
//!
//! # Not clobbering what the editor does not show
//!
//! An Outlook event carries a good deal the editor never shows: an HTML body
//! (a Teams invitation's whole join block), a location with an address and
//! coordinates, rooms booked as `resource` attendees. A PATCH replaces any
//! field it names outright, so the body and the location are only sent when
//! they actually changed -- moving a Teams meeting by an hour must not
//! flatten its invitation to plain text -- and the guest list goes back with
//! every booked room still on it and each existing guest's `required` or
//! `optional` kept.
//!
//! # Series
//!
//! Every [`Event`] the parent's sync files is one occurrence (or a one-off,
//! `singleInstance`); its series lives on the master, named by
//! `seriesMasterId`, which is the only place `recurrence` is ever read or
//! written. Changing a series patches the master, moved by however far the
//! occurrence moved (see [`super::super::google::draft_for_series`], which
//! both sources share), with the pattern's `range.startDate` moved along with
//! it. Graph has no way to turn a series back into a single event by PATCH,
//! so "does not repeat" on a whole series is refused in words rather than
//! sent and silently ignored.
//!
//! # Which failure is which
//!
//! The same as `google_write.rs`: a 401 is the credential
//! ([`codes::FORBIDDEN`]); a 403 is this calendar or event refusing the
//! change ([`codes::INVALID`], in Graph's own words), never the account's
//! sign-in; 429 and 503 are waited out the way the parent's reads wait them
//! out; a 404 or 410 is an event that has gone.

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    Attendee, Calendar, CalendarOrigin, EditableEvent, Event, EventDraft, EventScope,
};
use everyday_core::mail::AttendeeResponse;
use everyday_core::recurrence::{Frequency, Recurrence, Weekday};
use jiff::Timestamp;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::time::sleep;

use super::{API, PRIMARY, graph_bearer, graph_zone, parse_graph_datetime};
use crate::accountcal::google::{LocalStart, draft_for_series, urlencoding_light};
use crate::accountcal::{calendar_after_retry_after, retry_after_delay};
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::service::Service;

/// One event as `GET me/events/{id}` answers it -- only what a write needs.
/// Nearly everything is optional, because Graph answers `null` for a good
/// deal of what it has nothing to say about. `attendees` stays as Graph's
/// own JSON; see [`attendees_json`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEvent {
    #[serde(default)]
    id: String,
    subject: Option<String>,
    body: Option<WireBody>,
    location: Option<WireLocation>,
    start: Option<WireWhen>,
    end: Option<WireWhen>,
    is_all_day: Option<bool>,
    attendees: Option<Vec<Value>>,
    is_organizer: Option<bool>,
    organizer: Option<WirePerson>,
    /// `singleInstance`, `occurrence`, `exception` or `seriesMaster`.
    #[serde(rename = "type")]
    kind: Option<String>,
    series_master_id: Option<String>,
    /// The zone the organiser made the event in, by its Windows name. Every
    /// time comes back in UTC (see [`send`]), so this is the only place the
    /// event's own zone survives the trip.
    original_start_time_zone: Option<String>,
    /// A `patternedRecurrence`, on the series master only.
    recurrence: Option<Value>,
}

impl WireEvent {
    fn all_day(&self) -> bool {
        self.is_all_day.unwrap_or(false)
    }

    fn text(&self) -> &str {
        self.body.as_ref().and_then(|b| b.content.as_deref()).unwrap_or_default()
    }

    fn place(&self) -> &str {
        self.location.as_ref().and_then(|l| l.display_name.as_deref()).unwrap_or_default()
    }

    fn guests(&self) -> &[Value] {
        self.attendees.as_deref().unwrap_or_default()
    }

    /// The id of the master that holds this event's pattern: its
    /// `seriesMasterId` for an occurrence or an exception, its own id when
    /// it is the master, and `None` for an event that does not repeat.
    fn series(&self) -> Option<String> {
        self.series_master_id
            .clone()
            .or_else(|| (self.kind.as_deref() == Some("seriesMaster")).then(|| self.id.clone()))
    }

    fn repeats(&self) -> bool {
        self.series().is_some() || self.kind.as_deref().is_some_and(|k| k != "singleInstance")
    }
}

#[derive(Debug, Deserialize)]
struct WireBody {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireLocation {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireWhen {
    date_time: String,
    time_zone: Option<String>,
}

/// An organiser or an attendee, as Graph names them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePerson {
    email_address: Option<WireAddress>,
    status: Option<WireStatus>,
    /// `required`, `optional` or `resource`.
    #[serde(rename = "type")]
    kind: Option<String>,
}

impl WirePerson {
    fn label(&self) -> String {
        let Some(address) = &self.email_address else { return String::new() };
        address
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .or_else(|| address.address.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct WireAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireStatus {
    response: Option<String>,
}

pub(super) async fn create(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    draft: &EventDraft,
) -> CommandResult<()> {
    create_with_base(svc, vault, account, calendar, draft, API).await
}

pub(super) async fn load(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    _calendar: &Calendar,
    event: &Event,
) -> CommandResult<EditableEvent> {
    load_with_base(svc, vault, account, event, API).await
}

pub(super) async fn update(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    _calendar: &Calendar,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
) -> CommandResult<()> {
    update_with_base(svc, vault, account, event, draft, scope, API).await
}

pub(super) async fn delete(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    _calendar: &Calendar,
    event: &Event,
    scope: EventScope,
) -> CommandResult<()> {
    delete_with_base(svc, vault, account, event, scope, API).await
}

/// [`create`]'s own body, over `base` rather than the hardcoded [`API`] --
/// the parent's `sync_with_base` split, so a test can point the whole
/// conversation at a mock server. The other three `*_with_base` functions
/// below are split for the same reason; none of them needs the calendar,
/// because an event id already names its mailbox.
async fn create_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    draft: &EventDraft,
    base: &str,
) -> CommandResult<()> {
    let CalendarOrigin::Account { remote_id, .. } = &calendar.origin else {
        return Err(CommandError::new(codes::INVALID, "not an account calendar"));
    };
    let url = if remote_id == PRIMARY {
        format!("{base}/me/calendar/events")
    } else {
        format!("{base}/me/calendars/{}/events", urlencoding_light(remote_id))
    };
    let token = graph_bearer(svc, vault, account).await?;
    let mut body = event_body(draft, None);
    if let Some(rule) = &draft.recurrence {
        body.insert(
            "recurrence".into(),
            pattern_for(rule, draft.start_date(), &zone_name(&draft.tz)),
        );
    }
    send(Method::POST, &url, &token, Some(&Value::Object(body))).await?;
    Ok(())
}

async fn load_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    event: &Event,
    base: &str,
) -> CommandResult<EditableEvent> {
    let token = graph_bearer(svc, vault, account).await?;
    let item = get_event(base, &event.uid, &token).await?;

    // An occurrence does not carry its series' pattern; the master does. A
    // master this account cannot open is reported as a pattern the editor
    // cannot show, rather than refusing the whole event over it.
    let pattern = match &item.series_master_id {
        Some(master) => match get_event(base, master, &token).await {
            Ok(master) => master.recurrence,
            Err(e) if e.code == codes::NOT_FOUND => None,
            Err(e) => return Err(e),
        },
        None => item.recurrence.clone(),
    };
    let recurring = item.repeats();
    let recurrence =
        if recurring { pattern.as_ref().and_then(recurrence_from_pattern) } else { None };
    let custom_recurrence = recurring && recurrence.is_none();

    let tz = item
        .original_start_time_zone
        .as_deref()
        .and_then(everyday_core::ics::resolve_tzid)
        .into_iter()
        .chain([event.tz.clone()])
        .find(|tz| TimeZone::get(tz).is_ok())
        .unwrap_or_else(|| "UTC".to_string());
    let (start, end) = draft_bounds(&item, &graph_zone(&tz))?;

    Ok(EditableEvent {
        event_id: event.id,
        calendar_id: event.calendar_id,
        draft: EventDraft {
            title: item.subject.clone().unwrap_or_default(),
            description: item.text().to_string(),
            location: item.place().to_string(),
            start,
            end,
            all_day: item.all_day(),
            tz,
            attendees: guests(item.guests()),
            recurrence,
        },
        recurring,
        custom_recurrence,
        own: item.is_organizer.unwrap_or(false),
        organizer: item.organizer.as_ref().map(WirePerson::label).unwrap_or_default(),
    })
}

async fn update_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
    base: &str,
) -> CommandResult<()> {
    let token = graph_bearer(svc, vault, account).await?;
    // Read first: whether this is part of a series, where it is now, and
    // what its body, location and guests already are are the server's to
    // say, not the vault's.
    let item = get_event(base, &event.uid, &token).await?;
    let repeats = item.repeats();

    let Some(series) = item.series().filter(|_| scope == EventScope::Series) else {
        // Just this occurrence -- Graph keeps it as an exception -- or the
        // whole event, when it does not repeat, which is the one case a
        // pattern can be added here: a one-off made into a series.
        let mut body = event_body(draft, Some(&item));
        if !repeats && let Some(rule) = &draft.recurrence {
            body.insert(
                "recurrence".into(),
                pattern_for(rule, draft.start_date(), &zone_name(&draft.tz)),
            );
        }
        return patch(base, &event.uid, &token, body).await;
    };

    let fetched =
        if series == item.id { None } else { Some(get_event(base, &series, &token).await?) };
    let master = fetched.as_ref().unwrap_or(&item);
    let zone = graph_zone(&draft.tz);
    let first = local(master, &zone)?;
    let moved = draft_for_series(draft, first, local(&item, &zone)?)?;
    let mut body = event_body(&moved, Some(master));
    match (&draft.recurrence, master.recurrence.as_ref()) {
        (Some(rule), _) => {
            body.insert(
                "recurrence".into(),
                pattern_for(rule, moved.start_date(), &zone_name(&draft.tz)),
            );
        }
        (None, Some(current)) if recurrence_from_pattern(current).is_some() => {
            return Err(CommandError::new(
                codes::INVALID,
                "Outlook cannot stop a series repeating from here; delete the later events \
                 instead",
            ));
        }
        // A pattern the editor could not show was never offered for change.
        // It is left as it is -- except that a series whose first day moved
        // has to say so in its range too, or Graph would keep expanding from
        // the old one.
        (None, Some(current)) => {
            if moved.start_date() != first.date() {
                let mut kept = current.clone();
                if let Some(range) = kept.get_mut("range").and_then(Value::as_object_mut) {
                    range.insert("startDate".into(), json!(moved.start_date().to_string()));
                }
                body.insert("recurrence".into(), kept);
            }
        }
        (None, None) => {}
    }
    patch(base, &series, &token, body).await
}

async fn delete_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    event: &Event,
    scope: EventScope,
    base: &str,
) -> CommandResult<()> {
    let token = graph_bearer(svc, vault, account).await?;
    let id = match scope {
        EventScope::Occurrence => event.uid.clone(),
        EventScope::Series => match get_event(base, &event.uid, &token).await {
            Ok(item) => item.series().unwrap_or_else(|| event.uid.clone()),
            Err(e) if e.code == codes::NOT_FOUND => return Ok(()),
            Err(e) => return Err(e),
        },
    };
    match send(Method::DELETE, &event_url(base, &id), &token, None).await {
        Ok(_) => Ok(()),
        // Already gone -- from another device, or by a delete whose answer
        // never arrived. Either way, what was asked for is true.
        Err(e) if e.code == codes::NOT_FOUND => Ok(()),
        Err(e) => Err(e),
    }
}

fn event_url(base: &str, id: &str) -> String {
    format!("{base}/me/events/{}", urlencoding_light(id))
}

async fn get_event(base: &str, id: &str, token: &str) -> CommandResult<WireEvent> {
    let answer = send(Method::GET, &event_url(base, id), token, None).await?;
    serde_json::from_value(answer.unwrap_or(Value::Null)).map_err(|e| {
        CommandError::new(codes::NETWORK, format!("could not read Microsoft Graph's answer: {e}"))
    })
}

async fn patch(base: &str, id: &str, token: &str, body: Map<String, Value>) -> CommandResult<()> {
    send(Method::PATCH, &event_url(base, id), token, Some(&Value::Object(body))).await?;
    Ok(())
}

/// Everything about `draft` a create or a PATCH sends. `existing` is the
/// event as Graph has it now -- `None` for a new one -- and is what decides
/// whether the body and the location go at all: see the module doc's "Not
/// clobbering what the editor does not show".
fn event_body(draft: &EventDraft, existing: Option<&WireEvent>) -> Map<String, Value> {
    let zone = zone_name(&draft.tz);
    // Graph's all-day event runs midnight to midnight in its zone, the end
    // exclusive, with `isAllDay` saying so.
    let (start, end) = if draft.all_day {
        (
            draft.start_date().to_datetime(Time::midnight()),
            draft.end_date_exclusive().to_datetime(Time::midnight()),
        )
    } else {
        (draft.local_start(), draft.local_end())
    };
    let mut body = Map::new();
    body.insert("subject".into(), json!(draft.title));
    if existing.is_none_or(|e| !same_text(e.text(), &draft.description)) {
        body.insert("body".into(), json!({"contentType": "text", "content": draft.description}));
    }
    if existing.is_none_or(|e| e.place() != draft.location) {
        body.insert("location".into(), json!({"displayName": draft.location}));
    }
    body.insert("start".into(), when_json(start, &zone));
    body.insert("end".into(), when_json(end, &zone));
    body.insert("isAllDay".into(), json!(draft.all_day));
    let known = existing.map(WireEvent::guests).unwrap_or_default();
    body.insert("attendees".into(), attendees_json(&draft.attendees, known));
    body
}

/// The same text, give or take how a line ends and what trails it -- Graph's
/// HTML-to-text conversion and an editor's textarea do not agree on either,
/// and a description nobody touched must not count as changed.
fn same_text(a: &str, b: &str) -> bool {
    a.replace("\r\n", "\n").trim() == b.replace("\r\n", "\n").trim()
}

fn when_json(at: DateTime, zone: &str) -> Value {
    json!({"dateTime": at.strftime("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": zone})
}

/// The name Graph is sent for an IANA zone: Outlook's own Windows name when
/// the shared table has one, the IANA name otherwise. See the module doc.
fn zone_name(tz: &str) -> String {
    everyday_core::ics::WINDOWS_ZONES
        .iter()
        .find(|(_, iana)| *iana == tz)
        .map_or_else(|| tz.to_string(), |(windows, _)| (*windows).to_string())
}

/// One attendee already on the event, as much of them as is written back.
struct Known {
    address: String,
    name: String,
    kind: String,
}

/// The guest list as Graph's `attendees` array. A guest already on the
/// event keeps the address Graph has for them and their `required` or
/// `optional`, with the name the editor gave laid over theirs; a new guest
/// is `required`. Each booked room goes back as it was, because the array
/// replaces the old one wholesale and leaving a room out would cancel it.
/// Only the address, the name and the type are sent -- an attendee's
/// `status` is theirs to set by answering, not the organiser's to write.
fn attendees_json(guests: &[Attendee], existing: &[Value]) -> Value {
    let known: Vec<Known> = existing
        .iter()
        .filter_map(|entry| WirePerson::deserialize(entry).ok())
        .filter_map(|person| {
            let address = person.email_address?;
            Some(Known {
                address: address.address.filter(|a| !a.is_empty())?,
                name: address.name.unwrap_or_default(),
                kind: person.kind.unwrap_or_else(|| "required".to_string()),
            })
        })
        .collect();

    let mut out: Vec<Value> = guests
        .iter()
        .map(|guest| match known.iter().find(|k| same_address(&k.address, &guest.email)) {
            Some(k) => {
                let name = if guest.name.is_empty() { &k.name } else { &guest.name };
                attendee_json(&k.address, name, &k.kind)
            }
            None => attendee_json(&guest.email, &guest.name, "required"),
        })
        .collect();
    for room in known.iter().filter(|k| k.kind == "resource") {
        if !guests.iter().any(|guest| same_address(&guest.email, &room.address)) {
            out.push(attendee_json(&room.address, &room.name, &room.kind));
        }
    }
    Value::Array(out)
}

fn attendee_json(address: &str, name: &str, kind: &str) -> Value {
    json!({"emailAddress": {"address": address, "name": name}, "type": kind})
}

/// Two addresses for one attendee, compared the way [`Attendee::key`]
/// compares them.
fn same_address(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Graph's attendees as the editor's: every guest with their answer, the
/// organiser's own copy answering `organizer`, and no room -- a room is
/// booked rather than invited, and is never the editor's to change.
fn guests(entries: &[Value]) -> Vec<Attendee> {
    entries
        .iter()
        .filter_map(|entry| WirePerson::deserialize(entry).ok())
        .filter(|person| person.kind.as_deref() != Some("resource"))
        .filter_map(|person| {
            let address = person.email_address?;
            Some(Attendee {
                email: address.address.filter(|a| !a.is_empty())?,
                name: address.name.unwrap_or_default(),
                response: person.status.and_then(|s| s.response).as_deref().and_then(response),
            })
        })
        .collect()
}

fn response(answer: &str) -> Option<AttendeeResponse> {
    match answer {
        "accepted" | "organizer" => Some(AttendeeResponse::Accepted),
        "tentativelyAccepted" => Some(AttendeeResponse::Tentative),
        "declined" => Some(AttendeeResponse::Declined),
        "notResponded" | "none" => Some(AttendeeResponse::NeedsAction),
        _ => None,
    }
}

/// A [`Recurrence`] as Graph's `patternedRecurrence`, first falling on
/// `start` -- the series' first day -- and expanded in `zone`, the same zone
/// name the event's own times are sent in.
///
/// The four shapes a [`Recurrence`] holds land on five of Graph's six
/// pattern types:
///
/// ```text
///   daily                          daily
///   weekly + weekdays              weekly, daysOfWeek (none: the start's own day)
///   monthly                        absoluteMonthly, dayOfMonth = the start's
///   monthly + week_of_month        relativeMonthly, daysOfWeek + index
///   yearly                         absoluteYearly, dayOfMonth + month = the start's
/// ```
///
/// `relativeYearly` -- "the fourth Thursday of November" -- has no
/// [`Recurrence`] to come from, and is only ever read, by
/// [`recurrence_from_pattern`], as a pattern the editor cannot show. The
/// range ends the way the rule does: after `count` occurrences
/// (`numbered`), on `until` (`endDate`, inclusive, like
/// [`Recurrence::until`]), or never (`noEnd`).
fn pattern_for(rule: &Recurrence, start: Date, zone: &str) -> Value {
    let mut pattern = Map::new();
    let kind = match (rule.frequency, rule.week_of_month) {
        (Frequency::Daily, _) => "daily",
        (Frequency::Weekly, _) => {
            let days: Vec<&str> = rule.days_for(start).iter().map(|d| d.as_str()).collect();
            pattern.insert("daysOfWeek".into(), json!(days));
            "weekly"
        }
        (Frequency::Monthly, Some(week)) => {
            let day = rule
                .weekdays
                .first()
                .copied()
                .unwrap_or_else(|| Weekday::from_jiff(start.weekday()));
            pattern.insert("daysOfWeek".into(), json!([day.as_str()]));
            pattern.insert("index".into(), json!(week_index(week)));
            "relativeMonthly"
        }
        (Frequency::Monthly, None) => {
            pattern.insert("dayOfMonth".into(), json!(start.day()));
            "absoluteMonthly"
        }
        (Frequency::Yearly, _) => {
            pattern.insert("dayOfMonth".into(), json!(start.day()));
            pattern.insert("month".into(), json!(start.month()));
            "absoluteYearly"
        }
    };
    pattern.insert("type".into(), json!(kind));
    pattern.insert("interval".into(), json!(rule.interval.max(1)));
    pattern.insert("firstDayOfWeek".into(), json!("sunday"));

    let mut range = Map::new();
    range.insert("startDate".into(), json!(start.to_string()));
    range.insert("recurrenceTimeZone".into(), json!(zone));
    match (rule.count, rule.until) {
        (Some(count), _) => {
            range.insert("type".into(), json!("numbered"));
            range.insert("numberOfOccurrences".into(), json!(count));
        }
        (None, Some(until)) => {
            range.insert("type".into(), json!("endDate"));
            range.insert("endDate".into(), json!(until.to_string()));
        }
        (None, None) => {
            range.insert("type".into(), json!("noEnd"));
        }
    }
    json!({"pattern": pattern, "range": range})
}

fn week_index(week: i8) -> &'static str {
    match week {
        1 => "first",
        2 => "second",
        3 => "third",
        4 => "fourth",
        _ => "last",
    }
}

/// [`pattern_for`]'s inverse: a master's `patternedRecurrence` as a
/// [`Recurrence`], or `None` for one that shape cannot hold exactly --
/// `relativeYearly`, "the first weekday of the month" (`relativeMonthly`
/// over several days), a type Graph adds later -- so [`load`] can say the
/// series repeats in a way that can only be changed where it was made rather
/// than offer an editor that would flatten it on save. A `dayOfMonth` or
/// `month` is dropped, the way [`Recurrence::from_rrule`] drops a single
/// `BYMONTHDAY`: the series' own start already says which day.
fn recurrence_from_pattern(value: &Value) -> Option<Recurrence> {
    let pattern = value.get("pattern")?;
    let interval = match pattern.get("interval") {
        None | Some(Value::Null) => 1,
        Some(n) => u32::try_from(n.as_u64()?).ok()?,
    };
    let mut days = match pattern.get("daysOfWeek") {
        None | Some(Value::Null) => Vec::new(),
        Some(list) => list
            .as_array()?
            .iter()
            .map(|day| day.as_str().and_then(Weekday::parse))
            .collect::<Option<Vec<_>>>()?,
    };
    days.sort();
    days.dedup();
    let mut rule = match pattern.get("type")?.as_str()? {
        "daily" => Recurrence::every(Frequency::Daily),
        "weekly" => Recurrence { weekdays: days, ..Recurrence::every(Frequency::Weekly) },
        "absoluteMonthly" => Recurrence::every(Frequency::Monthly),
        "relativeMonthly" => {
            let week = match pattern.get("index").and_then(Value::as_str).unwrap_or("first") {
                "first" => 1,
                "second" => 2,
                "third" => 3,
                "fourth" => 4,
                "last" => -1,
                _ => return None,
            };
            Recurrence {
                weekdays: days,
                week_of_month: Some(week),
                ..Recurrence::every(Frequency::Monthly)
            }
        }
        "absoluteYearly" => Recurrence::every(Frequency::Yearly),
        _ => return None,
    };
    rule.interval = interval;
    let range = value.get("range");
    match range.and_then(|r| r.get("type")).and_then(Value::as_str).unwrap_or("noEnd") {
        "noEnd" => {}
        "endDate" => rule.until = Some(range?.get("endDate")?.as_str()?.parse().ok()?),
        "numbered" => {
            let count = range?.get("numberOfOccurrences")?.as_u64()?;
            rule.count = Some(u32::try_from(count).ok()?);
        }
        _ => return None,
    }
    // A relativeMonthly over several days, an interval of 0, a count of 0:
    // all refused here, by the same rules a rule made in the editor meets.
    rule.validate().ok()?;
    Some(rule)
}

/// `item`'s start and end as a draft holds them: instants, with an all-day
/// event running from midnight of its first day to midnight after its last
/// in `zone` -- the shape `EventDraft::validate` gives every all-day draft.
/// Graph's all-day times are a date at midnight whatever zone they are
/// quoted in, so only the date is read from them.
fn draft_bounds(item: &WireEvent, zone: &TimeZone) -> CommandResult<(Timestamp, Timestamp)> {
    let start = item.start.as_ref().ok_or_else(unreadable)?;
    let end = item.end.as_ref().unwrap_or(start);
    if item.all_day() {
        let first = date_part(&start.date_time)?;
        let after = date_part(&end.date_time)?.max(first.tomorrow().map_err(|_| unreadable())?);
        let midnight =
            |day: Date| day.to_zoned(zone.clone()).map(|z| z.timestamp()).map_err(|_| unreadable());
        return Ok((midnight(first)?, midnight(after)?));
    }
    let from = instant(start)?;
    Ok((from, instant(end)?.max(from)))
}

/// Where `item` starts on `zone`'s wall clock, for [`draft_for_series`].
fn local(item: &WireEvent, zone: &TimeZone) -> CommandResult<LocalStart> {
    let start = item.start.as_ref().ok_or_else(unreadable)?;
    if item.all_day() {
        Ok(LocalStart::Day(date_part(&start.date_time)?))
    } else {
        Ok(LocalStart::At(instant(start)?.to_zoned(zone.clone()).datetime()))
    }
}

fn instant(when: &WireWhen) -> CommandResult<Timestamp> {
    let zone = graph_zone(when.time_zone.as_deref().unwrap_or("UTC"));
    parse_graph_datetime(&when.date_time, &zone).ok_or_else(unreadable)
}

fn date_part(raw: &str) -> CommandResult<Date> {
    raw.split_once('T').map_or(raw, |(day, _)| day).parse::<Date>().map_err(|_| unreadable())
}

fn unreadable() -> CommandError {
    CommandError::new(
        codes::NETWORK,
        "Microsoft Graph's answer had a time in it that could not be read",
    )
}

fn gone() -> CommandError {
    CommandError::new(codes::NOT_FOUND, "that event is no longer on the calendar; refresh it")
}

/// One request, any method, with a bearer token and an optional JSON body --
/// answering Graph's JSON, or `None` when there is none (a delete's 204).
///
/// Every request carries the two preferences the module doc names:
/// `outlook.timezone="UTC"`, the parent's own, so a time read back is a plain
/// instant; and `outlook.body-content-type="text"`, so a description read
/// back is what a textarea can show rather than an Outlook HTML document.
/// 429 and 503 are waited out the way the parent's `get_json_full_url` waits
/// them out, honouring `Retry-After`; anything else that is not a 2xx goes
/// through [`refusal`]. The token never appears in an error: nothing here
/// formats the request, only Graph's answer to it.
async fn send(
    method: Method,
    url: &str,
    token: &str,
    body: Option<&Value>,
) -> CommandResult<Option<Value>> {
    let payload = body.map(serde_json::to_vec).transpose().map_err(|e| {
        CommandError::new(codes::INTERNAL, format!("could not write that event out for Graph: {e}"))
    })?;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut request = http::client()?
            .request(method.clone(), url)
            .bearer_auth(token)
            .header("Prefer", "outlook.timezone=\"UTC\"")
            .header("Prefer", "outlook.body-content-type=\"text\"");
        if let Some(payload) = &payload {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(payload.clone());
        }
        let response = request.send().await.map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not reach Microsoft Graph: {e}"))
        })?;
        let status = response.status().as_u16();
        let retry_after = retry_after_delay(response.headers());
        let answer = response.bytes().await.map_err(|e| {
            CommandError::new(
                codes::NETWORK,
                format!("could not read Microsoft Graph's answer: {e}"),
            )
        })?;
        if matches!(status, 429 | 503) {
            let policy = calendar_after_retry_after();
            if policy.gives_up_after(attempt) {
                return Err(CommandError::new(
                    codes::RATE_LIMITED,
                    format!(
                        "Microsoft Graph answered {status} too many times in a row; try again in \
                         a minute"
                    ),
                ));
            }
            sleep(retry_after.unwrap_or_else(|| policy.delay_for(attempt))).await;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(refusal(&method, status, &answer));
        }
        if answer.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        return serde_json::from_slice(&answer).map(Some).map_err(|e| {
            CommandError::new(
                codes::NETWORK,
                format!("could not read Microsoft Graph's answer: {e}"),
            )
        });
    }
}

/// A non-2xx answer to `method`, as the code and sentence the module doc's
/// "Which failure is which" gives it.
fn refusal(method: &Method, status: u16, answer: &[u8]) -> CommandError {
    let because = graph_says(answer).map(|reason| format!(": {reason}")).unwrap_or_default();
    match status {
        401 => {
            CommandError::new(codes::FORBIDDEN, "Microsoft Graph refused this account's credential")
        }
        400 => {
            CommandError::new(codes::INVALID, format!("Outlook would not take that event{because}"))
        }
        403 => CommandError::new(
            codes::INVALID,
            format!("Outlook would not let you {}{because}", doing(method)),
        ),
        404 | 410 if method.as_str() == "POST" => CommandError::new(
            codes::NOT_FOUND,
            "that calendar is no longer in Outlook; refresh your calendars",
        ),
        404 | 410 => gone(),
        409 | 412 => CommandError::new(
            codes::CONFLICT,
            "that event changed in Outlook while it was being saved; refresh it and try again",
        ),
        _ => CommandError::new(codes::NETWORK, format!("Microsoft Graph answered {status}")),
    }
}

fn doing(method: &Method) -> &'static str {
    match method.as_str() {
        "POST" => "add that event",
        "DELETE" => "delete that event",
        "GET" => "open that event",
        _ => "change that event",
    }
}

/// Graph's own sentence for a refusal -- `error.message` -- when it gave
/// one. It names the mailbox's rule, never the request's credential.
fn graph_says(answer: &[u8]) -> Option<String> {
    let parsed: Value = serde_json::from_slice(answer).ok()?;
    let message = parsed.pointer("/error/message")?.as_str()?.trim();
    (!message.is_empty()).then(|| message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use axum::Json;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use everyday_core::account::{AccountSecret, AuthMethod, Provider};
    use everyday_core::calendar::{AccountCalendarSource, EventStatus};
    use everyday_core::id::EventId;
    use jiff::civil::date;

    // ---- the pattern, both ways -------------------------------------------

    fn weekly(days: &[Weekday]) -> Recurrence {
        Recurrence { weekdays: days.to_vec(), ..Recurrence::every(Frequency::Weekly) }
    }

    #[test]
    fn a_rule_becomes_the_patterned_recurrence_graph_wants() {
        let monday = date(2026, 10, 5);
        let fortnightly = Recurrence {
            interval: 2,
            until: Some(date(2026, 12, 31)),
            ..weekly(&[Weekday::Wednesday, Weekday::Monday])
        };
        assert_eq!(
            pattern_for(&fortnightly, monday, "Pacific Standard Time"),
            json!({
                "pattern": {"type": "weekly", "interval": 2, "daysOfWeek": ["monday", "wednesday"],
                            "firstDayOfWeek": "sunday"},
                "range": {"type": "endDate", "startDate": "2026-10-05", "endDate": "2026-12-31",
                          "recurrenceTimeZone": "Pacific Standard Time"},
            })
        );

        let last_friday = Recurrence {
            weekdays: vec![Weekday::Friday],
            week_of_month: Some(-1),
            count: Some(6),
            ..Recurrence::every(Frequency::Monthly)
        };
        assert_eq!(
            pattern_for(&last_friday, date(2026, 10, 30), "UTC"),
            json!({
                "pattern": {"type": "relativeMonthly", "interval": 1, "daysOfWeek": ["friday"],
                            "index": "last", "firstDayOfWeek": "sunday"},
                "range": {"type": "numbered", "startDate": "2026-10-30", "numberOfOccurrences": 6,
                          "recurrenceTimeZone": "UTC"},
            })
        );

        let birthday = Recurrence::every(Frequency::Yearly);
        assert_eq!(
            pattern_for(&birthday, date(2026, 3, 14), "UTC")["pattern"],
            json!({"type": "absoluteYearly", "interval": 1, "dayOfMonth": 14, "month": 3,
                   "firstDayOfWeek": "sunday"})
        );
        assert_eq!(
            pattern_for(&Recurrence::every(Frequency::Monthly), date(2026, 3, 14), "UTC")["pattern"]
                ["dayOfMonth"],
            json!(14)
        );
        assert_eq!(
            pattern_for(&birthday, date(2026, 3, 14), "UTC")["range"]["type"],
            json!("noEnd")
        );
    }

    #[test]
    fn every_rule_graph_can_hold_survives_the_round_trip() {
        let monday = date(2026, 10, 5);
        let rules = [
            Recurrence::every(Frequency::Daily),
            Recurrence { interval: 3, ..weekly(&[Weekday::Monday, Weekday::Friday]) },
            Recurrence { count: Some(10), ..Recurrence::every(Frequency::Monthly) },
            Recurrence {
                weekdays: vec![Weekday::Thursday],
                week_of_month: Some(-1),
                ..Recurrence::every(Frequency::Monthly)
            },
            Recurrence {
                weekdays: vec![Weekday::Tuesday],
                week_of_month: Some(2),
                count: Some(6),
                ..Recurrence::every(Frequency::Monthly)
            },
            Recurrence { until: Some(date(2030, 10, 5)), ..Recurrence::every(Frequency::Yearly) },
        ];
        for rule in rules {
            let pattern = pattern_for(&rule, monday, "UTC");
            assert_eq!(recurrence_from_pattern(&pattern), Some(rule.clone()), "{pattern}");
        }

        // A weekly rule naming no day means the start's own, and comes back
        // naming it.
        let plain = Recurrence::every(Frequency::Weekly);
        assert_eq!(
            recurrence_from_pattern(&pattern_for(&plain, monday, "UTC")),
            Some(weekly(&[Weekday::Monday]))
        );
    }

    #[test]
    fn a_pattern_the_editor_cannot_hold_is_refused_rather_than_flattened() {
        let thanksgiving = json!({
            "pattern": {"type": "relativeYearly", "interval": 1, "month": 11,
                        "daysOfWeek": ["thursday"], "index": "fourth"},
            "range": {"type": "noEnd", "startDate": "2026-11-26"},
        });
        assert_eq!(recurrence_from_pattern(&thanksgiving), None);

        let first_weekday = json!({
            "pattern": {"type": "relativeMonthly", "interval": 1, "index": "first",
                        "daysOfWeek": ["monday", "tuesday", "wednesday", "thursday", "friday"]},
            "range": {"type": "noEnd", "startDate": "2026-10-01"},
        });
        assert_eq!(recurrence_from_pattern(&first_weekday), None);

        assert_eq!(recurrence_from_pattern(&json!({"pattern": {"type": "hourly"}})), None);
        assert_eq!(recurrence_from_pattern(&Value::Null), None);
    }

    #[test]
    fn an_iana_zone_goes_to_graph_by_its_windows_name_when_the_table_has_one() {
        assert_eq!(zone_name("America/Los_Angeles"), "Pacific Standard Time");
        assert_eq!(zone_name("Europe/Berlin"), "W. Europe Standard Time");
        assert_eq!(zone_name("Pacific/Chatham"), "Pacific/Chatham", "not in the table: as it is");
    }

    // ---- against a mock Graph ---------------------------------------------

    /// One request the mock saw, for a test to assert on afterwards.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        prefer: Vec<String>,
        body: Value,
    }

    /// A mock Graph. `/token` hands out an access token, the way the
    /// parent's own `a_full_delta_after_a_410_...` test does it; every other
    /// request is recorded, then answered by `answer(method, path)`. The
    /// path is the one sent, still percent-encoded. A `Value::Null` answer
    /// goes out with no body at all, as a 204 does.
    async fn mock<F>(answer: F) -> (String, Arc<Mutex<Vec<Seen>>>)
    where
        F: Fn(&str, &str) -> (StatusCode, Value) + Clone + Send + Sync + 'static,
    {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_route = seen.clone();
        let app = axum::Router::new()
            .route(
                "/token",
                axum::routing::post(|| async {
                    Json(json!({
                        "access_token": "access-1",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                    }))
                }),
            )
            .fallback(
                move |method: axum::http::Method,
                      uri: axum::http::Uri,
                      headers: axum::http::HeaderMap,
                      body: axum::body::Bytes| {
                    let seen = seen_by_route.clone();
                    let answer = answer.clone();
                    async move {
                        seen.lock().unwrap().push(Seen {
                            method: method.to_string(),
                            path: uri.path().to_string(),
                            prefer: headers
                                .get_all("prefer")
                                .iter()
                                .filter_map(|v| v.to_str().ok())
                                .map(str::to_string)
                                .collect(),
                            body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                        });
                        match answer(method.as_str(), uri.path()) {
                            (status, Value::Null) => status.into_response(),
                            (status, reply) => (status, Json(reply)).into_response(),
                        }
                    }
                },
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// A signed-in Microsoft account whose token endpoint is the mock's own,
    /// and its default calendar.
    struct SignedIn {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        account: Account,
        calendar: Calendar,
        _dir: tempfile::TempDir,
    }

    fn signed_in(base: &str) -> SignedIn {
        let (svc, vault, dir) = super::super::tests::test_vault();
        let mut account = Account::new(Provider::Microsoft, "person@example.com");
        account.services.calendar = true;
        account.auth = AuthMethod::OAuth {
            client_id: "test-client".into(),
            auth_url: "https://example.test/auth".into(),
            token_url: format!("{base}/token"),
            scopes: vec!["https://graph.microsoft.com/Calendars.ReadWrite".into()],
        };
        vault.save_account(&account).unwrap();
        vault
            .save_account_secret(
                account.id,
                &AccountSecret { refresh_token: Some("refresh-1".into()), ..Default::default() },
            )
            .unwrap();
        let calendar = Calendar::from_account(
            account.id,
            account.provider,
            AccountCalendarSource::Graph,
            PRIMARY,
            "Calendar",
        );
        SignedIn { svc, vault, account, calendar, _dir: dir }
    }

    /// The occurrence the vault's sync filed, as the editor hands it over:
    /// only `uid` and `tz` are ever read from it here -- and `tz` is the
    /// parent's `"UTC"`, as a Graph sync always files it.
    fn occurrence(calendar: &Calendar, uid: &str) -> Event {
        let now = Timestamp::now();
        Event {
            id: EventId::new(),
            calendar_id: calendar.id,
            uid: uid.to_string(),
            title: "Standup".into(),
            description: String::new(),
            location: String::new(),
            start: now,
            end: now,
            local_date: date(2026, 10, 12),
            end_date: date(2026, 10, 12),
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: Vec::new(),
            url: String::new(),
            busy: true,
            series: None,
            updated_at: now,
        }
    }

    fn at(rfc3339: &str) -> Timestamp {
        rfc3339.parse().unwrap()
    }

    fn draft(start: &str, end: &str) -> EventDraft {
        EventDraft {
            title: "Standup".into(),
            description: String::new(),
            location: String::new(),
            start: at(start),
            end: at(end),
            all_day: false,
            tz: "America/Los_Angeles".into(),
            attendees: Vec::new(),
            recurrence: None,
        }
    }

    /// A master's `recurrence` as Graph answers it, every field present
    /// whether it applies or not.
    fn weekly_on_mondays_from(start: &str) -> Value {
        json!({
            "pattern": {"type": "weekly", "interval": 1, "month": 0, "dayOfMonth": 0,
                        "daysOfWeek": ["monday"], "firstDayOfWeek": "sunday", "index": "first"},
            "range": {"type": "noEnd", "startDate": start, "endDate": "0001-01-01",
                      "recurrenceTimeZone": "Pacific Standard Time", "numberOfOccurrences": 0},
        })
    }

    // 2026-10-05 and 2026-10-12 are Mondays, and Los Angeles is on daylight
    // time (UTC-7) all through October: 09:00 there is 16:00 UTC.

    #[tokio::test]
    async fn create_posts_to_the_default_calendar_or_a_named_one_in_graphs_own_shapes() {
        let (base, seen) = mock(|method, _path| match method {
            "POST" => (StatusCode::CREATED, json!({"id": "new-1"})),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base);
        let team = Calendar::from_account(
            s.account.id,
            s.account.provider,
            AccountCalendarSource::Graph,
            "AAMkAD=",
            "Team",
        );

        let mut timed = draft("2026-10-05T09:00:00-07:00", "2026-10-05T09:30:00-07:00");
        timed.description = "Agenda".into();
        timed.location = "Room 4".into();
        timed.attendees =
            vec![Attendee { email: "sam@example.com".into(), name: "Sam".into(), response: None }];
        timed.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday]));
        create_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &timed, &base)
            .await
            .expect("the timed series is created");

        let mut all_day = draft("2026-10-05T00:00:00-07:00", "2026-10-07T00:00:00-07:00");
        all_day.all_day = true;
        create_with_base(&s.svc, &s.vault, &s.account, &team, &all_day, &base)
            .await
            .expect("the all-day event is created");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].path, "/me/calendar/events", "`primary` is Graph's own default");
        let timed = &seen[0].body;
        assert_eq!(timed["subject"], json!("Standup"));
        assert_eq!(timed["body"], json!({"contentType": "text", "content": "Agenda"}));
        assert_eq!(timed["location"], json!({"displayName": "Room 4"}));
        assert_eq!(
            timed["start"],
            json!({"dateTime": "2026-10-05T09:00:00", "timeZone": "Pacific Standard Time"}),
            "a wall clock and a Windows zone name, not an instant"
        );
        assert_eq!(
            timed["end"],
            json!({"dateTime": "2026-10-05T09:30:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(timed["isAllDay"], json!(false));
        assert_eq!(
            timed["attendees"],
            json!([{"emailAddress": {"address": "sam@example.com", "name": "Sam"},
                    "type": "required"}])
        );
        assert_eq!(
            timed["recurrence"],
            json!({
                "pattern": {"type": "weekly", "interval": 1,
                            "daysOfWeek": ["monday", "wednesday"], "firstDayOfWeek": "sunday"},
                "range": {"type": "noEnd", "startDate": "2026-10-05",
                          "recurrenceTimeZone": "Pacific Standard Time"},
            })
        );

        assert_eq!(seen[1].path, "/me/calendars/AAMkAD%3D/events", "a named calendar, encoded");
        let all_day = &seen[1].body;
        assert_eq!(all_day["isAllDay"], json!(true));
        assert_eq!(
            all_day["start"],
            json!({"dateTime": "2026-10-05T00:00:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(all_day["end"]["dateTime"], json!("2026-10-07T00:00:00"), "exclusive");
        assert!(all_day.get("recurrence").is_none(), "a one-off sends no pattern at all");
    }

    #[tokio::test]
    async fn load_reads_the_occurrence_and_its_masters_pattern_into_an_editable_event() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("GET", "/me/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "type": "occurrence", "seriesMasterId": "master-1",
                    "subject": "Standup",
                    "body": {"contentType": "text", "content": "Agenda"},
                    "location": {"displayName": "Room 4"},
                    "start": {"dateTime": "2026-10-12T16:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-12T16:30:00.0000000", "timeZone": "UTC"},
                    "isAllDay": false, "isOrganizer": true,
                    "originalStartTimeZone": "Pacific Standard Time",
                    "organizer": {"emailAddress": {"name": "Person", "address": "person@example.com"}},
                    "attendees": [
                        {"type": "required", "status": {"response": "accepted"},
                         "emailAddress": {"name": "Sam", "address": "sam@example.com"}},
                        {"type": "resource", "status": {"response": "accepted"},
                         "emailAddress": {"name": "Room 4", "address": "room4@example.com"}},
                    ],
                    "recurrence": null,
                }),
            ),
            ("GET", "/me/events/master-1") => (
                StatusCode::OK,
                json!({"id": "master-1", "type": "seriesMaster",
                       "recurrence": weekly_on_mondays_from("2026-10-05")}),
            ),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base);
        let event = occurrence(&s.calendar, "inst-1");

        let loaded =
            load_with_base(&s.svc, &s.vault, &s.account, &event, &base).await.expect("loaded");

        assert_eq!(loaded.event_id, event.id);
        assert!(loaded.recurring);
        assert!(!loaded.custom_recurrence);
        assert_eq!(loaded.draft.recurrence, Some(weekly(&[Weekday::Monday])));
        assert!(loaded.own);
        assert_eq!(loaded.organizer, "Person");
        assert_eq!(loaded.draft.title, "Standup");
        assert_eq!(loaded.draft.description, "Agenda");
        assert_eq!(loaded.draft.location, "Room 4");
        assert_eq!(
            loaded.draft.tz, "America/Los_Angeles",
            "the organiser's own zone, by its Windows name, not the sync's UTC"
        );
        assert_eq!(loaded.draft.start, at("2026-10-12T16:00:00Z"));
        assert_eq!(loaded.draft.end, at("2026-10-12T16:30:00Z"));
        assert_eq!(
            loaded.draft.attendees,
            vec![Attendee {
                email: "sam@example.com".into(),
                name: "Sam".into(),
                response: Some(AttendeeResponse::Accepted),
            }],
            "the room is booked, not invited"
        );

        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.iter().all(|r| {
                r.prefer.iter().any(|p| p == "outlook.timezone=\"UTC\"")
                    && r.prefer.iter().any(|p| p == "outlook.body-content-type=\"text\"")
            }),
            "every read asks for UTC times and a plain-text body: {seen:?}"
        );
    }

    #[tokio::test]
    async fn a_pattern_the_editor_cannot_hold_is_loaded_as_custom() {
        let (base, _seen) = mock(|method, path| match (method, path) {
            ("GET", "/me/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "type": "occurrence", "seriesMasterId": "master-1",
                    "subject": "Thanksgiving",
                    "start": {"dateTime": "2026-11-26T00:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-11-27T00:00:00.0000000", "timeZone": "UTC"},
                    "isAllDay": true, "isOrganizer": false,
                    "originalStartTimeZone": "Pacific Standard Time",
                    "organizer": {"emailAddress": {"name": "", "address": "family@example.com"}},
                }),
            ),
            ("GET", "/me/events/master-1") => (
                StatusCode::OK,
                json!({"id": "master-1", "type": "seriesMaster", "recurrence": {
                    "pattern": {"type": "relativeYearly", "interval": 1, "month": 11,
                                "daysOfWeek": ["thursday"], "index": "fourth"},
                    "range": {"type": "noEnd", "startDate": "2026-11-26"},
                }}),
            ),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base);
        let event = occurrence(&s.calendar, "inst-1");

        let loaded =
            load_with_base(&s.svc, &s.vault, &s.account, &event, &base).await.expect("loaded");

        assert!(loaded.recurring);
        assert!(loaded.custom_recurrence);
        assert_eq!(loaded.draft.recurrence, None);
        assert!(!loaded.own);
        assert_eq!(loaded.organizer, "family@example.com", "no name: the address");
        assert!(loaded.draft.all_day);
        assert_eq!(loaded.draft.start, at("2026-11-26T00:00:00-08:00"), "midnight, on its day");
        assert_eq!(loaded.draft.end, at("2026-11-27T00:00:00-08:00"), "midnight after it");
    }

    #[tokio::test]
    async fn changing_the_series_moves_the_master_by_as_far_as_the_occurrence_moved() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("GET", "/me/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "type": "occurrence", "seriesMasterId": "master-1",
                    "start": {"dateTime": "2026-10-12T16:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-12T16:30:00.0000000", "timeZone": "UTC"},
                    "isAllDay": false,
                }),
            ),
            ("GET", "/me/events/master-1") => (
                StatusCode::OK,
                json!({
                    "id": "master-1", "type": "seriesMaster",
                    "body": {"contentType": "text", "content": "Agenda\r\n"},
                    "location": {"displayName": "Room 4"},
                    "start": {"dateTime": "2026-10-05T16:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-05T16:30:00.0000000", "timeZone": "UTC"},
                    "isAllDay": false,
                    "recurrence": weekly_on_mondays_from("2026-10-05"),
                    "attendees": [
                        {"type": "optional", "status": {"response": "accepted"},
                         "emailAddress": {"name": "Sam", "address": "sam@example.com"}},
                        {"type": "resource", "status": {"response": "accepted"},
                         "emailAddress": {"name": "Room 4", "address": "room4@example.com"}},
                    ],
                }),
            ),
            ("PATCH", "/me/events/master-1") => (StatusCode::OK, json!({"id": "master-1"})),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base);
        let event = occurrence(&s.calendar, "inst-1");

        // The second Monday's standup, dragged from 9:00 to 10:30, "for every
        // event", with nothing else about it touched.
        let mut moved = draft("2026-10-12T10:30:00-07:00", "2026-10-12T11:00:00-07:00");
        moved.description = "Agenda".into();
        moved.location = "Room 4".into();
        moved.attendees = vec![Attendee::new("Sam@Example.com")];
        moved.recurrence = Some(weekly(&[Weekday::Monday]));
        update_with_base(&s.svc, &s.vault, &s.account, &event, &moved, EventScope::Series, &base)
            .await
            .expect("updated");

        let seen = seen.lock().unwrap().clone();
        let patches: Vec<&Seen> = seen.iter().filter(|r| r.method == "PATCH").collect();
        assert_eq!(patches.len(), 1, "{seen:?}");
        let patch = patches[0];
        assert_eq!(patch.path, "/me/events/master-1", "the master, not the occurrence");
        assert_eq!(
            patch.body["start"],
            json!({"dateTime": "2026-10-05T10:30:00", "timeZone": "Pacific Standard Time"}),
            "the first occurrence moves the same hour and a half the second one did"
        );
        assert_eq!(
            patch.body["end"],
            json!({"dateTime": "2026-10-05T11:00:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(patch.body["recurrence"]["range"]["startDate"], json!("2026-10-05"));
        assert_eq!(patch.body["recurrence"]["pattern"]["daysOfWeek"], json!(["monday"]));
        assert!(patch.body.get("body").is_none(), "an untouched description is not rewritten");
        assert!(patch.body.get("location").is_none(), "nor an untouched location");
        assert_eq!(
            patch.body["attendees"],
            json!([
                {"emailAddress": {"address": "sam@example.com", "name": "Sam"}, "type": "optional"},
                {"emailAddress": {"address": "room4@example.com", "name": "Room 4"},
                 "type": "resource"},
            ]),
            "Sam stays optional, and the room stays booked"
        );
    }

    #[tokio::test]
    async fn stopping_a_whole_series_repeating_is_refused_in_words_before_anything_is_sent() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("GET", "/me/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "type": "occurrence", "seriesMasterId": "master-1",
                    "start": {"dateTime": "2026-10-12T16:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-12T16:30:00.0000000", "timeZone": "UTC"},
                }),
            ),
            ("GET", "/me/events/master-1") => (
                StatusCode::OK,
                json!({
                    "id": "master-1", "type": "seriesMaster",
                    "start": {"dateTime": "2026-10-05T16:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-05T16:30:00.0000000", "timeZone": "UTC"},
                    "recurrence": weekly_on_mondays_from("2026-10-05"),
                }),
            ),
            _ => (StatusCode::OK, json!({})),
        })
        .await;
        let s = signed_in(&base);
        let event = occurrence(&s.calendar, "inst-1");

        let once = draft("2026-10-12T09:00:00-07:00", "2026-10-12T09:30:00-07:00");
        let err = update_with_base(
            &s.svc,
            &s.vault,
            &s.account,
            &event,
            &once,
            EventScope::Series,
            &base,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        assert!(err.message.contains("Outlook cannot stop a series"), "{}", err.message);
        assert!(
            seen.lock().unwrap().iter().all(|r| r.method == "GET"),
            "nothing was written on the way to saying no"
        );
    }

    #[tokio::test]
    async fn deleting_a_series_deletes_its_master_and_one_already_gone_is_no_error() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("GET", "/me/events/inst-1") => (
                StatusCode::OK,
                json!({"id": "inst-1", "type": "occurrence", "seriesMasterId": "master-1"}),
            ),
            ("DELETE", "/me/events/master-1") => (StatusCode::NO_CONTENT, Value::Null),
            _ => (
                StatusCode::NOT_FOUND,
                json!({"error": {"code": "ErrorItemNotFound",
                                 "message": "The specified object was not found in the store."}}),
            ),
        })
        .await;
        let s = signed_in(&base);

        for (uid, scope) in [("inst-1", EventScope::Series), ("gone-1", EventScope::Occurrence)] {
            let event = occurrence(&s.calendar, uid);
            delete_with_base(&s.svc, &s.vault, &s.account, &event, scope, &base)
                .await
                .unwrap_or_else(|e| panic!("deleting {uid} failed: {e:?}"));
        }

        let seen = seen.lock().unwrap().clone();
        let deletes: Vec<&str> =
            seen.iter().filter(|r| r.method == "DELETE").map(|r| r.path.as_str()).collect();
        assert_eq!(
            deletes,
            vec!["/me/events/master-1", "/me/events/gone-1"],
            "the series goes through its master; a 404 is already gone"
        );
    }

    #[tokio::test]
    async fn a_403_on_a_write_is_the_calendars_refusal_and_only_a_401_is_the_credential() {
        let (base, _seen) = mock(|_method, path| match path {
            "/me/calendar/events" => (
                StatusCode::FORBIDDEN,
                json!({"error": {"code": "ErrorAccessDenied",
                                 "message": "Access is denied. Check credentials and try again."}}),
            ),
            _ => (StatusCode::UNAUTHORIZED, Value::Null),
        })
        .await;
        let s = signed_in(&base);
        let new = draft("2026-10-05T09:00:00-07:00", "2026-10-05T09:30:00-07:00");

        let err = create_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &new, &base)
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            codes::INVALID,
            "a calendar that will not take the event is not a bad credential, or the account \
             would be sent to sign in again for nothing"
        );
        assert!(err.message.contains("Access is denied"), "Graph's own reason: {}", err.message);
        assert!(!err.message.contains("access-1"), "never the token: {}", err.message);

        let shared = Calendar::from_account(
            s.account.id,
            s.account.provider,
            AccountCalendarSource::Graph,
            "revoked",
            "Old",
        );
        let err =
            create_with_base(&s.svc, &s.vault, &s.account, &shared, &new, &base).await.unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
    }
}
