//! The Feed's memory of what was read: an article, once read or heard, kept
//! as Markdown under `Feed/Articles/<source>/` where the document index finds
//! it, linked as the story's text and the consumption's words. See
//! `docs/plans/podcasts.md` (phase 2) and `FeedSettings::keep_read`.
//!
//! # Why a file, and why only an article
//!
//! A story expires -- the Feed would otherwise be every page ever published
//! -- but what somebody read must not. The kept article is an ordinary file in
//! the files folder, so whatever Files does for a document it does for this:
//! the index searches it, the reader opens it, a backup carries it, and
//! `recall` finds what it says long after the story is a tombstone. The story
//! links to it ([`karmayoga_core::news::StoryFiles::text`]) and so does the
//! record of having read it ([`karmayoga_core::news::Consumption::text_file`]).
//!
//! Two kinds of story are left alone. An episode's words are its transcript,
//! which is kept for its own reasons (see `super::transcribe`). A newsletter
//! issue is mail the mail engine has already stored and searches, and a second
//! copy in the files folder would only make the issue turn up twice. What is
//! left is what was fetched from the web: a post, a page, a feed's entry.
//!
//! # Only once
//!
//! Finishing an article can be reported more than once -- a button pressed
//! again, two windows -- so keeping it must not write it twice. It is not kept
//! again while its link names a file the catalog still has, in the trash
//! included: putting an article in the trash is the person saying they do not
//! want it kept, and reading it a second time must not undo that. A copy that
//! has gone from the catalog altogether is written again. Two keeps for one
//! story at once would both see "not kept yet", so the second of them steps
//! aside for the first ([`Keeping`]).

use super::announce_one;
use super::downloads::safe_name;
use crate::error::CommandResult;
use crate::events::{Kind, Op};
use crate::files;
use crate::service::{Service, blocking};
use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::files::{FileOrigin, ImportOptions};
use karmayoga_core::id::{ConsumptionId, FileId, StoryId};
use karmayoga_core::news::podcast::ARTICLES_DIR;
use karmayoga_core::news::{ContentRef, Enclosure, Story, StoryBody};
use karmayoga_core::{Error, Vault};
use std::sync::{Arc, Mutex};

/// How long a source's folder name and an article's file name may be, in
/// bytes. The file system's own limit is 255 for the whole name; this leaves
/// room for the date, the extension and a ` (2)` after a clash.
const SOURCE_BYTES: usize = 80;
const TITLE_BYTES: usize = 120;

/// The most of a story's body that is kept, in bytes of Markdown. An article
/// is tens of kilobytes; this stops a feed that carries a whole book in one
/// entry from filling the files folder with it.
const MAX_BYTES: usize = 2 * 1024 * 1024;

/// Whether `story` is an article for keeping: it carries nothing to play, and
/// is not a newsletter issue. See the module docs.
pub fn is_article(story: &Story) -> bool {
    story.mail_message.is_none() && !story.media.as_ref().is_some_and(Enclosure::is_playable)
}

/// Keep `story`'s article as Markdown, unless it is kept already or is an
/// episode (whose transcript is its words) or a newsletter issue. Answers the
/// file, if one was written or was there.
///
/// Does nothing, and says `None`, with [`karmayoga_core::news::FeedSettings::keep_read`]
/// off: it is the setting for keeping what is read, and the person who turned
/// it off has said they do not want the memory.
pub async fn keep_article(svc: &Arc<Service>, story: StoryId) -> CommandResult<Option<FileId>> {
    let vault = svc.require()?;
    let Some(_keeping) = Keeping::start(story) else { return Ok(None) };
    let now = svc.now();
    let kept = blocking({
        let vault = vault.clone();
        move || keep(&vault, story, now)
    })
    .await?;
    let Some(kept) = kept else { return Ok(None) };

    if kept.written {
        announce_one(svc, Kind::File, Op::Created, kept.file);
        announce_one(svc, Kind::Story, Op::Updated, story);
        files::probe_later(svc, &vault, vec![kept.file]);
    }
    if let Some(record) = kept.linked {
        announce_one(svc, Kind::Consumption, Op::Updated, record);
    }
    Ok(Some(kept.file))
}

/// What [`keep`] did.
struct Kept {
    file: FileId,
    /// The file was written now, not found.
    written: bool,
    /// The record of having read it, if it was given the file as its words
    /// just now.
    linked: Option<ConsumptionId>,
}

/// The reads and the writes of one keep. Runs on a blocking thread.
fn keep(vault: &Vault, id: StoryId, now: Timestamp) -> CommandResult<Option<Kept>> {
    if !vault.feed_settings()?.keep_read {
        return Ok(None);
    }
    let story = vault.story(id)?;
    if story.expired || !is_article(&story) {
        return Ok(None);
    }
    let consumption = vault.consumption(&ContentRef::Story(id))?;

    // Kept already: its link names a file the catalog still has.
    let present = match story.files.text {
        Some(file) => match vault.file(file) {
            Ok(_) => Some(file),
            Err(Error::NotFound { .. }) => None,
            Err(e) => return Err(e.into()),
        },
        None => None,
    };
    let (file, written) = match present {
        Some(file) => (file, false),
        None => {
            let source = vault.news_source(story.source_id).map(|s| s.title).unwrap_or_default();
            let body = vault.story_body(id)?;
            let tz = karmayoga_core::model::system_tz();
            let published = karmayoga_core::model::local_date_in(story.published_at, &tz);
            let read = consumption.as_ref().map_or(now, |c| c.finished_at.unwrap_or(c.last_at));
            let read = karmayoga_core::model::local_date_in(read, &tz);
            let markdown = render(&story, &source, body.as_ref(), published, read);
            let name = article_name(published, &story.title);
            let options = ImportOptions {
                dir: Some(source_dir(&source)),
                name: Some(name.clone()),
                origin: FileOrigin::Feed,
                encrypted: None,
            };
            let file = vault.import_bytes(markdown.as_bytes(), &name, &options)?;
            if let Err(e) = vault.update_story_files(id, |files| files.text = Some(file.id)) {
                // The story expired or its source was deleted while this was
                // written: a file nothing refers to would sit in the folder
                // for ever, since only a story's own policies clear one out.
                let _ = vault.trash_files(&[file.id]);
                return Err(e.into());
            }
            (file.id, true)
        }
    };

    // The record of having read it carries the words, so `recall` reaches
    // them from the record alone, after the story has expired.
    let mut linked = None;
    if let Some(mut c) = consumption
        && c.text_file != Some(file)
    {
        c.text_file = Some(file);
        vault.save_consumption(&c)?;
        linked = Some(c.id);
    }
    Ok(Some(Kept { file, written, linked }))
}

// ---- the file --------------------------------------------------------------

/// The Markdown file for `story`: a small front matter that says where it is
/// from and when, a `# Title` heading, then what the feed carried -- its body
/// as Markdown, or with no body its summary and its address.
///
/// The front matter is YAML, but only as far as the document reader takes a
/// title from it: each value is one line, quoted. `read` is the day it was
/// read, which is what "what did I read last Tuesday" is answered from when
/// the record itself is gone.
fn render(
    story: &Story,
    source: &str,
    body: Option<&StoryBody>,
    published: Date,
    read: Date,
) -> String {
    let title = one_line(&story.title);
    let title = if title.is_empty() { "Untitled".to_string() } else { title };

    let mut out = String::from("---\n");
    out.push_str(&format!("title: {}\n", quoted(&title)));
    if !one_line(source).is_empty() {
        out.push_str(&format!("source: {}\n", quoted(source)));
    }
    if !story.url.trim().is_empty() {
        out.push_str(&format!("url: {}\n", quoted(&story.url)));
    }
    out.push_str(&format!("published: {published}\nread: {read}\n---\n\n# {title}\n\n"));

    let words = body.map(words_of).filter(|w| !w.trim().is_empty());
    match words {
        Some(words) => out.push_str(words.trim()),
        None => {
            // Nothing but what the list showed: say so, with where the rest
            // is, rather than keeping a heading and nothing under it.
            let summary = story.summary.trim();
            if !summary.is_empty() {
                out.push_str(summary);
                out.push_str("\n\n");
            }
            if story.url.trim().is_empty() {
                out.push_str("Only the feed's short summary was kept.");
            } else {
                out.push_str(&format!("The whole article is at <{}>.", story.url.trim()));
            }
        }
    }
    out.push('\n');
    if out.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('\n');
    }
    out
}

/// A body's words as Markdown: its HTML converted, with the story's own
/// pictures taken out, else its plain text.
fn words_of(body: &StoryBody) -> String {
    let markdown = karmayoga_mail::text::html_to_markdown(&body.html);
    let markdown = without_pictures(&markdown);
    if markdown.trim().is_empty() { body.text.clone() } else { markdown }
}

/// `markdown` without the pictures a story's body points at.
///
/// A body's pictures are rewritten to `karmayoga://news/img/...`, an address
/// only this application answers, and only while the story is there: in a file
/// that outlives it they are broken links. A picture is replaced by its
/// caption, if it has one, and a link that held nothing else goes with it.
fn without_pictures(markdown: &str) -> String {
    const OPEN: &str = "![";
    const OURS: &str = "(karmayoga://";
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let picture = after.find(']').and_then(|close| {
            let target = after[close + 1..].strip_prefix(OURS)?;
            let end = target.find(')')?;
            Some((&after[..close], &target[end + 1..]))
        });
        let Some((caption, mut tail)) = picture else {
            out.push_str(OPEN);
            rest = after;
            continue;
        };
        let caption = one_line(caption);
        if caption.is_empty() {
            // `[![](karmayoga://...)](https://example.com/big)`: a link that
            // held only the picture.
            if out.ends_with('[')
                && let Some(link) = tail.strip_prefix("](")
                && let Some(end) = link.find(')')
            {
                out.pop();
                tail = &link[end + 1..];
            }
        } else {
            out.push_str(&caption);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// `text` on one line, whitespace runs folded to a space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` as a YAML double-quoted string on one line. JSON's escapes are
/// YAML's, and folding the whitespace first takes out the line separators
/// JSON passes through and YAML would break a string at.
fn quoted(text: &str) -> String {
    serde_json::to_string(&one_line(text)).unwrap_or_else(|_| "\"\"".to_string())
}

// ---- names -----------------------------------------------------------------

/// The folder a source's articles go in, relative to the files folder.
fn source_dir(source: &str) -> String {
    format!("{ARTICLES_DIR}/{}", safe_name(source, SOURCE_BYTES, "Feed"))
}

/// An article's file name: the day it was published, which keeps a source's
/// folder in the order its articles came out, then its title.
fn article_name(published: Date, title: &str) -> String {
    format!(
        "{:04}-{:02}-{:02} {}.md",
        published.year(),
        published.month(),
        published.day(),
        safe_name(title, TITLE_BYTES, "Article")
    )
}

// ---- one at a time ---------------------------------------------------------

/// The stories being kept right now.
static IN_FLIGHT: Mutex<Vec<StoryId>> = Mutex::new(Vec::new());

/// Held while a story is being kept, so that a second keep of it at the same
/// moment does not write the article again. See the module docs.
struct Keeping(StoryId);

impl Keeping {
    /// `None` when `story` is being kept already.
    fn start(story: StoryId) -> Option<Keeping> {
        let mut busy = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if busy.contains(&story) {
            return None;
        }
        busy.push(story);
        Some(Keeping(story))
    }
}

impl Drop for Keeping {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| *s != self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Change, EventSink};
    use karmayoga_core::VaultConfig;
    use karmayoga_core::files::FileStatus;
    use karmayoga_core::news::{
        BodyOrigin, ConsumeMode, Consumption, Enclosure, FeedSettings, NewsSource, SourceKind,
    };

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
        fn count(&self, kind: Kind, op: Op) -> usize {
            self.changes.lock().unwrap().iter().filter(|c| c.kind == kind && c.op == op).count()
        }
    }

    struct World {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        events: Arc<Collector>,
        folder: tempfile::TempDir,
        _home: tempfile::TempDir,
    }

    fn world() -> World {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = karmayoga_vault::create(home.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        let events = Arc::new(Collector::default());
        svc.set_events(events.clone());
        let vault = svc.set(vault);
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        World { svc, vault, events, folder, _home: home }
    }

    fn follow(vault: &Vault, title: &str) -> NewsSource {
        let kind = SourceKind::Web { url: format!("https://example.com/{title}.xml") };
        let source = NewsSource::new(kind, title);
        vault.save_news_source(&source).unwrap();
        source
    }

    fn article(vault: &Vault, source: &NewsSource, title: &str, body: Option<&str>) -> Story {
        let published: Timestamp = "2026-03-09T12:00:00Z".parse().unwrap();
        let mut story = Story::new(source.id, format!("key {title}"), title, published);
        story.url = "https://example.com/post".into();
        story.summary = "A short summary.".into();
        let body = body.map(|html| StoryBody {
            story_id: story.id,
            html: html.into(),
            text: "Plain words of the post.".into(),
            origin: BodyOrigin::Feed,
            images: Vec::new(),
        });
        vault.insert_stories(&[(story.clone(), body)]).unwrap();
        vault.story(story.id).unwrap()
    }

    fn read_by(vault: &Vault, story: &Story) -> Consumption {
        let mut c =
            Consumption::new(ContentRef::Story(story.id), story.title.clone(), ConsumeMode::Read);
        c.finished = true;
        c.progress = 1.0;
        vault.save_consumption(&c).unwrap();
        c
    }

    fn text_of(w: &World, file: FileId) -> String {
        w.vault.read_file_text(file, 1 << 20).unwrap()
    }

    const POST: &str = "<h2>Why gardens</h2><p>Soil is <b>alive</b>, see \
        <a href=\"https://example.com/soil\">the soil page</a>.</p>\
        <p><img src=\"karmayoga://news/img/abc/def\" alt=\"A bed of leeks\"></p>\
        <p><a href=\"https://example.com/big\"><img src=\"karmayoga://news/img/abc/ghi\"></a></p>";

    #[tokio::test]
    async fn an_article_is_kept_as_markdown_under_its_source_and_linked() {
        let w = world();
        let source = follow(&w.vault, "Garden: Notes");
        let story = article(&w.vault, &source, "Why: gardens?", Some(POST));
        let said = read_by(&w.vault, &story);

        let file = keep_article(&w.svc, story.id).await.unwrap().expect("it was kept");
        files::settled().await;

        let entry = w.vault.file(file).unwrap();
        assert_eq!(entry.origin, FileOrigin::Feed);
        assert!(matches!(entry.status, FileStatus::Present));
        assert_eq!(entry.path(), Some("Feed/Articles/Garden - Notes/2026-03-09 Why - gardens.md"));
        assert!(w.folder.path().join(entry.path().unwrap()).is_file(), "it is in the folder");

        let text = text_of(&w, file);
        assert!(text.starts_with("---\ntitle: \"Why: gardens?\"\n"), "{text}");
        assert!(text.contains("\nsource: \"Garden: Notes\"\n"), "{text}");
        assert!(text.contains("\nurl: \"https://example.com/post\"\n"), "{text}");
        assert!(text.contains("\npublished: 2026-03-09\nread: "), "{text}");
        assert!(text.contains("\n---\n\n# Why: gardens?\n\n"), "{text}");
        assert!(text.contains("Soil is **alive**"), "the body, as Markdown: {text}");
        assert!(text.contains("[the soil page](https://example.com/soil)"), "{text}");
        assert!(text.contains("A bed of leeks"), "a picture is its caption: {text}");
        assert!(!text.contains("karmayoga://"), "no address only the app answers: {text}");
        assert!(!text.contains("[]("), "a link of only a picture goes with it: {text}");

        let linked = w.vault.story(story.id).unwrap();
        assert_eq!(linked.files.text, Some(file), "the story links the kept article");
        let c = w.vault.consumption(&ContentRef::Story(story.id)).unwrap().unwrap();
        assert_eq!(c.id, said.id);
        assert_eq!(c.text_file, Some(file), "and so does the record of having read it");

        assert_eq!(w.events.count(Kind::File, Op::Created), 1);
        assert_eq!(w.events.count(Kind::Story, Op::Updated), 1);
        assert_eq!(w.events.count(Kind::Consumption, Op::Updated), 1);
    }

    #[tokio::test]
    async fn an_article_is_kept_once_however_often_it_is_finished() {
        let w = world();
        let source = follow(&w.vault, "Blog");
        let story = article(&w.vault, &source, "Once", Some(POST));
        read_by(&w.vault, &story);

        let first = keep_article(&w.svc, story.id).await.unwrap().unwrap();
        let again = keep_article(&w.svc, story.id).await.unwrap().unwrap();
        files::settled().await;
        assert_eq!(first, again);
        assert_eq!(w.events.count(Kind::File, Op::Created), 1, "written once");
        let in_folder = std::fs::read_dir(w.folder.path().join("Feed/Articles/Blog")).unwrap();
        assert_eq!(in_folder.count(), 1);

        // Two at the same moment: the second steps aside.
        let held = Keeping::start(story.id).unwrap();
        assert!(keep_article(&w.svc, story.id).await.unwrap().is_none());
        drop(held);

        // In the trash, it is the person's call, and is not written again.
        w.vault.trash_files(&[first]).unwrap();
        let after_trash = keep_article(&w.svc, story.id).await.unwrap();
        assert_eq!(after_trash, Some(first));
        assert_eq!(w.events.count(Kind::File, Op::Created), 1);
    }

    #[tokio::test]
    async fn a_link_to_a_file_the_catalog_lost_is_written_again() {
        let w = world();
        let source = follow(&w.vault, "Blog");
        let story = article(&w.vault, &source, "Lost", Some(POST));
        // A link to a file that was never there, as if it had been emptied
        // out of the trash.
        let gone = FileId::new();
        w.vault.update_story_files(story.id, |f| f.text = Some(gone)).unwrap();

        let file = keep_article(&w.svc, story.id).await.unwrap().unwrap();
        files::settled().await;
        assert_ne!(file, gone);
        assert_eq!(w.vault.story(story.id).unwrap().files.text, Some(file));
    }

    #[tokio::test]
    async fn a_story_with_no_body_keeps_its_summary_and_its_address() {
        let w = world();
        let source = follow(&w.vault, "Blog");
        let story = article(&w.vault, &source, "Thin", None);
        let file = keep_article(&w.svc, story.id).await.unwrap().unwrap();
        files::settled().await;
        let text = text_of(&w, file);
        assert!(text.contains("# Thin\n\nA short summary.\n\n"), "{text}");
        assert!(text.contains("The whole article is at <https://example.com/post>."), "{text}");

        // A body that converts to nothing falls back to its plain text.
        let bare =
            article(&w.vault, &source, "Bare", Some("<img src=\"karmayoga://news/img/a/b\">"));
        let file = keep_article(&w.svc, bare.id).await.unwrap().unwrap();
        files::settled().await;
        assert!(text_of(&w, file).contains("Plain words of the post."));
    }

    #[tokio::test]
    async fn an_episode_a_newsletter_and_a_setting_turned_off_keep_nothing() {
        let w = world();
        let source = follow(&w.vault, "Blog");

        let mut episode = article(&w.vault, &source, "Episode", Some(POST));
        episode.media = Some(Enclosure {
            url: "https://x/a.mp3".into(),
            mime: "audio/mpeg".into(),
            length: None,
            duration_ms: None,
            transcript_url: String::new(),
            transcript_mime: String::new(),
            episode: None,
            season: None,
        });
        assert!(!is_article(&episode), "an episode's words are its transcript");
        let mut issue = episode.clone();
        issue.media = None;
        issue.mail_message = Some(karmayoga_core::id::MailMessageId::new());
        assert!(!is_article(&issue), "a newsletter is mail, which mail already searches");
        assert!(is_article(&article(&w.vault, &source, "Post", Some(POST))));

        // An enclosure that is a PDF is not something to play: an article.
        let mut pdf = episode.clone();
        pdf.media.as_mut().unwrap().mime = "application/pdf".into();
        assert!(is_article(&pdf));

        // A real story with the Feed's setting off.
        let post = article(&w.vault, &source, "Off", Some(POST));
        w.vault
            .save_feed_settings(&FeedSettings { keep_read: false, ..FeedSettings::default() })
            .unwrap();
        assert_eq!(keep_article(&w.svc, post.id).await.unwrap(), None);
        assert!(w.vault.story(post.id).unwrap().files.text.is_none());
        assert_eq!(w.events.count(Kind::File, Op::Created), 0);

        // And a story that is not there is said not to be.
        w.vault.save_feed_settings(&FeedSettings::default()).unwrap();
        let err = keep_article(&w.svc, karmayoga_core::id::StoryId::new()).await.unwrap_err();
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
    }

    #[test]
    fn pictures_are_taken_out_and_their_captions_kept() {
        assert_eq!(without_pictures("a ![](karmayoga://news/img/x/y) b"), "a  b");
        assert_eq!(without_pictures("a ![ A  leek ](karmayoga://news/img/x/y) b"), "a A leek b");
        assert_eq!(
            without_pictures("[![](karmayoga://news/img/x/y)](https://e.com/big) after"),
            " after"
        );
        assert_eq!(
            without_pictures("[![Cap](karmayoga://news/img/x/y)](https://e.com/big)"),
            "[Cap](https://e.com/big)"
        );
        // Anything else is not this module's to take out.
        let other = "![x](https://e.com/p.png) and ![unclosed";
        assert_eq!(without_pictures(other), other);
    }

    #[test]
    fn a_name_is_made_safe_and_a_quoted_value_is_one_line() {
        let day = Date::constant(2026, 3, 9);
        assert_eq!(article_name(day, "AC/DC: back?"), "2026-03-09 AC-DC - back.md");
        assert_eq!(article_name(day, "???"), "2026-03-09 Article.md");
        assert_eq!(source_dir("The \"Weekly\" | News"), "Feed/Articles/The Weekly News");
        assert_eq!(source_dir(" "), "Feed/Articles/Feed");
        assert_eq!(quoted("a \"b\"\n\tc\u{2028}d"), "\"a \\\"b\\\" c d\"");
    }
}
