//! The job half of the conformance suite: the assistant's background work.

use super::*;
use crate::id::JobId;
use crate::job::{Job, JobOutput, JobStatus};
use crate::store::jobs::JobQuery;

/// Everything a backend must do with the assistant's background work.
pub fn run_job_suite(store: &dyn JournalStore) {
    eprintln!("--- job conformance suite ---");

    jobs_start_empty(store);
    job_round_trips_every_field(store);
    job_put_is_idempotent(store);
    missing_job_records_are_not_found(store);
    jobs_are_listed_newest_first_and_filtered(store);
    unseen_jobs_are_counted_and_cleared(store);
    marking_a_job_seen_changes_nothing_else_about_it(store);
    a_jobs_transcript_is_hidden_from_the_chat_list(store);
    unicode_survives_a_job_round_trip(store);

    job_cleanup(store);
    eprintln!("--- job suite passed ---");
}

fn job_store(store: &dyn JournalStore) -> &dyn crate::store::jobs::JobStore {
    store.jobs().expect("the job suite needs a job store")
}

fn job_cleanup(store: &dyn JournalStore) {
    let j = job_store(store);
    for job in j.list_jobs(&JobQuery::default()).expect("list_jobs") {
        j.delete_job(job.id).expect("delete_job");
    }
    assert!(j.list_jobs(&JobQuery::default()).unwrap().is_empty(), "cleanup left jobs behind");
    assert_eq!(j.count_unseen_jobs().unwrap(), 0, "cleanup left an unseen count behind");
}

fn seeded_job(store: &dyn JournalStore, question: &str) -> Job {
    let job = Job::research(question, "Who is asking and why.", None);
    job_store(store).put_job(&job).expect("put_job");
    job
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

/// A job with every field set to something other than its default, so that a
/// column a backend forgot to carry shows up as a difference.
fn a_job_that_has_done_everything() -> Job {
    let mut job = Job::research(
        "Which e-bike for hills?",
        "Commutes 12 km each way and has ruled out hub motors.",
        Some(ConversationId::new()),
    );
    job.begin();
    job.progress = "Read 14 sources; writing up".into();
    job.steps = 31;
    job.add_note(NoteId::new());
    job.conversation_id = Some(ConversationId::new());
    job.finish("Three bikes are worth a look.");
    job.answered = true;
    job.attempts = 2;
    job
}

fn jobs_start_empty(store: &dyn JournalStore) {
    let j = job_store(store);
    assert!(j.list_jobs(&JobQuery::default()).unwrap().is_empty(), "a fresh store has no jobs");
    assert_eq!(j.count_unseen_jobs().unwrap(), 0);
}

fn job_round_trips_every_field(store: &dyn JournalStore) {
    let j = job_store(store);
    let mut job = a_job_that_has_done_everything();
    job.seen = true;
    j.put_job(&job).expect("put_job");

    assert_eq!(j.get_job(job.id).unwrap(), job, "every field must survive");
    assert_eq!(j.list_jobs(&JobQuery::default()).unwrap(), vec![job.clone()]);

    // And the other ways a job ends, which carry a reason instead of an
    // answer, and one that was never started.
    let mut failed = Job::research("Why is the sky that colour?", "", None);
    failed.begin();
    failed.fail("the endpoint refused");
    let mut cancelled = Job::research("Which tent?", "", None);
    cancelled.cancel("they changed their mind").expect("cancel a queued job");
    let queued = Job::research("Which stove?", "", Some(ConversationId::new()));
    for other in [&failed, &cancelled, &queued] {
        j.put_job(other).expect("put_job");
        assert_eq!(&j.get_job(other.id).unwrap(), other, "{:?}", other.status);
    }

    job_cleanup(store);
}

fn job_put_is_idempotent(store: &dyn JournalStore) {
    let j = job_store(store);
    let mut job = seeded_job(store, "Twice");
    j.put_job(&job).expect("second put");
    assert_eq!(j.list_jobs(&JobQuery::default()).unwrap().len(), 1, "saving twice leaves one");

    // A save of a changed job is the same job changed, not a second one.
    job.begin();
    job.progress = "Reading example.com".into();
    j.put_job(&job).expect("put a changed job");
    assert_eq!(j.list_jobs(&JobQuery::default()).unwrap().len(), 1);
    let back = j.get_job(job.id).unwrap();
    assert_eq!(back.status, JobStatus::Running);
    assert_eq!(back.progress, "Reading example.com");

    j.delete_job(job.id).expect("delete_job");
    super::assert_not_found(j.get_job(job.id));
    j.delete_job(job.id).expect("deleting a missing job is a no-op");
    job_cleanup(store);
}

fn missing_job_records_are_not_found(store: &dyn JournalStore) {
    let j = job_store(store);
    super::assert_not_found(j.get_job(JobId::new()));
    j.delete_job(JobId::new()).expect("deleting a missing job is a no-op");
}

fn jobs_are_listed_newest_first_and_filtered(store: &dyn JournalStore) {
    let j = job_store(store);
    let conversation = ConversationId::new();

    let mut first = Job::research("The first", "", Some(conversation));
    first.created_at = at(1_700_000_000);
    first.finish("the first");
    let mut second = Job::research("The second", "", Some(conversation));
    second.created_at = at(1_700_001_000);
    second.fail("the endpoint refused");
    let mut elsewhere = Job::research("Somebody else's", "", None);
    elsewhere.created_at = at(1_700_002_000);
    for job in [&first, &second, &elsewhere] {
        j.put_job(job).expect("put_job");
    }
    let ids = |query: &JobQuery| -> Vec<JobId> {
        j.list_jobs(query).expect("list_jobs").iter().map(|x| x.id).collect()
    };

    assert_eq!(ids(&JobQuery::default()), vec![elsewhere.id, second.id, first.id], "newest first");

    // The conversation and the status are both inside the sealed payload, so
    // these two exercise the filtering a backend cannot push into its own
    // query.
    assert_eq!(ids(&JobQuery::asked_in(conversation)), vec![second.id, first.id]);
    let failed = j
        .list_jobs(&JobQuery { statuses: vec![JobStatus::Failed], ..Default::default() })
        .expect("by status");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].reason, "the endpoint refused");
    let either =
        JobQuery { statuses: vec![JobStatus::Done, JobStatus::Failed], ..Default::default() };
    assert_eq!(ids(&either), vec![second.id, first.id], "any of the statuses, not all of them");
    assert_eq!(ids(&JobQuery::unfinished()), vec![elsewhere.id], "what the worker asks for");

    assert_eq!(ids(&JobQuery { limit: Some(1), ..Default::default() }), vec![elsewhere.id]);
    // The limit is applied after the filter, not before: capping first and
    // filtering after would have fetched the one job that is not wanted.
    let capped = JobQuery { statuses: vec![JobStatus::Done], limit: Some(1), ..Default::default() };
    assert_eq!(ids(&capped), vec![first.id], "a limit keeps the newest that matches");

    let since = |when: i64| JobQuery { since: Some(at(when)), ..Default::default() };
    assert_eq!(ids(&since(1_700_001_500)), vec![elsewhere.id]);
    assert_eq!(ids(&since(1_700_001_000)), vec![elsewhere.id, second.id], "at or after");

    job_cleanup(store);
}

fn unseen_jobs_are_counted_and_cleared(store: &dyn JournalStore) {
    let j = job_store(store);
    let one = seeded_job(store, "One");
    let two = seeded_job(store, "Two");
    let three = seeded_job(store, "Three");
    assert_eq!(j.count_unseen_jobs().unwrap(), 3, "a job nobody has looked at is counted");

    j.mark_jobs_seen(&[one.id]).expect("mark one");
    assert_eq!(j.count_unseen_jobs().unwrap(), 2);
    // Both copies of the flag have to move: it is a clear column *and* part of
    // the sealed payload, and a client reading the record back would otherwise
    // put the number straight back on the app bar.
    assert!(j.get_job(one.id).unwrap().seen, "the payload must agree with the column");
    assert!(!j.get_job(two.id).unwrap().seen, "and the others are left alone");
    let unseen = j.list_jobs(&JobQuery::unseen(10)).unwrap();
    assert_eq!(unseen.len(), 2);
    assert!(unseen.iter().all(|job| job.id != one.id));
    let seen = j.list_jobs(&JobQuery { unseen: Some(false), ..Default::default() }).unwrap();
    assert_eq!(seen.iter().map(|job| job.id).collect::<Vec<_>>(), vec![one.id]);

    j.mark_jobs_seen(&[one.id]).expect("marking a seen job again is harmless");
    assert_eq!(j.count_unseen_jobs().unwrap(), 2);

    j.mark_jobs_seen(&[two.id, three.id]).expect("mark several");
    assert_eq!(j.count_unseen_jobs().unwrap(), 0);

    // An empty list means all of them: the "mark everything as read" button.
    let four = seeded_job(store, "Four");
    let five = seeded_job(store, "Five");
    assert_eq!(j.count_unseen_jobs().unwrap(), 2);
    j.mark_jobs_seen(&[]).expect("mark all");
    assert_eq!(j.count_unseen_jobs().unwrap(), 0);
    assert!(j.get_job(four.id).unwrap().seen && j.get_job(five.id).unwrap().seen);
    assert!(j.list_jobs(&JobQuery::unseen(10)).unwrap().is_empty());

    job_cleanup(store);
}

fn marking_a_job_seen_changes_nothing_else_about_it(store: &dyn JournalStore) {
    let j = job_store(store);
    let job = a_job_that_has_done_everything();
    j.put_job(&job).expect("put_job");

    // Read-modify-write: a backend that rebuilt the record from its columns
    // would lose whatever is only in the sealed payload.
    j.mark_jobs_seen(&[job.id]).expect("mark one");
    let marked = j.get_job(job.id).unwrap();
    assert!(marked.seen);
    assert_eq!(marked, Job { seen: true, ..job.clone() }, "only the flag moved");

    j.mark_jobs_seen(&[]).expect("mark all");
    assert_eq!(j.get_job(job.id).unwrap(), marked, "and marking all moves nothing more");

    job_cleanup(store);
}

fn a_jobs_transcript_is_hidden_from_the_chat_list(store: &dyn JournalStore) {
    let Some(agent) = store.agent() else {
        eprintln!("  (no agent store; skipping the job's transcript)");
        return;
    };
    let chat = Conversation::new();
    let transcript = Conversation::for_job(JobId::new(), "Which e-bike for hills?");
    agent.put_conversation(&chat).expect("put_conversation");
    agent.put_conversation(&transcript).expect("put_conversation");

    let everything = agent.list_conversations(&ConversationQuery::default()).expect("all threads");
    assert_eq!(everything.len(), 2, "both are threads");

    let chats = agent.list_conversations(&ConversationQuery::chats(10)).expect("chats only");
    assert_eq!(chats.len(), 1, "an hour of research is not a conversation somebody had");
    assert_eq!(chats[0].id, chat.id);

    // The limit is honoured *after* the filter, as it is for a run's
    // transcript: a `LIMIT 1` pushed into SQL would have fetched the
    // transcript and answered with nothing at all.
    let one = agent.list_conversations(&ConversationQuery::chats(1)).expect("one chat");
    assert_eq!(one.len(), 1, "asking for one chat must give one chat");
    assert_eq!(one[0].id, chat.id);

    // The pointer back survives, which is how the Assistant app finds it.
    let back = agent.get_conversation(transcript.id).expect("get_conversation");
    assert_eq!(back.job_id, transcript.job_id);

    agent.delete_conversation(chat.id).expect("delete_conversation");
    agent.delete_conversation(transcript.id).expect("delete_conversation");
}

fn unicode_survives_a_job_round_trip(store: &dyn JournalStore) {
    let j = job_store(store);
    let mut job = Job::research(
        "மலையில் எந்த மின்சார சைக்கிள் நல்லது? \u{2600}",
        "தினமும் பன்னிரண்டு கிலோமீட்டர் பயணம்",
        None,
    );
    job.add_note(NoteId::new());
    job.finish("மூன்று மாதிரிகள்");
    j.put_job(&job).expect("put_job");

    let back = j.get_job(job.id).expect("get_job");
    assert_eq!(back.title(), job.title());
    assert_eq!(back, job);
    assert!(matches!(back.outputs[..], [JobOutput::Note { .. }]));
    j.delete_job(job.id).expect("delete_job");
}
