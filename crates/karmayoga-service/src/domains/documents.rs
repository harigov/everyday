//! Documents: reading a PDF, an EPUB, Markdown, text or a web page in the
//! files catalog, and searching every one at once. The work is
//! [`crate::documents`]'; these are its doors.
//!
//! Scope `Files`, as the files are. Health's files are refused to a
//! connection without `Health`, exactly as `domains::files` refuses them.

use std::collections::BTreeMap;
use std::sync::Arc;

use jiff::Timestamp;
use karmayoga_core::documents::{
    DocChapterView, DocFormat, DocHit, DocIndexStatus, DocOutline, DocSearch, DocSearchPage,
    READER_BLOCK_KEY, READER_PAGE_KEY,
};
use karmayoga_core::files::{FileEntry, FileOrigin, FileUpdate, PropValue};
use karmayoga_core::id::FileId;
use karmayoga_core::library::Progress;
use karmayoga_core::{ItemQuery, Vault};
use serde::Deserialize;

use crate::command;
use crate::ctx::{Ctx, Scope};
use crate::documents::DocumentsState;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};

use super::Nothing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRef {
    pub id: FileId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterRef {
    pub id: FileId,
    pub chapter: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocument {
    pub id: FileId,
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocuments {
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveReaderPosition {
    pub id: FileId,
    pub page: u32,
    #[serde(default)]
    pub block: Option<u32>,
}

/// What a search says when it is not told how many to answer with.
const DEFAULT_SEARCH_LIMIT: usize = 20;
/// What a search inside one document says when it is not told.
const DEFAULT_IN_DOCUMENT_LIMIT: usize = 30;
/// The most either answers at once.
const MAX_LIMIT: usize = 100;

/// Whether `ctx` may see Health's files, as `domains::files` decides it: a
/// clinical document's pages answer to Health's scope as well as Files'.
fn sees_health(ctx: &Ctx) -> bool {
    ctx.holds(Scope::Health)
}

/// Refuse a file of Health's to a connection that may not see them, in the
/// words `domains::files` uses. A file that is not there is left for the
/// command's own read to report.
fn refuse_health(ctx: &Ctx, vault: &Vault, id: FileId) -> CommandResult<()> {
    if sees_health(ctx) {
        return Ok(());
    }
    if vault.file(id).is_ok_and(|f| f.origin == FileOrigin::Health) {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "that file is one of Health's, which this connection is not allowed to reach",
        ));
    }
    Ok(())
}

/// The catalogued file `id`, for a caller who may see it.
fn file_for(ctx: &Ctx, vault: &Vault, id: FileId) -> CommandResult<FileEntry> {
    refuse_health(ctx, vault, id)?;
    Ok(vault.file(id)?)
}

async fn document_outline(
    svc: Arc<Service>,
    ctx: Ctx,
    args: DocumentRef,
) -> CommandResult<DocOutline> {
    let vault = svc.require()?;
    blocking(move || {
        let file = file_for(&ctx, &vault, args.id)?;
        Ok(svc.documents()?.outline(&vault, &file)?)
    })
    .await
}

async fn document_chapter(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ChapterRef,
) -> CommandResult<DocChapterView> {
    let vault = svc.require()?;
    blocking(move || {
        let file = file_for(&ctx, &vault, args.id)?;
        Ok(svc.documents()?.chapter(&vault, &file, args.chapter)?)
    })
    .await
}

async fn search_document(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SearchDocument,
) -> CommandResult<Vec<DocHit>> {
    let vault = svc.require()?;
    blocking(move || {
        let file = file_for(&ctx, &vault, args.id)?;
        let text = svc.documents()?.text(&vault, &file)?;
        Ok(text.search(&args.query, args.limit.unwrap_or(DEFAULT_IN_DOCUMENT_LIMIT).min(MAX_LIMIT)))
    })
    .await
}

/// Health's files are never indexed (`FilesSettings::indexes`), so the index
/// holds none to leave out for a caller without Health's scope; the check the
/// other commands make is not needed here.
async fn search_documents(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SearchDocuments,
) -> CommandResult<DocSearchPage> {
    let vault = svc.require()?;
    blocking(move || {
        let query = DocSearch {
            text: args.query,
            files: Vec::new(),
            limit: args.limit.unwrap_or(DEFAULT_SEARCH_LIMIT).clamp(1, MAX_LIMIT),
            offset: args.offset.unwrap_or(0),
        };
        Ok(svc.documents()?.search(&vault, &query)?)
    })
    .await
}

async fn save_reader_position(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SaveReaderPosition,
) -> CommandResult<()> {
    let vault = svc.require()?;
    let now = svc.now();
    let state = svc.documents()?;
    let moved = blocking(move || {
        refuse_health(&ctx, &vault, args.id)?;
        Ok(save_place(&vault, &state, args.id, args.page, args.block, now)?)
    })
    .await?;
    // The place itself is the reader's own bookkeeping and announces nothing
    // (see `INVISIBLE` in `command.rs`); an item whose progress moved is
    // something a shelf draws.
    if moved {
        svc.events().changed(Change::new(Kind::Item, Op::Updated));
    }
    Ok(())
}

/// Keep where somebody is in a document -- in the file's properties, so it
/// opens there next time, whichever window or device they read it on -- and
/// move the progress of every library item that has the file along with it.
///
/// Nothing is written that has not changed: a reader that reports its place
/// every few seconds does not make a record, or a change for every other
/// window to hear, of standing still. The items are looked at only when the
/// page moved, which is the only thing their progress says. Answers whether
/// any item's progress moved.
fn save_place(
    vault: &Vault,
    state: &DocumentsState,
    id: FileId,
    page: u32,
    block: Option<u32>,
    now: Timestamp,
) -> karmayoga_core::Result<bool> {
    let file = vault.file(id)?;
    if DocFormat::of_file(&file).is_none() {
        return Err(karmayoga_core::Error::Invalid(format!(
            "{} is not a document, so it has no place to keep",
            file.name
        )));
    }
    let page = page.max(1);
    let stored = |key: &str| match file.properties.get(key) {
        Some(PropValue::Number(n)) if n.is_finite() && *n >= 0.0 => Some(*n as u32),
        _ => None,
    };
    let (was_page, was_block) = (stored(READER_PAGE_KEY), stored(READER_BLOCK_KEY));
    if (was_page, was_block) != (Some(page), block) {
        let mut properties = BTreeMap::new();
        properties.insert(READER_PAGE_KEY.to_string(), Some(PropValue::Number(f64::from(page))));
        let block = block.map(|b| PropValue::Number(f64::from(b)));
        properties.insert(READER_BLOCK_KEY.to_string(), block);
        vault.update_files(&[id], &FileUpdate { properties, ..FileUpdate::default() })?;
    }
    // The place is kept whether or not this works; an item's progress is a
    // convenience on top of it.
    if was_page == Some(page) {
        return Ok(false);
    }
    match move_items(vault, state, &file, page, now) {
        Ok(moved) => Ok(moved > 0),
        Err(e) => {
            tracing::debug!(error = %e, "documents: could not move an item's progress");
            Ok(false)
        }
    }
}

/// Set the progress of every library item whose files include `file` to
/// `page` of the document's pages, in pages, for those it is not already
/// that.
fn move_items(
    vault: &Vault,
    state: &DocumentsState,
    file: &FileEntry,
    page: u32,
    now: Timestamp,
) -> karmayoga_core::Result<usize> {
    let mut owners: Vec<_> = vault
        .items(&ItemQuery::default())?
        .into_iter()
        .filter(|item| item.files.contains(&file.id))
        .collect();
    if owners.is_empty() {
        return Ok(0);
    }
    // The document's own count of its pages, when it can be read: the finishing
    // line that makes "page 47" a fraction.
    let total = state.text(vault, file).ok().map(|text| text.pages).filter(|pages| *pages > 0);
    let progress = Progress::new(page, total, "page");
    owners.retain(|item| item.progress.as_ref() != Some(&progress));
    for item in &mut owners {
        item.progress = Some(progress.clone());
        item.updated_at = now;
    }
    if !owners.is_empty() {
        vault.save_items(&owners)?;
    }
    Ok(owners.len())
}

async fn documents_status(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<DocIndexStatus> {
    let vault = svc.require()?;
    blocking(move || Ok(svc.documents()?.status(&vault))).await
}

async fn reindex_documents(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<DocIndexStatus> {
    let vault = svc.require()?;
    blocking(move || {
        let state = svc.documents()?;
        state.reindex();
        Ok(state.status(&vault))
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "document_outline", scope: Files, effect: Read,
        args: DocumentRef, returns: "DocOutline",
        signature: &[("id", "FileId", true)],
        run: document_outline,
    },
    command! {
        name: "document_chapter", scope: Files, effect: Read,
        args: ChapterRef, returns: "DocChapterView",
        signature: &[("id", "FileId", true), ("chapter", "number", true)],
        run: document_chapter,
    },
    command! {
        name: "search_document", scope: Files, effect: Read,
        args: SearchDocument, returns: "DocHit[]",
        signature: &[("id", "FileId", true), ("query", "string", true), ("limit", "number", false)],
        run: search_document,
    },
    command! {
        name: "search_documents", scope: Files, effect: Read,
        args: SearchDocuments, returns: "DocSearchPage",
        signature: &[
            ("query", "string", true),
            ("limit", "number", false),
            ("offset", "number", false),
        ],
        run: search_documents,
    },
    command! {
        name: "save_reader_position", scope: Files, effect: Write,
        args: SaveReaderPosition, returns: "void",
        signature: &[("id", "FileId", true), ("page", "number", true), ("block", "number", false)],
        run: save_reader_position,
    },
    command! {
        name: "documents_status", scope: Files, effect: Read,
        args: Nothing, returns: "DocIndexStatus", signature: &[],
        run: documents_status,
    },
    command! {
        name: "reindex_documents", scope: Files, effect: Write,
        args: Nothing, returns: "DocIndexStatus", signature: &[],
        run: reindex_documents,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Caller;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::files::ImportOptions;
    use karmayoga_core::library::Item;

    fn open() -> (tempfile::TempDir, tempfile::TempDir, Arc<Service>, Arc<Vault>) {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = karmayoga_vault::create(home.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        (home, folder, svc, vault)
    }

    /// A connection that holds Files and nothing else.
    fn files_only() -> Ctx {
        let scopes = vec![Scope::Files];
        Ctx { caller: Caller::Socket, scopes, proved_at: None, request_id: None }
    }

    fn plain(vault: &Vault, name: &str, bytes: &[u8]) -> FileEntry {
        vault.import_bytes(bytes, name, &ImportOptions::new(FileOrigin::Files)).unwrap()
    }

    fn health(vault: &Vault, name: &str, bytes: &[u8]) -> FileEntry {
        let options =
            ImportOptions { encrypted: Some(true), ..ImportOptions::new(FileOrigin::Health) };
        vault.import_bytes(bytes, name, &options).unwrap()
    }

    #[tokio::test]
    async fn health_files_are_refused_to_a_connection_without_health_and_every_door_is_shut() {
        let (_home, _folder, svc, vault) = open();
        let labs = health(&vault, "labs.txt", b"results");

        let refused = |e: CommandError| assert_eq!(e.code, codes::FORBIDDEN, "{e}");
        refused(
            document_outline(svc.clone(), files_only(), DocumentRef { id: labs.id })
                .await
                .unwrap_err(),
        );
        refused(
            document_chapter(svc.clone(), files_only(), ChapterRef { id: labs.id, chapter: 0 })
                .await
                .unwrap_err(),
        );
        refused(
            search_document(
                svc.clone(),
                files_only(),
                SearchDocument { id: labs.id, query: "results".into(), limit: None },
            )
            .await
            .unwrap_err(),
        );
        refused(
            save_reader_position(
                svc.clone(),
                files_only(),
                SaveReaderPosition { id: labs.id, page: 2, block: None },
            )
            .await
            .unwrap_err(),
        );
        // Nothing was written to it.
        assert!(vault.file(labs.id).unwrap().properties.is_empty());

        // A connection that holds Health is let as far as the state, which
        // never reads a Health file: documents are not where those are shown.
        let all = document_outline(svc.clone(), Ctx::local(), DocumentRef { id: labs.id })
            .await
            .unwrap_err();
        assert_eq!(all.code, codes::INVALID, "{all}");
        assert!(all.message.contains("Health"), "{all}");
    }

    #[tokio::test]
    async fn what_is_not_a_document_or_is_in_the_trash_is_refused_with_words() {
        let (_home, _folder, svc, vault) = open();
        let picture = plain(&vault, "holiday.png", b"\x89PNG\r\n\x1a\n");
        let e = document_outline(svc.clone(), Ctx::local(), DocumentRef { id: picture.id })
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::INVALID);
        assert!(e.message.contains("holiday.png"), "{e}");
        let e = save_reader_position(
            svc.clone(),
            Ctx::local(),
            SaveReaderPosition { id: picture.id, page: 1, block: None },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, codes::INVALID);

        let note = plain(&vault, "notes.txt", b"words");
        vault.trash_files(&[note.id]).unwrap();
        let e = document_outline(svc.clone(), Ctx::local(), DocumentRef { id: note.id })
            .await
            .unwrap_err();
        assert!(e.message.contains("trash"), "{e}");

        let e = document_outline(svc, Ctx::local(), DocumentRef { id: FileId::new() })
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_document_is_opened_read_in_and_searched_through_the_commands() {
        let (_home, _folder, svc, vault) = open();
        let file = plain(
            &vault,
            "dune.txt",
            b"The spice must flow.\n\nMortimer found the xylophone in the orchard.",
        );
        let outline =
            document_outline(svc.clone(), Ctx::local(), DocumentRef { id: file.id }).await.unwrap();
        assert_eq!((outline.file, outline.format), (file.id, DocFormat::Text));
        assert!(outline.blocks >= 1 && outline.position.is_none() && !outline.indexed);

        let chapter =
            document_chapter(svc.clone(), Ctx::local(), ChapterRef { id: file.id, chapter: 0 })
                .await
                .unwrap();
        assert!(chapter.blocks.iter().any(|b| b.html.contains("xylophone")));
        let hits = search_document(
            svc.clone(),
            Ctx::local(),
            SearchDocument { id: file.id, query: "xylophone".into(), limit: None },
        )
        .await
        .unwrap();
        assert!(!hits.is_empty(), "the document is searched by its own blocks");

        // Across every document: nothing is indexed until a pass has run.
        let page = search_documents(
            svc.clone(),
            files_only(),
            SearchDocuments { query: "xylophone".into(), limit: Some(1_000), offset: None },
        )
        .await
        .unwrap();
        assert!(page.hits.is_empty());
        let status = documents_status(svc.clone(), files_only(), Nothing {}).await.unwrap();
        assert!(status.enabled && status.indexed == 0);
        let status = reindex_documents(svc, files_only(), Nothing {}).await.unwrap();
        assert_eq!(status.indexed, 0);
    }

    #[tokio::test]
    async fn the_place_is_kept_in_the_file_and_moves_the_progress_of_the_items_that_have_it() {
        let (_home, _folder, svc, vault) = open();
        vault.seed_library().unwrap();
        let kind = vault.kinds().unwrap()[0].id;
        let file = plain(&vault, "dune.txt", b"one two three\n\nfour five six");
        let other = plain(&vault, "other.txt", b"unrelated");
        let mut owner = Item::new(kind, "Dune");
        owner.files = vec![file.id];
        let mut bystander = Item::new(kind, "Something else");
        bystander.files = vec![other.id];
        vault.save_item(&owner).unwrap();
        vault.save_item(&bystander).unwrap();

        let save = |page, block| {
            let args = SaveReaderPosition { id: file.id, page, block };
            save_reader_position(svc.clone(), Ctx::local(), args)
        };
        save(3, Some(7)).await.unwrap();
        let stored = vault.file(file.id).unwrap();
        assert_eq!(stored.properties.get(READER_PAGE_KEY), Some(&PropValue::Number(3.0)));
        assert_eq!(stored.properties.get(READER_BLOCK_KEY), Some(&PropValue::Number(7.0)));
        let outline =
            document_outline(svc.clone(), Ctx::local(), DocumentRef { id: file.id }).await.unwrap();
        assert_eq!(outline.position.map(|p| (p.page, p.block)), Some((3, Some(7))));

        let moved = vault.item(owner.id).unwrap();
        let progress = moved.progress.clone().expect("progress was set");
        assert_eq!((progress.position, progress.unit.as_str()), (3, "page"));
        let pages = svc.documents().unwrap().text(&vault, &stored).unwrap().pages;
        assert_eq!(progress.total, Some(pages).filter(|p| *p > 0));
        assert!(vault.item(bystander.id).unwrap().progress.is_none());

        // Only the block moved: the item is not written again.
        save(3, Some(9)).await.unwrap();
        let kept = vault.file(file.id).unwrap();
        assert_eq!(kept.properties.get(READER_BLOCK_KEY), Some(&PropValue::Number(9.0)));
        assert_eq!(vault.item(owner.id).unwrap().updated_at, moved.updated_at);
        // The same place again: nothing at all.
        let before = vault.file(file.id).unwrap().updated_at;
        save(3, Some(9)).await.unwrap();
        assert_eq!(vault.file(file.id).unwrap().updated_at, before);

        // A new page moves it; a block left out is a block forgotten; a page
        // of nought is the first.
        save(1, None).await.unwrap();
        assert_eq!(vault.item(owner.id).unwrap().progress.unwrap().position, 1);
        assert!(!vault.file(file.id).unwrap().properties.contains_key(READER_BLOCK_KEY));
        save(0, None).await.unwrap();
        let kept = vault.file(file.id).unwrap();
        assert_eq!(kept.properties.get(READER_PAGE_KEY), Some(&PropValue::Number(1.0)));
    }
}
