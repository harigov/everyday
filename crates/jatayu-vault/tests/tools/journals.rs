//! Journals and entries, through the tools a model actually has.

use super::support::{call, call_err, vault};

#[test]
fn an_entry_round_trips_as_markdown_because_that_is_what_a_model_writes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = jatayu_core::Journal::new("Daily");
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
    let journal = jatayu_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    let entry_id = call(
        &vault,
        "create_entry",
        serde_json::json!({ "journal_id": journal.id.to_string(), "body": "First draft." }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let eid: jatayu_core::EntryId = entry_id.parse().unwrap();
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
    let task_id: jatayu_core::TaskId = tid.parse().unwrap();
    let before = vault.task(task_id).unwrap().updated_at;
    call(&vault, "update_task", serde_json::json!({ "task_id": tid, "notes": "detail" }));
    assert!(vault.task(task_id).unwrap().updated_at > before);
}

#[test]
fn an_entry_written_in_markdown_is_stored_as_structure_rather_than_as_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = jatayu_core::Journal::new("Daily");
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
    let eid: jatayu_core::EntryId = id.parse().unwrap();

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
    let journal = jatayu_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    let blob = vault.put_blob(b"not really a photograph").unwrap();
    let mut entry = jatayu_core::Entry::new(journal.id, "UTC");
    entry.body = jatayu_core::RichDoc(serde_json::json!({
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
    let journal = jatayu_core::Journal::new("Daily");
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

#[test]
fn the_assistant_keeps_its_own_journal_one_page_a_day_apart_from_theirs() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let theirs = jatayu_core::Journal::new("Daily");
    vault.save_journal(&theirs).unwrap();

    // Nothing written yet: nothing to read, and no journal made for it.
    let none = call(&vault, "read_own_journal", serde_json::json!({}));
    assert_eq!(none["count"], 0);
    assert!(vault.assistant_journal().unwrap().is_none(), "made only when first written to");

    // A weekly plan leaves a note for a day ahead; a quarterly review adds one
    // to the same day. One page for the day, the second added to it.
    let first = call(
        &vault,
        "write_own_journal",
        serde_json::json!({
            "date": "2026-11-05",
            "heading": "From the weekly plan",
            "text": "Ask about the boiler quote before Friday.",
        }),
    );
    assert_eq!(first["action"], "created");
    let second = call(
        &vault,
        "write_own_journal",
        serde_json::json!({
            "date": "2026-11-05",
            "heading": "From the quarterly review",
            "text": "Budget review is due this month.",
        }),
    );
    assert_eq!(second["action"], "added to");
    assert_eq!(first["id"], second["id"], "one page a day");

    let mine = vault.assistant_journal().unwrap().expect("made by the first note");
    assert!(mine.assistant);
    assert_ne!(mine.id, theirs.id);
    assert!(!vault.journal(theirs.id).unwrap().assistant);

    // The run on that day reads both notes, in the order they were left.
    let page = call(&vault, "read_own_journal", serde_json::json!({ "from": "2026-11-05" }));
    assert_eq!(page["count"], 1, "{page}");
    let text = page["pages"][0]["text"].as_str().unwrap();
    let weekly = text.find("boiler quote").expect("the weekly note");
    let quarterly = text.find("Budget review").expect("the quarterly note");
    assert!(weekly < quarterly, "{text}");
    assert!(text.contains("From the weekly plan"), "{text}");

    // A day with nothing on it reads as nothing; a window reads every page in it.
    let empty = call(&vault, "read_own_journal", serde_json::json!({ "from": "2026-11-04" }));
    assert_eq!(empty["count"], 0);
    let week = call(
        &vault,
        "read_own_journal",
        serde_json::json!({ "from": "2026-11-01", "to": "2026-11-07" }),
    );
    assert_eq!(week["count"], 1);

    // Listed as its own, so it is not mistaken for theirs.
    let listed = call(&vault, "list_journals", serde_json::json!({}));
    let row =
        listed.as_array().unwrap().iter().find(|j| j["id"] == mine.id.to_string()).unwrap().clone();
    assert!(row["yours"].is_string(), "{row}");

    // A save from the interface cannot make one of theirs the assistant's, or
    // a second assistant journal, or take the mark off its own.
    let mut forged = vault.journal(theirs.id).unwrap();
    forged.assistant = true;
    vault.save_journal(&forged).unwrap();
    assert!(!vault.journal(theirs.id).unwrap().assistant);
    let mut another = jatayu_core::Journal::new("Another");
    another.assistant = true;
    vault.save_journal(&another).unwrap();
    assert!(!vault.journal(another.id).unwrap().assistant);
    let mut renamed = mine.clone();
    renamed.name = "Robin's notes".into();
    renamed.assistant = false;
    vault.save_journal(&renamed).unwrap();
    let kept = vault.journal(mine.id).unwrap();
    assert!(kept.assistant && kept.name == "Robin's notes");
}

#[test]
fn replacing_the_text_of_an_entry_is_confirmed_like_a_deletion() {
    // `update_entry` is a write, so it was never asked about -- and a body
    // replaces what was there whole, with no undo. Only the calls that lose
    // words are held; renaming, starring or re-filing is as free as ever.
    use super::support::ctx;
    use jatayu_core::agent::tools::{self, Effect};

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = jatayu_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();
    let made = call(
        &vault,
        "create_entry",
        serde_json::json!({
            "journal_id": journal.id.to_string(),
            "title": "Deck, day one",
            "body": "Cut the joists.\n\nRan out of screws.",
        }),
    );
    let id = made["id"].as_str().unwrap().to_string();
    let ctx = ctx(&vault);
    let effect = |args: serde_json::Value| tools::effect_for(&ctx, "update_entry", &args);

    let rewrite = serde_json::json!({ "entry_id": id, "body": "Nothing like it." });
    assert_eq!(effect(rewrite.clone()), Some(Effect::Destructive));
    for harmless in [
        serde_json::json!({ "entry_id": id, "title": "Deck, day two" }),
        serde_json::json!({ "entry_id": id, "starred": true, "tags": ["deck"] }),
        // The same words back, with a new title: nothing is lost.
        serde_json::json!({
            "entry_id": id,
            "body": "Cut the joists.\n\nRan out of screws.",
            "title": "x",
        }),
        serde_json::json!({ "entry_id": id, "body": "   " }),
    ] {
        assert_eq!(effect(harmless.clone()), Some(Effect::Write), "{harmless}");
    }

    // The card has a subject: which entry, and how much of it goes.
    let card = tools::describe(&ctx, "update_entry", &rewrite).expect("a subject to confirm");
    assert!(card.contains("Deck, day one") && card.contains("replaced by 3 words"), "{card}");
    assert!(!card.contains(&id), "named, not identified: {card}");
}

#[test]
fn every_entry_a_tool_returns_says_whose_writing_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = jatayu_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();
    let theirs = call(
        &vault,
        "create_entry",
        serde_json::json!({
            "journal_id": journal.id.to_string(),
            "body": "Cut the joists.",
            "date": "2026-09-07",
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &vault,
        "write_own_journal",
        serde_json::json!({ "date": "2026-09-07", "text": "Ask about the joists." }),
    );
    let own_journal = vault.assistant_journal().unwrap().expect("the page made one").id;

    // Theirs: `own`, in a read of one, a list, and a search.
    let got = call(&vault, "get_entry", serde_json::json!({ "entry_id": theirs }));
    assert_eq!(got["origin"], "own");
    let listed = call(&vault, "list_entries", serde_json::json!({}));
    assert!(listed["entries"].as_array().unwrap().iter().all(|e| e["origin"] == "own"), "{listed}");
    let found = call(&vault, "search", serde_json::json!({ "query": "joists" }));
    let hits = found["results"].as_array().unwrap();
    let hit = |own: bool| hits.iter().find(|h| h["yours"].is_boolean() == own).unwrap();
    assert_eq!(hit(false)["origin"], "own", "{found}");

    // The assistant's own journal: `assistant`, wherever it turns up.
    assert_eq!(hit(true)["origin"], "assistant", "{found}");
    let listed =
        call(&vault, "list_entries", serde_json::json!({ "journal_id": own_journal.to_string() }));
    assert_eq!(listed["entries"][0]["origin"], "assistant", "{listed}");
    let entry_id = listed["entries"][0]["id"].as_str().unwrap().to_string();
    let got = call(&vault, "get_entry", serde_json::json!({ "entry_id": entry_id }));
    assert_eq!(got["origin"], "assistant");
    let pages = call(
        &vault,
        "read_own_journal",
        serde_json::json!({ "from": "2026-09-07", "to": "2026-09-07" }),
    );
    assert_eq!(pages["pages"][0]["origin"], "assistant", "{pages}");
}

#[test]
fn a_blank_body_changes_nothing_and_so_is_not_a_loss() {
    // See the same test in `notes`: a blank string is an omitted argument.
    use super::support::ctx;
    use jatayu_core::agent::tools::{self, Effect};

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = jatayu_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();
    let made = call(
        &vault,
        "create_entry",
        serde_json::json!({ "journal_id": journal.id.to_string(), "body": "Cut the joists." }),
    );
    let id = made["id"].as_str().unwrap().to_string();
    let ctx = ctx(&vault);

    for blank in ["", "   ", "\n"] {
        let args = serde_json::json!({ "entry_id": id, "body": blank, "title": "Day one" });
        assert_eq!(tools::effect_for(&ctx, "update_entry", &args), Some(Effect::Write));
        call(&vault, "update_entry", args);
        let got = call(&vault, "get_entry", serde_json::json!({ "entry_id": id }));
        assert_eq!(got["title"], "Day one");
        let kept = &got["body"];
        assert_eq!(kept, "Cut the joists.", "a blank body must not wipe the entry ({blank:?})");
    }
}
