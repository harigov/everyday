//! A library item's files, over a real SQLite backend: which items refer to
//! which files, kept up by every save and delete, rebuilt when the table
//! drifts, the one kind of file a shelf is never allowed to hold, and what
//! the assistant is told about them and may say.
//!
//! The files are sealed, which needs no files folder, so nothing here touches
//! the disk beyond the vault's own directory.

mod support;
use support::{call, call_err, vault};

use jatayu_core::files::{FileOrigin, ImportOptions, RefKind, RefOwner};
use jatayu_core::{FileId, Item, Kind, Vault};

fn sealed(vault: &Vault, name: &str, origin: FileOrigin) -> FileId {
    let opts = ImportOptions { encrypted: Some(true), ..ImportOptions::new(origin) };
    vault.import_bytes(name.as_bytes(), name, &opts).unwrap().id
}

fn book_shelf(vault: &Vault) -> Kind {
    vault.seed_library().unwrap();
    vault.kinds().unwrap().into_iter().find(|k| k.slug == "book").unwrap()
}

fn owner(item: &Item) -> RefOwner {
    RefOwner::new(RefKind::Item, item.id)
}

#[test]
fn saving_an_item_records_its_files_and_the_files_app_names_the_item() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let epub = sealed(&vault, "dune.epub", FileOrigin::Files);
    let other = sealed(&vault, "dune-notes.pdf", FileOrigin::Files);

    let mut dune = Item::new(books.id, "Dune");
    dune.files = vec![epub];
    vault.save_item(&dune).unwrap();

    let uses = vault.file_uses(epub).unwrap();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].owner, owner(&dune));
    assert_eq!(uses[0].title, "Dune");
    assert_eq!(uses[0].when, Some(dune.created_at));
    assert_eq!(
        uses[0].parent.as_deref(),
        Some(books.id.to_string().as_str()),
        "the shelf, for opening it"
    );
    assert!(vault.file_uses(other).unwrap().is_empty());

    // Another file in its place moves the reference; renaming the item is
    // seen in the name the Files app gives it.
    dune.files = vec![other];
    dune.title = "Dune (1965)".into();
    vault.save_item(&dune).unwrap();
    assert!(vault.file_uses(epub).unwrap().is_empty());
    assert_eq!(vault.file_uses(other).unwrap()[0].title, "Dune (1965)");

    // Two items may hold one file -- a collection and the book in it -- and
    // deleting one leaves the other's reference.
    let mut again = Item::new(books.id, "Dune, again");
    again.files = vec![other];
    vault.save_item(&again).unwrap();
    assert_eq!(vault.file_uses(other).unwrap().len(), 2);
    vault.delete_item(dune.id).unwrap();
    let left = vault.file_uses(other).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].owner, owner(&again));
    vault.delete_item(again.id).unwrap();
    assert!(vault.file_uses(other).unwrap().is_empty());
}

#[test]
fn saving_many_items_records_each_ones_files() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let (a, b) =
        (sealed(&vault, "a.epub", FileOrigin::Files), sealed(&vault, "b.epub", FileOrigin::Files));
    let mut first = Item::new(books.id, "A");
    first.files = vec![a];
    let mut second = Item::new(books.id, "B");
    second.files = vec![b, a];
    vault.save_items(&[first, second]).unwrap();

    assert_eq!(vault.file_uses(a).unwrap().len(), 2);
    assert_eq!(vault.file_uses(b).unwrap().len(), 1);
}

#[test]
fn a_file_the_catalog_does_not_hold_stays_on_the_item_and_is_not_referenced() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let held = sealed(&vault, "held.epub", FileOrigin::Files);
    let gone = FileId::new();

    let mut item = Item::new(books.id, "Dune");
    item.files = vec![gone, held];
    vault.save_item(&item).unwrap();

    assert_eq!(vault.item(item.id).unwrap().files, vec![gone, held], "left as it was written");
    assert_eq!(vault.file_uses(held).unwrap().len(), 1);
    assert!(vault.file_uses(gone).unwrap().is_empty());
}

#[test]
fn deleting_a_shelf_forgets_what_its_items_referred_to() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let epub = sealed(&vault, "dune.epub", FileOrigin::Files);
    let mut dune = Item::new(books.id, "Dune");
    dune.files = vec![epub];
    vault.save_item(&dune).unwrap();
    assert_eq!(vault.file_uses(epub).unwrap().len(), 1);

    vault.delete_kind(books.id).unwrap();
    assert!(vault.file_uses(epub).unwrap().is_empty());
}

#[test]
fn rebuilding_the_references_finds_every_items_files() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let epub = sealed(&vault, "dune.epub", FileOrigin::Files);
    let pdf = sealed(&vault, "paper.pdf", FileOrigin::Files);
    let mut dune = Item::new(books.id, "Dune");
    dune.files = vec![epub, pdf];
    vault.save_item(&dune).unwrap();
    // A file the catalog never had is not a reference, here as at a save.
    let mut ghost = Item::new(books.id, "Ghost");
    ghost.files = vec![FileId::new()];
    vault.save_item(&ghost).unwrap();

    // The table drifts: somebody's sync, an older build, a crash between.
    vault.set_file_refs(&owner(&dune), &[]).unwrap();
    assert!(vault.file_uses(epub).unwrap().is_empty());

    assert_eq!(vault.rebuild_file_refs().unwrap(), 2);
    assert_eq!(vault.file_uses(epub).unwrap()[0].owner, owner(&dune));
    assert_eq!(vault.file_uses(pdf).unwrap()[0].owner, owner(&dune));
}

#[test]
fn an_item_may_not_hold_one_of_healths_files() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let report = sealed(&vault, "labs.pdf", FileOrigin::Health);
    let fine = sealed(&vault, "fine.pdf", FileOrigin::Files);

    let mut item = Item::new(books.id, "My bloods");
    item.files = vec![fine, report];
    let err = vault.save_item(&item).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("labs.pdf"), "{err}");
    assert!(err.to_string().contains("Health"), "{err}");
    assert!(vault.item(item.id).is_err(), "nothing was saved");
    assert!(vault.file_uses(report).unwrap().is_empty());
    assert!(vault.file_uses(fine).unwrap().is_empty());

    // Nor in a batch, which would otherwise be the way round.
    let err = vault.save_items(std::slice::from_ref(&item)).unwrap_err();
    assert_eq!(err.code(), "invalid");

    item.files = vec![fine];
    vault.save_item(&item).unwrap();
    assert_eq!(vault.file_uses(fine).unwrap().len(), 1);
}

// ---- the assistant's tools ---------------------------------------------------

fn shelf_id(vault: &Vault) -> String {
    book_shelf(vault).id.to_string()
}

#[test]
fn the_assistant_keeps_files_with_an_item_and_reads_back_what_they_are() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let shelf = shelf_id(&vault);
    let epub = sealed(&vault, "dune.epub", FileOrigin::Files);
    let pdf = sealed(&vault, "dune-study-guide.pdf", FileOrigin::Files);
    let picture = sealed(&vault, "cover.png", FileOrigin::Files);

    let made = call(
        &vault,
        "create_item",
        serde_json::json!({
            "shelf_id": shelf,
            "title": "Dune",
            "files": [epub.to_string(), epub.to_string(), pdf.to_string(), picture.to_string()],
        }),
    );
    let id = made["id"].as_str().unwrap();
    let stored = vault.item(id.parse().unwrap()).unwrap();
    assert_eq!(stored.files, vec![epub, pdf, picture], "in order, and a file named twice once");
    assert_eq!(vault.file_uses(epub).unwrap().len(), 1, "saved through the vault's own path");

    let item = call(&vault, "get_item", serde_json::json!({ "item_id": id }));
    let files = item["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0]["id"], epub.to_string());
    assert_eq!(files[0]["name"], "dune.epub");
    assert_eq!(files[0]["format"], "epub");
    assert_eq!(files[1]["format"], "pdf");
    assert!(files[2].get("format").is_none(), "a picture is not a document");
    assert!(item.get("has_files").is_none());
    assert!(item.get("filled_from").is_none(), "typed in, not filled in");

    // A listing says there is a file, and no more.
    let listed = call(&vault, "list_items", serde_json::json!({}));
    assert_eq!(listed["items"][0]["has_files"], true);
    assert!(listed["items"][0].get("files").is_none());

    // The tools that read them are named where the model will see it.
    let describe = |name: &str| jatayu_core::agent::tools::find(name).unwrap().description;
    for word in ["document_outline", "search_document", "read_document"] {
        assert!(describe("get_item").contains(word), "get_item does not name {word}");
    }
}

#[test]
fn updating_an_item_replaces_its_files_only_when_they_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let shelf = shelf_id(&vault);
    let (a, b) =
        (sealed(&vault, "a.epub", FileOrigin::Files), sealed(&vault, "b.pdf", FileOrigin::Files));
    let id = call(
        &vault,
        "create_item",
        serde_json::json!({ "shelf_id": shelf, "title": "Dune", "files": [a.to_string()] }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let item_id: jatayu_core::ItemId = id.parse().unwrap();

    // Naming something else leaves the files as they were.
    call(&vault, "update_item", serde_json::json!({ "item_id": id, "notes": "Loved it" }));
    assert_eq!(vault.item(item_id).unwrap().files, vec![a]);

    // Naming them replaces the list, and the references move with it.
    call(&vault, "update_item", serde_json::json!({ "item_id": id, "files": [b.to_string()] }));
    assert_eq!(vault.item(item_id).unwrap().files, vec![b]);
    assert!(vault.file_uses(a).unwrap().is_empty());
    assert_eq!(vault.file_uses(b).unwrap().len(), 1);

    // An empty list keeps none; and the rest of the item is untouched.
    call(&vault, "update_item", serde_json::json!({ "item_id": id, "files": [] }));
    let after = vault.item(item_id).unwrap();
    assert!(after.files.is_empty());
    assert_eq!(after.notes, "Loved it");
    assert!(vault.file_uses(b).unwrap().is_empty());
}

#[test]
fn an_id_that_is_not_a_file_this_shelf_may_hold_is_refused_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let shelf = shelf_id(&vault);
    let report = sealed(&vault, "labs.pdf", FileOrigin::Health);
    let old = sealed(&vault, "old.epub", FileOrigin::Files);
    vault.trash_files(&[old]).unwrap();

    let refused = |files: serde_json::Value| {
        call_err(
            &vault,
            "create_item",
            serde_json::json!({ "shelf_id": shelf, "title": "X", "files": files }),
        )
    };
    let err = refused(serde_json::json!(["dune.epub"]));
    assert!(err.contains("not a file id") && err.contains("find_files"), "{err}");
    let err = refused(serde_json::json!([FileId::new().to_string()]));
    assert!(err.contains("no file with id") && err.contains("find_files"), "{err}");
    let err = refused(serde_json::json!([report.to_string()]));
    assert!(err.contains("Health"), "{err}");
    let err = refused(serde_json::json!([old.to_string()]));
    assert!(err.contains("trash"), "{err}");
    let many: Vec<String> = (0..21)
        .map(|n| sealed(&vault, &format!("f{n}.epub"), FileOrigin::Files).to_string())
        .collect();
    let err = refused(serde_json::json!(many));
    assert!(err.contains("21 files") && err.contains("at most 20"), "{err}");

    // Nothing was saved by any of them.
    assert_eq!(call(&vault, "list_items", serde_json::json!({}))["count"], 0);
}

#[test]
fn a_file_that_is_gone_or_healths_is_not_shown_on_an_item_that_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let books = book_shelf(&vault);
    let kept = sealed(&vault, "kept.epub", FileOrigin::Files);
    let binned = sealed(&vault, "binned.epub", FileOrigin::Files);

    let mut item = Item::new(books.id, "Dune");
    item.files = vec![FileId::new(), binned, kept];
    item.source = jatayu_core::library::FILE_SOURCE.to_string();
    vault.save_item(&item).unwrap();
    vault.trash_files(&[binned]).unwrap();

    let shown = call(&vault, "get_item", serde_json::json!({ "item_id": item.id.to_string() }));
    let files = shown["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{shown}");
    assert_eq!(files[0]["name"], "kept.epub");
    assert_eq!(shown["filled_from"], "the file it was made from");

    // With nothing left to show, there is no `files` at all.
    item.files = vec![binned];
    vault.save_item(&item).unwrap();
    let shown = call(&vault, "get_item", serde_json::json!({ "item_id": item.id.to_string() }));
    assert!(shown.get("files").is_none() && shown.get("has_files").is_none(), "{shown}");
}
