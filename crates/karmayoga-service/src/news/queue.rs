//! Up Next as the interface draws it: each entry with what it stands for.
//! See `docs/plans/podcasts.md`.
//!
//! The store keeps an entry as a snapshot -- a title, where it is from, how
//! long it is -- so that a list can be drawn without opening anything. What
//! goes in the snapshot, and what may go in Up Next at all, is decided once,
//! in the vault ([`Vault::describe_content`]); this module is where the entry
//! is placed ([`add`]), where it is read back with the thing it stands for
//! beside it ([`view`]), and where Up Next keeps itself as long as the person
//! asked ([`autofill`]).
//!
//! # Who announces what
//!
//! [`put`] writes and says nothing: the `add_to_queue` command declares its
//! own change, stamped with the window that asked, so that window is not told
//! about its own write. [`add`] is the same write for a caller with no
//! command around it (a show putting a new episode in Up Next, the daily
//! podcast) and announces the entry itself.

use crate::error::{CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::news::{downloads, rank};
use crate::service::{Service, blocking};
use karmayoga_core::id::{FileId, NewsSourceId, QueueEntryId, StoryId};
use karmayoga_core::library::Item;
use karmayoga_core::news::{
    AddedBy, Consumption, ContentRef, EnclosureKind, NewsSource, QueueEntry, Story,
};
use karmayoga_core::vault::upnext_entry::folder_name;
use karmayoga_core::{Error, FileEntry, FileKind, FileStatus, QueuePlace, Vault};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// What the player plays for an entry. Absent for an article, which is heard
/// through Listen or read.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playable {
    pub file_id: FileId,
    pub kind: EnclosureKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Where to start: where it was left, past a show's intro.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
}

/// One entry and what it stands for. Any of the three may be gone.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub entry: QueueEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story: Option<Story>,
    /// The story's source's title, the file's folder, the item's shelf.
    pub source_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Item>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumption: Option<Consumption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play: Option<Playable>,
    /// Its episode is downloading or waiting to.
    pub downloading: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    pub items: Vec<QueueItem>,
}

// ---- reading ---------------------------------------------------------------

/// Up Next, in order, with what each entry stands for.
pub async fn view(svc: &Arc<Service>) -> CommandResult<QueueView> {
    let vault = svc.require()?;
    // Waiting counts as downloading: either way the episode is on its way and
    // the interface says so. A failed one is not, or "downloading" would
    // never go away.
    let downloading: HashSet<StoryId> = downloads::status(svc)
        .into_iter()
        .filter(|d| d.state != downloads::DownloadState::Failed)
        .map(|d| d.story_id)
        .collect();
    blocking(move || build(&vault, &downloading)).await
}

/// One read of everything an entry may stand for, so the list costs a handful
/// of queries however long it is.
fn build(vault: &Vault, downloading: &HashSet<StoryId>) -> CommandResult<QueueView> {
    let entries = vault.queue()?;
    let (mut story_ids, mut file_ids, mut item_ids) = (Vec::new(), Vec::new(), Vec::new());
    for entry in &entries {
        match entry.what {
            ContentRef::Story(id) => story_ids.push(id),
            ContentRef::File(id) => file_ids.push(id),
            ContentRef::Item(id) => item_ids.push(id),
        }
    }

    let stories: HashMap<StoryId, Story> = if story_ids.is_empty() {
        HashMap::new()
    } else {
        vault.stories_by_id(&story_ids)?.into_iter().map(|s| (s.id, s)).collect()
    };
    let sources: HashMap<NewsSourceId, NewsSource> = if stories.is_empty() {
        HashMap::new()
    } else {
        vault.news_sources()?.into_iter().map(|s| (s.id, s)).collect()
    };
    // The files of Up Next's own, and the episodes its stories link to: an
    // episode whose file was trashed or went missing is not playable.
    file_ids.extend(stories.values().filter_map(|s| s.files.media));
    let mut files = HashMap::new();
    for id in file_ids {
        match vault.file(id) {
            Ok(file) => {
                files.insert(id, file);
            }
            // Deleted from the folder since it was queued: the entry's own
            // snapshot is what is left to draw.
            Err(Error::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut items = HashMap::new();
    for id in item_ids {
        match vault.item(id) {
            Ok(item) => {
                items.insert(id, item);
            }
            Err(Error::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let shelves: HashMap<_, _> = if items.is_empty() {
        HashMap::new()
    } else {
        vault.kinds()?.into_iter().map(|k| (k.id, k.name)).collect()
    };
    let whats: Vec<ContentRef> = entries.iter().map(|e| e.what).collect();
    let consumptions: HashMap<ContentRef, Consumption> = if whats.is_empty() {
        HashMap::new()
    } else {
        vault.consumptions_for(&whats)?.into_iter().map(|c| (c.what, c)).collect()
    };

    let items = entries
        .into_iter()
        .map(|entry| {
            let consumption = consumptions.get(&entry.what).cloned();
            let mut row = QueueItem {
                source_title: entry.source.clone(),
                story: None,
                file: None,
                item: None,
                play: None,
                downloading: false,
                consumption,
                entry,
            };
            match row.entry.what {
                ContentRef::Story(id) => {
                    if let Some(story) = stories.get(&id) {
                        let source = sources.get(&story.source_id);
                        if let Some(source) = source {
                            row.source_title = source.title.clone();
                        }
                        let media = story.files.media.and_then(|id| files.get(&id));
                        row.play = story_playable(story, source, media, row.consumption.as_ref());
                        row.downloading = downloading.contains(&id);
                        row.story = Some(story.clone());
                    }
                }
                ContentRef::File(id) => {
                    if let Some(file) = files.get(&id) {
                        row.source_title = folder_name(file);
                        row.play = file_playable(file, row.consumption.as_ref());
                        row.file = Some(file.clone());
                    }
                }
                ContentRef::Item(id) => {
                    if let Some(item) = items.get(&id) {
                        if let Some(shelf) = shelves.get(&item.kind_id) {
                            row.source_title = shelf.clone();
                        }
                        row.item = Some(item.clone());
                    }
                }
            }
            row
        })
        .collect();
    Ok(QueueView { items })
}

/// What the player plays for an episode: the downloaded file -- `media` is
/// what the story links to, and counts only while it is in the folder -- from
/// where it was left, or past the show's intro when it has not been heard (or
/// has been heard to the end, and is being heard again).
fn story_playable(
    story: &Story,
    source: Option<&NewsSource>,
    media: Option<&FileEntry>,
    consumption: Option<&Consumption>,
) -> Option<Playable> {
    let enclosure = story.media.as_ref().filter(|m| m.is_playable())?;
    let file_id = media.filter(|f| matches!(f.status, FileStatus::Present))?.id;
    let intro = source.and_then(|s| s.podcast.as_ref()).and_then(|p| p.skip_intro_s);
    Some(Playable {
        file_id,
        kind: enclosure.kind(),
        duration_ms: consumption.and_then(|c| c.duration_ms).or(enclosure.duration_ms),
        start_ms: start_at(consumption, intro),
    })
}

/// What the player plays for a file of its own: any audio or video that is
/// still in the folder.
fn file_playable(file: &FileEntry, consumption: Option<&Consumption>) -> Option<Playable> {
    if !matches!(file.status, FileStatus::Present) {
        return None;
    }
    let kind = match file.kind {
        FileKind::Audio => EnclosureKind::Audio,
        FileKind::Video => EnclosureKind::Video,
        _ => return None,
    };
    Some(Playable {
        file_id: file.id,
        kind,
        duration_ms: consumption.and_then(|c| c.duration_ms).or(file.media.duration_ms),
        start_ms: start_at(consumption, None),
    })
}

/// Where playing starts: where it was left, unless it was finished -- heard
/// to the end is heard again from the top -- else past the show's intro.
fn start_at(consumption: Option<&Consumption>, intro_s: Option<u32>) -> Option<u64> {
    match consumption {
        Some(c) if !c.finished && c.position_ms.is_some_and(|p| p > 0) => c.position_ms,
        _ => intro_s.filter(|s| *s > 0).map(|s| u64::from(s) * 1000),
    }
}

// ---- adding ----------------------------------------------------------------

/// An entry as it was stored, and the episode it still needs downloaded.
struct Placed {
    entry: QueueEntry,
    download: Option<StoryId>,
}

/// Make the entry for `what` and store it at `place`. Runs on a blocking
/// thread.
fn place(
    vault: &Vault,
    what: ContentRef,
    at: QueuePlace,
    added_by: AddedBy,
) -> CommandResult<Placed> {
    let facts = vault.describe_content(what)?;
    let entry = vault.add_to_queue(facts.queue_entry(added_by), at)?;
    Ok(Placed { entry, download: facts.download })
}

/// Ask for an episode, if there is one to ask for. Never fails the add: the
/// thing is in Up Next either way, and the downloader works through what is
/// missing on its own.
fn download_for(svc: &Arc<Service>, placed: &Placed) {
    if let Some(story) = placed.download
        && let Err(e) = downloads::request(svc, story)
    {
        tracing::info!(error = %e, "could not ask for a downloaded episode for Up Next");
    }
}

/// Put `what` in Up Next at `place`, and say nothing: for the
/// `add_to_queue` command, whose declared change is the announcement.
pub(crate) async fn put(
    svc: &Arc<Service>,
    what: ContentRef,
    at: QueuePlace,
    added_by: AddedBy,
) -> CommandResult<QueueEntry> {
    let vault = svc.require()?;
    let placed = blocking(move || place(&vault, what, at, added_by)).await?;
    // Recommended offers only what is not queued, so it must not go on
    // showing what was just put here.
    rank::forget_window(svc);
    download_for(svc, &placed);
    Ok(placed.entry)
}

/// Put `what` in Up Next at `place`: its title, source, mode and length
/// filled from what it is, and an episode not yet downloaded asked for.
/// Announces the entry itself -- see the module docs.
pub async fn add(
    svc: &Arc<Service>,
    what: ContentRef,
    place: QueuePlace,
    added_by: AddedBy,
) -> CommandResult<QueueEntry> {
    let entry = put(svc, what, place, added_by).await?;
    changed(svc, Op::Created, &[entry.id]);
    Ok(entry)
}

/// Raise one change for what happened to `entries`, or the list as a whole
/// when none are named.
pub(crate) fn changed(svc: &Service, op: Op, entries: &[QueueEntryId]) {
    let mut change = Change::new(Kind::Queue, op);
    match entries {
        [] => {}
        [one] => change.id = Some(one.to_string()),
        many => change.ids = many.iter().map(ToString::to_string).collect(),
    }
    svc.events().changed(change);
}

/// Ask for the episode of every story in Up Next that has none in the folder
/// -- never downloaded, or its file since trashed or gone -- for what put
/// something there without going through [`add`]: the assistant's tool, which
/// can only write the vault.
pub async fn request_missing(svc: &Arc<Service>) -> CommandResult<u32> {
    let vault = svc.require()?;
    let missing = blocking(move || {
        let mut missing = Vec::new();
        for entry in vault.queue()? {
            let ContentRef::Story(_) = entry.what else { continue };
            match vault.describe_content(entry.what) {
                Ok(facts) => missing.extend(facts.download),
                // Expired since it was queued: nothing to ask for.
                Err(Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(missing)
    })
    .await?;
    let mut asked = 0;
    for story in missing {
        match downloads::request(svc, story) {
            Ok(()) => asked += 1,
            Err(e) => tracing::info!(error = %e, "could not ask for a downloaded episode"),
        }
    }
    Ok(asked)
}

// ---- keeping it full ---------------------------------------------------------

/// Top Up Next up from Recommended, as `FeedSettings::autofill` asks.
/// Answers how many were added.
///
/// Asks Recommended for twice as many as are missing, because some of what it
/// offers will already be queued (the ranker may not have noticed yet) and
/// the shortfall is filled from what is left, best first.
pub async fn autofill(svc: &Arc<Service>) -> CommandResult<u32> {
    let vault = svc.require()?;
    let (want, queued) = {
        let vault = vault.clone();
        blocking(move || {
            let want = vault.feed_settings()?.autofill.unwrap_or(0) as usize;
            if want == 0 {
                return Ok((0, HashSet::new()));
            }
            let queued: HashSet<ContentRef> = vault.queue()?.into_iter().map(|e| e.what).collect();
            Ok((want.saturating_sub(queued.len()), queued))
        })
        .await?
    };
    if want == 0 {
        return Ok(0);
    }

    let offered = rank::recommended(svc, (want * 2) as u32).await?;
    let chosen: Vec<StoryId> = offered
        .iter()
        .filter(|r| !r.queued && !queued.contains(&ContentRef::Story(r.story.id)))
        .map(|r| r.story.id)
        .take(want)
        .collect();
    let mut added = Vec::new();
    for id in chosen {
        match put(svc, ContentRef::Story(id), QueuePlace::End, AddedBy::Recommended).await {
            Ok(entry) => added.push(entry.id),
            // Expired or deleted between being recommended and being queued:
            // the next one in line is not worse.
            Err(e) if e.code == codes::NOT_FOUND => {}
            Err(e) => return Err(e),
        }
    }
    if !added.is_empty() {
        // One change for the whole top-up, with no id: what a listener does
        // about it is reload Up Next.
        changed(svc, Op::Created, &[]);
    }
    Ok(added.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use karmayoga_core::news::{ConsumeMode, Enclosure, PodcastSettings, SourceKind};
    use karmayoga_core::{FileOrigin, FileStorage, MediaInfo};

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

    fn consumption(position: Option<u64>, finished: bool) -> Consumption {
        let mut c =
            Consumption::new(ContentRef::Story(StoryId::new()), "Episode", ConsumeMode::Listen);
        c.position_ms = position;
        c.finished = finished;
        c
    }

    #[test]
    fn playing_starts_where_it_was_left_unless_it_was_finished() {
        assert_eq!(start_at(Some(&consumption(Some(61_000), false)), Some(30)), Some(61_000));
        // Heard to the end: again from the top, past the intro.
        assert_eq!(start_at(Some(&consumption(Some(3_000_000), true)), Some(30)), Some(30_000));
        assert_eq!(start_at(Some(&consumption(None, false)), Some(30)), Some(30_000));
        assert_eq!(start_at(None, None), None);
        assert_eq!(start_at(Some(&consumption(Some(0), false)), Some(0)), None);
    }

    #[test]
    fn an_episode_is_playable_only_while_its_file_is_in_the_folder() {
        let mut s = story("Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(60_000))));
        let mut show = NewsSource::new(SourceKind::Web { url: "https://x/feed".into() }, "Show");
        show.podcast = Some(PodcastSettings { skip_intro_s: Some(20), ..Default::default() });

        // Nothing linked yet.
        assert_eq!(story_playable(&s, Some(&show), None, None), None);

        let linked = file("ep.mp3", "Feed/Podcasts/Show", FileKind::Audio, None);
        s.files.media = Some(linked.id);
        let play = story_playable(&s, Some(&show), Some(&linked), None).unwrap();
        assert_eq!(play.file_id, linked.id);
        assert_eq!(play.kind, EnclosureKind::Audio);
        assert_eq!(play.duration_ms, Some(60_000));
        assert_eq!(play.start_ms, Some(20_000));

        // What the player measured beats what the feed said.
        let mut c = consumption(Some(5_000), false);
        c.duration_ms = Some(61_500);
        let play = story_playable(&s, Some(&show), Some(&linked), Some(&c)).unwrap();
        assert_eq!(play.duration_ms, Some(61_500));
        assert_eq!(play.start_ms, Some(5_000));

        // Linked, but the file was trashed, or went missing, or is not known
        // to the catalog at all: there is nothing to play.
        let at = Timestamp::from_second(1_790_000_000).unwrap();
        let mut trashed = linked.clone();
        trashed.status = FileStatus::Trashed { at, from: "ep.mp3".into() };
        assert_eq!(story_playable(&s, Some(&show), Some(&trashed), None), None);
        let mut missing = linked.clone();
        missing.status = FileStatus::Missing { since: at };
        assert_eq!(story_playable(&s, Some(&show), Some(&missing), None), None);
        assert_eq!(story_playable(&s, Some(&show), None, None), None);
    }

    #[test]
    fn a_file_plays_when_it_is_audio_or_video_and_still_there() {
        let audio = file("a.mp3", "", FileKind::Audio, Some(1_000));
        assert_eq!(file_playable(&audio, None).unwrap().kind, EnclosureKind::Audio);
        assert_eq!(
            file_playable(&file("a.mp4", "", FileKind::Video, None), None).unwrap().kind,
            EnclosureKind::Video
        );
        assert_eq!(file_playable(&file("a.pdf", "", FileKind::Document, None), None), None);
        let mut gone = file("a.mp3", "", FileKind::Audio, None);
        gone.status = FileStatus::Missing { since: Timestamp::from_second(1_790_000_000).unwrap() };
        assert_eq!(file_playable(&gone, None), None);
    }

    // ---- against a real vault --------------------------------------------------

    use crate::events::EventSink;
    use karmayoga_core::files::ImportOptions;
    use karmayoga_core::news::FeedSettings;
    use std::sync::Mutex;

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

    struct World {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        events: Arc<Collector>,
        dir: tempfile::TempDir,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(
            dir.path(),
            karmayoga_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        let events = Arc::new(Collector::default());
        svc.set_events(events.clone());
        World { svc, vault, events, dir }
    }

    impl World {
        fn show(&self, title: &str) -> NewsSource {
            let mut show = NewsSource::new(SourceKind::Web { url: "https://x/feed".into() }, title);
            show.podcast = Some(PodcastSettings { skip_intro_s: Some(20), ..Default::default() });
            self.vault.save_news_source(&show).unwrap();
            show
        }

        fn story(&self, show: &NewsSource, title: &str, media: Option<Enclosure>) -> Story {
            let key = self.vault.news_key(&format!("{}/{title}", show.id)).unwrap();
            let mut story = Story::new(show.id, key, title, Timestamp::now());
            story.media = media;
            self.vault.insert_stories(&[(story.clone(), None)]).unwrap();
            story
        }

        fn file(&self, name: &str, folder: &str) -> FileEntry {
            if self.vault.files_settings().unwrap().folder.is_none() {
                self.vault.set_up_files_folder(Some(&self.dir.path().join("Files"))).unwrap();
            }
            let options =
                ImportOptions { dir: Some(folder.into()), ..ImportOptions::new(FileOrigin::Files) };
            self.vault.import_bytes(b"bytes", name, &options).unwrap()
        }
    }

    #[tokio::test]
    async fn an_entry_is_made_from_its_story_and_asks_for_the_episode() {
        let w = world();
        let show = w.show("Hard Fork");
        let ep =
            w.story(&show, "Ep 1", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(60_000))));

        let entry =
            add(&w.svc, ContentRef::Story(ep.id), QueuePlace::End, AddedBy::Person).await.unwrap();
        assert_eq!(
            (entry.title.as_str(), entry.source.as_str(), entry.mode),
            ("Ep 1", "Hard Fork", ConsumeMode::Listen)
        );
        assert_eq!(entry.duration_ms, Some(60_000));
        assert_eq!(entry.added_by, AddedBy::Person);
        assert_eq!(w.vault.queue().unwrap(), vec![entry.clone()]);

        let changes = w.events.take();
        assert_eq!(changes.len(), 1);
        assert_eq!((changes[0].kind, changes[0].op), (Kind::Queue, Op::Created));
        assert_eq!(changes[0].id.as_deref(), Some(entry.id.to_string().as_str()));

        // On the way, and not yet playable.
        let view = view(&w.svc).await.unwrap();
        assert_eq!(view.items.len(), 1);
        let item = &view.items[0];
        assert_eq!(item.entry, entry);
        assert_eq!(item.story.as_ref().map(|s| s.id), Some(ep.id));
        assert_eq!(item.source_title, "Hard Fork");
        assert!(item.downloading, "the download was asked for");
        assert_eq!(item.play, None);

        // `put` is the same write, quietly: the command announces it.
        let article = w.story(&show, "Notes", None);
        put(&w.svc, ContentRef::Story(article.id), QueuePlace::Next, AddedBy::Show).await.unwrap();
        assert!(w.events.take().is_empty());
        let order: Vec<String> = w.vault.queue().unwrap().into_iter().map(|e| e.title).collect();
        assert_eq!(order, ["Notes", "Ep 1"]);
    }

    #[tokio::test]
    async fn a_downloaded_episode_is_playable_from_where_it_was_left() {
        let w = world();
        let show = w.show("Show");
        let ep =
            w.story(&show, "Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(60_000))));
        let file = w.file("ep.mp3", "Feed/Podcasts/Show");
        w.vault.update_story_files(ep.id, |f| f.media = Some(file.id)).unwrap();
        add(&w.svc, ContentRef::Story(ep.id), QueuePlace::End, AddedBy::Person).await.unwrap();

        let play = view(&w.svc).await.unwrap().items[0].play.clone().unwrap();
        assert_eq!(play.file_id, file.id);
        assert_eq!(play.kind, EnclosureKind::Audio);
        assert_eq!(play.start_ms, Some(20_000), "past the show's intro");

        let mut left = Consumption::new(ContentRef::Story(ep.id), "Ep", ConsumeMode::Listen);
        left.position_ms = Some(31_000);
        w.vault.save_consumption(&left).unwrap();
        let item = view(&w.svc).await.unwrap().items.remove(0);
        assert_eq!(item.play.unwrap().start_ms, Some(31_000));
        assert_eq!(item.consumption.unwrap().position_ms, Some(31_000));
        assert!(!item.downloading);
    }

    #[tokio::test]
    async fn an_episode_whose_file_was_trashed_is_not_playable_and_is_asked_for_again() {
        let w = world();
        let show = w.show("Show");
        let ep =
            w.story(&show, "Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", Some(60_000))));
        let file = w.file("ep.mp3", "Feed/Podcasts/Show");
        w.vault.update_story_files(ep.id, |f| f.media = Some(file.id)).unwrap();

        // In the folder: nothing to ask for, and queued it plays.
        add(&w.svc, ContentRef::Story(ep.id), QueuePlace::End, AddedBy::Person).await.unwrap();
        assert_eq!(request_missing(&w.svc).await.unwrap(), 0);
        assert!(view(&w.svc).await.unwrap().items[0].play.is_some());

        // Trashed by hand: the story still links to it, but it is not there.
        w.vault.trash_files(&[file.id]).unwrap();
        assert!(w.vault.story(ep.id).unwrap().files.media.is_some(), "the link outlives the file");
        let item = view(&w.svc).await.unwrap().items.remove(0);
        assert_eq!(item.play, None, "nothing to play");
        assert!(!item.downloading, "and nobody has asked yet");

        assert_eq!(request_missing(&w.svc).await.unwrap(), 1, "so it is asked for again");
        assert!(view(&w.svc).await.unwrap().items[0].downloading);

        // Put in Up Next again, it is asked for too.
        w.vault.remove_from_queue_for(&[ContentRef::Story(ep.id)]).unwrap();
        svc_cancel(&w, ep.id);
        add(&w.svc, ContentRef::Story(ep.id), QueuePlace::End, AddedBy::Person).await.unwrap();
        assert!(view(&w.svc).await.unwrap().items[0].downloading);
    }

    fn svc_cancel(w: &World, story: StoryId) {
        downloads::cancel(&w.svc, story);
    }

    #[tokio::test]
    async fn a_file_and_an_item_are_made_into_entries_and_a_gone_one_is_refused() {
        let w = world();
        w.vault.seed_library().unwrap();
        let talk = w.file("Keynote 2025.m4a", "Talks");
        let shelf = w.vault.kinds().unwrap().into_iter().find(|k| k.slug == "book").unwrap();
        let book = Item::new(shelf.id, "Piranesi");
        w.vault.save_item(&book).unwrap();

        let a =
            add(&w.svc, ContentRef::File(talk.id), QueuePlace::End, AddedBy::Person).await.unwrap();
        assert_eq!((a.title.as_str(), a.source.as_str()), ("Keynote 2025", "Talks"));
        let b =
            add(&w.svc, ContentRef::Item(book.id), QueuePlace::End, AddedBy::Person).await.unwrap();
        assert_eq!(
            (b.title.as_str(), b.source.as_str(), b.mode),
            ("Piranesi", shelf.name.as_str(), ConsumeMode::Read)
        );

        let items = view(&w.svc).await.unwrap().items;
        assert_eq!(items[0].file.as_ref().map(|f| f.id), Some(talk.id));
        assert_eq!(items[0].source_title, "Talks");
        assert_eq!(items[0].play.as_ref().map(|p| p.kind), Some(EnclosureKind::Audio));
        assert_eq!(items[1].item.as_ref().map(|i| i.id), Some(book.id));
        assert_eq!(items[1].source_title, shelf.name);
        assert_eq!(items[1].play, None);

        // The item is deleted: its entry still draws from its snapshot.
        w.vault.delete_item(book.id).unwrap();
        let items = view(&w.svc).await.unwrap().items;
        assert_eq!(items[1].item, None);
        assert_eq!(items[1].entry.title, "Piranesi");

        // A picture is not something to hear, watch or read; one of Health's is
        // not the Feed's to play at all.
        let photo = w.file("Beach.jpg", "Pictures");
        let err = add(&w.svc, ContentRef::File(photo.id), QueuePlace::End, AddedBy::Person)
            .await
            .unwrap_err();
        assert!(err.message.contains("hear, watch or read"), "{err}");
        let sealed =
            ImportOptions { encrypted: Some(true), ..ImportOptions::new(FileOrigin::Health) };
        let labs = w.vault.import_bytes(b"TSH", "labs.mp3", &sealed).unwrap();
        let err = add(&w.svc, ContentRef::File(labs.id), QueuePlace::End, AddedBy::Person)
            .await
            .unwrap_err();
        assert!(err.message.contains("Health"), "{err}");

        let gone = add(&w.svc, ContentRef::Story(StoryId::new()), QueuePlace::End, AddedBy::Person)
            .await
            .unwrap_err();
        assert_eq!(gone.code, codes::NOT_FOUND);
        assert_eq!(w.vault.queue().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn up_next_is_topped_up_to_the_length_asked_for_from_what_is_not_queued() {
        let w = world();
        let show = w.show("Blog");
        let stories: Vec<Story> =
            (0..5).map(|n| w.story(&show, &format!("Post {n}"), None)).collect();

        // Not asked for: nothing happens.
        assert_eq!(autofill(&w.svc).await.unwrap(), 0);

        let settings = FeedSettings { autofill: Some(3), ..Default::default() };
        w.vault.save_feed_settings(&settings).unwrap();
        put(&w.svc, ContentRef::Story(stories[0].id), QueuePlace::End, AddedBy::Person)
            .await
            .unwrap();
        w.events.take();

        assert_eq!(autofill(&w.svc).await.unwrap(), 2, "one is there already");
        let queued = w.vault.queue().unwrap();
        assert_eq!(queued.len(), 3);
        assert_eq!(queued[0].added_by, AddedBy::Person);
        assert!(queued[1..].iter().all(|e| e.added_by == AddedBy::Recommended));
        let whats: std::collections::HashSet<_> = queued.iter().map(|e| e.what).collect();
        assert_eq!(whats.len(), 3, "nothing twice");
        let changes = w.events.take();
        assert_eq!(changes.len(), 1, "one change for the whole top-up");
        assert_eq!((changes[0].kind, changes[0].id.clone()), (Kind::Queue, None));

        // Full: nothing more.
        assert_eq!(autofill(&w.svc).await.unwrap(), 0);
        assert!(w.events.take().is_empty());
    }

    #[tokio::test]
    async fn episodes_in_up_next_without_a_file_are_asked_for_again_on_request() {
        let w = world();
        let show = w.show("Show");
        let ep = w.story(&show, "Ep", Some(enclosure("https://x/a.mp3", "audio/mpeg", None)));
        let article = w.story(&show, "Post", None);
        for id in [ep.id, article.id] {
            // The vault directly: the way the assistant's tool puts things there.
            let mut entry = QueueEntry::new(ContentRef::Story(id), "x", 0);
            entry.mode = ConsumeMode::Listen;
            w.vault.add_to_queue(entry, QueuePlace::End).unwrap();
        }
        assert_eq!(request_missing(&w.svc).await.unwrap(), 1, "only the episode needs a file");
        assert!(view(&w.svc).await.unwrap().items.iter().any(|i| i.downloading));
    }
}
