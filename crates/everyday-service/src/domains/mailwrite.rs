//! Writing help's commands -- thin doors onto [`crate::mailwrite`], where
//! the gate, the voice and the model calls all live.
//!
//! Every command here is `effect: Read`, on `summarize_thread`'s own
//! reasoning (`domains::mail`'s own module docs): reaching a model is not a
//! write to the vault, and none of the three ever saves anything -- a
//! suggestion is only ever written once a person has picked it, through
//! `save_draft`, which announces its own `change:`.

use std::sync::Arc;

use everyday_core::id::{AccountId, MailMessageId, ThreadId};
use serde::{Deserialize, Serialize};

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::mailwrite::{ImproveMode, ReplySuggestion, WrittenText};
use crate::service::Service;

// ---- suggested replies ------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestRepliesArgs {
    pub id: ThreadId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplySuggestions {
    pub suggestions: Vec<ReplySuggestion>,
}

/// `suggest_replies`'s whole body is `crate::mailwrite::suggest_replies` --
/// the eligibility check, the gate, the cache and the model call all live
/// there.
async fn suggest_replies(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SuggestRepliesArgs,
) -> CommandResult<ReplySuggestions> {
    let suggestions = crate::mailwrite::suggest_replies(&svc, args.id).await?;
    Ok(ReplySuggestions { suggestions })
}

// ---- a draft from a few words ----------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftWithAiArgs {
    pub account: AccountId,
    #[serde(default)]
    pub in_reply_to: Option<MailMessageId>,
    pub instruction: String,
    #[serde(default)]
    pub current_text: Option<String>,
}

async fn draft_with_ai(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: DraftWithAiArgs,
) -> CommandResult<WrittenText> {
    crate::mailwrite::draft_with_ai(
        &svc,
        args.account,
        args.in_reply_to,
        args.instruction,
        args.current_text,
    )
    .await
}

// ---- improving what was written --------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImproveWritingArgs {
    pub account: AccountId,
    pub text: String,
    pub mode: ImproveMode,
    #[serde(default)]
    pub instruction: Option<String>,
}

async fn improve_writing(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ImproveWritingArgs,
) -> CommandResult<WrittenText> {
    crate::mailwrite::improve_writing(&svc, args.account, args.text, args.mode, args.instruction)
        .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "suggest_replies", scope: Mail, effect: Read,
        args: SuggestRepliesArgs, returns: "ReplySuggestions",
        signature: &[("id", "ThreadId", true)],
        run: suggest_replies,
    },
    command! {
        name: "draft_with_ai", scope: Mail, effect: Read,
        args: DraftWithAiArgs, returns: "WrittenText",
        signature: &[
            ("account", "AccountId", true),
            ("inReplyTo", "MailMessageId | null", false),
            ("instruction", "string", true),
            ("currentText", "string | null", false),
        ],
        run: draft_with_ai,
    },
    command! {
        name: "improve_writing", scope: Mail, effect: Read,
        args: ImproveWritingArgs, returns: "WrittenText",
        signature: &[
            ("account", "AccountId", true),
            ("text", "string", true),
            ("mode", "ImproveMode", true),
            ("instruction", "string | null", false),
        ],
        run: improve_writing,
    },
];
