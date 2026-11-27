//! Unsubscribing from mail: what leaving a sender's list would do, and
//! leaving it -- see [`crate::mail::unsubscribe`].
//!
//! # Gating
//!
//! The `Mail` scope. Unsubscribing sends a request in the person's name --
//! a POST to the sender, or an email from their address -- which is a write
//! the same scope already allows (`send_draft` is `Mail`). The commands take
//! threads, never a bare address: what is acted on is a header the person's
//! own mail carried. `unsubscribe` also wants the vault unlocked, as every
//! command that puts a request on the network does: the lock screen is not a
//! place requests leave the machine from.
//!
//! # What it announces
//!
//! `unsubscribe` declares a thread change with no ids, which has every list
//! and count reload: which messages it marks as asked, and which senders'
//! emails it queues, are only known once it has run, and a reload is cheap
//! beside the requests themselves. The emails it queues go out through the
//! outbox, which announces them as any send.

use std::sync::Arc;

use jatayu_core::id::ThreadId;
use serde::Deserialize;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::mail::unsubscribe::{self, UnsubscribeOffer, UnsubscribeOutcome};
use crate::service::{Service, blocking};

/// The most conversations one `unsubscribe` reads. Each is read whole, every
/// message decrypted, so this is what bounds a press of the button. Quick
/// Cleanup's rows list a sender's threads newest first, and the newest few of
/// each are all `senders` needs.
const MAX_THREADS: usize = 5_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeOfferArgs {
    pub thread: ThreadId,
}

async fn unsubscribe_offer(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: UnsubscribeOfferArgs,
) -> CommandResult<Option<UnsubscribeOffer>> {
    let vault = svc.require()?;
    let packs = svc.packs();
    blocking(move || unsubscribe::offer(&vault, packs.as_deref(), args.thread)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeArgs {
    /// The conversations whose senders to unsubscribe from.
    pub threads: Vec<ThreadId>,
    /// Only these senders' messages, each one sender however many of
    /// `threads` they wrote in -- see
    /// [`unsubscribe::targets_in_threads`]. All senders when absent.
    #[serde(default)]
    pub senders: Option<Vec<String>>,
}

/// Ask every sender `args` names to stop, and say how each went. Senders
/// are compared as addresses: trimmed, without case, and each once.
async fn unsubscribe(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: UnsubscribeArgs,
) -> CommandResult<Vec<UnsubscribeOutcome>> {
    if args.threads.len() > MAX_THREADS {
        return Err(CommandError::new(
            codes::INVALID,
            format!("unsubscribe from at most {MAX_THREADS} conversations at once"),
        ));
    }
    let senders = args.senders.map(|senders| {
        let mut senders: Vec<String> =
            senders.iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        senders.sort();
        senders.dedup();
        senders
    });
    // Nothing named is nothing to do, rather than "everybody".
    if args.threads.is_empty() || senders.as_ref().is_some_and(|s| s.is_empty()) {
        return Ok(Vec::new());
    }
    let vault = svc.require_unlocked()?;
    let packs = svc.packs();
    let found = vault.clone();
    let threads = args.threads;
    let (targets, mut outcomes) = blocking(move || {
        unsubscribe::targets_in_threads(&found, packs.as_deref(), &threads, senders.as_deref())
    })
    .await?;
    let mut asked = unsubscribe::unsubscribe_all(&svc, &vault, targets).await;
    asked.append(&mut outcomes);
    Ok(asked)
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "unsubscribe_offer", scope: Mail, effect: Read,
        args: UnsubscribeOfferArgs, returns: "UnsubscribeOffer | null",
        signature: &[("thread", "ThreadId", true)],
        run: unsubscribe_offer,
    },
    command! {
        name: "unsubscribe", scope: Mail, effect: Write,
        change: Thread/Updated,
        args: UnsubscribeArgs, returns: "UnsubscribeOutcome[]",
        signature: &[("threads", "ThreadId[]", true), ("senders", "string[] | null", false)],
        run: unsubscribe,
    },
];
