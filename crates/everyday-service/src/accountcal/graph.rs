//! Microsoft Graph: the calendars behind a Microsoft account.
//!
//! Graph, not the Outlook-flavoured IMAP endpoint mail reads from -- a
//! calendar is read over Graph even on an account whose mail is read over
//! IMAP, which is why `Provider::Microsoft`'s OAuth preset asks for
//! `Calendars.Read` alongside the IMAP/SMTP scopes rather than an
//! Outlook-namespaced one. Graph tokens are also a *different resource*
//! from the `outlook.office.com` one IMAP uses, which is the whole reason
//! [`super::tokens::Resource::Graph`] and its own `TokenCache` key exist --
//! see that module's doc.
//!
//! The primary calendar's events come from `calendarView/delta`, which
//! hands back a `deltaLink` to resume from and marks a deleted occurrence
//! with `"@removed": {"reason": "deleted"}` rather than simply omitting it
//! -- the shape [`sync`]'s paging loop watches for. Every other calendar is
//! polled with a plain, windowed `calendarView` and diffed against what was
//! there before, the same width as [`super::sync_window`]: Graph's delta
//! query is not guaranteed available on a calendar that is not the
//! account's default, and the plan settles for the simpler, always-correct
//! path there rather than probing for delta support per calendar.
//!
//! `Prefer: outlook.timezone="UTC"` on every request is what makes
//! `start.dateTime`/`end.dateTime` parse as plain UTC instants without this
//! module needing the Windows time zone names Graph would otherwise quote
//! them in; where a zone name is still read back (`start.timeZone` itself,
//! or a value from before this header existed on an older response cached
//! by a proxy), [`everyday_core::ics::resolve_tzid`] is the same table
//! `everyday-core`'s own feed parser uses to turn `"Pacific Standard Time"`
//! into `"America/Los_Angeles"`.
//!
//! # A full delta has to compute its own deletions too
//!
//! `@removed` only ever describes what changed *since the token Graph was
//! given*. A delta started fresh -- `since: None`, on a first sync or after
//! the 410 [`sync`] falls back to on an expired `deltaLink` -- has no such
//! token and so reports no `@removed` at all, even for an event deleted
//! while this vault's old link was going stale. [`sync`] treats that case
//! (and a non-primary calendar's windowed poll, which has no delta of its
//! own at all) the same way `google.rs` treats its own 410: after the list
//! comes back, [`super::missing_from_full_resync`] finds whatever this vault
//! already had in the synced window that the fresh list did not re-mention,
//! and removes it.

use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::calendar::{
    AccountCalendarSource, AccountSyncCursor, Calendar, CalendarOrigin, Event, EventStatus,
    SyncReport,
};
use everyday_core::id::CalendarId;
use serde::Deserialize;

use super::tokens::{self, Credential, Resource};
use super::{RemoteCalendar, deterministic_event_id, sync_window};
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::service::{Service, blocking};

const API: &str = "https://graph.microsoft.com/v1.0";
/// Graph's own alias for the account's default calendar, used as
/// `remote_id` so [`sync`] can tell "read this one with delta" from "read
/// this one with a windowed poll" without a second field on the record.
const PRIMARY: &str = "primary";

#[derive(Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    value: Vec<GraphCalendarListItem>,
}

#[derive(Deserialize)]
struct GraphCalendarListItem {
    id: String,
    #[serde(default)]
    name: String,
    color: Option<String>,
    #[serde(default, rename = "isDefaultCalendar")]
    is_default: bool,
    #[serde(rename = "hexColor")]
    hex_color: Option<String>,
}

pub async fn discover(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<Vec<RemoteCalendar>> {
    let token = graph_bearer(svc, vault, account).await?;
    let body: CalendarListResponse = get_json(&format!("{API}/me/calendars"), &token).await?;
    Ok(body
        .value
        .into_iter()
        .map(|item| RemoteCalendar {
            remote_id: if item.is_default { PRIMARY.to_string() } else { item.id },
            name: item.name,
            // Graph's "named colour" (`lightBlue`, `auto`, ...) is not a hex
            // triple the interface's swatches can use; `hexColor` is, when
            // the tenant has set one, and is preferred over it.
            color: item.hex_color.or(item.color).filter(|c| c.starts_with('#')),
            source: AccountCalendarSource::Graph,
        })
        .collect())
}

pub async fn sync(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
    calendar: &Calendar,
) -> CommandResult<SyncReport> {
    sync_with_base(svc, vault, account, calendar, API).await
}

/// [`sync`]'s own body, over `base` rather than the hardcoded [`API`] -- see
/// `google.rs`'s `sync_with_base` for why this split exists.
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
    let token = graph_bearer(svc, vault, account).await?;

    // `full_resync_window` is `Some` exactly when this sync's list cannot be
    // trusted to have mentioned a deletion on its own -- a primary
    // calendar's delta with no earlier token to resume from (a first sync,
    // or the 410 fallback), and a non-primary calendar's windowed poll,
    // which never has a delta at all. See the module doc.
    let (upsert_raw, removed_raw, next_link, full_resync_window) = if remote_id == PRIMARY {
        let since = calendar.account_sync.token.as_deref();
        match delta_sync(base, &token, since).await {
            Ok(page) => {
                let window = if since.is_none() { Some(sync_window()) } else { None };
                (page.0, page.1, page.2, window)
            }
            // Graph's 410: the delta link is too old to resume from. Start
            // a fresh delta over the sync window, the same fallback
            // `google.rs` takes on its own 410.
            Err(e) if e.code == codes::CONFLICT => {
                let window = sync_window();
                let page = delta_sync(base, &token, None).await?;
                (page.0, page.1, page.2, Some(window))
            }
            Err(e) => return Err(e),
        }
    } else {
        let window = sync_window();
        let page = windowed_sync(base, &token, remote_id).await?;
        (page.0, page.1, page.2, Some(window))
    };

    let mut upsert = Vec::new();
    let mut remove_ids: Vec<everyday_core::id::EventId> =
        removed_raw.iter().map(|raw_id| deterministic_event_id(calendar.id, raw_id)).collect();
    for item in &upsert_raw {
        if let Some(event) = to_event(calendar.id, item) {
            upsert.push(event);
        } else {
            remove_ids.push(deterministic_event_id(calendar.id, &item.id));
        }
    }

    // A full list -- a fresh delta with nothing to resume from, or a
    // non-primary calendar's plain windowed poll -- never speaks for a
    // deletion on its own; see the module doc.
    if let Some(window) = full_resync_window {
        let kept: std::collections::HashSet<_> =
            upsert.iter().map(|e| e.id).chain(remove_ids.iter().copied()).collect();
        let stale = super::missing_from_full_resync(vault, calendar.id, window, &kept).await?;
        remove_ids.extend(stale);
    }

    let cursor = AccountSyncCursor { token: next_link, ..Default::default() };
    let vault = vault.clone();
    let id = calendar.id;
    blocking(move || Ok(vault.sync_account_calendar(id, &upsert, &remove_ids, cursor)?)).await
}

#[derive(Deserialize)]
struct DeltaPage {
    #[serde(default)]
    value: Vec<GraphEvent>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

#[derive(Deserialize)]
struct GraphEvent {
    id: String,
    #[serde(default)]
    subject: String,
    #[serde(rename = "bodyPreview", default)]
    body_preview: String,
    location: Option<GraphLocation>,
    start: Option<GraphWhen>,
    end: Option<GraphWhen>,
    #[serde(rename = "isAllDay", default)]
    is_all_day: bool,
    #[serde(rename = "showAs", default)]
    show_as: String,
    #[serde(rename = "isCancelled", default)]
    is_cancelled: bool,
    organizer: Option<GraphAttendeeWrap>,
    #[serde(default)]
    attendees: Vec<GraphAttendeeWrap>,
    #[serde(rename = "webLink", default)]
    web_link: String,
    #[serde(rename = "@removed")]
    removed: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GraphLocation {
    #[serde(rename = "displayName", default)]
    display_name: String,
}

#[derive(Deserialize)]
struct GraphWhen {
    #[serde(rename = "dateTime")]
    date_time: String,
    #[serde(rename = "timeZone")]
    time_zone: String,
}

#[derive(Deserialize)]
struct GraphAttendeeWrap {
    #[serde(rename = "emailAddress")]
    email_address: GraphEmailAddress,
}

#[derive(Deserialize)]
struct GraphEmailAddress {
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
}

impl GraphAttendeeWrap {
    fn label(&self) -> String {
        if !self.email_address.name.is_empty() {
            self.email_address.name.clone()
        } else {
            self.email_address.address.clone()
        }
    }
}

/// `calendarView/delta` on the primary calendar: page through `@odata.nextLink`
/// until Graph hands back `@odata.deltaLink` instead, which is the token to
/// present next time. `since` is `None` on a first sync, which starts a
/// fresh delta over [`super::sync_window`]; `Some(link)` on every later one,
/// which resumes from exactly where the last sync left off and needs no
/// window at all -- delta already knows what it has told this vault before.
///
/// `base` is [`API`] in production and a mock server's own address under
/// test -- see this module's tests -- so `since: None`'s own request, the
/// paging loop and the `@removed` handling can all be checked without
/// reaching Graph at all. Ignored when `since` already names a full
/// `@odata.nextLink`/`deltaLink`, which is already an absolute URL.
async fn delta_sync(
    base: &str,
    token: &str,
    since: Option<&str>,
) -> CommandResult<(Vec<GraphEvent>, Vec<String>, Option<String>)> {
    let mut url = match since {
        Some(link) => link.to_string(),
        None => {
            let (from, to) = sync_window();
            format!(
                "{base}/me/calendarView/delta?startDateTime={}&endDateTime={}",
                rfc3339_start(from),
                rfc3339_start(to)
            )
        }
    };
    let mut items = Vec::new();
    // Graph's own event ids for everything `"@removed"` marked -- turned
    // into this vault's deterministic ids by `sync`, which is the one place
    // that knows the calendar these belong to; this function stays
    // calendar-agnostic, which is what lets it be tested (see the unit
    // tests below) without a `CalendarId` in the fixture.
    let mut removed = Vec::new();
    let delta_link;
    loop {
        let page: DeltaPage = get_json_full_url(&url, token).await?;
        for item in page.value {
            if item.removed.is_some() {
                removed.push(item.id);
            } else {
                items.push(item);
            }
        }
        if let Some(next) = page.next_link {
            url = next;
            continue;
        }
        delta_link = page.delta_link;
        break;
    }
    Ok((items, removed, delta_link))
}

/// A plain, windowed `calendarView` for a calendar that is not the primary
/// one. No delta and nothing to page beyond what a single bounded window
/// returns; [`sync`] diffs the result against what this vault already had
/// inside that window, through [`super::missing_from_full_resync`], the same
/// way a feed's whole-calendar replace does.
async fn windowed_sync(
    base: &str,
    token: &str,
    calendar_remote_id: &str,
) -> CommandResult<(Vec<GraphEvent>, Vec<String>, Option<String>)> {
    let (from, to) = sync_window();
    let url = format!(
        "{base}/me/calendars/{calendar_remote_id}/calendarView?startDateTime={}&endDateTime={}",
        rfc3339_start(from),
        rfc3339_start(to)
    );
    let mut items = Vec::new();
    let mut next = Some(url);
    while let Some(url) = next.take() {
        let page: DeltaPage = get_json_full_url(&url, token).await?;
        items.extend(page.value.into_iter().filter(|e| e.removed.is_none()));
        next = page.next_link;
    }
    Ok((items, Vec::new(), None))
}

/// One Graph event, mapped into this vault's shape. `None` when the event
/// is missing the one thing every occurrence needs -- a start -- which
/// `sync` reads as "this id is no longer a usable event" and removes rather
/// than drops silently.
fn to_event(calendar_id: CalendarId, item: &GraphEvent) -> Option<Event> {
    if item.is_cancelled {
        return None;
    }
    let start = item.start.as_ref()?;
    let end = item.end.as_ref().unwrap_or(start);
    let start_zone = graph_zone(&start.time_zone);
    let end_zone = graph_zone(&end.time_zone);
    let start_ts = parse_graph_datetime(&start.date_time, &start_zone)?;
    let end_ts = parse_graph_datetime(&end.date_time, &end_zone)?.max(start_ts);
    let local_date = start_ts.to_zoned(start_zone.clone()).date();
    let mut end_date = end_ts.to_zoned(end_zone).date();
    // Graph's all-day `end.dateTime` is exclusive, like RFC 5545's.
    if item.is_all_day && end_date > local_date {
        end_date = end_date.yesterday().unwrap_or(end_date);
    }

    Some(Event {
        id: deterministic_event_id(calendar_id, &item.id),
        calendar_id,
        uid: item.id.clone(),
        title: if item.subject.is_empty() {
            "(no title)".to_string()
        } else {
            item.subject.clone()
        },
        description: item.body_preview.clone(),
        location: item.location.as_ref().map(|l| l.display_name.clone()).unwrap_or_default(),
        start: start_ts,
        end: end_ts,
        local_date,
        end_date: end_date.max(local_date),
        tz: start.time_zone.clone(),
        all_day: item.is_all_day,
        status: EventStatus::Confirmed,
        organizer: item.organizer.as_ref().map(GraphAttendeeWrap::label).unwrap_or_default(),
        attendees: item.attendees.iter().map(GraphAttendeeWrap::label).collect(),
        url: item.web_link.clone(),
        busy: !matches!(item.show_as.as_str(), "free" | "workingElsewhere"),
        updated_at: jiff::Timestamp::now(),
    })
}

/// Graph's own zone name, resolved to an IANA one this application's own
/// `jiff` can look up. `Prefer: outlook.timezone="UTC"` (every request this
/// module makes carries it -- see [`get_json_full_url`]) means this is
/// almost always literally `"UTC"`; the fallback through
/// [`everyday_core::ics::resolve_tzid`] -- the same Windows-zone table a
/// subscribed Outlook feed is read through -- covers a cached or
/// unexpectedly-timezoned response rather than assuming one can never
/// arrive.
fn graph_zone(name: &str) -> jiff::tz::TimeZone {
    let iana = everyday_core::ics::resolve_tzid(name).unwrap_or_else(|| name.to_string());
    jiff::tz::TimeZone::get(&iana).unwrap_or(jiff::tz::TimeZone::UTC)
}

/// Graph's `dateTime` is a local wall-clock reading with no offset of its
/// own -- `"2026-09-14T09:00:00.0000000"` -- meant to be read in whatever
/// `timeZone` said alongside it.
fn parse_graph_datetime(raw: &str, zone: &jiff::tz::TimeZone) -> Option<jiff::Timestamp> {
    let trimmed = raw.split('.').next().unwrap_or(raw);
    let dt = jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M:%S", trimmed).ok()?;
    dt.to_zoned(zone.clone()).ok().map(|z| z.timestamp())
}

async fn graph_bearer(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    account: &Account,
) -> CommandResult<String> {
    match tokens::credential(svc, vault, account, Resource::Graph).await? {
        Credential::Bearer(token) => Ok(token),
        Credential::Basic(_) => {
            Err(CommandError::new(codes::INVALID, "a Microsoft account must sign in with OAuth"))
        }
    }
}

fn rfc3339_start(d: jiff::civil::Date) -> String {
    format!("{d}T00:00:00Z")
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str, token: &str) -> CommandResult<T> {
    get_json_full_url(url, token).await
}

async fn get_json_full_url<T: serde::de::DeserializeOwned>(
    url: &str,
    token: &str,
) -> CommandResult<T> {
    let response = http::client()?
        .get(url)
        .bearer_auth(token)
        // What makes `GraphWhen::date_time` a plain UTC instant rather than
        // a Windows-zoned local time -- see the module doc.
        .header("Prefer", "outlook.timezone=\"UTC\"")
        .send()
        .await
        .map_err(|e| {
            CommandError::new(codes::NETWORK, format!("could not reach Microsoft Graph: {e}"))
        })?;
    if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "Microsoft Graph refused this account's credential",
        ));
    }
    if response.status().as_u16() == 410 {
        // Graph's own token-expiry signal for a delta link, same idea as
        // Google's: the caller has to start over. Surfaced as `CONFLICT`
        // for the same reason `google.rs` uses it, though today only a
        // primary-calendar delta sync can produce this -- a windowed poll
        // never presents a token Graph could reject.
        return Err(CommandError::new(codes::CONFLICT, "Microsoft Graph's delta link has expired"));
    }
    if !response.status().is_success() {
        return Err(CommandError::new(
            codes::NETWORK,
            format!("Microsoft Graph answered {}", response.status()),
        ));
    }
    response.json().await.map_err(|e| {
        CommandError::new(codes::NETWORK, format!("could not read Microsoft Graph's answer: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_zone_names_resolve_through_the_shared_table() {
        // The same table `everyday_core::ics` uses for a subscribed Outlook
        // feed -- see `graph_zone`'s doc -- so a Windows zone name and its
        // IANA equivalent must parse the same `dateTime` to the same instant.
        let windows = parse_graph_datetime(
            "2026-01-15T09:00:00.0000000",
            &graph_zone("Pacific Standard Time"),
        )
        .unwrap();
        let iana =
            parse_graph_datetime("2026-01-15T09:00:00.0000000", &graph_zone("America/Los_Angeles"))
                .unwrap();
        assert_eq!(windows, iana);
    }

    #[test]
    fn an_unrecognised_zone_name_falls_back_to_utc_rather_than_failing() {
        let zone = graph_zone("Not A Real Zone");
        let parsed = parse_graph_datetime("2026-01-15T09:00:00.0000000", &zone).unwrap();
        let utc =
            parse_graph_datetime("2026-01-15T09:00:00.0000000", &jiff::tz::TimeZone::UTC).unwrap();
        assert_eq!(parsed, utc);
    }

    /// A two-page mock of `calendarView/delta`: page one has one live event
    /// and one `"@removed"` one, page two (reached only through
    /// `@odata.nextLink`) has one more live event and ends the page with
    /// `@odata.deltaLink` -- the token [`delta_sync`] hands back to resume
    /// from next time. Exercises paging and `@removed` handling together,
    /// which is the shape a real incremental sync actually produces: a
    /// deletion rarely lands alone on its own page.
    async fn mock_delta_pages() -> String {
        use axum::Json;
        use axum::extract::State;
        use axum::routing::get;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let next_link = format!("{base}/delta2");

        let app = axum::Router::new()
            .route(
                "/delta1",
                get(move || {
                    let next_link = next_link.clone();
                    async move {
                        Json(serde_json::json!({
                            "value": [
                                {
                                    "id": "evt-1",
                                    "subject": "Kept",
                                    "start": {"dateTime": "2026-09-14T09:00:00.0000000", "timeZone": "UTC"},
                                    "end": {"dateTime": "2026-09-14T09:30:00.0000000", "timeZone": "UTC"},
                                    "isAllDay": false,
                                    "showAs": "busy",
                                },
                                {
                                    "id": "evt-2-gone",
                                    "@removed": {"reason": "deleted"},
                                },
                            ],
                            "@odata.nextLink": next_link,
                        }))
                    }
                }),
            )
            .route(
                "/delta2",
                get(move |State(base): State<String>| async move {
                    Json(serde_json::json!({
                        "value": [
                            {
                                "id": "evt-3",
                                "subject": "Also kept",
                                "start": {"dateTime": "2026-09-15T09:00:00.0000000", "timeZone": "UTC"},
                                "end": {"dateTime": "2026-09-15T09:30:00.0000000", "timeZone": "UTC"},
                                "isAllDay": false,
                                "showAs": "busy",
                            },
                        ],
                        "@odata.deltaLink": format!("{base}/delta-resume-token"),
                    }))
                }),
            )
            .with_state(base.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("{base}/delta1")
    }

    #[tokio::test]
    async fn delta_paging_collects_every_page_and_treats_removed_as_deletions() {
        let first_page = mock_delta_pages().await;
        // `since` already names a full `@odata.nextLink` here, so `base` is
        // never consulted -- see `delta_sync`'s own doc.
        let (items, removed, delta_link) =
            delta_sync("unused-base", "token-does-not-matter-here", Some(&first_page))
                .await
                .expect("the mock server answers both pages");

        assert_eq!(items.len(), 2, "one live event from each page");
        assert_eq!(items[0].id, "evt-1");
        assert_eq!(items[1].id, "evt-3");
        assert_eq!(removed, vec!["evt-2-gone".to_string()]);
        assert!(
            delta_link.as_deref().is_some_and(|link| link.ends_with("/delta-resume-token")),
            "the final page's deltaLink is what a later sync resumes from: {delta_link:?}"
        );
    }

    // ---- finding 1: a full delta after a 410 must compute its own
    // deletions, for the primary calendar too --------------------------

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
    async fn a_full_delta_after_a_410_removes_an_event_the_fresh_list_no_longer_names() {
        use axum::Json;
        use axum::extract::Query;
        use axum::response::IntoResponse;
        use axum::routing::get;
        use everyday_core::account::{Account, AccountSecret, AuthMethod, Provider};
        use everyday_core::store::calendars::EventQuery;
        use std::sync::atomic::{AtomicU32, Ordering};

        let windowed_calls = std::sync::Arc::new(AtomicU32::new(0));
        let calls_for_route = windowed_calls.clone();
        let start = "2026-09-15T09:00:00.0000000".to_string();
        let end = "2026-09-15T09:30:00.0000000".to_string();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let base_for_route = base.clone();

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
                "/me/calendarView/delta",
                get(move |Query(_params): Query<std::collections::HashMap<String, String>>| {
                    let calls_for_route = calls_for_route.clone();
                    let start = start.clone();
                    let end = end.clone();
                    let base_for_route = base_for_route.clone();
                    async move {
                        let call = calls_for_route.fetch_add(1, Ordering::SeqCst);
                        if call == 0 {
                            // The first, genuine full delta: both events
                            // exist, and its own deltaLink becomes "stale".
                            let delta_link = format!("{base_for_route}/stale-delta");
                            Json(serde_json::json!({
                                "value": [
                                    {"id": "evt-a", "subject": "Keeps",
                                     "start": {"dateTime": start, "timeZone": "UTC"},
                                     "end": {"dateTime": end, "timeZone": "UTC"},
                                     "isAllDay": false, "showAs": "busy"},
                                    {"id": "evt-b", "subject": "Deleted while stale",
                                     "start": {"dateTime": start, "timeZone": "UTC"},
                                     "end": {"dateTime": end, "timeZone": "UTC"},
                                     "isAllDay": false, "showAs": "busy"},
                                ],
                                "@odata.deltaLink": delta_link,
                            }))
                            .into_response()
                        } else {
                            // The 410 fallback's own fresh, windowed delta:
                            // evt-b is simply absent.
                            Json(serde_json::json!({
                                "value": [
                                    {"id": "evt-a", "subject": "Keeps",
                                     "start": {"dateTime": start, "timeZone": "UTC"},
                                     "end": {"dateTime": end, "timeZone": "UTC"},
                                     "isAllDay": false, "showAs": "busy"},
                                ],
                                "@odata.deltaLink": "http://unused/fresh-delta",
                            }))
                            .into_response()
                        }
                    }
                }),
            )
            .route(
                "/stale-delta",
                get(|| async { axum::http::StatusCode::GONE.into_response() }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let (svc, vault, _dir) = test_vault();
        let mut account = Account::new(Provider::Microsoft, "person@example.com");
        account.services.calendar = true;
        account.auth = AuthMethod::OAuth {
            client_id: "test-client".into(),
            auth_url: "https://example.test/auth".into(),
            token_url: format!("{base}/token"),
            scopes: vec!["Calendars.Read".into()],
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
        vault.save_calendar(&calendar).unwrap();

        sync_with_base(&svc, &vault, &account, &calendar, &base).await.expect("first sync");
        let after_first = vault
            .events(&EventQuery { calendar_id: Some(calendar.id), ..Default::default() })
            .unwrap();
        assert_eq!(after_first.len(), 2, "both events land on a genuine first sync");

        let calendar = vault.calendar(calendar.id).unwrap();
        assert!(
            calendar.account_sync.token.as_deref().is_some_and(|t| t.ends_with("/stale-delta")),
            "the first sync's own deltaLink is what the second sync tries to resume from: {:?}",
            calendar.account_sync.token
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
            "evt-b must be gone once the fresh full delta stopped naming it, even though \
             Graph reported no @removed at all"
        );
        assert_eq!(after_second[0].uid, "evt-a");
    }
}
