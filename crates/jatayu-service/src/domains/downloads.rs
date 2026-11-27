//! Episodes downloaded into the files folder: asking for one, stopping one,
//! removing one, and what is under way. See `crate::news::downloads` and
//! `docs/plans/podcasts.md`.
//!
//! Asking returns at once: the episode goes on a list a background task works
//! through, and the interface reads `list_downloads` for how far each has
//! got. What changes when one lands is announced by the task, as a `Story`
//! and a `File` change, and not by the command that asked.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::news::downloads::{self, DownloadStatus};
use crate::service::Service;
use jatayu_core::StoryId;
use serde::Deserialize;
use std::sync::Arc;

use super::Nothing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRef {
    pub id: StoryId,
}

/// Download one episode into the files folder, whatever its show's policy
/// says. One already there is left alone.
async fn download_episode(svc: Arc<Service>, _ctx: Ctx, args: EpisodeRef) -> CommandResult<()> {
    downloads::request(&svc, args.id)
}

/// Stop one episode's download, or take it off the waiting list, or forget
/// that it failed. It writes nothing: what it changes is what
/// `list_downloads` says.
async fn cancel_download(svc: Arc<Service>, _ctx: Ctx, args: EpisodeRef) -> CommandResult<()> {
    downloads::cancel(&svc, args.id);
    Ok(())
}

/// Move one episode's file to the files trash and unlink it from its story.
async fn remove_episode_download(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: EpisodeRef,
) -> CommandResult<()> {
    downloads::remove_download(&svc, args.id).await
}

/// Every episode waiting, coming down, or that failed since it was last
/// asked for.
async fn list_downloads(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<DownloadStatus>> {
    Ok(downloads::status(&svc))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "download_episode", scope: News, effect: Write,
        change: Story / Updated,
        id: |a: &EpisodeRef| Some(a.id.to_string()),
        args: EpisodeRef, returns: "void",
        signature: &[("id", "StoryId", true)],
        run: download_episode,
    },
    command! {
        name: "cancel_download", scope: News, effect: Write,
        args: EpisodeRef, returns: "void",
        signature: &[("id", "StoryId", true)],
        run: cancel_download,
    },
    command! {
        name: "remove_episode_download", scope: News, effect: Write,
        change: Story / Updated,
        id: |a: &EpisodeRef| Some(a.id.to_string()),
        args: EpisodeRef, returns: "void",
        signature: &[("id", "StoryId", true)],
        run: remove_episode_download,
    },
    command! {
        name: "list_downloads", scope: News, effect: Read,
        args: Nothing, returns: "DownloadStatus[]", signature: &[],
        run: list_downloads,
    },
];
