//! The notes domain.
//!
//! Small, because a note is a small record: no journal to belong to, no date
//! range to scan, and no cascade out of it except the purpose pointer every
//! record has. The two things worth reading the code for are the same two the
//! entries table does, for the same reasons.
//!
//! **Two sealed columns.** `data` is the whole note; `summary` is the row a
//! list draws. A note holds a document and its photographs, and forty rows in
//! a sidebar must not cost forty decrypted bodies to show forty titles.
//!
//! **The ordering happens in Rust.** `NoteQuery::apply` sorts and paginates
//! after the rows are open, because one of the three orders is by title and a
//! title is sealed -- there is nothing in the file to `ORDER BY`. Doing it
//! once, above both backends, is also what stops the two of them disagreeing
//! about where an untitled note goes.

use everyday_core::error::{Error, Result};
use everyday_core::id::NoteId;
use everyday_core::note::{Note, NoteSummary};
use everyday_core::store::notes::{NoteQuery, NoteStore, note_aad};

use crate::conn::SqlExt;
use crate::purpose::{RecordKind, forget_purposes, set_purpose};
use crate::{SqlStore, to_us, vals};

impl SqlStore {
    fn seal_note(&self, n: &Note) -> Result<(Vec<u8>, Vec<u8>)> {
        let aad = note_aad(n.id);
        let data = self.cipher.seal(&aad, &serde_json::to_vec(n)?)?;
        let summary = self.cipher.seal(&aad, &serde_json::to_vec(&n.summarize())?)?;
        Ok((data, summary))
    }
}

impl NoteStore for SqlStore {
    fn list_notes(&self, query: &NoteQuery) -> Result<Vec<NoteSummary>> {
        // Everything, then filtered and ordered above. See the module note:
        // there is no sealed column to sort by, and a `LIMIT` applied before
        // the sort would return the wrong rows rather than fewer of them.
        let rows = self
            .read()
            .records("SELECT id, summary FROM notes ORDER BY pinned DESC, updated_us DESC", &[])?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, sealed) in rows {
            let id = NoteId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?;
            out.push(self.unseal::<NoteSummary>(&note_aad(id), &sealed)?);
        }
        Ok(query.apply(out))
    }

    fn get_note(&self, id: NoteId) -> Result<Note> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM notes WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("note", id))?;
        self.unseal(&note_aad(id), &sealed)
    }

    fn put_note(&self, note: &Note) -> Result<()> {
        let (data, summary) = self.seal_note(note)?;
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "INSERT INTO notes (id, pinned, created_us, updated_us, data, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                pinned = ?2, created_us = ?3, updated_us = ?4, data = ?5, summary = ?6",
            &vals![
                note.id.to_string(),
                note.pinned,
                to_us(note.created_at),
                to_us(note.updated_at),
                data,
                summary,
            ],
        )?;
        set_purpose(tx.as_mut(), RecordKind::Note, &note.id.to_string(), note.purpose.as_ref())?;
        tx.commit()
    }

    /// One statement, so the check and the write cannot be separated.
    ///
    /// The same argument `put_entry_if` makes: the shared default reads,
    /// compares and writes, which is right against another thread and leaves
    /// a window another *process* can step through -- and cross-process is
    /// precisely the case a conditional write is for.
    fn put_note_if(&self, note: &Note, expect: Option<jiff::Timestamp>) -> Result<()> {
        let (data, summary) = self.seal_note(note)?;
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let changed = match expect {
            // Updating: only if `updated_us` is still what the caller read. A
            // row that has moved on, or been deleted, matches nothing.
            Some(want) => tx.execute(
                "UPDATE notes SET pinned = ?2, created_us = ?3, updated_us = ?4,
                    data = ?5, summary = ?6
                 WHERE id = ?1 AND updated_us = ?7",
                &vals![
                    note.id.to_string(),
                    note.pinned,
                    to_us(note.created_at),
                    to_us(note.updated_at),
                    data,
                    summary,
                    to_us(want),
                ],
            )?,
            // Creating: `DO NOTHING` turns a primary-key clash into zero rows
            // rather than an error, so both branches report a conflict the
            // same way -- by the count.
            None => tx.execute(
                "INSERT INTO notes (id, pinned, created_us, updated_us, data, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO NOTHING",
                &vals![
                    note.id.to_string(),
                    note.pinned,
                    to_us(note.created_at),
                    to_us(note.updated_at),
                    data,
                    summary,
                ],
            )?,
        };
        if changed == 0 {
            return Err(Error::Conflict { kind: "note" });
        }
        set_purpose(tx.as_mut(), RecordKind::Note, &note.id.to_string(), note.purpose.as_ref())?;
        tx.commit()
    }

    fn delete_note(&self, id: NoteId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let gone = tx.execute("DELETE FROM notes WHERE id = ?1", &vals![id.to_string()])?;
        // Only for a row that was actually there: the pointer index is keyed
        // by an id the source table no longer has, so it is cleared with the
        // ids the delete really removed.
        if gone > 0 {
            forget_purposes(tx.as_mut(), RecordKind::Note, &[id.to_string()])?;
        }
        tx.commit()
    }

    fn all_notes(&self) -> Result<Vec<Note>> {
        let rows = self.read().records("SELECT id, data FROM notes ORDER BY created_us", &[])?;
        self.collect(rows, note_aad)
    }
}
