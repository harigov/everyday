//! Opening mail's storage when the vault unlocks, and registering one
//! supervised sync task per account.
//!
//! # Where each piece lives
//!
//! - **The pack store.** On a vault whose backend keeps its own files on
//!   this machine (SQLite, today), a [`FilePackStore`] in `mail-packs/`
//!   beside the backend's `media/` -- both live under
//!   [`Vault::store_root`]. On a vault with no local disk of its own
//!   (Postgres), [`VaultPacks`] reaches the same
//!   [`jatayu_core::packstore::PackStore`] trait through
//!   [`Vault::with_mail_packs`], one call at a time, rather than holding a
//!   connection this module does not own.
//! - **The search index.** A [`MailIndex`] in `mail-index/` beside the
//!   vault's own header, for a local backend. For Postgres there is no
//!   local vault directory worth writing an index into that would survive
//!   the vault moving to another machine -- the index is a derived
//!   structure that [`jatayu_core::MailSearch::rebuild_needed`] already
//!   promises can always be rebuilt, so it lives in this device's own cache
//!   directory instead, keyed by a hash of the vault's path so two vaults on
//!   one machine (or the same vault reopened) do not collide or share.
//!
//! # Keys
//!
//! Neither store reuses the vault's own cipher the way
//! [`jatayu_core::blobstore::FileBlobStore`] and media do -- see
//! [`jatayu_core::crypto::Cipher::derive_subkey`] for why mail's storage,
//! being new, gets a key of its own by label rather than the vault's.
//! [`PACKSTORE_KEY_LABEL`] and [`SEARCHINDEX_KEY_LABEL`] are those labels,
//! and they are never reused for anything else.
//!
//! # The write claim
//!
//! Both stores open in *every* process that unlocks the vault -- a second
//! window, or a read-only `jatayu-server`, still wants to search mail and
//! show an attachment. Only [`open`] additionally registers a sync task per
//! account, and only when [`Vault::is_writable`] is true: a process with no
//! write claim has nothing to sync with, since every write a sync task makes
//! -- a header ingested, a flag applied -- is a write to the vault.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use jatayu_core::crypto::{AeadCipher, Cipher};
use jatayu_core::error::{Error, Result};
use jatayu_core::id::{AccountId, PackId};
use jatayu_core::packstore::{
    CompactionResult, FilePackStore, PackRef, PackStore, ReferencedSnapshot,
};
use jatayu_core::{MailDoc, MailQuery, MailSearch, MessageKey, SearchCursor, SearchPage, Vault};
use jatayu_mailindex::MailIndex;

use crate::mail::sync::status::StatusRegistry;
use crate::service::Service;

/// Domain separator for the pack store's subkey -- see the module docs.
const PACKSTORE_KEY_LABEL: &str = "everyday.mail.packstore.v1";
/// Domain separator for the search index's subkey.
const SEARCHINDEX_KEY_LABEL: &str = "everyday.mail.searchindex.v1";

/// How much decrypted tantivy segment data [`MailIndex`] keeps warm in
/// memory beyond what its open searcher already holds. See that crate's
/// `cache` module for why a sealed segment is cached rather than
/// re-decrypted on every read; 128 MiB is generous for a laptop without
/// being a meaningful fraction of one.
const INDEX_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// What [`Service::packs`], [`Service::mail_index`] and
/// [`Service::mail_statuses`] answer once a vault is unlocked.
pub struct MailState {
    pub(crate) packs: Arc<dyn PackStore>,
    pub(crate) index: Arc<dyn jatayu_core::MailSearch>,
    pub(crate) statuses: StatusRegistry,
    pub(crate) unread_cache: Arc<crate::mail::sync::unread_cache::UnreadCache>,
    pub(crate) contacts: Arc<crate::mail::sync::contacts::ContactIndex>,
}

/// Open mail's storage against `vault`, and start its account tasks if this
/// process may write to it. See the module docs.
///
/// The pack store is open by the time this returns; the search index is
/// still opening. Opening it reads and decrypts every segment file -- the
/// whole index, a few hundred megabytes for a large mailbox -- and this runs
/// on the unlock path, so it happens on the blocking pool instead, behind a
/// [`DeferredIndex`] that is installed at once: [`Service::mail_index`]
/// answers `Some` immediately, and a call that reaches it before the open
/// finishes waits for it. Without a tokio runtime (tests) the index opens
/// here, as it always did.
///
/// Never fails outright: a pack store that cannot be opened -- a
/// permissions problem, a corrupt directory -- is logged and leaves
/// [`Service::packs`]/[`Service::mail_index`] answering `None`, and an
/// index that cannot be opened answers every call with an error and
/// [`jatayu_core::MailSearch::rebuild_needed`] with `true` -- the same
/// "derived structure, not the only copy of anything" tolerance that method
/// already asks callers to have. Nothing about a vault's own records is at
/// risk either way.
///
/// Returns the handle of the background open, which also runs the one-time
/// reindex the open queues when
/// [`jatayu_mailindex::MailIndex::healed_on_open`] comes back `true` -- see
/// [`reindex_after_self_heal`]'s own docs. Every real caller
/// (`Service::open_mail`) drops it, letting the work run detached exactly
/// as intended; it is returned at all only so a test can await the very
/// task a real unlock fires and forgets, rather than triggering a second,
/// separate reindex just to have something to `await`.
pub(crate) fn open(svc: &Arc<Service>, vault: &Arc<Vault>) -> Option<tokio::task::JoinHandle<()>> {
    let packs = match open_packs(vault) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "could not open the mail pack store");
            svc.set_mail_state(None);
            return None;
        }
    };
    let index = Arc::new(DeferredIndex::default());
    let previous = succeed_index(&index_dir(vault), &index.released);
    let opening = Opening { slot: Arc::downgrade(&index), vault: Arc::downgrade(vault), previous };
    // One small sealed row, read once here -- see `ContactIndex`'s own docs
    // on why this is not a scan of every message on unlock.
    let contacts = Arc::new(crate::mail::sync::contacts::ContactIndex::load(vault));
    // Installed before the open starts, so the state this replaces -- and
    // the index it held, with tantivy's writer lock -- is gone first.
    svc.set_mail_state(Some(MailState {
        packs,
        index,
        statuses: StatusRegistry::new(),
        unread_cache: Arc::new(crate::mail::sync::unread_cache::UnreadCache::new()),
        contacts: contacts.clone(),
    }));
    let opened = opening.start();

    if vault.is_writable() {
        register_account_tasks(svc, vault);
        backfill_contacts(vault, contacts);
    }
    opened
}

/// Serialises every open of the search index in this process, and the drop
/// of one opened for a state that was replaced while it opened. Without it,
/// a lock and a quick unlock could start the second open while the first
/// still held tantivy's writer lock, and the second would come up dead
/// until the next unlock. An index dropped by `close` itself needs no help:
/// that drop is over before `close` returns. One something else still held
/// when `close` let go of it is waited for -- see [`succeed_index`].
static OPENS: Mutex<()> = Mutex::new(());

/// The [`Released`] of the last [`DeferredIndex`] made for each index folder
/// in this process -- see [`succeed_index`].
static LATEST: Mutex<Option<HashMap<PathBuf, Weak<Released>>>> = Mutex::new(None);

/// Note `released` as the newest index for `dir`, handing back the one it
/// follows if that is still about: an index `close` let go of that a blocking
/// job -- a batch of bodies being indexed, a commit, a search -- still holds,
/// with tantivy's writer lock, past the lock that ended its pass. An open
/// that does not wait for it finds the writer lock taken and comes up dead
/// for the whole session.
fn succeed_index(dir: &Path, released: &Arc<Released>) -> Option<Arc<Released>> {
    let mut latest = LATEST.lock().unwrap_or_else(PoisonError::into_inner);
    let latest = latest.get_or_insert_with(HashMap::new);
    latest.retain(|_, held| held.strong_count() > 0);
    latest.insert(dir.to_path_buf(), Arc::downgrade(released)).and_then(|held| held.upgrade())
}

/// The longest an open waits for the index it follows to be let go of -- a
/// batch of bodies indexed and committed by a job that outlived its pass is
/// a few seconds at worst. Past it, the open goes ahead as it always did.
const PREVIOUS_INDEX_WAIT: Duration = Duration::from_secs(10);

/// Raised once a [`DeferredIndex`] and the [`MailIndex`] in it are gone, and
/// tantivy's writer lock with them -- see [`succeed_index`].
#[derive(Default)]
struct Released {
    done: Mutex<bool>,
    gone: Condvar,
}

impl Released {
    fn raise(&self) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.gone.notify_all();
    }

    /// Wait until raised, for at most `most`, and only while `wanted`.
    fn wait(&self, most: Duration, wanted: impl Fn() -> bool) {
        let until = Instant::now() + most;
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        while !*done && wanted() {
            let now = Instant::now();
            if now >= until {
                tracing::warn!("the last search index is still held; opening the next anyway");
                return;
            }
            // Woken a little early now and then, to notice `wanted` turn.
            let step = (until - now).min(Duration::from_millis(50));
            done = self.gone.wait_timeout(done, step).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// The open of one [`DeferredIndex`]'s [`MailIndex`], not yet run. See
/// [`open`].
struct Opening {
    /// Weak, so a lock arriving first is noticed rather than outlived: once
    /// the state holding the index is gone, there is nobody to open it for.
    slot: Weak<DeferredIndex>,
    /// Weak too: an open still waiting its turn when the vault is closed must
    /// not keep it -- and its write lock -- alive past the close. Held strong
    /// only for the open itself, which [`wait_for_opens`] lets a close wait
    /// out.
    vault: Weak<Vault>,
    /// The index this one follows in the same folder, if something may still
    /// hold it -- see [`succeed_index`].
    previous: Option<Arc<Released>>,
}

impl Opening {
    /// Run the open on the blocking pool, followed by the reindex a
    /// self-heal asks for, and hand back the handle of the whole job.
    /// Without a tokio runtime (tests) both run here and now, since there is
    /// no async worker to keep free.
    fn start(self) -> Option<tokio::task::JoinHandle<()>> {
        let vault = self.vault.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            if let Some(index) = self.run()
                && let Some(vault) = vault.upgrade()
                && let Err(e) = reindex_all(&vault, index.as_ref())
            {
                tracing::error!(
                    error = %e,
                    "could not reindex mail after the search index self-healed"
                );
            }
            return None;
        };
        // Started here rather than from inside the task below: a task is
        // first polled whenever its runtime gets to it, and a caller already
        // waiting on the index must not depend on that.
        let opening = runtime.spawn_blocking(move || self.run());
        Some(runtime.spawn(async move {
            if let Ok(Some(index)) = opening.await
                && let Some(vault) = vault.upgrade()
            {
                reindex_after_self_heal(vault, index).await;
            }
        }))
    }

    /// Open the index into the slot, unless the state it belongs to has
    /// already been replaced. Returns the index when the open self-healed it
    /// and this process should refill it -- see [`reindex_after_self_heal`].
    fn run(&self) -> Option<Arc<DeferredIndex>> {
        let _opening = OPENS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous) = &self.previous {
            previous.wait(PREVIOUS_INDEX_WAIT, || self.slot.strong_count() > 0);
        }
        if self.slot.strong_count() == 0 {
            // Locked, or opened again, before this got going.
            return None;
        }
        // Closed before this got going: nothing to open it for.
        let vault = self.vault.upgrade()?;
        let opened = if vault_folder_gone(&vault) {
            // Deleted while this waited its turn (a test's temporary vault,
            // most likely): opening would create the folder all over again.
            Err(Error::Invalid("the vault's folder is gone".into()))
        } else {
            open_index(&vault)
        };
        if let Err(e) = &opened {
            tracing::error!(error = %e, "could not open the mail search index");
        }
        // Both of these are dropped before `_opening`: an index opened for a
        // state that is gone, and this function's own hold on one that is
        // not, which may be the last if the vault locked a moment ago.
        let index = self.slot.upgrade()?;
        // Read before `opened` moves into the slot --
        // `jatayu_mailindex::MailIndex::healed_on_open`'s own docs promise
        // this is the one moment it is worth checking. See
        // `reindex_after_self_heal` for what a `true` here means.
        let healed = matches!(&opened, Ok(opened) if opened.healed_on_open());
        index.fill(opened);
        // Only when writable: reindexing writes nothing to the vault itself,
        // only to the index, but it is real disk-bound work (a full walk of
        // every account's threads and bodies), and gating it the same way
        // `register_account_tasks` already is keeps a second, read-only
        // window (or `jatayu-server`) from redoing the exact same walk the
        // writable process is already about to run.
        (healed && vault.is_writable()).then_some(index)
    }
}

/// Wait for an index open already under way to finish, and so to let go of
/// its vault: what a close does after dropping mail's state, so the vault it
/// closes is not kept open -- and locked for writing -- by an open that
/// started a moment before. An open that has not started yet finds the state
/// gone and does nothing.
pub(crate) fn wait_for_opens() {
    drop(OPENS.lock().unwrap_or_else(PoisonError::into_inner));
}

impl Drop for Opening {
    /// Answers whoever is waiting with an error if the open never finished
    /// -- it panicked, or a runtime shutting down dropped it unstarted --
    /// rather than leaving them waiting for ever.
    fn drop(&mut self) {
        // Under the lock, for the same reason as the drops in `run`: the
        // hold taken here may turn out to be the last.
        let _opening = OPENS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = self.slot.upgrade()
            && index.opened.get().is_none()
        {
            index.fill(Err(Error::Invalid("opening the mail search index failed".into())));
        }
    }
}

/// The search index [`MailState`] holds from the moment the vault unlocks,
/// while the [`MailIndex`] behind it is still opening. See [`open`].
///
/// Every method waits until the open has finished, then answers exactly as
/// the [`MailIndex`] would; after a failed open, every call answers an error
/// and [`MailSearch::rebuild_needed`] answers `true`. Waiting is fine where
/// these are called: the trait already asks to be called on a blocking pool.
#[derive(Default)]
struct DeferredIndex {
    opened: OnceLock<std::result::Result<MailIndex, String>>,
    waiting: Mutex<()>,
    ready: Condvar,
    /// Raised as this is dropped -- see [`succeed_index`].
    released: Arc<Released>,
}

impl Drop for DeferredIndex {
    /// Commits what was indexed and not yet made searchable before the
    /// writer goes, which would discard it: the mail of a pass a lock cut
    /// short is stored, so nothing would index it again. A no-op when nothing
    /// is waiting, which is every drop but that one; and never a wait, since
    /// an index still opening has nothing in it.
    fn drop(&mut self) {
        if let Some(Ok(index)) = self.opened.take() {
            let _ = index.commit();
        }
        self.released.raise();
    }
}

impl DeferredIndex {
    fn fill(&self, opened: Result<MailIndex>) {
        if self.opened.set(opened.map_err(|e| e.to_string())).is_err() {
            return;
        }
        // Taken so a waiter cannot check the cell and then miss this.
        let _waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        self.ready.notify_all();
    }

    fn ready(&self) -> Result<&MailIndex> {
        let opened = match self.opened.get() {
            Some(opened) => opened,
            None => {
                let waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
                let _waiting = self
                    .ready
                    .wait_while(waiting, |()| self.opened.get().is_none())
                    .unwrap_or_else(PoisonError::into_inner);
                self.opened.get().expect("waited until the open had finished")
            }
        };
        opened.as_ref().map_err(|_| Error::Invalid("the mail search index is not open".into()))
    }
}

impl MailSearch for DeferredIndex {
    fn index(&self, docs: &[MailDoc]) -> Result<()> {
        self.ready()?.index(docs)
    }

    fn delete(&self, message_ids: &[MessageKey]) -> Result<()> {
        self.ready()?.delete(message_ids)
    }

    fn delete_account(&self, account: &str) -> Result<()> {
        self.ready()?.delete_account(account)
    }

    fn commit(&self) -> Result<()> {
        self.ready()?.commit()
    }

    fn search(
        &self,
        query: &MailQuery,
        limit: usize,
        cursor: Option<SearchCursor>,
    ) -> Result<SearchPage> {
        self.ready()?.search(query, limit, cursor)
    }

    fn rebuild_needed(&self) -> bool {
        match self.ready() {
            Ok(index) => index.rebuild_needed(),
            Err(_) => true,
        }
    }

    fn rebuild_empty(&self) -> Result<()> {
        self.ready()?.rebuild_empty()
    }
}

/// Walk every account's threads and bodies back into a freshly self-healed
/// `index`, once, off the calling thread.
///
/// # Why this exists at all
///
/// `jatayu_mailindex::MailIndex::open` wiping and recreating the sealed
/// index on a schema mismatch (see that crate's own module docs,
/// "self-healing on open") fixes the *dead* half of the outage a persisted
/// tokenizer change caused, but it also leaves `index` genuinely, silently
/// empty. Nothing else in this crate ever re-populates it on its own: the
/// body pass (`crate::mail::sync::passes`) only calls `MailSearch::index` for
/// messages it *newly* fetches this run, never for ones already sitting in
/// storage from a previous sync. Without this function, a self-heal would
/// trade "search answers with an error" for "search answers with nothing",
/// which is not the fix `docs/plans/mail.md`'s search phase promises.
/// `rebuild_account` (`crate::domains::mailsync`) already knows how to walk
/// a vault's own storage back into an index -- this only decides *when* to
/// call it automatically, the one moment the open already knows the index
/// just went from `rebuild_needed() == true` to `false` by itself.
///
/// # Once, not per pass
///
/// Called exactly once per open of the index (see [`Opening::run`]), itself
/// started exactly once per process per vault unlock -- never from inside a
/// sync pass, and never on a timer -- because
/// `jatayu_mailindex::MailIndex::healed_on_open` is itself answered once, at
/// construction, from the one `MailIndex` that open produced. A later pass
/// finding `rebuild_needed() == false` (which this reindex itself brings
/// about) has no reason to call this again, and nothing in this module ever
/// checks `healed_on_open` a second time for the same `MailIndex` to let it.
///
/// # Off the calling thread
///
/// A hundred-thousand-message vault's worth of `rebuild_account` walks is
/// exactly the disk-bound work `service::blocking`'s own docs already warn
/// would "stall every other task" run inline, so the walk goes through
/// `blocking`, exactly like `rebuild_mail_index`'s own command handler.
///
/// # What this does not close
///
/// Two local processes racing to unlock the same on-disk vault at once
/// could both observe `healed_on_open() == true` and both queue this walk
/// -- harmless (the second `delete_account` before each account's own
/// re-index makes the result idempotent either way), but wasted work.
/// Nothing today coordinates across processes on that, the same as nothing
/// coordinates their two `MailIndex`es' self-heal wipes racing each other
/// a moment earlier; both are pre-existing properties of "every process
/// that unlocks the vault opens its own index" (see the module docs), not
/// something this fix introduces or was asked to solve.
async fn reindex_after_self_heal(vault: Arc<Vault>, index: Arc<DeferredIndex>) {
    let result = crate::service::blocking(move || Ok(reindex_all(&vault, index.as_ref())?)).await;
    if let Err(e) = result {
        tracing::error!(
            error = %e,
            "could not reindex mail after the search index self-healed"
        );
    }
}

/// The walk [`reindex_after_self_heal`] runs: every account, through
/// `rebuild_account`.
fn reindex_all(vault: &Vault, index: &dyn MailSearch) -> Result<()> {
    let account_ids: Vec<AccountId> = vault.accounts()?.into_iter().map(|a| a.id).collect();
    for account_id in account_ids {
        crate::domains::mailsync::rebuild_account(vault, index, account_id)?;
    }
    Ok(())
}

/// Run [`crate::mail::sync::contacts::ContactIndex::backfill`] off the
/// calling thread, the way [`reindex_after_self_heal`] runs its walk: a
/// recount decrypts every message the vault holds, once, and [`open`] runs
/// on the unlock command's own task. A no-op after the first time it
/// finishes for a vault -- the book remembers -- so every later unlock pays
/// one flag check and nothing else.
fn backfill_contacts(vault: &Arc<Vault>, contacts: Arc<crate::mail::sync::contacts::ContactIndex>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let vault = vault.clone();
    runtime.spawn(async move {
        let result = crate::service::blocking(move || Ok(contacts.backfill(&vault)?)).await;
        match result {
            Ok(true) => tracing::info!("counted stored mail into the contact book"),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "could not count stored mail into the contact book")
            }
        }
    });
}

/// Drop what [`open`] opened. See `Service::locked`/`Service::close` for the
/// two moments this is called from -- always after the account tasks
/// writing through it have already stopped, or been cut short, in which case
/// the index commits what they left as it goes (see [`DeferredIndex`]'s
/// `Drop`), here or in a blocking job still holding it.
pub(crate) fn close(svc: &Service) {
    svc.set_mail_state(None);
}

fn open_packs(vault: &Arc<Vault>) -> Result<Arc<dyn PackStore>> {
    if vault.backend() == "postgres" {
        return Ok(Arc::new(VaultPacks(vault.clone())));
    }
    let key = vault.derive_subkey(PACKSTORE_KEY_LABEL)?;
    let cipher: Arc<dyn Cipher> = Arc::new(AeadCipher::new(&key));
    let dir = vault.store_root().join("mail-packs");
    Ok(Arc::new(FilePackStore::open(dir, cipher)?))
}

fn open_index(vault: &Vault) -> Result<MailIndex> {
    let key = vault.derive_subkey(SEARCHINDEX_KEY_LABEL)?;
    let cipher: Arc<dyn Cipher> = Arc::new(AeadCipher::new(&key));
    MailIndex::open(index_dir(vault), cipher, INDEX_CACHE_BYTES)
}

/// Whether the vault's own folder -- where its header lives, whatever the
/// backend, and a local vault's index beside it -- no longer exists.
fn vault_folder_gone(vault: &Vault) -> bool {
    !vault.path().is_dir()
}

/// `<vault root>/mail-index` for a local backend -- beside the vault's own
/// header, per the plan. For Postgres, a per-device cache directory: the
/// index can always be rebuilt, so nothing about it belongs in a vault that
/// might be opened from a different machine tomorrow.
fn index_dir(vault: &Vault) -> PathBuf {
    if vault.backend() == "postgres" {
        jatayu_vault::config_dir().join("mail-index").join(cache_key(vault))
    } else {
        vault.path().join("mail-index")
    }
}

/// A stable, filesystem-safe name for `vault`'s own cache directory, so two
/// different vaults on one machine -- or the same vault reopened -- neither
/// collide nor share. Derived from the vault's path rather than anything
/// inside it, since a locked vault (a fresh Postgres one, say, whose header
/// this process has only just read) is exactly when this is first asked.
fn cache_key(vault: &Vault) -> String {
    jatayu_core::fsutil::cache_path_name(vault.path())
}

/// A [`PackStore`] over a Postgres vault's own tables, reached through
/// [`Vault::with_mail_packs`] on every call rather than held across the
/// life of a connection this module does not own -- see the module docs.
struct VaultPacks(Arc<Vault>);

impl PackStore for VaultPacks {
    fn append_batch(&self, account: &str, messages: &[&[u8]]) -> Result<Vec<PackRef>> {
        self.0.with_mail_packs(|p| p.append_batch(account, messages))
    }

    fn read(&self, r: &PackRef) -> Result<Vec<u8>> {
        self.0.with_mail_packs(|p| p.read(r))
    }

    fn mark_dead(&self, refs: &[PackRef]) -> Result<()> {
        self.0.with_mail_packs(|p| p.mark_dead(refs))
    }

    fn delete_account(&self, account: &str) -> Result<()> {
        self.0.with_mail_packs(|p| p.delete_account(account))
    }

    fn high_water_mark(&self, account: &str) -> Result<Option<PackId>> {
        self.0.with_mail_packs(|p| p.high_water_mark(account))
    }

    fn compaction_worthwhile(&self, account: &str, snapshot: &ReferencedSnapshot) -> Result<bool> {
        self.0.with_mail_packs(|p| p.compaction_worthwhile(account, snapshot))
    }

    fn compact(
        &self,
        account: &str,
        snapshot: &ReferencedSnapshot,
        should_continue: &dyn Fn() -> bool,
    ) -> Result<CompactionResult> {
        self.0.with_mail_packs(|p| p.compact(account, snapshot, should_continue))
    }

    fn drop_packs(&self, account: &str, packs: &[PackId]) -> Result<()> {
        self.0.with_mail_packs(|p| p.drop_packs(account, packs))
    }
}

/// Register a supervised task for every account with `services.mail` on. See
/// [`ensure_account_task`].
fn register_account_tasks(svc: &Arc<Service>, vault: &Arc<Vault>) {
    let Ok(accounts) = vault.accounts() else { return };
    for account in accounts {
        if account.services.mail {
            ensure_account_task(svc, vault, account.id);
        }
    }
}

/// The supervisor key one account's sync task is registered under.
pub fn task_key(account_id: AccountId) -> String {
    format!("mail:{account_id}")
}

/// Start (or, if it is already running, leave alone) `account_id`'s sync
/// task -- [`crate::supervisor::Supervisor::ensure`]'s own idempotence is
/// what makes calling this from every unlock, for every account, cheap and
/// safe.
///
/// Public so that a command which changes whether an account syncs --
/// `save_account` switching `services.mail` on, `set_agent_access` on a
/// freshly signed-in account -- can start its task immediately rather than
/// waiting for the next lock/unlock cycle, once such a command calls it.
pub fn ensure_account_task(svc: &Arc<Service>, vault: &Arc<Vault>, account_id: AccountId) {
    let key = task_key(account_id);
    let svc_for_task = svc.clone();
    let vault_for_task = vault.clone();
    svc.supervisor().ensure(key, move |stop| {
        let svc = svc_for_task.clone();
        let vault = vault_for_task.clone();
        Box::pin(
            async move { crate::mail::sync::task::run_account(svc, vault, account_id, stop).await },
        )
    });
}

/// Stop `account_id`'s task without forgetting it -- what a command that
/// switches `services.mail` off should call, so a later switch-on can
/// `ensure_account_task` again without another lock/unlock cycle.
pub async fn stop_account_task(svc: &Service, account_id: AccountId) {
    svc.supervisor().stop(&task_key(account_id)).await;
    if let Some(statuses) = svc.mail_statuses() {
        statuses.set_idle(account_id);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use jiff::Timestamp;

    /// Mail's storage keys are derived under these labels. They are inputs to
    /// the cipher, not names -- the `everyday` in them stays through the
    /// rename for exactly that reason -- and a different label would leave
    /// every packed message and the search index unreadable.
    #[test]
    fn the_storage_key_labels_are_pinned() {
        assert_eq!(PACKSTORE_KEY_LABEL, "everyday.mail.packstore.v1");
        assert_eq!(SEARCHINDEX_KEY_LABEL, "everyday.mail.searchindex.v1");
    }
    use jatayu_core::MailQuery;
    use jatayu_core::account::{Account, Provider};
    use jatayu_core::id::{MailMessageId, MailboxId, PackId, ThreadId};
    use jatayu_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
    };
    use jatayu_core::packstore::PackRef;
    use jatayu_core::store::mail::IngestMessage;

    pub(crate) fn test_vault() -> (tempfile::TempDir, jatayu_core::Vault) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = jatayu_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: jatayu_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = jatayu_vault::create(dir.path(), cfg).unwrap();
        (dir, vault)
    }

    /// Ingests one findable message directly into the vault's own storage,
    /// bypassing the sync engine entirely -- this test is about what
    /// happens to a *previously synced* mailbox, not about syncing one.
    pub(crate) fn seed_message(
        vault: &Vault,
        account: AccountId,
        mailbox: MailboxId,
        subject: &str,
    ) {
        let id = MailMessageId::new();
        let message = Message {
            id,
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{id}@wiring-test.example>"),
            date: Timestamp::now(),
            from: Address::bare("sender@example.com"),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: subject.to_string(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
            invite: None,
            unsubscribe: None,
        };
        vault
            .ingest_mail(account, vec![IngestMessage { message: message.clone(), mailbox, uid: 1 }])
            .unwrap();
        vault
            .save_body(&Body {
                message_id: id,
                html_sanitised: String::new(),
                text: subject.to_string(),
                quoted_ranges: Vec::new(),
                signature_range: None,
                parts: Vec::new(),
                remote_images: Vec::new(),
            })
            .unwrap();
    }

    /// A second handle on `vault`'s own index, holding its writer lock --
    /// standing in for another process that has the vault unlocked.
    pub(crate) fn hold_index(vault: &Vault) -> MailIndex {
        // After any open still running in the background, which would
        // otherwise be the one holding the lock.
        let _opening = OPENS.lock().unwrap_or_else(PoisonError::into_inner);
        open_index(vault).unwrap()
    }

    /// Unlocking does not wait for the search index: the state is installed
    /// at once, with the pack store open, and a search that arrives before
    /// the index has opened waits for it rather than failing.
    #[tokio::test]
    async fn the_index_opens_in_the_background_and_calls_wait_for_it() {
        let (_vdir, vault) = test_vault();
        let svc = Arc::new(Service::new());
        let vault = Arc::new(vault);

        // Holding the open lock keeps the background open from starting.
        let held = OPENS.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = open(&svc, &vault);
        assert!(svc.packs().is_some(), "the pack store opens on the spot");
        let index = svc.mail_index().expect("the index is installed before it opens");
        let searching = std::thread::spawn(move || {
            index.search(&MailQuery::parse("anything"), 10, None).map(|page| page.hits.len())
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!searching.is_finished(), "a search waits for the open");
        drop(held);
        assert_eq!(searching.join().unwrap().unwrap(), 0);
        assert!(!svc.mail_index().unwrap().rebuild_needed());
    }

    /// A lock that lands before the background open has finished, followed
    /// at once by another unlock, must not leave the second open fighting
    /// the first for tantivy's writer lock: that open would come up dead,
    /// and search with it, until the next unlock.
    #[tokio::test]
    async fn a_quick_lock_and_unlock_never_leaves_the_index_dead() {
        let (_vdir, vault) = test_vault();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        for _ in 0..5 {
            close(&svc);
            let _ = open(&svc, &vault);
        }
        assert!(!svc.mail_index().unwrap().rebuild_needed());

        // Opening again over an open state, with no lock between: the old
        // index goes before the new one opens.
        let _ = open(&svc, &vault);
        let _ = open(&svc, &vault);
        assert!(!svc.mail_index().unwrap().rebuild_needed());

        // And a lock before the open even started leaves nothing behind
        // holding the writer lock.
        let held = OPENS.lock().unwrap_or_else(PoisonError::into_inner);
        close(&svc);
        let _ = open(&svc, &vault);
        close(&svc);
        drop(held);
        assert!(!hold_index(&vault).rebuild_needed(), "the writer lock must be free");
    }

    /// A job that outlived the lock -- a batch of bodies still being indexed
    /// on the blocking pool when the account task was stopped -- holds the
    /// index, and tantivy's writer lock with it, a moment past `close`. An
    /// unlock in that moment waits for it to let go, rather than finding the
    /// writer lock taken and coming up dead for the whole session.
    #[tokio::test]
    async fn an_unlock_waits_for_an_index_a_lock_left_held() {
        let (_vdir, vault) = test_vault();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        let straggler = svc.mail_index().unwrap();
        assert!(!straggler.rebuild_needed(), "the test's own index opened");
        close(&svc);
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(straggler);
        });
        let _ = open(&svc, &vault);
        assert!(!svc.mail_index().unwrap().rebuild_needed(), "the index came up dead");
        letting_go.join().unwrap();
    }

    /// What was indexed and not yet committed when the vault locks -- by a
    /// pass the lock cut short -- is committed as the index goes rather than
    /// discarded with its writer: the mail is stored, so nothing would ever
    /// index it again.
    #[tokio::test]
    async fn what_was_indexed_but_not_committed_survives_a_lock() {
        let (_vdir, vault) = test_vault();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        let doc = MailDoc {
            message_key: MailMessageId::new().to_string(),
            thread_key: ThreadId::new().to_string(),
            account: AccountId::new().to_string(),
            mailboxes: Vec::new(),
            from: "ada@example.com".into(),
            to: String::new(),
            cc: String::new(),
            subject: "cut short by the lock".into(),
            body_text: String::new(),
            labels: Vec::new(),
            date: Timestamp::now(),
            has_attachment: false,
            unread: true,
            starred: false,
        };
        svc.mail_index().unwrap().index(&[doc]).unwrap();
        close(&svc);
        let _ = open(&svc, &vault);
        let page = svc.mail_index().unwrap().search(&MailQuery::parse("cut short"), 10, None);
        assert_eq!(page.unwrap().hits.len(), 1);
    }

    /// An open that never gets to run -- a runtime shutting down drops
    /// blocking work it has not started -- must not leave a caller waiting
    /// on the index for ever.
    #[test]
    fn an_open_that_never_runs_still_answers_whoever_waits() {
        let (_vdir, vault) = test_vault();
        let index = Arc::new(DeferredIndex::default());
        let vault = Arc::new(vault);
        let opening =
            Opening { slot: Arc::downgrade(&index), vault: Arc::downgrade(&vault), previous: None };
        drop(opening);
        assert!(index.rebuild_needed());
        assert!(index.search(&MailQuery::parse("anything"), 10, None).is_err());
    }

    /// End-to-end proof of the two halves this fix put together: a schema
    /// change makes `MailIndex::open` self-heal
    /// (`jatayu-mailindex::tests::reopening_with_a_different_schema_...`
    /// covers that half, in isolation, with a real mismatched schema), and
    /// *this* module is what has to notice the heal happened and drive
    /// `rebuild_account` for it, without anything calling
    /// `rebuild_mail_index` by hand.
    ///
    /// Simulates "the index cannot be opened as it stands" by corrupting
    /// `meta.json` in place rather than writing a real mismatched schema:
    /// `jatayu-service` does not depend on `tantivy` (only
    /// `jatayu-mailindex` does, deliberately -- see that crate's own
    /// module docs), and a schema mismatch and a corrupt meta file fail at
    /// the exact same place, `Index::open_or_create`, for the exact same
    /// reason -- see `jatayu-mailindex`'s own module docs, "self-healing
    /// on open". What this test is actually proving -- that `open` notices
    /// the heal and queues `rebuild_account` for every account -- does not
    /// depend on which of those two made the first open fail.
    #[tokio::test]
    async fn a_self_heal_on_open_triggers_one_automatic_reindex() {
        let (_vdir, vault) = test_vault();
        let mut account = Account::new(Provider::Custom, "me@example.com");
        // Left off deliberately: `Account::new` defaults `services.mail` to
        // `true`, which would have `register_account_tasks` below start a
        // real IMAP sync task for a `Provider::Custom` account with no real
        // server behind it. This test is about the reindex `open` queues
        // for *every* account regardless of whether mail sync is currently
        // switched on for it -- exactly what `rebuild_mail_index`'s own
        // `args.id: None` case already does -- not about the sync task.
        account.services.mail = false;
        let account_id = account.id;
        vault.save_account(&account).unwrap();
        let mailbox = Mailbox::new(account_id, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();
        seed_message(&vault, account_id, mailbox.id, "a message worth finding again");

        let svc = Arc::new(Service::new());
        // The first `set` opens mail against a brand new, empty,
        // *correctly-schema'd* index -- exactly like unlocking a vault that
        // has never synced mail before self-heal was ever a concern.
        let vault = svc.set(vault);

        // Populate the index the way a real sync would have, before the
        // "upgrade" below: `rebuild_account` is the same walk
        // `rebuild_mail_index` already drives by hand.
        {
            let index = svc.mail_index().unwrap();
            crate::domains::mailsync::rebuild_account(&vault, index.as_ref(), account_id).unwrap();
        }
        assert_eq!(
            svc.mail_index()
                .unwrap()
                .search(&MailQuery::parse("worth finding"), 10, None)
                .unwrap()
                .hits
                .len(),
            1,
            "the test's own setup must actually be searchable before simulating the upgrade"
        );

        // Release the writer lock `open_index` above is still holding --
        // `close`, the same call `Service::locked` makes -- before writing
        // straight over the same directory below.
        close(&svc);

        // The "upgrade": corrupt tantivy's own commit manifest in place, at
        // the exact path a real `open_index(&vault)` call will look again in
        // a moment. See this test's own docs for why this, and not a real
        // mismatched schema, is what an `jatayu-service` test reaches
        // for.
        std::fs::write(index_dir(&vault).join("meta.json"), b"not a readable sealed meta file")
            .unwrap();

        // The moment under test: a fresh `open`, exactly like the next
        // unlock after the upgrade. Nobody calls `rebuild_mail_index`.
        let queued = open(&svc, &vault);
        assert!(
            !svc.mail_index().unwrap().rebuild_needed(),
            "self-healing happens inside `MailIndex::open`, so once the background open has \
             finished (which `rebuild_needed` waits for) the index must already be reopened \
             -- just empty, not dead"
        );
        assert!(
            svc.mail_index()
                .unwrap()
                .search(&MailQuery::parse("worth finding"), 10, None)
                .unwrap()
                .hits
                .is_empty(),
            "the self-healed index is genuinely empty until the reindex below runs -- the \
             wipe erased the message the setup above had already indexed"
        );

        // `open` only *queues* the reindex (see `reindex_after_self_heal`'s
        // own "off the calling thread" docs) rather than running it inline
        // -- a real caller lets it run detached, but this test has to wait
        // for that exact task before it can assert anything about the
        // result, which is exactly what `open` returning its `JoinHandle`
        // is for. The walk starts only when that task is first polled,
        // which on this test's single-threaded runtime is the `await`
        // below, so the search above saw the index empty.
        queued.expect("a self-heal on open must queue exactly one reindex").await.unwrap();

        let index = svc.mail_index().unwrap();
        assert!(
            !index.rebuild_needed(),
            "a self-healed index must end up genuinely populated, not merely reopened"
        );
        let hits = index.search(&MailQuery::parse("worth finding"), 10, None).unwrap();
        assert_eq!(
            hits.hits.len(),
            1,
            "the message a real sync had already found before the schema changed must be \
             findable again without anyone calling rebuild_mail_index by hand"
        );
    }
}
