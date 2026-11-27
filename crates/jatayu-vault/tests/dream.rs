//! Dreaming, wired through a real vault: the digest builder against seeded
//! records, for each scope, and `Vault::set_dreaming`'s on/off/on
//! idempotence and its refusals.

use jatayu_core::dream;
use jatayu_core::purpose::{Area, Goal};
use jatayu_core::routine::{
    DreamScope, Outcome as RunOutcome, Routine, RoutineKind, RoutineRun, Trigger,
};
use jatayu_core::task::{Task, TaskStatus};
use jatayu_core::{Error, VaultConfig};
use jatayu_vault::create;

/// A real vault over the SQLite backend, with a fixed zone so every date in
/// these tests reads the same wherever they run.
fn vault_for_test() -> (tempfile::TempDir, jatayu_core::Vault) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: jatayu_core::crypto::KdfParams::insecure_fast(),
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
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
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

    let journal = jatayu_core::model::Journal::new("Journal");
    vault.save_journal(&journal).unwrap();
    let mut entry = jatayu_core::model::Entry::new(journal.id, "UTC");
    entry.local_date = jiff::civil::date(2026, 9, 15);
    entry.title = "A quiet Tuesday".into();
    entry.body = jatayu_core::richtext::RichDoc::from_plain_text("Nothing much happened.");
    vault.save_entry(&entry, None).unwrap();

    // A note the assistant left itself for the same day is not their writing.
    let mine = vault.assistant_journal_or_new("The assistant's journal").unwrap();
    let mut note = jatayu_core::model::Entry::new(mine.id, "UTC");
    note.local_date = jiff::civil::date(2026, 9, 15);
    note.body = jatayu_core::richtext::RichDoc::from_plain_text("Raise the boiler quote.");
    vault.save_entry(&note, None).unwrap();

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    assert!(!d.is_empty());
    assert_eq!(d.tasks_created, vec!["Book the dentist".to_string()]);
    assert_eq!(d.tasks_completed, vec!["Reply to Priya".to_string()]);
    assert_eq!(d.tasks_overdue, vec!["Renew the passport".to_string()]);
    assert_eq!(d.entries.len(), 1, "their entry, and not the assistant's own note");
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
    let journal = jatayu_core::model::Journal::new("Journal");
    vault.save_journal(&journal).unwrap();
    let mut entry = jatayu_core::model::Entry::new(journal.id, "UTC");
    entry.local_date = jiff::civil::date(2026, 9, 14);
    entry.body = jatayu_core::richtext::RichDoc::from_plain_text("Secret diary content.");
    vault.save_entry(&entry, None).unwrap();

    let now = zoned(2026, 9, 20, 3, 30);
    let d = dream::digest(&vault, DreamScope::Week, &now, &[]).unwrap();
    assert_eq!(d.entries.len(), 1);
    assert!(d.entries[0].contains("2 proposals, 1 memory"));
    assert!(!d.entries.iter().any(|e| e.contains("Secret diary")));
}

#[test]
fn the_month_digest_names_a_goal_with_no_activity() {
    let (_dir, vault) = vault_for_test();
    let area = Area::new("Health");
    vault.save_area(&area).unwrap();
    let goal = Goal::new(area.id, "Run a 10k");
    vault.save_goal(&goal).unwrap();

    let now = zoned(2026, 9, 30, 4, 0);
    let d = dream::digest(&vault, DreamScope::Month, &now, &[]).unwrap();
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
    let piano = jatayu_core::Tracker::new("Piano", jatayu_core::TrackerKind::Amount)
        .with_unit("min")
        .aiming(jatayu_core::Target::between(60.0, 120.0, jatayu_core::Period::Week));
    vault.save_tracker(&piano).unwrap();

    // Thursday the 10th, reading Wednesday: the week began on Monday the 7th.
    let now = zoned(2026, 9, 10, 4, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    let line = d.readings.iter().find(|r| r.starts_with("Piano")).expect("a line for Piano");
    assert!(line.contains("target between 60 and 120 min a week"), "{line}");
    assert!(line.contains("0 so far for 2026-09-07..2026-09-13, short"), "{line}");
    assert!(line.contains("even pace"), "{line}");

    // Retired, it is no longer a finding: an archived habit is not one the
    // dream should be proposing time for.
    let mut retired = vault.tracker(piano.id).unwrap();
    retired.archived = true;
    vault.save_tracker(&retired).unwrap();
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    assert!(!d.readings.iter().any(|r| r.starts_with("Piano")), "{:?}", d.readings);
}

#[test]
fn the_month_digest_names_birthdays_in_the_month_ahead() {
    use jatayu_core::library::{Item, LogEntry, LogEvent};
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
        .save_log(&LogEntry::new(dana.id, LogEvent::Contact, jiff::civil::date(2026, 7, 2), "UTC"))
        .unwrap();

    let mut far = Item::new(contacts.id, "Sam");
    far.facts.insert("birthday".into(), "1990-03-01".into());
    vault.save_item(&far).unwrap();

    let mut profile = vault.profile().unwrap();
    profile.born = Some(jiff::civil::date(1985, 10, 20));
    vault.save_profile(&profile).unwrap();

    let now = zoned(2026, 9, 30, 4, 0);
    let d = dream::digest(&vault, DreamScope::Month, &now, &[]).unwrap();
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

// ---- the mail a dream may read ------------------------------------------------

/// An account with one unread Priority thread in its Inbox. `subject` is what a
/// stranger wrote.
fn account_with_unread_priority(
    vault: &jatayu_core::Vault,
    address: &str,
    subject: &str,
) -> jatayu_core::account::Account {
    use jatayu_core::account::{Account, Provider};
    use jatayu_core::id::{MailMessageId, PackId, ThreadId};
    use jatayu_core::mail::{
        Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
    };
    use jatayu_core::packstore::PackRef;
    use jatayu_core::store::mail::IngestMessage;

    let mut account = Account::new(Provider::Custom, address);
    account.services.mail = true;
    vault.save_account(&account).unwrap();
    let mailbox = Mailbox::new(account.id, MailboxRole::Inbox.as_str(), MailboxRole::Inbox);
    vault.save_mailbox(&mailbox).unwrap();
    let id = MailMessageId::new();
    let message = Message {
        id,
        account_id: account.id,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{id}@example.com>"),
        date: ts("2026-09-15T14:00:00Z"),
        from: Address::new("Pat Stranger", "pat@elsewhere.example"),
        to: vec![Address::bare(address)],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: "Please look at this".into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: Some(Category::Priority),
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.id.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
        unsubscribe: None,
    };
    vault
        .ingest_mail(account.id, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
        .unwrap();
    account
}

#[test]
fn a_dream_reads_mail_only_from_the_accounts_it_is_given() {
    let (_dir, vault) = vault_for_test();
    let shared = account_with_unread_priority(&vault, "shared@example.com", "Quarterly numbers");
    let private = account_with_unread_priority(&vault, "private@example.com", "Secret reply");
    let now = zoned(2026, 9, 16, 9, 0);

    // Told of one account, it reads that one and not the other: the headers of
    // an account the assistant may not read are not the assistant's to have.
    let d = dream::digest(&vault, DreamScope::Day, &now, &[shared.id]).unwrap();
    let md = d.to_markdown();
    assert!(d.mail.iter().any(|m| m.starts_with("shared@example.com: 1 unread")), "{:?}", d.mail);
    assert!(
        d.mail.iter().any(|m| m.contains("Awaiting reply: \"Quarterly numbers\"")),
        "{:?}",
        d.mail
    );
    assert!(!md.contains("private@example.com") && !md.contains("Secret reply"), "{md}");
    assert!(!d.is_empty(), "mail it may read is something that happened");

    // Told of neither, it reads none at all -- not "every account", which is
    // what it did before it was told anything.
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    assert!(d.mail.is_empty(), "{:?}", d.mail);
    assert!(d.is_empty());

    // Told of both, both -- so it was the list that held the other back, not
    // the mail being absent.
    let d = dream::digest(&vault, DreamScope::Day, &now, &[shared.id, private.id]).unwrap();
    assert!(d.mail.iter().any(|m| m.contains("Secret reply")), "{:?}", d.mail);
    assert!(d.mail.iter().any(|m| m.contains("Quarterly numbers")), "{:?}", d.mail);
}

// ---- what is waiting, and the wider scopes ------------------------------------

#[test]
fn what_is_waiting_for_an_answer_is_named_so_a_dream_does_not_propose_it_twice() {
    use jatayu_core::{Payload, Proposal, ProposedRecord};
    let (_dir, vault) = vault_for_test();
    let record = ProposedRecord::Task(Task::new("Call the dentist"));
    let made = ts("2026-09-14T03:00:00Z");
    let proposal =
        Proposal::new(Payload::Create { record }, "Create task: Call the dentist", made, "UTC")
            .with_why("they wrote it down on Monday");
    vault.save_proposal(&proposal).unwrap();

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    assert_eq!(d.waiting.len(), 1, "{:?}", d.waiting);
    assert_eq!(d.waiting[0], "task: Create task: Call the dentist (made 2026-09-14)");
    assert_eq!(d.pending, 1);
    let md = d.to_markdown();
    assert!(md.contains("## Waiting for an answer"), "{md}");
    assert!(md.contains("Pending proposals: 1 of"), "{md}");

    // Answered, it no longer waits.
    vault.decline_proposal(proposal.id, None, now.timestamp()).unwrap();
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    assert!(d.waiting.is_empty(), "{:?}", d.waiting);
}

#[test]
fn only_so_many_waiting_proposals_are_named_and_the_rest_are_counted() {
    use jatayu_core::{Payload, Proposal, ProposedRecord};
    let (_dir, vault) = vault_for_test();
    for n in 0..(dream::MAX_WAITING_LISTED + 3) {
        let record = ProposedRecord::Task(Task::new(format!("Task {n}")));
        let at = ts("2026-09-14T03:00:00Z") + jiff::SignedDuration::from_secs(n as i64);
        vault
            .save_proposal(&Proposal::new(
                Payload::Create { record },
                format!("Task {n}"),
                at,
                "UTC",
            ))
            .unwrap();
    }
    let d = dream::digest(&vault, DreamScope::Day, &zoned(2026, 9, 16, 9, 0), &[]).unwrap();
    assert_eq!(d.waiting.len(), dream::MAX_WAITING_LISTED + 1, "{:?}", d.waiting);
    assert!(d.waiting.last().unwrap().contains("and 3 more"), "{:?}", d.waiting);
    assert!(d.waiting[0].contains("Task 14"), "newest first: {:?}", d.waiting);
}

#[test]
fn a_week_of_tasks_is_totals_and_a_few_examples_not_the_first_eight_rows() {
    let (_dir, vault) = vault_for_test();
    for n in 0..12 {
        let mut done = Task::new(format!("Finished {n}"));
        done.status = TaskStatus::Done;
        done.created_at = ts("2026-08-01T08:00:00Z");
        done.completed_at = Some(ts("2026-09-14T10:00:00Z") + jiff::SignedDuration::from_secs(n));
        vault.save_task(&done).unwrap();
    }
    let mut late = Task::new("Renew the passport");
    late.due_date = Some(jiff::civil::date(2026, 9, 1));
    vault.save_task(&late).unwrap();
    let mut later = Task::new("Book the boiler service");
    later.due_date = Some(jiff::civil::date(2026, 9, 10));
    vault.save_task(&later).unwrap();

    let now = zoned(2026, 9, 16, 3, 30);
    let d = dream::digest(&vault, DreamScope::Week, &now, &[]).unwrap();
    assert_eq!(d.tasks_completed.len(), 1, "one line of totals: {:?}", d.tasks_completed);
    assert!(
        d.tasks_completed[0].starts_with("12 completed over the week; the latest: "),
        "{:?}",
        d.tasks_completed
    );
    assert!(!d.tasks_completed[0].contains("and 4 more"));
    assert_eq!(d.tasks_overdue.len(), 1);
    assert!(
        d.tasks_overdue[0]
            .starts_with("2 overdue now; the oldest: Renew the passport (due 2026-09-01)"),
        "{:?}",
        d.tasks_overdue
    );
    assert!(d.tasks_overdue[0].contains("Book the boiler service (due 2026-09-10)"));

    // The nightly digest is still row by row, as before.
    let night = dream::digest(&vault, DreamScope::Day, &zoned(2026, 9, 15, 3, 0), &[]).unwrap();
    assert!(
        night.tasks_overdue.iter().any(|t| t == "Renew the passport"),
        "{:?}",
        night.tasks_overdue
    );
}

#[test]
fn a_months_calendar_is_a_count_and_its_busiest_days() {
    use jatayu_core::calendar::Calendar;
    let (_dir, vault) = vault_for_test();
    let calendar = Calendar::subscribed("Work", "webcal://example.com/work.ics");
    vault.save_calendar(&calendar).unwrap();
    let mut feed = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
    for (n, (title, day)) in [
        ("Standup", "20260908T090000Z"),
        ("Planning", "20260908T100000Z"),
        ("Review", "20260908T140000Z"),
        ("Dentist", "20260910T110000Z"),
    ]
    .into_iter()
    .enumerate()
    {
        feed.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:e{n}@example.com\r\nSUMMARY:{title}\r\nDTSTART:{day}\r\n\
             DTEND:{day}\r\nEND:VEVENT\r\n"
        ));
    }
    feed.push_str("END:VCALENDAR\r\n");
    let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31));
    vault.sync_calendar_from_ics(calendar.id, &feed, window, "UTC").unwrap();

    let now = zoned(2026, 9, 30, 4, 0);
    let d = dream::digest(&vault, DreamScope::Month, &now, &[]).unwrap();
    assert_eq!(d.events.len(), 1, "{:?}", d.events);
    assert!(
        d.events[0].starts_with("4 events; busiest days: Tue 8 Sep (3), Thu 10 Sep (1)"),
        "{:?}",
        d.events
    );
}

#[test]
fn the_next_scope_reads_a_nights_account_without_its_ids_and_on_the_persons_day() {
    let (_dir, vault) = vault_for_test();
    let mut settings = vault.agent_settings().unwrap();
    // Los Angeles is behind UTC: a night that finished at 03:05 UTC on the 15th
    // finished at ten past eight on the evening of the 14th, there.
    settings.timezone = Some("America/Los_Angeles".into());
    vault.save_agent_settings(&settings).unwrap();
    let routines = vault.set_dreaming(true).unwrap();
    let nightly =
        routines.iter().find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Day }).unwrap();

    let id = jatayu_core::id::MemoryId::new();
    let mut run = RoutineRun::new(nightly, None);
    run.started_at = ts("2026-09-15T03:00:00Z");
    run.finish(
        RunOutcome::Done,
        format!(
            "1 proposal, 0 memories, 0 notes.\n\nNoticed:\n- Skipped the run (2026-09-13)\n\
             Proposed:\n- Time for piano\nConfirmed memories:\n{id}\n"
        ),
    );
    run.finished_at = Some(ts("2026-09-15T03:05:00Z"));
    vault.save_run(&run).unwrap();

    let la = jiff::civil::date(2026, 9, 20).at(4, 0, 0, 0).in_tz("America/Los_Angeles").unwrap();
    let d = dream::digest(&vault, DreamScope::Week, &la, &[]).unwrap();
    assert_eq!(d.entries.len(), 1, "{:?}", d.entries);
    let entry = &d.entries[0];
    assert!(entry.starts_with("2026-09-14: 1 proposal"), "the person's day, not UTC's: {entry}");
    assert!(entry.contains("Noticed:") && entry.contains("Time for piano"), "{entry}");
    assert!(!entry.contains(&id.to_string()) && !entry.contains("Confirmed memories"), "{entry}");
    assert!(!entry.contains("\n\n"), "no blank line to end its bullet: {entry:?}");
    assert!(d.to_markdown().contains("- 2026-09-14: 1 proposal, 0 memories, 0 notes.\n  Noticed:"));
}

// ---- skills -----------------------------------------------------------------

#[test]
fn the_skills_section_lists_on_and_off_skills_and_stays_ambient() {
    let (_dir, vault) = vault_for_test();

    let mut plan = jatayu_core::Skill::new("Plan a trip");
    plan.description = "Use when asked to plan a trip or a multi-day journey.".into();
    plan.instructions = "Check the calendar, check the weather, propose blocks.".into();
    plan.updated_at = ts("2026-09-10T00:00:00Z");
    vault.save_skill(&plan).unwrap();

    let mut retired = jatayu_core::Skill::new("Retired review");
    retired.description = "No longer run.".into();
    retired.instructions = "Old steps nobody follows now.".into();
    retired.enabled = false;
    retired.updated_at = ts("2026-01-01T00:00:00Z");
    vault.save_skill(&retired).unwrap();

    // Nothing else happened, so the digest would otherwise be empty -- the
    // skills section must not be what breaks that.
    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
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
    let mut conversation = jatayu_core::Conversation::new();
    conversation.updated_at = ts("2000-01-01T00:00:00Z");
    vault.save_conversation(&conversation).unwrap();

    let mut ask =
        jatayu_core::Message::user(conversation.id, "Can you plan a week in Portugal for May?");
    ask.created_at = ts("2026-09-15T09:00:00Z");
    vault.save_message(&ask).unwrap();

    let mut loads_skill =
        jatayu_core::Message::assistant(conversation.id, "").with_tool_calls(vec![
            jatayu_core::ToolCall {
                id: "call_1".into(),
                name: "read_skill".into(),
                arguments: serde_json::json!({ "name": "Plan a trip" }),
            },
        ]);
    loads_skill.created_at = ts("2026-09-15T09:00:05Z");
    vault.save_message(&loads_skill).unwrap();

    let mut follow_up = jatayu_core::Message::user(
        conversation.id,
        "Actually make it ten days, starting the second Monday",
    );
    follow_up.created_at = ts("2026-09-15T09:05:00Z");
    vault.save_message(&follow_up).unwrap();

    // A dream's own transcript, updated the same day, must not be read as
    // one of the person's own conversations.
    let mut transcript =
        jatayu_core::Conversation::for_run(jatayu_core::RoutineRunId::new(), "Nightly run");
    transcript.updated_at = ts("2000-01-01T00:00:00Z");
    vault.save_conversation(&transcript).unwrap();
    let mut dream_message = jatayu_core::Message::user(transcript.id, "the digest itself");
    dream_message.created_at = ts("2026-09-15T03:00:00Z");
    vault.save_message(&dream_message).unwrap();

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
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
        let mut conversation = jatayu_core::Conversation::new();
        conversation.updated_at = ts("2000-01-01T00:00:00Z");
        vault.save_conversation(&conversation).unwrap();
        let mut msg = jatayu_core::Message::user(conversation.id, format!("request {i}"));
        msg.created_at = ts("2026-09-15T09:00:00Z");
        vault.save_message(&msg).unwrap();
    }

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
    assert_eq!(d.conversations.len(), dream::MAX_DIGEST_CONVERSATIONS + 1, "{:?}", d.conversations);
    assert!(d.conversations.last().unwrap().contains("and 2 more"), "{:?}", d.conversations);
}

#[test]
fn the_conversations_section_caps_excerpts_and_their_length() {
    let (_dir, vault) = vault_for_test();
    let mut conversation = jatayu_core::Conversation::new();
    conversation.updated_at = ts("2000-01-01T00:00:00Z");
    vault.save_conversation(&conversation).unwrap();

    let cap = dream::MAX_CONVERSATION_EXCERPTS;
    let long_text = "a".repeat(dream::MAX_EXCERPT_CHARS + 50);
    let mut long_msg = jatayu_core::Message::user(conversation.id, long_text.clone());
    long_msg.created_at = zoned(2026, 9, 15, 9, 0).timestamp();
    vault.save_message(&long_msg).unwrap();

    for i in 1..=(cap + 1) {
        let mut msg = jatayu_core::Message::user(conversation.id, format!("request number {i}"));
        msg.created_at = zoned(2026, 9, 15, 9, i as i8).timestamp();
        vault.save_message(&msg).unwrap();
    }

    let now = zoned(2026, 9, 16, 9, 0);
    let d = dream::digest(&vault, DreamScope::Day, &now, &[]).unwrap();
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
