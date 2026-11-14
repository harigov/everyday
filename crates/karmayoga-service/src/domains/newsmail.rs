//! The Feed's view of Mail: who sends newsletters, so one can be followed.
//!
//! Its own module, beside `news`, for the reason `mailcleanup` is beside
//! `mail`: it reads mail to answer a question the Feed asks.
//!
//! # Gating
//!
//! The `Mail` scope, and nothing finer: the answer is built from the same
//! decrypted messages `inbox_senders` counts, and a client that may read the
//! Inbox may already learn who writes to it. The Feed's own `News` scope is
//! not enough, since it would let a client that holds only that learn who
//! writes to somebody's inbox.

use std::sync::Arc;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::news::newsletters::{self, NewsletterSender};
use crate::service::{Service, blocking};
use serde::Deserialize;

/// How far back a sender must have written to be listed, when a caller does
/// not say: a quarter, long enough that a monthly shows up and a sender who
/// stopped writing last year does not.
const DEFAULT_DAYS: u32 = 90;

/// The longest window, as `inbox_senders` has it.
const MAX_DAYS: u32 = 365;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterSendersArgs {
    /// How far back to look, in days, ending now: 1 to `MAX_DAYS`.
    /// `DEFAULT_DAYS` when absent or `null`.
    #[serde(default)]
    pub days: Option<u32>,
}

async fn newsletter_senders(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: NewsletterSendersArgs,
) -> CommandResult<Vec<NewsletterSender>> {
    let days = args.days.unwrap_or(DEFAULT_DAYS);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(CommandError::new(
            codes::INVALID,
            format!("look between 1 and {MAX_DAYS} days back"),
        ));
    }
    let vault = svc.require()?;
    blocking(move || newsletters::senders(&svc, &vault, days)).await
}

pub static COMMANDS: &[crate::command::Command] = &[command! {
    name: "newsletter_senders", scope: Mail, effect: Read,
    args: NewsletterSendersArgs, returns: "NewsletterSender[]",
    signature: &[("days", "number | null", false)],
    run: newsletter_senders,
}];
