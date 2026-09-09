//! Storage for notes.
//!
//! A short trait, because a note is a short record: there is no date range to
//! scan, no journal to belong to, and no cascade. What a backend has to do
//! beyond the obvious is the same thing the entry table does -- keep the
//! payload sealed and leave in the clear only what an index reads.
//!
//! In the clear: whether a note is pinned, and when it was made and last
//! touched. That is what orders the list. Sealed: the title, the body, the
//! tags, the captions. So the file can say that somebody keeps eleven notes
//! and pinned two of them, and never what any of them is about -- which
//! matters more here than it does for entries, because a note is named and
//! the name is the part that gives it away.

use crate::id::NoteId;
use crate::note::{Note, NoteSummary};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// How a note list is ordered.
///
/// Fewer choices than [`crate::store::SortOrder`] because a note has fewer
/// things worth ordering by: it has no date of its own, and the two questions
/// people actually ask of a note list are "what was I just working on" and
/// "where is the one called X".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteSort {
    /// Most recently edited first.
    #[default]
    UpdatedDesc,
    CreatedDesc,
    TitleAsc,
}

/// Filter and pagination for [`NoteStore::list_notes`].
///
/// All filters are ANDed. An empty query matches every note.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NoteQuery {
    /// The note must carry *every* listed tag.
    pub tags: Vec<String>,
    /// Restrict to pinned notes.
    pub pinned: Option<bool>,
    pub sort: NoteSort,
    pub offset: u32,
    /// `None` means no limit.
    pub limit: Option<u32>,
}

impl NoteQuery {
    pub fn recent(limit: u32) -> Self {
        Self { limit: Some(limit), ..Default::default() }
    }

    /// Does this note pass the filters? Backends that cannot express one
    /// natively fall back to this, so behaviour stays identical across them.
    pub fn matches(&self, n: &NoteSummary) -> bool {
        if let Some(pinned) = self.pinned
            && n.pinned != pinned
        {
            return false;
        }
        // Case-insensitive, as everywhere else: "Boats" and "boats" are the
        // same tag to a person, so they are the same tag to the filter.
        self.tags.iter().all(|want| n.tags.iter().any(|have| have.eq_ignore_ascii_case(want)))
    }

    /// Sort and paginate a fully-materialised list.
    ///
    /// Done here rather than in SQL because the ordering is by title as often
    /// as not, and a title is sealed -- so the rows have to be decrypted
    /// before they can be compared, and one implementation of the comparison
    /// cannot disagree with itself across two backends.
    pub fn apply(&self, mut rows: Vec<NoteSummary>) -> Vec<NoteSummary> {
        rows.retain(|n| self.matches(n));
        sort_notes(&mut rows, self.sort);
        let start = (self.offset as usize).min(rows.len());
        rows.drain(..start);
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// Order `rows` in place. Pinned notes float to the top whatever the sort,
/// which is what a pin means everywhere else in the application.
pub fn sort_notes(rows: &mut [NoteSummary], sort: NoteSort) {
    rows.sort_by(|a, b| {
        b.pinned.cmp(&a.pinned).then_with(|| match sort {
            NoteSort::UpdatedDesc => b.updated_at.cmp(&a.updated_at),
            NoteSort::CreatedDesc => b.created_at.cmp(&a.created_at),
            NoteSort::TitleAsc => a
                .title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                // Two notes called "Untitled note" still need a stable order.
                .then_with(|| a.id.to_string().cmp(&b.id.to_string())),
        })
    });
}

/// What a backend must do to store notes.
pub trait NoteStore: Send + Sync {
    fn list_notes(&self, query: &NoteQuery) -> Result<Vec<NoteSummary>>;
    fn get_note(&self, id: NoteId) -> Result<Note>;
    /// Idempotent: saving the same note twice leaves one.
    fn put_note(&self, note: &Note) -> Result<()>;
    /// Deleting a note that is not there is not an error.
    fn delete_note(&self, id: NoteId) -> Result<()>;

    /// Every note, bodies and all.
    ///
    /// Two callers, the same two [`crate::store::JournalStore::all_entries`]
    /// has: building the search index when a vault is unlocked, and export.
    /// List views must never pay for this.
    fn all_notes(&self) -> Result<Vec<Note>>;

    /// Every tag in use, with how many notes carry it.
    fn note_tags(&self) -> Result<Vec<(String, u64)>> {
        let mut counts: std::collections::BTreeMap<String, u64> = Default::default();
        for note in self.list_notes(&NoteQuery::default())? {
            for tag in note.tags {
                *counts.entry(tag).or_default() += 1;
            }
        }
        Ok(counts.into_iter().collect())
    }
}

/// Additional authenticated data for a sealed note payload.
pub fn note_aad(id: NoteId) -> Vec<u8> {
    format!("everyday.note.v1:{id}").into_bytes()
}

/// The error a missing note gets, so every backend words it the same way.
pub fn note_not_found(id: NoteId) -> Error {
    Error::NotFound(format!("note {id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::Note;

    fn summary(title: &str, pinned: bool, tags: &[&str]) -> NoteSummary {
        let mut note = Note::new(title);
        note.pinned = pinned;
        note.tags = tags.iter().map(|t| (*t).to_string()).collect();
        note.summarize()
    }

    #[test]
    fn a_pinned_note_is_first_whatever_the_sort() {
        let rows = vec![summary("Beta", false, &[]), summary("Alpha", true, &[])];
        let sorted = NoteQuery { sort: NoteSort::TitleAsc, ..Default::default() }.apply(rows);
        assert_eq!(sorted[0].title, "Alpha");

        let rows = vec![summary("Zeta", true, &[]), summary("Alpha", false, &[])];
        let sorted = NoteQuery { sort: NoteSort::TitleAsc, ..Default::default() }.apply(rows);
        assert_eq!(sorted[0].title, "Zeta", "a pin outranks the alphabet");
    }

    #[test]
    fn a_tag_filter_is_case_insensitive_and_wants_all_of_them() {
        let rows = vec![
            summary("Sailing", false, &["Boats", "summer"]),
            summary("Rowing", false, &["boats"]),
        ];
        let hits = NoteQuery { tags: vec!["boats".into()], ..Default::default() }
            .apply(rows.clone());
        assert_eq!(hits.len(), 2, "case must not decide whether a tag matches");

        let hits = NoteQuery {
            tags: vec!["boats".into(), "summer".into()],
            ..Default::default()
        }
        .apply(rows);
        assert_eq!(hits.len(), 1, "every listed tag has to be there, not any of them");
    }

    #[test]
    fn the_sealing_label_names_the_note_it_belongs_to() {
        let one = NoteId::new();
        let two = NoteId::new();
        assert_ne!(note_aad(one), note_aad(two));
        assert!(String::from_utf8(note_aad(one)).unwrap().contains("note"));
    }
}
