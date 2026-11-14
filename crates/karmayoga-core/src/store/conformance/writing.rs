//! The writing half of the conformance suite: writing help over text
//! selected anywhere on the desktop.

use super::*;
use crate::id::WritingId;
use crate::store::writing::{WritingQuery, WritingStore};
use crate::writing::{WritingAction, WritingAssist, WritingOutcome};

/// Everything a backend must do with writing help.
pub fn run_writing_suite(store: &dyn JournalStore) {
    eprintln!("--- writing conformance suite ---");

    writing_starts_empty(store);
    an_assist_round_trips_every_field(store);
    putting_an_assist_twice_leaves_one(store);
    missing_assists_are_not_found(store);
    assists_are_listed_newest_first_and_filtered(store);

    writing_cleanup(store);
    eprintln!("--- writing suite passed ---");
}

fn writing_store(store: &dyn JournalStore) -> &dyn WritingStore {
    store.writing().expect("the writing suite needs a writing store")
}

fn writing_cleanup(store: &dyn JournalStore) {
    let w = writing_store(store);
    for assist in w.list_writing_assists(&WritingQuery::default()).expect("list") {
        w.delete_writing_assist(assist.id).expect("delete_writing_assist");
    }
    assert!(
        w.list_writing_assists(&WritingQuery::default()).unwrap().is_empty(),
        "cleanup left assists behind"
    );
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn writing_starts_empty(store: &dyn JournalStore) {
    let w = writing_store(store);
    assert!(w.list_writing_assists(&WritingQuery::default()).unwrap().is_empty());
}

fn an_assist_round_trips_every_field(store: &dyn JournalStore) {
    let w = writing_store(store);
    let mut assist = WritingAssist::new(
        WritingAction::Grammar,
        "  their going to the café — tomorow?  ",
        Some("org.gnome.TextEditor".into()),
    );
    assist.answer("gpt-test", "They're going to the café — tomorrow?");
    assist.settle(WritingOutcome::Replaced).unwrap();
    assist.error = Some("an earlier attempt timed out".into());
    w.put_writing_assist(&assist).expect("put_writing_assist");
    assert_eq!(w.get_writing_assist(assist.id).unwrap(), assist, "every field must survive");
    w.delete_writing_assist(assist.id).unwrap();
}

fn putting_an_assist_twice_leaves_one(store: &dyn JournalStore) {
    let w = writing_store(store);
    let mut assist = WritingAssist::new(WritingAction::Concise, "a long sentence", None);
    w.put_writing_assist(&assist).unwrap();
    assist.answer("gpt-test", "short");
    assist.settle(WritingOutcome::Cancelled).unwrap();
    w.put_writing_assist(&assist).unwrap();

    let all = w.list_writing_assists(&WritingQuery::default()).unwrap();
    assert_eq!(all.len(), 1, "a second put of the same id replaces the first");
    assert_eq!(all[0].outcome, WritingOutcome::Cancelled);
    assert_eq!(all[0].result, "short");
    w.delete_writing_assist(assist.id).unwrap();
}

fn missing_assists_are_not_found(store: &dyn JournalStore) {
    let w = writing_store(store);
    super::assert_not_found(w.get_writing_assist(WritingId::new()));
    w.delete_writing_assist(WritingId::new()).expect("deleting a missing assist is a no-op");
}

fn assists_are_listed_newest_first_and_filtered(store: &dyn JournalStore) {
    let w = writing_store(store);
    let mut seeded = Vec::new();
    for (second, action, outcome) in [
        (1_700_000_010, WritingAction::Grammar, WritingOutcome::Replaced),
        (1_700_000_030, WritingAction::Improve, WritingOutcome::Replaced),
        (1_700_000_020, WritingAction::Grammar, WritingOutcome::Cancelled),
        (1_700_000_040, WritingAction::Grammar, WritingOutcome::Replaced),
    ] {
        let mut a = WritingAssist::new(action, format!("text {second}"), None);
        a.created_at = at(second);
        a.outcome = outcome;
        w.put_writing_assist(&a).unwrap();
        seeded.push(a);
    }

    let originals = |q: &WritingQuery| -> Vec<String> {
        w.list_writing_assists(q).unwrap().into_iter().map(|a| a.original).collect()
    };
    assert_eq!(
        originals(&WritingQuery::default()),
        ["text 1700000040", "text 1700000030", "text 1700000020", "text 1700000010"]
    );
    assert_eq!(
        originals(&WritingQuery {
            actions: vec![WritingAction::Grammar],
            outcomes: vec![WritingOutcome::Replaced],
            ..Default::default()
        }),
        ["text 1700000040", "text 1700000010"]
    );
    assert_eq!(
        originals(&WritingQuery { since: Some(at(1_700_000_025)), ..Default::default() }),
        ["text 1700000040", "text 1700000030"]
    );
    assert_eq!(
        originals(&WritingQuery { limit: Some(1), ..Default::default() }),
        ["text 1700000040"]
    );

    for a in seeded {
        w.delete_writing_assist(a.id).unwrap();
    }
}
