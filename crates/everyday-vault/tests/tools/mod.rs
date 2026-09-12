//! The tests that are not any one domain's: the confirmation gate, the
//! read-only rule, and an unknown tool name. Each domain's own tools are
//! exercised in the sibling files, in the same grouping
//! `everyday_core::agent::tools` uses.

#[path = "../support/mod.rs"]
mod support;

mod journals;
mod library;
mod memory;
mod notes;
mod purpose;
mod tasks;
mod time;
mod trackers;

use everyday_core::agent::tools::{self, Effect};
use support::{call, call_err, ctx, vault};

#[test]
fn a_tool_that_names_a_record_that_does_not_exist_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let err = call_err(
        &vault,
        "update_task",
        serde_json::json!({ "task_id": everyday_core::TaskId::new().to_string(), "status": "done" }),
    );
    assert!(err.contains("not found") || err.contains("task"), "got {err}");

    // And an invented tool name comes back with the real ones, which is
    // what a model needs in order to recover on the next turn.
    let err = call_err(&vault, "add_task", serde_json::json!({ "title": "x" }));
    assert!(err.contains("no tool called"), "got {err}");
    assert!(err.contains("create_task"), "should list the real names: {err}");
}

#[test]
fn a_confirmation_names_what_it_will_destroy_rather_than_its_id() {
    // The gate exists so somebody can catch a misreading, and it can only
    // do that if the card says what the thing is. Every destructive tool
    // takes an id and nothing else, so the name has to be read back out
    // of the vault before the question is asked -- the tools themselves
    // read it, but a moment too late to be asked about.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();

    let project = call(&vault, "create_project", serde_json::json!({ "name": "The deck" }));
    let task = call(
        &vault,
        "create_task",
        serde_json::json!({ "title": "Order the timber", "project_id": project["id"] }),
    );
    let shelf = call(&vault, "list_shelves", serde_json::json!({}))[0]["id"].clone();
    let item =
        call(&vault, "create_item", serde_json::json!({ "shelf_id": shelf, "title": "Piranesi" }));
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();
    let entry = call(
        &vault,
        "create_entry",
        serde_json::json!({ "journal_id": journal.id.to_string(), "body": "Cut the joists." }),
    );
    let memory = call(&vault, "remember", serde_json::json!({ "fact": "Plans on Sundays" }));
    call(
        &vault,
        "create_time_block",
        serde_json::json!({
            "date": "2026-09-09",
            "start_time": "09:00",
            "end_time": "10:00",
            "task_id": task["id"],
        }),
    );
    let block = call(
        &vault,
        "list_time_blocks",
        serde_json::json!({ "from": "2026-09-09", "to": "2026-09-09" }),
    )["blocks"][0]["id"]
        .clone();

    let cases = [
        ("delete_project", "project_id", project["id"].clone(), "The deck"),
        ("delete_task", "task_id", task["id"].clone(), "Order the timber"),
        ("delete_item", "item_id", item["id"].clone(), "Piranesi"),
        ("delete_entry", "entry_id", entry["id"].clone(), "Cut the joists."),
        ("forget", "memory_id", memory["id"].clone(), "Plans on Sundays"),
        ("delete_time_block", "block_id", block, "Order the timber on 2026-09-09"),
    ];

    for (tool, key, id, expected) in cases {
        let args = serde_json::json!({ key: id });
        let described = tools::describe(&ctx(&vault), tool, &args)
            .unwrap_or_else(|| panic!("{tool} described nothing"));
        assert_eq!(described, expected, "{tool} should name what it will destroy");
        assert!(
            !described.contains('-')
                || !described.chars().any(|c| c.is_ascii_hexdigit())
                || described == expected,
            "{tool} must not fall back to an id: {described}"
        );
    }
}

#[test]
fn a_confirmation_for_something_that_is_gone_says_nothing_rather_than_guessing() {
    // A card with no subject asks somebody to think about the tool name.
    // One holding a stale id asks them to believe it.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let args = serde_json::json!({ "task_id": everyday_core::TaskId::new().to_string() });
    assert_eq!(tools::describe(&ctx(&vault), "delete_task", &args), None);

    // And a tool that destroys nothing has nothing to describe.
    assert_eq!(tools::describe(&ctx(&vault), "list_tasks", &serde_json::json!({})), None);
}

#[test]
fn the_catalogue_marks_exactly_the_tools_the_confirmation_gate_must_catch() {
    // The gate reads `Effect`, so this is the list that decides what a
    // person gets asked about. Worth asserting by name rather than by
    // count, so adding a destructive tool is a deliberate edit here.
    let destructive: Vec<&str> = tools::catalog()
        .iter()
        .filter(|t| t.effect == Effect::Destructive)
        .map(|t| t.name)
        .collect();
    assert_eq!(
        destructive,
        vec![
            "delete_entry",
            "delete_note",
            "delete_project",
            "delete_task",
            "delete_time_block",
            "delete_item",
            "delete_goal",
            "delete_routine",
            "forget",
        ]
    );
}

#[test]
fn a_read_only_vault_refuses_every_write_and_allows_every_read() {
    // Two processes on one vault: the second opens read-only. The
    // assistant must say so rather than failing somewhere deeper with a
    // message about lock files.
    let dir = tempfile::tempdir().unwrap();
    let first = vault(dir.path());
    first.save_journal(&everyday_core::Journal::new("Daily")).unwrap();

    let second = everyday_vault::open(dir.path()).unwrap();
    second.unlock(None).unwrap();
    assert!(!second.is_writable(), "the second open should be read-only");

    // Reads still work.
    assert_eq!(call(&second, "list_journals", serde_json::json!({}))[0]["name"], "Daily");

    let err = call_err(&second, "create_task", serde_json::json!({ "title": "x" }));
    assert!(err.contains("read-only"), "got {err}");
}
