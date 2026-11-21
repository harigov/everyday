//! Briefs, wired through a real vault: switching one on and the routine it
//! makes, the refusals that keep it the application's, the digest built from
//! seeded records, and reading the briefs back.

use karmayoga_core::brief::{self, BriefKind, BriefSection, BriefSpec, Extras};
use karmayoga_core::calendar::Calendar;
use karmayoga_core::routine::{Outcome, Routine, RoutineKind, RoutineRun, Trigger};
use karmayoga_core::task::{Priority, Task};
use karmayoga_core::{Error, VaultConfig};
use karmayoga_vault::create;

fn vault_for_test() -> (tempfile::TempDir, karmayoga_core::Vault) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
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

#[test]
fn switching_a_brief_on_makes_its_routine_once() {
    let (_dir, vault) = vault_for_test();
    assert!(vault.brief_routine(BriefKind::Morning).unwrap().is_none());

    let routine = vault.set_brief(BriefKind::Morning, true).unwrap();
    assert!(routine.enabled);
    assert_eq!(routine.name, "Morning brief");
    assert_eq!(routine.kind.brief_kind(), Some(BriefKind::Morning));
    assert_eq!(routine.kind.brief().unwrap().sections, BriefSection::defaults(BriefKind::Morning));

    let off = vault.set_brief(BriefKind::Morning, false).unwrap();
    assert_eq!(off.id, routine.id, "switching off keeps the same routine");
    assert!(!off.enabled);
    let again = vault.set_brief(BriefKind::Morning, true).unwrap();
    assert_eq!(again.id, routine.id);
    assert_eq!(vault.routines().unwrap().len(), 1);

    let evening = vault.set_brief(BriefKind::Evening, true).unwrap();
    assert_ne!(evening.id, routine.id);
    assert_eq!(vault.routines().unwrap().len(), 2);
}

#[test]
fn a_brief_is_the_applications_but_what_it_covers_is_the_persons() {
    let (_dir, vault) = vault_for_test();
    let mut routine = vault.set_brief(BriefKind::Morning, true).unwrap();

    // What it covers, how long, when: ordinary edits.
    let mut spec = routine.kind.brief().unwrap().clone();
    spec.sections = vec![BriefSection::Weather, BriefSection::Schedule];
    spec.length = brief::BriefLength::Short;
    routine.kind = RoutineKind::Brief(spec);
    routine.instructions = "End with one line about my Spanish.".into();
    vault.save_routine(&routine).unwrap();
    let saved = vault.routine(routine.id).unwrap();
    assert_eq!(
        saved.kind.brief().unwrap().sections,
        vec![BriefSection::Weather, BriefSection::Schedule]
    );

    // Not into another brief, nor into a person's routine.
    let mut other = saved.clone();
    other.kind = RoutineKind::Brief(BriefSpec::new(BriefKind::Evening));
    assert!(matches!(vault.save_routine(&other), Err(Error::Invalid(_))));
    let mut custom = saved.clone();
    custom.kind = RoutineKind::Custom;
    assert!(matches!(vault.save_routine(&custom), Err(Error::Invalid(_))));

    // Not made by hand, and not deleted.
    let mut by_hand = Routine::new("My brief", "", Trigger::Manual);
    by_hand.kind = RoutineKind::Brief(BriefSpec::new(BriefKind::Morning));
    assert!(matches!(vault.save_routine(&by_hand), Err(Error::Invalid(_))));
    assert!(matches!(vault.delete_routine(routine.id), Err(Error::Invalid(_))));

    // And it never asks.
    let mut asks = saved.clone();
    asks.delivery.ask = true;
    asks.delivery.chat = Some(karmayoga_core::id::ChatLinkId::new());
    assert!(matches!(vault.save_routine(&asks), Err(Error::Invalid(_))));
}

#[test]
fn the_digest_reads_the_day_from_the_vault() {
    let (_dir, vault) = vault_for_test();
    let calendar =
        Calendar::subscribed("Work", "webcal://example.com/work.ics").with_color("#0f766e");
    vault.save_calendar(&calendar).unwrap();
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:renewal@example.com\r\nSUMMARY:Acme renewal\r\n\
         DTSTART:20261121T140000Z\r\nDTEND:20261121T150000Z\r\n\
         LOCATION:Acme HQ\r\n\
         ORGANIZER;CN=Priya Shah:mailto:priya@acme.example\r\n\
         ATTENDEE;CN=Priya Shah:mailto:priya@acme.example\r\n\
         ATTENDEE;CN=Tom Li:mailto:tom@acme.example\r\n\
         END:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:dentist@example.com\r\nSUMMARY:Dentist\r\n\
         DTSTART:20261121T143000Z\r\nDTEND:20261121T153000Z\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:tomorrow@example.com\r\nSUMMARY:Not today\r\n\
         DTSTART:20261122T090000Z\r\nDTEND:20261122T100000Z\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n";
    let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31));
    vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();

    let mut due = Task::new("Send the grant report");
    due.due_date = Some(jiff::civil::date(2026, 11, 21));
    due.priority = Priority::High;
    vault.save_task(&due).unwrap();
    let mut late = Task::new("File expenses");
    late.due_date = Some(jiff::civil::date(2026, 11, 12));
    vault.save_task(&late).unwrap();

    let spec = BriefSpec::new(BriefKind::Morning);
    let now = zoned(2026, 11, 21, 7, 0);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();

    let titles: Vec<_> = d.events.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Acme renewal", "Dentist"], "today's, in time order");
    assert!(d.events.iter().all(|e| e.clash), "the two overlap");
    assert_eq!(d.events[0].color, "#0f766e");
    assert_eq!(d.events[0].calendar, "Work");
    assert!(d.free.iter().any(|f| f.minutes >= 300), "the morning is free: {:?}", d.free);

    let people: Vec<_> = d.people.iter().map(|p| p.name.as_str()).collect();
    assert!(people.contains(&"Priya Shah") && people.contains(&"Tom Li"), "{people:?}");

    assert_eq!(d.due.len(), 1);
    assert_eq!(d.due[0].title, "Send the grant report");
    assert_eq!(d.overdue_count, 1);
    assert_eq!(d.overdue[0].title, "File expenses");

    assert!(d.mail.is_empty());
    assert!(d.gaps.iter().any(|g| g.starts_with("mail:")), "no account is shared: {:?}", d.gaps);
    assert!(d.gaps.iter().any(|g| g.starts_with("weather:")), "{:?}", d.gaps);

    let md = d.to_markdown();
    assert!(md.contains("14:00\u{2013}15:00 Acme renewal"), "{md}");
    assert!(md.contains("CLASHES"), "{md}");
    assert!(md.contains("Send the grant report (high priority)"), "{md}");
    assert!(md.contains("1 day(s) late") || md.contains("9 day(s) late"), "{md}");
}

#[test]
fn the_evening_brief_looks_at_tomorrow() {
    let (_dir, vault) = vault_for_test();
    let mut tomorrow = Task::new("Pack for Lisbon");
    tomorrow.due_date = Some(jiff::civil::date(2026, 11, 22));
    vault.save_task(&tomorrow).unwrap();
    let mut slipped = Task::new("Send the grant report");
    slipped.due_date = Some(jiff::civil::date(2026, 11, 21));
    vault.save_task(&slipped).unwrap();
    let spec = BriefSpec::new(BriefKind::Evening);
    let now = zoned(2026, 11, 21, 20, 0);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();
    assert_eq!(d.ahead, Some(jiff::civil::date(2026, 11, 22)));
    assert_eq!(d.due.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), vec!["Pack for Lisbon"]);
    assert!(d.to_markdown().contains("## Today, so far"));
    // Today's own, still open at eight, is what slipped: the evening says so.
    assert_eq!(
        d.overdue.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        vec!["Send the grant report"]
    );
    assert!(d.to_markdown().contains("Still open, due today or before: 1"));
}

#[test]
fn briefs_lists_only_runs_that_kept_one_newest_first() {
    let (_dir, vault) = vault_for_test();
    let routine = vault.set_brief(BriefKind::Morning, true).unwrap();
    let digest = brief::BriefDigest::default();
    for (day, headline) in [(20, "Friday"), (21, "Saturday")] {
        let mut run = RoutineRun::new(&routine, None);
        run.started_at = format!("2026-11-{day}T07:00:00Z").parse().unwrap();
        run.outcome = Outcome::Done;
        run.brief = Some(Box::new(brief::Brief {
            kind: BriefKind::Morning,
            day: jiff::civil::date(2026, 11, day),
            headline: headline.into(),
            parts: Vec::new(),
            spoken: String::new(),
            digest: digest.clone(),
        }));
        vault.save_run(&run).unwrap();
    }
    // A failed run has no brief and is not one.
    let mut failed = RoutineRun::new(&routine, None);
    failed.fail("no model");
    vault.save_run(&failed).unwrap();

    let briefs = vault.briefs(None, 10).unwrap();
    let headlines: Vec<_> =
        briefs.iter().map(|r| r.brief.as_ref().unwrap().headline.as_str()).collect();
    assert_eq!(headlines, vec!["Saturday", "Friday"]);
    assert_eq!(vault.briefs(Some(BriefKind::Evening), 10).unwrap().len(), 0);

    // And the brief survives the round trip through the sealed store.
    let back = vault.run(briefs[0].id).unwrap();
    assert_eq!(back.brief.unwrap().day, jiff::civil::date(2026, 11, 21));
}

#[test]
fn the_weekly_brief_looks_at_today_and_the_week_ahead() {
    let (_dir, vault) = vault_for_test();
    let weekly = vault.set_brief(BriefKind::Weekly, true).unwrap();
    assert_eq!(weekly.name, "Weekly brief");
    // Sundays, by default -- 22 November 2026 is one.
    assert!(weekly.runs_on(jiff::civil::date(2026, 11, 22)));
    assert!(!weekly.runs_on(jiff::civil::date(2026, 11, 23)));

    let calendar = Calendar::subscribed("Work", "webcal://example.com/w.ics");
    vault.save_calendar(&calendar).unwrap();
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:board@example.com\r\nSUMMARY:Board meeting\r\n\
         DTSTART:20261123T100000Z\r\nDTEND:20261123T113000Z\r\n\
         ATTENDEE;CN=Priya Shah:mailto:priya@acme.example\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:trip@example.com\r\nSUMMARY:Lisbon\r\n\
         DTSTART;VALUE=DATE:20261125\r\nDTEND;VALUE=DATE:20261127\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:later@example.com\r\nSUMMARY:Too far\r\n\
         DTSTART:20261201T100000Z\r\nDTEND:20261201T110000Z\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n";
    let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31));
    vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();
    let mut friday = Task::new("Draft the Q1 plan");
    friday.due_date = Some(jiff::civil::date(2026, 11, 27));
    vault.save_task(&friday).unwrap();
    let mut done = Task::new("Board slides");
    done.status = karmayoga_core::task::TaskStatus::Done;
    done.completed_at = Some("2026-11-19T15:00:00Z".parse().unwrap());
    vault.save_task(&done).unwrap();

    let spec = weekly.kind.brief().unwrap().clone();
    let now = zoned(2026, 11, 22, 8, 0);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();

    assert_eq!(d.until, Some(jiff::civil::date(2026, 11, 29)));
    assert_eq!(d.week.len(), 7, "Monday to Sunday after today");
    let monday = &d.week[0];
    assert_eq!(monday.date, Some(jiff::civil::date(2026, 11, 23)));
    assert_eq!((monday.meetings, monday.minutes), (1, 90));
    assert_eq!(monday.events[0].title, "Board meeting");
    assert_eq!(d.week[2].all_day, vec!["Lisbon".to_string()], "Wednesday");
    assert_eq!(d.week[4].due, vec!["Draft the Q1 plan".to_string()], "Friday");
    assert!(d.week.iter().all(|day| day.events.iter().all(|e| e.title != "Too far")));

    assert!(d.people.iter().any(|p| p.name == "Priya Shah"), "people come from the whole week");
    assert_eq!(
        d.due.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        vec!["Draft the Q1 plan"]
    );
    assert_eq!(d.done.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), vec!["Board slides"]);

    let md = d.to_markdown();
    assert!(
        md.contains("## The week ahead, Monday 23 November 2026 to Sunday 29 November 2026"),
        "{md}"
    );
    assert!(md.contains("## Last seven days"), "{md}");
}
