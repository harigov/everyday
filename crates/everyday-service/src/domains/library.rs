//! Shelves, the things on them, and the log of what you did with them.
//!
//! The same division of labour the calendar makes, and for the same reason --
//! this is the second feature that touches a network:
//!
//!   this file        what to look up, when, and what to do with the answer
//!   websearch.rs     turning a request into bytes -- one of two sockets
//!   everyday-core    building every URL, parsing every reply, and deciding
//!                    what a result may change about an item
//!
//! The last of those is the part with the fiddly logic in it -- five reply
//! formats, five rating scales, and the rule that metadata fills gaps and
//! never argues -- and it is testable offline precisely because it never learns
//! that a network exists.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use crate::websearch;
use everyday_core::library::{Item, ItemStatus, Kind, LibraryStats, LogEntry, LogEvent, Progress};
use everyday_core::model::{system_tz, today_local};
use everyday_core::store::library::{ItemQuery, LogQuery};
use everyday_core::{ItemId, KindId, LogId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::Nothing;

/// A shelf, and how much is on it.
///
/// A named struct rather than widening `Kind` itself: the counts are facts
/// about storage at this instant, not properties of the shelf, and they must
/// not be written back when a caller saves one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindInfo {
    #[serde(flatten)]
    pub kind: Kind,
    pub items: u64,
    /// Wishlist, active and paused together: everything still ahead of you.
    pub open: u64,
}

/// What [`add_item`] hands back: the item, and whether the web knew it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedItem {
    pub item: Item,
    pub looked_up: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewKind {
    pub name: String,
    pub singular: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveKind {
    pub kind: Kind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindRef {
    pub id: KindId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Items {
    pub query: ItemQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRef {
    pub id: ItemId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveItem {
    pub item: Item,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveItems {
    pub items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddItem {
    pub kind_id: KindId,
    pub title: String,
    pub lookup: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStatus {
    pub id: ItemId,
    pub status: ItemStatus,
    pub log: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetProgress {
    pub id: ItemId,
    pub position: u32,
    #[serde(default)]
    pub total: Option<u32>,
    pub log: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Logs {
    pub query: LogQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLog {
    pub item_id: ItemId,
    pub event: LogEvent,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveLog {
    pub log: LogEntry,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRef {
    pub id: LogId,
}

/// Every shelf, with its counts, seeding the built-in set into an empty library
/// on the way.
///
/// The seeding is here, in the first call the library app makes, rather than in
/// `create_vault`. That is what makes it work for a vault somebody already had
/// before this app existed: the library appears on their next launch with
/// shelves in it rather than as an empty screen holding a "make a category"
/// button. It runs at most once per vault -- see `Vault::seed_library`, which
/// does nothing whenever there is any shelf at all, so a deleted shelf stays
/// deleted.
async fn list_kinds(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<KindInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        if let Err(e) = vault.seed_library() {
            // Not fatal. An unwritable vault cannot be seeded and can still be
            // read, and a library with no shelves is a screen that says so
            // rather than an error over the whole window.
            tracing::warn!(error = %e, "could not seed the library");
        }
        let mut out = Vec::new();
        for kind in vault.kinds()? {
            // Two `COUNT(*)`s over clear index columns, so listing ten shelves
            // decrypts ten records and nothing else.
            let (items, open) = vault
                .with_store(|s| {
                    s.library().map(|l| l.count_items(kind.id)).unwrap_or_else(|| Ok((0, 0)))
                })
                .unwrap_or((0, 0));
            out.push(KindInfo { kind, items, open });
        }
        Ok(out)
    })
    .await
}

/// Mint a shelf. Unsaved: fill it in and pass it to `save_kind`.
///
/// The slug is derived from the name here rather than in a client, because it
/// is the key metadata lookups and quick capture match on and it has to be
/// stable, lower case and free of spaces whatever somebody typed.
async fn new_kind(svc: Arc<Service>, _ctx: Ctx, args: NewKind) -> CommandResult<Kind> {
    let _ = svc.require()?;
    let name = args.name.trim();
    let singular = if args.singular.trim().is_empty() { name } else { args.singular.trim() };
    Ok(Kind::new(&slugify(singular), name, singular))
}

/// A stable, lower-case, hyphenless key for a name somebody typed.
///
/// Empty in, `custom` out: a shelf with no slug could never be looked up or
/// captured into, and refusing to create it over a punctuation-only name is a
/// worse answer than giving it a dull one.
fn slugify(name: &str) -> String {
    let out: String =
        name.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric()).take(24).collect();
    if out.is_empty() { "custom".to_string() } else { out }
}

async fn save_kind(svc: Arc<Service>, _ctx: Ctx, args: SaveKind) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_kind(&args.kind)).await
}

/// Delete the shelf, everything on it, and every log row those items had.
async fn delete_kind(svc: Arc<Service>, _ctx: Ctx, args: KindRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_kind(args.id)).await
}

async fn list_items(svc: Arc<Service>, _ctx: Ctx, args: Items) -> CommandResult<Vec<Item>> {
    svc.on_vault(move |vault| vault.items(&args.query)).await
}

async fn get_item(svc: Arc<Service>, _ctx: Ctx, args: ItemRef) -> CommandResult<Item> {
    svc.on_vault(move |vault| vault.item(args.id)).await
}

async fn save_item(svc: Arc<Service>, _ctx: Ctx, args: SaveItem) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_item(&args.item)).await
}

/// One write for many items: what a re-ordered shelf is.
async fn save_items(svc: Arc<Service>, _ctx: Ctx, args: SaveItems) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_items(&args.items)).await
}

/// Delete the item and its whole log.
async fn delete_item(svc: Arc<Service>, _ctx: Ctx, args: ItemRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_item(args.id)).await
}

/// Add something to a shelf, and -- if asked -- go and find out what it is.
///
/// One command rather than create-then-enrich, because the two are not
/// independent from a caller's point of view: what it wants back is the
/// finished card, and a two-step version would either draw a blank card that
/// changes under the cursor a second later or make the caller sequence two
/// awaits and handle a failure between them.
///
/// The lookup is best-effort by design, and it happens *after* the item is on
/// disk. A network that is off, a source that is down, a title nothing has
/// heard of -- none of those should stop something being added to a list, which
/// is the entire job of this app. `looked_up` says whether anything was found,
/// so a caller can offer to search again rather than silently implying it tried.
async fn add_item(svc: Arc<Service>, ctx: Ctx, args: AddItem) -> CommandResult<AddedItem> {
    let vault = svc.require()?;
    let title = args.title.trim().to_string();
    if title.is_empty() {
        return Err(CommandError::new(codes::INVALID, "give it a name and it will be added"));
    }
    let kind_id = args.kind_id;
    let kind = {
        let vault = vault.clone();
        blocking(move || Ok(vault.kind(kind_id)?)).await?
    };
    let mut item = Item::new(kind_id, &title);

    // Written *before* the lookup, not after.
    //
    // This is the ordering the whole feature turns on. A lookup can take up to
    // the fetch timeout, and if the item were only minted in memory until it
    // came back, an app that was quit -- or a machine that ran out of battery --
    // during those seconds would lose the thing somebody was trying not to
    // forget. Enriching costs a second write; getting this backwards costs the
    // note.
    {
        let (vault, saved) = (vault.clone(), item.clone());
        blocking(move || Ok(vault.save_item(&saved)?)).await?;
    }

    // A lookup is a request leaving the machine, so it needs the scope for
    // that. Refusing the *whole command* would be the wrong answer: a client
    // that may keep a list but not reach the web should still be able to add
    // something to it, and the item is already saved.
    if !args.lookup || !ctx.holds(crate::ctx::Scope::Web) {
        return Ok(AddedItem { item, looked_up: false });
    }

    let hit = match websearch::lookup(&title, &kind, 1).await {
        Ok(hits) => hits.into_iter().next(),
        // Worth a line in the log and nothing more: the item is already on
        // disk, and a network that is off is not a reason to refuse to keep a
        // list. See the doc comment.
        Err(e) => {
            tracing::info!(error = %e, "could not look up a new item");
            None
        }
    };
    let Some(hit) = hit else { return Ok(AddedItem { item, looked_up: false }) };

    websearch::apply_and_cover(&vault, &hit, &kind, &mut item, false).await;
    let (vault, saved) = (vault.clone(), item.clone());
    // The second write is the enrichment. If *it* fails, the item is still
    // there with the title that was typed, which is the outcome to prefer.
    blocking(move || Ok(vault.save_item(&saved)?)).await?;
    Ok(AddedItem { item, looked_up: true })
}

/// Move an item to `status`, dating it and -- optionally -- logging it.
///
/// The dates and the log row are coupled here rather than left to a client
/// because they are the same act. Marking a book read is the moment "finished
/// on" is known and the moment the log gains the row that makes "what did I
/// read this year" answerable, and an interface that had to remember to do all
/// three would eventually do two.
///
/// `Vault::save_item` still does the writing, so nothing here can produce an
/// item the ordinary save path would refuse.
async fn set_item_status(svc: Arc<Service>, _ctx: Ctx, args: SetStatus) -> CommandResult<Item> {
    let vault = svc.require()?;
    let today = today_local();
    let tz = system_tz();
    blocking(move || {
        let mut item = vault.item(args.id)?;
        if item.status == args.status {
            return Ok(item);
        }
        item.set_status(args.status, today);
        vault.save_item(&item)?;

        // Only the transitions that mean something happened on a day. Moving
        // something back to the wishlist is a correction, not an event, and
        // logging it would put a line in the history saying nothing.
        let event = match args.status {
            ItemStatus::Active => Some(LogEvent::Started),
            ItemStatus::Done => Some(LogEvent::Finished),
            ItemStatus::Paused | ItemStatus::Abandoned => Some(LogEvent::Stopped),
            ItemStatus::Wishlist => None,
        };
        if args.log
            && let Some(event) = event
        {
            vault.save_log(&LogEntry::new(item.id, event, today, &tz))?;
        }
        Ok(item)
    })
    .await
}

/// Record where you have got to, and log it.
///
/// The log row is what makes a reading pace visible later; the field on the
/// item is what the card draws now. Both, from one action, for the reason given
/// on [`set_item_status`].
async fn set_item_progress(svc: Arc<Service>, _ctx: Ctx, args: SetProgress) -> CommandResult<Item> {
    let vault = svc.require()?;
    let today = today_local();
    let tz = system_tz();
    blocking(move || {
        let mut item = vault.item(args.id)?;
        // The unit comes from the shelf, and is copied onto the item rather
        // than read live -- see `library::Progress`, which explains why changing
        // a shelf from pages to minutes must not relabel four hundred books.
        let unit = match &item.progress {
            Some(p) if !p.unit.is_empty() => p.unit.clone(),
            _ => vault.kind(item.kind_id).map(|k| k.progress_unit).unwrap_or_default(),
        };
        let total = args.total.or_else(|| item.progress.as_ref().and_then(|p| p.total));
        item.progress = Some(Progress::new(args.position, total, unit));
        // Recording progress on something you had only wished for is you
        // telling us you have started it.
        if item.status == ItemStatus::Wishlist {
            item.set_status(ItemStatus::Active, today);
        }
        item.updated_at = jiff::Timestamp::now();
        vault.save_item(&item)?;

        if args.log {
            let mut entry = LogEntry::new(item.id, LogEvent::Progress, today, &tz);
            entry.position = Some(args.position);
            vault.save_log(&entry)?;
        }
        Ok(item)
    })
    .await
}

async fn list_logs(svc: Arc<Service>, _ctx: Ctx, args: Logs) -> CommandResult<Vec<LogEntry>> {
    svc.on_vault(move |vault| vault.logs(&args.query)).await
}

/// Mint a log row dated today on the machine's own calendar. Unsaved.
///
/// The date and the time zone are resolved here rather than in a webview so
/// that "what did I finish today" is decided by the same code that decides
/// which day a journal entry is filed under.
async fn new_log(svc: Arc<Service>, _ctx: Ctx, args: NewLog) -> CommandResult<LogEntry> {
    let _ = svc.require()?;
    Ok(LogEntry::new(args.item_id, args.event, today_local(), system_tz()))
}

async fn save_log(svc: Arc<Service>, _ctx: Ctx, args: SaveLog) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_log(&args.log)).await
}

async fn delete_log(svc: Arc<Service>, _ctx: Ctx, args: LogRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_log(args.id)).await
}

/// Counts for the library sidebar, as of the machine's own calendar year.
async fn library_stats(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<LibraryStats> {
    let vault = svc.require()?;
    let year = today_local().year();
    blocking(move || Ok(vault.library_stats(year)?)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_kinds", scope: Library, effect: Read,
        args: Nothing, returns: "KindInfo[]", signature: &[],
        run: list_kinds,
    },
    command! {
        name: "new_kind", scope: Library, effect: Read,
        args: NewKind, returns: "Kind",
        signature: &[("name", "string", true), ("singular", "string", true)],
        run: new_kind,
    },
    command! {
        name: "save_kind", scope: Library, effect: Write,
        change: Shelf / Updated,
        args: SaveKind, returns: "void",
        signature: &[("kind", "Kind", true)],
        run: save_kind,
    },
    command! {
        name: "delete_kind", scope: Library, effect: Destructive,
        change: Shelf / Deleted,
        args: KindRef, returns: "void",
        signature: &[("id", "KindId", true)],
        run: delete_kind,
    },
    command! {
        name: "list_items", scope: Library, effect: Read,
        args: Items, returns: "Item[]",
        signature: &[("query", "ItemQuery", true)],
        run: list_items,
    },
    command! {
        name: "get_item", scope: Library, effect: Read,
        args: ItemRef, returns: "Item",
        signature: &[("id", "ItemId", true)],
        run: get_item,
    },
    command! {
        name: "save_item", scope: Library, effect: Write,
        change: Item / Updated,
        args: SaveItem, returns: "void",
        signature: &[("item", "Item", true)],
        run: save_item,
    },
    command! {
        name: "save_items", scope: Library, effect: Write,
        change: Item / Updated,
        args: SaveItems, returns: "void",
        signature: &[("items", "Item[]", true)],
        run: save_items,
    },
    command! {
        name: "delete_item", scope: Library, effect: Destructive,
        change: Item / Deleted,
        args: ItemRef, returns: "void",
        signature: &[("id", "ItemId", true)],
        run: delete_item,
    },
    command! {
        name: "add_item", scope: Library, effect: Write,
        change: Item / Created,
        args: AddItem, returns: "AddedItem",
        signature: &[
            ("kindId", "KindId", true),
            ("title", "string", true),
            ("lookup", "boolean", true),
        ],
        run: add_item,
    },
    command! {
        name: "set_item_status", scope: Library, effect: Write,
        change: Item / Updated,
        args: SetStatus, returns: "Item",
        signature: &[("id", "ItemId", true), ("status", "ItemStatus", true), ("log", "boolean", true)],
        run: set_item_status,
    },
    command! {
        name: "set_item_progress", scope: Library, effect: Write,
        change: Item / Updated,
        args: SetProgress, returns: "Item",
        signature: &[
            ("id", "ItemId", true),
            ("position", "number", true),
            ("total", "number | null", false),
            ("log", "boolean", true),
        ],
        run: set_item_progress,
    },
    command! {
        name: "list_logs", scope: Library, effect: Read,
        args: Logs, returns: "LogEntry[]",
        signature: &[("query", "LogQuery", true)],
        run: list_logs,
    },
    command! {
        name: "new_log", scope: Library, effect: Read,
        args: NewLog, returns: "LogEntry",
        signature: &[("itemId", "ItemId", true), ("event", "LogEvent", true)],
        run: new_log,
    },
    command! {
        name: "save_log", scope: Library, effect: Write,
        change: Log / Updated,
        args: SaveLog, returns: "void",
        signature: &[("log", "LogEntry", true)],
        run: save_log,
    },
    command! {
        name: "delete_log", scope: Library, effect: Destructive,
        change: Log / Deleted,
        args: LogRef, returns: "void",
        signature: &[("id", "LogId", true)],
        run: delete_log,
    },
    command! {
        name: "library_stats", scope: Library, effect: Read,
        args: Nothing, returns: "LibraryStats", signature: &[],
        run: library_stats,
    },
];
