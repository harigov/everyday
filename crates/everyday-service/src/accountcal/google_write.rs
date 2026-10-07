//! Google Calendar: putting events on it, changing them, and taking them off
//! again, over the same REST API the parent module reads with.
//!
//! The reading half is the parent module; this is the half that writes,
//! sharing the parent's token, its id encoding and its reading of Google's
//! rate limits. See `accountcal`'s module doc, "Writing", for the shape every
//! source follows: write to the server, and let the calendar's own sync bring
//! the result back. Nothing here touches an [`Event`] row.
//!
//! # Four calls
//!
//! | Write | Request |
//! |---|---|
//! | [`create`] | `POST calendars/{cal}/events?sendUpdates=all` |
//! | [`load`] | `GET calendars/{cal}/events/{id}`, and the series' own event for its rule |
//! | [`update`] | `PATCH` the occurrence, or the series' own event |
//! | [`delete`] | `DELETE` the occurrence, or the series' own event |
//!
//! `sendUpdates=all` on every write is what makes Google, not this
//! application, the one that tells the guests: an invitation to whoever was
//! added, a cancellation to whoever was taken off (or to everybody, when the
//! event goes), and an update to the rest. Without it Google changes the
//! organiser's copy quietly and every guest's calendar drifts from it.
//!
//! # Occurrences and series
//!
//! The parent's sync asks for `singleEvents=true`, so every [`Event`] this
//! vault holds is one *instance* -- `Event::uid` is that instance's own id
//! (`abc123_20261012T160000Z`), and the rule it came from lives on a separate
//! event, the series' own, named by the instance's `recurringEventId`.
//! Changing "just this one" patches the instance, which Google keeps as an
//! exception to the series; changing the series patches the series' event,
//! moved by however far the instance moved (see
//! [`super::draft_for_series`]); and reading the rule means fetching the
//! series' event too, because an instance does not carry one.
//!
//! # PATCH, not PUT -- and what a PATCH still replaces
//!
//! `events.update` (PUT) replaces the whole event with whatever is sent, and
//! this application does not model most of an event: reminders, colour,
//! conference links, attachments. PATCH changes only the fields it names. But
//! an *array* in a PATCH still replaces the whole array, and `attendees` is
//! one: sending the editor's guest list as it stands would reset every
//! guest's answer and cancel a booked room the editor never shows. So each
//! guest who was already there is sent back as Google's own entry for them,
//! answer and `optional` flag included, and a room (`resource: true`) stays
//! whether the editor mentioned it or not. `recurrence` is an array too, and
//! only its `RRULE` line is ever swapped -- an `EXDATE` an import brought in
//! stays where it was.
//!
//! # Which failure is which
//!
//! A 401 is the credential, exactly as on a read: [`codes::FORBIDDEN`],
//! which moves the account to `NeedsSignIn`. A 403 on a write is *not* --
//! the account can still read perfectly well; it is this one calendar or
//! event that will not take the change (a calendar shared read-only,
//! somebody else's invitation) -- so it is [`codes::INVALID`], in Google's
//! own words. Google's rate-limit 403s, 429 and 503 are waited out the way
//! the parent's reads wait them out, then [`codes::RATE_LIMITED`]. A 404 or
//! 410 means the event has gone: the end a delete wanted anyway, and
//! "refresh it" for anything else.

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    Attendee, Calendar, CalendarOrigin, EditableEvent, Event, EventDraft, EventScope,
};
use everyday_core::mail::AttendeeResponse;
use everyday_core::recurrence::Recurrence;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::time::sleep;

use super::{
    API, GoogleWhen, LocalStart, bearer, draft_for_series, is_rate_limit_reason, urlencoding_light,
    zone_or_utc,
};
use crate::accountcal::{calendar_after_retry_after, retry_after_delay};
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::service::Service;

/// One event as `events.get` answers it -- only what a write needs to read.
///
/// `attendees` stays as Google's own JSON rather than a typed list, so a
/// guest who is already there can be sent back exactly as Google knows them;
/// see the module doc.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    location: String,
    start: Option<GoogleWhen>,
    end: Option<GoogleWhen>,
    /// `RRULE:`, `EXDATE...` and `RDATE...` lines -- on a series' own event
    /// only; an instance carries none.
    #[serde(default)]
    recurrence: Vec<String>,
    recurring_event_id: Option<String>,
    organizer: Option<WirePerson>,
    #[serde(default)]
    attendees: Vec<Value>,
}

impl WireEvent {
    fn all_day(&self) -> bool {
        self.start.as_ref().is_some_and(|when| when.date.is_some())
    }

    /// The id of the event that holds this one's rule: the series' own for an
    /// instance, this event itself when it carries the rule, and `None` for
    /// an event that does not repeat at all.
    fn series(&self) -> Option<String> {
        self.recurring_event_id
            .clone()
            .or_else(|| (!self.recurrence.is_empty()).then(|| self.id.clone()))
    }
}

/// An organiser or a guest, as Google names them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePerson {
    #[serde(default)]
    email: String,
    display_name: Option<String>,
    /// Google's "this is the calendar you are reading" -- on the organiser,
    /// the whole of how [`load`] knows whether an event is the account's own.
    #[serde(rename = "self")]
    is_self: Option<bool>,
    /// A room or a piece of equipment, booked rather than invited.
    resource: Option<bool>,
    response_status: Option<String>,
}

impl WirePerson {
    fn label(&self) -> String {
        self.display_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| self.email.clone())
    }
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
    calendar: &Calendar,
    event: &Event,
) -> CommandResult<EditableEvent> {
    load_with_base(svc, vault, account, calendar, event, API).await
}

pub(super) async fn update(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
) -> CommandResult<()> {
    update_with_base(svc, vault, account, calendar, event, draft, scope, API).await
}

pub(super) async fn delete(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    scope: EventScope,
) -> CommandResult<()> {
    delete_with_base(svc, vault, account, calendar, event, scope, API).await
}

/// [`create`]'s own body, over `base` rather than the hardcoded [`API`] --
/// the same split as the parent's `sync_with_base`, so a test can point the
/// whole conversation at a mock server. The other three `*_with_base`
/// functions below are split for the same reason.
async fn create_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    draft: &EventDraft,
    base: &str,
) -> CommandResult<()> {
    let cal = calendar_path(calendar)?;
    let token = bearer(svc, vault, account).await?;
    let mut body = event_body(draft, &[], false);
    if let Some(rule) = &draft.recurrence {
        body.insert("recurrence".into(), json!([rrule_line(rule, draft)]));
    }
    let url = format!("{base}/calendars/{cal}/events?sendUpdates=all");
    send(Method::POST, &url, &token, Some(&Value::Object(body))).await?;
    Ok(())
}

async fn load_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    base: &str,
) -> CommandResult<EditableEvent> {
    let cal = calendar_path(calendar)?;
    let token = bearer(svc, vault, account).await?;
    let item = get_event(base, &cal, &event.uid, &token).await?;
    if item.status == "cancelled" {
        return Err(gone());
    }

    // An instance does not carry its series' rule; the series' own event
    // does. One this account cannot open is reported as a rule the editor
    // cannot show, rather than refusing the whole event over it.
    let lines = match &item.recurring_event_id {
        Some(series) => match get_event(base, &cal, series, &token).await {
            Ok(series) => series.recurrence,
            Err(e) if e.code == codes::NOT_FOUND => Vec::new(),
            Err(e) => return Err(e),
        },
        None => item.recurrence.clone(),
    };
    // A timed event names its zone; an all-day one does not, and the zone
    // the sync already filed it under is the next best thing.
    let tz = item
        .start
        .as_ref()
        .and_then(|when| when.time_zone.clone())
        .into_iter()
        .chain([event.tz.clone()])
        .find(|tz| TimeZone::get(tz).is_ok())
        .unwrap_or_else(|| "UTC".to_string());
    let recurring = item.recurring_event_id.is_some() || !lines.is_empty();
    let recurrence = if recurring { single_rule(&lines, &tz) } else { None };
    let custom_recurrence = recurring && recurrence.is_none();
    let (start, end) = draft_bounds(&item, &zone_or_utc(&tz))?;
    let all_day = item.all_day();
    let attendees = guests(&item.attendees);
    // No organiser at all is an event nobody else can claim.
    let own = item.organizer.as_ref().is_none_or(|o| o.is_self == Some(true));
    let organizer = item.organizer.as_ref().map(WirePerson::label).unwrap_or_default();

    Ok(EditableEvent {
        event_id: event.id,
        calendar_id: event.calendar_id,
        draft: EventDraft {
            title: item.summary,
            // As Google holds it, HTML and all: written back untouched
            // unless somebody changes it.
            description: item.description,
            location: item.location,
            start,
            end,
            all_day,
            tz,
            attendees,
            recurrence,
        },
        recurring,
        custom_recurrence,
        own,
        organizer,
    })
}

#[allow(clippy::too_many_arguments)]
async fn update_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
    base: &str,
) -> CommandResult<()> {
    let cal = calendar_path(calendar)?;
    let token = bearer(svc, vault, account).await?;
    // Read first: whether this is part of a series, where it is now, and
    // who is already on it are all the server's to say, not the vault's.
    let item = get_event(base, &cal, &event.uid, &token).await?;
    if item.status == "cancelled" {
        return Err(gone());
    }
    let series = item.series();
    let repeats = series.is_some();

    let Some(series) = series.filter(|_| scope == EventScope::Series) else {
        // Just this occurrence -- or the whole event, when it does not
        // repeat, which is the one case a rule can be added here: a one-off
        // made into a series.
        let mut body = event_body(draft, &item.attendees, item.all_day() != draft.all_day);
        if !repeats && let Some(rule) = &draft.recurrence {
            body.insert("recurrence".into(), json!([rrule_line(rule, draft)]));
        }
        return patch(base, &cal, &event.uid, &token, body).await;
    };

    let fetched =
        if series == item.id { None } else { Some(get_event(base, &cal, &series, &token).await?) };
    let master = fetched.as_ref().unwrap_or(&item);
    let recurrence = series_recurrence(&master.recurrence, draft);
    // "Does not repeat", chosen for the whole series: what is left is one
    // event, and the one somebody was looking at is where they will expect
    // to find it -- not the series' first occurrence, weeks back.
    let stops = matches!(&recurrence, Some(Value::Array(lines)) if lines.is_empty());
    let times = if stops {
        draft.clone()
    } else {
        let zone = zone_or_utc(&draft.tz);
        draft_for_series(
            draft,
            local(master.start.as_ref(), &zone)?,
            local(item.start.as_ref(), &zone)?,
        )?
    };
    let mut body = event_body(&times, &master.attendees, master.all_day() != draft.all_day);
    if let Some(recurrence) = recurrence {
        body.insert("recurrence".into(), recurrence);
    }
    patch(base, &cal, &series, &token, body).await
}

async fn delete_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    scope: EventScope,
    base: &str,
) -> CommandResult<()> {
    let cal = calendar_path(calendar)?;
    let token = bearer(svc, vault, account).await?;
    let id = match scope {
        EventScope::Occurrence => event.uid.clone(),
        EventScope::Series => match get_event(base, &cal, &event.uid, &token).await {
            Ok(item) => item.series().unwrap_or_else(|| event.uid.clone()),
            Err(e) if e.code == codes::NOT_FOUND => return Ok(()),
            Err(e) => return Err(e),
        },
    };
    let url = format!("{}?sendUpdates=all", event_url(base, &cal, &id));
    match send(Method::DELETE, &url, &token, None).await {
        Ok(_) => Ok(()),
        // Already gone -- from another device, or by a delete whose answer
        // never arrived. Either way, what was asked for is true.
        Err(e) if e.code == codes::NOT_FOUND => Ok(()),
        Err(e) => Err(e),
    }
}

/// The calendar's id, ready for a URL path -- see the parent's
/// [`urlencoding_light`] for why it needs encoding at all.
fn calendar_path(calendar: &Calendar) -> CommandResult<String> {
    match &calendar.origin {
        CalendarOrigin::Account { remote_id, .. } => Ok(urlencoding_light(remote_id)),
        _ => Err(CommandError::new(codes::INVALID, "not an account calendar")),
    }
}

fn event_url(base: &str, cal: &str, id: &str) -> String {
    format!("{base}/calendars/{cal}/events/{}", urlencoding_light(id))
}

async fn get_event(base: &str, cal: &str, id: &str, token: &str) -> CommandResult<WireEvent> {
    let answer = send(Method::GET, &event_url(base, cal, id), token, None).await?;
    serde_json::from_value(answer.unwrap_or(Value::Null)).map_err(|e| {
        CommandError::new(codes::NETWORK, format!("could not read Google's answer: {e}"))
    })
}

async fn patch(
    base: &str,
    cal: &str,
    id: &str,
    token: &str,
    body: Map<String, Value>,
) -> CommandResult<()> {
    let url = format!("{}?sendUpdates=all", event_url(base, cal, id));
    send(Method::PATCH, &url, token, Some(&Value::Object(body))).await?;
    Ok(())
}

/// Everything about `draft` a create or a PATCH sends. `existing` is the
/// event's guest list as Google has it now -- empty for a new event -- and
/// `switching` says the event is changing between all-day and timed, which is
/// the one time the *other* half of `start`/`end` has to be cleared: a PATCH
/// merges an object field by field, and a `date` left beside a new
/// `dateTime` would be an event that is both.
fn event_body(draft: &EventDraft, existing: &[Value], switching: bool) -> Map<String, Value> {
    let (start, end) = bounds_json(draft, switching);
    let mut body = Map::new();
    body.insert("summary".into(), json!(draft.title));
    body.insert("description".into(), json!(draft.description));
    body.insert("location".into(), json!(draft.location));
    body.insert("start".into(), start);
    body.insert("end".into(), end);
    body.insert("attendees".into(), attendees_json(&draft.attendees, existing));
    body
}

/// `start` and `end` as Google's `EventDateTime`: a bare `date` for an
/// all-day event, its end the day *after* the last (exclusive, like RFC
/// 5545's), and for a timed one an RFC 3339 instant with the zone's own
/// offset *and* the zone's name. The name is not decoration: Google expands a
/// series in it -- "every Monday at nine" means nine in that zone across a
/// change of clocks -- and refuses a repeating event without one.
fn bounds_json(draft: &EventDraft, switching: bool) -> (Value, Value) {
    if draft.all_day {
        let on = |day: Date| {
            let mut when = json!({ "date": day.to_string() });
            if switching {
                when["dateTime"] = Value::Null;
            }
            when
        };
        (on(draft.start_date()), on(draft.end_date_exclusive()))
    } else {
        let zone = zone_or_utc(&draft.tz);
        let at = |instant: Timestamp| {
            let mut when = json!({
                "dateTime": instant.display_with_offset(zone.to_offset(instant)).to_string(),
                "timeZone": draft.tz,
            });
            if switching {
                when["date"] = Value::Null;
            }
            when
        };
        (at(draft.start), at(draft.end))
    }
}

/// The guest list as Google's `attendees` array. A guest already on the
/// event is sent back as Google's own entry for them -- their answer, their
/// `optional` flag, anything else Google keeps -- with only the name the
/// editor gave laid over it; a new guest is just an address and a name. A
/// room the event had booked is kept even though the editor never lists one:
/// the array replaces the old one wholesale, and leaving it out would cancel
/// the booking. See the module doc.
fn attendees_json(guests: &[Attendee], existing: &[Value]) -> Value {
    let mut out: Vec<Value> = guests
        .iter()
        .map(|guest| {
            let wanted = Some(guest.key());
            let mut entry = existing
                .iter()
                .find(|entry| email_of(entry) == wanted)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_else(|| {
                    let mut fresh = Map::new();
                    fresh.insert("email".into(), json!(guest.email));
                    fresh
                });
            if !guest.name.is_empty() {
                entry.insert("displayName".into(), json!(guest.name));
            }
            Value::Object(entry)
        })
        .collect();
    for entry in existing {
        let room = entry.get("resource").and_then(Value::as_bool) == Some(true);
        if room && !out.iter().any(|kept| email_of(kept) == email_of(entry)) {
            out.push(entry.clone());
        }
    }
    Value::Array(out)
}

/// An attendee entry's address, compared the way [`Attendee::key`] compares
/// them.
fn email_of(entry: &Value) -> Option<String> {
    entry.get("email").and_then(Value::as_str).map(|e| e.trim().to_ascii_lowercase())
}

/// Google's guest list as the editor's: every guest, the organiser among
/// them when Google lists them, with their answer -- but not a room, which is
/// booked rather than invited and is never the editor's to change.
fn guests(entries: &[Value]) -> Vec<Attendee> {
    entries
        .iter()
        .filter_map(|entry| WirePerson::deserialize(entry).ok())
        .filter(|person| person.resource != Some(true) && !person.email.is_empty())
        .map(|person| Attendee {
            response: person.response_status.as_deref().and_then(response),
            name: person.display_name.unwrap_or_default(),
            email: person.email,
        })
        .collect()
}

fn response(status: &str) -> Option<AttendeeResponse> {
    match status {
        "accepted" => Some(AttendeeResponse::Accepted),
        "tentative" => Some(AttendeeResponse::Tentative),
        "declined" => Some(AttendeeResponse::Declined),
        "needsAction" => Some(AttendeeResponse::NeedsAction),
        _ => None,
    }
}

/// `rule` as one line of Google's `recurrence` array. `UNTIL` comes out in
/// the form RFC 5545 wants beside the event's own start -- see
/// [`Recurrence::to_rrule`] -- which is why it needs `all_day` and the zone.
fn rrule_line(rule: &Recurrence, draft: &EventDraft) -> String {
    format!("RRULE:{}", rule.to_rrule(draft.all_day, &draft.tz))
}

fn is_rrule(line: &str) -> bool {
    line.trim_start().get(..6).is_some_and(|head| head.eq_ignore_ascii_case("RRULE:"))
}

/// A series' rule as a [`Recurrence`], when its `recurrence` lines hold
/// exactly one `RRULE` and [`Recurrence::from_rrule_in`] can hold that. Two
/// rules, none at all (a series of `RDATE`s alone), or one that will not fit
/// -- "the last weekday of the month" -- all answer `None`, which [`load`]
/// reports as a rule that can only be changed where it was made.
fn single_rule(lines: &[String], tz: &str) -> Option<Recurrence> {
    let mut rules = lines.iter().filter(|line| is_rrule(line));
    match (rules.next(), rules.next()) {
        (Some(only), None) => Recurrence::from_rrule_in(only.trim()[6..].trim(), tz),
        _ => None,
    }
}

/// What a whole-series change writes to the series' `recurrence`, or `None`
/// to leave it exactly as it is.
///
/// A rule chosen in the editor replaces the series' `RRULE` line and only
/// that, in place; every other line stays. No rule chosen means one of two
/// things, told apart by what the series has now: over a rule the editor
/// could show, somebody picked "does not repeat", which is an empty array;
/// over one it could not, the editor never offered the rule for change at
/// all, and it is left alone.
fn series_recurrence(lines: &[String], draft: &EventDraft) -> Option<Value> {
    match &draft.recurrence {
        Some(rule) => {
            let line = rrule_line(rule, draft);
            let mut out = Vec::with_capacity(lines.len() + 1);
            let mut placed = false;
            for existing in lines {
                if !is_rrule(existing) {
                    out.push(existing.clone());
                } else if !placed {
                    out.push(line.clone());
                    placed = true;
                }
            }
            if !placed {
                out.insert(0, line);
            }
            Some(json!(out))
        }
        None if single_rule(lines, &draft.tz).is_some() => Some(json!([])),
        None => None,
    }
}

/// `item`'s start and end as a draft holds them: instants, with an all-day
/// event running from midnight of its first day to midnight after its last
/// in `zone` -- the shape `EventDraft::validate` gives every all-day draft.
fn draft_bounds(item: &WireEvent, zone: &TimeZone) -> CommandResult<(Timestamp, Timestamp)> {
    let start = item.start.as_ref().ok_or_else(unreadable)?;
    let end = item.end.as_ref().unwrap_or(start);
    if let Some(first) = &start.date {
        let first = parse_day(first)?;
        let after = match &end.date {
            Some(after) => parse_day(after)?,
            None => first,
        };
        let after = after.max(first.tomorrow().map_err(|_| unreadable())?);
        let midnight =
            |day: Date| day.to_zoned(zone.clone()).map(|z| z.timestamp()).map_err(|_| unreadable());
        return Ok((midnight(first)?, midnight(after)?));
    }
    let from = parse_instant(start.date_time.as_deref().ok_or_else(unreadable)?)?;
    let to = match end.date_time.as_deref() {
        Some(to) => parse_instant(to)?,
        None => from,
    };
    Ok((from, to.max(from)))
}

/// Where `when` falls on `zone`'s wall clock, for [`draft_for_series`].
fn local(when: Option<&GoogleWhen>, zone: &TimeZone) -> CommandResult<LocalStart> {
    let when = when.ok_or_else(unreadable)?;
    match (&when.date, &when.date_time) {
        (Some(day), _) => Ok(LocalStart::Day(parse_day(day)?)),
        (None, Some(at)) => {
            Ok(LocalStart::At(parse_instant(at)?.to_zoned(zone.clone()).datetime()))
        }
        (None, None) => Err(unreadable()),
    }
}

fn parse_day(raw: &str) -> CommandResult<Date> {
    raw.parse::<Date>().map_err(|_| unreadable())
}

fn parse_instant(raw: &str) -> CommandResult<Timestamp> {
    raw.parse::<Timestamp>().map_err(|_| unreadable())
}

fn unreadable() -> CommandError {
    CommandError::new(codes::NETWORK, "Google's answer had a time in it that could not be read")
}

fn gone() -> CommandError {
    CommandError::new(codes::NOT_FOUND, "that event is no longer on the calendar; refresh it")
}

/// One request, any method, with a bearer token and an optional JSON body --
/// answering Google's JSON, or `None` when there is none (a 204 from a
/// delete). Waits out a rate limit the way the parent's [`super::get_bytes`]
/// does, honouring `Retry-After`; everything else that is not a 2xx becomes
/// the code the module doc's "Which failure is which" names, through
/// [`refusal`]. The token never appears in an error: nothing here formats
/// the request, only Google's answer to it.
async fn send(
    method: Method,
    url: &str,
    token: &str,
    body: Option<&Value>,
) -> CommandResult<Option<Value>> {
    let payload = body.map(serde_json::to_vec).transpose().map_err(|e| {
        CommandError::new(
            codes::INTERNAL,
            format!("could not write that event out for Google: {e}"),
        )
    })?;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut request = http::client()?.request(method.clone(), url).bearer_auth(token);
        if let Some(payload) = &payload {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(payload.clone());
        }
        let response = request.send().await.map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not reach Google Calendar: {e}"))
        })?;
        let status = response.status().as_u16();
        let retry_after = retry_after_delay(response.headers());
        let answer = response.bytes().await.map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not read Google's answer: {e}"))
        })?;
        if matches!(status, 429 | 503) || (status == 403 && is_rate_limit_reason(&answer)) {
            let policy = calendar_after_retry_after();
            if policy.gives_up_after(attempt) {
                return Err(CommandError::new(
                    codes::RATE_LIMITED,
                    "Google Calendar is turning this account's requests away for now; try again \
                     in a minute",
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
            CommandError::new(codes::NETWORK, format!("could not read Google's answer: {e}"))
        });
    }
}

/// A non-2xx answer to `method`, as the code and sentence the module doc's
/// "Which failure is which" gives it.
fn refusal(method: &Method, status: u16, answer: &[u8]) -> CommandError {
    let because = google_says(answer).map(|reason| format!(": {reason}")).unwrap_or_default();
    match status {
        401 => CommandError::new(codes::FORBIDDEN, "Google refused this account's credential"),
        400 => {
            CommandError::new(codes::INVALID, format!("Google would not take that event{because}"))
        }
        403 => CommandError::new(
            codes::INVALID,
            format!("Google would not let you {}{because}", doing(method)),
        ),
        404 | 410 if method.as_str() == "POST" => CommandError::new(
            codes::NOT_FOUND,
            "that calendar is no longer on Google; refresh your calendars",
        ),
        404 | 410 => gone(),
        409 | 412 => CommandError::new(
            codes::CONFLICT,
            "that event changed on Google while it was being saved; refresh it and try again",
        ),
        _ => CommandError::new(codes::NETWORK, format!("Google Calendar answered {status}")),
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

/// Google's own sentence for a refusal -- `error.message` -- when it gave
/// one. It names the calendar's rule, never the request's credential.
fn google_says(answer: &[u8]) -> Option<String> {
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
    use everyday_core::recurrence::{Frequency, Weekday};

    /// One request the mock saw, for a test to assert on afterwards.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        query: String,
        body: Value,
    }

    /// A mock Google. `/token` hands out an access token, the way the
    /// parent's own `a_full_resync_after_a_410_...` test does it; every
    /// other request is recorded, then answered by `answer(method, path)`.
    /// The path is the one sent, still percent-encoded. A `Value::Null`
    /// answer goes out with no body at all, as a 204 does.
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
                move |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| {
                    let seen = seen_by_route.clone();
                    let answer = answer.clone();
                    async move {
                        seen.lock().unwrap().push(Seen {
                            method: method.to_string(),
                            path: uri.path().to_string(),
                            query: uri.query().unwrap_or_default().to_string(),
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

    /// A signed-in Google account whose token endpoint is the mock's own,
    /// and one of its calendars, `remote_id` on Google.
    struct SignedIn {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        account: Account,
        calendar: Calendar,
        _dir: tempfile::TempDir,
    }

    fn signed_in(base: &str, remote_id: &str) -> SignedIn {
        let (svc, vault, dir) = super::super::tests::test_vault();
        let mut account = Account::new(Provider::Google, "person@example.com");
        account.services.calendar = true;
        account.auth = AuthMethod::OAuth {
            client_id: "test-client".into(),
            auth_url: "https://example.test/auth".into(),
            token_url: format!("{base}/token"),
            scopes: vec!["https://www.googleapis.com/auth/calendar".into()],
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
            AccountCalendarSource::Google,
            remote_id,
            "Work",
        );
        SignedIn { svc, vault, account, calendar, _dir: dir }
    }

    /// The occurrence the vault's sync filed, as the editor hands it over:
    /// only `uid` and `tz` are ever read from it here.
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
            local_date: jiff::civil::date(2026, 10, 12),
            end_date: jiff::civil::date(2026, 10, 12),
            tz: "America/Los_Angeles".into(),
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

    fn weekly(days: &[Weekday]) -> Recurrence {
        Recurrence { weekdays: days.to_vec(), ..Recurrence::every(Frequency::Weekly) }
    }

    // 2026-10-05 and 2026-10-12 are Mondays, and Los Angeles is on daylight
    // time (UTC-7) all through October.

    #[tokio::test]
    async fn create_sends_googles_own_shapes_to_the_encoded_calendar_and_lets_google_invite() {
        let (base, seen) = mock(|method, _path| match method {
            "POST" => (StatusCode::OK, json!({"id": "new-1"})),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base, "en.usa#holiday@group.v.calendar.google.com");

        let mut timed = draft("2026-10-05T09:00:00-07:00", "2026-10-05T09:30:00-07:00");
        timed.attendees = vec![
            Attendee { email: "sam@example.com".into(), name: "Sam".into(), response: None },
            Attendee::new("kim@example.com"),
        ];
        timed.recurrence = Some(weekly(&[Weekday::Wednesday, Weekday::Monday]));
        create_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &timed, &base)
            .await
            .expect("the timed series is created");

        let mut all_day = draft("2026-10-05T00:00:00-07:00", "2026-10-07T00:00:00-07:00");
        all_day.all_day = true;
        create_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &all_day, &base)
            .await
            .expect("the all-day event is created");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].method, "POST");
        assert_eq!(
            seen[0].path, "/calendars/en.usa%23holiday%40group.v.calendar.google.com/events",
            "a bare `#` would end the path, and Google would never see the rest of the id"
        );
        assert_eq!(seen[0].query, "sendUpdates=all", "Google, not this app, tells the guests");

        let timed = &seen[0].body;
        assert_eq!(timed["summary"], json!("Standup"));
        assert_eq!(
            timed["start"],
            json!({"dateTime": "2026-10-05T09:00:00-07:00", "timeZone": "America/Los_Angeles"})
        );
        assert_eq!(
            timed["end"],
            json!({"dateTime": "2026-10-05T09:30:00-07:00", "timeZone": "America/Los_Angeles"})
        );
        assert_eq!(timed["recurrence"], json!(["RRULE:FREQ=WEEKLY;BYDAY=MO,WE"]));
        assert_eq!(
            timed["attendees"],
            json!([
                {"email": "sam@example.com", "displayName": "Sam"},
                {"email": "kim@example.com"},
            ]),
            "a guest with no name sends no displayName at all"
        );

        let all_day = &seen[1].body;
        assert_eq!(all_day["start"], json!({"date": "2026-10-05"}));
        assert_eq!(all_day["end"], json!({"date": "2026-10-07"}), "exclusive, like RFC 5545's");
        assert!(all_day.get("recurrence").is_none(), "a one-off sends no rule at all");
    }

    #[tokio::test]
    async fn load_reads_the_occurrence_and_its_series_rule_into_an_editable_event() {
        let (base, _seen) = mock(|method, path| match (method, path) {
            ("GET", "/calendars/cal-1/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "status": "confirmed", "summary": "Standup",
                    "description": "<b>Agenda</b>", "location": "Room 4",
                    "recurringEventId": "master-1",
                    "start": {"dateTime": "2026-10-12T09:00:00-07:00", "timeZone": "America/Los_Angeles"},
                    "end": {"dateTime": "2026-10-12T09:30:00-07:00", "timeZone": "America/Los_Angeles"},
                    "organizer": {"email": "person@example.com", "self": true},
                    "attendees": [
                        {"email": "person@example.com", "organizer": true, "self": true,
                         "responseStatus": "accepted"},
                        {"email": "sam@example.com", "displayName": "Sam",
                         "responseStatus": "tentative"},
                        {"email": "room-4@resource.calendar.google.com", "resource": true,
                         "responseStatus": "accepted"},
                    ],
                }),
            ),
            ("GET", "/calendars/cal-1/events/master-1") => (
                StatusCode::OK,
                json!({"id": "master-1", "status": "confirmed",
                       "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO"]}),
            ),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base, "cal-1");
        let event = occurrence(&s.calendar, "inst-1");

        let loaded = load_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &event, &base)
            .await
            .expect("loaded");

        assert_eq!(loaded.event_id, event.id);
        assert_eq!(loaded.calendar_id, s.calendar.id);
        assert!(loaded.recurring);
        assert!(!loaded.custom_recurrence);
        assert_eq!(loaded.draft.recurrence, Some(weekly(&[Weekday::Monday])));
        assert!(loaded.own, "organised by the calendar being read");
        assert_eq!(loaded.organizer, "person@example.com");
        assert_eq!(loaded.draft.title, "Standup");
        assert_eq!(loaded.draft.description, "<b>Agenda</b>", "exactly as Google holds it");
        assert_eq!(loaded.draft.location, "Room 4");
        assert_eq!(loaded.draft.tz, "America/Los_Angeles");
        assert!(!loaded.draft.all_day);
        assert_eq!(loaded.draft.start, at("2026-10-12T16:00:00Z"));
        assert_eq!(loaded.draft.end, at("2026-10-12T16:30:00Z"));
        assert_eq!(
            loaded.draft.attendees,
            vec![
                Attendee {
                    email: "person@example.com".into(),
                    name: String::new(),
                    response: Some(AttendeeResponse::Accepted),
                },
                Attendee {
                    email: "sam@example.com".into(),
                    name: "Sam".into(),
                    response: Some(AttendeeResponse::Tentative),
                },
            ],
            "the room is booked, not invited, and is not the editor's to show"
        );
    }

    #[tokio::test]
    async fn a_rule_the_editor_cannot_hold_is_reported_as_custom_rather_than_flattened() {
        let (base, _seen) = mock(|method, path| match (method, path) {
            ("GET", "/calendars/cal-1/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "status": "confirmed", "summary": "Payroll",
                    "recurringEventId": "master-1",
                    "start": {"date": "2026-10-30"}, "end": {"date": "2026-10-31"},
                    "organizer": {"email": "boss@example.com", "displayName": "The Boss"},
                }),
            ),
            ("GET", "/calendars/cal-1/events/master-1") => (
                StatusCode::OK,
                json!({"id": "master-1",
                       "recurrence": ["RRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"]}),
            ),
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base, "cal-1");
        let event = occurrence(&s.calendar, "inst-1");

        let loaded = load_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &event, &base)
            .await
            .expect("loaded");

        assert!(loaded.recurring);
        assert!(loaded.custom_recurrence, "the last weekday of the month is not a Recurrence");
        assert_eq!(loaded.draft.recurrence, None);
        assert!(!loaded.own, "somebody else's invitation");
        assert_eq!(loaded.organizer, "The Boss");
        assert!(loaded.draft.all_day);
        assert_eq!(
            loaded.draft.tz, "America/Los_Angeles",
            "an all-day event names no zone of its own; the synced row's stands in"
        );
        assert_eq!(loaded.draft.start, at("2026-10-30T00:00:00-07:00"));
        assert_eq!(loaded.draft.end, at("2026-10-31T00:00:00-07:00"), "midnight after its day");
    }

    #[tokio::test]
    async fn changing_the_series_moves_its_own_event_by_as_far_as_the_occurrence_moved() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("GET", "/calendars/cal-1/events/inst-1") => (
                StatusCode::OK,
                json!({
                    "id": "inst-1", "status": "confirmed", "recurringEventId": "master-1",
                    "start": {"dateTime": "2026-10-12T09:00:00-07:00", "timeZone": "America/Los_Angeles"},
                    "end": {"dateTime": "2026-10-12T09:30:00-07:00", "timeZone": "America/Los_Angeles"},
                }),
            ),
            ("GET", "/calendars/cal-1/events/master-1") => (
                StatusCode::OK,
                json!({
                    "id": "master-1", "status": "confirmed",
                    "start": {"dateTime": "2026-10-05T09:00:00-07:00", "timeZone": "America/Los_Angeles"},
                    "end": {"dateTime": "2026-10-05T09:30:00-07:00", "timeZone": "America/Los_Angeles"},
                    "recurrence": [
                        "RRULE:FREQ=WEEKLY;BYDAY=MO",
                        "EXDATE;TZID=America/Los_Angeles:20261019T090000",
                    ],
                    "attendees": [
                        {"email": "sam@example.com", "responseStatus": "accepted", "optional": true},
                        {"email": "room-4@resource.calendar.google.com", "resource": true,
                         "responseStatus": "accepted"},
                    ],
                }),
            ),
            ("PATCH", "/calendars/cal-1/events/master-1") => {
                (StatusCode::OK, json!({"id": "master-1"}))
            }
            _ => (StatusCode::NOT_FOUND, Value::Null),
        })
        .await;
        let s = signed_in(&base, "cal-1");
        let event = occurrence(&s.calendar, "inst-1");

        // The second Monday's standup, dragged from 9:00 to 10:30 and
        // stretched to an hour, "for every event".
        let mut moved = draft("2026-10-12T10:30:00-07:00", "2026-10-12T11:30:00-07:00");
        moved.attendees = vec![Attendee::new("Sam@Example.com")];
        moved.recurrence = Some(weekly(&[Weekday::Monday]));
        update_with_base(
            &s.svc,
            &s.vault,
            &s.account,
            &s.calendar,
            &event,
            &moved,
            EventScope::Series,
            &base,
        )
        .await
        .expect("updated");

        let seen = seen.lock().unwrap().clone();
        let patches: Vec<&Seen> = seen.iter().filter(|r| r.method == "PATCH").collect();
        assert_eq!(patches.len(), 1, "{seen:?}");
        let patch = patches[0];
        assert_eq!(
            patch.path, "/calendars/cal-1/events/master-1",
            "the series, not the occurrence"
        );
        assert_eq!(patch.query, "sendUpdates=all");
        assert_eq!(
            patch.body["start"],
            json!({"dateTime": "2026-10-05T10:30:00-07:00", "timeZone": "America/Los_Angeles"}),
            "the series' first occurrence moves the same hour and a half the second one did"
        );
        assert_eq!(
            patch.body["end"],
            json!({"dateTime": "2026-10-05T11:30:00-07:00", "timeZone": "America/Los_Angeles"}),
            "and is as long as the draft says"
        );
        assert_eq!(
            patch.body["recurrence"],
            json!([
                "RRULE:FREQ=WEEKLY;BYDAY=MO",
                "EXDATE;TZID=America/Los_Angeles:20261019T090000"
            ]),
            "only the RRULE line is the editor's; the EXDATE stays"
        );
        assert_eq!(
            patch.body["attendees"],
            json!([
                {"email": "sam@example.com", "responseStatus": "accepted", "optional": true},
                {"email": "room-4@resource.calendar.google.com", "resource": true,
                 "responseStatus": "accepted"},
            ]),
            "Sam keeps their answer and their optional flag, and the room stays booked"
        );
    }

    #[tokio::test]
    async fn deleting_a_series_deletes_its_own_event_and_one_already_gone_is_no_error() {
        let (base, seen) = mock(|method, path| match (method, path) {
            ("GET", "/calendars/cal-1/events/inst-1") => (
                StatusCode::OK,
                json!({"id": "inst-1", "status": "confirmed", "recurringEventId": "master-1"}),
            ),
            ("DELETE", "/calendars/cal-1/events/master-1") => (StatusCode::NO_CONTENT, Value::Null),
            ("DELETE", "/calendars/cal-1/events/gone-1") => (
                StatusCode::GONE,
                json!({"error": {"code": 410, "message": "Resource has been deleted"}}),
            ),
            _ => (StatusCode::NOT_FOUND, json!({"error": {"code": 404, "message": "Not Found"}})),
        })
        .await;
        let s = signed_in(&base, "cal-1");

        for (uid, scope) in [
            ("inst-1", EventScope::Series),
            ("gone-1", EventScope::Occurrence),
            ("never-was", EventScope::Occurrence),
        ] {
            let event = occurrence(&s.calendar, uid);
            delete_with_base(&s.svc, &s.vault, &s.account, &s.calendar, &event, scope, &base)
                .await
                .unwrap_or_else(|e| panic!("deleting {uid} failed: {e:?}"));
        }

        let seen = seen.lock().unwrap().clone();
        let deletes: Vec<&Seen> = seen.iter().filter(|r| r.method == "DELETE").collect();
        let paths: Vec<&str> = deletes.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/calendars/cal-1/events/master-1",
                "/calendars/cal-1/events/gone-1",
                "/calendars/cal-1/events/never-was",
            ],
            "the series is deleted through its own event; a 410 and a 404 are already gone"
        );
        assert!(deletes.iter().all(|r| r.query == "sendUpdates=all"), "guests hear of it");
    }

    #[tokio::test]
    async fn a_403_on_a_write_is_the_calendars_refusal_and_only_a_401_is_the_credential() {
        let (base, _seen) = mock(|_method, path| match path {
            "/calendars/shared-read-only/events" => (
                StatusCode::FORBIDDEN,
                json!({"error": {
                    "code": 403,
                    "message": "You need to have writer access to this calendar.",
                    "errors": [{"domain": "calendar", "reason": "requiredAccessLevel"}],
                }}),
            ),
            _ => (StatusCode::UNAUTHORIZED, Value::Null),
        })
        .await;
        let s = signed_in(&base, "shared-read-only");
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
        assert!(err.message.contains("writer access"), "Google's own reason: {}", err.message);
        assert!(!err.message.contains("access-1"), "never the token: {}", err.message);

        let revoked = Calendar::from_account(
            s.account.id,
            s.account.provider,
            AccountCalendarSource::Google,
            "revoked",
            "Old",
        );
        let err = create_with_base(&s.svc, &s.vault, &s.account, &revoked, &new, &base)
            .await
            .unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
    }
}
