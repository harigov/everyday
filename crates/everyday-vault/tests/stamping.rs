//! Phase 0.6 of `docs/plans/architecture-refactor.md`: who sets
//! `updated_at`, and when, pinned as behaviour rather than left to be
//! noticed only once a later phase moves the code that does it.
//!
//! "The one rule" at the top of the plan names this specifically: a phase
//! may not change "when `updated_at` is stamped, and by whom." Conflict
//! detection across the whole vault rests on "every writer owns its own
//! `updated_at`" -- see `Vault::save_note` and `Vault::save_entry`'s own
//! doc comments -- so an automatic re-stamp anywhere in the write path would
//! turn every second autosave into a false conflict.
//!
//! `crates/everyday-core/src/vault/tests.rs` covers the same property for
//! `save_entry`, over the in-memory backend that domain does not need a real
//! one for. Notes and the assistant's own conversations are optional
//! domains `MemStore` does not implement (see `Capabilities::notes` and
//! `Capabilities::agent`), so those live here, over the real SQLite backend
//! `support::vault` builds.

mod support;
use support::vault;

#[test]
fn save_note_stores_the_caller_supplied_updated_at_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut note = everyday_core::Note::new("Kept exactly as saved");
    note.updated_at = "2020-01-02T03:04:05Z".parse().unwrap();
    vault.save_note(&note, None).unwrap();
    assert_eq!(
        vault.note(note.id).unwrap().updated_at,
        note.updated_at,
        "save_note must not re-stamp updated_at"
    );

    // A second, conditional save whose `expect` is exactly what the first
    // save stored must succeed -- the case that makes a retried autosave
    // recoverable rather than a dead end.
    let mut second = note.clone();
    second.updated_at = "2021-06-15T12:00:00Z".parse().unwrap();
    vault.save_note(&second, Some(note.updated_at)).unwrap();
    assert_eq!(vault.note(note.id).unwrap().updated_at, second.updated_at);

    // And a save against a version that has been superseded -- what a
    // second, stale window holds -- is a conflict, not an overwrite.
    let mut stale = second.clone();
    stale.updated_at = "2022-01-01T00:00:00Z".parse().unwrap();
    assert_eq!(
        vault.save_note(&stale, Some(note.updated_at)).unwrap_err().code(),
        "conflict",
        "a write from a version that has been superseded must be refused"
    );
    assert_eq!(
        vault.note(note.id).unwrap().updated_at,
        second.updated_at,
        "the refused save must not have changed anything"
    );
}

#[test]
fn save_message_bumps_the_conversation_to_a_later_message() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut conversation = everyday_core::Conversation::new();
    conversation.updated_at = "2020-01-01T00:00:00Z".parse().unwrap();
    vault.save_conversation(&conversation).unwrap();

    let mut message = everyday_core::Message::user(conversation.id, "later than the thread");
    message.created_at = "2020-06-01T00:00:00Z".parse().unwrap();
    vault.save_message(&message).unwrap();

    assert_eq!(
        vault.conversation(conversation.id).unwrap().updated_at,
        message.created_at,
        "a message newer than the thread must become its updated_at"
    );
}

#[test]
fn save_message_does_not_move_the_conversation_backwards() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut conversation = everyday_core::Conversation::new();
    conversation.updated_at = "2020-06-01T00:00:00Z".parse().unwrap();
    vault.save_conversation(&conversation).unwrap();

    // A message older than the thread's current `updated_at` -- an
    // out-of-order write, or a backfilled import -- must not drag the
    // thread's own timestamp backwards in the history list.
    let mut message = everyday_core::Message::user(conversation.id, "earlier than the thread");
    message.created_at = "2020-01-01T00:00:00Z".parse().unwrap();
    vault.save_message(&message).unwrap();

    assert_eq!(
        vault.conversation(conversation.id).unwrap().updated_at,
        conversation.updated_at,
        "an older message must leave the thread's updated_at where it was"
    );
}
