//! Screen time: which app or which website had somebody's attention, on
//! which machine, and for how long.
//!
//! See `docs/plans/screentime.md` for the reasoning; this is the shape.
//!
//! ```text
//!   Heartbeat ──(merge)──▶ ScreenSpan ──(report)──▶ ScreenReport
//!   (a collector saw       (one unbroken run        (totals by app, site,
//!    this, now)             of one app or site)      category, day, hour)
//!                                │
//!                       ScreenApp ── ScreenCategory
//!                       (a person's   (Work, Social, ...)
//!                        label)
//! ```
//!
//! # Nothing here watches anything
//!
//! The core has no socket and no window system. Collectors -- the shell's
//! own, the GNOME Shell extension, the browser extension -- decide for
//! themselves whether what they watch is *in use* (focused, not idle, not
//! locked) and only then send a [`Heartbeat`]. What arrives here is already
//! the answer; [`merge`] only joins heartbeats into spans, and [`report`]
//! only adds spans up.
//!
//! # Apps and sites
//!
//! A span is of an app ([`SpanKind::App`], reported by a desktop collector)
//! or of a site ([`SpanKind::Site`], reported by a browser extension). While
//! a browser has focus both kinds describe the same seconds; [`report`]
//! gives them to the site and leaves the browser only the time no site
//! claimed. Which apps are browsers is [`defaults::is_browser`] plus
//! [`ScreenTimeSettings::browsers`].
//!
//! A site is stored as the *host* the browser saw (`mail.google.com`) and
//! grouped at report time into its registrable domain (`google.com`, see
//! [`domain`]) unless a [`ScreenApp`] for that domain says to keep its
//! subdomains apart. Grouping late is what lets a person change their mind
//! about it and have every day already recorded follow.
//!
//! # Item keys
//!
//! An app or a site is named everywhere -- labels, reports, filters -- by
//! one string, its *key*: `app:<app id, lower-case>` or `site:<domain>`.
//! [`item_key`] makes one from a span; nothing else should assemble one.
//!
//! # What is sealed
//!
//! Everything a person would recognise: the device's name, the app, the
//! host, the title. In the clear only when a span started and ended, and a
//! keyed hash of which device and collector sent it (the *stream*, see
//! `Vault::record_heartbeats`), so a database holder learns that somebody
//! used a computer from nine to five and never what on.

pub mod defaults;
pub mod domain;
pub mod merge;
pub mod report;

use crate::id::{ScreenAppId, ScreenCategoryId, ScreenSpanId};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// The label a stream key is derived under, so the clear `stream` column
/// cannot be matched against anything outside this vault.
pub const SCREEN_KEY_LABEL: &str = "everyday.screentime.stream.v1";

/// How far apart two heartbeats of the same thing may be and still be one
/// span. Collectors send every few seconds and flush every half minute, so
/// a gap longer than this is somebody having looked away.
pub const PULSE_SECS: i64 = 45;

/// The longest one span may grow before a new one is started. Keeps any one
/// row's reach bounded, which is what lets a range query use an index.
pub const MAX_SPAN_SECS: i64 = 12 * 3600;

/// The most heartbeats one batch may carry.
pub const MAX_BEATS: usize = 5000;

/// How old a heartbeat may be and still be taken: a collector that was
/// offline for a while sends its backlog.
pub const MAX_BACKLOG_DAYS: i64 = 30;

/// How far in the future a heartbeat may claim to be: clocks drift.
pub const MAX_FUTURE_SECS: i64 = 300;

/// Field limits, in characters, after trimming. Over-long text from a
/// collector is cut, not refused: a page with a long title should not lose
/// the time spent on it.
pub const MAX_DEVICE: usize = 100;
pub const MAX_COLLECTOR: usize = 40;
pub const MAX_APP: usize = 200;
pub const MAX_NAME: usize = 200;
pub const MAX_HOST: usize = 253;
pub const MAX_TITLE: usize = 500;

/// What a span is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SpanKind {
    /// A desktop application had focus.
    App,
    /// A website was the active tab of the focused browser window.
    Site,
}

/// What a collector saw: one app or site in use from `start` to `end`.
///
/// A collector may send a bare observation (`end` absent, meaning the same
/// instant as `start`) every few seconds, or join its own observations into
/// runs first and send those; both merge the same way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub kind: SpanKind,
    /// [`SpanKind::App`]: the application's id as the platform names it --
    /// a desktop file id (`org.gnome.Ptyxis`), an executable (`Code.exe`),
    /// a bundle id (`com.apple.Safari`), an X11 class (`firefox`).
    /// [`SpanKind::Site`]: the browser (`Firefox`, `Chrome`).
    pub app: String,
    /// A friendlier name for the app, when the collector knows one
    /// (`Terminal` for `org.gnome.Ptyxis`). Empty when it does not.
    #[serde(default)]
    pub name: String,
    /// [`SpanKind::Site`] only: the host, as the browser saw it. Lower-cased
    /// and stripped of a port and a trailing dot when stored.
    #[serde(default)]
    pub host: String,
    /// The window or tab title. Dropped on arrival unless
    /// [`ScreenTimeSettings::keep_titles`].
    #[serde(default)]
    pub title: String,
    pub start: Timestamp,
    /// The end of a run; absent for a single observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Timestamp>,
}

/// One delivery from one collector on one machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatBatch {
    /// The machine's name, as the person knows it ("laptop"). The same name
    /// from a desktop collector and a browser extension is what lets a
    /// report tell that they describe the same seconds.
    pub device: String,
    /// What sent it: `gnome`, `kde`, `wlroots`, `x11`, `windows`, `macos`,
    /// `chrome`, `firefox`, `edge`, ... Lower-case letters, digits and `-`.
    pub collector: String,
    pub beats: Vec<Heartbeat>,
}

/// What [`HeartbeatBatch`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    /// Heartbeats taken.
    pub accepted: u32,
    /// Heartbeats refused: too old, in the future, ending before they
    /// start, or naming no app.
    pub rejected: u32,
    /// Spans created or extended.
    pub spans: u32,
}

/// One unbroken run of one app or site having somebody's attention.
///
/// Written by the vault only, from heartbeats; never edited, only deleted
/// (forgetting a site, clearing a range, or the retention sweep).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScreenSpan {
    pub id: ScreenSpanId,
    pub device: String,
    pub collector: String,
    pub kind: SpanKind,
    /// See [`Heartbeat::app`].
    pub app: String,
    /// See [`Heartbeat::name`]. The latest non-empty one seen.
    #[serde(default)]
    pub name: String,
    /// See [`Heartbeat::host`]. Empty for an app.
    #[serde(default)]
    pub host: String,
    /// The latest non-empty title seen, when titles are kept.
    #[serde(default)]
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
}

impl ScreenSpan {
    pub fn secs(&self) -> i64 {
        (self.end.as_second() - self.start.as_second()).max(0)
    }
}

/// A person's word about one app or site: what to call it, which category
/// it belongs to, whether to count it at all.
///
/// One per key at most. Nothing needs one: an app nobody labelled is shown
/// under the name its collector gave and the category [`defaults`]
/// suggests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScreenApp {
    pub id: ScreenAppId,
    /// The item this labels -- see the module doc's "Item keys".
    pub key: String,
    pub kind: SpanKind,
    /// The person's name for it. Empty keeps the collector's.
    #[serde(default)]
    pub name: String,
    /// `None` falls back to the suggested category, if any; see
    /// [`ScreenApp::uncategorized`] for saying "none" on purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<ScreenCategoryId>,
    /// The person said this has no category, so no suggestion applies.
    #[serde(default)]
    pub uncategorized: bool,
    /// Left out of every total: a lock screen, a launcher, a desktop.
    #[serde(default)]
    pub ignored: bool,
    /// Sites only, on a registrable domain's key: count each host under it
    /// as its own site (`mail.google.com`, `docs.google.com`) rather than
    /// together.
    #[serde(default)]
    pub split: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ScreenApp {
    pub fn new(key: impl Into<String>, kind: SpanKind) -> Self {
        let now = Timestamp::now();
        Self {
            id: ScreenAppId::new(),
            key: key.into(),
            kind,
            name: String::new(),
            category: None,
            uncategorized: false,
            ignored: false,
            split: false,
            created_at: now,
            updated_at: now,
        }
    }
}

/// What a person calls a kind of screen time: Work, Social, Entertainment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScreenCategory {
    pub id: ScreenCategoryId,
    pub name: String,
    /// A hex colour (`#2563eb`). Empty is the default.
    #[serde(default)]
    pub color: String,
    /// Which starter category this began as (`work`, `social`, ...), so a
    /// suggestion from [`defaults`] can find it after a rename. Empty for
    /// one the person made.
    #[serde(default)]
    pub builtin: String,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ScreenCategory {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: ScreenCategoryId::new(),
            name: name.into(),
            color: String::new(),
            builtin: String::new(),
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

/// The vault's own screen-time settings. Per machine settings -- whether
/// this computer tracks, what it is called -- are the shell's, not these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScreenTimeSettings {
    /// Minutes without input after which a collector stops counting. Read by
    /// collectors when they connect. 1 to 60.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: u32,
    /// Keep window and tab titles. Off drops them on arrival.
    #[serde(default = "default_true")]
    pub keep_titles: bool,
    /// Clear window and tab titles from spans older than this many days.
    /// 0 keeps them for as long as the spans.
    ///
    /// Titles, not spans: the time itself is what reports and goal trackers
    /// are drawn from -- a tracker's readings are worked out from spans,
    /// never stored -- so deleting old spans would rewrite every old day as
    /// empty and every max target as met. A title is the part worth
    /// forgetting: a message's subject, a page's name.
    #[serde(default)]
    pub title_days: u32,
    /// How far back titles have already been cleared, so each sweep reads
    /// only what aged past the cut-off since the last. Kept by the vault;
    /// never sent to the interface, and kept as it was when settings are
    /// saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub titles_cleared_to: Option<Timestamp>,
    /// App ids, beyond [`defaults::is_browser`]'s, that are browsers.
    #[serde(default)]
    pub browsers: Vec<String>,
}

fn default_idle_minutes() -> u32 {
    3
}

fn default_true() -> bool {
    true
}

impl Default for ScreenTimeSettings {
    fn default() -> Self {
        Self {
            idle_minutes: default_idle_minutes(),
            keep_titles: true,
            title_days: 0,
            titles_cleared_to: None,
            browsers: Vec::new(),
        }
    }
}

/// What a collector needs to know, answered when it connects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorConfig {
    pub idle_minutes: u32,
    pub keep_titles: bool,
}

/// A report's range and filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenQuery {
    /// First day, inclusive, in `tz`.
    pub from: Date,
    /// Last day, inclusive, in `tz`.
    pub to: Date,
    /// An IANA time zone (`America/Los_Angeles`): where days begin and end.
    /// Empty is the system's.
    #[serde(default)]
    pub tz: String,
    /// Only these devices. Empty is all.
    #[serde(default)]
    pub devices: Vec<String>,
    /// Only this category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<ScreenCategoryId>,
    /// Only this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

/// One app or site's share of a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenItem {
    /// See the module doc's "Item keys".
    pub key: String,
    pub kind: SpanKind,
    /// The person's name, else the collector's, else the key's own text.
    pub name: String,
    pub category: Option<ScreenCategoryId>,
    /// `category` is [`defaults`]' suggestion, not the person's choice.
    pub suggested: bool,
    pub secs: u64,
    /// A browser's time that no site claimed -- "Firefox (other)".
    pub browser_rest: bool,
    /// The label, when there is one.
    pub app_id: Option<ScreenAppId>,
}

/// One category's share of a range. `category: None` is uncategorised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryTotal {
    pub category: Option<ScreenCategoryId>,
    pub secs: u64,
}

/// One day of a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayTotal {
    pub date: Date,
    pub secs: u64,
    /// The same day by category, largest first.
    pub categories: Vec<CategoryTotal>,
}

/// One device's share of a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTotal {
    pub device: String,
    pub secs: u64,
}

/// Everything a range adds up to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenReport {
    pub from: Date,
    pub to: Date,
    pub total_secs: u64,
    /// Largest first.
    pub items: Vec<ScreenItem>,
    /// Largest first.
    pub categories: Vec<CategoryTotal>,
    /// Every day of the range, in order, empty ones included.
    pub days: Vec<DayTotal>,
    /// Seconds in each hour of the day (0..24, in `tz`), summed over the
    /// range.
    pub hours: Vec<u64>,
    /// Largest first.
    pub devices: Vec<DeviceTotal>,
    /// The total of the range of the same length just before this one, for
    /// "compared with last week".
    pub previous_total_secs: u64,
}

/// One run on a day's timeline, after apps and sites have been reconciled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRun {
    pub start: Timestamp,
    pub end: Timestamp,
    pub key: String,
    pub kind: SpanKind,
    pub name: String,
    pub category: Option<ScreenCategoryId>,
    pub device: String,
    /// Empty when titles are not kept.
    pub title: String,
}

/// One day, run by run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenTimeline {
    pub date: Date,
    /// In start order. Runs from different devices may overlap; runs from
    /// one device never do.
    pub runs: Vec<TimelineRun>,
}

/// One app or site somebody has used lately, or labelled -- a row of the
/// list where labels are edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenAppRow {
    pub key: String,
    pub kind: SpanKind,
    /// The collector's name, or the key's text: what shows when the label
    /// gives none.
    pub default_name: String,
    /// The label, when there is one.
    pub label: Option<ScreenApp>,
    /// What [`defaults`] suggests, when the label chooses nothing.
    pub suggested: Option<ScreenCategoryId>,
    /// Hosts seen under a registrable domain, most used first. Empty for an
    /// app.
    pub hosts: Vec<String>,
    /// Seconds in the last 30 days.
    pub recent_secs: u64,
    pub last_seen: Option<Timestamp>,
}

/// A machine that has sent heartbeats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenDevice {
    pub device: String,
    /// Every collector it has sent through.
    pub collectors: Vec<String>,
    pub last_seen: Timestamp,
}

/// The item key for a span -- see the module doc's "Item keys".
///
/// `split` answers, for a registrable domain, whether a label asked for its
/// hosts to be kept apart. An app's key is its id lower-cased; a site's is
/// its registrable domain, or its host when `split` says so.
pub fn item_key(kind: SpanKind, app: &str, host: &str, split: &dyn Fn(&str) -> bool) -> String {
    match kind {
        SpanKind::App => format!("app:{}", app.trim().to_lowercase()),
        SpanKind::Site => {
            let host = domain::normalize_host(host);
            let registrable = domain::registrable(&host);
            if split(&registrable) { format!("site:{host}") } else { format!("site:{registrable}") }
        }
    }
}

/// The text a key shows when nothing better is known.
pub fn key_text(key: &str) -> &str {
    key.split_once(':').map(|(_, rest)| rest).unwrap_or(key)
}
