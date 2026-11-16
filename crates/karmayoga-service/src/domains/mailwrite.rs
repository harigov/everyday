//! Writing help's commands -- thin doors onto [`crate::mailwrite`], where
//! the gate, the voice and the model calls all live.
//!
//! The three that ask a model are `effect: Read`, on `summarize_thread`'s own
//! reasoning (`domains::mail`'s own module docs): reaching a model is not a
//! write to the vault, and none of them saves anything anybody sees -- a
//! suggestion is only ever written into a draft once a person has picked it,
//! through `save_draft`, which announces its own `change:`. `suggest_replies`
//! does keep what it offered in the learning record
//! (`crate::mailvoicelearn::remember_offered`), which nothing draws. Settings → Mail writing is here too, being what
//! they are all told.

use std::sync::Arc;

use karmayoga_core::id::{AccountId, MailMessageId, ThreadId};
use karmayoga_core::mail::{MailWriting, VoiceLearning};
use serde::{Deserialize, Serialize};

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::mailvoicelearn::VoiceLearningView;
use crate::mailwrite::{ImproveMode, ReplySuggestion, WrittenText};
use crate::service::{Service, blocking};

use super::Nothing;

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
    /// The address it is being written from, when that is not the account's own.
    #[serde(default)]
    pub identity: Option<String>,
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
        args.identity,
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

// ---- Settings → Mail writing -----------------------------------------------

async fn mail_writing(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<MailWriting> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.mail_writing()?)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMailWriting {
    pub writing: MailWriting,
}

async fn save_mail_writing(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveMailWriting,
) -> CommandResult<MailWriting> {
    let vault = svc.require()?;
    let saved = blocking(move || {
        vault.save_mail_writing(&args.writing)?;
        Ok(vault.mail_writing()?)
    })
    .await?;
    svc.mail_replies_cache_clear();
    Ok(saved)
}

// ---- what was learned from their replies ----------------------------------

async fn voice_learning(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<VoiceLearningView> {
    let vault = svc.require()?;
    blocking(move || Ok(VoiceLearningView::from(&vault.voice_learning()?))).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveVoiceLearning {
    /// The notes as the person left them. Empty forgets everything learned,
    /// and the next pass starts again from their recent replies.
    pub notes: String,
}

async fn save_voice_learning(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveVoiceLearning,
) -> CommandResult<VoiceLearningView> {
    let vault = svc.require()?;
    let s = svc.clone();
    let saved = blocking(move || {
        let _held = s.voice_learning_lock();
        let mut learning = vault.voice_learning()?;
        let notes = args.notes.trim().to_string();
        if notes.is_empty() {
            // Forgotten from now on, not rebuilt on the next tick from the
            // same replies: the next pass learns only from what is sent after
            // this, and only once there is enough of it.
            learning = VoiceLearning {
                offered: learning.offered,
                learned_through: Some(s.now()),
                ..VoiceLearning::default()
            };
        } else if notes != learning.notes {
            learning.notes = notes;
            learning.edited = true;
            learning.learned_at = Some(s.now());
        }
        vault.save_voice_learning(&learning)?;
        Ok(VoiceLearningView::from(&learning))
    })
    .await?;
    svc.mail_replies_cache_clear();
    Ok(saved)
}

async fn learn_from_replies(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<VoiceLearningView> {
    let learning = crate::mailvoicelearn::learn_now(&svc).await?;
    Ok(VoiceLearningView::from(&learning))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "voice_learning", scope: Mail, effect: Read,
        args: Nothing, returns: "VoiceLearningView", signature: &[],
        run: voice_learning,
    },
    command! {
        name: "save_voice_learning", scope: Mail, effect: Write,
        change: Settings / Updated,
        args: SaveVoiceLearning, returns: "VoiceLearningView",
        signature: &[("notes", "string", true)],
        run: save_voice_learning,
    },
    command! {
        name: "learn_from_replies", scope: Mail, effect: Write,
        change: Settings / Updated,
        args: Nothing, returns: "VoiceLearningView", signature: &[],
        run: learn_from_replies,
    },
    command! {
        name: "mail_writing", scope: Mail, effect: Read,
        args: Nothing, returns: "MailWriting", signature: &[],
        run: mail_writing,
    },
    command! {
        name: "save_mail_writing", scope: Mail, effect: Write,
        change: Settings / Updated,
        args: SaveMailWriting, returns: "MailWriting",
        signature: &[("writing", "MailWriting", true)],
        run: save_mail_writing,
    },
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
            ("identity", "string | null", false),
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
