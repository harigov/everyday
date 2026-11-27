//! What a story, a file or a library item says about itself when it goes in
//! Up Next or its reading is recorded: the one place that decides.
//!
//! Up Next keeps an entry as a snapshot -- a title, where it is from, how it
//! is taken in, how long it is -- and a [`Consumption`](crate::news::Consumption)
//! keeps the same words, so a list can be drawn and a history read long after
//! the story has expired or the file has gone. Three callers need exactly
//! that: the person's own button and the Feed's top-up (in the service), the
//! assistant's `add_to_up_next` (in the tool catalogue), and every progress
//! report. They used to read the three kinds of thing each their own way, and
//! the copies had already drifted -- one refused a picture and Health's
//! files, another did not. One rule set lives here instead:
//!
//! * A **story** is something to hear or watch when its enclosure is audio or
//!   video, something to hear when it has audio of its own -- an article
//!   turned into audio ([`StoryFiles::audio`](crate::news::StoryFiles::audio))
//!   -- and something to read otherwise. One that has expired is gone.
//! * A **file** is something to hear, watch or read -- audio, video or a
//!   document -- and must be in the folder: not in the trash, not missing. A
//!   picture or an unrecognised file is refused with words, and so is one of
//!   Health's: a clinical document or a meal's photograph is reached only
//!   through the health tools and their gates.
//! * An **item** is something to read, through its link or its first file.
//!
//! # An episode counts as downloaded only if its file is there
//!
//! A story links to its downloaded episode by id, and a link outlives the
//! file: it can be trashed by hand or by clean-up, or go missing from the
//! folder. [`ContentFacts::download`] is therefore set whenever an episode
//! has no *present* file, and [`ContentFacts::media_file`] is the file only
//! when it is present.
//!
//! An article's audio is made, not fetched, so there is nothing to download
//! for it: with no present file it is simply an article, read. And an episode
//! is its enclosure's even when audio was made of its show notes: the
//! recording is what is heard.

use super::Vault;
use crate::error::{Error, Result};
use crate::files::{FileEntry, FileKind, FileOrigin, FileStatus};
use crate::id::{FileId, StoryId};
use crate::library::Item;
use crate::news::{AddedBy, ConsumeMode, ContentRef, EnclosureKind, QueueEntry, Story};

/// What a thing says about itself, in the words a snapshot keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentFacts {
    pub what: ContentRef,
    pub title: String,
    /// The show, the feed, the folder, the shelf: where it is from, in a few
    /// words. Empty when its source is gone.
    pub source: String,
    /// The story's or the item's own address, for opening it again. Empty
    /// for a file.
    pub url: String,
    /// Heard, watched or read.
    pub mode: ConsumeMode,
    pub duration_ms: Option<u64>,
    /// The recording itself, when there is one in the folder.
    pub media_file: Option<FileId>,
    /// Its words in the files folder: a transcript, an article kept as
    /// Markdown, or the document itself.
    pub text_file: Option<FileId>,
    /// An episode with no file in the folder: it has to be downloaded before
    /// it can be played.
    pub download: Option<StoryId>,
    /// The story, when the thing is one: whether it is read, whether it is a
    /// newsletter.
    pub story: Option<Story>,
}

impl ContentFacts {
    /// An entry for Up Next, not yet placed in it. Its `sort_order` is for
    /// [`Vault::add_to_queue`] to decide.
    pub fn queue_entry(&self, added_by: AddedBy) -> QueueEntry {
        let mut entry = QueueEntry::new(self.what, self.title.clone(), 0);
        entry.source = self.source.clone();
        entry.mode = self.mode;
        entry.duration_ms = self.duration_ms;
        entry.added_by = added_by;
        entry
    }
}

/// A file's name without its extension -- `Standup 12` for `Standup 12.m4a`
/// -- the way a person calls it. A dotfile, or a name with no extension,
/// keeps its whole name.
pub fn file_title(file: &FileEntry) -> String {
    let name = file.name.trim();
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.trim().is_empty() => stem.trim().to_string(),
        _ => name.to_string(),
    }
}

/// The folder a file is in, by its own name -- `Hard Fork` for
/// `Feed/Podcasts/Hard Fork/Episode.mp3` -- or `Files` for one at the top or
/// sealed away, which have no folder to name.
pub fn folder_name(file: &FileEntry) -> String {
    file.dir()
        .and_then(|dir| dir.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("Files")
        .to_string()
}

/// How a story is taken in: heard or watched when its enclosure is audio or
/// video, heard when it has audio of its own, read when it carries nothing to
/// play.
fn story_mode(story: &Story, has_audio: bool) -> ConsumeMode {
    match story.media.as_ref().filter(|m| m.is_playable()).map(|m| m.kind()) {
        Some(EnclosureKind::Audio) => ConsumeMode::Listen,
        Some(EnclosureKind::Video) => ConsumeMode::Watch,
        None if has_audio => ConsumeMode::Listen,
        None => ConsumeMode::Read,
    }
}

/// A story's facts, given its source's name, whether the file it links to as
/// its episode is there, and the file it links to as its own audio, if that
/// is there.
fn story_facts(
    story: &Story,
    source: &str,
    media_present: bool,
    audio: Option<&FileEntry>,
) -> ContentFacts {
    let playable = story.media.as_ref().filter(|m| m.is_playable());
    let own_audio = audio.filter(|a| playable.is_none() && matches!(a.status, FileStatus::Present));
    let title = [story.title.trim(), story.url.trim()]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or("Untitled story");
    ContentFacts {
        what: ContentRef::Story(story.id),
        title: title.to_string(),
        source: source.trim().to_string(),
        url: story.url.clone(),
        mode: story_mode(story, own_audio.is_some()),
        duration_ms: playable
            .map(|m| m.duration_ms)
            .unwrap_or_else(|| own_audio.and_then(|a| a.media.duration_ms)),
        media_file: story.files.media.filter(|_| media_present).or(own_audio.map(|a| a.id)),
        text_file: story.files.text.or(story.files.transcript),
        download: (playable.is_some() && !media_present).then_some(story.id),
        story: Some(story.clone()),
    }
}

/// A file's facts, or the reason it is no use to Up Next.
fn file_facts(file: &FileEntry) -> Result<ContentFacts> {
    if file.origin == FileOrigin::Health {
        return Err(Error::Invalid(
            "that file is one of Health's -- a clinical document's page or a meal's \
             photograph -- and is not something to play or read here"
                .into(),
        ));
    }
    if !matches!(file.status, FileStatus::Present) {
        return Err(Error::not_found("file", file.id));
    }
    let (mode, media, text) = match file.kind {
        FileKind::Audio => (ConsumeMode::Listen, Some(file.id), None),
        FileKind::Video => (ConsumeMode::Watch, Some(file.id), None),
        FileKind::Document => (ConsumeMode::Read, None, Some(file.id)),
        FileKind::Image | FileKind::Other => {
            return Err(Error::Invalid(
                "Up Next holds things to hear, watch or read: audio, video and documents. \
                 That file is none of them."
                    .into(),
            ));
        }
    };
    Ok(ContentFacts {
        what: ContentRef::File(file.id),
        title: file_title(file),
        source: folder_name(file),
        url: String::new(),
        mode,
        duration_ms: file.media.duration_ms,
        media_file: media,
        text_file: text,
        download: None,
        story: None,
    })
}

/// An item's facts, given the name of its shelf. An article on the Articles
/// shelf keeps its address as a fact, not a link; anything else may have been
/// given a link.
fn item_facts(item: &Item, shelf: &str) -> ContentFacts {
    let url = item
        .links
        .first()
        .map(|l| l.url.clone())
        .or_else(|| item.facts.get("url").cloned())
        .unwrap_or_default();
    ContentFacts {
        what: ContentRef::Item(item.id),
        title: item.title.trim().to_string(),
        source: shelf.trim().to_string(),
        url,
        mode: ConsumeMode::Read,
        duration_ms: None,
        media_file: None,
        text_file: None,
        download: None,
        story: None,
    }
}

impl Vault {
    /// What `what` says about itself, for Up Next and for the record of
    /// having read or heard it. Fails with `NotFound` for something that is
    /// gone -- an expired story, a file that is trashed or missing -- and
    /// with `Invalid`, in words, for a file that is not something to hear,
    /// watch or read. See the module docs for the rules.
    pub fn describe_content(&self, what: ContentRef) -> Result<ContentFacts> {
        match what {
            ContentRef::Story(id) => {
                let story = self.story(id)?;
                if story.expired {
                    return Err(Error::not_found("story", id));
                }
                let source = self.news_source(story.source_id).map(|s| s.title).unwrap_or_default();
                let present = match story.files.media {
                    Some(file) => self.file_is_present(file)?,
                    None => false,
                };
                // Looked for only where it would be heard: an episode is its
                // enclosure's.
                let audio = match story.files.audio {
                    Some(file) if story.media.as_ref().is_none_or(|m| !m.is_playable()) => {
                        self.present_file(file)?
                    }
                    _ => None,
                };
                Ok(story_facts(&story, &source, present, audio.as_ref()))
            }
            ContentRef::File(id) => file_facts(&self.file(id)?),
            ContentRef::Item(id) => {
                let item = self.item(id)?;
                let shelf = self.kind(item.kind_id).map(|k| k.name).unwrap_or_default();
                Ok(item_facts(&item, &shelf))
            }
        }
    }

    /// An entry for Up Next made from what `what` says about itself.
    pub fn queue_entry_for(&self, what: ContentRef, added_by: AddedBy) -> Result<QueueEntry> {
        Ok(self.describe_content(what)?.queue_entry(added_by))
    }

    /// `file`, if it is in the files folder to be played; `None` when the
    /// catalog does not know it or it is in the trash or missing.
    fn present_file(&self, file: FileId) -> Result<Option<FileEntry>> {
        match self.file(file) {
            Ok(entry) if matches!(entry.status, FileStatus::Present) => Ok(Some(entry)),
            Ok(_) | Err(Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether `file` is in the files folder to be played: the catalog knows
    /// it and it is neither in the trash nor missing.
    pub fn file_is_present(&self, file: FileId) -> Result<bool> {
        match self.file(file) {
            Ok(entry) => Ok(matches!(entry.status, FileStatus::Present)),
            Err(Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{FileStorage, MediaInfo};
    use crate::id::{KindId, NewsSourceId};
    use crate::library::Link;
    use crate::news::{Enclosure, StoryFiles};
    use jiff::Timestamp;

    fn enclosure(url: &str, mime: &str, duration_ms: Option<u64>) -> Enclosure {
        Enclosure {
            url: url.into(),
            mime: mime.into(),
            length: None,
            duration_ms,
            transcript_url: String::new(),
            transcript_mime: String::new(),
            episode: None,
            season: None,
        }
    }

    fn story(title: &str, media: Option<Enclosure>) -> Story {
        let mut story = Story::new(
            NewsSourceId::new(),
            "key",
            title,
            Timestamp::from_second(1_790_000_000).unwrap(),
        );
        story.media = media;
        story
    }

    fn file(name: &str, dir: &str, kind: FileKind, duration_ms: Option<u64>) -> FileEntry {
        let now = Timestamp::from_second(1_790_000_000).unwrap();
        FileEntry {
            id: FileId::new(),
            storage: FileStorage::Plain {
                path: format!("{dir}/{name}").trim_start_matches('/').into(),
            },
            name: name.into(),
            kind,
            mime: String::new(),
            size: 1,
            hash: String::new(),
            modified_at: now,
            added_at: now,
            updated_at: now,
            origin: FileOrigin::Files,
            status: FileStatus::Present,
            media: MediaInfo { duration_ms, ..Default::default() },
            thumbnail: None,
            caption: String::new(),
            tags: Vec::new(),
            rating: None,
            favourite: false,
            hidden: false,
            people: Vec::new(),
            place: None,
            stack: None,
            properties: Default::default(),
        }
    }

    #[test]
    fn an_episode_is_heard_a_video_watched_and_an_article_read() {
        let audio = story_facts(
            &story("Ep 1", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(60_000)))),
            " The Show ",
            false,
            None,
        );
        assert_eq!(audio.mode, ConsumeMode::Listen);
        assert_eq!(audio.source, "The Show");
        assert_eq!(audio.duration_ms, Some(60_000));
        assert!(audio.download.is_some(), "an episode with no file has to be downloaded");

        let video = story_facts(
            &story("Ep 2", Some(enclosure("https://x/a.mp4", "", None))),
            "Show",
            false,
            None,
        );
        assert_eq!(video.mode, ConsumeMode::Watch);

        let article = story_facts(&story("Post", None), "Blog", false, None);
        assert_eq!(article.mode, ConsumeMode::Read);
        assert_eq!(article.download, None);

        // A PDF an episode attached is not something to play.
        let pdf = story_facts(
            &story("Notes", Some(enclosure("https://x/n.pdf", "application/pdf", None))),
            "Show",
            false,
            None,
        );
        assert_eq!(pdf.mode, ConsumeMode::Read);
        assert_eq!(pdf.download, None);
    }

    #[test]
    fn an_episode_is_downloaded_only_while_its_file_is_there() {
        let mut s = story("Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", None)));
        let linked = FileId::new();
        s.files = StoryFiles { media: Some(linked), ..Default::default() };

        let there = story_facts(&s, "Show", true, None);
        assert_eq!(there.download, None);
        assert_eq!(there.media_file, Some(linked));

        // Linked, but the file was trashed or went missing: not downloaded,
        // and no recording to point at.
        let gone = story_facts(&s, "Show", false, None);
        assert_eq!(gone.download, Some(s.id));
        assert_eq!(gone.media_file, None);
    }

    #[test]
    fn a_story_keeps_its_words_and_is_called_by_its_address_when_it_has_no_title() {
        let mut s = story("  ", None);
        s.url = "https://example.com/post".into();
        s.files.transcript = Some(FileId::new());
        let facts = story_facts(&s, "", true, None);
        assert_eq!(facts.title, "https://example.com/post");
        assert_eq!(facts.url, "https://example.com/post");
        assert_eq!(facts.text_file, s.files.transcript);
        s.url.clear();
        assert_eq!(story_facts(&s, "", true, None).title, "Untitled story");
        // The article kept as Markdown is its words, before the transcript.
        let kept = FileId::new();
        s.files.text = Some(kept);
        assert_eq!(story_facts(&s, "", true, None).text_file, Some(kept));
    }

    #[test]
    fn a_file_is_named_without_its_extension_and_filed_under_its_folder() {
        let f = file("Standup 12.m4a", "Feed/Podcasts/Hard Fork", FileKind::Audio, Some(90_000));
        let facts = file_facts(&f).unwrap();
        assert_eq!(facts.title, "Standup 12");
        assert_eq!(facts.source, "Hard Fork");
        assert_eq!(facts.mode, ConsumeMode::Listen);
        assert_eq!(facts.duration_ms, Some(90_000));
        assert_eq!(facts.media_file, Some(f.id));
        assert_eq!(facts.text_file, None);

        assert_eq!(
            file_facts(&file("Film.mkv", "", FileKind::Video, None)).unwrap().mode,
            ConsumeMode::Watch
        );
        let paper = file("Paper.pdf", "Reading", FileKind::Document, None);
        let doc = file_facts(&paper).unwrap();
        assert_eq!((doc.mode, doc.source.as_str()), (ConsumeMode::Read, "Reading"));
        assert_eq!((doc.media_file, doc.text_file), (None, Some(paper.id)));

        assert_eq!(file_title(&file(".profile", "", FileKind::Other, None)), ".profile");
        assert_eq!(file_title(&file("README", "", FileKind::Other, None)), "README");
        assert_eq!(folder_name(&file("a.mp3", "", FileKind::Audio, None)), "Files");
    }

    #[test]
    fn a_file_that_is_not_to_hear_watch_or_read_is_refused_in_words() {
        for kind in [FileKind::Image, FileKind::Other] {
            let err = file_facts(&file("a.bin", "", kind, None)).unwrap_err().to_string();
            assert!(err.contains("hear, watch or read"), "{err}");
        }
        let mut labs = file("labs.pdf", "", FileKind::Document, None);
        labs.origin = FileOrigin::Health;
        let err = file_facts(&labs).unwrap_err().to_string();
        assert!(err.contains("Health's"), "{err}");

        let mut trashed = file("a.mp3", "", FileKind::Audio, None);
        trashed.status = FileStatus::Trashed {
            at: Timestamp::from_second(1_790_000_000).unwrap(),
            from: "a.mp3".into(),
        };
        assert!(matches!(file_facts(&trashed), Err(Error::NotFound { .. })));
        let mut missing = file("a.mp3", "", FileKind::Audio, None);
        missing.status =
            FileStatus::Missing { since: Timestamp::from_second(1_790_000_000).unwrap() };
        assert!(matches!(file_facts(&missing), Err(Error::NotFound { .. })));
    }

    #[test]
    fn an_item_is_read_from_its_shelf_by_its_link_or_its_address_fact() {
        let mut item = Item::new(KindId::new(), "  Piranesi ");
        let plain = item_facts(&item, " Books ");
        assert_eq!((plain.title.as_str(), plain.source.as_str()), ("Piranesi", "Books"));
        assert_eq!(plain.mode, ConsumeMode::Read);
        assert_eq!(plain.url, "");

        item.facts.insert("url".into(), "https://example.com/essay".into());
        assert_eq!(item_facts(&item, "Articles").url, "https://example.com/essay");
        item.links.push(Link { label: "Site".into(), url: "https://example.com/home".into() });
        assert_eq!(item_facts(&item, "Articles").url, "https://example.com/home", "a link wins");
        assert_eq!(item_facts(&item, "Articles").what, ContentRef::Item(item.id));
    }

    #[test]
    fn an_entry_carries_the_snapshot_and_who_added_it() {
        let facts = story_facts(
            &story("Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(5_000)))),
            "Show",
            false,
            None,
        );
        let entry = facts.queue_entry(AddedBy::Assistant);
        assert_eq!(entry.what, facts.what);
        assert_eq!((entry.title.as_str(), entry.source.as_str()), ("Ep", "Show"));
        assert_eq!(entry.mode, ConsumeMode::Listen);
        assert_eq!(entry.duration_ms, Some(5_000));
        assert_eq!(entry.added_by, AddedBy::Assistant);
    }

    #[test]
    fn an_article_with_audio_of_its_own_is_heard_from_it() {
        let mut article = story("Post", None);
        let audio = file("2026-03-09 Post.opus", "Feed/Audio/Blog", FileKind::Audio, Some(95_000));
        article.files.audio = Some(audio.id);
        let facts = story_facts(&article, "Blog", false, Some(&audio));
        assert_eq!(facts.mode, ConsumeMode::Listen);
        assert_eq!(facts.duration_ms, Some(95_000), "as long as the file says");
        assert_eq!(facts.media_file, Some(audio.id));
        assert_eq!(facts.download, None, "it is made, not fetched");
        assert_eq!(facts.queue_entry(AddedBy::Person).duration_ms, Some(95_000));

        // The file went missing or into the trash: an article, to read.
        let mut trashed = audio.clone();
        trashed.status = FileStatus::Trashed {
            at: Timestamp::from_second(1_790_000_000).unwrap(),
            from: "Post.opus".into(),
        };
        let facts = story_facts(&article, "Blog", false, Some(&trashed));
        assert_eq!((facts.mode, facts.media_file), (ConsumeMode::Read, None));
        assert_eq!(story_facts(&article, "Blog", false, None).mode, ConsumeMode::Read);

        // An episode is its enclosure's, whatever else was made of it.
        let mut episode =
            story("Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(60_000))));
        episode.files.audio = Some(audio.id);
        let facts = story_facts(&episode, "Show", false, Some(&audio));
        assert_eq!(facts.duration_ms, Some(60_000));
        assert_eq!(facts.media_file, None);
        assert_eq!(facts.download, Some(episode.id));
    }
}
