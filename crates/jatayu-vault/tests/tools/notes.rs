//! Notes, through the tools a model actually has.

use super::support::{call, call_err, vault};

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

/// `update_note`'s half of the rule `journals.rs`'s
/// `an_entry_holding_photographs_will_not_have_its_text_replaced` pins for
/// entries: a body arrives whole or not at all, and replacing one that
/// holds a photograph would take it off the page with no way for the model
/// -- handed the body as Markdown, in which a photograph is a link it
/// cannot put back -- to know or undo it.
#[test]
fn a_note_holding_photographs_will_not_have_its_text_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let blob = vault.put_blob(b"not really a photograph").unwrap();
    let mut note = jatayu_core::Note::new("Moodboard");
    note.body = jatayu_core::RichDoc(serde_json::json!({
        "type": "doc",
        "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "Some ideas" }] },
            { "type": "media", "attrs": { "blob": blob.to_string(), "kind": "image" } },
        ],
    }));
    vault.save_note(&note, None).unwrap();

    let err = call_err(
        &vault,
        "update_note",
        serde_json::json!({ "note_id": note.id.to_string(), "body": "Rewritten." }),
    );
    assert!(err.contains("photographs"), "got {err}");

    // Everything else about it is still editable.
    call(
        &vault,
        "update_note",
        serde_json::json!({ "note_id": note.id.to_string(), "pinned": true }),
    );
    let after = vault.note(note.id).unwrap();
    assert!(after.pinned);
    assert_eq!(after.body.blob_refs().len(), 1, "the photograph is still on the page");
}

#[test]
fn replacing_the_text_of_a_note_is_confirmed_like_a_deletion() {
    use super::support::ctx;
    use jatayu_core::agent::tools::{self, Effect};

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // The person's own note: their words are what a rewrite would lose.
    let own = jatayu_core::Note::written("Monday", "Order the timber.\n\nRing the surveyor.");
    vault.save_note(&own, None).unwrap();
    let id = own.id.to_string();
    let empty = jatayu_core::Note::written("Blank", "");
    vault.save_note(&empty, None).unwrap();
    // One the assistant wrote: a research job rewrites its own answer whole.
    let made = call(
        &vault,
        "create_note",
        serde_json::json!({ "title": "Findings", "body": "So far: the Bosch motor." }),
    );
    let its_own = made["id"].as_str().unwrap().to_string();
    let ctx = ctx(&vault);
    let effect = |args: serde_json::Value| tools::effect_for(&ctx, "update_note", &args);

    let rewrite = serde_json::json!({ "note_id": id, "body": "Something else." });
    assert_eq!(effect(rewrite.clone()), Some(Effect::Destructive));
    for harmless in [
        serde_json::json!({ "note_id": id, "title": "Tuesday", "pinned": true }),
        serde_json::json!({ "note_id": id, "body": "Order the timber.\n\nRing the surveyor." }),
        // Writing the first words into a note that has none loses nothing.
        serde_json::json!({ "note_id": empty.id.to_string(), "body": "First words." }),
        // Nor does the assistant rewriting a note it wrote itself.
        serde_json::json!({ "note_id": its_own, "body": "The Bosch." }),
    ] {
        assert_eq!(effect(harmless.clone()), Some(Effect::Write), "{harmless}");
    }

    let card = tools::describe(&ctx, "update_note", &rewrite).expect("a subject to confirm");
    assert!(card.contains("Monday") && card.contains("replaced by 2 words"), "{card}");
    assert!(!card.contains(&id), "named, not identified: {card}");
}

#[test]
fn every_note_a_tool_returns_says_who_started_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mine = jatayu_core::Note::written("Recipe", "Whisk the joists.");
    vault.save_note(&mine, None).unwrap();
    let made = call(
        &vault,
        "create_note",
        serde_json::json!({ "title": "Weekly report", "body": "The joists are cut." }),
    );
    let theirs = made["id"].as_str().unwrap().to_string();

    let listed = call(&vault, "list_notes", serde_json::json!({}));
    let origin_of = |title: &str| {
        listed["notes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["title"] == title)
            .unwrap_or_else(|| panic!("{title} is not listed: {listed}"))["origin"]
            .clone()
    };
    assert_eq!(origin_of("Recipe"), "own");
    assert_eq!(origin_of("Weekly report"), "assistant");

    let got = call(&vault, "get_note", serde_json::json!({ "note_id": theirs }));
    assert_eq!(got["origin"], "assistant");
    let got = call(&vault, "get_note", serde_json::json!({ "note_id": mine.id.to_string() }));
    assert_eq!(got["origin"], "own");

    // A search hit that is a note says so too.
    let found = call(&vault, "search", serde_json::json!({ "query": "joists", "scope": "notes" }));
    let hits = found["results"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "{found}");
    for hit in hits {
        let expected = if hit["id"] == mine.id.to_string() { "own" } else { "assistant" };
        assert_eq!(hit["origin"], expected, "{hit}");
    }
}

#[test]
fn a_note_that_only_looks_like_the_assistants_is_still_the_persons_to_lose() {
    // A note saved before origins were kept has none, and is guessed at from
    // its tags -- so a `research` tag, which is a plain write, must not be a
    // way to overwrite it unasked. Only an origin that was recorded exempts a
    // note, which is what lets an interrupted research job rewrite its own.
    use super::support::ctx;
    use jatayu_core::NoteOrigin;
    use jatayu_core::agent::tools::{self, Effect};

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut legacy = jatayu_core::Note::written("Old notes", "What the surveyor said.");
    legacy.origin = None;
    vault.save_note(&legacy, None).unwrap();
    let ctx = ctx(&vault);
    let rewrite =
        |id: &str| serde_json::json!({ "note_id": id, "body": "Something else entirely." });
    let id = legacy.id.to_string();

    assert_eq!(tools::effect_for(&ctx, "update_note", &rewrite(&id)), Some(Effect::Destructive));
    // The tag goes on as a plain write, and the guess now says "assistant"...
    call(&vault, "update_note", serde_json::json!({ "note_id": id, "tags": ["research"] }));
    let tagged = vault.note(legacy.id).unwrap();
    assert_eq!(tagged.origin, None, "a guess is never saved");
    assert_eq!(tagged.origin(), NoteOrigin::Assistant);
    // ...and the rewrite is still asked about.
    assert_eq!(tools::effect_for(&ctx, "update_note", &rewrite(&id)), Some(Effect::Destructive));

    // A note the assistant made itself, with the origin recorded, is its own.
    let made = call(
        &vault,
        "create_note",
        serde_json::json!({ "title": "Answer", "body": "What the job found." }),
    );
    let mine = made["id"].as_str().unwrap().to_string();
    assert_eq!(vault.note(mine.parse().unwrap()).unwrap().origin, Some(NoteOrigin::Assistant));
    assert_eq!(tools::effect_for(&ctx, "update_note", &rewrite(&mine)), Some(Effect::Write));
}

#[test]
fn a_blank_body_changes_nothing_and_so_is_not_a_loss() {
    // A model fills in optional fields it has nothing for with "". The tools
    // read a blank string as an omitted argument, so the text stays -- and
    // there is nothing to confirm. (Were a blank body ever to be written, it
    // would be a replacement: see `replaces_text`.)
    use super::support::ctx;
    use jatayu_core::agent::tools::{self, Effect};

    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let made = call(
        &vault,
        "create_note",
        serde_json::json!({ "title": "Monday", "body": "Order the timber." }),
    );
    let id = made["id"].as_str().unwrap().to_string();
    let mine = jatayu_core::Note::written("Mine", "Ring the surveyor.");
    vault.save_note(&mine, None).unwrap();
    let ctx = ctx(&vault);

    for (note, text) in [(id, "Order the timber."), (mine.id.to_string(), "Ring the surveyor.")] {
        for blank in ["", "   ", "\n"] {
            let args = serde_json::json!({ "note_id": note, "body": blank, "title": "Renamed" });
            assert_eq!(tools::effect_for(&ctx, "update_note", &args), Some(Effect::Write));
            call(&vault, "update_note", args);
            let got = call(&vault, "get_note", serde_json::json!({ "note_id": note }));
            assert_eq!(got["title"], "Renamed");
            assert_eq!(got["body"], text, "a blank body must not wipe the note ({blank:?})");
        }
    }
}
