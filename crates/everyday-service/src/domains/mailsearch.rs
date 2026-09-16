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
use everyday_core::mail::{Address, Mailbox, Thread};
use everyday_core::{Clause, MailQuery, QueryGroup, QueryOp, SearchCursor, Vault};
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

/// Resolve every `in:<name>` clause [`MailQuery::parse`] produced to the
/// mailbox id(s) actually indexed under it.
///
/// `everyday-mailindex`'s query translation matches `Op::In` against the
/// indexed `mailboxes` field as a raw term, and what gets indexed there
/// (`crate::mailsync::passes::mail_doc`, in the sync passes) is a bare
/// mailbox UUID -- never the folder name or role a person types. Left
/// unresolved, `in:inbox` builds a query for the literal term `"inbox"`,
/// which nothing was ever indexed as, and silently matches nothing.
///
/// Resolution is scoped to `query.accounts` -- the same accounts the search
/// itself is restricted to, or every account `vault` has when
/// unrestricted, matching `search_mail`'s own "every thread this caller may
/// read" default -- and matches a name against either a mailbox's
/// [`everyday_core::mail::MailboxRole`] (`"inbox"`, `"archive"`, ...) or its
/// literal `remote_name`, lowercased, so both `in:inbox` and a custom
/// folder or Gmail label's own name work.
///
/// A name matching more than one mailbox -- the same role, or the same
/// folder name, across several accounts -- means a positive `in:` clause
/// fans its whole group out into one OR'd copy per match, so `in:inbox`
/// reaches every account's own inbox rather than only the first one this
/// happened to find. A negated `-in:` clause instead grows in place, within
/// the same group, into one negated clause per match: "not in any of them"
/// is already what several `MustNot` clauses ANDed together mean, so there
/// is nothing to duplicate. A name matching nothing at all is left as a
/// literal string nothing was ever indexed under -- exactly today's
/// behaviour for a name nobody recognises, just no longer wrong for one
/// that does.
///
/// A message is only ever indexed under the *one* mailbox
/// `mail_doc` was called with when it was last (re-)indexed, never every
/// mailbox it is actually filed under -- see that function's own docs. On
/// Gmail, where one message commonly carries several labels, this means
/// `in:<label>` can still miss a message truly filed there if the copy the
/// index kept happened to be indexed under a different one of its labels.
/// Closing that needs the indexer itself to merge rather than replace on
/// re-index, which is `crate::mailsync::passes`' own file, not this one's.
fn resolve_mailbox_names(vault: &Vault, query: MailQuery) -> MailQuery {
    let accounts: Vec<AccountId> = if query.accounts.is_empty() {
        vault.accounts().map(|a| a.into_iter().map(|acc| acc.id).collect()).unwrap_or_default()
    } else {
        query.accounts.iter().filter_map(|s| s.parse().ok()).collect()
    };
    let mailboxes: Vec<Mailbox> =
        accounts.iter().flat_map(|&id| vault.mailboxes(id).unwrap_or_default()).collect();

    let resolve = |name: &str| -> Vec<String> {
        let matches: Vec<String> = mailboxes
            .iter()
            .filter(|mb| mb.role.as_str() == name || mb.remote_name.to_lowercase() == name)
            .map(|mb| mb.id.to_string())
            .collect();
        if matches.is_empty() { vec![name.to_string()] } else { matches }
    };

    let mut groups = Vec::with_capacity(query.any_of.len());
    for group in query.any_of {
        // Every clause in this group, fanned out into however many copies
        // a positive `in:` match with more than one mailbox needs -- one
        // list of clauses per eventual OR'd group; starts as a single
        // empty one and only ever grows past that when such a clause is
        // actually seen.
        let mut expansions: Vec<Vec<Clause>> = vec![Vec::new()];
        for clause in group.clauses {
            match &clause.op {
                QueryOp::In(name) => {
                    let ids = resolve(name);
                    if clause.negate {
                        for expansion in &mut expansions {
                            for id in &ids {
                                expansion
                                    .push(Clause { op: QueryOp::In(id.clone()), negate: true });
                            }
                        }
                    } else if ids.len() <= 1 {
                        let id = ids.into_iter().next().unwrap_or_else(|| name.clone());
                        for expansion in &mut expansions {
                            expansion.push(Clause { op: QueryOp::In(id.clone()), negate: false });
                        }
                    } else {
                        expansions = ids
                            .iter()
                            .flat_map(|id| {
                                expansions.iter().map(move |base| {
                                    let mut next = base.clone();
                                    next.push(Clause {
                                        op: QueryOp::In(id.clone()),
                                        negate: false,
                                    });
                                    next
                                })
                            })
                            .collect();
                    }
                }
                _ => {
                    for expansion in &mut expansions {
                        expansion.push(clause.clone());
                    }
                }
            }
        }
        groups.extend(expansions.into_iter().map(|clauses| QueryGroup { clauses }));
    }
    MailQuery { any_of: groups, accounts: query.accounts }
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
    let query = resolve_mailbox_names(&vault, query);
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
