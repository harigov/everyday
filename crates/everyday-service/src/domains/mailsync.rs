//! Mail sync: forcing an account's next pass, reading every account's
//! progress, and rebuilding the search index from what is already stored.
//!
//! Everything else about how mail arrives -- the account task itself, the
//! three-pass first sync, the steady state -- lives in
//! `crate::mailsync`, out of the command surface entirely: nothing here
//! syncs anything directly, these three commands only reach into what
//! `crate::mailsync::wiring` already keeps running.

use std::sync::Arc;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::mailsync::status::Progress;
use crate::service::{Service, blocking};
use everyday_core::id::{AccountId, MailboxId, ThreadId};
use everyday_core::mail::Category;
use everyday_core::store::mail::ThreadFilter;
use everyday_core::{MailDoc, MailSearch, Vault};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRef {
    pub id: AccountId,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RebuildMailIndex {
    /// One account, or every account when absent.
    #[serde(default)]
    pub id: Option<AccountId>,
}

/// Force an immediate pass: start the account's task if it is not running
/// (which already means an immediate pass, since every task syncs before it
/// ever waits on `IDLE`), and wake it if it is already sitting in `IDLE` or
/// its poll sleep.
async fn sync_account(svc: Arc<Service>, _ctx: Ctx, args: AccountRef) -> CommandResult<()> {
    let vault = svc.require()?;
    crate::mailsync::wiring::ensure_account_task(&svc, &vault, args.id);
    if let Some(statuses) = svc.mail_statuses() {
        statuses.nudge(args.id);
    }
    Ok(())
}

/// Every account's sync progress this session knows about.
async fn sync_status(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<Vec<Progress>> {
    Ok(svc.mail_statuses().map(|s| s.all()).unwrap_or_default())
}

/// Reindex one account, or every account, from what the vault already has
/// stored -- the recovery path
/// [`everyday_core::MailSearch::rebuild_needed`] exists to be answered with:
/// the index is a derived structure, never the only copy of anything, so
/// losing it costs a walk over storage rather than data.
async fn rebuild_mail_index(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RebuildMailIndex,
) -> CommandResult<()> {
    let vault = svc.require()?;
    let Some(index) = svc.mail_index() else {
        return Err(CommandError::new(codes::UNSUPPORTED, "the mail search index is not open"));
    };
    blocking(move || {
        // A dead index cannot be healed by writing into it: `index` and
        // `commit` would fail exactly the way `rebuild_needed` already
        // reported, because there is nothing on disk for them to write
        // into. Recreate it first whenever that is the case -- but only
        // then: a healthy index is left alone, since
        // `MailSearch::rebuild_empty` erases *every* account's documents,
        // not only the ones this call was asked to rebuild (see that
        // method's own docs).
        if index.rebuild_needed() {
            index.rebuild_empty()?;
        }
        let account_ids: Vec<AccountId> = match args.id {
            Some(id) => vec![id],
            None => vault.accounts()?.into_iter().map(|a| a.id).collect(),
        };
        for account_id in account_ids {
            rebuild_account(&vault, index.as_ref(), account_id)?;
        }
        Ok(())
    })
    .await
}

/// A batch this size is indexed and committed at a time -- large enough that
/// a hundred-thousand-message rebuild is not one commit per message, small
/// enough that a batch's own memory is unremarkable.
const REBUILD_BATCH: usize = 500;

/// Reindex `account_id` from what the vault already has stored.
///
/// # Two passes, because one walk cannot see everything
///
/// `list_threads` pages over `thread_mailboxes`, but an optimistic archive
/// (`hidden_thread_mailboxes`, see `docs/plans/mail.md`'s "Delivered" note)
/// deletes exactly that row without touching the thread's own, so a thread
/// archived locally and not yet confirmed by the server sits in no
/// mailbox's list at all -- pass 1 below, walking every mailbox, never
/// visits it. `threads_in_category` reads the `threads` table directly by
/// account and category instead, which the hide never touches, so pass 2
/// reaches those threads too. `MailStore` has no single account-wide thread
/// walk this crate can call instead — see this fix's own report for what
/// that would need — so between them these two passes are the closest
/// approximation available without adding one: a thread hidden before it
/// was ever categorized (`threads.category` starts `NULL` until a separate
/// categorize pass sets it) is the one gap that remains, and it is narrow
/// and temporary rather than the permanent, total one pass 1 alone had.
///
/// `seen` deduplicates between the two passes -- a thread visible in some
/// mailbox's list is never re-walked just because it also has a category.
fn rebuild_account(
    vault: &Vault,
    index: &dyn MailSearch,
    account_id: AccountId,
) -> everyday_core::Result<()> {
    // Clear this account's existing documents first: a message deleted (or
    // a thread that no longer exists) while the index was broken must not
    // stay findable once this rebuild "fixes" it back to a working state --
    // nothing below ever tells the index to forget a message on its own,
    // only to add or replace one, so without this a stale hit would sit in
    // results until the vault's own post-search re-check silently drops it
    // every single time.
    index.delete_account(&account_id.to_string())?;

    let mut batch = Vec::with_capacity(REBUILD_BATCH);
    let mut seen: std::collections::HashSet<ThreadId> = std::collections::HashSet::new();

    // Pass 1: every thread still visible in some mailbox's own list -- the
    // ordinary, fast path, one page per mailbox, and every message in a
    // visited thread is attributed to the mailbox its own list page came
    // from (matching what a live sync would have indexed it under).
    for mailbox in vault.mailboxes(account_id)? {
        let mut cursor: Option<String> = None;
        loop {
            let page =
                vault.list_threads(mailbox.id, &ThreadFilter::default(), cursor.as_deref(), 200)?;
            for summary in &page.threads {
                if seen.insert(summary.id) {
                    index_thread(
                        vault,
                        index,
                        account_id,
                        summary.id,
                        Some(mailbox.id),
                        &mut batch,
                    )?;
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }

    // Pass 2: every thread pass 1 could not see -- see this function's own
    // docs for why `threads_in_category` reaches them. There is no single
    // mailbox this walk arrived through, so each message's own mailbox is
    // looked up instead.
    for category in Category::ALL {
        let mut cursor: Option<String> = None;
        loop {
            let page = vault.threads_in_category(account_id, category, cursor.as_deref(), 200)?;
            for summary in &page.threads {
                if seen.insert(summary.id) {
                    index_thread(vault, index, account_id, summary.id, None, &mut batch)?;
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }

    if !batch.is_empty() {
        index.index(&batch)?;
    }
    index.commit()
}

/// Index every message of `thread_id`, appending a [`MailDoc`] for each to
/// `batch` and flushing it once [`REBUILD_BATCH`] is reached.
///
/// `mailbox_hint` is the mailbox every message in this thread should be
/// attributed to when the caller already knows it -- pass 1 of
/// [`rebuild_account`], cheap, and matching what a live sync indexed it
/// under. `None` asks this function to look each message's own mailbox up
/// instead (pass 2, where there is no single mailbox this walk arrived
/// through); a message with no mailbox membership left at all is skipped,
/// since there is nothing left for a search result to open back up to
/// anyway.
fn index_thread(
    vault: &Vault,
    index: &dyn MailSearch,
    account_id: AccountId,
    thread_id: ThreadId,
    mailbox_hint: Option<MailboxId>,
    batch: &mut Vec<MailDoc>,
) -> everyday_core::Result<()> {
    let (_thread, messages) = vault.thread(thread_id)?;
    for message in &messages {
        let mailbox_id = match mailbox_hint {
            Some(id) => id,
            None => match vault.mail_message_locations(message.id)?.first() {
                Some((id, _)) => *id,
                None => continue,
            },
        };
        if let Ok(body) = vault.body(message.id) {
            let doc = crate::mailsync::passes::mail_doc(
                account_id,
                mailbox_id,
                message,
                &body.model_text(),
            );
            batch.push(doc);
        }
        if batch.len() >= REBUILD_BATCH {
            index.index(batch)?;
            batch.clear();
        }
    }
    Ok(())
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "sync_account", scope: Mail, effect: Write,
        args: AccountRef, returns: "void",
        signature: &[("id", "AccountId", true)],
        run: sync_account,
    },
    command! {
        name: "sync_status", scope: Mail, effect: Read,
        args: super::Nothing, returns: "MailSyncProgress[]", signature: &[],
        run: sync_status,
    },
    command! {
        name: "rebuild_mail_index", scope: Mail, effect: Write,
        args: RebuildMailIndex, returns: "void",
        signature: &[("id", "AccountId", false)],
        run: rebuild_mail_index,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::MailQuery;
    use everyday_core::crypto::{AeadCipher, SecretKey};
    use everyday_core::id::{MailMessageId, PackId, ThreadId};
    use everyday_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags, OpKind, Origin,
    };
    use everyday_core::packstore::PackRef;
    use everyday_core::store::mail::IngestMessage;
    use everyday_mailindex::MailIndex;
    use jiff::Timestamp;

    fn test_vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = everyday_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = everyday_vault::create(dir.path(), cfg).unwrap();
        (dir, vault)
    }

    fn test_index() -> (tempfile::TempDir, MailIndex) {
        let dir = tempfile::tempdir().unwrap();
        let cipher = Arc::new(AeadCipher::new(&SecretKey::from_bytes([7u8; 32])));
        let index = MailIndex::open(dir.path(), cipher, 16 * 1024 * 1024).unwrap();
        (dir, index)
    }

    fn fresh_message(account: AccountId, thread_id: ThreadId, subject: &str) -> Message {
        let id = MailMessageId::new();
        Message {
            id,
            account_id: account,
            thread_id,
            message_id_header: format!("<{id}@mailsync-test.example>"),
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
        }
    }

    /// Ingests one message into `mailbox` with a real body, so a rebuild has
    /// something to find, and returns its thread and message ids.
    fn seed(
        vault: &Vault,
        account: AccountId,
        mailbox: MailboxId,
        subject: &str,
    ) -> (ThreadId, MailMessageId) {
        let thread_id = ThreadId::new();
        let message = fresh_message(account, thread_id, subject);
        let message_id = message.id;
        vault
            .ingest_mail(account, vec![IngestMessage { message: message.clone(), mailbox, uid: 1 }])
            .unwrap();
        let body = Body {
            message_id,
            html_sanitised: String::new(),
            text: subject.to_string(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        vault.save_body(&body).unwrap();
        (thread_id, message_id)
    }

    /// Regression for bug 5's enumeration half: `list_threads` pages over
    /// `thread_mailboxes`, which an optimistic archive deletes without
    /// touching the thread's own row (see `hidden_thread_mailboxes` in
    /// `docs/plans/mail.md`'s "Delivered" note) -- before pass 2 existed, a
    /// thread archived locally and not yet confirmed by the server was in
    /// no mailbox at all and a rebuild simply never visited it.
    #[test]
    fn rebuild_account_reaches_an_optimistically_archived_thread() {
        let (_vdir, vault) = test_vault();
        let (_idir, index) = test_index();
        let account = AccountId::new();
        let inbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&inbox).unwrap();

        let (thread_id, message_id) = seed(&vault, account, inbox.id, "archived treasure");
        // A category has to land on the thread for `threads_in_category` --
        // pass 2's own reach -- to ever see it; a real account gets this
        // from the (separately owned) categorize pass shortly after ingest.
        vault.set_mail_message_category(message_id, Category::Important).unwrap();

        // Archive it: hides the thread from the inbox's own
        // `thread_mailboxes` row without touching `message_mailboxes` --
        // exactly the shape that makes `list_threads` blind to it.
        vault.apply_thread_ops(&[thread_id], OpKind::Archive, Origin::Person).unwrap();
        let visible = vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
        assert!(
            visible.threads.is_empty(),
            "the archive must actually hide the thread from the inbox's own list, or this \
             test is not exercising the gap it means to prove is closed"
        );

        rebuild_account(&vault, &index, account).unwrap();

        let hits = index.search(&MailQuery::parse("archived treasure"), 10, None).unwrap();
        assert_eq!(
            hits.hits.len(),
            1,
            "a thread hidden by an optimistic archive must still be reachable by a rebuild"
        );
    }

    /// Regression for bug 5's other half: a rebuild used to write straight
    /// into whatever the index already had for this account, so a message
    /// deleted (or a thread emptied) while the index was broken stayed
    /// findable through the "fixed" index forever.
    #[test]
    fn rebuild_account_clears_stale_documents_first() {
        let (_vdir, vault) = test_vault();
        let (_idir, index) = test_index();
        let account = AccountId::new();

        // A document for a message this vault has never heard of -- standing
        // in for "deleted while the index was broken".
        let stale = everyday_core::MailDoc {
            message_key: "stale-message".to_string(),
            thread_key: "stale-thread".to_string(),
            account: account.to_string(),
            mailboxes: vec!["inbox".to_string()],
            from: "ghost@example.com".to_string(),
            to: String::new(),
            cc: String::new(),
            subject: "ghost of mailboxes past".to_string(),
            body_text: String::new(),
            labels: Vec::new(),
            date: Timestamp::now(),
            has_attachment: false,
            unread: false,
            starred: false,
        };
        index.index(&[stale]).unwrap();
        index.commit().unwrap();
        assert_eq!(
            index.search(&MailQuery::parse("ghost of mailboxes"), 10, None).unwrap().hits.len(),
            1,
            "the test's own setup must actually be findable before the rebuild runs"
        );

        rebuild_account(&vault, &index, account).unwrap();

        let hits = index.search(&MailQuery::parse("ghost of mailboxes"), 10, None).unwrap();
        assert!(hits.hits.is_empty(), "a rebuild must clear an account's stale documents first");
    }
}
