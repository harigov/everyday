//! The task domain wired all the way through: a real vault, a real SQLite
//! file, a real cipher. The unit tests below the vault prove each layer;
//! this proves they are wired to each other.

use everyday_core::VaultConfig;
use everyday_vault::{create, open};

#[test]
fn the_todo_app_works_end_to_end_on_the_default_backend() {
    use everyday_core::store::tasks::{BlockQuery, TaskQuery};
    use everyday_core::task::{
        BlockKind, BlockSubject, Priority, Project, Task, TaskStatus, TimeBlock,
    };

    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = create(dir.path(), cfg.clone()).unwrap();
    assert!(vault.supports_tasks(), "the default backend must carry the task domain");

    let mut project = Project::new("Repaint the hall");
    project.due_date = Some(jiff::civil::date(2026, 5, 1));
    project.tags = vec!["home".into()];
    vault.save_project(&project).unwrap();

    let mut task = Task::new("Buy the paint").in_project(project.id);
    task.priority = Priority::High;
    task.estimate_minutes = Some(45);
    task.tags = vec!["errand".into(), "home".into()];
    vault.save_task(&task).unwrap();

    let subtask = Task::new("Match the old colour").in_project(project.id).under(task.id);
    vault.save_task(&subtask).unwrap();

    let start = "2026-04-20T09:00:00Z".parse::<jiff::Timestamp>().unwrap();
    let logged = TimeBlock::new(BlockSubject::Task { id: task.id }, start, 30, "UTC")
        .of_kind(BlockKind::Actual);
    vault.save_block(&logged).unwrap();

    // Tags come from all three record kinds, most used first.
    assert_eq!(
        vault.task_tags().unwrap(),
        vec![("home".to_string(), 2), ("errand".to_string(), 1)],
    );

    let today = jiff::civil::date(2026, 4, 20);
    let stats = vault.task_stats(today).unwrap();
    assert_eq!((stats.projects, stats.tasks, stats.open_tasks), (1, 2, 2));
    assert_eq!(stats.logged_minutes, 30);

    // Lock, reopen, unlock: the tasks must still be there and still
    // decryptable, which is the part that would break if the task
    // records were sealed against the wrong associated data.
    drop(vault);
    let vault = open(dir.path()).unwrap();
    assert_eq!(
        vault.tasks(&TaskQuery::default()).unwrap_err().code(),
        "locked",
        "a reopened encrypted vault must not hand out tasks"
    );
    vault.unlock(Some("correct horse battery")).unwrap();

    let round = vault.task(task.id).unwrap();
    assert_eq!(round, task, "a task must survive a lock/unlock cycle unchanged");
    assert_eq!(
        vault.tasks(&TaskQuery::children_of(task.id)).unwrap(),
        std::slice::from_ref(&subtask)
    );
    assert_eq!(vault.blocks(&BlockQuery::for_task(task.id)).unwrap().len(), 1);

    // And the cascade holds through the vault, not just the store.
    vault.delete_project(project.id).unwrap();
    assert!(vault.tasks(&TaskQuery::default()).unwrap().is_empty());
    assert!(vault.blocks(&BlockQuery::default()).unwrap().is_empty());
    assert_eq!(vault.task_stats(today).unwrap().tasks, 0);

    let _ = TaskStatus::Todo; // the status set is part of the contract
}

#[test]
fn the_vault_rejects_tasks_that_would_be_unusable() {
    use everyday_core::task::{BlockSubject, Task, TimeBlock};

    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = create(dir.path(), cfg).unwrap();

    // An untitled task is a row nothing can render and nobody can find.
    let blank = Task::new("   ");
    assert_eq!(vault.save_task(&blank).unwrap_err().code(), "invalid");

    // Its own parent: a cycle the subtree walk would have to defend
    // against for ever after.
    let mut loop_task = Task::new("ouroboros");
    loop_task.parent_id = Some(loop_task.id);
    assert_eq!(vault.save_task(&loop_task).unwrap_err().code(), "invalid");

    // A block that ends before it starts would subtract from a total.
    let start = "2026-04-20T09:00:00Z".parse::<jiff::Timestamp>().unwrap();
    let mut backwards = TimeBlock::new(BlockSubject::Adhoc, start, 30, "UTC");
    backwards.end = start - jiff::SignedDuration::from_mins(10);
    assert_eq!(vault.save_block(&backwards).unwrap_err().code(), "invalid");

    // A rejected write must leave nothing behind.
    assert!(vault.tasks(&Default::default()).unwrap().is_empty());
    assert!(vault.blocks(&Default::default()).unwrap().is_empty());
}

mod settling_blocks {
    //! A block's kind follows the clock -- see `Vault::settle_blocks`.

    use super::*;
    use everyday_core::Vault;
    use everyday_core::task::{BlockKind, BlockSubject, TimeBlock};
    use jiff::{SignedDuration, Timestamp};

    fn vault() -> (Vault, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let v = create(dir.path(), VaultConfig { password: None, ..Default::default() }).unwrap();
        (v, dir)
    }

    fn block(subject: BlockSubject, hours_from_now: i64, minutes: u32) -> TimeBlock {
        let start = Timestamp::now() + SignedDuration::from_hours(hours_from_now);
        TimeBlock::new(subject, start, minutes, "UTC")
    }

    fn kind(v: &Vault, b: &TimeBlock) -> BlockKind {
        v.block(b.id).unwrap().kind
    }

    #[test]
    fn saving_decides_plan_or_time_spent_by_the_clock() {
        let (v, _dir) = vault();
        let past = block(BlockSubject::Adhoc, -3, 60);
        let mut ahead = block(BlockSubject::Adhoc, 3, 60);
        ahead.kind = BlockKind::Actual;
        v.save_block(&past).unwrap();
        v.save_block(&ahead).unwrap();
        assert_eq!(kind(&v, &past), BlockKind::Actual, "an hour that has ended is time spent");
        assert_eq!(kind(&v, &ahead), BlockKind::Planned, "an hour still ahead is a plan");
    }

    #[test]
    fn a_plan_that_ends_becomes_time_spent_at_the_next_settle() {
        let (v, _dir) = vault();
        let b = block(BlockSubject::Adhoc, 1, 60);
        v.save_block(&b).unwrap();
        assert_eq!(kind(&v, &b), BlockKind::Planned);

        let later = Timestamp::now() + SignedDuration::from_hours(3);
        assert_eq!(v.settle_blocks(later, None).unwrap(), 1);
        assert_eq!(kind(&v, &b), BlockKind::Actual);
        assert_eq!(v.settle_blocks(later, None).unwrap(), 0, "nothing left to change");
    }

    #[test]
    fn a_plan_with_a_record_of_the_same_hour_is_not_counted_twice() {
        let (v, _dir) = vault();
        let task = everyday_core::task::Task::new("Write the report");
        v.save_task(&task).unwrap();
        let subject = BlockSubject::Task { id: task.id };

        // The old "This is what happened": a record, and the plan it was
        // copied from, of the same hour that has now passed.
        let mut record = block(subject, -3, 60);
        record.kind = BlockKind::Actual;
        let mut plan = record.clone();
        plan.id = everyday_core::id::BlockId::new();
        plan.kind = BlockKind::Planned;
        v.save_block(&record).unwrap();
        v.save_block(&plan).unwrap();
        // Something else, overlapping but on another subject, still counts.
        let other = block(BlockSubject::Adhoc, -3, 30);
        v.save_block(&other).unwrap();

        let now = Timestamp::now();
        v.settle_blocks(now, None).unwrap();
        assert_eq!(kind(&v, &record), BlockKind::Actual);
        assert_eq!(kind(&v, &plan), BlockKind::Planned, "the record already counts this hour");
        assert_eq!(kind(&v, &other), BlockKind::Actual);

        // Without the record, the plan is all there is to go on.
        v.delete_block(record.id).unwrap();
        v.settle_blocks(now, None).unwrap();
        assert_eq!(kind(&v, &plan), BlockKind::Actual);
    }
}
