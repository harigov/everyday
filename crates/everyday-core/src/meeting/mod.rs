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
pub mod echo;
pub mod filing;
pub mod hallucination;
pub mod identify;
pub mod merge;
pub mod people;
pub mod prompt;
pub mod template;
pub mod vocab;
pub mod words;

use crate::calendar::Event;
use crate::id::{CalendarId, NoteId, RecordingId, TemplateId, TranscriptId, VoiceprintId};
use crate::timestamped::Timestamped;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Which side of the call a stretch of audio came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Copied from [`Event::guests`]: everybody invited, by address, with
    /// their answer. Empty for a recording made before it existed, or of
    /// an event whose calendar had not been synced since; `attendees` is
    /// then all there is to go on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guests: Vec<crate::calendar::Attendee>,
    /// The join link, if one was found. See [`detect::join_link`].
    #[serde(default)]
    pub join_url: String,
    /// The calendar's name, for the details block.
    #[serde(default)]
    pub calendar_name: String,
    /// Copied from [`Event::series`]. `#[serde(default)]` so an `EventRef`
    /// sealed before this field existed still reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
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
            guests: event.guests.clone(),
            join_url: detect::join_link(event).unwrap_or_default(),
            calendar_name: calendar_name.to_string(),
            series: event.series.clone(),
        }
    }

    /// The series key "never for this meeting" remembers: `series` itself
    /// when the source gave one, or the UID with a recurrence suffix
    /// removed otherwise. See [`detect::series_key`].
    pub fn series_key(&self) -> String {
        detect::series_key_of(&self.uid, self.series.as_deref())
    }
}

/// Where a recording is in the pipeline.
///
/// `stage` is also a plaintext column, spelled by [`Stage::as_str`], so
/// recovery can find the stuck ones without unsealing every row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// How much of this chunk's start was already transcribed as the end of
    /// the previous chunk's last sentence: speech running across a chunk
    /// boundary is sent whole, with the previous chunk, rather than cut in
    /// two. Zero when nothing ran over.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub head_ms: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A recording: the pipeline's durable state, and afterwards the history of
/// which call a note came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// At or under this share of the words from the computer's own sound,
/// nobody was on a call: the meeting was in a room and everyone in it was
/// heard on the microphone. A stand-up heard through a laptop's microphone
/// measured 1%. Kept low on purpose: a call the owner does nearly all the
/// talking on -- a demo, with the client's few questions coming through the
/// speakers -- is still a call, and its microphone is still the owner alone.
pub const ROOM_SYSTEM_SHARE: f32 = 0.02;

/// The fewest words on the microphone before a recording is judged a room
/// at all: a few seconds of somebody talking to themselves is not a meeting
/// to change anything about.
pub const ROOM_MIN_WORDS: usize = 20;

/// Was everybody heard on the one microphone? Only for a meeting somebody
/// else was invited to (`others_invited`) -- a voice memo is the owner
/// alone -- and then when the microphone carried at least
/// [`ROOM_MIN_WORDS`] words and the system track at most
/// [`ROOM_SYSTEM_SHARE`] of all of them. Words are counted the way
/// [`words::tokens`] counts them, so a transcript in Japanese or Chinese is
/// measured in characters, not in segments. Echo is expected to have been
/// removed already, so the speakers leaking into the microphone does not
/// count for the room.
pub fn heard_in_one_room(segments: &[TrackSegment], others_invited: bool) -> bool {
    if !others_invited {
        return false;
    }
    let count = |track: Track| -> usize {
        segments.iter().filter(|s| s.track == track).map(|s| words::tokens(&s.text).len()).sum()
    };
    let (mic, system) = (count(Track::Mic), count(Track::System));
    mic >= ROOM_MIN_WORDS && (system as f32) <= (mic + system) as f32 * ROOM_SYSTEM_SHARE
}

/// How a speaker's name was arrived at. Shown, because a guess and a match
/// should not look the same.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: u16,
    pub text: String,
}

/// What was said, kept after the audio is gone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Most likely a meeting in a room, everybody heard on the one
    /// microphone (see [`heard_in_one_room`]), whose voices were not told
    /// apart: every line under the owner's label may be anyone in the room. A room whose
    /// voices were told apart is not flagged -- its labels are as sure, or
    /// unsure, as a call's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub room: bool,
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

    /// What the summarising model reads: one line per turn, `Name: text`,
    /// with consecutive segments from one speaker joined into one turn and
    /// no timestamps. A transcript arrives as hundreds of fragments -- a
    /// chunk boundary, a breath, a recogniser's own segmentation -- and a
    /// name and a stamp on every one of them cost tokens and make the
    /// conversation harder to follow than the turns people actually took.
    pub fn as_turns(&self) -> String {
        let mut out = String::new();
        let mut current: Option<u16> = None;
        for seg in &self.segments {
            let text = seg.text.trim();
            if text.is_empty() {
                continue;
            }
            if current == Some(seg.speaker) {
                out.push(' ');
            } else {
                if current.is_some() {
                    out.push('\n');
                }
                let name = self.speaker(seg.speaker).map(|s| s.label.as_str()).unwrap_or("Unknown");
                out.push_str(name);
                out.push_str(": ");
                current = Some(seg.speaker);
            }
            out.push_str(text);
        }
        if current.is_some() {
            out.push('\n');
        }
        out
    }

    /// The speaker the microphone track was attributed to, if one was.
    pub fn owner(&self) -> Option<&Speaker> {
        self.speakers.iter().find(|s| matches!(s.how, Attribution::Owner))
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

impl Timestamped for Voiceprint {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// When to offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarFilter {
    pub calendar_ids: Vec<CalendarId>,
    pub role_ids: Vec<crate::id::RoleId>,
}

/// The local recognisers on offer. See `everyday_service::meeting::models`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NoteTemplate {
    pub id: TemplateId,
    pub name: String,
    pub body: String,
}

/// Everything the meetings feature is configured by. Sealed in the vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct MeetingSettings {
    /// Refused by the service unless [`MeetingSettings::transcriber`] is set
    /// and usable.
    pub enabled: bool,
    pub offer: Offer,
    /// Also offer when another application starts using the microphone --
    /// the desktop shell's `micwatch` -- not only when a calendar meeting
    /// starts. On by default; [`Offer::Off`] turns this off too.
    pub mic_offer: bool,
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
    /// Names, products and acronyms to spell right in every meeting: told
    /// to the recogniser, to the summarising model, and used to correct a
    /// transcript's near misses. See [`vocab`].
    pub vocabulary: Vec<String>,
}

impl Default for MeetingSettings {
    // Seeds `templates` with [`template::starter`], so a fresh install
    // offers one template rather than none. `#[serde(default)]` on this
    // struct means *every* JSON decode of a `MeetingSettings`, not just a
    // genuinely absent settings row, builds one of these first and overlays
    // whatever the payload actually named on top -- so a fully-populated,
    // already-saved settings row calls this too, and simply has its
    // `templates` overwritten by its own. [`MeetingSettings::template`]
    // falls back to `starter` regardless whenever `templates` is empty, for
    // a settings row saved before this field existed at all.
    fn default() -> Self {
        let starter = template::starter();
        Self {
            enabled: false,
            offer: Offer::Ask,
            mic_offer: true,
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
            vocabulary: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(track: Track, text: &str) -> TrackSegment {
        TrackSegment {
            track,
            start_ms: 0,
            end_ms: 1_000,
            text: text.into(),
            speaker_hint: None,
            hint_scope: 0,
        }
    }

    #[test]
    fn a_room_is_a_meeting_with_others_heard_only_on_the_microphone() {
        let owner = "so the first thing on the list today is the pricing page and the launch date";
        let mic = vec![seg(Track::Mic, owner), seg(Track::Mic, owner)];
        assert!(heard_in_one_room(&mic, true));
        // A voice memo: nobody else was invited.
        assert!(!heard_in_one_room(&mic, false));
        // A demo the owner talks through, the client asking a little: a call.
        let mut demo = mic.clone();
        demo.push(seg(Track::System, "and what does it cost per seat"));
        assert!(!heard_in_one_room(&demo, true));
        // Too little said to judge.
        assert!(!heard_in_one_room(&[seg(Track::Mic, "testing one two")], true));
    }

    #[test]
    fn a_room_is_measured_in_characters_where_there_are_no_spaces() {
        // Many short lines of the owner's against a few long ones of theirs.
        let mut segments: Vec<TrackSegment> =
            (0..30).map(|_| seg(Track::Mic, "はい、そうです")).collect();
        segments.push(seg(Track::System, "来週の火曜日までに価格表を更新して送ります"));
        segments.push(seg(Track::System, "それからデザインの確認もお願いします"));
        assert!(!heard_in_one_room(&segments, true));
    }
}
