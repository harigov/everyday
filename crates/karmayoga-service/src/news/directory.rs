//! Finding something to follow: Apple's podcast directory and Feedly's feed
//! search. See `docs/plans/podcasts.md`.
//!
//! Neither needs a key. Every request goes over the public client -- the
//! one a feed's own addresses are fetched with (`super::fetch`) -- because
//! what a directory answers is a stranger's address, and so is what it is
//! asked from.
//!
//! # What is asked of whom
//!
//! - **Search** is Apple's iTunes Search API, which answers with each show's
//!   feed address along with its name and artwork.
//! - **Top charts** are two questions, because a chart lists shows by id and
//!   not by feed. The ids come from Apple's marketing-tools chart for all
//!   shows, or from the older `toppodcasts` feed for one genre (the newer one
//!   has no genres), and the feed addresses from iTunes' `lookup`, which is
//!   then put back in the chart's order.
//! - **Feeds** that are not podcasts are Feedly's public feed search.
//!
//! Every answer is capped and every request timed, an error is a sentence
//! that names the service and not the address, and a hit whose feed is not on
//! the internet -- a directory entry can say anything -- is dropped before it
//! is offered, for the reason `import_opml` refuses such an address: the
//! refresher would fetch it from inside the person's network on a stranger's
//! say-so.

use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use karmayoga_core::Vault;
use karmayoga_core::id::BlobId;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Apple's search and lookup. `lookup?id=1,2,3` answers like `search` does.
const ITUNES_SEARCH: &str = "https://itunes.apple.com/search";
const ITUNES_LOOKUP: &str = "https://itunes.apple.com/lookup";

/// Apple's marketing-tools chart of every show, `<this>/<n>/podcasts.json`.
/// It used to live at `rss.applemarketingtools.com`, which now only
/// redirects here.
const CHART: &str = "https://rss.marketingtools.apple.com/api/v2/us/podcasts/top";

/// Apple's older chart, which is the one that has genres:
/// `<this>/limit=<n>/genre=<id>/json`.
const GENRE_CHART: &str = "https://itunes.apple.com/us/rss/toppodcasts";

/// Feedly's public feed search.
const FEEDLY_SEARCH: &str = "https://cloud.feedly.com/v3/search/feeds";

/// The most one answer may be. A page of search results is a few tens of
/// kilobytes; a hundred shows from `lookup` a few hundred.
const MAX_ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// The largest artwork taken for an icon. A show's own picture is as often
/// as not three thousand pixels square, a megabyte or three; it is made into
/// an icon on its way in.
const MAX_ARTWORK_BYTES: usize = 8 * 1024 * 1024;

/// How long one question may take, which is less than the client's own half
/// minute: somebody is looking at a search box.
const ASK_TIMEOUT: Duration = Duration::from_secs(15);

/// How long [`artwork_icon`] may take in all. The icon is a nicety shown
/// beside a source being added; nobody should wait on a slow host for it.
const ARTWORK_BUDGET: Duration = Duration::from_secs(20);

/// How many results one question may ask for.
const MOST_SEARCH: u32 = 50;
const MOST_CHART: u32 = 100;

/// The chart lengths Apple's marketing-tools feed answers to, and only
/// those.
const CHART_LENGTHS: [u32; 4] = [10, 25, 50, 100];

const APPLE: &str = "Apple's podcast directory";
const FEEDLY: &str = "Feedly";

/// One show the directory knows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PodcastHit {
    /// Apple's `collectionId`, as text.
    pub directory_id: String,
    pub title: String,
    pub author: String,
    /// The show's RSS feed: what following it follows.
    pub feed_url: String,
    /// The show's artwork, as an address the webview may load -- see the
    /// module docs of whatever proxies remote pictures for search results.
    pub artwork: String,
    pub genre: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes: Option<u32>,
    pub explicit: bool,
    /// Already followed (filled by the command, by feed address).
    pub followed: bool,
}

/// One of the directory's genres, for the top charts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PodcastGenre {
    pub id: u32,
    pub name: String,
}

/// One feed a search found.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedHit {
    pub feed_url: String,
    pub title: String,
    pub site_url: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribers: Option<u64>,
    /// The site's icon, as an address the webview may load.
    pub icon: String,
    pub followed: bool,
}

// ── The questions ────────────────────────────────────────────────────────

/// Shows matching `query`, best first.
pub async fn search_podcasts(query: &str, limit: u32) -> CommandResult<Vec<PodcastHit>> {
    let query = clean_query(query)?;
    let limit = limit.clamp(1, MOST_SEARCH);
    let url = with_query(
        ITUNES_SEARCH,
        &[
            ("media", "podcast"),
            ("entity", "podcast"),
            ("term", query.as_str()),
            ("limit", &limit.to_string()),
        ],
    );
    let answer = ask(client()?.get(url), APPLE).await?;
    let mut hits = itunes_hits(&answer).map_err(|m| unreadable(APPLE, m))?;
    hits.truncate(limit as usize);
    Ok(hits)
}

/// The directory's top shows, in one genre or overall.
pub async fn top_podcasts(genre: Option<u32>, limit: u32) -> CommandResult<Vec<PodcastHit>> {
    let limit = limit.clamp(1, MOST_CHART);
    let ids = match genre {
        None => {
            // Apple answers to four lengths, so the next one up is asked for
            // and what was not wanted is dropped.
            let length = CHART_LENGTHS.into_iter().find(|&n| n >= limit).unwrap_or(MOST_CHART);
            let request = client()?.get(format!("{CHART}/{length}/podcasts.json"));
            chart_ids(&ask(request, APPLE).await?).map_err(|m| unreadable(APPLE, m))?
        }
        Some(genre) => {
            let request = client()?.get(format!("{GENRE_CHART}/limit={limit}/genre={genre}/json"));
            genre_chart_ids(&ask(request, APPLE).await?).map_err(|m| unreadable(APPLE, m))?
        }
    };
    let ids: Vec<String> = ids.into_iter().take(limit as usize).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let url = with_query(ITUNES_LOOKUP, &[("id", ids.join(",").as_str()), ("entity", "podcast")]);
    let answer = ask(client()?.get(url), APPLE).await?;
    let found = itunes_hits(&answer).map_err(|m| unreadable(APPLE, m))?;
    Ok(in_chart_order(&ids, found))
}

/// The genres [`top_podcasts`] takes: Apple's own, by its own ids.
pub fn genres() -> Vec<PodcastGenre> {
    const GENRES: [(u32, &str); 19] = [
        (1301, "Arts"),
        (1321, "Business"),
        (1303, "Comedy"),
        (1304, "Education"),
        (1483, "Fiction"),
        (1511, "Government"),
        (1512, "Health & Fitness"),
        (1487, "History"),
        (1305, "Kids & Family"),
        (1502, "Leisure"),
        (1310, "Music"),
        (1489, "News"),
        (1314, "Religion & Spirituality"),
        (1533, "Science"),
        (1324, "Society & Culture"),
        (1545, "Sports"),
        (1318, "Technology"),
        (1488, "True Crime"),
        (1309, "TV & Film"),
    ];
    GENRES.into_iter().map(|(id, name)| PodcastGenre { id, name: name.to_string() }).collect()
}

/// Feeds matching `query`, best first.
pub async fn search_feeds(query: &str, limit: u32) -> CommandResult<Vec<FeedHit>> {
    let query = clean_query(query)?;
    let limit = limit.clamp(1, MOST_SEARCH);
    let url =
        with_query(FEEDLY_SEARCH, &[("query", query.as_str()), ("count", &limit.to_string())]);
    let answer = ask(client()?.get(url), FEEDLY).await?;
    let mut hits = feedly_hits(&answer).map_err(|m| unreadable(FEEDLY, m))?;
    hits.truncate(limit as usize);
    Ok(hits)
}

/// A show's artwork made into an icon and kept in the blob store, or `None`
/// when it could not be had: the host did not answer, it is not on the
/// internet, or what it sent is not a picture. Over the public client, and
/// best-effort in every direction, as [`super::fetch::site_icon`] is -- a
/// show with initials where its picture would be is not worth an error.
pub async fn artwork_icon(vault: &Arc<Vault>, url: &str) -> Option<BlobId> {
    let address = public_address(url)?;
    let fetched = async {
        let response = client()
            .ok()?
            .get(address)
            .header(ACCEPT, "image/jpeg, image/png, image/*;q=0.8")
            .header(USER_AGENT, crate::http::USER_AGENT)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        http::read_capped(response, MAX_ARTWORK_BYTES, String::new).await.ok()
    };
    let bytes = tokio::time::timeout(ARTWORK_BUDGET, fetched).await.ok()??;
    // Sniffed rather than believed, and no vector pictures: an SVG can carry
    // script, and the webview would be handed it as an icon.
    if !matches!(
        karmayoga_core::media::sniff_mime(&bytes),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return None;
    }
    match crate::covers::store(vault, bytes).await {
        Ok(blob) => Some(blob),
        Err(e) => {
            tracing::warn!(error = %e, "could not store a show's artwork");
            None
        }
    }
}

// ── Plumbing ─────────────────────────────────────────────────────────────

fn client() -> CommandResult<&'static reqwest::Client> {
    http::public_client()
}

/// `base` with `pairs` as its query, each escaped. What somebody typed goes
/// in as a value and never as part of the address.
fn with_query(base: &str, pairs: &[(&str, &str)]) -> reqwest::Url {
    let mut url = reqwest::Url::parse(base).expect("a fixed address that parses");
    url.query_pairs_mut().extend_pairs(pairs);
    url
}

/// What somebody typed to search for: trimmed, and cut to a length no search
/// box needs more than.
fn clean_query(query: &str) -> CommandResult<String> {
    let query: String = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if query.is_empty() {
        return Err(CommandError::new(codes::INVALID, "type something to search for"));
    }
    Ok(query.chars().take(200).collect())
}

/// Ask, and read the answer as JSON, within the cap and the time.
async fn ask(request: reqwest::RequestBuilder, service: &str) -> CommandResult<Value> {
    let request =
        request.header(ACCEPT, "application/json").header(USER_AGENT, crate::http::USER_AGENT);
    let sent = tokio::time::timeout(ASK_TIMEOUT, async {
        let response = request.send().await.map_err(|e| trouble(service, &e))?;
        if !response.status().is_success() {
            return Err(refused(service, response.status()));
        }
        http::read_capped(response, MAX_ANSWER_BYTES, || {
            format!("{service} sent more than this app will read")
        })
        .await
    })
    .await;
    let bytes = match sent {
        Ok(read) => read?,
        Err(_) => {
            return Err(CommandError::new(
                codes::UNREACHABLE,
                format!("{service} did not answer in time"),
            ));
        }
    };
    serde_json::from_slice(&bytes).map_err(|_| unreadable(service, "it is not an answer".into()))
}

/// A request that failed on the way, with the address left out of it.
fn trouble(service: &str, e: &reqwest::Error) -> CommandError {
    let message = if e.is_timeout() {
        format!("{service} did not answer in time")
    } else if e.is_connect() {
        format!("could not reach {service}. Check the network.")
    } else {
        format!("the request to {service} failed")
    };
    CommandError::new(http::transport_code(e), message)
}

/// An answer that was not a yes.
fn refused(service: &str, status: StatusCode) -> CommandError {
    match status.as_u16() {
        429 => CommandError::new(
            codes::RATE_LIMITED,
            format!("{service} asked us to slow down. Try again in a moment."),
        ),
        500..=599 => {
            CommandError::new(codes::NETWORK, format!("{service} is having trouble ({status})"))
        }
        _ => CommandError::new(codes::NETWORK, format!("{service} answered {status}")),
    }
}

fn unreadable(service: &str, why: String) -> CommandError {
    CommandError::new(
        codes::UNREADABLE,
        format!("{service} sent an answer that could not be read: {why}"),
    )
}

/// `url` as an address a request may be made to: http(s), and not a machine
/// on the person's own network by what it says.
fn public_address(url: &str) -> Option<reqwest::Url> {
    let address = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(address.scheme(), "http" | "https") {
        return None;
    }
    let host = address.host_str()?;
    (!karmayoga_core::chat::is_private_network_host(host)).then_some(address)
}

/// `url` kept as written when it is an address on the internet, else empty.
fn kept_address(url: Option<&str>) -> String {
    url.map(str::trim)
        .filter(|url| public_address(url).is_some())
        .map(str::to_string)
        .unwrap_or_default()
}

// ── Reading the answers (pure) ───────────────────────────────────────────

/// A show, as `search` and `lookup` answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Collection {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    collection_id: Option<u64>,
    #[serde(default)]
    collection_name: Option<String>,
    #[serde(default)]
    track_name: Option<String>,
    #[serde(default)]
    artist_name: Option<String>,
    #[serde(default)]
    feed_url: Option<String>,
    #[serde(default)]
    artwork_url600: Option<String>,
    #[serde(default)]
    artwork_url100: Option<String>,
    #[serde(default)]
    primary_genre_name: Option<String>,
    #[serde(default)]
    track_count: Option<u32>,
    #[serde(default)]
    collection_explicitness: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Collections {
    #[serde(default)]
    results: Vec<Collection>,
}

/// The shows in an iTunes `search` or `lookup` answer that can be followed:
/// those with a name and a feed on the internet. A show that is only in
/// Apple's own catalogue has no feed to give.
fn itunes_hits(answer: &Value) -> Result<Vec<PodcastHit>, String> {
    let answered: Collections = serde_json::from_value(answer.clone())
        .map_err(|_| "it is not a list of shows".to_string())?;
    Ok(answered.results.into_iter().filter_map(podcast_hit).collect())
}

fn podcast_hit(show: Collection) -> Option<PodcastHit> {
    if show.kind.as_deref().is_some_and(|kind| kind != "podcast") {
        return None;
    }
    let feed_url = kept_address(show.feed_url.as_deref());
    let title = show.collection_name.or(show.track_name).map(|t| squash(&t)).unwrap_or_default();
    if feed_url.is_empty() || title.is_empty() {
        return None;
    }
    let artwork = kept_address(show.artwork_url600.as_deref().or(show.artwork_url100.as_deref()));
    Some(PodcastHit {
        directory_id: show.collection_id?.to_string(),
        title,
        author: show.artist_name.map(|a| squash(&a)).unwrap_or_default(),
        feed_url,
        artwork,
        genre: show.primary_genre_name.map(|g| squash(&g)).unwrap_or_default(),
        episodes: show.track_count,
        explicit: show
            .collection_explicitness
            .is_some_and(|explicitness| explicitness.eq_ignore_ascii_case("explicit")),
        followed: false,
    })
}

/// The ids on the marketing-tools chart, best first.
fn chart_ids(answer: &Value) -> Result<Vec<String>, String> {
    let results = answer
        .pointer("/feed/results")
        .and_then(Value::as_array)
        .ok_or_else(|| "it is not a chart".to_string())?;
    Ok(unique(results.iter().filter_map(|entry| id_text(entry.get("id")?))))
}

/// The ids on the older, per-genre chart, best first. A chart of one show is
/// an object where a longer one is a list.
fn genre_chart_ids(answer: &Value) -> Result<Vec<String>, String> {
    let entries = answer.pointer("/feed/entry");
    let entries: Vec<&Value> = match entries {
        // A genre with nothing in it has no `entry` at all.
        None => Vec::new(),
        Some(Value::Array(list)) => list.iter().collect(),
        Some(single @ Value::Object(_)) => vec![single],
        Some(_) => return Err("it is not a chart".to_string()),
    };
    Ok(unique(
        entries.into_iter().filter_map(|entry| id_text(entry.pointer("/id/attributes/im:id")?)),
    ))
}

/// An id, which Apple writes as text in one place and a number in another.
fn id_text(id: &Value) -> Option<String> {
    match id {
        Value::String(text) => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// `ids` without a repeat, in order.
fn unique(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.filter(|id| seen.insert(id.clone())).collect()
}

/// What `lookup` found, in the order the chart listed it. `lookup` answers in
/// no order of its own, and a chart is nothing but its order.
fn in_chart_order(ids: &[String], found: Vec<PodcastHit>) -> Vec<PodcastHit> {
    let mut by_id: HashMap<String, PodcastHit> =
        found.into_iter().map(|hit| (hit.directory_id.clone(), hit)).collect();
    ids.iter().filter_map(|id| by_id.remove(id)).collect()
}

/// A feed, as Feedly answers.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedlyFeed {
    #[serde(default)]
    feed_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    website: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    subscribers: Option<u64>,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    visual_url: Option<String>,
    /// Feedly's own word for a feed it could not read.
    #[serde(default)]
    valid: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct FeedlyAnswer {
    #[serde(default)]
    results: Vec<FeedlyFeed>,
}

/// The feeds in a Feedly search answer that can be followed.
fn feedly_hits(answer: &Value) -> Result<Vec<FeedHit>, String> {
    let answered: FeedlyAnswer = serde_json::from_value(answer.clone())
        .map_err(|_| "it is not a list of feeds".to_string())?;
    Ok(answered.results.into_iter().filter_map(feed_hit).collect())
}

fn feed_hit(feed: FeedlyFeed) -> Option<FeedHit> {
    if feed.valid == Some(false) {
        return None;
    }
    // Feedly's id for a feed is `feed/` and its address.
    let id = feed.feed_id?;
    let address = id.trim().strip_prefix("feed/").unwrap_or(id.trim());
    let feed_url = kept_address(Some(address));
    if feed_url.is_empty() {
        return None;
    }
    let site_url = kept_address(feed.website.as_deref());
    let title = feed.title.map(|t| squash(&t)).filter(|t| !t.is_empty()).unwrap_or_else(|| {
        reqwest::Url::parse(&feed_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| feed_url.clone())
    });
    Some(FeedHit {
        feed_url,
        title,
        site_url,
        description: feed.description.map(|d| squash(&d)).unwrap_or_default(),
        subscribers: feed.subscribers,
        icon: kept_address(feed.icon_url.as_deref().or(feed.visual_url.as_deref())),
        followed: false,
    })
}

/// Words on one line.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `search` answers for a show, trimmed to what is read.
    fn answer(json: &str) -> Value {
        serde_json::from_str(json).expect("a fixture that is JSON")
    }

    const SEARCH: &str = r#"{
      "resultCount": 4,
      "results": [
        {"wrapperType":"track", "kind":"podcast", "artistId":256201037, "collectionId":173001861,
         "trackId":173001861, "artistName":"Dan Carlin",
         "collectionName":"Dan Carlin's Hardcore History",
         "trackName":"Dan Carlin's Hardcore History",
         "feedUrl":"https://feeds.feedburner.com/dancarlin/history?format=xml",
         "artworkUrl60":"https://is1-ssl.mzstatic.com/a/60x60bb.jpg",
         "artworkUrl100":"https://is1-ssl.mzstatic.com/a/100x100bb.jpg",
         "collectionExplicitness":"notExplicit", "trackCount":13,
         "primaryGenreName":"History",
         "artworkUrl600":"https://is1-ssl.mzstatic.com/a/600x600bb.jpg",
         "genreIds":["1487", "26"], "genres":["History", "Podcasts"]},
        {"wrapperType":"track", "kind":"podcast", "collectionId":42,
         "artistName":"  Some  Band ", "collectionName":"Loud\n Show",
         "feedUrl":"http://loud.example/rss",
         "artworkUrl100":"https://is1-ssl.mzstatic.com/b/100x100bb.jpg",
         "collectionExplicitness":"explicit", "primaryGenreName":"Music"},
        {"wrapperType":"track", "kind":"podcast", "collectionId":43,
         "artistName":"Apple Only", "collectionName":"No Feed To Give"},
        {"wrapperType":"track", "kind":"podcast", "collectionId":44, "artistName":"Sneaky",
         "collectionName":"Points Inside", "feedUrl":"http://192.168.1.10/feed.xml"}
      ]
    }"#;

    #[test]
    fn a_search_answer_becomes_the_shows_that_can_be_followed() {
        let hits = itunes_hits(&answer(SEARCH)).unwrap();
        // The show Apple only holds has no feed, and the one that names a
        // machine on a private network is not offered.
        let titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, ["Dan Carlin's Hardcore History", "Loud Show"]);

        assert_eq!(
            hits[0],
            PodcastHit {
                directory_id: "173001861".into(),
                title: "Dan Carlin's Hardcore History".into(),
                author: "Dan Carlin".into(),
                feed_url: "https://feeds.feedburner.com/dancarlin/history?format=xml".into(),
                artwork: "https://is1-ssl.mzstatic.com/a/600x600bb.jpg".into(),
                genre: "History".into(),
                episodes: Some(13),
                explicit: false,
                followed: false,
            }
        );
        // The smaller artwork when there is no larger one, text on one line,
        // `explicit` as Apple says it.
        assert_eq!(hits[1].artwork, "https://is1-ssl.mzstatic.com/b/100x100bb.jpg");
        assert_eq!(hits[1].author, "Some Band");
        assert_eq!(hits[1].episodes, None);
        assert!(hits[1].explicit);
    }

    #[test]
    fn what_is_not_a_podcast_or_not_an_answer_is_left_out() {
        let mixed = answer(
            r#"{"results":[
                {"kind":"song","collectionId":1,"collectionName":"A Song","feedUrl":"https://s.example/f"},
                {"kind":"podcast","collectionId":2,"collectionName":"   ","feedUrl":"https://s.example/f"},
                {"kind":"podcast","collectionName":"No Id","feedUrl":"https://s.example/f"},
                {"collectionId":4,"trackName":"Named By Track","feedUrl":"https://s.example/4"}
            ]}"#,
        );
        let hits = itunes_hits(&mixed).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Named By Track");
        assert_eq!(hits[0].directory_id, "4");

        assert!(itunes_hits(&answer(r#"{"resultCount":0,"results":[]}"#)).unwrap().is_empty());
        assert!(itunes_hits(&answer("{}")).unwrap().is_empty());
        assert!(itunes_hits(&answer("[1,2]")).is_err());
    }

    #[test]
    fn the_marketing_chart_is_its_ids_in_order() {
        let chart = answer(
            r#"{"feed":{"title":"Top Shows","results":[
                {"id":"1200361736","name":"The Daily","kind":"podcasts"},
                {"id":"360084272","name":"The Joe Rogan Experience"},
                {"id":1192761536,"name":"Numbers Too"},
                {"id":"360084272","name":"Listed Twice"},
                {"name":"No Id"}
            ]}}"#,
        );
        assert_eq!(chart_ids(&chart).unwrap(), ["1200361736", "360084272", "1192761536"]);
        assert!(chart_ids(&answer(r#"{"feed":{"results":[]}}"#)).unwrap().is_empty());
        assert!(chart_ids(&answer(r#"{"feed":{}}"#)).is_err());
    }

    #[test]
    fn the_genre_chart_is_its_ids_whether_it_lists_one_show_or_many() {
        let many = answer(
            r#"{"feed":{"entry":[
                {"im:name":{"label":"The Rest Is History"},
                 "id":{"label":"https://podcasts.apple.com/us/podcast/x/id1537788786?uo=2",
                       "attributes":{"im:id":"1537788786"}}},
                {"id":{"attributes":{"im:id":"173001861"}}},
                {"title":{"label":"No id"}}
            ]}}"#,
        );
        assert_eq!(genre_chart_ids(&many).unwrap(), ["1537788786", "173001861"]);

        // With `limit=1` the one entry is not in a list.
        let one = answer(r#"{"feed":{"entry":{"id":{"attributes":{"im:id":"173001861"}}}}}"#);
        assert_eq!(genre_chart_ids(&one).unwrap(), ["173001861"]);

        // A genre with no shows has no `entry`.
        assert!(genre_chart_ids(&answer(r#"{"feed":{"author":{}}}"#)).unwrap().is_empty());
        assert!(genre_chart_ids(&answer(r#"{"feed":{"entry":"nope"}}"#)).is_err());
    }

    #[test]
    fn a_looked_up_chart_is_put_back_in_the_charts_order_and_drops_what_has_no_feed() {
        let found = itunes_hits(&answer(SEARCH)).unwrap();
        let ids: Vec<String> = ["42", "999", "173001861"].iter().map(|s| s.to_string()).collect();
        let ordered = in_chart_order(&ids, found);
        let titles: Vec<&str> = ordered.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, ["Loud Show", "Dan Carlin's Hardcore History"]);
    }

    const FEEDLY_ANSWER: &str = r#"{"results":[
        {"created":1370158690830,
         "description":"Entries from Daring Fireball.\n Feel free to comment.",
         "feedId":"feed/http://daringfireball.net/index.xml",
         "iconUrl":"https://storage.googleapis.com/site-assets/icon",
         "title":"Daring Fireball","subscribers":146051,"valid":true,
         "visualUrl":"https://storage.googleapis.com/site-assets/visual",
         "website":"https://daringfireball.net/"},
        {"feedId":"feed/https://b.example/rss","title":"  ","website":"javascript:alert(1)",
         "visualUrl":"https://storage.googleapis.com/site-assets/b"},
        {"feedId":"feed/https://dead.example/rss","title":"Dead","valid":false},
        {"feedId":"feed/http://localhost:8080/rss","title":"Inside"},
        {"title":"No id"}
      ],"success":true}"#;

    #[test]
    fn a_feedly_answer_becomes_the_feeds_that_can_be_followed() {
        let hits = feedly_hits(&answer(FEEDLY_ANSWER)).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0],
            FeedHit {
                feed_url: "http://daringfireball.net/index.xml".into(),
                title: "Daring Fireball".into(),
                site_url: "https://daringfireball.net/".into(),
                description: "Entries from Daring Fireball. Feel free to comment.".into(),
                subscribers: Some(146_051),
                icon: "https://storage.googleapis.com/site-assets/icon".into(),
                followed: false,
            }
        );
        // No title is the host, a site that is not a web address is none, and
        // the larger picture stands in for a missing icon.
        assert_eq!(hits[1].title, "b.example");
        assert_eq!(hits[1].site_url, "");
        assert_eq!(hits[1].icon, "https://storage.googleapis.com/site-assets/b");
        assert_eq!(hits[1].subscribers, None);
    }

    #[test]
    fn the_genres_are_apples_own_and_none_twice() {
        let genres = genres();
        assert_eq!(genres.len(), 19);
        let mut ids: Vec<u32> = genres.iter().map(|g| g.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 19);
        let named = |id: u32| genres.iter().find(|g| g.id == id).map(|g| g.name.as_str());
        assert_eq!(named(1318), Some("Technology"));
        assert_eq!(named(1488), Some("True Crime"));
        assert_eq!(named(1309), Some("TV & Film"));
    }

    #[test]
    fn a_search_is_words_on_one_line_and_not_nothing() {
        assert_eq!(clean_query("  hardcore \n history ").unwrap(), "hardcore history");
        assert_eq!(clean_query(&"a".repeat(500)).unwrap().chars().count(), 200);
        for nothing in ["", "   ", "\n\t"] {
            assert_eq!(clean_query(nothing).unwrap_err().code, codes::INVALID, "{nothing:?}");
        }
    }

    #[test]
    fn only_addresses_on_the_internet_are_kept() {
        for stranger in
            ["https://example.com/feed", " http://8.8.8.8/x ", "https://feeds.example.org"]
        {
            assert!(public_address(stranger).is_some(), "{stranger}");
        }
        for own in [
            "http://192.168.1.2/feed",
            "http://127.0.0.1/",
            "http://localhost:8080/",
            "http://router/rss",
            "https://box.local/rss",
            "https://bridge.tail1234.ts.net/rss",
            "ftp://example.com/feed",
            "javascript:alert(1)",
            "not an address",
            "",
        ] {
            assert!(public_address(own).is_none(), "{own}");
        }
    }

    #[test]
    fn a_service_that_is_down_is_a_sentence_without_an_address() {
        for status in
            [StatusCode::NOT_FOUND, StatusCode::BAD_GATEWAY, StatusCode::TOO_MANY_REQUESTS]
        {
            let e = refused(APPLE, status);
            assert!(e.message.contains("Apple's podcast directory"), "{}", e.message);
            assert!(!e.message.contains("http"), "{}", e.message);
        }
        assert_eq!(refused(FEEDLY, StatusCode::TOO_MANY_REQUESTS).code, codes::RATE_LIMITED);
        assert_eq!(refused(FEEDLY, StatusCode::BAD_GATEWAY).code, codes::NETWORK);
    }

    #[test]
    fn what_was_typed_is_a_value_in_the_query_and_never_part_of_the_address() {
        let url = with_query(ITUNES_SEARCH, &[("term", "a&limit=9#x y"), ("limit", "5")]);
        assert_eq!(url.host_str(), Some("itunes.apple.com"));
        assert_eq!(url.path(), "/search");
        assert_eq!(url.fragment(), None);
        let pairs: Vec<(String, String)> =
            url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(
            pairs,
            [("term".to_string(), "a&limit=9#x y".to_string()), ("limit".into(), "5".into())]
        );
    }

    #[test]
    fn a_chart_is_asked_for_at_a_length_apple_answers_to() {
        let length =
            |limit: u32| CHART_LENGTHS.into_iter().find(|&n| n >= limit).unwrap_or(MOST_CHART);
        assert_eq!(length(1), 10);
        assert_eq!(length(10), 10);
        assert_eq!(length(11), 25);
        assert_eq!(length(60), 100);
        assert_eq!(length(100), 100);
    }
}
