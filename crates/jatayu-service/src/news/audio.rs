//! An article turned into audio: read the way Listen reads it, rendered into
//! `Feed/Audio/`, linked as the story's audio, and played by the player like
//! an episode. See `docs/plans/podcasts.md` (phase 2).
//!
//! One supervised task ([`TASK_KEY`]) works through a list, one article at a
//! time -- a voice on this computer is the one thing in the application that
//! uses every core it is given, and two renders would only take twice as
//! long each. A job is three steps. [`script_for`] has the article read as
//! Listen reads it ([`super::listen::open`]: the model makes it fit to hear
//! when one is set up, else it is read as written) and puts it in a
//! [`Script`], a line to a paragraph, with its headline and where it is from
//! before it. [`render_script`] says it into `Feed/Audio/<source>/`, named
//! for the date it was published and its title. [`link`] makes the file the
//! story's audio, which is all it takes for Up Next to play it: a story with
//! no episode and a present audio file is something to hear
//! ([`jatayu_core::vault::upnext_entry`]).
//!
//! # Nothing here is a promise to keep
//!
//! The waiting list, the progress and the failures live in memory
//! ([`AudioJobs`]) and go when the vault locks, as downloads' do. A render
//! that was under way when the task was stopped goes back to the front of the
//! list and starts again: what is kept is the file, once it is whole.

use super::downloads::safe_name;
use super::work::{self, Ended, Measured, Queue, Task, Worker};
use super::{WorkItem, WorkKind, announce_one};
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Kind, Op};
use crate::news::listen;
use crate::service::{Service, blocking};
use crate::speech::listen::PieceStream;
use crate::speech::render::render_script;
use futures::StreamExt;
use futures::future::BoxFuture;
use jatayu_core::id::StoryId;
use jatayu_core::news::Story;
use jatayu_core::news::podcast::AUDIO_DIR;
use jatayu_core::news::script::Script;
use jatayu_core::{FileEntry, Vault};
use jiff::civil::Date;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// The supervisor key of the rendering task.
pub const TASK_KEY: &str = "news-audio";

/// The longest a source's folder name or an article's file name may be, in
/// bytes: well inside what a file system takes, and what a person can read in
/// a list.
const SOURCE_BYTES: usize = 80;
const TITLE_BYTES: usize = 120;

// ── What is going on ─────────────────────────────────────────────────────

/// Articles waiting to be rendered and the one under way, as a list
/// ([`work::Queue`]). Lives on [`Service`] (`Service::listening`).
pub type AudioJobs = Queue<Render, Progress>;

/// One article to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Render(StoryId);

impl Task for Render {
    fn same(self, other: Self) -> bool {
        self.0 == other.0
    }

    fn kind(self) -> WorkKind {
        WorkKind::Audio
    }

    fn story(self) -> Option<StoryId> {
        Some(self.0)
    }

    fn unnamed(self) -> &'static str {
        "Article"
    }
}

/// How far a render has got, written by the render and read by [`status`]:
/// thousandths, or `-1` while it is still being read and nothing can be said.
pub struct Progress(AtomicI32);

impl Progress {
    fn new() -> Self {
        Self(AtomicI32::new(-1))
    }

    fn set(&self, fraction: f32) {
        self.0.store((fraction.clamp(0.0, 1.0) * 1000.0).round() as i32, Ordering::Relaxed);
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Measured for Progress {
    fn fraction(&self) -> Option<f32> {
        let thousandths = self.0.load(Ordering::Relaxed);
        (thousandths >= 0).then(|| thousandths as f32 / 1000.0)
    }
}

// ── The task ─────────────────────────────────────────────────────────────

/// Start the rendering task if it is not already running.
///
/// Only where this process may write: the audio is linked to its story and
/// kept in the files folder, and a second window that merely reads must not
/// spend minutes making what it cannot keep.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    work::ensure_task(svc, vault, Rendering);
}

/// One article at a time: a voice on this computer is the one thing in the
/// application that uses every core it is given.
#[derive(Clone)]
struct Rendering;

impl Worker for Rendering {
    type Task = Render;
    type Progress = Progress;
    const KEY: &'static str = TASK_KEY;

    fn queue(svc: &Service) -> &AudioJobs {
        &svc.listening.audio
    }

    fn work<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        task: Render,
        progress: &'a Arc<Progress>,
    ) -> BoxFuture<'a, Ended<Render>> {
        Box::pin(async move {
            match render(svc, vault, task.0, progress).await {
                Ok(()) => Ended::Finished,
                // Leaving this future -- the task was stopped -- drops what is
                // rendering, which throws away what it has made.
                Err(e) => Ended::Failed(e.message),
            }
        })
    }
}

/// One article, from the list to a file linked to its story. Nothing is done
/// for one that has its audio already, in the folder and in place.
async fn render(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    id: StoryId,
    progress: &Progress,
) -> CommandResult<()> {
    let present = blocking({
        let vault = vault.clone();
        move || Ok(has_audio(&vault, &vault.story(id)?))
    })
    .await?;
    if present {
        return Ok(());
    }
    let (script, dir, name) = script_for(svc, id).await?;
    let file = render_script(svc, &script, &dir, &name, &|done| progress.set(done)).await?;
    link(svc, vault, id, file).await
}

// ── Asking ───────────────────────────────────────────────────────────────

/// Put `story` on the list (once) and wake the task. An article that has its
/// audio already is left as it is; removing the file is how it is made again.
///
/// Refuses, in a sentence, a vault this process may not write to and a story
/// that is not there. The reads are made on the blocking pool.
pub async fn request(svc: &Arc<Service>, story: StoryId) -> CommandResult<()> {
    let vault = svc.require_unlocked()?;
    if !vault.is_writable() {
        return Err(CommandError::new(
            codes::VAULT_IN_USE,
            "another copy of Jatayu has this vault open for writing, so nothing can be \
             rendered here.",
        ));
    }
    let found = blocking(move || {
        let found = vault.story(story)?;
        Ok((has_audio(&vault, &found), found.title))
    })
    .await?;
    let (has, title) = found;
    if has {
        return Ok(());
    }
    svc.listening.audio.queue(Render(story), title);
    svc.listening.audio.wake();
    Ok(())
}

/// Everything waiting, under way, or failed since it was last asked for.
pub fn status(svc: &Service) -> Vec<WorkItem> {
    svc.listening.audio.statuses()
}

// ── The script ───────────────────────────────────────────────────────────

/// The script `story` is said as, and where its audio goes: the folder, and
/// the file's name without its extension.
///
/// The article is read as Listen reads it, which is how it comes to be fit to
/// hear, and refused as Listen refuses it: a story with nothing in it to read
/// fails with Listen's own sentence. Whatever the reading says to the listener
/// -- the whole article could not be fetched, no model prepared it -- is a
/// note on the screen and has no place in a file, so it is dropped.
async fn script_for(svc: &Arc<Service>, id: StoryId) -> CommandResult<(Script, String, String)> {
    let vault = svc.require_unlocked()?;
    let (story, source) = blocking(move || {
        let story = vault.story(id)?;
        let source = vault.news_source(story.source_id).map(|s| s.title).unwrap_or_default();
        Ok((story, source))
    })
    .await?;
    let opened = listen::open(svc, id, false).await?;
    let paragraphs = read(opened.pieces).await?;
    let title = if opened.title.trim().is_empty() { story.title.clone() } else { opened.title };
    let mut lines = opening(&title, &source, paragraphs.first().map(String::as_str));
    lines.extend(paragraphs);
    let script = Script::narrated_paragraphs(title.clone(), lines);
    let date =
        jatayu_core::model::local_date_in(story.published_at, &jatayu_core::model::system_tz());
    Ok((script, audio_dir(&source), audio_name(date, &title)))
}

/// Everything an article's pieces say, as paragraphs. A piece belongs to a
/// part, which is a paragraph of the article as written or a stretch of what
/// the model wrote, and a part may hold several.
async fn read(mut pieces: PieceStream) -> CommandResult<Vec<String>> {
    let mut parts: Vec<(u32, String)> = Vec::new();
    // An error ends it. Listen says what was said and reports it, but a file
    // is the article or it is not.
    while let Some(piece) = pieces.next().await {
        let piece = piece?;
        match parts.last_mut() {
            Some((part, text)) if *part == piece.part => text.push_str(&piece.text),
            _ => parts.push((piece.part, piece.text)),
        }
    }
    Ok(parts.iter().flat_map(|(_, text)| paragraphs(text)).collect())
}

/// `text` as the paragraphs between its blank lines.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            open = false;
        } else if open {
            let paragraph = out.last_mut().expect("a paragraph is open");
            paragraph.push('\n');
            paragraph.push_str(line);
        } else {
            out.push(line.to_string());
            open = true;
        }
    }
    out
}

/// What is said ahead of the article, as a podcast says what it is about to
/// play: its headline, and where it is from. Nothing for a headline the
/// article's own first line already is, or for one it has not got.
fn opening(title: &str, source: &str, first: Option<&str>) -> Vec<String> {
    let (title, source) = (title.trim(), source.trim());
    let same = |a: &str, b: &str| {
        let plain = |t: &str| -> String {
            t.trim_start_matches('#').trim().trim_end_matches(['.', '!', '?', ':']).to_lowercase()
        };
        plain(a) == plain(b)
    };
    let mut lines = Vec::new();
    if !title.is_empty() && !first.is_some_and(|f| same(f, title)) {
        lines.push(title.to_string());
    }
    if !source.is_empty() && !same(source, title) {
        lines.push(format!("From {source}."));
    }
    lines
}

// ── Keeping ──────────────────────────────────────────────────────────────

/// Make `file` the audio of `story` and say so.
///
/// A story that is gone -- expired, or its source deleted -- while its audio
/// was being made leaves a file nothing refers to, and nothing would ever
/// clear it out; it goes to the trash instead.
async fn link(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    story: StoryId,
    file: FileEntry,
) -> CommandResult<()> {
    let file_id = file.id;
    blocking({
        let vault = vault.clone();
        move || {
            if let Err(e) = vault.update_story_files(story, |files| files.audio = Some(file_id)) {
                let _ = vault.trash_files(&[file_id]);
                return Err(e.into());
            }
            Ok(())
        }
    })
    .await?;
    announce_one(svc, Kind::File, Op::Created, file_id);
    announce_one(svc, Kind::Story, Op::Updated, story);
    Ok(())
}

// ── Names ────────────────────────────────────────────────────────────────

/// The folder an article's audio goes in, relative to the files folder: one
/// to a source, as an episode's is.
pub fn audio_dir(source: &str) -> String {
    format!("{AUDIO_DIR}/{}", safe_name(source, SOURCE_BYTES, "Articles"))
}

/// An article's file name, without its extension: the date it was published,
/// which keeps a source's folder in the order its articles came out, then its
/// title.
pub fn audio_name(published: Date, title: &str) -> String {
    format!(
        "{:04}-{:02}-{:02} {}",
        published.year(),
        published.month(),
        published.day(),
        safe_name(title, TITLE_BYTES, "Article")
    )
}

/// Whether `story`'s audio is in the folder: linked, and the file neither in
/// the trash nor missing. A link outlives the file, as an episode's does.
fn has_audio(vault: &Vault, story: &Story) -> bool {
    story.files.audio.is_some_and(|file| vault.file_is_present(file).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Change, EventSink};
    use crate::news::WorkState;
    use crate::supervisor::Outcome;
    use jatayu_core::VaultConfig;
    use jatayu_core::files::{FileOrigin, ImportOptions};
    use jatayu_core::news::{NewsSource, SourceKind, Story, StoryBody};
    use std::sync::Mutex;
    use tokio::sync::watch;

    fn story_id() -> StoryId {
        StoryId::new()
    }

    // ── The list ─────────────────────────────────────────────────────────

    #[test]
    fn an_article_on_the_list_is_audio_work_with_no_file() {
        let jobs = AudioJobs::default();
        let (a, b) = (story_id(), story_id());
        assert!(jobs.queue(Render(a), "A".into()));
        assert!(!jobs.queue(Render(a), "A".into()), "asking twice is one render");
        assert!(jobs.queue(Render(b), String::new()));
        let (first, _, _) = jobs.start_next().expect("one is next");
        assert_eq!(first, Render(a));
        let items = jobs.statuses();
        assert_eq!(
            items.iter().map(|i| (i.story_id, i.state)).collect::<Vec<_>>(),
            [(Some(a), WorkState::Working), (Some(b), WorkState::Waiting)]
        );
        assert!(items.iter().all(|i| i.kind == WorkKind::Audio && i.file_id.is_none()));
        assert_eq!((items[0].title.as_str(), items[1].title.as_str()), ("A", "Article"));
    }

    #[test]
    fn progress_is_unknown_until_there_is_some() {
        let jobs = AudioJobs::default();
        jobs.queue(Render(story_id()), "A".into());
        let (_, _, progress) = jobs.start_next().unwrap();
        assert_eq!(jobs.statuses()[0].progress, None, "still being read");
        progress.set(0.25);
        assert_eq!(jobs.statuses()[0].progress, Some(0.25));
        progress.set(7.0);
        assert_eq!(jobs.statuses()[0].progress, Some(1.0));
    }

    // ── What is said ahead of the article ────────────────────────────────

    #[test]
    fn an_article_opens_with_its_headline_and_where_it_is_from() {
        assert_eq!(
            opening("A new bridge", "The Gazette", Some("The first paragraph.")),
            ["A new bridge", "From The Gazette."]
        );
        // The article's own first line is the headline already.
        assert_eq!(
            opening("A new bridge", "The Gazette", Some("# A New Bridge")),
            ["From The Gazette."]
        );
        assert_eq!(opening("A new bridge!", "", Some("a new bridge")), Vec::<String>::new());
        assert_eq!(opening("", "The Gazette", None), ["From The Gazette."]);
        assert_eq!(opening("The Gazette", "The Gazette", None), ["The Gazette"]);
    }

    #[test]
    fn paragraphs_are_what_stands_between_blank_lines() {
        assert_eq!(
            paragraphs("One\nstill one.\n\n  \nTwo.\n\n\nThree\n"),
            ["One\nstill one.", "Two.", "Three"]
        );
        assert!(paragraphs(" \n\n ").is_empty());
    }

    // ── Names ────────────────────────────────────────────────────────────

    #[test]
    fn audio_is_named_for_its_source_its_date_and_its_title() {
        let date = Date::new(2026, 3, 9).unwrap();
        assert_eq!(audio_name(date, "Why: a test?"), "2026-03-09 Why - a test");
        assert_eq!(audio_name(date, "  "), "2026-03-09 Article");
        assert_eq!(audio_dir("The Gazette: Online"), "Feed/Audio/The Gazette - Online");
        assert_eq!(audio_dir("///"), "Feed/Audio/---");
        assert_eq!(audio_dir("?"), "Feed/Audio/Articles");
        assert_eq!(safe_name("..hidden..", 100, "x"), "hidden");
        assert_eq!(safe_name(&"é".repeat(100), 11, "x"), "é".repeat(5));
    }

    // ── In a vault ───────────────────────────────────────────────────────

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

    struct Fixture {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        events: Arc<Collector>,
        _folder: tempfile::TempDir,
        _home: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = jatayu_vault::create(home.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        let events = Arc::new(Collector::default());
        svc.set_events(events.clone());
        let vault = svc.set(vault);
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        Fixture { svc, vault, events, _folder: folder, _home: home }
    }

    /// A story with an article body, from a source with a title.
    fn article(vault: &Vault, source: &str, title: &str, html: &str) -> Story {
        let source = {
            let found = vault.news_sources().unwrap().into_iter().find(|s| s.title == source);
            found.unwrap_or_else(|| {
                let made = NewsSource::new(
                    SourceKind::Web { url: format!("https://example.com/{source}.xml") },
                    source,
                );
                vault.save_news_source(&made).unwrap();
                made
            })
        };
        let story = Story::new(
            source.id,
            format!("key {title}"),
            title,
            "2026-03-09T12:00:00Z".parse().unwrap(),
        );
        let body = StoryBody {
            byline: None,
            site_name: None,
            story_id: story.id,
            html: html.into(),
            text: String::new(),
            origin: jatayu_core::news::BodyOrigin::Extracted,
            images: Vec::new(),
        };
        vault.insert_stories(&[(story.clone(), Some(body))]).unwrap();
        story
    }

    /// An audio file in the files folder, as a render leaves one.
    fn audio_file(vault: &Vault, name: &str) -> FileEntry {
        let options = ImportOptions {
            dir: Some("Feed/Audio/Show".into()),
            name: Some(name.into()),
            origin: FileOrigin::Feed,
            encrypted: None,
        };
        vault.import_bytes(b"OggS....", name, &options).unwrap()
    }

    #[tokio::test]
    async fn an_article_is_read_as_listen_reads_it_with_its_headline_first() {
        let f = fixture();
        let story = article(
            &f.vault,
            "The Gazette: Online",
            "A new bridge: opens",
            "<p>The bridge opened on Monday after six years.</p><p>Crowds came.</p>\
             <ul><li>Cyclists</li><li>Walkers</li></ul>",
        );
        let (script, dir, name) = script_for(&f.svc, story.id).await.unwrap();
        assert_eq!(script.title, "A new bridge: opens");
        let said: Vec<&str> = script.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(said[0], "A new bridge: opens");
        assert_eq!(said[1], "From The Gazette: Online.");
        assert_eq!(said[2], "The bridge opened on Monday after six years.");
        assert_eq!(said[3], "Crowds came.");
        assert!(said[4].contains("Cyclists"), "{said:?}");
        assert!(script.lines.iter().all(|l| l.speaker == "narrator"));
        assert_eq!(dir, "Feed/Audio/The Gazette - Online");
        assert_eq!(name, "2026-03-09 A new bridge - opens");
    }

    #[tokio::test]
    async fn an_article_that_cannot_be_read_fails_in_listen_s_words() {
        let f = fixture();
        let story = article(&f.vault, "Show", "Nothing", "");
        let err = script_for(&f.svc, story.id).await.err().unwrap();
        assert_eq!(err.code, codes::INVALID);
        assert_eq!(err.message, "There is nothing in this story to read.");
        // And a job that does not exist is not found.
        let err = script_for(&f.svc, story_id()).await.err().unwrap();
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn asking_puts_an_article_on_the_list_once_and_a_finished_one_is_left_alone() {
        let f = fixture();
        let story = article(&f.vault, "Show", "Story", "<p>Something to read, aloud.</p>");
        request(&f.svc, story.id).await.unwrap();
        request(&f.svc, story.id).await.unwrap();
        let items = status(&f.svc);
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].state, items[0].title.as_str()), (WorkState::Waiting, "Story"));
        assert_eq!(request(&f.svc, story_id()).await.err().unwrap().code, codes::NOT_FOUND);

        // One that has its audio is not asked for again.
        let done = article(&f.vault, "Show", "Done", "<p>Something else to read, aloud.</p>");
        let file = audio_file(&f.vault, "2026-03-09 Done.opus");
        f.vault.update_story_files(done.id, |files| files.audio = Some(file.id)).unwrap();
        request(&f.svc, done.id).await.unwrap();
        assert_eq!(status(&f.svc).len(), 1);
        assert!(has_audio(&f.vault, &f.vault.story(done.id).unwrap()));
        // Until the file is gone from the folder.
        f.vault.trash_files(&[file.id]).unwrap();
        assert!(!has_audio(&f.vault, &f.vault.story(done.id).unwrap()));
        request(&f.svc, done.id).await.unwrap();
        assert_eq!(status(&f.svc).len(), 2);
    }

    #[tokio::test]
    async fn the_file_is_linked_and_announced() {
        let f = fixture();
        let story = article(&f.vault, "Show", "Story", "<p>Something to read, aloud.</p>");
        let file = audio_file(&f.vault, "2026-03-09 Story.opus");
        link(&f.svc, &f.vault, story.id, file.clone()).await.unwrap();
        assert_eq!(f.vault.story(story.id).unwrap().files.audio, Some(file.id));
        assert_eq!(f.events.count(Kind::File, Op::Created), 1);
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 1);
        // It is what makes the story something to hear in Up Next.
        let facts = f.vault.describe_content(jatayu_core::news::ContentRef::Story(story.id));
        assert_eq!(facts.unwrap().media_file, Some(file.id));
    }

    #[tokio::test]
    async fn audio_for_a_story_that_has_gone_is_not_left_in_the_folder() {
        let f = fixture();
        let file = audio_file(&f.vault, "2026-03-09 Gone.opus");
        let err = link(&f.svc, &f.vault, story_id(), file.clone()).await.err().unwrap();
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(!f.vault.file_is_present(file.id).unwrap(), "in the trash, not the folder");
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 0);
    }

    #[tokio::test]
    async fn the_task_works_through_the_list_says_why_one_failed_and_stops_when_told() {
        let f = fixture();
        let story = article(&f.vault, "Show", "Nothing", "");
        request(&f.svc, story.id).await.unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(work::run(&Rendering, f.svc.clone(), f.vault.clone(), stopped));
        let failed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let items = status(&f.svc);
                if let Some(item) = items.into_iter().find(|i| i.state == WorkState::Failed) {
                    break item;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the render came to an end");
        assert_eq!(failed.story_id, Some(story.id));
        assert_eq!(failed.error.as_deref(), Some("There is nothing in this story to read."));
        assert_eq!(status(&f.svc).len(), 1, "nothing else is left waiting");

        stop.send(true).unwrap();
        let ended = tokio::time::timeout(std::time::Duration::from_secs(20), task).await;
        assert!(matches!(ended.expect("it stopped").unwrap(), Ok(Outcome::Done)));
        // Asked for again, it is on the list again, and the failure is gone.
        request(&f.svc, story.id).await.unwrap();
        let items = status(&f.svc);
        assert_eq!((items.len(), items[0].state), (1, WorkState::Waiting));
    }
}
