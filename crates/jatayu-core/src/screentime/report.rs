//! Adding spans up.
//!
//! # Reconciling
//!
//! [`resolve`] turns raw spans into runs that can be added without counting
//! a second twice:
//!
//! 1. Per device, app spans are made not to overlap: where two overlap, the
//!    one that started later wins the overlap (it is the newer news). Site
//!    spans likewise, among themselves.
//! 2. Per device, a site run counts only while that device's app run is a
//!    browser ([`super::defaults::is_browser`] or
//!    [`ScreenTimeSettings::browsers`]) or while no app run covers it at all
//!    (no desktop collector was reporting). The browser's run gives up the
//!    seconds its sites claimed; what is left is the browser's own,
//!    `browser_rest`.
//! 3. Each run gets its item key ([`super::item_key`], honouring `split`
//!    labels), name, category (the label's, else the suggestion's, else
//!    none), and is dropped if its label says `ignored`.
//!
//! Runs from different devices are never reconciled against each other:
//! two machines in use at once are two machines in use.
//!
//! A browser's leftover is `browser_rest` only on a device that has site
//! spans in what is being added up: without the browser extension a browser
//! is simply one app, and calling all of its time "Firefox (other)" would
//! say something nobody measured.
//!
//! A host kept apart from its domain (`split`) with no label of its own
//! follows its domain's label for category and `ignored` -- the person said
//! "Google is work", and Docs is Google -- but not for its name, which
//! would make every host of the domain look alike.
//!
//! # Days and hours
//!
//! In the query's time zone, so a run across midnight is split between the
//! two days, and an hour bucket is a wall-clock hour. A clock change is
//! honoured: the spring-forward day is 23 hours long and has nothing in the
//! hour that was skipped; the fall-back day is 25 hours long, and the hour
//! that happens twice gets both.
//!
//! # Precision
//!
//! Arithmetic is in microseconds, rounded to whole seconds only when a total
//! is handed out, so a run split at midnight does not lose a second on each
//! side. A total is rounded on its own rather than summed from rounded
//! parts, so the items of a report may add up to a second or two either
//! side of its total.

use super::defaults;
use super::domain;
use super::{
    CategoryTotal, DayTotal, DeviceTotal, ScreenApp, ScreenAppRow, ScreenCategory, ScreenDevice,
    ScreenItem, ScreenQuery, ScreenReport, ScreenSpan, ScreenTimeSettings, ScreenTimeline,
    SpanKind, TimelineRun, item_key, key_text,
};
use crate::error::{Error, Result};
use crate::id::{ScreenAppId, ScreenCategoryId};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const MICROS: i64 = 1_000_000;

/// The most days [`build`] lays out, whatever it is asked: a guard for a
/// caller that did not check its range, far above the vault's own 400.
const MAX_DAYS: usize = 10_000;

/// What a report needs to know beyond the spans.
pub struct Labels<'a> {
    pub apps: &'a [ScreenApp],
    pub categories: &'a [ScreenCategory],
    pub settings: &'a ScreenTimeSettings,
}

/// One reconciled run -- see the module doc.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub start: Timestamp,
    pub end: Timestamp,
    pub key: String,
    pub kind: SpanKind,
    pub name: String,
    pub category: Option<ScreenCategoryId>,
    pub suggested: bool,
    pub device: String,
    pub title: String,
    pub browser_rest: bool,
    pub app_id: Option<ScreenAppId>,
}

/// A time zone by IANA name; empty is the system's. `Error::Invalid` for a
/// name that is not one.
pub fn zone(tz: &str) -> Result<TimeZone> {
    let tz = tz.trim();
    if tz.is_empty() {
        return Ok(TimeZone::system());
    }
    TimeZone::get(tz).map_err(|_| {
        Error::Invalid(format!("{tz:?} is not a time zone; name one like Europe/London"))
    })
}

/// The instants `from` begins and the day after `to` begins, in `tz`.
///
/// A day begins at its first instant, which is midnight except in the few
/// zones whose clocks spring forward at midnight, where it is one o'clock.
pub fn day_bounds(from: Date, to: Date, tz: &TimeZone) -> (Timestamp, Timestamp) {
    let start = day_start(from, tz).unwrap_or(Timestamp::MIN);
    let end = to.tomorrow().ok().and_then(|d| day_start(d, tz)).unwrap_or(Timestamp::MAX);
    (start, end)
}

fn day_start(date: Date, tz: &TimeZone) -> Option<Timestamp> {
    tz.to_timestamp(date.at(0, 0, 0, 0)).ok()
}

/// Reconcile spans into runs -- see the module doc. In start order.
pub fn resolve(spans: &[ScreenSpan], labels: &Labels) -> Vec<Resolved> {
    let lookup = Lookup::new(labels);
    let refs: Vec<&ScreenSpan> = spans.iter().collect();
    reconcile(&lookup, &refs).into_iter().filter(|r| !r.ignored).map(|r| r.resolved).collect()
}

/// The report for `query`. `current` and `previous` are every span that
/// overlaps the range and the equally long range before it; runs are
/// clipped to each range.
///
/// The query's devices, category and item narrow both ranges alike, so
/// "compared with last week" compares the same thing. An item that names a
/// registrable domain whose hosts are kept apart takes in all of them.
pub fn build(
    query: &ScreenQuery,
    tz: &TimeZone,
    current: &[ScreenSpan],
    previous: &[ScreenSpan],
    labels: &Labels,
) -> ScreenReport {
    let lookup = Lookup::new(labels);
    let (from, to) = day_bounds(query.from, query.to, tz);

    let mut total = 0i64;
    let mut items: HashMap<String, (ScreenItem, i64)> = HashMap::new();
    let mut categories: HashMap<Option<ScreenCategoryId>, i64> = HashMap::new();
    let mut days: BTreeMap<Date, (i64, HashMap<Option<ScreenCategoryId>, i64>)> = BTreeMap::new();
    let mut hours = [0i64; 24];
    let mut devices: HashMap<String, i64> = HashMap::new();

    for run in chosen(&lookup, current, query) {
        let Some((start, end)) = clip(run.start, run.end, from, to) else { continue };
        let micros = between(start, end);
        total += micros;
        *categories.entry(run.category).or_default() += micros;
        *devices.entry(run.device.clone()).or_default() += micros;
        walk(start, end, tz, |date, hour, us| {
            let day = days.entry(date).or_default();
            day.0 += us;
            *day.1.entry(run.category).or_default() += us;
            hours[hour.clamp(0, 23) as usize] += us;
        });
        let entry = items.entry(run.key.clone()).or_insert_with(|| {
            let item = ScreenItem {
                key: run.key.clone(),
                kind: run.kind,
                name: String::new(),
                category: run.category,
                suggested: run.suggested,
                secs: 0,
                browser_rest: false,
                app_id: run.app_id,
            };
            (item, 0)
        });
        // Runs come in start order, so the name kept is the latest one a
        // collector gave -- a label's, when there is one, is the same on
        // every run.
        entry.0.name = run.name.clone();
        entry.0.browser_rest |= run.browser_rest;
        entry.1 += micros;
    }

    let mut items: Vec<ScreenItem> = items
        .into_values()
        .filter(|(_, micros)| *micros > 0)
        .map(|(mut item, micros)| {
            item.secs = secs(micros);
            item
        })
        .collect();
    items.sort_by(|a, b| {
        b.secs
            .cmp(&a.secs)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.key.cmp(&b.key))
    });

    let mut day_rows = Vec::new();
    let mut date = query.from;
    while date <= query.to && day_rows.len() < MAX_DAYS {
        let (micros, split) = days.remove(&date).unwrap_or_default();
        day_rows.push(DayTotal { date, secs: secs(micros), categories: totals(split) });
        match date.tomorrow() {
            Ok(next) => date = next,
            Err(_) => break,
        }
    }

    let mut device_rows: Vec<DeviceTotal> = devices
        .into_iter()
        .map(|(device, micros)| DeviceTotal { device, secs: secs(micros) })
        .collect();
    device_rows.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.device.cmp(&b.device)));

    // The range of the same number of days just before this one.
    let length = query.from.until(query.to).map(|s| s.get_days()).unwrap_or(0).max(0) + 1;
    let previous_from =
        query.from.checked_sub(jiff::Span::new().days(length)).unwrap_or(query.from);
    let (previous_start, _) = day_bounds(previous_from, query.from, tz);
    let previous_total: i64 = chosen(&lookup, previous, query)
        .iter()
        .filter_map(|run| clip(run.start, run.end, previous_start, from))
        .map(|(start, end)| between(start, end))
        .sum();

    ScreenReport {
        from: query.from,
        to: query.to,
        total_secs: secs(total),
        items,
        categories: totals(categories),
        days: day_rows,
        hours: hours.iter().map(|us| secs(*us)).collect(),
        devices: device_rows,
        previous_total_secs: secs(previous_total),
    }
}

/// One day's runs, clipped to the day. `devices` empty is all. Titles are
/// left empty unless `labels.settings.keep_titles`.
///
/// Runs of one item on one device that meet end to end are drawn as one:
/// two collectors on one machine reporting the same app would otherwise
/// leave the timeline striped where reconciling cut one inside the other.
pub fn timeline(
    date: Date,
    tz: &TimeZone,
    spans: &[ScreenSpan],
    devices: &[String],
    labels: &Labels,
) -> ScreenTimeline {
    let lookup = Lookup::new(labels);
    let (from, to) = day_bounds(date, date, tz);
    let refs: Vec<&ScreenSpan> =
        spans.iter().filter(|s| devices.is_empty() || devices.contains(&s.device)).collect();
    let keep_titles = labels.settings.keep_titles;

    let mut runs: Vec<TimelineRun> = reconcile(&lookup, &refs)
        .into_iter()
        .filter(|r| !r.ignored)
        .filter_map(|r| {
            let (start, end) = clip(r.resolved.start, r.resolved.end, from, to)?;
            let r = r.resolved;
            Some(TimelineRun {
                start,
                end,
                key: r.key,
                kind: r.kind,
                name: r.name,
                category: r.category,
                device: r.device,
                title: if keep_titles { r.title } else { String::new() },
            })
        })
        .collect();

    runs.sort_by(|a, b| a.device.cmp(&b.device).then_with(|| a.start.cmp(&b.start)));
    let mut joined: Vec<TimelineRun> = Vec::with_capacity(runs.len());
    for run in runs {
        if let Some(last) = joined.last_mut()
            && last.device == run.device
            && last.key == run.key
            && last.end == run.start
        {
            last.end = run.end;
            if !run.title.is_empty() {
                last.title = run.title;
            }
            continue;
        }
        joined.push(run);
    }
    joined.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.device.cmp(&b.device)));
    ScreenTimeline { date, runs: joined }
}

/// Every item seen in `spans` (the last 30 days) and every labelled item,
/// most used first, labelled-but-unseen ones last by name. Ignored items
/// are listed -- this is where a person un-ignores them.
///
/// A site's row lists the hosts seen under it. A domain whose hosts are
/// kept apart has a row of its own as well as one per host -- the domain's
/// is where `split` is switched back off -- and its time is theirs added
/// together, so the same seconds show twice in this list. It is a list of
/// things to label, not a total.
pub fn app_rows(spans: &[ScreenSpan], labels: &Labels) -> Vec<ScreenAppRow> {
    struct Seen {
        kind: SpanKind,
        default_name: String,
        micros: i64,
        last_seen: Option<Timestamp>,
        hosts: HashMap<String, i64>,
    }
    fn seen(kind: SpanKind, default_name: String) -> Seen {
        Seen { kind, default_name, micros: 0, last_seen: None, hosts: HashMap::new() }
    }

    let lookup = Lookup::new(labels);
    let refs: Vec<&ScreenSpan> = spans.iter().collect();
    let mut rows: HashMap<String, Seen> = HashMap::new();
    for run in reconcile(&lookup, &refs) {
        let r = &run.resolved;
        let micros = between(r.start, r.end);
        let row = rows.entry(r.key.clone()).or_insert_with(|| seen(r.kind, String::new()));
        row.default_name = run.default_name.clone();
        row.micros += micros;
        row.last_seen = row.last_seen.max(Some(r.end));
        if r.kind != SpanKind::Site {
            continue;
        }
        let registrable = domain::registrable(&run.host);
        if key_text(&r.key) == registrable {
            *row.hosts.entry(run.host.clone()).or_default() += micros;
        } else {
            // A host of a split domain: the domain's own row hears of it.
            let domain_row = rows
                .entry(format!("site:{registrable}"))
                .or_insert_with(|| seen(SpanKind::Site, registrable.clone()));
            domain_row.micros += micros;
            domain_row.last_seen = domain_row.last_seen.max(Some(r.end));
            *domain_row.hosts.entry(run.host.clone()).or_default() += micros;
        }
    }

    let mut out: Vec<(ScreenAppRow, u64)> = rows
        .into_iter()
        .map(|(key, seen)| {
            let label = lookup.by_key.get(key.as_str()).map(|l| (*l).clone());
            let suggested = lookup.suggestion_unless_chosen(&key, label.as_ref());
            let mut hosts: Vec<(String, i64)> = seen.hosts.into_iter().collect();
            hosts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let recent = secs(seen.micros);
            let row = ScreenAppRow {
                default_name: if seen.default_name.is_empty() {
                    default_name_for(&key, "")
                } else {
                    seen.default_name
                },
                kind: seen.kind,
                label,
                suggested,
                hosts: if seen.kind == SpanKind::Site {
                    hosts.into_iter().map(|(h, _)| h).collect()
                } else {
                    Vec::new()
                },
                recent_secs: recent,
                last_seen: seen.last_seen,
                key,
            };
            (row, recent)
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| shown_name(&a.0).cmp(&shown_name(&b.0)))
            .then_with(|| a.0.key.cmp(&b.0.key))
    });
    let mut out: Vec<ScreenAppRow> = out.into_iter().map(|(row, _)| row).collect();

    let listed: HashSet<String> = out.iter().map(|r| r.key.clone()).collect();
    let mut unseen: Vec<ScreenAppRow> = labels
        .apps
        .iter()
        .filter(|label| !listed.contains(&label.key))
        .map(|label| ScreenAppRow {
            key: label.key.clone(),
            kind: label.kind,
            default_name: default_name_for(&label.key, ""),
            label: Some(label.clone()),
            suggested: lookup.suggestion_unless_chosen(&label.key, Some(label)),
            hosts: Vec::new(),
            recent_secs: 0,
            last_seen: None,
        })
        .collect();
    unseen.sort_by(|a, b| shown_name(a).cmp(&shown_name(b)).then_with(|| a.key.cmp(&b.key)));
    out.extend(unseen);
    out
}

/// Every device in `spans`, most recently seen first.
pub fn devices(spans: &[ScreenSpan]) -> Vec<ScreenDevice> {
    let mut seen: HashMap<&str, (BTreeSet<&str>, Timestamp)> = HashMap::new();
    for span in spans {
        let entry = seen.entry(span.device.as_str()).or_insert_with(|| (BTreeSet::new(), span.end));
        entry.0.insert(span.collector.as_str());
        entry.1 = entry.1.max(span.end);
    }
    let mut out: Vec<ScreenDevice> = seen
        .into_iter()
        .map(|(device, (collectors, last_seen))| ScreenDevice {
            device: device.to_string(),
            collectors: collectors.into_iter().map(str::to_string).collect(),
            last_seen,
        })
        .collect();
    out.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then_with(|| a.device.cmp(&b.device)));
    out
}

/// Minutes per day in `from..=to` (in `tz`), counting only runs in
/// `category` and of `app` (the label's key) when given -- what a tracker
/// with a screen-time source reads. Days with nothing are absent.
///
/// Minutes to one decimal place: a tracker draws "47.5 min", and a day with
/// twenty seconds on it is a day with something on it.
pub fn minutes_by_day(
    spans: &[ScreenSpan],
    labels: &Labels,
    tz: &TimeZone,
    from: Date,
    to: Date,
    category: Option<ScreenCategoryId>,
    app: Option<&str>,
) -> BTreeMap<Date, f64> {
    let lookup = Lookup::new(labels);
    let (lo, hi) = day_bounds(from, to, tz);
    let refs: Vec<&ScreenSpan> = spans.iter().collect();
    let app = app.map(|k| k.trim().to_lowercase());
    let mut by_day: BTreeMap<Date, i64> = BTreeMap::new();
    for run in reconcile(&lookup, &refs).into_iter().filter(|r| !r.ignored) {
        let r = run.resolved;
        if category.is_some() && r.category != category {
            continue;
        }
        if let Some(wanted) = &app
            && !item_matches(&r.key, wanted)
        {
            continue;
        }
        let Some((start, end)) = clip(r.start, r.end, lo, hi) else { continue };
        walk(start, end, tz, |date, _, us| *by_day.entry(date).or_default() += us);
    }
    by_day
        .into_iter()
        .filter(|(date, _)| *date >= from && *date <= to)
        .map(|(date, us)| (date, (us as f64 / (60.0 * MICROS as f64) * 10.0).round() / 10.0))
        .filter(|(_, minutes)| *minutes > 0.0)
        .collect()
}

// ---- reconciling ------------------------------------------------------------

/// The labels, categories and settings, arranged for asking about one run
/// at a time.
struct Lookup<'a> {
    by_key: HashMap<&'a str, &'a ScreenApp>,
    /// Registrable domains whose label says to keep their hosts apart.
    split: HashSet<String>,
    /// Starter key → the category that began as it.
    builtin: HashMap<&'a str, ScreenCategoryId>,
    known: HashSet<ScreenCategoryId>,
    /// [`ScreenTimeSettings::browsers`], lower-cased.
    browsers: HashSet<String>,
}

impl<'a> Lookup<'a> {
    fn new(labels: &Labels<'a>) -> Self {
        let mut by_key = HashMap::new();
        for app in labels.apps {
            by_key.entry(app.key.as_str()).or_insert(app);
        }
        let split = labels
            .apps
            .iter()
            .filter(|a| a.kind == SpanKind::Site && a.split)
            .map(|a| key_text(&a.key).to_string())
            .collect();
        let mut builtin = HashMap::new();
        for category in labels.categories.iter().filter(|c| !c.builtin.is_empty()) {
            builtin.entry(category.builtin.as_str()).or_insert(category.id);
        }
        Lookup {
            by_key,
            split,
            builtin,
            known: labels.categories.iter().map(|c| c.id).collect(),
            browsers: labels.settings.browsers.iter().map(|b| b.trim().to_lowercase()).collect(),
        }
    }

    fn is_browser(&self, app: &str) -> bool {
        defaults::is_browser(app) || self.browsers.contains(&app.trim().to_lowercase())
    }

    fn key(&self, span: &ScreenSpan) -> String {
        item_key(span.kind, &span.app, &span.host, &|domain| self.split.contains(domain))
    }

    /// The label that decides a key's category and whether it is ignored:
    /// its own, else -- for a host of a split domain -- the domain's.
    fn governing(&self, key: &str) -> Option<&'a ScreenApp> {
        if let Some(own) = self.by_key.get(key).copied() {
            return Some(own);
        }
        let host = key.strip_prefix("site:")?;
        let registrable = domain::registrable(host);
        if registrable == host {
            return None;
        }
        self.by_key.get(format!("site:{registrable}").as_str()).copied()
    }

    /// The suggested category for a key, mapped onto the vault's own
    /// categories through [`ScreenCategory::builtin`].
    fn suggestion(&self, key: &str) -> Option<ScreenCategoryId> {
        defaults::suggest(key).and_then(|b| self.builtin.get(b).copied())
    }

    /// [`Lookup::suggestion`], unless `label` already chose: a category,
    /// or none on purpose.
    fn suggestion_unless_chosen(
        &self,
        key: &str,
        label: Option<&ScreenApp>,
    ) -> Option<ScreenCategoryId> {
        match label {
            Some(l) if l.uncategorized || l.category.is_some_and(|c| self.known.contains(&c)) => {
                None
            }
            _ => self.suggestion(key),
        }
    }

    /// The category for a key and whether it is a suggestion.
    fn category(&self, key: &str, label: Option<&ScreenApp>) -> (Option<ScreenCategoryId>, bool) {
        if let Some(label) = label {
            if let Some(id) = label.category
                && self.known.contains(&id)
            {
                return (Some(id), false);
            }
            if label.uncategorized {
                return (None, false);
            }
        }
        match self.suggestion(key) {
            Some(id) => (Some(id), true),
            None => (None, false),
        }
    }

    fn run(&self, span: &ScreenSpan, piece: Piece, browser_rest: bool) -> Run {
        let key = self.key(span);
        let own = self.by_key.get(key.as_str()).copied();
        let governing = self.governing(&key);
        let default_name = match span.kind {
            SpanKind::App => default_name_for(&key, &span.name),
            SpanKind::Site => key_text(&key).to_string(),
        };
        let name = match own {
            Some(label) if !label.name.trim().is_empty() => label.name.trim().to_string(),
            _ => default_name.clone(),
        };
        let (category, suggested) = self.category(&key, governing);
        Run {
            resolved: Resolved {
                start: piece.start,
                end: piece.end,
                kind: span.kind,
                name,
                category,
                suggested,
                device: span.device.clone(),
                title: span.title.clone(),
                browser_rest,
                app_id: own.map(|l| l.id),
                key,
            },
            default_name,
            ignored: governing.is_some_and(|l| l.ignored),
            host: span.host.clone(),
        }
    }
}

/// What a key is called when no label names it: the collector's name, a
/// known app's friendly name, or the key's own text.
fn default_name_for(key: &str, collector_name: &str) -> String {
    let collector_name = collector_name.trim();
    if !collector_name.is_empty() {
        return collector_name.to_string();
    }
    if let Some(app) = key.strip_prefix("app:")
        && let Some(name) = defaults::friendly_name(app)
    {
        return name.to_string();
    }
    key_text(key).to_string()
}

fn shown_name(row: &ScreenAppRow) -> String {
    match &row.label {
        Some(label) if !label.name.trim().is_empty() => label.name.trim().to_lowercase(),
        _ => row.default_name.to_lowercase(),
    }
}

/// A reconciled run, with what only this module needs beside it.
struct Run {
    resolved: Resolved,
    /// The name the run would have without its label's.
    default_name: String,
    ignored: bool,
    /// A site's host as stored. Empty for an app.
    host: String,
}

/// The part of one span that counts.
#[derive(Debug, Clone, Copy)]
struct Piece {
    /// Index into the spans being reconciled.
    span: usize,
    start: Timestamp,
    end: Timestamp,
}

fn reconcile(lookup: &Lookup, spans: &[&ScreenSpan]) -> Vec<Run> {
    let mut by_device: BTreeMap<&str, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, span) in spans.iter().enumerate() {
        let entry = by_device.entry(span.device.as_str()).or_default();
        match span.kind {
            SpanKind::App => entry.0.push(i),
            SpanKind::Site => entry.1.push(i),
        }
    }

    let mut out = Vec::new();
    for (apps, sites) in by_device.into_values() {
        let apps = flatten(spans, apps);
        let sites = flatten(spans, sites);
        let has_sites = !sites.is_empty();
        let (browsers, others): (Vec<Piece>, Vec<Piece>) =
            apps.into_iter().partition(|p| lookup.is_browser(&spans[p.span].app));
        let counted = subtract(&sites, &cuts(&others));
        let rest = subtract(&browsers, &cuts(&counted));
        out.extend(others.into_iter().map(|p| lookup.run(spans[p.span], p, false)));
        out.extend(rest.into_iter().map(|p| lookup.run(spans[p.span], p, has_sites)));
        out.extend(counted.into_iter().map(|p| lookup.run(spans[p.span], p, false)));
    }
    out.sort_by(|a, b| {
        let (a, b) = (&a.resolved, &b.resolved);
        a.start.cmp(&b.start).then_with(|| a.device.cmp(&b.device)).then_with(|| a.key.cmp(&b.key))
    });
    out
}

/// Make spans of one kind on one device not overlap: where two do, the one
/// that started later has the overlap, and the earlier one keeps whatever
/// it covers on either side. Sorted, non-overlapping, never empty pieces.
///
/// Spans are taken in start order, so each new one starts at or after every
/// piece already laid down: it can only cut into the tail, which is where
/// the pieces that end after it starts must all be.
fn flatten(spans: &[&ScreenSpan], mut indices: Vec<usize>) -> Vec<Piece> {
    indices.sort_by_key(|&i| (spans[i].start, spans[i].id));
    let mut out: Vec<Piece> = Vec::with_capacity(indices.len());
    for i in indices {
        let span = spans[i];
        if span.end <= span.start {
            // A single heartbeat: an instant, and nothing to count.
            continue;
        }
        let mut before = None;
        let mut after = Vec::new();
        while let Some(&last) = out.last() {
            if last.end <= span.start {
                break;
            }
            out.pop();
            if last.end > span.end {
                after.push(Piece { start: last.start.max(span.end), ..last });
            }
            if last.start < span.start {
                before = Some(Piece { end: span.start, ..last });
                break;
            }
        }
        out.extend(before);
        out.push(Piece { span: i, start: span.start, end: span.end });
        after.reverse();
        out.extend(after);
    }
    out
}

fn cuts(pieces: &[Piece]) -> Vec<(Timestamp, Timestamp)> {
    pieces.iter().map(|p| (p.start, p.end)).collect()
}

/// `pieces` with every stretch in `cuts` taken out. Both sorted and
/// non-overlapping.
fn subtract(pieces: &[Piece], cuts: &[(Timestamp, Timestamp)]) -> Vec<Piece> {
    let mut out = Vec::with_capacity(pieces.len());
    let mut first = 0;
    for piece in pieces {
        // A cut that ends before this piece starts ends before every later
        // piece starts too.
        while first < cuts.len() && cuts[first].1 <= piece.start {
            first += 1;
        }
        let mut start = piece.start;
        for &(cut_start, cut_end) in &cuts[first..] {
            if cut_start >= piece.end {
                break;
            }
            if cut_start > start {
                out.push(Piece { start, end: cut_start, ..*piece });
            }
            start = start.max(cut_end);
            if start >= piece.end {
                break;
            }
        }
        if start < piece.end {
            out.push(Piece { start, ..*piece });
        }
    }
    out
}

// ---- adding up --------------------------------------------------------------

/// The runs a query asks about: its devices, its category, its item.
fn chosen(lookup: &Lookup, spans: &[ScreenSpan], query: &ScreenQuery) -> Vec<Resolved> {
    let refs: Vec<&ScreenSpan> = spans
        .iter()
        .filter(|s| query.devices.is_empty() || query.devices.contains(&s.device))
        .collect();
    let item = query.item.as_deref().map(|k| k.trim().to_lowercase());
    reconcile(lookup, &refs)
        .into_iter()
        .filter(|r| !r.ignored)
        .map(|r| r.resolved)
        .filter(|r| query.category.is_none() || r.category == query.category)
        .filter(|r| item.as_deref().is_none_or(|wanted| item_matches(&r.key, wanted)))
        .collect()
}

/// Does a run's key answer a filter's? The same key, or -- for a site --
/// a host of the registrable domain asked about, when its hosts are kept
/// apart.
fn item_matches(key: &str, wanted: &str) -> bool {
    if key == wanted {
        return true;
    }
    match (key.strip_prefix("site:"), wanted.strip_prefix("site:")) {
        (Some(host), Some(domain)) => domain::registrable(host) == domain,
        _ => false,
    }
}

fn clip(
    start: Timestamp,
    end: Timestamp,
    from: Timestamp,
    to: Timestamp,
) -> Option<(Timestamp, Timestamp)> {
    let (start, end) = (start.max(from), end.min(to));
    (start < end).then_some((start, end))
}

fn between(start: Timestamp, end: Timestamp) -> i64 {
    (end.as_microsecond() - start.as_microsecond()).max(0)
}

fn secs(micros: i64) -> u64 {
    ((micros.max(0) + MICROS / 2) / MICROS) as u64
}

/// Category totals, largest first, the uncategorised last among equals.
fn totals(by: HashMap<Option<ScreenCategoryId>, i64>) -> Vec<CategoryTotal> {
    let mut out: Vec<CategoryTotal> = by
        .into_iter()
        .filter(|(_, micros)| *micros > 0)
        .map(|(category, micros)| CategoryTotal { category, secs: secs(micros) })
        .collect();
    out.sort_by(|a, b| {
        b.secs
            .cmp(&a.secs)
            .then_with(|| a.category.is_none().cmp(&b.category.is_none()))
            .then_with(|| a.category.cmp(&b.category))
    });
    out
}

/// Call `each(date, hour, microseconds)` for every wall-clock hour of `tz`
/// that `start..end` touches, with how much of it falls there.
///
/// Steps from one local hour's start to the next by the wall clock and asks
/// the zone when that is, so a skipped hour is never visited and a repeated
/// one is visited once, for all of its time.
fn walk(start: Timestamp, end: Timestamp, tz: &TimeZone, mut each: impl FnMut(Date, i8, i64)) {
    let mut at = start;
    while at < end {
        let local = tz.to_datetime(at);
        let next = local
            .date()
            .at(local.hour(), 0, 0, 0)
            .checked_add(jiff::Span::new().hours(1))
            .ok()
            .and_then(|hour| tz.to_timestamp(hour).ok())
            .filter(|next| *next > at)
            .unwrap_or_else(|| {
                Timestamp::from_microsecond(at.as_microsecond().saturating_add(3_600 * MICROS))
                    .unwrap_or(Timestamp::MAX)
            });
        let stop = next.min(end);
        each(local.date(), local.hour(), between(at, stop));
        at = stop;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ScreenSpanId;
    use crate::screentime::defaults::starter_categories;
    use jiff::civil::date;

    const LA: &str = "America/Los_Angeles";

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn span(
        device: &str,
        kind: SpanKind,
        app: &str,
        host: &str,
        from: &str,
        to: &str,
    ) -> ScreenSpan {
        ScreenSpan {
            id: ScreenSpanId::new(),
            device: device.into(),
            collector: if kind == SpanKind::Site { "firefox".into() } else { "gnome".into() },
            kind,
            app: app.into(),
            name: String::new(),
            host: host.into(),
            title: format!("{app} {host}"),
            start: ts(from),
            end: ts(to),
        }
    }

    fn app(app: &str, from: &str, to: &str) -> ScreenSpan {
        span("laptop", SpanKind::App, app, "", from, to)
    }

    fn site(host: &str, from: &str, to: &str) -> ScreenSpan {
        span("laptop", SpanKind::Site, "Firefox", host, from, to)
    }

    struct World {
        apps: Vec<ScreenApp>,
        categories: Vec<ScreenCategory>,
        settings: ScreenTimeSettings,
    }

    impl World {
        fn new() -> Self {
            World {
                apps: Vec::new(),
                categories: starter_categories(),
                settings: ScreenTimeSettings::default(),
            }
        }

        fn labels(&self) -> Labels<'_> {
            Labels { apps: &self.apps, categories: &self.categories, settings: &self.settings }
        }

        fn category(&self, builtin: &str) -> ScreenCategoryId {
            self.categories.iter().find(|c| c.builtin == builtin).unwrap().id
        }
    }

    fn secs_of(runs: &[Resolved], key: &str) -> i64 {
        runs.iter().filter(|r| r.key == key).map(|r| between(r.start, r.end)).sum::<i64>() / MICROS
    }

    fn query(from: Date, to: Date, tz: &str) -> ScreenQuery {
        ScreenQuery { from, to, tz: tz.into(), devices: Vec::new(), category: None, item: None }
    }

    // ---- zone and bounds -----------------------------------------------------

    #[test]
    fn a_zone_is_named_or_the_systems() {
        assert!(zone("").is_ok());
        assert!(zone("Europe/London").is_ok());
        assert!(matches!(zone("Mars/Olympus_Mons"), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_days_bounds_are_its_first_instants() {
        let tz = zone(LA).unwrap();
        let (from, to) = day_bounds(date(2026, 11, 5), date(2026, 11, 5), &tz);
        assert_eq!(from, ts("2026-11-05T08:00:00Z"));
        assert_eq!(to, ts("2026-11-06T08:00:00Z"));
        // The day the clocks went back is 25 hours long.
        let (from, to) = day_bounds(date(2026, 11, 1), date(2026, 11, 1), &tz);
        assert_eq!(between(from, to) / MICROS, 25 * 3600);
        // And the day they went forward, 23.
        let (from, to) = day_bounds(date(2026, 3, 8), date(2026, 3, 8), &tz);
        assert_eq!(between(from, to) / MICROS, 23 * 3600);
    }

    // ---- flattening and reconciling -----------------------------------------

    #[test]
    fn where_two_apps_overlap_the_later_one_has_the_overlap() {
        let world = World::new();
        let spans = vec![
            app("code", "2026-11-05T10:00:00Z", "2026-11-05T11:00:00Z"),
            app("slack", "2026-11-05T10:10:00Z", "2026-11-05T10:20:00Z"),
        ];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "app:code"), 50 * 60, "code keeps both sides");
        assert_eq!(secs_of(&runs, "app:slack"), 10 * 60);
        let keys: Vec<&str> = runs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["app:code", "app:slack", "app:code"]);
    }

    #[test]
    fn a_later_span_cutting_across_several_keeps_them_in_order() {
        let world = World::new();
        let spans = vec![
            app("a", "2026-11-05T10:00:00Z", "2026-11-05T10:30:00Z"),
            app("b", "2026-11-05T10:05:00Z", "2026-11-05T10:10:00Z"),
            app("c", "2026-11-05T10:06:00Z", "2026-11-05T10:20:00Z"),
        ];
        let runs = resolve(&spans, &world.labels());
        let laid: Vec<(&str, i64)> =
            runs.iter().map(|r| (r.key.as_str(), between(r.start, r.end) / MICROS / 60)).collect();
        assert_eq!(laid, [("app:a", 5), ("app:b", 1), ("app:c", 14), ("app:a", 10)]);
        for pair in runs.windows(2) {
            assert!(pair[0].end <= pair[1].start, "one device's runs never overlap");
        }
    }

    #[test]
    fn sites_take_the_browsers_time_and_the_rest_is_the_browsers_own() {
        let world = World::new();
        let spans = vec![
            app("firefox", "2026-11-05T10:00:00Z", "2026-11-05T11:00:00Z"),
            site("github.com", "2026-11-05T10:00:00Z", "2026-11-05T10:20:00Z"),
            site("www.youtube.com", "2026-11-05T10:30:00Z", "2026-11-05T10:40:00Z"),
        ];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "site:github.com"), 20 * 60);
        assert_eq!(secs_of(&runs, "site:youtube.com"), 10 * 60);
        assert_eq!(secs_of(&runs, "app:firefox"), 30 * 60);
        assert!(runs.iter().filter(|r| r.key == "app:firefox").all(|r| r.browser_rest));
        let total: i64 = runs.iter().map(|r| between(r.start, r.end)).sum::<i64>() / MICROS;
        assert_eq!(total, 3600, "no second counted twice");
    }

    #[test]
    fn a_site_does_not_count_while_another_app_has_focus() {
        let world = World::new();
        let spans = vec![
            app("firefox", "2026-11-05T10:00:00Z", "2026-11-05T10:10:00Z"),
            app("code", "2026-11-05T10:10:00Z", "2026-11-05T10:30:00Z"),
            // The extension went on reporting the tab while the editor had
            // focus -- a collector being wrong is the report's to absorb.
            site("github.com", "2026-11-05T10:00:00Z", "2026-11-05T10:30:00Z"),
        ];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "site:github.com"), 10 * 60);
        assert_eq!(secs_of(&runs, "app:code"), 20 * 60);
        assert_eq!(secs_of(&runs, "app:firefox"), 0);
    }

    #[test]
    fn a_site_counts_when_no_desktop_collector_was_reporting() {
        let world = World::new();
        let spans = vec![site("github.com", "2026-11-05T10:00:00Z", "2026-11-05T10:30:00Z")];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "site:github.com"), 30 * 60);
    }

    #[test]
    fn without_the_extension_a_browser_is_one_app() {
        let world = World::new();
        let spans = vec![app("firefox", "2026-11-05T10:00:00Z", "2026-11-05T11:00:00Z")];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "app:firefox"), 3600);
        assert!(!runs[0].browser_rest);
        assert_eq!(runs[0].name, "Firefox");
    }

    #[test]
    fn a_browser_named_in_settings_is_one() {
        let mut world = World::new();
        world.settings.browsers = vec!["My-Browser".into()];
        let spans = vec![
            app("my-browser", "2026-11-05T10:00:00Z", "2026-11-05T11:00:00Z"),
            site("github.com", "2026-11-05T10:00:00Z", "2026-11-05T10:20:00Z"),
        ];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "site:github.com"), 20 * 60);
        assert_eq!(secs_of(&runs, "app:my-browser"), 40 * 60);
    }

    #[test]
    fn devices_are_never_reconciled_against_each_other() {
        let world = World::new();
        let spans = vec![
            app("code", "2026-11-05T10:00:00Z", "2026-11-05T11:00:00Z"),
            span(
                "desktop",
                SpanKind::App,
                "code",
                "",
                "2026-11-05T10:00:00Z",
                "2026-11-05T11:00:00Z",
            ),
            span(
                "desktop",
                SpanKind::Site,
                "Firefox",
                "github.com",
                "2026-11-05T10:00:00Z",
                "2026-11-05T10:30:00Z",
            ),
        ];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(secs_of(&runs, "app:code"), 7200, "two machines in use are two");
        assert_eq!(
            secs_of(&runs, "site:github.com"),
            0,
            "the desktop's editor had focus there, whatever the laptop did"
        );
    }

    #[test]
    fn labels_name_categorise_split_and_ignore() {
        let mut world = World::new();
        let social = world.category("social");
        let work = world.category("work");
        let mut code = ScreenApp::new("app:code", SpanKind::App);
        code.name = "Editor".into();
        code.category = Some(social);
        let mut lock = ScreenApp::new("app:gnome-shell", SpanKind::App);
        lock.ignored = true;
        let mut google = ScreenApp::new("site:google.com", SpanKind::Site);
        google.split = true;
        google.category = Some(work);
        let mut youtube = ScreenApp::new("site:youtube.com", SpanKind::Site);
        youtube.uncategorized = true;
        world.apps = vec![code.clone(), lock, google, youtube];

        let spans = vec![
            app("Code", "2026-11-05T09:00:00Z", "2026-11-05T09:30:00Z"),
            app("gnome-shell", "2026-11-05T09:30:00Z", "2026-11-05T09:40:00Z"),
            site("mail.google.com", "2026-11-05T10:00:00Z", "2026-11-05T10:10:00Z"),
            site("docs.google.com", "2026-11-05T10:10:00Z", "2026-11-05T10:20:00Z"),
            site("youtube.com", "2026-11-05T10:20:00Z", "2026-11-05T10:30:00Z"),
            site("github.com", "2026-11-05T10:30:00Z", "2026-11-05T10:40:00Z"),
        ];
        let runs = resolve(&spans, &world.labels());
        let by_key: HashMap<&str, &Resolved> = runs.iter().map(|r| (r.key.as_str(), r)).collect();

        let code_run = by_key["app:code"];
        assert_eq!(code_run.name, "Editor");
        assert_eq!(code_run.category, Some(social));
        assert!(!code_run.suggested);
        assert_eq!(code_run.app_id, Some(code.id));

        assert!(!by_key.contains_key("app:gnome-shell"), "ignored items are dropped");

        assert!(!by_key.contains_key("site:google.com"), "split: hosts stand alone");
        let docs = by_key["site:docs.google.com"];
        assert_eq!(docs.name, "docs.google.com");
        assert_eq!(docs.category, Some(work), "a host follows its domain's label");
        assert_eq!(docs.app_id, None);
        assert!(by_key.contains_key("site:mail.google.com"));

        assert_eq!(by_key["site:youtube.com"].category, None, "uncategorised on purpose");

        let github = by_key["site:github.com"];
        assert_eq!(github.category, Some(work));
        assert!(github.suggested);
    }

    #[test]
    fn a_suggestion_needs_its_starter_category_to_still_be_there() {
        let mut world = World::new();
        world.categories.retain(|c| c.builtin != "work");
        let spans = vec![app("code", "2026-11-05T09:00:00Z", "2026-11-05T09:30:00Z")];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(runs[0].category, None);
        assert!(!runs[0].suggested);
    }

    #[test]
    fn a_label_pointing_at_a_deleted_category_falls_back_to_the_suggestion() {
        let mut world = World::new();
        let mut code = ScreenApp::new("app:code", SpanKind::App);
        code.category = Some(ScreenCategoryId::new());
        world.apps = vec![code];
        let spans = vec![app("code", "2026-11-05T09:00:00Z", "2026-11-05T09:30:00Z")];
        let runs = resolve(&spans, &world.labels());
        assert_eq!(runs[0].category, Some(world.category("work")));
        assert!(runs[0].suggested);
    }

    #[test]
    fn a_name_comes_from_the_label_the_collector_a_friendly_name_or_the_key() {
        let world = World::new();
        let mut named = app("org.example.thing", "2026-11-05T09:00:00Z", "2026-11-05T09:10:00Z");
        named.name = "Thing".into();
        let spans = vec![
            named,
            app("org.gnome.Ptyxis", "2026-11-05T09:10:00Z", "2026-11-05T09:20:00Z"),
            app("mystery", "2026-11-05T09:20:00Z", "2026-11-05T09:30:00Z"),
        ];
        let names: Vec<String> =
            resolve(&spans, &world.labels()).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["Thing", "Terminal", "mystery"]);
    }

    // ---- build ----------------------------------------------------------------

    #[test]
    fn a_report_adds_up_by_item_category_day_hour_and_device() {
        let world = World::new();
        let tz = zone("UTC").unwrap();
        let spans = vec![
            app("code", "2026-11-05T09:00:00Z", "2026-11-05T10:30:00Z"),
            app("slack", "2026-11-05T10:30:00Z", "2026-11-05T11:00:00Z"),
            span(
                "desktop",
                SpanKind::App,
                "spotify",
                "",
                "2026-11-06T20:00:00Z",
                "2026-11-06T20:15:00Z",
            ),
        ];
        let previous = vec![app("code", "2026-11-03T09:00:00Z", "2026-11-03T09:10:00Z")];
        let report = build(
            &query(date(2026, 11, 5), date(2026, 11, 6), "UTC"),
            &tz,
            &spans,
            &previous,
            &world.labels(),
        );
        assert_eq!(report.total_secs, 2 * 3600 + 15 * 60);
        let items: Vec<(&str, u64)> =
            report.items.iter().map(|i| (i.key.as_str(), i.secs)).collect();
        assert_eq!(items, [("app:code", 5400), ("app:slack", 1800), ("app:spotify", 900)]);
        assert_eq!(report.items[0].name, "Visual Studio Code");
        assert!(report.items[0].suggested);

        assert_eq!(report.days.len(), 2);
        assert_eq!(report.days[0].secs, 7200);
        assert_eq!(report.days[1].secs, 900);
        assert_eq!(report.days[0].categories[0].category, Some(world.category("work")));
        assert_eq!(report.days[0].categories[0].secs, 5400);

        assert_eq!(report.hours.len(), 24);
        assert_eq!(report.hours[9], 3600);
        assert_eq!(report.hours[10], 3600);
        assert_eq!(report.hours[20], 900);

        let devices: Vec<(&str, u64)> =
            report.devices.iter().map(|d| (d.device.as_str(), d.secs)).collect();
        assert_eq!(devices, [("laptop", 7200), ("desktop", 900)]);

        assert_eq!(report.categories[0].category, Some(world.category("work")));
        assert_eq!(report.previous_total_secs, 600, "Nov 3-4 is the range before Nov 5-6");
    }

    #[test]
    fn a_report_clips_runs_to_its_range_and_lists_empty_days() {
        let world = World::new();
        let tz = zone("UTC").unwrap();
        let spans = vec![app("code", "2026-11-04T23:00:00Z", "2026-11-05T01:00:00Z")];
        let report = build(
            &query(date(2026, 11, 5), date(2026, 11, 7), "UTC"),
            &tz,
            &spans,
            &[],
            &world.labels(),
        );
        assert_eq!(report.total_secs, 3600);
        assert_eq!(report.days.iter().map(|d| d.secs).collect::<Vec<_>>(), [3600, 0, 0]);
        assert!(report.days[1].categories.is_empty());
    }

    #[test]
    fn a_run_across_midnight_is_split_between_the_days_in_the_queries_zone() {
        let world = World::new();
        let tz = zone(LA).unwrap();
        // 23:30 to 00:30 in Los Angeles, on the night of Nov 4.
        let spans = vec![app("code", "2026-11-05T07:30:00Z", "2026-11-05T08:30:00Z")];
        let report = build(
            &query(date(2026, 11, 4), date(2026, 11, 5), LA),
            &tz,
            &spans,
            &[],
            &world.labels(),
        );
        assert_eq!(report.days[0].secs, 1800);
        assert_eq!(report.days[1].secs, 1800);
        assert_eq!(report.hours[23], 1800);
        assert_eq!(report.hours[0], 1800);
    }

    #[test]
    fn the_night_the_clocks_go_back_has_two_one_oclocks_in_one_bucket() {
        let world = World::new();
        let tz = zone(LA).unwrap();
        // 00:30 PDT to 02:30 PST on Nov 1 2026: three real hours.
        let spans = vec![app("code", "2026-11-01T07:30:00Z", "2026-11-01T10:30:00Z")];
        let report = build(
            &query(date(2026, 11, 1), date(2026, 11, 1), LA),
            &tz,
            &spans,
            &[],
            &world.labels(),
        );
        assert_eq!(report.total_secs, 3 * 3600);
        assert_eq!(report.days[0].secs, 3 * 3600);
        assert_eq!(report.hours[0], 1800);
        assert_eq!(report.hours[1], 2 * 3600, "01:00 happened twice");
        assert_eq!(report.hours[2], 1800);
    }

    #[test]
    fn the_night_the_clocks_go_forward_has_no_two_oclock() {
        let world = World::new();
        let tz = zone(LA).unwrap();
        // 01:30 PST to 03:30 PDT on Mar 8 2026: one real hour.
        let spans = vec![app("code", "2026-03-08T09:30:00Z", "2026-03-08T10:30:00Z")];
        let report = build(
            &query(date(2026, 3, 8), date(2026, 3, 8), LA),
            &tz,
            &spans,
            &[],
            &world.labels(),
        );
        assert_eq!(report.total_secs, 3600);
        assert_eq!(report.hours[1], 1800);
        assert_eq!(report.hours[2], 0, "02:00 never happened");
        assert_eq!(report.hours[3], 1800);
    }

    #[test]
    fn a_report_narrowed_to_a_category_an_item_or_a_device() {
        let mut world = World::new();
        let mut google = ScreenApp::new("site:google.com", SpanKind::Site);
        google.split = true;
        world.apps = vec![google];
        let tz = zone("UTC").unwrap();
        let spans = vec![
            app("code", "2026-11-05T09:00:00Z", "2026-11-05T10:00:00Z"),
            site("mail.google.com", "2026-11-05T10:00:00Z", "2026-11-05T10:10:00Z"),
            site("docs.google.com", "2026-11-05T10:10:00Z", "2026-11-05T10:30:00Z"),
            span(
                "desktop",
                SpanKind::App,
                "spotify",
                "",
                "2026-11-05T12:00:00Z",
                "2026-11-05T12:15:00Z",
            ),
        ];
        let day = date(2026, 11, 5);

        let mut q = query(day, day, "UTC");
        q.category = Some(world.category("entertainment"));
        let report = build(&q, &tz, &spans, &[], &world.labels());
        assert_eq!(report.total_secs, 900);

        let mut q = query(day, day, "UTC");
        q.item = Some("site:google.com".into());
        let report = build(&q, &tz, &spans, &[], &world.labels());
        assert_eq!(report.total_secs, 1800, "a split domain takes in its hosts");
        assert_eq!(report.items.len(), 2);

        let mut q = query(day, day, "UTC");
        q.devices = vec!["desktop".into()];
        let report = build(&q, &tz, &spans, &[], &world.labels());
        assert_eq!(report.total_secs, 900);
    }

    // ---- timeline, rows, devices, minutes ---------------------------------------

    #[test]
    fn a_timeline_is_one_day_clipped_and_joined_and_titled_only_when_kept() {
        let mut world = World::new();
        let tz = zone("UTC").unwrap();
        let spans = vec![
            app("code", "2026-11-04T23:30:00Z", "2026-11-05T00:30:00Z"),
            // A second collector on the same machine, inside the first.
            ScreenSpan {
                collector: "x11".into(),
                ..app("code", "2026-11-05T00:10:00Z", "2026-11-05T00:20:00Z")
            },
            app("slack", "2026-11-05T09:00:00Z", "2026-11-05T09:30:00Z"),
            span(
                "desktop",
                SpanKind::App,
                "spotify",
                "",
                "2026-11-05T09:10:00Z",
                "2026-11-05T09:20:00Z",
            ),
        ];
        let timeline = timeline(date(2026, 11, 5), &tz, &spans, &[], &world.labels());
        let runs: Vec<(&str, &str)> =
            timeline.runs.iter().map(|r| (r.key.as_str(), r.device.as_str())).collect();
        assert_eq!(
            runs,
            [("app:code", "laptop"), ("app:slack", "laptop"), ("app:spotify", "desktop")]
        );
        assert_eq!(timeline.runs[0].start, ts("2026-11-05T00:00:00Z"));
        assert_eq!(timeline.runs[0].end, ts("2026-11-05T00:30:00Z"));
        assert_eq!(timeline.runs[1].title, "slack ");

        world.settings.keep_titles = false;
        let only_laptop =
            super::timeline(date(2026, 11, 5), &tz, &spans, &["laptop".into()], &world.labels());
        assert_eq!(only_laptop.runs.len(), 2);
        assert!(only_laptop.runs.iter().all(|r| r.title.is_empty()));
    }

    #[test]
    fn app_rows_list_seen_items_by_use_with_hosts_then_unseen_labels_by_name() {
        let mut world = World::new();
        let mut ignored = ScreenApp::new("app:gnome-shell", SpanKind::App);
        ignored.ignored = true;
        let mut zebra = ScreenApp::new("app:zebra", SpanKind::App);
        zebra.name = "Zebra".into();
        let aardvark = ScreenApp::new("app:aardvark", SpanKind::App);
        let mut social = ScreenApp::new("app:code", SpanKind::App);
        social.category = Some(world.category("social"));
        world.apps = vec![ignored, zebra, aardvark, social];
        let spans = vec![
            app("code", "2026-11-05T09:00:00Z", "2026-11-05T10:00:00Z"),
            app("gnome-shell", "2026-11-05T10:00:00Z", "2026-11-05T10:01:00Z"),
            site("mail.google.com", "2026-11-05T11:00:00Z", "2026-11-05T11:10:00Z"),
            site("docs.google.com", "2026-11-05T11:10:00Z", "2026-11-05T11:30:00Z"),
        ];
        let rows = app_rows(&spans, &world.labels());
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["app:code", "site:google.com", "app:gnome-shell", "app:aardvark", "app:zebra"]
        );
        assert_eq!(rows[0].suggested, None, "the label chose");
        assert!(rows[0].label.is_some());
        assert_eq!(rows[1].hosts, ["docs.google.com", "mail.google.com"]);
        assert_eq!(rows[1].recent_secs, 1800);
        assert_eq!(rows[1].suggested, Some(world.category("utilities")));
        assert_eq!(rows[2].recent_secs, 60, "ignored items are still listed");
        assert_eq!(rows[3].recent_secs, 0);
        assert_eq!(rows[3].last_seen, None);
    }

    #[test]
    fn a_split_domain_has_its_own_row_and_one_per_host() {
        let mut world = World::new();
        let mut google = ScreenApp::new("site:google.com", SpanKind::Site);
        google.split = true;
        world.apps = vec![google];
        let spans = vec![
            site("mail.google.com", "2026-11-05T11:00:00Z", "2026-11-05T11:10:00Z"),
            site("docs.google.com", "2026-11-05T11:10:00Z", "2026-11-05T11:30:00Z"),
        ];
        let rows = app_rows(&spans, &world.labels());
        let keys: Vec<(&str, u64)> = rows.iter().map(|r| (r.key.as_str(), r.recent_secs)).collect();
        assert_eq!(
            keys,
            [
                ("site:google.com", 1800),
                ("site:docs.google.com", 1200),
                ("site:mail.google.com", 600)
            ]
        );
        assert!(rows[0].label.as_ref().is_some_and(|l| l.split));
        assert_eq!(rows[0].hosts, ["docs.google.com", "mail.google.com"]);
    }

    #[test]
    fn devices_are_listed_most_recent_first_with_every_collector() {
        let spans = vec![
            app("code", "2026-11-05T09:00:00Z", "2026-11-05T10:00:00Z"),
            site("github.com", "2026-11-05T10:00:00Z", "2026-11-05T10:05:00Z"),
            span(
                "desktop",
                SpanKind::App,
                "code",
                "",
                "2026-11-05T11:00:00Z",
                "2026-11-05T11:30:00Z",
            ),
        ];
        let devices = devices(&spans);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].device, "desktop");
        assert_eq!(devices[1].device, "laptop");
        assert_eq!(devices[1].collectors, ["firefox", "gnome"]);
        assert_eq!(devices[1].last_seen, ts("2026-11-05T10:05:00Z"));
    }

    #[test]
    fn minutes_by_day_counts_a_category_or_an_item_and_leaves_empty_days_out() {
        let world = World::new();
        let tz = zone("UTC").unwrap();
        let spans = vec![
            app("code", "2026-11-05T09:00:00Z", "2026-11-05T09:30:00Z"),
            site("youtube.com", "2026-11-05T10:00:00Z", "2026-11-05T10:45:00Z"),
            site("youtube.com", "2026-11-07T23:50:00Z", "2026-11-08T00:10:00Z"),
        ];
        let labels = world.labels();
        let (from, to) = (date(2026, 11, 5), date(2026, 11, 7));

        let all = minutes_by_day(&spans, &labels, &tz, from, to, None, None);
        assert_eq!(all.get(&date(2026, 11, 5)), Some(&75.0));
        assert_eq!(all.get(&date(2026, 11, 6)), None);
        assert_eq!(all.get(&date(2026, 11, 7)), Some(&10.0), "clipped to the range");
        assert_eq!(all.len(), 2);

        let fun = minutes_by_day(
            &spans,
            &labels,
            &tz,
            from,
            to,
            Some(world.category("entertainment")),
            None,
        );
        assert_eq!(fun.get(&date(2026, 11, 5)), Some(&45.0));

        let code = minutes_by_day(&spans, &labels, &tz, from, to, None, Some("app:code"));
        assert_eq!(code.len(), 1);
        assert_eq!(code.get(&date(2026, 11, 5)), Some(&30.0));
    }

    #[test]
    fn subtracting_leaves_the_gaps_between_cuts() {
        let t = |m: i64| ts("2026-11-05T00:00:00Z") + jiff::SignedDuration::from_mins(m);
        let pieces = [
            Piece { span: 0, start: t(0), end: t(60) },
            Piece { span: 1, start: t(70), end: t(80) },
        ];
        let cuts = [(t(10), t(20)), (t(30), t(75))];
        let left: Vec<(i64, i64, usize)> = subtract(&pieces, &cuts)
            .iter()
            .map(|p| {
                (between(t(0), p.start) / MICROS / 60, between(t(0), p.end) / MICROS / 60, p.span)
            })
            .collect();
        assert_eq!(left, [(0, 10, 0), (20, 30, 0), (75, 80, 1)]);
    }
}
