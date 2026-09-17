//! What kind of record this is, named once.
//!
//! Every persisted record already had a spelling for its kind scattered
//! across several surfaces that must never drift from each other: the label
//! its ciphertext is bound under (`*_aad`), the column value a purpose
//! pointer files it under (`store-sql`'s `purposes.record_kind`), the table
//! `record_secrets.owner_kind` names it by, and the noun an "X not found"
//! error uses. [`RecordKind`] is the one enum that names a record kind;
//! [`aad_word`](RecordKind::aad_word), [`purpose_column`](RecordKind::purpose_column),
//! [`secret_owner`](RecordKind::secret_owner) and
//! [`not_found_noun`](RecordKind::not_found_noun) are what each surface
//! reads a literal spelling from.
//!
//! Every method below is a hand-written `match` over literal strings, never
//! a derivation from a variant's own name (`Debug`, `stringify!`, and so
//! on). A rename of a variant must not silently rename a byte on disk; the
//! literal has to be typed out again for that to happen, and typing it out
//! again is what the tests in `crates/everyday-core/tests/pinned_strings.rs`
//! and this module's own tests are there to catch.
//!
//! [`RecordDescriptor`] is the other half: the *identity* of one record
//! type — its kind, its id type, and the `aad` label a specific id seals
//! under. `aad` always forwards to the `*_aad` function that already
//! existed for that record, so moving a record's identity in here changes
//! nothing about what gets written.

use std::fmt::Display;
use std::str::FromStr;

use crate::id::{
    AccountId, BlockId, CalendarId, ConversationId, DraftId, EntryId, EventId, GoalId, ItemId,
    JournalId, KindId, LogId, MailMessageId, MailboxId, MemoryId, MessageId, NoteId, OpId,
    ProjectId, ProposalId, ReadingId, RecordingId, RoleId, RoutineId, RoutineRunId, TaskId,
    ThreadId, TrackerId, TranscriptId, VoiceprintId,
};

/// The kind of one persisted record — every table `everyday-store-sql`
/// gives the generic `Record` treatment to (see that crate's
/// `record.rs`), named once so the surfaces that each spell it differently
/// have one enum to spell it from.
///
/// Deliberately not `Copy`-derived-into-a-string: see the module docs for
/// why every method below is its own literal `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordKind {
    Journal,
    Entry,
    Note,
    Project,
    Task,
    Block,
    Calendar,
    Event,
    /// A shelf: what the library domain calls `Kind` — see
    /// [`crate::library::Kind`]. Named after the type and table
    /// (`kinds`), not the plainer "shelf" the interface shows, because
    /// that is what every other surface already calls it.
    Kind,
    Item,
    Log,
    Tracker,
    Reading,
    Role,
    Goal,
    Routine,
    /// One run of a routine — `RoutineRun` the type, `"run"` on the wire.
    /// See [`RecordKind::aad_word`].
    RoutineRun,
    Proposal,
    Conversation,
    /// A message in one of the assistant's own conversations. Not to be
    /// confused with [`RecordKind::MailMessage`], a piece of mail.
    Message,
    Memory,
    Account,
    Mailbox,
    /// A piece of mail. Its `aad_word` is `"mail_message"`; its
    /// `not_found_noun` stayed `"message"`, matching the literal
    /// `store-sql` already used — see that method's docs.
    MailMessage,
    Thread,
    Draft,
    Op,
    Recording,
    Transcript,
    Voiceprint,
}

impl RecordKind {
    /// The word inside `everyday.<word>.v1:<id>` — the label a record's
    /// ciphertext is bound to. Pinned, for a fixed id, in
    /// `crates/everyday-core/tests/pinned_strings.rs`; this is the second
    /// place that spelling has to be typed out, in
    /// `crates/everyday-core/src/record.rs`'s own tests, so the two cannot
    /// drift without a test noticing.
    pub fn aad_word(self) -> &'static str {
        match self {
            RecordKind::Journal => "journal",
            RecordKind::Entry => "entry",
            RecordKind::Note => "note",
            RecordKind::Project => "project",
            RecordKind::Task => "task",
            RecordKind::Block => "block",
            RecordKind::Calendar => "calendar",
            RecordKind::Event => "event",
            RecordKind::Kind => "kind",
            RecordKind::Item => "item",
            RecordKind::Log => "log",
            RecordKind::Tracker => "tracker",
            RecordKind::Reading => "reading",
            RecordKind::Role => "role",
            RecordKind::Goal => "goal",
            RecordKind::Routine => "routine",
            // Legacy: a `RoutineRun`'s aad has always been
            // `everyday.run.v1:<id>`, not `routine_run`.
            RecordKind::RoutineRun => "run",
            RecordKind::Proposal => "proposal",
            RecordKind::Conversation => "conversation",
            RecordKind::Message => "message",
            RecordKind::Memory => "memory",
            RecordKind::Account => "account",
            RecordKind::Mailbox => "mailbox",
            // Legacy: sealed as `mail_message`, distinct from the
            // assistant's own `message`.
            RecordKind::MailMessage => "mail_message",
            RecordKind::Thread => "thread",
            RecordKind::Draft => "draft",
            RecordKind::Op => "op",
            RecordKind::Recording => "recording",
            RecordKind::Transcript => "transcript",
            RecordKind::Voiceprint => "voiceprint",
        }
    }

    /// The literal `everyday-store-sql` `purposes.record_kind` value this
    /// kind's rows are filed under, for the eight kinds a purpose pointer
    /// can actually attach to (`store-sql`'s own `purpose::RecordKind`
    /// before this phase). `None` for every other kind, including
    /// `Calendar`'s own `Record` impl not carrying one directly — see
    /// `crates/everyday-store-sql/src/calendars.rs`'s docs on why a
    /// calendar's purpose is computed rather than stored on the record.
    pub fn purpose_column(self) -> Option<&'static str> {
        match self {
            RecordKind::Project => Some("project"),
            RecordKind::Task => Some("task"),
            RecordKind::Block => Some("block"),
            RecordKind::Entry => Some("entry"),
            RecordKind::Note => Some("note"),
            RecordKind::Item => Some("item"),
            RecordKind::Calendar => Some("calendar"),
            RecordKind::Tracker => Some("tracker"),
            RecordKind::Journal
            | RecordKind::Event
            | RecordKind::Kind
            | RecordKind::Log
            | RecordKind::Reading
            | RecordKind::Role
            | RecordKind::Goal
            | RecordKind::Routine
            | RecordKind::RoutineRun
            | RecordKind::Proposal
            | RecordKind::Conversation
            | RecordKind::Message
            | RecordKind::Memory
            | RecordKind::Account
            | RecordKind::Mailbox
            | RecordKind::MailMessage
            | RecordKind::Thread
            | RecordKind::Draft
            | RecordKind::Op
            | RecordKind::Recording
            | RecordKind::Transcript
            | RecordKind::Voiceprint => None,
        }
    }

    /// The literal `record_secrets.owner_kind` this kind's per-record
    /// secrets are filed under, where one exists. Only accounts have one
    /// today — see `everyday_core::account::ACCOUNT_SECRET_OWNER_KIND` — a
    /// transcriber's own secret is filed under `"transcriber"`, which is
    /// not a `RecordKind` at all (see
    /// `everyday_core::vault::meetings::TRANSCRIBER_SECRET_OWNER_KIND`).
    pub fn secret_owner(self) -> Option<&'static str> {
        match self {
            RecordKind::Account => Some("account"),
            RecordKind::Journal
            | RecordKind::Entry
            | RecordKind::Note
            | RecordKind::Project
            | RecordKind::Task
            | RecordKind::Block
            | RecordKind::Calendar
            | RecordKind::Event
            | RecordKind::Kind
            | RecordKind::Item
            | RecordKind::Log
            | RecordKind::Tracker
            | RecordKind::Reading
            | RecordKind::Role
            | RecordKind::Goal
            | RecordKind::Routine
            | RecordKind::RoutineRun
            | RecordKind::Proposal
            | RecordKind::Conversation
            | RecordKind::Message
            | RecordKind::Memory
            | RecordKind::Mailbox
            | RecordKind::MailMessage
            | RecordKind::Thread
            | RecordKind::Draft
            | RecordKind::Op
            | RecordKind::Recording
            | RecordKind::Transcript
            | RecordKind::Voiceprint => None,
        }
    }

    /// The word an "X not found" [`crate::error::Error`] names this kind
    /// with — `everyday-store-sql`'s old `Record::KIND` constant.
    ///
    /// Equal to [`aad_word`](RecordKind::aad_word) for every kind but one:
    /// a piece of mail seals as `mail_message` but has always been reported
    /// as a plain `"message"` not found, because `store-sql`'s `Record for
    /// Message` in `mail/mod.rs` gave it `const KIND: &'static str =
    /// "message"` while its `aad` pointed at `mail::message_aad`, which
    /// says `mail_message`. Kept apart here rather than merged, so that
    /// difference — which was already true before this phase — stays true
    /// rather than being quietly resolved one way or the other.
    pub fn not_found_noun(self) -> &'static str {
        match self {
            RecordKind::Journal => "journal",
            RecordKind::Entry => "entry",
            RecordKind::Note => "note",
            RecordKind::Project => "project",
            RecordKind::Task => "task",
            RecordKind::Block => "block",
            RecordKind::Calendar => "calendar",
            RecordKind::Event => "event",
            RecordKind::Kind => "kind",
            RecordKind::Item => "item",
            RecordKind::Log => "log",
            RecordKind::Tracker => "tracker",
            RecordKind::Reading => "reading",
            RecordKind::Role => "role",
            RecordKind::Goal => "goal",
            RecordKind::Routine => "routine",
            RecordKind::RoutineRun => "run",
            RecordKind::Proposal => "proposal",
            RecordKind::Conversation => "conversation",
            RecordKind::Message => "message",
            RecordKind::Memory => "memory",
            RecordKind::Account => "account",
            RecordKind::Mailbox => "mailbox",
            // The one exception -- see this method's docs.
            RecordKind::MailMessage => "message",
            RecordKind::Thread => "thread",
            RecordKind::Draft => "draft",
            RecordKind::Op => "op",
            RecordKind::Recording => "recording",
            RecordKind::Transcript => "transcript",
            RecordKind::Voiceprint => "voiceprint",
        }
    }
}

/// The identity of one record type: its kind, its id type, and how an id
/// becomes the associated data its ciphertext is bound to.
///
/// `aad` always forwards to the `*_aad` function that already existed for
/// this record — see the impls below — so that giving a record type this
/// trait cannot itself change a byte that gets sealed.
pub trait RecordDescriptor {
    const KIND: RecordKind;
    type Id: Display + FromStr + Copy;

    fn id(&self) -> Self::Id;
    fn aad(id: Self::Id) -> Vec<u8>;
}

impl RecordDescriptor for crate::model::Journal {
    const KIND: RecordKind = RecordKind::Journal;
    type Id = JournalId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::journal_aad(id)
    }
}

impl RecordDescriptor for crate::model::Entry {
    const KIND: RecordKind = RecordKind::Entry;
    type Id = EntryId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::entry_aad(id)
    }
}

impl RecordDescriptor for crate::note::Note {
    const KIND: RecordKind = RecordKind::Note;
    type Id = NoteId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::notes::note_aad(id)
    }
}

impl RecordDescriptor for crate::task::Project {
    const KIND: RecordKind = RecordKind::Project;
    type Id = ProjectId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::tasks::project_aad(id)
    }
}

impl RecordDescriptor for crate::task::Task {
    const KIND: RecordKind = RecordKind::Task;
    type Id = TaskId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::tasks::task_aad(id)
    }
}

impl RecordDescriptor for crate::task::TimeBlock {
    const KIND: RecordKind = RecordKind::Block;
    type Id = BlockId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::tasks::block_aad(id)
    }
}

impl RecordDescriptor for crate::calendar::Calendar {
    const KIND: RecordKind = RecordKind::Calendar;
    type Id = CalendarId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::calendars::calendar_aad(id)
    }
}

impl RecordDescriptor for crate::calendar::Event {
    const KIND: RecordKind = RecordKind::Event;
    type Id = EventId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::calendars::event_aad(id)
    }
}

impl RecordDescriptor for crate::library::Kind {
    const KIND: RecordKind = RecordKind::Kind;
    type Id = KindId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::library::kind_aad(id)
    }
}

impl RecordDescriptor for crate::library::Item {
    const KIND: RecordKind = RecordKind::Item;
    type Id = ItemId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::library::item_aad(id)
    }
}

impl RecordDescriptor for crate::library::LogEntry {
    const KIND: RecordKind = RecordKind::Log;
    type Id = LogId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::library::log_aad(id)
    }
}

impl RecordDescriptor for crate::tracker::Tracker {
    const KIND: RecordKind = RecordKind::Tracker;
    type Id = TrackerId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::trackers::tracker_aad(id)
    }
}

impl RecordDescriptor for crate::tracker::Reading {
    const KIND: RecordKind = RecordKind::Reading;
    type Id = ReadingId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::trackers::reading_aad(id)
    }
}

impl RecordDescriptor for crate::purpose::Role {
    const KIND: RecordKind = RecordKind::Role;
    type Id = RoleId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::purpose::role_aad(id)
    }
}

impl RecordDescriptor for crate::purpose::Goal {
    const KIND: RecordKind = RecordKind::Goal;
    type Id = GoalId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::purpose::goal_aad(id)
    }
}

impl RecordDescriptor for crate::routine::Routine {
    const KIND: RecordKind = RecordKind::Routine;
    type Id = RoutineId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::routines::routine_aad(id)
    }
}

impl RecordDescriptor for crate::routine::RoutineRun {
    const KIND: RecordKind = RecordKind::RoutineRun;
    type Id = RoutineRunId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::routines::run_aad(id)
    }
}

impl RecordDescriptor for crate::proposal::Proposal {
    const KIND: RecordKind = RecordKind::Proposal;
    type Id = ProposalId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::proposals::proposal_aad(id)
    }
}

impl RecordDescriptor for crate::agent::Conversation {
    const KIND: RecordKind = RecordKind::Conversation;
    type Id = ConversationId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::agent::conversation_aad(id)
    }
}

impl RecordDescriptor for crate::agent::Message {
    const KIND: RecordKind = RecordKind::Message;
    type Id = MessageId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::agent::message_aad(id)
    }
}

impl RecordDescriptor for crate::agent::Memory {
    const KIND: RecordKind = RecordKind::Memory;
    type Id = MemoryId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::agent::memory_aad(id)
    }
}

impl RecordDescriptor for crate::account::Account {
    const KIND: RecordKind = RecordKind::Account;
    type Id = AccountId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::accounts::account_aad(id)
    }
}

impl RecordDescriptor for crate::mail::Mailbox {
    const KIND: RecordKind = RecordKind::Mailbox;
    type Id = MailboxId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::mail::mailbox_aad(id)
    }
}

impl RecordDescriptor for crate::mail::Message {
    const KIND: RecordKind = RecordKind::MailMessage;
    type Id = MailMessageId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::mail::message_aad(id)
    }
}

impl RecordDescriptor for crate::mail::Thread {
    const KIND: RecordKind = RecordKind::Thread;
    type Id = ThreadId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::mail::thread_aad(id)
    }
}

impl RecordDescriptor for crate::mail::Draft {
    const KIND: RecordKind = RecordKind::Draft;
    type Id = DraftId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::mail::draft_aad(id)
    }
}

impl RecordDescriptor for crate::mail::Op {
    const KIND: RecordKind = RecordKind::Op;
    type Id = OpId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::store::mail::op_aad(id)
    }
}

impl RecordDescriptor for crate::meeting::Recording {
    const KIND: RecordKind = RecordKind::Recording;
    type Id = RecordingId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::meeting::recording_aad(id)
    }
}

impl RecordDescriptor for crate::meeting::Transcript {
    const KIND: RecordKind = RecordKind::Transcript;
    type Id = TranscriptId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::meeting::transcript_aad(id)
    }
}

impl RecordDescriptor for crate::meeting::Voiceprint {
    const KIND: RecordKind = RecordKind::Voiceprint;
    type Id = VoiceprintId;
    fn id(&self) -> Self::Id {
        self.id
    }
    fn aad(id: Self::Id) -> Vec<u8> {
        crate::meeting::voiceprint_aad(id)
    }
}

#[cfg(test)]
mod tests {
    //! Ties [`RecordKind::aad_word`] to every [`RecordDescriptor::aad`],
    //! for a fixed id, so the two cannot drift from each other. This is
    //! *not* a repeat of `crates/everyday-core/tests/pinned_strings.rs`:
    //! that file pins each `*_aad` function's literal output against a
    //! hand-typed string; this checks that going through the descriptor
    //! produces exactly what `aad_word` says it will, which is the thing
    //! that could actually break if a phase-6 impl below named the wrong
    //! `RecordKind` variant or called the wrong `*_aad` function.
    use super::*;

    const FIXED: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn id<T: From<uuid::Uuid>>() -> T {
        T::from(uuid::Uuid::parse_str(FIXED).unwrap())
    }

    fn check<R: RecordDescriptor>(rid: R::Id) {
        assert_eq!(
            R::aad(rid),
            format!("everyday.{}.v1:{rid}", R::KIND.aad_word()).into_bytes(),
            "{:?}",
            R::KIND
        );
    }

    #[test]
    fn every_record_descriptor_aad_matches_its_kinds_aad_word() {
        check::<crate::model::Journal>(id());
        check::<crate::model::Entry>(id());
        check::<crate::note::Note>(id());
        check::<crate::task::Project>(id());
        check::<crate::task::Task>(id());
        check::<crate::task::TimeBlock>(id());
        check::<crate::calendar::Calendar>(id());
        check::<crate::calendar::Event>(id());
        check::<crate::library::Kind>(id());
        check::<crate::library::Item>(id());
        check::<crate::library::LogEntry>(id());
        check::<crate::tracker::Tracker>(id());
        check::<crate::tracker::Reading>(id());
        check::<crate::purpose::Role>(id());
        check::<crate::purpose::Goal>(id());
        check::<crate::routine::Routine>(id());
        check::<crate::routine::RoutineRun>(id());
        check::<crate::proposal::Proposal>(id());
        check::<crate::agent::Conversation>(id());
        check::<crate::agent::Message>(id());
        check::<crate::agent::Memory>(id());
        check::<crate::account::Account>(id());
        check::<crate::mail::Mailbox>(id());
        check::<crate::mail::Message>(id());
        check::<crate::mail::Thread>(id());
        check::<crate::mail::Draft>(id());
        check::<crate::mail::Op>(id());
        check::<crate::meeting::Recording>(id());
        check::<crate::meeting::Transcript>(id());
        check::<crate::meeting::Voiceprint>(id());
    }

    #[test]
    fn not_found_noun_matches_aad_word_except_the_one_mail_exception() {
        for kind in ALL {
            if kind == RecordKind::MailMessage {
                assert_eq!(kind.not_found_noun(), "message");
                assert_eq!(kind.aad_word(), "mail_message");
            } else {
                assert_eq!(kind.not_found_noun(), kind.aad_word(), "{kind:?}");
            }
        }
    }

    /// Every kind, for the test above. Not `pub` -- nothing outside this
    /// module needs "all of them" as a list; `RecordDescriptor` impls are
    /// looked up by type, not iterated.
    const ALL: [RecordKind; 30] = [
        RecordKind::Journal,
        RecordKind::Entry,
        RecordKind::Note,
        RecordKind::Project,
        RecordKind::Task,
        RecordKind::Block,
        RecordKind::Calendar,
        RecordKind::Event,
        RecordKind::Kind,
        RecordKind::Item,
        RecordKind::Log,
        RecordKind::Tracker,
        RecordKind::Reading,
        RecordKind::Role,
        RecordKind::Goal,
        RecordKind::Routine,
        RecordKind::RoutineRun,
        RecordKind::Proposal,
        RecordKind::Conversation,
        RecordKind::Message,
        RecordKind::Memory,
        RecordKind::Account,
        RecordKind::Mailbox,
        RecordKind::MailMessage,
        RecordKind::Thread,
        RecordKind::Draft,
        RecordKind::Op,
        RecordKind::Recording,
        RecordKind::Transcript,
        RecordKind::Voiceprint,
    ];
}
