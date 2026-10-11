//! The Feed's refresher, end to end, against a feed served from loopback.
//!
//! The network half (`news::fetch`), the turn (`news::refresh`) and the
//! loop that gives every source its turn, driven against a vault in a
//! temporary directory and a `TcpListener` on loopback speaking just enough
//! HTTP. The typed feed address goes over `http::client()`, which has no
//! objection to loopback; everything a feed *names* goes over
//! `http::public_client()`, which does, and that is what the last few tests
//! lean on to show that an icon or an article on a private address is
//! refused before a request leaves.
//!
//! Parsing, the schedule and discovery are other modules' business and have
//! tests of their own. What is checked here is what only this layer decides:
//! what is sent, what is stored, what is said when it goes wrong, and when
//! the next attempt is.

#[allow(dead_code)]
mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use everyday_core::Vault;
use everyday_core::id::NewsSourceId;
use everyday_core::news::schedule::Hints;
use everyday_core::news::{FetchState, NewsSource, SourceKind, Story};
use everyday_core::store::news::StoryQuery;
use everyday_service::Service;
use everyday_service::error::codes;
use everyday_service::events::{Change, EventSink, Kind, Notification, Op, Reach};
use everyday_service::news::fetch::{self, Fetched};
use everyday_service::news::{extract, refresh};
use everyday_service::supervisor::Outcome;
use jiff::Timestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

// ── A feed server on loopback ────────────────────────────────────────────

/// What the server was asked, with header names lower-cased.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: HashMap<String, String>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Reply {
    fn ok(body: impl Into<String>, content_type: &'static str) -> Self {
        let headers = vec![("Content-Type", content_type.to_string())];
        Self { status: 200, headers, body: body.into() }
    }

    fn status(status: u16) -> Self {
        Self { status, headers: Vec::new(), body: String::new() }
    }

    fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn render(&self) -> String {
        let reason = match self.status {
            200 => "OK",
            304 => "Not Modified",
            404 => "Not Found",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "Status",
        };
        let mut out = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        for (name, value) in &self.headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", self.body.len()));
        out.push_str(&self.body);
        out
    }
}

/// A server on loopback. Dropping the handle stops it.
struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    _stop: watch::Sender<bool>,
}

impl Server {
    async fn start(respond: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let respond = Arc::new(respond);
        let (stop, mut stopped) = watch::channel(false);

        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = stopped.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let (respond, log) = (respond.clone(), log.clone());
                tokio::spawn(async move {
                    let Some(request) = read_request(&mut socket).await else { return };
                    let reply = respond(&request);
                    // Logged before the reply goes out, so a test that has
                    // been answered can rely on having been counted.
                    log.lock().unwrap().push(request);
                    let _ = socket.write_all(reply.render().as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Server { port, seen, _stop: stop }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<Seen> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines.next()?.split(' ').nth(1)?.to_string();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Some(Seen { path, headers })
}

/// An RSS 2.0 feed of `(title, slug)` items, each a post on `blog.example.com`
/// with a summary and full content.
fn feed_xml(items: &[(&str, &str)]) -> String {
    let items: String = items
        .iter()
        .map(|(title, slug)| {
            format!(
                r#"<item><title>{title}</title>
                   <link>https://blog.example.com/{slug}</link>
                   <guid isPermaLink="false">{slug}</guid>
                   <pubDate>Mon, 05 Oct 2026 09:00:00 GMT</pubDate>
                   <description>Summary of {slug}</description>
                   <content:encoded><![CDATA[<p>Body of {slug}</p>]]></content:encoded></item>"#
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
           <rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
           <channel><title>Blog</title><link>https://blog.example.com/</link>
           <description>A blog</description>{items}</channel></rss>"#
    )
}

const THREE: [(&str, &str); 3] = [("One", "one"), ("Two", "two"), ("Three", "three")];

fn rss(items: &[(&str, &str)]) -> Reply {
    Reply::ok(feed_xml(items), "application/rss+xml")
}

// ── Events, and a vault to run against ───────────────────────────────────

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
    notices: Mutex<Vec<Notification>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
    fn notify(&self, notification: Notification) {
        self.notices.lock().unwrap().push(notification);
    }
}

impl Collector {
    fn count(&self, kind: Kind, op: Op) -> usize {
        self.changes.lock().unwrap().iter().filter(|c| c.kind == kind && c.op == op).count()
    }

    fn notices(&self) -> Vec<Notification> {
        self.notices.lock().unwrap().clone()
    }
}

struct Fixture {
    svc: Arc<Service>,
    vault: Arc<Vault>,
    events: Arc<Collector>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let (svc, dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.require().unwrap();
    Fixture { svc, vault, events, _dir: dir }
}

impl Fixture {
    /// A followed feed at `url`.
    fn follow(&self, title: &str, url: String) -> NewsSource {
        let source = NewsSource::new(SourceKind::Web { url }, title);
        self.vault.save_news_source(&source).unwrap();
        source
    }

    fn state(&self, source: &NewsSource) -> FetchState {
        self.vault.news_fetch_state(source.id).unwrap().expect("a refresh leaves a fetch state")
    }

    fn stories(&self) -> Vec<Story> {
        self.vault.stories(&StoryQuery { limit: 100, ..Default::default() }).unwrap().stories
    }
}

// ── A source's turn ──────────────────────────────────────────────────────

#[tokio::test]
async fn the_first_refresh_stores_stories_and_a_matching_etag_answers_not_modified() {
    let fx = fixture();
    let server = Server::start(|req| {
        if req.header("if-none-match") == Some("\"v1\"") {
            return Reply::status(304);
        }
        rss(&THREE).with("ETag", "\"v1\"").with("Cache-Control", "max-age=300")
    })
    .await;
    let source = fx.follow("Blog", server.url("/feed.xml"));

    let before = fx.svc.now();
    let first = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!(first.error, None);
    assert_eq!(first.inserted, 3);
    assert!(!first.not_modified);
    assert_eq!(fx.stories().len(), 3);

    let state = fx.state(&source);
    assert_eq!(state.etag.as_deref(), Some("\"v1\""));
    assert!(state.last_ok.is_some());
    assert_eq!((state.last_error, state.failing_since, state.failures), (None, None, 0));
    assert!(state.next_at > before, "the next attempt is in the future");
    assert!(!state.recent.is_empty(), "the publication times were remembered");

    // The second request carries the validator, and `304` stores nothing.
    let second = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!(second.error, None);
    assert_eq!(second.inserted, 0);
    assert!(second.not_modified);
    assert_eq!(fx.stories().len(), 3);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/feed.xml");
    assert_eq!(requests[0].header("if-none-match"), None);
    assert_eq!(requests[1].header("if-none-match"), Some("\"v1\""));
    assert!(fx.state(&source).etag.is_some(), "a 304 keeps the validator");
}

#[tokio::test]
async fn a_feed_that_cannot_say_not_modified_still_stores_nothing_twice() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE)).await;
    let source = fx.follow("Blog", server.url("/feed.xml"));

    assert_eq!(refresh::refresh_source(&fx.svc, &fx.vault, source.id).await.inserted, 3);
    let again = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!(again.error, None);
    assert_eq!(again.inserted, 0);
    assert!(!again.not_modified);
    assert_eq!(fx.stories().len(), 3);
}

#[tokio::test]
async fn a_story_already_read_stays_read_when_the_feed_lists_it_again() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE)).await;
    let source = fx.follow("Blog", server.url("/feed.xml"));
    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;

    let first = fx.stories().remove(0);
    fx.vault.set_stories_read(&[first.id], true).unwrap();
    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;

    let after = fx.vault.story(first.id).unwrap();
    assert!(after.read);
    assert_eq!(after.title, first.title);
}

#[tokio::test]
async fn a_story_carries_a_summary_and_a_sanitised_body() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE[..1])).await;
    let source = fx.follow("Blog", server.url("/feed.xml"));
    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;

    let story = fx.stories().remove(0);
    assert_eq!(story.title, "One");
    assert_eq!(story.url, "https://blog.example.com/one");
    assert_eq!(story.summary, "Summary of one");
    assert!(story.has_body);
    let body = fx.vault.story_body(story.id).unwrap().expect("the feed carried content");
    assert!(body.html.contains("Body of one"), "{}", body.html);
    assert!(body.text.contains("Body of one"), "{}", body.text);
}

#[tokio::test]
async fn mute_words_keep_a_story_out() {
    let fx = fixture();
    let server = Server::start(|_| {
        rss(&[("Sponsored: a great deal", "ad"), ("Real news", "real"), ("Other news", "other")])
    })
    .await;
    let mut source = NewsSource::new(SourceKind::Web { url: server.url("/feed.xml") }, "Blog");
    source.mute = vec!["SPONSORED".into()];
    fx.vault.save_news_source(&source).unwrap();

    let report = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!(report.inserted, 2);
    let mut titles: Vec<String> = fx.stories().into_iter().map(|s| s.title).collect();
    titles.sort();
    assert_eq!(titles, ["Other news", "Real news"]);
}

#[tokio::test]
async fn the_same_article_from_a_second_source_is_one_row_that_says_so() {
    let fx = fixture();
    let first = Server::start(|_| rss(&[("Shared", "shared")])).await;
    let second = Server::start(|_| rss(&[("Shared", "shared")])).await;
    let a = fx.follow("Blog", first.url("/feed.xml"));
    let b = fx.follow("Mirror", second.url("/feed.xml"));

    assert_eq!(refresh::refresh_source(&fx.svc, &fx.vault, a.id).await.inserted, 1);
    let mirrored = refresh::refresh_source(&fx.svc, &fx.vault, b.id).await;
    assert_eq!((mirrored.inserted, mirrored.error), (0, None));

    let stories = fx.stories();
    assert_eq!(stories.len(), 1);
    assert_eq!(stories[0].source_id, a.id);
    assert_eq!(stories[0].also.len(), 1);
    assert_eq!(stories[0].also[0].source_id, b.id);

    // Said once, however often it is seen.
    refresh::refresh_source(&fx.svc, &fx.vault, b.id).await;
    assert_eq!(fx.stories()[0].also.len(), 1);
}

#[tokio::test]
async fn a_paused_source_is_refreshed_when_it_is_asked_for_by_name() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE)).await;
    let mut source = NewsSource::new(SourceKind::Web { url: server.url("/feed.xml") }, "Blog");
    source.paused = true;
    fx.vault.save_news_source(&source).unwrap();

    let report = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!((report.inserted, report.error), (3, None));
    assert!(fx.vault.news_source(source.id).unwrap().paused, "refreshing does not unpause");
}

#[tokio::test]
async fn a_refresh_does_not_touch_the_source_a_person_edited() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE)).await;
    let source = fx.follow("Blog", server.url("/feed.xml"));
    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    let kept = fx.vault.news_source(source.id).unwrap();
    assert_eq!((kept.title, kept.kind, kept.paused), (source.title, source.kind, false));
}

#[tokio::test]
async fn a_source_that_is_gone_reports_it_and_leaves_no_state_behind() {
    let fx = fixture();
    let ghost = NewsSourceId::new();
    let report = refresh::refresh_source(&fx.svc, &fx.vault, ghost).await;
    assert!(report.error.is_some());
    assert_eq!(report.inserted, 0);
    assert!(fx.vault.news_fetch_state(ghost).unwrap().is_none());
}

// ── When it goes wrong ───────────────────────────────────────────────────

#[tokio::test]
async fn a_429_with_retry_after_is_an_error_that_waits_as_long_as_it_was_told_to() {
    let fx = fixture();
    let mode = Arc::new(AtomicU8::new(0));
    let server = Server::start({
        let mode = mode.clone();
        move |_| match mode.load(Ordering::SeqCst) {
            0 => Reply::status(429).with("Retry-After", "10800"),
            _ => rss(&THREE),
        }
    })
    .await;
    let source = fx.follow("Blog", server.url("/secret-token/feed.xml"));

    let before = fx.svc.now();
    let report = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    let error = report.error.expect("a 429 is a failure");
    assert!(error.contains("slow down"), "{error}");
    assert!(!error.contains("127.0.0.1") && !error.contains("secret-token"), "{error}");
    assert_eq!((report.inserted, report.not_modified), (0, false));

    let state = fx.state(&source);
    assert_eq!(state.last_error.as_deref(), Some(error.as_str()));
    assert_eq!(state.failures, 1);
    assert!(state.failing_since.is_some());
    // Three hours were asked for; the schedule may jitter that by a tenth
    // either way and may lengthen it, never shorten it past that.
    assert!(
        state.next_at.as_second() >= before.as_second() + 9_000,
        "next attempt in {} s",
        state.next_at.as_second() - before.as_second()
    );
    assert!(state.interval_minutes >= 150, "{} minutes", state.interval_minutes);

    // Healthy again: everything the failure recorded is cleared.
    mode.store(1, Ordering::SeqCst);
    let recovered = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!((recovered.inserted, recovered.error), (3, None));
    let state = fx.state(&source);
    assert_eq!((state.last_error, state.failing_since, state.failures), (None, None, 0));
    assert!(state.last_ok.is_some());
}

#[tokio::test]
async fn an_outage_raises_one_notification_however_long_it_lasts_and_the_next_raises_another() {
    let fx = fixture();
    let mode = Arc::new(AtomicU8::new(0));
    let server = Server::start({
        let mode = mode.clone();
        move |_| match mode.load(Ordering::SeqCst) {
            0 => Reply::status(500),
            _ => rss(&THREE),
        }
    })
    .await;
    let source = fx.follow("Blog", server.url("/feed.xml"));
    let key = format!("news:{}", source.id);

    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    let notices = fx.events.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Blog could not be refreshed");
    assert_eq!(notices[0].key.as_deref(), Some(key.as_str()));
    let body = notices[0].body.clone().unwrap_or_default();
    assert!(body.contains("trouble"), "{body}");
    assert!(!body.contains("127.0.0.1"), "{body}");
    // Somebody pressed Refresh: it is for the window they are looking at.
    assert!(matches!(notices[0].reach, Reach::App));

    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!(fx.events.notices().len(), 1, "the same outage says nothing twice");

    mode.store(1, Ordering::SeqCst);
    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    mode.store(0, Ordering::SeqCst);
    refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!(fx.events.notices().len(), 2, "a new outage is news again");
}

#[tokio::test]
async fn a_body_that_is_not_a_feed_fails_without_keeping_its_validators() {
    let fx = fixture();
    let server = Server::start(|_| {
        Reply::ok("<html><body>Please sign in</body></html>", "text/html").with("ETag", "\"login\"")
    })
    .await;
    let source = fx.follow("Blog", server.url("/feed.xml"));

    let report = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    let error = report.error.expect("an HTML page is not a feed");
    assert!(!error.contains("127.0.0.1"), "{error}");
    let state = fx.state(&source);
    assert_eq!(state.etag, None, "a validator for a body we could not use would hide the feed");
    assert_eq!(state.failures, 1);
    assert!(fx.stories().is_empty());
}

// ── Doing a lot at once ──────────────────────────────────────────────────

#[tokio::test]
async fn refresh_all_raises_one_story_change_for_everything_it_stored() {
    let fx = fixture();
    let one = Server::start(|_| rss(&[("A1", "a1"), ("A2", "a2")])).await;
    let two = Server::start(|_| rss(&[("B1", "b1")])).await;
    let paused_server = Server::start(|_| rss(&[("C1", "c1")])).await;
    fx.follow("One", one.url("/feed.xml"));
    fx.follow("Two", two.url("/feed.xml"));
    let url = paused_server.url("/feed.xml");
    let mut paused = NewsSource::new(SourceKind::Web { url }, "Off");
    paused.paused = true;
    fx.vault.save_news_source(&paused).unwrap();

    let reports = refresh::refresh_all(&fx.svc, &fx.vault).await;
    assert_eq!(reports.len(), 2, "a paused source is not asked");
    assert_eq!(reports.iter().map(|r| r.inserted).sum::<u32>(), 3);
    assert!(reports.iter().all(|r| r.error.is_none()));
    assert!(paused_server.requests().is_empty());
    assert_eq!(fx.stories().len(), 3);

    assert_eq!(fx.events.count(Kind::Story, Op::Created), 1);
    assert_eq!(fx.events.count(Kind::NewsSource, Op::Updated), 0, "nobody's health moved");

    // Nothing new the second time: nothing to announce.
    let again = refresh::refresh_all(&fx.svc, &fx.vault).await;
    assert_eq!(again.iter().map(|r| r.inserted).sum::<u32>(), 0);
    assert_eq!(fx.events.count(Kind::Story, Op::Created), 1);
}

#[tokio::test]
async fn refresh_all_says_so_once_when_a_sources_health_changes() {
    let fx = fixture();
    let good = Server::start(|_| rss(&THREE)).await;
    let bad = Server::start(|_| Reply::status(404)).await;
    fx.follow("Good", good.url("/feed.xml"));
    let broken = fx.follow("Broken", bad.url("/feed.xml"));

    let reports = refresh::refresh_all(&fx.svc, &fx.vault).await;
    let failed: Vec<_> = reports.iter().filter(|r| r.error.is_some()).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].source_id, broken.id);
    assert!(failed[0].error.as_deref().unwrap().contains("nothing at that address"));
    assert_eq!(fx.events.count(Kind::NewsSource, Op::Updated), 1);

    // The same failure again is not a change.
    refresh::refresh_all(&fx.svc, &fx.vault).await;
    assert_eq!(fx.events.count(Kind::NewsSource, Op::Updated), 1);
}

#[tokio::test]
async fn sources_on_one_host_take_turns_and_all_of_them_are_refreshed() {
    let fx = fixture();
    let server = Server::start(|req| match req.path.as_str() {
        "/a.xml" => rss(&[("A", "a")]),
        "/b.xml" => rss(&[("B", "b")]),
        "/c.xml" => rss(&[("C", "c")]),
        _ => Reply::status(404),
    })
    .await;
    for path in ["/a.xml", "/b.xml", "/c.xml"] {
        fx.follow(path, server.url(path));
    }
    let reports = refresh::refresh_all(&fx.svc, &fx.vault).await;
    assert_eq!(reports.len(), 3);
    assert!(reports.iter().all(|r| r.error.is_none() && r.inserted == 1), "{reports:?}");
    assert_eq!(server.requests().len(), 3);
}

// ── The loop ─────────────────────────────────────────────────────────────

/// Poll `check` until it is true, or fail after five seconds.
async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

#[tokio::test]
async fn the_loop_refreshes_what_is_due_and_stops_when_it_is_told_to() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE)).await;
    let source = fx.follow("Blog", server.url("/feed.xml"));

    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(refresh::run(fx.svc.clone(), fx.vault.clone(), stopped));

    eventually("the first refresh", || fx.events.count(Kind::Story, Op::Created) == 1).await;
    assert_eq!(fx.stories().len(), 3);
    // The pass raised one change, and left the source waiting for its time.
    assert!(fx.state(&source).next_at > fx.svc.now());

    stop.send(true).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the loop stops promptly")
        .unwrap();
    assert_eq!(outcome.unwrap(), Outcome::Done);
    assert_eq!(server.requests().len(), 1, "it was due once");
}

#[tokio::test]
async fn the_loop_does_not_ask_a_paused_source() {
    let fx = fixture();
    let server = Server::start(|_| rss(&THREE)).await;
    let mut source = NewsSource::new(SourceKind::Web { url: server.url("/feed.xml") }, "Off");
    source.paused = true;
    fx.vault.save_news_source(&source).unwrap();

    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(refresh::run(fx.svc.clone(), fx.vault.clone(), stopped));
    tokio::time::sleep(Duration::from_millis(400)).await;
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();

    assert!(server.requests().is_empty());
    assert!(fx.stories().is_empty());
}

#[tokio::test]
async fn a_failure_in_the_loop_reaches_a_person_who_may_not_be_looking() {
    let fx = fixture();
    let server = Server::start(|_| Reply::status(500)).await;
    fx.follow("Blog", server.url("/feed.xml"));

    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(refresh::run(fx.svc.clone(), fx.vault.clone(), stopped));
    eventually("the notification", || !fx.events.notices().is_empty()).await;
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();

    let notices = fx.events.notices();
    assert_eq!(notices.len(), 1);
    assert!(matches!(notices[0].reach, Reach::User));
    assert_eq!(fx.events.count(Kind::NewsSource, Op::Updated), 1);
}

#[tokio::test]
async fn a_locked_vault_is_a_reason_to_wait_and_not_to_fail() {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: Some("correct horse battery staple".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    vault.save_journal(&everyday_core::Journal::new("Test")).unwrap();
    let svc = Arc::new(Service::new());
    svc.set(vault);
    let vault = svc.require().unwrap();
    vault.lock();

    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(refresh::run(svc.clone(), vault.clone(), stopped));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "it waits rather than giving up");
    stop.send(true).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    assert_eq!(outcome.unwrap(), Outcome::Done);
}

// ── The network half, on its own ─────────────────────────────────────────

#[tokio::test]
async fn validators_and_caching_headers_are_sent_and_read() {
    let since = "Mon, 05 Oct 2026 09:00:00 GMT";
    let server = Server::start(move |req| {
        if req.header("if-none-match") == Some("\"v7\"")
            && req.header("if-modified-since") == Some(since)
        {
            return Reply::status(304).with("Cache-Control", "max-age=900");
        }
        Reply::ok("<rss/>", "application/rss+xml")
            .with("ETag", "\"v7\"")
            .with("Last-Modified", since)
            .with("Cache-Control", "public, max-age=600")
    })
    .await;
    let url = server.url("/feed.xml");
    let mut state = FetchState::new(NewsSourceId::new());

    let Fetched::Body { bytes, etag, last_modified, final_url, hints } =
        fetch::feed(&url, &state).await.unwrap()
    else {
        panic!("the first answer is a body")
    };
    assert_eq!(bytes, b"<rss/>");
    assert_eq!(etag.as_deref(), Some("\"v7\""));
    assert_eq!(last_modified.as_deref(), Some(since));
    assert_eq!(final_url, url);
    assert_eq!(hints.max_age_secs, Some(600));
    assert_eq!(hints.retry_after_secs, None);

    state.etag = etag;
    state.last_modified = last_modified;
    match fetch::feed(&url, &state).await.unwrap() {
        Fetched::NotModified { hints } => assert_eq!(hints.max_age_secs, Some(900)),
        other => panic!("expected 304, got {other:?}"),
    }
    let accept = server.requests()[0].header("accept").unwrap_or_default().to_string();
    assert!(accept.contains("application/atom+xml") && accept.contains("application/rss+xml"));
}

#[tokio::test]
async fn a_failure_carries_retry_after_as_seconds_or_as_a_date() {
    let server = Server::start(|req| match req.path.as_str() {
        "/seconds" => Reply::status(503).with("Retry-After", "120"),
        "/date" => Reply::status(429).with("Retry-After", "Sun, 11 Oct 2099 12:00:00 GMT"),
        _ => Reply::status(500).with("Retry-After", "5"),
    })
    .await;
    let state = FetchState::new(NewsSourceId::new());

    let err = fetch::feed(&server.url("/seconds"), &state).await.unwrap_err();
    assert_eq!(err.hints.retry_after_secs, Some(120));
    assert!(err.message.contains("trouble") && err.message.contains("503"), "{}", err.message);

    let err = fetch::feed(&server.url("/date"), &state).await.unwrap_err();
    assert!(err.hints.retry_after_secs.unwrap() > 86_400 * 365, "{:?}", err.hints);

    // Only a 429 or a 503 is asking to wait.
    let err = fetch::feed(&server.url("/other"), &state).await.unwrap_err();
    assert_eq!(err.hints, Hints::default());
}

#[tokio::test]
async fn a_fetch_error_is_a_sentence_and_never_the_address() {
    let server = Server::start(|_| Reply::status(404)).await;
    let state = FetchState::new(NewsSourceId::new());
    let err = fetch::feed(&server.url("/secret-token/feed.xml"), &state).await.unwrap_err();
    assert!(err.message.contains("nothing at that address"), "{}", err.message);
    assert!(!err.message.contains("127.0.0.1") && !err.message.contains("secret-token"));

    // Nothing is listening where a server used to be.
    let port = {
        let gone = TcpListener::bind("127.0.0.1:0").await.unwrap();
        gone.local_addr().unwrap().port()
    };
    let err =
        fetch::feed(&format!("http://127.0.0.1:{port}/secret-token"), &state).await.unwrap_err();
    assert!(err.message.contains("could not reach"), "{}", err.message);
    assert!(!err.message.contains("127.0.0.1") && !err.message.contains("secret-token"));
}

#[tokio::test]
async fn an_address_that_is_not_a_web_address_is_refused_before_anything_is_sent() {
    let state = FetchState::new(NewsSourceId::new());
    let err = fetch::feed("file:///etc/passwd", &state).await.unwrap_err();
    assert!(!err.message.contains("passwd"), "{}", err.message);
    assert!(fetch::json("ftp://example.com/x").await.is_err());
}

#[tokio::test]
async fn a_feed_over_sixteen_megabytes_and_an_answer_over_two_are_refused() {
    let server = Server::start(|req| {
        let size = if req.path == "/feed" { 17 } else { 3 };
        Reply::ok("x".repeat(size * 1024 * 1024), "application/octet-stream")
    })
    .await;
    let state = FetchState::new(NewsSourceId::new());
    let err = fetch::feed(&server.url("/feed"), &state).await.unwrap_err();
    assert!(err.message.contains("16 MB"), "{}", err.message);
    let err = fetch::json(&server.url("/json")).await.unwrap_err();
    assert!(err.message.contains("2 MB"), "{}", err.message);
}

#[tokio::test]
async fn json_comes_back_as_it_was_sent() {
    let server = Server::start(|_| Reply::ok(r#"{"hits":[]}"#, "application/json")).await;
    assert_eq!(fetch::json(&server.url("/api")).await.unwrap(), br#"{"hits":[]}"#);
    let accept = server.requests()[0].header("accept").unwrap_or_default().to_string();
    assert!(accept.contains("application/json"));
}

// ── Finding a feed ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_pasted_feed_address_is_the_feed() {
    let server = Server::start(|_| rss(&THREE)).await;
    let found = fetch::discover(&server.url("/feed.xml")).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].url, server.url("/feed.xml"));
    assert_eq!(found[0].title, "Blog");
}

#[tokio::test]
async fn a_pasted_page_gives_the_feed_it_advertises() {
    let server = Server::start(|req| match req.path.as_str() {
        "/" => Reply::ok(
            r#"<html><head><title>Blog</title>
               <link rel="alternate" type="application/rss+xml" title="Blog posts"
                     href="/posts.xml">
               </head><body>hello</body></html>"#,
            "text/html",
        ),
        _ => Reply::status(404),
    })
    .await;
    let found = fetch::discover(&server.url("/")).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].url, server.url("/posts.xml"));
    assert_eq!(found[0].title, "Blog posts");
}

#[tokio::test]
async fn a_page_that_advertises_nothing_is_tried_at_the_usual_paths() {
    let server = Server::start(|req| match req.path.as_str() {
        "/" => Reply::ok("<html><body>no feed link</body></html>", "text/html"),
        "/feed" | "/atom.xml" => rss(&THREE),
        _ => Reply::status(404),
    })
    .await;
    let found = fetch::discover(&server.url("/")).await.unwrap();
    let urls: Vec<String> = found.into_iter().map(|f| f.url).collect();
    assert_eq!(urls, vec![server.url("/feed"), server.url("/atom.xml")]);
}

#[tokio::test]
async fn a_page_with_no_feed_anywhere_finds_none() {
    let server = Server::start(|req| match req.path.as_str() {
        "/" => Reply::ok("<html><body>no feed link</body></html>", "text/html"),
        _ => Reply::status(404),
    })
    .await;
    assert!(fetch::discover(&server.url("/")).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_page_that_cannot_be_reached_says_why() {
    let server = Server::start(|_| Reply::status(404)).await;
    let message = fetch::discover(&server.url("/gone")).await.unwrap_err();
    assert!(message.contains("nothing at that address"), "{message}");
    assert!(!message.contains("127.0.0.1"), "{message}");
}

// ── What a feed names is held to a higher standard ───────────────────────

#[tokio::test]
async fn an_icon_on_a_private_address_is_refused_before_a_request_leaves() {
    let fx = fixture();
    let server = Server::start(|_| Reply::ok("not a picture", "image/png")).await;
    assert_eq!(fetch::site_icon(&fx.vault, &server.url("/")).await, None);
    assert_eq!(fetch::site_icon(&fx.vault, "file:///etc/passwd").await, None);
    assert_eq!(fetch::site_icon(&fx.vault, "").await, None);
    assert!(server.requests().is_empty(), "{:?}", server.requests());
}

#[tokio::test]
async fn the_full_article_of_a_story_on_a_private_address_is_refused() {
    let fx = fixture();
    let server = Server::start(|_| Reply::ok("<html><body>hi</body></html>", "text/html")).await;
    let source = fx.follow("Blog", server.url("/feed.xml"));

    let key = fx.vault.news_key("loopback").unwrap();
    let mut story = Story::new(source.id, key, "Loopback", Timestamp::now());
    story.url = server.url("/article");
    let key = fx.vault.news_key("nowhere").unwrap();
    let nowhere = Story::new(source.id, key, "Nowhere", Timestamp::now());
    fx.vault.insert_stories(&[(story.clone(), None), (nowhere.clone(), None)]).unwrap();

    let err = extract::full_article(&fx.vault, story.id).await.unwrap_err();
    assert_eq!(err.code, codes::FORBIDDEN, "{err}");
    assert!(server.requests().is_empty());

    let err = extract::full_article(&fx.vault, nowhere.id).await.unwrap_err();
    assert_eq!(err.code, codes::INVALID, "{err}");
}
