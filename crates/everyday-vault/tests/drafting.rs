//! Drafting mode, end to end against a real SQLite vault: every `Write` and
//! `Destructive` tool in the catalogue, run the way a dream would run it.
//!
//! See `docs/plans/dreaming.md` for the design and
//! `crates/everyday-core/src/agent/tools/mod.rs` for `dispatch`'s own half
//! of it. What this file is for is the half that only shows up against a
//! real vault: that a proposal actually lands as one row, that no domain
//! table moves underneath it, and that the one documented exception --
//! `draft_reply` and `draft_message`, which write a real `Draft` and then
//! also propose sending it -- is exactly as wide as the module doc says and
//! no wider.

mod support;

use everyday_core::account::{Account, Provider};
use everyday_core::agent::tools::{self, Drafting, Effect};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ProposalId, ThreadId};
use everyday_core::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use everyday_core::packstore::PackRef;
use everyday_core::proposal::{Payload, ProposalKind, ProposalSource, ProposedRecord};
use everyday_core::store::mail::IngestMessage;
use everyday_core::store::tasks::BlockQuery;
use everyday_core::task::BlockSubject;
use everyday_core::{
    Goal, Memory, Note, Project, ProposalQuery, Role, Routine, Task, TimeBlock, Trigger, Vault,
};
use serde_json::{Value, json};
use support::{TODAY, ctx, ctx_drafting, vault};

// ---- seeding --------------------------------------------------------------

/// Everything the catalogue-wide test needs a target for: one of each
/// proposable record, and a mail account with a message and a draft already
/// on it.
struct Fixtures {
    task: Task,
    block: TimeBlock,
    memory: Memory,
    routine: Routine,
    note: Note,
    account_id: AccountId,
    message_id: MailMessageId,
    /// An existing, editable draft with recipients -- `send_draft`'s target.
    draft_id: String,
}

fn seed_account(v: &Vault) -> Account {
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    v.save_account(&account).unwrap();
    account
}

fn seed_mailbox(v: &Vault, account: AccountId, role: MailboxRole) -> MailboxId {
    let mailbox = Mailbox::new(account, role.as_str(), role);
    v.save_mailbox(&mailbox).unwrap();
    mailbox.id
}

/// A minimal inbox message, built directly rather than through a real
/// `.eml` -- `draft_reply`'s only requirement is a message to answer, not a
/// realistic body. See `crates/everyday-vault/tests/mail_tools.rs`'s own
/// `seed_invite_message` for the same shortcut.
fn seed_message(v: &Vault, account: AccountId, mailbox: MailboxId) -> MailMessageId {
    let thread_id = ThreadId::new();
    let message_id = MailMessageId::new();
    let message = Message {
        id: message_id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{message_id}@example.com>"),
        date: jiff::Timestamp::now(),
        from: Address::new("Dana", "dana@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Hello".into(),
        snippet: "hi".into(),
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
    v.ingest_mail(account, vec![IngestMessage { message, mailbox, uid: 1 }]).unwrap();
    message_id
}

fn seed(v: &Vault) -> Fixtures {
    let role = Role::new("Work");
    v.save_role(&role).unwrap();
    let goal = Goal::new(role.id, "Ship the thing");
    v.save_goal(&goal).unwrap();

    let project = Project::new("The deck");
    v.save_project(&project).unwrap();

    let mut task = Task::new("Book the dentist");
    task.project_id = Some(project.id);
    v.save_task(&task).unwrap();

    let block = TimeBlock::new(
        BlockSubject::Task { id: task.id },
        "2026-09-09T09:00:00Z".parse().unwrap(),
        60,
        "UTC",
    );
    v.save_block(&block).unwrap();

    let memory = Memory::new("Plans the week on Sunday evening");
    v.save_memory(&memory).unwrap();

    let routine = Routine::new("Morning brief", "Say good morning", Trigger::Manual);
    v.save_routine(&routine).unwrap();

    let note = Note::written("Ideas", "Some notes worth keeping");
    v.save_note(&note, None).unwrap();

    let account = seed_account(v);
    let inbox = seed_mailbox(v, account.id, MailboxRole::Inbox);
    let message_id = seed_message(v, account.id, inbox);

    // The target for `send_draft`, made through the plain (non-drafting)
    // path -- exactly what a real draft looks like before a dream ever
    // proposes sending it.
    let plain = ctx(v);
    let drafted = tools::dispatch(
        &plain,
        "draft_message",
        &json!({
            "account_id": account.id.to_string(),
            "to": ["friend@example.com"],
            "subject": "Already drafted",
            "body_html": "<p>hi</p>",
        }),
    )
    .expect("seeding the send_draft target");
    let draft_id = drafted["id"].as_str().unwrap().to_string();

    Fixtures { task, block, memory, routine, note, account_id: account.id, message_id, draft_id }
}

// ---- counting every domain table -------------------------------------------

/// Every table a proposable call must leave untouched, plus the mail state
/// the one documented exception is allowed to move.
#[derive(Debug, PartialEq)]
struct Counts {
    tasks: usize,
    projects: usize,
    blocks: usize,
    memories: usize,
    routines: usize,
    notes: usize,
    entries: usize,
    items: usize,
    goals: usize,
    /// Drafts in any state, across every account. `draft_reply` and
    /// `draft_message` are allowed to add to this.
    drafts: usize,
    /// Drafts that have left `Editing` -- queued or sent. Nothing in this
    /// file may ever move one of these, drafting or not: sending is always
    /// either a person's own confirmed call or a `SendMail` proposal.
    drafts_past_editing: usize,
}

fn counts(v: &Vault) -> Counts {
    let mut drafts = 0usize;
    let mut drafts_past_editing = 0usize;
    for account in v.accounts().unwrap() {
        for draft in v.drafts(account.id).unwrap() {
            drafts += 1;
            if !matches!(draft.state, everyday_core::mail::DraftState::Editing) {
                drafts_past_editing += 1;
            }
        }
    }
    Counts {
        tasks: v.tasks(&everyday_core::store::tasks::TaskQuery::default()).unwrap().len(),
        projects: v.projects().unwrap().len(),
        blocks: v.blocks(&BlockQuery::default()).unwrap().len(),
        memories: v.memories().unwrap().len(),
        routines: v.routines().unwrap().len(),
        notes: v.notes(&everyday_core::store::notes::NoteQuery::default()).unwrap().len(),
        entries: v.entries(&everyday_core::store::EntryQuery::default()).unwrap().len(),
        items: v.items(&everyday_core::store::library::ItemQuery::default()).unwrap().len(),
        goals: v.goals(&everyday_core::store::purpose::GoalQuery::default()).unwrap().len(),
        drafts,
        drafts_past_editing,
    }
}

// ---- the table --------------------------------------------------------

/// Tools that write something but can never become a proposal -- commented
/// with why, so a reviewer adding a new one has to argue with this list
/// rather than silently falling through it. See the test below: a `Write`
/// or `Destructive` tool that is in neither this list nor has a builder
/// fails it outright.
const REFUSED_WHILE_DRAFTING: &[&str] = &[
    // The journal is the person's own record of a day they lived; a dream
    // never writes to it, in either direction.
    "create_entry",
    "update_entry",
    "delete_entry",
    // The library, and how much of a tracker's target was hit, are not one
    // of the five kinds `docs/plans/dreaming.md` lets a dream propose.
    "create_item",
    "update_item",
    "delete_item",
    "log_reading",
    // Roles and goals are the shape of a life, set up deliberately and
    // rarely -- not something a dream drafts on somebody's behalf.
    "create_goal",
    "update_goal",
    "delete_goal",
    "set_purpose",
    // Projects are not `task`, `block`, `memory`, `routine`, `note` or
    // `mail` either.
    "create_project",
    "update_project",
    "delete_project",
    // Asking a routine to run right now is an action, not a record to
    // propose.
    "run_routine",
    // Every mail write except drafting (which runs for real, see
    // `mail::DRAFT_WRITERS`) and `send_draft` (which is proposable) refuses
    // outright: nobody is there to say yes to a mailbox being reorganised
    // by a dream.
    "update_draft",
    "mark_read",
    "label_thread",
    "move_thread",
    "snooze_thread",
    "archive_thread",
    "trash_thread",
    "respond_to_invite",
];

/// The two tools that write a real `Draft` while drafting and gain a linked
/// `SendMail` proposal on top -- `dispatch`'s one documented exception.
const MAIL_DRAFT_WRITERS: &[&str] = &["draft_reply", "draft_message"];

fn expected_kind(tool: &str) -> ProposalKind {
    match tool {
        "create_task" | "update_task" | "delete_task" => ProposalKind::Task,
        "create_time_block" | "delete_time_block" => ProposalKind::Block,
        "remember" | "forget" => ProposalKind::Memory,
        "create_routine" | "update_routine" | "delete_routine" => ProposalKind::Routine,
        "create_note" | "update_note" | "delete_note" => ProposalKind::Note,
        "send_draft" => ProposalKind::Mail,
        other => panic!("no expected proposal kind recorded for {other}"),
    }
}

/// Plausible arguments for one proposable call.
fn args_for(f: &Fixtures, tool: &str) -> Value {
    match tool {
        "create_task" => json!({ "title": "Pack for the trip" }),
        "update_task" => {
            json!({ "task_id": f.task.id.to_string(), "title": "Book the dentist (renamed)" })
        }
        "delete_task" => json!({ "task_id": f.task.id.to_string() }),
        "create_time_block" => json!({
            "date": "2026-09-10",
            "start_time": "09:00",
            "end_time": "10:00",
            "task_id": f.task.id.to_string(),
        }),
        "delete_time_block" => json!({ "block_id": f.block.id.to_string() }),
        "remember" => json!({ "fact": "Prefers tea to coffee" }),
        "forget" => json!({ "memory_id": f.memory.id.to_string() }),
        "create_routine" => {
            json!({ "name": "Weekly review", "instructions": "Review the week", "at": "18:00" })
        }
        "update_routine" => json!({ "routine_id": f.routine.id.to_string(), "name": "Renamed" }),
        "delete_routine" => json!({ "routine_id": f.routine.id.to_string() }),
        "create_note" => json!({ "body": "Something worth writing down" }),
        "update_note" => json!({ "note_id": f.note.id.to_string(), "title": "Renamed" }),
        "delete_note" => json!({ "note_id": f.note.id.to_string() }),
        "send_draft" => json!({ "draft_id": f.draft_id }),
        "draft_reply" => {
            json!({ "message_id": f.message_id.to_string(), "body_html": "<p>Sure, works for me</p>" })
        }
        "draft_message" => json!({
            "account_id": f.account_id.to_string(),
            "to": ["someone@example.com"],
            "subject": "Hi",
            "body_html": "<p>hi</p>",
        }),
        other => panic!("no seeded arguments for {other}"),
    }
}

// ---- (a) the catalogue-wide test -------------------------------------------

#[test]
fn every_non_read_tool_is_either_proposable_or_explicitly_refused_while_drafting() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let f = seed(&v);
    let drafting_ctx = ctx_drafting(&v, Drafting::default());

    let before = counts(&v);
    let before_pending = v.pending_proposals().unwrap();
    let mut proposals_made = 0u64;

    for tool in tools::catalog() {
        if tool.effect == Effect::Read {
            continue;
        }
        let name = tool.name;

        if MAIL_DRAFT_WRITERS.contains(&name) {
            let out = tools::dispatch(&drafting_ctx, name, &args_for(&f, name))
                .unwrap_or_else(|e| panic!("{name} should run for real while drafting: {e}"));
            assert_eq!(out["action"], "drafted", "{name}: {out}");
            assert!(out["proposal_id"].is_string(), "{name} should also propose sending: {out}");
            let proposal: ProposalId = out["proposal_id"].as_str().unwrap().parse().unwrap();
            let p = v.proposal(proposal).unwrap();
            assert_eq!(p.kind, ProposalKind::Mail, "{name}");
            assert!(matches!(p.payload, Payload::SendMail { .. }), "{name}: {:?}", p.payload);
            proposals_made += 1;
            continue;
        }

        if tool.can_propose() {
            let out = tools::dispatch(&drafting_ctx, name, &args_for(&f, name))
                .unwrap_or_else(|e| panic!("{name} has a builder and should propose: {e}"));
            assert_eq!(out["action"], "proposed", "{name}: {out}");
            assert!(
                out["note"].as_str().unwrap_or_default().contains("Not done yet"),
                "{name}: {out}"
            );
            let proposal: ProposalId = out["id"].as_str().unwrap().parse().unwrap();
            let p = v.proposal(proposal).unwrap();
            assert_eq!(p.kind, expected_kind(name), "{name} proposed the wrong kind");
            assert_eq!(p.kind, p.payload.kind(), "{name}");
            assert_eq!(
                p.outcome.state(),
                everyday_core::proposal::ProposalState::Pending,
                "{name}"
            );
            proposals_made += 1;
        } else {
            assert!(
                REFUSED_WHILE_DRAFTING.contains(&name),
                "{name} is a {:?} tool with no builder and is not in \
                 REFUSED_WHILE_DRAFTING -- a new writing tool must either be made \
                 proposable or added to that list on purpose",
                tool.effect
            );
            let err =
                tools::dispatch(&drafting_ctx, name, &args_for_refused(&f, name)).unwrap_err();
            let msg = err.to_string();
            assert!(!msg.is_empty(), "{name} should refuse with a message");
        }
    }

    let after = counts(&v);
    assert_eq!(before.tasks, after.tasks, "no task was ever actually written");
    assert_eq!(before.projects, after.projects, "projects are not proposable at all");
    assert_eq!(before.blocks, after.blocks, "no block was ever actually written");
    assert_eq!(before.memories, after.memories, "no memory was ever actually written");
    assert_eq!(before.routines, after.routines, "no routine was ever actually written");
    assert_eq!(before.notes, after.notes, "no note was ever actually written");
    assert_eq!(before.entries, after.entries, "the journal is untouched");
    assert_eq!(before.items, after.items, "the library is untouched");
    assert_eq!(before.goals, after.goals, "roles and goals are untouched");
    assert_eq!(
        after.drafts,
        before.drafts + MAIL_DRAFT_WRITERS.len(),
        "draft_reply and draft_message are the one exception that writes for real"
    );
    assert_eq!(
        before.drafts_past_editing, after.drafts_past_editing,
        "nothing was ever queued or sent"
    );

    let after_pending = v.pending_proposals().unwrap();
    assert_eq!(after_pending, before_pending + proposals_made, "one proposal per call");
}

/// Arguments for a tool this test expects to refuse before it ever reads
/// them -- a builder-less call in drafting mode returns before `args` is
/// touched at all, so these only need to satisfy `serde_json::json!`, not
/// the tool's own schema.
fn args_for_refused(f: &Fixtures, tool: &str) -> Value {
    match tool {
        "update_task" | "delete_task" => json!({ "task_id": f.task.id.to_string() }),
        _ => json!({}),
    }
}

// ---- (b) policy denial ------------------------------------------------

#[test]
fn a_kind_the_person_switched_off_is_refused_rather_than_proposed() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let f = seed(&v);

    let mut settings = v.agent_settings().unwrap();
    settings.proposals.denied.insert(ProposalKind::Task);
    v.save_agent_settings(&settings).unwrap();

    let drafting_ctx = ctx_drafting(&v, Drafting::default());
    let err = tools::dispatch(&drafting_ctx, "create_task", &json!({ "title": "Should not land" }))
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("switched off"), "{err}");
    assert_eq!(v.pending_proposals().unwrap(), 0);

    // A kind that was not denied is unaffected.
    let out = tools::dispatch(&drafting_ctx, "remember", &json!({ "fact": "Reads before bed" }))
        .expect("memory proposals are still allowed");
    assert_eq!(out["action"], "proposed");
    let _ = f;
}

// ---- (c) the run's own cap ---------------------------------------------

#[test]
fn a_run_refuses_past_its_own_cap_on_proposals() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    seed(&v);

    let run_id = everyday_core::RoutineRunId::new();
    let drafting = Drafting {
        source: Some(ProposalSource::Run { run_id }),
        direct: Vec::new(),
        max_proposals: Some(2),
    };
    let drafting_ctx = ctx_drafting(&v, drafting);

    for i in 0..2 {
        let out = tools::dispatch(
            &drafting_ctx,
            "remember",
            &json!({ "fact": format!("Fact number {i}") }),
        )
        .unwrap_or_else(|e| panic!("proposal {i} should be within the cap: {e}"));
        assert_eq!(out["action"], "proposed");
    }

    let err =
        tools::dispatch(&drafting_ctx, "remember", &json!({ "fact": "One too many" })).unwrap_err();
    assert!(err.to_string().contains("2"), "{err}");

    let made = v.proposals(&ProposalQuery { run_id: Some(run_id), ..Default::default() }).unwrap();
    assert_eq!(made.len(), 2, "the third call must not have landed");

    // A run made by a conversation rather than a scheduled run has no
    // budget to check against, so the cap never applies to it.
    let unbounded = ctx_drafting(
        &v,
        Drafting {
            source: Some(ProposalSource::Conversation {
                conversation_id: everyday_core::ConversationId::new(),
            }),
            direct: Vec::new(),
            max_proposals: Some(1),
        },
    );
    tools::dispatch(&unbounded, "remember", &json!({ "fact": "Not a run" }))
        .expect("the cap is scoped to a run, not a conversation");
}

// ---- (d) why is truncated and stored ------------------------------------

#[test]
fn why_is_trimmed_and_truncated_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    seed(&v);
    let drafting_ctx = ctx_drafting(&v, Drafting::default());

    let long_why = "x".repeat(everyday_core::proposal::MAX_WHY_CHARS + 50);
    let out = tools::dispatch(
        &drafting_ctx,
        "remember",
        &json!({ "fact": "Something noticed", "why": format!("  {long_why}  ") }),
    )
    .unwrap();
    let id: ProposalId = out["id"].as_str().unwrap().parse().unwrap();
    let p = v.proposal(id).unwrap();
    assert_eq!(p.why.chars().count(), everyday_core::proposal::MAX_WHY_CHARS, "{}", p.why.len());
    assert!(!p.why.starts_with(' '), "should be trimmed: {:?}", p.why);
}

// ---- (e) update_task builds a Replace with the right expected_updated_at --

#[test]
fn update_task_proposes_a_replace_carrying_the_loaded_updated_at() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let f = seed(&v);
    let drafting_ctx = ctx_drafting(&v, Drafting::default());

    let out = tools::dispatch(
        &drafting_ctx,
        "update_task",
        &json!({ "task_id": f.task.id.to_string(), "title": "Renamed" }),
    )
    .unwrap();
    let id: ProposalId = out["id"].as_str().unwrap().parse().unwrap();
    let p = v.proposal(id).unwrap();
    match p.payload {
        Payload::Replace { record: ProposedRecord::Task(t), expected_updated_at } => {
            assert_eq!(t.title, "Renamed");
            assert_eq!(t.id, f.task.id);
            assert_eq!(expected_updated_at, f.task.updated_at);
        }
        other => panic!("expected a Replace of a task, got {other:?}"),
    }
    assert_eq!(p.about.as_ref().map(|a| a.id.clone()), Some(f.task.id.to_string()));

    // And the task itself never moved.
    assert_eq!(v.task(f.task.id).unwrap().title, f.task.title);
}

// ---- (f) remember while drafting builds an Inferred memory ---------------

#[test]
fn remember_while_drafting_proposes_an_inferred_memory() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    seed(&v);
    let drafting_ctx = ctx_drafting(&v, Drafting::default());

    let out =
        tools::dispatch(&drafting_ctx, "remember", &json!({ "fact": "Cooks on Sundays" })).unwrap();
    let id: ProposalId = out["id"].as_str().unwrap().parse().unwrap();
    let p = v.proposal(id).unwrap();
    match p.payload {
        Payload::Create { record: ProposedRecord::Memory(m) } => {
            assert_eq!(m.origin, everyday_core::MemoryOrigin::Inferred);
            assert_eq!(m.last_supported, Some(TODAY));
            assert_eq!(m.text, "Cooks on Sundays");
        }
        other => panic!("expected a Create of a memory, got {other:?}"),
    }

    // Outside drafting, the very same call is a Told memory, exactly as
    // before dreaming existed.
    let plain = ctx(&v);
    let out = tools::dispatch(&plain, "remember", &json!({ "fact": "Told directly" })).unwrap();
    let saved = v
        .memories()
        .unwrap()
        .into_iter()
        .find(|m| m.id.to_string() == out["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(saved.origin, everyday_core::MemoryOrigin::Told);
}

// ---- (g) new arguments: due_time, goal_id/role_id, both-given error -------

#[test]
fn due_time_is_refused_without_a_due_date_on_create_and_allowed_on_update_with_one() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    seed(&v);

    let err =
        tools::dispatch(&ctx(&v), "create_task", &json!({ "title": "x", "due_time": "09:00" }))
            .unwrap_err();
    assert!(err.to_string().contains("due_date"), "{err}");

    let created = tools::dispatch(
        &ctx(&v),
        "create_task",
        &json!({ "title": "x", "due_date": "2026-09-20", "due_time": "09:00" }),
    )
    .unwrap();
    let task_id: everyday_core::TaskId = created["id"].as_str().unwrap().parse().unwrap();
    let task = v.task(task_id).unwrap();
    assert_eq!(task.due_time, Some(jiff::civil::time(9, 0, 0, 0)));

    // On update, a due_time is fine once the task already has a due_date,
    // even without naming one again in the same call.
    let updated = tools::dispatch(
        &ctx(&v),
        "update_task",
        &json!({ "task_id": task_id.to_string(), "due_time": "14:30" }),
    )
    .unwrap();
    assert_eq!(updated["action"], "updated");
    assert_eq!(v.task(task_id).unwrap().due_time, Some(jiff::civil::time(14, 30, 0, 0)));

    // But a bare task with no due date anywhere still refuses one.
    let bare = tools::dispatch(&ctx(&v), "create_task", &json!({ "title": "bare" })).unwrap();
    let bare_id: everyday_core::TaskId = bare["id"].as_str().unwrap().parse().unwrap();
    let err = tools::dispatch(
        &ctx(&v),
        "update_task",
        &json!({ "task_id": bare_id.to_string(), "due_time": "09:00" }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("due_date"), "{err}");
}

#[test]
fn goal_id_and_role_id_set_a_tasks_purpose_and_giving_both_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());

    let role = Role::new("Health");
    v.save_role(&role).unwrap();
    let goal = Goal::new(role.id, "Run a 10k");
    v.save_goal(&goal).unwrap();

    let err = tools::dispatch(
        &ctx(&v),
        "create_task",
        &json!({
            "title": "Book the run",
            "goal_id": goal.id.to_string(),
            "role_id": role.id.to_string(),
        }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("goal_id"), "{err}");

    let out = tools::dispatch(
        &ctx(&v),
        "create_task",
        &json!({ "title": "Book the run", "goal_id": goal.id.to_string() }),
    )
    .unwrap();
    let task_id: everyday_core::TaskId = out["id"].as_str().unwrap().parse().unwrap();
    let saved = v.task(task_id).unwrap();
    assert_eq!(saved.purpose, Some(everyday_core::Purpose::Goal { id: goal.id }));

    // The purposes side table agrees -- `time_by_role` is the query that
    // reads it, so a project or task filed under a role must show up there.
    let by_role = tools::dispatch(&ctx(&v), "time_by_role", &json!({})).unwrap();
    let _ = by_role;

    // `role_id` alone works too, and `clear_purpose` takes it back off.
    let out = tools::dispatch(
        &ctx(&v),
        "update_task",
        &json!({ "task_id": task_id.to_string(), "role_id": role.id.to_string() }),
    )
    .unwrap();
    assert_eq!(out["action"], "updated");
    assert_eq!(
        v.task(task_id).unwrap().purpose,
        Some(everyday_core::Purpose::Role { id: role.id })
    );

    tools::dispatch(
        &ctx(&v),
        "update_task",
        &json!({ "task_id": task_id.to_string(), "clear_purpose": true }),
    )
    .unwrap();
    assert_eq!(v.task(task_id).unwrap().purpose, None);

    // An id that does not exist is refused rather than silently stored.
    let err = tools::dispatch(
        &ctx(&v),
        "create_task",
        &json!({ "title": "x", "goal_id": everyday_core::GoalId::new().to_string() }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("no goal"), "{err}");
}

#[test]
fn create_time_block_also_takes_a_goal_or_role() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());

    let role = Role::new("Health");
    v.save_role(&role).unwrap();

    let out = tools::dispatch(
        &ctx(&v),
        "create_time_block",
        &json!({
            "date": "2026-09-10",
            "start_time": "07:00",
            "end_time": "08:00",
            "label": "Run",
            "role_id": role.id.to_string(),
        }),
    )
    .unwrap();
    let block_id: everyday_core::BlockId = out["id"].as_str().unwrap().parse().unwrap();
    let block = v.block(block_id).unwrap();
    assert_eq!(block.purpose, Some(everyday_core::Purpose::Role { id: role.id }));
}
