//! Journals and entries: the original domain, and the one every backend has.
//!
//! Unlike everything after it, this facade goes straight through
//! [`Vault::read`] and [`Vault::write`] rather than [`Vault::with_domain`] --
//! there is no `Option<&dyn JournalStore>` to unwrap, because a
//! [`JournalStore`](crate::store::JournalStore) *is* one of these.

use super::Vault;
use crate::error::Result;
use crate::id::{EntryId, JournalId};
use crate::model::{Entry, EntrySummary, Journal};
use crate::record::RecordKind;
use crate::store::EntryQuery;
use jiff::Timestamp;

impl Vault {
    pub fn journals(&self) -> Result<Vec<Journal>> {
        self.read(|u| {
            let mut js = u.store.list_journals()?;
            js.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(js)
        })
    }

    pub fn journal(&self, id: JournalId) -> Result<Journal> {
        self.read(|u| u.store.get_journal(id))
    }

    pub fn save_journal(&self, journal: &Journal) -> Result<()> {
        self.writable()?;
        // Tracker definitions are tidied here rather than trusted from the
        // caller. They arrive from a form, they are read by charts, and
        // "the scale is at least 1 and the target is a finite number" is an
        // assumption every reader downstream makes without checking.
        let mut journal = journal.clone();
        for tracker in &mut journal.trackers {
            tracker.normalize();
        }
        journal.trackers.retain(|t| !t.name.is_empty());
        // Whose journal it is is the vault's to say, not a caller's: a journal
        // already stored keeps what it was, and a new one is never the
        // assistant's -- only `assistant_journal_or_new` makes that one (and a
        // restored backup, which writes to the store directly). A journal a
        // client could mark as the assistant's would be pages served to the
        // next scheduled run as its own notes. See `Journal::assistant`.
        journal.assistant = match self.journal(journal.id) {
            Ok(stored) => stored.assistant,
            Err(crate::error::Error::NotFound { .. }) => false,
            Err(e) => return Err(e),
        };
        let id = journal.id;
        self.write(|u| u.store.put_journal(&journal))?;
        self.wrote(RecordKind::Journal, id);
        Ok(())
    }

    /// The assistant's own journal, if it has one yet. See
    /// [`Journal::assistant`].
    ///
    /// Should two ever exist -- two devices on one shared database each
    /// writing their first note in the same moment -- the older is the one,
    /// every time, so every reader agrees on which.
    pub fn assistant_journal(&self) -> Result<Option<Journal>> {
        Ok(self.journals()?.into_iter().filter(|j| j.assistant).min_by_key(|j| j.created_at))
    }

    /// The assistant's own journal, made -- called `name` -- if it has none:
    /// the first note it writes itself is what brings it into being, so a
    /// vault whose assistant never keeps one has no empty journal it did not
    /// ask for.
    pub fn assistant_journal_or_new(&self, name: &str) -> Result<Journal> {
        if let Some(journal) = self.assistant_journal()? {
            return Ok(journal);
        }
        self.writable()?;
        let mut journal = Journal::new(name).with_icon("\u{2728}");
        journal.description = "Notes the assistant leaves itself, filed under the day they \
                               are about."
            .into();
        journal.assistant = true;
        // Looked for again, and made, under the one write lock: two runs
        // writing their first note at once -- a dream and a routine -- must
        // not each make one.
        let made = self.write(|u| {
            let journals = u.store.list_journals()?;
            if let Some(existing) =
                journals.iter().filter(|j| j.assistant).min_by_key(|j| j.created_at)
            {
                return Ok(existing.clone());
            }
            // Last in the sidebar: it is theirs to read, not where they write.
            journal.sort_order = journals.iter().map(|j| j.sort_order).max().unwrap_or(0) + 1;
            u.store.put_journal(&journal)?;
            Ok(journal.clone())
        })?;
        if made.id == journal.id {
            self.wrote(RecordKind::Journal, made.id);
        }
        Ok(made)
    }

    /// Delete a journal, every entry inside it, and every reading its
    /// trackers made.
    ///
    /// Only the journal itself is recorded as touched -- the entry sweep
    /// below is for the search index, and the readings it detaches are
    /// still there afterwards, just unfiled, so neither is a write a
    /// listener needs telling about on its own.
    pub fn delete_journal(&self, id: JournalId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            for e in u.store.list_entries(&EntryQuery::in_journal(id))? {
                u.index.remove(e.id);
            }
            // Readings survive, and are detached rather than deleted.
            //
            // They used to go with the journal, because a tracker was a
            // field inside one and so its readings were part of it. Trackers
            // are vault records now: a reading belongs to the tracker, and
            // the journal is only where you happened to tick it. Deleting
            // the notebook you wrote in does not undo the run.
            //
            // Belt and braces, as the entry sweep is: a backend that holds
            // readings should do this inside its own delete -- the SQL one
            // does, in the same transaction -- and this second, idempotent
            // pass is what stops one that has not thought about it from
            // leaving readings pointing at a journal that is gone.
            if let Some(t) = u.store.trackers() {
                t.detach_readings_in(id)?;
            }
            u.store.delete_journal(id)
        })?;
        self.wrote(RecordKind::Journal, id);
        Ok(())
    }

    pub fn entries(&self, query: &EntryQuery) -> Result<Vec<EntrySummary>> {
        self.read(|u| u.store.list_entries(query))
    }

    pub fn entry(&self, id: EntryId) -> Result<Entry> {
        self.read(|u| u.store.get_entry(id))
    }

    /// Save `entry`, provided nobody else has written it since `expect`.
    ///
    /// `expect` is the `updated_at` the caller loaded, or `None` for a new
    /// entry; a mismatch is [`Error::Conflict`](crate::error::Error::Conflict)
    /// and nothing is written. It
    /// cannot be read off `entry`, because the caller has already stamped a
    /// fresh `updated_at` on the copy it is trying to save.
    ///
    /// This is what stops the second saver of an entry silently overwriting
    /// the first. The vault write lock already keeps two *processes* from
    /// both being writers, so what is left for this to catch is the case the
    /// lock cannot: an editor that has had an entry open since before some
    /// other change to it -- a CLI edit made while the app was closed, a
    /// vault in a synced folder written on another machine, or simply a stale
    /// tab. Losing a paragraph to any of those is the failure this exists to
    /// prevent, so the conflict is reported and the author decides.
    pub fn save_entry(&self, entry: &Entry, expect: Option<Timestamp>) -> Result<()> {
        self.writable()?;
        entry.body.validate()?;
        self.write(|u| {
            u.store.put_entry_if(entry, expect)?;
            u.index.insert(entry);
            Ok(())
        })?;
        self.wrote(RecordKind::Entry, entry.id);
        Ok(())
    }

    /// Save `entry` regardless of what is already stored.
    ///
    /// The deliberate resolution of a conflict [`Vault::save_entry`]
    /// reported, and the path an import takes. Separate rather than an
    /// `Option` flag so that overwriting somebody's work is something a
    /// caller has to name.
    pub fn overwrite_entry(&self, entry: &Entry) -> Result<()> {
        self.writable()?;
        entry.body.validate()?;
        self.write(|u| {
            u.store.put_entry(entry)?;
            u.index.insert(entry);
            Ok(())
        })?;
        self.wrote(RecordKind::Entry, entry.id);
        Ok(())
    }

    pub fn delete_entry(&self, id: EntryId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            u.store.delete_entry(id)?;
            u.index.remove(id);
            Ok(())
        })?;
        self.wrote(RecordKind::Entry, id);
        Ok(())
    }
}
