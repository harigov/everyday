//! Phase 4's two read commands: `search_mail`, over the sealed tantivy
//! index `crate::mailsync::wiring` opens, and `suggest_addresses`, over the
//! in-memory contact index `crate::mailsync::contacts` keeps.
//!
//! Kept apart from `domains::mail` -- which owns `list_threads` and
//! `get_thread`, the two read commands phase 2 shipped with -- because both
//! commands here reach into session state (`Service::mail_index`,
//! `Service::mail_contacts`) that domain's own read commands never touch,
//! and because the query-syntax and cursor-encoding logic below is enough
//! of its own thing to read better in a file that is only about it.
//!
//! # The shape `search_mail` answers with, and why
//!
//! `docs/plans/mail.md`'s phase 4 section asks for thread summaries, "the
//! same type `list_threads` returns" -- which is
//! [`everyday_core::mail::Thread`] itself, not a second, search-specific
//! shape. `search_mail`'s `threads` field is therefore a plain
//! `Vec<Thread>`, exactly what `ThreadPage::threads` already is, so a
//! client's row-drawing code for one list works unchanged for the other.

use std::collections::HashSet;
use std::sync::Arc;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use everyday_core::id::{AccountId, ThreadId};
use everyday_core::mail::{Address, Thread};
use everyday_core::{MailQuery, SearchCursor};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMail {
    pub query: String,
    #[serde(default)]
    pub account_ids: Option<Vec<AccountId>>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_search_limit")]
    pub limit: u32,
}

/// Matches `list_threads`' own default page size (`domains::mail`'s
/// `default_page_size`) -- a search result list scrolls exactly like an
/// ordinary mailbox list, so it asks for the same amount of the first page.
fn default_search_limit() -> u32 {
    50
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMailResult {
    pub threads: Vec<Thread>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// `search_mail`'s own opaque cursor, one microsecond timestamp and a
/// message key joined by a colon -- [`everyday_core::mail::MailMessageId`]
/// (what a real `message_key` always is once ingest has minted one) never
/// contains one, so this cannot be confused with the key itself. Encoded
/// and decoded only here: [`SearchCursor`] itself has no `Serialize` of its
/// own, on the same "an implementation detail of the index, not the wire"
/// reasoning [`everyday_core::store::mail::ThreadPage`]'s own opaque cursor
/// string keeps.
fn encode_cursor(cursor: SearchCursor) -> String {
    format!("{}:{}", cursor.date.as_microsecond(), cursor.message_key)
}

fn decode_cursor(raw: &str) -> Option<SearchCursor> {
    let (us, key) = raw.split_once(':')?;
    let date = Timestamp::from_microsecond(us.parse().ok()?).ok()?;
    Some(SearchCursor { date, message_key: key.to_string() })
}

/// Parse `args.query` in the interface's own syntax, search the sealed
/// index, and load the thread each distinct result belongs to -- first hit
/// per thread, in the order the index already ranked them, so two hits in
/// the same conversation never draw two rows for it. Every thread this
/// caller may read, or only `args.account_ids` when given.
async fn search_mail(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SearchMail,
) -> CommandResult<SearchMailResult> {
    let vault = svc.require()?;
    let index = svc
        .mail_index()
        .ok_or_else(|| CommandError::new(codes::NOT_FOUND, "the mail search index is not open"))?;
    let mut query = MailQuery::parse(&args.query);
    if let Some(ids) = &args.account_ids {
        query.accounts = ids.iter().map(ToString::to_string).collect();
    }
    let cursor = args.cursor.as_deref().and_then(decode_cursor);
    let limit = args.limit.max(1) as usize;

    blocking(move || {
        let page = index.search(&query, limit, cursor)?;
        let mut seen = HashSet::new();
        let mut threads = Vec::with_capacity(page.hits.len());
        for hit in &page.hits {
            if !seen.insert(hit.thread_key.clone()) {
                continue; // a later hit in an already-seen thread
            }
            let Ok(thread_id) = hit.thread_key.parse::<ThreadId>() else { continue };
            if let Ok((thread, _)) = vault.thread(thread_id) {
                threads.push(thread);
            }
        }
        Ok(SearchMailResult { threads, next: page.next.map(encode_cursor) })
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestAddresses {
    pub prefix: String,
    #[serde(default = "default_suggest_limit")]
    pub limit: u32,
}

fn default_suggest_limit() -> u32 {
    10
}

/// Fuzzy-match `args.prefix` against the in-memory contact index -- see
/// `crate::mailsync::contacts`'s module docs for how it is built and kept
/// current. Synchronous and in-memory: unlike `search_mail`, there is no
/// index to touch on the blocking pool.
async fn suggest_addresses(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SuggestAddresses,
) -> CommandResult<Vec<Address>> {
    let _ = svc.require()?;
    let contacts = svc
        .mail_contacts()
        .ok_or_else(|| CommandError::new(codes::NOT_FOUND, "the contact index is not open"))?;
    Ok(contacts.suggest(&args.prefix, args.limit.max(1) as usize))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "search_mail", scope: Mail, effect: Read,
        args: SearchMail, returns: "SearchMailResult",
        signature: &[
            ("query", "string", true),
            ("accountIds", "AccountId[] | null", false),
            ("cursor", "string | null", false),
            ("limit", "number | null", false),
        ],
        run: search_mail,
    },
    command! {
        name: "suggest_addresses", scope: Mail, effect: Read,
        args: SuggestAddresses, returns: "MailAddress[]",
        signature: &[("prefix", "string", true), ("limit", "number | null", false)],
        run: suggest_addresses,
    },
];
