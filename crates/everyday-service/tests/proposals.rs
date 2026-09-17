//! Proposal commands end to end through the service, and the scheduler's
//! sweep. See `everyday_core::proposal` and `docs/plans/dreaming.md`.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, Provider};
use everyday_core::mail::{Address, Draft, DraftState, Origin as MailOrigin};
use everyday_core::proposal::{Outcome as ProposalOutcome, Payload, ProposedRecord};
use everyday_core::{DraftId, Note, Proposal, Vault};
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use everyday_service::error::CommandError;
use everyday_service::events::{Change, EventSink, Kind};
use everyday_service::scheduler;
use jiff::Timestamp;
use serde_json::{Value, json};

fn service() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

fn seed_note_proposal(vault: &Vault, title: &str) -> Proposal {
    let proposal = Proposal::new(
        Payload::Create { record: ProposedRecord::Note(Note::written(title, "x")) },
        "Create note",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();
    proposal
}

/// An account with nowhere to sync from -- these tests never open a real
/// socket, only the vault and the outbox's own state machine.
fn seed_account(vault: &Vault) -> everyday_core::AccountId {
    let account = Account::new(Provider::Custom, "me@example.com");
    let id = account.id;
    vault.save_account(&account).unwrap();
    id
}

/// A draft ready to send: one recipient, in `Editing`.
fn seed_draft(vault: &Vault, account: everyday_core::AccountId) -> Draft {
    let mut draft = Draft::new(account, "me@example.com", MailOrigin::Person);
    draft.to = vec![Address::bare("someone@example.com")];
    vault.save_draft(&draft).unwrap();
    draft
}

#[tokio::test]
async fn accept_proposal_saves_the_record_and_raises_both_changes() {
    let (svc, _dir) = service();
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let proposal = seed_note_proposal(&vault, "Trip notes");

    let result = call(&svc, "accept_proposal", json!({ "id": proposal.id.to_string() })).await;
    assert_eq!(result["outcome"]["type"].as_str(), Some("accepted"));

    let changes = events.changes.lock().unwrap();
    assert!(
        changes.iter().any(|c| c.kind == Kind::Proposal),
        "the command table's own change must still fire"
    );
    assert!(
        changes.iter().any(|c| c.kind == Kind::Note),
        "and a second change for the record it actually saved"
    );
}

#[tokio::test]
async fn accept_proposal_on_a_bad_reference_declines_and_reports_no_second_change() {
    let (svc, _dir) = service();
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();

    let ghost_project = everyday_core::ProjectId::new();
    let task = everyday_core::Task::new("Buy the paint").in_project(ghost_project);
    let proposal = Proposal::new(
        Payload::Create { record: ProposedRecord::Task(task) },
        "Create task",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let err = fails(&svc, "accept_proposal", json!({ "id": proposal.id.to_string() })).await;
    assert_eq!(err.code, "invalid");

    let closed = vault.proposal(proposal.id).unwrap();
    assert!(matches!(closed.outcome, ProposalOutcome::Declined { .. }));

    // The command table raises `Proposal` for a write that fails just as
    // much as one that succeeds -- `svc.call` runs the body and stops
    // before it, so only the body's own emission is at stake here.
    let changes = events.changes.lock().unwrap();
    assert!(
        !changes.iter().any(|c| c.kind == Kind::Task),
        "nothing was saved, so nothing should be reported as a task change"
    );
}

#[tokio::test]
async fn decline_proposal_through_the_command_table() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let proposal = seed_note_proposal(&vault, "Trip notes");

    let result = call(
        &svc,
        "decline_proposal",
        json!({ "id": proposal.id.to_string(), "reason": { "type": "wrongTime" } }),
    )
    .await;
    assert_eq!(result["outcome"]["type"].as_str(), Some("declined"));
    assert_eq!(result["outcome"]["reason"]["type"].as_str(), Some("wrongTime"));
}

#[tokio::test]
async fn mark_proposals_seen_through_the_command_table() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let one = seed_note_proposal(&vault, "One");
    let _two = seed_note_proposal(&vault, "Two");
    assert_eq!(vault.unseen_proposals().unwrap(), 2);

    call(&svc, "mark_proposals_seen", json!({ "ids": [one.id.to_string()] })).await;
    assert_eq!(vault.unseen_proposals().unwrap(), 1);

    call(&svc, "mark_proposals_seen", json!({})).await;
    assert_eq!(vault.unseen_proposals().unwrap(), 0);
}

// ---- mail: the one payload the service handles itself ---------------------

#[tokio::test]
async fn accepting_a_send_mail_proposal_queues_the_draft_and_reports_it_changed() {
    let (svc, _dir) = service();
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let account = seed_account(&vault);
    let draft = seed_draft(&vault, account);

    let proposal = Proposal::new(
        Payload::SendMail { draft_id: draft.id },
        "Send mail",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let result = call(&svc, "accept_proposal", json!({ "id": proposal.id.to_string() })).await;
    assert_eq!(result["outcome"]["type"].as_str(), Some("accepted"));
    assert_eq!(result["outcome"]["savedAs"].as_str(), Some(draft.id.to_string().as_str()));

    let queued = vault.draft(draft.id).unwrap();
    assert!(matches!(queued.state, DraftState::Queued { .. }), "{:?}", queued.state);

    let changes = events.changes.lock().unwrap();
    assert!(changes.iter().any(|c| c.kind == Kind::Proposal));
    assert!(
        changes.iter().any(
            |c| c.kind == Kind::Draft && c.id.as_deref() == Some(draft.id.to_string().as_str())
        ),
        "the draft it queued must be reported too"
    );
}

#[tokio::test]
async fn accepting_a_send_mail_proposal_whose_draft_is_gone_declines_it() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();

    let proposal = Proposal::new(
        Payload::SendMail { draft_id: DraftId::new() },
        "Send mail",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    let err = fails(&svc, "accept_proposal", json!({ "id": proposal.id.to_string() })).await;
    assert_eq!(err.code, "not_found");

    let closed = vault.proposal(proposal.id).unwrap();
    assert!(matches!(closed.outcome, ProposalOutcome::Declined { .. }), "{:?}", closed.outcome);
}

// ---- the scheduler's sweep --------------------------------------------------

#[tokio::test]
async fn the_sweep_expires_a_pending_proposal_past_its_own_deadline() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let mut proposal = seed_note_proposal(&vault, "Overdue");
    proposal.expires_at = Timestamp::now() - jiff::SignedDuration::from_secs(1);
    vault.save_proposal(&proposal).unwrap();

    scheduler::tick(&svc).await;

    let closed = vault.proposal(proposal.id).unwrap();
    assert!(matches!(closed.outcome, ProposalOutcome::Expired { .. }));
}

#[tokio::test]
async fn the_sweep_expires_a_mail_proposal_whose_draft_is_no_longer_editing() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let account = seed_account(&vault);
    let mut draft = seed_draft(&vault, account);
    draft.state = DraftState::Sent;
    vault.save_draft(&draft).unwrap();

    let proposal = Proposal::new(
        Payload::SendMail { draft_id: draft.id },
        "Send mail",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    scheduler::tick(&svc).await;

    let closed = vault.proposal(proposal.id).unwrap();
    assert!(matches!(closed.outcome, ProposalOutcome::Expired { .. }), "{:?}", closed.outcome);
}

#[tokio::test]
async fn the_sweep_leaves_a_still_editing_mail_proposal_alone() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let account = seed_account(&vault);
    let draft = seed_draft(&vault, account);

    let proposal = Proposal::new(
        Payload::SendMail { draft_id: draft.id },
        "Send mail",
        Timestamp::now(),
        "UTC",
    );
    vault.save_proposal(&proposal).unwrap();

    scheduler::tick(&svc).await;

    let untouched = vault.proposal(proposal.id).unwrap();
    assert!(untouched.is_pending(), "a draft still being edited has nothing wrong with it yet");
}
