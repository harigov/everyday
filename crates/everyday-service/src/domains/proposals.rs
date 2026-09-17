//! Proposals: work the assistant prepared and did not do, waiting for a yes
//! or a no. See `everyday_core::proposal` and `docs/plans/dreaming.md`.
//!
//! Six commands. Accepting is the one with rules in it, and they live in the
//! vault -- except for mail, whose "yes" is a send and belongs to the outbox.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::Service;
use everyday_core::ProposalId;
use everyday_core::proposal::{DeclineReason, Proposal, ProposedRecord};
use everyday_core::store::proposals::ProposalQuery;
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

async fn accept_proposal(svc: Arc<Service>, _ctx: Ctx, args: Accept) -> CommandResult<Proposal> {
    svc.on_vault(move |vault| {
        vault.accept_proposal(args.id, args.edited, args.confirm, Timestamp::now())
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
