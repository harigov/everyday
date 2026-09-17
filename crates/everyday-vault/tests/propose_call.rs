//! `tools::propose_call`, end to end against a real vault: the one entry
//! point a caller outside drafting mode -- the rail's "later" answer, and an
//! unattended run's own park-instead-of-decline, both in
//! `everyday_service::agent::ConfirmGate` -- reaches to save a call as a
//! proposal instead of running it. See `docs/plans/dreaming.md`'s Phase 5.

mod support;
use support::{ctx, vault};

use everyday_core::agent::tools;
use everyday_core::{Payload, ProposalKind, ProposalSource, RoutineRunId, Task};
use serde_json::json;

#[test]
fn propose_call_builds_and_saves_a_proposal_for_a_proposable_tool() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());

    let task = Task::new("Book the dentist");
    v.save_task(&task).unwrap();

    let run_id = RoutineRunId::new();
    let result = tools::propose_call(
        &ctx(&v),
        "delete_task",
        &json!({ "task_id": task.id.to_string() }),
        ProposalSource::Run { run_id },
    )
    .expect("delete_task has a builder and can be proposed");

    assert_eq!(result["action"], "proposed");
    assert_eq!(result["kind"], "task");

    // Not actually deleted -- only proposed.
    assert!(v.task(task.id).is_ok(), "the real task is untouched");

    let pending = v.proposals(&Default::default()).unwrap();
    assert_eq!(pending.len(), 1, "exactly one proposal was saved");
    let proposal = &pending[0];
    assert_eq!(proposal.kind, ProposalKind::Task);
    match &proposal.payload {
        Payload::Delete { kind, id } => {
            assert_eq!(*kind, ProposalKind::Task);
            assert_eq!(id, &task.id.to_string());
        }
        other => panic!("expected a Delete payload, got {other:?}"),
    }
    assert_eq!(proposal.made_by, Some(ProposalSource::Run { run_id }));
}

#[test]
fn propose_call_refuses_a_tool_with_no_builder() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());

    // A vault made through `support::vault` has no journal yet -- make one
    // first, so there is somewhere for the entry to live.
    let journal = everyday_core::Journal::new("Journal");
    v.save_journal(&journal).unwrap();
    let mut entry = everyday_core::Entry::new(journal.id, "UTC");
    entry.title = "Do not touch".into();
    v.save_entry(&entry, None).unwrap();

    let err = tools::propose_call(
        &ctx(&v),
        "delete_entry",
        &json!({ "entry_id": entry.id.to_string() }),
        ProposalSource::Run { run_id: RoutineRunId::new() },
    )
    .expect_err("a journal entry cannot be proposed -- delete_entry has no builder");
    assert!(err.to_string().contains("no proposal form"), "got {err}");

    // Refused before it ever touched anything: still there, and nothing
    // was saved.
    assert!(v.entry(entry.id).is_ok());
    assert!(v.proposals(&Default::default()).unwrap().is_empty());
}

#[test]
fn propose_call_names_an_unknown_tool_rather_than_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());

    let err = tools::propose_call(
        &ctx(&v),
        "not_a_real_tool",
        &json!({}),
        ProposalSource::Run { run_id: RoutineRunId::new() },
    )
    .expect_err("there is no such tool");
    assert!(err.to_string().contains("not_a_real_tool"), "got {err}");
}
