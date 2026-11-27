//! A podcast's feed through the refresher, end to end, against a feed served
//! from loopback: the episodes it stores, and what the refresher learns about
//! the source from them. See `docs/plans/podcasts.md`.
//!
//! The same shape as `news_refresh.rs`, which has the full account of how a
//! turn goes; what is checked here is only what is new for a show -- that an
//! episode's file is kept on its story, that a feed of episodes makes its
//! source a podcast once and never again, and that a source's own settings
//! are left alone.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use jatayu_core::Vault;
use jatayu_core::news::{
    DownloadPolicy, NewsSource, PodcastSettings, SourceKind, Story, TranscribePolicy,
};
use jatayu_core::store::news::StoryQuery;
use jatayu_service::Service;
use jatayu_service::events::{Change, EventSink, Kind, Notification, Op};
use jatayu_service::news::refresh;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

// ── A feed server on loopback ────────────────────────────────────────────

/// Serves a body as an RSS feed to everything it is asked, and another one
/// once it has been told to. Dropping the handle stops it.
struct Server {
    port: u16,
    body: Arc<Mutex<String>>,
    _stop: watch::Sender<bool>,
}

impl Server {
    async fn start(body: String) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, mut stopped) = watch::channel(false);
        let body = Arc::new(Mutex::new(body));
        let served = body.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = stopped.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let body = served.lock().unwrap().clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 2048];
                    let _ = socket.read(&mut buffer).await;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Server { port, body, _stop: stop }
    }

    fn serve(&self, body: String) {
        *self.body.lock().unwrap() = body;
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/feed.xml", self.port)
    }
}

/// A show with two episodes. The enclosures are on a host nobody can reach:
/// nothing here downloads them.
const SHOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <rss version="2.0" xmlns:itunes="http://www.itunes.com/DTDs/Podcast-1.0.dtd">
    <channel>
      <title>Build Times</title>
      <link>https://build.example/</link>
      <itunes:author>Rhea &amp; Amit</itunes:author>
      <itunes:image href="http://127.0.0.1:9/art.jpg"/>
      <item><title>Two</title><link>https://build.example/2</link>
        <guid isPermaLink="false">build-2</guid>
        <pubDate>Fri, 02 Oct 2026 06:00:00 GMT</pubDate>
        <enclosure url="https://cdn.example/2.mp3" length="2000" type="audio/mpeg"/>
        <itunes:duration>45:12</itunes:duration><itunes:episode>2</itunes:episode></item>
      <item><title>One</title><link>https://build.example/1</link>
        <guid isPermaLink="false">build-1</guid>
        <pubDate>Fri, 25 Sep 2026 06:00:00 GMT</pubDate>
        <enclosure url="https://cdn.example/1.mp3" length="1000" type="audio/mpeg"/>
        <itunes:duration>1:02:03</itunes:duration><itunes:episode>1</itunes:episode></item>
    </channel></rss>"#;

/// A blog that once attached a recording to a post.
const BLOG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <rss version="2.0"><channel><title>Blog</title><link>https://blog.example/</link>
      <item><title>A post</title><link>https://blog.example/a</link></item>
      <item><title>Another</title><link>https://blog.example/b</link></item>
      <item><title>A talk</title><link>https://blog.example/c</link>
        <enclosure url="https://blog.example/c.mp3" length="9" type="audio/mpeg"/></item>
    </channel></rss>"#;

// ── Events, and a vault to run against ───────────────────────────────────

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
    fn notify(&self, _: Notification) {}
}

impl Collector {
    fn count(&self, kind: Kind, op: Op) -> usize {
        self.changes.lock().unwrap().iter().filter(|c| c.kind == kind && c.op == op).count()
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
    fn follow(&self, title: &str, url: String) -> NewsSource {
        let source = NewsSource::new(SourceKind::Web { url }, title);
        self.vault.save_news_source(&source).unwrap();
        source
    }

    fn stories(&self) -> Vec<Story> {
        self.vault.stories(&StoryQuery { limit: 100, ..Default::default() }).unwrap().stories
    }
}

// ── What a refresh makes of a show ───────────────────────────────────────

#[tokio::test]
async fn a_feed_of_episodes_makes_its_source_a_podcast_and_its_stories_carry_their_files() {
    let fx = fixture();
    let server = Server::start(SHOW.to_string()).await;
    let source = fx.follow("Mine", server.url());
    assert!(source.podcast.is_none());

    let reports = refresh::refresh_all(&fx.svc, &fx.vault).await;
    assert_eq!(reports.len(), 1);
    assert_eq!((reports[0].error.as_deref(), reports[0].inserted), (None, 2));

    // Every episode kept the file it names, and what the feed said of it.
    let stories = fx.stories();
    let two = stories.iter().find(|s| s.title == "Two").expect("an episode");
    let media = two.media.as_ref().expect("its file");
    assert_eq!(media.url, "https://cdn.example/2.mp3");
    assert_eq!((media.mime.as_str(), media.length), ("audio/mpeg", Some(2000)));
    // Minutes and seconds, which is how a show writes it.
    assert_eq!(media.duration_ms, Some((45 * 60 + 12) * 1000));
    assert_eq!(media.episode, Some(2));
    let one = stories.iter().find(|s| s.title == "One").expect("an episode");
    assert_eq!(one.media.as_ref().and_then(|m| m.duration_ms), Some(3_723_000));

    // The source is a show from then on, under the name the person gave it,
    // and took the show's own site and author.
    let kept = fx.vault.news_source(source.id).unwrap();
    assert_eq!(kept.title, "Mine");
    assert_eq!(kept.site_url, "https://build.example/");
    let show = kept.podcast.expect("a podcast");
    assert_eq!(show.author, "Rhea & Amit");
    assert_eq!((show.download, show.keep, show.queue_new), (None, None, None));
    // Its picture is on a machine of the person's own, which the show's
    // artwork is not fetched from.
    assert!(kept.icon.is_none());
    // The sidebar is told, once.
    assert_eq!(fx.events.count(Kind::NewsSource, Op::Updated), 1);
}

#[tokio::test]
async fn a_show_that_has_settings_keeps_them_and_is_not_announced_again() {
    let fx = fixture();
    let server = Server::start(SHOW.to_string()).await;
    let mut mine = NewsSource::new(SourceKind::Web { url: server.url() }, "Mine");
    mine.site_url = "https://my.example/".into();
    mine.podcast = Some(PodcastSettings {
        author: "Me".into(),
        download: Some(DownloadPolicy::Off),
        transcribe: Some(TranscribePolicy::Off),
        ..PodcastSettings::default()
    });
    fx.vault.save_news_source(&mine).unwrap();

    refresh::refresh_all(&fx.svc, &fx.vault).await;
    let kept = fx.vault.news_source(mine.id).unwrap();
    assert_eq!(kept.podcast, mine.podcast);
    assert_eq!(kept.site_url, "https://my.example/");
    assert_eq!(fx.stories().len(), 2);
    assert_eq!(fx.events.count(Kind::NewsSource, Op::Updated), 0);
}

#[tokio::test]
async fn a_blog_with_one_recording_is_not_made_a_podcast() {
    let fx = fixture();
    let server = Server::start(BLOG.to_string()).await;
    let source = fx.follow("Blog", server.url());

    let report = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!((report.error, report.inserted), (None, 3));
    assert!(fx.vault.news_source(source.id).unwrap().podcast.is_none());
    // The recording is still the talk's, to be played from its story.
    let stories = fx.stories();
    let with_files: Vec<&str> =
        stories.iter().filter(|s| s.media.is_some()).map(|s| s.title.as_str()).collect();
    assert_eq!(with_files, ["A talk"]);
}

#[tokio::test]
async fn episodes_stored_before_the_feed_was_known_to_be_a_show_are_given_their_files() {
    let fx = fixture();
    // The feed as it was before it had anything to play.
    let without_files: String =
        SHOW.lines().filter(|line| !line.contains("<enclosure")).collect::<Vec<_>>().join("\n");
    let server = Server::start(without_files).await;
    let source = fx.follow("Mine", server.url());

    let first = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!((first.error, first.inserted), (None, 2));
    assert!(fx.stories().iter().all(|s| s.media.is_none()));
    assert!(fx.vault.news_source(source.id).unwrap().podcast.is_none());

    // Now it has. Nothing is new, and the episodes are the same stories.
    server.serve(SHOW.to_string());
    let before: Vec<_> = fx.stories().iter().map(|s| s.id).collect();
    let second = refresh::refresh_source(&fx.svc, &fx.vault, source.id).await;
    assert_eq!((second.error, second.inserted), (None, 0));
    let stories = fx.stories();
    assert_eq!(stories.iter().map(|s| s.id).collect::<Vec<_>>(), before);
    assert!(stories.iter().all(|s| s.media.is_some()), "{stories:?}");
    assert!(fx.vault.news_source(source.id).unwrap().podcast.is_some());
}
