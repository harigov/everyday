//! Proposals: work the assistant prepared and did not do.
//!
//! A [`Proposal`] is a finished record -- a task, a planned block, a memory,
//! a routine, a note, a skill -- that the assistant built but did not save,
//! or a mail draft it wrote but did not send, or a deletion it wants. The
//! person accepts or declines it where the record would have been drawn. See
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

use crate::agent::{Memory, Skill};
use crate::error::{Error, Result};
use crate::id::{ConversationId, DraftId, ItemId, ProposalId, RoutineRunId};
use crate::library::{Item, LogEntry};
use crate::note::Note;
use crate::record::RecordKind;
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

/// How long a proposal is kept unless it is accepted.
///
/// The most any of them waits for an answer, whatever it is, and the point at
/// which [`Vault::purge_proposals`](crate::Vault::purge_proposals) deletes one
/// that was declined, ignored or left to expire. An accepted one stays, as the
/// only record of where its task came from.
pub const PROPOSAL_DAYS: i64 = 7;

/// How many [`ProposalTrace`]s are kept.
pub const MAX_PROPOSAL_TRACES: usize = 300;

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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ProposalKind {
    Task,
    Block,
    Memory,
    Routine,
    Note,
    Mail,
    Skill,
    /// A catch-up the journal describes, when it is not clear who with or the
    /// person is not a contact yet. See [`ProposedLog`].
    Log,
}

impl ProposalKind {
    pub const ALL: [ProposalKind; 8] = [
        ProposalKind::Task,
        ProposalKind::Block,
        ProposalKind::Memory,
        ProposalKind::Routine,
        ProposalKind::Note,
        ProposalKind::Mail,
        ProposalKind::Skill,
        ProposalKind::Log,
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
            ProposalKind::Skill => "skill",
            ProposalKind::Log => "log",
        }
    }

    pub fn parse(s: &str) -> Option<ProposalKind> {
        ProposalKind::ALL.iter().copied().find(|k| k.as_str() == s.trim().to_lowercase())
    }
}

/// Every [`ProposalKind`] names a [`RecordKind`] it would make or change --
/// `Mail` proposes a [`RecordKind::Draft`], not a mail message itself, since
/// what a mail proposal carries is a draft to send. Total: there is no
/// `ProposalKind` this cannot answer.
impl From<ProposalKind> for RecordKind {
    fn from(kind: ProposalKind) -> RecordKind {
        match kind {
            ProposalKind::Task => RecordKind::Task,
            ProposalKind::Block => RecordKind::Block,
            ProposalKind::Memory => RecordKind::Memory,
            ProposalKind::Routine => RecordKind::Routine,
            ProposalKind::Note => RecordKind::Note,
            ProposalKind::Mail => RecordKind::Draft,
            ProposalKind::Skill => RecordKind::Skill,
            ProposalKind::Log => RecordKind::Log,
        }
    }
}

/// The reverse of [`From<ProposalKind> for RecordKind`], for the eight kinds
/// a proposal can actually carry. `Err(())` for the other twenty-five --
/// there is no proposal kind for a journal entry or a mailbox, and returning
/// that plainly is more honest than picking one.
impl TryFrom<RecordKind> for ProposalKind {
    type Error = ();

    fn try_from(kind: RecordKind) -> Result<ProposalKind, ()> {
        match kind {
            RecordKind::Task => Ok(ProposalKind::Task),
            RecordKind::Block => Ok(ProposalKind::Block),
            RecordKind::Memory => Ok(ProposalKind::Memory),
            RecordKind::Routine => Ok(ProposalKind::Routine),
            RecordKind::Note => Ok(ProposalKind::Note),
            RecordKind::Draft => Ok(ProposalKind::Mail),
            RecordKind::Skill => Ok(ProposalKind::Skill),
            RecordKind::Log => Ok(ProposalKind::Log),
            RecordKind::Journal
            | RecordKind::Entry
            | RecordKind::Project
            | RecordKind::Calendar
            | RecordKind::Event
            | RecordKind::Kind
            | RecordKind::Item
            | RecordKind::Tracker
            | RecordKind::Reading
            | RecordKind::Role
            | RecordKind::Goal
            | RecordKind::RoutineRun
            | RecordKind::Proposal
            | RecordKind::Conversation
            | RecordKind::Message
            | RecordKind::Account
            | RecordKind::Mailbox
            | RecordKind::MailMessage
            | RecordKind::Thread
            | RecordKind::Op
            | RecordKind::Recording
            | RecordKind::Transcript
            | RecordKind::Voiceprint
            | RecordKind::Job
            | RecordKind::NewsSource
            | RecordKind::Story
            | RecordKind::WritingAssist
            | RecordKind::BankLink
            | RecordKind::BankAccount
            | RecordKind::Txn
            | RecordKind::MoneyCategory
            | RecordKind::MoneyRule
            | RecordKind::ScreenApp
            | RecordKind::ScreenCategory => Err(()),
        }
    }
}

/// One of the people a name might mean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogCandidate {
    pub item_id: ItemId,
    pub name: String,
}

/// A catch-up to write, once somebody has said who it was with.
///
/// Reading a journal logs a catch-up by itself when the name means one contact.
/// This is the other two cases. A name that fits nobody carries the card to make
/// for them in `new_contact`, and accepting makes it. A name that fits several
/// lists them in `candidates`, and accepting has to say which, by way of an
/// edited record whose `log` names that person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProposedLog {
    pub log: LogEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_contact: Option<Item>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<LogCandidate>,
    /// The name as the journal wrote it.
    pub asked_as: String,
}

/// A record a proposal carries, whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum ProposedRecord {
    Task(Task),
    Block(TimeBlock),
    Memory(Memory),
    Routine(Routine),
    Note(Note),
    Skill(Skill),
    /// Boxed: a catch-up can carry a whole contact card, which would make every
    /// other record pay for its size.
    Log(Box<ProposedLog>),
}

impl ProposedRecord {
    pub fn kind(&self) -> ProposalKind {
        match self {
            ProposedRecord::Task(_) => ProposalKind::Task,
            ProposedRecord::Block(_) => ProposalKind::Block,
            ProposedRecord::Memory(_) => ProposalKind::Memory,
            ProposedRecord::Routine(_) => ProposalKind::Routine,
            ProposedRecord::Note(_) => ProposalKind::Note,
            ProposedRecord::Skill(_) => ProposalKind::Skill,
            ProposedRecord::Log(_) => ProposalKind::Log,
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
            ProposedRecord::Skill(r) => r.id.to_string(),
            ProposedRecord::Log(r) => r.log.id.to_string(),
        }
    }

    pub fn updated_at(&self) -> Timestamp {
        match self {
            ProposedRecord::Task(r) => r.updated_at,
            ProposedRecord::Block(r) => r.updated_at,
            ProposedRecord::Memory(r) => r.updated_at,
            ProposedRecord::Routine(r) => r.updated_at,
            ProposedRecord::Note(r) => r.updated_at,
            ProposedRecord::Skill(r) => r.updated_at,
            ProposedRecord::Log(r) => r.log.updated_at,
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
            ProposedRecord::Skill(s) => s.validate(),
            ProposedRecord::Log(l) => {
                if l.new_contact.as_ref().is_some_and(|c| c.title.trim().is_empty()) {
                    return Err(Error::Invalid("a contact needs a name".into()));
                }
                Ok(())
            }
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
                if *kind == ProposalKind::Log {
                    return Err(Error::Invalid(
                        "a catch-up proposal writes a row; it cannot delete one".into(),
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Somebody on the Contacts shelf. See [`crate::keepintouch`].
    Item,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct About {
    pub kind: AboutKind,
    pub id: String,
}

/// Every [`AboutKind`] names exactly the [`RecordKind`] it points at. Total:
/// there is no `AboutKind` this cannot answer.
impl From<AboutKind> for RecordKind {
    fn from(kind: AboutKind) -> RecordKind {
        match kind {
            AboutKind::Task => RecordKind::Task,
            AboutKind::Block => RecordKind::Block,
            AboutKind::Event => RecordKind::Event,
            AboutKind::Note => RecordKind::Note,
            AboutKind::Entry => RecordKind::Entry,
            AboutKind::Thread => RecordKind::Thread,
            AboutKind::Memory => RecordKind::Memory,
            AboutKind::Routine => RecordKind::Routine,
            AboutKind::Goal => RecordKind::Goal,
            AboutKind::Item => RecordKind::Item,
        }
    }
}

/// The reverse, for the ten kinds a proposal's "about" can actually name.
/// `Err(())` for the other twenty-three -- nothing has ever reacted to a
/// mailbox or a recording, so there is no `AboutKind` for one.
impl TryFrom<RecordKind> for AboutKind {
    type Error = ();

    fn try_from(kind: RecordKind) -> Result<AboutKind, ()> {
        match kind {
            RecordKind::Task => Ok(AboutKind::Task),
            RecordKind::Block => Ok(AboutKind::Block),
            RecordKind::Event => Ok(AboutKind::Event),
            RecordKind::Note => Ok(AboutKind::Note),
            RecordKind::Entry => Ok(AboutKind::Entry),
            RecordKind::Thread => Ok(AboutKind::Thread),
            RecordKind::Memory => Ok(AboutKind::Memory),
            RecordKind::Routine => Ok(AboutKind::Routine),
            RecordKind::Goal => Ok(AboutKind::Goal),
            RecordKind::Item => Ok(AboutKind::Item),
            RecordKind::Journal
            | RecordKind::Project
            | RecordKind::Calendar
            | RecordKind::Kind
            | RecordKind::Log
            | RecordKind::Tracker
            | RecordKind::Reading
            | RecordKind::Role
            | RecordKind::RoutineRun
            | RecordKind::Proposal
            | RecordKind::Conversation
            | RecordKind::Message
            | RecordKind::Account
            | RecordKind::Mailbox
            | RecordKind::MailMessage
            | RecordKind::Draft
            | RecordKind::Op
            | RecordKind::Recording
            | RecordKind::Transcript
            | RecordKind::Voiceprint
            | RecordKind::Skill
            | RecordKind::Job
            | RecordKind::NewsSource
            | RecordKind::Story
            | RecordKind::WritingAssist
            | RecordKind::BankLink
            | RecordKind::BankAccount
            | RecordKind::Txn
            | RecordKind::MoneyCategory
            | RecordKind::MoneyRule
            | RecordKind::ScreenApp
            | RecordKind::ScreenCategory => Err(()),
        }
    }
}

/// Who made a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProposalSource {
    /// A scheduled run -- today, always a dream.
    Run { run_id: RoutineRunId },
    /// A conversation, through the rail's "later" button.
    Conversation { conversation_id: ConversationId },
    /// Keeping in touch's nightly pass. See [`crate::keepintouch`].
    KeepInTouch,
}

/// Why somebody said no. Optional, and one tap: forcing a reason is how a
/// signal stops being given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// What is left of a proposal nobody accepted, once it has been deleted.
///
/// That it happened, and nothing of what it was. The weekly dream's stop list
/// asks whether the last three of a kind went unaccepted, and three can span
/// more weeks than the proposals themselves are kept for. Without these it
/// could only ever see the week, and a proposal left to expire is deleted the
/// moment it does, so ignoring them would teach it nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProposalTrace {
    pub kind: ProposalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<AboutKind>,
    pub made_at: Timestamp,
}

impl ProposalTrace {
    pub fn of(p: &Proposal) -> Self {
        Self { kind: p.kind, about: p.about.as_ref().map(|a| a.kind), made_at: p.made_at }
    }
}

/// Whether a proposal says anything about what the dreams should propose.
///
/// Keeping in touch makes its proposals in code, with its own cool-off. Ignoring
/// three "get in touch with" tasks, or a festival's, is no reason to tell the
/// dreams to stop proposing tasks. Known by who made it, and by the two shapes
/// only it makes, for a proposal written before it said who.
pub fn counts_towards_stop_list(p: &Proposal) -> bool {
    p.made_by != Some(ProposalSource::KeepInTouch)
        && p.kind != ProposalKind::Log
        && p.about.as_ref().map(|a| a.kind) != Some(AboutKind::Item)
}

/// Where a proposal stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
/// A week, or sooner when the thing itself has a date: nothing waits longer
/// than [`PROPOSAL_DAYS`], so that what is on the screen is what the assistant
/// made lately rather than a pile of what nobody got round to.
///
/// | Kind | Expires |
/// |---|---|
/// | Task | the end of its due day, or a week |
/// | Block | the block's end, or a week |
/// | Everything else | a week; a mail's draft's own fate closes it sooner |
pub fn expiry_for(payload: &Payload, now: Timestamp, tz: &str) -> Timestamp {
    let latest = now + SignedDuration::from_hours(24 * PROPOSAL_DAYS);
    let sooner = match payload {
        Payload::Create { record } | Payload::Replace { record, .. } => match record {
            ProposedRecord::Task(t) => t.due_date.map(|due| {
                end_of_day(due, tz)
                    .filter(|end| *end > now)
                    .unwrap_or(now + SignedDuration::from_hours(24))
            }),
            // A block already over is not worth offering; one hour lets a
            // sweep close it rather than a save refuse it.
            ProposedRecord::Block(b) => {
                Some(if b.end > now { b.end } else { now + SignedDuration::from_hours(1) })
            }
            _ => None,
        },
        _ => None,
    };
    sooner.map_or(latest, |at| at.min(latest))
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
    fn everything_else_waits_a_week() {
        let week = now() + SignedDuration::from_hours(24 * PROPOSAL_DAYS);
        let cases = [
            Payload::Create { record: ProposedRecord::Memory(Memory::new("x")) },
            Payload::Create {
                record: ProposedRecord::Routine(Routine::new(
                    "r",
                    "do",
                    crate::routine::Trigger::Manual,
                )),
            },
            Payload::Create { record: ProposedRecord::Note(Note::new("n")) },
            Payload::Create { record: ProposedRecord::Skill(Skill::new("s")) },
            Payload::Delete { kind: ProposalKind::Task, id: "x".into() },
            Payload::SendMail { draft_id: DraftId::new() },
        ];
        for payload in cases {
            assert_eq!(expiry_for(&payload, now(), "UTC"), week, "{payload:?}");
        }
    }

    #[test]
    fn nothing_with_a_date_far_off_waits_past_the_week() {
        let week = now() + SignedDuration::from_hours(24 * PROPOSAL_DAYS);
        let mut task = Task::new("Renew the passport");
        task.due_date = Some(date(2027, 3, 1));
        let start: Timestamp = "2027-03-01T09:00:00Z".parse().unwrap();
        let block = TimeBlock::new(BlockSubject::Adhoc, start, 60, "UTC");
        for record in [ProposedRecord::Task(task), ProposedRecord::Block(block)] {
            assert_eq!(expiry_for(&Payload::Create { record }, now(), "UTC"), week);
        }
    }

    #[test]
    fn keeping_in_touch_says_nothing_to_the_stop_list() {
        let task = |about: Option<AboutKind>| {
            Proposal::new(
                Payload::Create { record: ProposedRecord::Task(Task::new("x")) },
                "x",
                now(),
                "UTC",
            )
            .with_about(about.map(|kind| About { kind, id: "i".into() }))
        };
        assert!(counts_towards_stop_list(&task(None)));
        assert!(counts_towards_stop_list(&task(Some(AboutKind::Goal))));
        assert!(!counts_towards_stop_list(&task(Some(AboutKind::Item))));
        // A festival's task is about an event, and still not the dreams' business.
        let festival = task(Some(AboutKind::Event)).made_by(ProposalSource::KeepInTouch);
        assert!(!counts_towards_stop_list(&festival));
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

    #[test]
    fn every_proposal_kind_round_trips_through_record_kind() {
        // `From<ProposalKind> for RecordKind` is total (an exhaustive match
        // with no wildcard, so this cannot silently start skipping a
        // variant); this checks `TryFrom<RecordKind> for ProposalKind`
        // agrees with it for every kind that made the trip.
        for k in ProposalKind::ALL {
            assert_eq!(ProposalKind::try_from(RecordKind::from(k)), Ok(k));
        }
    }

    #[test]
    fn every_about_kind_round_trips_through_record_kind() {
        for k in [
            AboutKind::Task,
            AboutKind::Block,
            AboutKind::Event,
            AboutKind::Note,
            AboutKind::Entry,
            AboutKind::Thread,
            AboutKind::Memory,
            AboutKind::Routine,
            AboutKind::Goal,
            AboutKind::Item,
        ] {
            assert_eq!(AboutKind::try_from(RecordKind::from(k)), Ok(k));
        }
    }
}
