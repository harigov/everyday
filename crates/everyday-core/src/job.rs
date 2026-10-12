//! Work the assistant does in the background.
//!
//! A [`Job`] is one piece of work the assistant took on because a question
//! needed more time than a turn has: it says so in the conversation, the job
//! is queued, and the turn ends. A worker in `everyday-service` picks it up,
//! runs it as an unattended turn of its own, and when it is done comes back
//! to the conversation that asked with an answer. See `docs/plans/jobs.md`.
//!
//! Research is the first kind. The record is shaped so the next -- reaching
//! the web through a browser -- is a new [`JobKind`] variant and an executor
//! in the service, not a second record: where it stands, what it made, its
//! transcript and where to come back to are the same for every kind.
//!
//! Nothing here opens a socket or runs a model. This file decides what a job
//! *is* and which moves between its states are allowed, which is why every
//! rule in it is a unit test.

use crate::error::{Error, Result};
use crate::id::{ConversationId, JobId, NoteId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Longest a research question may be. A question, not a document: the
/// brief beside it is where context goes.
pub const MAX_QUESTION_BYTES: usize = 2_000;

/// Longest a brief may be. Long enough for a paragraph of who is asking and
/// what they ruled out; short enough that it cannot carry a mailbox.
pub const MAX_BRIEF_BYTES: usize = 8_000;

/// How many times a job interrupted by a restart or a lock is started again
/// before it is failed. Once is the honest number: an interruption is not the
/// job's fault, two in a row probably say something about the job.
pub const MAX_ATTEMPTS: u32 = 2;

/// What a job is, with its arguments.
///
/// An enum rather than a name and a bag of fields, for the reason
/// [`crate::routine::Trigger`] is one: each kind carries exactly what it
/// needs and nothing it does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobKind {
    /// Find out, from the web and from the vault, and write it up as a note.
    Research {
        /// The question, in the person's words where the assistant has them.
        question: String,
        /// What the assistant that started the job knows and the job should:
        /// who is asking and what for, what they already know or ruled out,
        /// what shape of answer would help. Written by the model that
        /// started it, because nothing else about that conversation reaches
        /// the job.
        #[serde(default)]
        brief: String,
    },
}

impl JobKind {
    /// The kind's name on the wire and in a log line: `"research"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            JobKind::Research { .. } => "research",
        }
    }

    /// What the interface calls this kind of job: "Research".
    pub fn label(&self) -> &'static str {
        match self {
            JobKind::Research { .. } => "Research",
        }
    }

    /// One line saying what this job is about, for a card, a list row and
    /// a notification: a research job's question.
    pub fn title(&self) -> String {
        match self {
            JobKind::Research { question, .. } => question.trim().to_string(),
        }
    }
}

/// Where a job stands.
///
/// Its own set rather than [`crate::routine::Outcome`]: `Skipped` means
/// something precise for a routine (its moment passed) that a job never is,
/// and `Cancelled` means something a routine never is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    /// Asked for, and not started yet.
    #[default]
    Queued,
    /// Under way. A row in this state that no live process claims is one a
    /// dead process left, and the worker's sweep puts it back in the queue
    /// (or fails it, past [`MAX_ATTEMPTS`]).
    Running,
    /// Finished, and what it made is in [`Job::outputs`].
    Done,
    /// The model refused, the endpoint was unreachable, it ran out of time,
    /// it was interrupted too often. Why is in [`Job::reason`].
    Failed,
    /// Somebody stopped it: the person, or the assistant on their behalf.
    /// Whatever it had written by then is kept.
    Cancelled,
}

impl JobStatus {
    pub const ALL: [JobStatus; 5] = [
        JobStatus::Queued,
        JobStatus::Running,
        JobStatus::Done,
        JobStatus::Failed,
        JobStatus::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    /// Whether this job is over, one way or another.
    pub fn is_finished(self) -> bool {
        !matches!(self, JobStatus::Queued | JobStatus::Running)
    }
}

/// Something a job made, in the order it made them.
///
/// A pointer, never a copy: the note a research job wrote lives in the Notes
/// app like any other, and is found there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobOutput {
    /// A note. A research job's write-up.
    Note { note_id: NoteId },
}

/// One piece of background work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    /// The conversation to come back to when it is done. `None` for a job
    /// started from the Assistant app or the CLI rather than from a thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_in: Option<ConversationId>,
    pub status: JobStatus,
    /// Why it failed or was cancelled. Empty when it simply worked.
    #[serde(default)]
    pub reason: String,
    /// One line for the card while it runs: "Reading example.com". Empty
    /// until it has done something.
    #[serde(default)]
    pub progress: String,
    /// How many tools it has called.
    #[serde(default)]
    pub steps: u32,
    /// What it made. A research job's note goes in here the moment the note
    /// exists, not at the end, so a job that fails half way has still left
    /// what it found somewhere to be read.
    #[serde(default)]
    pub outputs: Vec<JobOutput>,
    /// The model's last message: what it has to say for itself.
    #[serde(default)]
    pub summary: String,
    /// The job's own transcript, kept out of the rail's history the way a
    /// routine run's is. Absent until it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    /// Whether the follow-up turn in [`Job::asked_in`] has been had. Apart
    /// from `status` so that a restart between finishing and answering can
    /// still answer.
    #[serde(default)]
    pub answered: bool,
    /// How many times it has been started. See [`MAX_ATTEMPTS`].
    #[serde(default)]
    pub attempts: u32,
    /// Whether anybody has looked at it. Part of the count on the app bar.
    #[serde(default)]
    pub seen: bool,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
}

impl Job {
    /// A queued job.
    pub fn new(kind: JobKind, asked_in: Option<ConversationId>) -> Self {
        Self {
            id: JobId::new(),
            kind,
            asked_in,
            status: JobStatus::Queued,
            reason: String::new(),
            progress: String::new(),
            steps: 0,
            outputs: Vec::new(),
            summary: String::new(),
            conversation_id: None,
            answered: false,
            attempts: 0,
            seen: false,
            created_at: Timestamp::now(),
            started_at: None,
            finished_at: None,
        }
    }

    /// A research job, queued.
    pub fn research(
        question: impl Into<String>,
        brief: impl Into<String>,
        asked_in: Option<ConversationId>,
    ) -> Self {
        Self::new(JobKind::Research { question: question.into(), brief: brief.into() }, asked_in)
    }

    /// See [`JobKind::title`].
    pub fn title(&self) -> String {
        self.kind.title()
    }

    /// The first note it made, if it has made one. A research job makes
    /// exactly one, and keeps writing into it.
    pub fn note_id(&self) -> Option<NoteId> {
        self.outputs
            .iter()
            .map(|o| match o {
                JobOutput::Note { note_id } => *note_id,
            })
            .next()
    }

    /// Record a note it made, once.
    pub fn add_note(&mut self, note_id: NoteId) {
        let output = JobOutput::Note { note_id };
        if !self.outputs.contains(&output) {
            self.outputs.push(output);
        }
    }

    /// Check what a job may hold before it is saved.
    pub fn validate(&self) -> Result<()> {
        match &self.kind {
            JobKind::Research { question, brief } => {
                if question.trim().is_empty() {
                    return Err(Error::Invalid(
                        "a research job needs a question: what should it find out?".into(),
                    ));
                }
                if question.len() > MAX_QUESTION_BYTES {
                    return Err(Error::Invalid(format!(
                        "the question is {} bytes; the limit is {MAX_QUESTION_BYTES}. Put \
                         the context in the brief",
                        question.len()
                    )));
                }
                if brief.len() > MAX_BRIEF_BYTES {
                    return Err(Error::Invalid(format!(
                        "the brief is {} bytes; the limit is {MAX_BRIEF_BYTES}",
                        brief.len()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Mark it under way, now. Counts an attempt.
    pub fn begin(&mut self) {
        self.status = JobStatus::Running;
        self.started_at = Some(Timestamp::now());
        self.finished_at = None;
        self.attempts += 1;
    }

    /// It finished. `summary` is the model's last message.
    pub fn finish(&mut self, summary: impl Into<String>) {
        self.status = JobStatus::Done;
        self.summary = summary.into();
        self.finished_at = Some(Timestamp::now());
    }

    /// It could not finish, and why.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.status = JobStatus::Failed;
        self.reason = reason.into();
        self.finished_at = Some(Timestamp::now());
    }

    /// Stop it, and say who did. Refused for a job that is already over:
    /// cancelling a finished job would rewrite what happened to it.
    pub fn cancel(&mut self, reason: impl Into<String>) -> Result<()> {
        if self.status.is_finished() {
            return Err(Error::Invalid(format!(
                "that job is already {}; there is nothing to cancel",
                self.status.as_str()
            )));
        }
        self.status = JobStatus::Cancelled;
        self.reason = reason.into();
        self.finished_at = Some(Timestamp::now());
        Ok(())
    }

    /// Put a job a dead process or a lock left `Running` back in the queue,
    /// or fail it if it has been started [`MAX_ATTEMPTS`] times already.
    /// Returns whether it was requeued.
    pub fn interrupted(&mut self, why: &str) -> bool {
        if self.attempts >= MAX_ATTEMPTS {
            self.fail(format!("it was interrupted {} times ({why})", self.attempts));
            false
        } else {
            self.status = JobStatus::Queued;
            self.progress = String::new();
            true
        }
    }

    /// Whether the conversation that asked is still owed a follow-up.
    pub fn owes_answer(&self) -> bool {
        self.status == JobStatus::Done && !self.answered && self.asked_in.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn research() -> Job {
        Job::research("Which e-bike for hills?", "Commutes 12 km; has ruled out hub motors.", None)
    }

    fn invalid(result: Result<()>) -> String {
        match result.expect_err("expected the job to be refused") {
            Error::Invalid(why) => why,
            other => panic!("expected an Invalid error, got {other:?}"),
        }
    }

    // ---- what a new job is -------------------------------------------------

    #[test]
    fn a_new_job_is_queued_and_has_done_nothing() {
        let asked_in = ConversationId::new();
        let job = Job::research("Which e-bike?", "For hills.", Some(asked_in));
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.asked_in, Some(asked_in));
        assert_eq!(job.attempts, 0);
        assert_eq!(job.steps, 0);
        assert!(job.outputs.is_empty() && job.note_id().is_none());
        assert!(job.conversation_id.is_none(), "no transcript until it starts");
        assert!(!job.answered && !job.seen);
        assert!(job.started_at.is_none() && job.finished_at.is_none());
        assert!(job.reason.is_empty() && job.progress.is_empty() && job.summary.is_empty());
        assert_eq!(job.kind.as_str(), "research");
    }

    #[test]
    fn a_kind_names_itself_and_its_job_for_a_card_and_a_notification() {
        let kind = JobKind::Research { question: "  Which e-bike?\n".into(), brief: "x".into() };
        assert_eq!(kind.as_str(), "research");
        assert_eq!(kind.label(), "Research");
        assert_eq!(kind.title(), "Which e-bike?", "the question, without its padding");
        assert_eq!(Job::new(kind, None).title(), "Which e-bike?");
    }

    // ---- what may be saved --------------------------------------------------

    #[test]
    fn a_research_job_needs_a_question() {
        for blank in ["", "   ", "\n\t"] {
            let job = Job::research(blank, "context", None);
            assert!(invalid(job.validate()).contains("question"), "{blank:?} is not a question");
        }
        assert!(research().validate().is_ok());
    }

    #[test]
    fn the_question_is_capped_in_bytes_and_the_refusal_points_at_the_brief() {
        let at_the_limit = Job::research("q".repeat(MAX_QUESTION_BYTES), "", None);
        assert!(at_the_limit.validate().is_ok(), "the limit itself is allowed");

        let over = Job::research("q".repeat(MAX_QUESTION_BYTES + 1), "", None);
        let why = invalid(over.validate());
        assert!(why.contains(&MAX_QUESTION_BYTES.to_string()), "should state the limit: {why}");
        assert!(why.contains("brief"), "should say where the context goes: {why}");

        // Bytes, not characters: 667 three-byte characters is 2001 bytes.
        let wide = Job::research("\u{0b95}".repeat(MAX_QUESTION_BYTES / 3 + 1), "", None);
        assert!(wide.validate().is_err(), "the limit is on bytes");
    }

    #[test]
    fn the_brief_may_be_empty_but_not_enormous() {
        assert!(Job::research("Which e-bike?", "", None).validate().is_ok());
        let at_the_limit = Job::research("Which e-bike?", "b".repeat(MAX_BRIEF_BYTES), None);
        assert!(at_the_limit.validate().is_ok(), "the limit itself is allowed");

        let over = Job::research("Which e-bike?", "b".repeat(MAX_BRIEF_BYTES + 1), None);
        let why = invalid(over.validate());
        assert!(why.contains("brief") && why.contains(&MAX_BRIEF_BYTES.to_string()), "{why}");
    }

    // ---- moving between states ---------------------------------------------

    #[test]
    fn beginning_marks_it_running_and_counts_the_attempt() {
        let mut job = research();
        job.begin();
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.attempts, 1);
        assert!(job.started_at.is_some());
        assert!(job.finished_at.is_none());
    }

    #[test]
    fn finishing_keeps_what_the_model_said_last() {
        let mut job = research();
        job.begin();
        job.finish("Three bikes are worth a look.");
        assert_eq!(job.status, JobStatus::Done);
        assert_eq!(job.summary, "Three bikes are worth a look.");
        assert!(job.finished_at.is_some());
        assert!(job.reason.is_empty(), "a job that worked has nothing to explain");
        assert!(job.status.is_finished());
    }

    #[test]
    fn failing_says_why() {
        let mut job = research();
        job.begin();
        job.fail("the endpoint refused");
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.reason, "the endpoint refused");
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn a_queued_or_running_job_can_be_cancelled() {
        let mut queued = research();
        queued.cancel("they changed their mind").unwrap();
        assert_eq!(queued.status, JobStatus::Cancelled);
        assert_eq!(queued.reason, "they changed their mind");
        assert!(queued.finished_at.is_some());

        let mut running = research();
        running.begin();
        running.cancel("stopped by the assistant").unwrap();
        assert_eq!(running.status, JobStatus::Cancelled);
        assert!(running.started_at.is_some(), "when it started is still true");
    }

    #[test]
    fn a_finished_job_cannot_be_cancelled_and_is_left_as_it_was() {
        let mut done = research();
        done.begin();
        done.finish("how it ended");
        let mut failed = research();
        failed.begin();
        failed.fail("how it ended");

        for (mut job, state) in [(done, "done"), (failed, "failed")] {
            let before = job.clone();
            let why = invalid(job.cancel("too late"));
            assert!(why.contains(state), "should say it is already {state}: {why}");
            assert_eq!(job, before, "a refused cancel rewrites nothing");
        }

        let mut cancelled = research();
        cancelled.cancel("first").unwrap();
        assert!(invalid(cancelled.cancel("second")).contains("cancelled"));
        assert_eq!(cancelled.reason, "first", "the first reason stands");
    }

    #[test]
    fn an_interrupted_job_is_requeued_once_and_then_failed() {
        let note = NoteId::new();
        let mut job = research();
        job.add_note(note);

        for _ in 1..MAX_ATTEMPTS {
            job.begin();
            job.progress = "Reading example.com".into();
            assert!(job.interrupted("the vault was locked"), "worth another go");
            assert_eq!(job.status, JobStatus::Queued);
            assert!(job.progress.is_empty(), "the line described a run that is over");
            assert_eq!(job.note_id(), Some(note), "what it wrote is kept to resume from");
            assert!(job.finished_at.is_none());
        }

        job.begin();
        assert_eq!(job.attempts, MAX_ATTEMPTS);
        assert!(!job.interrupted("the app was closed"), "out of attempts");
        assert_eq!(job.status, JobStatus::Failed);
        assert!(job.reason.contains(&MAX_ATTEMPTS.to_string()), "{}", job.reason);
        assert!(job.reason.contains("the app was closed"), "{}", job.reason);
        assert!(job.finished_at.is_some());
        assert_eq!(job.note_id(), Some(note), "and the note is still there to read");
    }

    #[test]
    fn a_job_that_never_began_is_requeued_not_failed() {
        let mut job = research();
        assert!(job.interrupted("a restart"));
        assert_eq!(job.status, JobStatus::Queued);
    }

    // ---- what it made -------------------------------------------------------

    #[test]
    fn adding_a_note_twice_records_it_once() {
        let first = NoteId::new();
        let mut job = research();
        assert_eq!(job.note_id(), None);

        job.add_note(first);
        job.add_note(first);
        assert_eq!(job.outputs, vec![JobOutput::Note { note_id: first }]);
        assert_eq!(job.note_id(), Some(first));

        // A second, different note is a second output, and the first is
        // still the one a research job means by "its note".
        let second = NoteId::new();
        job.add_note(second);
        assert_eq!(job.outputs.len(), 2);
        assert_eq!(job.note_id(), Some(first));
    }

    // ---- coming back --------------------------------------------------------

    #[test]
    fn only_a_finished_unanswered_job_that_was_asked_in_a_thread_owes_an_answer() {
        let asked = Some(ConversationId::new());
        let make = |status: JobStatus, answered: bool, asked_in: Option<ConversationId>| {
            let mut job = Job::research("Which e-bike?", "", asked_in);
            job.status = status;
            job.answered = answered;
            job.owes_answer()
        };

        assert!(make(JobStatus::Done, false, asked));
        assert!(!make(JobStatus::Done, true, asked), "already answered");
        assert!(!make(JobStatus::Done, false, None), "nobody to answer");
        for status in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Failed,
            // Whoever cancelled it already knows.
            JobStatus::Cancelled,
        ] {
            assert!(!make(status, false, asked), "{status:?} owes nothing");
        }
    }

    // ---- statuses -----------------------------------------------------------

    #[test]
    fn a_job_is_over_once_it_is_neither_queued_nor_running() {
        let finished: Vec<JobStatus> =
            JobStatus::ALL.into_iter().filter(|s| s.is_finished()).collect();
        assert_eq!(finished, [JobStatus::Done, JobStatus::Failed, JobStatus::Cancelled]);
        assert_eq!(JobStatus::default(), JobStatus::Queued);
    }

    #[test]
    fn a_status_is_spelled_the_same_in_code_and_on_the_wire() {
        for status in JobStatus::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(status.as_str()));
        }
    }

    // ---- the record on the wire --------------------------------------------

    #[test]
    fn a_job_serialises_in_camel_case_with_its_kind_tagged() {
        let asked_in = ConversationId::new();
        let note = NoteId::new();
        let mut job = Job::research("Which e-bike?", "For hills.", Some(asked_in));
        job.add_note(note);
        let wire = serde_json::to_value(&job).unwrap();

        assert_eq!(
            wire["kind"],
            json!({ "type": "research", "question": "Which e-bike?", "brief": "For hills." })
        );
        assert_eq!(wire["kind"]["type"], job.kind.as_str());
        assert_eq!(wire["status"], "queued");
        assert_eq!(wire["askedIn"], asked_in.to_string());
        assert_eq!(wire["outputs"], json!([{ "type": "note", "noteId": note.to_string() }]));
        assert!(wire["createdAt"].is_string());
        for snake in ["asked_in", "created_at", "conversation_id", "started_at"] {
            assert!(wire.get(snake).is_none(), "{snake} should be camelCase on the wire");
        }

        // What is not there yet is not written, so an old reader and a new
        // one agree about a job that has not started.
        for absent in ["conversationId", "startedAt", "finishedAt"] {
            assert!(wire.get(absent).is_none(), "{absent} should be left out until it is true");
        }
    }

    #[test]
    fn every_state_a_job_passes_through_survives_a_round_trip() {
        fn round_trips(job: &Job) {
            let back: Job = serde_json::from_value(serde_json::to_value(job).unwrap()).unwrap();
            assert_eq!(&back, job, "{:?}", job.status);
        }

        let mut job = Job::research("Which e-bike?", "For hills.", Some(ConversationId::new()));
        round_trips(&job);

        job.begin();
        job.progress = "Read 14 sources; writing up".into();
        job.steps = 31;
        job.conversation_id = Some(ConversationId::new());
        job.add_note(NoteId::new());
        round_trips(&job);

        job.finish("Three bikes are worth a look.");
        round_trips(&job);

        job.answered = true;
        job.seen = true;
        round_trips(&job);
    }

    #[test]
    fn a_record_with_only_what_the_first_version_wrote_still_reads() {
        // A job saved before a field existed has to open under the field's
        // default, or a build that adds one makes every record already on
        // disk unreadable.
        let old = json!({
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "kind": { "type": "research", "question": "Which e-bike?" },
            "status": "running",
            "createdAt": "2026-10-12T08:00:00Z",
        });
        let job: Job = serde_json::from_value(old).unwrap();
        assert_eq!(
            job.kind,
            JobKind::Research { question: "Which e-bike?".into(), brief: String::new() }
        );
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.asked_in, None);
        assert_eq!(job.conversation_id, None);
        assert_eq!(job.steps, 0);
        assert_eq!(job.attempts, 0);
        assert!(job.reason.is_empty() && job.progress.is_empty() && job.summary.is_empty());
        assert!(job.outputs.is_empty());
        assert!(!job.answered && !job.seen);
        assert!(job.started_at.is_none() && job.finished_at.is_none());
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_refused_rather_than_misread() {
        let future = json!({ "type": "browse", "goal": "book the table" });
        assert!(serde_json::from_value::<JobKind>(future).is_err());
    }
}
