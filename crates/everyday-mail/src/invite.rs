//! Reading a calendar invitation out of a message's `text/calendar` part,
//! and writing the iTIP `REPLY` an "accept", "tentative" or "decline" click
//! sends back.
//!
//! # Why `calcard`, again
//!
//! `everyday_service::accountcal::caldav` already depends on `calcard` to
//! parse `VEVENT`/`VTIMEZONE` off a CalDAV server, for exactly the reasons
//! that module's own docs give: its `datecalc` module already expands
//! `RRULE`, resolves `VTIMEZONE`, and is the one recurrence engine this
//! workspace trusts, so a second one (the `rrule` crate) never joins it.
//! [`parse_invite`] wants the same resolution — a Windows `TZID` such as
//! `"Pacific Standard Time"`, which only means something once the message's
//! own embedded `VTIMEZONE` is read, is exactly what an Outlook or Exchange
//! invitation sends — so this module reaches for the same crate rather than
//! inventing a second, smaller iCalendar reader. [`to_jiff`] is this
//! module's own copy of the one boundary `caldav::to_jiff` already is: the
//! only place in this crate a `chrono` type becomes a `jiff` one.
//!
//! # What an invitation *is*, here
//!
//! One `VEVENT` — the first that is not a `RECURRENCE-ID` override, on the
//! reasoning that an invitation banner shows the *series*, not one already
//! rescheduled occurrence of it — read into an [`everyday_core::mail::Invite`]
//! that never leaves this module's understanding of iCalendar behind: no
//! `chrono` type, no `calcard` type, crosses back out of [`parse_invite`].
//!
//! # The all-day convention
//!
//! See [`everyday_core::mail::Invite`]'s own docs for the exact rule
//! (midnight UTC) and why. It falls out of this module's machinery for
//! free: an all-day `DTSTART`/`DTEND` has no `TZID` at all, `calcard` reads
//! that as [`calcard::common::timezone::Tz::Floating`], and `Floating`'s own
//! [`chrono::TimeZone`] implementation is a fixed zero offset — so asking
//! for a floating date's instant already *is* asking for its midnight UTC,
//! with nothing else in this module having to special-case an all-day event
//! to get there.

use calcard::common::PartialDateTime;
use calcard::common::timezone::Tz;
use calcard::icalendar::dates::TimeOrDelta;
use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarEntry, ICalendarMethod,
    ICalendarParameter, ICalendarParameterName, ICalendarParameterValue,
    ICalendarParticipationStatus, ICalendarProperty, ICalendarValue, Uri,
};
use chrono::Timelike as _;
use jiff::Timestamp;

use everyday_core::mail::{Address, AttendeeResponse, Invite, InviteAttendee, InviteMethod};

/// Occurrences are only ever needed for the first one — an invitation names
/// a series ("every Tuesday", via [`Invite::recurrence`]'s raw `RRULE`
/// text), not a list of future dates — so this is small and bounded, unlike
/// `everyday_service::accountcal::caldav::EXPAND_LIMIT`, the much larger
/// bound a calendar sync needs for the same call when it actually has to
/// materialise every occurrence a feed's window covers.
const FIRST_OCCURRENCE_LIMIT: usize = 8;

/// Reads `calendar_bytes` — the raw bytes of a message's `text/calendar`
/// part — into an [`Invite`], or `None` when it is not a shape this
/// application draws a banner for: unparseable bytes, no `METHOD` (a bare
/// `.ics` shared as an attachment, not an iTIP message), a `METHOD` this
/// application has no button for (`PUBLISH`, `ADD`, `REFRESH`,
/// `DECLINECOUNTER`), no `VEVENT` at all, or a `VEVENT` missing the
/// properties an invitation cannot be drawn without (`UID`, `DTSTART`,
/// `ORGANIZER`).
///
/// `my_addresses` is every address this account answers to — its own and
/// every [`everyday_core::account::Identity`]'s — lower-cased comparison
/// against each [`InviteAttendee::address`], so [`Invite::my_response`] is
/// whichever attendee row is "us" without the caller having to know that
/// rule.
pub fn parse_invite(calendar_bytes: &[u8], my_addresses: &[String]) -> Option<Invite> {
    let text = std::str::from_utf8(calendar_bytes).ok()?;
    let parsed = ICalendar::parse(text).ok()?;
    let method = top_level_method(&parsed)?;

    let (comp_id, comp) = master_vevent(&parsed)?;
    let uid = text_property(comp, ICalendarProperty::Uid)?;
    let organizer = organizer_of(comp)?;
    let (start, end) = start_end(&parsed, comp_id as u32)?;

    let summary = text_property(comp, ICalendarProperty::Summary).unwrap_or_default();
    let location = text_property(comp, ICalendarProperty::Location).filter(|s| !s.is_empty());
    let attendees = attendees_of(comp);
    let all_day = is_all_day(comp);
    let recurrence = rrule_text(comp);

    let my_response = attendees
        .iter()
        .find(|a| my_addresses.iter().any(|mine| mine.eq_ignore_ascii_case(&a.address.email)))
        .map(|a| a.response);

    Some(Invite {
        uid,
        method,
        summary,
        start,
        end,
        all_day,
        location,
        organizer,
        attendees,
        my_response,
        recurrence,
    })
}

/// Builds an iTIP `REPLY` (RFC 5546 §3.2.3) answering the invitation in
/// `invite_calendar_bytes`: the same `UID`, `SEQUENCE`, `ORGANIZER`,
/// `DTSTART`/`DTEND` (or `DURATION`), `LOCATION` and `VTIMEZONE` the request
/// carried, a `DTSTAMP` of now, and exactly one `ATTENDEE` — the responder,
/// with the new `PARTSTAT`. `None` when `invite_calendar_bytes` does not
/// parse, or carries no `VEVENT`, `UID` or `ORGANIZER` to reply to.
///
/// RFC 5546 does not strictly require a `REPLY` to repeat the event's own
/// time -- only `UID`, `SEQUENCE`, `DTSTAMP`, `ORGANIZER` and the replying
/// `ATTENDEE` are -- but every real sender this application has been tested
/// against (Google, Outlook, Apple Mail) includes them anyway, precisely so
/// the *receiving* client can draw its own banner ("Hari has accepted")
/// without first correlating the reply against the request it answers by
/// `UID` alone. This mirrors that, and is also what lets [`parse_invite`]
/// read a `REPLY` this function built back into an [`Invite`] whole -- see
/// the round-trip test below. `DTSTART`/`DTEND` are copied as whichever
/// entries the original `VEVENT` actually had (a `TZID`-qualified pair, a
/// `DURATION`, or a bare `Z` stamp), and [`ICalendar::copy_timezones`]
/// brings across the `VTIMEZONE` a `TZID` needs to resolve, rather than this
/// function re-deriving any of that from the already-resolved instant
/// [`parse_invite`] itself would compute -- copying the original property is
/// simpler and cannot disagree with what the request said.
pub fn build_reply(
    invite_calendar_bytes: &[u8],
    responder: &Address,
    response: AttendeeResponse,
    comment: Option<&str>,
) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(invite_calendar_bytes).ok()?;
    let parsed = ICalendar::parse(text).ok()?;
    let (_, comp) = master_vevent(&parsed)?;

    let uid = text_property(comp, ICalendarProperty::Uid)?;
    let organizer = organizer_of(comp)?;
    let sequence = int_property(comp, ICalendarProperty::Sequence).unwrap_or(0);
    let summary = text_property(comp, ICalendarProperty::Summary);
    let location = text_property(comp, ICalendarProperty::Location);

    let mut event = ICalendarComponent::new(ICalendarComponentType::VEvent);
    event.add_uid(&uid);
    event.add_sequence(sequence);
    event.add_dtstamp(utc_partial(Timestamp::now()));
    copy_entry(comp, &mut event, ICalendarProperty::Dtstart);
    copy_entry(comp, &mut event, ICalendarProperty::Dtend);
    copy_entry(comp, &mut event, ICalendarProperty::Duration);
    if let Some(summary) = summary {
        event.add_property(ICalendarProperty::Summary, summary);
    }
    if let Some(location) = location {
        event.add_property(ICalendarProperty::Location, location);
    }
    event.add_property_with_params(
        ICalendarProperty::Organizer,
        organizer_params(&organizer),
        Uri::Location(format!("mailto:{}", organizer.email)),
    );
    event.add_property_with_params(
        ICalendarProperty::Attendee,
        attendee_reply_params(responder, response),
        Uri::Location(format!("mailto:{}", responder.email)),
    );
    if let Some(comment) = comment.map(str::trim).filter(|c| !c.is_empty()) {
        event.add_property(ICalendarProperty::Comment, comment.to_string());
    }

    let mut vcalendar = ICalendarComponent::new(ICalendarComponentType::VCalendar);
    vcalendar.add_property(ICalendarProperty::Version, "2.0");
    vcalendar.add_property(ICalendarProperty::Prodid, "-//Every Day//Invite Reply//EN");
    vcalendar.add_property(ICalendarProperty::Method, ICalendarMethod::Reply);
    vcalendar.component_ids.push(1);

    let mut out = ICalendar { components: vec![vcalendar, event] };
    // Whatever `VTIMEZONE`s the request carried, so a `TZID` on the
    // `DTSTART`/`DTEND` just copied above still resolves when this reply is
    // read back -- see this function's own docs.
    out.copy_timezones(&parsed);

    let mut text = String::new();
    out.write_to(&mut text).ok()?;
    Some(text.into_bytes())
}

/// Clones `prop` from `from` onto `into`, when `from` has it -- what carries
/// `DTSTART`/`DTEND`/`DURATION` forward into a reply verbatim, params and
/// all, rather than this module re-deriving them from an already-resolved
/// instant. A no-op when `from` does not have `prop` at all (a `DURATION`
/// event has no `DTEND`, and vice versa).
fn copy_entry(from: &ICalendarComponent, into: &mut ICalendarComponent, prop: ICalendarProperty) {
    if let Some(entry) = from.entries.iter().find(|e| e.name == prop) {
        into.entries.push(entry.clone());
    }
}

// ---- reading -------------------------------------------------------------

/// `METHOD`, read off the top-level `VCALENDAR` component, mapped onto the
/// four verbs this application draws a banner for. See the module docs on
/// why the rest of RFC 5546's methods answer `None` instead of growing
/// [`InviteMethod`].
fn top_level_method(parsed: &ICalendar) -> Option<InviteMethod> {
    let vcalendar =
        parsed.components.iter().find(|c| c.component_type == ICalendarComponentType::VCalendar)?;
    let entry = vcalendar.entries.iter().find(|e| e.name == ICalendarProperty::Method)?;
    let ICalendarValue::Method(method) = entry.values.first()? else { return None };
    match method {
        ICalendarMethod::Request => Some(InviteMethod::Request),
        ICalendarMethod::Cancel => Some(InviteMethod::Cancel),
        ICalendarMethod::Reply => Some(InviteMethod::Reply),
        ICalendarMethod::Counter => Some(InviteMethod::Counter),
        // `PUBLISH`, `ADD`, `REFRESH`, `DECLINECOUNTER` -- not a shape this
        // application offers a reply banner for; see the module docs.
        _ => None,
    }
}

/// The `VEVENT` an invitation banner is drawn from: the first one that is
/// not a `RECURRENCE-ID` override (a rescheduled single occurrence of a
/// recurring series), falling back to the first `VEVENT` at all when every
/// one of them is an override -- a shape no real sender produces, but one
/// this function still has to answer *something* for rather than panic.
fn master_vevent(parsed: &ICalendar) -> Option<(usize, &ICalendarComponent)> {
    parsed
        .components
        .iter()
        .enumerate()
        .find(|(_, c)| c.component_type == ICalendarComponentType::VEvent && !has_recurrence_id(c))
        .or_else(|| {
            parsed
                .components
                .iter()
                .enumerate()
                .find(|(_, c)| c.component_type == ICalendarComponentType::VEvent)
        })
}

fn has_recurrence_id(comp: &ICalendarComponent) -> bool {
    comp.entries.iter().any(|e| e.name == ICalendarProperty::RecurrenceId)
}

/// `(start, end)` for the master `VEVENT`'s own first occurrence, both
/// zoned by `calcard`'s own `VTIMEZONE`/`TZID` resolution -- see the module
/// docs for why this reuses [`ICalendar::expand_dates`] rather than reading
/// `DTSTART`/`DTEND` by hand. A `DTEND` given as a `DURATION` is added onto
/// the resolved start rather than read as its own instant, the same as
/// `accountcal::caldav::events_from_ics` does.
fn start_end(parsed: &ICalendar, comp_id: u32) -> Option<(Timestamp, Timestamp)> {
    let expanded = parsed.expand_dates(Tz::Floating, FIRST_OCCURRENCE_LIMIT);
    let occurrence =
        expanded.events.into_iter().filter(|e| e.comp_id == comp_id).min_by_key(|e| e.start)?;
    let start = to_jiff(occurrence.start);
    let end = match occurrence.end {
        TimeOrDelta::Time(t) => to_jiff(t),
        TimeOrDelta::Delta(d) => start + jiff::SignedDuration::new(d.num_seconds(), 0),
    };
    Some((start, end.max(start)))
}

/// The one place a `chrono::DateTime` becomes a `jiff::Timestamp` in this
/// module -- the twin of `accountcal::caldav::to_jiff`, kept as its own copy
/// rather than shared because the two crates' `chrono` boundaries are
/// deliberately separate facts (see this module's own docs).
fn to_jiff(dt: chrono::DateTime<Tz>) -> Timestamp {
    Timestamp::new(dt.timestamp(), dt.nanosecond() as i32).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn is_all_day(comp: &ICalendarComponent) -> bool {
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Dtstart)
        .and_then(|e| e.values.first())
        .is_some_and(|v| matches!(v, ICalendarValue::PartialDateTime(p) if p.hour.is_none()))
}

/// `RRULE`'s value, rendered back to iCalendar text (`FREQ=WEEKLY;COUNT=6`)
/// with [`calcard::icalendar::ICalendarRecurrenceRule`]'s own [`Display`],
/// rather than expanded -- see [`Invite::recurrence`]'s own docs.
///
/// [`Display`]: std::fmt::Display
fn rrule_text(comp: &ICalendarComponent) -> Option<String> {
    let entry = comp.entries.iter().find(|e| e.name == ICalendarProperty::Rrule)?;
    match entry.values.first()? {
        ICalendarValue::RecurrenceRule(rule) => Some(rule.to_string()),
        _ => None,
    }
}

fn organizer_of(comp: &ICalendarComponent) -> Option<Address> {
    let entry = comp.entries.iter().find(|e| e.name == ICalendarProperty::Organizer)?;
    address_of(entry)
}

fn attendees_of(comp: &ICalendarComponent) -> Vec<InviteAttendee> {
    comp.entries
        .iter()
        .filter(|e| e.name == ICalendarProperty::Attendee)
        .filter_map(|e| {
            let address = address_of(e)?;
            let response = e
                .params
                .iter()
                .find(|p| p.name == ICalendarParameterName::Partstat)
                .and_then(|p| match &p.value {
                    ICalendarParameterValue::Partstat(s) => Some(s.clone()),
                    _ => None,
                })
                .map(partstat_to_response)
                .unwrap_or(AttendeeResponse::NeedsAction);
            Some(InviteAttendee { address, response })
        })
        .collect()
}

/// An `ORGANIZER` or `ATTENDEE` entry's `mailto:` value and `CN` parameter,
/// read together as one [`Address`] -- both properties share this exact
/// shape in iCalendar (RFC 5545 §3.8.4.1, §3.8.4.3).
fn address_of(entry: &ICalendarEntry) -> Option<Address> {
    let email = entry.values.first().and_then(value_text).map(|s| strip_mailto(&s))?;
    if email.is_empty() {
        return None;
    }
    let name = entry
        .params
        .iter()
        .find(|p| p.name == ICalendarParameterName::Cn)
        .and_then(|p| match &p.value {
            ICalendarParameterValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Some(Address { name, email })
}

fn strip_mailto(s: &str) -> String {
    s.strip_prefix("mailto:").or_else(|| s.strip_prefix("MAILTO:")).unwrap_or(s).trim().to_string()
}

fn value_text(v: &ICalendarValue) -> Option<String> {
    match v {
        ICalendarValue::Text(s) => Some(s.clone()),
        ICalendarValue::Uri(Uri::Location(s)) => Some(s.clone()),
        _ => None,
    }
}

fn text_property(comp: &ICalendarComponent, prop: ICalendarProperty) -> Option<String> {
    comp.entries.iter().find(|e| e.name == prop).and_then(|e| e.values.first()).and_then(value_text)
}

fn int_property(comp: &ICalendarComponent, prop: ICalendarProperty) -> Option<i64> {
    comp.entries.iter().find(|e| e.name == prop).and_then(|e| e.values.first()).and_then(
        |v| match v {
            ICalendarValue::Integer(i) => Some(*i),
            _ => None,
        },
    )
}

/// iCalendar's `PARTSTAT` folded onto the four values this application
/// tracks -- see [`AttendeeResponse`]'s own docs for why `DELEGATED`,
/// `COMPLETED`, `IN-PROCESS` and `FAILED` all land on
/// [`AttendeeResponse::NeedsAction`] rather than growing that enum.
fn partstat_to_response(p: ICalendarParticipationStatus) -> AttendeeResponse {
    match p {
        ICalendarParticipationStatus::Accepted => AttendeeResponse::Accepted,
        ICalendarParticipationStatus::Declined => AttendeeResponse::Declined,
        ICalendarParticipationStatus::Tentative => AttendeeResponse::Tentative,
        ICalendarParticipationStatus::NeedsAction
        | ICalendarParticipationStatus::Delegated
        | ICalendarParticipationStatus::Completed
        | ICalendarParticipationStatus::InProcess
        | ICalendarParticipationStatus::Failed => AttendeeResponse::NeedsAction,
    }
}

// ---- writing ---------------------------------------------------------------

fn response_to_partstat(r: AttendeeResponse) -> ICalendarParticipationStatus {
    match r {
        AttendeeResponse::Accepted => ICalendarParticipationStatus::Accepted,
        AttendeeResponse::Tentative => ICalendarParticipationStatus::Tentative,
        AttendeeResponse::Declined => ICalendarParticipationStatus::Declined,
        AttendeeResponse::NeedsAction => ICalendarParticipationStatus::NeedsAction,
    }
}

fn organizer_params(organizer: &Address) -> Vec<ICalendarParameter> {
    if organizer.name.is_empty() {
        Vec::new()
    } else {
        vec![ICalendarParameter::cn(organizer.name.clone())]
    }
}

fn attendee_reply_params(
    responder: &Address,
    response: AttendeeResponse,
) -> Vec<ICalendarParameter> {
    let mut params = vec![ICalendarParameter::partstat(response_to_partstat(response))];
    if !responder.name.is_empty() {
        params.push(ICalendarParameter::cn(responder.name.clone()));
    }
    params
}

/// `ts`, as a UTC [`PartialDateTime`] with the `Z` designator -- what
/// [`ICalendarComponent::add_dtstamp`] writes as `DTSTAMP`. `tz_hour` and
/// `tz_minute` both `Some(0)` is the shape `PartialDateTime::format_as_ical`
/// reads as "write a trailing `Z`" -- see that function's own match arm.
fn utc_partial(ts: Timestamp) -> PartialDateTime {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::UTC);
    PartialDateTime {
        year: Some(zoned.year() as u16),
        month: Some(zoned.month() as u8),
        day: Some(zoned.day() as u8),
        hour: Some(zoned.hour() as u8),
        minute: Some(zoned.minute() as u8),
        second: Some(zoned.second() as u8),
        tz_hour: Some(0),
        tz_minute: Some(0),
        tz_minus: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE_REQUEST: &str = include_str!("../tests/fixtures/invites/google_request.ics");
    const OUTLOOK_REQUEST: &str = include_str!("../tests/fixtures/invites/outlook_request.ics");
    const APPLE_REQUEST: &str = include_str!("../tests/fixtures/invites/apple_request.ics");
    const CANCEL: &str = include_str!("../tests/fixtures/invites/cancel.ics");
    const ALLDAY: &str = include_str!("../tests/fixtures/invites/allday.ics");
    const RECURRING: &str = include_str!("../tests/fixtures/invites/recurring.ics");

    fn hari() -> Vec<String> {
        vec!["hari@example.com".to_string()]
    }

    #[test]
    fn a_google_request_is_read_with_its_own_response_and_organizer() {
        let invite = parse_invite(GOOGLE_REQUEST.as_bytes(), &hari()).expect("parses");
        assert_eq!(invite.method, InviteMethod::Request);
        assert_eq!(invite.uid, "google-standup-001@google.com");
        assert_eq!(invite.summary, "Standup");
        assert_eq!(invite.organizer.email, "priya@example.com");
        assert_eq!(invite.organizer.name, "Priya Patel");
        assert!(!invite.all_day);
        assert_eq!(invite.location.as_deref(), Some("Meet: https://meet.example.com/standup"));
        assert_eq!(invite.attendees.len(), 2);
        assert_eq!(invite.my_response, Some(AttendeeResponse::NeedsAction));
        // 10:00 America/Los_Angeles on 2026-06-16 is 17:00 UTC (PDT, UTC-7).
        assert_eq!(invite.start.to_string(), "2026-06-16T17:00:00Z");
        assert_eq!(invite.end.to_string(), "2026-06-16T17:30:00Z");
    }

    #[test]
    fn an_outlook_request_resolves_a_windows_tzid_from_its_own_embedded_vtimezone() {
        let invite = parse_invite(OUTLOOK_REQUEST.as_bytes(), &hari()).expect("parses");
        assert_eq!(invite.method, InviteMethod::Request);
        assert_eq!(invite.summary, "Budget review");
        assert_eq!(invite.organizer.email, "priya@example.com");
        // 14:00 "Pacific Standard Time" on 2026-07-01 is daylight (UTC-7): 21:00 UTC.
        assert_eq!(invite.start.to_string(), "2026-07-01T21:00:00Z");
        assert_eq!(invite.end.to_string(), "2026-07-01T22:00:00Z");
    }

    #[test]
    fn an_apple_request_is_read_the_same_way() {
        let invite = parse_invite(APPLE_REQUEST.as_bytes(), &hari()).expect("parses");
        assert_eq!(invite.method, InviteMethod::Request);
        assert_eq!(invite.summary, "Dinner with the team");
        assert_eq!(invite.location.as_deref(), Some("The Anchor, 12 Riverside"));
        assert_eq!(invite.my_response, Some(AttendeeResponse::NeedsAction));
    }

    #[test]
    fn a_cancel_is_read_as_the_cancel_method() {
        let invite = parse_invite(CANCEL.as_bytes(), &hari()).expect("parses");
        assert_eq!(invite.method, InviteMethod::Cancel);
        assert_eq!(invite.uid, "google-standup-001@google.com");
    }

    #[test]
    fn an_all_day_event_is_flagged_and_lands_on_utc_midnight() {
        let invite = parse_invite(ALLDAY.as_bytes(), &hari()).expect("parses");
        assert!(invite.all_day);
        assert_eq!(invite.start.to_string(), "2026-08-12T00:00:00Z");
        // DTEND is the exclusive end date, per iCalendar -- the day after.
        assert_eq!(invite.end.to_string(), "2026-08-13T00:00:00Z");
    }

    #[test]
    fn a_recurring_event_keeps_its_rrule_as_text_and_starts_at_its_first_occurrence() {
        let invite = parse_invite(RECURRING.as_bytes(), &hari()).expect("parses");
        assert_eq!(invite.recurrence.as_deref(), Some("FREQ=WEEKLY;COUNT=6"));
        assert_eq!(invite.summary, "Weekly standup");
        // 09:00 America/New_York on 2026-03-02 (before the DST change) is 14:00 UTC.
        assert_eq!(invite.start.to_string(), "2026-03-02T14:00:00Z");
    }

    #[test]
    fn a_message_with_no_method_is_not_an_invitation() {
        let ics = "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:x@example.com\nDTSTART:20260101T100000Z\nSUMMARY:No method\nEND:VEVENT\nEND:VCALENDAR\n";
        assert!(parse_invite(ics.as_bytes(), &hari()).is_none());
    }

    #[test]
    fn nonsense_bytes_are_not_an_invitation() {
        assert!(parse_invite(b"not a calendar at all", &hari()).is_none());
    }

    #[test]
    fn my_response_is_none_when_no_identity_was_invited() {
        let invite = parse_invite(GOOGLE_REQUEST.as_bytes(), &["nobody@example.com".to_string()])
            .expect("parses");
        assert_eq!(invite.my_response, None);
    }

    // ---- building a reply -----------------------------------------------

    #[test]
    fn a_reply_round_trips_the_uid_and_the_new_partstat() {
        let responder = Address::new("Hari Govardhanam", "hari@example.com");
        let ics =
            build_reply(GOOGLE_REQUEST.as_bytes(), &responder, AttendeeResponse::Accepted, None)
                .expect("builds");
        let text = String::from_utf8(ics.clone()).unwrap();
        assert!(text.contains("METHOD:REPLY"), "{text}");
        assert!(text.contains("UID:google-standup-001@google.com"), "{text}");
        assert!(text.contains("PARTSTAT=ACCEPTED"), "{text}");
        assert!(text.contains("mailto:hari@example.com"), "{text}");
        // Only the responder's own ATTENDEE -- never the organiser's.
        assert_eq!(text.matches("ATTENDEE").count(), 1, "{text}");

        // And it reads back as an invitation of its own, in the shape a
        // REPLY actually is.
        let read_back =
            parse_invite(&ics, &["hari@example.com".to_string()]).expect("the reply parses");
        assert_eq!(read_back.method, InviteMethod::Reply);
        assert_eq!(read_back.uid, "google-standup-001@google.com");
        assert_eq!(read_back.attendees.len(), 1);
        assert_eq!(read_back.attendees[0].response, AttendeeResponse::Accepted);
    }

    #[test]
    fn a_reply_carries_the_sequence_and_an_optional_comment() {
        let responder = Address::bare("hari@example.com");
        let ics = build_reply(
            OUTLOOK_REQUEST.as_bytes(),
            &responder,
            AttendeeResponse::Declined,
            Some("Clashes with another meeting"),
        )
        .expect("builds");
        let text = String::from_utf8(ics).unwrap();
        assert!(text.contains("SEQUENCE:0"), "{text}");
        assert!(text.contains("PARTSTAT=DECLINED"), "{text}");
        assert!(text.contains("COMMENT:Clashes with another meeting"), "{text}");
    }

    #[test]
    fn a_blank_comment_is_not_written_at_all() {
        let responder = Address::bare("hari@example.com");
        let ics = build_reply(
            GOOGLE_REQUEST.as_bytes(),
            &responder,
            AttendeeResponse::Tentative,
            Some("   "),
        )
        .expect("builds");
        let text = String::from_utf8(ics).unwrap();
        assert!(!text.contains("COMMENT"), "{text}");
    }

    #[test]
    fn building_a_reply_to_nonsense_bytes_is_none_not_a_panic() {
        let responder = Address::bare("hari@example.com");
        assert!(
            build_reply(b"not a calendar", &responder, AttendeeResponse::Accepted, None).is_none()
        );
    }
}
