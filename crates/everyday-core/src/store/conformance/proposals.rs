//! The proposal half of the conformance suite: work the assistant prepared
//! and did not do, waiting for a yes or a no.

use super::*;
use crate::id::{DraftId, ProposalId};
use crate::proposal::{
    About, AboutKind, DeclineReason, Outcome as ProposalOutcome, Payload, Proposal, ProposalKind,
    ProposalSource, ProposalState, ProposedRecord,
};
use crate::store::proposals::{ProposalQuery, ProposalStore};

/// Everything a backend must do with proposals.
pub fn run_proposal_suite(store: &dyn JournalStore) {
    eprintln!("--- proposal conformance suite ---");

    proposals_start_empty(store);
    proposal_round_trips_every_payload_and_outcome(store);
    proposal_put_is_idempotent(store);
    missing_proposal_is_not_found(store);
    proposals_are_queried_by_kind_state_and_window(store);
    proposals_are_queried_by_made_since_about_and_run(store);
    proposals_are_queried_unseen_limited_and_newest_first(store);
    counts_pending_and_unseen_pending(store);
    expire_proposals_closes_only_pending_past_due_rows(store);
    mark_proposals_seen_marks_ids_or_all_pending_but_never_the_answered(store);

    proposal_cleanup(store);
    eprintln!("--- proposal suite passed ---");
}

fn proposal_store(store: &dyn JournalStore) -> &dyn ProposalStore {
    store.proposals().expect("the proposal suite needs a proposal store")
}

fn proposal_cleanup(store: &dyn JournalStore) {
    let p = proposal_store(store);
    for proposal in p.list_proposals(&ProposalQuery::default()).expect("list_proposals") {
        p.delete_proposal(proposal.id).expect("delete_proposal");
    }
    assert!(
        p.list_proposals(&ProposalQuery::default()).unwrap().is_empty(),
        "cleanup left proposals behind"
    );
}

fn now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn any_task() -> Task {
    let mut t = Task::new("Write the report");
    t.due_date = Some(date(2026, 9, 20));
    t
}

fn any_block() -> TimeBlock {
    TimeBlock::new(BlockSubject::Adhoc, now(), 30, "UTC")
}

fn seeded(store: &dyn JournalStore, payload: Payload, caption: &str) -> Proposal {
    let proposal = Proposal::new(payload, caption, now(), "UTC");
    proposal_store(store).put_proposal(&proposal).expect("put_proposal");
    proposal
}

fn note_payload(title: &str) -> Payload {
    Payload::Create { record: ProposedRecord::Note(Note::written(title, "x")) }
}

fn proposals_start_empty(store: &dyn JournalStore) {
    let p = proposal_store(store);
    assert!(p.list_proposals(&ProposalQuery::default()).unwrap().is_empty());
    assert_eq!(p.count_pending().unwrap(), 0);
    assert_eq!(p.count_unseen_pending().unwrap(), 0);
}

/// Every [`Payload`] variant, and every [`ProposalOutcome`] it can be closed
/// with, must survive a round trip -- the clear columns and the sealed
/// payload agreeing throughout.
fn proposal_round_trips_every_payload_and_outcome(store: &dyn JournalStore) {
    let p = proposal_store(store);

    let payloads: Vec<(Payload, &str)> = vec![
        (Payload::Create { record: ProposedRecord::Task(any_task()) }, "Create task"),
        (
            Payload::Replace {
                record: ProposedRecord::Block(any_block()),
                expected_updated_at: now(),
            },
            "Move time block",
        ),
        (
            Payload::Delete { kind: ProposalKind::Note, id: NoteId::new().to_string() },
            "Delete note",
        ),
        (Payload::SendMail { draft_id: DraftId::new() }, "Send mail"),
        (
            Payload::Create { record: ProposedRecord::Memory(Memory::new("likes Wednesdays")) },
            "Remember",
        ),
        (
            Payload::Create {
                record: ProposedRecord::Routine(Routine::new(
                    "Weekly review",
                    "Say what is due.",
                    Trigger::Manual,
                )),
            },
            "Create routine",
        ),
        (note_payload("A plan"), "Create note"),
    ];

    for (payload, caption) in payloads {
        let mut proposal = Proposal::new(payload, caption, now(), "UTC")
            .with_why("because yesterday's data said so")
            .with_about(Some(About { kind: AboutKind::Task, id: "subject-1".into() }))
            .made_by(ProposalSource::Run { run_id: RoutineRunId::new() });
        p.put_proposal(&proposal).expect("put_proposal");
        assert_eq!(
            p.get_proposal(proposal.id).unwrap(),
            proposal,
            "a pending {caption} must round trip"
        );

        for outcome in [
            ProposalOutcome::Accepted { at: now(), saved_as: "saved-1".into(), edited: true },
            ProposalOutcome::Declined { at: now(), reason: Some(DeclineReason::WrongTime) },
            ProposalOutcome::Expired { at: now() },
        ] {
            proposal.close(outcome, now());
            p.put_proposal(&proposal).expect("put_proposal (closed)");
            let back = p.get_proposal(proposal.id).expect("get_proposal");
            assert_eq!(
                back, proposal,
                "{caption} closed as {:?} must round trip",
                proposal.outcome
            );
        }
    }

    proposal_cleanup(store);
}

fn proposal_put_is_idempotent(store: &dyn JournalStore) {
    let p = proposal_store(store);
    let proposal = seeded(store, note_payload("Twice"), "Create note");
    p.put_proposal(&proposal).expect("second put");
    assert_eq!(
        p.list_proposals(&ProposalQuery::default()).unwrap().len(),
        1,
        "saving twice leaves one"
    );

    p.delete_proposal(proposal.id).expect("delete_proposal");
    p.delete_proposal(proposal.id).expect("deleting a missing proposal is a no-op");
    proposal_cleanup(store);
}

fn missing_proposal_is_not_found(store: &dyn JournalStore) {
    let p = proposal_store(store);
    super::assert_not_found(p.get_proposal(ProposalId::new()));
    p.delete_proposal(ProposalId::new()).expect("deleting a missing proposal is a no-op");
}

fn proposals_are_queried_by_kind_state_and_window(store: &dyn JournalStore) {
    let p = proposal_store(store);

    let dated =
        seeded(store, Payload::Create { record: ProposedRecord::Task(any_task()) }, "Create task");
    let undated = seeded(
        store,
        Payload::Create { record: ProposedRecord::Memory(Memory::new("noticed something")) },
        "Remember",
    );
    let other_kind = seeded(store, note_payload("N"), "Create note");

    let tasks = p
        .list_proposals(&ProposalQuery { kinds: vec![ProposalKind::Task], ..Default::default() })
        .unwrap();
    assert_eq!(tasks.iter().map(|x| x.id).collect::<Vec<_>>(), vec![dated.id]);

    // A proposal with no day is excluded once either bound is set, even
    // though it would otherwise pass -- it has nothing to compare.
    let windowed = p
        .list_proposals(&ProposalQuery {
            from: Some(date(2026, 9, 1)),
            to: Some(date(2026, 9, 30)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        windowed.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![dated.id],
        "an undated proposal must not appear once a window is set"
    );

    let mut closed = other_kind.clone();
    closed.close(ProposalOutcome::Declined { at: now(), reason: None }, now());
    p.put_proposal(&closed).expect("put_proposal");

    let pending = p.list_proposals(&ProposalQuery::pending()).unwrap();
    let pending_ids: std::collections::BTreeSet<_> = pending.iter().map(|x| x.id).collect();
    assert!(pending_ids.contains(&dated.id));
    assert!(pending_ids.contains(&undated.id));
    assert!(!pending_ids.contains(&closed.id));

    let declined = p
        .list_proposals(&ProposalQuery {
            states: vec![ProposalState::Declined],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(declined.iter().map(|x| x.id).collect::<Vec<_>>(), vec![closed.id]);

    proposal_cleanup(store);
}

fn proposals_are_queried_by_made_since_about_and_run(store: &dyn JournalStore) {
    let p = proposal_store(store);
    let run = RoutineRunId::new();

    let mut old = Proposal::new(
        note_payload("Old"),
        "Create note",
        Timestamp::from_second(1_700_000_000).unwrap(),
        "UTC",
    );
    old.made_by = Some(ProposalSource::Run { run_id: run });
    p.put_proposal(&old).expect("put_proposal");

    let recent = Proposal::new(
        note_payload("Recent"),
        "Create note",
        Timestamp::from_second(1_700_100_000).unwrap(),
        "UTC",
    )
    .with_about(Some(About { kind: AboutKind::Note, id: "note-1".into() }));
    p.put_proposal(&recent).expect("put_proposal");

    let since = p
        .list_proposals(&ProposalQuery {
            made_since: Some(Timestamp::from_second(1_700_050_000).unwrap()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(since.iter().map(|x| x.id).collect::<Vec<_>>(), vec![recent.id]);

    let by_run =
        p.list_proposals(&ProposalQuery { run_id: Some(run), ..Default::default() }).unwrap();
    assert_eq!(by_run.iter().map(|x| x.id).collect::<Vec<_>>(), vec![old.id]);

    let by_about = p
        .list_proposals(&ProposalQuery {
            about_kind: Some(AboutKind::Note),
            about_id: Some("note-1".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_about.iter().map(|x| x.id).collect::<Vec<_>>(), vec![recent.id]);

    proposal_cleanup(store);
}

fn proposals_are_queried_unseen_limited_and_newest_first(store: &dyn JournalStore) {
    let p = proposal_store(store);

    let first = Proposal::new(
        note_payload("A"),
        "Create note",
        Timestamp::from_second(1_700_000_000).unwrap(),
        "UTC",
    );
    let second = Proposal::new(
        note_payload("B"),
        "Create note",
        Timestamp::from_second(1_700_001_000).unwrap(),
        "UTC",
    );
    let third = Proposal::new(
        note_payload("C"),
        "Create note",
        Timestamp::from_second(1_700_002_000).unwrap(),
        "UTC",
    );
    for pr in [&first, &second, &third] {
        p.put_proposal(pr).expect("put_proposal");
    }

    let all = p.list_proposals(&ProposalQuery::default()).unwrap();
    assert_eq!(
        all.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![third.id, second.id, first.id],
        "newest first"
    );

    let capped = p.list_proposals(&ProposalQuery { limit: Some(1), ..Default::default() }).unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, third.id, "a limit keeps the newest, not any one");

    p.mark_proposals_seen(&[first.id]).expect("mark one seen");
    let unseen =
        p.list_proposals(&ProposalQuery { unseen: Some(true), ..Default::default() }).unwrap();
    let unseen_ids: std::collections::BTreeSet<_> = unseen.iter().map(|x| x.id).collect();
    assert!(!unseen_ids.contains(&first.id));
    assert!(unseen_ids.contains(&second.id) && unseen_ids.contains(&third.id));

    proposal_cleanup(store);
}

fn counts_pending_and_unseen_pending(store: &dyn JournalStore) {
    let p = proposal_store(store);
    let one = seeded(store, note_payload("One"), "Create note");
    let two = seeded(store, note_payload("Two"), "Create note");
    assert_eq!(p.count_pending().unwrap(), 2);
    assert_eq!(p.count_unseen_pending().unwrap(), 2);

    p.mark_proposals_seen(&[one.id]).expect("mark one seen");
    assert_eq!(p.count_pending().unwrap(), 2, "seen is not the same as answered");
    assert_eq!(p.count_unseen_pending().unwrap(), 1);

    let mut closed = two.clone();
    closed
        .close(ProposalOutcome::Accepted { at: now(), saved_as: "x".into(), edited: false }, now());
    p.put_proposal(&closed).expect("put_proposal");
    assert_eq!(p.count_pending().unwrap(), 1);
    assert_eq!(p.count_unseen_pending().unwrap(), 0, "the only unseen one is now answered");

    proposal_cleanup(store);
}

fn expire_proposals_closes_only_pending_past_due_rows(store: &dyn JournalStore) {
    let p = proposal_store(store);
    let sweep_at = Timestamp::from_second(1_700_100_000).unwrap();
    let made_at = Timestamp::from_second(1_600_000_000).unwrap();

    let mut overdue = Proposal::new(note_payload("Overdue"), "Create note", made_at, "UTC");
    overdue.expires_at = Timestamp::from_second(1_700_000_000).unwrap();
    p.put_proposal(&overdue).expect("put_proposal");

    let mut not_due_yet = Proposal::new(note_payload("Not due"), "Create note", made_at, "UTC");
    not_due_yet.expires_at = Timestamp::from_second(1_800_000_000).unwrap();
    p.put_proposal(&not_due_yet).expect("put_proposal");

    // Already answered, and also past its own expiry -- the sweep must not
    // touch it a second time.
    let mut already_accepted =
        Proposal::new(note_payload("Accepted"), "Create note", made_at, "UTC");
    already_accepted.expires_at = Timestamp::from_second(1_650_000_000).unwrap();
    let accepted_at = Timestamp::from_second(1_650_000_001).unwrap();
    already_accepted.close(
        ProposalOutcome::Accepted { at: accepted_at, saved_as: "x".into(), edited: false },
        accepted_at,
    );
    p.put_proposal(&already_accepted).expect("put_proposal");

    let closed = p.expire_proposals(sweep_at).expect("expire_proposals");
    assert_eq!(closed, vec![overdue.id], "only the pending, past-due row is swept");

    // Visible both in the clear column...
    let expired = p
        .list_proposals(&ProposalQuery {
            states: vec![ProposalState::Expired],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(expired.iter().map(|x| x.id).collect::<Vec<_>>(), vec![overdue.id]);

    // ...and in the unsealed payload.
    assert_eq!(
        p.get_proposal(overdue.id).unwrap().outcome,
        ProposalOutcome::Expired { at: sweep_at }
    );

    // The other two are untouched.
    assert_eq!(p.get_proposal(not_due_yet.id).unwrap().outcome, ProposalOutcome::Pending);
    assert!(matches!(
        p.get_proposal(already_accepted.id).unwrap().outcome,
        ProposalOutcome::Accepted { .. }
    ));

    proposal_cleanup(store);
}

fn mark_proposals_seen_marks_ids_or_all_pending_but_never_the_answered(store: &dyn JournalStore) {
    let p = proposal_store(store);
    let pending_one = seeded(store, note_payload("One"), "Create note");
    let pending_two = seeded(store, note_payload("Two"), "Create note");

    let mut answered = seeded(store, note_payload("Three"), "Create note");
    answered.close(ProposalOutcome::Declined { at: now(), reason: None }, now());
    p.put_proposal(&answered).expect("put_proposal");

    p.mark_proposals_seen(&[]).expect("mark all pending");
    assert!(p.get_proposal(pending_one.id).unwrap().seen);
    assert!(p.get_proposal(pending_two.id).unwrap().seen);
    assert!(
        !p.get_proposal(answered.id).unwrap().seen,
        "an empty list must not touch an answered proposal"
    );

    let fresh = seeded(store, note_payload("Four"), "Create note");
    p.mark_proposals_seen(&[fresh.id]).expect("mark by id");
    assert!(p.get_proposal(fresh.id).unwrap().seen);

    proposal_cleanup(store);
}
