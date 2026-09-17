//! Builds one record of every kind the [`Vault`] API can write, with fixed
//! ids and fixed timestamps, so the bytes it produces are reproducible.
//!
//! This is Phase 0.3's fixture: `everyday-vault/tests/fixture_writer.rs`
//! calls [`build`] once to make the vaults checked into `tests/fixtures/`,
//! and `everyday-vault/tests/frozen_vault.rs` calls it a second time, at a
//! different [`Ids`]/[`Clock`] base, to prove today's write path still
//! produces a readable result after the fixture was frozen. Both callers
//! run the same function so the two can never quietly drift apart.
//!
//! # Why ids and timestamps are hand-rolled here
//!
//! Every `X::new(...)` constructor in the domain calls `Timestamp::now()`
//! and `XId::new()` calls `Uuid::now_v7()` -- right for the application,
//! wrong for a fixture that has to read back identically however many times
//! it is regenerated. [`Ids`] and [`Clock`] are deterministic stand-ins for
//! both, so every record built here gets a fixed id and a fixed
//! `created_at`/`updated_at` instead. Nothing in this file calls a mutator
//! that stamps `Timestamp::now()` on its own (`Journal::show`,
//! `Task::set_status`, and so on) -- fields are set directly instead, for
//! the same reason.

#![allow(dead_code)]

use everyday_core::account::{Account, AccountSecret, Provider as MailAccountProvider};
use everyday_core::agent::{
    AgentSettings, Conversation, Memory, MemoryOrigin, Message as ChatMessage,
};
use everyday_core::calendar::{AccountCalendarSource, Calendar, Event, EventStatus};
use everyday_core::id::{
    AccountId, BlockId, CalendarId, ConversationId, DraftId, EntryId, EventId, GoalId, ItemId,
    JournalId, KindId, LogId, MailMessageId, MailboxId, MemoryId, MessageId, NoteId, OpId,
    ProjectId, ProposalId, ReadingId, RecordingId, RoleId, RoutineId, RoutineRunId, TaskId,
    TemplateId, ThreadId, TrackerId, TranscriptId, VoiceprintId,
};
use everyday_core::library::{Item, ItemStatus, Kind, LogEntry, LogEvent};
use everyday_core::mail::{
    Address, Body, CategorySource, Draft, Mailbox, MailboxRole, Message as MailMessage,
    MessageFlags, Op, OpKind, OpState, OpTarget, Origin,
};
use everyday_core::meeting::{Recording, Stage, Transcript, Voiceprint};
use everyday_core::model::{Attachment, Entry, Journal, Location, MediaKind};
use everyday_core::note::Note;
use everyday_core::packstore::PackRef;
use everyday_core::proposal::{Payload, Proposal, ProposedRecord};
use everyday_core::purpose::{Goal, GoalStatus, Purpose, Role as LifeRole};
use everyday_core::richtext::{MEDIA_NODE, RichDoc};
use everyday_core::routine::{Outcome, Routine, RoutineRun, Trigger};
use everyday_core::store::mail::IngestMessage;
use everyday_core::task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, Task, TaskStatus, TimeBlock,
};
use everyday_core::tracker::{Reading, Tracker, TrackerKind};
use everyday_core::{PackId, Vault};
use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::json;

/// Fixed test password for the encrypted fixture vault. Not a secret worth
/// protecting -- it unlocks a vault that holds nothing but this file's own
/// fabricated data -- but named once so `fixture_writer.rs` and
/// `frozen_vault.rs` cannot say two different things.
pub const ENCRYPTED_PASSWORD: &str = "fixture-vault-password";

/// The id base [`build`] is called with to make the checked-in fixture.
pub const GEN_FROZEN: u128 = 1;

/// The id base `frozen_vault.rs` calls [`build`] with a second time, to add
/// "one more of everything" into a copy of the fixture. Far enough from
/// [`GEN_FROZEN`] that the two generations' ids never collide, however many
/// records either one grows to.
pub const GEN_APPENDED: u128 = 1_000_000;

/// Seconds since the epoch [`build`] starts stamping `created_at`/
/// `updated_at` from, for the [`GEN_FROZEN`] generation.
pub const TS_FROZEN: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

/// As [`TS_FROZEN`], for [`GEN_APPENDED`]. Comfortably past the other
/// generation's own span so the two eras of timestamps in a dump sort
/// apart, which makes a diff easier to read.
pub const TS_APPENDED: i64 = 1_800_000_000; // 2027-01-15T06:13:20Z

/// A deterministic id allocator. Every `next::<XId>()` call mints the next
/// id in sequence, formatted as a UUID, so one generation's ids are dense,
/// ordered and reproducible instead of random.
pub struct Ids {
    next: u128,
}

impl Ids {
    pub fn new(base: u128) -> Self {
        Self { next: base }
    }

    /// Mint the next id as `T`, whatever `typed_id!` type that is -- every
    /// one of them implements `FromStr` over a UUID's canonical text form,
    /// which is all this needs.
    pub fn next<T: std::str::FromStr>(&mut self) -> T
    where
        T::Err: std::fmt::Debug,
    {
        let n = self.next;
        self.next += 1;
        let hex = format!("{n:032x}");
        let text = format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        );
        text.parse().unwrap_or_else(|e| panic!("{text:?} did not parse as an id: {e:?}"))
    }
}

/// A deterministic clock. Every `next()` call returns the next second in
/// sequence, so a fixture's timestamps are fixed and yet each one is
/// distinct -- useful when a golden diff needs to say which field moved.
pub struct Clock {
    next: i64,
}

impl Clock {
    pub fn new(base: i64) -> Self {
        Self { next: base }
    }

    pub fn next(&mut self) -> Timestamp {
        let s = self.next;
        self.next += 1;
        Timestamp::from_second(s).unwrap()
    }
}

/// Build one record of every kind the [`Vault`] API can write, with ids
/// minted from `base` and timestamps from `ts_base`.
///
/// Self-contained: every record made here is new (its own journal, its own
/// account, its own everything), so calling this twice against the same
/// vault with two different `(base, ts_base)` pairs adds a second,
/// independent tree of records rather than colliding with the first.
///
/// Returns the fixed blob bytes behind the one entry's attachment, so a
/// caller can check them against what [`Vault::blob`] hands back without
/// this function needing to expose every id it minted.
pub fn build(vault: &Vault, base: u128, ts_base: i64) -> Vec<u8> {
    let mut ids = Ids::new(base);
    let mut clock = Clock::new(ts_base);
    let tag = format!("{base:x}");

    // ---- purpose: role and goal, first, so everything else can point at
    // them ------------------------------------------------------------

    let role_id: RoleId = ids.next();
    let mut role = LifeRole::new(format!("Fixture role {tag}"));
    role.id = role_id;
    role.created_at = clock.next();
    role.updated_at = role.created_at;
    vault.save_role(&role).unwrap();

    let goal_id: GoalId = ids.next();
    let mut goal = Goal::new(role.id, format!("Fixture goal {tag}"));
    goal.id = goal_id;
    goal.status = GoalStatus::Active;
    goal.created_at = clock.next();
    goal.updated_at = goal.created_at;
    vault.save_goal(&goal).unwrap();

    // ---- tracker and reading ------------------------------------------

    let tracker_id: TrackerId = ids.next();
    let mut tracker = Tracker::new(format!("Fixture tracker {tag}"), TrackerKind::Amount);
    tracker.id = tracker_id;
    tracker.purpose = Some(Purpose::Role { id: role.id });
    tracker.created_at = clock.next();
    tracker.updated_at = tracker.created_at;
    vault.save_tracker(&tracker).unwrap();

    let reading_date = fixture_date(base);
    let reading_id: ReadingId = ids.next();
    let mut reading = Reading::on(tracker.id, reading_date, 42.0);
    reading.id = reading_id;
    reading.note = format!("fixture reading {tag}");
    reading.created_at = clock.next();
    reading.updated_at = reading.created_at;
    vault.save_reading(&reading).unwrap();

    // ---- journal, and an entry with an attached blob -------------------

    let journal_id: JournalId = ids.next();
    let mut journal = Journal::new(format!("Fixture journal {tag}"));
    journal.id = journal_id;
    journal.shown_trackers = vec![tracker.id];
    journal.created_at = clock.next();
    journal.updated_at = journal.created_at;
    vault.save_journal(&journal).unwrap();

    let blob_bytes = blob_bytes_for(base);
    let blob_id = vault.put_blob(&blob_bytes).unwrap();

    let entry_id: EntryId = ids.next();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.id = entry_id;
    entry.title = format!("Fixture entry {tag}");
    entry.body = RichDoc(json!({
        "type": "doc",
        "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "A fixture entry."}]},
            {"type": MEDIA_NODE, "attrs": {
                "blob": blob_id.to_hex(), "kind": "image", "mime": "image/png",
                "filename": "fixture.png", "caption": "a fixture attachment"}},
        ]
    }));
    entry.local_date = reading_date;
    entry.tags = vec!["fixture".into()];
    entry.starred = true;
    entry.location = Some(Location {
        latitude: 51.5,
        longitude: -0.1,
        place_name: Some("Fixture Place".into()),
        locality: Some("Fixture City".into()),
        country: Some("Fixtureland".into()),
    });
    entry.attachments = vec![Attachment {
        blob: blob_id,
        kind: MediaKind::Image,
        mime: "image/png".into(),
        filename: "fixture.png".into(),
        byte_len: blob_bytes.len() as u64,
        width: Some(10),
        height: Some(10),
        duration_ms: None,
        caption: "a fixture attachment".into(),
    }];
    entry.purpose = Some(Purpose::Goal { id: goal.id });
    entry.created_at = clock.next();
    entry.updated_at = entry.created_at;
    vault.save_entry(&entry, None).unwrap();

    // ---- note, and the meeting note a transcript hangs off of -----------

    let note_id: NoteId = ids.next();
    let mut note = Note::new(format!("Fixture note {tag}"));
    note.body = RichDoc::from_plain_text("A fixture note.");
    note.tags = vec!["fixture".into()];
    note.pinned = true;
    note.purpose = Some(Purpose::Goal { id: goal.id });
    note.created_at = clock.next();
    note.updated_at = note.created_at;
    note.id = note_id;
    vault.save_note(&note, None).unwrap();

    // ---- tasks: project, task, subtask, and a time block -----------------

    let project_id: ProjectId = ids.next();
    let mut project = Project::new(format!("Fixture project {tag}"));
    project.id = project_id;
    project.status = ProjectStatus::Active;
    project.priority = Priority::Medium;
    project.purpose = Some(Purpose::Goal { id: goal.id });
    project.created_at = clock.next();
    project.updated_at = project.created_at;
    vault.save_project(&project).unwrap();

    let task_id: TaskId = ids.next();
    let mut task = Task::new(format!("Fixture task {tag}")).in_project(project.id);
    task.id = task_id;
    task.status = TaskStatus::Doing;
    task.priority = Priority::High;
    task.purpose = Some(Purpose::Role { id: role.id });
    task.created_at = clock.next();
    task.updated_at = task.created_at;
    vault.save_task(&task).unwrap();

    let subtask_id: TaskId = ids.next();
    let mut subtask =
        Task::new(format!("Fixture subtask {tag}")).in_project(project.id).under(task.id);
    subtask.id = subtask_id;
    subtask.status = TaskStatus::Done;
    subtask.completed_at = Some(clock.next());
    subtask.created_at = clock.next();
    subtask.updated_at = subtask.created_at;
    vault.save_task(&subtask).unwrap();

    let block_id: BlockId = ids.next();
    let block_start = clock.next();
    let mut block = TimeBlock::new(BlockSubject::Task { id: task.id }, block_start, 30, "UTC");
    block.id = block_id;
    block.kind = BlockKind::Planned;
    block.purpose = Some(Purpose::Goal { id: goal.id });
    block.created_at = clock.next();
    block.updated_at = block.created_at;
    vault.save_block(&block).unwrap();

    // ---- library: a shelf, an item on it, and a log entry ---------------

    let kind_id: KindId = ids.next();
    let kind_slug = format!("fixture-kind-{tag}");
    let mut kind = Kind::new(&kind_slug, "Fixtures", "Fixture");
    kind.id = kind_id;
    kind.created_at = clock.next();
    kind.updated_at = kind.created_at;
    vault.save_kind(&kind).unwrap();

    let item_id: ItemId = ids.next();
    let mut item = Item::new(kind.id, format!("Fixture item {tag}"));
    item.id = item_id;
    item.status = ItemStatus::Active;
    item.purpose = Some(Purpose::Goal { id: goal.id });
    item.created_at = clock.next();
    item.updated_at = item.created_at;
    vault.save_item(&item).unwrap();

    let log_id: LogId = ids.next();
    let mut log = LogEntry::new(item.id, LogEvent::Finished, reading_date, "UTC");
    log.id = log_id;
    log.note = format!("fixture log {tag}");
    log.created_at = clock.next();
    log.updated_at = log.created_at;
    vault.save_log(&log).unwrap();

    // ---- an account, its secret, and a calendar with one event ----------

    let account_id: AccountId = ids.next();
    let mut account =
        Account::new(MailAccountProvider::Custom, format!("fixture-{tag}@example.com"));
    account.id = account_id;
    account.display_name = format!("Fixture Account {tag}");
    account.created_at = clock.next();
    account.updated_at = account.created_at;
    vault.save_account(&account).unwrap();

    let secret = AccountSecret {
        refresh_token: Some(format!("fixture-refresh-token-{tag}")),
        access_token: Some((format!("fixture-access-token-{tag}"), clock.next())),
        password: None,
        client_secret: None,
    };
    vault.save_account_secret(account.id, &secret).unwrap();

    let calendar_id: CalendarId = ids.next();
    let mut calendar = Calendar::from_account(
        account.id,
        MailAccountProvider::Custom,
        AccountCalendarSource::CalDav,
        format!("fixture-remote-cal-{tag}"),
        format!("Fixture Calendar {tag}"),
    );
    calendar.id = calendar_id;
    calendar.role_id = Some(role.id);
    calendar.created_at = clock.next();
    calendar.updated_at = calendar.created_at;

    let event_id: EventId = ids.next();
    let event_start = clock.next();
    let event = Event {
        id: event_id,
        calendar_id: calendar.id,
        uid: format!("fixture-event-{tag}"),
        title: format!("Fixture event {tag}"),
        description: "A fixture event.".into(),
        location: "Fixtureland".into(),
        start: event_start,
        end: event_start + jiff::SignedDuration::from_mins(30),
        local_date: reading_date,
        end_date: reading_date,
        tz: "UTC".into(),
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: "organizer@example.com".into(),
        attendees: vec!["attendee@example.com".into()],
        url: String::new(),
        busy: true,
        series: None,
        updated_at: clock.next(),
    };
    // Through `with_store`, not `sync_account_calendar`: that method calls
    // `Calendar::mark_synced`, which stamps `Timestamp::now()` internally
    // and would make this fixture non-reproducible. Going straight to the
    // `CalendarStore` accessor keeps every timestamp the one this function
    // chose. See `Vault::with_store`'s own docs: this is the escape hatch
    // it exists for.
    vault
        .with_store(|s| {
            let calendars = s.calendars().expect("sqlite backend supports calendars");
            calendars.put_calendar(&calendar)?;
            calendars.upsert_events(calendar.id, &[event], &[])
        })
        .unwrap();

    // ---- routine and a run -----------------------------------------------

    let routine_id: RoutineId = ids.next();
    let mut routine =
        Routine::new(format!("Fixture routine {tag}"), "Do the fixture thing.", Trigger::Manual);
    routine.id = routine_id;
    routine.created_at = clock.next();
    routine.updated_at = routine.created_at;
    vault.save_routine(&routine).unwrap();

    let run_id: RoutineRunId = ids.next();
    let mut run = RoutineRun::new(&routine, None);
    run.id = run_id;
    run.started_at = clock.next();
    run.finished_at = Some(clock.next());
    run.outcome = Outcome::Done;
    run.summary = format!("Fixture run summary {tag}");
    vault.save_run(&run).unwrap();

    // ---- a proposal (a task the assistant prepared, undone) --------------

    let proposal_id: ProposalId = ids.next();
    let mut proposed_task = Task::new(format!("Fixture proposed task {tag}"));
    // The proposed record's own id is unrelated to the proposal's -- it is
    // what the task would be saved under if this were accepted, and never
    // is in this fixture.
    proposed_task.id = ids.next();
    proposed_task.created_at = clock.next();
    proposed_task.updated_at = proposed_task.created_at;
    let proposal_now = clock.next();
    let mut proposal = Proposal::new(
        Payload::Create { record: ProposedRecord::Task(proposed_task) },
        format!("Fixture proposal {tag}"),
        proposal_now,
        "UTC",
    );
    proposal.id = proposal_id;
    proposal.why = "because this is a fixture".into();
    vault.save_proposal(&proposal).unwrap();

    // ---- assistant: a conversation, a message, and a memory --------------

    let conversation_id: ConversationId = ids.next();
    let mut conversation = Conversation::new();
    conversation.id = conversation_id;
    conversation.title = format!("Fixture conversation {tag}");
    conversation.created_at = clock.next();
    conversation.updated_at = conversation.created_at;
    vault.save_conversation(&conversation).unwrap();

    let message_id: MessageId = ids.next();
    let mut message = ChatMessage::user(conversation.id, format!("Fixture message {tag}"));
    message.id = message_id;
    message.created_at = clock.next();
    vault.save_message(&message).unwrap();

    let memory_id: MemoryId = ids.next();
    let mut memory = Memory::new(format!("Fixture memory {tag}"));
    memory.id = memory_id;
    memory.source_id = Some(conversation.id);
    memory.origin = MemoryOrigin::Told;
    memory.created_at = clock.next();
    memory.updated_at = memory.created_at;
    vault.save_memory(&memory).unwrap();

    // ---- agent settings and its own secret --------------------------------

    let settings = AgentSettings {
        enabled: true,
        name: format!("Fixture Assistant {tag}"),
        instructions: "Be a fixture.".into(),
        ..AgentSettings::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    vault.set_agent_key(&format!("fixture-agent-key-{tag}")).unwrap();

    // ---- mail: mailbox, message, body, thread, draft, outbox op ----------

    let mailbox_id: MailboxId = ids.next();
    let mut mailbox = Mailbox::new(account.id, "INBOX", MailboxRole::Inbox);
    mailbox.id = mailbox_id;
    vault.save_mailbox(&mailbox).unwrap();

    let mail_message_id: MailMessageId = ids.next();
    let thread_id: ThreadId = ids.next();
    let mail_date = clock.next();
    let mail_message = MailMessage {
        id: mail_message_id,
        account_id: account.id,
        thread_id,
        message_id_header: format!("<fixture-{tag}@example.com>"),
        date: mail_date,
        from: Address::new("Fixture Sender", format!("sender-{tag}@example.com")),
        to: vec![Address::new("Fixture Recipient", format!("fixture-{tag}@example.com"))],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: format!("Fixture subject {tag}"),
        snippet: "A fixture message.".into(),
        flags: MessageFlags { seen: true, ..MessageFlags::default() },
        labels: vec!["fixture".into()],
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef {
            account: account.id.to_string(),
            pack: ids.next::<PackId>(),
            offset: 0,
            len: 0,
        },
        gmail: None,
        invite: None,
    };
    vault
        .ingest_mail(
            account.id,
            vec![IngestMessage { message: mail_message, mailbox: mailbox.id, uid: 1 }],
        )
        .unwrap();

    let body = Body {
        message_id: mail_message_id,
        html_sanitised: String::new(),
        text: format!("A fixture message body, generation {tag}."),
        quoted_ranges: Vec::new(),
        signature_range: None,
        parts: Vec::new(),
        remote_images: Vec::new(),
    };
    vault.save_body(&body).unwrap();

    let draft_id: DraftId = ids.next();
    let mut draft = Draft::new(account.id, account.address.clone(), Origin::Person);
    draft.id = draft_id;
    draft.to = vec![Address::bare(format!("draft-recipient-{tag}@example.com"))];
    draft.subject = format!("Fixture draft {tag}");
    draft.body_html = "<p>A fixture draft.</p>".into();
    draft.created_at = clock.next();
    draft.updated_at = draft.created_at;
    vault.save_draft(&draft).unwrap();

    let op_id: OpId = ids.next();
    let mut op = Op::new(account.id, OpKind::MarkRead, OpTarget::Thread(thread_id), Origin::Person);
    op.id = op_id;
    op.not_before = clock.next();
    op.state = OpState::Pending;
    op.created_at = clock.next();
    op.updated_at = op.created_at;
    vault.update_op(&op).unwrap();

    // ---- meetings: a recording, a transcript on the note above, a voice --

    let template_id: TemplateId = ids.next();
    let recording_id: RecordingId = ids.next();
    let mut recording = Recording::new(format!("Fixture recording {tag}"), None, template_id);
    recording.id = recording_id;
    recording.started_at = clock.next();
    recording.ended_at = Some(clock.next());
    recording.stage = Stage::Done;
    recording.note_id = Some(note.id);
    recording.updated_at = recording.started_at;
    vault.save_recording(&recording).unwrap();

    let transcript_id: TranscriptId = ids.next();
    let transcript = Transcript {
        id: transcript_id,
        note_id: note.id,
        recording_id: Some(recording.id),
        language: Some("en".into()),
        backend: "fixture".into(),
        speakers: Vec::new(),
        segments: Vec::new(),
        created_at: clock.next(),
        updated_at: clock.next(),
    };
    vault.save_transcript(&transcript).unwrap();

    let voiceprint_id: VoiceprintId = ids.next();
    let voiceprint = Voiceprint {
        id: voiceprint_id,
        name: format!("Fixture voice {tag}"),
        email: Some(format!("voice-{tag}@example.com")),
        is_owner: false,
        model: "fixture-embedding-model".into(),
        centroids: vec![vec![0.1, 0.2, 0.3]],
        samples: 1,
        created_at: clock.next(),
        updated_at: clock.next(),
    };
    vault.save_voiceprint(&voiceprint).unwrap();

    // ---- profile: the vault-wide singleton --------------------------------

    let mut profile = vault.profile().unwrap_or_default();
    profile.first_name = "Fixture".into();
    profile.last_name = format!("Person {tag}");
    profile.gender = "unspecified".into();
    profile.location = "Fixtureland".into();
    profile.about = format!("A fixture profile, generation {tag}.");
    profile.updated_at = Some(clock.next());
    vault.save_profile(&profile).unwrap();

    blob_bytes
}

/// The fixed bytes [`build`] stores as the one entry's attachment, for
/// generation `base`. Exposed so a caller can check [`Vault::blob`] against
/// them independently of the JSON dump -- see `frozen_vault.rs`.
pub fn blob_bytes_for(base: u128) -> Vec<u8> {
    format!("fixture attachment bytes for generation {base:x}").into_bytes()
}

/// A fixed civil date, distinct per generation but always the same for a
/// given `base` -- what entries, readings, logs and events are filed under.
fn fixture_date(base: u128) -> Date {
    // 2024-01-01 plus `base` days, wrapped into the range `jiff::civil::Date`
    // accepts. The exact day carries no meaning; only that it is fixed.
    let day_of_year = 1 + (base % 300) as i16;
    Date::new(2024, 1, 1)
        .unwrap()
        .checked_add(jiff::Span::new().days(i64::from(day_of_year)))
        .unwrap()
}
