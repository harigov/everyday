//! The unlocked session: what state exists while the vault is open, and the
//! handful of primitives every other file in this module is built from.
//!
//! Every lock this crate takes on a [`Vault`](super::Vault) goes through the
//! four accessors here, and every read or write of the store goes through
//! [`Vault::read`] or [`Vault::write`]. The eight optional stores --
//! tasks, calendars, the library, purpose, trackers, the assistant,
//! routines, notes -- each used to hand-roll the same "does this backend
//! carry it, and if not, say so" lookup; [`Vault::with_domain`] is that
//! lookup written once, and [`Domain`] is the label it is written once for.

use crate::error::{Error, Result};
use crate::search::SearchIndex;
use crate::store::JournalStore;
use std::sync::{Arc, RwLockReadGuard, RwLockWriteGuard};

use super::Vault;
use crate::crypto::Cipher;

/// Live state that exists only while unlocked.
pub(super) struct Unlocked {
    pub(super) store: Box<dyn JournalStore>,
    pub(super) index: SearchIndex,
    /// The same cipher the store was handed. Kept so the vault can reseal
    /// its backend settings -- a rotated database password -- without asking
    /// for the vault password a second time. Dropped, with the key inside
    /// it, by [`Vault::lock`].
    pub(super) cipher: Arc<dyn Cipher>,
}

/// Which optional facade a call wants from the backend.
///
/// Exists so the eight `with_x` accessors on [`Vault`] -- one per optional
/// store -- share a single lookup instead of each spelling out its own
/// "this backend does not support..." message. The strings below are the
/// whole reason this type exists: they are policy, shown to whoever asked
/// for something a Markdown vault cannot hold, and collecting them in one
/// match is what keeps a rewording of one from leaving the other seven
/// behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Domain {
    Tasks,
    Calendars,
    Library,
    Purpose,
    Trackers,
    Agent,
    Routines,
    Notes,
}

impl Domain {
    /// The word this domain is missing, exactly as it appears after
    /// "this backend does not support" in [`Error::Unsupported`].
    const fn message(self) -> &'static str {
        match self {
            Domain::Tasks => "tasks (this vault's backend stores journals only)",
            Domain::Calendars => "calendars (this vault's backend stores journals only)",
            Domain::Library => "a library (this vault's backend stores journals only)",
            Domain::Purpose => "roles and goals (this vault's backend stores journals only)",
            Domain::Trackers => "tracking (this vault's backend stores journals only)",
            Domain::Agent => "the assistant (this vault's backend stores journals only)",
            Domain::Routines => "routines (this vault's backend stores journals only)",
            Domain::Notes => "notes (this vault's backend stores journals only)",
        }
    }
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Borrow the store `domain` names out of `store`, or explain that this
/// backend does not carry it.
///
/// Split out of [`Vault::with_domain`] because [`Vault::save_note`] and its
/// two neighbours need the same lookup inside [`Vault::write`], which hands
/// them the mutable half of [`Unlocked`] that `with_domain`'s `read` cannot
/// reach.
pub(super) fn pick_domain<'u, S: ?Sized>(
    store: &'u dyn JournalStore,
    domain: Domain,
    pick: impl FnOnce(&'u dyn JournalStore) -> Option<&'u S>,
) -> Result<&'u S> {
    pick(store).ok_or(Error::Unsupported(domain.message()))
}

impl Vault {
    // ---- the write claim -------------------------------------------------

    /// May this process write to the vault?
    ///
    /// False when another process held the directory's write lock at open
    /// time. The vault still reads; see [`crate::lockfile`] for why that is
    /// the right split.
    pub fn is_writable(&self) -> bool {
        self.write_lock.is_some()
    }

    /// Guard at the top of every method that changes something on disk.
    ///
    /// Deliberately one call per mutator rather than something clever in
    /// `write`: the read/write split in this type does not line up with the
    /// lock helpers -- `save_calendar` goes through `read` because
    /// `with_calendars` does -- so a central hook would either miss writes or
    /// refuse reads. A `self.writable()?` on each is greppable, and a new
    /// mutator that forgets it is a visible omission rather than an invisible
    /// one.
    pub(super) fn writable(&self) -> Result<()> {
        if self.write_lock.is_some() {
            return Ok(());
        }
        Err(Error::VaultInUse {
            holder: crate::lockfile::holder(&self.root)
                .map(|h| h.to_string())
                .unwrap_or_else(|| "another process".into()),
        })
    }

    // ---- lock accessors --------------------------------------------------
    //
    // Every lock in this type is taken through the four helpers below, and
    // none of them treats poisoning as fatal.
    //
    // `unwrap()` on a poisoned lock turns a single panic anywhere in the
    // process into a vault that can never be used again: every later read and
    // every later *write* panics on the poison flag. For an app whose whole
    // job is not to lose what someone typed, that is the worst available
    // response -- the window stays open, the editor keeps accepting text, and
    // each autosave dies on a flag set minutes ago.
    //
    // What poisoning warns about does not really arise here either. The state
    // it guards is an open store plus a search index; a panic between the two
    // can leave the index stale, which shows up as a search result pointing
    // at an edited entry and is repaired by `reindex`. That is worth
    // continuing through, and losing the session's writing is not.

    pub(super) fn state_read(&self) -> RwLockReadGuard<'_, Option<Unlocked>> {
        self.state.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn state_write(&self) -> RwLockWriteGuard<'_, Option<Unlocked>> {
        self.state.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn header_read(&self) -> RwLockReadGuard<'_, super::VaultHeader> {
        self.header.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn header_write(&self) -> RwLockWriteGuard<'_, super::VaultHeader> {
        self.header.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---- data access ----------------------------------------------------

    /// Run `f` against the unlocked store, or fail with [`Error::Locked`].
    pub(super) fn read<T>(&self, f: impl FnOnce(&Unlocked) -> Result<T>) -> Result<T> {
        let guard = self.state_read();
        let unlocked = guard.as_ref().ok_or(Error::Locked)?;
        let out = f(unlocked);
        drop(guard);
        out
    }

    pub(super) fn write<T>(&self, f: impl FnOnce(&mut Unlocked) -> Result<T>) -> Result<T> {
        let mut guard = self.state_write();
        let unlocked = guard.as_mut().ok_or(Error::Locked)?;
        let out = f(unlocked);
        drop(guard);
        out
    }

    /// Read the store under a read lock, or answer `Unsupported` for
    /// `domain` if `pick` finds nothing there.
    ///
    /// The one thing each of the eight `with_x` methods across this module
    /// now does: pick the store out of `dyn JournalStore` and call `f` on
    /// it, or explain that this backend does not carry it. What used to be
    /// eight copies of that same "look it up, or an `Unsupported` naming
    /// this domain" is this call plus a `Domain` and a closure.
    ///
    /// `pick` takes `f`'s call inside itself (`|s| s.tasks().map(f)`) rather
    /// than handing back the picked store for `with_domain` to call `f` on
    /// separately. A `with_x` wrapper's `f` arrives as an opaque type chosen
    /// by that wrapper's own signature, and asking the compiler to match it
    /// against a second, independently generic parameter here -- one
    /// `impl Trait` elaborated against another -- is exactly the shape of
    /// closure-forwarding rustc does not thread borrowed-store lifetimes
    /// through. Calling `f` from inside the closure the wrapper writes at
    /// its own call site sidesteps that; nothing here needs `S` named at
    /// all once it does.
    pub(super) fn with_domain<T>(
        &self,
        domain: Domain,
        pick: impl FnOnce(&dyn JournalStore) -> Option<Result<T>>,
    ) -> Result<T> {
        self.read(|u| pick(u.store.as_ref()).unwrap_or(Err(Error::Unsupported(domain.message()))))
    }
}

#[cfg(test)]
mod tests {
    use super::Domain;

    // Pinned because these are the exact words each `with_x` method used to
    // spell out by hand, copied verbatim into `Domain::message` rather than
    // reworded on the way in. A change here is a change to what somebody
    // reads when a Markdown vault refuses a tool call, and that is worth
    // a test noticing on purpose rather than as a side effect of some other
    // one failing.
    #[test]
    fn each_domain_names_the_same_gap_the_old_with_x_methods_did() {
        assert_eq!(Domain::Tasks.to_string(), "tasks (this vault's backend stores journals only)");
        assert_eq!(
            Domain::Calendars.to_string(),
            "calendars (this vault's backend stores journals only)"
        );
        assert_eq!(
            Domain::Library.to_string(),
            "a library (this vault's backend stores journals only)"
        );
        assert_eq!(
            Domain::Purpose.to_string(),
            "roles and goals (this vault's backend stores journals only)"
        );
        assert_eq!(
            Domain::Trackers.to_string(),
            "tracking (this vault's backend stores journals only)"
        );
        assert_eq!(
            Domain::Agent.to_string(),
            "the assistant (this vault's backend stores journals only)"
        );
        assert_eq!(
            Domain::Routines.to_string(),
            "routines (this vault's backend stores journals only)"
        );
        assert_eq!(Domain::Notes.to_string(), "notes (this vault's backend stores journals only)");
    }
}
