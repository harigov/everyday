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

// ---- targets ------------------------------------------------------------
//
// "I want an hour or two a week on piano", "twelve books this year", "no
// more than an hour of TV a day" -- said to the assistant, which has to be
// able to set each up and later say how it is going. The harness's today is
// Tuesday 8 September 2026, so this week is the 7th to the 13th.

mod targets {
    use super::super::support::{call, call_err, vault};
    use everyday_core::library::{Item, Kind, LogEntry, LogEvent};
    use everyday_core::purpose::{Goal, Role};
    use everyday_core::task::{BlockKind, BlockSubject, Project, Task, TimeBlock};

    fn piano(v: &everyday_core::Vault) -> Goal {
        let role = Role::new("Myself");
        v.save_role(&role).unwrap();
        let goal = Goal::new(role.id, "Learn piano");
        v.save_goal(&goal).unwrap();
        goal
    }

    #[test]
    fn time_on_a_goal_is_a_target_the_assistant_can_set_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        let goal = piano(&v);

        let set = call(
            &v,
            "set_target",
            serde_json::json!({
                "measure": "time",
                "goal_id": goal.id.to_string(),
                "per": "week",
                "min": 60,
                "max": 120,
            }),
        );
        assert_eq!(set["name"], "Time on Learn piano");
        assert_eq!(set["source"], "time");
        assert_eq!(set["filed_under"], "goal: Learn piano");
        assert_eq!(set["targets"], serde_json::json!(["between 60 and 120 min a week"]));

        // Forty minutes of practice on Monday, filed once on the project.
        let mut project = Project::new("Piano");
        project.purpose = Some(goal.purpose());
        v.save_project(&project).unwrap();
        let scales = Task::new("Scales").in_project(project.id);
        v.save_task(&scales).unwrap();
        let at = jiff::civil::date(2026, 9, 7).at(19, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
        v.save_block(
            &TimeBlock::new(BlockSubject::Task { id: scales.id }, at, 40, "UTC")
                .of_kind(BlockKind::Actual),
        )
        .unwrap();

        let goals = call(&v, "list_goals", serde_json::json!({ "include_activity": true }));
        let target = &goals["goals"][0]["targets"][0];
        assert_eq!(target["tracker"], "Time on Learn piano");
        assert_eq!(target["target"], "between 60 and 120 min a week");
        assert_eq!(target["from"], "2026-09-07", "a Monday week");
        assert_eq!(target["to"], "2026-09-13");
        assert_eq!(target["so_far"], 40.0);
        assert_eq!(target["standing"], "short");
        assert_eq!(target["on_pace"], 17.1, "two of seven days at an even hour a week");

        // Without asking for activity, the goal list stays as small as before.
        let bare = call(&v, "list_goals", serde_json::json!({}));
        assert!(bare["goals"][0].get("targets").is_none());

        // Time is worked out, not recorded: the assistant cannot log it.
        let id = set["id"].as_str().unwrap();
        let err = call_err(&v, "log_reading", serde_json::json!({ "tracker_id": id, "value": 30 }));
        assert!(err.contains("cannot be recorded by hand"), "got {err}");
    }

    #[test]
    fn a_limit_on_a_new_tracker_is_replaced_by_saying_it_again_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());

        let tv = call(
            &v,
            "set_target",
            serde_json::json!({ "measure": "new", "name": "TV", "unit": "min", "per": "day", "max": 60 }),
        );
        assert_eq!(tv["source"], "manual");
        let id = tv["id"].as_str().unwrap().to_string();

        // A second period is a second target; the same period again replaces.
        call(&v, "set_target", serde_json::json!({ "tracker_id": id, "per": "week", "max": 300 }));
        let again = call(
            &v,
            "set_target",
            serde_json::json!({ "tracker_id": id, "per": "day", "max": 45 }),
        );
        assert_eq!(
            again["targets"],
            serde_json::json!(["at most 300 min a week", "at most 45 min a day"])
        );

        call(&v, "log_reading", serde_json::json!({ "tracker_id": id, "value": 80 }));
        let listed = call(&v, "list_trackers", serde_json::json!({}));
        let row =
            listed["trackers"].as_array().unwrap().iter().find(|t| t["name"] == "TV").unwrap();
        let daily =
            row["targets"].as_array().unwrap().iter().find(|t| t["from"] == t["to"]).unwrap();
        assert_eq!(daily["standing"], "over");
        assert!(daily.get("on_pace").is_none(), "a day has no pace");

        let removed = call(
            &v,
            "set_target",
            serde_json::json!({ "tracker_id": id, "per": "day", "remove": true }),
        );
        assert_eq!(removed["targets"], serde_json::json!(["at most 300 min a week"]));
    }

    #[test]
    fn a_count_of_books_is_measured_off_its_shelf_and_says_what_pace_would_be() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        let books = Kind::new("books", "Books", "Book");
        v.save_kind(&books).unwrap();
        let novel = Item::new(books.id, "A novel");
        v.save_item(&novel).unwrap();
        v.save_log(&LogEntry::new(
            novel.id,
            LogEvent::Finished,
            jiff::civil::date(2026, 3, 1),
            "UTC",
        ))
        .unwrap();

        let set = call(
            &v,
            "set_target",
            serde_json::json!({
                "measure": "finished",
                "shelf_id": books.id.to_string(),
                "per": "year",
                "min": 12,
            }),
        );
        assert_eq!(set["source"], "finished");

        let listed = call(&v, "list_trackers", serde_json::json!({}));
        let target = &listed["trackers"][0]["targets"][0];
        assert_eq!(target["target"], "at least 12 a year");
        assert_eq!(target["so_far"], 1.0);
        assert_eq!(target["from"], "2026-01-01");
        assert_eq!(target["on_pace"], 8.3, "251 of 365 days gone");
    }

    #[test]
    fn saying_a_measure_again_changes_its_target_rather_than_making_a_second_tracker() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        let goal = piano(&v);
        let set = |min: u32| {
            call(
                &v,
                "set_target",
                serde_json::json!({
                    "measure": "time",
                    "goal_id": goal.id.to_string(),
                    "per": "week",
                    "min": min,
                }),
            )
        };
        let first = set(60);
        let again = set(90);
        assert_eq!(first["id"], again["id"], "the same tracker, not a second one");
        assert_eq!(again["targets"], serde_json::json!(["at least 90 min a week"]));
        assert_eq!(v.trackers().unwrap().len(), 1);

        // By name, too, for one recorded by hand.
        let tv = |max: u32| {
            call(
                &v,
                "set_target",
                serde_json::json!({ "measure": "new", "name": "TV", "unit": "min", "per": "day", "max": max }),
            )
        };
        assert_eq!(tv(60)["id"], tv(45)["id"]);

        // And removing through a measure removes from that tracker.
        let removed = call(
            &v,
            "set_target",
            serde_json::json!({
                "measure": "time",
                "goal_id": goal.id.to_string(),
                "per": "week",
                "remove": true,
            }),
        );
        assert_eq!(removed["id"], first["id"]);
        assert_eq!(removed["targets"], serde_json::json!([]));
    }

    #[test]
    fn a_target_is_refused_rather_than_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        // Time on nothing measures nothing.
        let err = call_err(
            &v,
            "set_target",
            serde_json::json!({ "measure": "time", "per": "week", "min": 60 }),
        );
        assert!(err.contains("goal_id"), "got {err}");
        // A target with no number is no target.
        let err = call_err(
            &v,
            "set_target",
            serde_json::json!({ "measure": "new", "name": "TV", "per": "day" }),
        );
        assert!(err.contains("min"), "got {err}");
        // Removing a target from a tracker that does not exist is not a
        // reason to make one.
        let err = call_err(
            &v,
            "set_target",
            serde_json::json!({ "measure": "finished", "per": "year", "remove": true }),
        );
        assert!(err.contains("no tracker"), "got {err}");
        assert!(v.trackers().unwrap().is_empty(), "and nothing was made");
    }
}
