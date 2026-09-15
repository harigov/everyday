//! The calendar domain: subscribed calendars and the events on them.
//!
//! This is the third domain in the vault, and it is deliberately the
//! *smallest* of the three, because most of what a calendar app needs
//! already exists. Time you schedule for yourself is a
//! [`TimeBlock`](crate::task::TimeBlock) — it has been since the todo app
//! shipped, which is why a block carries [`BlockSubject::Adhoc`] and a
//! planned/actual split. The calendar view draws those; it does not store
//! its own copy of them.
//!
//! What is genuinely new is *other people's* calendars:
//!
//! ```text
//!   Calendar ────── Event      read-only, replaced wholesale on each sync
//!      (a subscription: a URL, a colour, a name)
//!
//!   TimeBlock                  yours: planned, actual, and ad-hoc events
//!   Task                       yours: drawn on its due date
//!   Entry                      yours: drawn as a mark on the day you wrote
//! ```
//!
//! # Why events are a separate record from time blocks
//!
//! A [`TimeBlock`](crate::task::TimeBlock) is something you decided. An
//! [`Event`] is something a server told us. Keeping them apart means a sync
//! can delete every row belonging to one calendar and write the new set
//! without a single user record being in the blast radius — which is the
//! only way to make "refetch the feed" a safe operation to run every hour.
//! Folding both into one table would make every sync a merge, and a merge
//! that goes wrong eats work that cannot be recovered from the server,
//! because the server never had it.
//!
//! # Why subscriptions rather than accounts
//!
//! Google, Outlook and Apple all publish a calendar as an iCalendar
//! ([RFC 5545](https://www.rfc-editor.org/rfc/rfc5545)) feed at a secret
//! URL, and all three let you revoke that URL without touching the account.
//! Subscribing to one is read-only, needs no OAuth client registered with a
//! vendor, no redirect server, no token to refresh and no scope that could
//! grow later. For an application whose whole premise is that your data is
//! yours and stays on your machine, "paste a URL, get your meetings" is the
//! right shape — and it is the only shape that keeps working when the app is
//! a binary someone built themselves rather than a product with a client id.
//!
//! The trade is honest and worth stating: the sync is one-way. Events you
//! create here are yours and stay here; they do not appear on your work
//! calendar. See `docs` on [`Calendar`] for what the interface says about it.

use crate::id::{AccountId, CalendarId, EventId, RoleId};
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which service a feed came from.
///
/// Presentation only — every provider is fetched and parsed identically,
/// because they all publish the same RFC 5545 document. It exists so the
/// interface can show the right name and the right "where do I find this
/// URL" instructions, which is the single hardest step for the person doing
/// the subscribing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarProvider {
    Google,
    Outlook,
    Apple,
    /// Any other publisher of an iCalendar feed, and the default: a team
    /// calendar, a sports fixture list, the national holidays.
    #[default]
    Other,
}

impl CalendarProvider {
    pub const ALL: [CalendarProvider; 4] = [
        CalendarProvider::Google,
        CalendarProvider::Outlook,
        CalendarProvider::Apple,
        CalendarProvider::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CalendarProvider::Google => "google",
            CalendarProvider::Outlook => "outlook",
            CalendarProvider::Apple => "apple",
            CalendarProvider::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|provider| provider.as_str() == s)
    }

    /// Guess the provider from a feed URL, so the person pasting one does
    /// not also have to answer a question the URL already answers.
    pub fn guess(url: &str) -> Self {
        let host = url.to_ascii_lowercase();
        if host.contains("google.com") || host.contains("googleusercontent.com") {
            CalendarProvider::Google
        } else if host.contains("outlook.")
            || host.contains("office.com")
            || host.contains("office365.com")
            || host.contains("live.com")
        {
            CalendarProvider::Outlook
        } else if host.contains("icloud.com") || host.contains("me.com") {
            CalendarProvider::Apple
        } else {
            CalendarProvider::Other
        }
    }

    /// Which of the three named providers an *account* calendar is shown
    /// under, from the account's own [`crate::account::Provider`] rather
    /// than from a guess at a URL.
    ///
    /// iCloud, Fastmail, Yahoo and Custom all read the same way -- CalDAV,
    /// with nothing about the wire that says which of the four it is -- so
    /// only Google and Microsoft (whose calendars are read over their own
    /// APIs, not a generic protocol) earn a name of their own here; the rest
    /// fall to `Other`, exactly like an unrecognised feed.
    pub fn from_account(provider: crate::account::Provider) -> Self {
        use crate::account::Provider;
        match provider {
            Provider::Google => CalendarProvider::Google,
            Provider::Microsoft => CalendarProvider::Outlook,
            Provider::ICloud => CalendarProvider::Apple,
            Provider::Fastmail | Provider::Yahoo | Provider::Custom => CalendarProvider::Other,
        }
    }
}

/// Which protocol an account calendar is read over.
///
/// Presentation and dispatch only: `everyday-service::accountcal` reads this
/// to decide which adapter a sync belongs to, the same way
/// [`CalendarProvider`] decides which name and icon the interface draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountCalendarSource {
    /// iCloud, Fastmail, Custom, and Google's own CalDAV endpoint.
    CalDav,
    /// Google Calendar's REST API, used instead of CalDAV where discovery
    /// over CalDAV is awkward -- see `accountcal::google`'s module doc for
    /// which of the two this application chose and why.
    Google,
    /// Microsoft Graph.
    Graph,
}

impl AccountCalendarSource {
    pub const ALL: [AccountCalendarSource; 3] = [
        AccountCalendarSource::CalDav,
        AccountCalendarSource::Google,
        AccountCalendarSource::Graph,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AccountCalendarSource::CalDav => "calDav",
            AccountCalendarSource::Google => "google",
            AccountCalendarSource::Graph => "graph",
        }
    }
}

/// What an account calendar's sync remembers between runs, so a later sync
/// can ask the server for only what changed rather than everything again.
///
/// Held on the calendar record itself, sealed with the rest of it: a CalDAV
/// sync-token or a Google/Graph delta token is nearly as sensitive as a
/// feed's URL — whichever provider issued it can often be replayed to
/// enumerate a good deal of the calendar's shape — so it lives beside the
/// origin rather than in a clear column.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSyncCursor {
    /// RFC 6578's `sync-token` for CalDAV, or Google's or Graph's own delta
    /// token. `None` until the first successful sync, and cleared by a
    /// provider's own "start over" signal — a sync-token the server no
    /// longer recognises, or Google's 410 Gone — which is what triggers the
    /// etag-diff or full-list fallback the plan calls for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// CalDAV's fallback when the server does not advertise sync-collection,
    /// or has just invalidated the token above: every resource href this
    /// vault last saw, and the etag it had then. A later sync fetches the
    /// current etags, keeps anything unchanged, and multigets only the rest.
    #[serde(default)]
    pub etags: BTreeMap<String, String>,
    /// CalDAV only: which of `etags`' hrefs hold a recurring `VEVENT`
    /// (`RRULE` or `RDATE`) -- see `accountcal::caldav`'s doc on "Re-
    /// expanding a recurring event as the window moves" for why this has to
    /// be remembered rather than reread from the resource each time: an
    /// unchanged etag means the multiget below is skipped entirely, so
    /// nothing else would tell a later sync which unchanged resources still
    /// need re-expanding once the window has moved on. Always empty for
    /// Google and Graph, whose own APIs expand recurrence for this
    /// application and leave nothing here to track.
    #[serde(default)]
    pub recurring_hrefs: std::collections::BTreeSet<String>,
    /// CalDAV only: the far edge of the window every href in
    /// `recurring_hrefs` was last expanded into. `None` until the first
    /// sync that has one. Compared against the *current* sync window on
    /// every later sync so that a recurring resource whose etag never
    /// changes -- an unending weekly meeting nobody has touched -- still
    /// gets fresh occurrences materialised as today moves forward, rather
    /// than stopping dead at whatever the window happened to reach the day
    /// the event was created or last edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_through: Option<Date>,
}

impl AccountSyncCursor {
    pub fn is_empty(&self) -> bool {
        self.token.is_none() && self.etags.is_empty()
    }
}

/// Where a calendar's events come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CalendarOrigin {
    /// A feed that is refetched. The URL is a secret — anyone holding it can
    /// read the calendar — so it is sealed with the rest of the record.
    Url { url: String },
    /// A `.ics` file that was read once. `label` is the file name, kept only
    /// so the interface can say where the events came from; the file is not
    /// watched and not re-read.
    File { label: String },
    /// One of an account's own calendars, read over CalDAV or the
    /// provider's API rather than a subscription URL.
    ///
    /// `remote_id` and `remote_name` are the server's own identifiers for
    /// the calendar -- an href for CalDAV, a calendar id for Google or
    /// Graph. Neither is a secret the way a feed's URL is (the credential
    /// that actually reaches the server lives on the account, not here), so
    /// there is nothing about carrying them in the clear that this sealed
    /// record needs to hide -- they simply travel sealed with everything
    /// else, because the whole record is.
    Account {
        account_id: AccountId,
        remote_id: String,
        remote_name: String,
        source: AccountCalendarSource,
    },
}

impl CalendarOrigin {
    pub fn url(&self) -> Option<&str> {
        match self {
            CalendarOrigin::Url { url } => Some(url),
            CalendarOrigin::File { .. } | CalendarOrigin::Account { .. } => None,
        }
    }

    /// Is this origin one a background sync can ever refetch? A file has
    /// nothing to refetch from; a subscription URL and an account calendar
    /// both do, over different transports.
    pub fn is_syncable(&self) -> bool {
        !matches!(self, CalendarOrigin::File { .. })
    }

    /// The account this calendar belongs to, for the cascade that removes
    /// it when the account is deleted.
    pub fn account_id(&self) -> Option<AccountId> {
        match self {
            CalendarOrigin::Account { account_id, .. } => Some(*account_id),
            CalendarOrigin::Url { .. } | CalendarOrigin::File { .. } => None,
        }
    }
}

/// A calendar you have subscribed to.
///
/// Always read-only. Nothing this application does ever writes back to the
/// server it came from, and the interface says so where you add one, because
/// an app that silently declines to publish your changes is worse than one
/// that never offered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: CalendarId,
    pub name: String,
    /// `#rrggbb`. Every event on this calendar is drawn in it, which is the
    /// whole of how you tell four calendars apart at a glance.
    pub color: String,
    pub origin: CalendarOrigin,
    pub provider: CalendarProvider,
    /// Drawn on the grid. Unticking hides a calendar without unsubscribing,
    /// which is what you want for the one you only care about on Mondays.
    #[serde(default = "yes")]
    pub visible: bool,
    /// How long before the feed is refetched. Zero means manual only.
    #[serde(default = "default_refresh")]
    pub refresh_minutes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<Timestamp>,
    /// Why the last sync failed, if it did. Kept on the record rather than
    /// raised as a dialog: a feed that is down is a state to display beside
    /// the calendar, not an error to interrupt someone with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Which role this feed serves.
    ///
    /// A role rather than a [`Purpose`](crate::purpose::Purpose), because a
    /// feed is not filed under one outcome: a work calendar is work, and the
    /// forty meetings on it are not each yours to attribute. It is the
    /// cheapest large win in the whole balance report — one click here
    /// attributes a year of somebody else's claims on your time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<RoleId>,
    /// Sync state for an account calendar: a sync-token, or the etags an
    /// etag-diff fallback compares against. Always empty for a `Url` or
    /// `File` origin, which have no such state to remember. `#[serde(default)]`
    /// so a calendar sealed before this field existed still deserialises.
    #[serde(default)]
    pub account_sync: AccountSyncCursor,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn yes() -> bool {
    true
}

fn default_refresh() -> u32 {
    DEFAULT_REFRESH_MINUTES
}

/// How often a subscribed feed is refetched by default.
///
/// An hour, not a minute: these are other people's calendars, the feeds are
/// cached by their publishers anyway, and a laptop that wakes up to hammer
/// four servers is a laptop with a worse battery.
pub const DEFAULT_REFRESH_MINUTES: u32 = 60;

/// Calendar accents. The journal palette, so one vault has one set of colours.
pub const DEFAULT_CALENDAR_COLORS: &[&str] = crate::model::DEFAULT_JOURNAL_COLORS;

impl Calendar {
    pub fn subscribed(name: impl Into<String>, url: impl Into<String>) -> Self {
        let url = url.into();
        let now = Timestamp::now();
        Self {
            id: CalendarId::new(),
            name: name.into(),
            color: DEFAULT_CALENDAR_COLORS[0].to_string(),
            provider: CalendarProvider::guess(&url),
            origin: CalendarOrigin::Url { url },
            visible: true,
            refresh_minutes: DEFAULT_REFRESH_MINUTES,
            last_synced_at: None,
            last_error: None,
            role_id: None,
            account_sync: AccountSyncCursor::default(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn imported(name: impl Into<String>, label: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: CalendarId::new(),
            name: name.into(),
            color: DEFAULT_CALENDAR_COLORS[0].to_string(),
            origin: CalendarOrigin::File { label: label.into() },
            provider: CalendarProvider::Other,
            visible: true,
            // A file is not refetched; it has nowhere to be refetched from.
            refresh_minutes: 0,
            last_synced_at: None,
            last_error: None,
            role_id: None,
            account_sync: AccountSyncCursor::default(),
            created_at: now,
            updated_at: now,
        }
    }

    /// One of an account's own calendars, discovered rather than pasted in.
    ///
    /// `account_provider` picks the [`CalendarProvider`] it is shown under;
    /// `remote_id` and `remote_name` are the server's own identifiers, kept
    /// so a later sync knows which calendar to ask for and
    /// `list_account_calendars` can tell which of the account's calendars
    /// this vault already has.
    pub fn from_account(
        account_id: AccountId,
        account_provider: crate::account::Provider,
        source: AccountCalendarSource,
        remote_id: impl Into<String>,
        remote_name: impl Into<String>,
    ) -> Self {
        let now = Timestamp::now();
        let remote_name = remote_name.into();
        Self {
            id: CalendarId::new(),
            name: remote_name.clone(),
            color: DEFAULT_CALENDAR_COLORS[0].to_string(),
            origin: CalendarOrigin::Account {
                account_id,
                remote_id: remote_id.into(),
                remote_name,
                source,
            },
            provider: CalendarProvider::from_account(account_provider),
            visible: true,
            refresh_minutes: DEFAULT_REFRESH_MINUTES,
            last_synced_at: None,
            last_error: None,
            role_id: None,
            account_sync: AccountSyncCursor::default(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = color.into();
        self
    }

    /// The URL to fetch, normalised, or an explanation of why there is none.
    ///
    /// Two jobs, and the second one matters more than it looks. `webcal://`
    /// is what Apple and Outlook hand you from a "subscribe" button and it
    /// is just `https://` wearing a hat, so it is rewritten rather than
    /// rejected. And *only* `http` and `https` survive: without this check a
    /// pasted `file:///etc/passwd` would be read by the fetcher and its
    /// contents parsed into a calendar, which is a file-disclosure bug
    /// wearing the same hat.
    pub fn fetch_url(&self) -> crate::Result<String> {
        match &self.origin {
            CalendarOrigin::Url { url } => normalize_feed_url(url),
            CalendarOrigin::File { .. } => Err(crate::Error::Invalid(
                "this calendar was imported from a file and has no address to refetch".into(),
            )),
            CalendarOrigin::Account { .. } => Err(crate::Error::Invalid(
                "this calendar is read from a signed-in account, which has no feed address to \
                 refetch"
                    .into(),
            )),
        }
    }

    /// Is this calendar due a refresh, as of `now`? True for both a
    /// subscription URL, which `feeds.rs` refetches, and an account
    /// calendar, which `accountcal` syncs on the same cadence -- a file has
    /// nowhere to refresh from and is never due.
    pub fn is_due(&self, now: Timestamp) -> bool {
        if self.refresh_minutes == 0 || !self.origin.is_syncable() {
            return false;
        }
        match self.last_synced_at {
            None => true,
            Some(then) => {
                now.as_second() - then.as_second() >= i64::from(self.refresh_minutes) * 60
            }
        }
    }

    /// Record a successful sync.
    pub fn mark_synced(&mut self) {
        let now = Timestamp::now();
        self.last_synced_at = Some(now);
        self.last_error = None;
        self.updated_at = now;
    }

    /// Record a failed one. The previous events stay: a calendar that empties
    /// itself because the wifi dropped is worse than a stale one.
    pub fn mark_failed(&mut self, why: impl Into<String>) {
        self.last_error = Some(why.into());
        self.updated_at = Timestamp::now();
    }
}

/// Rewrite a subscription address into something safe to fetch.
pub fn normalize_feed_url(url: &str) -> crate::Result<String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let out = if let Some(rest) = lower.strip_prefix("webcal://") {
        format!("https://{}", &trimmed[trimmed.len() - rest.len()..])
    } else if lower.starts_with("https://") || lower.starts_with("http://") {
        trimmed.to_string()
    } else if trimmed.is_empty() {
        return Err(crate::Error::Invalid("a calendar needs an address".into()));
    } else if looks_schemed(trimmed) {
        return Err(crate::Error::Invalid(format!(
            "{trimmed:?} is not a calendar feed; only http, https and webcal addresses are fetched"
        )));
    } else {
        // A bare `example.com/basic.ics`, which is what a paste out of a
        // browser's address bar looks like.
        format!("https://{trimmed}")
    };
    Ok(out)
}

/// Does this address already name a scheme?
///
/// Anything that does, and that was not one of the three handled above, is
/// refused rather than quietly prefixed with `https://` — `data:`,
/// `javascript:` and `file:` all reach the colon without a `//` after them,
/// so a `contains("://")` test lets every one of them through.
///
/// A colon followed by digits is a port, not a scheme: `example.com:8080/x.ics`
/// is a host someone pasted, and refusing it would be wrong.
fn looks_schemed(url: &str) -> bool {
    let Some(colon) = url.find(':') else {
        return false;
    };
    let scheme = &url[..colon];
    let after = url[colon + 1..].chars().next();
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !after.is_some_and(|c| c.is_ascii_digit())
}

/// Whether the organiser has committed to an event happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventStatus {
    #[default]
    Confirmed,
    /// Pencilled in by the organiser, or an invitation not yet answered.
    Tentative,
    /// Called off. Kept and drawn struck through rather than dropped,
    /// because "the meeting was cancelled" is information and a blank slot
    /// is not.
    Cancelled,
}

impl EventStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EventStatus::Confirmed => "confirmed",
            EventStatus::Tentative => "tentative",
            EventStatus::Cancelled => "cancelled",
        }
    }
}

/// One occurrence of something on a subscribed calendar.
///
/// A *occurrence*, not a rule: a weekly stand-up arrives as one `VEVENT`
/// with an `RRULE`, and is stored here as one row per week within the synced
/// window. That is a deliberate trade — a few hundred rows for a recurring
/// meeting, in exchange for the calendar grid being a date-range index scan
/// with no recurrence engine anywhere near the draw path. See
/// [`crate::ics`] for the expansion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: EventId,
    pub calendar_id: CalendarId,
    /// The feed's own `UID`, with the occurrence's start appended for a
    /// recurring event. Stable across refetches, which is what lets a sync
    /// recognise the event it already had.
    pub uid: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// First and last day the event covers, in `tz`. Two columns rather than
    /// one so a week query can find a holiday that started last Thursday:
    /// `end_date >= from AND local_date <= to` is the overlap test, and it
    /// is an index scan.
    pub local_date: Date,
    pub end_date: Date,
    /// IANA time zone the event is quoted in.
    pub tz: String,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub status: EventStatus,
    /// Who called it, as the feed gives it: usually a name, sometimes an
    /// address.
    #[serde(default)]
    pub organizer: String,
    /// Everybody else invited, as the feed gives them: a name where there is
    /// one, an address where there is not.
    ///
    /// Sealed with the rest of the payload, and it is the field that makes
    /// preparing for a meeting possible at all -- "who is coming" is the
    /// question, and until this existed the answer was one name.
    #[serde(default)]
    pub attendees: Vec<String>,
    /// A link the feed offered — the meeting room, the fixture page.
    #[serde(default)]
    pub url: String,
    /// Whether the feed marked the time as busy. A "free" event is drawn
    /// faintly, because it should not read as a clash.
    #[serde(default = "yes")]
    pub busy: bool,
    pub updated_at: Timestamp,
}

impl Event {
    /// Length in whole minutes, floored, never negative.
    pub fn minutes(&self) -> u32 {
        let secs = self.end.as_second() - self.start.as_second();
        u32::try_from(secs.max(0) / 60).unwrap_or(u32::MAX)
    }

    /// Does this event cover any part of the days `from..=to`?
    pub fn covers(&self, from: Option<Date>, to: Option<Date>) -> bool {
        !(from.is_some_and(|f| self.end_date < f) || to.is_some_and(|t| self.local_date > t))
    }

    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.title);
        out.push('\n');
        out.push_str(&self.description);
        out.push('\n');
        out.push_str(&self.location);
        out
    }
}

/// What one sync did, for the line the interface shows afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub calendar_id: Option<CalendarId>,
    /// Events written for this calendar, replacing whatever was there.
    pub events: u64,
    /// Occurrences the feed described that fell outside the synced window.
    pub skipped: u64,
    /// The name the feed calls itself, if it offered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webcal_is_just_https_wearing_a_hat() {
        assert_eq!(
            normalize_feed_url("webcal://p01.calendar.icloud.com/x/y.ics").unwrap(),
            "https://p01.calendar.icloud.com/x/y.ics",
        );
        // Case is preserved in the path, which for a secret feed URL is not
        // optional -- these are case-sensitive tokens.
        assert_eq!(
            normalize_feed_url("WEBCAL://host/AbCdEf.ics").unwrap(),
            "https://host/AbCdEf.ics",
        );
    }

    #[test]
    fn a_bare_host_is_assumed_to_be_https() {
        assert_eq!(
            normalize_feed_url("example.com/basic.ics").unwrap(),
            "https://example.com/basic.ics",
        );
    }

    #[test]
    fn only_http_schemes_are_ever_fetched() {
        // The bug this exists to prevent: a pasted file:// URL being read
        // off disk and parsed into a calendar.
        for bad in [
            "file:///etc/passwd",
            "ftp://host/cal.ics",
            "data:text/calendar,x",
            "javascript:alert(1)",
        ] {
            assert!(normalize_feed_url(bad).is_err(), "{bad} should be refused");
        }
        assert!(normalize_feed_url("   ").is_err());
        // ...but a colon that is a port is a host someone pasted, not a scheme.
        assert_eq!(
            normalize_feed_url("example.com:8080/basic.ics").unwrap(),
            "https://example.com:8080/basic.ics",
        );
    }

    #[test]
    fn a_file_calendar_has_nothing_to_refetch() {
        let cal = Calendar::imported("Holidays", "holidays.ics");
        assert!(cal.fetch_url().is_err());
        assert!(!cal.is_due(Timestamp::now()), "a file is never due a refetch");
    }

    #[test]
    fn a_new_subscription_is_due_immediately_and_then_on_its_interval() {
        let mut cal = Calendar::subscribed("Work", "https://example.com/w.ics");
        let now = Timestamp::now();
        assert!(cal.is_due(now), "a feed that has never synced is due");

        cal.mark_synced();
        assert!(!cal.is_due(now));
        assert!(cal.is_due(now + jiff::SignedDuration::from_mins(61)));

        cal.refresh_minutes = 0;
        assert!(!cal.is_due(now + jiff::SignedDuration::from_hours(48)), "manual means manual");
    }

    #[test]
    fn a_failed_sync_is_recorded_without_clearing_the_last_good_one() {
        let mut cal = Calendar::subscribed("Work", "https://example.com/w.ics");
        cal.mark_synced();
        let good = cal.last_synced_at;
        cal.mark_failed("the server said 404");
        assert_eq!(cal.last_synced_at, good, "a failure must not erase when it last worked");
        assert!(cal.last_error.as_deref().unwrap().contains("404"));
        cal.mark_synced();
        assert_eq!(cal.last_error, None, "a good sync clears the complaint");
    }

    #[test]
    fn an_account_calendar_takes_its_provider_from_the_account_not_a_url_guess() {
        use crate::account::Provider;
        let cal = Calendar::from_account(
            AccountId::new(),
            Provider::ICloud,
            AccountCalendarSource::CalDav,
            "https://caldav.icloud.com/1234/calendars/home/",
            "Home",
        );
        assert_eq!(cal.provider, CalendarProvider::Apple);
        assert_eq!(cal.name, "Home");
        assert!(cal.origin.account_id().is_some());
        assert!(cal.origin.url().is_none(), "an account calendar has no feed address");

        for (provider, expected) in [
            (Provider::Google, CalendarProvider::Google),
            (Provider::Microsoft, CalendarProvider::Outlook),
            (Provider::ICloud, CalendarProvider::Apple),
            (Provider::Fastmail, CalendarProvider::Other),
            (Provider::Yahoo, CalendarProvider::Other),
            (Provider::Custom, CalendarProvider::Other),
        ] {
            assert_eq!(CalendarProvider::from_account(provider), expected, "{provider:?}");
        }
    }

    #[test]
    fn an_account_calendar_is_due_like_a_feed_but_has_no_address_to_refetch() {
        let mut cal = Calendar::from_account(
            AccountId::new(),
            crate::account::Provider::Google,
            AccountCalendarSource::Google,
            "primary",
            "Work",
        );
        let now = Timestamp::now();
        assert!(cal.is_due(now), "never synced, so due immediately -- same as a feed");
        assert!(cal.fetch_url().is_err(), "there is no feed address; accountcal syncs it instead");

        cal.mark_synced();
        assert!(!cal.is_due(now));
        assert!(cal.is_due(now + jiff::SignedDuration::from_mins(61)));

        cal.refresh_minutes = 0;
        assert!(!cal.is_due(now + jiff::SignedDuration::from_hours(48)), "manual means manual");
    }

    #[test]
    fn a_calendar_sealed_before_account_sync_existed_still_deserialises() {
        // The whole point of `#[serde(default)]` on `account_sync`: a
        // calendar written by yesterday's binary has no such field in its
        // sealed JSON at all.
        let without_field = serde_json::json!({
            "id": CalendarId::new(),
            "name": "Old",
            "color": "#000000",
            "origin": { "type": "url", "url": "https://example.com/a.ics" },
            "provider": "other",
            "visible": true,
            "refreshMinutes": 60,
            "createdAt": Timestamp::now().to_string(),
            "updatedAt": Timestamp::now().to_string(),
        });
        let cal: Calendar = serde_json::from_value(without_field).unwrap();
        assert!(cal.account_sync.is_empty());
        assert!(cal.account_sync.recurring_hrefs.is_empty());
        assert!(cal.account_sync.expanded_through.is_none());
    }

    #[test]
    fn a_recurring_href_and_its_expanded_through_date_survive_a_json_round_trip() {
        // What `accountcal::caldav::sync` writes for a recurring resource --
        // see that module's "Re-expanding a recurring event as the window
        // moves" doc -- has to come back exactly as it went in, or a later
        // sync would forget which unchanged hrefs still need re-expanding.
        let mut cal = Calendar::from_account(
            AccountId::new(),
            crate::account::Provider::ICloud,
            AccountCalendarSource::CalDav,
            "https://caldav.example.com/home/",
            "Home",
        );
        cal.account_sync.etags.insert("/cal/standup.ics".to_string(), "etag-1".to_string());
        cal.account_sync.recurring_hrefs.insert("/cal/standup.ics".to_string());
        cal.account_sync.expanded_through = Some(jiff::civil::date(2026, 9, 14));
        let round: Calendar = serde_json::from_slice(&serde_json::to_vec(&cal).unwrap()).unwrap();
        assert_eq!(round, cal);
    }

    #[test]
    fn providers_are_guessed_from_the_address() {
        assert_eq!(
            CalendarProvider::guess("https://calendar.google.com/calendar/ical/x/basic.ics"),
            CalendarProvider::Google,
        );
        assert_eq!(
            CalendarProvider::guess(
                "https://outlook.office365.com/owa/calendar/x/reachcalendar.ics"
            ),
            CalendarProvider::Outlook,
        );
        assert_eq!(
            CalendarProvider::guess("webcal://p52-calendars.icloud.com/published/2/x"),
            CalendarProvider::Apple,
        );
        assert_eq!(
            CalendarProvider::guess("https://fixtures.example.org/team.ics"),
            CalendarProvider::Other,
        );
    }

    #[test]
    fn an_event_covers_every_day_between_its_ends() {
        use jiff::civil::date;
        let e = Event {
            id: EventId::new(),
            calendar_id: CalendarId::new(),
            uid: "u".into(),
            title: "Holiday".into(),
            description: String::new(),
            location: String::new(),
            start: Timestamp::now(),
            end: Timestamp::now(),
            local_date: date(2026, 7, 10),
            end_date: date(2026, 7, 20),
            tz: "UTC".into(),
            all_day: true,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: Vec::new(),
            url: String::new(),
            busy: false,
            updated_at: Timestamp::now(),
        };
        // The bug this guards: a week query keyed on the start day alone,
        // which loses a fortnight's holiday for the whole second week of it.
        assert!(e.covers(Some(date(2026, 7, 13)), Some(date(2026, 7, 19))));
        assert!(e.covers(Some(date(2026, 7, 20)), Some(date(2026, 7, 26))));
        assert!(!e.covers(Some(date(2026, 7, 21)), Some(date(2026, 7, 27))));
        assert!(!e.covers(Some(date(2026, 7, 1)), Some(date(2026, 7, 9))));
    }

    #[test]
    fn wire_names_match_the_serde_representation() {
        for p in CalendarProvider::ALL {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.as_str()));
        }
        for s in [EventStatus::Confirmed, EventStatus::Tentative, EventStatus::Cancelled] {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
        }
    }

    #[test]
    fn a_calendar_survives_a_json_round_trip() {
        let mut cal = Calendar::subscribed("Work", "webcal://host/x.ics").with_color("#0f766e");
        cal.mark_synced();
        let round: Calendar = serde_json::from_slice(&serde_json::to_vec(&cal).unwrap()).unwrap();
        assert_eq!(round, cal);
    }
}
