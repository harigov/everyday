//! Areas and goals, through the tools a model actually has.

use super::support::{call, call_err, vault};

#[test]
fn the_assistant_can_file_a_project_and_see_where_the_week_went() {
    // The whole point of the purpose domain, exercised through the tools
    // a model actually has: name an area, name a goal, file one project
    // under it, and read back where the hours went.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let area = jatayu_core::Area::new("Work");
    vault.save_area(&area).unwrap();

    let goal = call(
        &vault,
        "create_goal",
        serde_json::json!({ "area_id": area.id.to_string(), "title": "Ship the rewrite" }),
    );
    let goal_id = goal["id"].as_str().unwrap().to_string();

    let project = jatayu_core::Project::new("The rewrite");
    vault.save_project(&project).unwrap();
    let task = {
        let mut t = jatayu_core::Task::new("write it");
        t.project_id = Some(project.id);
        vault.save_task(&t).unwrap();
        t
    };

    // Filing the *project* is what attributes everything under it. One
    // call rather than one per task, which is the whole argument for
    // inheritance.
    let filed = call(
        &vault,
        "set_purpose",
        serde_json::json!({ "kind": "project", "id": project.id.to_string(), "goal_id": goal_id }),
    );
    assert_eq!(filed["action"], "filed");
    assert_eq!(filed["name"], "The rewrite");

    let day = jiff::civil::date(2026, 9, 14);
    let at = day.at(9, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let block = jatayu_core::TimeBlock::new(
        jatayu_core::BlockSubject::Task { id: task.id },
        at,
        120,
        "UTC",
    )
    .of_kind(jatayu_core::BlockKind::Actual);
    vault.save_block(&block).unwrap();

    let report = call(
        &vault,
        "time_by_area",
        serde_json::json!({ "from": "2026-09-14", "to": "2026-09-14" }),
    );
    let rows = report["areas"].as_array().unwrap();
    let work = rows
        .iter()
        .find(|r| r["area"] == "Work")
        .unwrap_or_else(|| panic!("expected a Work row, got {report}"));
    assert_eq!(work["minutes"], 120, "an hour under a task inherits its project's goal");

    // ...and the goal itself reports it.
    let goals = call(&vault, "list_goals", serde_json::json!({ "include_activity": true }));
    assert_eq!(goals["goals"][0]["activity"]["minutes"], 120);
    assert_eq!(goals["goals"][0]["area"], "Work");

    // Unfiling puts it back where unattributed time goes, rather than
    // leaving a pointer nothing resolves.
    call(
        &vault,
        "set_purpose",
        serde_json::json!({ "kind": "project", "id": project.id.to_string(), "clear": true }),
    );
    let after = call(
        &vault,
        "time_by_area",
        serde_json::json!({ "from": "2026-09-14", "to": "2026-09-14" }),
    );
    let rows = after["areas"].as_array().unwrap();
    assert!(rows.iter().all(|r| r["area"] != "Work"), "got {after}");
    assert!(rows.iter().any(|r| r["area"] == "not filed" && r["minutes"] == 120));
}

#[test]
fn set_purpose_refuses_a_call_that_says_two_things_or_nothing() {
    // Two would be a silent choice between them and none would be a
    // call that looks like it worked and did nothing -- which is the
    // worst outcome for a tool a model cannot see the effect of.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let project = jatayu_core::Project::new("x");
    vault.save_project(&project).unwrap();
    let area = jatayu_core::Area::new("Work");
    vault.save_area(&area).unwrap();
    let goal = jatayu_core::Goal::new(area.id, "g");
    vault.save_goal(&goal).unwrap();

    let both = call_err(
        &vault,
        "set_purpose",
        serde_json::json!({
            "kind": "project",
            "id": project.id.to_string(),
            "goal_id": goal.id.to_string(),
            "area_id": area.id.to_string(),
        }),
    );
    assert!(both.contains("exactly one"), "got {both}");

    let neither = call_err(
        &vault,
        "set_purpose",
        serde_json::json!({ "kind": "project", "id": project.id.to_string() }),
    );
    assert!(neither.contains("exactly one"), "got {neither}");

    // And a goal that does not exist is refused before anything is
    // written, rather than filing the project under nothing.
    let missing = call_err(
        &vault,
        "set_purpose",
        serde_json::json!({
            "kind": "project",
            "id": project.id.to_string(),
            "goal_id": jatayu_core::GoalId::new().to_string(),
        }),
    );
    assert!(!missing.is_empty());
    assert_eq!(vault.project(project.id).unwrap().purpose, None);
}

#[test]
fn a_goal_reports_never_touched_rather_than_saying_nothing() {
    // The most useful thing this domain can tell somebody, so it has to
    // be said rather than left as an absent field a model will not
    // mention.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let area = jatayu_core::Area::new("Myself");
    vault.save_area(&area).unwrap();
    call(
        &vault,
        "create_goal",
        serde_json::json!({ "area_id": area.id.to_string(), "title": "Write every week" }),
    );

    let goals = call(&vault, "list_goals", serde_json::json!({ "include_activity": true }));
    assert_eq!(goals["goals"][0]["activity"]["last_touched"], "never");
    // ...and nothing else is invented to fill the gap.
    assert!(goals["goals"][0]["activity"].get("minutes").is_none());
}

#[test]
fn dropping_a_goal_is_not_finishing_it() {
    // The distinction the whole status set exists for, checked through
    // the tool: a completion date on something you gave up on would
    // poison the one count anybody wants -- how many of the things you
    // set out to do you actually did.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let area = jatayu_core::Area::new("Work");
    vault.save_area(&area).unwrap();
    let made = call(
        &vault,
        "create_goal",
        serde_json::json!({ "area_id": area.id.to_string(), "title": "Hire someone" }),
    );
    let id: jatayu_core::GoalId = made["id"].as_str().unwrap().parse().unwrap();

    call(&vault, "update_goal", serde_json::json!({ "goal_id": id.to_string(), "status": "done" }));
    assert!(vault.goal(id).unwrap().completed_at.is_some());

    call(
        &vault,
        "update_goal",
        serde_json::json!({ "goal_id": id.to_string(), "status": "dropped" }),
    );
    assert_eq!(vault.goal(id).unwrap().completed_at, None);
    assert_eq!(vault.goal(id).unwrap().status, jatayu_core::GoalStatus::Dropped);
}

#[test]
fn a_call_that_still_says_role_id_moves_and_files_by_it() {
    // A pending approval saved before areas were renamed from roles, or a
    // model copying an earlier turn, still says `role_id`. Ignoring it would
    // report success having moved nothing.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let work = jatayu_core::Area::new("Work");
    let home = jatayu_core::Area::new("Home");
    vault.save_area(&work).unwrap();
    vault.save_area(&home).unwrap();

    let goal = call(
        &vault,
        "create_goal",
        serde_json::json!({ "role_id": work.id.to_string(), "title": "Ship the rewrite" }),
    );
    let goal_id = goal["id"].as_str().unwrap().to_string();
    let id = jatayu_core::GoalId::parse(&goal_id).unwrap();
    assert_eq!(vault.goal(id).unwrap().area_id, work.id);

    call(
        &vault,
        "update_goal",
        serde_json::json!({ "goal_id": goal_id, "role_id": home.id.to_string() }),
    );
    assert_eq!(vault.goal(id).unwrap().area_id, home.id, "the old argument was ignored");

    let listed = call(&vault, "list_goals", serde_json::json!({ "role_id": work.id.to_string() }));
    assert_eq!(listed["goals"].as_array().map(Vec::len), Some(0), "got {listed}");
}
