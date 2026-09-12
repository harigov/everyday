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

#[allow(dead_code)]
mod support;

/// A vault in a temporary directory, with a service around it.
fn service() -> (Arc<Service>, tempfile::TempDir) {
    with_password(None)
}

/// The same, with a password on it, for the tests about locking.
fn with_password(password: Option<&str>) -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(password)
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
async fn a_narrow_token_reaches_the_tools_of_its_own_domain_and_no_others() {
    // The arrangement an MCP token depends on, and the one that is easy to
    // break from a distance: `run_tool`'s row declares `Scope::Any`, so the
    // command table lets this caller through and the handler is what refuses
    // it. A row that named a domain instead -- which is what it used to do --
    // turned this whole test into one `forbidden` on the first call.
    let (svc, _dir) = service();
    let tasks_only = Ctx {
        caller: Caller::Device("agent".into()),
        scopes: vec![Scope::Tasks],
        proved_at: None,
        request_id: None,
    };

    // A tool in the domain it holds: reached, and actually run.
    let made = svc
        .call(
            tasks_only.clone(),
            "run_tool",
            json!({ "name": "create_task", "arguments": { "title": "Buy milk" } }),
        )
        .await
        .expect("a Tasks token must be able to run a Tasks tool");
    assert_eq!(made["ok"], true);
    assert_eq!(made["kind"], "task");
    assert_eq!(made["name"], "Buy milk");

    // A tool in a domain it does not hold: refused, and refused by scope
    // rather than by anything that would say whether the tool exists here.
    let e = svc
        .call(
            tasks_only.clone(),
            "run_tool",
            json!({ "name": "create_note", "arguments": { "title": "x" } }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert!(e.message.contains("notes"), "{}", e.message);

    // And the catalogue it is shown matches what it may actually call, so a
    // model is never offered a tool it will be refused.
    let listed = svc.call(tasks_only, "list_tools", json!({})).await.expect("list_tools");
    let names: Vec<&str> =
        listed.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"create_task"), "a Tasks tool belongs in the list: {names:?}");
    assert!(!names.iter().any(|n| n.contains("note")), "no Notes tool may appear: {names:?}");
    assert!(names.iter().all(|n| !n.contains("journal") && !n.contains("entry")), "{names:?}");
}

#[tokio::test]
async fn a_caller_holding_nothing_at_all_is_not_somebody() {
    // `Scope::Any` asks whether the caller is anybody, and an empty scope
    // list is the one answer that must be no. Getting this wrong would make
    // the two tool commands reachable by a token that holds nothing.
    let (svc, _dir) = service();
    let nobody = Ctx {
        caller: Caller::Device("x".into()),
        scopes: vec![],
        proved_at: None,
        request_id: None,
    };
    assert_eq!(
        svc.call(nobody.clone(), "list_tools", json!({})).await.unwrap_err().code,
        "forbidden"
    );
    let e = svc.call(nobody, "run_tool", json!({ "name": "create_task", "arguments": {} })).await;
    assert_eq!(e.unwrap_err().code, "forbidden");
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
    assert_eq!(save_entry["changes"], json!({ "kind": "entry", "op": "updated" }));
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

// ── two locks ───────────────────────────────────────────────────────────
//
// A screen and a key are different things, and the commands that end them
// are different commands. These check the seam rather than the crypto, which
// the core covers.

#[tokio::test]
async fn a_password_can_be_checked_without_unlocking_anything() {
    let (svc, _dir) = with_password(Some("correct horse"));
    let vault = svc.get().unwrap();

    call(&svc, "verify_password", json!({ "password": "correct horse" })).await;
    assert!(vault.is_unlocked(), "checking a password must not close an open vault");

    let err = fails(&svc, "verify_password", json!({ "password": "wrong" })).await;
    assert_eq!(err.code, "bad_password");
    assert!(vault.is_unlocked(), "a wrong guess must not close it either");

    // And from behind the lock screen the vault stays shut, which is the
    // whole point: proving who you are is not the same as opening anything.
    call(&svc, "lock", json!({})).await;
    call(&svc, "verify_password", json!({ "password": "correct horse" })).await;
    assert!(!vault.is_unlocked(), "checking a password must not open a locked vault");
}

#[tokio::test]
async fn checking_a_password_announces_no_change() {
    let (svc, _dir) = with_password(Some("correct horse"));
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    call(&svc, "verify_password", json!({ "password": "correct horse" })).await;
    assert!(
        events.changes.lock().unwrap().is_empty(),
        "nothing was written, so no client has anything to reload"
    );
}

#[tokio::test]
async fn a_person_defers_the_key_timeout_and_the_assistant_does_not() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    call(&svc, "set_forget_key", json!({ "seconds": 1 })).await;
    vault.touch();

    // The assistant reads this vault every minute of every day. If that
    // counted as somebody being here, a vault with one routine on it would
    // never let go of its key whatever the timeout said.
    let robot = Ctx { caller: Caller::Assistant("run".into()), ..Ctx::local() };
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(600));
        svc.call(robot.clone(), "list_journals", json!({})).await.unwrap();
    }
    assert!(vault.forget_key_if_idle(), "the assistant is not a person");

    // A window is.
    call(&svc, "unlock", json!({ "password": "" })).await;
    call(&svc, "set_forget_key", json!({ "seconds": 1 })).await;
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(600));
        call(&svc, "list_journals", json!({})).await;
    }
    assert!(!vault.forget_key_if_idle(), "somebody is plainly still here");
}

#[tokio::test]
async fn the_poll_that_checks_the_key_timeout_does_not_reset_it() {
    // The window asks this every five seconds to find out whether the vault
    // has been idle long enough to give up its key. A poll that deferred the
    // timeout on its way to reading it would reset the clock it was about to
    // check, and the timeout would never fire while any window was open.
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    call(&svc, "set_forget_key", json!({ "seconds": 1 })).await;
    vault.touch();

    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(400));
        // Twice a second, as a window does.
        let locked = call(&svc, "poll_auto_lock", json!({})).await;
        if locked == serde_json::Value::Bool(true) {
            assert!(!vault.is_unlocked(), "it said it locked, so it must have");
            return;
        }
    }
    panic!("polling for the timeout kept the timeout from ever firing");
}

#[tokio::test]
async fn the_assistant_stamps_its_writes_with_its_own_name() {
    let (svc, _dir) = service();
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let journal = call(&svc, "new_journal", json!({ "name": "Made by the assistant" })).await;
    let ctx = Ctx { caller: Caller::Assistant("run-1".into()), ..Ctx::local() };
    svc.call(ctx, "save_journal", json!({ "journal": journal })).await.unwrap();

    let changes = events.changes.lock().unwrap();
    let change = changes.first().expect("a write announces itself");
    assert_eq!(
        change.origin.as_deref(),
        Some("assistant"),
        "not `local`: a window drops its own origin, and would drop this with it"
    );
}

#[tokio::test]
async fn searching_is_narrowed_by_what_the_caller_may_read() {
    // One command over two domains, so the scope check is in the body rather
    // than on the entry. Left at `Journals` it meant a device paired to read
    // notes could not search them, and one paired to read journals could read
    // note bodies -- the exact thing `Scope::Notes` was added to prevent.
    let (svc, _dir) = service();
    let journal =
        call(&svc, "list_journals", json!({})).await[0]["id"].as_str().unwrap().to_string();
    let entry = call(&svc, "new_entry", json!({ "journalId": journal })).await;
    let mut entry = entry;
    entry["title"] = json!("Sailing to the island");
    call(&svc, "save_entry", json!({ "entry": entry })).await;

    let note = call(&svc, "new_note", json!({})).await;
    let mut note = note;
    note["title"] = json!("Sailing rig notes");
    call(&svc, "save_note", json!({ "note": note })).await;

    let query = json!({ "query": "sailing", "limit": 20 });
    let kinds = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|h| h["type"].as_str().unwrap().to_string()).collect()
    };

    // Everything, for a caller that holds everything.
    let all = call(&svc, "search", query.clone()).await;
    assert_eq!(kinds(&all).len(), 2, "one index over both");

    // Journals only: narrowed rather than refused, so it still gets its own.
    let only_journals = Ctx {
        caller: Caller::Device("phone".into()),
        scopes: vec![Scope::Journals],
        ..Ctx::local()
    };
    let hits = svc.call(only_journals, "search", query.clone()).await.expect("entries");
    assert_eq!(kinds(&hits), vec!["entry"], "and no note bodies");

    // Notes only: the browser-extension case, which used to be refused.
    let only_notes =
        Ctx { caller: Caller::Device("clip".into()), scopes: vec![Scope::Notes], ..Ctx::local() };
    let hits = svc.call(only_notes, "search", query.clone()).await.expect("notes");
    assert_eq!(kinds(&hits), vec!["note"]);

    // Asking for the kind it may not read is refused rather than narrowed.
    let only_notes =
        Ctx { caller: Caller::Device("clip".into()), scopes: vec![Scope::Notes], ..Ctx::local() };
    let err = svc
        .call(only_notes, "search", json!({ "query": "sailing", "kind": "entry", "limit": 20 }))
        .await
        .expect_err("asking for entries without the scope");
    assert_eq!(err.code, "forbidden");
}

#[tokio::test]
async fn a_hit_says_which_kind_it_is_in_the_spelling_a_client_reads() {
    // `rename_all` renames an enum's *variants*; the fields inside them need
    // `rename_all_fields`. Without it this went out as `journal_id` against an
    // interface reading `journalId`, and the journal's search list drew rows
    // with no colour and no date.
    let (svc, _dir) = service();
    let journal =
        call(&svc, "list_journals", json!({})).await[0]["id"].as_str().unwrap().to_string();
    let mut entry = call(&svc, "new_entry", json!({ "journalId": journal })).await;
    entry["title"] = json!("Herons");
    call(&svc, "save_entry", json!({ "entry": entry })).await;

    let hits = call(&svc, "search", json!({ "query": "herons", "limit": 5 })).await;
    let hit = &hits[0];
    assert_eq!(hit["type"], "entry");
    assert!(hit["journalId"].is_string(), "camelCase on the wire: got {hit}");
    assert!(hit["localDate"].is_string(), "got {hit}");
}
