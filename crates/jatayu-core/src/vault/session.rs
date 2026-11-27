//! The unlocked session: what state exists while the vault is open, and the
//! handful of primitives every other file in this module is built from.
//!
//! Every lock this crate takes on a [`Vault`](super::Vault) goes through the
//! four accessors here, and every read or write of the store goes through
//! [`Vault::read`] or [`Vault::write`]. The twelve optional stores --
//! tasks, calendars, the library, purpose, trackers, the assistant,
//! routines, notes, per-record secrets, accounts, mail, meetings -- each
//! used to hand-roll the same "does this backend carry it, and if not, say
//! so" lookup;
//! [`Vault::with_domain`] is that lookup written once, and [`Domain`] is the
//! label it is written once for.
//!
//! # Which writes are exclusive
//!
//! [`Vault::write`] takes the state lock exclusively, so it waits for every
//! reader and every later reader waits for it. That is what the mail,
//! account, model, profile and upkeep writers still use: their read-then-write
//! sections share data with writers that run under the shared lock (sync's
//! flag and body saves among them), and only the exclusive lock keeps those
//! out.
//!
//! Four domains have no such writer, because every write to their tables
//! goes through this module: writing (entries, notes, transcripts and the
//! search index), screen time, health and money. Their writes take the
//! shared lock plus that domain's own gate ([`Vault::write_in`]), so they are
//! one at a time within the domain, as before, and still wait for -- and are
//! waited for by -- every exclusive section, but no longer stop every read in
//! the vault for an fsync. The one other way in for a writer,
//! [`Vault::with_store_writing`] (an import), takes every gate, so it keeps
//! the gated writers out just as its shared lock kept out the exclusive
//! ones. So does [`Vault::with_store_quiet`] (an export), whose reads must
//! agree with each other as they did when those writers were exclusive.
//!
//! A delete that cascades into another domain's rows -- a journal's, a
//! note's -- stays exclusive, as does the meeting templates' move.
//!
//! # Lock order
//!
//! state lock (shared) -> domain gates, in [`Gates`]' field order -> the
//! search index's mutex -> the store's own locks. A gated section never
//! takes the state lock again (a second shared acquisition behind a queued
//! writer is a deadlock), never takes another gate, and never calls back
//! into a `Vault` method that locks; it works on the [`Unlocked`] it is
//! handed. The header lock and the integrity check's slot are leaves, taken
//! on their own.

use crate::error::{Error, Result};
use crate::store::JournalStore;
use crate::store::accounts::AccountStore;
use crate::store::agent::AgentStore;
use crate::store::files::FilesStore;
use crate::store::health::HealthStore;
use crate::store::mail::MailStore;
use crate::store::meetings::MeetingStore;
use crate::store::money::MoneyStore;
use crate::store::notes::NoteStore;
use crate::store::screentime::ScreenTimeStore;
use crate::store::secrets::SecretStore;
use std::sync::{Arc, Mutex, MutexGuard, RwLockReadGuard, RwLockWriteGuard};

use super::Vault;
use super::index::IndexSlot;
use crate::crypto::{Cipher, SecretKey};

/// One gate per domain whose writes no longer take the exclusive lock -- see
/// the module doc.
#[derive(Default)]
pub(super) struct Gates {
    /// Entries, notes, transcripts, and the search index over them.
    pub(super) text: Mutex<()>,
    pub(super) screen: Mutex<()>,
    pub(super) health: Mutex<()>,
    pub(super) money: Mutex<()>,
}

impl Gates {
    /// Every gate, taken in field order -- the one order two gates are ever
    /// held together in. For [`Vault::with_store_writing`].
    pub(super) fn all(&self) -> [MutexGuard<'_, ()>; 4] {
        [&self.text, &self.screen, &self.health, &self.money].map(take_gate)
    }
}

/// Take `gate`. Poisoned only by a panic in another write of the same
/// domain, which left nothing half-held: the store's own transaction is
/// what made that write all or nothing.
pub(super) fn take_gate(gate: &Mutex<()>) -> MutexGuard<'_, ()> {
    gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Live state that exists only while unlocked.
pub(super) struct Unlocked {
    pub(super) store: Box<dyn JournalStore>,
    /// Behind its own mutex, so the gated writers can keep it in step with
    /// the store under the shared lock. See [`super::index`].
    pub(super) index: IndexSlot,
    /// The same cipher the store was handed. Kept so the vault can reseal
    /// its backend settings -- a rotated database password -- without asking
    /// for the vault password a second time. Dropped, with the key inside
    /// it, by [`Vault::lock`].
    pub(super) cipher: Arc<dyn Cipher>,
    /// The meeting settings' old template list is known to have been moved
    /// into the templates table -- see `Vault::migrate_meeting_templates`.
    /// Held here so the check costs nothing after the first read of a
    /// session, and set under the write lock the move itself runs under.
    pub(super) templates_moved: bool,
    /// The integrity stamp, set aside while the store is open -- `None` in a
    /// read-only process. Last, so it is put back only after the store has
    /// closed. See [`super::maintenance::OpenStamp`].
    pub(super) _stamp: Option<super::maintenance::OpenStamp>,
}

impl Unlocked {
    /// The store for one optional domain, or `Unsupported` naming it when this
    /// backend does not carry it: [`pick_domain`] with the domain and its
    /// accessor written once, for the sections that already hold the lock.
    pub(super) fn notes(&self) -> Result<&dyn NoteStore> {
        pick_domain(self.store.as_ref(), Domain::Notes, |s| s.notes())
    }

    pub(super) fn health(&self) -> Result<&dyn HealthStore> {
        pick_domain(self.store.as_ref(), Domain::Health, |s| s.health())
    }

    pub(super) fn money(&self) -> Result<&dyn MoneyStore> {
        pick_domain(self.store.as_ref(), Domain::Money, |s| s.money())
    }

    pub(super) fn accounts(&self) -> Result<&dyn AccountStore> {
        pick_domain(self.store.as_ref(), Domain::Accounts, |s| s.accounts())
    }

    pub(super) fn mail(&self) -> Result<&dyn MailStore> {
        pick_domain(self.store.as_ref(), Domain::Mail, |s| s.mail())
    }

    pub(super) fn meetings(&self) -> Result<&dyn MeetingStore> {
        pick_domain(self.store.as_ref(), Domain::Meetings, |s| s.meetings())
    }

    pub(super) fn screentime(&self) -> Result<&dyn ScreenTimeStore> {
        pick_domain(self.store.as_ref(), Domain::ScreenTime, |s| s.screentime())
    }

    pub(super) fn files(&self) -> Result<&dyn FilesStore> {
        pick_domain(self.store.as_ref(), Domain::Files, |s| s.files())
    }

    pub(super) fn agent(&self) -> Result<&dyn AgentStore> {
        pick_domain(self.store.as_ref(), Domain::Agent, |s| s.agent())
    }

    /// A subkey of the vault key for `label`, for the sections that already
    /// hold the lock ([`Vault::derive_subkey`] would take it a second time).
    pub(super) fn subkey(&self, label: &str) -> SecretKey {
        self.cipher.derive_subkey(label)
    }
}

/// Which optional facade a call wants from the backend.
///
/// Exists so the twelve `with_x` accessors on [`Vault`] -- one per optional
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
    /// Background work -- see [`crate::job`].
    Jobs,
    /// Writing help -- see [`crate::writing`].
    Writing,
    /// Work the assistant prepared -- see [`crate::proposal`].
    Proposals,
    Notes,
    /// Per-record secrets -- see [`crate::store::secrets`]. Accounts are the
    /// first, and so far the only, caller.
    Secrets,
    /// A mailbox provider signed in to -- see [`crate::account`].
    Accounts,
    /// Mail: mailboxes, messages, threads, bodies, drafts and the outbox --
    /// see [`crate::mail`] and [`crate::store::mail`].
    Mail,
    /// Meeting notes: recordings, transcripts and voiceprints -- see
    /// [`crate::meeting`] and [`crate::store::meetings`].
    Meetings,
    /// The Feed: sources, stories and their bodies -- see [`crate::news`]
    /// and [`crate::store::news`].
    News,
    /// Money: links, accounts, transactions, categories and rules -- see
    /// [`crate::money`] and [`crate::store::money`].
    Money,
    /// Screen time: spans, labels, categories and settings -- see
    /// [`crate::screentime`] and [`crate::store::screentime`].
    ScreenTime,
    /// The files catalog -- see [`crate::files`] and [`crate::store::files`].
    Files,
    /// Health: series, sources, workouts, medications, conditions, labs,
    /// documents, allergies, foods and meals -- see [`crate::health`] and
    /// [`crate::store::health`].
    Health,
}

impl Domain {
    /// The word this domain is missing, exactly as it appears after
    /// "this backend does not support" in [`Error::Unsupported`].
    const fn message(self) -> &'static str {
        match self {
            Domain::Tasks => "tasks (this vault's backend stores journals only)",
            Domain::Calendars => "calendars (this vault's backend stores journals only)",
            Domain::Library => "a library (this vault's backend stores journals only)",
            Domain::Purpose => "areas and goals (this vault's backend stores journals only)",
            Domain::Trackers => "tracking (this vault's backend stores journals only)",
            Domain::Agent => "the assistant (this vault's backend stores journals only)",
            Domain::Routines => "routines (this vault's backend stores journals only)",
            Domain::Jobs => "background jobs (this vault's backend stores journals only)",
            Domain::Writing => "writing help (this vault's backend stores journals only)",
            Domain::Proposals => "proposals (this vault's backend stores journals only)",
            Domain::Notes => "notes (this vault's backend stores journals only)",
            Domain::Secrets => "per-record secrets (this vault's backend stores journals only)",
            Domain::Accounts => "accounts (this vault's backend stores journals only)",
            Domain::Mail => "mail (this vault's backend stores journals only)",
            Domain::Meetings => "meeting notes (this vault's backend stores journals only)",
            Domain::News => "a feed (this vault's backend stores journals only)",
            Domain::Money => "money (this vault's backend stores journals only)",
            Domain::ScreenTime => "screen time (this vault's backend stores journals only)",
            Domain::Files => "files (this vault's backend stores journals only)",
            Domain::Health => "health (this vault's backend stores journals only)",
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
/// Split out of [`Vault::with_domain`] for the sections that already hold
/// the lock and have the whole [`Unlocked`] in hand -- [`Vault::write`]'s and
/// [`Vault::write_in`]'s.
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

    /// Run a write of one gated domain: the shared lock, then `gate`. One
    /// writer of that domain at a time, as under [`Vault::write`], while
    /// readers -- and the other domains' writers -- carry on. See the module
    /// doc for which domains, and for what `f` must not do.
    pub(super) fn write_in<T>(
        &self,
        gate: &Mutex<()>,
        f: impl FnOnce(&Unlocked) -> Result<T>,
    ) -> Result<T> {
        let guard = self.state_read();
        let unlocked = guard.as_ref().ok_or(Error::Locked)?;
        let held = take_gate(gate);
        let out = f(unlocked);
        drop(held);
        drop(guard);
        out
    }

    /// Read the store under a read lock, or answer `Unsupported` for
    /// `domain` if `pick` finds nothing there.
    ///
    /// The one thing each of the twelve `with_x` methods across this module
    /// now does: pick the store out of `dyn JournalStore` and call `f` on
    /// it, or explain that this backend does not carry it. What used to be
    /// as many copies of that same "look it up, or an `Unsupported` naming
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

    /// [`Vault::with_domain`] for the per-record secrets a vault keeps beside
    /// its records: account passwords, chat tokens, backup and bank keys.
    ///
    /// One copy for every file that seals something there. It used to be
    /// written once per file under a different name each time, because item
    /// privacy is per module; `pub(super)` here reaches all of them.
    pub(super) fn with_secrets<T>(
        &self,
        f: impl FnOnce(&dyn SecretStore) -> Result<T>,
    ) -> Result<T> {
        self.with_domain(Domain::Secrets, |s| s.secrets().map(f))
    }

    /// [`Vault::with_domain`] for the assistant's conversations, memories
    /// and skills; shared by the agent and chat modules.
    pub(super) fn with_agent<T>(&self, f: impl FnOnce(&dyn AgentStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Agent, |s| s.agent().map(f))
    }

    /// Record that a write just landed for `kind`'s row `id` -- see
    /// [`super::touched`] for who reads this and why it costs nothing when
    /// nobody is.
    ///
    /// A short name and a method on `Vault` rather than a free function
    /// call at each of the ~110 sites that need it, so `self.wrote(...)`
    /// reads the same as the `self.writable()?` already at the top of every
    /// one of them. Not `touch`: [`Vault::touch`] already means "defer the
    /// idle timeout", a different fact about a different moment.
    pub(super) fn wrote(&self, kind: crate::record::RecordKind, id: impl std::fmt::Display) {
        super::touched::touch(kind, id);
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
            "areas and goals (this vault's backend stores journals only)"
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
        assert_eq!(
            Domain::Secrets.to_string(),
            "per-record secrets (this vault's backend stores journals only)"
        );
        assert_eq!(
            Domain::Accounts.to_string(),
            "accounts (this vault's backend stores journals only)"
        );
        assert_eq!(Domain::Mail.to_string(), "mail (this vault's backend stores journals only)");
        assert_eq!(
            Domain::Meetings.to_string(),
            "meeting notes (this vault's backend stores journals only)"
        );
        assert_eq!(Domain::News.to_string(), "a feed (this vault's backend stores journals only)");
    }
}
