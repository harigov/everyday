//! The library, through the tools a model actually has.

use super::support::{TODAY, call, call_err, vault};

#[test]
fn finishing_something_on_a_shelf_writes_the_log_the_year_in_review_reads() {
    // An item marked done without a log line is a book that silently
    // misses the list.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();

    let shelves = call(&vault, "list_shelves", serde_json::json!({}));
    let shelf = shelves[0]["id"].as_str().unwrap().to_string();

    let id = call(
        &vault,
        "create_item",
        serde_json::json!({ "shelf_id": shelf, "title": "Piranesi", "creator": "Susanna Clarke" }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    call(
        &vault,
        "update_item",
        serde_json::json!({ "item_id": id, "status": "done", "rating": 9 }),
    );

    let items = call(&vault, "list_items", serde_json::json!({ "status": "done" }));
    assert_eq!(items["count"], 1);
    assert_eq!(items["items"][0]["rating_out_of_10"], 9.0, "a rating out of ten survives");
    assert_eq!(
        items["items"][0]["finished_on"], "2026-09-08",
        "finishing without a date means today"
    );

    let logs = vault.logs(&everyday_core::LogQuery::default()).unwrap();
    assert_eq!(logs.len(), 1, "the finish must be logged");
    assert_eq!(logs[0].event, everyday_core::LogEvent::Finished);
}

#[test]
fn a_rating_on_the_wrong_scale_is_refused_rather_than_recorded() {
    // 80 and 8 must not silently record wildly different opinions.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();
    let shelf =
        call(&vault, "list_shelves", serde_json::json!({}))[0]["id"].as_str().unwrap().to_string();

    let err = call_err(
        &vault,
        "create_item",
        serde_json::json!({ "shelf_id": shelf, "title": "Dune", "rating": 80 }),
    );
    assert!(err.contains("out of 10"), "got {err}");
}

#[test]
fn moving_something_off_a_shelf_and_back_clears_the_dates_it_never_earned() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();
    let shelf =
        call(&vault, "list_shelves", serde_json::json!({}))[0]["id"].as_str().unwrap().to_string();

    let id =
        call(&vault, "create_item", serde_json::json!({ "shelf_id": shelf, "title": "Piranesi" }))
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
    let item_id: everyday_core::ItemId = id.parse().unwrap();

    call(&vault, "update_item", serde_json::json!({ "item_id": id, "status": "active" }));
    assert_eq!(
        vault.item(item_id).unwrap().started_on,
        Some(TODAY),
        "starting something records when"
    );

    call(&vault, "update_item", serde_json::json!({ "item_id": id, "status": "done" }));
    assert_eq!(vault.item(item_id).unwrap().finished_on, Some(TODAY));

    call(&vault, "update_item", serde_json::json!({ "item_id": id, "status": "wishlist" }));
    let back = vault.item(item_id).unwrap();
    assert!(back.finished_on.is_none(), "it is not still finished this year");
    assert!(back.started_on.is_none(), "nor still started");

    // The history the year-in-review reads: started, finished, and
    // nothing for the correction back to the wishlist.
    let events: Vec<everyday_core::LogEvent> =
        vault.logs(&everyday_core::LogQuery::default()).unwrap().iter().map(|l| l.event).collect();
    assert_eq!(events.len(), 2, "got {events:?}");
    assert!(events.contains(&everyday_core::LogEvent::Started));
    assert!(events.contains(&everyday_core::LogEvent::Finished));
}

#[test]
fn a_library_seeded_by_an_older_build_is_brought_up_to_date_on_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // The shelves as an older build left them: unstamped, Films and Series,
    // no type on a place and no shelf of people.
    for mut kind in everyday_core::library::default_kinds() {
        kind.revision = 0;
        match kind.slug.as_str() {
            "contact" => continue,
            "film" => (kind.name, kind.singular) = ("Films".into(), "Film".into()),
            "series" => (kind.name, kind.singular) = ("Series".into(), "Series".into()),
            "place" => kind.fields.retain(|f| f.key != "type"),
            _ => {}
        }
        vault.save_kind(&kind).unwrap();
    }

    vault.seed_library().unwrap();
    let names: Vec<String> = call(&vault, "list_shelves", serde_json::json!({}))
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"Movies".to_string()), "got {names:?}");
    assert!(names.contains(&"TV Shows".to_string()), "got {names:?}");
    assert_eq!(names.last().map(String::as_str), Some("Contacts"), "got {names:?}");
    let place = vault.kinds().unwrap().into_iter().find(|k| k.slug == "place").unwrap();
    assert_eq!(place.fields[0].key, "type");

    // Deleting the new shelf is a decision the next open respects.
    let contacts = vault.kinds().unwrap().into_iter().find(|k| k.slug == "contact").unwrap();
    vault.delete_kind(contacts.id).unwrap();
    assert_eq!(vault.seed_library().unwrap(), 0);
    assert!(vault.kinds().unwrap().iter().all(|k| k.slug != "contact"));
}
