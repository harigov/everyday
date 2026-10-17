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
    // no type on a place, no shelf of people or of drinks, and a recipe with
    // nothing to cook from.
    for mut kind in everyday_core::library::default_kinds() {
        kind.revision = 0;
        match kind.slug.as_str() {
            "contact" | "drink" => continue,
            "film" => (kind.name, kind.singular) = ("Films".into(), "Film".into()),
            "series" => (kind.name, kind.singular) = ("Series".into(), "Series".into()),
            "place" => kind.fields.retain(|f| f.key != "type"),
            "recipe" => {
                kind.fields.retain(|f| !["ingredients", "method"].contains(&f.key.as_str()))
            }
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
    assert_eq!(names[names.len() - 2..], ["Contacts", "Drinks"], "got {names:?}");
    let place = vault.kinds().unwrap().into_iter().find(|k| k.slug == "place").unwrap();
    assert_eq!(place.fields[0].key, "type");
    let recipe = vault.kinds().unwrap().into_iter().find(|k| k.slug == "recipe").unwrap();
    assert!(recipe.fields.iter().any(|f| f.key == "method"), "a recipe can be cooked from");

    // Deleting the new shelf is a decision the next open respects.
    let contacts = vault.kinds().unwrap().into_iter().find(|k| k.slug == "contact").unwrap();
    vault.delete_kind(contacts.id).unwrap();
    assert_eq!(vault.seed_library().unwrap(), 0);
    assert!(vault.kinds().unwrap().iter().all(|k| k.slug != "contact"));
}

/// The shelf a test wants, by its slug, from what list_shelves says.
fn shelf(vault: &everyday_core::Vault, slug: &str) -> String {
    let id = vault.kinds().unwrap().into_iter().find(|k| k.slug == slug).unwrap().id;
    id.to_string()
}

#[test]
fn a_recipe_is_saved_with_its_ingredients_and_read_back_whole() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();

    // The shelf says what it keeps, so a model knows the keys to use.
    let shelves = call(&vault, "list_shelves", serde_json::json!({}));
    let recipes = shelves.as_array().unwrap().iter().find(|s| s["name"] == "Recipes").unwrap();
    let ingredients =
        recipes["fields"].as_array().unwrap().iter().find(|f| f["key"] == "ingredients").unwrap();
    assert_eq!(ingredients["type"], "list");

    let id = call(
        &vault,
        "create_item",
        serde_json::json!({
            "shelf_id": shelf(&vault, "recipe"),
            "title": "Cacio e pepe",
            "fields": {
                "cuisine": "Italian",
                "serves": "2",
                // A list sent as a list is kept one to a line.
                "ingredients": ["200g spaghetti", "100g pecorino", "black pepper"],
                "method": "Cook the pasta.\nToast the pepper.\nMake the sauce off the heat.",
            },
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    let full = call(&vault, "get_item", serde_json::json!({ "item_id": id }));
    assert_eq!(full["shelf"], "Recipes");
    assert_eq!(full["fields"]["ingredients"], "200g spaghetti\n100g pecorino\nblack pepper");
    assert_eq!(full["fields"]["method"].as_str().unwrap().lines().count(), 3);

    // A listing carries the short fields and leaves the method to get_item.
    let listed = call(&vault, "list_items", serde_json::json!({ "text": "cacio" }));
    let row = &listed["items"][0];
    assert_eq!(row["fields"]["cuisine"], "Italian");
    assert!(row["fields"].get("method").is_none(), "got {row}");

    // An update names only what it changes, and an empty value clears.
    call(
        &vault,
        "update_item",
        serde_json::json!({ "item_id": id, "fields": { "serves": "4", "cuisine": "" } }),
    );
    let after = call(&vault, "get_item", serde_json::json!({ "item_id": id }));
    assert_eq!(after["fields"]["serves"], "4");
    assert!(after["fields"].get("cuisine").is_none(), "got {after}");
    assert!(after["fields"].get("ingredients").is_some(), "an unnamed field was touched");
}

#[test]
fn a_field_the_shelf_does_not_have_is_refused_with_the_ones_it_does() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();

    let err = call_err(
        &vault,
        "create_item",
        serde_json::json!({
            "shelf_id": shelf(&vault, "drink"),
            "title": "Negroni",
            "fields": { "glassware": "rocks" },
        }),
    );
    assert!(err.contains("`glassware`") && err.contains("ingredients"), "got {err}");
    assert!(vault.items(&everyday_core::ItemQuery::default()).unwrap().is_empty());
}
