//! Notes: writing that is not filed under a day.

use super::Vault;
use super::session::{Domain, pick_domain};
use crate::error::Result;
use crate::id::NoteId;
use crate::note::{Note, NoteSummary};
use crate::store::notes::{NoteQuery, NoteStore};
use jiff::Timestamp;

impl Vault {
    /// Does this vault's backend hold notes at all?
    pub fn supports_notes(&self) -> bool {
        self.with_notes(|_| Ok(())).is_ok()
    }

    fn with_notes<T>(&self, f: impl FnOnce(&dyn NoteStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Notes, |s| s.notes().map(f))
    }

    pub fn notes(&self, query: &NoteQuery) -> Result<Vec<NoteSummary>> {
        self.with_notes(|n| n.list_notes(query))
    }

    pub fn note(&self, id: NoteId) -> Result<Note> {
        self.with_notes(|n| n.get_note(id))
    }

    /// Save a note, refusing to overwrite somebody else's edit.
    ///
    /// `expect` is the `updated_at` the caller last read, exactly as for
    /// [`Vault::save_entry`], and for the same reason: a note is a document
    /// that is typed into and autosaved, so two windows on one vault will
    /// find each other sooner or later.
    pub fn save_note(&self, note: &Note, expect: Option<Timestamp>) -> Result<()> {
        self.writable()?;
        note.validate()?;
        note.body.validate()?;
        self.write(|u| {
            let notes = pick_domain(u.store.as_ref(), Domain::Notes, |s| s.notes())?;
            notes.put_note_if(note, expect)?;
            u.index.insert_note(note);
            Ok(())
        })
    }

    /// Save a note whatever is already stored. The deliberate "keep mine".
    pub fn overwrite_note(&self, note: &Note) -> Result<()> {
        self.writable()?;
        note.validate()?;
        note.body.validate()?;
        self.write(|u| {
            let notes = pick_domain(u.store.as_ref(), Domain::Notes, |s| s.notes())?;
            notes.put_note(note)?;
            u.index.insert_note(note);
            Ok(())
        })
    }

    pub fn delete_note(&self, id: NoteId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let notes = pick_domain(u.store.as_ref(), Domain::Notes, |s| s.notes())?;
            notes.delete_note(id)?;
            u.index.remove_note(id);
            Ok(())
        })
    }

    pub fn note_tags(&self) -> Result<Vec<(String, u64)>> {
        self.with_notes(|n| n.note_tags())
    }

    /// Every note, bodies included. For export.
    pub fn all_notes(&self) -> Result<Vec<Note>> {
        self.with_notes(|n| n.all_notes())
    }
}
