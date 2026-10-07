//! CalDAV: creating, changing and deleting events on a CalDAV server --
//! iCloud, Fastmail, Yahoo, or one somebody runs themselves.
//!
//! The reading half is the parent module; this is the half that writes,
//! sharing the parent's request helpers. See `accountcal`'s module doc,
//! "Writing", for the shape every source follows: write to the server, and
//! let the calendar's own sync bring the result back.
//!
//! # One file per event, and every write is the whole file
//!
//! Google and Graph each have a call for "move this one occurrence" and
//! another for "rename the series". CalDAV has `PUT` and `DELETE`, of whole
//! resources, and nothing else. One resource is one event -- RFC 4791 §4.1
//! allows a single `UID` per resource -- and everything about a repeating
//! event lives in it together: the master `VEVENT` with its `DTSTART` and
//! `RRULE`, an `EXDATE` on the master for each occurrence somebody deleted,
//! and an override `VEVENT` sharing its `UID`, named by a `RECURRENCE-ID`,
//! for each occurrence somebody moved or changed. So every change here is the
//! same three steps: fetch the resource with its etag, edit the text, and put
//! it back with `If-Match` on that etag. Somebody else's change in between --
//! a phone, a colleague with edit rights -- comes back as a 412, and becomes
//! [`codes::CONFLICT`] rather than being quietly undone.
//!
//! # Edited, never rebuilt
//!
//! A change rewrites the properties an [`EventDraft`] speaks for and leaves
//! every other line of the file exactly as the server sent it: alarms, the
//! server's own `SCHEDULE-STATUS` on each guest, Apple's and Google's `X-`
//! properties, overrides another client made. Rebuilding the file from the
//! draft would be shorter, and would strip all of that out of somebody's
//! calendar the first time they fixed a typo in a title.
//!
//! # Pure in the middle
//!
//! The iCalendar work is a handful of pure functions -- text in, text out:
//! [`new_resource_ics`], [`editable_from_ics`], [`apply_edit`] and
//! [`removal_for`] -- and the four entry points around them are thin:
//! connect, fetch, call one, `PUT` or `DELETE`. That is what lets the tests
//! below try every shape of edit and read each result back through the very
//! `calcard` expansion [`super::sync`] runs, with no server at all;
//! `tests/caldav_docker.rs` makes the same round trip against a real one.
//! Building and serialising go through `calcard` too, which already escapes
//! `TEXT` values and folds lines at 75 octets the way RFC 5545 §3.1 and
//! §3.3.11 ask -- `everyday_core::ics::Ics` does both as well, but it only
//! ever writes a file from nothing, in UTC, and can neither carry a
//! parameter-bearing property like `ATTENDEE` nor edit what a server sent.
//!
//! # Guests are invited by the server, or by nobody
//!
//! An event with guests is written with an `ORGANIZER` -- this account --
//! and an `ATTENDEE` per guest, `PARTSTAT=NEEDS-ACTION;RSVP=TRUE`. A server
//! that schedules implicitly (RFC 6638 §3; iCloud and Fastmail both do) reads
//! those lines on the `PUT` and sends the invitations itself, and the
//! updates and cancellations after them; nothing here sends any mail. A
//! server that does not schedule -- Radicale, most self-hosted ones -- keeps
//! the guest list and tells nobody, which is exactly what the same file
//! written by the person's phone would have done there too.
//!
//! # `SEQUENCE`, and what it pairs
//!
//! RFC 5546 §2.1.4 has an organiser bump `SEQUENCE` on every change that
//! matters, so a guest's calendar can tell a newer version from an older
//! one. There is a second reason here: `calcard`'s expansion only pairs an
//! override with the occurrence it replaces when the two `VEVENT`s carry the
//! *same* `SEQUENCE` (`ICalendar::expand_dates` keys its overrides on
//! `(SEQUENCE, RECURRENCE-ID)`) -- an override one revision ahead of its
//! master is drawn as an extra event beside the very occurrence it was meant
//! to replace. So when this account organises the event, every `VEVENT` in
//! the resource moves to one new number together (see [`settle`]); when
//! somebody else does, none is touched -- an attendee bumping `SEQUENCE` is
//! claiming to be the organiser -- and a new override starts on its master's.

use std::sync::Arc;

use calcard::common::PartialDateTime;
use calcard::common::timezone::Tz;
use calcard::icalendar::timezone::TzResolver;
use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarEntry, ICalendarParameter,
    ICalendarParameterName, ICalendarParameterValue, ICalendarParticipationRole,
    ICalendarParticipationStatus, ICalendarProperty, ICalendarValue, ICalendarValueType, Uri,
};
use jiff::civil::Date;
use jiff::{SignedDuration, Span, Timestamp};
use libdav::caldav::GetCalendarResources;
use libdav::dav::{Delete, PutResource, WebDavClient, WebDavError};

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    Attendee, Calendar, CalendarOrigin, EditableEvent, Event, EventDraft, EventScope,
};
use everyday_core::mail::AttendeeResponse;
use everyday_core::recurrence::Recurrence;

use super::{Client, base_uri, client_for, text_property, to_jiff};
use crate::accountcal::tokens;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::Service;

/// What every write `PUT`s: RFC 4791 §4.1's media type, with the charset
/// spelled out -- iCalendar is UTF-8 by definition, but a server left to
/// guess otherwise mangles every accented name on a guest list.
const CONTENT_TYPE: &str = "text/calendar; charset=utf-8";

/// `PRODID` on a calendar this module writes from nothing. An edited
/// resource keeps whatever `PRODID` it came with: the program that made the
/// file is still the program that made it.
const PRODID: &str = "-//Every Day//EN";

// ---- the four writes -------------------------------------------------------

/// Put `draft` on `calendar` as a brand-new resource.
///
/// The href and the `UID` are minted here, from one fresh UUID: the server
/// never chooses either for a `PUT`, and `If-None-Match: *` (what
/// [`PutResource::create`] sends) means a collision -- vanishingly unlikely
/// with a v4 UUID -- is refused rather than overwriting somebody's event.
pub(super) async fn create(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    draft: &EventDraft,
) -> CommandResult<()> {
    let collection = collection_of(calendar)?;
    let id = uuid::Uuid::new_v4();
    let href = format!("{}/{id}.ics", collection.trim_end_matches('/'));
    let ics = new_resource_ics(draft, &format!("{id}@everyday"), &Me::of(account), svc.now());
    let webdav = connect(svc, vault, account).await?;
    webdav.request(PutResource::new(&href).create(ics, CONTENT_TYPE)).await.map_err(failure)?;
    Ok(())
}

/// Read `event` back from its resource as the server holds it right now.
pub(super) async fn load(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
) -> CommandResult<EditableEvent> {
    let collection = collection_of(calendar)?;
    let webdav = connect(svc, vault, account).await?;
    let current = fetch(&webdav, collection, href_of(event)).await?;
    editable_from_ics(&current.data, event, &Me::of(account))
}

/// Change `event` -- one occurrence, or its series -- to match `draft`, on a
/// copy of the resource fetched fresh for the purpose and put back only if
/// nobody else changed it in between.
pub(super) async fn update(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
) -> CommandResult<()> {
    let collection = collection_of(calendar)?;
    let href = href_of(event);
    let webdav = connect(svc, vault, account).await?;
    let current = fetch(&webdav, collection, href).await?;
    let ics = apply_edit(&current.data, event, draft, scope, &Me::of(account), svc.now())?;
    webdav
        .request(PutResource::new(href).update(ics, CONTENT_TYPE, current.etag))
        .await
        .map_err(failure)?;
    Ok(())
}

/// Delete `event`. A series, or an event that never repeated, takes its
/// whole resource with it; one occurrence of a series becomes an `EXDATE`
/// on the master and the resource stays. An event that is already gone
/// from the server -- deleted on a phone since the last sync -- is a
/// success: the person asked for it not to be there, and it is not.
pub(super) async fn delete(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    event: &Event,
    scope: EventScope,
) -> CommandResult<()> {
    let collection = collection_of(calendar)?;
    let href = href_of(event);
    let webdav = connect(svc, vault, account).await?;
    // Fetched fresh rather than taken from `calendar.account_sync.etags`,
    // for the same reason an edit is: the etag on record is only as new as
    // the last sync, and deleting against a stale one is a 412 on a
    // resource that merely changed, where a fresh one deletes what the
    // person is actually looking at.
    let current = match fetch(&webdav, collection, href).await {
        Ok(current) => current,
        Err(e) if e.code == codes::NOT_FOUND => return Ok(()),
        Err(e) => return Err(e),
    };
    match removal_for(&current.data, event, scope, &Me::of(account), svc.now())? {
        Removal::Resource => {
            match webdav.request(Delete::new(href).with_etag(current.etag)).await {
                Ok(_) => Ok(()),
                Err(WebDavError::BadStatusCode(status)) if is_gone(status) => Ok(()),
                Err(e) => Err(failure(e)),
            }
        }
        Removal::Rewrite(ics) => {
            webdav
                .request(PutResource::new(href).update(ics, CONTENT_TYPE, current.etag))
                .await
                .map_err(failure)?;
            Ok(())
        }
    }
}

// ---- the network, briefly --------------------------------------------------

/// A client for `account`'s CalDAV server -- the same credential, transport
/// and base address [`super::sync`] uses.
async fn connect(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<WebDavClient<Client>> {
    let credential = tokens::credential(svc, vault, account, tokens::Resource::Native).await?;
    Ok(WebDavClient::new(base_uri(account)?, client_for(credential)?))
}

/// The collection a calendar mirrors: [`CalendarOrigin::Account`]'s
/// `remote_id`, which for CalDAV is the collection's href.
fn collection_of(calendar: &Calendar) -> CommandResult<&str> {
    match &calendar.origin {
        CalendarOrigin::Account { remote_id, .. } => Ok(remote_id.as_str()),
        _ => Err(CommandError::new(codes::INVALID, "not an account calendar")),
    }
}

/// The resource an occurrence came from. [`super::events_from_ics`] stores
/// every occurrence's uid as `{href}#{start}`, and [`super::sync`] already
/// recovers the href the same way when it decides what to delete.
fn href_of(event: &Event) -> &str {
    event.uid.split('#').next().unwrap_or_default()
}

/// A resource as the server holds it now, and the etag that names that
/// version -- what a following `If-Match` is checked against.
struct Current {
    data: String,
    etag: String,
}

/// `href`, fetched with a single-href `calendar-multiget` -- the request
/// [`super::sync`] already makes, so a server that answers the sync answers
/// this, and the etag comes back in the same response as the data rather
/// than from a second request that could race a change between the two.
async fn fetch(
    webdav: &WebDavClient<Client>,
    collection: &str,
    href: &str,
) -> CommandResult<Current> {
    let response = webdav
        .request(GetCalendarResources::new(collection).with_hrefs([href]))
        .await
        .map_err(failure)?;
    let mut resources = response.resources;
    // The server's own spelling of the href, when it percent-encodes it
    // differently than this vault stored it, should not turn a resource it
    // did send back into "gone": one answer to a one-href request is the
    // answer.
    let found = resources
        .iter()
        .position(|resource| resource.href == href)
        .or_else(|| (resources.len() == 1).then_some(0));
    let Some(resource) = found.map(|at| resources.swap_remove(at)) else {
        return Err(gone());
    };
    match resource.content {
        Ok(content) => Ok(Current { data: content.data, etag: content.etag }),
        Err(status) => Err(status_failure(status)),
    }
}

/// A `libdav` failure on a write, in the words the interface shows. Not
/// [`super::describe`]: that one is for a sync, where anything but a 401 is
/// "could not be reached"; a write has more to say about a 403, a 404 and
/// a 412, and says it here.
fn failure<E: std::fmt::Display>(e: WebDavError<E>) -> CommandError {
    match e {
        WebDavError::BadStatusCode(status) => status_failure(status),
        // `libdav` only raises this on a `PUT` for CalDAV's
        // `supported-calendar-component` precondition: the collection
        // holds tasks, not events -- an iCloud Reminders list, say.
        WebDavError::PreconditionFailed(_) => CommandError::new(
            codes::INVALID,
            "this calendar does not take events -- it may be a list of reminders or tasks",
        ),
        other => CommandError::new(
            codes::NETWORK,
            format!("the CalDAV server could not be reached: {other}"),
        ),
    }
}

/// What an HTTP status means for a write.
///
/// A 401 is the credential itself, and is the one answer that moves the
/// account to "sign in again" (`accountcal::after_write` reads
/// [`codes::FORBIDDEN`] for exactly that). A 403 is not: reading may still
/// work perfectly well, and this account simply may not change this
/// calendar -- somebody else's, shared read-only, or an invitation's copy a
/// scheduling server will only let its organiser edit.
fn status_failure(status: http::StatusCode) -> CommandError {
    match status.as_u16() {
        401 => CommandError::new(
            codes::FORBIDDEN,
            "the CalDAV server refused this account's credential",
        ),
        403 => CommandError::new(
            codes::INVALID,
            "the server would not let this account change that calendar",
        ),
        404 | 410 => gone(),
        412 => CommandError::new(
            codes::CONFLICT,
            "it changed on the server since it was last synced -- refresh and try again",
        ),
        _ => CommandError::new(codes::NETWORK, format!("the CalDAV server answered {status}")),
    }
}

fn is_gone(status: http::StatusCode) -> bool {
    matches!(status.as_u16(), 404 | 410)
}

fn gone() -> CommandError {
    CommandError::new(
        codes::NOT_FOUND,
        "this event is no longer on the server -- it may have been deleted somewhere else; \
         refresh and try again",
    )
}

fn unreadable() -> CommandError {
    CommandError::new(
        codes::UNREADABLE,
        "the CalDAV server holds this event in a form that could not be read",
    )
}

fn read(ics: &str) -> CommandResult<ICalendar> {
    ICalendar::parse(ics).map_err(|_| unreadable())
}

// ---- who "we" are ----------------------------------------------------------

/// This account, as far as an iCalendar file is concerned: the address it
/// organises from, the name to sign that with, and every address that
/// counts as "us" when an `ORGANIZER` is read back.
struct Me {
    name: String,
    address: String,
    /// Lower-cased: the account's own address and every identity's. An
    /// event organised from an alias is still this account's to change.
    addresses: Vec<String>,
}

impl Me {
    fn of(account: &Account) -> Self {
        let address = account.address.trim().to_string();
        let name = Some(account.display_name.trim())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                account
                    .identities
                    .iter()
                    .find(|identity| identity.address.trim().eq_ignore_ascii_case(&address))
                    .map(|identity| identity.name.trim())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_default()
            .to_string();
        let addresses = std::iter::once(address.as_str())
            .chain(account.identities.iter().map(|identity| identity.address.trim()))
            .filter(|address| !address.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        Self { name, address, addresses }
    }

    fn is(&self, address: &str) -> bool {
        let address = address.trim().to_ascii_lowercase();
        self.addresses.contains(&address)
    }
}

// ---- reading a resource ----------------------------------------------------

/// How a resource's own times are to be read: its `VTIMEZONE`s, and the
/// zone a time with no zone of its own falls back to.
///
/// Built the way [`super::events_from_ics`] reads the same file -- through
/// `calcard`'s resolver, which knows a `VTIMEZONE` by its `TZID`, a Windows
/// zone name, and Mozilla's prefixed ones -- because every comparison made
/// here is against an instant that expansion produced. A `RECURRENCE-ID`
/// written from a zone resolved any other way would name an occurrence a
/// few hours away from the one meant, and match nothing.
struct Reading {
    resolver: TzResolver<String>,
    /// The sync's own default zone, recovered: an event with no `TZID` was
    /// stored with that zone as its `tz`, and an unparseable one falls back
    /// to the machine's zone, exactly as the sync did.
    fallback: Tz,
}

impl Reading {
    fn of(parsed: &ICalendar, event: &Event) -> Self {
        let fallback = event
            .tz
            .parse::<Tz>()
            .or_else(|_| everyday_core::model::system_tz().parse::<Tz>())
            .unwrap_or(Tz::UTC);
        Self { resolver: parsed.build_owned_tz_resolver(), fallback }
    }

    /// The zone a value is read in: its own `TZID`, else `otherwise` (the
    /// `TZID` of its component's `DTSTART`, which is what `calcard` falls
    /// back to), else the fallback.
    fn zone_for(&self, tzid: Option<&str>, otherwise: Option<&str>) -> Tz {
        tzid.or(otherwise).and_then(|name| self.resolver.resolve(name)).unwrap_or(self.fallback)
    }

    /// Which of the four shapes `entry` -- a `DTSTART`, usually -- is in.
    fn form(&self, entry: &ICalendarEntry) -> Form {
        match moment(entry) {
            Some(at) if !at.has_time() => Form::Date,
            Some(at) if at.tz_hour.is_some() => Form::Utc,
            _ => match entry.tz_id() {
                Some(tzid) => Form::Zoned {
                    tzid: tzid.to_string(),
                    zone: self.zone_for(Some(tzid), None),
                    iana: iana_name(tzid, &self.resolver),
                },
                None => Form::Floating { zone: self.fallback },
            },
        }
    }

    /// Does `entry` -- an override's `DTSTART` or `RECURRENCE-ID` -- name
    /// the moment `event` starts? A date names an all-day occurrence on that
    /// day; anything else has to resolve to `event.start` exactly.
    fn names(&self, entry: &ICalendarEntry, start_tzid: Option<&str>, event: &Event) -> bool {
        let Some(at) = moment(entry) else { return false };
        if !at.has_time() {
            return event.all_day && date_of(at) == Some(event.local_date);
        }
        instant_of(at, self.zone_for(entry.tz_id(), start_tzid)) == Some(event.start)
    }

    /// A component's own start and end, fresh from the file: `DTEND`, else
    /// `DURATION`, else RFC 5545 §3.6.1's defaults (a day for a date, no
    /// length at all for a time).
    fn times(&self, comp: &ICalendarComponent) -> Option<Times> {
        let start_entry = comp.property(&ICalendarProperty::Dtstart)?;
        let start = moment(start_entry)?;
        let start_tzid = start_entry.tz_id();
        let end_entry = comp.property(&ICalendarProperty::Dtend);
        let seconds = comp.property(&ICalendarProperty::Duration).and_then(|entry| {
            match entry.values.first() {
                Some(ICalendarValue::Duration(duration)) => Some(duration.as_seconds()),
                _ => None,
            }
        });
        if !start.has_time() {
            let first = date_of(start)?;
            let after = match (end_entry.and_then(moment), seconds) {
                (Some(end), _) => date_of(end),
                (None, Some(seconds)) => add_days(first, seconds / 86_400),
                (None, None) => None,
            }
            .filter(|after| *after > first)
            .or_else(|| first.tomorrow().ok())?;
            return Some(Times::Days { first, after });
        }
        let begins = instant_of(start, self.zone_for(start_tzid, None))?;
        let ends = match (end_entry, seconds) {
            (Some(entry), _) => moment(entry)
                .and_then(|end| instant_of(end, self.zone_for(entry.tz_id(), start_tzid))),
            (None, Some(seconds)) => begins.checked_add(SignedDuration::from_secs(seconds)).ok(),
            (None, None) => None,
        }
        .filter(|ends| *ends >= begins)
        .unwrap_or(begins);
        Some(Times::Instants { start: begins, end: ends })
    }
}

/// A component's start and end, as [`Reading::times`] found them.
enum Times {
    /// All day: the first day, and the day after the last.
    Days {
        first: Date,
        after: Date,
    },
    Instants {
        start: Timestamp,
        end: Timestamp,
    },
}

/// The four shapes RFC 5545 lets a `DTSTART` take -- and so the shape every
/// `RECURRENCE-ID` and `EXDATE` that names one of its occurrences has to be
/// written in too (§3.8.4.4: "the value type ... MUST be the same as the
/// value type of the DTSTART property", and a `TZID` there means the same
/// one).
#[derive(Debug, Clone)]
enum Form {
    /// `;VALUE=DATE` -- a day, no time.
    Date,
    /// `...Z`.
    Utc,
    /// `;TZID=<tzid>`, a wall-clock time. `tzid` is exactly as written --
    /// `W. Europe Standard Time` stays `W. Europe Standard Time` -- `zone`
    /// is what `calcard` resolves it to, and `iana` the name jiff knows it
    /// by, when it has one.
    Zoned { tzid: String, zone: Tz, iana: Option<String> },
    /// A wall-clock time with no zone at all, read in `zone`.
    Floating { zone: Tz },
}

impl Form {
    /// A wall-clock time in the IANA zone `name`, which every draft's `tz`
    /// already is.
    fn zoned(name: &str) -> Self {
        Form::Zoned {
            tzid: name.to_string(),
            zone: name.parse::<Tz>().unwrap_or(Tz::UTC),
            iana: Some(name.to_string()),
        }
    }

    fn is_date(&self) -> bool {
        matches!(self, Form::Date)
    }

    /// The zone a value written in this form, with no `TZID` of its own, is
    /// read in.
    fn zone(&self, fallback: Tz) -> Tz {
        match self {
            Form::Utc => Tz::UTC,
            Form::Zoned { zone, .. } | Form::Floating { zone } => *zone,
            Form::Date => fallback,
        }
    }

    /// The parameters a property needs to be read in this form.
    fn params(&self) -> Vec<ICalendarParameter> {
        match self {
            // Without it the writer would spell a bare date as midnight,
            // `20260710T000000`, which is a time and not a day.
            Form::Date => vec![ICalendarParameter::value(ICalendarValueType::Date)],
            Form::Zoned { tzid, .. } => vec![ICalendarParameter::tzid(tzid.clone())],
            Form::Utc | Form::Floating { .. } => Vec::new(),
        }
    }

    /// The instant `at` -- or, for a date, the day `day` -- in this form.
    fn moment_at(&self, at: Timestamp, day: Date) -> Option<PartialDateTime> {
        match self {
            Form::Date => Some(date_partial(day)),
            Form::Utc => Some(utc_partial(at)),
            Form::Zoned { zone, .. } | Form::Floating { zone } => local_partial(at, *zone),
        }
    }
}

/// The IANA name a `TZID` stands for, if it stands for one jiff knows --
/// what a draft's `tz` has to be. The file's own spelling first, through
/// core's mapping (plain IANA, Windows names, Mozilla's prefixes), then
/// whatever `calcard` made of it, which also knows a `VTIMEZONE` that
/// carries `X-LIC-LOCATION` or Microsoft's zone number.
fn iana_name(tzid: &str, resolver: &TzResolver<String>) -> Option<String> {
    everyday_core::ics::resolve_tzid(tzid).or_else(|| {
        resolver
            .resolve(tzid)
            .and_then(|tz| tz.name())
            .map(|name| name.into_owned())
            .filter(|name| jiff::tz::TimeZone::get(name).is_ok())
    })
}

fn is_vevent(comp: &ICalendarComponent) -> bool {
    comp.component_type == ICalendarComponentType::VEvent
}

/// The master `VEVENT`: the one with no `RECURRENCE-ID`. A resource can lack
/// one -- somebody invited to a single occurrence of another person's series
/// gets only that occurrence's override -- and every caller has an answer
/// for that.
fn master_index(parsed: &ICalendar) -> Option<usize> {
    parsed.components.iter().position(|comp| is_vevent(comp) && !comp.is_recurrence_override())
}

fn override_indices(parsed: &ICalendar) -> Vec<u32> {
    parsed
        .components
        .iter()
        .enumerate()
        .filter(|(_, comp)| is_vevent(comp) && comp.is_recurrence_override())
        .map(|(index, _)| index as u32)
        .collect()
}

/// The override `event` came from, if it came from one: the override whose
/// own `DTSTART` is `event.start` -- which is the start the sync stored for
/// it -- or, failing that, the one whose `RECURRENCE-ID` is, for an
/// occurrence somebody else moved after this vault last synced. `DTSTART`
/// is tried across every override before `RECURRENCE-ID` is tried on any,
/// so two occurrences that swapped days are each found as what they now
/// are.
fn override_for(parsed: &ICalendar, event: &Event, reading: &Reading) -> Option<usize> {
    let naming = |prop: ICalendarProperty| {
        parsed.components.iter().position(|comp| {
            is_vevent(comp)
                && comp.is_recurrence_override()
                && comp.property(&prop).is_some_and(|entry| {
                    let start_tzid =
                        comp.property(&ICalendarProperty::Dtstart).and_then(|start| start.tz_id());
                    reading.names(entry, start_tzid, event)
                })
        })
    };
    naming(ICalendarProperty::Dtstart).or_else(|| naming(ICalendarProperty::RecurrenceId))
}

/// What a master's repeat rule is, as far as [`Recurrence`] is concerned.
enum Rule {
    /// It does not repeat.
    Once,
    /// One `RRULE`, and [`Recurrence`] holds it exactly.
    Fits(Recurrence),
    /// It repeats in a way [`Recurrence`] cannot hold -- a `BYSETPOS`, two
    /// `RRULE`s, only `RDATE`s -- and so can only be changed where it was
    /// made. Kept untouched by every edit here.
    Custom,
}

fn rule_of(master: &ICalendarComponent) -> Rule {
    let rules: Vec<&ICalendarEntry> =
        master.entries.iter().filter(|entry| entry.name == ICalendarProperty::Rrule).collect();
    match rules.as_slice() {
        [] if master.has_property(&ICalendarProperty::Rdate) => Rule::Custom,
        [] => Rule::Once,
        [only] => rrule_text(only)
            .and_then(|text| Recurrence::from_rrule(&text))
            .map_or(Rule::Custom, Rule::Fits),
        _ => Rule::Custom,
    }
}

/// An `RRULE`'s value as text: `calcard`'s own rendering of a rule it
/// parsed, or the raw text of one it could not.
fn rrule_text(entry: &ICalendarEntry) -> Option<String> {
    match entry.values.first()? {
        ICalendarValue::RecurrenceRule(rule) => Some(rule.to_string()),
        ICalendarValue::Text(text) => Some(text.clone()),
        _ => None,
    }
}

/// The last day a timed rule's `UNTIL` allows, on the event's own wall
/// clock.
///
/// [`Recurrence::to_rrule`] writes a timed event's last day as 23:59:59 of
/// it, in UTC (RFC 5545 §3.3.10 wants `UNTIL` in UTC whenever `DTSTART`
/// has a zone), and [`Recurrence::from_rrule`] reads that back as the UTC
/// *day* -- which, anywhere west of Greenwich, is the day after. Converting
/// the instant back to the event's zone gives the day the rule was written
/// for, whoever wrote it.
fn until_day(master: &ICalendarComponent, tz: &str) -> Option<Date> {
    let entry = master.property(&ICalendarProperty::Rrule)?;
    let ICalendarValue::RecurrenceRule(rule) = entry.values.first()? else { return None };
    let until = rule.until.as_ref()?;
    if !until.has_time() || until.tz_hour.is_none() {
        // A bare date, or a floating time: already the event's own day.
        return None;
    }
    let at = instant_of(until, Tz::UTC)?;
    Some(at.to_zoned(jiff::tz::TimeZone::get(tz).ok()?).date())
}

fn moment(entry: &ICalendarEntry) -> Option<&PartialDateTime> {
    entry.values.first()?.as_partial_date_time()
}

/// An address-valued property's address, `mailto:` and all whitespace off.
fn address_of(entry: &ICalendarEntry) -> Option<String> {
    entry
        .calendar_address()
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(str::to_string)
}

fn cn_of(entry: &ICalendarEntry) -> Option<String> {
    entry
        .parameter(&ICalendarParameterName::Cn)
        .and_then(|value| value.as_text())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Where a guest's answer stands. `DELEGATED` and the task-only states have
/// no [`AttendeeResponse`] to be, and read as "the server did not say".
fn response_of(entry: &ICalendarEntry) -> Option<AttendeeResponse> {
    match entry.parameter(&ICalendarParameterName::Partstat)? {
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::Accepted) => {
            Some(AttendeeResponse::Accepted)
        }
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::Tentative) => {
            Some(AttendeeResponse::Tentative)
        }
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::Declined) => {
            Some(AttendeeResponse::Declined)
        }
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::NeedsAction) => {
            Some(AttendeeResponse::NeedsAction)
        }
        _ => None,
    }
}

/// Is this component this account's to change? Yes when nobody organised it
/// -- a plain event on somebody's own calendar -- or this account did.
fn owned_by(comp: &ICalendarComponent, me: &Me) -> bool {
    comp.property(&ICalendarProperty::Organizer)
        .and_then(address_of)
        .is_none_or(|address| me.is(&address))
}

/// The zone a loaded draft is meant on: its `DTSTART`'s, when that names one
/// jiff knows, else the zone the sync stored it under, else the machine's.
fn draft_zone(form: &Form, event: &Event) -> String {
    if let Form::Zoned { iana: Some(name), .. } = form {
        return name.clone();
    }
    [event.tz.clone(), everyday_core::model::system_tz()]
        .into_iter()
        .find(|name| jiff::tz::TimeZone::get(name).is_ok())
        .unwrap_or_else(|| "UTC".to_string())
}

// ---- times, between jiff and calcard ---------------------------------------

fn date_of(at: &PartialDateTime) -> Option<Date> {
    Date::new(at.year? as i16, at.month? as i8, at.day? as i8).ok()
}

fn civil_of(at: &PartialDateTime) -> Option<jiff::civil::DateTime> {
    let time = jiff::civil::Time::new(
        at.hour.unwrap_or(0) as i8,
        at.minute.unwrap_or(0) as i8,
        at.second.unwrap_or(0) as i8,
        0,
    )
    .ok()?;
    Some(date_of(at)?.to_datetime(time))
}

/// The instant a written time names, read in `zone` unless it carries its
/// own `Z` -- the same resolution, through the same `calcard` call, the
/// sync's expansion makes.
fn instant_of(at: &PartialDateTime, zone: Tz) -> Option<Timestamp> {
    at.to_date_time_with_tz(zone).map(to_jiff)
}

fn date_partial(day: Date) -> PartialDateTime {
    PartialDateTime {
        year: Some(day.year() as u16),
        month: Some(day.month() as u8),
        day: Some(day.day() as u8),
        ..PartialDateTime::default()
    }
}

fn civil_partial(at: jiff::civil::DateTime) -> PartialDateTime {
    PartialDateTime {
        year: Some(at.year() as u16),
        month: Some(at.month() as u8),
        day: Some(at.day() as u8),
        hour: Some(at.hour() as u8),
        minute: Some(at.minute() as u8),
        second: Some(at.second() as u8),
        ..PartialDateTime::default()
    }
}

/// `at` with the `Z` designator: `tz_hour` and `tz_minute` both `Some(0)` is
/// the shape `calcard`'s writer spells as a trailing `Z`.
fn utc_partial(at: Timestamp) -> PartialDateTime {
    let mut written = civil_partial(at.to_zoned(jiff::tz::TimeZone::UTC).datetime());
    written.tz_hour = Some(0);
    written.tz_minute = Some(0);
    written
}

/// `at` on the wall clock of a `calcard` zone -- the one place this crate
/// goes from jiff *to* chrono (see `accountcal`'s module doc for why chrono
/// is here at all): a `RECURRENCE-ID` has to be written on exactly the wall
/// clock the sync's expansion read its occurrence from, and that clock is
/// `calcard`'s, `VTIMEZONE`s and Windows names included.
fn local_partial(at: Timestamp, zone: Tz) -> Option<PartialDateTime> {
    use chrono::{Datelike, Timelike};
    let local = chrono::DateTime::<chrono::Utc>::from_timestamp(at.as_second(), 0)?
        .with_timezone(&zone)
        .naive_local();
    Some(PartialDateTime {
        year: Some(local.year() as u16),
        month: Some(local.month() as u8),
        day: Some(local.day() as u8),
        hour: Some(local.hour() as u8),
        minute: Some(local.minute() as u8),
        second: Some(local.second() as u8),
        ..PartialDateTime::default()
    })
}

/// Midnight starting `day`, in `tz` -- the instants an all-day draft holds.
fn midnight(day: Date, tz: &str) -> Timestamp {
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
    zone.to_ambiguous_zoned(day.to_datetime(jiff::civil::Time::midnight()))
        .earlier()
        .map(|zoned| zoned.timestamp())
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

fn add_days(day: Date, days: i64) -> Option<Date> {
    day.checked_add(Span::new().try_days(days).ok()?).ok()
}

// ---- the edits ---------------------------------------------------------------

/// What a draft's times are written as.
///
/// An all-day draft is a pair of dates. A timed one keeps the zone it
/// already had when that is still the draft's zone -- an event quoted in
/// `Europe/Berlin` stays quoted there -- and otherwise follows the rule a
/// new event does: wall-clock time in the draft's zone when it repeats, so
/// "every Monday at nine" stays at nine across a change of clocks, and UTC
/// when it does not, where nothing is anchored to a clock and UTC is the
/// form no reader can misread. `keep_anchor` is for a series that already
/// repeats in UTC or in floating time: it stays anchored the way it was
/// unless the draft changed its zone, so renaming a series does not quietly
/// move half its occurrences by an hour.
fn form_for(
    draft: &EventDraft,
    repeats: bool,
    existing: Option<&Form>,
    keep_anchor: bool,
    event_tz: &str,
) -> Form {
    if draft.all_day {
        return Form::Date;
    }
    if let Some(form) = existing {
        let keep = match form {
            Form::Zoned { iana, .. } => iana.as_deref() == Some(draft.tz.as_str()),
            Form::Utc | Form::Floating { .. } => keep_anchor && draft.tz == event_tz,
            Form::Date => false,
        };
        if keep {
            return form.clone();
        }
    }
    if repeats { Form::zoned(&draft.tz) } else { Form::Utc }
}

/// The draft's own start and end, in `form`. A floating or zoned form is
/// only ever one whose wall clock is the draft's own zone's -- see
/// [`form_for`] -- so the draft's local times are the ones to write.
fn draft_times(draft: &EventDraft, form: &Form) -> (PartialDateTime, PartialDateTime) {
    match form {
        Form::Date => (date_partial(draft.start_date()), date_partial(draft.end_date_exclusive())),
        Form::Utc => (utc_partial(draft.start), utc_partial(draft.end)),
        Form::Zoned { .. } | Form::Floating { .. } => {
            (civil_partial(draft.local_start()), civil_partial(draft.local_end()))
        }
    }
}

/// Replace a component's `DTSTART` and its end -- `DTEND` or `DURATION`,
/// whichever it had -- with these, written in `form`.
fn set_times(
    comp: &mut ICalendarComponent,
    form: &Form,
    (start, end): (PartialDateTime, PartialDateTime),
) {
    comp.entries.retain(|entry| {
        !matches!(
            entry.name,
            ICalendarProperty::Dtstart | ICalendarProperty::Dtend | ICalendarProperty::Duration
        )
    });
    for (name, at) in [(ICalendarProperty::Dtstart, start), (ICalendarProperty::Dtend, end)] {
        comp.entries.push(ICalendarEntry::new(name).with_params(form.params()).with_value(at));
    }
}

/// The draft's repeat, in place of whatever `RRULE` the component had --
/// none, when the draft does not repeat. Written as text, exactly as
/// [`Recurrence::to_rrule`] spells it: `calcard`'s writer does not escape an
/// `RRULE`'s value, and its `;`s are the rule's own separators.
fn set_rule(comp: &mut ICalendarComponent, draft: &EventDraft) {
    comp.entries.retain(|entry| entry.name != ICalendarProperty::Rrule);
    if let Some(rule) = &draft.recurrence {
        comp.entries.push(
            ICalendarEntry::new(ICalendarProperty::Rrule)
                .with_value(ICalendarValue::Text(rule.to_rrule(draft.all_day, &draft.tz))),
        );
    }
}

/// One text property set to `value` -- the first of its name rewritten in
/// place, keeping a `LANGUAGE` it had, any repeats of it dropped -- or
/// removed outright when `value` is empty: a `LOCATION:` with nothing after
/// it is legal, and absence is the more honest statement.
fn set_text(comp: &mut ICalendarComponent, prop: ICalendarProperty, value: &str) {
    let mut kept = false;
    comp.entries.retain_mut(|entry| {
        if entry.name != prop {
            return true;
        }
        if kept || value.is_empty() {
            return false;
        }
        kept = true;
        entry.values = vec![ICalendarValue::Text(value.to_string())];
        // An `ALTREP` points at a richer copy of the *old* text.
        entry.params.retain(|param| param.name != ICalendarParameterName::Altrep);
        true
    });
    if !kept && !value.is_empty() {
        comp.entries.push(ICalendarEntry::new(prop).with_value(value.to_string()));
    }
}

/// Title, description, place and guests, from the draft.
fn set_content(comp: &mut ICalendarComponent, draft: &EventDraft, me: &Me) {
    if text_property(comp, ICalendarProperty::Description).unwrap_or_default() != draft.description
    {
        // Outlook and Exchange keep an HTML copy of the description beside
        // the plain one, and show it in preference; left behind, it would
        // go on showing the old text in every Microsoft client.
        comp.entries.retain(|entry| match &entry.name {
            ICalendarProperty::StyledDescription => false,
            ICalendarProperty::Other(name) => !name.eq_ignore_ascii_case("X-ALT-DESC"),
            _ => true,
        });
    }
    set_text(comp, ICalendarProperty::Summary, &draft.title);
    set_text(comp, ICalendarProperty::Description, &draft.description);
    set_text(comp, ICalendarProperty::Location, &draft.location);
    set_guests(comp, &draft.attendees, me);
}

fn mailto(address: &str) -> Uri {
    Uri::Location(format!("mailto:{}", address.trim()))
}

fn organizer_entry(me: &Me) -> ICalendarEntry {
    let params =
        if me.name.is_empty() { Vec::new() } else { vec![ICalendarParameter::cn(me.name.clone())] };
    ICalendarEntry::new(ICalendarProperty::Organizer)
        .with_params(params)
        .with_value(mailto(&me.address))
}

/// A guest newly invited: a required participant, not answered yet, asked
/// to answer -- what every calendar writes for a name typed into "Add
/// guests", and what an implicitly scheduling server turns into an
/// invitation.
fn attendee_entry(guest: &Attendee) -> ICalendarEntry {
    let mut params = Vec::new();
    if !guest.name.trim().is_empty() {
        params.push(ICalendarParameter::cn(guest.name.trim().to_string()));
    }
    params.push(ICalendarParameter::role(ICalendarParticipationRole::ReqParticipant));
    params.push(ICalendarParameter::partstat(ICalendarParticipationStatus::NeedsAction));
    params.push(ICalendarParameter::rsvp(true));
    ICalendarEntry::new(ICalendarProperty::Attendee)
        .with_params(params)
        .with_value(mailto(&guest.email))
}

/// An existing guest's line, kept -- with their answer, and the server's
/// own `SCHEDULE-STATUS` -- under the name the draft calls them by, when it
/// gives one.
fn renamed(mut entry: ICalendarEntry, name: &str) -> ICalendarEntry {
    let name = name.trim();
    if !name.is_empty() {
        entry.params.retain(|param| param.name != ICalendarParameterName::Cn);
        entry.params.insert(0, ICalendarParameter::cn(name.to_string()));
    }
    entry
}

/// The guest list, to match `guests`.
///
/// A guest already on the list keeps their line, answer and all; a new one
/// gets [`attendee_entry`]; one the draft no longer names is dropped, which
/// is what tells a scheduling server to send them a cancellation. The
/// organiser's own `ATTENDEE` line, when the file has one, is kept too:
/// [`editable_from_ics`] leaves it out of the draft, so its absence from the
/// draft says nothing. Guests where there were none gain this account as
/// `ORGANIZER`; no guests left, on an event this account organised, drops
/// the `ORGANIZER` too and it goes back to being a plain event.
fn set_guests(comp: &mut ICalendarComponent, guests: &[Attendee], me: &Me) {
    let organizer = comp.property(&ICalendarProperty::Organizer).and_then(address_of);
    let is_organizer =
        |address: &str| organizer.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(address));
    let (listed, rest): (Vec<ICalendarEntry>, Vec<ICalendarEntry>) =
        std::mem::take(&mut comp.entries)
            .into_iter()
            .partition(|entry| entry.name == ICalendarProperty::Attendee);
    comp.entries = rest;
    if guests.is_empty() {
        if organizer.as_deref().is_some_and(|address| me.is(address)) {
            comp.entries.retain(|entry| entry.name != ICalendarProperty::Organizer);
        }
        return;
    }
    if organizer.is_none() {
        comp.entries.push(organizer_entry(me));
    }
    comp.entries.extend(
        listed
            .iter()
            .filter(|entry| address_of(entry).is_some_and(|address| is_organizer(address.as_str())))
            .cloned(),
    );
    for guest in guests {
        let email = guest.email.trim();
        if email.is_empty() || is_organizer(email) {
            continue;
        }
        let existing = listed.iter().find(|entry| {
            address_of(entry).is_some_and(|address| address.eq_ignore_ascii_case(email))
        });
        comp.entries.push(match existing {
            Some(entry) => renamed(entry.clone(), &guest.name),
            None => attendee_entry(guest),
        });
    }
}

fn sequence_of(comp: &ICalendarComponent) -> Option<i64> {
    comp.property(&ICalendarProperty::Sequence)?.values.first()?.as_integer()
}

fn set_sequence(comp: &mut ICalendarComponent, sequence: i64) {
    match comp.property_mut(&ICalendarProperty::Sequence) {
        Some(entry) => entry.values = vec![ICalendarValue::Integer(sequence)],
        None => comp.add_sequence(sequence),
    }
}

/// `DTSTAMP` -- and `LAST-MODIFIED`, where the file keeps one -- set to now.
fn stamp(comp: &mut ICalendarComponent, now: Timestamp) {
    let at = utc_partial(now);
    match comp.property_mut(&ICalendarProperty::Dtstamp) {
        Some(entry) => {
            entry.params.clear();
            entry.values = vec![ICalendarValue::from(at.clone())];
        }
        None => comp.add_dtstamp(at.clone()),
    }
    if let Some(entry) = comp.property_mut(&ICalendarProperty::LastModified) {
        entry.params.clear();
        entry.values = vec![ICalendarValue::from(at)];
    }
}

/// `SEQUENCE` and `DTSTAMP` after an edit -- see the module doc's
/// "`SEQUENCE`, and what it pairs". This account's own event: every
/// `VEVENT` to one past the highest any of them had, stamped now. Somebody
/// else's: only the component that changed is stamped, and no number moves.
fn settle(parsed: &mut ICalendar, own: bool, touched: usize, now: Timestamp) {
    if own {
        let next = parsed
            .components
            .iter()
            .filter(|comp| is_vevent(comp))
            .filter_map(sequence_of)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        for comp in parsed.components.iter_mut().filter(|comp| is_vevent(comp)) {
            set_sequence(comp, next);
            stamp(comp, now);
        }
    } else if let Some(comp) = parsed.components.get_mut(touched) {
        stamp(comp, now);
    }
}

/// A new override of `master`'s occurrence `recurrence_id`, appended to the
/// resource; answers its index.
///
/// RFC 5545 §3.8.4.4 makes an override a *replacement* for its occurrence,
/// not a patch on it -- a property the override lacks, the occurrence
/// lacks. So it starts as a copy of the master, minus what only a master
/// may carry (its rule, its exclusions) and what the edit is about to set
/// anyway, and its alarms are copied with it: without them, the one
/// meeting that moved is the one meeting nobody is reminded of. Its
/// `SEQUENCE` is the master's own, for the pairing the module doc explains.
fn add_override(parsed: &mut ICalendar, master: usize, recurrence_id: ICalendarEntry) -> usize {
    let source = &parsed.components[master];
    let mut comp = ICalendarComponent::new(ICalendarComponentType::VEvent);
    comp.entries = source
        .entries
        .iter()
        .filter(|entry| {
            !matches!(
                entry.name,
                ICalendarProperty::Dtstart
                    | ICalendarProperty::Dtend
                    | ICalendarProperty::Duration
                    | ICalendarProperty::Rrule
                    | ICalendarProperty::Rdate
                    | ICalendarProperty::Exdate
                    | ICalendarProperty::Exrule
                    | ICalendarProperty::RecurrenceId
                    | ICalendarProperty::Dtstamp
                    | ICalendarProperty::Created
                    | ICalendarProperty::LastModified
            )
        })
        .cloned()
        .collect();
    comp.entries.push(recurrence_id);
    let alarms: Vec<ICalendarComponent> = source
        .component_ids
        .iter()
        .filter_map(|id| parsed.components.get(*id as usize))
        .filter(|child| {
            child.component_type == ICalendarComponentType::VAlarm && child.component_ids.is_empty()
        })
        .cloned()
        .collect();
    let index = parsed.components.len();
    parsed.components.push(comp);
    parsed.components[0].component_ids.push(index as u32);
    for alarm in alarms {
        let id = parsed.components.len() as u32;
        parsed.components.push(alarm);
        parsed.components[index].component_ids.push(id);
    }
    index
}

/// What every edit of one resource needs besides the resource itself.
struct Edit<'a> {
    event: &'a Event,
    draft: &'a EventDraft,
    me: &'a Me,
    reading: Reading,
}

/// How far a series edit moves every occurrence.
///
/// Worked out once, from the occurrence that was edited -- how far the
/// draft's start is from where that occurrence was -- and then applied to
/// the master's `DTSTART` and to every `EXDATE`, `RDATE` and override
/// `RECURRENCE-ID`, so each keeps naming the occurrence it named before.
/// Without that, moving a series half an hour would bring back every
/// occurrence somebody had deleted and draw every moved one twice. Measured
/// three ways, one per form, so that a wall-clock series moves on the wall
/// clock -- half an hour later is half an hour later in March and in
/// November alike -- a UTC one moves by the instant, and an all-day one by
/// whole days.
struct Shift {
    form: Form,
    /// The draft's zone: where wall-clock shifts are measured.
    zone: jiff::tz::TimeZone,
    days: i64,
    instant: SignedDuration,
    civil: SignedDuration,
}

impl Shift {
    fn new(form: Form, draft: &EventDraft, event: &Event) -> Self {
        let zone = jiff::tz::TimeZone::get(&draft.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let was = event.start.to_zoned(zone.clone());
        let was_day = if event.all_day { event.local_date } else { was.date() };
        Shift {
            days: i64::from((draft.start_date() - was_day).get_days()),
            instant: draft.start.duration_since(event.start),
            civil: draft.local_start().duration_since(was.datetime()),
            form,
            zone,
        }
    }

    /// `at`, read in `zone` unless it says otherwise, moved and written in
    /// this shift's form.
    fn apply(&self, at: &PartialDateTime, zone: Tz) -> Option<PartialDateTime> {
        match &self.form {
            Form::Date => {
                let day = if at.has_time() {
                    instant_of(at, zone)?.to_zoned(self.zone.clone()).date()
                } else {
                    date_of(at)?
                };
                Some(date_partial(add_days(day, self.days)?))
            }
            Form::Utc => Some(utc_partial(instant_of(at, zone)?.checked_add(self.instant).ok()?)),
            Form::Zoned { .. } | Form::Floating { .. } => {
                let local = instant_of(at, zone)?.to_zoned(self.zone.clone()).datetime();
                Some(civil_partial(local.checked_add(self.civil).ok()?))
            }
        }
    }

    /// The master's new start, from its old one, and an end the draft's
    /// own length after it.
    fn master(
        &self,
        start: &PartialDateTime,
        zone: Tz,
        draft: &EventDraft,
    ) -> Option<(PartialDateTime, PartialDateTime)> {
        let start = self.apply(start, zone)?;
        let end = match &self.form {
            Form::Date => {
                let length = (draft.end_date_exclusive() - draft.start_date()).get_days().max(1);
                date_partial(add_days(date_of(&start)?, i64::from(length))?)
            }
            Form::Utc => utc_partial(
                instant_of(&start, Tz::UTC)?
                    .checked_add(draft.end.duration_since(draft.start))
                    .ok()?,
            ),
            Form::Zoned { .. } | Form::Floating { .. } => civil_partial(
                civil_of(&start)?
                    .checked_add(draft.local_end().duration_since(draft.local_start()))
                    .ok()?,
            ),
        };
        Some((start, end))
    }

    /// Every date-time value of an `EXDATE`, `RDATE` or `RECURRENCE-ID`,
    /// moved. A value that will not move -- an `RDATE` given as a `PERIOD`,
    /// or a time that falls in a gap the clocks skipped -- leaves the whole
    /// property exactly as it was, rather than half-moved.
    fn move_refs(&self, entry: &mut ICalendarEntry, reading: &Reading, default_zone: Tz) {
        let zone =
            entry.tz_id().and_then(|name| reading.resolver.resolve(name)).unwrap_or(default_zone);
        let moved: Option<Vec<ICalendarValue>> = entry
            .values
            .iter()
            .map(|value| {
                let at = value.as_partial_date_time()?;
                self.apply(at, zone).map(ICalendarValue::from)
            })
            .collect();
        let Some(moved) = moved else { return };
        entry.values = moved;
        entry.params.retain(|param| !reading_param(param));
        entry.params.extend(self.form.params());
    }
}

/// A parameter that says how a date-time value is read -- its `TZID`, or
/// `VALUE=DATE` -- as opposed to one about the property itself, like a
/// `RECURRENCE-ID`'s `RANGE`.
fn reading_param(param: &ICalendarParameter) -> bool {
    matches!(param.name, ICalendarParameterName::Tzid | ICalendarParameterName::Value)
}

/// An event that does not repeat -- or a lone override, with no master to
/// repeat it -- edited in place. A master may start repeating here; a lone
/// override may not, since an override with an `RRULE` is no longer a
/// replacement for anything.
fn edit_single(comp: &mut ICalendarComponent, is_master: bool, edit: &Edit<'_>) {
    let draft = edit.draft;
    let existing = comp.property(&ICalendarProperty::Dtstart).map(|entry| edit.reading.form(entry));
    let repeats = is_master && draft.recurrence.is_some();
    let form = form_for(draft, repeats, existing.as_ref(), false, &edit.event.tz);
    set_times(comp, &form, draft_times(draft, &form));
    if is_master {
        set_rule(comp, draft);
    }
    set_content(comp, draft, edit.me);
}

/// One occurrence of a series, changed: its override rewritten when it
/// already has one, or a new override made for it.
///
/// The `RECURRENCE-ID` of a new one is the occurrence's *original* start --
/// for an occurrence the rule produced, the start the sync stored for it --
/// written in the master's own form, which is what pairs the two. An
/// existing override keeps the `RECURRENCE-ID` it has: that, and not
/// wherever the occurrence was moved to since, is the slot it replaces.
fn edit_occurrence(
    parsed: &mut ICalendar,
    master: usize,
    overridden: Option<usize>,
    edit: &Edit<'_>,
) -> CommandResult<usize> {
    let target = match overridden {
        Some(index) => index,
        None => {
            let start = parsed.components[master]
                .property(&ICalendarProperty::Dtstart)
                .ok_or_else(unreadable)?;
            let form = edit.reading.form(start);
            let original =
                form.moment_at(edit.event.start, edit.event.local_date).ok_or_else(unreadable)?;
            let recurrence_id = ICalendarEntry::new(ICalendarProperty::RecurrenceId)
                .with_params(form.params())
                .with_value(original);
            add_override(parsed, master, recurrence_id)
        }
    };
    let comp = &mut parsed.components[target];
    let existing = comp.property(&ICalendarProperty::Dtstart).map(|entry| edit.reading.form(entry));
    let form = form_for(edit.draft, true, existing.as_ref(), false, &edit.event.tz);
    set_times(comp, &form, draft_times(edit.draft, &form));
    set_content(comp, edit.draft, edit.me);
    Ok(target)
}

/// The whole series, changed from one of its occurrences.
///
/// While it goes on repeating, the master moves by the [`Shift`] the edited
/// occurrence made, and its rule becomes the draft's -- or stays exactly as
/// it was, when it is one [`Recurrence`] cannot hold and the draft
/// therefore carries none. Overrides keep their own changes; only the
/// `RECURRENCE-ID` naming their slot moves with the series. Switching
/// between all-day and timed is the exception: no `EXDATE` or
/// `RECURRENCE-ID` can name an occurrence of a different value type
/// (§3.8.4.4), so those exceptions go.
///
/// When the draft stops it repeating, the event that remains is the one the
/// person was looking at, at the times they gave it -- not the series' first
/// occurrence moved by the same amount, which is somewhere they never saw
/// in the editor. Everything that only made sense of a series -- exclusions,
/// extra dates, overrides -- goes with the rule.
fn edit_series(
    parsed: &mut ICalendar,
    master: usize,
    edit: &Edit<'_>,
    doomed: &mut Vec<u32>,
) -> CommandResult<()> {
    let (event, draft) = (edit.event, edit.draft);
    let start_entry =
        parsed.components[master].property(&ICalendarProperty::Dtstart).ok_or_else(unreadable)?;
    let old = edit.reading.form(start_entry);
    let start = moment(start_entry).cloned().ok_or_else(unreadable)?;
    let overrides = override_indices(parsed);
    let keeps_repeating =
        draft.recurrence.is_some() || matches!(rule_of(&parsed.components[master]), Rule::Custom);

    if !keeps_repeating {
        let form = form_for(draft, false, Some(&old), false, &event.tz);
        let comp = &mut parsed.components[master];
        set_times(comp, &form, draft_times(draft, &form));
        comp.entries.retain(|entry| {
            !matches!(
                entry.name,
                ICalendarProperty::Rrule
                    | ICalendarProperty::Rdate
                    | ICalendarProperty::Exdate
                    | ICalendarProperty::Exrule
            )
        });
        doomed.extend(overrides);
    } else {
        let form = form_for(draft, true, Some(&old), true, &event.tz);
        let shift = Shift::new(form.clone(), draft, event);
        let old_zone = old.zone(edit.reading.fallback);
        let times = shift.master(&start, old_zone, draft).ok_or_else(unreadable)?;
        let toggled = old.is_date() != form.is_date();
        let comp = &mut parsed.components[master];
        set_times(comp, &form, times);
        if toggled {
            comp.entries.retain(|entry| {
                !matches!(entry.name, ICalendarProperty::Rdate | ICalendarProperty::Exdate)
            });
            doomed.extend(overrides);
        } else {
            for entry in comp.entries.iter_mut().filter(|entry| {
                matches!(entry.name, ICalendarProperty::Rdate | ICalendarProperty::Exdate)
            }) {
                shift.move_refs(entry, &edit.reading, old_zone);
            }
            for index in overrides {
                if let Some(entry) =
                    parsed.components[index as usize].property_mut(&ICalendarProperty::RecurrenceId)
                {
                    shift.move_refs(entry, &edit.reading, old_zone);
                }
            }
        }
        if draft.recurrence.is_some() {
            set_rule(&mut parsed.components[master], draft);
        }
    }
    set_content(&mut parsed.components[master], draft, edit.me);
    Ok(())
}

// ---- the pure functions the four writes are made of ------------------------

/// A whole new resource for `draft`: one `VCALENDAR`, one `VEVENT`.
///
/// When a `TZID` is written -- a repeating timed event -- `calcard` builds
/// its `VTIMEZONE` from the same tz database it reads with, as RFC 5545
/// §3.2.19 asks of any file that names a zone. The servers this targets
/// resolve an IANA `TZID` themselves regardless (Radicale, Fastmail and
/// iCloud all do), so the definition is for whichever stricter client
/// reads the file next.
fn new_resource_ics(draft: &EventDraft, uid: &str, me: &Me, now: Timestamp) -> String {
    let form = form_for(draft, draft.recurrence.is_some(), None, false, &draft.tz);
    let mut event = ICalendarComponent::new(ICalendarComponentType::VEvent);
    event.add_uid(uid);
    event.add_dtstamp(utc_partial(now));
    set_times(&mut event, &form, draft_times(draft, &form));
    set_rule(&mut event, draft);
    event.add_sequence(0);
    set_content(&mut event, draft, me);

    let mut root = ICalendarComponent::new(ICalendarComponentType::VCalendar);
    root.add_property(ICalendarProperty::Version, "2.0");
    root.add_property(ICalendarProperty::Prodid, PRODID);
    root.component_ids.push(1);
    let mut calendar = ICalendar { components: vec![root, event] };
    calendar.add_missing_timezones();
    calendar.to_string()
}

/// `event` as `ics` -- its resource, fetched fresh -- has it, ready to be
/// changed.
///
/// An occurrence the rule produced has no times of its own in the file: the
/// master only says where the *first* one falls. Its draft starts where the
/// occurrence does and runs as long as the master says an occurrence runs.
/// An overridden occurrence, or an event that never repeated, has its own
/// `DTSTART` and end, and those are read fresh rather than trusted from the
/// vault's possibly older copy.
fn editable_from_ics(ics: &str, event: &Event, me: &Me) -> CommandResult<EditableEvent> {
    let parsed = read(ics)?;
    let reading = Reading::of(&parsed, event);
    let master = master_index(&parsed).map(|index| &parsed.components[index]);
    let overridden = override_for(&parsed, event, &reading);
    let source = overridden.map(|index| &parsed.components[index]).or(master).ok_or_else(gone)?;
    let recurring = master.is_some_and(ICalendarComponent::is_recurrent);
    let start_entry = source.property(&ICalendarProperty::Dtstart).ok_or_else(unreadable)?;
    let tz = draft_zone(&reading.form(start_entry), event);

    let expanded = recurring && overridden.is_none();
    let (all_day, start, end) = match reading.times(source).ok_or_else(unreadable)? {
        Times::Days { first, after } => {
            let (first, after) = if expanded {
                let length = i64::from((after - first).get_days().max(1));
                (event.local_date, add_days(event.local_date, length).unwrap_or(after))
            } else {
                (first, after)
            };
            (true, midnight(first, &tz), midnight(after, &tz))
        }
        Times::Instants { start, end } if expanded => (
            false,
            event.start,
            event.start.checked_add(end.duration_since(start)).unwrap_or(event.end),
        ),
        Times::Instants { start, end } => (false, start, end),
    };

    let (recurrence, custom_recurrence) = match master.map(rule_of) {
        Some(Rule::Fits(mut rule)) => {
            if rule.until.is_some()
                && let Some(day) = master.and_then(|m| until_day(m, &tz))
            {
                rule.until = Some(day);
            }
            (Some(rule), false)
        }
        Some(Rule::Custom) => (None, true),
        Some(Rule::Once) | None => (None, false),
    };

    let organizer = source
        .property(&ICalendarProperty::Organizer)
        .or_else(|| master.and_then(|m| m.property(&ICalendarProperty::Organizer)));
    let organizer_address = organizer.and_then(address_of);
    // The organiser's own `ATTENDEE` line, which Apple and Google both
    // write, is who sent the invitation, not a guest of it.
    let attendees = source
        .entries
        .iter()
        .filter(|entry| entry.name == ICalendarProperty::Attendee)
        .filter_map(|entry| {
            let email = address_of(entry)?;
            if organizer_address.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&email)) {
                return None;
            }
            let name = cn_of(entry).unwrap_or_default();
            Some(Attendee { email, name, response: response_of(entry) })
        })
        .collect();

    Ok(EditableEvent {
        event_id: event.id,
        calendar_id: event.calendar_id,
        draft: EventDraft {
            title: text_property(source, ICalendarProperty::Summary).unwrap_or_default(),
            description: text_property(source, ICalendarProperty::Description).unwrap_or_default(),
            location: text_property(source, ICalendarProperty::Location).unwrap_or_default(),
            start,
            end,
            all_day,
            tz,
            attendees,
            recurrence,
        },
        recurring,
        custom_recurrence,
        own: organizer_address.as_deref().is_none_or(|address| me.is(address)),
        organizer: organizer.and_then(cn_of).or(organizer_address).unwrap_or_default(),
    })
}

/// `ics` with `event` changed to match `draft` -- see [`edit_single`],
/// [`edit_occurrence`] and [`edit_series`] for the three shapes that takes.
///
/// A resource that does not repeat is one event whichever scope was asked
/// for, as [`EventScope::Occurrence`]'s own doc says; a draft's repeat rule
/// on an occurrence edit is ignored, since one occurrence cannot repeat.
fn apply_edit(
    ics: &str,
    event: &Event,
    draft: &EventDraft,
    scope: EventScope,
    me: &Me,
    now: Timestamp,
) -> CommandResult<String> {
    let mut parsed = read(ics)?;
    let edit = Edit { event, draft, me, reading: Reading::of(&parsed, event) };
    let master = master_index(&parsed);
    let overridden = override_for(&parsed, event, &edit.reading);
    let recurring = master.is_some_and(|index| parsed.components[index].is_recurrent());
    let primary = master.or(overridden).ok_or_else(gone)?;
    let own = owned_by(&parsed.components[primary], me);
    let mut doomed = Vec::new();

    let touched = match (recurring, scope) {
        (false, _) => {
            edit_single(&mut parsed.components[primary], master.is_some(), &edit);
            primary
        }
        (true, EventScope::Occurrence) => edit_occurrence(&mut parsed, primary, overridden, &edit)?,
        (true, EventScope::Series) => {
            edit_series(&mut parsed, primary, &edit, &mut doomed)?;
            primary
        }
    };

    settle(&mut parsed, own, touched, now);
    if !doomed.is_empty() {
        parsed.remove_component_ids(&doomed);
    }
    parsed.add_missing_timezones();
    Ok(parsed.to_string())
}

/// What deleting `event` does to the resource it came from.
enum Removal {
    /// The whole resource goes.
    Resource,
    /// The resource stays, rewritten as this.
    Rewrite(String),
}

/// What deleting `event`, with `scope`, does to `ics`.
///
/// A series, or an event that does not repeat, is its whole resource. One
/// occurrence of a series is [`exclude_occurrence`]. A resource holding only
/// overrides -- occurrences of somebody else's series this account was
/// invited to one at a time -- loses the one override, or is deleted with
/// its last.
fn removal_for(
    ics: &str,
    event: &Event,
    scope: EventScope,
    me: &Me,
    now: Timestamp,
) -> CommandResult<Removal> {
    if scope == EventScope::Series {
        return Ok(Removal::Resource);
    }
    let mut parsed = read(ics)?;
    match master_index(&parsed) {
        Some(master) if parsed.components[master].is_recurrent() => {
            exclude_occurrence(parsed, event, me, now).map(Removal::Rewrite)
        }
        Some(_) => Ok(Removal::Resource),
        None => {
            let reading = Reading::of(&parsed, event);
            let vevents = parsed.components.iter().filter(|comp| is_vevent(comp)).count();
            match override_for(&parsed, event, &reading) {
                Some(index) if vevents > 1 => {
                    parsed.remove_component_ids(&[index as u32]);
                    Ok(Removal::Rewrite(parsed.to_string()))
                }
                _ => Ok(Removal::Resource),
            }
        }
    }
}

/// One occurrence of a series, deleted: an `EXDATE` on the master for its
/// original start -- in the master's own form, or copied from the
/// `RECURRENCE-ID` of the override that already replaced it -- and that
/// override gone with it.
fn exclude_occurrence(
    mut parsed: ICalendar,
    event: &Event,
    me: &Me,
    now: Timestamp,
) -> CommandResult<String> {
    let reading = Reading::of(&parsed, event);
    let master = master_index(&parsed).ok_or_else(gone)?;
    let overridden = override_for(&parsed, event, &reading);
    let replaced = overridden
        .and_then(|index| parsed.components[index].property(&ICalendarProperty::RecurrenceId));
    let excluded = match replaced {
        Some(recurrence_id) => {
            // Only what says how to read the value: a `RANGE` on the
            // override's `RECURRENCE-ID` means nothing on an `EXDATE`.
            let params =
                recurrence_id.params.iter().filter(|param| reading_param(param)).cloned().collect();
            ICalendarEntry::new(ICalendarProperty::Exdate)
                .with_params(params)
                .with_values(recurrence_id.values.clone())
        }
        None => {
            let start = parsed.components[master]
                .property(&ICalendarProperty::Dtstart)
                .ok_or_else(unreadable)?;
            let form = reading.form(start);
            let at = form.moment_at(event.start, event.local_date).ok_or_else(unreadable)?;
            ICalendarEntry::new(ICalendarProperty::Exdate).with_params(form.params()).with_value(at)
        }
    };
    let own = owned_by(&parsed.components[master], me);
    parsed.components[master].entries.push(excluded);
    settle(&mut parsed, own, master, now);
    if let Some(index) = overridden {
        parsed.remove_component_ids(&[index as u32]);
    }
    parsed.add_missing_timezones();
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::account::Provider;
    use everyday_core::id::CalendarId;
    use everyday_core::recurrence::{Frequency, Weekday};
    use jiff::civil::date;

    const NY: &str = "America/New_York";

    /// A weekly stand-up, six Mondays from 2026-10-05, across the end of US
    /// daylight saving time on 2026-11-01 -- organised by this account, with
    /// one guest who has already accepted.
    const STANDUP: &str = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        PRODID:-//Test//EN\r\n\
        BEGIN:VEVENT\r\n\
        UID:standup@example.com\r\n\
        DTSTAMP:20260901T000000Z\r\n\
        DTSTART;TZID=America/New_York:20261005T090000\r\n\
        DTEND;TZID=America/New_York:20261005T093000\r\n\
        RRULE:FREQ=WEEKLY;COUNT=6\r\n\
        SEQUENCE:0\r\n\
        SUMMARY:Standup\r\n\
        ORGANIZER;CN=Hari:mailto:hari@example.com\r\n\
        ATTENDEE;CN=Sam;PARTSTAT=ACCEPTED:mailto:sam@example.com\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    /// "The last weekday of the month", three times -- a rule `Recurrence`
    /// cannot hold.
    const LAST_WEEKDAY: &str = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        PRODID:-//Test//EN\r\n\
        BEGIN:VEVENT\r\n\
        UID:review@example.com\r\n\
        DTSTAMP:20260901T000000Z\r\n\
        DTSTART;TZID=Europe/London:20261030T160000\r\n\
        DTEND;TZID=Europe/London:20261030T170000\r\n\
        RRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3\r\n\
        SUMMARY:Month-end review\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    /// Somebody else's meeting, on this account's calendar because it was
    /// invited.
    const INVITATION: &str = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        PRODID:-//Test//EN\r\n\
        BEGIN:VEVENT\r\n\
        UID:offsite@example.com\r\n\
        DTSTAMP:20260901T000000Z\r\n\
        DTSTART:20261020T150000Z\r\n\
        DTEND:20261020T160000Z\r\n\
        SUMMARY:Offsite\r\n\
        SEQUENCE:2\r\n\
        ORGANIZER;CN=Boss:mailto:boss@example.com\r\n\
        ATTENDEE;CN=Boss;PARTSTAT=ACCEPTED:mailto:boss@example.com\r\n\
        ATTENDEE;CN=Hari;PARTSTAT=TENTATIVE:mailto:Hari@Example.com\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    fn me() -> Me {
        let mut account = Account::new(Provider::ICloud, "hari@example.com");
        account.display_name = "Hari".into();
        Me::of(&account)
    }

    fn now() -> Timestamp {
        "2026-10-07T12:00:00Z".parse().unwrap()
    }

    fn at(day: Date, hour: i8, minute: i8, tz: &str) -> Timestamp {
        let zone = jiff::tz::TimeZone::get(tz).unwrap();
        day.at(hour, minute, 0, 0).to_zoned(zone).unwrap().timestamp()
    }

    /// Content lines as a reader sees them, RFC 5545 §3.1's folding undone,
    /// so an assertion can look for a whole property without caring where
    /// the writer chose to wrap it.
    fn unfolded(ics: &str) -> String {
        ics.replace("\r\n ", "")
    }

    /// Every occurrence the sync would store for `ics` -- through the parent
    /// module's own expansion, so each test proves its edit reads back the
    /// way the next sync will read it, not merely that some text is there.
    fn occurrences(ics: &str) -> Vec<Event> {
        assert!(ICalendar::parse(ics).is_ok(), "calcard must read back what was written:\n{ics}");
        let window = (date(2020, 1, 1), date(2030, 12, 31));
        let (mut events, _, _) = super::super::events_from_ics(
            ics,
            CalendarId::new(),
            "/cal/event.ics",
            "UTC",
            window,
            now(),
        );
        events.sort_by_key(|event| event.start);
        events
    }

    fn on(events: &[Event], day: Date) -> Event {
        events
            .iter()
            .find(|event| event.local_date == day)
            .unwrap_or_else(|| panic!("no occurrence on {day}"))
            .clone()
    }

    fn lunch() -> EventDraft {
        EventDraft {
            title: "Lunch".into(),
            description: String::new(),
            location: String::new(),
            start: at(date(2026, 10, 8), 12, 0, NY),
            end: at(date(2026, 10, 8), 13, 0, NY),
            all_day: false,
            tz: NY.into(),
            attendees: Vec::new(),
            recurrence: None,
        }
    }

    #[test]
    fn a_new_repeating_event_is_written_on_its_own_wall_clock_with_its_rule_and_its_guests() {
        let draft = EventDraft {
            title: "Planning".into(),
            description: "Agenda: budget, hiring; then\nwhatever is left".into(),
            location: "Room 2, the small one".into(),
            start: at(date(2026, 10, 5), 9, 0, NY),
            end: at(date(2026, 10, 5), 9, 30, NY),
            all_day: false,
            tz: NY.into(),
            attendees: vec![Attendee { name: "Sam".into(), ..Attendee::new("sam@example.com") }],
            recurrence: Some(Recurrence {
                weekdays: vec![Weekday::Monday, Weekday::Wednesday],
                until: Some(date(2026, 12, 31)),
                ..Recurrence::every(Frequency::Weekly)
            }),
        };
        let ics = new_resource_ics(&draft, "abc@everyday", &me(), now());
        let text = unfolded(&ics);
        for line in [
            "PRODID:-//Every Day//EN",
            "UID:abc@everyday",
            "DTSTAMP:20261007T120000Z",
            "DTSTART;TZID=America/New_York:20261005T090000",
            "DTEND;TZID=America/New_York:20261005T093000",
            // 23:59:59 on the last day in New York, in winter, is 04:59:59
            // UTC the morning after.
            "RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20270101T045959Z",
            "SEQUENCE:0",
            "LOCATION:Room 2\\, the small one",
            "DESCRIPTION:Agenda: budget\\, hiring\\; then\\nwhatever is left",
            "ORGANIZER;CN=Hari:mailto:hari@example.com",
            "ATTENDEE;CN=Sam;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:sam@example.com",
            "BEGIN:VTIMEZONE",
            "TZID:America/New_York",
        ] {
            assert!(text.contains(line), "missing {line:?} in:\n{text}");
        }
        for physical in ics.split("\r\n") {
            assert!(physical.len() <= 75, "a line of {} octets: {physical:?}", physical.len());
        }

        let events = occurrences(&ics);
        assert_eq!(events[0].start, draft.start);
        assert_eq!(events.last().unwrap().local_date, date(2026, 12, 30), "the last Wednesday");
        let ny = jiff::tz::TimeZone::get(NY).unwrap();
        for event in &events {
            let local = event.start.to_zoned(ny.clone());
            let wall = (local.hour(), local.minute());
            assert_eq!(wall, (9, 0), "{} left nine o'clock", event.local_date);
        }

        // ...and loaded back for editing, it is the draft that was written.
        let editable = editable_from_ics(&ics, &events[0], &me()).unwrap();
        assert!(editable.recurring && editable.own && !editable.custom_recurrence);
        assert_eq!(editable.draft.title, draft.title);
        assert_eq!(editable.draft.description, draft.description);
        assert_eq!(editable.draft.location, draft.location);
        assert_eq!((editable.draft.start, editable.draft.end), (draft.start, draft.end));
        assert!(!editable.draft.all_day);
        assert_eq!(editable.draft.tz, draft.tz);
        assert_eq!(
            editable.draft.recurrence, draft.recurrence,
            "UNTIL comes back as the day it was written for, not the UTC day after it"
        );
        assert_eq!(
            editable
                .draft
                .attendees
                .iter()
                .map(|guest| (guest.email.as_str(), guest.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("sam@example.com", "Sam")],
        );
    }

    #[test]
    fn a_new_all_day_event_is_a_pair_of_dates_and_its_end_is_the_day_after() {
        let berlin = "Europe/Berlin";
        let mut draft = EventDraft {
            title: "Trip".into(),
            description: String::new(),
            location: String::new(),
            start: at(date(2026, 7, 10), 0, 0, berlin),
            end: at(date(2026, 7, 13), 0, 0, berlin),
            all_day: true,
            tz: berlin.into(),
            attendees: Vec::new(),
            recurrence: None,
        };
        draft.validate().unwrap();
        let ics = new_resource_ics(&draft, "trip@everyday", &me(), now());
        let text = unfolded(&ics);
        assert!(text.contains("DTSTART;VALUE=DATE:20260710"), "{text}");
        assert!(text.contains("DTEND;VALUE=DATE:20260713"), "exclusive, the day after: {text}");
        assert!(!text.contains("ORGANIZER"), "no guests, so nobody organises it: {text}");
        assert!(!text.contains("RRULE"), "{text}");
        assert!(!text.contains("VTIMEZONE"), "a date names no zone to define: {text}");

        let events = occurrences(&ics);
        assert_eq!(events.len(), 1);
        assert!(events[0].all_day);
        assert_eq!(events[0].local_date, date(2026, 7, 10));
        assert_eq!(events[0].end_date, date(2026, 7, 12), "the last day it covers, inclusive");
    }

    #[test]
    fn editing_one_occurrence_of_a_weekly_series_adds_an_override_and_leaves_the_rule() {
        let before = occurrences(STANDUP);
        assert_eq!(before.len(), 6);
        let second = on(&before, date(2026, 10, 12));
        let editable = editable_from_ics(STANDUP, &second, &me()).unwrap();
        assert!(editable.recurring && editable.own);
        let mut draft = editable.draft;
        assert_eq!(draft.tz, NY);
        assert_eq!((draft.start, draft.end), (second.start, second.end));
        draft.title = "Standup (moved)".into();
        draft.start = at(date(2026, 10, 12), 10, 0, NY);
        draft.end = at(date(2026, 10, 12), 10, 30, NY);

        let edited =
            apply_edit(STANDUP, &second, &draft, EventScope::Occurrence, &me(), now()).unwrap();
        let text = unfolded(&edited);
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 2, "{text}");
        // Counted by its frequency, not as `RRULE:` -- the `VTIMEZONE`
        // added for New York carries yearly rules of its own.
        let rules = text.matches("RRULE:FREQ=WEEKLY").count();
        assert_eq!(rules, 1, "an override never repeats: {text}");
        assert!(text.contains("RRULE:FREQ=WEEKLY;COUNT=6"), "{text}");
        assert!(text.contains("RECURRENCE-ID;TZID=America/New_York:20261012T090000"), "{text}");
        assert!(text.contains("DTSTART;TZID=America/New_York:20261012T100000"), "{text}");
        assert!(
            text.contains("ATTENDEE;CN=Sam;PARTSTAT=ACCEPTED:mailto:sam@example.com"),
            "a guest who already answered keeps the answer: {text}"
        );
        let revised = text.matches("SEQUENCE:1").count();
        assert_eq!(revised, 2, "master and override move together: {text}");

        let after = occurrences(&edited);
        assert_eq!(
            after.len(),
            6,
            "the moved occurrence takes its own slot's place rather than adding one: {:?}",
            after.iter().map(|event| (event.start, event.title.as_str())).collect::<Vec<_>>()
        );
        assert!(!after.iter().any(|event| event.start == second.start));
        let moved = after.iter().find(|event| event.title == "Standup (moved)").unwrap().clone();
        assert_eq!(moved.start, draft.start);
        assert_eq!(after.iter().filter(|event| event.title == "Standup").count(), 5);

        // Moved again, it is the same override that changes: same slot, new
        // time, still two VEVENTs.
        let mut again = editable_from_ics(&edited, &moved, &me()).unwrap().draft;
        assert_eq!(again.title, "Standup (moved)");
        assert_eq!((again.start, again.end), (draft.start, draft.end));
        again.start = at(date(2026, 10, 12), 11, 0, NY);
        again.end = at(date(2026, 10, 12), 11, 30, NY);
        let twice =
            apply_edit(&edited, &moved, &again, EventScope::Occurrence, &me(), now()).unwrap();
        let text = unfolded(&twice);
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 2, "{text}");
        assert!(text.contains("RECURRENCE-ID;TZID=America/New_York:20261012T090000"), "{text}");
        assert!(text.contains("DTSTART;TZID=America/New_York:20261012T110000"), "{text}");
        let after = occurrences(&twice);
        assert_eq!(after.len(), 6);
        assert!(after.iter().any(|event| event.start == again.start));
    }

    #[test]
    fn a_series_edit_moves_the_masters_start_by_as_much_as_the_occurrence_moved() {
        let before = occurrences(STANDUP);
        let third = on(&before, date(2026, 10, 19));
        let mut draft = editable_from_ics(STANDUP, &third, &me()).unwrap().draft;
        assert_eq!(
            draft.recurrence,
            Some(Recurrence { count: Some(6), ..Recurrence::every(Frequency::Weekly) })
        );
        draft.start = at(date(2026, 10, 19), 9, 30, NY);
        draft.end = at(date(2026, 10, 19), 10, 0, NY);

        let edited = apply_edit(STANDUP, &third, &draft, EventScope::Series, &me(), now()).unwrap();
        let text = unfolded(&edited);
        assert!(text.contains("DTSTART;TZID=America/New_York:20261005T093000"), "{text}");
        assert!(text.contains("DTEND;TZID=America/New_York:20261005T100000"), "{text}");
        assert!(text.contains("RRULE:FREQ=WEEKLY;COUNT=6"), "{text}");
        assert!(text.contains("SEQUENCE:1"), "{text}");

        let after = occurrences(&edited);
        assert_eq!(after.len(), 6);
        let ny = jiff::tz::TimeZone::get(NY).unwrap();
        for event in &after {
            let local = event.start.to_zoned(ny.clone());
            assert_eq!(
                (local.hour(), local.minute()),
                (9, 30),
                "{} must be at half past nine on its own wall clock, either side of the change",
                event.local_date
            );
        }
    }

    #[test]
    fn deleting_one_occurrence_excludes_it_and_deleting_the_series_takes_the_resource() {
        let before = occurrences(STANDUP);
        let second = on(&before, date(2026, 10, 12));
        let Removal::Rewrite(edited) =
            removal_for(STANDUP, &second, EventScope::Occurrence, &me(), now()).unwrap()
        else {
            panic!("one occurrence of a series is an EXDATE, not a DELETE");
        };
        let text = unfolded(&edited);
        assert!(text.contains("EXDATE;TZID=America/New_York:20261012T090000"), "{text}");
        let after = occurrences(&edited);
        assert_eq!(after.len(), 5);
        assert!(!after.iter().any(|event| event.local_date == date(2026, 10, 12)));

        assert!(matches!(
            removal_for(STANDUP, &second, EventScope::Series, &me(), now()).unwrap(),
            Removal::Resource
        ));

        // An event that never repeated has no occurrence to exclude: either
        // scope deletes it. And, not repeating, it was written in UTC.
        let single = new_resource_ics(&lunch(), "lunch@everyday", &me(), now());
        assert!(unfolded(&single).contains("DTSTART:20261008T160000Z"), "{single}");
        let lunch = occurrences(&single).remove(0);
        assert!(matches!(
            removal_for(&single, &lunch, EventScope::Occurrence, &me(), now()).unwrap(),
            Removal::Resource
        ));
    }

    #[test]
    fn a_rule_recurrence_cannot_hold_loads_as_custom_and_survives_a_series_edit() {
        let first = occurrences(LAST_WEEKDAY).remove(0);
        assert_eq!(first.local_date, date(2026, 10, 30));
        let editable = editable_from_ics(LAST_WEEKDAY, &first, &me()).unwrap();
        assert!(editable.recurring);
        assert!(editable.custom_recurrence);
        assert_eq!(editable.draft.recurrence, None);
        assert_eq!(editable.draft.tz, "Europe/London");

        let mut draft = editable.draft;
        draft.title = "Month-end close".into();
        let edited =
            apply_edit(LAST_WEEKDAY, &first, &draft, EventScope::Series, &me(), now()).unwrap();
        let text = unfolded(&edited);
        assert!(text.contains("BYSETPOS=-1"), "the rule this editor cannot hold is kept: {text}");
        let after = occurrences(&edited);
        assert_eq!(after.len(), 3);
        assert!(after.iter().all(|event| event.title == "Month-end close"));
    }

    #[test]
    fn an_invitation_from_somebody_else_is_theirs_to_change() {
        let event = occurrences(INVITATION).remove(0);
        let editable = editable_from_ics(INVITATION, &event, &me()).unwrap();
        assert!(!editable.own);
        assert!(!editable.recurring);
        assert_eq!(editable.organizer, "Boss");
        assert_eq!(
            editable.draft.attendees,
            vec![Attendee {
                email: "Hari@Example.com".into(),
                name: "Hari".into(),
                response: Some(AttendeeResponse::Tentative),
            }],
            "the organiser's own ATTENDEE line is not a guest"
        );

        // A change this copy takes anyway leaves the organiser's revision
        // number, and the organiser's own line, where they were.
        let mut draft = editable.draft;
        draft.description = "Bring a laptop".into();
        let edited =
            apply_edit(INVITATION, &event, &draft, EventScope::Occurrence, &me(), now()).unwrap();
        let text = unfolded(&edited);
        assert!(text.contains("SEQUENCE:2"), "{text}");
        let organiser = "ATTENDEE;CN=Boss;PARTSTAT=ACCEPTED:mailto:boss@example.com";
        assert!(text.contains(organiser), "{text}");
        assert!(text.contains("DESCRIPTION:Bring a laptop"), "{text}");
    }
}
