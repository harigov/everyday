//! `create_item_from_file`, end to end against a real vault: who may ask, which
//! files are refused before anything is read, and what a document that is
//! read becomes on a shelf.
//!
//! Nothing here touches the network: a document with no DOI looks nothing up,
//! and what is looked up for one that has is tested where it is parsed
//! (`karmayoga_core::websearch`) and applied (`karmayoga_core::library`).

use karmayoga_core::files::{FileOrigin, ImportOptions};
use karmayoga_core::{FileId, Vault};
use karmayoga_service::Service;
use karmayoga_service::ctx::{Ctx, Scope};
use karmayoga_service::error::{CommandError, codes};
use serde_json::{Value, json};
use std::sync::Arc;

#[allow(dead_code)]
mod support;

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, ctx: Ctx, args: Value) -> CommandError {
    svc.call(ctx, "create_item_from_file", args).await.expect_err("expected a failure")
}

fn sealed(vault: &Vault, name: &str, bytes: &[u8], origin: FileOrigin) -> FileId {
    let opts = ImportOptions { encrypted: Some(true), ..ImportOptions::new(origin) };
    vault.import_bytes(bytes, name, &opts).unwrap().id
}

#[tokio::test]
async fn a_connection_that_may_reach_the_library_and_not_the_files_may_not_ask() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.require().unwrap();
    let file = sealed(&vault, "notes.md", b"# Notes", FileOrigin::Files);

    // The command is the library's; reading a document is the files'. Both,
    // or a connection that could keep a shelf could read any file by naming it.
    let library_only = Ctx { scopes: vec![Scope::Library], ..Ctx::local() };
    let err = fails(&svc, library_only, json!({ "file": file.to_string() })).await;
    assert_eq!(err.code, codes::FORBIDDEN, "{}", err.message);

    let files_only = Ctx { scopes: vec![Scope::Files], ..Ctx::local() };
    let err = fails(&svc, files_only, json!({ "file": file.to_string() })).await;
    assert_eq!(err.code, codes::FORBIDDEN, "no library scope, no command: {}", err.message);
}

#[tokio::test]
async fn a_health_file_a_picture_and_a_file_that_is_not_there_are_refused_before_any_read() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.require().unwrap();
    let labs = sealed(&vault, "labs.pdf", b"%PDF-1.4 labs", FileOrigin::Health);
    let boat = sealed(&vault, "boat.png", b"\x89PNG\r\n\x1a\nnot really", FileOrigin::Files);
    let old = sealed(&vault, "old.md", b"# Old", FileOrigin::Files);
    vault.trash_files(&[old]).unwrap();

    for (file, code) in [
        (labs, codes::FORBIDDEN),
        (boat, codes::UNSUPPORTED),
        (old, codes::INVALID),
        (FileId::new(), codes::NOT_FOUND),
    ] {
        let err = fails(&svc, Ctx::local(), json!({ "file": file.to_string() })).await;
        assert_eq!(err.code, code, "{file}: {}", err.message);
    }
    // Health's, whoever asks and whatever else they may reach.
    let err = fails(&svc, Ctx::local(), json!({ "file": labs.to_string() })).await;
    assert!(err.message.contains("Health"), "{}", err.message);

    // Nothing was made.
    vault.seed_library().unwrap();
    let kinds = call(&svc, "list_kinds", json!({})).await;
    assert!(kinds.as_array().unwrap().iter().all(|k| k["items"] == 0), "{kinds}");
}

#[tokio::test]
async fn a_document_becomes_one_item_on_books_and_pressed_again_is_the_same_item() {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.require().unwrap();
    vault.seed_library().unwrap();
    let folder = dir.path().join("files");
    assert!(vault.set_up_files_folder(Some(&folder)).unwrap().is_available());
    let opts = ImportOptions::new(FileOrigin::Files);
    let file = vault
        .import_bytes(b"# Chapter One\n\nIt was a dark night.\n", "A Dark Night.md", &opts)
        .unwrap();
    let kinds = vault.kinds().unwrap();
    let shelf = |slug: &str| kinds.iter().find(|k| k.slug == slug).unwrap().id.to_string();

    let made = call(&svc, "create_item_from_file", json!({ "file": file.id.to_string() })).await;
    assert_eq!(made["kindId"], shelf("book"), "anything that is not a paper is a book");
    assert_eq!(made["files"], json!([file.id.to_string()]));
    assert_eq!(made["status"], "wishlist");
    assert_eq!(made["source"], "file", "somebody else's words, and marked so");
    assert!(!made["title"].as_str().unwrap().is_empty());
    assert!(made.get("progress").is_none());

    // Saved through the vault: the item is on the shelf and the file is used.
    let id = made["id"].as_str().unwrap().to_string();
    let uses = vault.file_uses(file.id).unwrap();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].owner.id, id);

    // Pressed again, it is the item that was made, unchanged.
    let again = call(&svc, "create_item_from_file", json!({ "file": file.id.to_string() })).await;
    assert_eq!(again["id"], made["id"]);
    assert_eq!(again["updatedAt"], made["updatedAt"], "not so much as touched");
    let on_shelf = call(&svc, "list_items", json!({ "query": { "kindId": shelf("book") } })).await;
    assert_eq!(on_shelf.as_array().unwrap().len(), 1);

    // The same file on another shelf is another item's to make, when asked
    // for; and it is the same file under both.
    let on_articles = call(
        &svc,
        "create_item_from_file",
        json!({ "file": file.id.to_string(), "kindId": shelf("article") }),
    )
    .await;
    assert_ne!(on_articles["id"], made["id"]);
    assert_eq!(on_articles["kindId"], shelf("article"));
    assert_eq!(vault.file_uses(file.id).unwrap().len(), 2);
}
