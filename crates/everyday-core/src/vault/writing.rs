//! Writing help: every time it was asked over text selected anywhere on the
//! desktop, and what came of it. See [`crate::writing`] and
//! `docs/plans/writing-tools.md`.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::id::WritingId;
use crate::record::RecordKind;
use crate::store::writing::{WritingQuery, WritingStore};
use crate::writing::{WritingAssist, WritingOutcome};

impl Vault {
    /// Does this vault's backend keep writing help at all?
    pub fn supports_writing(&self) -> bool {
        self.with_writing(|_| Ok(())).is_ok()
    }

    fn with_writing<T>(&self, f: impl FnOnce(&dyn WritingStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Writing, |s| s.writing().map(f))
    }

    pub fn writing_assists(&self, query: &WritingQuery) -> Result<Vec<WritingAssist>> {
        self.with_writing(|w| w.list_writing_assists(query))
    }

    pub fn writing_assist(&self, id: WritingId) -> Result<WritingAssist> {
        self.with_writing(|w| w.get_writing_assist(id))
    }

    /// Save an assist, validated.
    pub fn save_writing_assist(&self, assist: &WritingAssist) -> Result<()> {
        self.writable()?;
        assist.validate()?;
        self.with_writing(|w| w.put_writing_assist(assist))?;
        self.wrote(RecordKind::WritingAssist, assist.id);
        Ok(())
    }

    /// Say what became of an assist's answer, and hand it back as saved.
    pub fn settle_writing_assist(
        &self,
        id: WritingId,
        outcome: WritingOutcome,
    ) -> Result<WritingAssist> {
        self.writable()?;
        let mut assist = self.writing_assist(id)?;
        assist.settle(outcome)?;
        self.save_writing_assist(&assist)?;
        Ok(assist)
    }

    pub fn delete_writing_assist(&self, id: WritingId) -> Result<()> {
        self.writable()?;
        self.with_writing(|w| w.delete_writing_assist(id))?;
        self.wrote(RecordKind::WritingAssist, id);
        Ok(())
    }
}
