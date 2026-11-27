//! The unlocked vault's search index, and building it off the unlock.
//!
//! Every entry and note is read, decrypted and parsed inside the unlock, as
//! it always was: a store that cannot be read should fail the unlock, and the
//! store is only reachable through the vault's lock. Tokenising what was read
//! and filing it is the other half of the work, and that half runs on a
//! thread of its own, so the unlock answers without waiting for it.
//!
//! While that thread works the slot is *building*. A save in that time does
//! not wait: its change is queued, and the thread applies the queue, in
//! order, before it says the index is ready. What the thread files was read
//! before the vault was unlocked, so before any save could happen, and every
//! queued change replaces or removes a record by id -- so the index it ends
//! with is the one a synchronous build followed by the same saves would have
//! made. A search waits until the index is ready, a bounded while, and never
//! while holding the vault's own lock.

use crate::id::{EntryId, NoteId};
use crate::model::Entry;
use crate::note::Note;
use crate::search::{IndexDoc, SearchIndex};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The longest a search waits for an index still being built. Far longer
/// than a build takes -- a few thousand entries is a fraction of a second --
/// so reaching it means something went wrong, and an empty answer is better
/// than a window that hangs.
pub(super) const READY_WAIT: Duration = Duration::from_secs(10);

/// Below this many documents the index is filed in the unlock itself: a
/// thread costs more than the work it would take off the unlock.
const THREAD_FROM: usize = 64;

/// One change to the index, owned so it can wait in the queue.
pub(super) enum IndexOp {
    Put(IndexDoc),
    RemoveEntry(EntryId),
    RemoveNote(NoteId),
}

impl IndexOp {
    fn apply(self, index: &mut SearchIndex) {
        match self {
            IndexOp::Put(doc) => index.put_doc(doc),
            IndexOp::RemoveEntry(id) => index.remove(id),
            IndexOp::RemoveNote(id) => index.remove_note(id),
        }
    }
}

enum State {
    /// The thread is still filing; changes wait here, in the order made.
    Building(Vec<IndexOp>),
    Ready(SearchIndex),
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    /// Set when the slot is dropped -- the vault locked, or the index was
    /// rebuilt -- so the thread stops between documents.
    cancel: AtomicBool,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        // A panic while filing leaves an index that may miss a document,
        // which `reindex` repairs; not a reason to stop searching.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The search index of an unlocked vault, ready or still being built.
pub(super) struct IndexSlot {
    shared: Arc<Shared>,
    builder: Option<JoinHandle<()>>,
}

/// A handle that waits for the index to be ready without holding the slot,
/// so the wait can happen outside the vault's lock.
pub(super) struct IndexWaiter(Arc<Shared>);

impl IndexWaiter {
    /// Wait until the index is ready, the slot is gone, or `bound` passes.
    pub(super) fn wait(&self, bound: Duration) {
        let deadline = Instant::now() + bound;
        let mut state = self.0.state();
        while matches!(*state, State::Building(_)) && !self.0.cancel.load(Ordering::Relaxed) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return };
            state = self.0.ready.wait_timeout(state, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
}

impl IndexSlot {
    /// A slot holding `index`, already built.
    pub(super) fn ready(index: SearchIndex) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::Ready(index)),
                ready: Condvar::new(),
                cancel: AtomicBool::new(false),
            }),
            builder: None,
        }
    }

    /// A slot that files `entries` and `notes` (each note with its words from
    /// `extras`) on a thread of its own. Built here and now instead if the
    /// thread cannot be started.
    pub(super) fn build(
        entries: Vec<Entry>,
        notes: Vec<Note>,
        extras: HashMap<NoteId, String>,
    ) -> Self {
        if entries.len() + notes.len() < THREAD_FROM {
            return Self::ready(SearchIndex::build_with_extras(&entries, &notes, &extras));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State::Building(Vec::new())),
            ready: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        let work = Arc::new(Mutex::new(Some((entries, notes, extras))));
        let spawned = {
            let (shared, work) = (shared.clone(), work.clone());
            std::thread::Builder::new().name("search-index".into()).spawn(move || {
                let taken = work.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some((entries, notes, extras)) = taken {
                    fill(&shared, entries, notes, extras);
                }
            })
        };
        match spawned {
            Ok(thread) => Self { shared, builder: Some(thread) },
            Err(e) => {
                tracing::warn!(error = %e, "could not start the search index's thread; building it here");
                let taken = work.lock().unwrap_or_else(|p| p.into_inner()).take();
                let (entries, notes, extras) = taken.unwrap_or_default();
                Self::ready(SearchIndex::build_with_extras(&entries, &notes, &extras))
            }
        }
    }

    /// Apply `op` now, or queue it for the thread if the index is still
    /// being built.
    pub(super) fn update(&self, op: IndexOp) {
        match &mut *self.shared.state() {
            State::Ready(index) => op.apply(index),
            State::Building(pending) => pending.push(op),
        }
    }

    pub(super) fn put_entry(&self, entry: &Entry) {
        // Worked out before the index is locked: this is the part that walks
        // the document.
        self.update(IndexOp::Put(IndexDoc::entry(entry)));
    }

    pub(super) fn put_note(&self, note: &Note, extra: &str) {
        self.update(IndexOp::Put(IndexDoc::note(note, extra)));
    }

    pub(super) fn remove_entry(&self, id: EntryId) {
        self.update(IndexOp::RemoveEntry(id));
    }

    pub(super) fn remove_note(&self, id: NoteId) {
        self.update(IndexOp::RemoveNote(id));
    }

    pub(super) fn waiter(&self) -> IndexWaiter {
        IndexWaiter(self.shared.clone())
    }

    /// `f` against the index if it is ready, without waiting; `None` if it
    /// is still being built.
    pub(super) fn read<T>(&self, f: impl FnOnce(&SearchIndex) -> T) -> Option<T> {
        match &*self.shared.state() {
            State::Ready(index) => Some(f(index)),
            State::Building(_) => None,
        }
    }
}

impl Drop for IndexSlot {
    /// Stop the thread and wait for it, so that nothing it read outlives the
    /// lock that dropped this slot. It checks between documents, so the wait
    /// is for one document at most.
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.ready.notify_all();
        if let Some(thread) = self.builder.take() {
            let _ = thread.join();
        }
        // Emptied here rather than left to the last handle: a search waiting
        // for this slot holds one, and the text in it goes with the lock.
        let emptied =
            std::mem::replace(&mut *self.shared.state(), State::Ready(SearchIndex::new()));
        drop(emptied);
    }
}

/// The thread's work: file everything, then the queue, then say so.
fn fill(shared: &Shared, entries: Vec<Entry>, notes: Vec<Note>, extras: HashMap<NoteId, String>) {
    // Whatever happens below -- a panic included -- the slot ends ready, so
    // nothing queues changes for a build that is never coming. An index a
    // panic left short is what `reindex` is for.
    struct Finish<'a> {
        shared: &'a Shared,
        index: Option<SearchIndex>,
    }
    impl Drop for Finish<'_> {
        fn drop(&mut self) {
            let mut index = self.index.take().unwrap_or_default();
            let mut state = self.shared.state();
            if let State::Building(pending) = &mut *state {
                for op in pending.drain(..) {
                    op.apply(&mut index);
                }
            }
            *state = State::Ready(index);
            drop(state);
            self.shared.ready.notify_all();
        }
    }

    let mut finish = Finish { shared, index: None };
    let started = Instant::now();
    let documents = entries.len() + notes.len();
    let mut index = SearchIndex::new();
    for entry in &entries {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        index.insert(entry);
    }
    drop(entries);
    for note in &notes {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        index.insert_note_with_extra(note, extras.get(&note.id).map_or("", String::as_str));
    }
    tracing::debug!(
        documents,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "search index built"
    );
    finish.index = Some(index);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::JournalId;
    use crate::richtext::RichDoc;
    use crate::search::SearchScope;

    fn entry(journal: JournalId, body: &str) -> Entry {
        let mut e = Entry::new(journal, "UTC");
        e.body = RichDoc::from_plain_text(body);
        e
    }

    fn hits(slot: &IndexSlot, query: &str) -> usize {
        slot.waiter().wait(READY_WAIT);
        slot.read(|i| i.search_in(query, SearchScope::Everything, 1000).len()).unwrap()
    }

    #[test]
    fn changes_made_while_building_are_all_there_once_it_is_ready() {
        let journal = JournalId::new();
        let entries: Vec<Entry> =
            (0..500).map(|i| entry(journal, &format!("heron number{i}"))).collect();
        let gone = entries[0].id;
        let mut edited = entries[1].clone();
        edited.body = RichDoc::from_plain_text("kingfisher");
        let added = entry(journal, "kingfisher too");

        let slot = IndexSlot::build(entries, Vec::new(), HashMap::new());
        slot.remove_entry(gone);
        slot.put_entry(&edited);
        slot.put_entry(&added);

        assert_eq!(hits(&slot, "heron"), 498);
        assert_eq!(hits(&slot, "kingfisher"), 2);
        assert_eq!(hits(&slot, "number0"), 0);
    }

    #[test]
    fn a_note_is_filed_with_its_extra_words() {
        let mut note = Note::new("Standup");
        note.body = RichDoc::from_plain_text("agenda");
        let extras = HashMap::from([(note.id, "the budget was discussed".to_string())]);
        // Filed on the unlock for a vault this small, and on the thread for
        // one with enough entries beside it: the same index either way.
        let journal = JournalId::new();
        for filler in [0, THREAD_FROM] {
            let entries = (0..filler).map(|i| entry(journal, &format!("filler{i}"))).collect();
            let slot = IndexSlot::build(entries, vec![note.clone()], extras.clone());
            assert_eq!(hits(&slot, "budget"), 1);
            assert_eq!(hits(&slot, "agenda"), 1);
        }
    }

    #[test]
    fn dropping_the_slot_stops_the_build_and_wakes_whoever_waits() {
        let journal = JournalId::new();
        let entries: Vec<Entry> = (0..5000).map(|i| entry(journal, &format!("word{i}"))).collect();
        let slot = IndexSlot::build(entries, Vec::new(), HashMap::new());
        let waiter = slot.waiter();
        drop(slot);
        let started = Instant::now();
        waiter.wait(READY_WAIT);
        assert!(started.elapsed() < Duration::from_secs(5), "a dropped slot left a search waiting");
    }
}
