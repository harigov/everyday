//! Transcripts and articles: an episode or a recording transcribed, an
//! episode's article written again, and what is under way. See
//! `crate::news::{transcribe, article}` and `docs/plans/podcasts.md`.
//!
//! Asking returns at once: the work goes on a list a background task works
//! through, and the interface reads `listening_work` for how far each piece
//! has got. What changes when one lands is announced by the task, as a `Story`
//! and a `File` change, and not by the command that asked.
//!
//! What is asked for here is asked for on purpose, so a transcript that
//! already exists is made again -- beside the old one, which is left as it
//! is -- where a policy or a first play would have left it alone.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::news::transcribe::{self, Job};
use crate::news::{WorkItem, article, audio};
use crate::service::{Service, blocking};
use std::sync::Arc;

use super::Nothing;
use super::files::FileRef;
use super::news::StoryRef;

/// Transcribe an episode: the publisher's own transcript when its feed names
/// one, otherwise by listening to it, downloading it first if it is not on
/// this computer. For an episode, its article is written from the transcript
/// next. One that cannot be transcribed fails, in a sentence, on
/// `listening_work`.
async fn transcribe_episode(svc: Arc<Service>, _ctx: Ctx, args: StoryRef) -> CommandResult<()> {
    let vault = svc.require_unlocked()?;
    ready_to_write(&vault)?;
    let id = args.id;
    // Read on the blocking pool, as every read of the vault from here is.
    blocking(move || {
        if vault.story(id)?.media.is_none() {
            return Err(CommandError::new(
                codes::INVALID,
                "That story has no episode to transcribe.",
            ));
        }
        Ok(())
    })
    .await?;
    transcribe::request_again(&svc, Job::Story(args.id));
    Ok(())
}

/// Transcribe a recording or a film in Files. One kept by Health is refused.
async fn transcribe_file(svc: Arc<Service>, _ctx: Ctx, args: FileRef) -> CommandResult<()> {
    let vault = svc.require_unlocked()?;
    ready_to_write(&vault)?;
    let id = args.id;
    blocking(move || transcribe::check_recording(&vault.file(id)?)).await?;
    transcribe::request_again(&svc, Job::File(args.id));
    Ok(())
}

/// Write an episode's article again from its transcript, keeping the show
/// notes the feed carried below it. Refused, in a sentence, for an episode
/// with no transcript, and when no Medium model is set up to write it.
async fn write_article(svc: Arc<Service>, _ctx: Ctx, args: StoryRef) -> CommandResult<()> {
    let vault = svc.require_unlocked()?;
    ready_to_write(&vault)?;
    let id = args.id;
    blocking(move || article::ready(&vault, id)).await?;
    transcribe::request_article(&svc, args.id);
    Ok(())
}

/// Everything the Feed is working on in the background for what is heard and
/// read: transcripts, articles, summaries, and articles turned into audio.
/// Waiting, under way, and failed since last asked for.
async fn listening_work(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<WorkItem>> {
    let mut work = transcribe::status(&svc);
    work.extend(audio::status(&svc));
    Ok(work)
}

/// Refuses, in a sentence, a vault this process may not write to: a transcript
/// is a file and a link.
fn ready_to_write(vault: &karmayoga_core::Vault) -> CommandResult<()> {
    if vault.is_writable() {
        return Ok(());
    }
    Err(CommandError::new(
        codes::VAULT_IN_USE,
        "Another copy of Karma Yoga has this vault open for writing, so nothing can be \
         transcribed here.",
    ))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "transcribe_episode", scope: News, effect: Write,
        args: StoryRef, returns: "void",
        signature: &[("id", "StoryId", true)],
        run: transcribe_episode,
    },
    command! {
        name: "transcribe_file", scope: News, effect: Write,
        args: FileRef, returns: "void",
        signature: &[("id", "FileId", true)],
        run: transcribe_file,
    },
    command! {
        name: "write_article", scope: News, effect: Write,
        args: StoryRef, returns: "void",
        signature: &[("id", "StoryId", true)],
        run: write_article,
    },
    command! {
        name: "listening_work", scope: News, effect: Read,
        args: Nothing, returns: "WorkItem[]", signature: &[],
        run: listening_work,
    },
];
