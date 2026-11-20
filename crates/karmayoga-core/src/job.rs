//! Work the assistant does in the background.
//!
//! A [`Job`] is one piece of work the assistant took on because a question
//! needed more time than a turn has: it says so in the conversation, the job
//! is queued, and the turn ends. A worker in `karmayoga-service` picks it up,
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
//!
//! # Jobs that sleep and wake
//!
//! Research and a task are one long turn each. A [`JobKind::Watch`] is not: it
//! looks, sleeps, looks again, and says something only when what it sees is
//! what the person asked to hear about. Between looks it is
//! [`JobStatus::Waiting`], with [`Job::wake_at`] saying when it next looks,
//! [`Job::observed`] what it saw last, and [`Job::updates`] what it has told
//! them so far. The person can change how it behaves while it runs --
//! [`JobRules`] -- from any conversation. See `docs/plans/watches.md`.

use crate::error::{Error, Result};
use crate::id::{ConversationId, JobId, NoteId, TaskId};
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

/// How long a watch keeps looking when nobody says: eight hours. Long enough
/// for a working day's CI or a delivery, short enough that one nobody
/// remembers starting does not look all week.
pub const DEFAULT_WATCH_HOURS: i64 = 8;

/// The longest a watch may be told to keep looking: a week from now. Held by
/// [`JobRules::validate_since`], so a record with a later end is not saved.
pub const MAX_WATCH_HOURS: i64 = 7 * 24;

/// How often a watch looks when nobody says.
pub const DEFAULT_EVERY_MINUTES: u32 = 5;

/// The most often a watch with a fixed check may look.
pub const MIN_EVERY_MINUTES: u32 = 1;

/// The most often a watch whose every look is a turn of the assistant's may
/// look: each costs a model turn, and a quarter of an hour is as fresh as a
/// thing worth that is likely to need.
pub const MIN_ASSISTANT_EVERY_MINUTES: u32 = 15;

/// The least often a watch may look.
pub const MAX_EVERY_MINUTES: u32 = 24 * 60;

/// Longest a rule the person gives -- when to tell them, what to do -- may be.
pub const MAX_RULE_BYTES: usize = 1_000;

/// Longest the few words saying what is watched may be. A card's title.
pub const MAX_WHAT_BYTES: usize = 300;

/// Longest a project's goal may be. A goal; the brief holds the context.
pub const MAX_GOAL_BYTES: usize = 2_000;

/// How many of the updates a job has sent are kept on it, newest last.
pub const MAX_UPDATES_KEPT: usize = 30;

/// The most updates a job holds when some of them have not been sent yet. An
/// update still to go is never dropped to make room -- the person has not been
/// told it -- until this many are held, which only a worker that has stopped
/// sending them could reach.
pub const MAX_UPDATES_HELD: usize = 2 * MAX_UPDATES_KEPT;

/// Longest one update may be.
pub const MAX_UPDATE_BYTES: usize = 4_000;

/// The most of one observation kept to compare the next with.
pub const MAX_OBSERVATION_BYTES: usize = 16_000;

/// How many looks in a row may fail before a watch gives up.
pub const MAX_FAILED_LOOKS: u32 = 3;

/// How far a watch's end may be from the moment it is measured against, in
/// either direction, before it is refused: a clock a minute out, or a save a
/// moment after the end was worked out, is not a reason to refuse a watch.
pub const END_GRACE_SECS: i64 = 60;

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
    /// Do one of the assistant's own tasks -- see
    /// [`Assignee::Assistant`](crate::task::Assignee::Assistant). Queued by
    /// the scheduler when the task's reminder comes round, never by a model:
    /// what to do is read from the task when the job starts.
    Task {
        task_id: TaskId,
        /// The task's title when the job was queued, for a card and a list
        /// row that should not have to read the task to say what it is.
        title: String,
    },
    /// Keep an eye on something -- a CI run, a delivery, an issue -- and tell
    /// the person when it changes in a way they asked to hear about. Looks
    /// every [`JobRules::every_minutes`] until [`JobRules::until`], sleeping
    /// in between as [`JobStatus::Waiting`].
    Watch {
        /// What is watched, in a few words: "CI on pull request 412". The
        /// card's title and what an update says it is about.
        what: String,
        /// How each look is taken.
        probe: Probe,
    },
    /// A longer piece of work the assistant gets on with in the background:
    /// planning a trip, drafting a proposal. Like research it writes into a
    /// note and comes back to the conversation that asked when it is done;
    /// unlike research it may ask the person a question in their chat while
    /// it works, and send them a short update when it reaches a milestone.
    Project {
        /// What to get done, in their words where the assistant has them.
        goal: String,
        /// What the job should know that it will not otherwise, as for
        /// research.
        #[serde(default)]
        brief: String,
    },
}

/// How a watch looks at what it is watching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Probe {
    /// One tool of a connected service, called with the same arguments every
    /// time, no model involved: the cheap way, and the one to prefer. Only a
    /// tool the person allows to run unasked may be used, since nobody is
    /// there when it runs.
    Connection {
        /// The connection's name, as Settings → Connections shows it.
        connection: String,
        /// The tool's name on that connection.
        tool: String,
        /// Its arguments. `{}` for a tool that takes none.
        #[serde(default = "empty_arguments")]
        arguments: serde_json::Value,
    },
    /// No fixed check fits: the assistant looks, with its tools, as `how`
    /// says, and what it reports is the observation. Every look is a model
    /// turn, so it may look at most every
    /// [`MIN_ASSISTANT_EVERY_MINUTES`].
    Assistant {
        /// How to look, in a sentence or two: "Check my inbox for a reply
        /// from Sam about the lease".
        how: String,
    },
}

fn empty_arguments() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl Probe {
    /// The kind of look on the wire and in a log line.
    pub fn as_str(&self) -> &'static str {
        match self {
            Probe::Connection { .. } => "connection",
            Probe::Assistant { .. } => "assistant",
        }
    }

    /// The fewest minutes between two looks of this kind.
    pub fn min_every_minutes(&self) -> u32 {
        match self {
            Probe::Connection { .. } => MIN_EVERY_MINUTES,
            Probe::Assistant { .. } => MIN_ASSISTANT_EVERY_MINUTES,
        }
    }

    /// How it looks, in a clause: "calls get_issue on Work Linear", "asks the
    /// assistant: check my inbox for a reply from Sam". For the assistant to
    /// say back and for a card; the arguments of a call are left out, since
    /// they are for the service and not for a sentence.
    pub fn in_words(&self) -> String {
        match self {
            Probe::Connection { connection, tool, .. } => {
                format!("calls {} on {}", tool.trim(), connection.trim())
            }
            Probe::Assistant { how } => format!("asks the assistant: {}", how.trim()),
        }
    }
}

/// How a job that runs in steps behaves, which the person may change while it
/// runs -- "only tell me if it fails" -- from any conversation, through the
/// assistant's `update_job`. Empty for research and tasks, which have none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct JobRules {
    /// When to tell them, in their words: "when it finishes or fails",
    /// "whenever anything changes". Empty means any change worth knowing.
    pub notify_when: String,
    /// What to do as well as tell, when there is something to tell: "read
    /// the failing step's log and say why". Empty means just tell.
    pub act: String,
    /// Minutes between looks. Zero means [`DEFAULT_EVERY_MINUTES`].
    pub every_minutes: u32,
    /// When it stops on its own. Set when a watch is made, to
    /// [`DEFAULT_WATCH_HOURS`] from then unless they said otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
    /// Ring them on a call with an update rather than writing it, when they
    /// are not already talking to the assistant -- the routine's "call me
    /// with it".
    pub call_me: bool,
}

impl JobRules {
    /// Whether nothing has been set: what a research job or a task has.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Check what the rules may hold. `probe` is the watch's, which decides
    /// how often it may look. The end is measured from now: see
    /// [`JobRules::validate_since`].
    pub fn validate(&self, probe: Option<&Probe>) -> Result<()> {
        self.validate_since(probe, Timestamp::now())
    }

    /// As [`JobRules::validate`], with the end measured from `since`: the
    /// moment the watch was made.
    ///
    /// The end may not be before `since`, nor more than [`MAX_WATCH_HOURS`]
    /// after the later of `since` and now. Measured from when the watch was
    /// made rather than from now at the early end, because a watch that has
    /// run its eight hours still has to be saved -- with its look, its last
    /// update, its ending -- and an end that is behind us by then is not a
    /// reason to refuse it. At the late end, now is what counts, since the end
    /// is moved on from now (`update_job`'s `hours`) and may be a week from
    /// today for a watch made a week ago; a fixed date that was fine when it
    /// was written stays fine, since now only moves forward.
    pub fn validate_since(&self, probe: Option<&Probe>, since: Timestamp) -> Result<()> {
        if let Some(until) = self.until {
            let grace = jiff::SignedDuration::from_secs(END_GRACE_SECS);
            if until < since - grace {
                return Err(Error::Invalid(
                    "a watch cannot stop before it starts: its end is in the past".into(),
                ));
            }
            let latest = since.max(Timestamp::now())
                + jiff::SignedDuration::from_hours(MAX_WATCH_HOURS)
                + grace;
            if until > latest {
                return Err(Error::Invalid(format!(
                    "a watch may keep going at most {MAX_WATCH_HOURS} hours (a week) from now"
                )));
            }
        }
        for (name, rule) in [("when to tell them", &self.notify_when), ("what to do", &self.act)] {
            if rule.len() > MAX_RULE_BYTES {
                return Err(Error::Invalid(format!(
                    "{name} is {} bytes; the limit is {MAX_RULE_BYTES}",
                    rule.len()
                )));
            }
        }
        if self.every_minutes != 0 {
            let least = probe.map_or(MIN_EVERY_MINUTES, Probe::min_every_minutes);
            if self.every_minutes < least || self.every_minutes > MAX_EVERY_MINUTES {
                return Err(Error::Invalid(format!(
                    "a watch looks every {least} to {MAX_EVERY_MINUTES} minutes, not every {}",
                    self.every_minutes
                )));
            }
        }
        Ok(())
    }

    /// How long to sleep between looks, with the default filled in and held
    /// to what `probe` allows.
    pub fn every(&self, probe: Option<&Probe>) -> std::time::Duration {
        let least = probe.map_or(MIN_EVERY_MINUTES, Probe::min_every_minutes);
        let minutes = match self.every_minutes {
            0 => DEFAULT_EVERY_MINUTES,
            n => n,
        }
        .clamp(least, MAX_EVERY_MINUTES);
        std::time::Duration::from_secs(u64::from(minutes) * 60)
    }

    /// The rules as one line a person can read and the assistant can say
    /// back: "tells them: when it fails; looks every 5 minutes; stops at
    /// 2026-11-20 18:40". The end is shown in `zone`, the person's own.
    ///
    /// What the person wrote -- `notify_when`, `act` -- is quoted as it
    /// stands, so a line built from rules the assistant was handed can carry
    /// whatever those words say.
    pub fn in_words(&self, probe: Option<&Probe>, zone: &jiff::tz::TimeZone) -> String {
        let mut parts = vec![match self.notify_when.trim() {
            "" => "tells them about any change worth knowing".to_string(),
            when => format!("tells them: {when}"),
        }];
        if !self.act.trim().is_empty() {
            parts
                .push(format!("also does this when it has something to tell: {}", self.act.trim()));
        }
        let minutes = self.every(probe).as_secs() / 60;
        parts.push(format!("looks every {}", minutes_in_words(minutes)));
        if let Some(until) = self.until {
            parts.push(format!(
                "stops at {}",
                until.to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M")
            ));
        }
        if self.call_me {
            parts.push("rings them with it when they are not already talking to you".to_string());
        }
        parts.join("; ")
    }
}

/// `5` as "5 minutes", `60` as "hour", `90` as "90 minutes", `120` as "2 hours".
fn minutes_in_words(minutes: u64) -> String {
    match minutes {
        1 => "minute".to_string(),
        60 => "hour".to_string(),
        m if m > 60 && m.is_multiple_of(60) => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

/// What a watch saw last, which the next look is compared with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct Observed {
    /// A hash of the last observation, made after the parts that change on
    /// every look (request ids, "seconds ago") were taken out. Only a change
    /// in this asks a model anything.
    pub fingerprint: String,
    /// The last observation itself, cut to [`MAX_OBSERVATION_BYTES`]: what
    /// the next one is judged against. Written by a service or a page, so
    /// never instructions.
    pub last: String,
    /// When it last looked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Timestamp>,
    /// How many looks it has taken.
    pub checks: u32,
    /// How many looks in a row have failed. See [`MAX_FAILED_LOOKS`].
    pub failures: u32,
    /// Why the last look failed, when it did.
    pub trouble: String,
}

/// Something a job told the person, or has to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobUpdate {
    pub at: Timestamp,
    /// What it said, in Markdown, short enough for a phone.
    pub text: String,
    /// Whether it has been sent on. A project's own turn writes one with
    /// `report_progress` and the worker sends it; a watch's are sent as they
    /// are made.
    #[serde(default)]
    pub delivered: bool,
    /// Where it went: "thread", "chat", "voice", "call", "notification",
    /// "held" (said later, after quiet hours). For the card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent_to: Vec<String>,
}

impl JobKind {
    /// The kind's name on the wire and in a log line: `"research"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            JobKind::Research { .. } => "research",
            JobKind::Task { .. } => "task",
            JobKind::Watch { .. } => "watch",
            JobKind::Project { .. } => "project",
        }
    }

    /// What the interface calls this kind of job: "Research".
    pub fn label(&self) -> &'static str {
        match self {
            JobKind::Research { .. } => "Research",
            JobKind::Task { .. } => "Task",
            JobKind::Watch { .. } => "Watch",
            JobKind::Project { .. } => "Project",
        }
    }

    /// One line saying what this job is about, for a card, a list row and
    /// a notification: a research job's question, a task's title.
    pub fn title(&self) -> String {
        match self {
            JobKind::Research { question, .. } => question.trim().to_string(),
            JobKind::Task { title, .. } => title.trim().to_string(),
            JobKind::Watch { what, .. } => what.trim().to_string(),
            JobKind::Project { goal, .. } => goal.trim().to_string(),
        }
    }

    /// The task this job is doing, if it is one of the assistant's tasks.
    pub fn task_id(&self) -> Option<TaskId> {
        match self {
            JobKind::Task { task_id, .. } => Some(*task_id),
            _ => None,
        }
    }

    /// The watch's probe, if this is a watch.
    pub fn probe(&self) -> Option<&Probe> {
        match self {
            JobKind::Watch { probe, .. } => Some(probe),
            _ => None,
        }
    }

    /// Whether this kind runs in steps, sleeping as [`JobStatus::Waiting`]
    /// in between -- a watch -- rather than as one long turn.
    pub fn is_stepped(&self) -> bool {
        matches!(self, JobKind::Watch { .. })
    }

    /// Whether this kind has [`JobRules`] the person may change while it runs.
    pub fn has_rules(&self) -> bool {
        matches!(self, JobKind::Watch { .. })
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
    /// Between steps: a watch asleep until [`Job::wake_at`]. Nothing claims
    /// it while it sleeps, so the sweep leaves it alone.
    Waiting,
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
    pub const ALL: [JobStatus; 6] = [
        JobStatus::Queued,
        JobStatus::Running,
        JobStatus::Waiting,
        JobStatus::Done,
        JobStatus::Failed,
        JobStatus::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Waiting => "waiting",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    /// Whether this job is over, one way or another.
    pub fn is_finished(self) -> bool {
        !matches!(self, JobStatus::Queued | JobStatus::Running | JobStatus::Waiting)
    }

    /// Whether this job is still going: queued, running, or asleep between
    /// steps. The opposite of [`JobStatus::is_finished`].
    pub fn is_live(self) -> bool {
        !self.is_finished()
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
    /// How it behaves, for a kind that has rules: when to tell them, how
    /// often to look, when to stop. Empty for every other kind.
    #[serde(default, skip_serializing_if = "JobRules::is_empty")]
    pub rules: JobRules,
    /// When a job asleep between steps ([`JobStatus::Waiting`]) next wakes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<Timestamp>,
    /// What a watch saw last. `None` for every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Observed>,
    /// What it has told the person while it ran, oldest first, the last
    /// [`MAX_UPDATES_KEPT`] of them -- and what it still has to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updates: Vec<JobUpdate>,
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
            rules: JobRules::default(),
            wake_at: None,
            observed: None,
            updates: Vec::new(),
        }
    }

    /// A watch, asleep until its first look, which is due at once. `rules`
    /// as the person gave them; an unset end becomes
    /// [`DEFAULT_WATCH_HOURS`] from `now`.
    pub fn watch(
        what: impl Into<String>,
        probe: Probe,
        mut rules: JobRules,
        asked_in: Option<ConversationId>,
        now: Timestamp,
    ) -> Self {
        if rules.until.is_none() {
            rules.until = Some(now + jiff::SignedDuration::from_hours(DEFAULT_WATCH_HOURS));
        }
        let mut job = Self::new(JobKind::Watch { what: what.into(), probe }, asked_in);
        job.created_at = now;
        job.status = JobStatus::Waiting;
        job.started_at = Some(now);
        job.wake_at = Some(now);
        job.rules = rules;
        job.observed = Some(Observed::default());
        job
    }

    /// A project, queued.
    pub fn project(
        goal: impl Into<String>,
        brief: impl Into<String>,
        asked_in: Option<ConversationId>,
    ) -> Self {
        Self::new(JobKind::Project { goal: goal.into(), brief: brief.into() }, asked_in)
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
            JobKind::Task { title, .. } => {
                if title.trim().is_empty() {
                    return Err(Error::Invalid("a task job needs the task's title".into()));
                }
            }
            JobKind::Watch { what, probe } => {
                if what.trim().is_empty() {
                    return Err(Error::Invalid(
                        "a watch needs to say what it watches, in a few words".into(),
                    ));
                }
                if what.len() > MAX_WHAT_BYTES {
                    return Err(Error::Invalid(format!(
                        "what is watched is {} bytes; the limit is {MAX_WHAT_BYTES}. Put the \
                         detail in when to tell them",
                        what.len()
                    )));
                }
                match probe {
                    Probe::Connection { connection, tool, arguments } => {
                        if connection.trim().is_empty() || tool.trim().is_empty() {
                            return Err(Error::Invalid(
                                "a watch on a connection needs the connection's name and the \
                                 tool's"
                                    .into(),
                            ));
                        }
                        if !arguments.is_object() {
                            return Err(Error::Invalid(
                                "a watch's tool arguments must be an object, {} for none".into(),
                            ));
                        }
                    }
                    Probe::Assistant { how } => {
                        if how.trim().is_empty() {
                            return Err(Error::Invalid(
                                "a watch with no fixed check needs to say how to look".into(),
                            ));
                        }
                        if how.len() > MAX_RULE_BYTES {
                            return Err(Error::Invalid(format!(
                                "how to look is {} bytes; the limit is {MAX_RULE_BYTES}",
                                how.len()
                            )));
                        }
                    }
                }
                self.rules.validate_since(Some(probe), self.created_at)?;
            }
            JobKind::Project { goal, brief } => {
                if goal.trim().is_empty() {
                    return Err(Error::Invalid(
                        "a project needs a goal: what should it get done?".into(),
                    ));
                }
                if goal.len() > MAX_GOAL_BYTES {
                    return Err(Error::Invalid(format!(
                        "the goal is {} bytes; the limit is {MAX_GOAL_BYTES}. Put the context \
                         in the brief",
                        goal.len()
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
    ///
    /// Returns `true` when the job is back in the queue. `false` is anything
    /// else: it was failed -- or it runs in steps, and was put back to sleep,
    /// due now, which is not the queue and never counts against it. A caller
    /// that needs to know which reads `status`.
    pub fn interrupted(&mut self, why: &str) -> bool {
        // A job that runs in steps lost one step, not the job: it goes back to
        // sleep and looks again at once.
        if self.kind.is_stepped() {
            self.status = JobStatus::Waiting;
            self.wake_at = Some(Timestamp::now());
            self.progress = String::new();
            return false;
        }
        if self.attempts >= MAX_ATTEMPTS {
            self.fail(format!("it was interrupted {} times ({why})", self.attempts));
            false
        } else {
            self.status = JobStatus::Queued;
            self.progress = String::new();
            true
        }
    }

    /// Put a job that runs in steps to sleep until `at`.
    pub fn sleep_until(&mut self, at: Timestamp) {
        self.status = JobStatus::Waiting;
        self.wake_at = Some(at);
        self.progress = String::new();
    }

    /// Whether a sleeping job's next step is due at `now`.
    pub fn is_due(&self, now: Timestamp) -> bool {
        self.status == JobStatus::Waiting && self.wake_at.is_none_or(|at| at <= now)
    }

    /// Whether a job with an end has reached it at `now`.
    pub fn has_expired(&self, now: Timestamp) -> bool {
        self.rules.until.is_some_and(|until| until <= now)
    }

    /// How long to sleep between steps, by the job's rules and its probe.
    pub fn every(&self) -> std::time::Duration {
        self.rules.every(self.kind.probe())
    }

    /// Record something told, or to be told, keeping the last
    /// [`MAX_UPDATES_KEPT`]. The text is cut to [`MAX_UPDATE_BYTES`], marked
    /// with an ellipsis that is part of that, on a character boundary.
    ///
    /// What is dropped to make room is the oldest that has been sent. One not
    /// sent yet is something the person has not been told, and is kept past
    /// the limit -- up to [`MAX_UPDATES_HELD`], past which the oldest goes
    /// whether it was sent or not, so that a job nobody is sending for cannot
    /// grow without end.
    pub fn add_update(&mut self, text: impl Into<String>, at: Timestamp, delivered: bool) {
        const ELLIPSIS: char = '\u{2026}';
        let mut text: String = text.into().trim().to_string();
        if text.len() > MAX_UPDATE_BYTES {
            let mut cut = MAX_UPDATE_BYTES - ELLIPSIS.len_utf8();
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push(ELLIPSIS);
        }
        self.updates.push(JobUpdate { at, text, delivered, sent_to: Vec::new() });
        while self.updates.len() > MAX_UPDATES_KEPT {
            if let Some(sent) = self.updates.iter().position(|u| u.delivered) {
                self.updates.remove(sent);
            } else if self.updates.len() > MAX_UPDATES_HELD {
                self.updates.remove(0);
            } else {
                break;
            }
        }
    }

    /// The updates still to be sent on, oldest first.
    pub fn undelivered(&self) -> impl Iterator<Item = &JobUpdate> {
        self.updates.iter().filter(|u| !u.delivered)
    }

    /// The last update that went out, if any.
    pub fn last_update(&self) -> Option<&JobUpdate> {
        self.updates.iter().rev().find(|u| u.delivered)
    }

    /// Whether the conversation that asked is still owed a follow-up.
    ///
    /// A watch never is: what it had to say it said as it went, its last
    /// update included -- see `karmayoga_service::jobs::watch`.
    pub fn owes_answer(&self) -> bool {
        self.status == JobStatus::Done
            && !self.answered
            && self.asked_in.is_some()
            && !self.kind.is_stepped()
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
    fn a_job_is_over_once_it_is_neither_queued_nor_running_nor_asleep() {
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
        // Nor had it any of what steps and updates added.
        assert!(job.rules.is_empty() && job.wake_at.is_none());
        assert!(job.observed.is_none() && job.updates.is_empty());
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_refused_rather_than_misread() {
        let future = json!({ "type": "browse", "goal": "book the table" });
        assert!(serde_json::from_value::<JobKind>(future).is_err());
        assert!(serde_json::from_value::<JobStatus>(json!("sleeping")).is_err());
        assert!(serde_json::from_value::<Probe>(json!({ "type": "telepathy" })).is_err());
    }

    // ---- watches: what one is -----------------------------------------------

    /// An arbitrary moment, `second` seconds in. Fixed so that "eight hours
    /// from now" is arithmetic rather than a race with the clock.
    fn t(second: i64) -> Timestamp {
        Timestamp::from_second(1_790_000_000 + second).unwrap()
    }

    fn ci() -> Probe {
        Probe::Connection {
            connection: "Work GitHub".into(),
            tool: "get_pull_request".into(),
            arguments: json!({ "number": 412 }),
        }
    }

    fn look() -> Probe {
        Probe::Assistant { how: "Check my inbox for a reply from Sam about the lease".into() }
    }

    fn watch() -> Job {
        Job::watch("CI on pull request 412", ci(), JobRules::default(), None, t(0))
    }

    fn watch_of(what: &str, probe: Probe, rules: JobRules) -> Job {
        Job::watch(what, probe, rules, None, t(0))
    }

    fn eight_hours() -> jiff::SignedDuration {
        jiff::SignedDuration::from_hours(DEFAULT_WATCH_HOURS)
    }

    #[test]
    fn a_watch_starts_asleep_with_its_first_look_due_at_once() {
        let asked_in = ConversationId::new();
        let now = t(0);
        let job =
            Job::watch("CI on pull request 412", ci(), JobRules::default(), Some(asked_in), now);

        assert_eq!(job.status, JobStatus::Waiting);
        assert_eq!(job.asked_in, Some(asked_in));
        assert_eq!(job.wake_at, Some(now));
        assert!(job.is_due(now), "nothing has been seen yet, so look straight away");
        assert_eq!(job.created_at, now);
        assert_eq!(job.started_at, Some(now));
        assert!(job.finished_at.is_none());
        assert_eq!(job.attempts, 0, "a watch is not a run that has been started");
        assert_eq!(job.observed, Some(Observed::default()), "nothing seen, but a place for it");
        assert!(job.updates.is_empty());
        assert!(job.outputs.is_empty() && job.conversation_id.is_none());
        assert!(job.validate().is_ok());

        assert_eq!(job.kind.as_str(), "watch");
        assert_eq!(job.kind.label(), "Watch");
        assert_eq!(job.title(), "CI on pull request 412");
        assert_eq!(job.kind.probe(), Some(&ci()));
        assert!(job.kind.is_stepped() && job.kind.has_rules());
    }

    #[test]
    fn a_watch_with_no_end_given_stops_after_eight_hours() {
        assert_eq!(DEFAULT_WATCH_HOURS, 8, "Hari's call, 20 November");
        let job = watch();
        assert_eq!(job.rules.until, Some(t(0) + eight_hours()));

        assert!(!job.has_expired(t(0)));
        assert!(!job.has_expired(t(8 * 3600 - 1)));
        assert!(job.has_expired(t(8 * 3600)), "the moment it names is the end");
        assert!(job.has_expired(t(9 * 3600)));
    }

    #[test]
    fn an_end_the_person_gave_is_kept_and_a_job_with_none_never_expires() {
        let until = t(2 * 3600);
        let rules = JobRules { until: Some(until), ..Default::default() };
        let job = watch_of("CI", ci(), rules);
        assert_eq!(job.rules.until, Some(until), "not replaced by the default");
        assert!(job.has_expired(until));

        // Research and a task have no rules, and so no end.
        assert!(!research().has_expired(t(1_000_000_000)));
        assert!(research().rules.is_empty());
    }

    #[test]
    fn a_job_asleep_between_looks_is_still_going() {
        assert!(JobStatus::Waiting.is_live());
        assert!(!JobStatus::Waiting.is_finished());
        for status in JobStatus::ALL {
            assert_eq!(status.is_live(), !status.is_finished(), "{status:?}");
        }
        let live: Vec<JobStatus> = JobStatus::ALL.into_iter().filter(|s| s.is_live()).collect();
        assert_eq!(live, [JobStatus::Queued, JobStatus::Running, JobStatus::Waiting]);
        assert_eq!(serde_json::to_value(JobStatus::Waiting).unwrap(), json!("waiting"));
    }

    #[test]
    fn only_a_sleeping_job_whose_time_has_come_is_due() {
        let mut job = watch();
        job.progress = "Looking now".into();
        job.sleep_until(t(600));
        assert_eq!(job.status, JobStatus::Waiting);
        assert_eq!(job.wake_at, Some(t(600)));
        assert!(job.progress.is_empty(), "the line described a look that is over");
        assert!(!job.is_due(t(599)));
        assert!(job.is_due(t(600)), "at the time itself");
        assert!(job.is_due(t(601)));

        // A job that is not asleep is never due, whatever its wake time says.
        let mut running = watch();
        running.status = JobStatus::Running;
        assert!(!running.is_due(t(1_000_000)));
        let mut cancelled = watch();
        cancelled.cancel("no longer needed").unwrap();
        assert!(!cancelled.is_due(t(1_000_000)));

        // Asleep with no time set at all is due: nothing says to wait.
        let mut unset = watch();
        unset.wake_at = None;
        assert!(unset.is_due(t(0)));
    }

    #[test]
    fn an_interrupted_watch_goes_back_to_sleep_due_now_and_is_never_failed_for_it() {
        let mut job = watch();
        job.attempts = MAX_ATTEMPTS + 5;
        job.progress = "Looking now".into();
        job.wake_at = Some(t(86_400));

        let before = Timestamp::now();
        assert!(
            !job.interrupted("the app was closed"),
            "false: not back in the queue, which is for one long turn"
        );
        assert_eq!(job.status, JobStatus::Waiting, "but asleep, which is where it looks again");
        assert!(job.progress.is_empty());
        assert!(job.reason.is_empty() && job.finished_at.is_none(), "nothing went wrong with it");
        assert!(job.wake_at.unwrap() >= before, "due now, not at the time it had");
        assert!(job.is_due(Timestamp::now()));

        // However often it happens: an interruption lost one look, not the job.
        for _ in 0..10 {
            assert!(!job.interrupted("a restart"));
            assert_eq!(job.status, JobStatus::Waiting);
        }
    }

    #[test]
    fn a_watch_asleep_can_be_cancelled_and_one_that_has_stopped_cannot() {
        let mut job = watch();
        job.cancel("they no longer need it").unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert_eq!(job.reason, "they no longer need it");
        assert!(job.finished_at.is_some());
        assert!(!job.status.is_live());

        let mut done = watch();
        done.finish("I've stopped watching CI: the 8 hours are up.");
        let before = done.clone();
        assert!(invalid(done.cancel("late")).contains("done"));
        assert_eq!(done, before);
    }

    // ---- watches: what may be saved ----------------------------------------

    #[test]
    fn a_watch_needs_to_say_what_it_watches_in_a_few_words() {
        for blank in ["", "   ", "\n\t"] {
            let job = watch_of(blank, ci(), JobRules::default());
            assert!(invalid(job.validate()).contains("what it watches"), "{blank:?}");
        }
        let at_the_limit = watch_of(&"w".repeat(MAX_WHAT_BYTES), ci(), JobRules::default());
        assert!(at_the_limit.validate().is_ok(), "the limit itself is allowed");

        let over = watch_of(&"w".repeat(MAX_WHAT_BYTES + 1), ci(), JobRules::default());
        let why = invalid(over.validate());
        assert!(why.contains(&MAX_WHAT_BYTES.to_string()), "should state the limit: {why}");
        assert!(why.contains("when to tell them"), "should say where the detail goes: {why}");
    }

    #[test]
    fn a_watch_on_a_connection_needs_the_connection_the_tool_and_an_object_of_arguments() {
        let call = |connection: &str, tool: &str, arguments: serde_json::Value| {
            let probe =
                Probe::Connection { connection: connection.into(), tool: tool.into(), arguments };
            watch_of("CI", probe, JobRules::default()).validate()
        };
        assert!(call("Work GitHub", "get_pull_request", json!({})).is_ok(), "{{}} is no arguments");
        assert!(call("Work GitHub", "get_pull_request", json!({ "n": 1 })).is_ok());

        let blanks = [("", "get_pull_request"), ("  ", "x"), ("Work GitHub", ""), ("", "")];
        for (connection, tool) in blanks {
            let why = invalid(call(connection, tool, json!({})));
            assert!(why.contains("connection") && why.contains("tool"), "{why}");
        }
        for arguments in [json!([1, 2]), json!("number=412"), json!(null), json!(412)] {
            let why = invalid(call("Work GitHub", "get_pull_request", arguments.clone()));
            assert!(why.contains("must be an object"), "{arguments}: {why}");
        }
    }

    #[test]
    fn a_watch_that_looks_for_itself_needs_to_say_how_and_not_at_length() {
        let how = |how: &str| {
            watch_of("The lease", Probe::Assistant { how: how.into() }, JobRules::default())
                .validate()
        };
        for blank in ["", "  ", "\n"] {
            assert!(invalid(how(blank)).contains("how to look"), "{blank:?}");
        }
        assert!(how("Check my inbox for a reply from Sam").is_ok());
        assert!(how(&"h".repeat(MAX_RULE_BYTES)).is_ok(), "the limit itself is allowed");
        let why = invalid(how(&"h".repeat(MAX_RULE_BYTES + 1)));
        assert!(why.contains(&MAX_RULE_BYTES.to_string()), "{why}");
    }

    #[test]
    fn the_rules_a_person_gives_are_held_to_length_and_to_how_often_a_look_may_be() {
        let with = |rules: JobRules, probe: Probe| watch_of("CI", probe, rules).validate();

        // What to tell them and what to do are capped alike.
        let long = "x".repeat(MAX_RULE_BYTES + 1);
        let why = invalid(with(JobRules { notify_when: long.clone(), ..Default::default() }, ci()));
        assert!(why.contains("when to tell them") && why.contains("1000"), "{why}");
        let why = invalid(with(JobRules { act: long, ..Default::default() }, ci()));
        assert!(why.contains("what to do") && why.contains("1000"), "{why}");
        let at_the_limit = "x".repeat(MAX_RULE_BYTES);
        let rules =
            JobRules { notify_when: at_the_limit.clone(), act: at_the_limit, ..Default::default() };
        assert!(with(rules, ci()).is_ok());

        // How often: zero is "the default", and a fixed check may look every minute.
        for minutes in [0, 1, 5, MAX_EVERY_MINUTES] {
            let rules = JobRules { every_minutes: minutes, ..Default::default() };
            assert!(with(rules, ci()).is_ok(), "{minutes}");
        }
        let too_often = JobRules { every_minutes: MAX_EVERY_MINUTES + 1, ..Default::default() };
        let why = invalid(with(too_often, ci()));
        assert!(why.contains(&(MAX_EVERY_MINUTES + 1).to_string()), "{why}");

        // A look that is a model turn is no more often than every quarter hour.
        for minutes in [1, 14] {
            let rules = JobRules { every_minutes: minutes, ..Default::default() };
            let why = invalid(with(rules, look()));
            assert!(why.contains("15") && why.contains(&format!("every {minutes}")), "{why}");
        }
        for minutes in [0, MIN_ASSISTANT_EVERY_MINUTES, 60] {
            let rules = JobRules { every_minutes: minutes, ..Default::default() };
            assert!(with(rules, look()).is_ok(), "{minutes}");
        }

        // With no probe to hold it to, the fixed check's bounds apply.
        let rules = JobRules { every_minutes: 1, ..Default::default() };
        assert!(rules.validate(None).is_ok());
    }

    #[test]
    fn a_watch_may_not_end_before_it_begins_or_more_than_a_week_after_now() {
        assert_eq!(MAX_WATCH_HOURS, 168);
        let now = Timestamp::now();
        let hours = jiff::SignedDuration::from_hours;
        let ending = |until: Timestamp| {
            let rules = JobRules { until: Some(until), ..Default::default() };
            Job::watch("CI", ci(), rules, None, now)
        };

        assert!(ending(now + hours(1)).validate().is_ok());
        assert!(ending(now + hours(MAX_WATCH_HOURS)).validate().is_ok(), "a week exactly");
        let why = invalid(ending(now + hours(MAX_WATCH_HOURS + 1)).validate());
        assert!(why.contains("168 hours"), "{why}");

        let why = invalid(ending(now - hours(2)).validate());
        assert!(why.contains("past"), "{why}");
        let a_moment_ago = now - jiff::SignedDuration::from_secs(END_GRACE_SECS / 2);
        assert!(ending(a_moment_ago).validate().is_ok(), "a clock a little out is not refused");

        // The same, asked of the rules alone: measured from now.
        let too_far = now + hours(MAX_WATCH_HOURS + 1);
        let late = JobRules { until: Some(too_far), ..Default::default() };
        assert!(late.validate(Some(&ci())).is_err());
        let past = JobRules { until: Some(now - hours(2)), ..Default::default() };
        assert!(past.validate(None).is_err());
        assert!(JobRules::default().validate(None).is_ok(), "no end is for the job to fill in");
    }

    #[test]
    fn a_watch_that_has_run_its_time_can_still_be_saved_and_one_can_be_carried_on_for_a_week() {
        // Made nine hours ago, with the default eight: the watch loop saves it
        // as it ends, its last look and its last update with it.
        let hours = jiff::SignedDuration::from_hours;
        let nine_hours_ago = Timestamp::now() - hours(9);
        let mut over = Job::watch("CI", ci(), JobRules::default(), None, nine_hours_ago);
        assert!(over.has_expired(Timestamp::now()));
        assert!(over.validate().is_ok(), "an end that is behind us is not a reason to refuse it");
        over.finish("I've stopped watching CI: the 8 hours are up.");
        assert!(over.validate().is_ok());

        // Made a week ago, and then told to carry on for a week from now.
        let a_week_ago = Timestamp::now() - hours(MAX_WATCH_HOURS);
        let mut old = Job::watch("CI", ci(), JobRules::default(), None, a_week_ago);
        old.rules.until = Some(Timestamp::now() + hours(MAX_WATCH_HOURS));
        assert!(old.validate().is_ok());
    }

    #[test]
    fn a_watch_is_checked_against_the_rules_of_its_own_probe() {
        // `Job::validate` reaches the rules through the watch's own probe, so
        // the same number is fine for a fixed check and not for a model turn.
        let rules = JobRules { every_minutes: 3, ..Default::default() };
        assert!(watch_of("CI", ci(), rules.clone()).validate().is_ok());
        let why = invalid(watch_of("The lease", look(), rules).validate());
        assert!(why.contains("every 15"), "{why}");
    }

    #[test]
    fn how_often_a_watch_looks_has_a_default_and_is_held_to_what_its_probe_allows() {
        let minutes = |every_minutes: u32, probe: Option<&Probe>| {
            JobRules { every_minutes, ..Default::default() }.every(probe).as_secs() / 60
        };
        assert_eq!(DEFAULT_EVERY_MINUTES, 5);
        assert_eq!(minutes(0, None), 5, "unsaid is the default");
        assert_eq!(minutes(0, Some(&ci())), 5);
        assert_eq!(minutes(1, Some(&ci())), 1, "a fixed check may look every minute");
        assert_eq!(minutes(30, Some(&ci())), 30);
        let a_day = u64::from(MAX_EVERY_MINUTES);
        assert_eq!(minutes(u32::MAX, Some(&ci())), a_day, "and no less often than that");

        // A model turn at every look is held to a quarter of an hour, the
        // default included.
        assert_eq!(minutes(0, Some(&look())), 15);
        assert_eq!(minutes(3, Some(&look())), 15);
        assert_eq!(minutes(15, Some(&look())), 15);
        assert_eq!(minutes(45, Some(&look())), 45);

        // And a job asks through its own probe.
        assert_eq!(watch().every(), std::time::Duration::from_secs(300));
        let slow = watch_of("The lease", look(), JobRules::default());
        assert_eq!(slow.every(), std::time::Duration::from_secs(15 * 60));
        assert_eq!(research().every(), std::time::Duration::from_secs(300), "defined, never read");
    }

    #[test]
    fn a_probe_names_its_kind_its_minimum_and_how_it_looks() {
        assert_eq!(ci().as_str(), "connection");
        assert_eq!(look().as_str(), "assistant");
        assert_eq!(ci().min_every_minutes(), MIN_EVERY_MINUTES);
        assert_eq!(look().min_every_minutes(), MIN_ASSISTANT_EVERY_MINUTES);
        assert_eq!(ci().in_words(), "calls get_pull_request on Work GitHub");
        assert_eq!(
            look().in_words(),
            "asks the assistant: Check my inbox for a reply from Sam about the lease"
        );
    }

    #[test]
    fn rules_are_put_into_words_the_person_can_read_and_the_assistant_can_say_back() {
        let utc = jiff::tz::TimeZone::UTC;
        let until = jiff::civil::date(2026, 11, 20).at(18, 40, 0, 0).in_tz("UTC").unwrap();
        let until = until.timestamp();

        let full = JobRules {
            notify_when: "when it finishes or fails".into(),
            act: "read the failing step's log and say why".into(),
            every_minutes: 2,
            until: Some(until),
            call_me: true,
        };
        assert_eq!(
            full.in_words(Some(&ci()), &utc),
            "tells them: when it finishes or fails; also does this when it has something to \
             tell: read the failing step's log and say why; looks every 2 minutes; stops at \
             2026-11-20 18:40; rings them with it when they are not already talking to you"
        );

        let plain = JobRules { until: Some(until), ..Default::default() };
        assert_eq!(
            plain.in_words(Some(&ci()), &utc),
            "tells them about any change worth knowing; looks every 5 minutes; stops at \
             2026-11-20 18:40"
        );
        assert!(
            plain.in_words(Some(&look()), &utc).contains("looks every 15 minutes"),
            "the default is held up to what a model turn may be"
        );

        // The end is in the person's own zone.
        let pacific = jiff::tz::TimeZone::fixed(jiff::tz::offset(-8));
        assert!(plain.in_words(Some(&ci()), &pacific).ends_with("stops at 2026-11-20 10:40"));

        // No end, nothing about one.
        assert!(!JobRules::default().in_words(None, &utc).contains("stops"));

        let every = |every_minutes: u32| {
            JobRules { every_minutes, ..Default::default() }.in_words(Some(&ci()), &utc)
        };
        assert!(every(1).contains("looks every minute"));
        assert!(every(60).contains("looks every hour"));
        assert!(every(90).contains("looks every 90 minutes"));
        assert!(every(120).contains("looks every 2 hours"));
    }

    #[test]
    fn a_job_has_rules_only_when_something_was_set() {
        assert!(JobRules::default().is_empty());
        assert!(research().rules.is_empty());
        for rules in [
            JobRules { notify_when: "x".into(), ..Default::default() },
            JobRules { act: "x".into(), ..Default::default() },
            JobRules { every_minutes: 1, ..Default::default() },
            JobRules { until: Some(t(0)), ..Default::default() },
            JobRules { call_me: true, ..Default::default() },
        ] {
            assert!(!rules.is_empty(), "{rules:?}");
        }
        assert!(!watch().rules.is_empty(), "a watch always has an end");
    }

    // ---- updates -----------------------------------------------------------

    #[test]
    fn only_the_last_thirty_updates_are_kept() {
        assert_eq!(MAX_UPDATES_KEPT, 30);
        let mut job = watch();
        for i in 0..MAX_UPDATES_KEPT + 5 {
            job.add_update(format!("update {i}"), t(i as i64), true);
        }
        assert_eq!(job.updates.len(), MAX_UPDATES_KEPT);
        assert_eq!(job.updates[0].text, "update 5", "the oldest went first");
        assert_eq!(job.updates[0].at, t(5));
        assert_eq!(job.updates.last().unwrap().text, format!("update {}", MAX_UPDATES_KEPT + 4));
        let order: Vec<Timestamp> = job.updates.iter().map(|u| u.at).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "oldest first, newest last");
    }

    #[test]
    fn an_update_is_trimmed_and_cut_to_length_on_a_character_boundary() {
        let mut job = watch();
        job.add_update("  CI passed.\n", t(0), true);
        assert_eq!(job.updates[0].text, "CI passed.");
        assert!(job.updates[0].sent_to.is_empty(), "nowhere yet");

        let exact = "u".repeat(MAX_UPDATE_BYTES);
        job.add_update(exact.clone(), t(1), true);
        assert_eq!(job.updates[1].text, exact, "an update that fits is not marked as cut");

        // Three bytes to a character, and 4000 is not a multiple of three, so
        // a cut by bytes would land inside one.
        job.add_update("\u{0b95}".repeat(MAX_UPDATE_BYTES / 3 + 10), t(2), true);
        let text = &job.updates[2].text;
        assert!(text.ends_with('\u{2026}'), "a cut is marked");
        assert!(text.len() <= MAX_UPDATE_BYTES, "the mark is part of the limit: {}", text.len());
        let kept = text.trim_end_matches('\u{2026}');
        assert!(kept.chars().all(|c| c == '\u{0b95}'), "no character was split");
        assert!(kept.len() > MAX_UPDATE_BYTES - 6, "and little was lost: {}", kept.len());

        // A plain one fills the limit to the byte, mark included.
        job.add_update("u".repeat(MAX_UPDATE_BYTES + 100), t(3), true);
        let text = &job.updates[3].text;
        assert_eq!(text.len(), MAX_UPDATE_BYTES);
        assert!(text.ends_with('\u{2026}'));
    }

    #[test]
    fn an_update_not_yet_sent_is_never_dropped_to_make_room_for_another() {
        let mut job = watch();
        // Five the person has not been told, then far more than the limit that
        // were sent.
        for i in 0..5 {
            job.add_update(format!("pending {i}"), t(i), false);
        }
        for i in 0..MAX_UPDATES_KEPT + 20 {
            job.add_update(format!("sent {i}"), t(100 + i as i64), true);
        }
        assert_eq!(job.updates.len(), MAX_UPDATES_KEPT);
        let pending: Vec<&str> = job.undelivered().map(|u| u.text.as_str()).collect();
        assert_eq!(pending, ["pending 0", "pending 1", "pending 2", "pending 3", "pending 4"]);
        assert_eq!(job.updates[0].text, "pending 0", "they are the oldest, and still here");
        assert_eq!(
            job.updates.last().unwrap().text,
            format!("sent {}", MAX_UPDATES_KEPT + 19),
            "and the oldest of those sent went instead"
        );

        // More unsent than the limit are held, since each is unsaid.
        let mut backlog = watch();
        for i in 0..MAX_UPDATES_KEPT + 10 {
            backlog.add_update(format!("pending {i}"), t(i as i64), false);
        }
        assert_eq!(backlog.updates.len(), MAX_UPDATES_KEPT + 10);
        assert_eq!(backlog.updates[0].text, "pending 0");

        // Until a ceiling, past which the oldest goes whether it was sent or not.
        for i in 0..MAX_UPDATES_KEPT {
            backlog.add_update(format!("later {i}"), t(1000 + i as i64), false);
        }
        assert_eq!(MAX_UPDATES_HELD, 2 * MAX_UPDATES_KEPT);
        assert_eq!(backlog.updates.len(), MAX_UPDATES_HELD);
        assert_eq!(backlog.updates[0].text, "pending 10", "the oldest ten went");
        let newest = format!("later {}", MAX_UPDATES_KEPT - 1);
        assert_eq!(backlog.updates.last().unwrap().text, newest);
    }

    #[test]
    fn what_is_still_to_be_sent_is_told_apart_from_what_was() {
        let mut job = watch();
        assert!(job.undelivered().next().is_none() && job.last_update().is_none());

        job.add_update("one", t(1), true);
        job.add_update("two", t(2), false);
        job.add_update("three", t(3), true);
        job.add_update("four", t(4), false);

        let waiting: Vec<&str> = job.undelivered().map(|u| u.text.as_str()).collect();
        assert_eq!(waiting, ["two", "four"], "oldest first");
        assert_eq!(
            job.last_update().map(|u| u.text.as_str()),
            Some("three"),
            "the last that went out, not the last written"
        );
    }

    #[test]
    fn a_watch_never_owes_an_answer_and_a_project_does() {
        let asked = Some(ConversationId::new());
        let mut watch = Job::watch("CI", ci(), JobRules::default(), asked, t(0));
        watch.finish("I've stopped watching CI: the 8 hours are up.");
        assert_eq!(watch.status, JobStatus::Done);
        assert!(!watch.answered && watch.asked_in.is_some());
        assert!(!watch.owes_answer(), "it said what it had to as it went");

        let mut project = Job::project("Plan a week in Lisbon", "", asked);
        assert!(!project.owes_answer(), "not until it is done");
        project.begin();
        project.finish("Planned.");
        assert!(project.owes_answer());
        project.answered = true;
        assert!(!project.owes_answer());
    }

    // ---- projects ----------------------------------------------------------

    #[test]
    fn a_project_is_queued_like_research_and_has_no_steps_or_rules() {
        let asked_in = ConversationId::new();
        let job = Job::project("Plan a week in Lisbon", "Late March, two adults.", Some(asked_in));
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.asked_in, Some(asked_in));
        assert_eq!(job.kind.as_str(), "project");
        assert_eq!(job.kind.label(), "Project");
        assert_eq!(job.title(), "Plan a week in Lisbon");
        assert!(job.rules.is_empty() && job.wake_at.is_none());
        assert!(job.observed.is_none() && job.updates.is_empty());
        assert!(!job.kind.is_stepped() && !job.kind.has_rules() && job.kind.probe().is_none());
        assert!(job.validate().is_ok());
    }

    #[test]
    fn a_project_needs_a_goal_that_is_a_goal_and_not_a_document() {
        for blank in ["", "   ", "\n\t"] {
            let job = Job::project(blank, "context", None);
            assert!(invalid(job.validate()).contains("goal"), "{blank:?}");
        }
        assert!(Job::project("g".repeat(MAX_GOAL_BYTES), "", None).validate().is_ok());

        let why = invalid(Job::project("g".repeat(MAX_GOAL_BYTES + 1), "", None).validate());
        assert!(why.contains(&MAX_GOAL_BYTES.to_string()), "should state the limit: {why}");
        assert!(why.contains("brief"), "should say where the context goes: {why}");
    }

    #[test]
    fn a_projects_brief_may_be_empty_but_not_enormous() {
        assert!(Job::project("Plan a trip", "", None).validate().is_ok());
        let at_the_limit = Job::project("Plan a trip", "b".repeat(MAX_BRIEF_BYTES), None);
        assert!(at_the_limit.validate().is_ok());
        let over = Job::project("Plan a trip", "b".repeat(MAX_BRIEF_BYTES + 1), None);
        let why = invalid(over.validate());
        assert!(why.contains("brief") && why.contains(&MAX_BRIEF_BYTES.to_string()), "{why}");
    }

    // ---- the new records on the wire ---------------------------------------

    #[test]
    fn a_probe_is_tagged_with_its_kind_and_a_call_without_arguments_reads_back_as_none() {
        assert_eq!(
            serde_json::to_value(ci()).unwrap(),
            json!({
                "type": "connection",
                "connection": "Work GitHub",
                "tool": "get_pull_request",
                "arguments": { "number": 412 },
            })
        );
        assert_eq!(
            serde_json::to_value(look()).unwrap(),
            json!({
                "type": "assistant",
                "how": "Check my inbox for a reply from Sam about the lease",
            })
        );

        let call: Probe = serde_json::from_value(
            json!({ "type": "connection", "connection": "Work GitHub", "tool": "list_runs" }),
        )
        .unwrap();
        assert_eq!(
            call,
            Probe::Connection {
                connection: "Work GitHub".into(),
                tool: "list_runs".into(),
                arguments: json!({}),
            },
            "a tool that takes none needs none written"
        );
    }

    #[test]
    fn a_watch_serialises_with_its_rules_probe_and_updates_in_camel_case() {
        let asked_in = ConversationId::new();
        let rules = JobRules {
            notify_when: "when it finishes or fails".into(),
            act: "read the failing step's log".into(),
            every_minutes: 2,
            until: Some(t(3600)),
            call_me: true,
        };
        let mut job = Job::watch("CI on pull request 412", ci(), rules, Some(asked_in), t(0));
        job.observed = Some(Observed {
            fingerprint: "abc123".into(),
            last: "status: running".into(),
            checked_at: Some(t(60)),
            checks: 3,
            failures: 1,
            trouble: "the server timed out".into(),
        });
        job.add_update("CI is running.", t(120), true);
        job.updates[0].sent_to = vec!["chat".into(), "thread".into()];
        job.add_update("It failed.", t(180), false);

        let wire = serde_json::to_value(&job).unwrap();
        assert_eq!(
            wire["kind"],
            json!({
                "type": "watch",
                "what": "CI on pull request 412",
                "probe": {
                    "type": "connection",
                    "connection": "Work GitHub",
                    "tool": "get_pull_request",
                    "arguments": { "number": 412 },
                },
            })
        );
        assert_eq!(wire["status"], "waiting");
        assert_eq!(wire["askedIn"], asked_in.to_string());
        assert_eq!(
            wire["rules"],
            json!({
                "notifyWhen": "when it finishes or fails",
                "act": "read the failing step's log",
                "everyMinutes": 2,
                "until": t(3600).to_string(),
                "callMe": true,
            })
        );
        assert_eq!(wire["wakeAt"], t(0).to_string());
        assert_eq!(
            wire["observed"],
            json!({
                "fingerprint": "abc123",
                "last": "status: running",
                "checkedAt": t(60).to_string(),
                "checks": 3,
                "failures": 1,
                "trouble": "the server timed out",
            })
        );
        assert_eq!(
            wire["updates"],
            json!([
                {
                    "at": t(120).to_string(),
                    "text": "CI is running.",
                    "delivered": true,
                    "sentTo": ["chat", "thread"],
                },
                { "at": t(180).to_string(), "text": "It failed.", "delivered": false },
            ])
        );
        for snake in ["wake_at", "notify_when"] {
            assert!(wire.get(snake).is_none(), "{snake} should be camelCase on the wire");
        }

        let back: Job = serde_json::from_value(wire).unwrap();
        assert_eq!(back, job);
    }

    #[test]
    fn every_state_a_watch_passes_through_survives_a_round_trip() {
        fn round_trips(job: &Job) {
            let back: Job = serde_json::from_value(serde_json::to_value(job).unwrap()).unwrap();
            assert_eq!(&back, job, "{:?}", job.status);
        }

        let mut job = Job::watch("The lease", look(), JobRules::default(), None, t(0));
        round_trips(&job);

        job.observed = Some(Observed { checks: 1, checked_at: Some(t(5)), ..Default::default() });
        job.sleep_until(t(905));
        round_trips(&job);

        job.add_update("Sam replied.", t(906), true);
        job.finish("Sam replied.");
        round_trips(&job);

        let mut cancelled = watch();
        cancelled.cancel("no longer needed").unwrap();
        round_trips(&cancelled);
    }

    #[test]
    fn a_project_survives_a_round_trip_and_is_written_without_rules() {
        let mut job = Job::project(
            "Plan a week in Lisbon",
            "Late March, two adults.",
            Some(ConversationId::new()),
        );
        job.begin();
        job.add_update("Flights found; now looking at places to stay.", t(60), false);

        let wire = serde_json::to_value(&job).unwrap();
        assert_eq!(
            wire["kind"],
            json!({
                "type": "project",
                "goal": "Plan a week in Lisbon",
                "brief": "Late March, two adults.",
            })
        );
        assert_eq!(wire["status"], "running");
        assert_eq!(wire["updates"][0]["delivered"], false);
        for absent in ["rules", "wakeAt", "observed"] {
            assert!(wire.get(absent).is_none(), "{absent} is nothing to a project: {wire}");
        }
        let back: Job = serde_json::from_value(wire).unwrap();
        assert_eq!(back, job);
    }

    #[test]
    fn research_and_a_task_are_written_exactly_as_they_were_before_steps_existed() {
        // An older build reads what this one writes for these kinds, so
        // nothing of the new fields may appear in them.
        let mut researched = research();
        researched.begin();
        researched.finish("Three bikes.");
        let kind = JobKind::Task { task_id: TaskId::new(), title: "Ring the vet".into() };
        let task = Job::new(kind, None);

        for job in [researched, task] {
            let wire = serde_json::to_value(&job).unwrap();
            for absent in ["rules", "wakeAt", "observed", "updates"] {
                let kind = job.kind.as_str();
                assert!(wire.get(absent).is_none(), "{absent} in a {kind} job: {wire}");
            }
        }
    }
}
