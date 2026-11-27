//! Karma: what each day's deeds came to, and the total since it started.
//!
//! One read. The rules are [`jatayu_core::karma`], tested there; the
//! gathering is `Vault::karma`. What is here is the window and "today".
//!
//! Scoped `All` rather than to any one domain: a report reads finished
//! tasks, trackers, the journal and the library, and names what it found
//! there, so a client granted only one of those would learn the others
//! through it.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use jatayu_core::karma::KarmaReport;
use jatayu_core::model::{local_date_in, system_tz};
use jiff::civil::Date;
use serde::Deserialize;
use std::sync::Arc;

/// The most days one report gives day by day: a year and a little, enough
/// for a year's heatmap padded to whole weeks.
const LONGEST_WINDOW: i64 = 400;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KarmaWindow {
    /// The first day given day by day.
    pub from: Date,
    /// The last, inclusive.
    pub to: Date,
}

async fn karma(svc: Arc<Service>, _ctx: Ctx, args: KarmaWindow) -> CommandResult<KarmaReport> {
    let days = (args.to - args.from).get_days();
    if args.to < args.from || i64::from(days) > LONGEST_WINDOW {
        return Err(CommandError::new(
            codes::INVALID,
            format!("a window runs forwards and covers at most {LONGEST_WINDOW} days"),
        ));
    }
    let vault = svc.require()?;
    let today = local_date_in(svc.now(), &system_tz());
    blocking(move || Ok(vault.karma(args.from, args.to, today)?)).await
}

pub static COMMANDS: &[crate::command::Command] = &[command! {
    name: "karma", scope: All, effect: Read,
    args: KarmaWindow, returns: "KarmaReport",
    signature: &[("from", "string", true), ("to", "string", true)],
    run: karma,
}];
