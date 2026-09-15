//! Mail: mailboxes, threads and the messages in them.
//!
//! Read-only, for now, and deliberately so. This is phase 2 of
//! `docs/plans/mail.md`'s storage half landing in the service layer ahead of
//! the sync engine, the outbox drain loop and the interface that will
//! actually write to any of this -- three commands, matching exactly what a
//! thread list and a thread view need to draw, and nothing that mutates a
//! row. `list_threads` and `get_thread` are also the two calls phase 5's
//! `list_threads` and `read_thread` tools will end up wrapping, so their
//! shape is worth getting right now rather than guessed at twice.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::id::{AccountId, MailboxId, ThreadId};
use everyday_core::mail::{Mailbox, Message, Thread};
use everyday_core::store::mail::{ThreadFilter, ThreadPage};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxesQuery {
    pub account: AccountId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListThreads {
    pub mailbox: MailboxId,
    #[serde(default)]
    pub filter: ThreadFilter,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_page_size")]
    pub limit: u32,
}

/// What a thread list asks for when a caller does not say -- large enough
/// that a virtual list's first paint has plenty to scroll, small enough that
/// decrypting one page is not itself the delay it exists to avoid.
fn default_page_size() -> u32 {
    50
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    pub id: ThreadId,
}

/// A thread and every message in it -- what opening one reads. Bodies are
/// not included; the interface asks for each with `get_body` as it draws
/// them, once that command exists.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread: Thread,
    pub messages: Vec<Message>,
}

async fn list_mailboxes(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MailboxesQuery,
) -> CommandResult<Vec<Mailbox>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.mailboxes(args.account)?)).await
}

async fn list_threads(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ListThreads,
) -> CommandResult<ThreadPage> {
    let vault = svc.require()?;
    blocking(move || {
        Ok(vault.list_threads(args.mailbox, &args.filter, args.cursor.as_deref(), args.limit)?)
    })
    .await
}

async fn get_thread(svc: Arc<Service>, _ctx: Ctx, args: ThreadRef) -> CommandResult<ThreadDetail> {
    let vault = svc.require()?;
    blocking(move || {
        let (thread, messages) = vault.thread(args.id)?;
        Ok(ThreadDetail { thread, messages })
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_mailboxes", scope: Mail, effect: Read,
        args: MailboxesQuery, returns: "Mailbox[]",
        signature: &[("account", "AccountId", true)],
        run: list_mailboxes,
    },
    command! {
        name: "list_threads", scope: Mail, effect: Read,
        args: ListThreads, returns: "ThreadPage",
        signature: &[
            ("mailbox", "MailboxId", true),
            ("filter", "ThreadFilter", false),
            ("cursor", "string | null", false),
            ("limit", "number | null", false),
        ],
        run: list_threads,
    },
    command! {
        name: "get_thread", scope: Mail, effect: Read,
        args: ThreadRef, returns: "ThreadDetail",
        signature: &[("id", "ThreadId", true)],
        run: get_thread,
    },
];
