//! Screen time: every span recorded, the labels you gave apps and sites, the
//! categories, and the settings.
//!
//! ```text
//!   screentime/
//!     spans.csv         one row per span, oldest first
//!     labels.json       your labels for apps and sites, whole
//!     categories.json   the categories, whole
//!     settings.json     the vault's screen-time settings
//! ```
//!
//! # Spans as a spreadsheet
//!
//! `spans.csv` is what somebody would open to ask their own question of it --
//! how long in the terminal on Tuesdays -- so it is a flat table with a
//! column per field: `id,start,end,device,collector,kind,app,name,host,title`.
//! Times are RFC 3339 instants in UTC, which every spreadsheet and every
//! language reads. The host is the one the browser saw (`mail.google.com`),
//! not the site it is grouped under: grouping is done at report time, and a
//! file that had already grouped would have thrown away what the grouping is
//! made from.
//!
//! # A spreadsheet will open this
//!
//! So a cell that starts the way a formula does -- `=`, `+`, `-`, `@`, a tab
//! or a carriage return, after any leading spaces -- is written with an
//! apostrophe in front, which a spreadsheet shows as text rather than
//! running. A window title is whatever the page or the program chose to call
//! itself, and a page called `=HYPERLINK(...)` is somebody else's idea of a
//! cell. Money's part does the same; this one also reads the files back, so
//! the apostrophe is taken off again on the way in, and a cell that really
//! did begin with an apostrophe before a formula character is given a second
//! one on the way out, so that every cell comes back exactly as it went.
//!
//! # Reading it back
//!
//! Labels, categories and settings merge by id, the way every part's records
//! do: one already here is left alone, or overwritten under
//! [`Mode::Replace`]. Two things are matched by more than id:
//!
//! - A **label** whose id is new but whose key -- `app:firefox`,
//!   `site:github.com` -- this vault already labels is the same label: the
//!   one here keeps its id, and the file's fields go onto it only under
//!   [`Mode::Replace`]. Two labels for one key would disagree about what to
//!   call it.
//! - A **category** whose id is new but which this vault already has under
//!   another id -- the same starter (`work`), or the same name -- is the same
//!   category, and the labels from the file that name it are filed under the
//!   one here. Every vault makes its own starter categories, each with ids of
//!   its own, so without this an archive would arrive with a second Work.
//!
//! Spans merge by id and nothing else, and are never merged *into*: one
//! already here is left as it is unless the file's copy differs and the mode
//! is [`Mode::Replace`]. Re-importing the same archive therefore changes
//! nothing, in either mode.
//!
//! # The stream an imported span is written under
//!
//! Every span in the store carries a *stream* -- a keyed hash of the device
//! and the collector that sent it, which is how the next heartbeat from that
//! collector finds the span to extend. The key belongs to the vault, and an
//! import reaches only the store, so an imported span is written under the
//! stream [`IMPORT_STREAM`] instead. That stream matches no collector's, so
//! no heartbeat will ever extend an imported span: a collector that sends
//! again starts a new one beside it, which is what it would have done
//! anyway, since the archive's spans are over by the time they are read.
//!
//! # What a file is held to
//!
//! What the vault holds a heartbeat to: a span needs a device, a collector,
//! an app, a start no later than its end, at most twelve hours between them,
//! and a host when it is of a site. Over-long text is cut rather than the row
//! refused, as it is when a collector sends it. Titles are dropped on the way
//! in when this vault's settings say not to keep them -- an archive is not a
//! way round a setting.

use crate::text::{Csv, Row, Table};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::screentime::{
    MAX_APP, MAX_COLLECTOR, MAX_DEVICE, MAX_HOST, MAX_NAME, MAX_SPAN_SECS, MAX_TITLE, ScreenApp,
    ScreenCategory, ScreenSpan, ScreenTimeSettings, SpanKind, domain,
};
use karmayoga_core::store::JournalStore;
use karmayoga_core::store::screentime::ScreenTimeStore;
use karmayoga_core::{Error, Result, ScreenCategoryId, ScreenSpanId};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub struct ScreenTimePart;
pub static SCREENTIME: ScreenTimePart = ScreenTimePart;

static SPEC: Spec = Spec {
    id: "screentime",
    label: "Screen time",
    summary: "Every stretch of time an app or a website had your attention, on every device, \
              with its window or tab title when titles are kept; your names and categories for \
              apps and sites; and the settings.",
    format: "CSV for the time, JSON for labels, categories and settings",
    media: false,
    imports: true,
};

const SPANS: &str = "spans.csv";
const LABELS: &str = "labels.json";
const CATEGORIES: &str = "categories.json";
const SETTINGS: &str = "settings.json";

const SPAN_COLUMNS: &[&str] =
    &["id", "start", "end", "device", "collector", "kind", "app", "name", "host", "title"];

/// The stream every imported span is written under. See the module doc.
pub const IMPORT_STREAM: &str = "import";

/// How many spans go to the store in one call. Each call is all or nothing;
/// a year of screen time is a quarter of a million rows, and holding the
/// vault's one writer through all of them would stall every heartbeat that
/// arrived meanwhile. A failure part way leaves the calls before it landed,
/// which a second import completes without duplicating, because spans merge
/// by id.
const SPAN_BATCH: usize = 1_000;

/// After this many rows of `spans.csv` that could not be read, the rest are
/// counted rather than named: a file that is wrong on every line should say
/// so in a sentence, not in ten thousand.
const ROW_PROBLEMS: usize = 20;

#[derive(serde::Serialize)]
struct LabelsOut<'a> {
    labels: &'a [ScreenApp],
}

#[derive(serde::Serialize)]
struct CategoriesOut<'a> {
    categories: &'a [ScreenCategory],
}

/// The two lists, as read. Each record is left as a raw value until it is
/// its turn, so one that cannot be understood costs that record and not the
/// file, as the Feed's sidecar does.
#[derive(serde::Deserialize)]
struct LabelsIn {
    #[serde(default)]
    labels: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct CategoriesIn {
    #[serde(default)]
    categories: Vec<serde_json::Value>,
}

impl Portable for ScreenTimePart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    /// Labels and spans: what a person made, and what was recorded of them.
    /// Not categories, which a vault makes for itself the first time the app
    /// is opened, and which would make an untouched app look used.
    ///
    /// Counting spans opens every one: the store has no count of its own.
    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(screen) = store.screentime() else { return Ok(None) };
        let spans = screen.spans(Timestamp::MIN, Timestamp::MAX)?.len();
        Ok(Some((screen.list_apps()?.len() + spans) as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(screen) = store.screentime() else { return Ok(()) };
        let spans = screen.spans(Timestamp::MIN, Timestamp::MAX)?;
        let mut labels = screen.list_apps()?;
        let categories = screen.list_categories()?;
        let settings = screen.screen_settings()?;
        // The starter categories and the default settings alone are not
        // something anybody made. A category of their own, or a setting
        // changed, is -- so either still writes the folder.
        let made_nothing = spans.is_empty()
            && labels.is_empty()
            && categories.iter().all(|c| !c.builtin.is_empty())
            && settings == ScreenTimeSettings::default();
        if made_nothing {
            return Ok(());
        }
        // By key, so two exports of one vault are the same file.
        labels.sort_by(|a, b| a.key.cmp(&b.key).then(a.id.cmp(&b.id)));

        if !spans.is_empty() {
            let mut csv = Csv::new(SPAN_COLUMNS);
            for span in &spans {
                csv.row(&span_row(span));
            }
            out.records(SPANS, csv.finish(), spans.len() as u64)?;
        }
        if !labels.is_empty() {
            let json = serde_json::to_string_pretty(&LabelsOut { labels: &labels })?;
            out.records(LABELS, format!("{json}\n"), labels.len() as u64)?;
        }
        if !categories.is_empty() {
            let json = serde_json::to_string_pretty(&CategoriesOut { categories: &categories })?;
            out.text(CATEGORIES, format!("{json}\n"))?;
        }
        // Not the title sweep's bookmark: it says how far *this* vault's
        // spans were cleared, and would stop another vault clearing the
        // older ones it imports.
        let settings = ScreenTimeSettings { titles_cleared_to: None, ..settings };
        let json = serde_json::to_string_pretty(&settings)?;
        out.text(SETTINGS, format!("{json}\n"))?;
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(screen) = store.screentime() else {
            report.problem(SPEC.label, "this vault's backend does not store screen time");
            return Ok(report);
        };
        // In this order, because each reads what the one before it wrote:
        // labels name categories, and spans are held to the settings.
        read_settings(screen, src, mode, &mut report)?;
        let categories = read_categories(screen, src, mode, &mut report)?;
        read_labels(screen, src, mode, &categories, &mut report)?;
        read_spans(screen, src, mode, &mut report)?;
        Ok(report)
    }
}

// ---- writing --------------------------------------------------------------

fn span_row(span: &ScreenSpan) -> Vec<String> {
    vec![
        span.id.to_string(),
        span.start.to_string(),
        span.end.to_string(),
        text(&span.device),
        text(&span.collector),
        word(&span.kind),
        text(&span.app),
        text(&span.name),
        text(&span.host),
        text(&span.title),
    ]
}

/// Would a spreadsheet run this cell? See the module doc.
fn runs(value: &str) -> bool {
    let first = value.trim_start_matches(' ').chars().next();
    matches!(first, Some('=' | '+' | '-' | '@' | '\t' | '\r'))
}

/// Does this cell need an apostrophe in front? One that would run does; so
/// does one that already starts with an apostrophe before something that
/// would, or reading it back would take that apostrophe off.
fn needs_guard(value: &str) -> bool {
    runs(value) || value.strip_prefix('\'').is_some_and(needs_guard)
}

/// A cell of somebody's text, made safe to open in a spreadsheet.
fn text(value: &str) -> String {
    if needs_guard(value) { format!("'{value}") } else { value.to_string() }
}

/// [`text`], undone.
fn untext(value: &str) -> String {
    match value.strip_prefix('\'') {
        Some(rest) if needs_guard(rest) => rest.to_string(),
        _ => value.to_string(),
    }
}

/// An enum's own word on the wire -- `app`, `site`.
fn word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

// ---- reading --------------------------------------------------------------

/// Is the vault's own copy of this record there? `None` when it is not.
fn found<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(t) => Ok(Some(t)),
        Err(Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Why these settings cannot be kept, if they cannot. The rules the vault
/// holds them to when they are saved through the app, restated here because
/// an import writes to the store directly.
fn unusable_settings(settings: &ScreenTimeSettings) -> Option<&'static str> {
    if !(1..=60).contains(&settings.idle_minutes) {
        return Some("idle minutes must be between 1 and 60");
    }
    if settings.title_days > 3650 {
        return Some("titles can be kept for at most ten years");
    }
    if settings.browsers.len() > 50 || settings.browsers.iter().any(|b| b.chars().count() > 200) {
        return Some("at most 50 browsers, each of at most 200 characters");
    }
    None
}

/// The file's settings, over these when this vault has never been told
/// anything (the defaults are what it would answer), or under
/// [`Mode::Replace`].
fn read_settings(
    screen: &dyn ScreenTimeStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let Some(text) = src.text(SETTINGS) else { return Ok(()) };
    let settings: ScreenTimeSettings = match serde_json::from_str(text) {
        // This vault's own sweep bookmark is kept whatever the file says.
        Ok(settings) => ScreenTimeSettings {
            titles_cleared_to: screen.screen_settings()?.titles_cleared_to,
            ..settings
        },
        Err(e) => {
            report.problem(SETTINGS, e);
            return Ok(());
        }
    };
    if let Some(why) = unusable_settings(&settings) {
        report.problem(SETTINGS, why);
        return Ok(());
    }
    let here = screen.screen_settings()?;
    if here == settings {
        report.skipped += 1;
        return Ok(());
    }
    let existing = (here != ScreenTimeSettings::default()).then_some(here);
    let mut landing = land(existing, || settings.clone(), mode);
    let Some(target) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(());
    };
    *target = settings;
    screen.put_screen_settings(target)?;
    report.landed(&landing);
    Ok(())
}

/// The categories from the file, landed. Answers where each of the file's
/// category ids now lives in this vault -- its own id, or the id of the one
/// here it turned out to be -- for the labels that name them.
fn read_categories(
    screen: &dyn ScreenTimeStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<HashMap<ScreenCategoryId, ScreenCategoryId>> {
    let mut map = HashMap::new();
    let Some(text) = src.text(CATEGORIES) else { return Ok(map) };
    let unread: CategoriesIn = match serde_json::from_str(text) {
        Ok(unread) => unread,
        Err(e) => {
            report.problem(CATEGORIES, e);
            return Ok(map);
        }
    };
    let mut have = screen.list_categories()?;
    for (n, value) in unread.categories.into_iter().enumerate() {
        let category = match serde_json::from_value::<ScreenCategory>(value) {
            Ok(category) => category,
            Err(e) => {
                report.problem(CATEGORIES, format!("category {} could not be read: {e}", n + 1));
                continue;
            }
        };
        if category.name.trim().is_empty() {
            report.problem(CATEGORIES, format!("category {} has no name", n + 1));
            continue;
        }
        let existing = found(screen.get_category(category.id))?;
        if existing.is_none()
            && let Some(same) = have.iter().find(|other| same_category(other, &category))
        {
            // Already here under another id: the file's labels are filed
            // under this one.
            map.insert(category.id, same.id);
            report.skipped += 1;
            continue;
        }
        map.insert(category.id, category.id);
        let mut landing = land(existing, || category.clone(), mode);
        let Some(target) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        *target = category;
        screen.put_category(target)?;
        have.retain(|other| other.id != target.id);
        have.push(target.clone());
        report.landed(&landing);
    }
    Ok(map)
}

/// The same starter, or the same name without regard to case or spaces.
fn same_category(a: &ScreenCategory, b: &ScreenCategory) -> bool {
    (!a.builtin.is_empty() && a.builtin == b.builtin)
        || a.name.trim().to_lowercase() == b.name.trim().to_lowercase()
}

/// Why a label from the file cannot be kept, if it cannot.
fn unusable_label(label: &ScreenApp) -> Option<&'static str> {
    let kind = match label.key.split_once(':') {
        Some(("app", rest)) if !rest.trim().is_empty() => SpanKind::App,
        Some(("site", rest)) if !rest.trim().is_empty() => SpanKind::Site,
        Some(("app" | "site", _)) => return Some("a label's key names nothing"),
        _ => return Some("a label's key must start with app: or site:"),
    };
    if kind != label.kind {
        return Some("a label's kind does not match its key");
    }
    if label.name.chars().count() > 200 {
        return Some("a label's name may be at most 200 characters");
    }
    None
}

fn read_labels(
    screen: &dyn ScreenTimeStore,
    src: &Part<'_>,
    mode: Mode,
    categories: &HashMap<ScreenCategoryId, ScreenCategoryId>,
    report: &mut Report,
) -> Result<()> {
    let Some(text) = src.text(LABELS) else { return Ok(()) };
    let unread: LabelsIn = match serde_json::from_str(text) {
        Ok(unread) => unread,
        Err(e) => {
            report.problem(LABELS, e);
            return Ok(());
        }
    };
    let known: BTreeSet<ScreenCategoryId> =
        screen.list_categories()?.into_iter().map(|c| c.id).collect();
    let mut have: BTreeMap<String, ScreenApp> =
        screen.list_apps()?.into_iter().map(|a| (a.key.clone(), a)).collect();

    for (n, value) in unread.labels.into_iter().enumerate() {
        let mut label = match serde_json::from_value::<ScreenApp>(value) {
            Ok(label) => label,
            Err(e) => {
                report.problem(LABELS, format!("label {} could not be read: {e}", n + 1));
                continue;
            }
        };
        if let Some(why) = unusable_label(&label) {
            report.problem(LABELS, format!("{}: {why}", label.key));
            continue;
        }
        // Filed under where its category landed; one that is in neither the
        // file nor this vault names nothing, and is let go.
        label.category = label
            .category
            .map(|id| categories.get(&id).copied().unwrap_or(id))
            .filter(|id| known.contains(id));

        let by_key = have.get(&label.key).cloned();
        let existing = match found(screen.get_app(label.id))? {
            Some(existing) => {
                if by_key.as_ref().is_some_and(|other| other.id != existing.id) {
                    // Writing it would give one key two labels.
                    report.problem(
                        LABELS,
                        format!("{}: this vault already labels it under another id", label.key),
                    );
                    continue;
                }
                Some(existing)
            }
            // The same key under another id: the one here keeps its id.
            None => by_key,
        };
        if let Some(existing) = &existing {
            label.id = existing.id;
        }
        let mut landing = land(existing, || label.clone(), mode);
        let Some(target) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        let old_key = std::mem::replace(target, label).key;
        screen.put_app(target)?;
        if old_key != target.key {
            have.remove(&old_key);
        }
        have.insert(target.key.clone(), target.clone());
        report.landed(&landing);
    }
    Ok(())
}

/// Text from a cell, its apostrophe taken off, trimmed and cut to `max`
/// characters, as the vault cuts what a collector sends.
fn cell(row: &Row<'_>, column: &str, max: usize) -> String {
    untext(row.get(column)).trim().chars().take(max).collect()
}

/// One row of `spans.csv` as a span, or why it is not one.
fn read_span(row: &Row<'_>, keep_titles: bool) -> std::result::Result<ScreenSpan, String> {
    let id = match row.get("id").trim() {
        // A row somebody typed in by hand: new, and new again each time the
        // file is read, since there is nothing to know it by.
        "" => ScreenSpanId::new(),
        id => id.parse::<ScreenSpanId>().map_err(|e| format!("{id:?} is not an id: {e}"))?,
    };
    let time = |column: &str| -> std::result::Result<Timestamp, String> {
        let raw = row.get(column).trim();
        raw.parse().map_err(|e| format!("{column} {raw:?} is not a time: {e}"))
    };
    let (start, end) = (time("start")?, time("end")?);
    if end < start {
        return Err("it ends before it starts".into());
    }
    if end.duration_since(start) > SignedDuration::from_secs(MAX_SPAN_SECS) {
        return Err("it is longer than one span may be".into());
    }
    let kind = match row.get("kind").trim().to_ascii_lowercase().as_str() {
        "app" => SpanKind::App,
        "site" => SpanKind::Site,
        other => return Err(format!("{other:?} is not app or site")),
    };
    let span = ScreenSpan {
        id,
        device: cell(row, "device", MAX_DEVICE),
        collector: cell(row, "collector", MAX_COLLECTOR),
        kind,
        app: cell(row, "app", MAX_APP),
        name: cell(row, "name", MAX_NAME),
        host: match kind {
            SpanKind::App => String::new(),
            SpanKind::Site => domain::normalize_host(&cell(row, "host", MAX_HOST)),
        },
        title: if keep_titles { cell(row, "title", MAX_TITLE) } else { String::new() },
        start,
        end,
    };
    if span.device.is_empty() || span.collector.is_empty() || span.app.is_empty() {
        return Err("a span needs a device, a collector and an app".into());
    }
    if span.kind == SpanKind::Site && span.host.is_empty() {
        return Err("a site needs a host".into());
    }
    Ok(span)
}

fn read_spans(
    screen: &dyn ScreenTimeStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let Some(text) = src.text(SPANS) else { return Ok(()) };
    let table = Table::parse(text);
    if !(table.has("start") && table.has("end")) {
        report.problem(SPANS, "this file has no start and end columns");
        return Ok(());
    }
    let keep_titles = screen.screen_settings()?.keep_titles;

    let mut spans = Vec::new();
    let mut seen = BTreeSet::new();
    let mut unread = 0usize;
    // Line 1 is the header.
    for (n, row) in table.rows().enumerate() {
        match read_span(&row, keep_titles) {
            Ok(span) if !seen.insert(span.id) => {
                report.problem(SPANS, format!("row {}: {} is already in the file", n + 2, span.id));
            }
            Ok(span) => spans.push(span),
            Err(why) => {
                unread += 1;
                if unread <= ROW_PROBLEMS {
                    report.problem(SPANS, format!("row {}: {why}", n + 2));
                }
            }
        }
    }
    if unread > ROW_PROBLEMS {
        report.problem(
            SPANS,
            format!("{} more rows could not be read either", unread - ROW_PROBLEMS),
        );
    }
    let (Some(first), Some(last)) =
        (spans.iter().map(|s| s.start).min(), spans.iter().map(|s| s.end).max())
    else {
        return Ok(());
    };

    // What is here already, over the file's whole reach -- widened by a
    // second each way so that a span of no length at either edge, which
    // overlaps nothing, is still found.
    let second = SignedDuration::from_secs(1);
    let from = first.checked_sub(second).unwrap_or(first);
    let to = last.checked_add(second).unwrap_or(last);
    let here: HashMap<ScreenSpanId, ScreenSpan> =
        screen.spans(from, to)?.into_iter().map(|s| (s.id, s)).collect();

    let mut write = Vec::new();
    let mut counts = (0u64, 0u64, 0u64);
    for span in spans {
        match here.get(&span.id) {
            // The same span: nothing to do in either mode, so importing one
            // archive twice changes nothing.
            Some(existing) if *existing == span => counts.2 += 1,
            Some(_) if mode == Mode::Skip => counts.2 += 1,
            Some(_) => {
                counts.1 += 1;
                write.push(span);
            }
            None => {
                counts.0 += 1;
                write.push(span);
            }
        }
    }
    for batch in write.chunks(SPAN_BATCH) {
        screen.put_spans(IMPORT_STREAM, batch)?;
    }
    // Counted once everything has landed: a batch that failed has returned
    // above, and the report it would have given is replaced by the error.
    report.added += counts.0;
    report.replaced += counts.1;
    report.skipped += counts.2;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_that_would_run_as_a_formula_is_written_as_text() {
        for risky in ["=cmd|' /C calc'!A0", "+SUM(1+1)", "-2+3", "@cmd", "\t=1", "\r=1", "  =1"] {
            assert!(text(risky).starts_with('\''), "{risky:?}");
        }
        for plain in ["github.com", "Terminal", "", "1-800-FLOWERS", "Café", "'quoted'"] {
            assert_eq!(text(plain), plain);
        }
    }

    #[test]
    fn every_cell_comes_back_exactly_as_it_went() {
        for value in [
            "=1", "'=1", "''=1", "  =1", "' =1", "'", "''", "'quoted'", "plain", "", "-", "'-",
            "@home",
        ] {
            assert_eq!(untext(&text(value)), value, "{value:?} -> {:?}", text(value));
        }
    }

    #[test]
    fn kinds_are_written_in_the_words_the_app_uses() {
        assert_eq!(word(&SpanKind::App), "app");
        assert_eq!(word(&SpanKind::Site), "site");
    }

    #[test]
    fn a_starter_or_a_name_already_here_is_the_same_category() {
        let mut work = ScreenCategory::new("Work");
        work.builtin = "work".into();
        let mut renamed = ScreenCategory::new("Job");
        renamed.builtin = "work".into();
        assert!(same_category(&work, &renamed), "the same starter, renamed");
        assert!(same_category(&ScreenCategory::new(" social "), &ScreenCategory::new("Social")));
        assert!(!same_category(&ScreenCategory::new("Games"), &ScreenCategory::new("Gaming")));
        // Two of the person's own, both with no starter, are not the same
        // because neither has one.
        assert!(!same_category(&ScreenCategory::new("A"), &ScreenCategory::new("B")));
    }

    #[test]
    fn a_label_from_a_file_is_held_to_the_rules_the_app_holds_it_to() {
        assert_eq!(unusable_label(&ScreenApp::new("app:firefox", SpanKind::App)), None);
        assert_eq!(unusable_label(&ScreenApp::new("site:github.com", SpanKind::Site)), None);
        assert!(unusable_label(&ScreenApp::new("site:github.com", SpanKind::App)).is_some());
        assert!(unusable_label(&ScreenApp::new("web:github.com", SpanKind::Site)).is_some());
        assert!(unusable_label(&ScreenApp::new("app: ", SpanKind::App)).is_some());
    }
}
