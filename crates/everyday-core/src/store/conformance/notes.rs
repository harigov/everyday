//! The notes half of the conformance suite.

use super::*;

/// Everything a backend must do with notes.
pub fn run_note_suite(store: &dyn JournalStore) {
    eprintln!("--- note conformance suite ---");

    notes_start_empty(store);
    note_round_trips_every_field(store);
    note_put_is_idempotent(store);
    a_conditional_note_put_refuses_a_stale_write(store);
    missing_note_is_not_found(store);
    list_notes_filters_and_sorts(store);
    all_notes_carries_bodies_and_the_list_does_not(store);
    a_notes_purpose_survives_and_goes_with_it(store);
    garbage_collection_keeps_pictures_in_notes(store);
    unicode_survives_a_note_round_trip(store);

    note_cleanup(store);
    eprintln!("--- note suite passed ---");
}

fn note_store(store: &dyn JournalStore) -> &dyn crate::store::notes::NoteStore {
    store.notes().expect("the note suite needs a note store")
}

fn note_cleanup(store: &dyn JournalStore) {
    let n = note_store(store);
    for note in n.list_notes(&NoteQuery::default()).expect("list_notes") {
        n.delete_note(note.id).expect("delete_note");
    }
    assert!(n.list_notes(&NoteQuery::default()).unwrap().is_empty(), "cleanup left notes behind");
}

fn seeded_note(store: &dyn JournalStore, title: &str, body: &str) -> Note {
    let note = Note::written(title, body);
    note_store(store).put_note(&note).expect("put_note");
    note
}

fn notes_start_empty(store: &dyn JournalStore) {
    assert!(
        note_store(store).list_notes(&NoteQuery::default()).unwrap().is_empty(),
        "a fresh store must have no notes"
    );
}

fn note_round_trips_every_field(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut note = Note::written("Sailing", "Reach, run, beat.");
    note.tags = vec!["boats".into(), "summer".into()];
    note.pinned = true;
    n.put_note(&note).expect("put_note");

    let back = n.get_note(note.id).expect("get_note");
    assert_eq!(back, note, "every field must survive the round trip");

    let listed = n.list_notes(&NoteQuery::default()).expect("list_notes");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Sailing");
    assert_eq!(listed[0].tags, vec!["boats".to_string(), "summer".to_string()]);
    assert!(listed[0].pinned);

    n.delete_note(note.id).expect("delete_note");
    assert!(n.list_notes(&NoteQuery::default()).unwrap().is_empty());
}

fn note_put_is_idempotent(store: &dyn JournalStore) {
    let n = note_store(store);
    let note = Note::written("Twice", "once");
    n.put_note(&note).expect("first put");
    n.put_note(&note).expect("second put");
    assert_eq!(n.list_notes(&NoteQuery::default()).unwrap().len(), 1, "saving twice leaves one");

    // Deleting something that is not there is not an error, so a client that
    // retries a delete over a dropped connection is not told it failed.
    n.delete_note(note.id).expect("delete_note");
    n.delete_note(note.id).expect("deleting a missing note is a no-op");
}

fn a_conditional_note_put_refuses_a_stale_write(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut note = Note::written("Shared", "first");
    n.put_note_if(&note, None).expect("creating with no expectation");

    // Creating something that is already there is a conflict, not an
    // overwrite: two windows both minting the same note is the case.
    assert_eq!(
        n.put_note_if(&note, None).unwrap_err().code(),
        "conflict",
        "creating over an existing note must be refused"
    );

    let seen = note.updated_at;
    note.title = "Shared, edited".into();
    note.updated_at = Timestamp::now();
    n.put_note_if(&note, Some(seen)).expect("writing over the version we read");

    // The stale expectation is what a second window holds.
    let mut stale = note.clone();
    stale.title = "Someone else's edit".into();
    assert_eq!(
        n.put_note_if(&stale, Some(seen)).unwrap_err().code(),
        "conflict",
        "a write over a version that has moved on must be refused"
    );

    // And a note that has been deleted under a caller who thought they were
    // updating it is a conflict too, not a silent resurrection.
    n.delete_note(note.id).expect("delete_note");
    assert_eq!(
        n.put_note_if(&note, Some(note.updated_at)).unwrap_err().code(),
        "conflict",
        "updating a note that has been deleted must be refused"
    );
    note_cleanup(store);
}

fn missing_note_is_not_found(store: &dyn JournalStore) {
    super::assert_not_found(note_store(store).get_note(NoteId::new()));
}

fn list_notes_filters_and_sorts(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut alpha = Note::written("Alpha", "first");
    alpha.tags = vec!["Boats".into()];
    let mut zeta = Note::written("Zeta", "second");
    zeta.tags = vec!["boats".into(), "summer".into()];
    zeta.pinned = true;
    n.put_note(&alpha).expect("put alpha");
    n.put_note(&zeta).expect("put zeta");

    let by_title = n
        .list_notes(&NoteQuery { sort: NoteSort::TitleAsc, ..Default::default() })
        .expect("list by title");
    assert_eq!(
        by_title.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(),
        ["Zeta", "Alpha"],
        "a pin outranks the alphabet"
    );

    // Case must not decide whether a tag matches: "Boats" and "boats" are
    // one tag to a person.
    let tagged = n
        .list_notes(&NoteQuery { tags: vec!["boats".into()], ..Default::default() })
        .expect("list by tag");
    assert_eq!(tagged.len(), 2);

    let both = n
        .list_notes(&NoteQuery {
            tags: vec!["boats".into(), "summer".into()],
            ..Default::default()
        })
        .expect("list by two tags");
    assert_eq!(both.len(), 1, "every listed tag has to be there, not any of them");

    let pinned =
        n.list_notes(&NoteQuery { pinned: Some(true), ..Default::default() }).expect("list pinned");
    assert_eq!(pinned.len(), 1);

    let capped = n
        .list_notes(&NoteQuery { limit: Some(1), ..Default::default() })
        .expect("list with a limit");
    assert_eq!(capped.len(), 1);

    note_cleanup(store);
}

fn all_notes_carries_bodies_and_the_list_does_not(store: &dyn JournalStore) {
    let n = note_store(store);
    let note = seeded_note(store, "Recipe", "Two hundred grams of flour.");

    let listed = n.list_notes(&NoteQuery::default()).expect("list_notes");
    assert_eq!(listed[0].excerpt, "Two hundred grams of flour.");

    let all = n.all_notes().expect("all_notes");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].body.plain_text().trim(), "Two hundred grams of flour.");
    assert_eq!(all[0].id, note.id);

    let tags = n.note_tags().expect("note_tags");
    assert!(tags.is_empty(), "an untagged note contributes no tags");

    note_cleanup(store);
}

fn a_notes_purpose_survives_and_goes_with_it(store: &dyn JournalStore) {
    let Some(p) = store.purpose() else {
        eprintln!("  (no purpose store; skipping the note's purpose)");
        return;
    };
    let role = LifeRole::new("Sailor");
    p.put_role(&role).expect("put_role");

    let n = note_store(store);
    let mut note = Note::written("Log", "Wind from the south-west.");
    note.purpose = Some(Purpose::Role { id: role.id });
    n.put_note(&note).expect("put_note");

    assert_eq!(
        n.get_note(note.id).unwrap().purpose,
        Some(Purpose::Role { id: role.id }),
        "a note's purpose must survive the round trip"
    );
    assert_eq!(
        n.list_notes(&NoteQuery::default()).unwrap()[0].purpose,
        Some(Purpose::Role { id: role.id }),
        "and be on the summary, so a list can say what a note is filed under"
    );

    // The side table is an index over the payload, and deleting the record
    // it indexes must take the row with it.
    n.delete_note(note.id).expect("delete_note");
    let mut revived = note.clone();
    revived.purpose = None;
    n.put_note(&revived).expect("put_note");
    assert_eq!(
        n.get_note(note.id).unwrap().purpose,
        None,
        "a re-made note must not inherit the deleted one's filing"
    );

    n.delete_note(note.id).expect("delete_note");
    p.delete_role(role.id).expect("delete_role");
}

fn garbage_collection_keeps_pictures_in_notes(store: &dyn JournalStore) {
    if !store.capabilities().blobs {
        eprintln!("  (no blobs; skipping the note's pictures)");
        return;
    }
    let blob = store.put_blob(b"a photograph in a note").expect("put_blob");
    let mut note = Note::new("Illustrated");
    note.body = RichDoc(json!({
        "type": "doc",
        "content": [{"type": MEDIA_NODE, "attrs": {"blob": blob.to_hex(), "kind": "image"}}]
    }));
    note_store(store).put_note(&note).expect("put_note");

    store.collect_garbage(std::time::Duration::ZERO).expect("collect_garbage");
    assert!(
        store.has_blob(blob).unwrap(),
        "a picture in a note is referenced, and must survive a sweep"
    );

    note_store(store).delete_note(note.id).expect("delete_note");
    store.collect_garbage(std::time::Duration::ZERO).expect("collect_garbage");
    assert!(!store.has_blob(blob).unwrap(), "and must be swept once nothing points at it");
}

fn unicode_survives_a_note_round_trip(store: &dyn JournalStore) {
    let n = note_store(store);
    let mut note = Note::written("மீன் \u{1f41f}", "நீரில் நீந்துகிறது");
    note.tags = vec!["தமிழ்".into()];
    n.put_note(&note).expect("put_note");
    let back = n.get_note(note.id).expect("get_note");
    assert_eq!(back.title, "மீன் \u{1f41f}");
    assert_eq!(back.tags, vec!["தமிழ்".to_string()]);
    assert_eq!(back.body.plain_text().trim(), "நீரில் நீந்துகிறது");
    n.delete_note(note.id).expect("delete_note");
}
