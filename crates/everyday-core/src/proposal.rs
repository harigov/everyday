//! Proposals: work the assistant prepared and did not do.
//!
//! A [`Proposal`] is a finished record -- a task, a planned block, a memory,
//! a routine, a note -- that the assistant built but did not save, or a mail
//! draft it wrote but did not send, or a deletion it wants. The person
//! accepts or declines it where the record would have been drawn. See
//! `docs/plans/dreaming.md` for the whole design; this module is its data.
//!
//! # Only unasked work drafts
//!
//! Work asked for in the rail, or written as a routine in the person's own
//! words, is done and audited. Work the assistant starts on its own -- a
//! dream -- produces proposals. That line is the reason this record exists
//! at all, and the next feature that wants to draft rather than do has to
//! argue with it.
//!
//! # A record, never a tool call
//!
//! [`Payload`] carries the domain record as it would be *after* saving, not
//! the tool name and arguments that built it. A tool's argument schema
//! changes whenever a prompt is tuned and promises nothing across versions;
//! the records are the one thing this vault already keeps readable, with
//! `#[serde(default)]` on every field added since a row was first sealed. A
//! proposal sealed by one build and accepted by the next therefore reads the
//! way any other sealed task would. Nothing in this module, or anywhere else,
//! executes a stored tool call.
//!
//! An update is a [`Payload::Replace`]: the whole after-image plus the
//! `updated_at` it was built against, so a record changed underneath it is
//! refused rather than overwritten. A patch would be a set of argument names
//! and would inherit the schema problem by another route.

use std::collections::BTreeSet;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::agent::Memory;
use crate::error::{Error, Result};
use crate::id::{ConversationId, DraftId, ProposalId, RoutineRunId};
use crate::note::Note;
use crate::routine::{DreamScope, Routine};
use crate::task::{Task, TimeBlock};

/// Longest a proposal's caption may be. A sentence.
pub const MAX_CAPTION_CHARS: usize = 400;

/// Longest the model's one line of "why" may be.
pub const MAX_WHY_CHARS: usize = 200;

/// How many proposals may be pending at once.
///
/// A person with forty unanswered proposals does not want a forty-first. The
/// vault refuses a new pending proposal past this, and a dream is told how
/// many are pending before it starts.
pub const MAX_PENDING_PROPOSALS: u64 = 40;

/// How many proposals one dream run may make.
pub const fn max_per_run(scope: DreamScope) -> usize {
    match scope {
        DreamScope::Day => 5,
        DreamScope::Week | DreamScope::Month => 8,
    }
}

/// What a proposal would make or change.
///
/// Also the unit [`ProposalPolicy`] switches on and off, so its spelling is
/// the settings' as well as the wire's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalKind {
    Task,
    Block,
    Memory,
    Routine,
    Note,
    Mail,
}

impl ProposalKind {
    pub const ALL: [ProposalKind; 6] = [
        ProposalKind::Task,
        ProposalKind::Block,
        ProposalKind::Memory,
        ProposalKind::Routine,
        ProposalKind::Note,
        ProposalKind::Mail,
    ];

    /// The clear-column spelling. Stable: it is stored.
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalKind::Task => "task",
            ProposalKind::Block => "block",
            ProposalKind::Memory => "memory",
            ProposalKind::Routine => "routine",
            ProposalKind::Note => "note",
            ProposalKind::Mail => "mail",
        }
    }

    pub fn parse(s: &str) -> Option<ProposalKind> {
        ProposalKind::ALL.iter().copied().find(|k| k.as_str() == s.trim().to_lowercase())
    }
}

/// A record a proposal carries, whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum ProposedRecord {
    Task(Task),
    Block(TimeBlock),
    Memory(Memory),
    Routine(Routine),
    Note(Note),
}

impl ProposedRecord {
    pub fn kind(&self) -> ProposalKind {
        match self {
            ProposedRecord::Task(_) => ProposalKind::Task,
            ProposedRecord::Block(_) => ProposalKind::Block,
            ProposedRecord::Memory(_) => ProposalKind::Memory,
            ProposedRecord::Routine(_) => ProposalKind::Routine,
            ProposedRecord::Note(_) => ProposalKind::Note,
        }
    }

    /// The record's own id, as a string.
    pub fn id(&self) -> String {
        match self {
            ProposedRecord::Task(r) => r.id.to_string(),
            ProposedRecord::Block(r) => r.id.to_string(),
            ProposedRecord::Memory(r) => r.id.to_string(),
            ProposedRecord::Routine(r) => r.id.to_string(),
            ProposedRecord::Note(r) => r.id.to_string(),
        }
    }

    pub fn updated_at(&self) -> Timestamp {
        match self {
            ProposedRecord::Task(r) => r.updated_at,
            ProposedRecord::Block(r) => r.updated_at,
            ProposedRecord::Memory(r) => r.updated_at,
            ProposedRecord::Routine(r) => r.updated_at,
            ProposedRecord::Note(r) => r.updated_at,
        }
    }

    /// The record's own checks. The references it carries -- a project, a
    /// task, a goal -- are the vault's to check at accept time, because they
    /// may have gone since the proposal was made.
    pub fn validate(&self) -> Result<()> {
        match self {
            ProposedRecord::Task(t) => {
                if t.title.trim().is_empty() {
                    return Err(Error::Invalid("a task needs a title".into()));
                }
                Ok(())
            }
            ProposedRecord::Block(b) => b.validate(),
            ProposedRecord::Memory(m) => m.validate(),
            ProposedRecord::Routine(r) => r.validate(),
            ProposedRecord::Note(n) => n.validate(),
        }
    }

    /// The day this would be drawn on, if it is drawn on a day at all.
    pub fn target_date(&self) -> Option<Date> {
        match self {
            ProposedRecord::Task(t) => t.due_date.or(t.start_date),
            ProposedRecord::Block(b) => Some(b.local_date),
            _ => None,
        }
    }
}

/// What accepting a proposal would do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Payload {
    /// Save a record that does not exist yet.
    Create { record: ProposedRecord },
    /// Overwrite a record with this after-image -- but only if it has not
    /// changed since `expected_updated_at`.
    Replace { record: ProposedRecord, expected_updated_at: Timestamp },
    /// Remove a record. `id` is the record's own id, as a string.
    Delete { kind: ProposalKind, id: String },
    /// Send a mail draft the assistant already wrote. The draft is its own
    /// record, with its own origin; this only points at it.
    SendMail { draft_id: DraftId },
}

impl Payload {
    pub fn kind(&self) -> ProposalKind {
        match self {
            Payload::Create { record } | Payload::Replace { record, .. } => record.kind(),
            Payload::Delete { kind, .. } => *kind,
            Payload::SendMail { .. } => ProposalKind::Mail,
        }
    }

    pub fn record(&self) -> Option<&ProposedRecord> {
        match self {
            Payload::Create { record } | Payload::Replace { record, .. } => Some(record),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Payload::Create { record } | Payload::Replace { record, .. } => record.validate(),
            Payload::Delete { kind, id } => {
                if *kind == ProposalKind::Mail {
                    return Err(Error::Invalid(
                        "a mail proposal sends a draft; it cannot delete one".into(),
                    ));
                }
                if id.trim().is_empty() {
                    return Err(Error::Invalid(
                        "a deletion needs the id of what it deletes".into(),
                    ));
                }
                Ok(())
            }
            Payload::SendMail { .. } => Ok(()),
        }
    }
}

/// What a proposal was reacting to, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AboutKind {
    Task,
    Block,
    Event,
    Note,
    Entry,
    Thread,
    Memory,
    Routine,
    Goal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct About {
    pub kind: AboutKind,
    pub id: String,
}

/// Who made a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProposalSource {
    /// A scheduled run -- today, always a dream.
    Run { run_id: RoutineRunId },
    /// A conversation, through the rail's "later" button.
    Conversation { conversation_id: ConversationId },
}

/// Why somebody said no. Optional, and one tap: forcing a reason is how a
/// signal stops being given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DeclineReason {
    NotNow,
    WrongTime,
    NeverThis,
    /// Also what a proposal that could no longer be applied is closed with,
    /// with the reason in `text`.
    Other {
        text: String,
    },
}

/// Where a proposal stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Outcome {
    Pending,
    /// Saved. `saved_as` is the id of the record it became; `edited` says the
    /// person changed it first, and the difference is recoverable from the
    /// proposal's record and the saved one.
    Accepted {
        at: Timestamp,
        saved_as: String,
        edited: bool,
    },
    Declined {
        at: Timestamp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<DeclineReason>,
    },
    /// Nobody answered in time. Not the same as a no, and counted apart.
    Expired {
        at: Timestamp,
    },
}

impl Outcome {
    pub fn state(&self) -> ProposalState {
        match self {
            Outcome::Pending => ProposalState::Pending,
            Outcome::Accepted { .. } => ProposalState::Accepted,
            Outcome::Declined { .. } => ProposalState::Declined,
            Outcome::Expired { .. } => ProposalState::Expired,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Outcome::Pending)
    }
}

/// [`Outcome`] without its payload: the clear column, and the query filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalState {
    Pending,
    Accepted,
    Declined,
    Expired,
}

impl ProposalState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalState::Pending => "pending",
            ProposalState::Accepted => "accepted",
            ProposalState::Declined => "declined",
            ProposalState::Expired => "expired",
        }
    }
}

/// Which kinds may be proposed. Everything, unless switched off.
///
/// Shaped like [`crate::quick::QuickPolicy`]: a set of exceptions, so a kind
/// added by a later build is on by default for a vault written earlier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProposalPolicy {
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub denied: BTreeSet<ProposalKind>,
}

impl ProposalPolicy {
    pub fn allows(&self, kind: ProposalKind) -> bool {
        !self.denied.contains(&kind)
    }
}

/// Work the assistant prepared and did not do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: ProposalId,
    /// Derived from `payload`, and kept so a list can be filtered without
    /// unsealing. [`Proposal::validate`] refuses a mismatch.
    pub kind: ProposalKind,
    pub payload: Payload,
    /// The sentence a confirmation card would show for the call that made
    /// it. Stored, so a proposal still reads after that tool is renamed.
    pub caption: String,
    /// One line from the model: why this is here.
    #[serde(default)]
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<About>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_by: Option<ProposalSource>,
    /// The day this is drawn on, if any. Derived from the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_date: Option<Date>,
    pub made_at: Timestamp,
    pub expires_at: Timestamp,
    pub outcome: Outcome,
    /// Whether anybody has looked at it. What the count on the app bar is.
    #[serde(default)]
    pub seen: bool,
    pub updated_at: Timestamp,
}

impl Proposal {
    /// A pending proposal, with its kind, day and expiry worked out.
    ///
    /// `tz` is the person's zone, for the end of a task's due day.
    pub fn new(payload: Payload, caption: impl Into<String>, now: Timestamp, tz: &str) -> Self {
        let kind = payload.kind();
        let target_date = payload.record().and_then(ProposedRecord::target_date);
        let expires_at = expiry_for(&payload, now, tz);
        Self {
            id: ProposalId::new(),
            kind,
            payload,
            caption: caption.into(),
            why: String::new(),
            about: None,
            made_by: None,
            target_date,
            made_at: now,
            expires_at,
            outcome: Outcome::Pending,
            seen: false,
            updated_at: now,
        }
    }

    pub fn with_why(mut self, why: impl Into<String>) -> Self {
        self.why = why.into();
        self
    }

    pub fn with_about(mut self, about: Option<About>) -> Self {
        self.about = about;
        self
    }

    pub fn made_by(mut self, source: ProposalSource) -> Self {
        self.made_by = Some(source);
        self
    }

    pub fn validate(&self) -> Result<()> {
        if self.caption.trim().is_empty() {
            return Err(Error::Invalid("a proposal needs a caption".into()));
        }
        if self.caption.chars().count() > MAX_CAPTION_CHARS {
            return Err(Error::Invalid(format!(
                "a proposal's caption must be under {MAX_CAPTION_CHARS} characters"
            )));
        }
        if self.why.chars().count() > MAX_WHY_CHARS {
            return Err(Error::Invalid(format!(
                "a proposal's reason must be under {MAX_WHY_CHARS} characters"
            )));
        }
        if self.kind != self.payload.kind() {
            return Err(Error::Invalid(format!(
                "a proposal says it is a {} but carries a {}",
                self.kind.as_str(),
                self.payload.kind().as_str()
            )));
        }
        self.payload.validate()
    }

    pub fn is_pending(&self) -> bool {
        self.outcome.is_pending()
    }

    /// Close this proposal. Stamps `updated_at`.
    pub fn close(&mut self, outcome: Outcome, now: Timestamp) {
        self.outcome = outcome;
        self.updated_at = now;
    }
}

/// When a proposal of this shape stops being worth answering.
///
/// | Kind | Expires |
/// |---|---|
/// | Task | the end of its due day, or seven days |
/// | Block | the block's end |
/// | Memory | thirty days |
/// | Routine | fourteen days |
/// | Note | seven days |
/// | Mail | thirty days; the draft's own fate closes it sooner |
/// | Delete | seven days |
pub fn expiry_for(payload: &Payload, now: Timestamp, tz: &str) -> Timestamp {
    let days = |n: i64| now + SignedDuration::from_hours(24 * n);
    match payload {
        Payload::Delete { .. } => days(7),
        Payload::SendMail { .. } => days(30),
        Payload::Create { record } | Payload::Replace { record, .. } => match record {
            ProposedRecord::Task(t) => match t.due_date {
                Some(due) => end_of_day(due, tz).filter(|end| *end > now).unwrap_or(days(1)),
                None => days(7),
            },
            ProposedRecord::Block(b) => {
                if b.end > now {
                    b.end
                } else {
                    // A block already over is not worth offering; one hour
                    // lets a sweep close it rather than a save refuse it.
                    now + SignedDuration::from_hours(1)
                }
            }
            ProposedRecord::Memory(_) => days(30),
            ProposedRecord::Routine(_) => days(14),
            ProposedRecord::Note(_) => days(7),
        },
    }
}

fn end_of_day(day: Date, tz: &str) -> Option<Timestamp> {
    let zone = jiff::tz::TimeZone::get(tz).ok()?;
    let next = day.tomorrow().ok()?;
    next.to_zoned(zone).ok().map(|z| z.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::BlockSubject;
    use jiff::civil::date;

    fn now() -> Timestamp {
        "2026-09-16T10:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_task_proposal_expires_at_the_end_of_its_due_day() {
        let mut t = Task::new("Book the dentist");
        t.due_date = Some(date(2026, 9, 18));
        let p = Proposal::new(
            Payload::Create { record: ProposedRecord::Task(t) },
            "Create task",
            now(),
            "UTC",
        );
        assert_eq!(p.kind, ProposalKind::Task);
        assert_eq!(p.target_date, Some(date(2026, 9, 18)));
        assert_eq!(p.expires_at, "2026-09-19T00:00:00Z".parse::<Timestamp>().unwrap());
        assert!(p.is_pending());
        p.validate().unwrap();
    }

    #[test]
    fn an_undated_task_proposal_lasts_a_week() {
        let p = Proposal::new(
            Payload::Create { record: ProposedRecord::Task(Task::new("Call Ravi")) },
            "Create task",
            now(),
            "UTC",
        );
        assert_eq!(p.expires_at, now() + SignedDuration::from_hours(24 * 7));
        assert_eq!(p.target_date, None);
    }

    #[test]
    fn a_block_proposal_expires_when_the_block_ends() {
        let start: Timestamp = "2026-09-17T09:00:00Z".parse().unwrap();
        let block = TimeBlock::new(BlockSubject::Adhoc, start, 60, "UTC");
        let end = block.end;
        let p = Proposal::new(
            Payload::Create { record: ProposedRecord::Block(block) },
            "Plan time",
            now(),
            "UTC",
        );
        assert_eq!(p.expires_at, end);
        assert_eq!(p.target_date, Some(date(2026, 9, 17)));
    }

    #[test]
    fn memory_routine_note_delete_and_mail_have_fixed_lifetimes() {
        let day = |n: i64| now() + SignedDuration::from_hours(24 * n);
        let cases = [
            (Payload::Create { record: ProposedRecord::Memory(Memory::new("x")) }, day(30)),
            (
                Payload::Create {
                    record: ProposedRecord::Routine(Routine::new(
                        "r",
                        "do",
                        crate::routine::Trigger::Manual,
                    )),
                },
                day(14),
            ),
            (Payload::Create { record: ProposedRecord::Note(Note::new("n")) }, day(7)),
            (Payload::Delete { kind: ProposalKind::Task, id: "x".into() }, day(7)),
            (Payload::SendMail { draft_id: DraftId::new() }, day(30)),
        ];
        for (payload, expected) in cases {
            assert_eq!(expiry_for(&payload, now(), "UTC"), expected, "{payload:?}");
        }
    }

    #[test]
    fn a_policy_is_everything_but_what_it_denies() {
        let mut policy = ProposalPolicy::default();
        assert!(ProposalKind::ALL.iter().all(|k| policy.allows(*k)));
        policy.denied.insert(ProposalKind::Mail);
        assert!(!policy.allows(ProposalKind::Mail));
        assert!(policy.allows(ProposalKind::Task));
    }

    #[test]
    fn validate_refuses_an_empty_caption_and_a_mismatched_kind() {
        let mut p = Proposal::new(
            Payload::Create { record: ProposedRecord::Task(Task::new("x")) },
            "  ",
            now(),
            "UTC",
        );
        assert!(p.validate().is_err());
        p.caption = "Create task".into();
        p.kind = ProposalKind::Note;
        assert!(p.validate().is_err());
    }

    #[test]
    fn a_proposal_round_trips_and_reads_without_its_optional_fields() {
        let p = Proposal::new(
            Payload::Replace {
                record: ProposedRecord::Task(Task::new("x")),
                expected_updated_at: now(),
            },
            "Update task",
            now(),
            "UTC",
        )
        .with_why("It has been rescheduled three times")
        .with_about(Some(About { kind: AboutKind::Task, id: "abc".into() }))
        .made_by(ProposalSource::Run { run_id: RoutineRunId::new() });
        let json = serde_json::to_value(&p).unwrap();
        let back: Proposal = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, p);

        // A proposal sealed by a build that had fewer fields still reads.
        let mut thin = json;
        let obj = thin.as_object_mut().unwrap();
        for k in ["why", "about", "madeBy", "targetDate", "seen"] {
            obj.remove(k);
        }
        let read: Proposal = serde_json::from_value(thin).unwrap();
        assert_eq!(read.why, "");
        assert!(read.about.is_none());
    }

    #[test]
    fn kinds_parse_from_their_stored_spelling() {
        for k in ProposalKind::ALL {
            assert_eq!(ProposalKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ProposalKind::parse("Event"), None);
    }
}
