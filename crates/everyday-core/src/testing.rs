//! The in-memory [`JournalStore`] every test in this crate runs against.
//!
//! It exists so that testing the vault -- lock and unlock, the search index,
//! the header's recovery from a bad shutdown -- never needs a real storage
//! crate on the other end. That is what keeps this crate's own tests free of
//! a dependency on `everyday-store-sql` or `everyday-store-sqlite`, and it is
//! offered to downstream crates behind the `testing` feature for the same
//! reason: a service or transfer test that wants a vault, but is not itself
//! testing a storage engine, should not have to bring one in to get one.
//!
//! [`registry`] is the way in: it hands back a [`BackendRegistry`] with this
//! backend registered under the id `"memory"`, ready to pass to
//! [`Vault::create`](crate::vault::Vault::create).

use crate::crypto::Cipher;
use crate::error::{Error, Result};
use crate::id::{BlobId, EntryId, JournalId};
use crate::model::{Entry, EntrySummary, Journal};
use crate::store::{
    BackendRegistry, Capabilities, EntryQuery, JournalStore, StoreContext, StoreFactory, StoreStats,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// A minimal in-memory backend, so the vault can be tested without
// depending on a concrete storage crate.
#[derive(Default)]
pub struct MemStore {
    cipher: Option<Arc<dyn Cipher>>,
    journals: Mutex<BTreeMap<JournalId, Vec<u8>>>,
    entries: Mutex<BTreeMap<EntryId, Vec<u8>>>,
    /// Sealed bytes and when they were stored. The timestamp exists
    /// for the same reason the file stores keep an mtime: GC has to be
    /// able to tell a settled orphan from a blob written a moment ago.
    blobs: Mutex<BTreeMap<BlobId, (Vec<u8>, std::time::Instant)>>,
}

impl MemStore {
    fn cipher(&self) -> &dyn Cipher {
        self.cipher.as_deref().unwrap()
    }
}

impl JournalStore for MemStore {
    fn backend(&self) -> &'static str {
        "memory"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            notes: false,
            routines: false,
            blobs: true,
            transactional: false,
            human_readable: false,
            max_blob_bytes: None,
            tasks: false,
            calendars: false,
            library: false,
            trackers: false,
            purpose: false,
            agent: false,
        }
    }
    fn list_journals(&self) -> Result<Vec<Journal>> {
        self.journals
            .lock()
            .unwrap()
            .iter()
            .map(|(id, v)| {
                let plain = self.cipher().open(&crate::store::journal_aad(*id), v)?;
                Ok(serde_json::from_slice(&plain)?)
            })
            .collect()
    }
    fn get_journal(&self, id: JournalId) -> Result<Journal> {
        let g = self.journals.lock().unwrap();
        let v = g.get(&id).ok_or_else(|| Error::not_found("journal", id))?;
        let plain = self.cipher().open(&crate::store::journal_aad(id), v)?;
        Ok(serde_json::from_slice(&plain)?)
    }
    fn put_journal(&self, j: &Journal) -> Result<()> {
        let sealed =
            self.cipher().seal(&crate::store::journal_aad(j.id), &serde_json::to_vec(j)?)?;
        self.journals.lock().unwrap().insert(j.id, sealed);
        Ok(())
    }
    fn delete_journal(&self, id: JournalId) -> Result<()> {
        self.journals.lock().unwrap().remove(&id);
        let doomed: Vec<EntryId> =
            self.all_entries()?.into_iter().filter(|e| e.journal_id == id).map(|e| e.id).collect();
        let mut g = self.entries.lock().unwrap();
        for d in doomed {
            g.remove(&d);
        }
        Ok(())
    }
    fn list_entries(&self, q: &EntryQuery) -> Result<Vec<EntrySummary>> {
        Ok(q.apply(self.all_entries()?.iter().map(Entry::summarize).collect()))
    }
    fn get_entry(&self, id: EntryId) -> Result<Entry> {
        let g = self.entries.lock().unwrap();
        let v = g.get(&id).ok_or_else(|| Error::not_found("entry", id))?;
        let plain = self.cipher().open(&crate::store::entry_aad(id), v)?;
        Ok(serde_json::from_slice(&plain)?)
    }
    fn put_entry(&self, e: &Entry) -> Result<()> {
        let sealed = self.cipher().seal(&crate::store::entry_aad(e.id), &serde_json::to_vec(e)?)?;
        self.entries.lock().unwrap().insert(e.id, sealed);
        Ok(())
    }
    fn delete_entry(&self, id: EntryId) -> Result<()> {
        self.entries.lock().unwrap().remove(&id);
        Ok(())
    }
    fn all_entries(&self) -> Result<Vec<Entry>> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, v)| {
                let plain = self.cipher().open(&crate::store::entry_aad(*id), v)?;
                Ok(serde_json::from_slice(&plain)?)
            })
            .collect()
    }
    fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
        let id = BlobId::of(bytes);
        let sealed = self.cipher().seal(&crate::store::blob_aad(id), bytes)?;
        self.blobs.lock().unwrap().insert(id, (sealed, std::time::Instant::now()));
        Ok(id)
    }
    fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
        let g = self.blobs.lock().unwrap();
        let (v, _) = g.get(&id).ok_or_else(|| Error::not_found("blob", id))?;
        self.cipher().open(&crate::store::blob_aad(id), v)
    }
    fn has_blob(&self, id: BlobId) -> Result<bool> {
        Ok(self.blobs.lock().unwrap().contains_key(&id))
    }
    fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        Ok(self.blobs.lock().unwrap().get(&id).map(|(_, at)| at.elapsed()))
    }
    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.blobs.lock().unwrap().remove(&id);
        Ok(())
    }
    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        Ok(self.blobs.lock().unwrap().keys().copied().collect())
    }
    fn stats(&self) -> Result<StoreStats> {
        // Take each lock into its own binding: temporaries inside a
        // struct expression live to the end of the statement, so locking
        // `blobs` twice inline would deadlock on the second acquire.
        let journals = self.journals.lock().unwrap().len() as u64;
        let entries = self.entries.lock().unwrap().len() as u64;
        let blobs = self.blobs.lock().unwrap();
        Ok(StoreStats {
            journals,
            entries,
            blobs: blobs.len() as u64,
            blob_bytes: blobs.values().map(|(v, _)| v.len() as u64).sum(),
        })
    }
}

// Backing maps persist across open/close so that "reopen the vault"
// tests exercise real decryption rather than a fresh empty store.
#[derive(Default)]
pub struct MemFactory {
    stores: Mutex<BTreeMap<PathBuf, Arc<MemStore>>>,
}

pub struct Handle(Arc<MemStore>);

impl JournalStore for Handle {
    fn backend(&self) -> &'static str {
        self.0.backend()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn list_journals(&self) -> Result<Vec<Journal>> {
        self.0.list_journals()
    }
    fn get_journal(&self, id: JournalId) -> Result<Journal> {
        self.0.get_journal(id)
    }
    fn put_journal(&self, j: &Journal) -> Result<()> {
        self.0.put_journal(j)
    }
    fn delete_journal(&self, id: JournalId) -> Result<()> {
        self.0.delete_journal(id)
    }
    fn list_entries(&self, q: &EntryQuery) -> Result<Vec<EntrySummary>> {
        self.0.list_entries(q)
    }
    fn get_entry(&self, id: EntryId) -> Result<Entry> {
        self.0.get_entry(id)
    }
    fn put_entry(&self, e: &Entry) -> Result<()> {
        self.0.put_entry(e)
    }
    fn delete_entry(&self, id: EntryId) -> Result<()> {
        self.0.delete_entry(id)
    }
    fn all_entries(&self) -> Result<Vec<Entry>> {
        self.0.all_entries()
    }
    fn put_blob(&self, b: &[u8]) -> Result<BlobId> {
        self.0.put_blob(b)
    }
    fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
        self.0.get_blob(id)
    }
    fn has_blob(&self, id: BlobId) -> Result<bool> {
        self.0.has_blob(id)
    }
    fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
        self.0.blob_age(id)
    }
    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.0.delete_blob(id)
    }
    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        self.0.list_blobs()
    }
    fn stats(&self) -> Result<StoreStats> {
        self.0.stats()
    }
}

impl StoreFactory for Arc<MemFactory> {
    fn id(&self) -> &'static str {
        "memory"
    }
    fn name(&self) -> &'static str {
        "Memory"
    }
    fn describe(&self) -> &'static str {
        "in-memory (tests only)"
    }
    fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        let mut g = self.stores.lock().unwrap();
        let store = g
            .entry(ctx.root.clone())
            .or_insert_with(|| Arc::new(MemStore { cipher: None, ..Default::default() }));
        // Rebind the cipher for this session.
        let rebound = Arc::new(MemStore {
            cipher: Some(ctx.cipher.clone()),
            journals: Mutex::new(store.journals.lock().unwrap().clone()),
            entries: Mutex::new(store.entries.lock().unwrap().clone()),
            blobs: Mutex::new(store.blobs.lock().unwrap().clone()),
        });
        g.insert(ctx.root, rebound.clone());
        Ok(Box::new(Handle(rebound)))
    }
}

pub fn registry() -> Arc<BackendRegistry> {
    let mut reg = BackendRegistry::new();
    reg.register(Arc::new(MemFactory::default()));
    Arc::new(reg)
}
