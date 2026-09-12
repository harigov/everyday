//! The journal, as a folder of dated Markdown.
//!
//! ```text
//!   journal/
//!     Personal/
//!       _journal.md                    the journal itself: name, colour, icon
//!       2026-09-10-morning-pages-a1b2c3d4.md
//!     media/
//!       a1b2c3d4-IMG_0042.jpg
//! ```
//!
//! One file per entry, named so that a plain alphabetical listing is
//! chronological, in the folder of the journal it belongs to. The short id on
//! the end is what keeps two entries written on the same day about the same
//! thing from being one file.

use super::doc;
use super::index::Namer;
use crate::text::{Csv, FrontMatter, safe_name};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::model::{Entry, Journal, Location, Weather};
use everyday_core::store::JournalStore;
use everyday_core::{EntryId, JournalId, Result};
use std::collections::BTreeMap;

pub struct JournalPart;
pub static JOURNAL: JournalPart = JournalPart;

static SPEC: Spec = Spec {
    id: "journal",
    label: "Journal",
    summary: "Every entry, in the journal it was written in, with its date, its tags and \
              the photographs and video in it.",
    format: "Markdown with YAML front matter, one file per entry",
    media: true,
    imports: true,
};

/// The file in a journal's folder that describes the journal rather than a
/// day in it. Named so it sorts above the entries, which all begin with a
/// year.
const JOURNAL_FILE: &str = "_journal.md";

impl Portable for JournalPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        // Journals are the one domain every backend must carry, so this is
        // never `None`. Counted from the store's own statistics rather than
        // by listing, which would decrypt every entry to answer "how many".
        Ok(Some(store.stats()?.entries))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, opts: &Options) -> Result<()> {
        let journals = store.list_journals()?;
        let mut folders: BTreeMap<JournalId, String> = BTreeMap::new();
        let mut namer = Namer::new();

        for journal in &journals {
            // Two journals may share a name, and after `safe_name` more of
            // them may. A folder per journal that silently merged two of them
            // would put one journal's entries in another's.
            let base = safe_name(&journal.name);
            let folder = namer.unique(&base, |b, n| format!("{b}-{n}"));
            out.text(&format!("{folder}/{JOURNAL_FILE}"), render_journal(journal))?;
            folders.insert(journal.id, folder);
        }

        let entries = store.all_entries()?;
        for entry in &entries {
            let folder = folders.get(&entry.journal_id).cloned().unwrap_or_else(|| {
                // An entry whose journal has gone. It still exists and is
                // still somebody's writing, so it is exported rather than
                // dropped, into a folder that says what happened.
                "Unfiled".to_string()
            });
            let name = format!(
                "{folder}/{}-{}-{}.md",
                entry.local_date,
                safe_name(&entry.display_title()),
                entry.id.short()
            );
            let written = doc::write(
                store,
                out,
                opts,
                front_matter(entry, &folder),
                doc::Doc {
                    heading: &entry.display_title(),
                    body: &entry.body,
                    attachments: &entry.attachments,
                    depth: 1,
                },
            )?;
            out.records(&name, written.markdown, 1)?;
            out.text(&format!("{}{}.json", doc::SIDECAR, entry.id), written.sidecar)?;
        }

        if !entries.is_empty() {
            out.text("index.csv", index(&entries, &folders))?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);

        // Journals first: an entry names the journal it belongs to, and one
        // that arrives before it would be filed in the wrong place and stay
        // there.
        let mut by_folder: BTreeMap<String, JournalId> = BTreeMap::new();
        for (name, body) in src.documents(".md") {
            if !name.ends_with(JOURNAL_FILE) {
                continue;
            }
            match read_journal(store, name, body, mode, &mut report) {
                Ok(id) => {
                    let folder = name.trim_end_matches(JOURNAL_FILE).trim_end_matches('/');
                    by_folder.insert(folder.to_string(), id);
                }
                Err(e) => report.problem(name, e),
            }
        }

        // Somewhere for an entry whose folder had no `_journal.md` -- which is
        // what a folder of Markdown from another program looks like.
        let fallback = || -> Result<JournalId> {
            Ok(match store.list_journals()?.first() {
                Some(journal) => journal.id,
                None => {
                    let journal = Journal::new("Journal");
                    store.put_journal(&journal)?;
                    journal.id
                }
            })
        };

        for (name, body) in src.documents(".md") {
            if name.ends_with(JOURNAL_FILE) {
                continue;
            }
            let folder = name.rsplit_once('/').map_or("", |(dir, _)| dir);
            let journal_id = match by_folder.get(folder) {
                Some(id) => *id,
                None => {
                    let id = fallback()?;
                    by_folder.insert(folder.to_string(), id);
                    id
                }
            };
            match read_entry(store, src, name, body, journal_id, mode, &mut report) {
                Ok(()) => {}
                Err(e) => report.problem(name, e),
            }
        }
        Ok(report)
    }
}

fn render_journal(journal: &Journal) -> String {
    let mut front = FrontMatter::new();
    front
        .always("name", &journal.name)
        .always("id", journal.id.to_string())
        .set("color", &journal.color)
        .set("icon", &journal.icon)
        .set_opt("order", Some(journal.sort_order))
        .set("created", doc::stamp(journal.created_at))
        .set("updated", doc::stamp(journal.updated_at));
    format!("{}# {}\n\n{}\n", front.render(), journal.name, journal.description)
}

fn read_journal(
    store: &dyn JournalStore,
    name: &str,
    source: &str,
    mode: Mode,
    report: &mut Report,
) -> Result<JournalId> {
    let (fields, body) = crate::text::split_front_matter(source);
    let id = fields.parse::<JournalId>("id").unwrap_or_else(JournalId::new);
    let title = fields
        .get("name")
        .map(str::to_string)
        .unwrap_or_else(|| safe_name(name.trim_end_matches(JOURNAL_FILE)).replace('-', " "));

    let existing = store.get_journal(id).ok();
    let mut landing = land(existing, || Journal::new(&title), mode);
    let Some(journal) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(id);
    };

    journal.id = id;
    journal.name = title;
    journal.description = body.trim_start_matches("# ").trim_start().to_string();
    if let Some(line) = journal.description.split_once('\n') {
        // The heading the exporter wrote is the name, and it is already a
        // field. What follows it is the description.
        if line.0.trim() == journal.name {
            journal.description = line.1.trim().to_string();
        }
    }
    if let Some(color) = fields.get("color") {
        journal.color = color.to_string();
    }
    if let Some(icon) = fields.get("icon") {
        journal.icon = icon.to_string();
    }
    if let Some(order) = fields.parse("order") {
        journal.sort_order = order;
    }
    store.put_journal(journal)?;
    report.landed(&landing);
    Ok(id)
}

fn front_matter(entry: &Entry, journal: &str) -> FrontMatter {
    let mut front = FrontMatter::new();
    front
        .set("title", &entry.title)
        .always("date", entry.local_date.to_string())
        .always("id", entry.id.to_string())
        .always("journal", journal)
        .always("journalId", entry.journal_id.to_string())
        .list("tags", &entry.tags)
        .flag("starred", entry.starred)
        .set("timezone", &entry.tz)
        .set("purpose", doc::purpose_text(entry.purpose.as_ref()))
        .set("created", doc::stamp(entry.created_at))
        .set("updated", doc::stamp(entry.updated_at));
    if let Some(place) = &entry.location {
        front
            .set("place", place.place_name.clone().unwrap_or_default())
            .set("locality", place.locality.clone().unwrap_or_default())
            .set("country", place.country.clone().unwrap_or_default())
            // One field rather than two, in the order every mapping tool
            // writes a coordinate pair, so it can be pasted straight in.
            .set("coordinates", format!("{}, {}", place.latitude, place.longitude));
    }
    if let Some(weather) = &entry.weather {
        front.set("weather", format!("{}, {}\u{b0}C", weather.condition, weather.temperature_c));
    }
    front
}

fn read_entry(
    store: &dyn JournalStore,
    src: &Part<'_>,
    name: &str,
    source: &str,
    journal_id: JournalId,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let read = doc::read(store, src, name, source)?;
    let fields = &read.fields;
    let id = fields.parse::<EntryId>("id").unwrap_or_else(EntryId::new);

    let date = fields
        .parse::<jiff::civil::Date>("date")
        // A file the exporter named keeps its date in the name, so a note
        // whose front matter was stripped by a tool still lands on the right
        // day. Failing that, today.
        .or_else(|| date_from_name(name))
        .unwrap_or_else(|| jiff::Zoned::now().date());

    let now = jiff::Timestamp::now();
    let tz = fields.get("timezone").unwrap_or("UTC").to_string();

    let existing = store.get_entry(id).ok();
    let mut landing = land(existing, || Entry::new(journal_id, &tz), mode);
    let Some(entry) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(());
    };
    entry.id = id;
    // The folder decides, unless the front matter names a journal that is
    // actually here. An entry whose journal was deleted before the export was
    // written to `Unfiled/` on purpose, and taking its stated `journalId` on
    // trust would file it under a journal that does not exist -- invisible in
    // every list, and unreachable without editing the database.
    entry.journal_id = fields
        .parse::<JournalId>("journalId")
        .filter(|id| store.get_journal(*id).is_ok())
        .unwrap_or(journal_id);
    entry.title = read.title;
    entry.body = read.body;
    entry.attachments = read.attachments;
    entry.local_date = date;
    entry.tags = fields.list("tags");
    entry.starred = fields.flag("starred");
    // An export written before entries lost their pin may still carry
    // `pinned:`. It is read past rather than acted on: an unknown front
    // matter field has never been an import failure, and the record it
    // would set no longer exists.
    entry.purpose = doc::parse_purpose(&fields.text("purpose"));
    if let Some(tz) = fields.get("timezone") {
        entry.tz = tz.to_string();
    }
    entry.location = location(fields);
    entry.weather = weather(fields);
    entry.created_at = fields.parse("created").unwrap_or(entry.created_at);
    entry.updated_at = fields.parse("updated").unwrap_or(now);

    // A vault that already holds this entry is asked to replace it
    // unconditionally: the conditional save exists to catch two *editors*
    // racing, and an import is neither of them.
    store.put_entry(entry)?;
    report.landed(&landing);
    Ok(())
}

/// The date at the front of an exported file name.
fn date_from_name(name: &str) -> Option<jiff::civil::Date> {
    let file = name.rsplit('/').next()?;
    file.get(..10)?.parse().ok()
}

fn location(fields: &crate::text::Fields) -> Option<Location> {
    let raw = fields.get("coordinates")?;
    let (lat, lon) = raw.split_once(',')?;
    Some(Location {
        latitude: lat.trim().parse().ok()?,
        longitude: lon.trim().parse().ok()?,
        place_name: fields.get("place").map(str::to_string),
        locality: fields.get("locality").map(str::to_string),
        country: fields.get("country").map(str::to_string),
    })
}

fn weather(fields: &crate::text::Fields) -> Option<Weather> {
    let raw = fields.get("weather")?;
    let (condition, temp) = raw.rsplit_once(',')?;
    Some(Weather {
        temperature_c: temp.trim().trim_end_matches("\u{b0}C").trim().parse().ok()?,
        condition: condition.trim().to_string(),
        icon: None,
    })
}

/// One row per entry: where it is, when it was written, how long it is.
///
/// Nothing reads this back, which is exactly why it can be the shape a
/// spreadsheet wants rather than the shape an importer needs. It is the file
/// somebody opens to ask "how much did I write in 2024" without installing
/// anything.
fn index(entries: &[Entry], journals: &BTreeMap<JournalId, String>) -> String {
    let mut csv = Csv::new(&["date", "journal", "title", "tags", "words", "starred", "id"]);
    let mut rows: Vec<&Entry> = entries.iter().collect();
    rows.sort_by_key(|e| (e.local_date, e.created_at));
    for entry in rows {
        csv.row(&[
            entry.local_date.to_string(),
            journals.get(&entry.journal_id).cloned().unwrap_or_default(),
            entry.display_title(),
            entry.tags.join(", "),
            entry.body.plain_text().split_whitespace().count().to_string(),
            entry.starred.to_string(),
            entry.id.to_string(),
        ]);
    }
    csv.finish()
}
