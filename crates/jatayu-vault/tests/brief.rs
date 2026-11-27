//! Briefs, wired through a real vault: switching one on and the routine it
//! makes, the refusals that keep it the application's, the digest built from
//! seeded records, and reading the briefs back.

use jatayu_core::account::{Account, Provider};
use jatayu_core::brief::{
    self, BriefKind, BriefSection, BriefSpec, ExcerptFrom, Extras, PodcastBrief, PodcastDigest,
    PodcastStory,
};
use jatayu_core::calendar::Calendar;
use jatayu_core::id::{FileId, MailMessageId, PackId, ThreadId};
use jatayu_core::mail::{
    Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
};
use jatayu_core::news::{
    BodyOrigin, ConsumeMode, Consumption, ContentRef, Enclosure, NewsSource, SourceKind, Story,
    StoryBody,
};
use jatayu_core::packstore::PackRef;
use jatayu_core::routine::{Outcome, Routine, RoutineKind, RoutineRun, Trigger};
use jatayu_core::store::mail::IngestMessage;
use jatayu_core::task::{Priority, Task};
use jatayu_core::{Error, Vault, VaultConfig};
use jatayu_vault::create;
use jiff::SignedDuration;
use jiff::Timestamp;
use jiff::civil::time;

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
    asks.delivery.chat = Some(jatayu_core::id::ChatLinkId::new());
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
            script: None,
            audio: None,
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
    done.status = jatayu_core::task::TaskStatus::Done;
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

#[test]
fn the_weekly_brief_connects_last_week_to_their_areas_and_goals() {
    use jatayu_core::purpose::{Area, Goal};
    use jatayu_core::task::{BlockKind, BlockSubject, Project, TaskStatus, TimeBlock};
    let (_dir, vault) = vault_for_test();
    let (work, fun) = (Area::new("Work"), Area::new("Fun"));
    vault.save_area(&work).unwrap();
    vault.save_area(&fun).unwrap();
    let mut book = Goal::new(work.id, "Ship the book");
    book.notes =
        "Tier 1 -- the one that matters this year\nA second line, which is not quoted".into();
    book.horizon = Some(jiff::civil::date(2027, 3, 1));
    vault.save_goal(&book).unwrap();

    // A delegated project under it with nothing finished in it, due on Wednesday.
    let mut project = Project::new("Copy edit");
    project.purpose = Some(book.purpose());
    project.tags = vec!["delegated".into()];
    project.due_date = Some(jiff::civil::date(2026, 11, 25));
    vault.save_project(&project).unwrap();
    let mut cover = Task::new("Approve the cover").in_project(project.id);
    cover.due_date = Some(jiff::civil::date(2026, 11, 26));
    vault.save_task(&cover).unwrap();

    // What slipped: due last Wednesday and still open, for the goal.
    let mut contract = Task::new("Send the contract");
    contract.due_date = Some(jiff::civil::date(2026, 11, 18));
    contract.purpose = Some(book.purpose());
    vault.save_task(&contract).unwrap();
    // What got done, for the goal.
    let mut outline = Task::new("Outline chapter 9");
    outline.status = TaskStatus::Done;
    outline.completed_at = Some("2026-11-19T15:00:00Z".parse().unwrap());
    outline.purpose = Some(book.purpose());
    vault.save_task(&outline).unwrap();
    // Two hours at work last week and one more; nothing at all for Fun. (What
    // is past is what was done: the clock settles planned against actual.)
    let start: Timestamp = "2026-11-18T10:00:00Z".parse().unwrap();
    let morning = TimeBlock::new(BlockSubject::Adhoc, start, 120, "UTC")
        .of_kind(BlockKind::Actual)
        .for_purpose(jatayu_core::purpose::Purpose::Area { id: work.id });
    vault.save_block(&morning).unwrap();
    let afternoon =
        TimeBlock::new(BlockSubject::Adhoc, start + SignedDuration::from_hours(5), 60, "UTC")
            .of_kind(BlockKind::Actual)
            .for_purpose(jatayu_core::purpose::Purpose::Area { id: work.id });
    vault.save_block(&afternoon).unwrap();

    let spec = BriefSpec::new(BriefKind::Weekly);
    let now = zoned(2026, 11, 22, 8, 0);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();

    // Goal lines carry the first line of their notes, where the tier lives.
    assert_eq!(
        d.active_goals,
        vec!["Ship the book (Work) -- Tier 1 -- the one that matters this year -- by 2027-03-01"]
    );
    // Where the time went, by area; an area with none says so.
    assert_eq!(d.areas_last_week.len(), 2, "{:?}", d.areas_last_week);
    assert!(d.areas_last_week.contains(&"Work: 3 h actual".to_string()), "{:?}", d.areas_last_week);
    assert!(d.areas_last_week.contains(&"Fun: nothing".to_string()), "{:?}", d.areas_last_week);
    // What slipped, with how late and what for.
    assert_eq!(d.slipped.len(), 1, "{:?}", d.slipped);
    assert!(d.slipped[0].starts_with("Send the contract (for Ship the book)"), "{:?}", d.slipped);
    assert!(d.slipped[0].ends_with("4 day(s) late"), "{:?}", d.slipped);
    // How the goal stands, and the project under it.
    assert_eq!(d.goal_progress.len(), 1, "{:?}", d.goal_progress);
    let line = &d.goal_progress[0];
    assert!(line.starts_with("Ship the book (Work): last touched"), "{line}");
    assert!(line.contains("open tasks 2; done last week 1; due this week 1"), "{line}");
    assert!(
        line.ends_with("projects: Copy edit (due Wed, #delegated, nothing done in 14 days)"),
        "{line}"
    );

    let md = d.to_markdown();
    assert!(md.contains("Where the time went in those seven days, by area:\n- "), "{md}");
    assert!(md.contains("\n- Work: 3 h actual\n") && md.contains("\n- Fun: nothing\n"), "{md}");
    assert!(md.contains("Due in those seven days and still open:\n- Send the contract"), "{md}");
    assert!(md.contains("## Goals and projects\n- Ship the book (Work): "), "{md}");
    assert!(md.contains("Active goals (with the first line of their notes"), "{md}");
    assert!(!md.contains("a second line, which is not quoted"), "only the first line: {md}");

    // Not for the other briefs: they never ask for it.
    for kind in [BriefKind::Morning, BriefKind::Evening] {
        let d = brief::digest(&vault, &BriefSpec::new(kind), &now, Extras::default()).unwrap();
        assert!(
            d.areas_last_week.is_empty() && d.goal_progress.is_empty() && d.slipped.is_empty(),
            "{kind:?}"
        );
    }
    // And a weekly brief that asks for neither goals nor a review reads none.
    let mut lean = BriefSpec::new(BriefKind::Weekly);
    lean.sections = vec![BriefSection::Week];
    let d = brief::digest(&vault, &lean, &now, Extras::default()).unwrap();
    assert!(d.areas_last_week.is_empty() && d.goal_progress.is_empty() && d.slipped.is_empty());
}

#[test]
fn an_older_stored_brief_without_the_weekly_fields_still_reads() {
    // A digest sealed before `areasLastWeek`, `goalProgress`, `slipped` and
    // `meetingNotes` existed: none of them is there, and it decodes anyway.
    let old = serde_json::json!({ "kind": "weekly", "week": [], "zone": "UTC", "done": [] });
    let digest: brief::BriefDigest = serde_json::from_value(old).unwrap();
    assert!(digest.areas_last_week.is_empty() && digest.goal_progress.is_empty());
    assert!(digest.slipped.is_empty() && digest.meeting_notes.is_empty());
    let json = serde_json::to_value(&digest).unwrap();
    for field in ["areasLastWeek", "goalProgress", "slipped", "meetingNotes"] {
        assert!(json.get(field).is_some_and(|v| v.is_array()), "{field} is camelCase on the wire");
    }
}

#[test]
fn the_evening_review_names_the_meeting_notes_finished_today() {
    use jatayu_core::id::{NoteId, TemplateId};
    use jatayu_core::meeting::{Recording, Stage};
    let (_dir, vault) = vault_for_test();
    let note = NoteId::new();
    let mut finished = Recording::new("Design sync", None, TemplateId::new());
    finished.started_at = "2026-11-21T14:00:00Z".parse().unwrap();
    finished.stage = Stage::Done;
    finished.note_id = Some(note);
    vault.save_recording(&finished).unwrap();
    // Yesterday's, and one still being written up, are not today's finished.
    let mut yesterday = Recording::new("Old sync", None, TemplateId::new());
    yesterday.started_at = "2026-11-20T14:00:00Z".parse().unwrap();
    yesterday.stage = Stage::Done;
    yesterday.note_id = Some(NoteId::new());
    vault.save_recording(&yesterday).unwrap();
    let mut writing = Recording::new("Standup", None, TemplateId::new());
    writing.started_at = "2026-11-21T15:00:00Z".parse().unwrap();
    vault.save_recording(&writing).unwrap();

    let spec = BriefSpec::new(BriefKind::Evening);
    let d = brief::digest(&vault, &spec, &zoned(2026, 11, 21, 20, 0), Extras::default()).unwrap();
    assert_eq!(d.meeting_notes, vec![format!("\"Design sync\" (note {note})")]);
    let md = d.to_markdown();
    assert!(md.contains("## Today, so far"), "{md}");
    assert!(
        md.contains(&format!("\n- \"Design sync\" (note {note})\n")),
        "the review can read the note to find what the meeting left them to do: {md}"
    );
    assert!(md.contains("read one with get_note"), "{md}");

    // The morning has no review, so reads none.
    let morning = brief::digest(
        &vault,
        &BriefSpec::new(BriefKind::Morning),
        &zoned(2026, 11, 21, 7, 0),
        Extras::default(),
    )
    .unwrap();
    assert!(morning.meeting_notes.is_empty());
}

#[test]
fn the_digest_says_what_the_last_brief_was_and_nothing_about_what_to_do_with_it() {
    let (_dir, vault) = vault_for_test();
    let routine = vault.set_brief(BriefKind::Morning, true).unwrap();
    let mut run = RoutineRun::new(&routine, None);
    run.started_at = "2026-11-20T07:00:00Z".parse().unwrap();
    run.outcome = Outcome::Done;
    run.brief = Some(Box::new(brief::Brief {
        kind: BriefKind::Morning,
        day: jiff::civil::date(2026, 11, 20),
        headline: "A heavy Friday".into(),
        parts: Vec::new(),
        spoken: String::new(),
        script: None,
        audio: None,
        digest: brief::BriefDigest::default(),
    }));
    vault.save_run(&run).unwrap();

    let spec = BriefSpec::new(BriefKind::Morning);
    let d = brief::digest(&vault, &spec, &zoned(2026, 11, 21, 7, 0), Extras::default()).unwrap();
    let md = d.to_markdown();
    assert!(md.contains("The last brief: \"A heavy Friday\" (2026-11-20)"), "{md}");
    assert!(!md.to_lowercase().contains("do not repeat"), "an instruction inside the fence: {md}");
}

// ---- the daily podcast ------------------------------------------------------

fn follow(vault: &Vault, title: &str) -> NewsSource {
    let source = NewsSource::new(
        SourceKind::Web { url: format!("https://{}.example/feed", title.to_lowercase()) },
        title,
    );
    vault.save_news_source(&source).unwrap();
    source
}

/// A story from `source`, published at `at`, with the feed's short `summary`
/// and, when there is one, the `body` it carried.
fn arrive(
    vault: &Vault,
    source: &NewsSource,
    title: &str,
    at: Timestamp,
    summary: &str,
    body: Option<(&str, BodyOrigin)>,
) -> Story {
    let key = vault.news_key(&format!("{}/{title}", source.id)).unwrap();
    let mut story = Story::new(source.id, key, title, at);
    // It arrived the moment it was published, unless a test says otherwise.
    story.fetched_at = at;
    story.url = format!("https://example.com/{}", title.replace(' ', "-"));
    story.summary = summary.into();
    let stored = body.map(|(text, origin)| StoryBody {
        byline: None,
        site_name: None,
        story_id: story.id,
        html: format!("<p>{text}</p>"),
        text: text.into(),
        origin,
        images: Vec::new(),
    });
    vault.insert_stories(&[(story.clone(), stored)]).unwrap();
    story
}

fn episode_of(minutes: u64) -> Enclosure {
    Enclosure {
        url: "https://shows.example/ep.mp3".into(),
        mime: "audio/mpeg".into(),
        length: None,
        duration_ms: Some(minutes * 60_000),
        transcript_url: String::new(),
        transcript_mime: String::new(),
        episode: None,
        season: None,
    }
}

#[test]
fn switching_the_daily_podcast_on_makes_a_brief_with_no_sections() {
    let (_dir, vault) = vault_for_test();
    let routine = vault.set_brief(BriefKind::Podcast, true).unwrap();
    assert_eq!(routine.name, "Daily podcast");
    assert!(routine.enabled);
    let spec = routine.kind.brief().unwrap().clone();
    assert!(spec.sections.is_empty());
    assert_eq!(spec.podcast, Some(PodcastBrief::default()));
    // Its own time, every day: not the morning brief's.
    match &routine.trigger {
        Trigger::Schedule { at, days, day_of_month } => {
            assert_eq!(*at, time(6, 0, 0, 0));
            assert!(days.is_empty() && day_of_month.is_none());
        }
        other => panic!("a clock trigger, not {other:?}"),
    }
    let morning = vault.set_brief(BriefKind::Morning, true).unwrap();
    assert_ne!(morning.id, routine.id, "the podcast takes no other brief's place");
    assert_eq!(vault.routines().unwrap().len(), 2);

    // What it is made from and how long it runs are the person's to change,
    // with the schedule.
    let blog = follow(&vault, "Blog");
    let mut edited = routine.clone();
    let mut podcast = spec.podcast.clone().unwrap();
    podcast.minutes = 30;
    podcast.sources = vec![blog.id];
    edited.kind = RoutineKind::Brief(BriefSpec { podcast: Some(podcast.clone()), ..spec.clone() });
    vault.save_routine(&edited).unwrap();
    let saved = vault.routine(routine.id).unwrap();
    assert_eq!(saved.kind.brief().unwrap().podcast, Some(podcast.clone()));

    // But within what a podcast may be.
    let mut too_short = saved.clone();
    let mut short = podcast.clone();
    short.minutes = 3;
    too_short.kind = RoutineKind::Brief(BriefSpec { podcast: Some(short), ..spec.clone() });
    assert!(matches!(vault.save_routine(&too_short), Err(Error::Invalid(_))));
    let mut sectioned = saved.clone();
    sectioned.kind = RoutineKind::Brief(BriefSpec {
        sections: vec![BriefSection::Schedule],
        podcast: Some(podcast),
        ..spec
    });
    assert!(matches!(vault.save_routine(&sectioned), Err(Error::Invalid(_))));
    // And it is still the application's: not made by hand, not another brief's.
    let mut by_hand = Routine::new("My podcast", "", Trigger::Manual);
    by_hand.kind = RoutineKind::Brief(BriefSpec::new(BriefKind::Podcast));
    assert!(matches!(vault.save_routine(&by_hand), Err(Error::Invalid(_))));
    assert!(matches!(vault.delete_routine(routine.id), Err(Error::Invalid(_))));
}

#[test]
fn the_podcast_digest_is_what_arrived_in_the_feed_newest_first() {
    let (_dir, vault) = vault_for_test();
    let now = zoned(2026, 11, 22, 6, 0);
    let ago = |hours: i64| now.timestamp() - SignedDuration::from_hours(hours);
    let blog = follow(&vault, "Blog");
    let show = follow(&vault, "The Show");

    let chips = arrive(
        &vault,
        &blog,
        "Chips are late",
        ago(2),
        "A summary of the chips story.",
        Some((
            "The fab says its new line will start a year late, after the tools arrived.",
            BodyOrigin::Extracted,
        )),
    );
    let mut episode =
        Story::new(show.id, vault.news_key("show/episode").unwrap(), "The margins episode", ago(4));
    episode.fetched_at = ago(4);
    episode.media = Some(episode_of(50));
    episode.summary = "Show notes.".into();
    let article = StoryBody {
        byline: None,
        site_name: None,
        story_id: episode.id,
        html: "<p>x</p>".into(),
        text: "The guest explains why margins matter more than growth.".into(),
        origin: BodyOrigin::Transcript,
        images: Vec::new(),
    };
    vault.insert_stories(&[(episode.clone(), Some(article))]).unwrap();
    let bare = arrive(&vault, &blog, "Just a title", ago(1), "", None);
    let _old = arrive(&vault, &blog, "Old news", ago(72), "From three days ago.", None);
    let read = arrive(&vault, &blog, "Already read", ago(3), "I read this one.", None);
    let mut done = Consumption::new(ContentRef::Story(read.id), "Already read", ConsumeMode::Read);
    done.finished = true;
    vault.save_consumption(&done).unwrap();

    let spec = BriefSpec::new(BriefKind::Podcast);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();
    assert_eq!(d.kind, BriefKind::Podcast);
    assert!(d.sections.is_empty());
    assert_eq!(d.day, Some(jiff::civil::date(2026, 11, 22)));
    let podcast = d.podcast.clone().expect("the podcast's digest");
    assert_eq!(podcast.minutes, 15);
    assert_eq!(podcast.words, 2_250);
    assert_eq!(podcast.since, Some(ago(24)), "a day back when there was no podcast before");

    // The ones with something to say first, newest first; the one known by
    // its title alone last; nothing old, nothing already read.
    let titles: Vec<_> = podcast.stories.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["Chips are late", "The margins episode", "Just a title"]);
    let from: Vec<_> = podcast.stories.iter().map(|s| s.from).collect();
    assert_eq!(from, vec![ExcerptFrom::Article, ExcerptFrom::Transcript, ExcerptFrom::Title]);
    assert!(podcast.stories[0].excerpt.starts_with("The fab says its new line"));
    assert_eq!(
        podcast.stories[1].excerpt,
        "The guest explains why margins matter more than growth."
    );
    assert!(podcast.stories[1].episode && !podcast.stories[0].episode);
    assert_eq!(podcast.stories[2].story_id, bare.id);
    assert_eq!(podcast.stories[0].story_id, chips.id);
    assert_eq!(podcast.stories[0].source, "Blog");
    assert_eq!(podcast.sources, vec!["Blog", "The Show"]);
    assert_eq!(podcast.heard_already, 1);
    assert_eq!(podcast.left_out, 0);

    let md = d.to_markdown();
    assert!(md.contains("## Stories to cover (3), newest first"), "{md}");
    assert!(md.contains("margins matter more than growth"), "{md}");
    assert!(md.contains("Known by title alone"), "{md}");

    // Only the sources it was set to.
    let mut only_the_show = BriefSpec::new(BriefKind::Podcast);
    only_the_show.podcast.as_mut().unwrap().sources = vec![show.id];
    let d = brief::digest(&vault, &only_the_show, &now, Extras::default()).unwrap();
    let titles: Vec<_> = d.podcast.unwrap().stories.iter().map(|s| s.title.clone()).collect();
    assert_eq!(titles, vec!["The margins episode"]);

    // A source that is no longer followed is said, and the rest still made.
    let mut gone = BriefSpec::new(BriefKind::Podcast);
    gone.podcast.as_mut().unwrap().sources = vec![blog.id, jatayu_core::NewsSourceId::new()];
    let d = brief::digest(&vault, &gone, &now, Extras::default()).unwrap();
    assert!(d.gaps.iter().any(|g| g.contains("no longer followed")), "{:?}", d.gaps);
    assert_eq!(d.podcast.unwrap().stories.len(), 2, "the blog's two, the one read left out");
}

#[test]
fn a_story_is_new_to_a_podcast_from_when_it_arrived_not_when_it_was_published() {
    let (_dir, vault) = vault_for_test();
    let now = zoned(2026, 11, 22, 6, 0);
    let ago = |hours: i64| now.timestamp() - SignedDuration::from_hours(hours);
    let blog = follow(&vault, "Blog");
    // Stored as they came, not as `arrive` stores a story: the moment it was
    // published.
    let mut stored = Vec::new();
    for (title, published, fetched) in [
        // Published days ago, reached the Feed this morning: a newsletter
        // filed late, a show followed an hour ago.
        ("Filed late", ago(80), ago(3)),
        // A back catalogue: more than a week older than the day it arrived.
        ("A year ago", ago(24 * 365), ago(3)),
        // Published an hour ago, but it arrived before the last day began.
        ("Arrived long ago", ago(1), ago(40)),
    ] {
        let mut story = Story::new(blog.id, vault.news_key(title).unwrap(), title, published);
        story.summary = "Something said.".into();
        story.fetched_at = fetched;
        stored.push((story, None));
    }
    vault.insert_stories(&stored).unwrap();

    let spec = BriefSpec::new(BriefKind::Podcast);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();
    let titles: Vec<_> =
        d.podcast.as_ref().unwrap().stories.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["Filed late"]);
    assert!(d.to_markdown().contains("arrived since"));
}

#[test]
fn a_podcast_is_held_to_what_fits_its_length() {
    let (_dir, vault) = vault_for_test();
    let now = zoned(2026, 11, 22, 6, 0);
    let ago = |minutes: i64| now.timestamp() - SignedDuration::from_mins(minutes);
    let busy = follow(&vault, "Busy");
    let quiet = follow(&vault, "Quiet");
    for n in 0..12 {
        arrive(&vault, &busy, &format!("Busy {n}"), ago(10 + n), "About it.", None);
    }
    arrive(&vault, &quiet, "Quiet one", ago(600), "Said once.", None);

    let mut spec = BriefSpec::new(BriefKind::Podcast);
    spec.podcast.as_mut().unwrap().minutes = 5;
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();
    let podcast = d.podcast.clone().unwrap();
    // About a story a minute, and no one source is more than its share of
    // them -- so the quiet source's story is in, though it is the oldest.
    assert_eq!(podcast.stories.len(), 4, "{:?}", podcast.stories);
    assert!(podcast.stories.iter().any(|s| s.title == "Quiet one"));
    assert_eq!(podcast.stories.iter().filter(|s| s.source == "Busy").count(), 3);
    assert_eq!(podcast.left_out, 9);
    assert!(d.to_markdown().contains("9 more arrived than fit in 5 minutes"));
}

#[test]
fn the_next_podcast_leaves_out_what_the_last_one_with_audio_told() {
    let (_dir, vault) = vault_for_test();
    let now = zoned(2026, 11, 22, 6, 0);
    let ago = |hours: i64| now.timestamp() - SignedDuration::from_hours(hours);
    let blog = follow(&vault, "Blog");
    let told = arrive(&vault, &blog, "Told yesterday", ago(30), "Already in the last one.", None);
    let late =
        arrive(&vault, &blog, "Late arrival", ago(26), "Published just before the last one.", None);
    let fresh = arrive(&vault, &blog, "Fresh", ago(2), "New this morning.", None);
    let _stale = arrive(&vault, &blog, "Stale", ago(60), "Long before.", None);

    let routine = vault.set_brief(BriefKind::Podcast, true).unwrap();
    let kept = |started: &str, made: Timestamp, audio: Option<FileId>, stories: &[&Story]| {
        let mut run = RoutineRun::new(&routine, None);
        run.started_at = started.parse().unwrap();
        run.outcome = Outcome::Done;
        run.brief = Some(Box::new(brief::Brief {
            kind: BriefKind::Podcast,
            day: jiff::civil::date(2026, 11, 21),
            headline: "Yesterday".into(),
            parts: Vec::new(),
            spoken: String::new(),
            script: None,
            audio,
            digest: brief::BriefDigest {
                kind: BriefKind::Podcast,
                made_at: Some(made),
                podcast: Some(PodcastDigest {
                    stories: stories
                        .iter()
                        .map(|s| PodcastStory {
                            story_id: s.id,
                            title: s.title.clone(),
                            source: "Blog".into(),
                            url: String::new(),
                            published_at: s.published_at,
                            episode: false,
                            excerpt: s.summary.clone(),
                            from: ExcerptFrom::Summary,
                        })
                        .collect(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }));
        vault.save_run(&run).unwrap();
    };
    // Yesterday's, with its audio, made at six in the morning; and a later
    // one that never got its audio, which told nobody anything.
    kept("2026-11-21T06:00:00Z", ago(24), Some(FileId::new()), &[&told]);
    kept("2026-11-21T20:00:00Z", ago(10), None, &[&fresh]);

    let spec = BriefSpec::new(BriefKind::Podcast);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();
    let podcast = d.podcast.unwrap();
    // From six hours before yesterday's: the late arrival is in, what that
    // one told is not, and the older stale one is out of reach.
    assert_eq!(podcast.since, Some(ago(30)));
    let titles: Vec<_> = podcast.stories.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["Fresh", "Late arrival"]);
    assert!(!titles.contains(&"Told yesterday") && !titles.contains(&"Stale"));
    assert!(late.published_at < ago(24));
    assert!(d.previous.is_some_and(|p| p.contains("Yesterday")), "told not to repeat it");
}

#[test]
fn newsletters_are_read_only_from_mail_the_assistant_may_read() {
    let (_dir, vault) = vault_for_test();
    let now = zoned(2026, 11, 22, 6, 0);
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    vault.save_account(&account).unwrap();
    let mailbox = Mailbox::new(account.id, MailboxRole::Inbox.as_str(), MailboxRole::Inbox);
    vault.save_mailbox(&mailbox).unwrap();
    let message_id = MailMessageId::new();
    let sent = now.timestamp() - SignedDuration::from_hours(3);
    let message = Message {
        id: message_id,
        account_id: account.id,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{message_id}@example.com>"),
        date: sent,
        from: Address::new("Morning Brew", "news@brew.example"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Markets".into(),
        snippet: "Stocks fell".into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.id.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault
        .ingest_mail(account.id, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
        .unwrap();
    vault
        .save_body(&Body {
            message_id,
            html_sanitised: String::new(),
            text: "Stocks fell three percent on Tuesday, the worst day since March.".into(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        })
        .unwrap();
    let source = NewsSource::new(
        SourceKind::Mail { sender: "news@brew.example".into(), name: String::new() },
        "Morning Brew",
    );
    vault.save_news_source(&source).unwrap();
    let mut issue = Story::new(source.id, vault.news_key("brew/markets").unwrap(), "Markets", sent);
    issue.fetched_at = sent;
    issue.summary = "Stocks fell".into();
    issue.mail_message = Some(message_id);
    vault.insert_stories(&[(issue.clone(), None)]).unwrap();

    // Mail the assistant may not read never reaches a podcast.
    let spec = BriefSpec::new(BriefKind::Podcast);
    let d = brief::digest(&vault, &spec, &now, Extras::default()).unwrap();
    assert!(d.podcast.as_ref().unwrap().stories.is_empty());
    assert!(
        d.gaps.iter().any(|g| g.starts_with("newsletters:") && g.contains("no mail account")),
        "{:?}",
        d.gaps
    );

    // Shared, it is told from the issue's own words, not its snippet.
    let extras = Extras { mail_accounts: vec![account.id], ..Default::default() };
    let d = brief::digest(&vault, &spec, &now, extras).unwrap();
    let podcast = d.podcast.unwrap();
    assert_eq!(podcast.stories.len(), 1);
    assert_eq!(podcast.stories[0].from, ExcerptFrom::Newsletter);
    assert!(podcast.stories[0].excerpt.contains("worst day since March"));
    assert!(d.gaps.is_empty(), "{:?}", d.gaps);
}

#[test]
fn asked_for_my_brief_the_podcast_is_not_the_newest() {
    let (_dir, vault) = vault_for_test();
    let morning = vault.set_brief(BriefKind::Morning, true).unwrap();
    let podcast = vault.set_brief(BriefKind::Podcast, true).unwrap();
    for (routine, kind, started, headline) in [
        (&morning, BriefKind::Morning, "2026-11-22T07:00:00Z", "The morning"),
        (&podcast, BriefKind::Podcast, "2026-11-22T08:00:00Z", "The podcast"),
    ] {
        let mut run = RoutineRun::new(routine, None);
        run.started_at = started.parse().unwrap();
        run.outcome = Outcome::Done;
        run.brief = Some(Box::new(brief::Brief {
            kind,
            day: jiff::civil::date(2026, 11, 22),
            headline: headline.into(),
            parts: Vec::new(),
            spoken: String::new(),
            script: None,
            audio: None,
            digest: brief::BriefDigest::default(),
        }));
        vault.save_run(&run).unwrap();
    }
    let both = vault.briefs(None, 10).unwrap();
    assert_eq!(both.len(), 2, "briefs lists the podcast too");
    assert_eq!(both[0].brief.as_ref().unwrap().kind, BriefKind::Podcast);
    let mine = brief::newest(&vault, None).unwrap().unwrap();
    assert_eq!(mine.brief.unwrap().headline, "The morning");
    let show = brief::newest(&vault, Some(BriefKind::Podcast)).unwrap().unwrap();
    assert_eq!(show.brief.unwrap().headline, "The podcast");
}
