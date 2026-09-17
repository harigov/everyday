//! Dreaming, wired through a real vault: the digest builder against seeded
//! records, for each scope, and `Vault::set_dreaming`'s on/off/on
//! idempotence and its refusals.

use everyday_core::dream;
use everyday_core::purpose::{Goal, Role};
use everyday_core::routine::{
    DreamScope, Outcome as RunOutcome, Routine, RoutineKind, RoutineRun, Trigger,
};
use everyday_core::task::{Task, TaskStatus};
use everyday_core::{Error, VaultConfig};
use everyday_vault::create;

/// A real vault over the SQLite backend, with a fixed zone so every date in
/// these tests reads the same wherever they run.
fn vault_for_test() -> (tempfile::TempDir, everyday_core::Vault) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = create(dir.path(), cfg).unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.timezone = Some("UTC".into());
    vault.save_agent_settings(&settings).unwrap();
    (dir, vault)
}

fn zoned(y: i16, m: i8, d: i8, h: i8, mi: i8) -> jiff::Zoned {
    jiff::civil::date(y, m, d).at(h, mi, 0, 0).in_tz("UTC").unwrap()
}

fn ts(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
}

// ---- the digest, per scope -------------------------------------------------

#[test]
fn a_quiet_day_yields_an_empty_digest() {
    let (_dir, vault) = vault_for_test();
    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert!(d.is_empty());
    assert_eq!(d.from, jiff::civil::date(2026, 9, 15));
    assert_eq!(d.to, jiff::civil::date(2026, 9, 15));
    assert_eq!(d.as_of, jiff::civil::date(2026, 9, 15));
}

#[test]
fn the_day_digest_reads_tasks_and_entries_from_yesterday() {
    let (_dir, vault) = vault_for_test();

    let mut created = Task::new("Book the dentist");
    created.created_at = ts("2026-09-15T08:00:00Z");
    vault.save_task(&created).unwrap();

    let mut completed = Task::new("Reply to Priya");
    completed.status = TaskStatus::Done;
    completed.completed_at = Some(ts("2026-09-15T18:00:00Z"));
    // Keep it out of "created" too, so the two lists are tested apart.
    completed.created_at = ts("2026-08-01T08:00:00Z");
    vault.save_task(&completed).unwrap();

    let mut overdue = Task::new("Renew the passport");
    overdue.due_date = Some(jiff::civil::date(2026, 9, 10));
    vault.save_task(&overdue).unwrap();

    let journal = everyday_core::model::Journal::new("Journal");
    vault.save_journal(&journal).unwrap();
    let mut entry = everyday_core::model::Entry::new(journal.id, "UTC");
    entry.local_date = jiff::civil::date(2026, 9, 15);
    entry.title = "A quiet Tuesday".into();
    entry.body = everyday_core::richtext::RichDoc::from_plain_text("Nothing much happened.");
    vault.save_entry(&entry, None).unwrap();

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert!(!d.is_empty());
    assert_eq!(d.tasks_created, vec!["Book the dentist".to_string()]);
    assert_eq!(d.tasks_completed, vec!["Reply to Priya".to_string()]);
    assert_eq!(d.tasks_overdue, vec!["Renew the passport".to_string()]);
    assert_eq!(d.entries.len(), 1);
    assert!(d.entries[0].contains("A quiet Tuesday"));
    assert!(d.entries[0].contains("Nothing much happened"));

    let md = d.to_markdown();
    assert!(
        !md.contains("--- digest ---"),
        "the marker line belongs to the scheduler's prompt, not the digest itself"
    );
}

#[test]
fn the_week_digest_reads_the_nightly_dreams_run_summaries_not_raw_entries() {
    let (_dir, vault) = vault_for_test();
    let routines = vault.set_dreaming(true).unwrap();
    let nightly =
        routines.iter().find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Day }).unwrap();

    let mut run = RoutineRun::new(nightly, None);
    run.started_at = ts("2026-09-14T03:00:00Z");
    run.finished_at = Some(ts("2026-09-14T03:05:00Z"));
    run.finish(RunOutcome::Done, "2 proposals, 1 memory. Quiet night otherwise.");
    // `finish` re-stamps `finished_at`; put it back inside the window.
    run.finished_at = Some(ts("2026-09-14T03:05:00Z"));
    vault.save_run(&run).unwrap();

    // An entry in the same window must NOT appear directly -- only the
    // nightly dream's own words about it should.
    let journal = everyday_core::model::Journal::new("Journal");
    vault.save_journal(&journal).unwrap();
    let mut entry = everyday_core::model::Entry::new(journal.id, "UTC");
    entry.local_date = jiff::civil::date(2026, 9, 14);
    entry.body = everyday_core::richtext::RichDoc::from_plain_text("Secret diary content.");
    vault.save_entry(&entry, None).unwrap();

    let now = zoned(2026, 9, 20, 3, 30);
    let d = dream::digest(&vault, DreamScope::Week, &now).unwrap();
    assert_eq!(d.entries.len(), 1);
    assert!(d.entries[0].contains("2 proposals, 1 memory"));
    assert!(!d.entries.iter().any(|e| e.contains("Secret diary")));
}

#[test]
fn the_month_digest_names_a_goal_with_no_activity() {
    let (_dir, vault) = vault_for_test();
    let role = Role::new("Health");
    vault.save_role(&role).unwrap();
    let goal = Goal::new(role.id, "Run a 10k");
    vault.save_goal(&goal).unwrap();

    let now = zoned(2026, 9, 30, 4, 0);
    let d = dream::digest(&vault, DreamScope::Month, &now).unwrap();
    assert!(
        d.arcs.iter().any(|a| a.contains("Run a 10k") && a.contains("no activity")),
        "{:?}",
        d.arcs
    );
}

#[test]
fn the_month_digest_names_birthdays_in_the_month_ahead() {
    use everyday_core::library::{Item, LogEntry, LogEvent};
    let (_dir, vault) = vault_for_test();
    // What the application does when a vault opens.
    vault.seed_library().unwrap();
    let contacts = vault
        .kinds()
        .unwrap()
        .into_iter()
        .find(|k| k.slug == "contact")
        .expect("a seeded library has a Contacts shelf");

    let mut dana = Item::new(contacts.id, "Dana");
    dana.facts.insert("birthday".into(), "1988-10-08".into());
    vault.save_item(&dana).unwrap();
    vault
        .save_log(&LogEntry::new(dana.id, LogEvent::Finished, jiff::civil::date(2026, 7, 2), "UTC"))
        .unwrap();

    let mut far = Item::new(contacts.id, "Sam");
    far.facts.insert("birthday".into(), "1990-03-01".into());
    vault.save_item(&far).unwrap();

    let mut profile = vault.profile().unwrap();
    profile.born = Some(jiff::civil::date(1985, 10, 20));
    vault.save_profile(&profile).unwrap();

    let now = zoned(2026, 9, 30, 4, 0);
    let d = dream::digest(&vault, DreamScope::Month, &now).unwrap();
    let md = d.to_markdown();
    assert!(
        d.arcs.iter().any(|a| a.contains("Dana") && a.contains("last caught up 2 July 2026")),
        "{:?}",
        d.arcs
    );
    assert!(d.arcs.iter().any(|a| a.starts_with("Their own birthday")), "{:?}", d.arcs);
    assert!(!md.contains("Sam"), "a birthday five months away is not this month's business");
    let dana_at = d.arcs.iter().position(|a| a.contains("Dana")).unwrap();
    let own_at = d.arcs.iter().position(|a| a.starts_with("Their own")).unwrap();
    assert!(dana_at < own_at, "soonest first");
}

// ---- set_dreaming ----------------------------------------------------------

#[test]
fn set_dreaming_creates_the_three_and_is_idempotent() {
    let (_dir, vault) = vault_for_test();

    let first = vault.set_dreaming(true).unwrap();
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|r| r.enabled));
    for scope in DreamScope::ALL {
        assert!(first.iter().any(|r| r.kind == RoutineKind::Dream { scope }));
    }
    let all_routines = vault.routines().unwrap();
    assert_eq!(all_routines.iter().filter(|r| r.kind.is_dream()).count(), 3);

    // Switching on again must not mint a second set.
    let second = vault.set_dreaming(true).unwrap();
    let mut first_ids: Vec<_> = first.iter().map(|r| r.id).collect();
    let mut second_ids: Vec<_> = second.iter().map(|r| r.id).collect();
    first_ids.sort();
    second_ids.sort();
    assert_eq!(first_ids, second_ids);
    assert_eq!(vault.routines().unwrap().iter().filter(|r| r.kind.is_dream()).count(), 3);

    let off = vault.set_dreaming(false).unwrap();
    assert!(off.iter().all(|r| !r.enabled));
    let mut off_ids: Vec<_> = off.iter().map(|r| r.id).collect();
    off_ids.sort();
    assert_eq!(off_ids, first_ids, "the same three, merely disabled");

    let on_again = vault.set_dreaming(true).unwrap();
    assert!(on_again.iter().all(|r| r.enabled));
    let mut on_again_ids: Vec<_> = on_again.iter().map(|r| r.id).collect();
    on_again_ids.sort();
    assert_eq!(on_again_ids, first_ids);
}

#[test]
fn a_freshly_made_nightly_dream_has_the_documented_schedule() {
    let (_dir, vault) = vault_for_test();
    let routines = vault.set_dreaming(true).unwrap();
    let nightly =
        routines.iter().find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Day }).unwrap();
    assert_eq!(nightly.grace_minutes, 20 * 60);
    assert!(matches!(nightly.trigger, Trigger::Schedule { day_of_month: None, .. }));

    let monthly = routines
        .iter()
        .find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Month })
        .unwrap();
    assert_eq!(monthly.grace_minutes, 6 * 24 * 60);
    assert!(matches!(monthly.trigger, Trigger::Schedule { day_of_month: Some(1), .. }));
}

// ---- refusals ---------------------------------------------------------------

#[test]
fn a_dream_cannot_be_deleted_by_hand() {
    let (_dir, vault) = vault_for_test();
    let routines = vault.set_dreaming(true).unwrap();
    let nightly = &routines[0];
    let err = vault.delete_routine(nightly.id).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)));
    assert!(vault.routine(nightly.id).is_ok(), "still there");
}

#[test]
fn a_new_routine_cannot_be_made_a_dream_by_hand() {
    let (_dir, vault) = vault_for_test();
    let mut routine = Routine::new("Sneaky", "", Trigger::Manual);
    routine.kind = RoutineKind::Dream { scope: DreamScope::Day };
    let err = vault.save_routine(&routine).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)));
}

#[test]
fn an_existing_routines_kind_cannot_be_changed_either_way() {
    let (_dir, vault) = vault_for_test();

    let mut custom = Routine::new("Morning brief", "Say what is due.", Trigger::Manual);
    vault.save_routine(&custom).unwrap();
    custom.kind = RoutineKind::Dream { scope: DreamScope::Day };
    assert!(vault.save_routine(&custom).is_err(), "custom -> dream");

    let routines = vault.set_dreaming(true).unwrap();
    let mut dream_routine = routines[0].clone();
    dream_routine.kind = RoutineKind::Custom;
    assert!(vault.save_routine(&dream_routine).is_err(), "dream -> custom");
}

#[test]
fn a_dreams_schedule_grace_and_instructions_may_still_be_edited() {
    let (_dir, vault) = vault_for_test();
    let routines = vault.set_dreaming(true).unwrap();
    let mut nightly = routines
        .iter()
        .find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Day })
        .unwrap()
        .clone();
    nightly.grace_minutes = 5 * 60;
    nightly.instructions = "Also mention the cat.".into();
    vault.save_routine(&nightly).unwrap();

    let reloaded = vault.routine(nightly.id).unwrap();
    assert_eq!(reloaded.grace_minutes, 5 * 60);
    assert_eq!(reloaded.instructions, "Also mention the cat.");
    assert_eq!(reloaded.kind, RoutineKind::Dream { scope: DreamScope::Day });
}
