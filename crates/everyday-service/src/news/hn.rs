//! Hacker News, through Algolia's API: one request for a whole list, with
//! points and comment counts, and no key. Pure: this builds the address and
//! reads the answer; `refresh` makes the request.
//!
//! | List | Request |
//! |---|---|
//! | Front page | `search?tags=front_page&hitsPerPage=30` |
//! | Show HN | `search_by_date?tags=show_hn&numericFilters=points>N` |
//! | Ask HN | `search_by_date?tags=ask_hn&numericFilters=points>N` |
//! | Top (above N points) | `search_by_date?tags=story&numericFilters=points>N` |
//!
//! The front page is Algolia's own ranking, and it cannot be filtered by
//! points on the server: [`keep`] is the filter the refresher applies to what
//! comes back, and it is harmless on the other lists, which the server has
//! already cut to the same threshold.

use super::{Incoming, canonical};
use everyday_core::news::{BodyOrigin, HnList};
use jiff::Timestamp;
use serde::Deserialize;

const BASE: &str = "https://hn.algolia.com/api/v1/";

/// What [`HnList::Top`] means by "above N points" when it is given no N.
const TOP_DEFAULT_POINTS: u32 = 100;

/// The Algolia address for one list.
pub fn url(list: HnList, min_points: u32) -> String {
    let (path, tags, per_page) = match list {
        HnList::Front => ("search", "front_page", 30),
        HnList::Show => ("search_by_date", "show_hn", 30),
        HnList::Ask => ("search_by_date", "ask_hn", 30),
        HnList::Top => ("search_by_date", "story", 50),
    };
    let mut url = format!("{BASE}{path}?tags={tags}&hitsPerPage={per_page}");
    let points = threshold(list, min_points);
    if list != HnList::Front && points > 0 {
        // `points>=N`, with the operators encoded.
        url.push_str(&format!("&numericFilters=points%3E%3D{points}"));
    }
    url
}

/// The points a story needs to be on `list`: what the source asked for, and
/// for [`HnList::Top`], whose whole definition it is, a hundred if it asked
/// for nothing.
fn threshold(list: HnList, min_points: u32) -> u32 {
    if list == HnList::Top && min_points == 0 { TOP_DEFAULT_POINTS } else { min_points }
}

/// Does `item` have the points a story needs to be on `list`? Algolia cannot
/// filter the front page by points, so the refresher asks this of everything
/// that comes back; a story with no score counts as having none.
pub fn keep(list: HnList, min_points: u32, item: &Incoming) -> bool {
    item.score.unwrap_or(0) >= threshold(list, min_points)
}

/// What Algolia answers with, as far as this reads it.
#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    hits: Vec<Hit>,
}

/// One hit. Every field is optional because a hit can be a comment, a job
/// or a poll as well as a story, and none of them has all of these.
#[derive(Deserialize)]
struct Hit {
    #[serde(rename = "objectID")]
    object_id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    author: Option<String>,
    points: Option<u32>,
    num_comments: Option<u32>,
    created_at_i: Option<i64>,
    story_text: Option<String>,
}

/// Read Algolia's answer into items. An Ask HN post's `story_text` becomes
/// its `content` with [`everyday_core::news::BodyOrigin::HackerNews`]; every
/// item's `discussion_url` is its `news.ycombinator.com/item?id=` page, and a
/// post with no link of its own takes that as its `url` too.
pub fn parse(json: &[u8]) -> Result<Vec<Incoming>, String> {
    let response: Response = serde_json::from_slice(json)
        .map_err(|e| format!("Hacker News answered with something unexpected: {e}."))?;
    Ok(response.hits.into_iter().filter_map(item).collect())
}

/// One story. `None` for a hit that is not one: it has no id or no title.
fn item(hit: Hit) -> Option<Incoming> {
    let id = hit.object_id?.trim().to_string();
    let title = hit.title?.split_whitespace().collect::<Vec<_>>().join(" ");
    if id.is_empty() || title.is_empty() {
        return None;
    }

    let discussion_url = format!("https://news.ycombinator.com/item?id={id}");
    // A post with no link of its own -- an Ask HN, a Launch HN -- is its own
    // page. So is one whose link is not a web address, which is somebody's
    // mistake, and not an address to hand to the browser.
    let url = hit
        .url
        .map(|url| url.trim().to_string())
        .filter(|url| canonical::canonicalise(url).is_some())
        .unwrap_or_else(|| discussion_url.clone());

    let text = hit.story_text.filter(|text| !text.trim().is_empty());
    Some(Incoming {
        identity: canonical::identity(&url, &format!("hn:{id}"), &title),
        title,
        author: hit.author.unwrap_or_default().trim().to_string(),
        published: hit.created_at_i.and_then(|secs| Timestamp::from_second(secs).ok()),
        // The same words stand in as the summary, for the refresher to cut
        // to a line: Hacker News has no other.
        summary: text.clone().unwrap_or_default(),
        content: text,
        base_url: url.clone(),
        url,
        score: hit.points,
        comments: hit.num_comments,
        discussion_url,
        origin: BodyOrigin::HackerNews,
        ..Incoming::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALGOLIA: &str = include_str!("../../tests/fixtures/news/hn-algolia.json");

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn each_list_has_its_request() {
        let cases = [
            (HnList::Front, 0, "search?tags=front_page&hitsPerPage=30"),
            // The front page cannot be filtered by points, so asking is no use.
            (HnList::Front, 50, "search?tags=front_page&hitsPerPage=30"),
            (HnList::Show, 0, "search_by_date?tags=show_hn&hitsPerPage=30"),
            (
                HnList::Show,
                20,
                "search_by_date?tags=show_hn&hitsPerPage=30&numericFilters=points%3E%3D20",
            ),
            (HnList::Ask, 0, "search_by_date?tags=ask_hn&hitsPerPage=30"),
            (
                HnList::Ask,
                5,
                "search_by_date?tags=ask_hn&hitsPerPage=30&numericFilters=points%3E%3D5",
            ),
            // Top is defined by its points, a hundred if none are given.
            (
                HnList::Top,
                0,
                "search_by_date?tags=story&hitsPerPage=50&numericFilters=points%3E%3D100",
            ),
            (
                HnList::Top,
                300,
                "search_by_date?tags=story&hitsPerPage=50&numericFilters=points%3E%3D300",
            ),
        ];
        for (list, min_points, request) in cases {
            assert_eq!(url(list, min_points), format!("{BASE}{request}"), "{list:?} {min_points}");
        }
    }

    #[test]
    fn a_link_story_keeps_its_link_and_its_numbers() {
        let items = parse(ALGOLIA.as_bytes()).unwrap();
        let story = &items[0];
        assert_eq!(story.title, "Rust 2.0 will not happen, say maintainers");
        assert_eq!(
            story.url,
            "https://blog.rust-example.org/2026/10/no-rust-2/?utm_source=hn&ref=front"
        );
        // The identity is the canonical address, so the same article from
        // its own blog's feed is the same story.
        assert_eq!(story.identity, "https://blog.rust-example.org/2026/10/no-rust-2");
        assert_eq!(story.base_url, story.url);
        assert_eq!(story.author, "ferris");
        assert_eq!(story.score, Some(412));
        assert_eq!(story.comments, Some(180));
        assert_eq!(story.discussion_url, "https://news.ycombinator.com/item?id=41990001");
        assert_eq!(story.published, Some(ts("2026-10-11T07:41:04Z")));
        assert_eq!(story.content, None);
        assert_eq!(story.summary, "");
        assert_eq!(story.origin, BodyOrigin::HackerNews);
    }

    #[test]
    fn an_ask_hn_is_its_own_page_and_its_text_is_its_body() {
        let items = parse(ALGOLIA.as_bytes()).unwrap();
        let ask = &items[1];
        assert_eq!(ask.title, "Ask HN: How do you keep a reading list small?");
        assert_eq!(ask.url, "https://news.ycombinator.com/item?id=41990002");
        assert_eq!(ask.url, ask.discussion_url);
        assert_eq!(ask.identity, "https://news.ycombinator.com/item?id=41990002");
        let text = "<p>I follow far too many feeds and read none of them.<p>What has worked for \
                    you? I&#x27;d rather not use a tool that nags.";
        assert_eq!(ask.content.as_deref(), Some(text));
        assert_eq!(ask.summary, text);
        assert_eq!(ask.origin, BodyOrigin::HackerNews);
        assert_eq!((ask.score, ask.comments), (Some(87), Some(64)));
    }

    #[test]
    fn a_hit_with_no_url_key_and_no_numbers_is_still_a_story() {
        let items = parse(ALGOLIA.as_bytes()).unwrap();
        let launch = &items[2];
        // Whitespace in a title is collapsed.
        assert_eq!(launch.title, "Launch HN: Tiny Reader (YC S26)");
        assert_eq!(launch.url, "https://news.ycombinator.com/item?id=41990003");
        assert_eq!((launch.score, launch.comments), (None, None));
        assert_eq!(launch.content, None);
        assert_eq!(launch.published, Some(ts("2026-10-10T22:05:51Z")));
    }

    #[test]
    fn a_comment_among_the_hits_is_not_a_story() {
        let items = parse(ALGOLIA.as_bytes()).unwrap();
        // Four hits, the fourth a comment with no title.
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| !i.title.is_empty()));
    }

    #[test]
    fn a_link_that_is_not_a_web_address_is_replaced_by_the_discussion() {
        let json = br#"{"hits":[{"objectID":"7","title":"Odd","url":"javascript:alert(1)"}]}"#;
        let items = parse(json).unwrap();
        assert_eq!(items[0].url, "https://news.ycombinator.com/item?id=7");
    }

    #[test]
    fn the_front_page_is_filtered_here_and_the_other_lists_agree() {
        let items = parse(ALGOLIA.as_bytes()).unwrap();
        let points: Vec<Option<u32>> = items.iter().map(|i| i.score).collect();
        assert_eq!(points, [Some(412), Some(87), None]);

        let kept = |list, min_points| -> Vec<&str> {
            items.iter().filter(|i| keep(list, min_points, i)).map(|i| i.title.as_str()).collect()
        };
        // No minimum keeps everything, a story with no score included.
        assert_eq!(kept(HnList::Front, 0).len(), 3);
        assert_eq!(kept(HnList::Front, 100), ["Rust 2.0 will not happen, say maintainers"]);
        assert_eq!(kept(HnList::Front, 87).len(), 2, "the threshold is inclusive");
        // Top with no minimum is "a hundred", as the request says.
        assert_eq!(kept(HnList::Top, 0), ["Rust 2.0 will not happen, say maintainers"]);
        assert_eq!(kept(HnList::Show, 0).len(), 3);
    }

    #[test]
    fn an_answer_that_is_not_json_says_so() {
        for bytes in [&b""[..], b"<html>Bad gateway</html>", br#"["not", "an", "object"]"#] {
            let error = parse(bytes).unwrap_err();
            let prefix = "Hacker News answered with something unexpected: ";
            assert!(error.starts_with(prefix), "{error}");
        }
    }

    #[test]
    fn an_answer_with_no_hits_is_an_empty_list() {
        assert!(parse(br#"{"hits": []}"#).unwrap().is_empty());
        assert!(parse(br#"{"nbHits": 0}"#).unwrap().is_empty());
    }
}
