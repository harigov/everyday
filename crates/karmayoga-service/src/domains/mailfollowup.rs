//! Follow-ups' commands -- thin doors onto [`crate::mail::followup`]: the
//! threads waiting at the top of the Inbox, and dismissing one.

use std::sync::Arc;

use karmayoga_core::id::{AccountId, ThreadId};
use karmayoga_core::mail::Thread;
use serde::{Deserialize, Serialize};

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::Service;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailFollowUpsArgs {
    /// One account's, or every mail account's.
    #[serde(default)]
    pub account: Option<AccountId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpList {
    /// Each with its `followUp` set: Reply ones first, newest first.
    pub threads: Vec<Thread>,
}

async fn mail_follow_ups(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MailFollowUpsArgs,
) -> CommandResult<FollowUpList> {
    let threads = crate::mail::followup::follow_ups(&svc, args.account).await?;
    Ok(FollowUpList { threads })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DismissFollowUp {
    pub id: ThreadId,
}

async fn dismiss_follow_up(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: DismissFollowUp,
) -> CommandResult<()> {
    crate::mail::followup::dismiss_follow_up(&svc, args.id).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "mail_follow_ups", scope: Mail, effect: Read,
        args: MailFollowUpsArgs, returns: "FollowUpList",
        signature: &[("account", "AccountId | null", false)],
        run: mail_follow_ups,
    },
    command! {
        name: "dismiss_follow_up", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &DismissFollowUp| vec![a.id.to_string()],
        args: DismissFollowUp, returns: "void",
        signature: &[("id", "ThreadId", true)],
        run: dismiss_follow_up,
    },
];
