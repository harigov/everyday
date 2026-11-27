//! Notes: writing that is not a day.
//!
//! A journal entry is filed under a date, in a journal, and that is most of
//! what it is for -- the question it answers is "what happened on the
//! fourteenth". A note answers a different one. It is a recipe, a reading
//! list, a draft of a difficult message, the notes from a call, the plan for
//! a trip. It has a title because you will look for it by name, and it has no
//! date because the day it was written is not how anybody finds it again.
//!
//! Everything else it shares with an entry, deliberately: the same
//! [`RichDoc`] body, so the same editor draws it and the same media pipeline
//! takes a photograph dropped into it; the same tags; the same optional
//! [`Purpose`]; the same plain-text projection feeding the same search index.
//! The whole record is a hundred lines because the interesting parts were all
//! written once already.
//!
//! It is also where the assistant puts prose. A routine that reads the week
//! and has three paragraphs to say has nowhere good to put them otherwise: an
//! entry would file a report under a day as though somebody had lived it, and
//! three hundred and sixty-five of those are not a journal.

use crate::id::{BlobId, FileId, NoteId};
use crate::model::{Attachment, MediaKind};
use crate::purpose::Purpose;
use crate::richtext::RichDoc;
use crate::timestamped::Timestamped;
use crate::{Error, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Longest title we will store. Past this it is a body.
pub const MAX_TITLE_BYTES: usize = 500;

/// Who started a note: the person, the assistant, or a recorded call.
///
/// Kept so the list can tell the three apart at a glance. A routine's
/// weekly report and the notes from a call are not something the person
/// wrote, and a list that draws them exactly like a recipe somebody typed
/// leaves them to work out which is which from the title.
///
/// It says who *started* the note and never changes after. Editing the
/// assistant's report does not make it yours, any more than tidying a
/// meeting's notes makes it not a meeting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum NoteOrigin {
    /// The person wrote it, or imported it.
    #[default]
    Own,
    /// The assistant wrote it: a routine's report, a research job's answer,
    /// a note it was asked to make.
    Assistant,
    /// The meeting-notes pipeline wrote it from a recording.
    Meeting,
}

impl NoteOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteOrigin::Own => "own",
            NoteOrigin::Assistant => "assistant",
            NoteOrigin::Meeting => "meeting",
        }
    }

    /// The best reading of a note saved before notes recorded who started
    /// them, from the tags the two writers that are not the person always
    /// put on what they make: the meeting pipeline's `meeting`, and a
    /// research job's `research`. Everything else is taken to be the
    /// person's -- including a routine's report from before then, which
    /// carried no mark of its own.
    pub fn guess(tags: &[String]) -> Self {
        let has = |want: &str| tags.iter().any(|t| t.eq_ignore_ascii_case(want));
        if has("meeting") {
            NoteOrigin::Meeting
        } else if has("research") {
            NoteOrigin::Assistant
        } else {
            NoteOrigin::Own
        }
    }
}

impl std::str::FromStr for NoteOrigin {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim() {
            "own" => Ok(NoteOrigin::Own),
            "assistant" => Ok(NoteOrigin::Assistant),
            "meeting" => Ok(NoteOrigin::Meeting),
            other => Err(Error::Invalid(format!("{other:?} is not a note origin"))),
        }
    }
}

/// A note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: NoteId,
    /// May be empty, in which case the interface derives a heading from the
    /// first line of the body -- the same rule an entry follows.
    #[serde(default)]
    pub title: String,
    pub body: RichDoc,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Kept at the top of the list. There is no starring here as well: a note
    /// list is short and one kind of emphasis is enough.
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    /// Media referenced by the body, in document order.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Who started it. `None` on a note saved before this was kept, which
    /// [`Note::origin`] reads from its tags instead -- see
    /// [`NoteOrigin::guess`]. Left `None` when such a note is saved back:
    /// only a writer that knows sets it, so a wrong guess is never kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<NoteOrigin>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Note {
    pub fn new(title: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: NoteId::new(),
            title: title.into(),
            body: RichDoc::empty(),
            tags: Vec::new(),
            pinned: false,
            purpose: None,
            attachments: Vec::new(),
            origin: Some(NoteOrigin::Own),
            created_at: now,
            updated_at: now,
        }
    }

    /// A note with a body already in it, which is how the assistant makes one.
    pub fn written(title: impl Into<String>, markdown: &str) -> Self {
        Self { body: RichDoc::from_markdown(markdown), ..Self::new(title) }
    }

    /// The same, started by someone other than the person: the assistant,
    /// or a recorded call.
    pub fn written_by(origin: NoteOrigin, title: impl Into<String>, markdown: &str) -> Self {
        Self { origin: Some(origin), ..Self::written(title, markdown) }
    }

    /// Who started it, read from its tags for a note older than the field.
    pub fn origin(&self) -> NoteOrigin {
        self.origin.unwrap_or_else(|| NoteOrigin::guess(&self.tags))
    }

    pub fn validate(&self) -> Result<()> {
        if self.title.len() > MAX_TITLE_BYTES {
            return Err(Error::Invalid(format!(
                "a note title must be under {MAX_TITLE_BYTES} bytes"
            )));
        }
        Ok(())
    }

    /// The heading to show: the title if there is one, else the first line of
    /// the body, else a placeholder.
    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        let text = self.body.plain_text();
        match text.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => crate::model::truncate_on_char_boundary(line, 120),
            None => "Untitled note".to_string(),
        }
    }

    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str(&self.title);
        out.push('\n');
        out.push_str(&self.body.plain_text());
        for tag in &self.tags {
            out.push('\n');
            out.push_str(tag);
        }
        for att in &self.attachments {
            if !att.caption.is_empty() {
                out.push('\n');
                out.push_str(&att.caption);
            }
        }
        out
    }

    pub fn word_count(&self) -> u32 {
        self.body.plain_text().split_whitespace().count() as u32
    }

    /// What a list row needs, so drawing one never loads a document and its
    /// media. The same trade [`crate::model::EntrySummary`] makes.
    pub fn summarize(&self) -> NoteSummary {
        let plain = self.body.plain_text();
        let excerpt = plain
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            // Skip the first line when it is standing in for the title, or
            // every row would say its own heading twice.
            .skip(usize::from(self.title.trim().is_empty()))
            .collect::<Vec<_>>()
            .join(" ");
        NoteSummary {
            id: self.id,
            title: self.display_title(),
            excerpt: crate::model::truncate_on_char_boundary(excerpt.trim(), 240),
            tags: self.tags.clone(),
            pinned: self.pinned,
            purpose: self.purpose,
            word_count: plain.split_whitespace().count() as u32,
            attachment_count: self.attachments.len() as u32,
            cover: self.attachments.iter().find(|a| a.kind == MediaKind::Image).map(|a| a.blob),
            cover_file: self.body.first_image_file(),
            origin: Some(self.origin()),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl Timestamped for Note {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// A row in the note list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: NoteId,
    pub title: String,
    pub excerpt: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    pub word_count: u32,
    pub attachment_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<BlobId>,
    /// The first picture in the body that is kept in the files catalog, for
    /// a note whose pictures are files rather than attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_file: Option<FileId>,
    /// Who started the note. `None` only on a summary sealed before this was
    /// kept; [`crate::Vault::notes`] fills it in from the tags, as
    /// [`NoteSummary::origin`] does, so every row handed out carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<NoteOrigin>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl NoteSummary {
    /// Who started the note, read from its tags for a row older than the
    /// field -- the same reading [`Note::origin`] makes.
    pub fn origin(&self) -> NoteOrigin {
        self.origin.unwrap_or_else(|| NoteOrigin::guess(&self.tags))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_with_no_title_borrows_its_first_line() {
        let note = Note::written("", "Buy oat milk\nand the good coffee");
        assert_eq!(note.display_title(), "Buy oat milk");
        assert_eq!(
            note.summarize().excerpt,
            "and the good coffee",
            "the line standing in for the title must not be repeated under it"
        );
    }

    #[test]
    fn a_note_with_a_title_keeps_its_whole_body_in_the_excerpt() {
        let note = Note::written("Shopping", "Buy oat milk\nand the good coffee");
        assert_eq!(note.display_title(), "Shopping");
        assert_eq!(note.summarize().excerpt, "Buy oat milk and the good coffee");
    }

    #[test]
    fn an_empty_note_still_has_something_to_call_itself() {
        assert_eq!(Note::new("").display_title(), "Untitled note");
    }

    #[test]
    fn a_title_longer_than_a_title_is_refused() {
        let mut note = Note::new("fine");
        note.validate().unwrap();
        note.title = "x".repeat(MAX_TITLE_BYTES + 1);
        assert!(note.validate().is_err(), "past a point it is a body, not a title");
    }

    #[test]
    fn a_note_says_who_started_it() {
        assert_eq!(Note::new("mine").origin(), NoteOrigin::Own);
        let report = Note::written_by(NoteOrigin::Assistant, "Week", "Eleven hours on the deck.");
        assert_eq!(report.origin(), NoteOrigin::Assistant);
        assert_eq!(report.summarize().origin, Some(NoteOrigin::Assistant));
    }

    #[test]
    fn an_older_note_is_read_from_the_tags_its_writer_left() {
        // Saved before the field existed: no `origin` in the payload at all.
        let mut json = serde_json::to_value(Note::written("Design sync", "x")).unwrap();
        json.as_object_mut().unwrap().remove("origin");
        json["tags"] = serde_json::json!(["meeting", "design-sync"]);
        let old: Note = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(old.origin, None);
        assert_eq!(old.origin(), NoteOrigin::Meeting);
        assert_eq!(old.summarize().origin, Some(NoteOrigin::Meeting));

        json["tags"] = serde_json::json!(["Research"]);
        let old: Note = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(old.origin(), NoteOrigin::Assistant, "case aside, as tags are");

        json["tags"] = serde_json::json!(["boats"]);
        let old: Note = serde_json::from_value(json).unwrap();
        assert_eq!(old.origin(), NoteOrigin::Own);
    }

    #[test]
    fn a_recorded_origin_outranks_the_tags() {
        // A note the person wrote and tagged "meeting" by hand is still theirs.
        let mut note = Note::written("Notes for Tuesday", "agenda");
        note.tags = vec!["meeting".into()];
        assert_eq!(note.origin(), NoteOrigin::Own);
    }

    #[test]
    fn an_origin_round_trips_through_its_name() {
        for origin in [NoteOrigin::Own, NoteOrigin::Assistant, NoteOrigin::Meeting] {
            assert_eq!(origin.as_str().parse::<NoteOrigin>().unwrap(), origin);
            assert_eq!(serde_json::to_value(origin).unwrap(), origin.as_str());
        }
        assert!("robot".parse::<NoteOrigin>().is_err());
    }

    #[test]
    fn everything_findable_is_in_the_searchable_text() {
        let mut note = Note::written("Sailing", "Reach, run, beat.");
        note.tags = vec!["boats".into()];
        let text = note.searchable_text();
        for needle in ["Sailing", "beat", "boats"] {
            assert!(text.contains(needle), "{needle:?} must be findable");
        }
    }
}
