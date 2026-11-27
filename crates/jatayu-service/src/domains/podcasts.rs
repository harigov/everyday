//! Finding podcasts and feeds to follow: the directory's search, its top
//! charts and genres, and Feedly's feed search. See `crate::news::directory`
//! and `docs/plans/podcasts.md`.
//!
//! Each of these is a request leaving the machine, so a locked vault does not
//! make one -- `discover_feeds` keeps the same rule, for the same reason --
//! and none writes anything: what a hit says about being followed is worked
//! out here, by comparing its feed's address with the feeds followed, so the
//! directory itself needs no vault. Following a hit is `add_news_source`.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::news::directory::{self, FeedHit, PodcastGenre, PodcastHit};
use crate::service::{Service, blocking};
use serde::Deserialize;
use std::sync::Arc;

use super::Nothing;
use super::news::{followed_feeds, is_followed};

/// How many results a search or a chart is asked for when the caller does not
/// say.
const DEFAULT_LIMIT: u32 = 25;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Search {
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Top {
    /// One of [`directory::genres`]; every show when it is left out.
    #[serde(default)]
    pub genre: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The feeds somebody follows, by address.
async fn followed(svc: &Arc<Service>) -> CommandResult<Vec<String>> {
    let vault = svc.require_unlocked()?;
    blocking(move || Ok(followed_feeds(&vault.news_sources()?))).await
}

/// Shows matching what somebody typed, best first, each saying whether it is
/// already followed.
async fn search_podcasts(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Search,
) -> CommandResult<Vec<PodcastHit>> {
    let followed = followed(&svc).await?;
    let mut hits =
        directory::search_podcasts(&args.query, args.limit.unwrap_or(DEFAULT_LIMIT)).await?;
    for hit in &mut hits {
        hit.followed = is_followed(&followed, &hit.feed_url);
    }
    Ok(hits)
}

/// The directory's top shows, in one genre or overall.
async fn top_podcasts(svc: Arc<Service>, _ctx: Ctx, args: Top) -> CommandResult<Vec<PodcastHit>> {
    let followed = followed(&svc).await?;
    let mut hits = directory::top_podcasts(args.genre, args.limit.unwrap_or(DEFAULT_LIMIT)).await?;
    for hit in &mut hits {
        hit.followed = is_followed(&followed, &hit.feed_url);
    }
    Ok(hits)
}

/// The genres `top_podcasts` takes. Fixed, and asked of nobody.
async fn podcast_genres(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<PodcastGenre>> {
    Ok(directory::genres())
}

/// Feeds matching what somebody typed, best first, each saying whether it is
/// already followed.
async fn search_feeds(svc: Arc<Service>, _ctx: Ctx, args: Search) -> CommandResult<Vec<FeedHit>> {
    let followed = followed(&svc).await?;
    let mut hits =
        directory::search_feeds(&args.query, args.limit.unwrap_or(DEFAULT_LIMIT)).await?;
    for hit in &mut hits {
        hit.followed = is_followed(&followed, &hit.feed_url);
    }
    Ok(hits)
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "search_podcasts", scope: News, effect: Read,
        args: Search, returns: "PodcastHit[]",
        signature: &[("query", "string", true), ("limit", "number | null", false)],
        run: search_podcasts,
    },
    command! {
        name: "top_podcasts", scope: News, effect: Read,
        args: Top, returns: "PodcastHit[]",
        signature: &[("genre", "number | null", false), ("limit", "number | null", false)],
        run: top_podcasts,
    },
    command! {
        name: "podcast_genres", scope: News, effect: Read,
        args: Nothing, returns: "PodcastGenre[]", signature: &[],
        run: podcast_genres,
    },
    command! {
        name: "search_feeds", scope: News, effect: Read,
        args: Search, returns: "FeedHit[]",
        signature: &[("query", "string", true), ("limit", "number | null", false)],
        run: search_feeds,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::news::{NewsSource, SourceKind};

    fn web(url: &str) -> NewsSource {
        NewsSource::new(SourceKind::Web { url: url.into() }, "x")
    }

    #[test]
    fn the_arguments_are_what_the_interface_sends_with_a_missing_or_null_limit() {
        let search: Search =
            serde_json::from_value(serde_json::json!({"query": "history"})).unwrap();
        assert_eq!((search.query.as_str(), search.limit), ("history", None));
        let search: Search =
            serde_json::from_value(serde_json::json!({"query": "x", "limit": null})).unwrap();
        assert_eq!(search.limit, None);
        let top: Top =
            serde_json::from_value(serde_json::json!({"genre": null, "limit": 40})).unwrap();
        assert_eq!((top.genre, top.limit), (None, Some(40)));
        let top: Top = serde_json::from_value(serde_json::json!({"genre": 1318})).unwrap();
        assert_eq!((top.genre, top.limit), (Some(1318), None));
        assert!(serde_json::from_value::<Search>(serde_json::json!({})).is_err());
    }

    #[test]
    fn a_hit_is_followed_by_its_feeds_address_whichever_way_it_was_followed() {
        let mut podcast = web("https://feeds.example.com/show/");
        podcast.podcast = Some(Default::default());
        let sources = vec![
            podcast,
            web("https://blog.example.com/feed"),
            NewsSource::new(SourceKind::Newsletters, "Newsletters"),
        ];
        let followed = followed_feeds(&sources);
        assert_eq!(followed.len(), 2);
        // A podcast and a plain feed both count, an address is compared as the
        // refresher would know it, and a newsletter is not a feed.
        assert!(is_followed(&followed, "https://Feeds.example.com/show"));
        assert!(is_followed(&followed, "https://blog.example.com/feed?utm_source=x"));
        assert!(!is_followed(&followed, "https://feeds.example.com/other"));
        assert!(!is_followed(&[], "https://blog.example.com/feed"));
    }
}
