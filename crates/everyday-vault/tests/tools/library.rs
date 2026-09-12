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
