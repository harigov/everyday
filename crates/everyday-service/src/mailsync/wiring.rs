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
use everyday_core::packstore::{
    CompactionResult, FilePackStore, PackRef, PackStore, ReferencedSnapshot,
};
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
///
/// Returns the handle of the one-time reindex this call queues when
/// [`everyday_mailindex::MailIndex::healed_on_open`] comes back `true` --
/// see [`reindex_after_self_heal`]'s own docs. Every real caller
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
    let index = match open_index(vault) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, "could not open the mail search index");
            svc.set_mail_state(None);
            return None;
        }
    };
    // Read before `index` moves into the `Arc<dyn MailSearch>` below --
    // `everyday_mailindex::MailIndex::healed_on_open`'s own docs promise
    // this is the one moment it is worth checking. See
    // `reindex_after_self_heal`'s own docs for what a `true` here means and
    // why it is answered only once, right here, rather than on every sync
    // pass.
    let healed_on_open = index.healed_on_open();
    let index: Arc<dyn everyday_core::MailSearch> = Arc::new(index);
    svc.set_mail_state(Some(MailState {
        packs,
        index: index.clone(),
        statuses: StatusRegistry::new(),
        unread_cache: Arc::new(crate::mailsync::unread_cache::UnreadCache::new()),
        // One small sealed row, read once here -- see `ContactIndex`'s own
        // docs on why this is not a scan of every message on unlock.
        contacts: Arc::new(crate::mailsync::contacts::ContactIndex::load(vault)),
    }));

    if vault.is_writable() {
        register_account_tasks(svc, vault);
        // Only when writable: reindexing writes nothing to the vault
        // itself, only to `index`, but it is real disk-bound work (a full
        // walk of every account's threads and bodies), and gating it the
        // same way `register_account_tasks` already is keeps a second,
        // read-only window (or `everyday-server`) from redoing the exact
        // same walk the writable process is already about to run. See
        // `reindex_after_self_heal`'s own docs for the rest of the
        // reasoning, including the one race this does not close.
        if healed_on_open {
            return Some(reindex_after_self_heal(vault, index));
        }
    }
    None
}

/// Walk every account's threads and bodies back into a freshly self-healed
/// `index`, once, off the calling thread.
///
/// # Why this exists at all
///
/// `everyday_mailindex::MailIndex::open` wiping and recreating the sealed
/// index on a schema mismatch (see that crate's own module docs,
/// "self-healing on open") fixes the *dead* half of the outage a persisted
/// tokenizer change caused, but it also leaves `index` genuinely, silently
/// empty. Nothing else in this crate ever re-populates it on its own: the
/// body pass (`crate::mailsync::passes`) only calls `MailSearch::index` for
/// messages it *newly* fetches this run, never for ones already sitting in
/// storage from a previous sync. Without this function, a self-heal would
/// trade "search answers with an error" for "search answers with nothing",
/// which is not the fix `docs/plans/mail.md`'s search phase promises.
/// `rebuild_account` (`crate::domains::mailsync`) already knows how to walk
/// a vault's own storage back into an index -- this only decides *when* to
/// call it automatically, the one moment `open` already knows the index
/// just went from `rebuild_needed() == true` to `false` by itself.
///
/// # Once, not per pass
///
/// Called exactly once from [`open`], itself called exactly once per
/// process per vault unlock -- never from inside a sync pass, and never on
/// a timer -- because `everyday_mailindex::MailIndex::healed_on_open` is
/// itself answered once, at construction, from the one `MailIndex` this
/// call was handed. A later pass finding `rebuild_needed() == false` (which
/// this reindex itself brings about) has no reason to call this again, and
/// nothing in this module ever checks `healed_on_open` a second time for
/// the same `MailIndex` to let it.
///
/// # Off the calling thread
///
/// [`open`] runs synchronously wherever `Service::unlocked` does -- today,
/// directly on the async task the `unlock` command is already running on
/// (see `domains::vault::unlock`), with no `blocking` wrapper around that
/// call. A hundred-thousand-message vault's worth of `rebuild_account`
/// walks is exactly the disk-bound work `service::blocking`'s own docs
/// already warn would "stall every other task" run inline, so this hands
/// the whole walk to `tokio::spawn` -- already this crate's pattern for
/// fire-and-forget background work (see `mailview.rs`, `signin.rs`) -- and
/// the walk itself still goes through `blocking` underneath, exactly like
/// `rebuild_mail_index`'s own command handler.
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
///
/// Returns the spawned task's handle so a test can await the exact work a
/// real caller lets run detached -- see [`open`]'s own call site.
fn reindex_after_self_heal(
    vault: &Arc<Vault>,
    index: Arc<dyn everyday_core::MailSearch>,
) -> tokio::task::JoinHandle<()> {
    let vault = vault.clone();
    tokio::spawn(async move {
        let result = crate::service::blocking(move || {
            let account_ids: Vec<AccountId> = vault.accounts()?.into_iter().map(|a| a.id).collect();
            for account_id in account_ids {
                crate::domains::mailsync::rebuild_account(&vault, index.as_ref(), account_id)?;
            }
            Ok(())
        })
        .await;
        if let Err(e) = result {
            tracing::error!(
                error = %e,
                "could not reindex mail after the search index self-healed"
            );
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::MailQuery;
    use everyday_core::account::{Account, Provider};
    use everyday_core::id::{MailMessageId, MailboxId, PackId, ThreadId};
    use everyday_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
    };
    use everyday_core::packstore::PackRef;
    use everyday_core::store::mail::IngestMessage;
    use jiff::Timestamp;

    fn test_vault() -> (tempfile::TempDir, everyday_core::Vault) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = everyday_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = everyday_vault::create(dir.path(), cfg).unwrap();
        (dir, vault)
    }

    /// Ingests one findable message directly into the vault's own storage,
    /// bypassing the sync engine entirely -- this test is about what
    /// happens to a *previously synced* mailbox, not about syncing one.
    fn seed_message(vault: &Vault, account: AccountId, mailbox: MailboxId, subject: &str) {
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

    /// End-to-end proof of the two halves this fix put together: a schema
    /// change makes `MailIndex::open` self-heal
    /// (`everyday-mailindex::tests::reopening_with_a_different_schema_...`
    /// covers that half, in isolation, with a real mismatched schema), and
    /// *this* module is what has to notice the heal happened and drive
    /// `rebuild_account` for it, without anything calling
    /// `rebuild_mail_index` by hand.
    ///
    /// Simulates "the index cannot be opened as it stands" by corrupting
    /// `meta.json` in place rather than writing a real mismatched schema:
    /// `everyday-service` does not depend on `tantivy` (only
    /// `everyday-mailindex` does, deliberately -- see that crate's own
    /// module docs), and a schema mismatch and a corrupt meta file fail at
    /// the exact same place, `Index::open_or_create`, for the exact same
    /// reason -- see `everyday-mailindex`'s own module docs, "self-healing
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
        // mismatched schema, is what an `everyday-service` test reaches
        // for.
        std::fs::write(index_dir(&vault).join("meta.json"), b"not a readable sealed meta file")
            .unwrap();

        // The moment under test: a fresh `open`, exactly like the next
        // unlock after the upgrade. Nobody calls `rebuild_mail_index`.
        let queued = open(&svc, &vault);
        assert!(
            !svc.mail_index().unwrap().rebuild_needed(),
            "self-healing happens synchronously inside `MailIndex::open`, so by the time \
             `open` returns the index must already be reopened -- just empty, not dead"
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
        // is for.
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
