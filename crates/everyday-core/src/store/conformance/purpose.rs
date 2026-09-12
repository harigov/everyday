//! The purpose half of the conformance suite: roles, goals and the reports.

use super::calendars::sample_event;
use super::library::seeded_kind;
use super::*;

/// The purpose half of the suite. Called by [`run_all`] when the backend has
/// a [`PurposeStore`](crate::store::purpose::PurposeStore).
///
/// Handed the whole journal store, because half of what it checks is that
/// *other* domains report their purpose correctly. The store must be empty
/// of roles on entry; it is left empty on success.
pub fn run_purpose_suite(store: &dyn JournalStore) {
    eprintln!("--- purpose conformance suite ---");

    purpose_starts_empty(store);
    role_crud(store);
    missing_purpose_records_are_not_found(store);
    goal_round_trips_and_filters(store);
    batch_goal_writes_land_together(store);
    a_role_with_goals_under_it_refuses_to_be_deleted(store);
    role_goal_counts_are_answered_by_the_backend(store);
    a_purpose_survives_a_round_trip_on_every_record(store);
    time_is_attributed_down_the_inheritance_chain(store);
    unattributed_time_is_reported_rather_than_dropped(store);
    clearing_a_purpose_removes_it_from_the_reports(store);
    deleting_a_record_takes_its_purpose_with_it(store);
    goal_activity_counts_what_points_at_it(store);
    events_are_attributed_by_their_calendar(store);
    unicode_survives_a_purpose_round_trip(store);

    purpose_cleanup(store);
    eprintln!("--- purpose suite passed ---");
}

fn purpose_store(store: &dyn JournalStore) -> &dyn crate::store::purpose::PurposeStore {
    store.purpose().expect("the purpose suite needs a purpose store")
}

fn purpose_cleanup(store: &dyn JournalStore) {
    let p = purpose_store(store);
    for goal in p.list_goals(&GoalQuery::default()).expect("list_goals") {
        p.delete_goal(goal.id).expect("delete_goal");
    }
    for role in p.list_roles().expect("list_roles") {
        p.delete_role(role.id).expect("delete_role");
    }
    assert!(p.list_roles().unwrap().is_empty(), "cleanup left roles behind");
    assert!(p.list_goals(&GoalQuery::default()).unwrap().is_empty(), "cleanup left goals behind");
}

fn seeded_role(store: &dyn JournalStore, name: &str) -> LifeRole {
    let role = LifeRole::new(name);
    purpose_store(store).put_role(&role).expect("put_role");
    role
}

fn seeded_goal(store: &dyn JournalStore, role: &LifeRole, title: &str) -> Goal {
    let goal = Goal::new(role.id, title);
    purpose_store(store).put_goal(&goal).expect("put_goal");
    goal
}

fn purpose_starts_empty(store: &dyn JournalStore) {
    let p = purpose_store(store);
    assert!(p.list_roles().unwrap().is_empty(), "a fresh store must have no roles");
    assert!(p.list_goals(&GoalQuery::default()).unwrap().is_empty(), "a fresh store has no goals");
}

fn role_crud(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let mut role = LifeRole::new("Parent").with_color("#0f766e").with_icon("\u{1f3e1}");
    role.notes = "the one that matters".into();
    role.sort_order = 3;
    p.put_role(&role).unwrap();

    assert_eq!(p.get_role(role.id).unwrap(), role, "a role must survive the round trip whole");
    assert_eq!(p.list_roles().unwrap().len(), 1);

    // Idempotent, as every `put_` in this codebase is.
    p.put_role(&role).unwrap();
    assert_eq!(p.list_roles().unwrap().len(), 1, "putting twice must not make two roles");

    role.name = "Father".into();
    role.archived = true;
    p.put_role(&role).unwrap();
    let back = p.get_role(role.id).unwrap();
    assert_eq!(back.name, "Father");
    assert!(back.archived, "an archived role is still listed; it is only hidden from pickers");
    assert_eq!(p.list_roles().unwrap().len(), 1, "archiving is not deleting");

    p.delete_role(role.id).unwrap();
    assert!(p.list_roles().unwrap().is_empty());
}

fn missing_purpose_records_are_not_found(store: &dyn JournalStore) {
    let p = purpose_store(store);
    // A role that was never written is not found, not an empty one.
    super::assert_not_found(p.get_role(RoleId::new()));
    super::assert_not_found(p.get_goal(GoalId::new()));
    // Deleting what is not there is not an error: it is already true.
    p.delete_goal(GoalId::new()).expect("deleting a missing goal is a no-op");
    p.delete_role(RoleId::new()).expect("deleting a missing role is a no-op");
}

fn goal_round_trips_and_filters(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let parent = seeded_role(store, "Parent");
    let work = seeded_role(store, "Work");

    let mut riding = Goal::new(parent.id, "Viya rides without stabilisers");
    riding.notes = "start on the grass".into();
    riding.horizon = Some(date(2027, 3, 1));
    riding.sort_order = 1;
    p.put_goal(&riding).unwrap();

    let mut shipped = Goal::new(work.id, "ship the thing");
    shipped.set_status(GoalStatus::Done);
    p.put_goal(&shipped).unwrap();

    let mut later = Goal::new(parent.id, "teach her to swim");
    later.set_status(GoalStatus::Paused);
    p.put_goal(&later).unwrap();

    assert_eq!(p.get_goal(riding.id).unwrap(), riding, "every field must survive");

    let all = p.list_goals(&GoalQuery::default()).unwrap();
    assert_eq!(all.len(), 3);

    let under_parent = p.list_goals(&GoalQuery::under(parent.id)).unwrap();
    assert_eq!(under_parent.len(), 2);
    assert!(under_parent.iter().all(|g| g.role_id == parent.id));

    // Paused is open; done is not.
    let open = p.list_goals(&GoalQuery::open()).unwrap();
    assert_eq!(open.len(), 2, "a paused goal is still one you are pursuing");
    assert!(!open.iter().any(|g| g.id == shipped.id));

    // An undated goal is outside every horizon window rather than inside
    // all of them.
    let by_horizon = p
        .list_goals(&GoalQuery { horizon_to: Some(date(2027, 12, 31)), ..Default::default() })
        .unwrap();
    assert_eq!(by_horizon.len(), 1);
    assert_eq!(by_horizon[0].id, riding.id);

    let capped = p.list_goals(&GoalQuery { limit: Some(1), ..Default::default() }).unwrap();
    assert_eq!(capped.len(), 1);

    for g in [riding, shipped, later] {
        p.delete_goal(g.id).unwrap();
    }
    p.delete_role(parent.id).unwrap();
    p.delete_role(work.id).unwrap();
}

fn batch_goal_writes_land_together(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let role = seeded_role(store, "Batch");
    let goals: Vec<Goal> = (0..5).map(|i| Goal::new(role.id, format!("goal {i}"))).collect();
    p.put_goals(&goals).unwrap();
    assert_eq!(p.list_goals(&GoalQuery::under(role.id)).unwrap().len(), 5);

    // An empty batch is a no-op rather than an error, because "save the
    // selection" with nothing selected is a real call.
    p.put_goals(&[]).unwrap();

    for g in goals {
        p.delete_goal(g.id).unwrap();
    }
    p.delete_role(role.id).unwrap();
}

fn a_role_with_goals_under_it_refuses_to_be_deleted(store: &dyn JournalStore) {
    // The one parent in this vault that does not take its children with it.
    // A shelf is what its items are made of; a role is not what a year of
    // attributed hours is made of, and one click is the wrong distance from
    // losing them.
    let p = purpose_store(store);
    let role = seeded_role(store, "Held");
    let goal = seeded_goal(store, &role, "still wanted");

    let refused = p.delete_role(role.id);
    assert!(refused.is_err(), "a role with goals under it must not be deletable");
    assert!(p.get_role(role.id).is_ok(), "the refusal must leave the role alone");
    assert!(p.get_goal(goal.id).is_ok(), "and must certainly leave the goal alone");

    // ...and it becomes deletable once nothing points at it.
    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn role_goal_counts_are_answered_by_the_backend(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let role = seeded_role(store, "Counted");
    assert_eq!(p.count_goals(role.id).unwrap(), (0, 0));

    let a = seeded_goal(store, &role, "open one");
    let mut b = Goal::new(role.id, "finished one");
    b.set_status(GoalStatus::Done);
    p.put_goal(&b).unwrap();
    let mut c = Goal::new(role.id, "paused one");
    c.set_status(GoalStatus::Paused);
    p.put_goal(&c).unwrap();

    assert_eq!(
        p.count_goals(role.id).unwrap(),
        (3, 2),
        "three goals, two of them still being pursued"
    );

    for g in [a.id, b.id, c.id] {
        p.delete_goal(g).unwrap();
    }
    p.delete_role(role.id).unwrap();
}

fn a_purpose_survives_a_round_trip_on_every_record(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let role = seeded_role(store, "Round trip");
    let goal = seeded_goal(store, &role, "the goal");
    let by_goal = goal.purpose();
    let by_role = role.purpose();

    // A project and a task, pointing at a goal.
    if let Some(tasks) = store.tasks() {
        let mut project = Project::new("filed");
        project.purpose = Some(by_goal);
        tasks.put_project(&project).unwrap();
        assert_eq!(tasks.get_project(project.id).unwrap().purpose, Some(by_goal));

        let mut task = Task::new("filed too");
        task.purpose = Some(by_role);
        tasks.put_task(&task).unwrap();
        assert_eq!(
            tasks.get_task(task.id).unwrap().purpose,
            Some(by_role),
            "pointing straight at a role is not a degraded case of pointing at a goal"
        );

        let mut block = TimeBlock::new(
            BlockSubject::Adhoc,
            Timestamp::from_second(1_800_000_000).unwrap(),
            60,
            "UTC",
        );
        block.purpose = Some(by_goal);
        tasks.put_block(&block).unwrap();
        assert_eq!(tasks.get_block(block.id).unwrap().purpose, Some(by_goal));

        tasks.delete_block(block.id).unwrap();
        tasks.delete_task(task.id).unwrap();
        tasks.delete_project(project.id).unwrap();
    }

    // An entry.
    let journal = Journal::new("purpose");
    store.put_journal(&journal).unwrap();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.purpose = Some(by_goal);
    store.put_entry(&entry).unwrap();
    assert_eq!(store.get_entry(entry.id).unwrap().purpose, Some(by_goal));
    store.delete_journal(journal.id).unwrap();

    // A shelf item.
    if let Some(library) = store.library() {
        let kind = seeded_kind(library, "purpose");
        let mut item = Item::new(kind.id, "the book for it");
        item.purpose = Some(by_goal);
        library.put_item(&item).unwrap();
        assert_eq!(library.get_item(item.id).unwrap().purpose, Some(by_goal));
        library.delete_kind(kind.id).unwrap();
    }

    // A subscribed calendar, which carries a role rather than a purpose.
    if let Some(calendars) = store.calendars() {
        let mut cal = Calendar::subscribed("work", "https://example.com/w.ics");
        cal.role_id = Some(role.id);
        calendars.put_calendar(&cal).unwrap();
        assert_eq!(calendars.get_calendar(cal.id).unwrap().role_id, Some(role.id));
        calendars.delete_calendar(cal.id).unwrap();
    }

    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn time_is_attributed_down_the_inheritance_chain(store: &dyn JournalStore) {
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Chain");
    let goal = seeded_goal(store, &role, "the goal");
    let other = seeded_goal(store, &role, "the other goal");

    // A project with a purpose, a task under it with none, and a block under
    // that with none: the block must be attributed to the project's goal.
    let mut project = Project::new("attributed");
    project.purpose = Some(goal.purpose());
    tasks.put_project(&project).unwrap();

    let mut inherits = Task::new("inherits");
    inherits.project_id = Some(project.id);
    tasks.put_task(&inherits).unwrap();

    // ...and a sibling task that overrides it, to prove the block does not
    // simply take the project's in every case.
    let mut overrides = Task::new("overrides");
    overrides.project_id = Some(project.id);
    overrides.purpose = Some(other.purpose());
    tasks.put_task(&overrides).unwrap();

    let day = date(2026, 9, 14);
    let at = day.at(9, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let inherited_block = TimeBlock::new(BlockSubject::Task { id: inherits.id }, at, 60, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&inherited_block).unwrap();

    let overridden_block = TimeBlock::new(BlockSubject::Task { id: overrides.id }, at, 30, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&overridden_block).unwrap();

    // A block with its own purpose, overriding both.
    let mut own = TimeBlock::new(BlockSubject::Task { id: inherits.id }, at, 15, "UTC")
        .of_kind(BlockKind::Planned);
    own.purpose = Some(role.purpose());
    tasks.put_block(&own).unwrap();

    let window = PurposeWindow::new(day, day);
    let rows = p.time_by_purpose(window).unwrap();

    let find = |want: Purpose| {
        rows.iter()
            .find(|r| r.purpose == Some(want))
            .unwrap_or_else(|| panic!("expected a row for {want:?}, got {rows:?}"))
    };

    assert_eq!(
        find(goal.purpose()).actual_minutes,
        60,
        "a block with no purpose and a task with none takes its project's"
    );
    assert_eq!(
        find(other.purpose()).actual_minutes,
        30,
        "a task's own purpose beats the project it is filed under"
    );
    let owned = find(role.purpose());
    assert_eq!(owned.planned_minutes, 15, "a block's own purpose beats everything above it");
    assert_eq!(owned.actual_minutes, 0, "planned and actual are counted apart, never summed");

    // Outside the window nothing is reported, which is what makes the window
    // worth passing at all.
    let elsewhere =
        p.time_by_purpose(PurposeWindow::new(date(2026, 1, 1), date(2026, 1, 2))).unwrap();
    assert!(elsewhere.iter().all(|r| r.actual_minutes == 0 && r.planned_minutes == 0));

    tasks.delete_project(project.id).unwrap();
    p.delete_goal(goal.id).unwrap();
    p.delete_goal(other.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn unattributed_time_is_reported_rather_than_dropped(store: &dyn JournalStore) {
    // Most of a life is not booked against anything, and a report that
    // quietly dropped that share would be flattering rather than useful.
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);

    let day = date(2026, 9, 15);
    let at = day.at(11, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let block = TimeBlock::new(BlockSubject::Adhoc, at, 45, "UTC").of_kind(BlockKind::Actual);
    tasks.put_block(&block).unwrap();

    let rows = p.time_by_purpose(PurposeWindow::new(day, day)).unwrap();
    let none =
        rows.iter().find(|r| r.purpose.is_none()).expect("the unattributed row is always present");
    assert_eq!(none.actual_minutes, 45);
    assert_eq!(none.blocks, 1);

    tasks.delete_block(block.id).unwrap();
}

fn clearing_a_purpose_removes_it_from_the_reports(store: &dyn JournalStore) {
    // Setting a purpose and then unsetting it must leave no trace. The
    // pointer is an index over what the sealed record says, and an index
    // that keeps rows the record no longer claims is an index that lies.
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Cleared");
    let goal = seeded_goal(store, &role, "briefly");

    let day = date(2026, 9, 16);
    let at = day.at(8, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let mut block = TimeBlock::new(BlockSubject::Adhoc, at, 20, "UTC").of_kind(BlockKind::Actual);
    block.purpose = Some(goal.purpose());
    tasks.put_block(&block).unwrap();

    let window = PurposeWindow::new(day, day);
    assert!(p.time_by_purpose(window).unwrap().iter().any(|r| r.purpose == Some(goal.purpose())));

    block.purpose = None;
    tasks.put_block(&block).unwrap();
    assert_eq!(tasks.get_block(block.id).unwrap().purpose, None);

    let rows = p.time_by_purpose(window).unwrap();
    assert!(
        !rows.iter().any(|r| r.purpose == Some(goal.purpose())),
        "an unset purpose must vanish from the report, not linger in the index"
    );
    assert!(rows.iter().any(|r| r.purpose.is_none() && r.actual_minutes == 20));

    tasks.delete_block(block.id).unwrap();
    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn deleting_a_record_takes_its_purpose_with_it(store: &dyn JournalStore) {
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Deleted");
    let goal = seeded_goal(store, &role, "doomed");

    let day = date(2026, 9, 17);
    let at = day.at(8, 0, 0, 0).in_tz("UTC").unwrap().timestamp();

    // A project with a task and a block beneath it, all attributed, then the
    // whole tree removed in one call. Nothing of it may show in the report.
    let mut project = Project::new("doomed project");
    project.purpose = Some(goal.purpose());
    tasks.put_project(&project).unwrap();
    let mut task = Task::new("doomed task");
    task.project_id = Some(project.id);
    tasks.put_task(&task).unwrap();
    let block = TimeBlock::new(BlockSubject::Task { id: task.id }, at, 90, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&block).unwrap();

    let window = PurposeWindow::new(day, day);
    assert!(p.time_by_purpose(window).unwrap().iter().any(|r| r.purpose == Some(goal.purpose())));

    tasks.delete_project(project.id).unwrap();
    let rows = p.time_by_purpose(window).unwrap();
    assert!(
        rows.iter().all(|r| r.purpose != Some(goal.purpose())),
        "a deleted tree must leave no pointer rows behind"
    );
    assert_eq!(p.goal_activity(goal.id).unwrap(), Default::default());

    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn goal_activity_counts_what_points_at_it(store: &dyn JournalStore) {
    let Some(tasks) = store.tasks() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Active");
    let goal = seeded_goal(store, &role, "measured");

    let mut project = Project::new("for the goal");
    project.purpose = Some(goal.purpose());
    tasks.put_project(&project).unwrap();

    let mut open = Task::new("still to do");
    open.project_id = Some(project.id);
    tasks.put_task(&open).unwrap();

    let mut done = Task::new("did it");
    done.project_id = Some(project.id);
    done.set_status(TaskStatus::Done);
    tasks.put_task(&done).unwrap();

    let at = date(2026, 9, 18).at(8, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
    let logged = TimeBlock::new(BlockSubject::Task { id: open.id }, at, 120, "UTC")
        .of_kind(BlockKind::Actual);
    tasks.put_block(&logged).unwrap();
    // Planned time is an intention and must not be counted as effort spent.
    let planned = TimeBlock::new(BlockSubject::Task { id: open.id }, at, 240, "UTC");
    tasks.put_block(&planned).unwrap();

    let activity = p.goal_activity(goal.id).unwrap();
    assert_eq!(activity.projects, 1);
    assert_eq!(activity.open_tasks, 1);
    assert_eq!(activity.done_tasks, 1);
    assert_eq!(activity.actual_minutes, 120, "only what actually happened counts as hours");
    assert!(!activity.is_empty());
    assert!(activity.last_touched.is_some(), "something happened, so there is a latest moment");

    // A goal nothing points at reports nothing rather than failing.
    let untouched = seeded_goal(store, &role, "nothing yet");
    let empty = p.goal_activity(untouched.id).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.last_touched, None);

    tasks.delete_project(project.id).unwrap();
    p.delete_goal(goal.id).unwrap();
    p.delete_goal(untouched.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn events_are_attributed_by_their_calendar(store: &dyn JournalStore) {
    let Some(calendars) = store.calendars() else { return };
    let p = purpose_store(store);
    let role = seeded_role(store, "Meetings");

    let mut cal = Calendar::subscribed("work", "https://example.com/work.ics");
    cal.role_id = Some(role.id);
    calendars.put_calendar(&cal).unwrap();

    let mut unfiled = Calendar::subscribed("holidays", "https://example.com/hol.ics");
    unfiled.role_id = None;
    calendars.put_calendar(&unfiled).unwrap();

    // `sample_event` runs 09:00 to 10:00 on its first day, so each of these
    // is one hour and the two-day one is twenty-five.
    let day = date(2026, 9, 19);
    calendars.replace_events(cal.id, &[sample_event(cal.id, "purpose-1", day, day)]).unwrap();
    calendars
        .replace_events(unfiled.id, &[sample_event(unfiled.id, "purpose-2", day, day)])
        .unwrap();

    let rows = p.events_by_role(PurposeWindow::new(day, day)).unwrap();
    let mine = rows
        .iter()
        .find(|r| r.role_id == Some(role.id))
        .expect("a feed with a role reports under it");
    assert_eq!(mine.minutes, 60);
    assert_eq!(mine.events, 1);

    let theirs = rows
        .iter()
        .find(|r| r.role_id.is_none())
        .expect("a feed with no role is reported, not dropped");
    assert_eq!(theirs.minutes, 60);

    calendars.delete_calendar(cal.id).unwrap();
    calendars.delete_calendar(unfiled.id).unwrap();
    p.delete_role(role.id).unwrap();
}

fn unicode_survives_a_purpose_round_trip(store: &dyn JournalStore) {
    let p = purpose_store(store);
    let mut role = LifeRole::new("親 · роль · 🧭");
    role.notes = "\u{1f3e1} home".into();
    p.put_role(&role).unwrap();
    assert_eq!(p.get_role(role.id).unwrap().name, "親 · роль · 🧭");

    let mut goal = Goal::new(role.id, "Viya lernt Fahrrad fahren 🚲");
    goal.notes = "auf dem Gras anfangen".into();
    p.put_goal(&goal).unwrap();
    assert_eq!(p.get_goal(goal.id).unwrap(), goal);

    p.delete_goal(goal.id).unwrap();
    p.delete_role(role.id).unwrap();
}
