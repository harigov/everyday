//! The command layer, end to end, against a real vault.
//!
//! What was untestable while the commands were Tauri handlers: the dispatch
//! itself. These are about the layer rather than about any one domain -- the
//! domains have their behaviour checked in the core's conformance suite -- so
//! what is here is the things dispatch is responsible for and every command
//! body inherits: the scope check, the argument parsing, the change event, and
//! the answer to a retry.

use everyday_service::ctx::{Caller, Ctx, Scope};
use everyday_service::events::{Change, EventSink};
use everyday_service::{Service, error::CommandError};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// A vault in a temporary directory, with a service around it.
fn service() -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    // A vault with no journal is a dead end, and it is whoever creates one --
    // the shell, the `serve` command -- that gives it its first. See
    // `create_vault` in the shell.
    vault.save_journal(&everyday_core::Journal::new("Journal")).unwrap();
    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

async fn call(svc: &Arc<Service>, name: &str, args: serde_json::Value) -> serde_json::Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: serde_json::Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

#[tokio::test]
async fn a_journal_round_trips_through_the_command_layer() {
    let (svc, _dir) = service();

    // A vault is created with one journal in it.
    let journals = call(&svc, "list_journals", json!({})).await;
    assert_eq!(journals.as_array().unwrap().len(), 1);

    let minted = call(&svc, "new_journal", json!({ "name": "Field notes" })).await;
    call(&svc, "save_journal", json!({ "journal": minted })).await;

    let journals = call(&svc, "list_journals", json!({})).await;
    assert_eq!(journals.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_entry_saved_is_an_entry_read_back() {
    let (svc, _dir) = service();
    let journals = call(&svc, "list_journals", json!({})).await;
    let journal_id = journals[0]["id"].clone();

    let mut entry = call(&svc, "new_entry", json!({ "journalId": journal_id })).await;
    entry["title"] = json!("It rained all afternoon");
    call(&svc, "save_entry", json!({ "entry": entry, "expect": null })).await;

    let back = call(&svc, "get_entry", json!({ "id": entry["id"] })).await;
    assert_eq!(back["title"], "It rained all afternoon");
}

#[tokio::test]
async fn a_name_nobody_declared_is_refused_by_name() {
    let (svc, _dir) = service();
    let e = fails(&svc, "delete_everything", json!({})).await;
    assert_eq!(e.code, "unknown_command");
    assert!(e.message.contains("delete_everything"), "{}", e.message);
}

#[tokio::test]
async fn an_argument_of_the_wrong_shape_names_the_command() {
    let (svc, _dir) = service();
    let e = fails(&svc, "get_entry", json!({ "id": 42 })).await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("get_entry"), "{}", e.message);
}

#[tokio::test]
async fn a_streaming_command_cannot_be_called_for_a_value() {
    let (svc, _dir) = service();
    let e = fails(&svc, "send_message", json!({ "conversationId": "x", "prompt": "hello" })).await;
    assert_eq!(e.code, "unknown_command");
}

#[tokio::test]
async fn a_scope_the_caller_lacks_is_refused_before_the_body_runs() {
    let (svc, _dir) = service();
    let shelf_only = Ctx {
        caller: Caller::Device("phone".into()),
        scopes: vec![Scope::Library],
        proved_at: None,
        request_id: None,
    };
    let e = svc.call(shelf_only.clone(), "list_journals", json!({})).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert!(e.message.contains("journals"), "{}", e.message);

    // ...and the scope it does hold still works.
    assert!(svc.call(shelf_only, "list_kinds", json!({})).await.is_ok());
}

#[tokio::test]
async fn a_write_announces_what_it_touched_and_who_did_it() {
    let (svc, _dir) = service();
    let collector = Arc::new(Collector::default());
    svc.set_events(collector.clone());

    let minted = call(&svc, "new_journal", json!({ "name": "Ledger" })).await;
    call(&svc, "save_journal", json!({ "journal": minted })).await;

    let changes = collector.changes.lock().unwrap();
    let change = changes.last().expect("a save must announce itself");
    assert_eq!(serde_json::to_value(change.kind).unwrap(), "journal");
    assert_eq!(change.origin.as_deref(), Some("local"));
}

#[tokio::test]
async fn a_read_announces_nothing() {
    let (svc, _dir) = service();
    let collector = Arc::new(Collector::default());
    svc.set_events(collector.clone());

    call(&svc, "list_journals", json!({})).await;
    call(&svc, "list_tags", json!({})).await;

    assert!(collector.changes.lock().unwrap().is_empty());
}

/// The failure this whole mechanism exists for.
///
/// A save lands, its answer is lost to a dropped connection, and the client
/// retries. Without a request id the second attempt is refused as a conflict
/// against the first one's own write, and the editor offers "keep mine" for a
/// change that is already saved.
#[tokio::test]
async fn a_retried_save_is_answered_from_the_first_attempt() {
    let (svc, _dir) = service();
    let journals = call(&svc, "list_journals", json!({})).await;
    let journal_id = journals[0]["id"].clone();

    let mut entry = call(&svc, "new_entry", json!({ "journalId": journal_id })).await;
    entry["title"] = json!("first");
    call(&svc, "save_entry", json!({ "entry": entry.clone(), "expect": null })).await;

    // Load it as a client would, edit, and save with a request id. The client
    // stamps `updatedAt` itself -- see `state.svelte.ts` -- which is what makes
    // the second attempt's `expect` genuinely stale.
    let loaded = call(&svc, "get_entry", json!({ "id": entry["id"] })).await;
    let mut edited = loaded.clone();
    edited["title"] = json!("second");
    edited["updatedAt"] = json!(jiff::Timestamp::now().to_string());
    let args = json!({ "entry": edited, "expect": loaded["updatedAt"] });

    let ctx = Ctx {
        caller: Caller::Device("laptop".into()),
        scopes: vec![Scope::All],
        proved_at: None,
        request_id: Some("save-1".into()),
    };
    svc.call(ctx.clone(), "save_entry", args.clone()).await.expect("the first save");

    // The retry: the same id, the same arguments, and now a stale `expect`.
    svc.call(ctx, "save_entry", args).await.expect("a retry must not be a conflict");

    let back = call(&svc, "get_entry", json!({ "id": entry["id"] })).await;
    assert_eq!(back["title"], "second");
}

/// ...and the same save without a request id still conflicts, because that is
/// the check doing its job.
#[tokio::test]
async fn a_stale_save_without_a_request_id_is_still_a_conflict() {
    let (svc, _dir) = service();
    let journals = call(&svc, "list_journals", json!({})).await;
    let journal_id = journals[0]["id"].clone();

    let mut entry = call(&svc, "new_entry", json!({ "journalId": journal_id })).await;
    entry["title"] = json!("first");
    call(&svc, "save_entry", json!({ "entry": entry.clone(), "expect": null })).await;

    let loaded = call(&svc, "get_entry", json!({ "id": entry["id"] })).await;
    let mut edited = loaded.clone();
    edited["title"] = json!("second");
    edited["updatedAt"] = json!(jiff::Timestamp::now().to_string());
    let args = json!({ "entry": edited, "expect": loaded["updatedAt"] });

    call(&svc, "save_entry", args.clone()).await;
    let e = fails(&svc, "save_entry", args).await;
    assert_eq!(e.code, "conflict");
}

#[tokio::test]
async fn a_read_is_never_answered_from_the_retry_record() {
    // A repeated read must go to storage: answering it from a cache would
    // serve a stale list to whoever asked again, which is the opposite of what
    // they wanted.
    let (svc, _dir) = service();
    let ctx = Ctx {
        caller: Caller::Device("laptop".into()),
        scopes: vec![Scope::All],
        proved_at: None,
        request_id: Some("read-1".into()),
    };
    let before = svc.call(ctx.clone(), "list_journals", json!({})).await.unwrap();
    assert_eq!(before.as_array().unwrap().len(), 1);

    let minted = call(&svc, "new_journal", json!({ "name": "Second" })).await;
    call(&svc, "save_journal", json!({ "journal": minted })).await;

    let after = svc.call(ctx, "list_journals", json!({})).await.unwrap();
    assert_eq!(after.as_array().unwrap().len(), 2, "a read was served from the retry record");
}

#[tokio::test]
async fn the_catalogue_describes_itself() {
    let (svc, _dir) = service();
    let surface = call(&svc, "list_commands", json!({})).await;
    assert_eq!(surface["protocol"], everyday_service::PROTOCOL);

    let commands = surface["commands"].as_array().unwrap();
    let save_entry = commands.iter().find(|c| c["name"] == "save_entry").unwrap();
    assert_eq!(save_entry["scope"], "journals");
    assert_eq!(save_entry["effect"], "write");
    assert_eq!(save_entry["changes"], "entry");
    assert_eq!(save_entry["args"][0]["name"], "entry");
}

#[tokio::test]
async fn a_tool_can_be_run_without_a_model() {
    let (svc, _dir) = service();
    let tools = call(&svc, "list_tools", json!({})).await;
    assert!(!tools.as_array().unwrap().is_empty());

    let overview = call(&svc, "run_tool", json!({ "name": "overview" })).await;
    assert!(overview.is_object(), "{overview}");
}

#[tokio::test]
async fn a_destructive_tool_will_not_run_unasked() {
    let (svc, _dir) = service();
    let e = fails(&svc, "run_tool", json!({ "name": "delete_task", "arguments": {} })).await;
    // Either it wanted the confirmation or it does not exist on this build;
    // what must not happen is that it ran.
    assert!(matches!(e.code.as_str(), "confirm_required" | "unknown_tool" | "unsupported"), "{e}");
}

/// A request that is dropped mid-flight must not park its own retry.
///
/// What a client hanging up looks like from here: the command's future is
/// dropped rather than completed. Without the guard in `Service::call` the
/// slot stays claimed with no answer in it, and the retry -- which is the
/// whole reason a request id exists -- waits for ever on a command that will
/// never finish.
#[tokio::test]
async fn a_cancelled_request_does_not_park_its_retry() {
    let (svc, _dir) = service();
    let ctx = Ctx {
        caller: Caller::Device("flaky".into()),
        scopes: vec![Scope::All],
        proved_at: None,
        request_id: Some("save-1".into()),
    };

    let journals = call(&svc, "list_journals", json!({})).await;
    let minted = call(&svc, "new_journal", json!({ "name": "Held" })).await;
    let _ = journals;

    // Start a write and drop it before it can finish. `now_or_never` polls the
    // future exactly once and throws it away, which is precisely the shape of
    // a client that went away.
    {
        use futures::FutureExt;
        let call = svc.call(ctx.clone(), "save_journal", json!({ "journal": minted.clone() }));
        let _ = call.now_or_never();
    }

    // The retry must be answered -- either applied, or told to ask again --
    // rather than waiting for a future that no longer exists.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        svc.call(ctx, "save_journal", json!({ "journal": minted })),
    )
    .await
    .expect("the retry parked for ever on an abandoned slot");

    match outcome {
        Ok(_) => {}
        Err(e) => assert_eq!(e.code, "retry", "{e}"),
    }
}
