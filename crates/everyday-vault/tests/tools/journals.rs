//! Journals and entries, through the tools a model actually has.

use super::support::{call, call_err, vault};

#[test]
fn an_entry_round_trips_as_markdown_because_that_is_what_a_model_writes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    let created = call(
        &vault,
        "create_entry",
        serde_json::json!({
            "journal_id": journal.id.to_string(),
            "title": "Deck, day one",
            "body": "Cut the joists.\n\nRan out of screws.",
            "date": "2026-09-07",
            "tags": ["deck"],
        }),
    );
    let id = created["id"].as_str().unwrap().to_string();

    let back = call(&vault, "get_entry", serde_json::json!({ "entry_id": id }));
    assert_eq!(back["title"], "Deck, day one");
    assert!(back["body"].as_str().unwrap().contains("Cut the joists"));
    assert!(back["body"].as_str().unwrap().contains("Ran out of screws"));
    assert_eq!(back["date"], "2026-09-07", "back-dating must work");

    // And it is findable by the search tool, which is how a model gets
    // an id in the first place.
    let hits = call(&vault, "search", serde_json::json!({ "query": "joists" }));
    assert_eq!(hits["count"], 1, "got {hits}");
    assert_eq!(hits["results"][0]["id"], id);
}

#[test]
fn an_edit_moves_the_record_forward_so_an_open_editor_notices() {
    // Every writer stamps its own `updated_at`; the vault does not do it.
    // Without the stamp the entry's version never moves, so an editor
    // still holding the old one saves over the assistant's work and
    // `put_entry_if` reports no conflict -- the exact loss it exists for.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    let entry_id = call(
        &vault,
        "create_entry",
        serde_json::json!({ "journal_id": journal.id.to_string(), "body": "First draft." }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let eid: everyday_core::EntryId = entry_id.parse().unwrap();
    let before = vault.entry(eid).unwrap().updated_at;

    call(
        &vault,
        "update_entry",
        serde_json::json!({ "entry_id": entry_id, "title": "Second draft" }),
    );
    let after = vault.entry(eid).unwrap().updated_at;
    assert!(after > before, "the entry's version must move");

    // And a stale writer is now told, rather than winning silently.
    let mut stale = vault.entry(eid).unwrap();
    stale.title = "From an old tab".into();
    let err = vault.save_entry(&stale, Some(before)).unwrap_err();
    assert_eq!(err.code(), "conflict", "got {err}");

    // Tasks and items carry the same stamp.
    let tid = call(&vault, "create_task", serde_json::json!({ "title": "x" }))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let task_id: everyday_core::TaskId = tid.parse().unwrap();
    let before = vault.task(task_id).unwrap().updated_at;
    call(&vault, "update_task", serde_json::json!({ "task_id": tid, "notes": "detail" }));
    assert!(vault.task(task_id).unwrap().updated_at > before);
}

#[test]
fn an_entry_written_in_markdown_is_stored_as_structure_rather_than_as_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    // The bullets and the tick-boxes are two lists, not one, so the
    // canonical form has a blank line between them -- which is what a
    // round trip has to agree on.
    let body = "## What is left\n\n- Sand the rails\n\n- [x] Ring the yard\n\nSee **the plan**.";
    let id = call(
        &vault,
        "create_entry",
        serde_json::json!({ "journal_id": journal.id.to_string(), "body": body }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let eid: everyday_core::EntryId = id.parse().unwrap();

    let stored = vault.entry(eid).unwrap();
    let blocks = stored.body.0["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "heading", "got {blocks:?}");
    assert_eq!(blocks[1]["type"], "bulletList");
    assert_eq!(blocks[2]["type"], "taskList");
    assert!(!stored.body.plain_text().contains('#'), "the hashes must not survive as literal text");

    // And what the assistant reads back is what it wrote, so an edit to
    // one line does not flatten the rest.
    assert_eq!(call(&vault, "get_entry", serde_json::json!({ "entry_id": id }))["body"], body);
}

#[test]
fn an_entry_holding_photographs_will_not_have_its_text_replaced() {
    // A body arrives whole. Replacing one that holds media takes the
    // media off the page, and the model -- handed the body as Markdown --
    // has no way to know it did that or to put it back.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    let blob = vault.put_blob(b"not really a photograph").unwrap();
    let mut entry = everyday_core::Entry::new(journal.id, "UTC");
    entry.body = everyday_core::RichDoc(serde_json::json!({
        "type": "doc",
        "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "The deck" }] },
            { "type": "media", "attrs": { "blob": blob.to_string(), "kind": "image" } },
        ],
    }));
    vault.save_entry(&entry, None).unwrap();

    let err = call_err(
        &vault,
        "update_entry",
        serde_json::json!({ "entry_id": entry.id.to_string(), "body": "Rewritten." }),
    );
    assert!(err.contains("photographs"), "got {err}");

    // Everything else about it is still editable.
    call(
        &vault,
        "update_entry",
        serde_json::json!({ "entry_id": entry.id.to_string(), "tags": ["deck"] }),
    );
    let after = vault.entry(entry.id).unwrap();
    assert_eq!(after.tags, vec!["deck"]);
    assert_eq!(after.body.blob_refs().len(), 1, "the photograph is still on the page");
}

#[test]
fn a_note_and_an_entry_are_found_by_the_same_search() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();
    let journal_id = journal.id.to_string();

    call(
        &vault,
        "create_entry",
        serde_json::json!({ "journal_id": journal_id, "body": "Sailed to the island." }),
    );
    call(&vault, "create_note", serde_json::json!({ "body": "Sailed rig notes." }));

    let hits = call(&vault, "search", serde_json::json!({ "query": "sailed" }));
    assert_eq!(hits["count"], 2, "one index over both, so both are found");
    let kinds: Vec<&str> =
        hits["results"].as_array().unwrap().iter().map(|h| h["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"entry") && kinds.contains(&"note"));

    // Naming a journal narrows to entries, because a note is in no
    // journal and returning one anyway would answer a different question.
    let narrowed =
        call(&vault, "search", serde_json::json!({ "query": "sailed", "journal_id": journal_id }));
    assert_eq!(narrowed["count"], 1);
    assert_eq!(narrowed["results"][0]["kind"], "entry");
}
