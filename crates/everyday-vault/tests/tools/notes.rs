//! Notes, through the tools a model actually has.

use super::support::{call, vault};

#[test]
fn a_note_written_by_the_assistant_reads_back_as_markdown() {
    // The round trip that matters for a proactive assistant: it writes
    // prose, a person opens it in the editor, and the assistant can read
    // its own words back later without them having been mangled.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let created = call(
        &vault,
        "create_note",
        serde_json::json!({
            "title": "Monday",
            "body": "## What is due\n\n- Order the timber\n- Ring the surveyor",
            "tags": ["planning"],
        }),
    );
    assert_eq!(created["action"], "created");
    assert_eq!(created["kind"], "note");
    assert_eq!(created["name"], "Monday");
    let id = created["id"].as_str().unwrap().to_string();

    let listed = call(&vault, "list_notes", serde_json::json!({}));
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["notes"][0]["title"], "Monday");
    assert_eq!(listed["notes"][0]["tags"][0], "planning");
    assert!(
        listed["notes"][0].get("body").is_none(),
        "a list must not carry bodies; that is what get_note is for"
    );

    let got = call(&vault, "get_note", serde_json::json!({ "note_id": id }));
    let body = got["body"].as_str().unwrap();
    assert!(body.contains("What is due"), "the heading must survive: {body}");
    assert!(body.contains("Order the timber"), "and the list: {body}");

    // Found by search, beside entries rather than in a separate index.
    let hits = call(&vault, "search", serde_json::json!({ "query": "surveyor" }));
    assert_eq!(hits["count"], 1);
    assert_eq!(hits["results"][0]["kind"], "note", "a hit says which kind it is");
    assert_eq!(hits["results"][0]["id"], id);
}

#[test]
fn updating_a_note_replaces_what_it_was_given_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let id = call(
        &vault,
        "create_note",
        serde_json::json!({ "title": "Shopping", "body": "oat milk", "tags": ["food"] }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Only the body named, so the title and the tags must stay.
    call(&vault, "update_note", serde_json::json!({ "note_id": id, "body": "oat milk\ncoffee" }));
    let got = call(&vault, "get_note", serde_json::json!({ "note_id": id }));
    assert_eq!(got["title"], "Shopping");
    assert_eq!(got["tags"][0], "food");
    assert!(got["body"].as_str().unwrap().contains("coffee"));

    // And deleting says what went, by name rather than by id.
    let gone = call(&vault, "delete_note", serde_json::json!({ "note_id": id }));
    assert_eq!(gone["name"], "Shopping");
    assert_eq!(call(&vault, "list_notes", serde_json::json!({}))["count"], 0);
}
