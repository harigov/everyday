//! Notes, as a flat folder of Markdown.
//!
//! ```text
//!   notes/
//!     Recipes-worth-keeping-9f2a1c0d.md
//!     media/
//! ```
//!
//! Flat rather than foldered, because notes have no journal and no date to
//! file them under: what they have is tags, and those are in the front matter
//! where a tool like Obsidian will find them. This is the part most likely to
//! be pointed at another program, which is why its layout is the plainest one
//! here -- a folder of `.md` files and nothing else.

use super::doc;
use crate::text::{FrontMatter, safe_name};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::note::Note;
use everyday_core::store::JournalStore;
use everyday_core::store::notes::NoteQuery;
use everyday_core::{NoteId, Result};

pub struct NotesPart;
pub static NOTES: NotesPart = NotesPart;

static SPEC: Spec = Spec {
    id: "notes",
    label: "Notes",
    summary: "Every note, with its tags and anything pinned to it. A plain folder of \
              Markdown that drops straight into Obsidian or any editor.",
    format: "Markdown with YAML front matter, one file per note",
    media: true,
    imports: true,
};

impl Portable for NotesPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(notes) = store.notes() else { return Ok(None) };
        // Summaries rather than bodies: the count is the same and the work is
        // not.
        Ok(Some(notes.list_notes(&NoteQuery::default())?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, opts: &Options) -> Result<()> {
        let Some(notes) = store.notes() else { return Ok(()) };
        for note in notes.all_notes()? {
            let mut front = FrontMatter::new();
            front
                .set("title", &note.title)
                .always("id", note.id.to_string())
                .list("tags", &note.tags)
                .flag("pinned", note.pinned)
                .set("purpose", doc::purpose_text(note.purpose.as_ref()))
                .set("created", doc::stamp(note.created_at))
                .set("updated", doc::stamp(note.updated_at));

            let written = doc::write(
                store,
                out,
                opts,
                front,
                doc::Doc {
                    heading: &note.display_title(),
                    body: &note.body,
                    attachments: &note.attachments,
                    depth: 0,
                },
            )?;
            let name = format!("{}-{}.md", safe_name(&note.display_title()), note.id.short());
            out.records(&name, written.markdown, 1)?;
            out.text(&format!("{}{}.json", doc::SIDECAR, note.id), written.sidecar)?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(notes) = store.notes() else {
            report.problem(SPEC.label, "this vault's backend does not store notes");
            return Ok(report);
        };

        for (name, source) in src.documents(".md") {
            match read(store, notes, src, name, source, mode, &mut report) {
                Ok(()) => {}
                Err(e) => report.problem(name, e),
            }
        }
        Ok(report)
    }
}

fn read(
    store: &dyn JournalStore,
    notes: &dyn everyday_core::store::notes::NoteStore,
    src: &Part<'_>,
    name: &str,
    source: &str,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let read = doc::read(store, src, name, source)?;
    let fields = &read.fields;
    let id = fields.parse::<NoteId>("id").unwrap_or_else(NoteId::new);

    let existing = notes.get_note(id).ok();
    let mut landing = land(existing, || Note::new(&read.title), mode);
    let Some(note) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(());
    };
    note.id = id;
    note.title = read.title;
    note.body = read.body;
    note.attachments = read.attachments;
    note.tags = fields.list("tags");
    note.pinned = fields.flag("pinned");
    note.purpose = doc::parse_purpose(&fields.text("purpose"));
    note.created_at = fields.parse("created").unwrap_or(note.created_at);
    note.updated_at = fields.parse("updated").unwrap_or_else(jiff::Timestamp::now);
    notes.put_note(note)?;
    report.landed(&landing);
    Ok(())
}
