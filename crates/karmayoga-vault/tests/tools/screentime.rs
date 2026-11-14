//! Screen time, through the tools a model actually has.
//!
//! Every test starts from the same morning on one laptop: half an hour in
//! an editor, twenty minutes in Firefox of which a quarter of an hour was
//! YouTube, recorded with window titles kept and a device name that says
//! more than it should -- neither of which any answer may carry unless it
//! is asked for the device.
//!
//! Heartbeats are refused when they are more than thirty days old, so the
//! morning is a few hours before the real clock and every call names its
//! dates, rather than leaning on the fixed `TODAY` the other tool tests
//! pretend it is.

use super::support::{call, call_err, vault};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::agent::tools::{self, Domain, Effect};
use karmayoga_core::screentime::{Heartbeat, HeartbeatBatch, SpanKind};
use karmayoga_core::{TrackerSource, Vault};
use serde_json::{Value, json};

const DEVICE: &str = "Hari's work MacBook";
const TITLE: &str = "SECRET-TITLE quarterly numbers.xlsx";

fn beat(kind: SpanKind, app: &str, host: &str, at: Timestamp, from: i64, to: i64) -> Heartbeat {
    Heartbeat {
        kind,
        app: app.into(),
        name: String::new(),
        host: host.into(),
        title: TITLE.into(),
        start: at + SignedDuration::from_secs(from),
        end: Some(at + SignedDuration::from_secs(to)),
    }
}

/// The morning, and the UTC day it happened on.
fn seed(vault: &Vault) -> Date {
    vault.ensure_screen_defaults().unwrap();
    let at = Timestamp::from_second(Timestamp::now().as_second() - 3 * 3600).unwrap();
    let send = |collector: &str, beats: Vec<Heartbeat>| {
        vault
            .record_heartbeats(&HeartbeatBatch {
                device: DEVICE.into(),
                collector: collector.into(),
                beats,
            })
            .unwrap();
    };
    send(
        "macos",
        vec![
            beat(SpanKind::App, "code", "", at, 0, 1800),
            beat(SpanKind::App, "firefox", "", at, 1800, 3000),
        ],
    );
    send("firefox", vec![beat(SpanKind::Site, "Firefox", "www.youtube.com", at, 2100, 3000)]);
    at.to_zoned(TimeZone::UTC).date()
}

/// The day before the morning to the day after: wide enough to hold it
/// whatever the hour, in the UTC the tool tests run in.
fn around(day: Date) -> Value {
    json!({
        "from": day.yesterday().unwrap().to_string(),
        "to": day.tomorrow().unwrap().to_string(),
    })
}

fn with(mut args: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

fn item<'a>(answer: &'a Value, key: &str) -> &'a Value {
    answer["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("no {key} in {answer}"))
}

#[test]
fn screen_time_is_a_read_in_its_own_domain() {
    let tool = tools::find("screen_time").unwrap();
    assert_eq!(tool.effect, Effect::Read);
    assert_eq!(tool.domain, Domain::ScreenTime);
}

#[test]
fn the_answer_adds_up_by_item_and_category_and_compares_with_before() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = seed(&vault);

    let answer = call(&vault, "screen_time", around(day));
    assert_eq!(answer["total_minutes"], 50);
    assert_eq!(answer["previous_total_minutes"], 0);

    let code = item(&answer, "app:code");
    assert_eq!(code["minutes"], 30);
    assert_eq!(code["kind"], "app");
    assert_eq!(code["untrusted_text"]["name"], "Visual Studio Code");
    assert!(code.get("name").is_none(), "a name is only ever under untrusted_text");
    assert_eq!(code["category"], "Work");

    let youtube = item(&answer, "site:youtube.com");
    assert_eq!(youtube["minutes"], 15);
    assert_eq!(youtube["kind"], "site");
    assert_eq!(youtube["category"], "Entertainment");

    let firefox = item(&answer, "app:firefox");
    assert_eq!(firefox["minutes"], 5, "what YouTube left of the browser's twenty minutes");
    assert_eq!(firefox["browser_other"], true);
    assert!(firefox["category"].is_null());

    let categories = answer["categories"].as_array().unwrap();
    assert_eq!(categories[0], json!({ "category": "Work", "minutes": 30 }));
    assert!(categories.contains(&json!({ "category": "uncategorized", "minutes": 5 })));

    let days = answer["days"].as_array().unwrap();
    assert_eq!(days.len(), 3, "a multi-day range is laid out day by day");
    let summed: u64 = days.iter().map(|d| d["minutes"].as_u64().unwrap()).sum();
    assert!((49..=51).contains(&summed), "{summed}");
}

#[test]
fn no_title_is_ever_handed_over_and_the_device_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = seed(&vault);

    let plain = call(&vault, "screen_time", around(day)).to_string();
    assert!(!plain.contains("SECRET-TITLE"), "{plain}");
    assert!(!plain.contains("MacBook"), "{plain}");

    let by_device = call(&vault, "screen_time", with(around(day), json!({ "by_device": true })));
    assert!(!by_device.to_string().contains("SECRET-TITLE"));
    assert_eq!(by_device["devices"], json!([{ "device": DEVICE, "minutes": 50 }]));
}

#[test]
fn an_answer_narrows_to_a_category_or_an_item() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = seed(&vault);

    let fun =
        call(&vault, "screen_time", with(around(day), json!({ "category": "entertainment" })));
    assert_eq!(fun["total_minutes"], 15);

    let none =
        call(&vault, "screen_time", with(around(day), json!({ "category": "uncategorized" })));
    assert_eq!(none["total_minutes"], 5);
    assert_eq!(none["items"].as_array().unwrap().len(), 1);

    let err = call_err(&vault, "screen_time", with(around(day), json!({ "category": "Hobbies" })));
    assert!(err.contains("Social"), "the real names are listed: {err}");

    for named in ["site:youtube.com", "youtube.com", "https://www.youtube.com/watch?v=x"] {
        let one = call(&vault, "screen_time", with(around(day), json!({ "item": named })));
        assert_eq!(one["total_minutes"], 15, "{named}");
    }
    let by_name =
        call(&vault, "screen_time", with(around(day), json!({ "item": "visual studio code" })));
    assert_eq!(by_name["total_minutes"], 30);

    let nothing = call(&vault, "screen_time", with(around(day), json!({ "item": "Photoshop" })));
    assert_eq!(nothing["total_minutes"], 0);
    assert!(nothing["note"].as_str().unwrap().contains("Photoshop"));
}

#[test]
fn one_day_has_no_day_by_day_and_a_limit_says_how_many_more() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = seed(&vault);

    // One day has no day-by-day. Which day the seed lands on depends on
    // the zone the tests run in, so only the shape is asked of it.
    let one =
        call(&vault, "screen_time", json!({ "from": day.to_string(), "to": day.to_string() }));
    assert!(one.get("days").is_none());

    // The window around it holds all three items whatever the zone.
    let limited = call(&vault, "screen_time", with(around(day), json!({ "limit": 1 })));
    assert_eq!(limited["items"].as_array().unwrap().len(), 1);
    assert_eq!(limited["more_items"], 2, "{limited}");

    let err =
        call_err(&vault, "screen_time", json!({ "from": "2024-01-01", "to": day.to_string() }));
    assert!(err.contains("400"), "{err}");
}

#[test]
fn set_target_measures_screen_time() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    seed(&vault);

    let social = call(
        &vault,
        "set_target",
        json!({ "measure": "screen", "screen_category": "social", "per": "day", "max": 60 }),
    );
    assert_eq!(social["source"], "screenTime");
    assert_eq!(social["name"], "Screen time: Social");
    let tracker = vault.tracker(social["id"].as_str().unwrap().parse().unwrap()).unwrap();
    assert_eq!(tracker.unit, "min");
    let social_id = vault.screen_categories().unwrap().into_iter().find(|c| c.name == "Social");
    assert_eq!(
        tracker.source,
        TrackerSource::ScreenTime { category: social_id.map(|c| c.id), item: None }
    );

    // Saying it again changes the same tracker's target.
    let again = call(
        &vault,
        "set_target",
        json!({ "measure": "screen", "screen_category": "Social", "per": "day", "max": 45 }),
    );
    assert_eq!(again["id"], social["id"]);

    // An app or site is pointed at by its key, not a label: resetting the
    // label must not cost the goal what it counts.
    let youtube = call(
        &vault,
        "set_target",
        json!({ "measure": "screen", "screen_item": "youtube.com", "per": "week", "max": 300 }),
    );
    assert_eq!(youtube["name"], "Screen time: youtube.com");
    let tracker = vault.tracker(youtube["id"].as_str().unwrap().parse().unwrap()).unwrap();
    assert_eq!(
        tracker.source,
        TrackerSource::ScreenTime { category: None, item: Some("site:youtube.com".into()) }
    );
    let again = call(
        &vault,
        "set_target",
        json!({ "measure": "screen", "screen_item": "site:youtube.com", "per": "day", "max": 60 }),
    );
    assert_eq!(again["id"], youtube["id"], "the same key, so the same tracker");

    let err = call_err(
        &vault,
        "set_target",
        json!({ "measure": "screen", "screen_category": "Gardening", "per": "day", "max": 60 }),
    );
    assert!(err.contains("Gardening"), "{err}");
}
