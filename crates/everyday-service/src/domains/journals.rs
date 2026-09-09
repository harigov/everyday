//! Journals, entries, search and tags.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::model::{local_date_in, system_tz};
use everyday_core::search::{SearchHit, SearchScope};
use everyday_core::store::EntryQuery;
use everyday_core::{Entry, EntryId, EntrySummary, Journal, JournalId};
use serde::Deserialize;
use std::sync::Arc;

use super::vault::Nothing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Named {
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveJournal {
    pub journal: Journal,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalRef {
    pub id: JournalId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entries {
    pub query: EntryQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRef {
    pub id: EntryId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEntry {
    pub journal_id: JournalId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEntry {
    pub entry: Entry,
    /// The `updatedAt` the caller last read for this entry, or `null` for one
    /// it has just created.
    #[serde(default)]
    pub expect: Option<jiff::Timestamp>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForceEntry {
    pub entry: Entry,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Search {
    pub query: String,
    /// Narrow to one journal. Naming one also narrows to *entries*, since a
    /// note is in no journal.
    #[serde(default)]
    pub journal_id: Option<JournalId>,
    /// Narrow to one kind of record. Absent means both, which is what the
    /// palette wants: a half-remembered phrase should be found wherever it
    /// was written down.
    #[serde(default)]
    pub kind: Option<String>,
    pub limit: usize,
}

async fn list_journals(svc: Arc<Service>, _ctx: Ctx, _a: Nothing) -> CommandResult<Vec<Journal>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.journals()?)).await
}

/// Mint a journal, without saving it.
///
/// The id is the core's to allocate, not a client's. Ids here are UUIDv7,
/// which the storage layer relies on to sort chronologically; the interface
/// had been minting v4 with `crypto.randomUUID`, which is both a different
/// ordering and unavailable outside a secure context.
async fn new_journal(svc: Arc<Service>, _ctx: Ctx, args: Named) -> CommandResult<Journal> {
    let _ = svc.require()?;
    Ok(Journal::new(args.name))
}

async fn save_journal(svc: Arc<Service>, _ctx: Ctx, args: SaveJournal) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_journal(&args.journal)?)).await
}

async fn delete_journal(svc: Arc<Service>, _ctx: Ctx, args: JournalRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_journal(args.id)?)).await
}

async fn list_entries(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Entries,
) -> CommandResult<Vec<EntrySummary>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.entries(&args.query)?)).await
}

async fn get_entry(svc: Arc<Service>, _ctx: Ctx, args: EntryRef) -> CommandResult<Entry> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.entry(args.id)?)).await
}

/// A blank entry, filed under today in the machine's own time zone.
async fn new_entry(svc: Arc<Service>, _ctx: Ctx, args: NewEntry) -> CommandResult<Entry> {
    let _ = svc.require()?;
    let tz = system_tz();
    let mut entry = Entry::new(args.journal_id, &tz);
    entry.local_date = local_date_in(entry.created_at, &tz);
    Ok(entry)
}

/// Save an entry, refusing to overwrite a change made since it was loaded.
///
/// A mismatch comes back as `conflict` and nothing is written, which is what
/// the editor's conflict banner is driven by. `save_entry_force` is the "keep
/// mine" on that banner.
///
/// This is the command a retried save over a dropped connection meets, and the
/// reason [`crate::idempotency`] exists: without it, the retry is refused as a
/// conflict against its own earlier write.
async fn save_entry(svc: Arc<Service>, _ctx: Ctx, args: SaveEntry) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_entry(&args.entry, args.expect)?)).await
}

async fn save_entry_force(svc: Arc<Service>, _ctx: Ctx, args: ForceEntry) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.overwrite_entry(&args.entry)?)).await
}

async fn delete_entry(svc: Arc<Service>, _ctx: Ctx, args: EntryRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_entry(args.id)?)).await
}

async fn search(svc: Arc<Service>, _ctx: Ctx, args: Search) -> CommandResult<Vec<SearchHit>> {
    let vault = svc.require()?;
    // Naming a journal narrows to entries as well: a note is in no journal,
    // so returning some anyway would answer a different question.
    let scope = match (args.kind.as_deref(), args.journal_id) {
        (Some("note"), _) => SearchScope::Notes,
        (_, Some(id)) => SearchScope::Entries(Some(id)),
        (Some("entry"), None) => SearchScope::Entries(None),
        _ => SearchScope::Everything,
    };
    blocking(move || Ok(vault.search(&args.query, scope, args.limit.min(200))?)).await
}

async fn list_tags(svc: Arc<Service>, _ctx: Ctx, _a: Nothing) -> CommandResult<Vec<String>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.entry_tags()?.into_iter().map(|(tag, _)| tag).collect())).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_journals", scope: Journals, effect: Read,
        args: Nothing, returns: "Journal[]", signature: &[],
        run: list_journals,
    },
    command! {
        name: "new_journal", scope: Journals, effect: Read,
        args: Named, returns: "Journal",
        signature: &[("name", "string", true)],
        run: new_journal,
    },
    command! {
        name: "save_journal", scope: Journals, effect: Write,
        change: Journal / Updated,
        args: SaveJournal, returns: "void",
        signature: &[("journal", "Journal", true)],
        run: save_journal,
    },
    command! {
        name: "delete_journal", scope: Journals, effect: Destructive,
        change: Journal / Deleted,
        args: JournalRef, returns: "void",
        signature: &[("id", "JournalId", true)],
        run: delete_journal,
    },
    command! {
        name: "list_entries", scope: Journals, effect: Read,
        args: Entries, returns: "EntrySummary[]",
        signature: &[("query", "EntryQuery", true)],
        run: list_entries,
    },
    command! {
        name: "get_entry", scope: Journals, effect: Read,
        args: EntryRef, returns: "Entry",
        signature: &[("id", "EntryId", true)],
        run: get_entry,
    },
    command! {
        name: "new_entry", scope: Journals, effect: Read,
        args: NewEntry, returns: "Entry",
        signature: &[("journalId", "JournalId", true)],
        run: new_entry,
    },
    command! {
        name: "save_entry", scope: Journals, effect: Write,
        change: Entry / Updated,
        args: SaveEntry, returns: "void",
        signature: &[("entry", "Entry", true), ("expect", "string | null", false)],
        run: save_entry,
    },
    command! {
        name: "save_entry_force", scope: Journals, effect: Write,
        change: Entry / Updated,
        args: ForceEntry, returns: "void",
        signature: &[("entry", "Entry", true)],
        run: save_entry_force,
    },
    command! {
        name: "delete_entry", scope: Journals, effect: Destructive,
        change: Entry / Deleted,
        args: EntryRef, returns: "void",
        signature: &[("id", "EntryId", true)],
        run: delete_entry,
    },
    command! {
        name: "search", scope: Journals, effect: Read,
        args: Search, returns: "SearchHit[]",
        signature: &[
            ("query", "string", true),
            ("journalId", "JournalId | null", false),
            ("kind", "SearchKind | null", false),
            ("limit", "number", true),
        ],
        run: search,
    },
    command! {
        name: "list_tags", scope: Journals, effect: Read,
        args: Nothing, returns: "string[]", signature: &[],
        run: list_tags,
    },
];
