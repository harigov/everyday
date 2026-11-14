//! Screen time: heartbeats in, reports out, and the labels in between.
//!
//! Everything goes through [`Vault::with_screen`], which fails with
//! `unsupported` on a backend that holds journals only. The core watches
//! nothing -- collectors decide what is in use and send heartbeats -- so
//! this layer owns only what is worth testing: checking a batch, joining it
//! onto the stream's last span under the write lock, and adding spans up.
//!
//! # Streams
//!
//! One collector on one machine is one *stream*. Its spans are joined only
//! to each other, never to another stream's, so a browser extension and the
//! desktop collector on the same laptop never extend each other's spans.
//! The stream is kept in the clear as a keyed hash of the device and
//! collector (see [`SCREEN_KEY_LABEL`]), so the store can find a stream's
//! last span without anybody being able to tell which machine it is.
//!
//! # Read-modify-write
//!
//! Joining a batch onto the last span, saving a label that must be the only
//! one for its key, deleting a category out from under its labels: each
//! reads before it writes, so each does both inside one [`Vault::write`],
//! which is exclusive. Two batches from one stream arriving together are
//! then joined one after the other rather than both onto the same last span.

use super::Vault;
use super::session::{Domain, pick_domain};
use crate::crypto::SecretKey;
use crate::error::{Error, Result};
use crate::id::{ScreenAppId, ScreenCategoryId, ScreenSpanId};
use crate::record::RecordKind;
use crate::screentime::report::{self, Labels};
use crate::screentime::{
    CollectorConfig, HeartbeatBatch, IngestReport, SCREEN_KEY_LABEL, ScreenApp, ScreenAppRow,
    ScreenCategory, ScreenDevice, ScreenQuery, ScreenReport, ScreenSpan, ScreenTimeSettings,
    ScreenTimeline, SpanKind, defaults, domain, item_key, merge,
};
use crate::store::screentime::ScreenTimeStore;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};

/// The longest range one report adds up: a year and a bit, enough for "this
/// year against last" without a request that reads a decade of spans.
const MAX_REPORT_DAYS: i32 = 400;
/// How far back the list of apps to label looks.
const APPS_DAYS: i64 = 30;
/// How far back the list of devices looks.
const DEVICES_DAYS: i64 = 90;
/// The longest name a person may give an app or a category.
const MAX_LABEL_NAME: usize = 200;
/// Settings limits -- see [`Vault::save_screen_settings`].
const MAX_BROWSERS: usize = 50;
const MAX_BROWSER_ID: usize = 200;
const MAX_TITLE_DAYS: u32 = 3650;
/// How often heartbeats may trigger the retention sweep.
const PRUNE_EVERY_SECS: i64 = 3600;

/// When the retention sweep last ran from [`Vault::record_heartbeats`], in
/// seconds since the epoch. Process-wide rather than per vault: a process
/// has one vault open in practice, and the worst a second one costs is a
/// sweep an hour late.
static LAST_PRUNE: AtomicI64 = AtomicI64::new(0);

impl Vault {
    /// Does this vault's backend store screen time?
    pub fn supports_screen_time(&self) -> bool {
        self.with_screen(|_| Ok(())).is_ok()
    }

    pub(super) fn with_screen<T>(
        &self,
        f: impl FnOnce(&dyn ScreenTimeStore) -> Result<T>,
    ) -> Result<T> {
        self.with_domain(Domain::ScreenTime, |s| s.screentime().map(f))
    }

    // ---- heartbeats -----------------------------------------------------

    /// Take one batch from one collector: check it
    /// ([`merge::clean`](crate::screentime::merge::clean)), join it onto the
    /// stream's last span ([`merge::fold`](crate::screentime::merge::fold)),
    /// and write what changed -- one exclusive section. Now and then clears
    /// titles past [`ScreenTimeSettings::title_days`].
    ///
    /// The stream's key is derived from the session's own cipher, inside
    /// the section: [`Vault::derive_subkey`] takes the lock this already
    /// holds.
    pub fn record_heartbeats(&self, batch: &HeartbeatBatch) -> Result<IngestReport> {
        self.writable()?;
        let now = Timestamp::now();
        self.write(|u| {
            let key = u.cipher.derive_subkey(SCREEN_KEY_LABEL);
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            let settings = screen.screen_settings()?;
            let cleaned = merge::clean(batch, now, settings.keep_titles)?;
            let mut report = IngestReport {
                accepted: cleaned.beats.len() as u32,
                rejected: cleaned.rejected,
                spans: 0,
            };
            if !cleaned.beats.is_empty() {
                let stream = stream_key(&key, &cleaned.device, &cleaned.collector);
                let last = screen.last_span(&stream)?;
                let spans =
                    merge::fold(last.as_ref(), &cleaned.device, &cleaned.collector, &cleaned.beats);
                if !spans.is_empty() {
                    screen.put_spans(&stream, &spans)?;
                }
                report.spans = spans.len() as u32;
            }
            if settings.title_days > 0 && prune_due(now) {
                clear_old_titles(screen, settings, now)?;
            }
            Ok(report)
        })
    }

    /// What a collector is told when it connects.
    pub fn collector_config(&self) -> Result<CollectorConfig> {
        let settings = self.screen_settings()?;
        Ok(CollectorConfig {
            idle_minutes: settings.idle_minutes,
            keep_titles: settings.keep_titles,
        })
    }

    // ---- reports --------------------------------------------------------

    /// The report for a range. Validates the range (`from <= to`, at most
    /// 400 days) and the time zone.
    ///
    /// Reads the range and the equally long one before it in one read, so a
    /// batch landing in between cannot make "compared with last week"
    /// compare two different moments.
    pub fn screen_report(&self, query: &ScreenQuery) -> Result<ScreenReport> {
        let days = range_days(query.from, query.to)?;
        let tz = report::zone(&query.tz)?;
        let previous_from = query
            .from
            .checked_sub(jiff::Span::new().days(days))
            .map_err(|_| Error::Invalid("that range starts too long ago".into()))?;
        let (start, end) = report::day_bounds(query.from, query.to, &tz);
        let (previous_start, _) = report::day_bounds(previous_from, query.from, &tz);
        self.with_screen(|s| {
            let current = s.spans(start, end)?;
            let previous = s.spans(previous_start, start)?;
            let (apps, categories, settings) = labels_of(s)?;
            let labels = Labels { apps: &apps, categories: &categories, settings: &settings };
            Ok(report::build(query, &tz, &current, &previous, &labels))
        })
    }

    /// One day, run by run. `devices` empty is all.
    pub fn screen_timeline(
        &self,
        date: Date,
        tz: &str,
        devices: &[String],
    ) -> Result<ScreenTimeline> {
        let tz = report::zone(tz)?;
        let (start, end) = report::day_bounds(date, date, &tz);
        self.with_screen(|s| {
            let spans = s.spans(start, end)?;
            let (apps, categories, settings) = labels_of(s)?;
            let labels = Labels { apps: &apps, categories: &categories, settings: &settings };
            Ok(report::timeline(date, &tz, &spans, devices, &labels))
        })
    }

    /// Every app and site used in the last 30 days or labelled -- the list
    /// labels are edited in.
    pub fn screen_apps(&self) -> Result<Vec<ScreenAppRow>> {
        let now = Timestamp::now();
        self.with_screen(|s| {
            let spans = s.spans(days_before(now, APPS_DAYS), far_future(now))?;
            let (apps, categories, settings) = labels_of(s)?;
            let labels = Labels { apps: &apps, categories: &categories, settings: &settings };
            Ok(report::app_rows(&spans, &labels))
        })
    }

    /// Every machine that sent heartbeats in the last 90 days.
    pub fn screen_devices(&self) -> Result<Vec<ScreenDevice>> {
        let now = Timestamp::now();
        self.with_screen(|s| {
            Ok(report::devices(&s.spans(days_before(now, DEVICES_DAYS), far_future(now))?))
        })
    }

    /// Minutes per day for a tracker with a screen-time source, in this
    /// machine's time zone. `item` is an item key (`site:youtube.com`); one
    /// nothing was recorded under counts nothing.
    ///
    /// A tracker may ask about any window, including one open at both ends,
    /// so the window is first narrowed to the days there can be anything on:
    /// from the earliest span's day to tomorrow. A vault with no spans
    /// answers nothing without reading any.
    pub fn screen_minutes_by_day(
        &self,
        from: Date,
        to: Date,
        category: Option<ScreenCategoryId>,
        item: Option<&str>,
    ) -> Result<BTreeMap<Date, f64>> {
        let tz = TimeZone::system();
        self.with_screen(|s| {
            let Some(earliest) = s.earliest_span()? else { return Ok(BTreeMap::new()) };
            let (apps, categories, settings) = labels_of(s)?;
            let key = item.map(|k| k.trim().to_lowercase());
            let first = tz.to_datetime(earliest).date();
            let tomorrow = Timestamp::now().to_zoned(tz.clone()).date().tomorrow().ok();
            let from = from.max(first);
            let to = match tomorrow {
                Some(tomorrow) => to.min(tomorrow),
                None => to,
            };
            if from > to {
                return Ok(BTreeMap::new());
            }
            let (start, end) = report::day_bounds(from, to, &tz);
            let spans = s.spans(start, end)?;
            let labels = Labels { apps: &apps, categories: &categories, settings: &settings };
            Ok(report::minutes_by_day(&spans, &labels, &tz, from, to, category, key.as_deref()))
        })
    }

    // ---- labels ---------------------------------------------------------

    /// Validate (a well-formed key, at most one label per key, an existing
    /// category, `split` only on a site's registrable domain, a name of at
    /// most 200 characters) and store.
    ///
    /// The key is normalised as [`crate::screentime::item_key`] would make
    /// it -- an app's id lower-cased, a site's host stripped and lower-cased
    /// -- so `app:Code` and `app:code` are one label. A site's key may be a
    /// registrable domain or one host under it; a host's label applies only
    /// while its domain's label says `split`.
    ///
    /// **One label per key.** Saving a label under a new id for a key that
    /// already has one updates the one there instead -- its id and its
    /// creation time are kept, everything else is taken from `app`. An
    /// interface that offers "label this" from a report row without having
    /// fetched the label first then edits it rather than failing, and there
    /// is never a second label to disagree with the first.
    ///
    /// A category given beside `uncategorized` wins, and `uncategorized` is
    /// cleared: the person picked one.
    pub fn save_screen_app(&self, app: &ScreenApp) -> Result<()> {
        self.writable()?;
        let (key, kind) = parse_key(&app.key)?;
        if kind != app.kind {
            return Err(Error::Invalid(format!(
                "{key} is {}, but the label says it is {}",
                kind_word(kind),
                kind_word(app.kind)
            )));
        }
        let mut app = app.clone();
        app.key = key;
        app.name = label_name("an app's name", &app.name, false)?;
        if app.category.is_some() {
            app.uncategorized = false;
        }
        if app.split {
            let host = app.key.strip_prefix("site:").unwrap_or_default();
            if kind != SpanKind::Site || domain::registrable(host) != host {
                return Err(Error::Invalid(
                    "only a site's registrable domain (google.com, not mail.google.com) can be \
                     split into its hosts"
                        .into(),
                ));
            }
        }
        let id = self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            if let Some(category) = app.category {
                existing_category(screen, category)?;
            }
            if let Some(existing) =
                screen.list_apps()?.into_iter().find(|other| other.key == app.key)
            {
                app.id = existing.id;
                app.created_at = existing.created_at;
            }
            app.updated_at = Timestamp::now();
            screen.put_app(&app)?;
            Ok(app.id)
        })?;
        self.wrote(RecordKind::ScreenApp, id);
        Ok(())
    }

    pub fn delete_screen_app(&self, id: ScreenAppId) -> Result<()> {
        self.writable()?;
        self.with_screen(|s| s.delete_app(id))?;
        self.wrote(RecordKind::ScreenApp, id);
        Ok(())
    }

    /// The label for `key`, made (unsaved) if there is none -- for a caller
    /// that wants to change one field of whatever is there. The key is
    /// normalised and checked as [`Vault::save_screen_app`] would.
    pub fn screen_app_for(&self, key: &str) -> Result<ScreenApp> {
        let (key, kind) = parse_key(key)?;
        let found = self.with_screen(|s| Ok(s.list_apps()?.into_iter().find(|a| a.key == key)))?;
        Ok(found.unwrap_or_else(|| ScreenApp::new(key, kind)))
    }

    // ---- categories -----------------------------------------------------

    /// Every category, in order: `sort_order`, then name.
    pub fn screen_categories(&self) -> Result<Vec<ScreenCategory>> {
        self.with_screen(|s| {
            let mut all = s.list_categories()?;
            sort_categories(&mut all);
            Ok(all)
        })
    }

    /// Write the starter categories into a vault that has none. Harmless to
    /// call again: does nothing once any category exists, including after a
    /// person deleted every starter and made their own.
    pub fn ensure_screen_defaults(&self) -> Result<()> {
        self.writable()?;
        let written = self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            if !screen.list_categories()?.is_empty() {
                return Ok(Vec::new());
            }
            let starters = defaults::starter_categories();
            for category in &starters {
                screen.put_category(category)?;
            }
            Ok(starters.into_iter().map(|c| c.id).collect::<Vec<_>>())
        })?;
        for id in &written {
            self.wrote(RecordKind::ScreenCategory, id);
        }
        Ok(())
    }

    /// Validate (non-blank unique name, hex colour) and store.
    ///
    /// The name is trimmed, at most 200 characters, and unique among the
    /// categories case-insensitively. The colour is empty or `#rrggbb`: it
    /// is drawn straight into a style attribute, so nothing a `;` could add
    /// a second declaration to. [`ScreenCategory::builtin`] is the vault's,
    /// not the caller's: kept from the stored category, empty for a new one,
    /// so a suggestion can never be pointed at a category somebody made.
    pub fn save_screen_category(&self, category: &ScreenCategory) -> Result<()> {
        self.writable()?;
        let mut category = category.clone();
        category.name = label_name("a category's name", &category.name, true)?;
        category.color = category.color.trim().to_string();
        if !category.color.is_empty() && !is_hex_colour(&category.color) {
            return Err(Error::Invalid("a category's colour must be like #2563eb".into()));
        }
        self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            let all = screen.list_categories()?;
            let folded = category.name.to_lowercase();
            if all
                .iter()
                .any(|other| other.id != category.id && other.name.trim().to_lowercase() == folded)
            {
                return Err(Error::Invalid(format!(
                    "there is already a category called {:?}",
                    category.name
                )));
            }
            match all.iter().find(|other| other.id == category.id) {
                Some(stored) => {
                    category.builtin = stored.builtin.clone();
                    category.created_at = stored.created_at;
                }
                None => category.builtin = String::new(),
            }
            category.updated_at = Timestamp::now();
            screen.put_category(&category)
        })?;
        self.wrote(RecordKind::ScreenCategory, category.id);
        Ok(())
    }

    /// Delete a category; every label filed under it loses its category.
    ///
    /// The labels are cleared before the category goes, so a failure part
    /// way leaves a category that still exists rather than labels pointing
    /// at one that does not. A cleared label falls back to the suggestion,
    /// as one that never chose would.
    ///
    /// Refused while a tracker measures the category: its goal would go on
    /// reading zero minutes a day, which every max target calls met. The
    /// refusal names the tracker, so the person can point it elsewhere or
    /// delete it first.
    pub fn delete_screen_category(&self, id: ScreenCategoryId) -> Result<()> {
        self.writable()?;
        if self.supports_trackers() {
            let measuring = self.trackers()?.into_iter().find(|t| {
                matches!(
                    &t.source,
                    crate::tracker::TrackerSource::ScreenTime { category: Some(c), .. } if *c == id
                )
            });
            if let Some(tracker) = measuring {
                return Err(Error::Invalid(format!(
                    "the tracker {:?} measures this category; change or delete it first",
                    tracker.name
                )));
            }
        }
        let cleared = self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            let now = Timestamp::now();
            let mut cleared = Vec::new();
            for mut app in screen.list_apps()?.into_iter().filter(|a| a.category == Some(id)) {
                app.category = None;
                app.updated_at = now;
                screen.put_app(&app)?;
                cleared.push(app.id);
            }
            screen.delete_category(id)?;
            Ok(cleared)
        })?;
        self.wrote(RecordKind::ScreenCategory, id);
        for app in &cleared {
            self.wrote(RecordKind::ScreenApp, app);
        }
        Ok(())
    }

    // ---- settings -------------------------------------------------------

    pub fn screen_settings(&self) -> Result<ScreenTimeSettings> {
        self.with_screen(|s| s.screen_settings())
    }

    /// Validate (idle minutes 1..=60, title days 0..=3650, at most 50
    /// browsers of at most 200 characters) and store.
    ///
    /// Browser ids are trimmed, blank ones dropped, and repeats -- compared
    /// case-insensitively, as [`crate::screentime::defaults::is_browser`]
    /// compares -- kept once, before they are counted.
    pub fn save_screen_settings(&self, settings: &ScreenTimeSettings) -> Result<()> {
        self.writable()?;
        if !(1..=60).contains(&settings.idle_minutes) {
            return Err(Error::Invalid("idle minutes must be between 1 and 60".into()));
        }
        if settings.title_days > MAX_TITLE_DAYS {
            return Err(Error::Invalid(format!(
                "titles can be kept for at most {MAX_TITLE_DAYS} days, or 0 for always"
            )));
        }
        let mut browsers: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for id in &settings.browsers {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            if id.chars().count() > MAX_BROWSER_ID {
                return Err(Error::Invalid(format!(
                    "a browser's id can be at most {MAX_BROWSER_ID} characters"
                )));
            }
            if seen.insert(id.to_lowercase()) {
                browsers.push(id.to_string());
            }
        }
        if browsers.len() > MAX_BROWSERS {
            return Err(Error::Invalid(format!("at most {MAX_BROWSERS} browsers can be named")));
        }
        self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            // The sweep's own bookmark is the vault's, not the caller's: the
            // interface never sees it, so whatever it sent is ignored.
            let cleared_to = screen.screen_settings()?.titles_cleared_to;
            let settings =
                ScreenTimeSettings { browsers, titles_cleared_to: cleared_to, ..settings.clone() };
            screen.put_screen_settings(&settings)
        })
    }

    // ---- forgetting -----------------------------------------------------

    /// Delete every span of one item (an app's or a site's key, as a report
    /// names it). Answers how many spans went.
    ///
    /// Each span's key is worked out as a report would today, with today's
    /// `split` labels, so what goes is exactly what the report showed under
    /// that name. A site's registrable domain also takes every host under
    /// it, split or not: forgetting `google.com` means all of Google.
    pub fn forget_screen_item(&self, key: &str) -> Result<u64> {
        self.writable()?;
        let (key, kind) = parse_key(key)?;
        self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            let Some(earliest) = screen.earliest_span()? else { return Ok(0) };
            let split: HashSet<String> = screen
                .list_apps()?
                .into_iter()
                .filter(|a| a.kind == SpanKind::Site && a.split)
                .map(|a| crate::screentime::key_text(&a.key).to_string())
                .collect();
            let domain_key =
                |span: &ScreenSpan| format!("site:{}", domain::registrable(&span.host));
            let ids: Vec<ScreenSpanId> = screen
                .spans(earliest - SignedDuration::from_secs(1), Timestamp::MAX)?
                .into_iter()
                .filter(|span| span.kind == kind)
                .filter(|span| {
                    item_key(span.kind, &span.app, &span.host, &|d| split.contains(d)) == key
                        || (kind == SpanKind::Site && domain_key(span) == key)
                })
                .map(|span| span.id)
                .collect();
            if ids.is_empty() {
                return Ok(0);
            }
            screen.delete_spans(&ids)
        })
    }

    /// Delete every span overlapping `from..to`. Answers how many went.
    pub fn delete_screen_time(&self, from: Timestamp, to: Timestamp) -> Result<u64> {
        self.writable()?;
        if from >= to {
            return Err(Error::Invalid("the end of the range must be after its start".into()));
        }
        self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            let ids: Vec<ScreenSpanId> =
                screen.spans(from, to)?.into_iter().map(|span| span.id).collect();
            if ids.is_empty() {
                return Ok(0);
            }
            screen.delete_spans(&ids)
        })
    }

    /// Clear titles past [`ScreenTimeSettings::title_days`]. Answers how
    /// many spans changed; nothing when titles are kept as long as spans.
    pub fn prune_screen_time(&self, now: Timestamp) -> Result<u64> {
        self.writable()?;
        self.write(|u| {
            let screen = pick_domain(u.store.as_ref(), Domain::ScreenTime, |s| s.screentime())?;
            let settings = screen.screen_settings()?;
            if settings.title_days == 0 {
                return Ok(0);
            }
            clear_old_titles(screen, settings, now)
        })
    }
}

/// Empty the titles of spans that aged past the cut-off since the last
/// sweep, and move the bookmark up to it. Called inside `Vault::write`.
fn clear_old_titles(
    screen: &dyn ScreenTimeStore,
    mut settings: ScreenTimeSettings,
    now: Timestamp,
) -> Result<u64> {
    let cutoff = days_before(now, i64::from(settings.title_days));
    let from = settings.titles_cleared_to.unwrap_or(Timestamp::MIN);
    if from >= cutoff {
        return Ok(0);
    }
    let changed = screen.strip_titles(from, cutoff)?;
    settings.titles_cleared_to = Some(cutoff);
    screen.put_screen_settings(&settings)?;
    Ok(changed)
}

/// The stream a device's collector writes: a BLAKE3 keyed hash of
/// `device \0 collector` under the vault's screen-time subkey, hex, 32
/// characters. The `\0` keeps `("ab", "c")` and `("a", "bc")` apart.
fn stream_key(key: &SecretKey, device: &str, collector: &str) -> String {
    let identity = format!("{device}\0{collector}");
    blake3::keyed_hash(key.expose(), identity.as_bytes()).to_hex()[..32].to_string()
}

/// Has an hour passed since the last sweep? Claims the next one if so, so
/// two batches arriving together do not both sweep.
fn prune_due(now: Timestamp) -> bool {
    let now = now.as_second();
    let last = LAST_PRUNE.load(Ordering::Relaxed);
    now - last >= PRUNE_EVERY_SECS
        && LAST_PRUNE.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
}

fn days_before(now: Timestamp, days: i64) -> Timestamp {
    now.checked_sub(SignedDuration::from_hours(days * 24)).unwrap_or(Timestamp::MIN)
}

/// Comfortably past anything a collector may claim (see
/// [`crate::screentime::MAX_FUTURE_SECS`]).
fn far_future(now: Timestamp) -> Timestamp {
    now.checked_add(SignedDuration::from_hours(24)).unwrap_or(Timestamp::MAX)
}

/// How many days `from..=to` is, refusing a range that runs backwards or is
/// longer than [`MAX_REPORT_DAYS`].
fn range_days(from: Date, to: Date) -> Result<i32> {
    if from > to {
        return Err(Error::Invalid("a range's last day cannot be before its first".into()));
    }
    let days = from.until(to).map(|s| s.get_days()).unwrap_or(i32::MAX).saturating_add(1);
    if days > MAX_REPORT_DAYS {
        return Err(Error::Invalid(format!(
            "a screen time report covers at most {MAX_REPORT_DAYS} days"
        )));
    }
    Ok(days)
}

/// The labels, categories and settings a report reads beside the spans.
fn labels_of(
    screen: &dyn ScreenTimeStore,
) -> Result<(Vec<ScreenApp>, Vec<ScreenCategory>, ScreenTimeSettings)> {
    let mut categories = screen.list_categories()?;
    sort_categories(&mut categories);
    Ok((screen.list_apps()?, categories, screen.screen_settings()?))
}

fn sort_categories(categories: &mut [ScreenCategory]) {
    categories.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn existing_category(screen: &dyn ScreenTimeStore, id: ScreenCategoryId) -> Result<()> {
    if screen.list_categories()?.iter().any(|c| c.id == id) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("there is no screen time category with id {id}")))
    }
}

/// An item key, checked and normalised: `app:<id>` lower-cased, or
/// `site:<host>` stripped and lower-cased. Answers the key and its kind.
fn parse_key(key: &str) -> Result<(String, SpanKind)> {
    let bad = || {
        Error::Invalid(format!(
            "{key:?} is not an app or a site: a key looks like app:firefox or site:github.com"
        ))
    };
    let (prefix, rest) = key.trim().split_once(':').ok_or_else(bad)?;
    let (kind, normal) = match prefix.trim().to_lowercase().as_str() {
        "app" => (SpanKind::App, item_key(SpanKind::App, rest, "", &|_| false)),
        "site" => {
            let host = domain::normalize_host(rest);
            if host.chars().any(char::is_whitespace) {
                return Err(bad());
            }
            (SpanKind::Site, format!("site:{host}"))
        }
        _ => return Err(bad()),
    };
    if crate::screentime::key_text(&normal).is_empty() {
        return Err(bad());
    }
    Ok((normal, kind))
}

fn kind_word(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::App => "an app",
        SpanKind::Site => "a site",
    }
}

/// A name a person typed, trimmed and at most [`MAX_LABEL_NAME`]
/// characters; `required` refuses a blank one.
fn label_name(what: &str, text: &str, required: bool) -> Result<String> {
    let name = text.trim();
    if name.chars().count() > MAX_LABEL_NAME {
        return Err(Error::Invalid(format!("{what} can be at most {MAX_LABEL_NAME} characters")));
    }
    if required && name.is_empty() {
        return Err(Error::Invalid(format!("{what} cannot be blank")));
    }
    Ok(name.to_string())
}

/// `#rrggbb`, exactly.
fn is_hex_colour(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else { return false };
    hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn keys_are_normalised_or_refused() {
        assert_eq!(parse_key("app:Code.exe").unwrap(), ("app:code.exe".into(), SpanKind::App));
        assert_eq!(
            parse_key(" site:https://Mail.Google.com/inbox ").unwrap(),
            ("site:mail.google.com".into(), SpanKind::Site)
        );
        for bad in ["", "firefox", "app:", "site:", "site:  ", "thing:x", "site:a b.com"] {
            assert!(matches!(parse_key(bad), Err(Error::Invalid(_))), "{bad:?}");
        }
    }

    #[test]
    fn a_stream_key_is_32_hex_characters_and_tells_its_parts_apart() {
        let key = SecretKey::from_bytes([7u8; 32]);
        let a = stream_key(&key, "laptop", "gnome");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, stream_key(&key, "laptop", "gnome"));
        assert_ne!(a, stream_key(&key, "laptop", "firefox"));
        assert_ne!(stream_key(&key, "ab", "c"), stream_key(&key, "a", "bc"));
        assert_ne!(a, stream_key(&SecretKey::from_bytes([8u8; 32]), "laptop", "gnome"));
    }

    #[test]
    fn a_range_is_at_most_400_days_and_never_backwards() {
        assert_eq!(range_days(date(2026, 11, 5), date(2026, 11, 5)).unwrap(), 1);
        assert_eq!(range_days(date(2026, 11, 1), date(2026, 11, 7)).unwrap(), 7);
        assert!(range_days(date(2026, 11, 6), date(2026, 11, 5)).is_err());
        assert!(range_days(date(2025, 1, 1), date(2026, 2, 4)).is_ok());
        assert!(range_days(date(2025, 1, 1), date(2026, 2, 5)).is_err());
    }

    #[test]
    fn a_colour_is_six_hex_digits() {
        assert!(is_hex_colour("#2563eb"));
        assert!(is_hex_colour("#ABCDEF"));
        assert!(!is_hex_colour("#abc"));
        assert!(!is_hex_colour("2563eb"));
        assert!(!is_hex_colour("#2563eb;color:red"));
    }
}
