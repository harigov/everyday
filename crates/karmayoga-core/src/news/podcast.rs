//! Podcasts: a web feed whose stories carry something to hear or watch.
//!
//! A podcast is not a kind of source of its own. It is a
//! [`SourceKind::Web`](super::SourceKind::Web) feed like any other, whose
//! items each name a file -- an `<enclosure>` -- and whose
//! [`NewsSource::podcast`](super::NewsSource::podcast) is set: when it was
//! followed from the podcast directory, or when its feed turned out to carry
//! audio or video. Everything the refresher, OPML and the sidebar already do
//! for a feed they do for a show, and a story that carries an episode is a
//! story with [`Story::media`](super::Story::media) set.
//!
//! # Where an episode lives
//!
//! Downloaded into the files folder, under `Feed/Podcasts/<show>/`, as a
//! file like any other ([`crate::files::FileOrigin::Feed`]), so whatever
//! Files can do -- play it, back it up, sync it, index its transcript -- it
//! does for an episode. The story points at it through
//! [`Story::files`](super::Story::files), the way a library item points at
//! its book.
//!
//! # Who decides what is downloaded and kept
//!
//! [`PodcastDefaults`] in [`super::FeedSettings`] say it for every show, and
//! each show's [`PodcastSettings`] may say otherwise; [`PodcastPolicy`] is
//! the two put together. Clean-up only ever moves to the files trash an
//! episode the Feed itself downloaded, never one somebody starred or that a
//! saved story holds, and never a transcript or an article: those are the
//! Feed's memory of what was heard.

use crate::id::FileId;
use serde::{Deserialize, Serialize};

/// The folder every file the Feed keeps goes under, relative to the files
/// folder.
pub const FEED_DIR: &str = "Feed";
/// Where episodes go: `Feed/Podcasts/<show>/`.
pub const PODCASTS_DIR: &str = "Feed/Podcasts";
/// Where articles kept as Markdown go: `Feed/Articles/<source>/`.
pub const ARTICLES_DIR: &str = "Feed/Articles";
/// Where articles turned into audio go.
pub const AUDIO_DIR: &str = "Feed/Audio";
/// Where the daily podcast goes.
pub const DAILY_DIR: &str = "Feed/Daily";
/// Where a story's summary is kept, by source: `Feed/Summaries/<source>/`.
pub const SUMMARIES_DIR: &str = "Feed/Summaries";

/// The file a story carries: a podcast episode's audio or video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Enclosure {
    /// Absolute. Fetched only by the downloader, over the public client.
    pub url: String,
    /// As the feed said it, lower-cased: `audio/mpeg`, `video/mp4`. Empty when
    /// it said nothing, and then [`Enclosure::kind`] goes by the extension.
    #[serde(default)]
    pub mime: String,
    /// Bytes, as the feed said. Often wrong, sometimes zero; a hint for the
    /// progress bar and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// `itunes:duration`, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// `podcast:transcript`: the publisher's own transcript, the best of the
    /// ones it offered (WebVTT, then SRT, then JSON, then HTML, then text).
    /// Empty when it offered none.
    #[serde(default)]
    pub transcript_url: String,
    /// That transcript's type, as the feed said it.
    #[serde(default)]
    pub transcript_mime: String,
    /// `itunes:episode` and `itunes:season`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
}

impl Enclosure {
    /// Audio or video: by the type the feed gave, else by the address's
    /// extension, else audio -- which is what almost every podcast is.
    pub fn kind(&self) -> EnclosureKind {
        let mime = self.mime.trim().to_ascii_lowercase();
        if mime.starts_with("video/") {
            return EnclosureKind::Video;
        }
        if mime.starts_with("audio/") {
            return EnclosureKind::Audio;
        }
        let path = self.url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        let video = [".mp4", ".m4v", ".mov", ".webm", ".mkv"];
        if video.iter().any(|ext| path.ends_with(ext)) {
            EnclosureKind::Video
        } else {
            EnclosureKind::Audio
        }
    }

    /// Whether this is something to hear or watch at all, rather than a PDF
    /// or a picture a feed attached.
    pub fn is_playable(&self) -> bool {
        let mime = self.mime.trim().to_ascii_lowercase();
        if mime.starts_with("audio/") || mime.starts_with("video/") {
            return true;
        }
        if !mime.is_empty() && mime != "application/octet-stream" {
            return false;
        }
        let path = self.url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        [
            ".mp3", ".m4a", ".aac", ".ogg", ".oga", ".opus", ".wav", ".flac", ".mp4", ".m4v",
            ".mov", ".webm",
        ]
        .iter()
        .any(|ext| path.ends_with(ext))
    }
}

/// What an enclosure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum EnclosureKind {
    Audio,
    Video,
}

/// The files a story has in the files folder. Each is a file like any other,
/// referred to by id, the way [`crate::library::Item::files`] are; the
/// catalog's references table knows them under
/// [`crate::files::RefKind::Story`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StoryFiles {
    /// The episode, downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<FileId>,
    /// What was said in it, as Markdown with a time on every paragraph --
    /// see `karmayoga_core::news::transcript`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<FileId>,
    /// The article, kept as Markdown once it was read or heard: the Feed's
    /// memory of it, which outlives the story's own expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<FileId>,
    /// The article turned into audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<FileId>,
    /// A summary of an article, as Markdown -- see `docs/plans/feed-usability.md`.
    /// An episode's summary is the article written from its transcript, kept
    /// as its body, not this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<FileId>,
}

impl StoryFiles {
    pub fn is_empty(&self) -> bool {
        self.media.is_none()
            && self.transcript.is_none()
            && self.text.is_none()
            && self.audio.is_none()
            && self.summary.is_none()
    }

    /// Every file, for the references table.
    pub fn all(&self) -> impl Iterator<Item = FileId> + '_ {
        [self.media, self.transcript, self.text, self.audio, self.summary].into_iter().flatten()
    }
}

/// One show's own say over what is downloaded and kept. Every field left
/// `None` follows the Feed's [`PodcastDefaults`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PodcastSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<KeepPolicy>,
    /// Put every new episode in Up Next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_new: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcribe: Option<TranscribePolicy>,
    /// This show's playback speed, a multiple of normal. `None` is the
    /// player's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    /// Seconds to skip at the start of every episode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_intro_s: Option<u32>,
    /// `itunes:author`, or the directory's artist name.
    #[serde(default)]
    pub author: String,
    /// The directory's own id for the show (Apple's `collectionId`), when it
    /// was followed from there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_id: Option<String>,
}

/// Which episodes are downloaded without being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DownloadPolicy {
    /// Only what somebody asks for, or puts in Up Next.
    Off,
    /// The newest `count` episodes, as they arrive.
    #[serde(rename_all = "camelCase")]
    Newest { count: u32 },
    /// Every new episode.
    All,
}

/// How long a downloaded episode is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KeepPolicy {
    /// Until somebody deletes it.
    Forever,
    /// Moved to the files trash `days` after it was played to the end.
    #[serde(rename_all = "camelCase")]
    AfterPlayed { days: u32 },
    /// Only the newest `count` episodes of the show are kept.
    #[serde(rename_all = "camelCase")]
    Newest { count: u32 },
}

/// When an episode is transcribed -- which is what lets it be read as an
/// article, asked about, and found again by what was said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TranscribePolicy {
    /// Only when somebody asks.
    Off,
    /// As soon as it is downloaded.
    WhenDownloaded,
    /// Once somebody starts playing it.
    WhenPlayed,
}

/// The Feed's say over every show, which a show's [`PodcastSettings`] may
/// override.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PodcastDefaults {
    #[serde(default = "PodcastDefaults::default_download")]
    pub download: DownloadPolicy,
    #[serde(default = "PodcastDefaults::default_keep")]
    pub keep: KeepPolicy,
    #[serde(default)]
    pub queue_new: bool,
    #[serde(default = "PodcastDefaults::default_transcribe")]
    pub transcribe: TranscribePolicy,
}

impl PodcastDefaults {
    fn default_download() -> DownloadPolicy {
        DownloadPolicy::Newest { count: 1 }
    }
    fn default_keep() -> KeepPolicy {
        KeepPolicy::AfterPlayed { days: 7 }
    }
    fn default_transcribe() -> TranscribePolicy {
        TranscribePolicy::WhenPlayed
    }
}

impl Default for PodcastDefaults {
    fn default() -> Self {
        Self {
            download: Self::default_download(),
            keep: Self::default_keep(),
            queue_new: false,
            transcribe: Self::default_transcribe(),
        }
    }
}

/// What applies to one show: its own settings where it has them, the Feed's
/// defaults where it does not.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PodcastPolicy {
    pub download: DownloadPolicy,
    pub keep: KeepPolicy,
    pub queue_new: bool,
    pub transcribe: TranscribePolicy,
}

impl PodcastPolicy {
    /// `show`'s own settings over `defaults`.
    pub fn of(show: &PodcastSettings, defaults: &PodcastDefaults) -> Self {
        Self {
            download: show.download.unwrap_or(defaults.download),
            keep: show.keep.unwrap_or(defaults.keep),
            queue_new: show.queue_new.unwrap_or(defaults.queue_new),
            transcribe: show.transcribe.unwrap_or(defaults.transcribe),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enclosure(url: &str, mime: &str) -> Enclosure {
        Enclosure {
            url: url.into(),
            mime: mime.into(),
            length: None,
            duration_ms: None,
            transcript_url: String::new(),
            transcript_mime: String::new(),
            episode: None,
            season: None,
        }
    }

    #[test]
    fn an_enclosure_is_audio_or_video_by_type_then_extension() {
        assert_eq!(enclosure("https://x/a", "video/mp4").kind(), EnclosureKind::Video);
        assert_eq!(enclosure("https://x/a.mp4?t=1", "").kind(), EnclosureKind::Video);
        assert_eq!(enclosure("https://x/a.mp3", "").kind(), EnclosureKind::Audio);
        assert!(enclosure("https://x/a.mp3", "").is_playable());
        assert!(!enclosure("https://x/a.pdf", "application/pdf").is_playable());
        assert!(!enclosure("https://x/a.jpg", "image/jpeg").is_playable());
    }

    #[test]
    fn a_show_overrides_only_what_it_says() {
        let defaults = PodcastDefaults::default();
        let show = PodcastSettings { keep: Some(KeepPolicy::Forever), ..Default::default() };
        let policy = PodcastPolicy::of(&show, &defaults);
        assert_eq!(policy.keep, KeepPolicy::Forever);
        assert_eq!(policy.download, DownloadPolicy::Newest { count: 1 });
        assert_eq!(policy.transcribe, TranscribePolicy::WhenPlayed);
    }

    #[test]
    fn policies_are_tagged_by_type() {
        let json = serde_json::to_value(KeepPolicy::AfterPlayed { days: 3 }).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "afterPlayed", "days": 3 }));
        let json = serde_json::to_value(DownloadPolicy::Off).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "off" }));
    }
}
