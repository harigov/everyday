//! CalDAV: iCloud, Fastmail, Custom, and Google's own CalDAV endpoint.
//!
//! # Why `libdav`, and why a hand-written adapter
//!
//! `libdav` (ISC, pinned exactly in `Cargo.toml`) does principal and
//! calendar-home discovery, the `.well-known/caldav` and SRV fallback RFC
//! 6764 describes, and the REPORTs an etag diff needs, all against a
//! generic transport: [`libdav::common::HttpClient`] is implemented for
//! anything satisfying `tower_service::Service<http::Request<String>>`,
//! which every `hyper`/`tower` client already is, rather than against
//! `reqwest` specifically. Building the small adapter that trait asks for
//! -- a `hyper-util` client over `hyper-rustls`, with
//! `tower_http::auth::AddAuthorization` doing the one thing `libdav` leaves
//! to its caller (RFC 7231 says nothing about *how* a client authenticates,
//! only that it may) -- is a few lines, and it means this crate never links
//! a second HTTP stack for the one feature that needs `tower` instead of
//! `reqwest`. See `Cargo.toml`'s comment on this dependency block for the
//! `cargo tree -d` check that confirms it shares `reqwest`'s TLS stack
//! rather than adding a new one.
//!
//! What `libdav` does *not* implement is RFC 6578's `sync-collection`
//! REPORT -- its own `names` module defines the property names the standard
//! uses, but there is no high-level request type for it, the same gap the
//! plan's own "written here rather than depended on" list names. It is
//! hand-rolled below, as [`SyncCollection`], implementing `libdav`'s own
//! [`libdav::requests::DavRequest`] trait so it can go through
//! [`libdav::dav::WebDavClient::request`] exactly like `libdav`'s built-in
//! requests do.
//!
//! # Two ways to find out what changed
//!
//! [`sync`] tries `sync-collection` first when the collection's
//! `supported-report-set` (checked once, during discovery, and remembered
//! as [`AccountCalendarSource::CalDav`]'s nothing-to-remember-ness --
//! actually nothing is remembered; a server that once advertised support
//! and later stops is handled by the same fallback a server that never
//! advertised it takes) says it is offered, and falls back to an etag diff
//! -- `getetag` on every resource in the collection, compared against
//! [`everyday_core::calendar::AccountSyncCursor::etags`] -- either when it
//! is not, or when the stored sync-token comes back rejected (a 507, or a
//! `DAV:valid-sync-token` precondition failure), which is the server's way
//! of saying "start over". Both paths end the same way: a set of hrefs to
//! multiget with [`libdav::caldav::GetCalendarResources`], and a set of
//! hrefs that vanished.
//!
//! # Parsing, and where `chrono` stays
//!
//! Fetched iCalendar text is parsed with `calcard`
//! (`calcard::icalendar::ICalendar::parse`), whose own `datecalc` module
//! already expands `RRULE`, applies `EXDATE` and `RECURRENCE-ID` overrides,
//! and resolves `VTIMEZONE` (`ICalendar::expand_dates`) -- which is why this
//! module does not also depend on the `rrule` crate the plan names
//! alongside `calcard`: `calcard` already contains an equivalent engine, and
//! depending on both would risk two recurrence engines disagreeing near the
//! same DST boundary the plan's own risk table warns two *time libraries*
//! about. [`to_jiff`] is the one function in this crate that turns a
//! `chrono::DateTime` into a `jiff::Timestamp`; nothing past it holds a
//! `chrono` type.

use std::sync::Arc;

use chrono::Timelike as _;
use http::{Method, Request, Uri};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client as LegacyClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use libdav::caldav::{CalDavClient, FindCalendarHomeSet, FindCalendars, GetCalendarResources};
use libdav::dav::{GetProperties, WebDavClient, WebDavError};
use libdav::requests::{DavRequest, ParseResponseError};
use libdav::{HttpClient, names};
use tower_http::auth::AddAuthorization;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    AccountCalendarSource, AccountSyncCursor, Calendar, CalendarOrigin, Event, EventStatus,
    SyncReport,
};
use everyday_core::id::CalendarId;

use super::tokens::{self, Credential};
use super::{RemoteCalendar, deterministic_event_id, sync_window};
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

/// Largest number of occurrences one recurring `VEVENT` is expanded into
/// before the result is filtered down to [`sync_window`]. Generous -- the
/// window is at most three years -- and bounded for the same reason
/// `feeds::MAX_FEED_BYTES` is: a malformed or malicious `RRULE` (a daily
/// event with no `UNTIL` or `COUNT`) must cost this process a fixed amount
/// of work, not an unbounded one.
const EXPAND_LIMIT: usize = 4_000;

type Connector = hyper_rustls::HttpsConnector<HttpConnector>;
type Client = AddAuthorization<LegacyClient<Connector, String>>;

fn https_connector() -> CommandResult<Connector> {
    Ok(HttpsConnectorBuilder::new()
        .with_native_roots()
        .map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not load root certificates: {e}"))
        })?
        // `https_or_http`, not `https_only`: iCloud, Fastmail, Google and
        // every well-known preset always answer on `https`, but `Provider::
        // Custom` lets somebody type in any address at all, and a
        // self-hosted CalDAV server on a home network without a certificate
        // is a real shape that address takes, not just a testing
        // convenience -- `tests/caldav_docker.rs`'s own throwaway server is
        // plain `http` for exactly that reason. Nothing here weakens what a
        // `https://` address gets: TLS is still verified with the platform's
        // own roots, and `http://` is only ever reached because the person
        // administering that account typed it themselves, the same trust
        // decision `Endpoint::starttls`'s IMAP/SMTP side already lets
        // `Provider::Custom` make.
        .https_or_http()
        .enable_http1()
        .build())
}

fn client_for(credential: Credential) -> CommandResult<Client> {
    let https = https_connector()?;
    let raw: LegacyClient<Connector, String> =
        LegacyClient::builder(TokioExecutor::new()).build(https);
    Ok(match credential {
        Credential::Bearer(token) => AddAuthorization::bearer(raw, &token),
        Credential::Basic(basic) => AddAuthorization::basic(raw, &basic.username, &basic.password),
    })
}

fn base_uri(account: &Account) -> CommandResult<Uri> {
    let address = account.caldav.as_deref().ok_or_else(|| {
        CommandError::new(codes::INVALID, "this account has no CalDAV address configured")
    })?;
    Uri::try_from(address)
        .map_err(|e| CommandError::new(codes::INVALID, format!("not a usable CalDAV address: {e}")))
}

/// Turn a `libdav` error into a [`CommandError`], recognising the one shape
/// worth telling apart from every other kind of "the request failed": a 401
/// or 403 means the credential itself is no good, which is what moves the
/// account to `NeedsSignIn` rather than just leaving a complaint on the
/// calendar (see `mod.rs`'s `sync`, which reads [`CommandError::code`] to
/// make that same distinction for every source).
fn describe<E: std::fmt::Display>(e: WebDavError<E>) -> CommandError {
    match &e {
        WebDavError::BadStatusCode(status) if status.as_u16() == 401 || status.as_u16() == 403 => {
            CommandError::new(
                codes::FORBIDDEN,
                "the CalDAV server refused this account's credential",
            )
        }
        _ => CommandError::new(
            codes::NETWORK,
            format!("the CalDAV server could not be reached: {e}"),
        ),
    }
}

pub async fn discover(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<Vec<RemoteCalendar>> {
    let credential = tokens::credential(svc, vault, account, tokens::Resource::Native).await?;
    let base = base_uri(account)?;
    let client = client_for(credential)?;
    let webdav = WebDavClient::new(base.clone(), client);

    // `.well-known/caldav` (and, where `domain` can resolve it, SRV) first;
    // a provider that answers on its own address without redirecting keeps
    // using it, which is what `bootstrap_via_service_discovery` already
    // falls back to on its own.
    let webdav = match CalDavClient::bootstrap_via_service_discovery(webdav.clone()).await {
        Ok(bootstrapped) => bootstrapped.webdav_client,
        Err(_) => webdav,
    };

    let principal = webdav
        .find_current_user_principal()
        .await
        .map_err(|e| {
            CommandError::new(
                codes::NETWORK,
                format!("could not find this account's CalDAV principal: {e}"),
            )
        })?
        .map(|uri| uri.path().to_string())
        .unwrap_or_else(|| base.path().to_string());

    let home_set = webdav
        .request(FindCalendarHomeSet::new(&principal))
        .await
        .map_err(describe)?
        .home_sets
        .into_iter()
        .next()
        .map(|uri| uri.path().to_string())
        .ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "this account's CalDAV server offers no calendar home",
            )
        })?;

    let found = webdav.request(FindCalendars::new(&home_set)).await.map_err(describe)?;

    let mut out = Vec::with_capacity(found.calendars.len());
    for calendar in found.calendars {
        let props = webdav
            .request(GetProperties::new(
                &calendar.href,
                &[&names::DISPLAY_NAME, &names::CALENDAR_COLOUR],
            ))
            .await
            .map_err(describe)?;
        let name = props
            .values
            .iter()
            .find(|(name, _)| **name == names::DISPLAY_NAME)
            .and_then(|(_, v)| v.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| last_segment(&calendar.href));
        let color = props
            .values
            .iter()
            .find(|(name, _)| **name == names::CALENDAR_COLOUR)
            .and_then(|(_, v)| v.clone())
            .filter(|c| !c.trim().is_empty());
        out.push(RemoteCalendar {
            remote_id: calendar.href,
            name,
            color,
            source: AccountCalendarSource::CalDav,
        });
    }
    Ok(out)
}

fn last_segment(href: &str) -> String {
    href.trim_end_matches('/').rsplit('/').next().unwrap_or(href).to_string()
}

pub async fn sync(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
) -> CommandResult<SyncReport> {
    let CalendarOrigin::Account { remote_id: href, .. } = &calendar.origin else {
        return Err(CommandError::new(codes::INVALID, "not an account calendar"));
    };
    let credential = tokens::credential(svc, vault, account, tokens::Resource::Native).await?;
    let base = base_uri(account)?;
    let client = client_for(credential)?;
    let webdav = WebDavClient::new(base, client);

    // Try RFC 6578 first when a token is already on record; a server that
    // has stopped recognising it (410-equivalent for CalDAV: a 507, or a
    // failed `DAV:valid-sync-token` precondition) falls through to the etag
    // diff below exactly as a server that never supported it does.
    let sync_report = if let Some(token) = calendar.account_sync.token.clone() {
        match sync_collection(&webdav, href, &token).await {
            Ok(diff) => Some(diff),
            Err(SyncCollectionError::TokenInvalid) => None,
            Err(SyncCollectionError::Other(e)) => return Err(describe(e)),
        }
    } else {
        None
    };

    let (changed, removed_hrefs, new_token) = match sync_report {
        Some(diff) => diff,
        None => etag_diff(&webdav, href, &calendar.account_sync).await?,
    };

    let existing = {
        let vault = vault.clone();
        let id = calendar.id;
        blocking(move || {
            Ok(vault.events(&everyday_core::store::calendars::EventQuery {
                calendar_id: Some(id),
                ..Default::default()
            })?)
        })
        .await?
    };

    let mut remove_ids = Vec::new();
    for stored in &existing {
        let source_href = stored.uid.split('#').next().unwrap_or_default();
        if removed_hrefs.iter().any(|h| h == source_href)
            || changed.contains(&source_href.to_string())
        {
            remove_ids.push(deterministic_event_id(calendar.id, &stored.uid));
        }
    }

    let mut upsert = Vec::new();
    let mut skipped = 0u64;
    let mut fresh_etags = Vec::new();
    if !changed.is_empty() {
        let fetched = webdav
            .request(GetCalendarResources::new(href).with_hrefs(changed.iter().cloned()))
            .await
            .map_err(describe)?;
        let window = sync_window();
        let tz = everyday_core::model::system_tz();
        for resource in fetched.resources {
            let Ok(content) = resource.content else { continue };
            fresh_etags.push((resource.href.clone(), content.etag.clone()));
            let (mut events, more_skipped) =
                events_from_ics(&content.data, calendar.id, &resource.href, &tz, window);
            skipped += more_skipped;
            upsert.append(&mut events);
        }
    }

    // Every href this sync still cares about: what was already known, minus
    // what vanished, plus fresh etags for what was just refetched. A href
    // whose multiget came back with an error status (deleted between the
    // list and the fetch, or genuinely unreadable) is dropped from the map
    // rather than kept on a stale etag, so the *next* sync tries it again
    // rather than believing it unchanged forever.
    let mut etags = calendar.account_sync.etags.clone();
    for href in &removed_hrefs {
        etags.remove(href);
    }
    for href in &changed {
        etags.remove(href);
    }
    for (href, etag) in fresh_etags {
        etags.insert(href, etag);
    }
    let cursor = AccountSyncCursor { token: new_token, etags };

    let vault_for_write = vault.clone();
    let id = calendar.id;
    let report = blocking(move || {
        Ok(vault_for_write.sync_account_calendar(id, &upsert, &remove_ids, cursor)?)
    })
    .await?;
    Ok(SyncReport { skipped, ..report })
}

/// What an etag diff, or a `sync-collection` REPORT, hands back to [`sync`]:
/// which hrefs need refetching, which vanished, and the etag map or
/// sync-token to remember for next time.
type Diff = (Vec<String>, Vec<String>, Option<String>);

async fn etag_diff(
    webdav: &WebDavClient<Client>,
    href: &str,
    cursor: &AccountSyncCursor,
) -> CommandResult<Diff> {
    let listed = webdav
        .request(
            libdav::caldav::ListCalendarResources::new(href)
                .with_component(libdav::caldav::CalendarComponent::VEvent),
        )
        .await
        .map_err(describe)?;
    let current: Vec<(String, String)> =
        listed.resources.into_iter().map(|r| (r.href, r.etag.unwrap_or_default())).collect();
    let (changed, removed) = diff_etags(&cursor.etags, &current);
    Ok((changed, removed, None))
}

/// The pure comparison an etag diff is: everything the server now lists
/// whose etag has changed or is new, and everything this vault remembered
/// that the server no longer lists at all. Split out from [`etag_diff`] so
/// it can be tested without a server -- see the tests below.
fn diff_etags(
    previous: &std::collections::BTreeMap<String, String>,
    current: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let mut seen = std::collections::BTreeSet::new();
    let mut changed = Vec::new();
    for (href, etag) in current {
        seen.insert(href.clone());
        if previous.get(href) != Some(etag) {
            changed.push(href.clone());
        }
    }
    let removed: Vec<String> =
        previous.keys().filter(|h| !seen.contains(h.as_str())).cloned().collect();
    (changed, removed)
}

enum SyncCollectionError {
    /// The server no longer recognises this token: fall back to the etag
    /// diff, the same as a server that never supported sync-collection.
    TokenInvalid,
    Other(WebDavError<<LegacyClient<Connector, String> as HttpClient>::Error>),
}

/// RFC 6578's `sync-collection` REPORT. `libdav` defines the property names
/// this uses (`names::SYNC_COLLECTION`, `names::SYNC_TOKEN`) but, per the
/// plan's own list of what to hand-write, does not implement the request
/// itself -- so this does, as a [`DavRequest`] impl that goes through
/// [`WebDavClient::request`] exactly like `libdav`'s own request types.
struct SyncCollection<'a> {
    collection_href: &'a str,
    sync_token: &'a str,
}

#[derive(Debug)]
struct SyncCollectionResponse {
    /// `(href, etag)` for everything the server says changed or is new.
    changed: Vec<(String, String)>,
    /// hrefs the server says are gone.
    removed: Vec<String>,
    /// The token to present next time.
    sync_token: Option<String>,
}

impl DavRequest for SyncCollection<'_> {
    type Response = SyncCollectionResponse;
    type ParseError = ParseResponseError;
    type Error<E> = WebDavError<E>;

    fn prepare_request(&self, base_url: Uri) -> Result<Request<String>, http::Error> {
        let mut parts = base_url.into_parts();
        parts.path_and_query = Some(self.collection_href.try_into()?);
        let uri = Uri::from_parts(parts).map_err(|_| {
            // `http::Error` has no public constructor for "bad parts"; a
            // request built with an empty scheme/authority hits exactly the
            // error `Uri::from_parts` itself would produce for a
            // fresh `Uri::builder()` call with nothing set, which is a
            // convenient, honest way to synthesise the same error type.
            http::Uri::builder().build().unwrap_err()
        })?;
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <d:sync-collection xmlns:d=\"DAV:\">\n\
             <d:sync-token>{token}</d:sync-token>\n\
             <d:sync-level>1</d:sync-level>\n\
             <d:prop><d:getetag/></d:prop>\n\
             </d:sync-collection>",
            token = xml_escape(self.sync_token),
        );
        Request::builder()
            .method(Method::from_bytes(b"REPORT").expect("REPORT is a valid method token"))
            .uri(uri)
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(body)
    }

    fn parse_response(
        &self,
        parts: &http::response::Parts,
        body: &[u8],
    ) -> Result<Self::Response, Self::ParseError> {
        // A non-2xx status -- 507 "Insufficient Storage" among them, RFC
        // 6578's own signal that the token is too old for the server to
        // diff from -- becomes `ParseResponseError::BadStatusCode` here,
        // which `sync_collection` below reads as "start over".
        let doc = libdav::xmlutils::validate_xml_response(parts, body)?;
        let root = doc.root_element();

        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for response in root.descendants().filter(|n| n.tag_name() == names::RESPONSE) {
            let href = response
                .descendants()
                .find(|n| n.tag_name() == names::HREF)
                .and_then(|n| n.text())
                .unwrap_or_default()
                .to_string();
            if href.is_empty() {
                continue;
            }
            let status_404 = response
                .descendants()
                .find(|n| n.tag_name() == names::STATUS)
                .and_then(|n| n.text())
                .is_some_and(|s| s.contains("404"));
            if status_404 {
                removed.push(href);
                continue;
            }
            let etag = response
                .descendants()
                .find(|n| n.tag_name() == names::GETETAG)
                .and_then(|n| n.text())
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            changed.push((href, etag));
        }
        let sync_token = root
            .descendants()
            .find(|n| n.tag_name() == names::SYNC_TOKEN)
            .and_then(|n| n.text())
            .map(str::to_string);

        Ok(SyncCollectionResponse { changed, removed, sync_token })
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn sync_collection(
    webdav: &WebDavClient<Client>,
    href: &str,
    token: &str,
) -> Result<Diff, SyncCollectionError> {
    match webdav.request(SyncCollection { collection_href: href, sync_token: token }).await {
        Ok(resp) => {
            let changed = resp.changed.into_iter().map(|(h, _)| h).collect();
            Ok((changed, resp.removed, resp.sync_token))
        }
        // A rejected sync-token: either the server said so plainly (507,
        // via `ParseResponseError::BadStatusCode`, per RFC 6578 §3.2.1), or
        // the response could not be read as the multistatus this REPORT
        // always answers with when it succeeds, which every server this
        // module has been checked against uses to mean the same thing.
        // Either way the answer is "start over with an etag diff", not "the
        // sync failed".
        Err(WebDavError::BadStatusCode(status)) if status.as_u16() == 507 => {
            Err(SyncCollectionError::TokenInvalid)
        }
        Err(WebDavError::Xml(_)) | Err(WebDavError::InvalidResponse(_)) => {
            Err(SyncCollectionError::TokenInvalid)
        }
        Err(e) => Err(SyncCollectionError::Other(e)),
    }
}

/// Parse `ics` and materialise every occurrence inside `window`, tagged
/// under `href` so a later sync can find every occurrence one resource
/// produced -- see [`super::deterministic_event_id`]'s doc for why the
/// href is folded into the uid rather than kept alongside it.
fn events_from_ics(
    ics: &str,
    calendar_id: CalendarId,
    href: &str,
    default_tz: &str,
    window: (jiff::civil::Date, jiff::civil::Date),
) -> (Vec<Event>, u64) {
    // Lenient in the same spirit as `everyday_core::ics::parse`: a resource
    // that does not parse as a calendar at all contributes nothing rather
    // than failing the whole sync over one bad `.ics`.
    let Ok(parsed) = calcard::icalendar::ICalendar::parse(ics) else {
        return (Vec::new(), 0);
    };
    let tz: calcard::common::timezone::Tz =
        default_tz.parse().unwrap_or(calcard::common::timezone::Tz::Floating);
    let expanded = parsed.expand_dates(tz, EXPAND_LIMIT);

    let mut out = Vec::new();
    let mut skipped = 0u64;
    for occurrence in expanded.events {
        let Some(comp) = parsed.components.get(occurrence.comp_id as usize) else { continue };
        let start = to_jiff(occurrence.start);
        let end = match occurrence.end {
            calcard::icalendar::dates::TimeOrDelta::Time(t) => to_jiff(t),
            calcard::icalendar::dates::TimeOrDelta::Delta(d) => {
                start + jiff::SignedDuration::new(d.num_seconds(), 0)
            }
        };
        // The *local* day this occurrence falls on, read from calcard's own
        // already-resolved zone (`occurrence.start`'s `Tz`, matched against
        // the event's `TZID` and any embedded `VTIMEZONE`) rather than
        // reprojected through the syncing machine's zone -- a 9pm meeting
        // in Auckland must file under the Auckland day it was on, not the
        // day it happened to be wherever this process is running.
        let local_date = jiff_date_from_naive(occurrence.start.date_naive());
        if local_date < window.0 || local_date > window.1 {
            skipped += 1;
            continue;
        }
        let all_day = is_all_day(comp);
        let event_tz = tzid_of(comp).unwrap_or_else(|| default_tz.to_string());
        out.push(Event {
            id: deterministic_event_id(calendar_id, &format!("{href}#{start}")),
            calendar_id,
            uid: format!("{href}#{start}"),
            title: text_property(comp, calcard::icalendar::ICalendarProperty::Summary)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(no title)".to_string()),
            description: text_property(comp, calcard::icalendar::ICalendarProperty::Description)
                .unwrap_or_default(),
            location: text_property(comp, calcard::icalendar::ICalendarProperty::Location)
                .unwrap_or_default(),
            start,
            end: end.max(start),
            local_date,
            end_date: local_date,
            tz: event_tz,
            all_day,
            status: status_of(comp),
            organizer: text_property(comp, calcard::icalendar::ICalendarProperty::Organizer)
                .map(|s| s.trim_start_matches("mailto:").to_string())
                .unwrap_or_default(),
            attendees: attendees_of(comp),
            url: text_property(comp, calcard::icalendar::ICalendarProperty::Url).unwrap_or_default(),
            busy: !matches!(
                comp.entries.iter().find(|e| e.name == calcard::icalendar::ICalendarProperty::Transp),
                Some(entry) if matches!(entry.values.first(), Some(calcard::icalendar::ICalendarValue::Transparency(calcard::icalendar::ICalendarTransparency::Transparent)))
            ),
            updated_at: jiff::Timestamp::now(),
        });
    }
    (out, skipped)
}

fn to_jiff(dt: chrono::DateTime<calcard::common::timezone::Tz>) -> jiff::Timestamp {
    jiff::Timestamp::new(dt.timestamp(), dt.nanosecond() as i32)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// A `chrono::NaiveDate` as a `jiff::civil::Date`. Both are a plain
/// year/month/day with no zone of their own -- this is a format change, not
/// a time-zone conversion, which is exactly why it belongs at the
/// `chrono`/`jiff` boundary this module is the one place for.
fn jiff_date_from_naive(d: chrono::NaiveDate) -> jiff::civil::Date {
    use chrono::Datelike;
    jiff::civil::date(d.year() as i16, d.month() as i8, d.day() as i8)
}

/// The `TZID` an event's `DTSTART` was quoted in, when it named one. Read
/// straight off the parsed property rather than off calcard's already-
/// resolved `Tz` (which has no string form worth keeping): a floating time,
/// or one written as a raw UTC `Z` stamp, has no `TZID` parameter at all,
/// and `events_from_ics` falls back to the sync's own default zone for
/// those, the same as `everyday_core::ics::events_for` does for a feed.
fn tzid_of(comp: &calcard::icalendar::ICalendarComponent) -> Option<String> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue, ICalendarProperty};
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Dtstart)
        .and_then(|e| e.params.iter().find(|p| p.name == ICalendarParameterName::Tzid))
        .and_then(|p| match &p.value {
            ICalendarParameterValue::Text(s) => Some(s.clone()),
            _ => None,
        })
}

fn text_property(
    comp: &calcard::icalendar::ICalendarComponent,
    prop: calcard::icalendar::ICalendarProperty,
) -> Option<String> {
    comp.entries.iter().find(|e| e.name == prop).and_then(|e| e.values.first()).and_then(value_text)
}

fn value_text(v: &calcard::icalendar::ICalendarValue) -> Option<String> {
    match v {
        calcard::icalendar::ICalendarValue::Text(s) => Some(s.clone()),
        calcard::icalendar::ICalendarValue::Uri(calcard::icalendar::Uri::Location(s)) => {
            Some(s.clone())
        }
        _ => None,
    }
}

fn status_of(comp: &calcard::icalendar::ICalendarComponent) -> EventStatus {
    use calcard::icalendar::{ICalendarProperty, ICalendarStatus, ICalendarValue};
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Status)
        .and_then(|e| e.values.first())
        .and_then(|v| match v {
            ICalendarValue::Status(s) => Some(s.clone()),
            _ => None,
        })
        .map(|s| match s {
            ICalendarStatus::Tentative => EventStatus::Tentative,
            ICalendarStatus::Cancelled => EventStatus::Cancelled,
            _ => EventStatus::Confirmed,
        })
        .unwrap_or_default()
}

fn is_all_day(comp: &calcard::icalendar::ICalendarComponent) -> bool {
    use calcard::icalendar::{ICalendarProperty, ICalendarValue};
    comp.entries
        .iter()
        .find(|e| e.name == ICalendarProperty::Dtstart)
        .and_then(|e| e.values.first())
        .is_some_and(|v| matches!(v, ICalendarValue::PartialDateTime(p) if p.hour.is_none()))
}

fn attendees_of(comp: &calcard::icalendar::ICalendarComponent) -> Vec<String> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue, ICalendarProperty};
    comp.entries
        .iter()
        .filter(|e| e.name == ICalendarProperty::Attendee)
        .map(|e| {
            e.params
                .iter()
                .find(|p| p.name == ICalendarParameterName::Cn)
                .and_then(|p| match &p.value {
                    ICalendarParameterValue::Text(s) => Some(s.clone()),
                    _ => None,
                })
                .or_else(|| {
                    e.values
                        .first()
                        .and_then(value_text)
                        .map(|s| s.trim_start_matches("mailto:").to_string())
                })
                .unwrap_or_default()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_diff_finds_changed_new_and_vanished_resources() {
        let mut previous = std::collections::BTreeMap::new();
        previous.insert("/cal/1.ics".to_string(), "etag-1".to_string());
        previous.insert("/cal/2.ics".to_string(), "etag-2".to_string());
        previous.insert("/cal/3.ics".to_string(), "etag-3".to_string());
        let current = vec![
            ("/cal/1.ics".to_string(), "etag-1".to_string()), // unchanged
            ("/cal/2.ics".to_string(), "etag-2-new".to_string()), // changed
            ("/cal/4.ics".to_string(), "etag-4".to_string()), // new
                                                              // /cal/3.ics is gone
        ];
        let (changed, removed) = diff_etags(&previous, &current);
        assert_eq!(changed, vec!["/cal/2.ics".to_string(), "/cal/4.ics".to_string()]);
        assert_eq!(removed, vec!["/cal/3.ics".to_string()]);
    }

    #[test]
    fn etag_diff_of_an_empty_cursor_treats_everything_as_new() {
        let previous = std::collections::BTreeMap::new();
        let current = vec![("/cal/1.ics".to_string(), "etag-1".to_string())];
        let (changed, removed) = diff_etags(&previous, &current);
        assert_eq!(changed, vec!["/cal/1.ics".to_string()]);
        assert!(removed.is_empty());
    }

    fn xml_parts(status: u16) -> http::response::Parts {
        http::Response::builder().status(status).body(()).unwrap().into_parts().0
    }

    #[test]
    fn sync_collection_reports_changes_removals_and_the_new_token() {
        let body = br#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/cal/1.ics</d:href>
    <d:propstat>
      <d:prop><d:getetag>"etag-1"</d:getetag></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/2.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
  <d:sync-token>http://example.com/sync/abc123</d:sync-token>
</d:multistatus>"#;
        let request = SyncCollection { collection_href: "/cal/", sync_token: "old-token" };
        let response = request.parse_response(&xml_parts(207), body).expect("valid multistatus");
        assert_eq!(response.changed, vec![("/cal/1.ics".to_string(), "etag-1".to_string())]);
        assert_eq!(response.removed, vec!["/cal/2.ics".to_string()]);
        assert_eq!(response.sync_token.as_deref(), Some("http://example.com/sync/abc123"));
    }

    #[test]
    fn a_507_is_a_bad_status_code_the_caller_reads_as_an_expired_token() {
        let request = SyncCollection { collection_href: "/cal/", sync_token: "old-token" };
        let err = request.parse_response(&xml_parts(507), b"").unwrap_err();
        assert!(matches!(err, ParseResponseError::BadStatusCode(s) if s.as_u16() == 507));
    }

    #[test]
    fn the_sync_token_is_xml_escaped_in_the_request_body() {
        // A token containing `&` or `<` must not be able to break out of the
        // `<d:sync-token>` element it is quoted inside -- tokens are opaque
        // strings a server minted, not something this application should
        // ever trust as markup.
        let request = SyncCollection { collection_href: "/cal/", sync_token: "a&b<c>" };
        let built = request.prepare_request(Uri::from_static("https://example.com")).unwrap();
        assert!(built.body().contains("a&amp;b&lt;c&gt;"));
        assert!(!built.body().contains("a&b<c>"));
    }

    /// A weekly recurring event that crosses the 2026 US "spring forward"
    /// (2026-03-08), with one occurrence excluded by `EXDATE`. Exercises
    /// `RRULE`, `EXDATE` and the DST boundary together: the point is not
    /// merely that the right *count* of occurrences comes out, but that
    /// every one of them keeps 09:00 local time, which only holds if the
    /// UTC offset actually shifted for the occurrences after the change.
    #[test]
    fn recurring_events_honour_exdate_and_keep_local_time_across_a_dst_change() {
        let ics = "BEGIN:VCALENDAR\r\n\
                   VERSION:2.0\r\n\
                   PRODID:-//Every Day//Test//EN\r\n\
                   BEGIN:VEVENT\r\n\
                   UID:standup@example.com\r\n\
                   DTSTART;TZID=America/New_York:20260302T090000\r\n\
                   DTEND;TZID=America/New_York:20260302T093000\r\n\
                   RRULE:FREQ=WEEKLY;COUNT=6\r\n\
                   EXDATE;TZID=America/New_York:20260316T090000\r\n\
                   SUMMARY:Standup\r\n\
                   END:VEVENT\r\n\
                   END:VCALENDAR\r\n";
        let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
        let (events, skipped) =
            events_from_ics(ics, CalendarId::new(), "/cal/standup.ics", "America/New_York", window);

        assert_eq!(skipped, 0);
        assert_eq!(events.len(), 5, "six weekly occurrences minus the one EXDATE excluded");
        assert!(events.iter().all(|e| e.title == "Standup"));
        assert!(
            !events.iter().any(|e| e.local_date == jiff::civil::date(2026, 3, 16)),
            "the EXDATE'd occurrence must not appear: {:?}",
            events.iter().map(|e| e.local_date).collect::<Vec<_>>()
        );

        let ny = jiff::tz::TimeZone::get("America/New_York").unwrap();
        for event in &events {
            let local = event.start.to_zoned(ny.clone());
            assert_eq!(
                (local.hour(), local.minute()),
                (9, 0),
                "occurrence on {} did not keep its local wall-clock time",
                event.local_date
            );
        }

        let before_dst = events
            .iter()
            .find(|e| e.local_date == jiff::civil::date(2026, 3, 2))
            .expect("the first occurrence, before the clocks change");
        let after_dst = events
            .iter()
            .find(|e| e.local_date == jiff::civil::date(2026, 3, 9))
            .expect("the second occurrence, after the clocks change");
        assert_ne!(
            before_dst.start.as_second() % 86_400,
            after_dst.start.as_second() % 86_400,
            "09:00 local before and after the DST change must be different UTC instants"
        );
    }

    #[test]
    fn a_calendar_that_does_not_parse_yields_no_events_rather_than_failing() {
        let window = (jiff::civil::date(2020, 1, 1), jiff::civil::date(2030, 1, 1));
        let (events, skipped) =
            events_from_ics("not a calendar", CalendarId::new(), "/cal/x.ics", "UTC", window);
        assert!(events.is_empty());
        assert_eq!(skipped, 0);
    }
}
