//! The summary of an article. See `crate::news::summary` and
//! `docs/plans/feed-usability.md`.
//!
//! Asking returns at once: the article goes on the list a background task works
//! through, and the interface reads `listening_work` for whether it is waiting,
//! under way or failed, with the reason. What lands is announced by the task, as
//! a `Story` and a `File` change, and not by the command that asked.
//!
//! What is asked for here is asked for on purpose, so an article that has a
//! summary already is summarised again -- beside the old one, which is left in
//! Files -- where a source's automation would have left it alone.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::news::summary;
use crate::service::Service;
use std::sync::Arc;

use super::news::StoryRef;

/// Summarise an article, a web feed's story or a newsletter issue, in a file in
/// Files linked to the story. Refused, in a sentence, for an episode (whose
/// summary is the article written from its transcript), for a story with no
/// words to summarise, and when no Medium model is set up to write it -- or,
/// for a newsletter, Mail has not been allowed to send its account's mail to
/// one.
async fn summarize_story(svc: Arc<Service>, _ctx: Ctx, args: StoryRef) -> CommandResult<()> {
    summary::request(&svc, args.id).await
}

pub static COMMANDS: &[crate::command::Command] = &[command! {
    name: "summarize_story", scope: News, effect: Write,
    args: StoryRef, returns: "void",
    signature: &[("id", "StoryId", true)],
    run: summarize_story,
}];
