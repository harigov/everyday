//! Phase 0.1 of `docs/plans/architecture-refactor.md`: every string this
//! crate seals a record's ciphertext under, or writes into a clear index
//! column, pinned to its literal spelling.
//!
//! These are not "does the code do what the code says" tests -- `format!`
//! reproducing its own format string would always pass. They exist so that a
//! later phase that *moves* where a label comes from (phase 6's
//! `RecordDescriptor`, for instance) cannot also *change* it by accident: an
//! id encrypted under `everyday.task.v1:<id>` today must still open under
//! that exact string tomorrow, because the ciphertext already on disk was
//! sealed against it and nothing in this refactor is allowed to touch a byte
//! already written. See "The one rule" at the top of the plan.
//!
//! Every expected value below is a string literal, not a value built from
//! the same pieces the function under test builds it from -- an expected
//! string assembled with the type's own `Display` or a `match` on its
//! variant names would drift in lockstep with a bug in either and prove
//! nothing. A fixed id, spelled out once at the top, stands in for "some
//! id" throughout.

use everyday_core::id::*;
use everyday_core::store;
use everyday_core::{account, meeting, packstore, profile};

/// An arbitrary, fixed, well-formed UUID. Not a nil or all-same-digit id on
/// purpose: a bug that mishandled the nil UUID specifically, or that
/// transposed two identical hex groups, would still pass with one of those.
const FIXED: &str = "01234567-89ab-cdef-0123-456789abcdef";

fn id<T: From<uuid::Uuid>>() -> T {
    T::from(uuid::Uuid::parse_str(FIXED).unwrap())
}

/// `BlobId` is content-addressed, not a `typed_id!`, so it has no `From<Uuid>`
/// and no textual form that comes from a UUID at all -- its `Display` is 64
/// hex digits over the raw bytes. All-zero bytes are used here rather than a
/// hash of some fixed input, so the expected hex is checkable by inspection
/// rather than by trusting a second BLAKE3 computation in the test itself.
fn zero_blob_id() -> BlobId {
    BlobId([0u8; 32])
}

/// 32 zero bytes, hex-encoded, computed independently of `BlobId::to_hex` --
/// `"0".repeat(64)` rather than a hand-typed run of zeros, so there is no
/// hex digit count to miscount by hand and no dependence on the encoder
/// under test to produce it.
fn zero_hex_64() -> String {
    "0".repeat(64)
}

#[test]
fn zero_blob_id_hexes_to_sixty_four_zeros() {
    // A sanity check on the fixture above, not on production code.
    assert_eq!(zero_blob_id().to_hex().len(), 64);
    assert_eq!(zero_hex_64(), zero_blob_id().to_hex());
}

// ---- the journal domain (crate::store, top level) -------------------------

#[test]
fn journal_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::entry_aad(id::<EntryId>()),
        b"everyday.entry.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::journal_aad(id::<JournalId>()),
        b"everyday.journal.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::blob_aad(zero_blob_id()),
        format!("everyday.blob.v1:{}", zero_hex_64()).into_bytes()
    );
}

// ---- accounts ---------------------------------------------------------

#[test]
fn account_aad_label_is_pinned() {
    assert_eq!(
        store::accounts::account_aad(id::<AccountId>()),
        b"everyday.account.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

#[test]
fn account_secret_owner_kind_is_pinned() {
    // The `record_secrets.owner_kind` an `AccountSecret` is filed under.
    assert_eq!(account::ACCOUNT_SECRET_OWNER_KIND, "account");
}

// ---- the assistant's own domain ----------------------------------------

#[test]
fn agent_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::agent::conversation_aad(id::<ConversationId>()),
        b"everyday.conversation.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::agent::message_aad(id::<MessageId>()),
        b"everyday.message.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::agent::memory_aad(id::<MemoryId>()),
        b"everyday.memory.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(store::agent::settings_aad(), b"everyday.agent-settings.v1".to_vec());
    assert_eq!(store::agent::secret_aad(), b"everyday.agent-secret.v1".to_vec());
}

// ---- calendars ----------------------------------------------------------

#[test]
fn calendar_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::calendars::calendar_aad(id::<CalendarId>()),
        b"everyday.calendar.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::calendars::event_aad(id::<EventId>()),
        b"everyday.event.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- the library domain --------------------------------------------------

#[test]
fn library_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::library::kind_aad(id::<KindId>()),
        b"everyday.kind.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::library::item_aad(id::<ItemId>()),
        b"everyday.item.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::library::log_aad(id::<LogId>()),
        b"everyday.log.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- mail -----------------------------------------------------------------

#[test]
fn mail_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::mail::mailbox_aad(id::<MailboxId>()),
        b"everyday.mailbox.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::message_aad(id::<MailMessageId>()),
        b"everyday.mail_message.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::thread_aad(id::<ThreadId>()),
        b"everyday.thread.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::body_aad(id::<MailMessageId>()),
        b"everyday.mail_body.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::category_rules_aad(id::<AccountId>()),
        b"everyday.mail_category_rules.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::draft_aad(id::<DraftId>()),
        b"everyday.draft.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::op_aad(id::<OpId>()),
        b"everyday.op.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::mail::remote_image_settings_aad(),
        b"everyday.mail_remote_image_settings.v1".to_vec()
    );
    assert_eq!(store::mail::contacts_aad(), b"everyday.mail_contacts.v1".to_vec());
}

// ---- notes ------------------------------------------------------------

#[test]
fn note_aad_label_is_pinned() {
    assert_eq!(
        store::notes::note_aad(id::<NoteId>()),
        b"everyday.note.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- proposals ----------------------------------------------------------

#[test]
fn proposal_aad_label_is_pinned() {
    assert_eq!(
        store::proposals::proposal_aad(id::<ProposalId>()),
        b"everyday.proposal.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- the purpose domain ---------------------------------------------------

#[test]
fn purpose_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::purpose::role_aad(id::<RoleId>()),
        b"everyday.role.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::purpose::goal_aad(id::<GoalId>()),
        b"everyday.goal.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

#[test]
fn purpose_kind_str_and_from_columns_are_pinned() {
    use everyday_core::purpose::Purpose;

    let goal_id: GoalId = id();
    let role_id: RoleId = id();
    let goal = Purpose::Goal { id: goal_id };
    let role = Purpose::Role { id: role_id };

    assert_eq!(goal.kind_str(), "goal");
    assert_eq!(role.kind_str(), "role");

    assert_eq!(
        Purpose::from_columns(Some("goal"), Some(&goal_id.to_string())),
        Some(Purpose::Goal { id: goal_id })
    );
    assert_eq!(
        Purpose::from_columns(Some("role"), Some(&role_id.to_string())),
        Some(Purpose::Role { id: role_id })
    );
}

// ---- routines ---------------------------------------------------------

#[test]
fn routine_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::routines::routine_aad(id::<RoutineId>()),
        b"everyday.routine.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::routines::run_aad(id::<RoutineRunId>()),
        b"everyday.run.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- per-record secrets -------------------------------------------------

#[test]
fn record_secret_aad_label_is_pinned() {
    assert_eq!(
        store::secrets::record_secret_aad("account", "1234"),
        b"everyday.record-secret.v1:account:1234".to_vec()
    );
}

// ---- tasks ------------------------------------------------------------

#[test]
fn task_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::tasks::project_aad(id::<ProjectId>()),
        b"everyday.project.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::tasks::task_aad(id::<TaskId>()),
        b"everyday.task.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::tasks::block_aad(id::<BlockId>()),
        b"everyday.block.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- trackers -----------------------------------------------------------

#[test]
fn tracker_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::trackers::reading_aad(id::<ReadingId>()),
        b"everyday.reading.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::trackers::tracker_aad(id::<TrackerId>()),
        b"everyday.tracker.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

// ---- meeting notes ------------------------------------------------------

#[test]
fn meeting_domain_aad_labels_are_pinned() {
    assert_eq!(
        meeting::recording_aad(id::<RecordingId>()),
        b"everyday.recording.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        meeting::transcript_aad(id::<TranscriptId>()),
        b"everyday.transcript.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        meeting::voiceprint_aad(id::<VoiceprintId>()),
        b"everyday.voiceprint.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        meeting::chunk_aad(id::<RecordingId>(), meeting::Track::Mic, 7),
        b"everyday.chunk.v1:01234567-89ab-cdef-0123-456789abcdef:mic:7".to_vec()
    );
    assert_eq!(meeting::SETTINGS_AAD.to_vec(), b"everyday.meeting-settings.v1".to_vec());
}

// ---- the mail pack file ---------------------------------------------------

#[test]
fn pack_aad_label_is_pinned() {
    assert_eq!(
        packstore::pack_aad(id::<PackId>(), 42),
        b"everyday.mailpack.v1:01234567-89ab-cdef-0123-456789abcdef:42".to_vec()
    );
}

// ---- the profile singleton ------------------------------------------------

#[test]
fn profile_aad_label_is_pinned() {
    assert_eq!(profile::profile_aad(), b"everyday.profile.v1".to_vec());
}

// ---- every typed id's KIND word -------------------------------------------

/// `typed_id!`'s `KIND` is what a `RecordDescriptor` (phase 6) would read as
/// its `RecordKind`'s wire spelling, and today is folded straight into every
/// `Debug` impl the macro generates -- so a value here changing is a change
/// to what a panic message says an id's type is, not only a persistence
/// concern. Pinned all the same, one per id, because phase 6 is explicitly
/// the phase these are not allowed to move under.
#[test]
fn every_typed_id_kind_is_pinned() {
    assert_eq!(JournalId::KIND, "journal");
    assert_eq!(EntryId::KIND, "entry");
    assert_eq!(NoteId::KIND, "note");
    assert_eq!(RoutineId::KIND, "routine");
    assert_eq!(RoutineRunId::KIND, "run");
    assert_eq!(ProposalId::KIND, "proposal");
    assert_eq!(RecordingId::KIND, "recording");
    assert_eq!(TranscriptId::KIND, "transcript");
    assert_eq!(VoiceprintId::KIND, "voiceprint");
    assert_eq!(TemplateId::KIND, "template");
    assert_eq!(ProjectId::KIND, "project");
    assert_eq!(TaskId::KIND, "task");
    assert_eq!(BlockId::KIND, "block");
    assert_eq!(CalendarId::KIND, "calendar");
    assert_eq!(EventId::KIND, "event");
    assert_eq!(KindId::KIND, "kind");
    assert_eq!(ItemId::KIND, "item");
    assert_eq!(LogId::KIND, "log");
    assert_eq!(TrackerId::KIND, "tracker");
    assert_eq!(ReadingId::KIND, "reading");
    assert_eq!(RoleId::KIND, "role");
    assert_eq!(GoalId::KIND, "goal");
    assert_eq!(ConversationId::KIND, "conversation");
    assert_eq!(MessageId::KIND, "message");
    assert_eq!(MemoryId::KIND, "memory");
    assert_eq!(PackId::KIND, "pack");
    assert_eq!(AccountId::KIND, "account");
    assert_eq!(MailboxId::KIND, "mailbox");
    assert_eq!(MailMessageId::KIND, "mail_message");
    assert_eq!(ThreadId::KIND, "thread");
    assert_eq!(DraftId::KIND, "draft");
    assert_eq!(OpId::KIND, "op");
}
