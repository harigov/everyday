//! The Feed, out of a vault and back into another.
//!
//! Its own file rather than another corner of `roundtrip.rs`: that test is
//! one joint claim over everything a person writes, and the Feed is the one
//! app whose export is deliberately not everything -- stories that were not
//! saved stay behind -- so what comes back is checked against what was meant
//! to, not against what was there.

use everyday_core::news::{HnList, NewsSource, Retention, SourceKind, Story};
use everyday_core::store::news::StoryQuery;
use everyday_core::{Vault, VaultConfig};
use everyday_transfer::{Mode, Options, zip};
use std::collections::BTreeMap;

fn vault(dir: &std::path::Path) -> Vault {
    everyday_vault::create(
        dir,
        VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
            forget_key_seconds: 0,
        },
    )
    .unwrap()
}

fn only_the_feed() -> Options {
    Options { parts: vec!["feed".into()], media: false }
}

fn feed() -> Vec<String> {
    vec!["feed".to_string()]
}

fn text<'a>(archive: &'a zip::Reader, name: &str) -> &'a str {
    std::str::from_utf8(archive.get(name).unwrap_or_else(|| panic!("no {name} in the archive")))
        .unwrap()
}

/// A story from `source`, published at `when`, stored.
fn arrive(vault: &Vault, source: &NewsSource, title: &str, when: &str) -> Story {
    let key = vault.news_key(&format!("{}/{title}", source.id)).unwrap();
    let mut story = Story::new(source.id, key, title, when.parse().unwrap());
    story.url = format!("https://example.com/{}", title.replace(' ', "-").to_lowercase());
    story.summary = format!("{title} is about something.");
    vault.insert_stories(&[(story.clone(), None)]).unwrap();
    story
}

/// A web feed with everything switched to something other than its default,
/// a Hacker News list, and a newsletter that asked to be kept out of the
/// inbox; and two saved stories among three.
fn follow_things(vault: &Vault) -> (NewsSource, NewsSource, NewsSource) {
    let mut blog =
        NewsSource::new(SourceKind::Web { url: "https://blog.example/feed".into() }, "Blog");
    blog.folder = "Tech".into();
    blog.site_url = "https://blog.example".into();
    blog.images = false;
    blog.full_text = true;
    blog.mute = vec!["crypto".into()];
    blog.retention = Some(Retention { read_days: 7, unread_days: 14 });
    blog.sort_order = 2;
    blog.icon = Some(vault.put_blob(b"an icon, which means nothing in another vault").unwrap());

    let hn = NewsSource::new(
        SourceKind::HackerNews { list: HnList::Top, min_points: 300 },
        "Best of Hacker News",
    );
    let mut weekly =
        NewsSource::new(SourceKind::Mail { sender: "news@example.com".into() }, "Example Weekly");
    weekly.keep_out_of_inbox = true;
    for source in [&blog, &hn, &weekly] {
        vault.save_news_source(source).unwrap();
    }

    let why = arrive(vault, &blog, "Why Rust", "2026-09-06T12:00:00Z");
    arrive(vault, &blog, "Weather report", "2026-09-06T13:00:00Z");
    let mut thing = Story::new(
        hn.id,
        vault.news_key("show-hn").unwrap(),
        "Show HN: a [thing]",
        "2026-09-07T10:00:00Z".parse().unwrap(),
    );
    thing.url = "https://thing.example/".into();
    thing.summary = "A thing\n\nwith two paragraphs.".into();
    thing.score = Some(412);
    thing.discussion_url = "https://news.example/item?id=1".into();
    vault.insert_stories(&[(thing.clone(), None)]).unwrap();
    vault.set_stories_saved(&[why.id, thing.id], true).unwrap();
    (blog, hn, weekly)
}

#[test]
fn what_you_follow_comes_back_and_what_you_saved_is_written_out() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let (blog, _hn, _weekly) = follow_things(&source);

    let (bytes, manifest) =
        everyday_transfer::export(&source, &only_the_feed(), Vec::new()).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    assert_eq!(manifest.parts[0].id, "feed");
    let archive = zip::Reader::open(&bytes).unwrap();
    for name in ["feed/subscriptions.opml", "feed/sources.json", "feed/saved.md"] {
        assert!(archive.contains(name), "{name} is missing");
    }

    // For other readers: the web feed, in its folder, and nothing they
    // could not fetch.
    let opml = text(&archive, "feed/subscriptions.opml");
    assert!(opml.contains("https://blog.example/feed"), "{opml}");
    assert!(opml.contains("Tech"), "{opml}");
    assert!(!opml.contains("news@example.com"), "a newsletter is not a feed: {opml}");
    assert!(!opml.contains("Best of Hacker News"), "nor is a Hacker News list: {opml}");

    // For this one: every source, and no pointer into the vault's blob store.
    let sidecar = text(&archive, "feed/sources.json");
    let parsed: serde_json::Value = serde_json::from_str(sidecar).unwrap();
    assert_eq!(parsed["sources"].as_array().unwrap().len(), 3);
    assert!(!sidecar.contains("icon"), "an icon is a blob id, which means nothing here: {sidecar}");

    // The saved stories, newest first; the one that was not saved stays out.
    let saved = text(&archive, "feed/saved.md");
    let first = "[Why Rust](https://example.com/why-rust) — Blog, 2026-09-06";
    assert!(saved.contains(first), "{saved}");
    assert!(saved.contains("\n  Why Rust is about something.\n"), "{saved}");
    assert!(saved.contains("[Show HN: a \\[thing\\]](https://thing.example/)"), "{saved}");
    assert!(saved.contains("([discussion](https://news.example/item?id=1))"), "{saved}");
    assert!(saved.contains("\n  A thing with two paragraphs.\n"), "{saved}");
    assert!(!saved.contains("Weather report"), "{saved}");
    assert!(saved.find("Show HN").unwrap() < saved.find("Why Rust").unwrap(), "{saved}");

    // Into a vault that has never heard of any of it.
    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let reports = everyday_transfer::import(&target, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!((report.added, report.skipped, report.replaced), (3, 0, 0), "{report:?}");
    assert_eq!(report.problems.len(), 1, "only the newsletter's switch is mentioned: {report:?}");
    assert!(report.problems[0].contains("Example Weekly"), "{report:?}");

    // Every source, whole, under the id it had -- except what must not travel.
    let mut want = source.news_sources().unwrap();
    for s in &mut want {
        s.icon = None;
        s.keep_out_of_inbox = false;
    }
    assert_eq!(target.news_sources().unwrap(), want);
    let back = target.news_source(blog.id).unwrap();
    assert_eq!(back.folder, "Tech");
    assert!(!back.images && back.full_text);
    assert_eq!(back.mute, ["crypto"]);
    assert_eq!(back.retention, Some(Retention { read_days: 7, unread_days: 14 }));

    // No story came with them: news is not writing you walk away with.
    let page = target.stories(&StoryQuery { unread_only: false, ..Default::default() }).unwrap();
    assert!(page.stories.is_empty(), "{:?}", page.stories);

    // And a second import adds nothing.
    let again = everyday_transfer::import(&target, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!((again[0].added, again[0].skipped, again[0].replaced), (0, 3, 0), "{again:?}");
    assert_eq!(target.news_sources().unwrap().len(), 3);
}

#[test]
fn replace_puts_the_file_over_a_source_that_was_renamed_since() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let (blog, _, _) = follow_things(&source);
    let (bytes, _) = everyday_transfer::export(&source, &only_the_feed(), Vec::new()).unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    let mut renamed = source.news_source(blog.id).unwrap();
    renamed.title = "Renamed".into();
    source.save_news_source(&renamed).unwrap();

    let kept = everyday_transfer::import(&source, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!(kept[0].skipped, 3, "{kept:?}");
    assert_eq!(source.news_source(blog.id).unwrap().title, "Renamed");

    let replaced = everyday_transfer::import(&source, &archive, &feed(), Mode::Replace).unwrap();
    assert_eq!(replaced[0].replaced, 3, "{replaced:?}");
    assert_eq!(source.news_source(blog.id).unwrap().title, "Blog");
    assert_eq!(source.news_sources().unwrap().len(), 3, "replacing made no second copy");
}

#[test]
fn a_feed_with_nothing_followed_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let (bytes, manifest) =
        everyday_transfer::export(&vault, &only_the_feed(), Vec::new()).unwrap();
    assert!(manifest.parts.is_empty(), "{:?}", manifest.parts);
    let archive = zip::Reader::open(&bytes).unwrap();
    assert!(archive.names().all(|n| !n.starts_with("feed/")));
}

#[test]
fn a_feed_already_followed_under_another_id_is_not_followed_twice() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let (blog, _, _) = follow_things(&source);
    let (bytes, _) = everyday_transfer::export(&source, &only_the_feed(), Vec::new()).unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    // The same feed, typed in by hand here before the archive arrived.
    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let mine =
        NewsSource::new(SourceKind::Web { url: "https://BLOG.example/feed/".into() }, "My blog");
    target.save_news_source(&mine).unwrap();

    let reports = everyday_transfer::import(&target, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!((reports[0].added, reports[0].skipped), (2, 1), "{reports:?}");
    assert_eq!(target.news_sources().unwrap().len(), 3);
    assert!(target.news_source(blog.id).is_err(), "the duplicate was not added");
    assert_eq!(target.news_source(mine.id).unwrap().title, "My blog", "and mine was not touched");
}

#[test]
fn a_source_that_cannot_be_followed_is_named_and_the_rest_still_land() {
    let good = NewsSource::new(SourceKind::Web { url: "https://good.example/feed".into() }, "Good");
    let bad = NewsSource::new(SourceKind::Web { url: "ftp://bad.example/feed".into() }, "Bad");
    let sidecar = serde_json::json!({
        "sources": [
            serde_json::to_value(&good).unwrap(),
            serde_json::to_value(&bad).unwrap(),
            { "id": 5 },
        ]
    });
    let mut files = BTreeMap::new();
    files.insert("feed/sources.json".to_string(), sidecar.to_string().into_bytes());
    let archive = zip::Reader::from_files(files);

    let dir = tempfile::tempdir().unwrap();
    let target = vault(dir.path());
    let reports = everyday_transfer::import(&target, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!(reports[0].added, 1, "{reports:?}");
    let problems = &reports[0].problems;
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("Bad") && p.contains("https://")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("source 3 could not be read")), "{problems:?}");
    let titles: Vec<String> = target.news_sources().unwrap().into_iter().map(|s| s.title).collect();
    assert_eq!(titles, ["Good"]);
}

#[test]
fn an_opml_file_alone_says_why_it_was_not_read_and_a_broken_sidecar_costs_only_itself() {
    let dir = tempfile::tempdir().unwrap();
    let target = vault(dir.path());

    let mut files = BTreeMap::new();
    files.insert("feed/subscriptions.opml".to_string(), b"<opml version=\"2.0\"/>".to_vec());
    let archive = zip::Reader::from_files(files);
    let reports = everyday_transfer::import(&target, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!(reports[0].added, 0);
    assert_eq!(reports[0].problems.len(), 1, "{reports:?}");
    assert!(reports[0].problems[0].contains("sources.json"), "{reports:?}");

    let mut files = BTreeMap::new();
    files.insert("feed/sources.json".to_string(), b"{ not json".to_vec());
    let archive = zip::Reader::from_files(files);
    let reports = everyday_transfer::import(&target, &archive, &feed(), Mode::Skip).unwrap();
    assert_eq!(reports[0].added, 0);
    assert_eq!(reports[0].problems.len(), 1, "{reports:?}");
    assert!(target.news_sources().unwrap().is_empty());
}
