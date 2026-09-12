//! The routine half of the conformance suite: the assistant's standing work.

use super::*;

/// Everything a backend must do with the assistant's standing work.
pub fn run_routine_suite(store: &dyn JournalStore) {
    eprintln!("--- routine conformance suite ---");

    routines_start_empty(store);
    routine_round_trips_every_field(store);
    routine_put_is_idempotent(store);
    missing_routine_records_are_not_found(store);
    runs_are_listed_newest_first_and_filtered(store);
    unseen_runs_are_counted_and_cleared(store);
    deleting_a_routine_takes_its_runs(store);
    a_runs_transcript_is_hidden_from_the_chat_list(store);
    unicode_survives_a_routine_round_trip(store);

    routine_cleanup(store);
    eprintln!("--- routine suite passed ---");
}

fn routine_store(store: &dyn JournalStore) -> &dyn crate::store::routines::RoutineStore {
    store.routines().expect("the routine suite needs a routine store")
}

fn routine_cleanup(store: &dyn JournalStore) {
    let r = routine_store(store);
    for routine in r.list_routines().expect("list_routines") {
        r.delete_routine(routine.id).expect("delete_routine");
    }
    for run in r.list_runs(&RunQuery::default()).expect("list_runs") {
        r.delete_run(run.id).expect("delete_run");
    }
    assert!(r.list_routines().unwrap().is_empty(), "cleanup left routines behind");
    assert!(r.list_runs(&RunQuery::default()).unwrap().is_empty(), "cleanup left runs behind");
}

fn seeded_routine(store: &dyn JournalStore, name: &str) -> Routine {
    let routine = Routine::new(name, "Say what is due today.", Trigger::Manual);
    routine_store(store).put_routine(&routine).expect("put_routine");
    routine
}

fn routines_start_empty(store: &dyn JournalStore) {
    let r = routine_store(store);
    assert!(r.list_routines().unwrap().is_empty(), "a fresh store has no routines");
    assert!(r.list_runs(&RunQuery::default()).unwrap().is_empty(), "and no runs");
    assert_eq!(r.count_unseen_runs().unwrap(), 0);
}

fn routine_round_trips_every_field(store: &dyn JournalStore) {
    let r = routine_store(store);
    let mut routine = Routine::new(
        "Morning brief",
        "Look at what is due and leave me a note.",
        Trigger::Schedule { at: time(7, 0, 0, 0), days: everyday_weekdays() },
    );
    routine.grace_minutes = 90;
    routine.last_run_at = Some(Timestamp::now());
    r.put_routine(&routine).expect("put_routine");

    assert_eq!(r.get_routine(routine.id).unwrap(), routine, "every field must survive");
    assert_eq!(r.list_routines().unwrap().len(), 1);

    let mut run = RoutineRun::new(&routine, Some(Timestamp::now()));
    run.subject = Some("the 3pm with Priya".into());
    run.summary = "Three things are due and one is overdue.".into();
    run.steps = 4;
    run.finish(Outcome::Done, run.summary.clone());
    r.put_run(&run).expect("put_run");
    assert_eq!(r.get_run(run.id).unwrap(), run, "and every field of a run");

    routine_cleanup(store);
}

fn routine_put_is_idempotent(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Twice");
    r.put_routine(&routine).expect("second put");
    assert_eq!(r.list_routines().unwrap().len(), 1, "saving twice leaves one");

    r.delete_routine(routine.id).expect("delete_routine");
    r.delete_routine(routine.id).expect("deleting a missing routine is a no-op");
    routine_cleanup(store);
}

fn missing_routine_records_are_not_found(store: &dyn JournalStore) {
    let r = routine_store(store);
    super::assert_not_found(r.get_routine(RoutineId::new()));
    super::assert_not_found(r.get_run(RoutineRunId::new()));
    r.delete_run(RoutineRunId::new()).expect("deleting a missing run is a no-op");
}

fn runs_are_listed_newest_first_and_filtered(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Brief");
    let other = seeded_routine(store, "Review");

    let mut first = RoutineRun::new(&routine, None);
    first.started_at = Timestamp::from_second(1_700_000_000).unwrap();
    first.finish(Outcome::Done, "the first");
    let mut second = RoutineRun::new(&routine, None);
    second.started_at = Timestamp::from_second(1_700_001_000).unwrap();
    second.fail("the endpoint refused");
    let mut elsewhere = RoutineRun::new(&other, None);
    elsewhere.started_at = Timestamp::from_second(1_700_002_000).unwrap();
    elsewhere.finish(Outcome::Done, "somebody else's");
    for run in [&first, &second, &elsewhere] {
        r.put_run(run).expect("put_run");
    }

    let all = r.list_runs(&RunQuery::default()).expect("list_runs");
    assert_eq!(
        all.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![elsewhere.id, second.id, first.id],
        "newest first"
    );

    let mine = r.list_runs(&RunQuery::for_routine(routine.id)).expect("one routine's log");
    assert_eq!(mine.len(), 2, "and only that routine's");

    let failed = r
        .list_runs(&RunQuery { outcomes: vec![Outcome::Failed], ..Default::default() })
        .expect("by outcome");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].reason, "the endpoint refused");

    let capped =
        r.list_runs(&RunQuery { limit: Some(1), ..Default::default() }).expect("with a limit");
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, elsewhere.id, "a limit keeps the newest, not any one");

    let recent = r
        .list_runs(&RunQuery {
            since: Some(Timestamp::from_second(1_700_001_500).unwrap()),
            ..Default::default()
        })
        .expect("since");
    assert_eq!(recent.len(), 1);

    routine_cleanup(store);
}

fn unseen_runs_are_counted_and_cleared(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Brief");
    let mut one = RoutineRun::new(&routine, None);
    one.finish(Outcome::Done, "first");
    let mut two = RoutineRun::new(&routine, None);
    two.finish(Outcome::Done, "second");
    r.put_run(&one).expect("put_run");
    r.put_run(&two).expect("put_run");
    assert_eq!(r.count_unseen_runs().unwrap(), 2);

    r.mark_runs_seen(&[one.id]).expect("mark one");
    assert_eq!(r.count_unseen_runs().unwrap(), 1);
    // Both copies of the flag have to move: it is a clear column *and* part of
    // the sealed payload, and a client reading the record back would otherwise
    // put the number straight back on the app bar.
    assert!(r.get_run(one.id).unwrap().seen, "the payload must agree with the column");
    assert_eq!(r.list_runs(&RunQuery::unseen(10)).unwrap().len(), 1);

    r.mark_runs_seen(&[]).expect("mark all");
    assert_eq!(r.count_unseen_runs().unwrap(), 0);
    assert!(r.get_run(two.id).unwrap().seen);

    // A skipped run is born seen: there is nothing to look at, so it must not
    // put a number on the app bar.
    let skipped = RoutineRun::skipped(&routine, None, "the vault was locked");
    r.put_run(&skipped).expect("put_run");
    assert_eq!(r.count_unseen_runs().unwrap(), 0, "a skipped run asks for no attention");

    routine_cleanup(store);
}

fn deleting_a_routine_takes_its_runs(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine = seeded_routine(store, "Brief");
    let other = seeded_routine(store, "Review");
    let mut mine = RoutineRun::new(&routine, None);
    mine.finish(Outcome::Done, "mine");
    let mut theirs = RoutineRun::new(&other, None);
    theirs.finish(Outcome::Done, "theirs");
    r.put_run(&mine).expect("put_run");
    r.put_run(&theirs).expect("put_run");

    r.delete_routine(routine.id).expect("delete_routine");
    // Its runs go with it.
    super::assert_not_found(r.get_run(mine.id));
    assert!(r.get_run(theirs.id).is_ok(), "and nobody else's do");

    routine_cleanup(store);
}

fn a_runs_transcript_is_hidden_from_the_chat_list(store: &dyn JournalStore) {
    let Some(agent) = store.agent() else {
        eprintln!("  (no agent store; skipping the run's transcript)");
        return;
    };
    let chat = Conversation::new();
    let transcript = Conversation::for_run(RoutineRunId::new(), "Morning brief");
    agent.put_conversation(&chat).expect("put_conversation");
    agent.put_conversation(&transcript).expect("put_conversation");

    let everything = agent.list_conversations(&ConversationQuery::default()).expect("all threads");
    assert_eq!(everything.len(), 2, "both are threads");

    let chats = agent.list_conversations(&ConversationQuery::chats(10)).expect("chats only");
    assert_eq!(chats.len(), 1, "a week of morning briefs is not a list of conversations");
    assert_eq!(chats[0].id, chat.id);

    // And the limit is honoured *after* the filter, not before: a `LIMIT 1`
    // pushed into SQL would have fetched the transcript and answered with
    // nothing at all.
    let one = agent.list_conversations(&ConversationQuery::chats(1)).expect("one chat");
    assert_eq!(one.len(), 1, "asking for one chat must give one chat");
    assert_eq!(one[0].id, chat.id);

    agent.delete_conversation(chat.id).expect("delete_conversation");
    agent.delete_conversation(transcript.id).expect("delete_conversation");
}

fn unicode_survives_a_routine_round_trip(store: &dyn JournalStore) {
    let r = routine_store(store);
    let routine =
        Routine::new("காலை அறிக்கை \u{2600}", "இன்று என்ன செய்ய வேண்டும் என்று சொல்", Trigger::Manual);
    r.put_routine(&routine).expect("put_routine");
    let back = r.get_routine(routine.id).expect("get_routine");
    assert_eq!(back.name, "காலை அறிக்கை \u{2600}");
    assert_eq!(back.instructions, routine.instructions);
    r.delete_routine(routine.id).expect("delete_routine");
}

/// Monday to Friday, spelled out so the suite does not depend on a constant
/// that could quietly change meaning.
fn everyday_weekdays() -> Vec<crate::routine::Weekday> {
    use crate::routine::Weekday as W;
    vec![W::Mon, W::Tue, W::Wed, W::Thu, W::Fri]
}
