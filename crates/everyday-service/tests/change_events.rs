//! Phase 0.4 of `docs/plans/architecture-refactor.md`: the change events
//! each command -- and each assistant tool, and the scheduler's own tick --
//! actually emits, pinned.
//!
//! "The one rule" that plan states is that a refactor may not change "the
//! number, kind, ids or origin of change events". Phase 7 is explicitly
//! named as the risk this net exists to catch: it moves the nine
//! hand-written `events().changed(..)` calls in `domains/`, and the
//! scheduler's own, behind one emitter. That is exactly the kind of change
//! that looks safe and silently drops, doubles or reorders an event -- so
//! this test drives every offline-reachable write, in a fixed order, and
//! snapshots what came out the other side: `(command or tool, [(kind, op, id
//! present?, ids.len, origin), ...])`, in the order it happened.
//!
//! Deliberately *not* what surface.rs's `placeholder` does: a generic value
//! that merely satisfies a type is enough to prove a command's signature
//! agrees with its struct, but it is not enough to make a write actually
//! land -- a `save_task` with a made-up `projectId` fails before anything is
//! touched. So this seeds real records first, mostly through the very
//! `new_*`/`save_*` command pairs the interface itself uses (`new_journal`
//! then `save_journal`, and so on), and only reaches for the vault directly
//! where there is no command that mints the thing a later command needs --
//! a `Thread`, which only exists as the aggregate `MailStore::ingest`
//! computes, or an `Account`, which nothing but a real sign-in otherwise
//! creates.
//!
//! # What is skipped, and why
//!
//! Only a write command that can complete against a plain, unlocked, offline
//! vault is run here. Skipped, each for a reason that would not change by
//! trying harder in this test file, are:
//!
//! - **OAuth**: `begin_oauth_sign_in`, `cancel_oauth_sign_in`,
//!   `attach_oauth_sign_in` -- a loopback redirect to a real provider.
//! - **A real account's own sync**: `sync_account` (starts a supervised sync
//!   task against a real IMAP/Gmail/Graph server).
//! - **A remote calendar**: `sync_calendar`, `sync_due_calendars`,
//!   `subscribe_calendar`, `subscribe_account_calendar` -- each fetches a
//!   feed over the network. (`import_calendar` is *not* skipped: it parses
//!   ICS text the caller already has in hand, no socket involved.)
//! - **Fetching something remote**: `fetch_attachment` (a real synced pack),
//!   `fetch_image` (an HTTP request).
//! - **`respond_to_invite`**: needs a message carrying a real,
//!   ICS-bearing invite written into the local pack store -- more raw-byte
//!   plumbing than this net is trying to buy -- and its own reply queues an
//!   outbound send.
//! - **A model download**: `download_speech_model` (a real ~40MB fetch).
//!   `cancel_speech_model_download` is *not* skipped: cancelling a download
//!   that was never started is a harmless, fully local no-op, and its
//!   `change:` still fires.
//! - **The meeting-notes pipeline**: `begin_recording` refuses outright
//!   unless the configured transcriber's speech kit is genuinely present on
//!   disk (see `tests/meetings_spool.rs`'s own doc on exactly this).
//!   `finish_recording` and `retry_recording` move a seeded recording into
//!   that same pipeline, which continues as *detached background work* once
//!   the call returns -- driving it deeper here would mean this test's own
//!   event capture racing that background task, which is precisely the
//!   nondeterminism this net must not have. `enrol_voice` needs the speech
//!   kit to turn PCM into an embedding, and `name_speaker` needs a
//!   transcript, which only a finished recording produces.
//!   (`append_recording_chunk` is *not* skipped: it only spools bytes
//!   against an already-`Recording`-stage row seeded directly, the same way
//!   `tests/meetings_spool.rs` does.)
//! - **`send_message`**: `streams: true`, and therefore unreachable through
//!   `Service::call` at all -- see `tests/call.rs`'s
//!   `a_streaming_command_cannot_be_called_for_a_value`. The assistant's own
//!   write path is still covered here, through `run_tool` and through the
//!   scheduler's execution of a due routine below.
//!
//! Every assistant write *tool* that exists is run, offline: none of the 28
//! need anything this list above does not already avoid.
//!
//! # Determinism
//!
//! Everything here runs on one task, awaited in a fixed order, against a
//! handful of freshly created temp-directory vaults -- there is no timer, no
//! real socket and no background task left uncollected by the time this
//! test's assertions run, which is what makes running it three times in a
//! row byte-identical. Two places background work could race this test's
//! own capture, and both are why this file is careful rather than simpler:
//!
//! - The meeting-notes pipeline past `finish_recording` -- skipped above.
//! - `save_account_password` on a mail-enabled account starts a real sync
//!   task (`crate::mailsync::wiring::ensure_account_task`). Starting one is
//!   synchronous and harmless -- `Supervisor::start` raises its own
//!   `backgroundTask` change inline, before the task is even spawned -- but
//!   that task's eventual *completion* runs on a separately spawned future
//!   and raises a second `backgroundTask` change whenever the runtime gets
//!   to it, which is not a promise this test can keep pinned to one place
//!   in the order. An earlier version of this file learned that the hard
//!   way: it passed locally and then failed on a bare rerun, with the same
//!   two events present but reordered. The fix is below, not a skip: the
//!   account this test runs `save_account`/`save_account_password`/
//!   `set_agent_access` against never carries mail, and a *second* account
//!   -- seeded straight into the vault, never through a command -- is what
//!   every mail command and mail tool below actually uses. Nothing in this
//!   test ever asks the supervisor to start anything, so there is nothing
//!   left for it to finish.
//!
//! To accept a change: `UPDATE_SURFACE=1 cargo test -p everyday-service
//! --test change_events`.

#[allow(dead_code)]
mod support;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine;
use everyday_core::account::{Account, Provider};
use everyday_core::agent::tools::Effect;
use everyday_core::id::{MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use everyday_core::meeting::{MeetingSettings, Recording};
use everyday_core::packstore::PackRef;
use everyday_core::proposal::{Payload, ProposedRecord};
use everyday_core::routine::Trigger;
use everyday_core::store::mail::IngestMessage;
use everyday_core::{AgentSettings, Note, Profile, Proposal};
use everyday_service::Service;
use everyday_service::command;
use everyday_service::ctx::Ctx;
use everyday_service::events::{Change, EventSink};
use everyday_service::scheduler;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("change_events.json")
}

// ── recording ────────────────────────────────────────────────────────────

/// The sink every service in this test shares, so one ordered log covers
/// every vault it is pointed at. Nothing here reads a vault or holds one:
/// it only ever sees what a [`Change`] says, which is the whole point --
/// this is a wire-level net, not a storage one (`0.3` is that one).
#[derive(Default)]
struct Harness {
    /// Events raised since the last [`Harness::take`], by whichever
    /// service this is currently the sink for.
    pending: Mutex<Vec<Value>>,
    commands: Mutex<Vec<Value>>,
    tools: Mutex<Vec<Value>>,
    /// Every write command actually driven through [`run`] or
    /// [`run_tool_call`] (the latter always adds `"run_tool"` itself, since
    /// that is the command a tool call goes through) -- checked at the end
    /// against every `effect: Write` row in [`command::catalog`], so a
    /// command nobody remembered to add here, or to [`SKIPPED_COMMANDS`],
    /// fails the test by name rather than by a silently incomplete
    /// snapshot.
    executed: Mutex<HashSet<String>>,
}

impl EventSink for Harness {
    fn changed(&self, change: Change) {
        // Ids are random every run; only their presence and count are part
        // of the contract this pins. `kind` and `op` serialise through their
        // own `Serialize` impl, the same way `wire_names.rs` reads them, so
        // this can never spell one differently from what a real client sees.
        self.pending.lock().unwrap().push(json!({
            "kind": change.kind,
            "op": change.op,
            "idPresent": change.id.is_some(),
            "idsLen": change.ids.len(),
            "origin": change.origin,
        }));
    }
}

impl Harness {
    fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }
}

/// Call a write command with a known origin (`Ctx::local`, "local" on the
/// wire), and record what it announced under its own name, in order.
async fn run(svc: &Arc<Service>, h: &Arc<Harness>, name: &str, args: Value) -> Value {
    let result = svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"));
    let events = h.take();
    h.executed.lock().unwrap().insert(name.to_string());
    h.commands.lock().unwrap().push(json!({ "command": name, "events": events }));
    result
}

/// Call a *read* command -- minting a blank record (`new_journal` and
/// friends), or listing what already exists -- that this test only needs to
/// get a real id to build a write's arguments from. Not logged and not
/// marked executed: `surface.json`'s own `effect` says these are reads, and
/// this snapshot is about writes. (`take` still drains whatever the sink
/// saw, on the off chance a "read" is not one -- `tests/call.rs`'s own
/// `a_read_announces_nothing` is what says that never happens, not an
/// assumption this file repeats.)
async fn call(svc: &Arc<Service>, h: &Arc<Harness>, name: &str, args: Value) -> Value {
    let result = svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"));
    let leftover = h.take();
    assert!(leftover.is_empty(), "{name}: a read command announced a change: {leftover:?}");
    result
}

/// Run one assistant tool through `run_tool`, with no model in the loop --
/// what a palette entry or a keyboard shortcut does. Recorded under the
/// tool's own name, not `run_tool`'s, since that is the name a reader of
/// this snapshot actually cares about; `run_tool` itself is still marked
/// executed, satisfying the self-check below for that command's own row.
async fn run_tool_call(
    svc: &Arc<Service>,
    h: &Arc<Harness>,
    name: &str,
    arguments: Value,
) -> Value {
    let result = svc
        .call(Ctx::local(), "run_tool", json!({ "name": name, "arguments": arguments }))
        .await
        .unwrap_or_else(|e| panic!("run_tool({name}): {e}"));
    let events = h.take();
    h.executed.lock().unwrap().insert("run_tool".to_string());
    h.tools.lock().unwrap().push(json!({ "tool": name, "events": events }));
    result
}

/// Write commands this test does not, and cannot usefully, run -- see the
/// module doc for the reasoning behind each. Checked for completeness (every
/// `effect: Write` row is here or was run) and for staleness (nothing here
/// was, despite that, actually run) at the bottom of the test.
const SKIPPED_COMMANDS: &[(&str, &str)] = &[
    ("attach_oauth_sign_in", "OAuth: attaches a real sign-in's tokens to an account"),
    ("begin_oauth_sign_in", "OAuth: a loopback redirect to a real provider"),
    ("begin_recording", "refuses unless the transcriber's speech kit is genuinely on disk"),
    ("cancel_oauth_sign_in", "OAuth: cancels the sign-in begin_oauth_sign_in above needs"),
    ("download_speech_model", "a real ~40MB network download"),
    ("enrol_voice", "needs the speech/voice model kit to embed real audio"),
    ("fetch_attachment", "needs a message's raw bytes from a real synced pack"),
    ("fetch_image", "fetches a remote image over HTTP"),
    ("finish_recording", "moves the recording into the transcription pipeline: background work"),
    ("name_speaker", "needs a transcript, which only a finished recording produces"),
    ("respond_to_invite", "needs an ICS-bearing message in the local pack store, and sends"),
    ("retry_recording", "resumes the same background pipeline finish_recording does"),
    ("send_message", "streams: true, unreachable through Service::call at all"),
    ("subscribe_account_calendar", "fetches a remote account's calendar over the network"),
    ("subscribe_calendar", "fetches a remote ICS feed over the network"),
    ("sync_account", "starts a supervised sync task against a real mail server"),
    ("sync_calendar", "refreshes one calendar from its remote source"),
    ("sync_due_calendars", "refreshes every subscribed calendar from its remote source"),
];

fn today() -> String {
    Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date().to_string()
}

fn pcm_base64(len: usize) -> String {
    let bytes = vec![0u8; len];
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── the big scripted sequence ───────────────────────────────────────────

#[tokio::test]
async fn every_offline_write_announces_what_was_written_down() {
    let h = Arc::new(Harness::default());
    let (svc, _dir) = support::vault::service(None);
    svc.set_events(h.clone());

    // ---- journals, entries, notes -----------------------------------
    let journals = call(&svc, &h, "list_journals", json!({})).await;
    let journal_id = journals[0]["id"].clone();

    let minted = call(&svc, &h, "new_journal", json!({ "name": "Field notes" })).await;
    run(&svc, &h, "save_journal", json!({ "journal": minted })).await;

    let mut entry = call(&svc, &h, "new_entry", json!({ "journalId": journal_id })).await;
    entry["title"] = json!("Rained all afternoon");
    let entry_id = entry["id"].clone();
    run(&svc, &h, "save_entry", json!({ "entry": entry, "expect": null })).await;

    let mut entry2 = call(&svc, &h, "new_entry", json!({ "journalId": journal_id })).await;
    entry2["title"] = json!("Forced through anyway");
    run(&svc, &h, "save_entry_force", json!({ "entry": entry2 })).await;

    let mut note = call(&svc, &h, "new_note", json!({})).await;
    note["title"] = json!("Sailing rig notes");
    let note_id = note["id"].clone();
    run(&svc, &h, "save_note", json!({ "note": note, "expect": null })).await;

    let mut note2 = call(&svc, &h, "new_note", json!({})).await;
    note2["title"] = json!("Forced note");
    run(&svc, &h, "save_note_force", json!({ "note": note2 })).await;

    run(&svc, &h, "save_profile", json!({ "profile": Profile::default() })).await;

    // ---- tasks, projects, blocks --------------------------------------
    let project = call(&svc, &h, "new_project", json!({ "name": "Renovate" })).await;
    let project_id = project["id"].clone();
    run(&svc, &h, "save_project", json!({ "project": project })).await;

    let mut task = call(
        &svc,
        &h,
        "new_task",
        json!({ "projectId": project_id, "parentId": null, "status": null }),
    )
    .await;
    task["title"] = json!("Buy paint");
    let task_id = task["id"].clone();
    run(&svc, &h, "save_task", json!({ "task": task })).await;

    let mut task2 = call(
        &svc,
        &h,
        "new_task",
        json!({ "projectId": project_id, "parentId": null, "status": null }),
    )
    .await;
    task2["title"] = json!("Choose colour");
    run(&svc, &h, "save_tasks", json!({ "tasks": [task2] })).await;

    let block = call(
        &svc,
        &h,
        "new_block",
        json!({ "subject": { "type": "adhoc" }, "start": Timestamp::now().to_string(), "minutes": 30 }),
    )
    .await;
    run(&svc, &h, "save_block", json!({ "block": block })).await;

    // ---- purpose: roles and goals ---------------------------------------
    let role = call(&svc, &h, "new_role", json!({ "name": "Homeowner" })).await;
    let role_id = role["id"].clone();
    run(&svc, &h, "save_role", json!({ "role": role })).await;

    let goal =
        call(&svc, &h, "new_goal", json!({ "roleId": role_id, "title": "A tidy house" })).await;
    let goal_id = goal["id"].clone();
    run(&svc, &h, "save_goal", json!({ "goal": goal })).await;

    let goal2 =
        call(&svc, &h, "new_goal", json!({ "roleId": role_id, "title": "A tidier house" })).await;
    run(&svc, &h, "save_goals", json!({ "goals": [goal2] })).await;

    run(&svc, &h, "seed_roles", json!({})).await;

    // ---- trackers --------------------------------------------------------
    let tracker = call(&svc, &h, "new_tracker", json!({ "name": "Steps", "kind": "check" })).await;
    let tracker_id = tracker["id"].clone();
    run(&svc, &h, "save_tracker", json!({ "tracker": tracker })).await;

    let mut reading = run(
        &svc,
        &h,
        "log_reading",
        json!({ "trackerId": tracker_id, "value": 1, "date": today(), "at": null, "journalId": null, "entryId": null }),
    )
    .await;
    reading["value"] = json!(0);
    run(&svc, &h, "save_reading", json!({ "reading": reading })).await;

    // ---- library: shelves and items --------------------------------------
    let kind = call(&svc, &h, "new_kind", json!({ "name": "Books", "singular": "Book" })).await;
    let kind_id = kind["id"].clone();
    run(&svc, &h, "save_kind", json!({ "kind": kind })).await;

    let added1 =
        run(&svc, &h, "add_item", json!({ "kindId": kind_id, "title": "Dune", "lookup": false }))
            .await;
    let mut item1 = added1["item"].clone();
    let item1_id = item1["id"].clone();
    item1["title"] = json!("Dune (revised)");
    run(&svc, &h, "save_item", json!({ "item": item1 })).await;

    let added2 = run(
        &svc,
        &h,
        "add_item",
        json!({ "kindId": kind_id, "title": "Dune Messiah", "lookup": false }),
    )
    .await;
    run(&svc, &h, "save_items", json!({ "items": [added2["item"]] })).await;

    run(&svc, &h, "set_item_status", json!({ "id": item1_id, "status": "active", "log": false }))
        .await;
    run(
        &svc,
        &h,
        "set_item_progress",
        json!({ "id": item1_id, "position": 10, "total": 100, "log": false }),
    )
    .await;

    let log_entry =
        call(&svc, &h, "new_log", json!({ "itemId": item1_id, "event": "started" })).await;
    run(&svc, &h, "save_log", json!({ "log": log_entry })).await;

    run(
        &svc,
        &h,
        "apply_metadata",
        json!({ "id": item1_id, "result": { "title": "A better title" }, "overwrite": false }),
    )
    .await;

    // ---- accounts ---------------------------------------------------------
    //
    // `services.mail` is left off on this one: `save_account_password` would
    // otherwise start a real sync task through `ensure_account_task`, whose
    // *completion* -- not its start, which fires a `backgroundTask` change
    // synchronously and harmlessly inside `Supervisor::start` -- runs on a
    // separately spawned task and lands its own `backgroundTask` change
    // whenever the scheduler gets to it. That raced this very test: the
    // same run produced a different event order from one invocation to the
    // next. So this account exists only to exercise `save_account`,
    // `save_account_password` and `set_agent_access` themselves, and never
    // carries mail -- a second account below carries it instead, seeded
    // straight into the vault, never through a command, so nothing here
    // ever asks the supervisor to start anything.
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = false;
    let account_id = account.id;
    let account_value = serde_json::to_value(&account).unwrap();
    run(&svc, &h, "save_account", json!({ "account": account_value.clone() })).await;
    run(
        &svc,
        &h,
        "save_account_password",
        json!({ "id": account_id.to_string(), "password": "hunter2hunter2" }),
    )
    .await;
    run(
        &svc,
        &h,
        "set_agent_access",
        json!({
            "id": account_id.to_string(),
            "caller": "assistant",
            "access": { "read": true, "draft": true, "edit": true, "remove": true, "archive": true, "send": false },
        }),
    )
    .await;

    // ---- mail: a real thread, seeded directly ------------------------------
    //
    // There is no `new_thread`/`save_thread` command -- a `Thread` is an
    // aggregate `MailStore::ingest` computes from a `Message`, never a row
    // anything mints on its own -- so this reaches for the vault the same
    // way `tests/mail_outbox.rs` and `tests/meetings_spool.rs` seed what a
    // command needs but cannot itself create. The account is seeded the
    // same way, straight into the vault: `agent::tools::mail::account_eligible`
    // requires `services.mail` for the tools section below to see any mail
    // tool at all, and seeding it here rather than through `save_account`
    // is what keeps the supervisor out of this test entirely -- see the
    // comment above.
    let vault = svc.get().unwrap();
    let mut mail_account = Account::new(Provider::Custom, "inbox@example.com");
    mail_account.services.mail = true;
    let mail_account_id = mail_account.id;
    vault.save_account(&mail_account).unwrap();
    let inbox_id = {
        let mailbox = Mailbox::new(mail_account_id, "INBOX", MailboxRole::Inbox);
        let id = mailbox.id;
        vault.save_mailbox(&mailbox).unwrap();
        id
    };
    let archive_id = {
        let mailbox = Mailbox::new(mail_account_id, "Archive", MailboxRole::Archive);
        let id = mailbox.id;
        vault.save_mailbox(&mailbox).unwrap();
        id
    };
    let thread_id = seed_thread(&vault, mail_account_id, inbox_id, 1, "A message");

    run(&svc, &h, "mark_read", json!({ "threads": [thread_id.to_string()] })).await;
    run(&svc, &h, "mark_unread", json!({ "threads": [thread_id.to_string()] })).await;
    run(&svc, &h, "star", json!({ "threads": [thread_id.to_string()] })).await;
    run(&svc, &h, "unstar", json!({ "threads": [thread_id.to_string()] })).await;
    run(&svc, &h, "label", json!({ "threads": [thread_id.to_string()], "label": "Important" }))
        .await;
    run(&svc, &h, "unlabel", json!({ "threads": [thread_id.to_string()], "label": "Important" }))
        .await;
    run(
        &svc,
        &h,
        "set_thread_category",
        json!({ "threads": [thread_id.to_string()], "category": "important" }),
    )
    .await;
    let until = (Timestamp::now() + SignedDuration::from_hours(2)).to_string();
    run(&svc, &h, "snooze", json!({ "threads": [thread_id.to_string()], "until": until })).await;
    run(&svc, &h, "unsnooze", json!({ "threads": [thread_id.to_string()] })).await;
    run(
        &svc,
        &h,
        "move_to_mailbox",
        json!({ "threads": [thread_id.to_string()], "to": archive_id.to_string() }),
    )
    .await;
    run(&svc, &h, "archive", json!({ "threads": [thread_id.to_string()] })).await;
    run(&svc, &h, "trash", json!({ "threads": [thread_id.to_string()] })).await;

    run(
        &svc,
        &h,
        "allow_remote_images",
        json!({ "sender": "someone@example.com", "domain": null, "messageId": null }),
    )
    .await;
    run(
        &svc,
        &h,
        "revoke_remote_image_allowance",
        json!({ "sender": "someone@example.com", "domain": null }),
    )
    .await;
    // Both real, local sweeps over what is already stored -- neither talks
    // to a server -- and both are `INVISIBLE` in `command.rs`: their own
    // `change:` is `None`, so an empty `events` array below is the correct
    // recording, not a gap in one.
    run(&svc, &h, "recategorize_mail", json!({})).await;
    run(&svc, &h, "rebuild_mail_index", json!({ "id": null })).await;

    // ---- drafts -----------------------------------------------------------
    let mut draft = run(&svc, &h, "new_draft", json!({ "account": mail_account_id.to_string(), "inReplyTo": null, "forwardOf": null, "replyAll": false })).await;
    draft["to"] = json!([{ "name": "", "email": "someone@example.com" }]);
    let draft_id = draft["id"].clone();
    run(&svc, &h, "save_draft", json!({ "draft": draft })).await;
    run(&svc, &h, "send_draft", json!({ "id": draft_id, "delaySeconds": 120, "sendAt": null }))
        .await;
    run(&svc, &h, "undo_send", json!({ "draftId": draft_id })).await;

    let mut draft2 = run(&svc, &h, "new_draft", json!({ "account": mail_account_id.to_string(), "inReplyTo": null, "forwardOf": null, "replyAll": false })).await;
    draft2["to"] = json!([{ "name": "", "email": "someone-else@example.com" }]);
    let draft2_id = draft2["id"].clone();
    run(&svc, &h, "save_draft", json!({ "draft": draft2 })).await;
    run(&svc, &h, "discard_draft", json!({ "id": draft2_id })).await;

    // ---- calendars ---------------------------------------------------------
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20260914T140000Z\r\nDTEND:20260914T150000Z\r\nSUMMARY:Test Event\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let calendar_info = run(
        &svc,
        &h,
        "import_calendar",
        json!({ "name": "Test Calendar", "label": "work", "color": "#336699", "ics": ics }),
    )
    .await;
    // `CalendarInfo` is `#[serde(flatten)]` over the `Calendar` itself, plus
    // an `events` count beside it -- there is no nested `"calendar"` key.
    let mut calendar = calendar_info.clone();
    calendar.as_object_mut().unwrap().remove("events");
    let calendar_id = calendar["id"].clone();
    calendar["color"] = json!("#112233");
    run(&svc, &h, "save_calendar", json!({ "calendar": calendar })).await;

    // ---- meetings -----------------------------------------------------------
    let meeting_settings = MeetingSettings { enabled: false, ..Default::default() };
    run(
        &svc,
        &h,
        "save_meeting_settings",
        json!({ "settings": serde_json::to_value(&meeting_settings).unwrap() }),
    )
    .await;
    run(&svc, &h, "set_transcriber_key", json!({ "key": "fake-key" })).await;
    run(&svc, &h, "cancel_speech_model_download", json!({ "id": "nonexistent-model" })).await;
    run(
        &svc,
        &h,
        "dismiss_meeting_offer",
        json!({ "calendarId": calendar_id, "uid": "uid-1", "series": null, "never": true }),
    )
    .await;

    let recording_id = {
        let recording = Recording::new("Design sync", None, everyday_core::TemplateId::new());
        let id = recording.id;
        vault.save_recording(&recording).unwrap();
        id
    };
    run(
        &svc,
        &h,
        "append_recording_chunk",
        json!({
            "id": recording_id.to_string(),
            "track": "mic",
            "seq": 0,
            "startMs": 0,
            "pcm": pcm_base64(1_600),
        }),
    )
    .await;

    // ---- assistant settings and memory --------------------------------------
    let agent_settings = AgentSettings { enabled: false, ..Default::default() };
    run(
        &svc,
        &h,
        "save_agent_settings",
        json!({ "settings": serde_json::to_value(&agent_settings).unwrap() }),
    )
    .await;
    run(&svc, &h, "set_agent_key", json!({ "key": "sk-test-key" })).await;
    run(&svc, &h, "clear_agent_key", json!({})).await;
    run(&svc, &h, "set_quick_job", json!({ "name": "todo.parse", "on": true })).await;

    let mut memory = call(&svc, &h, "new_memory", json!({})).await;
    memory["text"] = json!("Prefers concise replies.");
    let memory_id = memory["id"].clone();
    run(&svc, &h, "save_memory", json!({ "memory": memory })).await;
    run(
        &svc,
        &h,
        "set_memory_origin",
        // Only a dream may set `inferred` -- `Vault::set_memory_origin`
        // refuses that by hand -- so this exercises the one transition a
        // person actually makes: striking a told memory out.
        json!({ "id": memory_id, "origin": "rejected" }),
    )
    .await;

    // ---- routines and runs ---------------------------------------------------
    let mut routine = call(&svc, &h, "new_routine", json!({})).await;
    routine["name"] = json!("Evening review");
    routine["instructions"] = json!("Summarise the day.");
    let routine_id = routine["id"].clone();
    run(&svc, &h, "save_routine", json!({ "routine": routine })).await;

    // `run_routine` only queues -- see its own doc in
    // `domains::routines`: runs happen one at a time, off the scheduler's
    // own tick, never inline in the command. So this is offline and
    // synchronous, with nothing left running once it returns.
    let queued_run = run(&svc, &h, "run_routine", json!({ "id": routine_id })).await;
    let run_id = queued_run["id"].clone();
    run(&svc, &h, "mark_runs_seen", json!({ "ids": [run_id] })).await;

    // Nothing is parked on this id -- `Pending::answer` says so plainly
    // rather than erroring (see `crate::agent::Pending`'s own unit test),
    // which is what makes this reachable with no in-flight turn at all.
    run(
        &svc,
        &h,
        "confirm_tool_call",
        json!({ "callId": "nonexistent-call-id", "approved": true, "later": false }),
    )
    .await;

    // ---- proposals -------------------------------------------------------
    let proposal1 = seed_note_proposal(&vault, "Trip notes");
    run(&svc, &h, "accept_proposal", json!({ "id": proposal1.id.to_string() })).await;

    let proposal2 = seed_note_proposal(&vault, "Second trip");
    run(
        &svc,
        &h,
        "decline_proposal",
        json!({ "id": proposal2.id.to_string(), "reason": { "type": "wrongTime" } }),
    )
    .await;

    let proposal3 = seed_note_proposal(&vault, "Third trip");
    run(&svc, &h, "mark_proposals_seen", json!({ "ids": [proposal3.id.to_string()] })).await;

    // ---- journal housekeeping, none of it lock-state changing ---------------
    run(&svc, &h, "touch", json!({})).await;
    run(&svc, &h, "poll_auto_lock", json!({})).await;
    run(&svc, &h, "set_auto_lock", json!({ "seconds": 900 })).await;
    run(&svc, &h, "set_forget_key", json!({ "seconds": 0 })).await;
    run(&svc, &h, "flush", json!({})).await;

    // ---- the assistant's write tools, through run_tool -----------------------
    //
    // Every one of them, seeded from the records above: none needs the
    // network, an account, a model provider or a microphone. Most do not
    // announce anything at all through this door -- `run_tool`'s own
    // `mail_tool_change` only names an event for the ten mail tools it
    // lists, which is today's real behaviour and precisely what phase 7 is
    // not allowed to change quietly. That asymmetry, not a mistake in this
    // test, is why most entries below carry an empty `events` array.
    run_tool_call(
        &svc,
        &h,
        "create_entry",
        json!({ "journal_id": journal_id, "body": "Written by a tool." }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "update_entry",
        json!({ "entry_id": entry_id, "title": "Updated by a tool" }),
    )
    .await;
    run_tool_call(&svc, &h, "create_goal", json!({ "role_id": role_id, "title": "A tool's goal" }))
        .await;
    run_tool_call(
        &svc,
        &h,
        "update_goal",
        json!({ "goal_id": goal_id, "title": "A tool's renamed goal" }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "set_purpose",
        json!({ "kind": "task", "id": task_id, "role_id": role_id }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "create_item",
        json!({ "shelf_id": kind_id, "title": "A tool's item" }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "update_item",
        json!({ "item_id": item1_id, "title": "A tool's renamed item" }),
    )
    .await;
    run_tool_call(&svc, &h, "create_note", json!({ "body": "A tool's note." })).await;
    run_tool_call(
        &svc,
        &h,
        "update_note",
        json!({ "note_id": note_id, "title": "A tool's renamed note" }),
    )
    .await;
    let tool_project =
        run_tool_call(&svc, &h, "create_project", json!({ "name": "Tool project" })).await;
    let tool_project_id = tool_project["id"].clone();
    run_tool_call(
        &svc,
        &h,
        "update_project",
        json!({ "project_id": tool_project_id, "name": "Tool project renamed" }),
    )
    .await;
    let tool_task = run_tool_call(
        &svc,
        &h,
        "create_task",
        json!({ "title": "A tool's task", "project_id": project_id }),
    )
    .await;
    let tool_task_id = tool_task["id"].clone();
    run_tool_call(
        &svc,
        &h,
        "update_task",
        json!({ "task_id": tool_task_id, "title": "A tool's renamed task" }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "create_time_block",
        json!({ "date": today(), "start_time": "09:00", "end_time": "09:30", "label": "Focus" }),
    )
    .await;
    run_tool_call(&svc, &h, "log_reading", json!({ "tracker_id": tracker_id, "value": 1 })).await;
    let tool_routine = run_tool_call(
        &svc,
        &h,
        "create_routine",
        json!({ "name": "Tool routine", "instructions": "Check the inbox.", "at": "08:30" }),
    )
    .await;
    let tool_routine_id = tool_routine["id"].clone();
    run_tool_call(
        &svc,
        &h,
        "update_routine",
        json!({ "routine_id": tool_routine_id, "name": "Tool routine renamed" }),
    )
    .await;
    run_tool_call(&svc, &h, "run_routine", json!({ "routine_id": tool_routine_id })).await;
    run_tool_call(&svc, &h, "remember", json!({ "fact": "Prefers short summaries." })).await;

    // A second thread and message, kept apart from the mail commands' own
    // above so a tool run here is never the second write to an
    // already-archived-and-trashed thread -- nothing about `apply_thread_ops`
    // requires that separation, but nothing about this test should have to
    // reason about it either.
    let thread2_id = seed_thread(&vault, mail_account_id, inbox_id, 2, "A second message");
    let message2_id = {
        let messages = vault.thread(thread2_id).unwrap().1;
        messages[0].id
    };
    run_tool_call(&svc, &h, "archive_thread", json!({ "thread_id": thread2_id.to_string() })).await;
    run_tool_call(
        &svc,
        &h,
        "mark_read",
        json!({ "thread_id": thread2_id.to_string(), "read": true }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "move_thread",
        json!({ "thread_id": thread2_id.to_string(), "to": "archive" }),
    )
    .await;
    let snooze_until = (Timestamp::now() + SignedDuration::from_hours(2)).to_string();
    run_tool_call(
        &svc,
        &h,
        "snooze_thread",
        json!({ "thread_id": thread2_id.to_string(), "until": snooze_until }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "label_thread",
        json!({ "thread_id": thread2_id.to_string(), "label": "Follow up" }),
    )
    .await;
    run_tool_call(&svc, &h, "trash_thread", json!({ "thread_id": thread2_id.to_string() })).await;
    let drafted = run_tool_call(
        &svc,
        &h,
        "draft_message",
        json!({
            "account_id": mail_account_id.to_string(),
            "to": ["someone@example.com"],
            "subject": "Hi",
            "body_html": "<p>hi</p>",
        }),
    )
    .await;
    let tool_draft_id = drafted["id"].clone();
    run_tool_call(
        &svc,
        &h,
        "update_draft",
        json!({ "draft_id": tool_draft_id, "subject": "Updated subject" }),
    )
    .await;
    run_tool_call(
        &svc,
        &h,
        "draft_reply",
        json!({ "message_id": message2_id.to_string(), "body_html": "<p>reply</p>" }),
    )
    .await;

    // ---- the isolated lock-and-password block -------------------------
    //
    // Its own vault, because `lock`, `unlock` and `change_password` alter
    // exactly the state every command above depends on: run them against
    // the shared vault and every later call would be reasoning about a
    // vault that just changed keys or shut its door.
    {
        let (pw_svc, _pw_dir) = support::vault::service(Some("correct horse"));
        pw_svc.set_events(h.clone());
        run(&pw_svc, &h, "verify_password", json!({ "password": "correct horse" })).await;
        run(
            &pw_svc,
            &h,
            "change_password",
            json!({ "current": "correct horse", "next": "new password stronger" }),
        )
        .await;
        run(&pw_svc, &h, "lock", json!({})).await;
        run(&pw_svc, &h, "unlock", json!({ "password": "new password stronger" })).await;
    }

    // ---- the scheduler: one tick, a due routine and an expiring proposal ----
    let scheduler_events = run_scheduler_tick_section().await;

    // ---- completeness: every write command was run, or was named above ----
    let executed = h.executed.lock().unwrap().clone();
    let mut missing = Vec::new();
    for cmd in command::catalog() {
        if cmd.effect != Effect::Write {
            continue;
        }
        let skipped = SKIPPED_COMMANDS.iter().any(|(n, _)| *n == cmd.name);
        if !executed.contains(cmd.name) && !skipped {
            missing.push(cmd.name);
        }
    }
    assert!(
        missing.is_empty(),
        "write commands neither run by this test nor listed in SKIPPED_COMMANDS: {missing:?}"
    );
    for (name, reason) in SKIPPED_COMMANDS {
        assert!(
            !executed.contains(*name),
            "{name} is listed as skipped ({reason}) but this test actually ran it; \
             move it out of SKIPPED_COMMANDS"
        );
    }

    // ---- the snapshot -------------------------------------------------------
    let mut skipped_sorted: Vec<&(&str, &str)> = SKIPPED_COMMANDS.iter().collect();
    skipped_sorted.sort_by_key(|(name, _)| *name);

    let doc = json!({
        "commands": h.commands.lock().unwrap().clone(),
        "skippedCommands": skipped_sorted
            .iter()
            .map(|(name, reason)| json!({ "name": name, "reason": reason }))
            .collect::<Vec<_>>(),
        "tools": h.tools.lock().unwrap().clone(),
        "schedulerTick": scheduler_events,
    });
    let current = format!("{}\n", serde_json::to_string_pretty(&doc).unwrap());

    support::compare(
        &snapshot_path(),
        &current,
        "the change events a write command or assistant tool announces have changed.\n\n\
         This is phase 0.4 of docs/plans/architecture-refactor.md's regression net: a \
         refactor may not change the number, kind, ids or origin of any change event, so a \
         diff here means production behaviour moved, not just this test.",
        "UPDATE_SURFACE=1 cargo test -p everyday-service --test change_events",
    );
}

/// A real `Thread`, seeded the only way one can be: through
/// `Vault::ingest_mail`, which is what actually computes the aggregate row
/// `apply_thread_ops`-based commands read. `uid` only has to be unique
/// within the mailbox it lands in.
fn seed_thread(
    vault: &everyday_core::Vault,
    account: everyday_core::AccountId,
    mailbox: MailboxId,
    uid: u32,
    subject: &str,
) -> ThreadId {
    let thread_id = ThreadId::new();
    let message_id = MailMessageId::new();
    let message = Message {
        id: message_id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{message_id}@example.com>"),
        date: Timestamp::now(),
        from: Address::bare("sender@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.to_string(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault.ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    thread_id
}

/// A pending "create a note" proposal, exactly `tests/proposals.rs`'s own
/// `seed_note_proposal`: the shared fixture two test files in this crate
/// both build by hand is where the drift starts, and this one is small
/// enough that copying it beside its sibling was simpler than adding a
/// third crate to `support` for one function three tests use.
fn seed_note_proposal(vault: &everyday_core::Vault, title: &str) -> Proposal {
    let proposal = Proposal::new(
        Payload::Create { record: ProposedRecord::Note(Note::written(title, "x")) },
        "Create note",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();
    proposal
}

/// A vault whose assistant is pointed at a scripted local model -- a
/// `TcpListener` on loopback speaking just enough of the Chat Completions
/// shape to answer one turn -- and nothing else, adapted from
/// `tests/routines.rs`'s own `fake_model`/`service`. No key, no real
/// network, no bill; the "provider" this test's own module doc says is
/// fine to depend on is this one, not a real endpoint.
async fn run_scheduler_tick_section() -> Vec<Value> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = stop_rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                let body = scripted_reply("Nothing urgent today.");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    let endpoint = format!("http://127.0.0.1:{port}/v1");

    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    vault.save_journal(&everyday_core::Journal::new("Journal")).unwrap();

    let mut settings = AgentSettings {
        enabled: true,
        // UTC throughout, on the same reasoning `tests/routines.rs` gives:
        // this is about the scheduler, not about the machine running it.
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    settings.provider_config.base_url = Some(endpoint);
    settings.assistant_model.model = "scripted".into();
    vault.save_agent_settings(&settings).unwrap();

    let svc = Arc::new(Service::new());
    let vault = svc.set(vault);
    let h = Arc::new(Harness::default());
    svc.set_events(h.clone());

    // A routine whose moment was a minute ago, so one tick runs it -- the
    // same construction `tests/routines.rs`'s `due_now` uses.
    let now = vault.agent_settings().unwrap().now();
    let then = now.timestamp() - jiff::SignedDuration::from_mins(1);
    let local = then.to_zoned(now.time_zone().clone());
    let mut routine = everyday_core::Routine::new(
        "Morning brief",
        "Say what is due today.",
        Trigger::Schedule {
            at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
            days: vec![],
            day_of_month: None,
        },
    );
    routine.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&routine).unwrap();

    // A proposal already past its own deadline, exactly
    // `tests/proposals.rs`'s `the_sweep_expires_a_pending_proposal_past_its_own_deadline`.
    let mut proposal = seed_note_proposal(&vault, "Overdue");
    proposal.expires_at = Timestamp::now() - jiff::SignedDuration::from_secs(1);
    vault.save_proposal(&proposal).unwrap();

    scheduler::tick(&svc).await;

    // Nothing left running: `resume` (what a due routine's execution calls)
    // awaits the whole turn before `tick` itself returns, and the proposal
    // sweep above is a synchronous pass over already-loaded rows -- so
    // there is nothing to wait for here, and nothing that could still land
    // an event after this point.
    let _ = stop_tx.send(true);

    h.take()
}

/// One fixed reply, as a stream of Chat Completions chunks -- the harness
/// streams every turn, so a plain JSON body is refused on its content type
/// before anything reads it. Trimmed from `tests/routines.rs`'s own `reply`
/// to the one shape this test needs: a plain answer, no tool call.
fn scripted_reply(text: &str) -> String {
    let mut out = String::new();
    let chunk1 = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "scripted",
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text }, "finish_reason": null }],
    });
    out.push_str(&format!("data: {chunk1}\n\n"));
    let chunk2 = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "scripted",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    });
    out.push_str(&format!("data: {chunk2}\n\n"));
    out.push_str("data: [DONE]\n\n");
    out
}
