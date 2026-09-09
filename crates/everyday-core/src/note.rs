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

use crate::id::{BlobId, NoteId};
use crate::model::{Attachment, MediaKind};
use crate::purpose::Purpose;
use crate::richtext::RichDoc;
use crate::{Error, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Longest title we will store. Past this it is a body.
pub const MAX_TITLE_BYTES: usize = 500;

/// A note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            created_at: now,
            updated_at: now,
        }
    }

    /// A note with a body already in it, which is how the assistant makes one.
    pub fn written(title: impl Into<String>, markdown: &str) -> Self {
        Self { body: RichDoc::from_markdown(markdown), ..Self::new(title) }
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
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// A row in the note list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
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
    fn everything_findable_is_in_the_searchable_text() {
        let mut note = Note::written("Sailing", "Reach, run, beat.");
        note.tags = vec!["boats".into()];
        let text = note.searchable_text();
        for needle in ["Sailing", "beat", "boats"] {
            assert!(text.contains(needle), "{needle:?} must be findable");
        }
    }
}
