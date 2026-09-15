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
//!   [`everyday_core::packstore::PackStore`] trait through
//!   [`Vault::with_mail_packs`], one call at a time, rather than holding a
//!   connection this module does not own.
//! - **The search index.** A [`MailIndex`] in `mail-index/` beside the
//!   vault's own header, for a local backend. For Postgres there is no
//!   local vault directory worth writing an index into that would survive
//!   the vault moving to another machine -- the index is a derived
//!   structure that [`everyday_core::MailSearch::rebuild_needed`] already
//!   promises can always be rebuilt, so it lives in this device's own cache
//!   directory instead, keyed by a hash of the vault's path so two vaults on
//!   one machine (or the same vault reopened) do not collide or share.
//!
//! # Keys
//!
//! Neither store reuses the vault's own cipher the way
//! [`everyday_core::blobstore::FileBlobStore`] and media do -- see
//! [`everyday_core::crypto::Cipher::derive_subkey`] for why mail's storage,
//! being new, gets a key of its own by label rather than the vault's.
//! [`PACKSTORE_KEY_LABEL`] and [`SEARCHINDEX_KEY_LABEL`] are those labels,
//! and they are never reused for anything else.
//!
//! # The write claim
//!
//! Both stores open in *every* process that unlocks the vault -- a second
//! window, or a read-only `everyday-server`, still wants to search mail and
//! show an attachment. Only [`open`] additionally registers a sync task per
//! account, and only when [`Vault::is_writable`] is true: a process with no
//! write claim has nothing to sync with, since every write a sync task makes
//! -- a header ingested, a flag applied -- is a write to the vault.

use std::path::PathBuf;
use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::crypto::{AeadCipher, Cipher};
use everyday_core::error::Result;
use everyday_core::id::{AccountId, PackId};
use everyday_core::packstore::{CompactionResult, FilePackStore, PackRef, PackStore};
use everyday_mailindex::MailIndex;

use crate::mailsync::status::StatusRegistry;
use crate::service::Service;

/// Domain separator for the pack store's subkey -- see the module docs.
const PACKSTORE_KEY_LABEL: &str = "everyday.mail.packstore.v1";
/// Domain separator for the search index's subkey.
const SEARCHINDEX_KEY_LABEL: &str = "everyday.mail.searchindex.v1";

/// How much decrypted tantivy segment data [`MailIndex`] keeps warm in
/// memory at once. See that crate's `cache` module for why a sealed segment
/// is cached rather than re-decrypted on every read; 128 MiB is generous for
/// a laptop without being a meaningful fraction of one.
const INDEX_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// What [`Service::packs`], [`Service::mail_index`] and
/// [`Service::mail_statuses`] answer once a vault is unlocked.
pub struct MailState {
    pub(crate) packs: Arc<dyn PackStore>,
    pub(crate) index: Arc<dyn everyday_core::MailSearch>,
    pub(crate) statuses: StatusRegistry,
    pub(crate) unread_cache: Arc<crate::mailsync::unread_cache::UnreadCache>,
    pub(crate) contacts: Arc<crate::mailsync::contacts::ContactIndex>,
}

/// Open mail's storage against `vault`, and start its account tasks if this
/// process may write to it. See the module docs.
///
/// Never fails outright: a pack store or index that cannot be opened -- a
/// permissions problem, a corrupt directory -- is logged and leaves
/// [`Service::packs`]/[`Service::mail_index`] answering `None`, the same
/// "derived structure, not the only copy of anything" tolerance
/// [`everyday_core::MailSearch::rebuild_needed`] already asks callers to
/// have. Nothing about a vault's own records is at risk either way.
pub(crate) fn open(svc: &Arc<Service>, vault: &Arc<Vault>) {
    let packs = match open_packs(vault) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "could not open the mail pack store");
            svc.set_mail_state(None);
            return;
        }
    };
    let index = match open_index(vault) {
        Ok(i) => Arc::new(i) as Arc<dyn everyday_core::MailSearch>,
        Err(e) => {
            tracing::error!(error = %e, "could not open the mail search index");
            svc.set_mail_state(None);
            return;
        }
    };
    svc.set_mail_state(Some(MailState {
        packs,
        index,
        statuses: StatusRegistry::new(),
        unread_cache: Arc::new(crate::mailsync::unread_cache::UnreadCache::new()),
        // One small sealed row, read once here -- see `ContactIndex`'s own
        // docs on why this is not a scan of every message on unlock.
        contacts: Arc::new(crate::mailsync::contacts::ContactIndex::load(vault)),
    }));

    if vault.is_writable() {
        register_account_tasks(svc, vault);
    }
}

/// Drop what [`open`] opened. See `Service::locked`/`Service::close` for the
/// two moments this is called from -- always after the account tasks
/// writing through it have already stopped.
pub(crate) fn close(svc: &Service) {
    svc.set_mail_state(None);
}

fn open_packs(vault: &Arc<Vault>) -> Result<Arc<dyn PackStore>> {
    if vault.status().backend == "postgres" {
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

/// `<vault root>/mail-index` for a local backend -- beside the vault's own
/// header, per the plan. For Postgres, a per-device cache directory: the
/// index can always be rebuilt, so nothing about it belongs in a vault that
/// might be opened from a different machine tomorrow.
fn index_dir(vault: &Vault) -> PathBuf {
    if vault.status().backend == "postgres" {
        everyday_vault::config_dir().join("mail-index").join(cache_key(vault))
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
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    vault.path().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

    fn compact(&self, account: &str, referenced: &[PackId]) -> Result<CompactionResult> {
        self.0.with_mail_packs(|p| p.compact(account, referenced))
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
            async move { crate::mailsync::task::run_account(svc, vault, account_id, stop).await },
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
