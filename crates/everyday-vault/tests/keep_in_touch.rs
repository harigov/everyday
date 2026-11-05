//! Keeping in touch against a real vault: what a pass writes, what it asks, and
//! that doing any of it twice changes nothing. The model is not here; what it
//! would have said is handed in as [`Mention`]s.

use everyday_core::keepintouch::{
    CIRCLE_CLOSE, CIRCLE_INNER, FACT_ALIASES, FACT_BIRTHDAY, FACT_CELEBRATES, FACT_CIRCLE,
    FACT_EMAIL, Mention,
};
use everyday_core::library::{CONTACT_SLUG, Item, ItemStatus, LogEntry, LogEvent, LogSource, Via};
use everyday_core::proposal::{
    AboutKind, DeclineReason, Outcome, Payload, ProposalKind, ProposedRecord,
};
use everyday_core::store::library::LogQuery;
use everyday_core::store::proposals::ProposalQuery;
use everyday_core::task::TaskStatus;
use everyday_core::{EntryId, ItemId, Vault, VaultConfig};
use everyday_vault::create;
use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};

const TODAY: Date = date(2026, 11, 5);

fn now() -> Timestamp {
    "2026-11-05T10:00:00Z".parse().unwrap()
}

fn vault_for_test() -> (tempfile::TempDir, Vault) {
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
    vault.seed_library().unwrap();
    vault
        .update_keep_in_touch(|k| {
            k.settings.enabled = true;
        })
        .unwrap();
    (dir, vault)
}

/// Somebody on the Contacts shelf, on it since well before any of these dates.
fn contact(vault: &Vault, name: &str, facts: &[(&str, &str)]) -> Item {
    let shelf = vault.kinds().unwrap().into_iter().find(|k| k.slug == CONTACT_SLUG).unwrap();
    let mut item = Item::new(shelf.id, name);
    for (key, value) in facts {
        item.facts.insert((*key).into(), (*value).into());
    }
    item.created_at = "2026-01-01T00:00:00Z".parse().unwrap();
    vault.save_item(&item).unwrap();
    item
}

fn mention(name: &str, via: Option<Via>, note: &str) -> Mention {
    Mention { name: name.into(), via, note: note.into() }
}

fn logs_of(vault: &Vault, id: ItemId) -> Vec<LogEntry> {
    vault.logs(&LogQuery::for_item(id)).unwrap()
}

fn pending(vault: &Vault, kind: ProposalKind) -> Vec<everyday_core::Proposal> {
    vault.proposals(&ProposalQuery::pending_of(kind)).unwrap()
}

#[test]
fn a_clear_mention_is_logged_once_and_a_deleted_row_stays_deleted() {
    let (_dir, vault) = vault_for_test();
    let ravi = contact(&vault, "Ravi Kumar", &[]);
    let entry = EntryId::new();
    let said = [mention("Ravi", Some(Via::Call), "the move")];

    let done = vault.log_mentions(entry, TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!((done.logged, done.asked), (1, 0));
    let logs = logs_of(&vault, ravi.id);
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].via, Some(Via::Call));
    assert_eq!(logs[0].event, LogEvent::Contact, "a catch-up, and no completion");
    assert_eq!(logs[0].note, "the move");
    assert_eq!(logs[0].source, Some(LogSource::Entry { id: entry }));
    assert_eq!(vault.item(ravi.id).unwrap().status, ItemStatus::Active, "met, not to meet");

    // The same entry read again, as an edit or tomorrow's pass would.
    let again = vault.log_mentions(entry, TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!(again.logged, 0);
    assert_eq!(logs_of(&vault, ravi.id).len(), 1);

    // Deleted by hand: wrong, and not to come back.
    vault.delete_log(logs[0].id).unwrap();
    vault.log_mentions(entry, TODAY, &said, 5, "UTC", now()).unwrap();
    assert!(logs_of(&vault, ravi.id).is_empty());
}

#[test]
fn a_call_logged_by_hand_is_not_logged_again_from_the_journal() {
    let (_dir, vault) = vault_for_test();
    let ravi = contact(&vault, "Ravi", &[]);
    let mut typed = LogEntry::new(ravi.id, LogEvent::Revisited, TODAY, "UTC");
    typed.via = Some(Via::Call);
    vault.save_log(&typed).unwrap();
    let said = [mention("Ravi", None, "")];
    vault.log_mentions(EntryId::new(), TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!(logs_of(&vault, ravi.id).len(), 1);
}

#[test]
fn a_name_that_fits_two_people_is_asked_and_answered_with_a_choice() {
    let (_dir, vault) = vault_for_test();
    let kumar = contact(&vault, "Ravi Kumar", &[]);
    let menon = contact(&vault, "Ravi Menon", &[]);
    let entry = EntryId::new();
    let said = [mention("Ravi", Some(Via::InPerson), "lunch")];

    let done = vault.log_mentions(entry, TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!((done.logged, done.asked), (0, 1));
    let asks = pending(&vault, ProposalKind::Log);
    assert_eq!(asks.len(), 1);
    let p = &asks[0];
    assert_eq!(p.caption, "Which Ravi were you in touch with on 5 November?");
    assert_eq!(p.about.as_ref().map(|a| a.kind), Some(AboutKind::Entry));

    // Asked again, the same question is not asked twice.
    vault.log_mentions(entry, TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!(pending(&vault, ProposalKind::Log).len(), 1);

    // Accepting without saying who is refused, and leaves it waiting.
    assert!(vault.accept_proposal(p.id, None, false, now()).is_err());
    assert!(vault.proposal(p.id).unwrap().is_pending());

    let Payload::Create { record: ProposedRecord::Log(mut log) } = p.payload.clone() else {
        panic!("a catch-up proposal");
    };
    assert_eq!(log.candidates.len(), 2);
    log.log.item_id = menon.id;
    vault.accept_proposal(p.id, Some(ProposedRecord::Log(log)), false, now()).unwrap();
    assert_eq!(logs_of(&vault, menon.id).len(), 1);
    assert!(logs_of(&vault, kumar.id).is_empty());
}

#[test]
fn somebody_new_is_offered_as_a_contact_and_a_no_is_remembered() {
    let (_dir, vault) = vault_for_test();
    let said = [mention("Priya", Some(Via::Message), "her new job")];
    let done = vault.log_mentions(EntryId::new(), TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!(done.asked, 1);
    let p = pending(&vault, ProposalKind::Log).remove(0);
    assert_eq!(p.caption, "Add Priya to Contacts? You were in touch on 5 November");

    vault.accept_proposal(p.id, None, false, now()).unwrap();
    let priya = vault.contacts().unwrap().into_iter().find(|c| c.title == "Priya").unwrap();
    assert_eq!(priya.status, ItemStatus::Active);
    assert_eq!(logs_of(&vault, priya.id)[0].note, "her new job");

    // Somebody else, declined as nobody to keep: never asked about again.
    let said = [mention("Dr Shah", None, "")];
    vault.log_mentions(EntryId::new(), TODAY, &said, 5, "UTC", now()).unwrap();
    let p = pending(&vault, ProposalKind::Log).remove(0);
    vault.decline_proposal(p.id, Some(DeclineReason::NeverThis), now()).unwrap();
    let done = vault.log_mentions(EntryId::new(), TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!(done.asked, 0);
}

#[test]
fn asking_stops_at_the_allowance() {
    let (_dir, vault) = vault_for_test();
    let said: Vec<_> = ["Ann", "Bo", "Cy"].iter().map(|n| mention(n, None, "")).collect();
    let done = vault.log_mentions(EntryId::new(), TODAY, &said, 2, "UTC", now()).unwrap();
    assert_eq!(done.asked, 2);
}

#[test]
fn whoever_is_furthest_past_due_is_proposed_within_the_weeks_allowance() {
    let (_dir, vault) = vault_for_test();
    vault.update_keep_in_touch(|k| k.settings.per_week = 1).unwrap();
    let inner = contact(&vault, "Asha", &[(FACT_CIRCLE, CIRCLE_INNER)]);
    let close = contact(&vault, "Ben", &[(FACT_CIRCLE, CIRCLE_CLOSE)]);
    let _nobody = contact(&vault, "Cara", &[]);
    for (who, day) in [(&inner, date(2026, 10, 1)), (&close, date(2026, 9, 1))] {
        vault.save_log(&LogEntry::new(who.id, LogEvent::Revisited, day, "UTC")).unwrap();
    }

    let due = vault.people_due(TODAY, "UTC").unwrap();
    assert_eq!(due.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Asha", "Ben"]);

    assert_eq!(vault.suggest_people(TODAY, now(), "UTC").unwrap(), 1);
    let tasks = pending(&vault, ProposalKind::Task);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].caption, "Get in touch with Asha");
    assert_eq!(tasks[0].about.as_ref().map(|a| a.id.clone()), Some(inner.id.to_string()));
    assert!(tasks[0].why.starts_with("Every 14 days, and it has been 35."));

    // The allowance is spent for the week; and a week on, Asha is cooling off.
    assert_eq!(vault.suggest_people(TODAY, now(), "UTC").unwrap(), 0);
    vault.update_keep_in_touch(|k| k.settings.per_week = 5).unwrap();
    assert_eq!(vault.suggest_people(TODAY, now(), "UTC").unwrap(), 1, "Ben, and not Asha again");
}

#[test]
fn ticking_off_an_accepted_suggestion_counts_as_the_catch_up() {
    let (_dir, vault) = vault_for_test();
    let asha = contact(&vault, "Asha", &[(FACT_CIRCLE, CIRCLE_INNER)]);
    vault.suggest_people(TODAY, now(), "UTC").unwrap();
    let p = pending(&vault, ProposalKind::Task).remove(0);
    let accepted = vault.accept_proposal(p.id, None, false, now()).unwrap();
    let Outcome::Accepted { saved_as, .. } = accepted.outcome else { panic!("accepted") };
    let mut task = vault.task(saved_as.parse().unwrap()).unwrap();

    // Still open: nothing to count, and Asha is not proposed again meanwhile.
    assert_eq!(vault.settle_touch_tasks(now(), "UTC").unwrap(), 0);
    vault.update_keep_in_touch(|k| k.suggested.clear()).unwrap();
    assert_eq!(vault.suggest_people(TODAY, now(), "UTC").unwrap(), 0, "a task is already open");

    task.status = TaskStatus::Done;
    task.completed_at = Some(now());
    vault.save_task(&task).unwrap();
    assert_eq!(vault.settle_touch_tasks(now(), "UTC").unwrap(), 1);
    let logs = logs_of(&vault, asha.id);
    assert_eq!(logs[0].source, Some(LogSource::Task { id: task.id }));
    assert!(vault.keep_in_touch().unwrap().tracked.is_empty(), "settled, and no longer followed");
}

#[test]
fn birthdays_and_festivals_are_proposed_once_ahead_of_the_day() {
    let (_dir, vault) = vault_for_test();
    let ravi =
        contact(&vault, "Ravi", &[(FACT_BIRTHDAY, "1988-11-07"), (FACT_CELEBRATES, "Diwali")]);
    contact(&vault, "Priya", &[(FACT_CELEBRATES, "Diwali\nChristmas")]);
    contact(&vault, "Sam", &[(FACT_CELEBRATES, "Christmas")]);

    let calendar = everyday_core::Calendar::subscribed("Holidays", "https://example.com/h.ics");
    vault.save_calendar(&calendar).unwrap();
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:diwali@example.com\r\nSUMMARY:Diwali/Deepavali\r\n\
         DTSTART;VALUE=DATE:20261106\r\nDTEND;VALUE=DATE:20261107\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n";
    let window = (date(2026, 1, 1), date(2026, 12, 31));
    vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();

    assert_eq!(vault.propose_occasions(TODAY, now(), "UTC").unwrap(), 2);
    let tasks = pending(&vault, ProposalKind::Task);
    let birthday = tasks.iter().find(|p| p.caption == "Wish Ravi a happy birthday").unwrap();
    assert_eq!(birthday.target_date, Some(date(2026, 11, 7)));
    let Some(ProposedRecord::Task(task)) = birthday.payload.record() else { panic!("a task") };
    assert_eq!(task.remind_at, Some("2026-11-07T09:00:00Z".parse().unwrap()));
    assert_eq!(birthday.about.as_ref().map(|a| a.id.clone()), Some(ravi.id.to_string()));

    let diwali = tasks.iter().find(|p| p.caption == "Send Diwali/Deepavali wishes").unwrap();
    let Some(ProposedRecord::Task(task)) = diwali.payload.record() else { panic!("a task") };
    assert_eq!(task.title, "Send Diwali/Deepavali wishes to Priya and Ravi");

    // Tomorrow's pass, and one after a decline: neither proposes them again.
    vault.decline_proposal(diwali.id, None, now()).unwrap();
    let tomorrow = now() + SignedDuration::from_hours(24);
    assert_eq!(vault.propose_occasions(date(2026, 11, 6), tomorrow, "UTC").unwrap(), 0);
}

#[test]
fn a_small_meeting_with_somebody_counts_and_an_all_hands_does_not() {
    let (_dir, vault) = vault_for_test();
    let ravi = contact(&vault, "Ravi Kumar", &[(FACT_EMAIL, "ravi@example.com")]);
    let calendar = everyday_core::Calendar::subscribed("Work", "https://example.com/w.ics");
    vault.save_calendar(&calendar).unwrap();
    let crowd: String =
        (0..8).map(|i| format!("ATTENDEE;CN=Person {i}:mailto:p{i}@example.com\r\n")).collect();
    let feed = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:coffee@example.com\r\nSUMMARY:Coffee\r\nLOCATION:Cafe\r\n\
         DTSTART:20261104T090000Z\r\nDTEND:20261104T093000Z\r\n\
         ATTENDEE;CN=Ravi Kumar:mailto:ravi@example.com\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:allhands@example.com\r\nSUMMARY:All hands\r\n\
         DTSTART:20261103T090000Z\r\nDTEND:20261103T100000Z\r\n\
         ATTENDEE;CN=Ravi Kumar:mailto:ravi@example.com\r\n{crowd}END:VEVENT\r\n\
         END:VCALENDAR\r\n"
    );
    let window = (date(2026, 1, 1), date(2026, 12, 31));
    vault.sync_calendar_from_ics(calendar.id, &feed, window, "UTC").unwrap();

    assert_eq!(vault.log_meetings(date(2026, 11, 1), now(), "UTC").unwrap(), 1);
    let logs = logs_of(&vault, ravi.id);
    assert_eq!(logs.len(), 1);
    assert_eq!((logs[0].date, logs[0].via), (date(2026, 11, 4), Some(Via::InPerson)));
    assert_eq!(logs[0].note, "", "the calendar's words stay on the calendar");
    assert_eq!(vault.log_meetings(date(2026, 11, 1), now(), "UTC").unwrap(), 0, "once");
}

#[test]
fn a_meeting_every_week_is_a_catch_up_every_week() {
    let (_dir, vault) = vault_for_test();
    let ravi = contact(&vault, "Ravi Kumar", &[(FACT_EMAIL, "ravi@example.com")]);
    let calendar = everyday_core::Calendar::subscribed("Work", "https://example.com/w.ics");
    vault.save_calendar(&calendar).unwrap();
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:one-to-one@example.com\r\nSUMMARY:One to one\r\n\
         DTSTART:20261028T090000Z\r\nDTEND:20261028T093000Z\r\nRRULE:FREQ=WEEKLY;COUNT=2\r\n\
         ATTENDEE;CN=Ravi Kumar:mailto:ravi@example.com\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n";
    let window = (date(2026, 1, 1), date(2026, 12, 31));
    vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();
    assert_eq!(vault.log_meetings(date(2026, 10, 25), now(), "UTC").unwrap(), 2);
    let days: Vec<Date> = logs_of(&vault, ravi.id).into_iter().map(|l| l.date).collect();
    assert!(days.contains(&date(2026, 10, 28)) && days.contains(&date(2026, 11, 4)), "{days:?}");
}

#[test]
fn changing_a_cards_status_does_not_reset_the_clock() {
    let (_dir, vault) = vault_for_test();
    let asha = contact(&vault, "Asha", &[(FACT_CIRCLE, CIRCLE_INNER)]);
    vault.save_log(&LogEntry::new(asha.id, LogEvent::Contact, date(2026, 10, 1), "UTC")).unwrap();
    // What moving the card to "In touch" and then "Met" writes.
    vault.save_log(&LogEntry::new(asha.id, LogEvent::Started, TODAY, "UTC")).unwrap();
    vault.save_log(&LogEntry::new(asha.id, LogEvent::Finished, TODAY, "UTC")).unwrap();
    let due = vault.people_due(TODAY, "UTC").unwrap();
    assert_eq!(due[0].last_contact, Some(date(2026, 10, 1)));
    assert_eq!(due[0].days_over, 21);
}

#[test]
fn a_full_set_of_proposals_does_not_stop_the_clear_names_being_logged() {
    let (_dir, vault) = vault_for_test();
    let ravi = contact(&vault, "Ravi", &[]);
    for i in 0..everyday_core::proposal::MAX_PENDING_PROPOSALS {
        let note = everyday_core::note::Note::written(format!("n{i}"), "x");
        let p = everyday_core::Proposal::new(
            Payload::Create { record: ProposedRecord::Note(note) },
            "Create note",
            now(),
            "UTC",
        );
        vault.save_proposal(&p).unwrap();
    }
    let said = [mention("Somebody New", None, ""), mention("Ravi", None, "")];
    let done = vault.log_mentions(EntryId::new(), TODAY, &said, 5, "UTC", now()).unwrap();
    assert_eq!((done.logged, done.asked), (1, 0));
    assert_eq!(logs_of(&vault, ravi.id).len(), 1);
}

#[test]
fn festival_tasks_are_keeping_in_touchs_and_leave_the_stop_list_alone() {
    let (_dir, vault) = vault_for_test();
    contact(&vault, "Ravi", &[(FACT_BIRTHDAY, "1988-11-07")]);
    vault.propose_occasions(TODAY, now(), "UTC").unwrap();
    let made = pending(&vault, ProposalKind::Task);
    assert_eq!(made[0].made_by, Some(everyday_core::ProposalSource::KeepInTouch));
    vault.decline_proposal(made[0].id, None, now()).unwrap();
    vault.purge_proposals(now() + SignedDuration::from_hours(24 * 8)).unwrap();
    assert!(vault.proposal_traces().unwrap().is_empty());
}

#[test]
fn an_unaccepted_proposal_is_gone_a_week_after_it_was_made() {
    let (_dir, vault) = vault_for_test();
    contact(&vault, "Asha", &[(FACT_CIRCLE, CIRCLE_INNER)]);
    contact(&vault, "Ben", &[(FACT_CIRCLE, CIRCLE_INNER), (FACT_ALIASES, "B")]);
    vault.suggest_people(TODAY, now(), "UTC").unwrap();
    let made = pending(&vault, ProposalKind::Task);
    assert_eq!(made.len(), 2);
    vault.decline_proposal(made[0].id, None, now()).unwrap();
    vault.accept_proposal(made[1].id, None, false, now()).unwrap();

    let six_days = now() + SignedDuration::from_hours(24 * 6);
    assert!(vault.purge_proposals(six_days).unwrap().is_empty());
    let eight_days = now() + SignedDuration::from_hours(24 * 8);
    assert_eq!(vault.purge_proposals(eight_days).unwrap(), vec![made[0].id]);
    let left = vault.proposals(&ProposalQuery::default()).unwrap();
    assert_eq!(left.len(), 1, "the accepted one stays");
    assert!(vault.proposal_traces().unwrap().is_empty(), "keeping in touch leaves no trace");
}

#[test]
fn catch_ups_are_not_counted_as_things_finished() {
    let (_dir, vault) = vault_for_test();
    let ravi = contact(&vault, "Ravi", &[]);
    vault.save_log(&LogEntry::new(ravi.id, LogEvent::Revisited, date(2026, 3, 1), "UTC")).unwrap();
    assert_eq!(vault.library_stats(2026).unwrap().finished_this_year, 0);
}
