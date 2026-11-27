//! Meeting notes: a call, recorded, transcribed, attributed and summarised.
//!
//! The records live here; the work is split the way the rest of the app
//! splits it. This module decides and cannot open a socket or a device:
//! which events are calls ([`detect`]), how a template is filled
//! ([`template`]), what the model is asked ([`prompt`]), how two tracks
//! become one transcript ([`merge`]) and which voice is whose
//! ([`identify`]). `jatayu_service::meeting` owns the models, the spool and
//! the sockets, and `jatayu-app` owns the microphone.
//!
//! What is sealed: everything a person said, everybody's name, every
//! voiceprint, and which event a recording was of. What is in the clear: that
//! a recording exists, the stage it is at, the calendar it came from and when
//! it started -- enough for recovery to find a stuck one and for the watcher
//! to know a call was already recorded.
//!
//! The audio itself is never a record. It is a spool of sealed chunk files
//! that exists until the note does. See `docs/plans/meeting-notes.md`.

pub mod brief;
pub mod detect;
pub mod echo;
pub mod filing;
pub mod hallucination;
pub mod identify;
pub mod linking;
pub mod merge;
pub mod people;
pub mod prompt;
pub mod steps;
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

/// How soon after one recording of a meeting ends that a new one, of the
/// same meeting, carries it on rather than starting a note of its own: the
/// person left the call and came back (a microphone that failed, a browser
/// that had to restart). Ten minutes, in milliseconds. Past it, a second
/// recording of the same meeting is a second sitting, with its own note.
pub const REJOIN_WINDOW_MS: i64 = 10 * 60 * 1000;

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
    /// The note this recording's words are written into. Set as soon as the
    /// call ends, when the pipeline writes the draft (title, details and the
    /// transcript, with the summary still to come), and kept at `Done`.
    /// Cleared if the note is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_id: Option<NoteId>,
    /// What the person wrote down themself while it was recorded (the
    /// panel's recording card), in the order written. The note is written
    /// with them, and keeps them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<RecordingNote>,
    /// The earlier recording of the same meeting this one carries on, when
    /// the person left the call and came back within [`REJOIN_WINDOW_MS`]
    /// (or the shell rolled one call into the next by mistake and this was
    /// put back together): its note is the one this recording's words are
    /// added to, rather than a note of this recording's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<RecordingId>,
    /// Whether the pipeline has already looked for the calendar meeting a
    /// recording begun without one belongs to -- so a retry does not ask
    /// the model twice. Never set on a recording begun with its meeting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub link_checked: bool,
    /// `updated_at` of the note as the pipeline last wrote it (the draft
    /// written when the call ended, or the finished notes). A note whose
    /// `updated_at` still matches was not touched by the person since, so
    /// the next write may replace it outright; one that moved has their
    /// edits in it, which a later write keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_written_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}

/// A line the person wrote down during a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RecordingNote {
    /// How far into the recording it was written, in milliseconds.
    pub at_ms: u64,
    pub text: String,
}

/// How far into a recording something happened, as the panel's card, the
/// model writing the notes and the note itself all say it: "12 min", "1 h",
/// "1 h 5 min". Whole minutes, rounded down, so a note written in the first
/// minute is at "0 min" -- the minute it belongs to, as a clock would show it.
pub fn minute_mark(at_ms: u64) -> String {
    let minutes = at_ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// What the person wrote down during the meeting, for the model writing its
/// notes: each line with how far in it was written, oldest first, under a
/// heading that says whose they are. Empty when they wrote nothing.
///
/// Not fenced as somebody else's words, as the invitation and the transcript
/// are: these are the person's own, written as it happened, and the best
/// guide there is to what they thought mattered.
pub fn owner_notes_block(notes: &[RecordingNote]) -> String {
    let lines = note_lines(notes, |mark, text| format!("- {mark}: {text}"));
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "What the owner wrote down during the meeting:\n{}\nThese are their own words, written \
         as it happened: give what they point at its place in the notes, and never contradict \
         them with a guess.",
        lines.join("\n")
    )
}

/// The note's last section, keeping what the person wrote down word for word:
/// "## Your notes" and a line each, with how far in it was written. Empty when
/// they wrote nothing, so a note without any has no empty heading.
pub fn owner_notes_section(notes: &[RecordingNote]) -> String {
    let lines = note_lines(notes, |mark, text| format!("- **{mark}** {text}"));
    if lines.is_empty() {
        return String::new();
    }
    format!("## Your notes\n\n{}", lines.join("\n"))
}

/// The notes with any words in them, oldest first, each made into one line
/// by `line` from its [`minute_mark`] and its text. A note is kept to one line
/// when it is written, but one from elsewhere is made one here too: a line
/// break inside a list item would end it.
fn note_lines(notes: &[RecordingNote], line: impl Fn(&str, &str) -> String) -> Vec<String> {
    let mut sorted: Vec<&RecordingNote> = notes.iter().collect();
    sorted.sort_by_key(|n| n.at_ms);
    sorted
        .into_iter()
        .map(|n| (n, n.text.split_whitespace().collect::<Vec<_>>().join(" ")))
        .filter(|(_, text)| !text.is_empty())
        .map(|(n, text)| line(&minute_mark(n.at_ms), &text))
        .collect()
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
            notes: Vec::new(),
            continues: None,
            link_checked: false,
            note_written_at: None,
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

    /// Take back out whatever a later part put in: every segment starting at
    /// or after `offset_ms` on this transcript's clock (the earlier parts all
    /// end before the later one starts), and the speakers no segment refers
    /// to any more. The owner is always kept.
    ///
    /// What makes writing a part idempotent -- a draft, a refresh and the
    /// final write all put the same recording's words in, and a retry puts
    /// them in again -- see [`Transcript::replace_part`].
    pub fn truncate_from(&mut self, offset_ms: u64) {
        let before = self.segments.len();
        self.segments.retain(|s| s.start_ms < offset_ms);
        let kept: std::collections::HashSet<u16> =
            self.segments.iter().map(|s| s.speaker).collect();
        let speakers = self.speakers.len();
        self.speakers.retain(|s| kept.contains(&s.key) || matches!(s.how, Attribution::Owner));
        if self.segments.len() != before || self.speakers.len() != speakers {
            self.updated_at = Timestamp::now();
        }
    }

    /// Put a later part in as [`Transcript::append_part`] does, after taking
    /// out whatever the same recording put in before
    /// ([`Transcript::truncate_from`]), so the part is there once however
    /// many times it is written.
    pub fn replace_part(
        &mut self,
        part_speakers: &[Speaker],
        part_segments: &[Segment],
        offset_ms: u64,
    ) {
        self.truncate_from(offset_ms);
        self.append_part(part_speakers, part_segments, offset_ms);
    }

    /// Carry this transcript on with a later part of the same meeting -- the
    /// person left the call and came back: `part`'s segments, `offset_ms`
    /// later than this transcript's own clock (how long after this one's
    /// recording started the later one did), under this transcript's
    /// speakers where they are the same person (the owner, and anyone named
    /// the same), and under new keys where they are not. Unknown voices are
    /// never assumed to be the same voice across the two parts.
    pub fn append_part(
        &mut self,
        part_speakers: &[Speaker],
        part_segments: &[Segment],
        offset_ms: u64,
    ) {
        use std::collections::HashMap;

        // "Unknown 3" -> 3: where the later part's unnamed voices carry on
        // counting from, so no two voices in the joined transcript share a
        // label.
        let unknown_number = |label: &str| -> Option<u32> {
            label.strip_prefix("Unknown ").and_then(|n| n.trim().parse().ok())
        };
        let mut next_unknown =
            self.speakers.iter().filter_map(|s| unknown_number(&s.label)).max().unwrap_or(0) + 1;
        let mut next_key =
            self.speakers.iter().map(|s| s.key).max().map_or(0, |k| k.saturating_add(1));

        let mut keys: HashMap<u16, u16> = HashMap::new();
        for part in part_speakers {
            let named = !matches!(part.how, Attribution::Unknown | Attribution::Owner)
                && part.label != people::OTHERS
                && unknown_number(&part.label).is_none();
            let existing = if matches!(part.how, Attribution::Owner) {
                self.speakers.iter().find(|s| matches!(s.how, Attribution::Owner))
            } else if named {
                self.speakers.iter().find(|s| {
                    !matches!(s.how, Attribution::Unknown)
                        && s.label != people::OTHERS
                        && s.label.eq_ignore_ascii_case(&part.label)
                })
            } else if part.label == people::OTHERS {
                // The bucket for voices nobody could tell apart is one
                // bucket, whichever part it came from.
                self.speakers.iter().find(|s| s.label == people::OTHERS)
            } else {
                None
            };
            if let Some(existing) = existing {
                keys.insert(part.key, existing.key);
                continue;
            }
            let mut fresh = part.clone();
            fresh.key = next_key;
            next_key = next_key.saturating_add(1);
            if matches!(fresh.how, Attribution::Unknown) && fresh.label != people::OTHERS {
                // An unknown voice is never assumed to be one heard before.
                fresh.label = format!("Unknown {next_unknown}");
                next_unknown += 1;
            }
            keys.insert(part.key, fresh.key);
            self.speakers.push(fresh);
        }

        for seg in part_segments {
            let speaker = *keys.entry(seg.speaker).or_insert_with(|| {
                // A line whose speaker the part did not list: kept, under a
                // voice of its own.
                let key = next_key;
                next_key = next_key.saturating_add(1);
                self.speakers.push(Speaker {
                    key,
                    label: format!("Unknown {next_unknown}"),
                    email: None,
                    voiceprint_id: None,
                    how: Attribution::Unknown,
                    centroid: Vec::new(),
                    embedding_model: String::new(),
                });
                next_unknown += 1;
                key
            });
            self.segments.push(Segment {
                start_ms: seg.start_ms.saturating_add(offset_ms),
                end_ms: seg.end_ms.saturating_add(offset_ms),
                speaker,
                text: seg.text.clone(),
            });
        }
        self.updated_at = Timestamp::now();
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
    /// The owner's own voice, kept from the last time they recorded it:
    /// 16-bit mono PCM at [`SAMPLE_RATE`], little-endian, base64, at most
    /// [`MAX_VOICE_SAMPLE_SECONDS`]. What taking other voices out of the
    /// microphone listens for (`jatayu_service::speech::cleanup`), since
    /// an embedding is not something that model can be given. Only ever on
    /// the owner's voiceprint, and gone with it; `None` on one recorded
    /// before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_sample: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// The longest [`Voiceprint::voice_sample`] kept. The model that uses it
/// picks the loudest three seconds of it, and a sample with pauses in it
/// can leave it three seconds of the wrong thing: in testing, three seconds
/// cut from a recording made it keep the background instead, while twelve
/// to fifteen let it find the person.
pub const MAX_VOICE_SAMPLE_SECONDS: u32 = 12;

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
    /// Calendars serving any of these areas. Settings saved before areas
    /// were renamed from roles say `roleIds`; the alias reads them.
    #[serde(alias = "roleIds")]
    pub area_ids: Vec<crate::id::AreaId>,
}

/// How speech becomes text is the platform's speech model now -- see
/// [`crate::speech`] -- and these two are its types. Named from here still,
/// so everything that took them from meetings, where they were born, keeps
/// working.
pub use crate::speech::{LocalModel, TranscriberConfig};

/// A meeting template as builds before shared templates stored it: inside
/// [`MeetingSettings`]. Read once, moved into the templates table as a
/// [`crate::template::Template`] offered for meetings, and never written
/// again -- see [`MeetingSettings::legacy_templates`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NoteTemplate {
    pub id: TemplateId,
    pub name: String,
    pub body: String,
}

impl NoteTemplate {
    /// The shared record this becomes.
    pub fn into_template(self) -> crate::template::Template {
        let now = jiff::Timestamp::now();
        crate::template::Template {
            id: self.id,
            name: self.name,
            uses: vec![crate::template::TemplateUse::Meetings],
            title: String::new(),
            body: self.body,
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Everything the meetings feature is configured by. Sealed in the vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct MeetingSettings {
    /// Refused by the service unless the speech model --
    /// [`crate::llm::ModelSettings::speech`] -- is set and usable.
    pub enabled: bool,
    pub offer: Offer,
    /// Also offer when another application starts using the microphone --
    /// the desktop shell's `micwatch` -- not only when a calendar meeting
    /// starts. On by default; [`Offer::Off`] turns this off too.
    pub mic_offer: bool,
    pub calendars: CalendarFilter,
    /// "Never for this meeting": series keys. See [`EventRef::series_key`].
    pub skipped_series: BTreeSet<String>,
    /// How speech became text before that was the platform's speech model:
    /// written by older builds, and moved into
    /// [`crate::llm::ModelSettings::speech`] once, by
    /// `Vault::carry_over_speech`, which empties it and sets
    /// [`MeetingSettings::speech_moved`]. Read by nothing else -- not even
    /// what a caller sends: `Vault::save_meeting_settings` keeps whatever the
    /// vault holds here. Left out when empty, so a saved row carries none of
    /// the three and a vault that never had one reads as it always did.
    #[serde(rename = "transcriber", skip_serializing_if = "Option::is_none")]
    pub legacy_transcriber: Option<TranscriberConfig>,
    /// Older builds' switch for using the models' OpenAI key instead of a
    /// second copy. Nothing replaces it and nothing moves with it: OpenAI
    /// speech files its key under the endpoint an OpenAI language model
    /// does, so the two share one key whenever both are at OpenAI.
    #[serde(rename = "useAssistantKey", skip_serializing_if = "std::ops::Not::not")]
    pub legacy_use_assistant_key: bool,
    /// The recogniser's language, as older builds kept it -- now
    /// [`crate::speech::SpeechConfig::language`], moved with the transcriber.
    #[serde(rename = "language", skip_serializing_if = "Option::is_none")]
    pub legacy_language: Option<String>,
    /// Set once [`MeetingSettings::legacy_transcriber`], its language and
    /// its key have moved to the models, after which the three are ignored.
    /// Kept beside the emptied fields so the move is a question of one flag
    /// rather than of which fields happen to be empty -- the way
    /// [`MeetingSettings::templates_moved`] is -- and owned by the vault the
    /// same way: `Vault::save_meeting_settings` never lets a caller set or
    /// clear it.
    pub speech_moved: bool,
    /// Where meeting templates were kept before they became shared
    /// [`crate::template::Template`]s: written by older builds, read once
    /// by `Vault::migrate_meeting_templates`, which moves them into the
    /// templates table and sets [`MeetingSettings::templates_moved`].
    ///
    /// Seeded with the starter by [`Default`], as it always was, so a vault
    /// that has never saved meeting settings moves the starter in too --
    /// which is how a new vault comes to have one. Written empty once moved,
    /// rather than left out, so a saved row reads back exactly as it was and
    /// an older build opening it finds an empty list (and falls back to its
    /// own starter) rather than a default one.
    #[serde(rename = "templates")]
    pub legacy_templates: Vec<NoteTemplate>,
    /// Set once [`MeetingSettings::legacy_templates`] has been moved, after
    /// which it is ignored. Needed beside the emptied list because a payload
    /// that names no list at all -- a form that never knew of it -- is given
    /// the seeded starter by `Default`, and only this says not to move it
    /// back in after somebody deleted it. Owned by the vault:
    /// `Vault::save_meeting_settings` never lets a caller turn it off.
    pub templates_moved: bool,
    /// The template a recording is written up from when nobody picked one.
    /// A [`crate::template::Template`]'s id; see
    /// [`crate::template::for_meeting`] for what happens when it names one
    /// that is gone.
    pub default_template: Option<TemplateId>,
    /// Off by default: voiceprints are biometric data.
    pub voiceprints: bool,
    /// Stop after the event's end once both tracks have been quiet a while.
    pub auto_stop: bool,
    /// When [`Offer`] is [`Offer::Ask`]: a card in the corner of the screen
    /// instead of a system notification, and a short countdown once the
    /// meeting is joined before recording starts. Off and Always ignore it.
    pub auto_record: bool,
    /// Summary budget override, in tokens. `None` picks by endpoint.
    pub summary_budget: Option<u32>,
    /// Names, products and acronyms to spell right in every meeting: told
    /// to the recogniser, to the summarising model, and used to correct a
    /// transcript's near misses. See [`vocab`]. Meetings' own rather than the
    /// speech model's, though the recogniser is told it: it is about what is
    /// said in meetings, and goes with the invite's names it is told beside.
    /// Every other listener's recogniser is told it too -- see
    /// [`crate::speech::words`].
    pub vocabulary: Vec<String>,
    /// The brief prepared before each meeting -- see [`brief`].
    pub brief: brief::MeetingBriefSettings,
}

impl Default for MeetingSettings {
    // Seeds `legacy_templates` with the starter, so a fresh install moves
    // one template into the table rather than none. `#[serde(default)]` on
    // this struct means *every* JSON decode of a `MeetingSettings`, not just
    // a genuinely absent settings row, builds one of these first and
    // overlays whatever the payload actually named on top -- which is why
    // `templates_moved`, not an empty list, is what says the move is done.
    fn default() -> Self {
        let starter = template::starter();
        let starter = NoteTemplate { id: starter.id, name: starter.name, body: starter.body };
        Self {
            enabled: false,
            offer: Offer::Ask,
            mic_offer: true,
            calendars: CalendarFilter::default(),
            skipped_series: BTreeSet::new(),
            legacy_transcriber: None,
            legacy_use_assistant_key: false,
            legacy_language: None,
            speech_moved: false,
            default_template: Some(starter.id),
            legacy_templates: vec![starter],
            templates_moved: false,
            voiceprints: false,
            auto_stop: true,
            auto_record: false,
            summary_budget: None,
            vocabulary: Vec::new(),
            brief: brief::MeetingBriefSettings::default(),
        }
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

    #[test]
    fn a_calendar_filter_saved_with_roles_still_names_its_areas() {
        // Meeting settings saved before areas were renamed from roles.
        let id = crate::id::AreaId::new();
        let old = serde_json::json!({ "calendarIds": [], "roleIds": [id] });
        let filter: CalendarFilter = serde_json::from_value(old).expect("the old spelling reads");
        assert_eq!(filter.area_ids, vec![id]);
    }

    fn who(key: u16, label: &str, how: Attribution) -> Speaker {
        Speaker {
            key,
            label: label.into(),
            email: None,
            voiceprint_id: None,
            how,
            centroid: Vec::new(),
            embedding_model: String::new(),
        }
    }

    fn said(start_ms: u64, speaker: u16, text: &str) -> Segment {
        Segment { start_ms, end_ms: start_ms + 1_000, speaker, text: text.into() }
    }

    fn joined_transcript() -> Transcript {
        let now = Timestamp::now();
        Transcript {
            id: TranscriptId::new(),
            note_id: NoteId::new(),
            recording_id: None,
            language: None,
            backend: String::new(),
            speakers: vec![
                who(1, "Hari", Attribution::Owner),
                who(2, "Priya Raman", Attribution::Named),
                who(3, "Unknown 1", Attribution::Unknown),
            ],
            segments: vec![said(0, 1, "hello"), said(2_000, 2, "hi"), said(4_000, 3, "um")],
            room: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_later_part_is_shifted_and_appended_in_order() {
        let mut t = joined_transcript();
        let part = [who(1, "Hari", Attribution::Owner)];
        t.append_part(&part, &[said(500, 1, "back again"), said(3_000, 1, "so")], 60_000);
        let starts: Vec<u64> = t.segments.iter().map(|s| s.start_ms).collect();
        assert_eq!(starts, vec![0, 2_000, 4_000, 60_500, 63_000]);
        assert_eq!(t.segments[3].end_ms, 61_500);
    }

    #[test]
    fn truncating_from_an_offset_drops_the_later_words_and_the_voices_only_they_used() {
        let mut t = joined_transcript();
        let part = [who(1, "Hari", Attribution::Owner), who(2, "Sam", Attribution::Named)];
        t.append_part(&part, &[said(0, 1, "back"), said(1_000, 2, "me too")], 60_000);
        assert_eq!(t.segments.len(), 5);
        assert!(t.speakers.iter().any(|s| s.label == "Sam"));

        t.truncate_from(60_000);
        assert_eq!(t.segments.len(), 3, "the first part is untouched");
        assert!(t.speakers.iter().all(|s| s.label != "Sam"), "a voice only the part used goes");
        assert_eq!(t.speakers.len(), 3);

        // The owner stays even when no line is theirs.
        t.truncate_from(0);
        assert!(t.segments.is_empty());
        assert_eq!(t.speakers.len(), 1);
        assert!(matches!(t.speakers[0].how, Attribution::Owner));
    }

    #[test]
    fn a_part_replaced_is_there_once_however_often_it_is_written() {
        let mut t = joined_transcript();
        let part = [who(1, "Hari", Attribution::Owner), who(5, "Sam", Attribution::Named)];
        let lines = [said(0, 1, "back"), said(1_000, 5, "me too")];
        for _ in 0..3 {
            t.replace_part(&part, &lines, 60_000);
        }
        let count = |text: &str| t.segments.iter().filter(|s| s.text == text).count();
        assert_eq!(count("back"), 1);
        assert_eq!(count("me too"), 1);
        assert_eq!(count("hello"), 1);
        assert_eq!(t.speakers.iter().filter(|s| s.label == "Sam").count(), 1);
    }

    #[test]
    fn the_owner_and_anyone_named_the_same_keep_their_speaker() {
        let mut t = joined_transcript();
        let part = [
            who(7, "Someone Else Entirely", Attribution::Owner),
            who(8, "priya raman", Attribution::Matched { score: 0.8 }),
        ];
        t.append_part(&part, &[said(0, 7, "mine"), said(1_000, 8, "hers")], 10_000);
        assert_eq!(t.speakers.len(), 3, "nobody new: {:?}", t.speakers);
        assert_eq!(t.segments[3].speaker, 1);
        assert_eq!(t.segments[4].speaker, 2);
    }

    #[test]
    fn an_owner_the_first_part_lacked_is_added() {
        let mut t = joined_transcript();
        t.speakers.retain(|s| s.key != 1);
        t.segments.retain(|s| s.speaker != 1);
        t.append_part(&[who(1, "Hari", Attribution::Owner)], &[said(0, 1, "hi")], 5_000);
        let owner = t.owner().expect("the owner is there now");
        assert_eq!(t.segments.last().unwrap().speaker, owner.key);
    }

    #[test]
    fn unknown_voices_are_never_taken_for_ones_heard_before() {
        let mut t = joined_transcript();
        let part = [
            who(1, "Unknown 1", Attribution::Unknown),
            who(2, "Unknown 2", Attribution::Unknown),
            who(3, "Dev Patel", Attribution::Named),
        ];
        t.append_part(&part, &[said(0, 1, "a"), said(1_000, 2, "b"), said(2_000, 3, "c")], 9_000);
        let labels: Vec<&str> = t.speakers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Hari", "Priya Raman", "Unknown 1", "Unknown 2", "Unknown 3", "Dev Patel"]
        );
        let keys: Vec<u16> = t.speakers.iter().map(|s| s.key).collect();
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "keys stay unique: {keys:?}");
        // The new lines point at the new voices.
        assert_eq!(t.speaker(t.segments[3].speaker).unwrap().label, "Unknown 2");
        assert_eq!(t.speaker(t.segments[4].speaker).unwrap().label, "Unknown 3");
        assert_eq!(t.speaker(t.segments[5].speaker).unwrap().label, "Dev Patel");
    }

    #[test]
    fn the_others_bucket_stays_one_bucket() {
        let mut t = joined_transcript();
        t.speakers.push(who(4, people::OTHERS, Attribution::Unknown));
        t.append_part(
            &[who(2, people::OTHERS, Attribution::Unknown)],
            &[said(0, 2, "crowd")],
            1_000,
        );
        assert_eq!(t.speakers.iter().filter(|s| s.label == people::OTHERS).count(), 1);
        assert_eq!(t.segments.last().unwrap().speaker, 4);
    }

    #[test]
    fn a_line_from_a_speaker_the_part_did_not_list_is_kept() {
        let mut t = joined_transcript();
        t.append_part(&[], &[said(0, 9, "who said this")], 1_000);
        let last = t.segments.last().unwrap();
        assert_eq!(t.speaker(last.speaker).unwrap().label, "Unknown 2");
    }

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

    /// An older build's row still reads, its transcriber landing in the
    /// legacy fields for the carry-over to find; a row written now carries
    /// none of them once they are empty.
    #[test]
    fn the_old_speech_fields_still_read_and_are_not_written_empty() {
        let old: MeetingSettings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "transcriber": { "type": "openAi", "model": "gpt-4o-transcribe" },
            "useAssistantKey": true,
            "language": "en"
        }))
        .unwrap();
        assert_eq!(
            old.legacy_transcriber,
            Some(TranscriberConfig::OpenAi { model: "gpt-4o-transcribe".into() })
        );
        assert!(old.legacy_use_assistant_key);
        assert_eq!(old.legacy_language.as_deref(), Some("en"));
        assert!(!old.speech_moved, "a row from before the move says nothing of it");

        let moved = MeetingSettings { speech_moved: true, ..MeetingSettings::default() };
        let json = serde_json::to_value(&moved).unwrap();
        for gone in ["transcriber", "useAssistantKey", "language"] {
            assert!(json.get(gone).is_none(), "{gone} is still written");
        }
        assert_eq!(json["speechMoved"], true);
    }

    fn note(at_ms: u64, text: &str) -> RecordingNote {
        RecordingNote { at_ms, text: text.into() }
    }

    #[test]
    fn a_minute_mark_is_whole_minutes_and_then_hours() {
        assert_eq!(minute_mark(0), "0 min");
        assert_eq!(minute_mark(59_999), "0 min");
        assert_eq!(minute_mark(12 * 60_000 + 30_000), "12 min");
        assert_eq!(minute_mark(60 * 60_000), "1 h");
        assert_eq!(minute_mark(65 * 60_000), "1 h 5 min");
    }

    #[test]
    fn the_owners_notes_reach_the_model_oldest_first_with_their_minutes() {
        assert_eq!(owner_notes_block(&[]), "", "nothing written, nothing said");
        assert_eq!(owner_notes_block(&[note(0, "   ")]), "", "a blank note is not a note");
        let block =
            owner_notes_block(&[note(12 * 60_000, "Budget\nis $40k"), note(60_000, "Ask Priya")]);
        assert!(block.starts_with("What the owner wrote down during the meeting:\n"));
        let lines: Vec<&str> = block.lines().skip(1).take(2).collect();
        assert_eq!(lines, ["- 1 min: Ask Priya", "- 12 min: Budget is $40k"]);
    }

    #[test]
    fn the_note_keeps_the_owners_words_in_a_section_of_their_own() {
        assert_eq!(owner_notes_section(&[]), "", "no empty heading");
        assert_eq!(
            owner_notes_section(&[note(3 * 60_000, "Ship by Friday"), note(65 * 60_000, "Done")]),
            "## Your notes\n\n- **3 min** Ship by Friday\n- **1 h 5 min** Done"
        );
    }

    #[test]
    fn a_recording_from_before_notes_reads_with_none_and_writes_none() {
        let mut recording = Recording::new("Design sync", None, TemplateId::new());
        let json = serde_json::to_value(&recording).unwrap();
        assert!(json.get("notes").is_none(), "an empty list is not written");
        let back: Recording = serde_json::from_value(json).unwrap();
        assert!(back.notes.is_empty());
        recording.notes.push(note(90_000, "Ask about the budget"));
        let json = serde_json::to_value(&recording).unwrap();
        assert_eq!(json["notes"][0]["atMs"], 90_000);
        assert_eq!(serde_json::from_value::<Recording>(json).unwrap(), recording);
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
