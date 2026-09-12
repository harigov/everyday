//! Export and import over the command surface, the way a window does it.
//!
//! The formats have their own round-trip test in `everyday-transfer`. What is
//! here is the protocol between a client and a vault: the handle, the chunks,
//! the dry run, and the two rules that are about safety rather than about
//! files -- that a locked vault gives nothing up, and that an archive held in
//! memory goes when the key does.

use everyday_service::ctx::{Caller, Ctx, Scope};
use everyday_service::{Service, error::CommandError};
use serde_json::{Value, json};
use std::sync::Arc;

#[allow(dead_code)]
mod support;

/// The shared fixture, plus one entry -- these tests are about moving a
/// vault's contents over the wire, so unlike `tests/call.rs` they need
/// something in it to move.
fn service(password: Option<&str>) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(password);
    let vault = svc.get().unwrap();
    let journal = vault.journals().unwrap().into_iter().next().unwrap();

    let mut entry = everyday_core::model::Entry::new(journal.id, "Europe/London");
    entry.title = "Morning pages".into();
    entry.body = everyday_core::richtext::RichDoc::from_markdown("It **rained**.\n");
    vault.save_entry(&entry, None).unwrap();

    (svc, dir)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Result<Value, CommandError> {
    svc.call(Ctx::local(), name, args).await
}

/// Pull an archive the way a browser does: a handle, then chunks.
async fn fetch(svc: &Arc<Service>, handle: &str, total: u64) -> Vec<u8> {
    use base64::Engine;
    let mut out = Vec::new();
    let mut offset = 0u64;
    while offset < total {
        let chunk =
            call(svc, "read_export", json!({ "handle": handle, "offset": offset })).await.unwrap();
        let data = chunk["data"].as_str().unwrap();
        out.extend_from_slice(&base64::engine::general_purpose::STANDARD.decode(data).unwrap());
        offset = chunk["offset"].as_u64().unwrap();
        if chunk["done"].as_bool().unwrap() {
            break;
        }
    }
    out
}

/// Push one the way a browser does.
async fn upload(svc: &Arc<Service>, bytes: &[u8]) -> String {
    use base64::Engine;
    let started =
        call(svc, "start_import", json!({ "name": "in.zip", "bytes": bytes.len() })).await.unwrap();
    let handle = started["handle"].as_str().unwrap().to_string();
    let chunk = started["chunk"].as_u64().unwrap() as usize;
    for offset in (0..bytes.len()).step_by(chunk) {
        let end = (offset + chunk).min(bytes.len());
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end]);
        call(svc, "write_import", json!({ "handle": handle, "offset": offset, "data": data }))
            .await
            .unwrap();
    }
    handle
}

#[tokio::test]
async fn a_window_exports_and_imports_over_the_wire() {
    let (svc, _dir) = service(None);

    let parts = call(&svc, "list_parts", json!({})).await.unwrap();
    let ids: Vec<&str> =
        parts.as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"journal"), "{ids:?}");

    let started = call(&svc, "start_export", json!({ "parts": [], "media": true })).await.unwrap();
    let handle = started["handle"].as_str().unwrap().to_string();
    let total = started["bytes"].as_u64().unwrap();
    assert!(total > 0);
    assert!(started["name"].as_str().unwrap().ends_with(".zip"));

    let bytes = fetch(&svc, &handle, total).await;
    assert_eq!(bytes.len() as u64, total);

    // Reading to the end releases it, so a window that finished has nothing
    // left resident.
    assert!(svc.transfers().is_empty(), "the archive outlived its download");
    assert_eq!(
        call(&svc, "read_export", json!({ "handle": handle, "offset": 0 })).await.unwrap_err().code,
        "not_found"
    );

    // Back in, into the same vault: everything is already here, so a `skip`
    // import must change nothing at all.
    let handle = upload(&svc, &bytes).await;
    let manifest = call(&svc, "read_import", json!({ "handle": handle })).await.unwrap();
    assert!(!manifest["parts"].as_array().unwrap().is_empty());

    let parts: Vec<String> = manifest["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["imports"].as_bool().unwrap_or(false))
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    let result =
        call(&svc, "run_import", json!({ "handle": handle, "parts": parts, "mode": "skip" }))
            .await
            .unwrap();
    assert_eq!(result["added"].as_u64().unwrap(), 0);
    assert_eq!(result["replaced"].as_u64().unwrap(), 0);
    assert!(result["skipped"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn an_import_says_which_lists_to_reload() {
    use everyday_service::events::{Change, EventSink, Kind};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collector(Mutex<Vec<Change>>);
    impl EventSink for Collector {
        fn changed(&self, change: Change) {
            self.0.lock().unwrap().push(change);
        }
    }

    let (svc, _dir) = service(None);
    let started =
        call(&svc, "start_export", json!({ "parts": ["journal"], "media": false })).await.unwrap();
    let bytes =
        fetch(&svc, started["handle"].as_str().unwrap(), started["bytes"].as_u64().unwrap()).await;

    let sink = Arc::new(Collector::default());
    svc.set_events(sink.clone());
    let handle = upload(&svc, &bytes).await;
    call(&svc, "run_import", json!({ "handle": handle, "parts": ["journal"], "mode": "replace" }))
        .await
        .unwrap();

    // One `change:` on the table entry would have named one kind and left
    // every other list stale, so the body emits one per kind it could have
    // moved. A journal import moves two.
    let kinds: Vec<Kind> = sink.0.lock().unwrap().iter().map(|c| c.kind).collect();
    assert!(kinds.contains(&Kind::Entry), "{kinds:?}");
    assert!(kinds.contains(&Kind::Journal), "{kinds:?}");
}

#[tokio::test]
async fn a_locked_vault_hands_nothing_over() {
    let (svc, _dir) = service(Some("correct horse battery"));
    svc.get().unwrap().lock();

    for (name, args) in [
        ("list_parts", json!({})),
        ("start_export", json!({ "parts": [], "media": false })),
        ("start_import", json!({ "name": "x.zip", "bytes": 10 })),
    ] {
        assert_eq!(
            call(&svc, name, args).await.unwrap_err().code,
            "locked",
            "{name} answered a locked vault"
        );
    }
}

#[tokio::test]
async fn locking_drops_an_export_that_was_still_being_saved() {
    let (svc, _dir) = service(Some("correct horse battery"));
    let started = call(&svc, "start_export", json!({ "parts": [], "media": true })).await.unwrap();
    let handle = started["handle"].as_str().unwrap().to_string();
    assert!(!svc.transfers().is_empty());

    // An archive is the one plaintext copy of a vault this process ever
    // holds. The lock screen must mean it holds none.
    call(&svc, "lock", json!({})).await.unwrap();
    assert!(svc.transfers().is_empty(), "a plaintext archive survived the lock");
    assert_eq!(
        call(&svc, "read_export", json!({ "handle": handle, "offset": 0 })).await.unwrap_err().code,
        "not_found"
    );
}

#[tokio::test]
async fn a_client_without_the_run_of_the_vault_cannot_export_it() {
    let (svc, _dir) = service(None);
    // A browser extension that clips pages into notes holds `Notes`. An
    // export is every app at once, so it is `All` or nothing -- otherwise the
    // weakest client in the system is a way to read the diary.
    let narrow = Ctx {
        caller: Caller::Device("ext".into()),
        scopes: vec![Scope::Notes],
        proved_at: None,
        request_id: None,
    };
    let e =
        svc.call(narrow, "start_export", json!({ "parts": [], "media": false })).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
}

#[tokio::test]
async fn an_import_that_is_not_an_archive_says_so_rather_than_half_running() {
    let (svc, _dir) = service(None);
    let handle = upload(&svc, b"this is a text file, not a zip").await;
    let e = call(&svc, "read_import", json!({ "handle": handle })).await.unwrap_err();
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("not a zip"), "{}", e.message);
}

#[tokio::test]
async fn an_upload_that_never_finished_is_not_read() {
    let (svc, _dir) = service(None);
    let started =
        call(&svc, "start_import", json!({ "name": "in.zip", "bytes": 4096 })).await.unwrap();
    let handle = started["handle"].as_str().unwrap();
    call(&svc, "write_import", json!({ "handle": handle, "offset": 0, "data": "AAAA" }))
        .await
        .unwrap();
    let e = call(&svc, "read_import", json!({ "handle": handle })).await.unwrap_err();
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("have arrived"), "{}", e.message);
}
