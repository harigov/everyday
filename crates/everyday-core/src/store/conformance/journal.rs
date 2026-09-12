//! The journal half of the conformance suite: journals, entries, blobs,
//! stats and the profile row every backend keeps regardless of which
//! optional domains it also implements.

use super::*;

/// The base battery every backend runs, whether or not it has any of the
/// optional domains: journals, entries, blobs, the profile row. Not gated
/// behind an accessor -- unlike the suites in the other files -- because
/// every [`JournalStore`] has these regardless of what else it stores.
pub(super) fn run_journal_suite(store: &dyn JournalStore) {
    starts_empty(store);
    journal_crud(store);
    journal_get_missing_is_not_found(store);
    entry_round_trips_every_field(store);
    entry_put_is_idempotent(store);
    a_conditional_put_refuses_a_stale_write(store);
    entry_get_missing_is_not_found(store);
    list_entries_filters_and_sorts(store);
    list_entries_paginates(store);
    blobs_are_content_addressed_and_deduplicated(store);
    blob_ranges_match_the_full_payload(store);
    blob_get_missing_is_not_found(store);
    large_blob_round_trips(store);
    deleting_a_journal_removes_its_entries(store);
    garbage_collection_keeps_referenced_blobs(store);
    stats_reflect_contents(store);
    unicode_survives_a_round_trip(store);
    the_owner_round_trips_and_starts_empty(store);
}

pub(super) fn cleanup(store: &dyn JournalStore) {
    for j in store.list_journals().expect("list_journals") {
        store.delete_journal(j.id).expect("delete_journal");
    }
    for b in store.list_blobs().expect("list_blobs") {
        store.delete_blob(b).expect("delete_blob");
    }
    assert!(store.list_journals().unwrap().is_empty(), "cleanup left journals behind");
    assert!(
        store.list_entries(&EntryQuery::default()).unwrap().is_empty(),
        "cleanup left entries behind"
    );
}

fn seeded_journal(store: &dyn JournalStore, name: &str) -> Journal {
    let j = Journal::new(name);
    store.put_journal(&j).expect("put_journal");
    j
}

fn seeded_entry(store: &dyn JournalStore, jid: JournalId, when: Date, text: &str) -> Entry {
    let mut e = Entry::new(jid, "UTC");
    e.local_date = when;
    e.body = RichDoc::from_plain_text(text);
    store.put_entry(&e).expect("put_entry");
    e
}

fn starts_empty(store: &dyn JournalStore) {
    assert!(store.list_journals().unwrap().is_empty(), "a fresh store must have no journals");
    assert!(
        store.list_entries(&EntryQuery::default()).unwrap().is_empty(),
        "a fresh store must have no entries"
    );
    assert_eq!(store.stats().unwrap().entries, 0);
}

fn journal_crud(store: &dyn JournalStore) {
    let mut j = Journal::new("Travel").with_color("#0f766e").with_icon("\u{2708}");
    j.description = "Trips and trains".into();
    j.sort_order = 3;
    store.put_journal(&j).unwrap();

    let got = store.get_journal(j.id).expect("journal should exist after put");
    assert_eq!(got, j, "journal must round-trip unchanged");

    j.name = "Travel & Trains".into();
    store.put_journal(&j).unwrap();
    assert_eq!(store.get_journal(j.id).unwrap().name, "Travel & Trains", "put must upsert");
    assert_eq!(store.list_journals().unwrap().len(), 1, "upsert must not duplicate");

    store.delete_journal(j.id).unwrap();
    assert!(store.get_journal(j.id).is_err(), "journal must be gone after delete");
    assert!(store.list_journals().unwrap().is_empty());
}

fn journal_get_missing_is_not_found(store: &dyn JournalStore) {
    super::assert_not_found(store.get_journal(JournalId::new()));
}

fn entry_round_trips_every_field(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Daily");
    let blob = store.put_blob(b"fake jpeg bytes").unwrap();

    let mut e = Entry::new(j.id, "Europe/Berlin");
    e.title = "A long walk".into();
    e.body = RichDoc(json!({
        "type": "doc",
        "content": [
            {"type": "heading", "attrs": {"level": 1},
             "content": [{"type": "text", "text": "A long walk"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "We went "},
                {"type": "text", "text": "far", "marks": [{"type": "bold"}]},
            ]},
            {"type": MEDIA_NODE, "attrs": {
                "blob": blob.to_hex(), "kind": "image", "mime": "image/jpeg",
                "filename": "walk.jpg", "caption": "the ridge"}},
        ]
    }));
    e.local_date = date(2024, 7, 14);
    e.tags = vec!["walking".into(), "summer".into()];
    e.starred = true;
    e.location = Some(Location {
        latitude: 52.52,
        longitude: 13.405,
        place_name: Some("Tempelhofer Feld".into()),
        locality: Some("Berlin".into()),
        country: Some("Germany".into()),
    });
    e.attachments = vec![Attachment {
        blob,
        kind: MediaKind::Image,
        mime: "image/jpeg".into(),
        filename: "walk.jpg".into(),
        byte_len: 15,
        width: Some(4032),
        height: Some(3024),
        duration_ms: None,
        caption: "the ridge".into(),
    }];
    store.put_entry(&e).unwrap();

    let got = store.get_entry(e.id).expect("entry should exist after put");
    assert_eq!(got.id, e.id);
    assert_eq!(got.journal_id, e.journal_id);
    assert_eq!(got.title, e.title);
    assert_eq!(got.body, e.body, "rich text body must round-trip byte-identically");
    assert_eq!(got.local_date, e.local_date);
    assert_eq!(got.tz, e.tz, "time zone must survive; it is how local dates are recomputed");
    assert_eq!(got.tags, e.tags);
    assert_eq!(got.starred, e.starred);
    assert_eq!(got.location, e.location);
    assert_eq!(got.attachments, e.attachments);
    assert_eq!(got.created_at, e.created_at);

    // Summaries must be derived consistently with the full entry.
    let rows = store.list_entries(&EntryQuery::in_journal(j.id)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "A long walk");
    assert_eq!(rows[0].cover, Some(blob), "first image should become the list thumbnail");
    assert_eq!(rows[0].attachment_count, 1);

    // all_entries must return full bodies, not summaries.
    let all = store.all_entries().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].body, e.body);

    cleanup(store);
}

fn entry_put_is_idempotent(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Daily");
    let mut e = seeded_entry(store, j.id, date(2024, 1, 1), "first");

    e.body = RichDoc::from_plain_text("second");
    e.updated_at = jiff::Timestamp::now();
    store.put_entry(&e).unwrap();

    assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1, "put must upsert");
    assert_eq!(store.get_entry(e.id).unwrap().body.plain_text(), "second");

    store.delete_entry(e.id).unwrap();
    assert!(store.get_entry(e.id).is_err());
    // Deleting twice is not an error worth crashing the UI over, but it must
    // not resurrect anything either.
    let _ = store.delete_entry(e.id);
    assert!(store.list_entries(&EntryQuery::default()).unwrap().is_empty());

    cleanup(store);
}

/// The optimistic-concurrency contract, which every backend must honour.
///
/// The scenario is two writers with the same entry open. Both loaded version
/// `v1`; one saves and the store moves to `v2`; the other saves believing it
/// is still replacing `v1`. Without the check the second write wins and the
/// first author's paragraph is gone with no error anywhere.
fn a_conditional_put_refuses_a_stale_write(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Contended");

    // A create: nothing should be there, and nothing is.
    let mut mine = Entry::new(j.id, "UTC");
    mine.body = RichDoc::from_plain_text("v1");
    store.put_entry_if(&mine, None).expect("creating a new entry must be allowed");

    // Creating the same id twice is a conflict, not an overwrite.
    let mut clash = mine.clone();
    clash.body = RichDoc::from_plain_text("also v1");
    assert_eq!(
        store.put_entry_if(&clash, None).unwrap_err().code(),
        "conflict",
        "a create must not silently replace an entry that is already there"
    );

    let v1 = store.get_entry(mine.id).unwrap().updated_at;

    // The other writer saves first, moving the stored version on.
    let mut theirs = mine.clone();
    theirs.body = RichDoc::from_plain_text("v2 from the other window");
    theirs.updated_at = jiff::Timestamp::now();
    store.put_entry_if(&theirs, Some(v1)).expect("a write from the version we read must land");
    let v2 = store.get_entry(mine.id).unwrap().updated_at;

    // Now ours, still believing the world is at v1.
    let mut stale = mine.clone();
    stale.body = RichDoc::from_plain_text("v2 from this window");
    stale.updated_at = jiff::Timestamp::now();
    assert_eq!(
        store.put_entry_if(&stale, Some(v1)).unwrap_err().code(),
        "conflict",
        "a write from a version that has been superseded must be refused"
    );
    assert_eq!(
        store.get_entry(mine.id).unwrap().body.plain_text(),
        "v2 from the other window",
        "a refused write must change nothing"
    );

    // Re-reading and retrying against the current version succeeds, which is
    // what makes the conflict recoverable rather than a dead end.
    stale.updated_at = jiff::Timestamp::now();
    store.put_entry_if(&stale, Some(v2)).expect("a retry from the current version must land");

    // An update whose row has been deleted underneath it is also a conflict:
    // recreating it silently would undo somebody's deletion.
    let current = store.get_entry(mine.id).unwrap().updated_at;
    store.delete_entry(mine.id).unwrap();
    assert_eq!(
        store.put_entry_if(&stale, Some(current)).unwrap_err().code(),
        "conflict",
        "an update must not resurrect a deleted entry"
    );

    // And the unconditional write is still available for the deliberate
    // "keep mine", which is what the interface offers on a conflict.
    store.put_entry(&stale).expect("an unconditional write must always be allowed");

    cleanup(store);
}

fn entry_get_missing_is_not_found(store: &dyn JournalStore) {
    super::assert_not_found(store.get_entry(EntryId::new()));
}

fn list_entries_filters_and_sorts(store: &dyn JournalStore) {
    let a = seeded_journal(store, "A");
    let b = seeded_journal(store, "B");

    let e1 = seeded_entry(store, a.id, date(2024, 1, 10), "oldest");
    let e2 = seeded_entry(store, a.id, date(2024, 6, 1), "middle");
    let e3 = seeded_entry(store, b.id, date(2024, 12, 25), "newest");

    let mut starred = store.get_entry(e2.id).unwrap();
    starred.starred = true;
    starred.tags = vec!["Highlight".into()];
    store.put_entry(&starred).unwrap();

    let ids = |q: &EntryQuery| -> Vec<EntryId> {
        store.list_entries(q).unwrap().into_iter().map(|r| r.id).collect()
    };

    assert_eq!(ids(&EntryQuery::default()), [e3.id, e2.id, e1.id], "default sort is date desc");

    assert_eq!(
        ids(&EntryQuery { sort: SortOrder::DateAsc, ..Default::default() }),
        [e1.id, e2.id, e3.id]
    );

    assert_eq!(ids(&EntryQuery::in_journal(a.id)), [e2.id, e1.id], "journal filter");

    assert_eq!(
        ids(&EntryQuery {
            from: Some(date(2024, 6, 1)),
            to: Some(date(2024, 6, 30)),
            ..Default::default()
        }),
        [e2.id],
        "date range is inclusive on both ends"
    );

    assert_eq!(
        ids(&EntryQuery { starred: Some(true), ..Default::default() }),
        [e2.id],
        "starred filter"
    );

    assert_eq!(
        ids(&EntryQuery { tags: vec!["highlight".into()], ..Default::default() }),
        [e2.id],
        "tag filter must be case-insensitive"
    );

    assert!(ids(&EntryQuery { tags: vec!["nonexistent".into()], ..Default::default() }).is_empty());

    cleanup(store);
}

fn list_entries_paginates(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Long");
    let mut ids = Vec::new();
    for d in 1..=10u8 {
        ids.push(seeded_entry(store, j.id, date(2024, 1, d as i8), &format!("day {d}")).id);
    }

    let page = |offset, limit| -> Vec<EntryId> {
        store
            .list_entries(&EntryQuery {
                sort: SortOrder::DateAsc,
                offset,
                limit: Some(limit),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    };

    assert_eq!(page(0, 3), ids[0..3]);
    assert_eq!(page(3, 3), ids[3..6]);
    assert_eq!(page(9, 3), ids[9..10], "a partial final page is fine");
    assert!(page(50, 3).is_empty(), "an offset past the end must not panic");

    cleanup(store);
}

fn blobs_are_content_addressed_and_deduplicated(store: &dyn JournalStore) {
    let bytes = b"\x89PNG\r\n\x1a\n and then some pixels";
    let id = store.put_blob(bytes).unwrap();

    assert_eq!(id, BlobId::of(bytes), "blob id must be the BLAKE3 of the plaintext");
    assert!(store.has_blob(id).unwrap());
    assert_eq!(store.get_blob(id).unwrap(), bytes, "blob must round-trip byte-identically");

    let again = store.put_blob(bytes).unwrap();
    assert_eq!(again, id, "identical bytes must deduplicate");
    assert_eq!(store.list_blobs().unwrap().len(), 1);

    let other = store.put_blob(b"different").unwrap();
    assert_ne!(other, id);
    assert_eq!(store.list_blobs().unwrap().len(), 2);

    store.delete_blob(id).unwrap();
    assert!(!store.has_blob(id).unwrap());
    assert!(store.get_blob(id).is_err());

    cleanup(store);
}

fn blob_ranges_match_the_full_payload(store: &dyn JournalStore) {
    // 900 KiB spans several chunks in the chunk-encrypted backends, so this
    // exercises boundary-straddling reads rather than a single-chunk case.
    let mut data = Vec::with_capacity(900 * 1024);
    let mut x: u32 = 0xdead_beef;
    for _ in 0..900 * 1024 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        data.push((x >> 24) as u8);
    }
    let id = store.put_blob(&data).unwrap();
    assert_eq!(
        store.blob_len(id).unwrap(),
        data.len() as u64,
        "blob_len must be the plaintext length"
    );

    for (off, len) in [(0u64, 16u64), (1, 1), (262_143, 4), (262_144, 100), (700_000, 200_000)] {
        assert_eq!(
            store.get_blob_range(id, off, len).unwrap(),
            data[off as usize..(off + len) as usize],
            "range ({off}, {len}) must match the full payload"
        );
    }
    // Clamping, not erroring, past the end.
    assert_eq!(
        store.get_blob_range(id, data.len() as u64 - 5, 500).unwrap(),
        data[data.len() - 5..]
    );
    assert!(store.get_blob_range(id, data.len() as u64, 10).unwrap().is_empty());

    store.delete_blob(id).unwrap();
    cleanup(store);
}

fn blob_get_missing_is_not_found(store: &dyn JournalStore) {
    let missing = BlobId::of(b"never stored");
    assert!(!store.has_blob(missing).unwrap());
    super::assert_not_found(store.get_blob(missing));
}

fn large_blob_round_trips(store: &dyn JournalStore) {
    // 4 MiB — a plausible phone photo, and large enough to catch backends
    // that quietly truncate or that mishandle chunked encryption.
    let mut bytes = Vec::with_capacity(4 << 20);
    let mut x: u32 = 0x9e37_79b9;
    for _ in 0..(4 << 20) {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((x >> 24) as u8);
    }
    let id = store.put_blob(&bytes).unwrap();
    assert_eq!(store.get_blob(id).unwrap(), bytes, "a 4 MiB blob must round-trip intact");
    store.delete_blob(id).unwrap();

    cleanup(store);
}

fn deleting_a_journal_removes_its_entries(store: &dyn JournalStore) {
    let keep = seeded_journal(store, "Keep");
    let drop = seeded_journal(store, "Drop");
    let survivor = seeded_entry(store, keep.id, date(2024, 2, 2), "still here");
    let doomed = seeded_entry(store, drop.id, date(2024, 2, 3), "not for long");

    store.delete_journal(drop.id).unwrap();

    assert!(store.get_entry(doomed.id).is_err(), "entries must cascade with their journal");
    assert!(store.get_entry(survivor.id).is_ok(), "other journals must be untouched");
    assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1);

    cleanup(store);
}

fn garbage_collection_keeps_referenced_blobs(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Photos");
    let used = store.put_blob(b"in use").unwrap();
    let orphan = store.put_blob(b"nobody wants me").unwrap();

    let mut e = Entry::new(j.id, "UTC");
    e.body = RichDoc(json!({
        "type": "doc",
        "content": [{"type": MEDIA_NODE,
                     "attrs": {"blob": used.to_hex(), "kind": "image"}}]
    }));
    store.put_entry(&e).unwrap();

    // A grace period protects blobs younger than it, referenced or not.
    // Both of these were written a moment ago, so an hour's grace must
    // collect neither -- this is what keeps GC from eating the image that
    // has been pasted into a draft but not yet saved.
    assert_eq!(
        store.collect_garbage(std::time::Duration::from_secs(3600)).unwrap(),
        0,
        "nothing younger than the grace period may be collected"
    );
    assert!(store.has_blob(orphan).unwrap(), "a young orphan must survive a graced GC");

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(store.has_blob(used).unwrap(), "a referenced blob must survive GC");
    assert!(!store.has_blob(orphan).unwrap(), "an unreferenced blob must be collected");

    // GC must be safe to run repeatedly.
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 0);

    cleanup(store);
}

fn stats_reflect_contents(store: &dyn JournalStore) {
    let j = seeded_journal(store, "Counting");
    seeded_entry(store, j.id, date(2024, 3, 1), "one");
    seeded_entry(store, j.id, date(2024, 3, 2), "two");
    store.put_blob(b"a blob").unwrap();

    let s = store.stats().unwrap();
    assert_eq!(s.journals, 1);
    assert_eq!(s.entries, 2);
    assert_eq!(s.blobs, 1);
    assert!(s.blob_bytes > 0, "blob_bytes should report on-disk size");

    cleanup(store);
}

fn unicode_survives_a_round_trip(store: &dyn JournalStore) {
    let j = seeded_journal(store, "\u{65e5}\u{8a18}");
    // Emoji with ZWJ + skin tone, RTL text, combining marks, and a NUL-free
    // but control-character-adjacent string.
    let tricky = "\u{1f469}\u{200d}\u{1f4bb} \u{5bb6}\u{65cf} \u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} e\u{301}cole \u{1f1ef}\u{1f1f5}";
    let mut e = Entry::new(j.id, "Asia/Tokyo");
    e.title = tricky.into();
    e.body = RichDoc::from_plain_text(tricky);
    e.tags = vec!["\u{30bf}\u{30b0}".into()];
    store.put_entry(&e).unwrap();

    let got = store.get_entry(e.id).unwrap();
    assert_eq!(got.title, tricky, "unicode title must survive");
    assert_eq!(got.body.plain_text(), tricky, "unicode body must survive");
    assert_eq!(got.tags, e.tags, "unicode tags must survive");
    assert_eq!(store.get_journal(j.id).unwrap().name, "\u{65e5}\u{8a18}");

    cleanup(store);
}

/// The one row that says whose vault this is.
///
/// Part of the base battery rather than an optional suite, because every
/// backend must answer it: the default on the trait says "nothing filled in",
/// which is a real answer, and a backend that cannot store one has to say so
/// rather than silently forgetting what was typed.
fn the_owner_round_trips_and_starts_empty(store: &dyn JournalStore) {
    let blank = store.profile().expect("a fresh store still has an answer");
    assert!(blank.is_empty(), "nobody has said who they are yet");

    let mine = Profile {
        first_name: "Hari".into(),
        last_name: "Govardhanam".into(),
        born: Some(date(1985, 3, 14)),
        gender: "male".into(),
        location: "Seattle".into(),
        about: "Software, two children, sailing at weekends".into(),
        updated_at: Some(Timestamp::now()),
    };
    match store.put_profile(&mine) {
        Ok(()) => {}
        // A backend written before this existed says so rather than pretending.
        Err(e) if e.code() == "unsupported" => {
            eprintln!("  (backend stores no profile; skipping the owner)");
            return;
        }
        Err(e) => panic!("put_profile: {e}"),
    }

    assert_eq!(store.profile().unwrap(), mine, "every field must survive the round trip");

    // Idempotent, and a second write replaces rather than adding a row.
    store.put_profile(&mine).expect("put_profile again");
    assert_eq!(store.profile().unwrap(), mine);

    // Emptied by hand is emptied, not reverted to what was there before.
    store.put_profile(&Profile::default()).expect("clearing the profile");
    assert!(store.profile().unwrap().is_empty(), "clearing it must actually clear it");
}
