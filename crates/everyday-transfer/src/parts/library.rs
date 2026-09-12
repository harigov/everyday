//! The library, as a spreadsheet per shelf.
//!
//! ```text
//!   library/
//!     Books.csv           one row per book, one column per field
//!     Films.csv
//!     shelves.csv         what a shelf is: its verbs, its fields, its icon
//!     log.csv             what you did with them, and when
//!     media/              cover art
//! ```
//!
//! # Why CSV and not Markdown
//!
//! Because a shelf is a table and a diary is not. Every program that holds a
//! list of books -- Goodreads, StoryGraph, Letterboxd, a spreadsheet somebody
//! has kept since 2009 -- imports and exports CSV, and one file per shelf is
//! what makes an Every Day shelf openable by them and theirs openable here.
//!
//! # A shelf's own columns
//!
//! A [`Kind`] carries its own fields -- an ISBN for books, a director for
//! films -- so the header is per shelf rather than fixed. Those columns come
//! last, after the ones every shelf has, so that a file opened in a
//! spreadsheet reads left to right from the general to the particular. An
//! importer matches on the header, so a column moved or deleted by hand is a
//! column that is missing rather than a file that is wrong.

use super::doc;
use super::index::{Namer, index_map, owner};
use crate::text::{Csv, Table, safe_name};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::library::{Item, ItemStatus, Kind, LogEntry, LogEvent, Progress};
use everyday_core::store::JournalStore;
use everyday_core::store::library::{ItemQuery, LibraryStore, LogQuery};
use everyday_core::{ItemId, KindId, LogId, Result};
use std::collections::BTreeMap;

pub struct LibraryPart;
pub static LIBRARY: LibraryPart = LibraryPart;

static SPEC: Spec = Spec {
    id: "library",
    label: "Library",
    summary: "Everything on every shelf — what you have read, watched, played and cooked \
              — with your ratings, your notes and the log of when.",
    format: "CSV, one file per shelf, plus cover art",
    media: true,
    imports: true,
};

const SHELVES: &str = "shelves.csv";
const LOG: &str = "log.csv";

/// The columns every shelf has, whatever is on it.
const COMMON: &[&str] = &[
    "title",
    "creator",
    "subtitle",
    "year",
    "status",
    "rating",
    "favourite",
    "started",
    "finished",
    "progress",
    "total",
    "unit",
    "tags",
    "purpose",
    "summary",
    "notes",
    "cover",
    "cover_url",
    "links",
    "source",
    "shelf",
    "id",
];

const SHELF_COLUMNS: &[&str] = &[
    "name",
    "file",
    "singular",
    "slug",
    "icon",
    "color",
    "wishlist_verb",
    "active_verb",
    "done_verb",
    "log_verb",
    "fields",
    "progress_unit",
    "visible",
    "order",
    "id",
];

const LOG_COLUMNS: &[&str] =
    &["date", "item", "shelf", "event", "rating", "position", "minutes", "note", "item_id", "id"];

impl Portable for LibraryPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(library) = store.library() else { return Ok(None) };
        Ok(Some(library.list_items(&ItemQuery::default())?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, opts: &Options) -> Result<()> {
        let Some(library) = store.library() else { return Ok(()) };
        let kinds = library.list_kinds()?;
        let items = library.list_items(&ItemQuery::default())?;
        let names: BTreeMap<KindId, String> =
            kinds.iter().map(|k| (k.id, k.name.clone())).collect();

        let mut shelves = Csv::new(SHELF_COLUMNS);
        let mut namer = Namer::new();
        for kind in &kinds {
            // Two shelves may share a name, and after `safe_name` more of them
            // may -- "Books" and "Books!" reduce to the same file. Writing
            // both to it would silently replace the first shelf's entire
            // contents with the second's, which is the worst kind of bug this
            // crate can have: an export that looks complete and is not.
            let base = safe_name(&kind.name);
            let stem = namer.unique(&base, |b, _| format!("{b}-{}", kind.id.short()));
            let file = format!("{stem}.csv");
            shelves.row(&[
                kind.name.clone(),
                file.clone(),
                kind.singular.clone(),
                kind.slug.clone(),
                kind.icon.clone(),
                kind.color.clone(),
                kind.verbs.wishlist.clone(),
                kind.verbs.active.clone(),
                kind.verbs.done.clone(),
                kind.verbs.log.clone(),
                // `key:label:type` each, which is enough to rebuild the shelf
                // and readable enough to edit.
                kind.fields
                    .iter()
                    .map(|f| format!("{}:{}:{}", f.key, f.label, f.field_type.as_str()))
                    .collect::<Vec<_>>()
                    .join("; "),
                kind.progress_unit.clone(),
                kind.visible.to_string(),
                kind.sort_order.to_string(),
                kind.id.to_string(),
            ]);

            let extra: Vec<&str> = kind.fields.iter().map(|f| f.key.as_str()).collect();
            let mut header: Vec<&str> = COMMON.to_vec();
            header.extend(&extra);
            let mut csv = Csv::new(&header);

            let mine: Vec<&Item> = items.iter().filter(|i| i.kind_id == kind.id).collect();
            for item in &mine {
                let cover = match item.cover {
                    Some(blob) => out.media(store, blob, &safe_name(&item.title), opts)?,
                    None => None,
                }
                .unwrap_or_default();
                let mut row = common_row(item, &names, &cover);
                row.extend(
                    extra.iter().map(|key| item.facts.get(*key).cloned().unwrap_or_default()),
                );
                csv.row(&row);
            }
            out.records(&file, csv.finish(), mine.len() as u64)?;
        }
        if shelves.rows() > 0 {
            out.text(SHELVES, shelves.finish())?;
        }

        let logs = library.list_logs(&LogQuery::default())?;
        if !logs.is_empty() {
            let titles: BTreeMap<ItemId, &Item> = items.iter().map(|i| (i.id, i)).collect();
            let mut csv = Csv::new(LOG_COLUMNS);
            for log in &logs {
                let item = titles.get(&log.item_id);
                csv.row(&[
                    log.date.to_string(),
                    item.map(|i| i.title.clone()).unwrap_or_default(),
                    item.and_then(|i| names.get(&i.kind_id).cloned()).unwrap_or_default(),
                    log.event.as_str().to_string(),
                    log.rating.map(|r| r.to_string()).unwrap_or_default(),
                    log.position.map(|p| p.to_string()).unwrap_or_default(),
                    log.minutes.map(|m| m.to_string()).unwrap_or_default(),
                    log.note.clone(),
                    log.item_id.to_string(),
                    log.id.to_string(),
                ]);
            }
            out.records(LOG, csv.finish(), logs.len() as u64)?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(library) = store.library() else {
            report.problem(SPEC.label, "this vault's backend does not store a library");
            return Ok(report);
        };

        // Shelves first: an item names the shelf it is on.
        let mut by_file: BTreeMap<String, KindId> =
            index_map(src.text(SHELVES), |row| match read_shelf(library, row, mode, &mut report) {
                Ok(id) => Some((row.get("file").to_string(), id)),
                Err(e) => {
                    report.problem(SHELVES, e);
                    None
                }
            });

        for (name, text) in src.documents(".csv") {
            if name == SHELVES || name == LOG {
                continue;
            }
            let table = Table::parse(text);
            // A CSV with no `title` column is not a shelf, whatever it is.
            // Saying so beats importing a column of empty books.
            if !table.has("title") {
                report.problem(name, "no `title` column, so this is not a shelf");
                continue;
            }
            let kind = match owner(&mut by_file, name, || {
                shelf_named(library, name.trim_end_matches(".csv"))
            }) {
                Ok(id) => id,
                Err(e) => {
                    report.problem(name, e);
                    continue;
                }
            };
            for row in table.rows() {
                match read_item(store, library, src, &row, kind, mode, &mut report) {
                    Ok(()) => {}
                    Err(e) => report.problem(name, e),
                }
            }
        }

        if let Some(text) = src.text(LOG) {
            for row in Table::parse(text).rows() {
                match read_log(library, &row, mode, &mut report) {
                    Ok(()) => {}
                    Err(e) => report.problem(LOG, e),
                }
            }
        }
        Ok(report)
    }
}

fn common_row(item: &Item, names: &BTreeMap<KindId, String>, cover: &str) -> Vec<String> {
    vec![
        item.title.clone(),
        item.creator.clone(),
        item.subtitle.clone(),
        item.year.map(|y| y.to_string()).unwrap_or_default(),
        item.status.as_str().to_string(),
        item.rating.map(|r| r.to_string()).unwrap_or_default(),
        item.favourite.to_string(),
        item.started_on.map(|d| d.to_string()).unwrap_or_default(),
        item.finished_on.map(|d| d.to_string()).unwrap_or_default(),
        item.progress.as_ref().map(|p| p.position.to_string()).unwrap_or_default(),
        item.progress.as_ref().and_then(|p| p.total).map(|t| t.to_string()).unwrap_or_default(),
        item.progress.as_ref().map(|p| p.unit.clone()).unwrap_or_default(),
        item.tags.join(", "),
        doc::purpose_text(item.purpose.as_ref()),
        item.summary.clone(),
        item.notes.clone(),
        cover.to_string(),
        item.cover_url.clone(),
        item.links.iter().map(|l| format!("{}: {}", l.label, l.url)).collect::<Vec<_>>().join("; "),
        item.source.clone(),
        names.get(&item.kind_id).cloned().unwrap_or_default(),
        item.id.to_string(),
    ]
}

fn read_shelf(
    library: &dyn LibraryStore,
    row: &crate::text::Row<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<KindId> {
    let id = KindId::parse(row.get("id")).unwrap_or_else(|_| KindId::new());
    let name = row.get("name");
    let slug = match row.get("slug") {
        "" => safe_name(name).to_lowercase(),
        slug => slug.to_string(),
    };

    let existing = library.get_kind(id).ok();
    let mut landing = land(existing, || Kind::new(&slug, name, row.get("singular")), mode);
    let Some(kind) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(id);
    };
    kind.id = id;
    kind.name = name.to_string();
    if !row.get("singular").is_empty() {
        kind.singular = row.get("singular").to_string();
    }
    if !row.get("slug").is_empty() {
        kind.slug = row.get("slug").to_string();
    }
    if !row.get("icon").is_empty() {
        kind.icon = row.get("icon").to_string();
    }
    if !row.get("color").is_empty() {
        kind.color = row.get("color").to_string();
    }
    for (verb, value) in [
        (&mut kind.verbs.wishlist, row.get("wishlist_verb")),
        (&mut kind.verbs.active, row.get("active_verb")),
        (&mut kind.verbs.done, row.get("done_verb")),
        (&mut kind.verbs.log, row.get("log_verb")),
    ] {
        if !value.is_empty() {
            *verb = value.to_string();
        }
    }
    let fields = row.get("fields");
    if !fields.trim().is_empty() {
        kind.fields = fields.split(';').filter_map(parse_field).collect();
    }
    kind.progress_unit = row.get("progress_unit").to_string();
    kind.visible = row.get("visible").is_empty() || row.flag("visible");
    if let Some(order) = row.parse("order") {
        kind.sort_order = order;
    }
    library.put_kind(kind)?;
    report.landed(&landing);
    Ok(id)
}

fn parse_field(spec: &str) -> Option<everyday_core::library::FieldDef> {
    let mut parts = spec.trim().splitn(3, ':');
    let key = parts.next()?.trim();
    if key.is_empty() {
        return None;
    }
    let label = parts.next().unwrap_or(key).trim();
    Some(everyday_core::library::FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        field_type: everyday_core::library::FieldType::parse(parts.next().unwrap_or("text").trim())
            .unwrap_or_default(),
        placeholder: String::new(),
    })
}

/// The shelf a file belongs to when no index named one: an existing shelf of
/// that name, or a new one.
fn shelf_named(library: &dyn LibraryStore, name: &str) -> Result<KindId> {
    let name = name.replace('-', " ");
    if let Some(kind) =
        library.list_kinds()?.into_iter().find(|k| k.name.eq_ignore_ascii_case(&name))
    {
        return Ok(kind.id);
    }
    let singular = name.strip_suffix('s').unwrap_or(&name).to_string();
    let kind = Kind::new(&safe_name(&name).to_lowercase(), &name, &singular);
    library.put_kind(&kind)?;
    Ok(kind.id)
}

fn read_item(
    store: &dyn JournalStore,
    library: &dyn LibraryStore,
    src: &Part<'_>,
    row: &crate::text::Row<'_>,
    kind_id: KindId,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let id = ItemId::parse(row.get("id")).unwrap_or_else(|_| ItemId::new());

    let existing = library.get_item(id).ok();
    let mut landing = land(existing, || Item::new(kind_id, row.get("title")), mode);
    let Some(item) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(());
    };
    item.id = id;
    item.kind_id = kind_id;
    item.title = row.get("title").to_string();
    item.creator = row.get("creator").to_string();
    item.subtitle = row.get("subtitle").to_string();
    item.year = row.parse("year");
    item.status = lenient_status(row.get("status")).unwrap_or(item.status);
    item.rating = row.parse::<u8>("rating").filter(|r| (1..=10).contains(r));
    item.favourite = row.flag("favourite");
    item.started_on = row.parse("started");
    item.finished_on = row.parse("finished");
    item.tags = row.list("tags");
    item.purpose = doc::parse_purpose(row.get("purpose"));
    item.summary = row.get("summary").to_string();
    item.notes = row.get("notes").to_string();
    item.cover_url = row.get("cover_url").to_string();
    item.source = row.get("source").to_string();
    item.progress = row.parse::<u32>("progress").map(|position| Progress {
        position,
        total: row.parse("total"),
        unit: row.get("unit").to_string(),
    });
    item.links = row
        .get("links")
        .split(';')
        .filter_map(|link| {
            let (label, url) = link.split_once(':')?;
            Some(everyday_core::library::Link {
                label: label.trim().to_string(),
                url: url.trim().to_string(),
            })
        })
        .collect();

    // Cover art, when the archive carries it. A missing file is not an error:
    // an export made without attachments still lists the name it would have
    // had, and the URL beside it is enough to fetch it again.
    let cover = row.get("cover");
    if !cover.is_empty()
        && let Some(bytes) = src.get(cover)
    {
        item.cover = store.put_blob(bytes).ok();
    }

    let facts: BTreeMap<String, String> = library
        .get_kind(kind_id)
        .map(|kind| {
            kind.fields
                .iter()
                .filter_map(|field| {
                    let value = row.get(&field.key);
                    (!value.is_empty()).then(|| (field.key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    if !facts.is_empty() {
        item.facts = facts;
    }
    item.updated_at = jiff::Timestamp::now();
    library.put_item(item)?;
    report.landed(&landing);
    Ok(())
}

fn read_log(
    library: &dyn LibraryStore,
    row: &crate::text::Row<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let id = LogId::parse(row.get("id")).unwrap_or_else(|_| LogId::new());
    let existing = library.get_log(id).ok();
    // As in `read_time`: a log row already here is never asked to name a
    // valid item when the mode says to leave it alone, so this check comes
    // before the one that can fail.
    if existing.is_some() && mode == Mode::Skip {
        report.skipped += 1;
        return Ok(());
    }
    let item_id = ItemId::parse(row.get("item_id"))
        .map_err(|_| everyday_core::Error::Invalid("a log row names no item".into()))?;

    // `existing` can only be `None` or a record `mode` allows overwriting --
    // the `Mode::Skip` case was ruled out above, before there was a valid
    // `item_id` to build a fresh entry from.
    let mut landing = land(
        existing,
        || LogEntry {
            id,
            item_id,
            event: LogEvent::Finished,
            date: jiff::Zoned::now().date(),
            tz: "UTC".into(),
            note: String::new(),
            rating: None,
            position: None,
            minutes: None,
            created_at: jiff::Timestamp::now(),
            updated_at: jiff::Timestamp::now(),
        },
        mode,
    );
    let Some(log) = landing.as_mut() else { return Ok(()) };
    log.id = id;
    log.item_id = item_id;
    log.event = LogEvent::parse(&row.get("event").trim().to_ascii_lowercase()).unwrap_or(log.event);
    log.date = row.parse("date").unwrap_or(log.date);
    log.rating = row.parse::<u8>("rating").filter(|r| (1..=10).contains(r));
    log.position = row.parse("position");
    log.minutes = row.parse("minutes");
    log.note = row.get("note").to_string();
    log.updated_at = jiff::Timestamp::now();
    library.put_log(log)?;
    report.landed(&landing);
    Ok(())
}

/// Reads a status from an export with a little more grace than
/// [`ItemStatus::parse`] alone: earlier exports, and files typed by hand, use
/// words a person would reach for — "want", "to-read", "dnf" — that never
/// were the canonical spelling. The alias table is tried first and the
/// canonical parser second, so a wire name added to core needs nothing here.
fn lenient_status(name: &str) -> Option<ItemStatus> {
    match name.trim().to_ascii_lowercase().as_str() {
        "want" | "to-read" => Some(ItemStatus::Wishlist),
        "reading" | "currently-reading" => Some(ItemStatus::Active),
        "read" | "finished" => Some(ItemStatus::Done),
        "dnf" => Some(ItemStatus::Abandoned),
        other => ItemStatus::parse(other),
    }
}
