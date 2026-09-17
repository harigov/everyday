//! Google Calendar, read over its own REST API rather than CalDAV.
//!
//! Google does publish a CalDAV endpoint (`caldav.rs` can reach it -- see
//! that module's doc on which providers share the one adapter), but its
//! principal and calendar-home discovery is unusually brittle in practice,
//! and the API this module uses instead needs none of that: `calendarList`
//! already enumerates every calendar the account can see, and
//! `events.list` with `singleEvents=true` hands back every occurrence of a
//! recurring event *already expanded*, in the account's own time zone, with
//! no `RRULE` or `VTIMEZONE` for this application to interpret at all. That
//! is the whole of why the API is the default for a Google account: less
//! code, run against a stable, documented surface, for a job CalDAV would
//! need `calcard`'s recurrence engine for anyway.
//!
//! `syncToken` (RFC-less, Google's own convention, but the same idea as
//! CalDAV's `sync-token`) is what makes a resync incremental; a `410 Gone`
//! on it means "start over", handled by [`sync`] falling back to a bounded
//! `timeMin`/`timeMax` list the same width as [`super::sync_window`].
//!
//! # A full resync has to compute its own deletions
//!
//! Google only ever reports a cancelled event (`status: "cancelled"`)
//! inside an *incremental* page -- one fetched with `syncToken`. A plain,
//! windowed `timeMin`/`timeMax` list, which is what both a first sync and
//! the 410 fallback fall back to, never sets `showDeleted` and so never
//! mentions a cancelled event at all: it simply is not in the list. Reading
//! that silence as "nothing was deleted" is the bug this module used to
//! have -- an event deleted while this vault's `syncToken` was stale would
//! stay in the local store forever, because nothing ever told it to leave.
//! [`sync`] closes that gap with [`super::missing_from_full_resync`]: after
//! any windowed list, whatever this vault already had for the calendar
//! inside that window that the fresh list did not re-mention is gone.
//!
//! # 403 is not always a bad credential
//!
//! Google spends the same status, 403, on two very different situations --
//! see <https://developers.google.com/calendar/api/guides/errors>.
//! `rateLimitExceeded`, `userRateLimitExceeded` and `quotaExceeded` mean
//! "you are asking too fast", the same as a 429 everywhere else, and
//! [`get_bytes`] retries those with a short backoff, honouring `Retry-After`
//! when Google sends one, before giving up and asking the next scheduled
//! poll to try again. Every other 403 reason (`accessNotConfigured`,
//! `insufficientPermissions`, and the rest) means this account can reach
//! Google fine but the calendar itself refused the request -- not a
//! credential problem, so it must not move the account to `NeedsSignIn` the
//! way a 401 does. Only a 401 -- the token itself rejected -- is
//! [`codes::FORBIDDEN`] here; everything else a 403 can mean is
//! [`codes::NETWORK`] or [`codes::RATE_LIMITED`], both of which `mod.rs`'s
//! `sync` reads as an ordinary failure to record on the calendar, leaving
//! the account alone.

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    AccountCalendarSource, AccountSyncCursor, Calendar, CalendarOrigin, Event, EventStatus,
    SyncReport,
};
use everyday_core::id::CalendarId;
use serde::Deserialize;
use tokio::time::sleep;

use super::tokens::{self, Credential, Resource};
use super::{
    RemoteCalendar, calendar_after_retry_after, deterministic_event_id, retry_after_delay,
    sync_window,
};
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::service::{Service, blocking};

const API: &str = "https://www.googleapis.com/calendar/v3";

#[derive(Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarListItem>,
}

#[derive(Deserialize)]
struct CalendarListItem {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    #[serde(rename = "summaryOverride")]
    summary_override: Option<String>,
    #[serde(default)]
    #[serde(rename = "backgroundColor")]
    background_color: Option<String>,
}

pub async fn discover(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<Vec<RemoteCalendar>> {
    let token = bearer(svc, vault, account).await?;
    let url = format!("{API}/users/me/calendarList");
    let body: CalendarListResponse = get_json(&url, &token).await?;
    Ok(body
        .items
        .into_iter()
        .map(|item| RemoteCalendar {
            remote_id: item.id,
            name: item.summary_override.filter(|s| !s.is_empty()).unwrap_or(item.summary),
            color: item.background_color,
            source: AccountCalendarSource::Google,
        })
        .collect())
}

#[derive(Deserialize)]
struct EventsResponse {
    #[serde(default)]
    items: Vec<GoogleEvent>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "nextSyncToken")]
    next_sync_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleEvent {
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
    #[serde(default)]
    transparency: String,
    organizer: Option<GoogleAttendee>,
    #[serde(default)]
    attendees: Vec<GoogleAttendee>,
    #[serde(rename = "htmlLink")]
    #[serde(default)]
    html_link: String,
    /// Set on every instance of a recurring event, to the id of the
    /// recurring event they all expand from -- stable across occurrences,
    /// unlike `id` itself, which `singleEvents=true` mints fresh per
    /// instance. Absent on a one-off event. See [`Event::series`]
    /// (`everyday_core::calendar`) and `detect::series_key`'s own doc for
    /// why "never for this meeting" needs this rather than `id`.
    #[serde(rename = "recurringEventId")]
    recurring_event_id: Option<String>,
    /// The iCalendar UID, present on every event, recurring or not, and
    /// also shared by every occurrence of a recurring one -- the fallback
    /// when `recurringEventId` is absent (a one-off event has no series to
    /// share a key with, so falling back to this is harmless: it is just
    /// that event's own durable id).
    #[serde(rename = "iCalUID")]
    i_cal_uid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleWhen {
    date: Option<String>,
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
    #[serde(rename = "timeZone")]
    time_zone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleAttendee {
    #[serde(default)]
    email: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

impl GoogleAttendee {
    fn label(&self) -> String {
        self.display_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| self.email.clone())
    }
}

pub async fn sync(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
) -> CommandResult<SyncReport> {
    sync_with_base(svc, vault, account, calendar, API).await
}

/// [`sync`]'s own body, over `base` rather than the hardcoded [`API`] --
/// split out so a test can point the whole conversation (events, and the
/// diff a full resync now has to compute) at a mock server, the same way
/// [`list_events`] already lets its own tests choose `base`.
async fn sync_with_base(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
    base: &str,
) -> CommandResult<SyncReport> {
    let CalendarOrigin::Account { remote_id, .. } = &calendar.origin else {
        return Err(CommandError::new(codes::INVALID, "not an account calendar"));
    };
    let token = bearer(svc, vault, account).await?;
    let encoded = urlencoding_light(remote_id);

    let (events, next_token, full_resync_window) = match &calendar.account_sync.token {
        Some(sync_token) => {
            match list_events(base, &encoded, &token, IncrementalOrFull::Incremental(sync_token))
                .await
            {
                Ok(pages) => (pages.0, pages.1, None),
                Err(e) if e.code == codes::CONFLICT => {
                    // Google's 410 Gone: the token is too old. Start over with a
                    // bounded window, same as a first sync.
                    let window = sync_window();
                    let pages = list_events(
                        base,
                        &encoded,
                        &token,
                        IncrementalOrFull::Windowed(window.0, window.1),
                    )
                    .await?;
                    (pages.0, pages.1, Some(window))
                }
                Err(e) => return Err(e),
            }
        }
        None => {
            let window = sync_window();
            let pages = list_events(
                base,
                &encoded,
                &token,
                IncrementalOrFull::Windowed(window.0, window.1),
            )
            .await?;
            (pages.0, pages.1, Some(window))
        }
    };

    let mut upsert = Vec::new();
    let mut remove_ids = Vec::new();
    let default_tz = everyday_core::model::system_tz();
    for item in events {
        let id = deterministic_event_id(calendar.id, &item.id);
        if item.status == "cancelled" {
            remove_ids.push(id);
            continue;
        }
        if let Some(event) = to_event(calendar.id, &item, &default_tz, svc.now()) {
            upsert.push(event);
        }
    }

    // A full, windowed list never mentions a cancelled event at all -- see
    // the module doc's "A full resync has to compute its own deletions".
    // Whatever this vault already had in the window that the fresh list did
    // not just re-list is gone.
    if let Some(window) = full_resync_window {
        let kept: std::collections::HashSet<_> =
            upsert.iter().map(|e| e.id).chain(remove_ids.iter().copied()).collect();
        let stale = super::missing_from_full_resync(vault, calendar.id, window, &kept).await?;
        remove_ids.extend(stale);
    }

    let mut etags = calendar.account_sync.etags.clone();
    if full_resync_window.is_some() {
        etags.clear();
    }
    let cursor = AccountSyncCursor { token: next_token, etags, ..Default::default() };

    let vault = vault.clone();
    let id = calendar.id;
    blocking(move || Ok(vault.sync_account_calendar(id, &upsert, &remove_ids, cursor)?)).await
}

enum IncrementalOrFull<'a> {
    Incremental(&'a str),
    Windowed(jiff::civil::Date, jiff::civil::Date),
}

/// Page through `events.list` until Google stops handing back a
/// `nextPageToken`, returning every item across every page and the final
/// `nextSyncToken`.
///
/// `base` is [`API`] in production and a mock server's own address under
/// test -- see this module's tests -- so the paging loop, the 410 fallback
/// and the JSON shapes can be checked without reaching Google at all.
async fn list_events(
    base: &str,
    calendar_id: &str,
    token: &str,
    mode: IncrementalOrFull<'_>,
) -> CommandResult<(Vec<GoogleEvent>, Option<String>)> {
    let mut items = Vec::new();
    let mut page_token: Option<String> = None;
    let mut next_sync_token = None;
    loop {
        let mut url =
            format!("{base}/calendars/{calendar_id}/events?singleEvents=true&maxResults=250");
        match &mode {
            IncrementalOrFull::Incremental(sync_token) => {
                url.push_str(&format!("&syncToken={sync_token}"));
            }
            IncrementalOrFull::Windowed(from, to) => {
                url.push_str(&format!(
                    "&timeMin={}&timeMax={}",
                    rfc3339_start(*from),
                    rfc3339_start(*to)
                ));
            }
        }
        if let Some(pt) = &page_token {
            url.push_str(&format!("&pageToken={pt}"));
        }
        let bytes = get_bytes(&url, token).await?;
        let page: EventsResponse = serde_json::from_slice(&bytes).map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not read Google's answer: {e}"))
        })?;
        items.extend(page.items);
        if page.next_sync_token.is_some() {
            next_sync_token = page.next_sync_token;
        }
        page_token = page.next_page_token;
        if page_token.is_none() {
            break;
        }
    }
    Ok((items, next_sync_token))
}

fn to_event(
    calendar_id: CalendarId,
    item: &GoogleEvent,
    default_tz: &str,
    now: jiff::Timestamp,
) -> Option<Event> {
    let start = item.start.as_ref()?;
    let end = item.end.as_ref().unwrap_or(start);
    let all_day = start.date.is_some();
    let tz = start.time_zone.clone().unwrap_or_else(|| default_tz.to_string());

    let (start_ts, local_date) = if all_day {
        let d = start.date.as_deref()?;
        let date = jiff::civil::Date::strptime("%Y-%m-%d", d).ok()?;
        (date.at(0, 0, 0, 0).to_zoned(zone_or_utc(&tz)).ok()?.timestamp(), date)
    } else {
        let dt = start.date_time.as_deref()?;
        let ts: jiff::Timestamp = dt.parse().ok()?;
        (ts, ts.to_zoned(zone_or_utc(&tz)).date())
    };
    let (end_ts, end_date) = if all_day {
        let d = end.date.as_deref().unwrap_or(start.date.as_deref()?);
        let date = jiff::civil::Date::strptime("%Y-%m-%d", d).ok()?;
        // Google's all-day `end.date` is exclusive, like RFC 5545's.
        let last = date.yesterday().unwrap_or(date).max(local_date);
        (last.at(23, 59, 59, 0).to_zoned(zone_or_utc(&tz)).ok()?.timestamp(), last)
    } else {
        let dt = end.date_time.as_deref().unwrap_or(start.date_time.as_deref()?);
        let ts: jiff::Timestamp = dt.parse().ok()?;
        (ts, ts.to_zoned(zone_or_utc(&tz)).date())
    };

    Some(Event {
        id: deterministic_event_id(calendar_id, &item.id),
        calendar_id,
        uid: item.id.clone(),
        title: if item.summary.is_empty() {
            "(no title)".to_string()
        } else {
            item.summary.clone()
        },
        description: item.description.clone(),
        location: item.location.clone(),
        start: start_ts,
        end: end_ts.max(start_ts),
        local_date,
        end_date: end_date.max(local_date),
        tz,
        all_day,
        status: match item.status.as_str() {
            "tentative" => EventStatus::Tentative,
            "cancelled" => EventStatus::Cancelled,
            _ => EventStatus::Confirmed,
        },
        organizer: item.organizer.as_ref().map(GoogleAttendee::label).unwrap_or_default(),
        attendees: item.attendees.iter().map(GoogleAttendee::label).collect(),
        url: item.html_link.clone(),
        busy: item.transparency != "transparent",
        series: item.recurring_event_id.clone().or_else(|| item.i_cal_uid.clone()),
        updated_at: now,
    })
}

fn zone_or_utc(tz: &str) -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC)
}

fn rfc3339_start(d: jiff::civil::Date) -> String {
    format!("{d}T00:00:00Z")
}

async fn bearer(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<String> {
    match tokens::credential(svc, vault, account, Resource::Native).await? {
        Credential::Bearer(token) => Ok(token),
        Credential::Basic(_) => {
            Err(CommandError::new(codes::INVALID, "a Google account must sign in with OAuth"))
        }
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str, token: &str) -> CommandResult<T> {
    let bytes = get_bytes(url, token).await?;
    serde_json::from_slice(&bytes).map_err(|e| {
        CommandError::new(codes::NETWORK, format!("could not read Google's answer: {e}"))
    })
}

/// A GET with a bearer token, retrying a rate-limited 403 with a short
/// backoff before giving up -- see the module doc's "403 is not always a bad
/// credential".
async fn get_bytes(url: &str, token: &str) -> CommandResult<Vec<u8>> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let response = http::client()?.get(url).bearer_auth(token).send().await.map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not reach Google Calendar: {e}"))
        })?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(CommandError::new(
                codes::FORBIDDEN,
                "Google refused this account's credential",
            ));
        }
        if status == 403 {
            let retry_after = retry_after_delay(response.headers());
            let body = response.bytes().await.unwrap_or_default();
            if is_rate_limit_reason(&body) {
                let policy = calendar_after_retry_after();
                if policy.gives_up_after(attempt) {
                    return Err(CommandError::new(
                        codes::RATE_LIMITED,
                        "Google Calendar is rate-limiting this account; it will be tried again \
                         on the next sync",
                    ));
                }
                sleep(retry_after.unwrap_or_else(|| policy.delay_for(attempt))).await;
                continue;
            }
            // A 403 for any other reason: this account's credential is
            // fine, but this calendar specifically refused the request.
            return Err(CommandError::new(
                codes::NETWORK,
                format!("Google Calendar refused this request: {}", String::from_utf8_lossy(&body)),
            ));
        }
        if status == 410 {
            return Err(CommandError::new(codes::CONFLICT, "Google's sync token has expired"));
        }
        if !response.status().is_success() {
            return Err(CommandError::new(
                codes::NETWORK,
                format!("Google Calendar answered {status}"),
            ));
        }
        return response.bytes().await.map(|b| b.to_vec()).map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not read Google's answer: {e}"))
        });
    }
}

/// Does a Google error body name one of the three reasons that mean "you are
/// asking too fast"? A body that does not parse, or names none of the three,
/// reads as "no" -- safer to treat a reason this module does not recognise
/// as an ordinary failure than to retry something that will never succeed.
fn is_rate_limit_reason(body: &[u8]) -> bool {
    #[derive(Default, Deserialize)]
    struct Body {
        #[serde(default)]
        error: ErrorDetail,
    }
    #[derive(Default, Deserialize)]
    struct ErrorDetail {
        #[serde(default)]
        errors: Vec<ErrorReason>,
    }
    #[derive(Deserialize)]
    struct ErrorReason {
        #[serde(default)]
        reason: String,
    }
    let Ok(parsed) = serde_json::from_slice::<Body>(body) else { return false };
    parsed.error.errors.iter().any(|e| {
        matches!(e.reason.as_str(), "rateLimitExceeded" | "userRateLimitExceeded" | "quotaExceeded")
    })
}

/// Percent-encode the handful of characters a Google calendar id can
/// contain that are not already URL-safe -- chiefly `@` in a personal
/// address used as a calendar id. Not a general-purpose encoder: this
/// application never puts arbitrary text here, only what `discover` itself
/// already read back from Google.
fn urlencoding_light(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            for b in c.to_string().as_bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// This source's [`super::CalendarProvider`] -- see `caldav.rs`'s own
/// `CalDavProvider` for why this thin wrapper exists.
pub(crate) struct GoogleProvider;

impl super::CalendarProvider for GoogleProvider {
    fn discover<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
    ) -> super::BoxFuture<'a, CommandResult<Vec<RemoteCalendar>>> {
        Box::pin(discover(svc, vault, account))
    }

    fn sync<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        account: &'a Account,
        calendar: &'a Calendar,
    ) -> super::BoxFuture<'a, CommandResult<SyncReport>> {
        Box::pin(sync(svc, vault, account, calendar))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::extract::Query;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A mock `events.list`: page one always has one event and a
    /// `nextPageToken`; page two ends the list with a `nextSyncToken`. A
    /// request naming `syncToken=stale-token` answers 410, the way Google
    /// does for a token it no longer recognises.
    async fn mock_events_list() -> (String, std::sync::Arc<AtomicU32>) {
        let calls = std::sync::Arc::new(AtomicU32::new(0));
        let calls_for_route = calls.clone();
        let app = axum::Router::new().route(
            "/calendars/cal-1/events",
            get(move |Query(params): Query<HashMap<String, String>>| {
                calls_for_route.fetch_add(1, Ordering::SeqCst);
                async move {
                    if params.get("syncToken").map(String::as_str) == Some("stale-token") {
                        return axum::http::StatusCode::GONE.into_response();
                    }
                    if !params.contains_key("pageToken") {
                        Json(serde_json::json!({
                            "items": [{"id": "evt-1", "status": "confirmed", "summary": "First",
                                "start": {"dateTime": "2026-09-14T09:00:00Z"},
                                "end": {"dateTime": "2026-09-14T09:30:00Z"}}],
                            "nextPageToken": "page-2",
                        }))
                        .into_response()
                    } else {
                        Json(serde_json::json!({
                            "items": [{"id": "evt-2", "status": "cancelled"}],
                            "nextSyncToken": "fresh-token",
                        }))
                        .into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://127.0.0.1:{port}"), calls)
    }

    #[tokio::test]
    async fn paging_collects_every_page_and_hands_back_the_final_sync_token() {
        let (base, _calls) = mock_events_list().await;
        let (items, sync_token) =
            list_events(&base, "cal-1", "tok", IncrementalOrFull::Incremental("first-sync"))
                .await
                .expect("both pages answer");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "evt-1");
        assert_eq!(items[1].id, "evt-2");
        assert_eq!(items[1].status, "cancelled", "a cancelled item is a deletion, not an event");
        assert_eq!(sync_token.as_deref(), Some("fresh-token"));
    }

    #[tokio::test]
    async fn a_410_on_the_sync_token_is_reported_as_a_conflict_to_fall_back_on() {
        let (base, _calls) = mock_events_list().await;
        let err = list_events(&base, "cal-1", "tok", IncrementalOrFull::Incremental("stale-token"))
            .await
            .unwrap_err();
        assert_eq!(err.code, codes::CONFLICT);
    }

    // ---- finding 1: a full resync after a 410 must compute its own
    // deletions --------------------------------------------------------

    /// A vault and a service around it, in a directory nobody has to clean
    /// up -- the same shape `tests/support/vault.rs` builds for the
    /// integration tests, reproduced here (rather than shared with it)
    /// because that module is only reachable from `tests/*.rs`, not from a
    /// unit test compiled into this crate itself.
    fn test_vault() -> (Arc<Service>, Arc<Vault>, tempfile::TempDir) {
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
        let vault = everyday_vault::create(dir.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault, dir)
    }

    #[tokio::test]
    async fn a_full_resync_after_a_410_removes_an_event_the_fresh_list_no_longer_names() {
        use everyday_core::account::{Account, AccountSecret, AuthMethod, Provider};
        use everyday_core::store::calendars::EventQuery;

        let windowed_calls = std::sync::Arc::new(AtomicU32::new(0));
        let calls_for_route = windowed_calls.clone();
        // "Tomorrow", not a fixed date, so this test is not hostage to
        // whenever it happens to run: `sync_window` reaches a year back and
        // two years forward from today, and tomorrow is always inside that.
        let start = (jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24)).to_string();
        let end = (jiff::Timestamp::now() + jiff::SignedDuration::from_hours(25)).to_string();

        let app = axum::Router::new()
            .route(
                "/token",
                axum::routing::post(|| async {
                    Json(serde_json::json!({
                        "access_token": "access-1",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                    }))
                }),
            )
            .route(
                "/calendars/cal-1/events",
                get(move |Query(params): Query<HashMap<String, String>>| {
                    let calls_for_route = calls_for_route.clone();
                    let start = start.clone();
                    let end = end.clone();
                    async move {
                        if params.contains_key("syncToken") {
                            // The stored sync-token has gone stale.
                            return axum::http::StatusCode::GONE.into_response();
                        }
                        let call = calls_for_route.fetch_add(1, Ordering::SeqCst);
                        if call == 0 {
                            // The first, genuine full sync: both events exist.
                            Json(serde_json::json!({
                                "items": [
                                    {"id": "evt-a", "status": "confirmed", "summary": "Keeps",
                                     "start": {"dateTime": start}, "end": {"dateTime": end}},
                                    {"id": "evt-b", "status": "confirmed", "summary": "Deleted while stale",
                                     "start": {"dateTime": start}, "end": {"dateTime": end}},
                                ],
                                "nextSyncToken": "sync-1",
                            }))
                            .into_response()
                        } else {
                            // The 410 fallback's own windowed list: evt-b is
                            // simply absent -- exactly how a deletion looks
                            // with no `syncToken` in play, and the shape
                            // that used to be read as "nothing changed".
                            Json(serde_json::json!({
                                "items": [
                                    {"id": "evt-a", "status": "confirmed", "summary": "Keeps",
                                     "start": {"dateTime": start}, "end": {"dateTime": end}},
                                ],
                                "nextSyncToken": "sync-2",
                            }))
                            .into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let (svc, vault, _dir) = test_vault();
        let mut account = Account::new(Provider::Google, "person@example.com");
        account.services.calendar = true;
        account.auth = AuthMethod::OAuth {
            client_id: "test-client".into(),
            auth_url: "https://example.test/auth".into(),
            token_url: format!("{base}/token"),
            scopes: vec!["https://www.googleapis.com/auth/calendar.readonly".into()],
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
            "cal-1",
            "Work",
        );
        vault.save_calendar(&calendar).unwrap();

        sync_with_base(&svc, &vault, &account, &calendar, &base).await.expect("first sync");
        let after_first = vault
            .events(&EventQuery { calendar_id: Some(calendar.id), ..Default::default() })
            .unwrap();
        assert_eq!(after_first.len(), 2, "both events land on a genuine first sync");

        let calendar = vault.calendar(calendar.id).unwrap();
        assert_eq!(
            calendar.account_sync.token.as_deref(),
            Some("sync-1"),
            "the first sync's own token is what the second sync tries incrementally"
        );

        sync_with_base(&svc, &vault, &account, &calendar, &base)
            .await
            .expect("second sync, after the 410 fallback");
        let after_second = vault
            .events(&EventQuery { calendar_id: Some(calendar.id), ..Default::default() })
            .unwrap();
        assert_eq!(
            after_second.len(),
            1,
            "evt-b must be gone once the fresh full list stopped naming it, even though \
             Google never said it was cancelled"
        );
        assert_eq!(after_second[0].uid, "evt-a");
    }

    // ---- finding 2: 403 is not always a bad credential --------------------

    fn google_error_body(reason: &str) -> serde_json::Value {
        serde_json::json!({
            "error": { "errors": [{ "domain": "usageLimits", "reason": reason }] }
        })
    }

    /// A route that always answers 403 with `reason`, counting how many
    /// times it was asked.
    async fn mock_always_403(reason: &'static str) -> (String, std::sync::Arc<AtomicU32>) {
        let calls = std::sync::Arc::new(AtomicU32::new(0));
        let calls_for_route = calls.clone();
        let app = axum::Router::new().route(
            "/x",
            get(move || {
                calls_for_route.fetch_add(1, Ordering::SeqCst);
                async move {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [(axum::http::header::RETRY_AFTER, "0")],
                        Json(google_error_body(reason)),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://127.0.0.1:{port}/x"), calls)
    }

    #[tokio::test]
    async fn a_rate_limited_403_is_retried_and_never_reported_as_forbidden() {
        let (url, calls) = mock_always_403("rateLimitExceeded").await;
        let err = get_bytes(&url, "tok").await.unwrap_err();
        assert_eq!(
            err.code,
            codes::RATE_LIMITED,
            "a rate limit must never be the credential-is-bad code, or the account would be \
             wrongly marked needing sign-in"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            calendar_after_retry_after().max_attempts().unwrap(),
            "it must have actually retried, honouring the mock's Retry-After: 0"
        );
    }

    #[tokio::test]
    async fn a_403_for_any_other_reason_is_an_ordinary_failure_not_a_credential_one() {
        let (url, calls) = mock_always_403("insufficientPermissions").await;
        let err = get_bytes(&url, "tok").await.unwrap_err();
        assert_eq!(
            err.code,
            codes::NETWORK,
            "a calendar this account cannot read is not a bad credential"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "not a rate limit, so never retried");
    }

    #[tokio::test]
    async fn a_401_is_still_reported_as_forbidden() {
        let calls = std::sync::Arc::new(AtomicU32::new(0));
        let calls_for_route = calls.clone();
        let app = axum::Router::new().route(
            "/x",
            get(move || {
                calls_for_route.fetch_add(1, Ordering::SeqCst);
                async { axum::http::StatusCode::UNAUTHORIZED }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/x");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let err = get_bytes(&url, "tok").await.unwrap_err();
        assert_eq!(err.code, codes::FORBIDDEN);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a bad credential is never retried");
    }

    // ---- Event::series mapping ----------------------------------------------

    fn bare_google_event(id: &str) -> GoogleEvent {
        GoogleEvent {
            id: id.to_string(),
            status: "confirmed".into(),
            summary: "Weekly standup".into(),
            description: String::new(),
            location: String::new(),
            start: Some(GoogleWhen {
                date: None,
                date_time: Some("2026-09-16T09:00:00Z".into()),
                time_zone: Some("UTC".into()),
            }),
            end: Some(GoogleWhen {
                date: None,
                date_time: Some("2026-09-16T09:30:00Z".into()),
                time_zone: Some("UTC".into()),
            }),
            transparency: String::new(),
            organizer: None,
            attendees: Vec::new(),
            html_link: String::new(),
            recurring_event_id: None,
            i_cal_uid: None,
        }
    }

    /// The finding this guards: `singleEvents=true` mints a fresh `id` per
    /// occurrence of a recurring event, so two occurrences of the same
    /// series must still map to the same `Event::series` -- here,
    /// `recurringEventId`, which Google keeps stable across every instance.
    #[test]
    fn recurring_event_id_becomes_the_events_series() {
        let mut item = bare_google_event("instance-1");
        item.recurring_event_id = Some("master-abc".into());
        item.i_cal_uid = Some("master-abc@google.com".into());
        let event = to_event(CalendarId::new(), &item, "UTC", jiff::Timestamp::now()).unwrap();
        assert_eq!(event.series.as_deref(), Some("master-abc"), "recurringEventId wins");
    }

    /// `iCalUID` is present on every event, recurring or not, and is the
    /// fallback when `recurringEventId` is absent -- a one-off event still
    /// gets a `series`, harmlessly identical to its own durable id.
    #[test]
    fn i_cal_uid_is_the_series_when_there_is_no_recurring_event_id() {
        let mut item = bare_google_event("evt-1");
        item.i_cal_uid = Some("evt-1@google.com".into());
        let event = to_event(CalendarId::new(), &item, "UTC", jiff::Timestamp::now()).unwrap();
        assert_eq!(event.series.as_deref(), Some("evt-1@google.com"));
    }

    #[test]
    fn an_event_with_neither_field_has_no_series() {
        let item = bare_google_event("evt-1");
        let event = to_event(CalendarId::new(), &item, "UTC", jiff::Timestamp::now()).unwrap();
        assert_eq!(event.series, None);
    }
}
