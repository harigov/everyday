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
//! again is what the tests in `crates/karmayoga-core/tests/pinned_strings.rs`
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
    AccountId, AreaId, BlockId, CalendarId, ConversationId, DraftId, EntryId, EventId, GoalId,
    ItemId, JobId, JournalId, KindId, LogId, MailMessageId, MailboxId, MemoryId, MessageId,
    NewsSourceId, NoteId, OpId, ProjectId, ProposalId, ReadingId, RecordingId, RoutineId,
    RoutineRunId, SkillId, StoryId, TaskId, TemplateId, ThreadId, TrackerId, TranscriptId,
    VoiceprintId, WritingId,
};

/// The kind of one persisted record — every table `karmayoga-store-sql`
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
    /// A shape to start writing from -- see [`crate::template::Template`].
    Template,
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
    /// A part of a life -- `Area` the type, `"role"` in its AEAD label, the
    /// word areas went by until November 2026. See [`RecordKind::aad_word`].
    Area,
    Goal,
    Routine,
    /// One run of a routine — `RoutineRun` the type, `"run"` on the wire.
    /// See [`RecordKind::aad_word`].
    RoutineRun,
    /// Background work -- see [`crate::job::Job`].
    Job,
    Proposal,
    Conversation,
    /// A message in one of the assistant's own conversations. Not to be
    /// confused with [`RecordKind::MailMessage`], a piece of mail.
    Message,
    Memory,
    /// A process the assistant was given for a certain kind of request. See
    /// [`crate::agent::Skill`].
    Skill,
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
    /// Something followed in the Feed -- see [`crate::news::NewsSource`].
    NewsSource,
    /// A story a source brought -- see [`crate::news::Story`].
    Story,
    /// One use of writing help -- see [`crate::writing::WritingAssist`].
    WritingAssist,
    /// One sign-in at a bank -- see [`crate::money::BankLink`].
    BankLink,
    /// An account a link brought, or one kept by hand -- see
    /// [`crate::money::BankAccount`].
    BankAccount,
    /// A transaction -- see [`crate::money::Txn`].
    Txn,
    /// A spending or income category, with its budget -- see
    /// [`crate::money::MoneyCategory`].
    MoneyCategory,
    /// A rule that sorts transactions as they arrive -- see
    /// [`crate::money::MoneyRule`].
    MoneyRule,
    /// A person's label for an app or a site -- see
    /// [`crate::screentime::ScreenApp`].
    ScreenApp,
    /// A category screen time is filed under -- see
    /// [`crate::screentime::ScreenCategory`].
    ScreenCategory,
    /// One file in the files catalog -- see [`crate::files::FileEntry`].
    File,
    /// An album of files -- see [`crate::files::Album`].
    Album,
    /// A connection health data arrives through -- see [`crate::health::HealthSource`].
    HealthSource,
    /// A workout -- see [`crate::health::Workout`].
    Workout,
    /// A medication or supplement -- see [`crate::health::Medication`].
    Medication,
    /// One dose taken or skipped -- see [`crate::health::Dose`].
    Dose,
    /// A condition somebody lives with, and its care plan -- see [`crate::health::Condition`].
    Condition,
    /// One lab test result -- see [`crate::health::LabResult`].
    LabResult,
    /// A clinical document and its sealed files -- see [`crate::health::HealthDocument`].
    HealthDocument,
    /// An allergy -- see [`crate::health::Allergy`].
    Allergy,
    /// A food, with its nutrients -- see [`crate::health::Food`].
    Food,
    /// A meal eaten -- see [`crate::health::Meal`].
    Meal,
}

impl RecordKind {
    /// The word inside `everyday.<word>.v1:<id>` — the label a record's
    /// ciphertext is bound to. Pinned, for a fixed id, in
    /// `crates/karmayoga-core/tests/pinned_strings.rs`; this is the second
    /// place that spelling has to be typed out, in
    /// `crates/karmayoga-core/src/record.rs`'s own tests, so the two cannot
    /// drift without a test noticing.
    pub fn aad_word(self) -> &'static str {
        match self {
            RecordKind::Journal => "journal",
            RecordKind::Entry => "entry",
            RecordKind::Note => "note",
            RecordKind::Template => "template",
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
            // Legacy: areas were roles when the first of them was sealed, and
            // the label bound into every one of those ciphertexts says so.
            RecordKind::Area => "role",
            RecordKind::Goal => "goal",
            RecordKind::Routine => "routine",
            // Legacy: a `RoutineRun`'s aad has always been
            // `everyday.run.v1:<id>`, not `routine_run`.
            RecordKind::RoutineRun => "run",
            RecordKind::Job => "job",
            RecordKind::Proposal => "proposal",
            RecordKind::Conversation => "conversation",
            RecordKind::Message => "message",
            RecordKind::Memory => "memory",
            RecordKind::Skill => "skill",
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
            RecordKind::NewsSource => "news_source",
            RecordKind::Story => "story",
            RecordKind::WritingAssist => "writing_assist",
            RecordKind::BankLink => "bank_link",
            RecordKind::BankAccount => "bank_account",
            RecordKind::Txn => "txn",
            RecordKind::MoneyCategory => "money_category",
            RecordKind::MoneyRule => "money_rule",
            RecordKind::ScreenApp => "screen_app",
            RecordKind::ScreenCategory => "screen_category",
            RecordKind::File => "file",
            RecordKind::Album => "album",
            RecordKind::HealthSource => "health_source",
            RecordKind::Workout => "workout",
            RecordKind::Medication => "medication",
            RecordKind::Dose => "dose",
            RecordKind::Condition => "condition",
            RecordKind::LabResult => "lab_result",
            RecordKind::HealthDocument => "health_document",
            RecordKind::Allergy => "allergy",
            RecordKind::Food => "food",
            RecordKind::Meal => "meal",
        }
    }

    /// The literal `karmayoga-store-sql` `purposes.record_kind` value this
    /// kind's rows are filed under, for the eight kinds a purpose pointer
    /// can actually attach to (`store-sql`'s own `purpose::RecordKind`
    /// before this phase). `None` for every other kind, including
    /// `Calendar`'s own `Record` impl not carrying one directly — see
    /// `crates/karmayoga-store-sql/src/calendars.rs`'s docs on why a
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
            | RecordKind::Template
            | RecordKind::Event
            | RecordKind::Kind
            | RecordKind::Log
            | RecordKind::Reading
            | RecordKind::Area
            | RecordKind::Goal
            | RecordKind::Routine
            | RecordKind::RoutineRun
            | RecordKind::Job
            | RecordKind::Proposal
            | RecordKind::Conversation
            | RecordKind::Message
            | RecordKind::Memory
            | RecordKind::Skill
            | RecordKind::Account
            | RecordKind::Mailbox
            | RecordKind::MailMessage
            | RecordKind::Thread
            | RecordKind::Draft
            | RecordKind::Op
            | RecordKind::Recording
            | RecordKind::Transcript
            | RecordKind::Voiceprint
            | RecordKind::NewsSource
            | RecordKind::Story
            | RecordKind::WritingAssist
            | RecordKind::BankLink
            | RecordKind::BankAccount
            | RecordKind::Txn
            | RecordKind::MoneyCategory
            | RecordKind::MoneyRule
            | RecordKind::ScreenApp
            | RecordKind::ScreenCategory
            | RecordKind::File
            | RecordKind::Album => None,
            RecordKind::HealthSource
            | RecordKind::Workout
            | RecordKind::Medication
            | RecordKind::Dose
            | RecordKind::Condition
            | RecordKind::LabResult
            | RecordKind::HealthDocument
            | RecordKind::Allergy
            | RecordKind::Food
            | RecordKind::Meal => None,
        }
    }

    /// The literal `record_secrets.owner_kind` this kind's per-record
    /// secrets are filed under, where one exists. Only accounts have one
    /// today — see `karmayoga_core::account::ACCOUNT_SECRET_OWNER_KIND` — an
    /// older build's transcriber key is filed under `"transcriber"`, which
    /// is not a `RecordKind` at all, until unlock moves it into the model
    /// keys (see
    /// `karmayoga_core::vault::meetings::TRANSCRIBER_SECRET_OWNER_KIND`).
    pub fn secret_owner(self) -> Option<&'static str> {
        match self {
            RecordKind::Account => Some("account"),
            // A link's access token -- see
            // `karmayoga_core::money::BANK_LINK_SECRET_OWNER_KIND`.
            RecordKind::BankLink => Some("bank_link"),
            RecordKind::Journal
            | RecordKind::Entry
            | RecordKind::Note
            | RecordKind::Template
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
            | RecordKind::Area
            | RecordKind::Goal
            | RecordKind::Routine
            | RecordKind::RoutineRun
            | RecordKind::Job
            | RecordKind::Proposal
            | RecordKind::Conversation
            | RecordKind::Message
            | RecordKind::Memory
            | RecordKind::Skill
            | RecordKind::Mailbox
            | RecordKind::MailMessage
            | RecordKind::Thread
            | RecordKind::Draft
            | RecordKind::Op
            | RecordKind::Recording
            | RecordKind::Transcript
            | RecordKind::Voiceprint
            | RecordKind::NewsSource
            | RecordKind::Story
            | RecordKind::WritingAssist
            | RecordKind::BankAccount
            | RecordKind::Txn
            | RecordKind::MoneyCategory
            | RecordKind::MoneyRule
            | RecordKind::ScreenApp
            | RecordKind::ScreenCategory
            | RecordKind::File
            | RecordKind::Album => None,
            RecordKind::HealthSource
            | RecordKind::Workout
            | RecordKind::Medication
            | RecordKind::Dose
            | RecordKind::Condition
            | RecordKind::LabResult
            | RecordKind::HealthDocument
            | RecordKind::Allergy
            | RecordKind::Food
            | RecordKind::Meal => None,
        }
    }

    /// The word an "X not found" [`crate::error::Error`] names this kind
    /// with — `karmayoga-store-sql`'s old `Record::KIND` constant.
    ///
    /// Equal to [`aad_word`](RecordKind::aad_word) for every kind but two.
    /// An area seals as `role`, the word it went by when its label was fixed,
    /// and is reported as the `"area"` the interface calls it. And a piece of
    /// mail seals as `mail_message` but has always been reported
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
            RecordKind::Template => "template",
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
            // The other exception: what a person reads says what the
            // interface calls it, not the label its ciphertext is bound to.
            RecordKind::Area => "area",
            RecordKind::Goal => "goal",
            RecordKind::Routine => "routine",
            RecordKind::RoutineRun => "run",
            RecordKind::Job => "job",
            RecordKind::Proposal => "proposal",
            RecordKind::Conversation => "conversation",
            RecordKind::Message => "message",
            RecordKind::Memory => "memory",
            RecordKind::Skill => "skill",
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
            RecordKind::NewsSource => "news_source",
            RecordKind::Story => "story",
            RecordKind::WritingAssist => "writing_assist",
            RecordKind::BankLink => "bank_link",
            RecordKind::BankAccount => "bank_account",
            RecordKind::Txn => "txn",
            RecordKind::MoneyCategory => "money_category",
            RecordKind::MoneyRule => "money_rule",
            RecordKind::ScreenApp => "screen_app",
            RecordKind::ScreenCategory => "screen_category",
            RecordKind::File => "file",
            RecordKind::Album => "album",
            RecordKind::HealthSource => "health_source",
            RecordKind::Workout => "workout",
            RecordKind::Medication => "medication",
            RecordKind::Dose => "dose",
            RecordKind::Condition => "condition",
            RecordKind::LabResult => "lab_result",
            RecordKind::HealthDocument => "health_document",
            RecordKind::Allergy => "allergy",
            RecordKind::Food => "food",
            RecordKind::Meal => "meal",
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

/// Writes the [`RecordDescriptor`] impl for each record: the record type, its
/// kind, its id type, and the function that builds the id's AAD. Every record
/// reads its id from an `id` field, so that part is the same each time.
macro_rules! record_descriptors {
    ($($record:ty, $kind:ident, $id:ty, $aad:path;)*) => {$(
        impl RecordDescriptor for $record {
            const KIND: RecordKind = RecordKind::$kind;
            type Id = $id;
            fn id(&self) -> Self::Id {
                self.id
            }
            fn aad(id: Self::Id) -> Vec<u8> {
                $aad(id)
            }
        }
    )*};
}

record_descriptors! {
    crate::model::Journal, Journal, JournalId, crate::store::journal_aad;
    crate::model::Entry, Entry, EntryId, crate::store::entry_aad;
    crate::note::Note, Note, NoteId, crate::store::notes::note_aad;
    crate::template::Template, Template, TemplateId, crate::store::notes::template_aad;
    crate::task::Project, Project, ProjectId, crate::store::tasks::project_aad;
    crate::task::Task, Task, TaskId, crate::store::tasks::task_aad;
    crate::task::TimeBlock, Block, BlockId, crate::store::tasks::block_aad;
    crate::calendar::Calendar, Calendar, CalendarId, crate::store::calendars::calendar_aad;
    crate::calendar::Event, Event, EventId, crate::store::calendars::event_aad;
    crate::library::Kind, Kind, KindId, crate::store::library::kind_aad;
    crate::library::Item, Item, ItemId, crate::store::library::item_aad;
    crate::library::LogEntry, Log, LogId, crate::store::library::log_aad;
    crate::tracker::Tracker, Tracker, TrackerId, crate::store::trackers::tracker_aad;
    crate::tracker::Reading, Reading, ReadingId, crate::store::trackers::reading_aad;
    crate::purpose::Area, Area, AreaId, crate::store::purpose::area_aad;
    crate::purpose::Goal, Goal, GoalId, crate::store::purpose::goal_aad;
    crate::routine::Routine, Routine, RoutineId, crate::store::routines::routine_aad;
    crate::routine::RoutineRun, RoutineRun, RoutineRunId, crate::store::routines::run_aad;
    crate::job::Job, Job, JobId, crate::store::jobs::job_aad;
    crate::proposal::Proposal, Proposal, ProposalId, crate::store::proposals::proposal_aad;
    crate::agent::Conversation, Conversation, ConversationId, crate::store::agent::conversation_aad;
    crate::agent::Message, Message, MessageId, crate::store::agent::message_aad;
    crate::agent::Memory, Memory, MemoryId, crate::store::agent::memory_aad;
    crate::agent::Skill, Skill, SkillId, crate::store::agent::skill_aad;
    crate::account::Account, Account, AccountId, crate::store::accounts::account_aad;
    crate::mail::Mailbox, Mailbox, MailboxId, crate::store::mail::mailbox_aad;
    crate::mail::Message, MailMessage, MailMessageId, crate::store::mail::message_aad;
    crate::mail::Thread, Thread, ThreadId, crate::store::mail::thread_aad;
    crate::mail::Draft, Draft, DraftId, crate::store::mail::draft_aad;
    crate::mail::Op, Op, OpId, crate::store::mail::op_aad;
    crate::meeting::Recording, Recording, RecordingId, crate::meeting::recording_aad;
    crate::meeting::Transcript, Transcript, TranscriptId, crate::meeting::transcript_aad;
    crate::meeting::Voiceprint, Voiceprint, VoiceprintId, crate::meeting::voiceprint_aad;
    crate::news::NewsSource, NewsSource, NewsSourceId, crate::store::news::source_aad;
    crate::news::Story, Story, StoryId, crate::store::news::story_aad;
    crate::writing::WritingAssist, WritingAssist, WritingId, crate::store::writing::writing_assist_aad;
    crate::money::BankLink, BankLink, crate::id::BankLinkId, crate::store::money::bank_link_aad;
    crate::money::BankAccount, BankAccount, crate::id::BankAccountId, crate::store::money::bank_account_aad;
    crate::money::Txn, Txn, crate::id::TxnId, crate::store::money::txn_aad;
    crate::money::MoneyCategory, MoneyCategory, crate::id::MoneyCategoryId, crate::store::money::money_category_aad;
    crate::money::MoneyRule, MoneyRule, crate::id::MoneyRuleId, crate::store::money::money_rule_aad;
    crate::screentime::ScreenApp, ScreenApp, crate::id::ScreenAppId, crate::store::screentime::screen_app_aad;
    crate::screentime::ScreenCategory, ScreenCategory, crate::id::ScreenCategoryId, crate::store::screentime::screen_category_aad;
    crate::files::FileEntry, File, crate::id::FileId, crate::store::files::file_aad;
    crate::files::Album, Album, crate::id::AlbumId, crate::store::files::album_aad;
    crate::health::HealthSource, HealthSource, crate::id::HealthSourceId, crate::store::health::health_source_aad;
    crate::health::Workout, Workout, crate::id::WorkoutId, crate::store::health::workout_aad;
    crate::health::Medication, Medication, crate::id::MedicationId, crate::store::health::medication_aad;
    crate::health::Dose, Dose, crate::id::DoseId, crate::store::health::dose_aad;
    crate::health::Condition, Condition, crate::id::ConditionId, crate::store::health::condition_aad;
    crate::health::LabResult, LabResult, crate::id::LabResultId, crate::store::health::lab_result_aad;
    crate::health::HealthDocument, HealthDocument, crate::id::HealthDocumentId, crate::store::health::health_document_aad;
    crate::health::Allergy, Allergy, crate::id::AllergyId, crate::store::health::allergy_aad;
    crate::health::Food, Food, crate::id::FoodId, crate::store::health::food_aad;
    crate::health::Meal, Meal, crate::id::MealId, crate::store::health::meal_aad;
}

#[cfg(test)]
mod tests {
    //! Ties [`RecordKind::aad_word`] to every [`RecordDescriptor::aad`],
    //! for a fixed id, so the two cannot drift from each other. This is
    //! *not* a repeat of `crates/karmayoga-core/tests/pinned_strings.rs`:
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
        check::<crate::template::Template>(id());
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
        check::<crate::purpose::Area>(id());
        check::<crate::purpose::Goal>(id());
        check::<crate::routine::Routine>(id());
        check::<crate::routine::RoutineRun>(id());
        check::<crate::job::Job>(id());
        check::<crate::proposal::Proposal>(id());
        check::<crate::agent::Conversation>(id());
        check::<crate::agent::Message>(id());
        check::<crate::agent::Memory>(id());
        check::<crate::agent::Skill>(id());
        check::<crate::account::Account>(id());
        check::<crate::mail::Mailbox>(id());
        check::<crate::mail::Message>(id());
        check::<crate::mail::Thread>(id());
        check::<crate::mail::Draft>(id());
        check::<crate::mail::Op>(id());
        check::<crate::meeting::Recording>(id());
        check::<crate::meeting::Transcript>(id());
        check::<crate::meeting::Voiceprint>(id());
        check::<crate::news::NewsSource>(id());
        check::<crate::news::Story>(id());
        check::<crate::writing::WritingAssist>(id());
        check::<crate::money::BankLink>(id());
        check::<crate::money::BankAccount>(id());
        check::<crate::money::Txn>(id());
        check::<crate::money::MoneyCategory>(id());
        check::<crate::money::MoneyRule>(id());
        check::<crate::screentime::ScreenApp>(id());
        check::<crate::screentime::ScreenCategory>(id());
        check::<crate::files::FileEntry>(id());
        check::<crate::files::Album>(id());
        check::<crate::health::HealthSource>(id());
        check::<crate::health::Workout>(id());
        check::<crate::health::Medication>(id());
        check::<crate::health::Dose>(id());
        check::<crate::health::Condition>(id());
        check::<crate::health::LabResult>(id());
        check::<crate::health::HealthDocument>(id());
        check::<crate::health::Allergy>(id());
        check::<crate::health::Food>(id());
        check::<crate::health::Meal>(id());
    }

    #[test]
    fn not_found_noun_matches_aad_word_except_the_two_exceptions() {
        for kind in ALL {
            if kind == RecordKind::MailMessage {
                assert_eq!(kind.not_found_noun(), "message");
                assert_eq!(kind.aad_word(), "mail_message");
            } else if kind == RecordKind::Area {
                assert_eq!(kind.not_found_noun(), "area");
                assert_eq!(kind.aad_word(), "role");
            } else {
                assert_eq!(kind.not_found_noun(), kind.aad_word(), "{kind:?}");
            }
        }
    }

    /// Every kind, for the test above. Not `pub` -- nothing outside this
    /// module needs "all of them" as a list; `RecordDescriptor` impls are
    /// looked up by type, not iterated.
    const ALL: [RecordKind; 55] = [
        RecordKind::Journal,
        RecordKind::Entry,
        RecordKind::Note,
        RecordKind::Template,
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
        RecordKind::Area,
        RecordKind::Goal,
        RecordKind::Routine,
        RecordKind::RoutineRun,
        RecordKind::Job,
        RecordKind::Proposal,
        RecordKind::Conversation,
        RecordKind::Message,
        RecordKind::Memory,
        RecordKind::Skill,
        RecordKind::Account,
        RecordKind::Mailbox,
        RecordKind::MailMessage,
        RecordKind::Thread,
        RecordKind::Draft,
        RecordKind::Op,
        RecordKind::Recording,
        RecordKind::Transcript,
        RecordKind::Voiceprint,
        RecordKind::NewsSource,
        RecordKind::Story,
        RecordKind::WritingAssist,
        RecordKind::BankLink,
        RecordKind::BankAccount,
        RecordKind::Txn,
        RecordKind::MoneyCategory,
        RecordKind::MoneyRule,
        RecordKind::ScreenApp,
        RecordKind::ScreenCategory,
        RecordKind::File,
        RecordKind::Album,
        RecordKind::HealthSource,
        RecordKind::Workout,
        RecordKind::Medication,
        RecordKind::Dose,
        RecordKind::Condition,
        RecordKind::LabResult,
        RecordKind::HealthDocument,
        RecordKind::Allergy,
        RecordKind::Food,
        RecordKind::Meal,
    ];
}
