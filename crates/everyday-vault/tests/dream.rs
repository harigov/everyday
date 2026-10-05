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
fn a_tracker_with_a_target_says_where_its_period_stands_even_when_nothing_was_logged() {
    // "An hour or two of piano a week" with no practice yet is the finding;
    // a digest that only listed trackers with readings would never say so.
    let (_dir, vault) = vault_for_test();
    let piano = everyday_core::Tracker::new("Piano", everyday_core::TrackerKind::Amount)
        .with_unit("min")
        .aiming(everyday_core::Target::between(60.0, 120.0, everyday_core::Period::Week));
    vault.save_tracker(&piano).unwrap();

    // Thursday the 10th, reading Wednesday: the week began on Monday the 7th.
    let now = zoned(2026, 9, 10, 4, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    let line = d.readings.iter().find(|r| r.starts_with("Piano")).expect("a line for Piano");
    assert!(line.contains("target between 60 and 120 min a week"), "{line}");
    assert!(line.contains("0 so far for 2026-09-07..2026-09-13, short"), "{line}");
    assert!(line.contains("even pace"), "{line}");

    // Retired, it is no longer a finding: an archived habit is not one the
    // dream should be proposing time for.
    let mut retired = vault.tracker(piano.id).unwrap();
    retired.archived = true;
    vault.save_tracker(&retired).unwrap();
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert!(!d.readings.iter().any(|r| r.starts_with("Piano")), "{:?}", d.readings);
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

// ---- skills -----------------------------------------------------------------

#[test]
fn the_skills_section_lists_on_and_off_skills_and_stays_ambient() {
    let (_dir, vault) = vault_for_test();

    let mut plan = everyday_core::Skill::new("Plan a trip");
    plan.description = "Use when asked to plan a trip or a multi-day journey.".into();
    plan.instructions = "Check the calendar, check the weather, propose blocks.".into();
    plan.updated_at = ts("2026-09-10T00:00:00Z");
    vault.save_skill(&plan).unwrap();

    let mut retired = everyday_core::Skill::new("Retired review");
    retired.description = "No longer run.".into();
    retired.instructions = "Old steps nobody follows now.".into();
    retired.enabled = false;
    retired.updated_at = ts("2026-01-01T00:00:00Z");
    vault.save_skill(&retired).unwrap();

    // Nothing else happened, so the digest would otherwise be empty -- the
    // skills section must not be what breaks that.
    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert!(d.is_empty(), "skills are ambient state, not a finding in the window: {:?}", d.skills);

    assert!(
        d.skills.iter().any(|s| s.contains(&plan.id.to_string())
            && s.contains("Plan a trip")
            && s.contains("-- on --")),
        "{:?}",
        d.skills
    );
    assert!(
        d.skills.iter().any(|s| s.contains(&retired.id.to_string())
            && s.contains("Retired review")
            && s.contains("-- off --")),
        "a switched-off skill is listed too: {:?}",
        d.skills
    );
}

// ---- conversations ------------------------------------------------------

#[test]
fn the_conversations_section_names_requests_and_the_skills_loaded_for_them() {
    let (_dir, vault) = vault_for_test();

    // `save_message` bumps a conversation's `updated_at` to the *later* of
    // the message's own and whatever it already was, so it has to start
    // older than every message below -- otherwise the real wall clock
    // `Conversation::new` stamped it with would win the comparison instead.
    let mut conversation = everyday_core::Conversation::new();
    conversation.updated_at = ts("2000-01-01T00:00:00Z");
    vault.save_conversation(&conversation).unwrap();

    let mut ask =
        everyday_core::Message::user(conversation.id, "Can you plan a week in Portugal for May?");
    ask.created_at = ts("2026-09-15T09:00:00Z");
    vault.save_message(&ask).unwrap();

    let mut loads_skill =
        everyday_core::Message::assistant(conversation.id, "").with_tool_calls(vec![
            everyday_core::ToolCall {
                id: "call_1".into(),
                name: "read_skill".into(),
                arguments: serde_json::json!({ "name": "Plan a trip" }),
            },
        ]);
    loads_skill.created_at = ts("2026-09-15T09:00:05Z");
    vault.save_message(&loads_skill).unwrap();

    let mut follow_up = everyday_core::Message::user(
        conversation.id,
        "Actually make it ten days, starting the second Monday",
    );
    follow_up.created_at = ts("2026-09-15T09:05:00Z");
    vault.save_message(&follow_up).unwrap();

    // A dream's own transcript, updated the same day, must not be read as
    // one of the person's own conversations.
    let mut transcript =
        everyday_core::Conversation::for_run(everyday_core::RoutineRunId::new(), "Nightly run");
    transcript.updated_at = ts("2000-01-01T00:00:00Z");
    vault.save_conversation(&transcript).unwrap();
    let mut dream_message = everyday_core::Message::user(transcript.id, "the digest itself");
    dream_message.created_at = ts("2026-09-15T03:00:00Z");
    vault.save_message(&dream_message).unwrap();

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert!(!d.is_empty(), "somebody chatting with the assistant is a thing that happened");
    assert_eq!(d.conversations.len(), 1, "{:?}", d.conversations);
    let line = &d.conversations[0];
    assert!(line.contains("2 requests"), "{line}");
    assert!(line.contains("skills loaded: Plan a trip"), "{line}");
    assert!(line.contains("plan a week in Portugal"), "{line}");
    assert!(line.contains("ten days"), "{line}");
    assert!(!line.contains("the digest itself"), "a dream's own transcript is not a conversation");
}

#[test]
fn the_conversations_section_caps_how_many_conversations_it_lists() {
    let (_dir, vault) = vault_for_test();
    for i in 0..(dream::MAX_DIGEST_CONVERSATIONS + 2) {
        let mut conversation = everyday_core::Conversation::new();
        conversation.updated_at = ts("2000-01-01T00:00:00Z");
        vault.save_conversation(&conversation).unwrap();
        let mut msg = everyday_core::Message::user(conversation.id, format!("request {i}"));
        msg.created_at = ts("2026-09-15T09:00:00Z");
        vault.save_message(&msg).unwrap();
    }

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert_eq!(d.conversations.len(), dream::MAX_DIGEST_CONVERSATIONS + 1, "{:?}", d.conversations);
    assert!(d.conversations.last().unwrap().contains("and 2 more"), "{:?}", d.conversations);
}

#[test]
fn the_conversations_section_caps_excerpts_and_their_length() {
    let (_dir, vault) = vault_for_test();
    let mut conversation = everyday_core::Conversation::new();
    conversation.updated_at = ts("2000-01-01T00:00:00Z");
    vault.save_conversation(&conversation).unwrap();

    let cap = dream::MAX_CONVERSATION_EXCERPTS;
    let long_text = "a".repeat(dream::MAX_EXCERPT_CHARS + 50);
    let mut long_msg = everyday_core::Message::user(conversation.id, long_text.clone());
    long_msg.created_at = zoned(2026, 9, 15, 9, 0).timestamp();
    vault.save_message(&long_msg).unwrap();

    for i in 1..=(cap + 1) {
        let mut msg = everyday_core::Message::user(conversation.id, format!("request number {i}"));
        msg.created_at = zoned(2026, 9, 15, 9, i as i8).timestamp();
        vault.save_message(&msg).unwrap();
    }

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now).unwrap();
    assert_eq!(d.conversations.len(), 1, "{:?}", d.conversations);
    let line = &d.conversations[0];
    assert!(line.contains(&format!("{} requests", cap + 2)), "{line}");
    assert!(line.contains('\u{2026}'), "a long excerpt is cut with an ellipsis: {line}");
    assert!(!line.contains(&long_text), "the cut excerpt must not contain the whole thing: {line}");
    assert!(
        !line.contains(&format!("\"request number {}\"", cap + 1)),
        "only the capped number of excerpts are quoted: {line}"
    );
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
