//! Meeting notes: a call, recorded, transcribed, attributed and summarised.
//!
//! The records live here; the work is split the way the rest of the app
//! splits it. This module decides and cannot open a socket or a device:
//! which events are calls ([`detect`]), how a template is filled
//! ([`template`]), what the model is asked ([`prompt`]), how two tracks
//! become one transcript ([`merge`]) and which voice is whose
//! ([`identify`]). `everyday_service::meeting` owns the models, the spool and
//! the sockets, and `everyday-app` owns the microphone.
//!
//! What is sealed: everything a person said, everybody's name, every
//! voiceprint, and which event a recording was of. What is in the clear: that
//! a recording exists, the stage it is at, the calendar it came from and when
//! it started -- enough for recovery to find a stuck one and for the watcher
//! to know a call was already recorded.
//!
//! The audio itself is never a record. It is a spool of sealed chunk files
//! that exists until the note does. See `docs/plans/meeting-notes.md`.

pub mod detect;
pub mod filing;
pub mod identify;
pub mod merge;
pub mod prompt;
pub mod template;

use crate::calendar::Event;
use crate::id::{CalendarId, NoteId, RecordingId, TemplateId, TranscriptId, VoiceprintId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Which side of the call a stretch of audio came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Track {
    /// The microphone: the vault's owner.
    Mic,
    /// What the computer played: everybody else.
    System,
}

impl Track {
    pub const BOTH: [Track; 2] = [Track::Mic, Track::System];

    pub fn as_str(&self) -> &'static str {
        match self {
            Track::Mic => "mic",
            Track::System => "system",
        }
    }
}

/// The sample rate every chunk is stored at, and every speech model wants.
pub const SAMPLE_RATE: u32 = 16_000;

/// How long one spooled chunk is. A crash loses at most this much.
pub const CHUNK_SECONDS: u32 = 30;

/// Which call a recording was of, copied out of the event at the time.
///
/// Copied rather than pointed at: a feed event gets a new `EventId` on every
/// sync and may be gone by the time the pipeline finishes, and the note
/// still has to say who was invited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRef {
    pub calendar_id: CalendarId,
    /// The publisher's UID -- the durable name. See `Event::uid`.
    pub uid: String,
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// IANA zone the event is quoted in, for writing its times.
    #[serde(default)]
    pub tz: String,
    #[serde(default)]
    pub organizer: String,
    #[serde(default)]
    pub attendees: Vec<String>,
    /// The join link, if one was found. See [`detect::join_link`].
    #[serde(default)]
    pub join_url: String,
    /// The calendar's name, for the details block.
    #[serde(default)]
    pub calendar_name: String,
}

impl EventRef {
    pub fn from_event(event: &Event, calendar_name: &str) -> Self {
        Self {
            calendar_id: event.calendar_id,
            uid: event.uid.clone(),
            title: event.title.clone(),
            start: event.start,
            end: event.end,
            tz: event.tz.clone(),
            organizer: event.organizer.clone(),
            attendees: event.attendees.clone(),
            join_url: detect::join_link(event).unwrap_or_default(),
            calendar_name: calendar_name.to_string(),
        }
    }

    /// The UID with a recurrence suffix removed: what "never for this
    /// meeting" remembers. See [`detect::series_key`].
    pub fn series_key(&self) -> String {
        detect::series_key(&self.uid)
    }
}

/// Where a recording is in the pipeline.
///
/// `stage` is also a plaintext column, spelled by [`Stage::as_str`], so
/// recovery can find the stuck ones without unsealing every row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Stage {
    Recording,
    Transcribing,
    Identifying,
    Summarising,
    /// The note exists and the spool is gone.
    Done,
    /// Stopped at `at`, for `reason`. The spool is kept until a retry or a
    /// discard, or [`FAILED_SPOOL_DAYS`] pass.
    Failed {
        reason: String,
        at: Box<Stage>,
    },
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Recording => "recording",
            Stage::Transcribing => "transcribing",
            Stage::Identifying => "identifying",
            Stage::Summarising => "summarising",
            Stage::Done => "done",
            Stage::Failed { .. } => "failed",
        }
    }

    /// Still owed work by the pipeline (not recording, not finished).
    pub fn is_pending(&self) -> bool {
        matches!(self, Stage::Transcribing | Stage::Identifying | Stage::Summarising)
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Stage::Done)
    }
}

/// How long a failed recording's audio is kept before it is discarded.
pub const FAILED_SPOOL_DAYS: i64 = 7;

/// One spooled chunk of one track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkMeta {
    pub track: Track,
    /// Per track, from zero, with no gaps unless audio was actually lost.
    pub seq: u32,
    /// Milliseconds from the recording's start to this chunk's first sample.
    pub start_ms: u64,
    /// Samples at [`SAMPLE_RATE`], mono.
    pub samples: u64,
    /// Transcribed already. Lets a long call be mostly done by its end, and
    /// a retry skip what already worked.
    #[serde(default)]
    pub transcribed: bool,
}

/// A recording: the pipeline's durable state, and afterwards the history of
/// which call a note came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: RecordingId,
    /// `None` for a call that was not on the calendar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventRef>,
    pub title: String,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    pub stage: Stage,
    pub template_id: TemplateId,
    /// Started by "Always" rather than by a press.
    #[serde(default)]
    pub automatic: bool,
    /// Emptied at `Done`.
    #[serde(default)]
    pub chunks: Vec<ChunkMeta>,
    /// Segments already transcribed, kept across restarts so a retry does
    /// not pay twice. Emptied at `Done`, when the transcript record has them.
    #[serde(default)]
    pub partial: Vec<TrackSegment>,
    /// Set at `Done`. Cleared if the note is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_id: Option<NoteId>,
    pub updated_at: Timestamp,
}

impl Recording {
    pub fn new(title: impl Into<String>, event: Option<EventRef>, template_id: TemplateId) -> Self {
        let now = Timestamp::now();
        Self {
            id: RecordingId::new(),
            event,
            title: title.into(),
            started_at: now,
            ended_at: None,
            stage: Stage::Recording,
            template_id,
            automatic: false,
            chunks: Vec::new(),
            partial: Vec::new(),
            note_id: None,
            updated_at: now,
        }
    }

    pub fn calendar_id(&self) -> Option<CalendarId> {
        self.event.as_ref().map(|e| e.calendar_id)
    }
}

/// A transcribed stretch of one track, before speakers are settled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSegment {
    pub track: Track,
    /// From the recording's start.
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// The backend's own label, scoped to `hint_scope`. Only ever a hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_hint: Option<String>,
    /// Which request the hint came from: labels from two requests are not
    /// comparable. Usually the chunk's seq.
    #[serde(default)]
    pub hint_scope: u32,
}

/// How a speaker's name was arrived at. Shown, because a guess and a match
/// should not look the same.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Attribution {
    /// The microphone track.
    Owner,
    /// A voiceprint matched, with this cosine similarity.
    Matched {
        score: f32,
    },
    /// The only attendee left for the only voice left.
    Inferred,
    /// Somebody said so, in the note.
    Named,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speaker {
    /// Referred to by [`Segment::speaker`]. Stable within one transcript.
    pub key: u16,
    /// "Priya Raman", "Unknown 1", or the owner's name.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voiceprint_id: Option<VoiceprintId>,
    pub how: Attribution,
    /// The cluster's mean embedding, kept so naming this speaker later can
    /// make or update a voiceprint without the audio. Empty when
    /// voiceprints are off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub centroid: Vec<f32>,
    /// Which embedding model made `centroid`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub embedding_model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: u16,
    pub text: String,
}

/// What was said, kept after the audio is gone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub id: TranscriptId,
    pub note_id: NoteId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<RecordingId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// "local:parakeet-tdt-0.6b-v3", "openai:gpt-4o-transcribe-diarize".
    pub backend: String,
    pub speakers: Vec<Speaker>,
    pub segments: Vec<Segment>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Transcript {
    pub fn speaker(&self, key: u16) -> Option<&Speaker> {
        self.speakers.iter().find(|s| s.key == key)
    }

    /// One turn of dialogue: `[mm:ss] Name: text`, or `[h:mm:ss] Name: text`
    /// past an hour. Broken out of [`Transcript::as_lines`] so a caller
    /// paging through a long transcript -- `get_transcript`, in
    /// `agent::tools::meetings` -- can format one segment at a time without
    /// building the whole thing first only to slice it.
    pub fn line(&self, seg: &Segment) -> String {
        let name = self.speaker(seg.speaker).map(|s| s.label.as_str()).unwrap_or("Unknown");
        let secs = seg.start_ms / 1000;
        let stamp = if secs >= 3600 {
            format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
        } else {
            format!("{:02}:{:02}", secs / 60, secs % 60)
        };
        format!("[{stamp}] {name}: {}", seg.text.trim())
    }

    /// Every line, one per segment: what search indexes.
    pub fn as_lines(&self) -> String {
        let mut out = String::new();
        for seg in &self.segments {
            out.push_str(&self.line(seg));
            out.push('\n');
        }
        out
    }

    pub fn searchable_text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// At most this many centroids per voice.
pub const MAX_CENTROIDS: usize = 4;

/// A voice. Several centroids rather than one, because a person sounds
/// different on a headset and on a laptop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Voiceprint {
    pub id: VoiceprintId,
    pub name: String,
    /// How an attendee string finds this voice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default)]
    pub is_owner: bool,
    /// Embeddings from different models do not compare.
    pub model: String,
    pub centroids: Vec<Vec<f32>>,
    /// How many clusters have been folded in.
    pub samples: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// When to offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Offer {
    /// Never ask; recording only by hand.
    Off,
    /// Ask when a call starts.
    #[default]
    Ask,
    /// Record every detected call without asking.
    Always,
}

/// Which calendars the watcher looks at. Empty lists mean every calendar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarFilter {
    pub calendar_ids: Vec<CalendarId>,
    pub role_ids: Vec<crate::id::RoleId>,
}

/// The local recognisers on offer. See `everyday_service::meeting::models`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalModel {
    /// Parakeet TDT 0.6B v3, int8: English and European languages, fast.
    ParakeetV3,
    /// Whisper large-v3-turbo: any language, slower.
    WhisperTurbo,
}

impl LocalModel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LocalModel::ParakeetV3 => "parakeet-tdt-0.6b-v3",
            LocalModel::WhisperTurbo => "whisper-large-v3-turbo",
        }
    }
}

/// Who turns speech into text. There is deliberately no default: the switch
/// stays off until somebody has chosen one of these and made it usable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TranscriberConfig {
    Local {
        model: LocalModel,
    },
    OpenAi {
        model: String,
    },
    Google {
        model: String,
    },
    /// Anything that speaks OpenAI's `/v1/audio/transcriptions`.
    Compatible {
        base_url: String,
        model: String,
    },
}

impl TranscriberConfig {
    /// Whether this backend needs a key. A loopback compatible server does
    /// not, the same rule `Provider::needs_key` applies to the assistant.
    pub fn needs_key(&self) -> bool {
        match self {
            TranscriberConfig::Local { .. } => false,
            TranscriberConfig::OpenAi { .. } | TranscriberConfig::Google { .. } => true,
            TranscriberConfig::Compatible { base_url, .. } => !crate::agent::is_loopback(base_url),
        }
    }

    /// "local:…", "openai:…" -- what a transcript records as its backend.
    pub fn label(&self) -> String {
        match self {
            TranscriberConfig::Local { model } => format!("local:{}", model.as_str()),
            TranscriberConfig::OpenAi { model } => format!("openai:{model}"),
            TranscriberConfig::Google { model } => format!("google:{model}"),
            TranscriberConfig::Compatible { model, .. } => format!("compatible:{model}"),
        }
    }

    /// Whether audio leaves the machine.
    pub fn is_remote(&self) -> bool {
        match self {
            TranscriberConfig::Local { .. } => false,
            TranscriberConfig::Compatible { base_url, .. } => !crate::agent::is_loopback(base_url),
            _ => true,
        }
    }
}

/// A note shape. See [`template`] for what the body means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteTemplate {
    pub id: TemplateId,
    pub name: String,
    pub body: String,
}

/// Everything the meetings feature is configured by. Sealed in the vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeetingSettings {
    /// Refused by the service unless [`MeetingSettings::transcriber`] is set
    /// and usable.
    pub enabled: bool,
    pub offer: Offer,
    pub calendars: CalendarFilter,
    /// "Never for this meeting": series keys. See [`EventRef::series_key`].
    pub skipped_series: BTreeSet<String>,
    pub transcriber: Option<TranscriberConfig>,
    /// When the transcriber is OpenAI and the assistant already talks to
    /// api.openai.com, use the assistant's key rather than a second copy.
    pub use_assistant_key: bool,
    /// BCP-47-ish language code, or `None` to detect.
    pub language: Option<String>,
    pub templates: Vec<NoteTemplate>,
    pub default_template: Option<TemplateId>,
    /// Off by default: voiceprints are biometric data.
    pub voiceprints: bool,
    /// Stop after the event's end once both tracks have been quiet a while.
    pub auto_stop: bool,
    /// Summary budget override, in tokens. `None` picks by endpoint.
    pub summary_budget: Option<u32>,
}

impl Default for MeetingSettings {
    // Does *not* seed `templates` with [`template::starter`]. It would be
    // the more complete default -- a fresh install offering one template
    // rather than none -- but `starter` is a stub (`todo!`) until the
    // template work lands, and `#[serde(default)]` on this struct means
    // *every* JSON decode of a `MeetingSettings`, not just a genuinely
    // absent settings row, builds one of these to seed fields a payload
    // did not carry. Calling `starter` here would make reading back a
    // fully-populated, already-saved settings row panic today. Once
    // `starter` is real this can go back to calling it directly; until
    // then, [`MeetingSettings::template`] already falls back to it when
    // `templates` is empty, so nothing downstream silently loses the
    // built-in template -- it is only absent from a *fresh* install's
    // settings until this is restored.
    fn default() -> Self {
        let starter = template::starter();
        Self {
            enabled: false,
            offer: Offer::Ask,
            calendars: CalendarFilter::default(),
            skipped_series: BTreeSet::new(),
            transcriber: None,
            use_assistant_key: false,
            language: None,
            default_template: Some(starter.id),
            templates: vec![starter],
            voiceprints: false,
            auto_stop: true,
            summary_budget: None,
        }
    }
}

impl MeetingSettings {
    /// The template to use: the one asked for, else the default, else the
    /// first, else the built-in starter.
    pub fn template(&self, id: Option<TemplateId>) -> NoteTemplate {
        let pick = |want: Option<TemplateId>| {
            want.and_then(|w| self.templates.iter().find(|t| t.id == w).cloned())
        };
        pick(id)
            .or_else(|| pick(self.default_template))
            .or_else(|| self.templates.first().cloned())
            .unwrap_or_else(template::starter)
    }
}

/// Additional authenticated data for sealed payloads.
pub fn recording_aad(id: RecordingId) -> Vec<u8> {
    format!("everyday.recording.v1:{id}").into_bytes()
}

pub fn transcript_aad(id: TranscriptId) -> Vec<u8> {
    format!("everyday.transcript.v1:{id}").into_bytes()
}

pub fn voiceprint_aad(id: VoiceprintId) -> Vec<u8> {
    format!("everyday.voiceprint.v1:{id}").into_bytes()
}

/// For one spooled chunk file. The track and seq are bound in, so a chunk
/// cannot be swapped for another of the same recording.
pub fn chunk_aad(id: RecordingId, track: Track, seq: u32) -> Vec<u8> {
    format!("everyday.chunk.v1:{id}:{}:{seq}", track.as_str()).into_bytes()
}

pub const SETTINGS_AAD: &[u8] = b"everyday.meeting-settings.v1";
