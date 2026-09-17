//! Proposals wired all the way through a real vault: accepting, declining,
//! and everything a proposal's own rules refuse. See `everyday_core::proposal`
//! and `docs/plans/dreaming.md`.

mod support;
use support::vault;

use everyday_core::proposal::{DeclineReason, MAX_PENDING_PROPOSALS, Outcome as ProposalOutcome};
use everyday_core::{
    BlockSubject, DreamScope, Memory, MemoryOrigin, Note, Payload, Project, Proposal, ProposalKind,
    ProposedRecord, Purpose, Role, Routine, RoutineKind, Task, TimeBlock, Trigger,
};
use jiff::Timestamp;

fn now() -> Timestamp {
    Timestamp::now()
}

fn create(record: ProposedRecord, caption: &str) -> Proposal {
    Proposal::new(Payload::Create { record }, caption, now(), "UTC")
}

// ---- accept: task -----------------------------------------------------

#[test]
fn accepting_a_create_saves_the_task_and_closes_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let project = Project::new("Repaint the hall");
    vault.save_project(&project).unwrap();

    let task = Task::new("Buy the paint").in_project(project.id);
    let task_id = task.id;
    let proposal = create(ProposedRecord::Task(task), "Create task");
    vault.save_proposal(&proposal).unwrap();

    let closed = vault.accept_proposal(proposal.id, None, false, now()).unwrap();
    match closed.outcome {
        ProposalOutcome::Accepted { saved_as, edited, .. } => {
            assert_eq!(saved_as, task_id.to_string());
            assert!(!edited);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert!(closed.seen, "an accepted proposal is seen");
    assert_eq!(vault.task(task_id).unwrap().title, "Buy the paint");
}

#[test]
fn a_task_whose_project_has_gone_is_declined_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    // The project this task names was never saved -- as good as deleted.
    let ghost_project = Project::new("Never saved").id;
    let task = Task::new("Buy the paint").in_project(ghost_project);
    let task_id = task.id;
    let proposal = create(ProposedRecord::Task(task), "Create task");
    vault.save_proposal(&proposal).unwrap();

    let err = vault.accept_proposal(proposal.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");

    let closed = vault.proposal(proposal.id).unwrap();
    match closed.outcome {
        ProposalOutcome::Declined { reason: Some(DeclineReason::Other { text }), .. } => {
            assert!(text.contains("project"), "{text}");
        }
        other => panic!("expected Declined(Other), got {other:?}"),
    }
    assert!(vault.task(task_id).is_err(), "nothing stale is ever written");
}

#[test]
fn a_stale_replace_is_declined_and_the_newer_write_survives() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut task = Task::new("Draft the proposal");
    vault.save_task(&task).unwrap();
    let original_updated_at = task.updated_at;

    // Somebody else edits it before the proposal is answered.
    task.title = "Draft the final proposal".into();
    task.updated_at = now() + jiff::SignedDuration::from_secs(1);
    vault.save_task(&task).unwrap();

    let mut edited = task.clone();
    edited.title = "Draft the assistant's proposal".into();
    let replace = Proposal::new(
        Payload::Replace {
            record: ProposedRecord::Task({
                let mut t = edited.clone();
                t.updated_at = original_updated_at;
                t
            }),
            expected_updated_at: original_updated_at,
        },
        "Update task",
        now(),
        "UTC",
    );
    vault.save_proposal(&replace).unwrap();

    let err = vault.accept_proposal(replace.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    let closed = vault.proposal(replace.id).unwrap();
    assert!(
        matches!(
            closed.outcome,
            ProposalOutcome::Declined { reason: Some(DeclineReason::Other { .. }), .. }
        ),
        "{:?}",
        closed.outcome
    );
    assert_eq!(
        vault.task(task.id).unwrap().title,
        "Draft the final proposal",
        "the newer write must survive a stale proposal"
    );
}

#[test]
fn a_fresh_replace_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let task = Task::new("Draft the proposal");
    vault.save_task(&task).unwrap();

    let mut edited = task.clone();
    edited.title = "Draft the final proposal".into();
    let replace = Proposal::new(
        Payload::Replace {
            record: ProposedRecord::Task(edited),
            expected_updated_at: task.updated_at,
        },
        "Update task",
        now(),
        "UTC",
    );
    vault.save_proposal(&replace).unwrap();

    let closed = vault.accept_proposal(replace.id, None, false, now()).unwrap();
    assert!(matches!(closed.outcome, ProposalOutcome::Accepted { .. }));
    assert_eq!(vault.task(task.id).unwrap().title, "Draft the final proposal");
}

#[test]
fn accepting_a_delete_removes_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let task = Task::new("No longer needed");
    vault.save_task(&task).unwrap();

    let proposal = Proposal::new(
        Payload::Delete { kind: ProposalKind::Task, id: task.id.to_string() },
        "Delete task",
        now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let closed = vault.accept_proposal(proposal.id, None, false, now()).unwrap();
    match closed.outcome {
        ProposalOutcome::Accepted { saved_as, .. } => assert_eq!(saved_as, task.id.to_string()),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert!(vault.task(task.id).is_err());
}

#[test]
fn accepting_a_delete_of_something_already_gone_is_declined() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let task = Task::new("Already deleted by someone else");
    let proposal = Proposal::new(
        Payload::Delete { kind: ProposalKind::Task, id: task.id.to_string() },
        "Delete task",
        now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let err = vault.accept_proposal(proposal.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    let closed = vault.proposal(proposal.id).unwrap();
    assert!(matches!(
        closed.outcome,
        ProposalOutcome::Declined { reason: Some(DeclineReason::Other { .. }), .. }
    ));
}

// ---- accept: memory -----------------------------------------------------

#[test]
fn accepting_a_memory_plainly_leaves_it_inferred() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let memory = Memory::new("prefers mornings for deep work");
    let memory_id = memory.id;
    let proposal = create(ProposedRecord::Memory(memory), "Remember");
    vault.save_proposal(&proposal).unwrap();

    vault.accept_proposal(proposal.id, None, false, now()).unwrap();
    let saved = vault.memories().unwrap().into_iter().find(|m| m.id == memory_id).unwrap();
    assert_eq!(saved.origin, MemoryOrigin::Inferred);
}

#[test]
fn confirming_a_memory_from_the_list_marks_it_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let memory = Memory::new("prefers mornings for deep work");
    let memory_id = memory.id;
    let proposal = create(ProposedRecord::Memory(memory), "Remember");
    vault.save_proposal(&proposal).unwrap();

    vault.accept_proposal(proposal.id, None, true, now()).unwrap();
    let saved = vault.memories().unwrap().into_iter().find(|m| m.id == memory_id).unwrap();
    assert_eq!(saved.origin, MemoryOrigin::Confirmed);
}

#[test]
fn editing_a_memory_before_accepting_also_marks_it_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let memory = Memory::new("prefers mornings for deep work");
    let memory_id = memory.id;
    let proposal = create(ProposedRecord::Memory(memory.clone()), "Remember");
    vault.save_proposal(&proposal).unwrap();

    let mut edited = memory;
    edited.text = "strongly prefers mornings for deep work".into();
    vault.accept_proposal(proposal.id, Some(ProposedRecord::Memory(edited)), false, now()).unwrap();

    let saved = vault.memories().unwrap().into_iter().find(|m| m.id == memory_id).unwrap();
    assert_eq!(saved.origin, MemoryOrigin::Confirmed, "a person who rewrote it stands behind it");
    assert_eq!(saved.text, "strongly prefers mornings for deep work");
}

// ---- accept: routine, note ----------------------------------------------

#[test]
fn a_routine_proposal_is_always_saved_as_custom() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut routine = Routine::new("Weekly review", "Say what is due.", Trigger::Manual);
    // Nothing this vault proposes should ever carry the application's own
    // kind, but the save must force it regardless of what arrives.
    routine.kind = RoutineKind::Dream { scope: DreamScope::Day };
    let routine_id = routine.id;
    let proposal = create(ProposedRecord::Routine(routine), "Create routine");
    vault.save_proposal(&proposal).unwrap();

    vault.accept_proposal(proposal.id, None, false, now()).unwrap();
    assert_eq!(vault.routine(routine_id).unwrap().kind, RoutineKind::Custom);
}

#[test]
fn a_dream_routine_cannot_be_deleted_through_a_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    // Made the only way a dream can be: by switching dreaming on.
    let dream = vault
        .set_dreaming(true)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Day })
        .unwrap();

    let proposal = Proposal::new(
        Payload::Delete { kind: ProposalKind::Routine, id: dream.id.to_string() },
        "Delete routine",
        now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let err = vault.accept_proposal(proposal.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(vault.routine(dream.id).is_ok(), "a dream routine must survive");
}

#[test]
fn accepting_a_note_saves_it_through_the_force_path() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let note = Note::written("Trip notes", "Pack sunscreen.");
    let note_id = note.id;
    let proposal = create(ProposedRecord::Note(note), "Create note");
    vault.save_proposal(&proposal).unwrap();

    vault.accept_proposal(proposal.id, None, false, now()).unwrap();
    assert_eq!(vault.note(note_id).unwrap().title, "Trip notes");
}

// ---- accept: block, and a purpose that has gone --------------------------

#[test]
fn a_block_whose_purpose_role_has_gone_is_declined() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let ghost_role = Role::new("never saved").id;
    let start: Timestamp = "2026-09-20T09:00:00Z".parse().unwrap();
    let block = TimeBlock::new(BlockSubject::Adhoc, start, 30, "UTC")
        .for_purpose(Purpose::Role { id: ghost_role });
    let block_id = block.id;
    let proposal = create(ProposedRecord::Block(block), "Plan time");
    vault.save_proposal(&proposal).unwrap();

    let err = vault.accept_proposal(proposal.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(vault.block(block_id).is_err());
}

// ---- accept: shape and lifecycle rules -----------------------------------

#[test]
fn an_edit_naming_a_different_record_is_refused_without_touching_the_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let task = Task::new("Original");
    let proposal = create(ProposedRecord::Task(task), "Create task");
    vault.save_proposal(&proposal).unwrap();

    let mismatched = ProposedRecord::Task(Task::new("A completely different task"));
    let err = vault.accept_proposal(proposal.id, Some(mismatched), false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(
        vault.proposal(proposal.id).unwrap().is_pending(),
        "a shape mismatch must not close it"
    );
}

#[test]
fn accepting_a_mail_proposal_is_refused_here() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let proposal = Proposal::new(
        Payload::SendMail { draft_id: everyday_core::DraftId::new() },
        "Send mail",
        now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let err = vault.accept_proposal(proposal.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(vault.proposal(proposal.id).unwrap().is_pending(), "mail is the outbox's to close");
}

#[test]
fn accepting_or_declining_an_already_answered_proposal_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let proposal = create(ProposedRecord::Note(Note::written("Once", "x")), "Create note");
    vault.save_proposal(&proposal).unwrap();
    vault.decline_proposal(proposal.id, None, now()).unwrap();

    assert_eq!(
        vault.accept_proposal(proposal.id, None, false, now()).unwrap_err().code(),
        "invalid"
    );
    assert_eq!(vault.decline_proposal(proposal.id, None, now()).unwrap_err().code(), "invalid");
}

#[test]
fn an_expired_proposal_is_closed_and_refused_on_accept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut proposal = create(ProposedRecord::Note(Note::written("Late", "x")), "Create note");
    proposal.expires_at = now() - jiff::SignedDuration::from_secs(1);
    vault.save_proposal(&proposal).unwrap();

    let err = vault.accept_proposal(proposal.id, None, false, now()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(matches!(
        vault.proposal(proposal.id).unwrap().outcome,
        ProposalOutcome::Expired { .. }
    ));
}

// ---- decline --------------------------------------------------------------

#[test]
fn declining_a_memory_keeps_it_as_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let memory = Memory::new("dislikes early meetings");
    let memory_id = memory.id;
    let proposal = create(ProposedRecord::Memory(memory), "Remember");
    vault.save_proposal(&proposal).unwrap();

    vault.decline_proposal(proposal.id, Some(DeclineReason::WrongTime), now()).unwrap();

    let kept = vault.memories().unwrap().into_iter().find(|m| m.id == memory_id);
    let kept = kept.expect("a declined memory is kept, not thrown away");
    assert_eq!(kept.origin, MemoryOrigin::Rejected);
}

#[test]
fn declining_not_now_does_not_keep_the_memory() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let memory = Memory::new("dislikes early meetings");
    let memory_id = memory.id;
    let proposal = create(ProposedRecord::Memory(memory), "Remember");
    vault.save_proposal(&proposal).unwrap();

    vault.decline_proposal(proposal.id, Some(DeclineReason::NotNow), now()).unwrap();

    assert!(
        vault.memories().unwrap().into_iter().all(|m| m.id != memory_id),
        "\"not now\" is timing, not truth, and keeps nothing behind"
    );
}

#[test]
fn declining_a_send_mail_proposal_leaves_the_draft_alone() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let proposal = Proposal::new(
        Payload::SendMail { draft_id: everyday_core::DraftId::new() },
        "Send mail",
        now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let closed =
        vault.decline_proposal(proposal.id, Some(DeclineReason::NeverThis), now()).unwrap();
    assert!(matches!(closed.outcome, ProposalOutcome::Declined { .. }));
}

// ---- the pending cap ------------------------------------------------------

#[test]
fn save_proposal_refuses_a_pending_row_past_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut last = None;
    for i in 0..MAX_PENDING_PROPOSALS {
        let p =
            create(ProposedRecord::Note(Note::written(format!("Note {i}"), "x")), "Create note");
        vault.save_proposal(&p).unwrap();
        last = Some(p);
    }

    let over = create(ProposedRecord::Note(Note::written("One too many", "x")), "Create note");
    let err = vault.save_proposal(&over).unwrap_err();
    assert_eq!(err.code(), "invalid");

    // Closing one reopens a slot, and re-saving an existing proposal --
    // pending or not -- never trips the cap in the first place.
    let last = last.unwrap();
    vault.decline_proposal(last.id, None, now()).unwrap();
    vault.save_proposal(&over).unwrap();
}
