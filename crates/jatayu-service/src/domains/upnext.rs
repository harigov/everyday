//! Up Next, what was read and heard, and the Feed's settings. See
//! `crate::news::{queue, consumed}` and `docs/plans/podcasts.md`.
//!
//! Up Next is one list for the whole window: what to hear, watch or read
//! next, wherever it came from -- an episode, an article from the Feed, a
//! file in Files, an item on a shelf. The player and the Feed draw it from
//! [`list_queue`] and are told when it changes by [`Kind::Queue`].
//!
//! [`record_progress`] is what the player calls every fifteen seconds, so it
//! announces the consumption it wrote and nothing more; what finishing does
//! (leaving Up Next, marking a story read) is announced by
//! `crate::news::consumed`, where it happens.
//!
//! [`Kind::Queue`]: crate::events::Kind::Queue

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::news::consumed::{self, ProgressReport};
use crate::news::queue::{self, QueueView};
use crate::news::rank;
use crate::service::Service;
use jatayu_core::news::{
    AddedBy, ConsumedQuery, Consumption, ContentRef, FeedSettings, QueueEntry,
};
use jatayu_core::{ConsumptionId, QueueEntryId, QueuePlace};
use serde::Deserialize;
use std::sync::Arc;

use super::Nothing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddToQueue {
    pub what: ContentRef,
    /// Where it goes; the end when it is left out.
    #[serde(default)]
    pub place: Option<QueuePlace>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFromQueue {
    pub ids: Vec<QueueEntryId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveInQueue {
    pub id: QueueEntryId,
    /// Just before this entry; the end when it is left out.
    #[serde(default)]
    pub before: Option<QueueEntryId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordProgress {
    pub progress: ProgressReport,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentArg {
    pub what: ContentRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consumed {
    pub query: ConsumedQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumptionRef {
    pub id: ConsumptionId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFeedSettings {
    pub settings: FeedSettings,
}

// ---- Up Next --------------------------------------------------------------

async fn list_queue(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<QueueView> {
    queue::view(&svc).await
}

/// Put something in Up Next. Something already there is moved to where it
/// was asked to go, and keeps its entry.
///
/// An episode that has not been downloaded is asked for; streaming is not
/// offered, so playback never depends on the network.
async fn add_to_queue(svc: Arc<Service>, _ctx: Ctx, args: AddToQueue) -> CommandResult<QueueEntry> {
    // `put`, not `add`: the command's declared change is the announcement,
    // and it carries the window that asked.
    queue::put(&svc, args.what, args.place.unwrap_or_default(), AddedBy::Person).await
}

async fn remove_from_queue(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RemoveFromQueue,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.remove_from_queue(&args.ids)).await?;
    // What was taken out is Recommended's to offer again.
    rank::forget_window(&svc);
    Ok(())
}

async fn move_in_queue(svc: Arc<Service>, _ctx: Ctx, args: MoveInQueue) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.move_in_queue(args.id, args.before)).await
}

// ---- what was read, heard and watched ----------------------------------------

/// Fold one report from the player, the reader or Listen into the record of
/// the thing, creating it on the first. See [`consumed::record`].
async fn record_progress(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RecordProgress,
) -> CommandResult<Consumption> {
    consumed::record(&svc, args.progress).await
}

async fn get_consumption(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ContentArg,
) -> CommandResult<Option<Consumption>> {
    svc.on_vault(move |vault| vault.consumption(&args.what)).await
}

async fn list_consumed(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Consumed,
) -> CommandResult<Vec<Consumption>> {
    svc.on_vault(move |vault| vault.consumptions(&args.query)).await
}

/// Forget that something was read or heard. The words kept in the files
/// folder stay; only the record goes. Destructive, like forgetting screen
/// time: it is a memory somebody asked to lose, and there is no getting it
/// back.
async fn forget_consumption(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConsumptionRef,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.forget_consumption(args.id)).await?;
    // Recommended shows how far through each story is.
    rank::forget_window(&svc);
    Ok(())
}

// ---- Settings -> Feed -------------------------------------------------------

async fn feed_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<FeedSettings> {
    svc.on_vault(|vault| vault.feed_settings()).await
}

/// Check, tidy and save the Feed's settings, and answer what was saved: the
/// criteria's words are trimmed and its weights held to range, so the
/// interface shows what is kept and not what was typed.
///
/// New criteria make every rank stale, so the ranker is woken; and if Up Next
/// is to be kept a certain length it is topped up now rather than whenever
/// something next finishes. That is a courtesy, not a part of saving: it
/// never fails the save.
async fn save_feed_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveFeedSettings,
) -> CommandResult<FeedSettings> {
    let saved = svc.on_vault(move |vault| vault.save_feed_settings(&args.settings)).await?;
    rank::wake(&svc);
    if saved.autofill.is_some()
        && let Err(e) = queue::autofill(&svc).await
    {
        tracing::info!(error = %e, "could not top Up Next up after the Feed's settings changed");
    }
    Ok(saved)
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_queue", scope: News, effect: Read,
        args: Nothing, returns: "QueueView", signature: &[],
        run: list_queue,
    },
    command! {
        name: "add_to_queue", scope: News, effect: Write,
        change: Queue / Created,
        args: AddToQueue, returns: "QueueEntry",
        signature: &[("what", "ContentRef", true), ("place", "QueuePlace | null", false)],
        run: add_to_queue,
    },
    command! {
        name: "remove_from_queue", scope: News, effect: Write,
        change: Queue / Deleted,
        ids: |a: &RemoveFromQueue| a.ids.iter().map(|i| i.to_string()).collect(),
        args: RemoveFromQueue, returns: "void",
        signature: &[("ids", "QueueEntryId[]", true)],
        run: remove_from_queue,
    },
    command! {
        name: "move_in_queue", scope: News, effect: Write,
        change: Queue / Updated,
        id: |a: &MoveInQueue| Some(a.id.to_string()),
        args: MoveInQueue, returns: "void",
        signature: &[("id", "QueueEntryId", true), ("before", "QueueEntryId | null", false)],
        run: move_in_queue,
    },
    command! {
        name: "record_progress", scope: News, effect: Write,
        change: Consumption / Updated,
        args: RecordProgress, returns: "Consumption",
        signature: &[("progress", "ProgressReport", true)],
        run: record_progress,
    },
    command! {
        name: "get_consumption", scope: News, effect: Read,
        args: ContentArg, returns: "Consumption | null",
        signature: &[("what", "ContentRef", true)],
        run: get_consumption,
    },
    command! {
        name: "list_consumed", scope: News, effect: Read,
        args: Consumed, returns: "Consumption[]",
        signature: &[("query", "ConsumedQuery", true)],
        run: list_consumed,
    },
    command! {
        name: "forget_consumption", scope: News, effect: Destructive,
        change: Consumption / Deleted,
        id: |a: &ConsumptionRef| Some(a.id.to_string()),
        args: ConsumptionRef, returns: "void",
        signature: &[("id", "ConsumptionId", true)],
        run: forget_consumption,
    },
    command! {
        name: "feed_settings", scope: News, effect: Read,
        args: Nothing, returns: "FeedSettings", signature: &[],
        run: feed_settings,
    },
    command! {
        name: "save_feed_settings", scope: News, effect: Write,
        change: Settings / Updated,
        args: SaveFeedSettings, returns: "FeedSettings",
        signature: &[("settings", "FeedSettings", true)],
        run: save_feed_settings,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_place_is_tagged_by_type_and_left_out_means_the_end() {
        let id = QueueEntryId::new();
        let at = |v: serde_json::Value| serde_json::from_value::<AddToQueue>(v).unwrap();

        let a = at(json!({ "what": { "type": "story", "id": jatayu_core::StoryId::new() } }));
        assert_eq!(a.place, None);
        assert_eq!(a.place.unwrap_or_default(), QueuePlace::End);

        let a = at(json!({ "what": { "type": "story", "id": jatayu_core::StoryId::new() },
                           "place": { "type": "next" } }));
        assert_eq!(a.place, Some(QueuePlace::Next));
        let a = at(json!({ "what": { "type": "file", "id": jatayu_core::FileId::new() },
                           "place": { "type": "before", "id": id } }));
        assert_eq!(a.place, Some(QueuePlace::Before(id)));
        let a = at(json!({ "what": { "type": "item", "id": jatayu_core::ItemId::new() },
                           "place": null }));
        assert_eq!(a.place, None);
    }

    #[test]
    fn a_progress_report_needs_only_what_and_mode() {
        let story = jatayu_core::StoryId::new();
        let args: RecordProgress = serde_json::from_value(json!({
            "progress": { "what": { "type": "story", "id": story }, "mode": "listen" }
        }))
        .unwrap();
        assert_eq!(args.progress.position_ms, None);
        assert_eq!(args.progress.finished, None);

        let args: RecordProgress = serde_json::from_value(json!({
            "progress": {
                "what": { "type": "story", "id": story }, "mode": "listen",
                "positionMs": 15000, "durationMs": 600000,
                "heard": { "startMs": 0, "endMs": 15000 }, "finished": false
            }
        }))
        .unwrap();
        assert_eq!(args.progress.position_ms, Some(15_000));
        assert_eq!(args.progress.heard.unwrap().end_ms, 15_000);
        assert_eq!(args.progress.finished, Some(false));
    }

    #[test]
    fn every_command_here_is_in_scope_news_and_a_read_never_announces() {
        for command in COMMANDS {
            assert_eq!(command.scope, crate::ctx::Scope::News, "{}", command.name);
            if matches!(command.effect, jatayu_core::agent::tools::Effect::Read) {
                assert!(command.change.is_none(), "{} is a read", command.name);
            } else {
                assert!(command.change.is_some(), "{} writes and says nothing", command.name);
            }
        }
    }
}
