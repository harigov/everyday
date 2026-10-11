//! The Feed's commands, end to end against a real vault.
//!
//! What is here is what only the command layer decides: what following
//! something does about a duplicate, what a bad address is told, which
//! changes a write announces, and what the Library is asked to hold. The
//! parsing of feeds, the refresher's schedule and the store's own rules have
//! their tests where they live.
//!
//! Nothing here touches the network. A web address or a Hacker News list is
//! looked up on the way in, so each is only ever added in a test that is
//! refused first -- as a duplicate, or as not an address -- and the source
//! the other tests need is saved straight into the vault.

use everyday_core::news::{HnList, NewsSource, SourceKind, Story};
use everyday_core::{NewsSourceId, StoryId, Vault};
use everyday_service::ctx::Ctx;
use everyday_service::events::{Change, EventSink, Kind};
use everyday_service::{Service, error::CommandError};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
mod support;

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

impl Collector {
    fn take(&self) -> Vec<Change> {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A web source saved straight into the vault, with no lookup and no fetch.
fn follow_web(vault: &Vault, url: &str, title: &str) -> NewsSource {
    let source = NewsSource::new(SourceKind::Web { url: url.into() }, title);
    vault.save_news_source(&source).unwrap();
    source
}

/// `count` stories under `source`, newest first, an hour apart.
fn stories(vault: &Vault, source: NewsSourceId, count: usize) -> Vec<Story> {
    let now = Timestamp::now();
    let mut out = Vec::new();
    for n in 0..count {
        let key = vault.news_key(&format!("https://blog.example/post-{n}")).unwrap();
        let published = now - SignedDuration::from_hours(n as i64 + 1);
        let mut story = Story::new(source, key, format!("Post {n}"), published);
        story.url = format!("https://blog.example/post-{n}");
        story.author = "A. Writer".into();
        story.summary = format!("What post {n} says.");
        out.push(story);
    }
    let rows: Vec<_> = out.iter().cloned().map(|s| (s, None)).collect();
    vault.insert_stories(&rows).unwrap();
    out
}

fn ids(stories: &[Story]) -> Vec<String> {
    stories.iter().map(|s| s.id.to_string()).collect()
}

// ---- following -----------------------------------------------------------

#[tokio::test]
async fn a_newsletter_is_followed_by_its_lowercased_sender_and_not_twice() {
    let (svc, _dir) = support::vault::service(None);

    let source = call(
        &svc,
        "add_news_source",
        json!({ "source": { "type": "mail", "sender": "  Letters@Example.com " } }),
    )
    .await;
    assert_eq!(source["kind"], json!({ "type": "mail", "sender": "letters@example.com" }));
    // Named after the sender when nobody gave it a name.
    assert_eq!(source["title"], "letters@example.com");

    let again = fails(
        &svc,
        "add_news_source",
        json!({ "source": { "type": "mail", "sender": "LETTERS@example.com", "title": "Again" } }),
    )
    .await;
    assert_eq!(again.code, "invalid");
    assert!(again.message.contains("already follow"), "{}", again.message);

    let listed = call(&svc, "list_news_sources", json!({})).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_newsletter_needs_a_sender_address() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(&svc, "add_news_source", json!({ "source": { "type": "mail", "sender": "no" } }))
        .await;
    assert_eq!(e.code, "invalid");
}

#[tokio::test]
async fn an_address_already_followed_is_refused_before_anything_is_fetched() {
    let (svc, _dir) = support::vault::service(None);
    follow_web(&svc.get().unwrap(), "https://example.com/feed", "Example");

    // Typed without the scheme, and with a trailing slash: the same feed.
    for typed in ["example.com/feed", "https://example.com/feed/"] {
        let e =
            fails(&svc, "add_news_source", json!({ "source": { "type": "web", "url": typed } }))
                .await;
        assert_eq!(e.code, "invalid", "{typed}");
        assert!(e.message.contains("already follow"), "{typed}: {}", e.message);
    }
}

#[tokio::test]
async fn a_hacker_news_list_is_followed_once_per_list_and_points() {
    let (svc, _dir) = support::vault::service(None);
    let kind = SourceKind::HackerNews { list: HnList::Front, min_points: 0 };
    svc.get().unwrap().save_news_source(&NewsSource::new(kind, "Hacker News")).unwrap();

    let e = fails(
        &svc,
        "add_news_source",
        json!({ "source": { "type": "hackerNews", "list": "front" } }),
    )
    .await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("already follow"), "{}", e.message);
}

#[tokio::test]
async fn something_that_is_not_a_web_address_is_told_so_in_words() {
    let (svc, _dir) = support::vault::service(None);
    for typed in ["file:///etc/passwd", "ftp://example.com/feed", "   "] {
        let e =
            fails(&svc, "add_news_source", json!({ "source": { "type": "web", "url": typed } }))
                .await;
        assert_eq!(e.code, "invalid", "{typed:?}");
        let e = fails(&svc, "discover_feeds", json!({ "url": typed })).await;
        assert_eq!(e.code, "invalid", "{typed:?}");
    }
}

#[tokio::test]
async fn saving_and_unfollowing_a_source_announce_it() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let source = follow_web(&vault, "https://example.com/feed", "Example");
    stories(&vault, source.id, 2);

    let mut renamed = serde_json::to_value(&source).unwrap();
    renamed["title"] = json!("Renamed");
    call(&svc, "save_news_source", json!({ "source": renamed })).await;
    let changes = events.take();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, Kind::NewsSource);
    assert_eq!(changes[0].id, Some(source.id.to_string()));

    call(&svc, "delete_news_source", json!({ "id": source.id })).await;
    assert_eq!(events.take().len(), 1);

    // The stories went with it, saved or not.
    let page = call(&svc, "list_stories", json!({ "query": {} })).await;
    assert!(page["stories"].as_array().unwrap().is_empty());
}

// ---- stories -------------------------------------------------------------

#[tokio::test]
async fn stories_are_marked_read_saved_and_listed_newest_first() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let source = follow_web(&vault, "https://blog.example/feed", "Blog");
    let made = stories(&vault, source.id, 3);

    let page = call(&svc, "list_stories", json!({ "query": {} })).await;
    let titles: Vec<&str> =
        page["stories"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Post 0", "Post 1", "Post 2"]);

    call(&svc, "mark_stories_read", json!({ "ids": ids(&made[..2]), "read": true })).await;
    let changes = events.take();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].kind, changes[0].ids.len()), (Kind::Story, 2));

    let unread = call(&svc, "list_stories", json!({ "query": { "unreadOnly": true } })).await;
    assert_eq!(unread["stories"].as_array().unwrap().len(), 1);
    assert_eq!(unread["stories"][0]["title"], "Post 2");

    call(&svc, "save_stories", json!({ "ids": [made[0].id], "saved": true })).await;
    let saved = call(&svc, "list_stories", json!({ "query": { "savedOnly": true } })).await;
    assert_eq!(saved["stories"].as_array().unwrap().len(), 1);
    assert_eq!(call(&svc, "get_story", json!({ "id": made[0].id })).await["saved"], true);

    // What "mark all read" changed is what it hands back, so an undo marks
    // those and not the two that were already read.
    let changed = call(
        &svc,
        "mark_all_stories_read",
        json!({ "sources": [], "upTo": Timestamp::now().to_string() }),
    )
    .await;
    assert_eq!(changed, json!([made[2].id]));

    let listed = call(&svc, "list_news_sources", json!({})).await;
    assert_eq!(listed[0]["unread"], 0);
    assert_eq!(listed[0]["total"], 3);
}

#[tokio::test]
async fn marking_stories_unread_again_brings_them_back() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let source = follow_web(&vault, "https://blog.example/feed", "Blog");
    let made = stories(&vault, source.id, 2);

    call(&svc, "mark_stories_read", json!({ "ids": ids(&made), "read": true })).await;
    call(&svc, "mark_stories_read", json!({ "ids": [made[1].id], "read": false })).await;
    let unread = call(&svc, "list_stories", json!({ "query": { "unreadOnly": true } })).await;
    assert_eq!(unread["stories"].as_array().unwrap().len(), 1);
    assert_eq!(unread["stories"][0]["id"], json!(made[1].id));
}

#[tokio::test]
async fn refreshing_a_source_that_is_not_there_is_not_found() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(&svc, "refresh_news", json!({ "source": NewsSourceId::new() })).await;
    assert_eq!(e.code, "not_found");
}

#[tokio::test]
async fn a_story_that_is_not_there_is_not_found() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(&svc, "get_story", json!({ "id": StoryId::new() })).await;
    assert_eq!(e.code, "not_found");
}

// ---- the library ---------------------------------------------------------

#[tokio::test]
async fn a_story_goes_onto_the_articles_shelf_once() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let source = follow_web(&vault, "https://blog.example/feed", "The Blog");
    let made = stories(&vault, source.id, 1);

    // The vault has no shelves at all, so the built-in ones are made first.
    let item = call(&svc, "add_story_to_library", json!({ "id": made[0].id })).await;
    let shown = call(&svc, "get_item", json!({ "id": item })).await;
    assert_eq!(shown["title"], "Post 0");
    assert_eq!(shown["creator"], "A. Writer");
    assert_eq!(shown["summary"], "What post 0 says.");
    assert_eq!(shown["status"], "wishlist");
    assert_eq!(shown["facts"]["url"], "https://blog.example/post-0");
    assert_eq!(shown["facts"]["author"], "A. Writer");
    assert_eq!(shown["facts"]["publication"], "The Blog");

    // Pressed twice, it is still one item.
    let again = call(&svc, "add_story_to_library", json!({ "id": made[0].id })).await;
    assert_eq!(again, item);
    let all = call(&svc, "list_items", json!({ "query": {} })).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_library_without_an_articles_shelf_is_not_given_one() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let source = follow_web(&vault, "https://blog.example/feed", "The Blog");
    let made = stories(&vault, source.id, 1);

    // Somebody who has shelves, and none of them is Articles, has said so.
    let books = call(&svc, "new_kind", json!({ "name": "Books", "singular": "Book" })).await;
    call(&svc, "save_kind", json!({ "kind": books })).await;

    let e = fails(&svc, "add_story_to_library", json!({ "id": made[0].id })).await;
    assert_eq!(e.code, "not_found");
    assert!(e.message.contains("Articles"), "{}", e.message);
    let kinds = call(&svc, "list_kinds", json!({})).await;
    let invented = kinds.as_array().unwrap().iter().any(|k| k["slug"] == "article");
    assert!(!invented, "no Articles shelf was invented");
}

// ---- OPML ----------------------------------------------------------------

const OPML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head><title>Subscriptions</title></head>
  <body>
    <outline text="Tech" title="Tech">
      <outline type="rss" text="Example" title="Example" xmlUrl="https://feeds.example.com/a.xml" htmlUrl="https://example.com"/>
      <outline type="rss" text="Already" title="Already" xmlUrl="https://blog.example/feed"/>
    </outline>
    <outline type="rss" text="Other" title="Other" xmlUrl="https://feeds.example.com/b.xml"/>
    <outline type="rss" text="Not a web feed" xmlUrl="mailto:someone@example.com"/>
  </body>
</opml>"#;

#[tokio::test]
async fn an_opml_import_adds_what_is_new_and_skips_what_is_not() {
    let (svc, _dir) = support::vault::service(None);
    follow_web(&svc.get().unwrap(), "https://blog.example/feed", "Blog");

    // Two are new. One is followed already, and one is not an address a feed
    // can be fetched from. The reader may drop that last one before the
    // command sees it, so the skipped count is a floor and not an equality.
    let report = call(&svc, "import_opml", json!({ "xml": OPML })).await;
    assert_eq!(report["added"], 2);
    assert!(report["skipped"].as_u64().unwrap() >= 1, "{report}");

    let listed = call(&svc, "list_news_sources", json!({})).await;
    let sources = listed.as_array().unwrap();
    assert_eq!(sources.len(), 3);
    let example = sources.iter().find(|s| s["source"]["title"] == "Example").unwrap();
    assert_eq!(example["source"]["folder"], "Tech");
    assert_eq!(example["source"]["siteUrl"], "https://example.com");
    assert_eq!(example["source"]["kind"]["url"], "https://feeds.example.com/a.xml");
    // Nothing was fetched: there is no fetch state, so it is due at once.
    assert!(example["lastOk"].is_null());

    // The same list again adds nothing.
    let again = call(&svc, "import_opml", json!({ "xml": OPML })).await;
    assert_eq!(again["added"], 0);
    assert!(again["skipped"].as_u64().unwrap() >= 3, "{again}");
    assert_eq!(call(&svc, "list_news_sources", json!({})).await.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn something_that_is_not_opml_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(&svc, "import_opml", json!({ "xml": "this is not xml" })).await;
    assert_eq!(e.code, "invalid");
}

#[tokio::test]
async fn the_export_lists_web_sources_and_leaves_out_what_no_other_reader_can_fetch() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut web = NewsSource::new(
        SourceKind::Web { url: "https://feeds.example.com/a.xml".into() },
        "Example",
    );
    web.folder = "Tech".into();
    web.site_url = "https://example.com".into();
    vault.save_news_source(&web).unwrap();
    let hn = SourceKind::HackerNews { list: HnList::Front, min_points: 0 };
    vault.save_news_source(&NewsSource::new(hn, "Hacker News")).unwrap();

    let opml = call(&svc, "export_opml", json!({})).await;
    let opml = opml.as_str().unwrap();
    assert!(opml.contains("https://feeds.example.com/a.xml"), "{opml}");
    assert!(opml.contains("Tech"), "{opml}");
    assert!(!opml.contains("Hacker News"), "{opml}");

    // What we wrote, we can read: importing it into an empty vault follows
    // the same feed in the same folder.
    let (other, _other_dir) = support::vault::service(None);
    let report = call(&other, "import_opml", json!({ "xml": opml })).await;
    assert_eq!(report, json!({ "added": 1, "skipped": 0 }));
    let listed = call(&other, "list_news_sources", json!({})).await;
    assert_eq!(listed[0]["source"]["folder"], "Tech");
    assert_eq!(listed[0]["source"]["siteUrl"], "https://example.com");
}
