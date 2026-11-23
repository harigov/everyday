//! An article turned into audio. See `crate::news::audio`,
//! `crate::speech::render` and `docs/plans/podcasts.md`.
//!
//! Asking returns at once: the article goes on a list a background task works
//! through, and the interface reads `listening_work` for how far it has got.
//! What changes when one lands is announced by the task, as a `Story` and a
//! `File` change, and not by the command that asked.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::news::audio;
use crate::service::Service;
use std::sync::Arc;

use super::news::StoryRef;

/// Turn an article into audio kept in the files folder, linked to the story
/// and played like an episode. One that has its audio already is left alone;
/// one that cannot be read fails, in a sentence, on `listening_work`.
async fn render_story_audio(svc: Arc<Service>, _ctx: Ctx, args: StoryRef) -> CommandResult<()> {
    audio::request(&svc, args.id).await
}

pub static COMMANDS: &[crate::command::Command] = &[command! {
    name: "render_story_audio", scope: News, effect: Write,
    args: StoryRef, returns: "void",
    signature: &[("id", "StoryId", true)],
    run: render_story_audio,
}];
