//! Trackers and readings, through the tools a model actually has.

use super::support::{call, vault};

#[test]
fn readings_can_be_lined_up_against_each_other_day_by_day() {
    // The question this tool exists for: "pull up the days I spent time
    // in the pool and tell me whether my mood improved the day after".
    // `tracker_summary` cannot answer it -- it returns one number per
    // day per tracker, already aggregated, with no way to walk two
    // series against each other.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let swim = everyday_core::Tracker::new("Swimming", everyday_core::TrackerKind::Amount)
        .with_unit("min");
    vault.save_tracker(&swim).unwrap();
    let mut mood = everyday_core::Tracker::new("Mood", everyday_core::TrackerKind::Scale);
    mood.scale_max = 10.0;
    vault.save_tracker(&mood).unwrap();

    // Swam on the 1st and the 5th; mood every day.
    for day in [1, 5] {
        call(
            &vault,
            "log_reading",
            serde_json::json!({
                "tracker_id": swim.id.to_string(),
                "value": 60,
                "date": format!("2026-09-0{day}"),
            }),
        );
    }
    for (day, value) in [(1, 5), (2, 8), (3, 6), (4, 5), (5, 4), (6, 9)] {
        call(
            &vault,
            "log_reading",
            serde_json::json!({
                "tracker_id": mood.id.to_string(),
                "value": value,
                "date": format!("2026-09-0{day}"),
            }),
        );
    }

    let all = call(
        &vault,
        "list_readings",
        serde_json::json!({ "from": "2026-09-01", "to": "2026-09-06" }),
    );
    assert_eq!(all["count"], 8);
    // Every row names its tracker in words, so a model can reason about
    // "swimming" rather than about a uuid.
    let rows = all["readings"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["tracker"] == "Swimming"));
    assert!(rows.iter().any(|r| r["tracker"] == "Mood"));

    // One tracker at a time is what actually makes the comparison
    // possible: two calls, two series, aligned on the date.
    let swims = call(
        &vault,
        "list_readings",
        serde_json::json!({
            "tracker_id": swim.id.to_string(),
            "from": "2026-09-01",
            "to": "2026-09-06",
        }),
    );
    let days: Vec<&str> =
        swims["readings"].as_array().unwrap().iter().map(|r| r["date"].as_str().unwrap()).collect();
    assert_eq!(days, ["2026-09-01", "2026-09-05"]);

    let moods = call(
        &vault,
        "list_readings",
        serde_json::json!({
            "tracker_id": mood.id.to_string(),
            "from": "2026-09-01",
            "to": "2026-09-06",
        }),
    );
    let after: Vec<f64> = moods["readings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| matches!(r["date"].as_str(), Some("2026-09-02") | Some("2026-09-06")))
        .map(|r| r["value"].as_f64().unwrap())
        .collect();
    assert_eq!(after, [8.0, 9.0], "the day after each swim is reachable");

    // A reading logged for a past day carries no time of day, so an
    // hour-of-day filter excludes it rather than averaging in a
    // defaulted instant.
    let timed = call(
        &vault,
        "list_readings",
        serde_json::json!({ "from": "2026-09-01", "to": "2026-09-06", "timed_only": true }),
    );
    assert_eq!(timed["count"], 0, "a day written up later knows no minute");
}

#[test]
fn a_reading_reports_the_value_that_was_stored_rather_than_the_one_asked_for() {
    // The vault clamps to the tracker's scale. If the tool echoed the
    // argument, the assistant would tell somebody it recorded a 99 when
    // the vault holds a 10.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut pain = everyday_core::Tracker::new("Headache", everyday_core::TrackerKind::Scale);
    pain.scale_max = 10.0;
    vault.save_tracker(&pain).unwrap();
    let tracker_id = pain.id.to_string();

    let logged = call(
        &vault,
        "log_reading",
        serde_json::json!({ "tracker_id": tracker_id, "value": 99, "date": "2026-09-08" }),
    );
    assert_eq!(logged["value"], 10.0, "the clamped value is what to report");
    assert_eq!(logged["name"], "Headache");

    let summary = call(
        &vault,
        "tracker_summary",
        serde_json::json!({ "from": "2026-09-01", "to": "2026-09-08" }),
    );
    assert_eq!(summary["count"], 1);
    assert_eq!(summary["days"][0]["value"], 10.0);
    // A severity averages rather than sums: a 3 in the morning and a 3
    // at night is not a 6. The aggregate is named in the reply so the
    // model can say "averaging 10" rather than a bare number.
    assert_eq!(summary["days"][0]["aggregate"], "mean");
}
