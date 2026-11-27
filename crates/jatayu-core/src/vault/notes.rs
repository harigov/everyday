//! Notes: writing that is not filed under a day.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::files::{RefKind, RefOwner};
use crate::id::NoteId;
use crate::note::{Note, NoteSummary};
use crate::record::RecordKind;
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

    /// The list, every row saying who started its note -- read from the
    /// tags for a row sealed before that was kept. See
    /// [`crate::note::NoteOrigin::guess`].
    pub fn notes(&self, query: &NoteQuery) -> Result<Vec<NoteSummary>> {
        let mut rows = self.with_notes(|n| n.list_notes(query))?;
        for row in &mut rows {
            row.origin = Some(row.origin());
        }
        Ok(rows)
    }

    /// One note, whole, exactly as stored. An older note's origin is left
    /// unsaid rather than filled in from its tags: whoever reads it here may
    /// save it back, and a guess saved is a guess nobody can take back -- a
    /// note the person tagged "meeting" by hand would be a call's notes for
    /// good. [`Note::origin`] reads it for anyone who needs to know.
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
        self.write_in(&self.gates.text, |u| {
            let notes = u.notes()?;
            notes.put_note_if(note, expect)?;
            // Not `u.index.insert_note(note)`: a note that is a meeting note
            // has a transcript whose words belong in the same search entry,
            // and only `reindex_note` knows to go and fetch them. See its
            // doc, and `save_transcript`, for why the two must never drift
            // apart -- an autosave is exactly the path that used to lose
            // this.
            super::meetings::reindex_note(u, note.id);
            Ok(())
        })?;
        self.track_file_refs(RefOwner::new(RefKind::Note, note.id), note.body.file_refs());
        self.wrote(RecordKind::Note, note.id);
        Ok(())
    }

    /// Save a note whatever is already stored. The deliberate "keep mine".
    pub fn overwrite_note(&self, note: &Note) -> Result<()> {
        self.writable()?;
        note.validate()?;
        note.body.validate()?;
        self.write_in(&self.gates.text, |u| {
            let notes = u.notes()?;
            notes.put_note(note)?;
            super::meetings::reindex_note(u, note.id);
            Ok(())
        })?;
        self.track_file_refs(RefOwner::new(RefKind::Note, note.id), note.body.file_refs());
        self.wrote(RecordKind::Note, note.id);
        Ok(())
    }

    /// Delete a note, its transcript if it had one, and the pointer a
    /// finished recording kept back to it -- see
    /// `super::meetings::detach_note_from_meetings`. A note carries no hint
    /// of its own that it was ever a meeting note, so this cascade runs
    /// unconditionally rather than only when one is expected; it is a no-op,
    /// two cheap lookups, on every note that never had a recording behind
    /// it.
    ///
    /// Exclusive, unlike the saves: the cascade rewrites recordings, which
    /// the meeting pipeline saves under the shared lock, and a deletion is
    /// rare enough not to be worth a gate of its own.
    pub fn delete_note(&self, id: NoteId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let notes = u.notes()?;
            notes.delete_note(id)?;
            u.index.remove_note(id);
            super::meetings::detach_note_from_meetings(u, id)?;
            Ok(())
        })?;
        self.drop_file_refs(RefOwner::new(RefKind::Note, id));
        self.wrote(RecordKind::Note, id);
        Ok(())
    }

    pub fn note_tags(&self) -> Result<Vec<(String, u64)>> {
        self.with_notes(|n| n.note_tags())
    }

    /// Every note, bodies included. For export.
    pub fn all_notes(&self) -> Result<Vec<Note>> {
        self.with_notes(|n| n.all_notes())
    }
}
