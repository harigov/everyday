//! Recommended, and the ranking that orders it. See `crate::news::rank`
//! and `docs/plans/podcasts.md`.
//!
//! Thin doors onto the ranker. Ranking itself is a background pass, so none
//! of these declares a change: `recommended` and `ranking_status` read, and
//! `rerank` only forgets the ranks and wakes the pass, whose own writes raise
//! the `Story` change a list reloads on.

use std::sync::Arc;

use serde::Deserialize;

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::news::rank::{self, RankingStatus, Recommendation};
use crate::service::Service;

use super::Nothing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendedArgs {
    /// At most this many; 30 when left out, and never more than 200.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Stories worth reading next, best first.
async fn recommended(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RecommendedArgs,
) -> CommandResult<Vec<Recommendation>> {
    rank::recommended(&svc, args.limit.unwrap_or(0)).await
}

/// Which model ranks, and how many stories it has got through.
async fn ranking_status(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<RankingStatus> {
    rank::status(&svc).await
}

/// Forget every rank and ask about everything again.
async fn rerank(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    rank::rerank(&svc).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "recommended", scope: News, effect: Read,
        args: RecommendedArgs, returns: "Recommendation[]",
        signature: &[("limit", "number | null", false)],
        run: recommended,
    },
    command! {
        name: "ranking_status", scope: News, effect: Read,
        args: Nothing, returns: "RankingStatus", signature: &[],
        run: ranking_status,
    },
    command! {
        name: "rerank", scope: News, effect: Write,
        args: Nothing, returns: "void", signature: &[],
        run: rerank,
    },
];
