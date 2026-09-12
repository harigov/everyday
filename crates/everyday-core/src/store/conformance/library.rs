//! The library half of the conformance suite: shelves, items and the log.

use super::*;

/// The library half of the suite. Called by [`run_all`] when the backend has
/// a [`LibraryStore`]; public so a backend under construction can run it
/// alone.
///
/// The store must be empty of kinds on entry; it is left empty on success.
pub fn run_library_suite(store: &dyn LibraryStore) {
    eprintln!("--- library conformance suite ---");

    library_starts_empty(store);
    kind_crud(store);
    missing_library_records_are_not_found(store);
    item_round_trips_every_field(store);
    batch_item_writes_land_together(store);
    listing_items_filters_and_sorts(store);
    listing_items_paginates(store);
    shelf_counts_are_answered_without_decrypting_everything(store);
    the_log_round_trips_and_queries_by_window(store);
    deleting_an_item_takes_its_log_with_it(store);
    deleting_a_kind_takes_its_shelf_with_it(store);
    unicode_survives_a_library_round_trip(store);

    library_cleanup(store);
    eprintln!("--- library suite passed ---");
}

/// A cover must survive the garbage collector.
///
/// The bug this exists to prevent is the quietest kind there is. Liveness is
/// decided by walking the records that reference a blob, and until the
/// library was added that walk knew only about entries. Missing it here
/// meant the first `everyday gc` after a day's grace deleted the cover of
/// every book on every shelf -- with no error anywhere, the items still
/// present, and each one holding a blob id pointing at nothing.
pub(super) fn garbage_collection_keeps_library_covers(store: &dyn JournalStore) {
    let Some(library) = store.library() else { return };

    let kind = seeded_kind(library, "gc");
    let cover = store.put_blob(b"a book jacket").unwrap();
    let orphan = store.put_blob(b"nobody's cover").unwrap();

    let mut item = Item::new(kind.id, "Dune");
    item.cover = Some(cover);
    library.put_item(&item).unwrap();

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(store.has_blob(cover).unwrap(), "a cover on a shelf is a live reference");
    assert!(!store.has_blob(orphan).unwrap());

    // ...and it stops being one when the item goes.
    library.delete_item(item.id).unwrap();
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 1);
    assert!(!store.has_blob(cover).unwrap(), "a cover nothing points at is collectable");

    library.delete_kind(kind.id).unwrap();
}

fn library_cleanup(store: &dyn LibraryStore) {
    for kind in store.list_kinds().expect("list_kinds") {
        store.delete_kind(kind.id).expect("delete_kind");
    }
    assert!(store.list_kinds().unwrap().is_empty(), "cleanup left kinds behind");
    assert!(
        store.list_items(&ItemQuery::default()).unwrap().is_empty(),
        "cleanup left items behind"
    );
    assert!(store.list_logs(&LogQuery::default()).unwrap().is_empty(), "cleanup left logs behind");
}

pub(super) fn seeded_kind(store: &dyn LibraryStore, slug: &str) -> Kind {
    let kind = Kind::new(slug, "Books", "Book")
        .with_verbs(Verbs::new("To read", "Reading", "Read", "read"))
        .with_fields(vec![
            FieldDef::new("author", "Author", FieldType::Text),
            FieldDef::new("pages", "Pages", FieldType::Number),
        ]);
    store.put_kind(&kind).expect("put_kind");
    kind
}

fn seeded_item(store: &dyn LibraryStore, kind: KindId, title: &str) -> Item {
    let item = Item::new(kind, title);
    store.put_item(&item).expect("put_item");
    item
}

fn library_starts_empty(store: &dyn LibraryStore) {
    assert!(store.list_kinds().unwrap().is_empty(), "a fresh store must have no kinds");
    assert!(
        store.list_items(&ItemQuery::default()).unwrap().is_empty(),
        "a fresh store must have no items"
    );
    assert!(store.list_logs(&LogQuery::default()).unwrap().is_empty(), "a fresh store has no log");
}

fn kind_crud(store: &dyn LibraryStore) {
    let mut kind = seeded_kind(store, "crud");
    kind.icon = "\u{1f4d9}".into();
    kind.color = "#0f766e".into();
    kind.progress_unit = "page".into();
    kind.source = "openLibrary".into();
    kind.sort_order = 3;
    kind.visible = false;
    kind.builtin = true;
    store.put_kind(&kind).expect("put_kind");

    let back = store.get_kind(kind.id).expect("get_kind");
    assert_eq!(back, kind, "a kind must survive the round trip unchanged");
    assert_eq!(back.fields.len(), 2, "a kind's field definitions must persist");
    assert_eq!(back.verbs.active, "Reading");
    assert_eq!(store.list_kinds().unwrap().len(), 1);

    // Idempotent: writing the same id twice updates rather than duplicates.
    kind.name = "Books (renamed)".into();
    store.put_kind(&kind).expect("put_kind again");
    assert_eq!(store.list_kinds().unwrap().len(), 1, "put must not duplicate");
    assert_eq!(store.get_kind(kind.id).unwrap().name, "Books (renamed)");

    store.delete_kind(kind.id).expect("delete_kind");
    assert!(store.list_kinds().unwrap().is_empty());
}

fn missing_library_records_are_not_found(store: &dyn LibraryStore) {
    super::assert_not_found(store.get_kind(KindId::new()));
    super::assert_not_found(store.get_item(ItemId::new()));
    super::assert_not_found(store.get_log(LogId::new()));
    assert_eq!(
        store.count_items(KindId::new()).expect("count_items"),
        (0, 0),
        "a shelf that does not exist holds nothing, and asking is not an error",
    );
}

fn item_round_trips_every_field(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "round-trip");
    let mut item = Item::new(kind.id, "Dune");
    item.subtitle = "Book one".into();
    item.creator = "Frank Herbert".into();
    item.year = Some(1965);
    item.status = ItemStatus::Done;
    item.rating = Some(90);
    item.external = vec![ExternalRating {
        source: "Open Library".into(),
        score: 84,
        count: Some(1204),
        url: "https://openlibrary.org/works/OL893415W".into(),
    }];
    item.cover = Some(BlobId::of(b"a cover"));
    item.cover_url = "https://covers.openlibrary.org/b/id/1.jpg".into();
    item.summary = "A desert planet.".into();
    item.notes = "Lent to Ana.".into();
    item.tags = vec!["sci-fi".into(), "reread".into()];
    item.facts.insert("author".into(), "Frank Herbert".into());
    item.facts.insert("pages".into(), "412".into());
    item.links = vec![Link { label: "Open Library".into(), url: "https://ol.example".into() }];
    item.progress = Some(Progress::new(412, Some(412), "page"));
    item.favourite = true;
    item.started_on = Some(date(2026, 3, 3));
    item.finished_on = Some(date(2026, 4, 2));
    item.source = "openLibrary".into();
    item.sort_order = 7;

    store.put_item(&item).expect("put_item");
    let back = store.get_item(item.id).expect("get_item");
    assert_eq!(back, item, "an item must survive the round trip unchanged");

    // Idempotent, like every other `put` in this crate.
    store.put_item(&item).expect("put_item again");
    assert_eq!(store.list_items(&ItemQuery::on_shelf(kind.id)).unwrap().len(), 1);

    store.delete_kind(kind.id).expect("delete_kind");
}

fn batch_item_writes_land_together(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "batch");
    let mut items: Vec<Item> = ["a", "b", "c"].iter().map(|t| Item::new(kind.id, *t)).collect();
    store.put_items(&items).expect("put_items");
    assert_eq!(store.list_items(&ItemQuery::on_shelf(kind.id)).unwrap().len(), 3);

    // What a re-ordered shelf is: one write for many rows.
    for (i, item) in items.iter_mut().enumerate() {
        item.sort_order = 10 - i as i32;
    }
    store.put_items(&items).expect("put_items again");
    let back = store
        .list_items(&ItemQuery { sort: ItemSort::Manual, ..ItemQuery::on_shelf(kind.id) })
        .unwrap();
    assert_eq!(
        back.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["c", "b", "a"],
        "a batch write must land in full",
    );
    // An empty batch is a no-op rather than an error.
    store.put_items(&[]).expect("put_items with none");

    store.delete_kind(kind.id).expect("delete_kind");
}

fn listing_items_filters_and_sorts(store: &dyn LibraryStore) {
    let books = seeded_kind(store, "filter-books");
    let films = seeded_kind(store, "filter-films");

    let mut dune = Item::new(books.id, "Dune");
    dune.creator = "Frank Herbert".into();
    dune.status = ItemStatus::Done;
    dune.rating = Some(90);
    dune.tags = vec!["Sci-Fi".into()];
    dune.finished_on = Some(date(2026, 4, 2));
    dune.year = Some(1965);

    let mut pnin = Item::new(books.id, "Pnin");
    pnin.creator = "Vladimir Nabokov".into();
    pnin.status = ItemStatus::Wishlist;
    pnin.year = Some(1957);

    let mut arrival = Item::new(films.id, "Arrival");
    arrival.status = ItemStatus::Done;
    arrival.rating = Some(95);
    arrival.finished_on = Some(date(2025, 11, 1));

    store.put_items(&[dune.clone(), pnin.clone(), arrival.clone()]).expect("put_items");

    let on_shelf = store.list_items(&ItemQuery::on_shelf(books.id)).unwrap();
    assert_eq!(on_shelf.len(), 2, "the shelf filter must exclude other kinds");

    let done = store
        .list_items(&ItemQuery { statuses: vec![ItemStatus::Done], ..Default::default() })
        .unwrap();
    assert_eq!(done.len(), 2, "the status filter spans every shelf when no kind is given");

    // Text search reaches the creator, and ignores case.
    let by_author =
        store.list_items(&ItemQuery { text: "NABOKOV".into(), ..Default::default() }).unwrap();
    assert_eq!(by_author.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["Pnin"]);

    // Tags, likewise.
    let tagged =
        store.list_items(&ItemQuery { tags: vec!["sci-fi".into()], ..Default::default() }).unwrap();
    assert_eq!(tagged.len(), 1);

    // An unrated item must not pass a lower bound on the rating.
    let good =
        store.list_items(&ItemQuery { rating_at_least: Some(80), ..Default::default() }).unwrap();
    assert_eq!(good.len(), 2, "an unrated item is not a zero-rated one");

    // The year-in-review window.
    let this_year = store
        .list_items(&ItemQuery {
            finished_from: Some(date(2026, 1, 1)),
            finished_to: Some(date(2026, 12, 31)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(this_year.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["Dune"]);

    // Sorting: best first, with the unrated last rather than first.
    let by_rating =
        store.list_items(&ItemQuery { sort: ItemSort::RatingDesc, ..Default::default() }).unwrap();
    assert_eq!(
        by_rating.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["Arrival", "Dune", "Pnin"],
    );
    let by_year =
        store.list_items(&ItemQuery { sort: ItemSort::YearAsc, ..Default::default() }).unwrap();
    assert_eq!(by_year[0].title, "Pnin", "1957 comes before 1965");
    assert_eq!(by_year[2].title, "Arrival", "an unknown year sorts last");

    store.delete_kind(books.id).expect("delete_kind");
    store.delete_kind(films.id).expect("delete_kind");
}

fn listing_items_paginates(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "paginate");
    let items: Vec<Item> =
        ["e", "d", "c", "b", "a"].iter().map(|t| Item::new(kind.id, *t)).collect();
    store.put_items(&items).expect("put_items");

    let page = store
        .list_items(&ItemQuery {
            sort: ItemSort::TitleAsc,
            offset: 1,
            limit: Some(2),
            ..ItemQuery::on_shelf(kind.id)
        })
        .unwrap();
    assert_eq!(page.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["b", "c"]);

    let past_the_end =
        store.list_items(&ItemQuery { offset: 99, ..ItemQuery::on_shelf(kind.id) }).unwrap();
    assert!(past_the_end.is_empty(), "paging past the end yields nothing, not an error");

    store.delete_kind(kind.id).expect("delete_kind");
}

fn shelf_counts_are_answered_without_decrypting_everything(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "counts");
    let mut items = Vec::new();
    for status in ItemStatus::ALL {
        let mut item = Item::new(kind.id, status.as_str());
        item.status = status;
        items.push(item);
    }
    store.put_items(&items).expect("put_items");

    // Five statuses, three of which are still ahead of you.
    assert_eq!(store.count_items(kind.id).expect("count_items"), (5, 3));

    store.delete_kind(kind.id).expect("delete_kind");
}

fn the_log_round_trips_and_queries_by_window(store: &dyn LibraryStore) {
    let kind = seeded_kind(store, "log");
    let dune = seeded_item(store, kind.id, "Dune");
    let pnin = seeded_item(store, kind.id, "Pnin");

    let mut started = LogEntry::new(dune.id, LogEvent::Started, date(2026, 3, 3), "Europe/London");
    started.note = "Picked it up again.".into();
    let mut finished = LogEntry::new(dune.id, LogEvent::Finished, date(2026, 4, 2), "UTC");
    finished.rating = Some(90);
    finished.minutes = Some(640);
    finished.position = Some(412);
    let reread = LogEntry::new(dune.id, LogEvent::Revisited, date(2026, 9, 1), "UTC");
    let other = LogEntry::new(pnin.id, LogEvent::Finished, date(2025, 12, 30), "UTC");
    for log in [&started, &finished, &reread, &other] {
        store.put_log(log).expect("put_log");
    }

    assert_eq!(store.get_log(finished.id).expect("get_log"), finished, "a log must round trip");

    // One item's history, newest first.
    let history = store.list_logs(&LogQuery::for_item(dune.id)).expect("list_logs");
    assert_eq!(
        history.iter().map(|l| l.date).collect::<Vec<_>>(),
        [date(2026, 9, 1), date(2026, 4, 2), date(2026, 3, 3)],
        "a history reads newest first",
    );

    // The year in review: both ways of getting to the end of something, and
    // nothing else.
    let year = store
        .list_logs(&LogQuery::completions(date(2026, 1, 1), date(2026, 12, 31)))
        .expect("list_logs");
    assert_eq!(year.len(), 2, "started is not a completion, and last year is not this one");

    // Idempotent, and a limit is honoured.
    store.put_log(&finished).expect("put_log again");
    let capped = store
        .list_logs(&LogQuery { limit: Some(1), ..LogQuery::for_item(dune.id) })
        .expect("list_logs");
    assert_eq!(capped.len(), 1);

    store.delete_log(started.id).expect("delete_log");
    assert_eq!(store.list_logs(&LogQuery::for_item(dune.id)).unwrap().len(), 2);

    store.delete_kind(kind.id).expect("delete_kind");
}

fn deleting_an_item_takes_its_log_with_it(store: &dyn LibraryStore) {
    // The bug this guards: a log row surviving its item, so the year in
    // review counts a book that is no longer in the vault and cannot be
    // named.
    let kind = seeded_kind(store, "cascade-item");
    let dune = seeded_item(store, kind.id, "Dune");
    let pnin = seeded_item(store, kind.id, "Pnin");
    store
        .put_log(&LogEntry::new(dune.id, LogEvent::Finished, date(2026, 4, 2), "UTC"))
        .expect("put_log");
    let keep = LogEntry::new(pnin.id, LogEvent::Finished, date(2026, 5, 1), "UTC");
    store.put_log(&keep).expect("put_log");

    store.delete_item(dune.id).expect("delete_item");
    assert!(store.get_item(dune.id).is_err(), "the item should be gone");
    assert!(
        store.list_logs(&LogQuery::for_item(dune.id)).unwrap().is_empty(),
        "an item's log must go with it",
    );
    assert_eq!(
        store.list_logs(&LogQuery::default()).unwrap().len(),
        1,
        "one item's deletion must not take another's log",
    );

    store.delete_kind(kind.id).expect("delete_kind");
}

fn deleting_a_kind_takes_its_shelf_with_it(store: &dyn LibraryStore) {
    let doomed = seeded_kind(store, "cascade-kind");
    let spared = seeded_kind(store, "cascade-spared");
    let dune = seeded_item(store, doomed.id, "Dune");
    let pnin = seeded_item(store, spared.id, "Pnin");
    store
        .put_log(&LogEntry::new(dune.id, LogEvent::Finished, date(2026, 4, 2), "UTC"))
        .expect("put_log");
    store
        .put_log(&LogEntry::new(pnin.id, LogEvent::Finished, date(2026, 5, 1), "UTC"))
        .expect("put_log");

    store.delete_kind(doomed.id).expect("delete_kind");
    assert!(store.get_item(dune.id).is_err(), "a shelf takes its items");
    assert!(
        store.list_logs(&LogQuery::for_item(dune.id)).unwrap().is_empty(),
        "a shelf takes its items' logs too",
    );
    assert!(store.get_item(pnin.id).is_ok(), "another shelf must be untouched");
    assert_eq!(store.list_logs(&LogQuery::default()).unwrap().len(), 1);

    store.delete_kind(spared.id).expect("delete_kind");
}

fn unicode_survives_a_library_round_trip(store: &dyn LibraryStore) {
    let mut kind = Kind::new("\u{6f22}\u{5b57}", "\u{6620}\u{753b}", "\u{6620}\u{753b}");
    kind.icon = "\u{1f3ac}".into();
    store.put_kind(&kind).expect("put_kind");

    let mut item = Item::new(kind.id, "\u{1f480} Se\u{00f1}or Babel \u{2014} \u{6f22}\u{5b57}");
    item.creator = "\u{00c1}sd\u{00ed}s \u{00d3}sk".into();
    item.notes =
        "emoji \u{1f4da}\u{1f3c1}, combining a\u{0301}, RTL \u{05e2}\u{05d1}\u{05e8}".into();
    item.facts.insert("author".into(), "\u{6751}\u{4e0a}\u{6625}\u{6a39}".into());
    store.put_item(&item).expect("put_item");

    let back = store.get_item(item.id).expect("get_item");
    assert_eq!(back, item, "unicode must survive sealing and storage");
    assert_eq!(store.get_kind(kind.id).unwrap().name, "\u{6620}\u{753b}");

    // ...and the text filter must find it, which is a different question.
    let found =
        store.list_items(&ItemQuery { text: "\u{6f22}\u{5b57}".into(), ..Default::default() });
    assert_eq!(found.unwrap().len(), 1, "a text filter must match non-ASCII");

    store.delete_kind(kind.id).expect("delete_kind");
}
