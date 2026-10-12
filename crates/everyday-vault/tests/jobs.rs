//! Background jobs over a real vault, below the tool catalogue: what the
//! facade promises about queueing, cancelling, deleting and counting them.
//! `tests/tools/jobs.rs` runs the same records through `dispatch` the way a
//! model would; the worker that actually runs one has its own tests in
//! `everyday-service`.

mod support;

use everyday_core::id::{ConversationId, JobId};
use everyday_core::{
    Conversation, ConversationQuery, Job, JobKind, JobOutput, JobQuery, JobStatus, Message, Note,
    Vault, VaultConfig,
};
use support::vault;

fn research(question: &str) -> JobKind {
    JobKind::Research { question: question.into(), brief: "For a commute.".into() }
}

/// A job that has run: it has a transcript with something in it, and it made
/// a note. Returns the three, none of which the vault should confuse.
fn a_job_that_ran(vault: &Vault) -> (Job, Conversation, Note) {
    let mut job = vault.queue_job(research("Which e-bike?"), None).unwrap();
    let transcript = Conversation::for_job(job.id, "Which e-bike?");
    vault.save_conversation(&transcript).unwrap();
    vault.save_message(&Message::assistant(transcript.id, "Reading example.com")).unwrap();
    let note = Note::written("Which e-bike?", "Three are worth a look.");
    vault.save_note(&note, None).unwrap();

    job.begin();
    job.conversation_id = Some(transcript.id);
    job.add_note(note.id);
    job.finish("Three bikes.");
    vault.save_job(&job).unwrap();
    (job, transcript, note)
}

// ---- queueing --------------------------------------------------------------

#[test]
fn queueing_saves_a_queued_job_and_hands_back_what_was_saved() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(vault.supports_jobs(), "the default backend must carry the job domain");
    assert!(vault.jobs(&JobQuery::default()).unwrap().is_empty());

    let conversation = ConversationId::new();
    let job = vault.queue_job(research("Which e-bike?"), Some(conversation)).unwrap();
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.asked_in, Some(conversation));
    assert_eq!(vault.job(job.id).unwrap(), job, "what it returned is what it stored");
    assert_eq!(vault.jobs(&JobQuery::unfinished()).unwrap(), vec![job.clone()]);
    assert_eq!(vault.jobs(&JobQuery::asked_in(conversation)).unwrap(), vec![job]);
}

#[test]
fn a_job_that_could_not_be_valid_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let blank = vault.queue_job(research("   "), None).unwrap_err();
    assert_eq!(blank.code(), "invalid");
    let oversize = vault.queue_job(research(&"q".repeat(10_000)), None).unwrap_err();
    assert_eq!(oversize.code(), "invalid");
    assert!(vault.jobs(&JobQuery::default()).unwrap().is_empty());

    // And a save of a job that has gone bad since is held to the same rule.
    let mut job = vault.queue_job(research("Which e-bike?"), None).unwrap();
    job.kind = JobKind::Research { question: "Which e-bike?".into(), brief: "b".repeat(100_000) };
    assert_eq!(vault.save_job(&job).unwrap_err().code(), "invalid");
}

#[test]
fn what_a_job_asked_is_sealed_with_the_rest_of_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = everyday_vault::create(dir.path(), cfg).unwrap();
    let job = vault
        .queue_job(
            JobKind::Research {
                question: "Is the oncology appointment worth rescheduling?".into(),
                brief: "Mentions the Hartley clinic.".into(),
            },
            Some(ConversationId::new()),
        )
        .unwrap();
    drop(vault);

    // Nothing a job says is in the database in the clear: a file that said
    // "researching the oncology appointment" would be telling somebody a
    // great deal.
    for needle in ["oncology", "Hartley"] {
        let mut leaked = Vec::new();
        scan(dir.path(), needle.as_bytes(), &mut leaked);
        assert!(leaked.is_empty(), "{needle:?} is readable in {leaked:?}");
    }

    // Locked, it is unreadable; unlocked, it opens under the label it was
    // sealed with, which is what would fail if it were written under another.
    let vault = everyday_vault::open(dir.path()).unwrap();
    assert_eq!(vault.jobs(&JobQuery::default()).unwrap_err().code(), "locked");
    vault.unlock(Some("correct horse battery")).unwrap();
    assert_eq!(vault.job(job.id).unwrap(), job);
}

/// Every file under `dir` whose bytes contain `needle`.
fn scan(dir: &std::path::Path, needle: &[u8], found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, needle, found);
        } else if let Ok(bytes) = std::fs::read(&path)
            && bytes.windows(needle.len()).any(|w| w == needle)
        {
            found.push(path);
        }
    }
}

// ---- cancelling ------------------------------------------------------------

#[test]
fn cancelling_saves_the_job_as_cancelled_and_hands_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let job = vault.queue_job(research("Which e-bike?"), None).unwrap();

    let cancelled = vault.cancel_job(job.id, "they changed their mind").unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert_eq!(cancelled.reason, "they changed their mind");
    assert!(cancelled.finished_at.is_some());
    assert_eq!(vault.job(job.id).unwrap(), cancelled, "and it is what was stored");
    assert!(vault.jobs(&JobQuery::unfinished()).unwrap().is_empty());
}

#[test]
fn a_running_job_is_cancelled_by_its_record_alone_and_keeps_what_it_made() {
    // The worker stops the turn by noticing this record change; the facade
    // is the same call whoever makes it, which is why it asks nothing of the
    // worker.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut job = vault.queue_job(research("Which e-bike?"), None).unwrap();
    job.begin();
    job.add_note(everyday_core::NoteId::new());
    job.progress = "Reading example.com".into();
    vault.save_job(&job).unwrap();

    let cancelled = vault.cancel_job(job.id, "stopped").unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert_eq!(cancelled.outputs, job.outputs, "what it had written is kept");
    assert_eq!(cancelled.progress, "Reading example.com");
}

#[test]
fn a_finished_or_missing_job_cannot_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let (done, _, _) = a_job_that_ran(&vault);

    let err = vault.cancel_job(done.id, "too late").unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("already done"), "got {err}");
    assert_eq!(vault.job(done.id).unwrap(), done, "a refused cancel rewrites nothing");

    assert_eq!(vault.cancel_job(JobId::new(), "nothing").unwrap_err().code(), "not_found");

    let job = vault.queue_job(research("Which tent?"), None).unwrap();
    vault.cancel_job(job.id, "first").unwrap();
    assert_eq!(vault.cancel_job(job.id, "second").unwrap_err().code(), "invalid");
    assert_eq!(vault.job(job.id).unwrap().reason, "first", "the first reason stands");
}

// ---- deleting --------------------------------------------------------------

#[test]
fn deleting_a_job_takes_its_transcript_and_leaves_the_note_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let (job, transcript, note) = a_job_that_ran(&vault);

    // The thread that asked, with the line a finished job leaves in it.
    let asking = Conversation::new();
    vault.save_conversation(&asking).unwrap();
    vault.save_message(&Message::system(asking.id, "Research finished: Which e-bike?")).unwrap();
    // And somebody else's job, with a transcript of its own.
    let (other, other_transcript, _) = a_job_that_ran(&vault);

    vault.delete_job(job.id).unwrap();

    assert_eq!(vault.job(job.id).unwrap_err().code(), "not_found");
    assert_eq!(vault.conversation(transcript.id).unwrap_err().code(), "not_found");
    assert!(vault.messages(transcript.id).unwrap().is_empty(), "its messages go with it");

    // The note is a note now: it is in the Notes app, and somebody may have
    // edited it.
    assert_eq!(vault.note(note.id).unwrap().id, note.id, "the note stays");
    assert!(vault.conversation(asking.id).is_ok(), "the thread that asked stays");
    assert_eq!(vault.message_count(asking.id).unwrap(), 1);
    assert!(vault.job(other.id).is_ok() && vault.conversation(other_transcript.id).is_ok());
}

#[test]
fn deleting_a_job_that_never_ran_needs_no_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let queued = vault.queue_job(research("Which e-bike?"), None).unwrap();

    vault.delete_job(queued.id).unwrap();
    assert!(vault.jobs(&JobQuery::default()).unwrap().is_empty());
    assert_eq!(vault.delete_job(queued.id).unwrap_err().code(), "not_found");
}

#[test]
fn a_job_that_is_still_running_must_be_stopped_before_it_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut job = vault.queue_job(research("Which e-bike?"), None).unwrap();
    let transcript = Conversation::for_job(job.id, "Which e-bike?");
    vault.save_conversation(&transcript).unwrap();
    job.begin();
    job.conversation_id = Some(transcript.id);
    vault.save_job(&job).unwrap();

    let err = vault.delete_job(job.id).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("cancel it first"), "got {err}");
    assert_eq!(vault.job(job.id).unwrap().status, JobStatus::Running, "still there");
    assert!(vault.conversation(transcript.id).is_ok(), "and its transcript is untouched");

    // Stopped, it can go.
    vault.cancel_job(job.id, "stopped").unwrap();
    vault.delete_job(job.id).unwrap();
    assert_eq!(vault.conversation(transcript.id).unwrap_err().code(), "not_found");
}

#[test]
fn a_job_whose_transcript_is_already_gone_can_still_be_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let (job, transcript, _) = a_job_that_ran(&vault);
    vault.delete_conversation(transcript.id).unwrap();

    vault.delete_job(job.id).expect("a missing transcript is not a reason to keep the job");
    assert_eq!(vault.job(job.id).unwrap_err().code(), "not_found");
}

// ---- looking ---------------------------------------------------------------

#[test]
fn unseen_jobs_are_counted_and_marking_them_seen_changes_the_record_too() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert_eq!(vault.unseen_jobs().unwrap(), 0);

    let one = vault.queue_job(research("One?"), None).unwrap();
    let two = vault.queue_job(research("Two?"), None).unwrap();
    let three = vault.queue_job(research("Three?"), None).unwrap();
    assert_eq!(vault.unseen_jobs().unwrap(), 3);

    vault.mark_jobs_seen(&[one.id]).unwrap();
    assert_eq!(vault.unseen_jobs().unwrap(), 2);
    assert!(vault.job(one.id).unwrap().seen, "the record agrees with the count");
    assert!(!vault.job(two.id).unwrap().seen);
    assert_eq!(vault.jobs(&JobQuery::unseen(10)).unwrap().len(), 2);

    // Marking is not a rewrite of anything else about the job.
    let mut expected = one.clone();
    expected.seen = true;
    assert_eq!(vault.job(one.id).unwrap(), expected);

    // An empty list is "all of them".
    vault.mark_jobs_seen(&[]).unwrap();
    assert_eq!(vault.unseen_jobs().unwrap(), 0);
    assert!(vault.job(two.id).unwrap().seen && vault.job(three.id).unwrap().seen);

    // And what arrives afterwards counts again.
    vault.queue_job(research("Four?"), None).unwrap();
    assert_eq!(vault.unseen_jobs().unwrap(), 1);
}

#[test]
fn a_jobs_transcript_is_kept_out_of_the_history_of_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let chat = Conversation::new();
    vault.save_conversation(&chat).unwrap();
    let (_, transcript, _) = a_job_that_ran(&vault);
    assert!(chat.is_chat() && !transcript.is_chat());

    let chats = vault.conversations(&ConversationQuery::chats(10)).unwrap();
    assert_eq!(chats.iter().map(|c| c.id).collect::<Vec<_>>(), vec![chat.id]);
    let one = vault.conversations(&ConversationQuery::chats(1)).unwrap();
    assert_eq!(one.len(), 1, "the limit is applied after the transcripts are left out");

    // The Assistant app lists them all, which is where a job's transcript is read.
    let everything = vault.conversations(&ConversationQuery::default()).unwrap();
    assert_eq!(everything.len(), 2);
    assert_eq!(
        vault.conversation(transcript.id).unwrap().job_id,
        transcript.job_id,
        "and the pointer back to the job survives"
    );
}

#[test]
fn a_job_lists_the_note_it_wrote_in_the_order_it_wrote_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let (job, _, note) = a_job_that_ran(&vault);

    let back = vault.job(job.id).unwrap();
    assert_eq!(back.outputs, [JobOutput::Note { note_id: note.id }]);
    assert_eq!(back.note_id(), Some(note.id));
    assert!(back.conversation_id.is_some());
    assert_eq!(back.status, JobStatus::Done);
}
