//! The task half of the conformance suite: projects, tasks and time blocks.

use super::*;

/// The task half of the suite. Called by [`run_all`] when the backend has a
/// [`TaskStore`]; public so a backend under construction can run it alone.
///
/// The store must be empty of tasks on entry; it is left empty on success.
pub fn run_task_suite(store: &dyn TaskStore) {
    eprintln!("--- task conformance suite ---");

    tasks_start_empty(store);
    project_crud(store);
    task_round_trips_every_field(store);
    task_put_is_idempotent(store);
    missing_task_records_are_not_found(store);
    list_tasks_filters_and_sorts(store);
    list_tasks_paginates(store);
    batch_writes_land_together(store);
    subtasks_are_reachable_from_their_parent(store);
    deleting_a_task_takes_its_subtrees_with_it(store);
    deleting_a_project_takes_its_tasks_with_it(store);
    time_blocks_round_trip_and_query_by_day(store);
    deleting_a_task_removes_its_time_blocks(store);
    task_stats_reflect_contents(store);
    unicode_survives_a_task_round_trip(store);

    task_cleanup(store);
    eprintln!("--- task suite passed ---");
}

fn task_cleanup(store: &dyn TaskStore) {
    for p in store.list_projects().expect("list_projects") {
        store.delete_project(p.id).expect("delete_project");
    }
    for t in store.list_tasks(&TaskQuery::default()).expect("list_tasks") {
        // Deleting a parent takes its children, so a child may already be
        // gone by the time this reaches it. That is not a failure.
        let _ = store.delete_task(t.id);
    }
    for b in store.list_blocks(&BlockQuery::default()).expect("list_blocks") {
        store.delete_block(b.id).expect("delete_block");
    }
    assert!(store.list_projects().unwrap().is_empty(), "cleanup left projects behind");
    assert!(
        store.list_tasks(&TaskQuery::default()).unwrap().is_empty(),
        "cleanup left tasks behind"
    );
    assert!(
        store.list_blocks(&BlockQuery::default()).unwrap().is_empty(),
        "cleanup left time blocks behind"
    );
}

fn seeded_project(store: &dyn TaskStore, name: &str) -> Project {
    let p = Project::new(name);
    store.put_project(&p).expect("put_project");
    p
}

fn seeded_task(store: &dyn TaskStore, project: Option<ProjectId>, title: &str) -> Task {
    let t = Task::new(title).in_project(project);
    store.put_task(&t).expect("put_task");
    t
}

fn tasks_start_empty(store: &dyn TaskStore) {
    assert!(store.list_projects().unwrap().is_empty(), "a fresh store must have no projects");
    assert!(
        store.list_tasks(&TaskQuery::default()).unwrap().is_empty(),
        "a fresh store must have no tasks"
    );
    assert!(
        store.list_blocks(&BlockQuery::default()).unwrap().is_empty(),
        "a fresh store must have no time blocks"
    );
    assert_eq!(store.task_stats(date(2026, 6, 15)).unwrap().tasks, 0);
}

fn project_crud(store: &dyn TaskStore) {
    let mut p = Project::new("Kitchen").with_color("#0f766e").with_icon("\u{1f6e0}");
    p.notes = "The one that never ends".into();
    p.priority = Priority::High;
    p.start_date = Some(date(2026, 1, 5));
    p.due_date = Some(date(2026, 4, 1));
    p.estimate_minutes = Some(4_800);
    p.tags = vec!["home".into(), "money".into()];
    p.sort_order = 2;
    store.put_project(&p).unwrap();

    assert_eq!(store.get_project(p.id).unwrap(), p, "project must round-trip unchanged");

    p.set_status(ProjectStatus::Done);
    store.put_project(&p).unwrap();
    let got = store.get_project(p.id).unwrap();
    assert_eq!(got.status, ProjectStatus::Done, "put must upsert");
    assert!(got.completed_at.is_some());
    assert_eq!(store.list_projects().unwrap().len(), 1, "upsert must not duplicate");

    // Archived projects are still stored; hiding them is the caller's job.
    p.set_status(ProjectStatus::Archived);
    store.put_project(&p).unwrap();
    assert_eq!(store.list_projects().unwrap().len(), 1, "archiving must not delete");

    store.delete_project(p.id).unwrap();
    assert!(store.get_project(p.id).is_err(), "project must be gone after delete");
    task_cleanup(store);
}

fn task_round_trips_every_field(store: &dyn TaskStore) {
    let p = seeded_project(store, "Move house");
    let parent = seeded_task(store, Some(p.id), "Pack the study");

    let mut t = Task::new("Box up the books").in_project(p.id).under(parent.id);
    t.notes = "Heavy. Use the small boxes.\n\nTwo lines, deliberately.".into();
    t.status = TaskStatus::Doing;
    t.priority = Priority::Urgent;
    t.start_date = Some(date(2026, 5, 1));
    t.due_date = Some(date(2026, 5, 9));
    t.due_time = Some(time(17, 30, 0, 0));
    t.estimate_minutes = Some(120);
    t.tags = vec!["moving".into(), "\u{1f4da}".into()];
    t.sort_order = 7;
    store.put_task(&t).unwrap();

    assert_eq!(store.get_task(t.id).unwrap(), t, "task must round-trip every field unchanged");
    task_cleanup(store);
}

fn task_put_is_idempotent(store: &dyn TaskStore) {
    let mut t = Task::new("Water the plants");
    store.put_task(&t).unwrap();
    store.put_task(&t).unwrap();
    assert_eq!(store.list_tasks(&TaskQuery::default()).unwrap().len(), 1, "put must not duplicate");

    t.title = "Water the plants properly".into();
    store.put_task(&t).unwrap();
    assert_eq!(store.get_task(t.id).unwrap().title, "Water the plants properly");
    assert_eq!(store.list_tasks(&TaskQuery::default()).unwrap().len(), 1);
    task_cleanup(store);
}

fn missing_task_records_are_not_found(store: &dyn TaskStore) {
    super::assert_not_found(store.get_project(ProjectId::new()));
    super::assert_not_found(store.get_task(TaskId::new()));
    super::assert_not_found(store.get_block(BlockId::new()));
    // Deleting something that is not there is a no-op, not an error: two
    // clients racing to tick off the same task should both succeed.
    store.delete_task(TaskId::new()).expect("deleting a missing task must be a no-op");
    store.delete_block(BlockId::new()).expect("deleting a missing block must be a no-op");
    store.delete_project(ProjectId::new()).expect("deleting a missing project must be a no-op");
}

fn list_tasks_filters_and_sorts(store: &dyn TaskStore) {
    let p = seeded_project(store, "Filtering");
    let other = seeded_project(store, "Elsewhere");

    let mut a = Task::new("alpha").in_project(p.id);
    a.due_date = Some(date(2026, 2, 3));
    a.priority = Priority::High;
    a.tags = vec!["Work".into()];
    a.sort_order = 1;

    let mut b = Task::new("bravo").in_project(p.id);
    b.status = TaskStatus::Doing;
    b.due_date = Some(date(2026, 2, 1));
    b.sort_order = 0;

    let mut c = Task::new("charlie").in_project(other.id);
    c.set_status(TaskStatus::Done);

    let d = Task::new("delta"); // inbox, no project, no due date

    for t in [&a, &b, &c, &d] {
        store.put_task(t).unwrap();
    }

    let titles = |q: &TaskQuery| -> Vec<String> {
        store.list_tasks(q).unwrap().into_iter().map(|t| t.title).collect()
    };

    assert_eq!(
        titles(&TaskQuery::in_project(p.id)),
        ["bravo", "alpha"],
        "a project query must be scoped and in manual order"
    );
    assert_eq!(
        titles(&TaskQuery { project: ProjectScope::Inbox, ..Default::default() }),
        ["delta"],
        "the inbox is tasks with no project"
    );
    assert_eq!(
        titles(&TaskQuery { statuses: vec![TaskStatus::Done], ..Default::default() }),
        ["charlie"],
    );
    assert_eq!(titles(&TaskQuery::open()).len(), 3, "open excludes only done and cancelled");
    assert_eq!(
        titles(&TaskQuery { tags: vec!["work".into()], ..Default::default() }),
        ["alpha"],
        "tag filtering must ignore case"
    );
    assert_eq!(
        titles(&TaskQuery { text: "BRAV".into(), ..Default::default() }),
        ["bravo"],
        "text filtering must ignore case"
    );
    assert_eq!(
        titles(&TaskQuery { priority_at_least: Some(Priority::High), ..Default::default() }),
        ["alpha"],
    );
    assert_eq!(
        titles(&TaskQuery { has_due: Some(false), ..Default::default() }),
        ["charlie", "delta"],
        "has_due=false must find the undated ones"
    );
    assert_eq!(
        titles(&TaskQuery {
            due_to: Some(date(2026, 2, 2)),
            sort: TaskSort::DueAsc,
            ..Default::default()
        }),
        ["bravo"],
        "an undated task must not fall inside a date window"
    );
    assert_eq!(
        titles(&TaskQuery { sort: TaskSort::TitleAsc, ..Default::default() }),
        ["alpha", "bravo", "charlie", "delta"],
    );
    assert_eq!(
        titles(&TaskQuery { sort: TaskSort::PriorityDesc, ..Default::default() })[0],
        "alpha",
        "the most important task must come first",
    );
    // Undated tasks sort last under DueAsc rather than first.
    let by_due = titles(&TaskQuery { sort: TaskSort::DueAsc, ..Default::default() });
    assert_eq!(&by_due[..2], ["bravo", "alpha"]);

    task_cleanup(store);
}

fn list_tasks_paginates(store: &dyn TaskStore) {
    let p = seeded_project(store, "Long");
    for i in 0..20i32 {
        let mut t = Task::new(format!("task {i:02}")).in_project(p.id);
        t.sort_order = i;
        store.put_task(&t).unwrap();
    }
    let window = TaskQuery {
        offset: 5,
        limit: Some(4),
        sort: TaskSort::Manual,
        ..TaskQuery::in_project(p.id)
    };
    let titles: Vec<String> =
        store.list_tasks(&window).unwrap().into_iter().map(|t| t.title).collect();
    assert_eq!(titles, ["task 05", "task 06", "task 07", "task 08"]);

    let past_the_end = TaskQuery { offset: 999, ..TaskQuery::in_project(p.id) };
    assert!(
        store.list_tasks(&past_the_end).unwrap().is_empty(),
        "paging past the end yields nothing rather than failing"
    );

    let unlimited = TaskQuery { offset: 18, ..TaskQuery::in_project(p.id) };
    assert_eq!(store.list_tasks(&unlimited).unwrap().len(), 2, "offset without limit must work");

    task_cleanup(store);
}

fn batch_writes_land_together(store: &dyn TaskStore) {
    // What a board reorder looks like: renumber a column in one call.
    let p = seeded_project(store, "Reordering");
    let mut rows: Vec<Task> = (0..5)
        .map(|i| {
            let mut t = Task::new(format!("card {i}")).in_project(p.id);
            t.sort_order = i;
            t
        })
        .collect();
    store.put_tasks(&rows).unwrap();
    assert_eq!(store.list_tasks(&TaskQuery::in_project(p.id)).unwrap().len(), 5);

    rows.reverse();
    for (i, t) in rows.iter_mut().enumerate() {
        t.sort_order = i as i32;
    }
    store.put_tasks(&rows).unwrap();

    let titles: Vec<String> = store
        .list_tasks(&TaskQuery::in_project(p.id))
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(titles, ["card 4", "card 3", "card 2", "card 1", "card 0"]);

    // An empty batch is a legitimate no-op, not an error.
    store.put_tasks(&[]).unwrap();
    task_cleanup(store);
}

fn subtasks_are_reachable_from_their_parent(store: &dyn TaskStore) {
    let p = seeded_project(store, "Nesting");
    let parent = seeded_task(store, Some(p.id), "parent");
    let mut child = Task::new("child").in_project(p.id).under(parent.id);
    child.sort_order = 0;
    let mut grandchild = Task::new("grandchild").in_project(p.id).under(child.id);
    grandchild.sort_order = 0;
    store.put_tasks(&[child.clone(), grandchild.clone()]).unwrap();

    let kids: Vec<String> = store
        .list_tasks(&TaskQuery::children_of(parent.id))
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(kids, ["child"], "children_of must return one level, not the whole subtree");

    let top: Vec<String> = store
        .list_tasks(&TaskQuery { parent: ParentScope::TopLevel, ..TaskQuery::in_project(p.id) })
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(top, ["parent"], "top level must exclude subtasks at any depth");

    assert_eq!(
        store.list_tasks(&TaskQuery::in_project(p.id)).unwrap().len(),
        3,
        "an unscoped project query returns every level"
    );
    task_cleanup(store);
}

fn deleting_a_task_takes_its_subtrees_with_it(store: &dyn TaskStore) {
    let p = seeded_project(store, "Cascades");
    let parent = seeded_task(store, Some(p.id), "parent");
    let child = Task::new("child").in_project(p.id).under(parent.id);
    let grandchild = Task::new("grandchild").in_project(p.id).under(child.id);
    let bystander = seeded_task(store, Some(p.id), "bystander");
    store.put_tasks(&[child.clone(), grandchild.clone()]).unwrap();

    store.delete_task(parent.id).unwrap();

    for gone in [parent.id, child.id, grandchild.id] {
        assert!(store.get_task(gone).is_err(), "the whole subtree must go, not just the root");
    }
    assert!(store.get_task(bystander.id).is_ok(), "a sibling must be untouched");
    task_cleanup(store);
}

fn deleting_a_project_takes_its_tasks_with_it(store: &dyn TaskStore) {
    let doomed = seeded_project(store, "Doomed");
    let kept = seeded_project(store, "Kept");
    let inside = seeded_task(store, Some(doomed.id), "inside");
    let nested = Task::new("nested").in_project(doomed.id).under(inside.id);
    store.put_task(&nested).unwrap();
    let elsewhere = seeded_task(store, Some(kept.id), "elsewhere");
    let loose = seeded_task(store, None, "loose");

    store.delete_project(doomed.id).unwrap();

    assert!(store.get_project(doomed.id).is_err());
    assert!(store.get_task(inside.id).is_err(), "a project takes its tasks with it");
    assert!(store.get_task(nested.id).is_err(), "and their subtasks");
    assert!(store.get_task(elsewhere.id).is_ok(), "another project must be untouched");
    assert!(store.get_task(loose.id).is_ok(), "the inbox must be untouched");
    task_cleanup(store);
}

fn time_blocks_round_trip_and_query_by_day(store: &dyn TaskStore) {
    let p = seeded_project(store, "Timed");
    let t = seeded_task(store, Some(p.id), "the work");

    // 09:00 and 14:00 UTC on one day, and one the day after.
    let base = "2026-06-15T09:00:00Z".parse::<Timestamp>().unwrap();
    let hour = jiff::SignedDuration::from_hours(1);

    let mut morning = TimeBlock::new(BlockSubject::Task { id: t.id }, base, 90, "UTC");
    morning.notes = "the good bit of the day".into();
    morning.tags = vec!["deep".into()];
    let afternoon = TimeBlock::new(BlockSubject::Task { id: t.id }, base + hour * 5, 60, "UTC")
        .of_kind(BlockKind::Actual);
    let tomorrow = TimeBlock::new(BlockSubject::Project { id: p.id }, base + hour * 24, 30, "UTC");
    let mut errand = TimeBlock::new(BlockSubject::Adhoc, base + hour * 3, 45, "UTC");
    errand.title = "dentist".into();

    for b in [&morning, &afternoon, &tomorrow, &errand] {
        store.put_block(b).unwrap();
    }

    assert_eq!(store.get_block(morning.id).unwrap(), morning, "a block must round-trip unchanged");

    let day = date(2026, 6, 15);
    let on_the_day = store.list_blocks(&BlockQuery::between(day, day)).unwrap();
    assert_eq!(on_the_day.len(), 3, "the day query must not reach into tomorrow");
    assert!(
        on_the_day.windows(2).all(|w| w[0].start <= w[1].start),
        "blocks must come back in time order"
    );

    let for_task = store.list_blocks(&BlockQuery::for_task(t.id)).unwrap();
    assert_eq!(for_task.len(), 2, "a task's own blocks, whatever day they are on");

    let logged = store
        .list_blocks(&BlockQuery { kind: Some(BlockKind::Actual), ..Default::default() })
        .unwrap();
    assert_eq!(logged.len(), 1, "planned and actual must be distinguishable");
    assert_eq!(logged[0].minutes(), 60);

    let for_project =
        store.list_blocks(&BlockQuery { project_id: Some(p.id), ..Default::default() }).unwrap();
    assert_eq!(for_project.len(), 1, "a project block is the one addressed to the project");

    // Upsert, not insert.
    let mut moved = morning.clone();
    moved.start = base + hour;
    moved.end = moved.start + hour;
    store.put_block(&moved).unwrap();
    assert_eq!(store.list_blocks(&BlockQuery::default()).unwrap().len(), 4);
    assert_eq!(store.get_block(morning.id).unwrap().minutes(), 60);

    store.delete_block(errand.id).unwrap();
    assert!(store.get_block(errand.id).is_err());

    task_cleanup(store);
}

fn deleting_a_task_removes_its_time_blocks(store: &dyn TaskStore) {
    // Orphaned blocks would quietly corrupt every time-spent total
    // afterwards, which is the one question this domain exists to answer.
    let p = seeded_project(store, "Accounting");
    let doomed = seeded_task(store, Some(p.id), "doomed");
    let child = Task::new("child").in_project(p.id).under(doomed.id);
    store.put_task(&child).unwrap();
    let kept = seeded_task(store, Some(p.id), "kept");

    let base = "2026-06-15T09:00:00Z".parse::<Timestamp>().unwrap();
    for subject in [
        BlockSubject::Task { id: doomed.id },
        BlockSubject::Task { id: child.id },
        BlockSubject::Task { id: kept.id },
    ] {
        store.put_block(&TimeBlock::new(subject, base, 30, "UTC")).unwrap();
    }
    assert_eq!(store.list_blocks(&BlockQuery::default()).unwrap().len(), 3);

    store.delete_task(doomed.id).unwrap();
    let left = store.list_blocks(&BlockQuery::default()).unwrap();
    assert_eq!(left.len(), 1, "the deleted task and its child took their blocks with them");
    assert_eq!(left[0].subject.task_id(), Some(kept.id));

    // And a project delete does the same, transitively.
    store.delete_project(p.id).unwrap();
    assert!(
        store.list_blocks(&BlockQuery::default()).unwrap().is_empty(),
        "a deleted project must leave no time behind either"
    );
    task_cleanup(store);
}

fn task_stats_reflect_contents(store: &dyn TaskStore) {
    let p = seeded_project(store, "Counting");
    let loose = seeded_task(store, None, "in the inbox");
    let mut archived = Project::new("Old");
    archived.set_status(ProjectStatus::Archived);
    store.put_project(&archived).unwrap();

    let open = seeded_task(store, Some(p.id), "open");
    let mut done = Task::new("done").in_project(p.id);
    done.set_status(TaskStatus::Done);
    let mut cancelled = Task::new("cancelled").in_project(p.id);
    cancelled.set_status(TaskStatus::Cancelled);
    // Dated either side of the day the stats are taken on, plus one with no
    // deadline at all -- which must fall outside both counts.
    let mut late = Task::new("late").in_project(p.id);
    late.due_date = Some(date(2026, 6, 14));
    let mut today_task = Task::new("today").in_project(p.id);
    today_task.due_date = Some(date(2026, 6, 15));
    let mut soon = Task::new("soon").in_project(p.id);
    soon.due_date = Some(date(2026, 6, 16));
    let dated = [late.id, today_task.id, soon.id];
    store.put_tasks(&[done, cancelled, late, today_task, soon]).unwrap();

    let base = "2026-06-15T09:00:00Z".parse::<Timestamp>().unwrap();
    store.put_block(&TimeBlock::new(BlockSubject::Task { id: open.id }, base, 90, "UTC")).unwrap();
    store
        .put_block(
            &TimeBlock::new(BlockSubject::Task { id: open.id }, base, 45, "UTC")
                .of_kind(BlockKind::Actual),
        )
        .unwrap();

    let s = store.task_stats(date(2026, 6, 15)).unwrap();
    assert_eq!(s.projects, 2);
    assert_eq!(s.active_projects, 1, "an archived project is not an active one");
    assert_eq!(s.tasks, 7);
    assert_eq!(s.open_tasks, 5, "cancelled counts as closed, but not as done");
    assert_eq!(s.due_today, 2, "due today counts today's and everything overdue");
    assert_eq!(s.overdue, 1, "overdue is the deadlines already missed");
    assert_eq!(
        store.task_stats(date(2026, 6, 20)).unwrap().overdue,
        3,
        "the counts move with the day they are asked about",
    );
    assert_eq!(s.done_tasks, 1);
    assert_eq!(s.blocks, 2);
    assert_eq!(s.planned_minutes, 90);
    assert_eq!(s.logged_minutes, 45);

    // The per-project roll-up: outstanding work only, with the inbox under
    // the `None` key, and no row at all for a project with nothing left.
    let mut by_project = s.open_by_project.clone();
    by_project.sort_by_key(|c| c.project_id.map(|id| id.to_string()));
    assert_eq!(
        by_project,
        vec![
            ProjectTaskCount { project_id: None, open: 1 },
            ProjectTaskCount { project_id: Some(p.id), open: 4 },
        ],
        "open_by_project must count only outstanding work, inbox included",
    );

    // Finish the last open task in the project and its row disappears
    // entirely rather than reporting zero.
    for id in std::iter::once(open.id).chain(dated) {
        let mut t = store.get_task(id).unwrap();
        t.set_status(TaskStatus::Done);
        store.put_task(&t).unwrap();
    }
    let s = store.task_stats(date(2026, 6, 15)).unwrap();
    assert_eq!(
        s.open_by_project.iter().filter(|c| c.project_id == Some(p.id)).count(),
        0,
        "a project with nothing open must not be listed",
    );
    assert!(s.open_by_project.iter().any(|c| c.project_id.is_none() && c.open == 1));
    assert_eq!((s.due_today, s.overdue), (0, 0), "finished work is never due");
    let _ = loose;

    task_cleanup(store);
}

fn unicode_survives_a_task_round_trip(store: &dyn TaskStore) {
    let tricky = "\u{1f469}\u{200d}\u{1f4bb} \u{5bb6}\u{65cf} \u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} e\u{301}cole \u{1f1ef}\u{1f1f5}";
    let mut p = Project::new(tricky);
    p.notes = tricky.into();
    p.tags = vec!["\u{30bf}\u{30b0}".into()];
    store.put_project(&p).unwrap();

    let mut t = Task::new(tricky).in_project(p.id);
    t.notes = tricky.into();
    t.tags = vec!["\u{30bf}\u{30b0}".into()];
    store.put_task(&t).unwrap();

    assert_eq!(store.get_project(p.id).unwrap().name, tricky, "unicode project name must survive");
    let got = store.get_task(t.id).unwrap();
    assert_eq!(got.title, tricky, "unicode task title must survive");
    assert_eq!(got.notes, tricky, "unicode notes must survive");
    assert_eq!(got.tags, t.tags, "unicode tags must survive");

    // And the filters must find it, which is the part a naive byte-wise
    // lowercase would break.
    let found = store
        .list_tasks(&TaskQuery { text: "\u{5bb6}\u{65cf}".into(), ..Default::default() })
        .unwrap();
    assert_eq!(found.len(), 1, "text search must work on non-ASCII");

    task_cleanup(store);
}
