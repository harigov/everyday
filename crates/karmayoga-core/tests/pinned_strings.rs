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

use karmayoga_core::id::*;
use karmayoga_core::store;
use karmayoga_core::{account, meeting, packstore, profile};

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
    assert_eq!(
        store::agent::skill_aad(id::<SkillId>()),
        b"everyday.skill.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
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

// ---- the feed -------------------------------------------------------------

#[test]
fn news_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::news::source_aad(id::<NewsSourceId>()),
        b"everyday.news_source.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::news::story_aad(id::<StoryId>()),
        b"everyday.story.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::news::story_body_aad(id::<StoryId>()),
        b"everyday.story_body.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::news::fetch_state_aad(id::<NewsSourceId>()),
        b"everyday.news_fetch.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(karmayoga_core::news::NEWS_KEY_LABEL, "everyday.news.key.v1");
}

// ---- money ----------------------------------------------------------------

#[test]
fn money_domain_aad_labels_are_pinned() {
    assert_eq!(
        store::money::bank_link_aad(id::<BankLinkId>()),
        b"everyday.bank_link.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::money::link_state_aad(id::<BankLinkId>()),
        b"everyday.bank_link_state.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::money::bank_account_aad(id::<BankAccountId>()),
        b"everyday.bank_account.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::money::balance_aad(id::<BankAccountId>(), jiff::civil::date(2026, 11, 5)),
        b"everyday.balance.v1:01234567-89ab-cdef-0123-456789abcdef:2026-11-05".to_vec()
    );
    assert_eq!(
        store::money::txn_aad(id::<TxnId>()),
        b"everyday.txn.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::money::money_category_aad(id::<MoneyCategoryId>()),
        b"everyday.money_category.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::money::money_rule_aad(id::<MoneyRuleId>()),
        b"everyday.money_rule.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(karmayoga_core::money::MONEY_KEY_LABEL, "everyday.money.key.v1");
    assert_eq!(karmayoga_core::money::PLAID_KEYS_OWNER_KIND, "plaid-keys");
    assert_eq!(karmayoga_core::money::PLAID_KEYS_OWNER_ID, "vault");
    assert_eq!(karmayoga_core::money::BANK_LINK_SECRET_OWNER_KIND, "bank_link");
}

// ---- health ---------------------------------------------------------------

#[test]
fn health_domain_aad_labels_are_pinned() {
    let day = jiff::civil::date(2026, 11, 15);
    assert_eq!(
        store::health::health_series_aad("0123456789abcdef0123456789abcdef", day),
        b"everyday.health_series.v1:0123456789abcdef0123456789abcdef:2026-11-15".to_vec()
    );
    assert_eq!(
        store::health::health_summary_aad("0123456789abcdef0123456789abcdef", day),
        b"everyday.health_summary.v1:0123456789abcdef0123456789abcdef:2026-11-15".to_vec()
    );
    assert_eq!(store::health::health_settings_aad(), b"everyday.health-settings.v1".to_vec());
    assert_eq!(
        store::health::health_source_aad(id::<HealthSourceId>()),
        b"everyday.health_source.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::workout_aad(id::<WorkoutId>()),
        b"everyday.workout.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::medication_aad(id::<MedicationId>()),
        b"everyday.medication.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::dose_aad(id::<DoseId>()),
        b"everyday.dose.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::condition_aad(id::<ConditionId>()),
        b"everyday.condition.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::lab_result_aad(id::<LabResultId>()),
        b"everyday.lab_result.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::health_document_aad(id::<HealthDocumentId>()),
        b"everyday.health_document.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::allergy_aad(id::<AllergyId>()),
        b"everyday.allergy.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::food_aad(id::<FoodId>()),
        b"everyday.food.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    assert_eq!(
        store::health::meal_aad(id::<MealId>()),
        b"everyday.meal.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

/// The label every series hash, workout key and dose key is derived under,
/// and the owner the food-lookup keys are sealed for. A change to either
/// orphans every stored day of samples, or the stored keys.
#[test]
fn health_key_label_and_secret_owner_are_pinned() {
    assert_eq!(karmayoga_core::health::HEALTH_KEY_LABEL, "karmayoga.health.key.v1");
    assert_eq!(karmayoga_core::health::HEALTH_KEYS_OWNER_KIND, "health-keys");
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
    assert_eq!(store::mail::mail_sorting_aad(), b"everyday.mail_sorting.v1".to_vec());
    assert_eq!(store::mail::mail_writing_aad(), b"everyday.mail_writing.v1".to_vec());
    assert_eq!(store::mail::voice_learning_aad(), b"everyday.mail_voice_learning.v1".to_vec());
}

// ---- notes ------------------------------------------------------------

#[test]
fn note_aad_label_is_pinned() {
    assert_eq!(
        store::notes::note_aad(id::<NoteId>()),
        b"everyday.note.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

#[test]
fn template_aad_label_is_pinned() {
    assert_eq!(
        store::notes::template_aad(id::<TemplateId>()),
        b"everyday.template.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
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
    use karmayoga_core::purpose::Purpose;

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

// ---- background jobs ----------------------------------------------------

#[test]
fn writing_assist_aad_label_is_pinned() {
    assert_eq!(
        store::writing::writing_assist_aad(id::<WritingId>()),
        b"everyday.writing_assist.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
}

#[test]
fn job_aad_label_is_pinned() {
    assert_eq!(
        store::jobs::job_aad(id::<JobId>()),
        b"everyday.job.v1:01234567-89ab-cdef-0123-456789abcdef".to_vec()
    );
    // A job and a routine's run are both "work the assistant did", sealed in
    // tables of the same shape, and a record that opened under the other's
    // label would be a record read as the wrong thing.
    let job = store::jobs::job_aad(id::<JobId>());
    let run = store::routines::run_aad(id::<RoutineRunId>());
    assert_ne!(job, run);
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
    assert_eq!(JobId::KIND, "job");
    assert_eq!(WritingId::KIND, "writing");
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
    assert_eq!(SkillId::KIND, "skill");
    assert_eq!(PackId::KIND, "pack");
    assert_eq!(AccountId::KIND, "account");
    assert_eq!(MailboxId::KIND, "mailbox");
    assert_eq!(MailMessageId::KIND, "mail_message");
    assert_eq!(ThreadId::KIND, "thread");
    assert_eq!(DraftId::KIND, "draft");
    assert_eq!(OpId::KIND, "op");
}

// ---- backups (crate::backup) ------------------------------------------------

#[test]
fn backup_settings_aad_labels_are_pinned() {
    use karmayoga_core::backup;
    assert_eq!(backup::settings_aad(), b"karmayoga.backup-settings.v1");
    assert_eq!(backup::status_aad(), b"karmayoga.backup-status.v1");
    assert_eq!(backup::BACKUP_SECRET_OWNER_KIND, "backup");
    assert_eq!(backup::BACKUP_SECRET_OWNER_ID, "s3");
}

/// The labels a backup's keys are derived under, and the associated data its
/// chunks and manifests are sealed against, pinned by their effect: a chunk
/// sealed today must open tomorrow, and must be named the same, or every
/// backup ever made stops opening -- or stops sharing chunks with the next.
/// Computed here from the literal labels, independently of the module.
#[test]
fn backup_key_labels_and_chunk_aad_are_pinned() {
    use karmayoga_core::backup::BackupKeys;
    use karmayoga_core::crypto::{AeadCipher, Cipher, SecretKey};
    let vault = AeadCipher::new(&SecretKey::from_bytes([7u8; 32]));
    let keys = BackupKeys::from_cipher(&vault, "2026-11-14T03:05:12Z");

    let name_key = vault.derive_subkey("karmayoga.backup.name.v1");
    let expected_name = blake3::keyed_hash(name_key.expose(), b"chunk").to_hex()[..32].to_string();
    assert_eq!(keys.chunk_name(b"chunk"), expected_name);
    let expected_repo = blake3::keyed_hash(
        name_key.expose(),
        b"karmayoga.backup.repository\x002026-11-14T03:05:12Z",
    )
    .to_hex()[..32]
        .to_string();
    assert_eq!(keys.repository_id(), expected_repo);

    let seal = AeadCipher::new(&vault.derive_subkey("karmayoga.backup.seal.v1"));
    let sealed = keys.seal_chunk(&expected_name, b"chunk").unwrap();
    let aad = format!("karmayoga.backup.chunk.v1:{expected_name}");
    assert_eq!(seal.open(aad.as_bytes(), &sealed).unwrap(), b"chunk");
    let manifest = keys.seal_manifest("2026-11-14T030512Z", b"{}").unwrap();
    assert_eq!(
        seal.open(b"karmayoga.backup.snapshot.v1:2026-11-14T030512Z", &manifest).unwrap(),
        b"{}"
    );
}
