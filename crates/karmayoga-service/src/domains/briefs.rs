//! The daily briefs: switching one on, and reading back what they wrote.
//!
//! A brief is a routine the application owns (`RoutineKind::Brief`), so
//! everything else about it -- its time, where it goes, what it covers, how
//! long it is, the person's own paragraph -- is saved with `save_routine`, and
//! "make one now" is `run_routine`. These two are what a routine's commands
//! cannot say: make the routine the first time it is wanted, and list what
//! the briefs wrote. See `docs/plans/briefs.md`.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use karmayoga_core::brief::BriefKind;
use karmayoga_core::routine::{Routine, RoutineRun};
use serde::Deserialize;
use std::sync::Arc;

/// The most briefs one call hands back: a month of mornings and evenings.
const MOST_BRIEFS: u32 = 60;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBrief {
    pub kind: BriefKind,
    pub on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Briefs {
    /// One brief's history. Absent for both, newest first.
    #[serde(default)]
    pub kind: Option<BriefKind>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Switch a brief on or off, making its routine the first time.
async fn set_brief(svc: Arc<Service>, _ctx: Ctx, args: SetBrief) -> CommandResult<Routine> {
    svc.on_vault(move |vault| vault.set_brief(args.kind, args.on)).await
}

/// What the briefs wrote, newest first: each a finished run with its brief.
async fn briefs(svc: Arc<Service>, _ctx: Ctx, args: Briefs) -> CommandResult<Vec<RoutineRun>> {
    let vault = svc.require()?;
    let limit = args.limit.unwrap_or(14).clamp(1, MOST_BRIEFS);
    blocking(move || Ok(vault.briefs(args.kind, limit)?)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "set_brief", scope: Agent, effect: Write,
        change: Routine / Updated,
        args: SetBrief, returns: "Routine",
        signature: &[("kind", "BriefKind", true), ("on", "boolean", true)],
        run: set_brief,
    },
    command! {
        name: "briefs", scope: Agent, effect: Read,
        args: Briefs, returns: "RoutineRun[]",
        signature: &[("kind", "BriefKind | null", false), ("limit", "number | null", false)],
        run: briefs,
    },
];
