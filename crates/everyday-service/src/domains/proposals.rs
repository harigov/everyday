//! Proposals: work the assistant prepared and did not do, waiting for a yes
//! or a no. See `everyday_core::proposal` and `docs/plans/dreaming.md`.
//!
//! Six commands. Accepting is the one with rules in it, and they live in the
//! vault -- except for mail, whose "yes" is a send and belongs to the outbox.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};
use everyday_core::id::DraftId;
use everyday_core::proposal::{
    DeclineReason, Outcome, Payload, Proposal, ProposalKind, ProposedRecord,
};
use everyday_core::store::proposals::ProposalQuery;
use everyday_core::{ProposalId, Vault};
use jiff::Timestamp;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposals {
    #[serde(default)]
    pub query: ProposalQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalRef {
    pub id: ProposalId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    pub id: ProposalId,
    /// The record as the person changed it before saying yes. Absent means
    /// as proposed.
    #[serde(default)]
    pub edited: Option<ProposedRecord>,
    /// For a memory: accepted as *true*, from the memory list, rather than
    /// merely "keep that".
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decline {
    pub id: ProposalId,
    #[serde(default)]
    pub reason: Option<DeclineReason>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenProposals {
    /// Which proposals to mark. Empty means every pending one.
    #[serde(default)]
    pub ids: Vec<ProposalId>,
}

async fn list_proposals(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Proposals,
) -> CommandResult<Vec<Proposal>> {
    svc.on_vault(move |vault| vault.proposals(&args.query)).await
}

async fn get_proposal(svc: Arc<Service>, _ctx: Ctx, args: ProposalRef) -> CommandResult<Proposal> {
    svc.on_vault(move |vault| vault.proposal(args.id)).await
}

/// Accept a proposal.
///
/// Every kind but one goes straight to [`everyday_core::Vault::accept_proposal`],
/// which carries the rules: validate, check references, save through the
/// domain's own path, close. `Mail` is the exception -- sending is the
/// outbox's job, not the vault's, so its "yes" is handled here: the draft is
/// queued to send through exactly [`crate::domains::mail::queue_send`], the
/// same tail a person's own "send" in the composer runs, and the proposal is
/// closed on the result rather than inside the vault call.
///
/// Either way, a successful accept also raises a second [`Change`] for the
/// record it saved -- the task, block, memory, routine, note or draft --
/// beside the `Proposal` change the command table raises for every write in
/// this file, so a window showing that list refreshes without having to
/// special-case where a row came from.
async fn accept_proposal(svc: Arc<Service>, ctx: Ctx, args: Accept) -> CommandResult<Proposal> {
    let vault = svc.require()?;
    let id = args.id;
    let proposal = blocking({
        let vault = vault.clone();
        move || Ok(vault.proposal(id)?)
    })
    .await?;
    if !proposal.is_pending() {
        return Err(CommandError::new(codes::INVALID, "this proposal has already been answered"));
    }

    let accepted = match proposal.payload {
        Payload::SendMail { draft_id } => accept_mail_proposal(&svc, &vault, id, draft_id).await?,
        _ => {
            blocking(move || {
                Ok(vault.accept_proposal(id, args.edited, args.confirm, Timestamp::now())?)
            })
            .await?
        }
    };

    if let Some(change) = change_for_accept(&accepted) {
        svc.events().changed(Change { origin: ctx.caller.origin().map(str::to_string), ..change });
    }
    Ok(accepted)
}

/// The second [`Change`] a successful accept raises, for the record the
/// proposal actually saved or removed -- `None` for anything that did not
/// finish `Accepted` (there is nothing to report for a decline raised from
/// inside the vault call, which answers `Err` instead).
fn change_for_accept(proposal: &Proposal) -> Option<Change> {
    let Outcome::Accepted { saved_as, .. } = &proposal.outcome else { return None };
    let kind = match proposal.kind {
        ProposalKind::Task => Kind::Task,
        ProposalKind::Block => Kind::Block,
        ProposalKind::Memory => Kind::Memory,
        ProposalKind::Routine => Kind::Routine,
        ProposalKind::Note => Kind::Note,
        ProposalKind::Mail => Kind::Draft,
    };
    let op = match &proposal.payload {
        Payload::Create { .. } => Op::Created,
        Payload::Replace { .. } => Op::Updated,
        Payload::Delete { .. } => Op::Deleted,
        // Accepting a mail proposal moves an existing draft from editing to
        // queued; nothing is created.
        Payload::SendMail { .. } => Op::Updated,
    };
    Some(Change { kind, op, id: Some(saved_as.clone()), ids: Vec::new(), origin: None })
}

/// Accept a `SendMail` proposal: send the draft it points at, then close the
/// proposal here -- not inside [`everyday_core::Vault::accept_proposal`],
/// which refuses `Mail` outright, because queuing a send is the outbox's
/// write, not the vault's own domain save.
///
/// A draft that is gone, sent, or discarded answers `Err` from
/// [`crate::domains::mail::queue_send`] -- [`everyday_core::Vault::queue_draft_send`]'s
/// own checks -- and closes the proposal `Declined { Other }` with that
/// same message, rather than leave it pending over something that can no
/// longer happen.
async fn accept_mail_proposal(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    id: ProposalId,
    draft_id: DraftId,
) -> CommandResult<Proposal> {
    let now = Timestamp::now();
    match crate::domains::mail::queue_send(svc, draft_id, None, None).await {
        Ok(draft) => {
            close_proposal(
                vault,
                id,
                Outcome::Accepted { at: now, saved_as: draft.id.to_string(), edited: false },
                now,
            )
            .await
        }
        Err(err) => {
            let _ = close_proposal(
                vault,
                id,
                Outcome::Declined {
                    at: now,
                    reason: Some(DeclineReason::Other { text: err.message.clone() }),
                },
                now,
            )
            .await;
            Err(err)
        }
    }
}

/// Close a proposal with a decided [`Outcome`], from the service rather than
/// through [`everyday_core::Vault::accept_proposal`] or
/// [`everyday_core::Vault::decline_proposal`] -- both of which insist on
/// deciding the outcome themselves. Only [`accept_mail_proposal`] needs
/// this: the vault refuses to touch a `Mail` payload at all.
async fn close_proposal(
    vault: &Arc<Vault>,
    id: ProposalId,
    outcome: Outcome,
    now: Timestamp,
) -> CommandResult<Proposal> {
    let vault = vault.clone();
    blocking(move || {
        let mut proposal = vault.proposal(id)?;
        proposal.close(outcome, now);
        proposal.seen = true;
        vault.save_proposal(&proposal)?;
        Ok(proposal)
    })
    .await
}

async fn decline_proposal(svc: Arc<Service>, _ctx: Ctx, args: Decline) -> CommandResult<Proposal> {
    svc.on_vault(move |vault| vault.decline_proposal(args.id, args.reason, Timestamp::now())).await
}

async fn mark_proposals_seen(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SeenProposals,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.mark_proposals_seen(&args.ids)).await
}

async fn unseen_proposals(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<u64> {
    svc.on_vault(move |vault| vault.unseen_proposals()).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_proposals", scope: Agent, effect: Read,
        args: Proposals, returns: "Proposal[]",
        signature: &[("query", "ProposalQuery", false)],
        run: list_proposals,
    },
    command! {
        name: "get_proposal", scope: Agent, effect: Read,
        args: ProposalRef, returns: "Proposal",
        signature: &[("id", "ProposalId", true)],
        run: get_proposal,
    },
    command! {
        name: "accept_proposal", scope: Agent, effect: Write,
        change: Proposal / Updated,
        id: |a: &Accept| Some(a.id.to_string()),
        args: Accept, returns: "Proposal",
        signature: &[
            ("id", "ProposalId", true),
            ("edited", "ProposedRecord | null", false),
            ("confirm", "boolean", false),
        ],
        run: accept_proposal,
    },
    command! {
        name: "decline_proposal", scope: Agent, effect: Write,
        change: Proposal / Updated,
        id: |a: &Decline| Some(a.id.to_string()),
        args: Decline, returns: "Proposal",
        signature: &[("id", "ProposalId", true), ("reason", "DeclineReason | null", false)],
        run: decline_proposal,
    },
    command! {
        name: "mark_proposals_seen", scope: Agent, effect: Write,
        change: Proposal / Updated,
        ids: |a: &SeenProposals| a.ids.iter().map(|id| id.to_string()).collect(),
        args: SeenProposals, returns: "void",
        signature: &[("ids", "ProposalId[]", false)],
        run: mark_proposals_seen,
    },
    command! {
        name: "unseen_proposals", scope: Agent, effect: Read,
        args: Nothing, returns: "number", signature: &[],
        run: unseen_proposals,
    },
];
