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
use karmayoga_core::Error;
use karmayoga_core::brief::{BriefKind, PodcastBrief};
use karmayoga_core::routine::{Routine, RoutineKind, RoutineRun};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

/// The most briefs one call hands back: a month of mornings and evenings.
const MOST_BRIEFS: u32 = 60;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBrief {
    pub kind: BriefKind,
    pub on: bool,
    /// The daily podcast's settings -- what it is made from, how long it
    /// runs, who says it -- saved with the switch. Only for `podcast`; the
    /// other briefs' are saved with the routine (`save_routine`), as the
    /// podcast's may be.
    #[serde(default)]
    pub podcast: Option<PodcastBrief>,
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

/// Switch a brief on or off, making its routine the first time -- and, for
/// the daily podcast, saving what it is made from.
///
/// The settings are made acceptable first -- a length that overshot is held
/// to what a podcast may run, a source named twice kept once -- and then
/// checked: every source named must be one that is followed, or the podcast
/// would be made from nothing and never say why. Checked before the switch
/// moves, so a refusal leaves the brief as it was.
async fn set_brief(svc: Arc<Service>, _ctx: Ctx, args: SetBrief) -> CommandResult<Routine> {
    svc.on_vault(move |vault| {
        let podcast = match args.podcast {
            None => None,
            Some(_) if args.kind != BriefKind::Podcast => {
                return Err(Error::Invalid("only the daily podcast has podcast settings".into()));
            }
            Some(podcast) => {
                let podcast = podcast.tidied();
                if !podcast.sources.is_empty() {
                    let followed: HashSet<_> =
                        vault.news_sources()?.into_iter().map(|s| s.id).collect();
                    if podcast.sources.iter().any(|id| !followed.contains(id)) {
                        return Err(Error::Invalid(
                            "a source chosen for the daily podcast is not one that is followed"
                                .into(),
                        ));
                    }
                }
                podcast.validate()?;
                Some(podcast)
            }
        };
        let mut routine = vault.set_brief(args.kind, args.on)?;
        if let Some(podcast) = podcast
            && let RoutineKind::Brief(spec) = &mut routine.kind
        {
            spec.podcast = Some(podcast);
            vault.save_routine(&routine)?;
            routine = vault.routine(routine.id)?;
        }
        Ok(routine)
    })
    .await
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
        signature: &[
            ("kind", "BriefKind", true),
            ("on", "boolean", true),
            ("podcast", "PodcastBrief | null", false),
        ],
        run: set_brief,
    },
    command! {
        name: "briefs", scope: Agent, effect: Read,
        args: Briefs, returns: "RoutineRun[]",
        signature: &[("kind", "BriefKind | null", false), ("limit", "number | null", false)],
        run: briefs,
    },
];
