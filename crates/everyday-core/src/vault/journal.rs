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
        self.write(|u| u.store.put_journal(&journal))
    }

    /// Delete a journal, every entry inside it, and every reading its
    /// trackers made.
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
        })
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
        })
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
        })
    }

    pub fn delete_entry(&self, id: EntryId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            u.store.delete_entry(id)?;
            u.index.remove(id);
            Ok(())
        })
    }
}
