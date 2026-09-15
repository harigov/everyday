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
use everyday_core::id::AccountId;
use everyday_core::store::mail::ThreadFilter;
use everyday_core::{MailSearch, Vault};
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

fn rebuild_account(
    vault: &Vault,
    index: &dyn MailSearch,
    account_id: AccountId,
) -> everyday_core::Result<()> {
    let mut batch = Vec::with_capacity(REBUILD_BATCH);
    for mailbox in vault.mailboxes(account_id)? {
        let mut cursor: Option<String> = None;
        loop {
            let page =
                vault.list_threads(mailbox.id, &ThreadFilter::default(), cursor.as_deref(), 200)?;
            for summary in &page.threads {
                let (_thread, messages) = vault.thread(summary.id)?;
                for message in &messages {
                    if let Ok(body) = vault.body(message.id) {
                        let doc = crate::mailsync::passes::mail_doc(
                            message.account_id,
                            mailbox.id,
                            message,
                            &body.model_text(),
                        );
                        batch.push(doc);
                    }
                    if batch.len() >= REBUILD_BATCH {
                        index.index(&batch)?;
                        batch.clear();
                    }
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
