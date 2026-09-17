use std::sync::Arc;
use std::time::Duration;

use everyday_core::meeting::{
    ChunkMeta, EventRef, Recording, SAMPLE_RATE, Stage, Track, TrackSegment, TranscriberConfig,
    Voiceprint,
};
use everyday_core::{RecordingId, Vault};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult, codes};
use crate::meeting::transcribe::{Hints, Limits, RawSegment, SpeechChunk, Transcriber};
use crate::service::{Service, blocking};
use crate::supervisor::Outcome;

use super::adapters::*;
use super::drivers::*;
use super::enqueue::*;
use super::failure::*;
use super::retry::*;
use super::stages::*;
use super::turns::*;

use super::test_hooks;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

// ---- group_turns / map_segment ----------------------------------------

fn samples(n: usize) -> Vec<i16> {
    (0..n).map(|i| (i % 100) as i16).collect()
}

fn ms_to_samples(ms: u64) -> usize {
    (ms * u64::from(SAMPLE_RATE) / 1000) as usize
}

#[test]
fn a_single_turn_maps_straight_back_with_the_chunk_offset() {
    let total_ms = 5_000;
    let sig = samples(ms_to_samples(total_ms));
    let turns = vec![(1_000, 3_000)];
    let limits = Limits { max_bytes: 100_000_000, max_seconds: 3_600 };
    let groups = group_turns(&turns, &sig, 30_000, limits);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].map.len(), 1);
    // The whole grouped buffer, reported by the backend, maps back to
    // the turn's real position: chunk offset (30s) + turn start (1s).
    let (start, end) = map_segment(&groups[0].map, 0, 2_000);
    assert_eq!(start, 31_000);
    assert_eq!(end, 33_000);
}

#[test]
fn two_turns_concatenated_map_back_to_their_own_separate_times() {
    let total_ms = 10_000;
    let sig = samples(ms_to_samples(total_ms));
    // Turn 1: 0-1s. Turn 2: 5-6s -- far apart in the recording, but
    // folded together with only GROUP_SILENCE_MS of padding between.
    let turns = vec![(0, 1_000), (5_000, 6_000)];
    let limits = Limits { max_bytes: 100_000_000, max_seconds: 3_600 };
    let groups = group_turns(&turns, &sig, 0, limits);
    assert_eq!(groups.len(), 1, "both turns fit comfortably under the limit");
    assert_eq!(groups[0].map.len(), 2);

    // A segment the backend reports at the very start of the buffer is
    // turn 1: maps to recording time 0.
    let (s0, e0) = map_segment(&groups[0].map, 0, 500);
    assert_eq!((s0, e0), (0, 500));

    // A segment reported after turn 1 (1000ms) + the silence pad lands
    // in turn 2's span, and must map back to *its* real time (5000ms
    // onward), not to 1000ms + delta as a naive offset-add would give.
    let turn2_local_start = 1_000 + GROUP_SILENCE_MS;
    let (s1, e1) = map_segment(&groups[0].map, turn2_local_start + 100, turn2_local_start + 300);
    assert_eq!(s1, 5_100, "must use turn 2's real recording time, not a flat offset");
    assert_eq!(e1, 5_300);
}

#[test]
fn turns_that_would_exceed_the_backend_limit_split_into_more_than_one_group() {
    let total_ms = 20_000;
    let sig = samples(ms_to_samples(total_ms));
    let turns = vec![(0, 5_000), (6_000, 11_000), (12_000, 17_000)];
    // A tight limit: the first turn alone (5s) fits, but adding the
    // second (5s + 300ms pad) would not.
    let limits = Limits { max_bytes: 100_000_000, max_seconds: 6 };
    let groups = group_turns(&turns, &sig, 0, limits);
    assert!(groups.len() >= 2, "expected the turns to split across groups: {}", groups.len());
    // Every turn is accounted for exactly once, across whichever groups
    // it landed in.
    let total_turns: usize = groups.iter().map(|g| g.map.len()).sum();
    assert_eq!(total_turns, 3);
}

#[test]
fn no_turns_at_all_produces_no_groups() {
    let limits = Limits { max_bytes: 100_000_000, max_seconds: 3_600 };
    assert!(group_turns(&[], &samples(1_000), 0, limits).is_empty());
}

#[test]
fn a_whole_chunk_with_no_vad_is_one_turn_that_maps_identically() {
    // The no-`speech` path: one turn spanning the whole chunk.
    let total_ms = 3_000;
    let sig = samples(ms_to_samples(total_ms));
    let turns = vec![(0, total_ms)];
    let limits = Limits { max_bytes: 100_000_000, max_seconds: 3_600 };
    let groups = group_turns(&turns, &sig, 60_000, limits);
    assert_eq!(groups.len(), 1);
    let (start, end) = map_segment(&groups[0].map, 500, 1_500);
    assert_eq!((start, end), (60_500, 61_500));
}

// ---- read_range ---------------------------------------------------------

struct FakeAudio {
    chunks: Mutex<HashMap<(RecordingId, Track, u32), Vec<i16>>>,
    removed: Mutex<Vec<RecordingId>>,
}

impl FakeAudio {
    fn new() -> Self {
        Self { chunks: Mutex::new(HashMap::new()), removed: Mutex::new(Vec::new()) }
    }

    fn put(&self, id: RecordingId, track: Track, seq: u32, samples: Vec<i16>) {
        self.chunks.lock().unwrap().insert((id, track, seq), samples);
    }
}

impl AudioSource for FakeAudio {
    fn read_chunk(&self, id: RecordingId, track: Track, seq: u32) -> CommandResult<Vec<i16>> {
        self.chunks
            .lock()
            .unwrap()
            .get(&(id, track, seq))
            .cloned()
            .ok_or_else(|| CommandError::new(codes::NOT_FOUND, "no such chunk"))
    }

    fn remove_audio(&self, id: RecordingId) -> CommandResult<()> {
        self.removed.lock().unwrap().push(id);
        self.chunks.lock().unwrap().retain(|(rid, _, _), _| *rid != id);
        Ok(())
    }
}

fn meta(track: Track, seq: u32, start_ms: u64, dur_ms: u64) -> ChunkMeta {
    ChunkMeta { track, seq, start_ms, samples: ms_to_samples(dur_ms) as u64, transcribed: false }
}

#[test]
fn read_range_spans_two_chunks() {
    let id = RecordingId::new();
    let audio = FakeAudio::new();
    audio.put(id, Track::System, 0, samples(ms_to_samples(30_000)));
    audio.put(id, Track::System, 1, samples(ms_to_samples(30_000)));
    let chunks = vec![meta(Track::System, 0, 0, 30_000), meta(Track::System, 1, 30_000, 30_000)];
    // A range spanning the boundary at 30s.
    let out = read_range(&audio, id, Track::System, &chunks, 29_000, 31_000).unwrap();
    assert_eq!(out.len(), ms_to_samples(2_000));
}

#[test]
fn read_range_ignores_chunks_of_the_other_track() {
    let id = RecordingId::new();
    let audio = FakeAudio::new();
    audio.put(id, Track::Mic, 0, samples(ms_to_samples(10_000)));
    let chunks = vec![meta(Track::Mic, 0, 0, 10_000)];
    let out = read_range(&audio, id, Track::System, &chunks, 0, 5_000).unwrap();
    assert!(out.is_empty());
}

// ---- candidates_for / owner_addresses -----------------------------------

fn event_with(attendees: Vec<&str>) -> EventRef {
    EventRef {
        calendar_id: everyday_core::CalendarId::new(),
        uid: "u1".into(),
        title: "Design sync".into(),
        start: "2026-09-16T14:00:00Z".parse().unwrap(),
        end: "2026-09-16T14:30:00Z".parse().unwrap(),
        tz: "Europe/London".into(),
        organizer: String::new(),
        attendees: attendees.into_iter().map(String::from).collect(),
        join_url: String::new(),
        calendar_name: "Work".into(),
        series: None,
    }
}

#[test]
fn candidates_exclude_the_owners_own_address() {
    let event = event_with(vec!["Priya Raman <priya@x.com>", "me@example.com"]);
    let mut owner = std::collections::HashSet::new();
    owner.insert("me@example.com".to_string());
    let candidates = candidates_for(Some(&event), &[], false, "model", &owner);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].attendee, "Priya Raman <priya@x.com>");
}

#[test]
fn no_event_means_no_candidates() {
    assert!(candidates_for(None, &[], false, "model", &Default::default()).is_empty());
}

fn voiceprint(name: &str, email: Option<&str>, model: &str) -> Voiceprint {
    Voiceprint {
        id: everyday_core::VoiceprintId::new(),
        name: name.to_string(),
        email: email.map(String::from),
        is_owner: false,
        model: model.to_string(),
        centroids: vec![vec![1.0, 0.0]],
        samples: 1,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    }
}

#[test]
fn a_voiceprint_from_a_different_model_is_never_matched() {
    let vp = voiceprint("Priya", Some("priya@x.com"), "old-model");
    let found = find_voiceprint("Priya Raman <priya@x.com>", &[vp], "new-model");
    assert!(found.is_none());
}

#[test]
fn a_voiceprint_matches_by_email_then_falls_back_to_name() {
    let by_email = voiceprint("Priya R", Some("priya@x.com"), "m");
    let found = find_voiceprint("Priya Raman <priya@x.com>", &[by_email], "m");
    assert_eq!(found.unwrap().name, "Priya R");

    let by_name = voiceprint("Bob", None, "m");
    let found = find_voiceprint("Bob", &[by_name], "m");
    assert_eq!(found.unwrap().name, "Bob");
}

// ---- pipeline stage progression, against a fake transcriber and audio --

struct FakeTranscriber {
    limits: Limits,
    diarises: bool,
    reply: String,
}

impl Transcriber for FakeTranscriber {
    fn limits(&self) -> Limits {
        self.limits
    }

    fn diarises(&self) -> bool {
        self.diarises
    }

    fn transcribe<'a>(
        &'a self,
        chunk: &'a SpeechChunk,
        _hints: &'a Hints,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
        let reply = self.reply.clone();
        let duration = chunk.duration_ms();
        Box::pin(async move {
            Ok(vec![RawSegment { start_ms: 0, end_ms: duration, text: reply, speaker_hint: None }])
        })
    }
}

struct FakeSummariser;
impl Summariser for FakeSummariser {
    fn ask<'a>(
        &'a self,
        _system: &'a str,
        _user: &'a str,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move {
            Ok("# Design sync\n\n## Summary\nWent well.\n\n## Decisions\nNone.\n\n## Action items\nNone.\n\n## Open questions\nNone.\n".to_string())
        })
    }
}

struct FailingSummariser;
impl Summariser for FailingSummariser {
    fn ask<'a>(
        &'a self,
        _system: &'a str,
        _user: &'a str,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move { Err(CommandError::new(codes::AGENT, "the assistant is not usable")) })
    }
}

fn test_vault() -> (tempfile::TempDir, Arc<Vault>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = everyday_core::VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = everyday_vault::create(dir.path(), cfg).unwrap();
    (dir, Arc::new(vault))
}

fn seed_recording(
    vault: &Vault,
    audio: &FakeAudio,
    mic_chunks: u32,
    system_chunks: u32,
) -> RecordingId {
    let mut settings = vault.meeting_settings().unwrap();
    settings.transcriber = Some(TranscriberConfig::Compatible {
        base_url: "http://127.0.0.1:1/v1".into(),
        model: "whisper-1".into(),
    });
    settings.voiceprints = false;
    vault.save_meeting_settings(&settings).unwrap();

    let event = EventRef {
        calendar_id: everyday_core::CalendarId::new(),
        uid: "u1".into(),
        title: "Design sync".into(),
        start: "2026-09-16T14:00:00Z".parse().unwrap(),
        end: "2026-09-16T14:30:00Z".parse().unwrap(),
        tz: "Europe/London".into(),
        organizer: String::new(),
        attendees: vec!["Priya Raman <priya@example.com>".into()],
        join_url: String::new(),
        calendar_name: "Work".into(),
        series: None,
    };
    let mut recording = Recording::new("Design sync", Some(event), settings.template(None).id);
    recording.stage = Stage::Transcribing;
    for seq in 0..mic_chunks {
        audio.put(recording.id, Track::Mic, seq, samples(ms_to_samples(2_000)));
        recording.chunks.push(meta(Track::Mic, seq, u64::from(seq) * 30_000, 2_000));
    }
    for seq in 0..system_chunks {
        audio.put(recording.id, Track::System, seq, samples(ms_to_samples(2_000)));
        recording.chunks.push(meta(Track::System, seq, u64::from(seq) * 30_000, 2_000));
    }
    vault.save_recording(&recording).unwrap();
    recording.id
}

#[tokio::test]
async fn full_progression_reaches_done_with_a_note_and_a_transcript() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 1);

    let source: Arc<dyn AudioSource> = audio.clone();
    let transcriber = FakeTranscriber {
        limits: Limits { max_bytes: 100_000_000, max_seconds: 3_600 },
        diarises: false,
        reply: "hello there".into(),
    };
    do_transcribe_with(&vault, &source, id, &transcriber).await.unwrap();

    let recording = vault.recording(id).unwrap();
    assert_eq!(recording.stage, Stage::Identifying);
    assert!(recording.chunks.iter().all(|c| c.transcribed));

    do_identify(&vault, &source, id).await.unwrap();
    let recording = vault.recording(id).unwrap();
    assert_eq!(recording.stage, Stage::Summarising);

    let events = Arc::new(crate::events::Silent);
    do_summarise_and_write(&vault, &source, id, &FakeSummariser, events.as_ref()).await.unwrap();

    let recording = vault.recording(id).unwrap();
    assert_eq!(recording.stage, Stage::Done);
    assert!(recording.note_id.is_some());
    assert!(recording.chunks.is_empty());

    let note = vault.note(recording.note_id.unwrap()).unwrap();
    assert!(note.body.to_markdown().contains("**When:**"), "{}", note.body.to_markdown());
    assert!(note.body.to_markdown().contains("## Summary"));

    let transcript = vault.transcript_for_note(recording.note_id.unwrap()).unwrap();
    assert!(transcript.is_some());

    assert_eq!(audio.removed.lock().unwrap().as_slice(), &[id]);
}

#[tokio::test]
async fn retrying_transcription_skips_chunks_already_marked_done() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 0, 1);
    let source: Arc<dyn AudioSource> = audio.clone();

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    struct Counting {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Transcriber for Counting {
        fn limits(&self) -> Limits {
            Limits { max_bytes: 100_000_000, max_seconds: 3_600 }
        }
        fn diarises(&self) -> bool {
            false
        }
        fn transcribe<'a>(
            &'a self,
            chunk: &'a SpeechChunk,
            _hints: &'a Hints,
        ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let duration = chunk.duration_ms();
            Box::pin(async move {
                Ok(vec![RawSegment {
                    start_ms: 0,
                    end_ms: duration,
                    text: "hi".into(),
                    speaker_hint: None,
                }])
            })
        }
    }
    let t = Counting { calls: calls.clone() };
    do_transcribe_with(&vault, &source, id, &t).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Reset stage and retry: the already-transcribed chunk must not be
    // sent again.
    let mut recording = vault.recording(id).unwrap();
    recording.stage = Stage::Transcribing;
    vault.save_recording(&recording).unwrap();
    do_transcribe_with(&vault, &source, id, &t).await.unwrap();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no new calls: the chunk was already transcribed"
    );
}

#[tokio::test]
async fn a_summariser_failure_leaves_the_recording_failed_with_the_spool_kept() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 1);
    let source: Arc<dyn AudioSource> = audio.clone();

    let transcriber = FakeTranscriber {
        limits: Limits { max_bytes: 100_000_000, max_seconds: 3_600 },
        diarises: false,
        reply: "hi".into(),
    };
    do_transcribe_with(&vault, &source, id, &transcriber).await.unwrap();
    do_identify(&vault, &source, id).await.unwrap();

    let events = Arc::new(crate::events::Silent);
    let err = do_summarise_and_write(&vault, &source, id, &FailingSummariser, events.as_ref())
        .await
        .unwrap_err();
    fail(&vault, id, Stage::Summarising, &err, events.as_ref()).await.unwrap();

    let recording = vault.recording(id).unwrap();
    match &recording.stage {
        Stage::Failed { at, reason } => {
            assert_eq!(**at, Stage::Summarising);
            assert!(!reason.is_empty());
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // The spool is kept on failure: nothing here called `remove_audio`.
    assert!(audio.removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn echo_is_removed_before_identification() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 1);
    let mut recording = vault.recording(id).unwrap();
    recording.partial.push(TrackSegment {
        track: Track::System,
        start_ms: 1_000,
        end_ms: 3_000,
        text: "let's ship on Friday".into(),
        speaker_hint: None,
        hint_scope: 0,
    });
    recording.partial.push(TrackSegment {
        track: Track::Mic,
        start_ms: 1_200,
        end_ms: 3_100,
        text: "let's ship on Friday".into(),
        speaker_hint: None,
        hint_scope: 0,
    });
    vault.save_recording(&recording).unwrap();

    let source: Arc<dyn AudioSource> = audio.clone();
    let settings = vault.meeting_settings().unwrap();
    let identified = identify_stage(
        &vault,
        &source,
        &recording,
        &settings,
        "test-model",
        #[cfg(feature = "speech")]
        None,
    )
    .await
    .unwrap();
    // The mic echo was dropped: only the system segment's text survives.
    assert_eq!(identified.segments.iter().filter(|s| s.text.contains("ship on Friday")).count(), 1);
}

#[tokio::test]
async fn a_calendars_role_becomes_the_notes_purpose() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    let mut recording = vault.recording(id).unwrap();
    let calendar_id = recording.event.as_ref().unwrap().calendar_id;
    let mut calendar = everyday_core::Calendar::subscribed("Work", "https://example.com/w.ics");
    calendar.id = calendar_id;
    let role = everyday_core::RoleId::new();
    calendar.role_id = Some(role);
    vault.save_calendar(&calendar).unwrap();

    recording.stage = Stage::Summarising;
    vault.save_recording(&recording).unwrap();

    let source: Arc<dyn AudioSource> = audio.clone();
    let events = Arc::new(crate::events::Silent);
    do_summarise_and_write(&vault, &source, id, &FakeSummariser, events.as_ref()).await.unwrap();

    let recording = vault.recording(id).unwrap();
    let note = vault.note(recording.note_id.unwrap()).unwrap();
    assert_eq!(note.purpose, Some(everyday_core::Purpose::Role { id: role }));
}

/// Wraps [`transcribe_stage`] the way [`do_transcribe`] does, minus the
/// settings lookup -- tests hand it a fake transcriber directly rather
/// than building one from `MeetingSettings::transcriber`.
async fn do_transcribe_with(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    transcriber: &dyn Transcriber,
) -> CommandResult<()> {
    let hints = Hints::default();
    transcribe_stage(
        vault,
        source,
        transcriber,
        id,
        &hints,
        #[cfg(feature = "speech")]
        None,
    )
    .await?;
    let vault = vault.clone();
    blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, |recording| {
            recording.stage = Stage::Identifying;
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await
    .map(|_| ())
}

// ---- transient vs permanent errors, and the bounded retry -------------

/// Fails its first `fails_left` calls with `code`, then succeeds -- for
/// proving [`retry_transient`] keeps going through a transient error and
/// stops the moment it clears.
struct FlakyTranscriber {
    code: &'static str,
    fails_left: AtomicU32,
    calls: AtomicU32,
}

impl Transcriber for FlakyTranscriber {
    fn limits(&self) -> Limits {
        Limits { max_bytes: 100_000_000, max_seconds: 3_600 }
    }
    fn diarises(&self) -> bool {
        false
    }
    fn transcribe<'a>(
        &'a self,
        chunk: &'a SpeechChunk,
        _hints: &'a Hints,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let duration = chunk.duration_ms();
        Box::pin(async move {
            let mut remaining = self.fails_left.load(Ordering::SeqCst);
            while remaining > 0 {
                match self.fails_left.compare_exchange(
                    remaining,
                    remaining - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return Err(CommandError::new(self.code, "not yet")),
                    Err(seen) => remaining = seen,
                }
            }
            Ok(vec![RawSegment {
                start_ms: 0,
                end_ms: duration,
                text: "hi".into(),
                speaker_hint: None,
            }])
        })
    }
}

/// Always fails with `code` -- for proving a permanent error is never
/// retried, and that a transient one which never clears still gives up
/// once its budget is spent rather than retrying for ever.
struct AlwaysFails {
    code: &'static str,
    calls: AtomicU32,
}

impl Transcriber for AlwaysFails {
    fn limits(&self) -> Limits {
        Limits { max_bytes: 100_000_000, max_seconds: 3_600 }
    }
    fn diarises(&self) -> bool {
        false
    }
    fn transcribe<'a>(
        &'a self,
        _chunk: &'a SpeechChunk,
        _hints: &'a Hints,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Err(CommandError::new(self.code, "still no")) })
    }
}

#[tokio::test]
async fn a_transient_error_is_retried_until_it_clears() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    let source: Arc<dyn AudioSource> = audio.clone();
    let transcriber = FlakyTranscriber {
        code: codes::NETWORK,
        fails_left: AtomicU32::new(2),
        calls: AtomicU32::new(0),
    };

    let result = retry_transient_from(
        || do_transcribe_with(&vault, &source, id, &transcriber),
        Duration::from_millis(1),
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        transcriber.calls.load(Ordering::SeqCst),
        3,
        "two transient failures, then a third attempt that succeeded"
    );
    let recording = vault.recording(id).unwrap();
    assert_eq!(
        recording.stage,
        Stage::Identifying,
        "the stage still advanced once retried through"
    );
}

#[tokio::test]
async fn a_permanent_error_is_never_retried() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    let source: Arc<dyn AudioSource> = audio.clone();
    let transcriber = AlwaysFails { code: codes::FORBIDDEN, calls: AtomicU32::new(0) };

    let result = retry_transient_from(
        || do_transcribe_with(&vault, &source, id, &transcriber),
        Duration::from_millis(1),
    )
    .await;

    let err = result.unwrap_err();
    assert_eq!(err.code, codes::FORBIDDEN);
    assert_eq!(
        transcriber.calls.load(Ordering::SeqCst),
        1,
        "a permanent error must fail on the very first attempt, never retried"
    );
}

#[tokio::test]
async fn a_transient_error_that_never_clears_still_gives_up_eventually() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    let source: Arc<dyn AudioSource> = audio.clone();
    let transcriber = AlwaysFails { code: codes::NETWORK, calls: AtomicU32::new(0) };

    let result = retry_transient_from(
        || do_transcribe_with(&vault, &source, id, &transcriber),
        Duration::from_millis(1),
    )
    .await;

    let err = result.unwrap_err();
    assert_eq!(err.code, codes::NETWORK);
    assert_eq!(
        transcriber.calls.load(Ordering::SeqCst),
        TRANSIENT_RETRIES,
        "bounded: a transient error that never clears must not be retried for ever"
    );
}

// ---- the same classification against a real transcriber over HTTP -----

async fn spawn_flaky_server(
    fail_times: usize,
    status: axum::http::StatusCode,
) -> (String, Arc<AtomicU32>) {
    let calls = Arc::new(AtomicU32::new(0));
    let calls_for_route = calls.clone();
    let app = axum::Router::new().route(
        "/audio/transcriptions",
        axum::routing::post(move |_body: axum::body::Bytes| {
            let calls = calls_for_route.clone();
            async move {
                use axum::response::IntoResponse;
                let n = calls.fetch_add(1, Ordering::SeqCst) as usize;
                if n < fail_times {
                    (status, "server trouble").into_response()
                } else {
                    axum::Json(serde_json::json!({"text": "hello from the fake server"}))
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), calls)
}

/// The same retry this module promises, proven against the real
/// [`build_transcriber`]/`do_transcribe` path rather than a fake
/// `Transcriber` -- a 500 is exactly what `OpenAiTranscriber::status_error`
/// classifies as [`codes::NETWORK`], so this is what a real, flaky
/// compatible server looks like from here.
#[tokio::test]
async fn a_real_transcribers_500_is_retried_and_the_recording_is_never_failed() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    test_hooks::skip_vad(id);
    let (base, calls) = spawn_flaky_server(2, axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
    let mut settings = vault.meeting_settings().unwrap();
    settings.transcriber =
        Some(TranscriberConfig::Compatible { base_url: base, model: "whisper-1".into() });
    vault.save_meeting_settings(&settings).unwrap();
    let source: Arc<dyn AudioSource> = audio.clone();

    let result =
        retry_transient_from(|| do_transcribe(&vault, &source, id), Duration::from_millis(1)).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "two 500s, then a third request that succeeded");
    let recording = vault.recording(id).unwrap();
    assert_eq!(recording.stage, Stage::Identifying);
}

/// The permanent half of the same proof: a 401 is
/// `OpenAiTranscriber::status_error`'s [`codes::FORBIDDEN`], and must
/// cost exactly one request.
#[tokio::test]
async fn a_real_transcribers_401_is_never_retried() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    test_hooks::skip_vad(id);
    let (base, calls) = spawn_flaky_server(usize::MAX, axum::http::StatusCode::UNAUTHORIZED).await;
    let mut settings = vault.meeting_settings().unwrap();
    settings.transcriber =
        Some(TranscriberConfig::Compatible { base_url: base, model: "whisper-1".into() });
    vault.save_meeting_settings(&settings).unwrap();
    let source: Arc<dyn AudioSource> = audio.clone();

    let result =
        retry_transient_from(|| do_transcribe(&vault, &source, id), Duration::from_millis(1)).await;

    let err = result.unwrap_err();
    assert_eq!(err.code, codes::FORBIDDEN);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---- early transcription while a call is still live --------------------

/// A local, immediate fake server -- `spawn_flaky_server(0, ..)` always
/// succeeds, standing in for a healthy remote transcriber that early
/// transcription talks to while a call is still going.
async fn spawn_ok_server() -> String {
    spawn_flaky_server(0, axum::http::StatusCode::OK).await.0
}

/// The regression this finding is about: before `run_recording`'s
/// `Stage::Recording` arm ran early transcription, a chunk closed
/// mid-call was never transcribed until the call ended -- see
/// `chunk_closed`'s own doc. Proven against [`transcribe_pending`], the
/// shared engine [`transcribe_live`] calls once its own remote check has
/// passed: every base URL reachable from inside a test is loopback (see
/// `test_hooks`'s own doc), so `transcribe_live` itself is exercised
/// separately, by `early_transcription_does_nothing_for_a_local_transcriber`
/// below and by the supervisor-race test, rather than by asserting on a
/// remote check this process cannot genuinely satisfy.
#[tokio::test]
async fn early_transcription_marks_chunks_transcribed_without_advancing_the_stage() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    test_hooks::skip_vad(id);
    // `seed_recording` leaves the row at `Stage::Transcribing`; put it
    // back to `Stage::Recording`, the state a still-live call is
    // actually in when `chunk_closed` fires.
    let mut recording = vault.recording(id).unwrap();
    recording.stage = Stage::Recording;
    vault.save_recording(&recording).unwrap();

    let base = spawn_ok_server().await;
    let mut settings = vault.meeting_settings().unwrap();
    settings.transcriber =
        Some(TranscriberConfig::Compatible { base_url: base, model: "whisper-1".into() });
    vault.save_meeting_settings(&settings).unwrap();
    let source: Arc<dyn AudioSource> = audio.clone();

    transcribe_pending(&vault, &source, id).await.unwrap();

    let recording = vault.recording(id).unwrap();
    assert_eq!(recording.stage, Stage::Recording, "the call is still live; nothing advances it");
    assert!(
        recording.chunks.iter().all(|c| c.transcribed),
        "the closed chunk was transcribed early"
    );
    assert!(!recording.partial.is_empty(), "the early transcript text was saved");
}

/// The other half of `chunk_closed`'s own gate: a local transcriber is
/// never asked to do this early -- see that function's doc on why, and
/// `transcribe_live`'s own doc on re-checking rather than trusting the
/// caller.
#[tokio::test]
async fn early_transcription_does_nothing_for_a_local_transcriber() {
    let (_dir, vault) = test_vault();
    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    let mut recording = vault.recording(id).unwrap();
    recording.stage = Stage::Recording;
    vault.save_recording(&recording).unwrap();

    let mut settings = vault.meeting_settings().unwrap();
    settings.transcriber =
        Some(TranscriberConfig::Local { model: everyday_core::meeting::LocalModel::ParakeetV3 });
    vault.save_meeting_settings(&settings).unwrap();
    let source: Arc<dyn AudioSource> = audio.clone();

    transcribe_live(&vault, &source, id).await.unwrap();

    let recording = vault.recording(id).unwrap();
    assert!(
        recording.chunks.iter().all(|c| !c.transcribed),
        "a local transcriber must never be asked to transcribe early"
    );
}

// ---- the supervisor race: `finish` landing while an early attempt is --
// ---- still mid-flight ---------------------------------------------------

/// The same shape [`enqueue`] gives the supervisor, with `summariser`
/// injected directly instead of built from `vault.agent_settings()` --
/// this test's own seam for driving the real `Supervisor::ensure`/
/// `chunk_closed`/`finish` wiring (and the race
/// `supervisor::Entry::rerun_requested` closes) all the way to
/// `Stage::Done` without a real assistant answering. Everything else is
/// exactly what `enqueue` itself does.
fn enqueue_test(
    svc: &Arc<Service>,
    vault: Arc<Vault>,
    source: Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: Arc<dyn Summariser>,
) {
    let events = svc.events();
    svc.supervisor().ensure(task_key(id), move |_stop| {
        let vault = vault.clone();
        let source = source.clone();
        let summariser = summariser.clone();
        let events = events.clone();
        Box::pin(async move {
            match run_recording(vault, source, id, Some(summariser), events).await {
                Ok(()) => Ok(Outcome::Done),
                Err(e) => {
                    Err(Box::new(std::io::Error::other(e.message)) as crate::supervisor::TaskError)
                }
            }
        }) as crate::supervisor::TaskFuture
    });
}

/// Poll `f` against real wall-clock time rather than a virtual one --
/// this test drives real scheduling (and a real TCP round trip over
/// loopback), which a paused clock cannot stand in for.
async fn settle_real(f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(f(), "did not settle");
}

/// The regression finding 2 is about: `chunk_closed` enqueues the
/// pipeline for a still-live recording; that early attempt reaches
/// `Stage::Recording`'s early transcription and is genuinely still
/// running -- parked on `test_hooks`'s gate, standing in for the network
/// call a remote transcriber would actually be mid-flight on -- the
/// instant `finish` lands, simulated here by flipping the stage and
/// calling `enqueue` again exactly as `spool::finish` itself does.
/// Before `Entry::rerun_requested` existed, the supervisor's own
/// idempotency check silently dropped that second `enqueue`, and the
/// recording sat in `Stage::Transcribing` for good (until an unrelated
/// unlock's `recover` swept it up). Now it reaches `Stage::Done` here,
/// in the same attempt, once the gate is released.
#[tokio::test]
async fn finish_landing_while_the_early_attempt_is_still_running_still_reaches_done() {
    // Built directly, rather than through `test_vault`, so the vault is
    // owned by a real `Service` from the start -- this test needs
    // `svc.supervisor()`, which `test_vault`'s bare `Arc<Vault>` has no
    // way to reach.
    let _dir = tempfile::tempdir().unwrap();
    let cfg = everyday_core::VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = everyday_vault::create(_dir.path(), cfg).unwrap();
    let svc = Arc::new(Service::new());
    let vault = svc.set(vault);

    let audio = Arc::new(FakeAudio::new());
    let id = seed_recording(&vault, &audio, 1, 0);
    let mut recording = vault.recording(id).unwrap();
    recording.stage = Stage::Recording;
    vault.save_recording(&recording).unwrap();

    // A real, immediate fake server: `transcribe_live` will not reach
    // it (loopback reads as local, not remote -- see `test_hooks`'s own
    // doc), but `do_transcribe` will, for real, once the recording is
    // woken back up at `Stage::Transcribing` below.
    let base = spawn_ok_server().await;
    let mut settings = vault.meeting_settings().unwrap();
    settings.transcriber =
        Some(TranscriberConfig::Compatible { base_url: base, model: "whisper-1".into() });
    vault.save_meeting_settings(&settings).unwrap();

    let source: Arc<dyn AudioSource> = audio.clone();
    let gate = test_hooks::arm(id);

    // Stands in for `chunk_closed`'s own `enqueue`: the call is still
    // live, so this attempt reaches `Stage::Recording`'s early
    // transcription and, immediately after, parks on the gate.
    enqueue_test(&svc, vault.clone(), source.clone(), id, Arc::new(FakeSummariser));
    settle_real(|| gate.reached()).await;

    // Stands in for `spool::finish`: the call has now genuinely ended,
    // while the early attempt above is still parked, its own `handle`
    // still registered with the supervisor.
    crate::meeting::spool::mutate_recording(&vault, id, |r| {
        r.stage = Stage::Transcribing;
        r.ended_at = Some(Timestamp::now());
        Ok(())
    })
    .unwrap();
    enqueue_test(&svc, vault.clone(), source.clone(), id, Arc::new(FakeSummariser));

    // Let the parked attempt notice it has been asked to run again
    // rather than simply retiring.
    gate.release();

    settle_real(|| matches!(vault.recording(id).map(|r| r.stage), Ok(Stage::Done))).await;
    let recording = vault.recording(id).unwrap();
    assert_eq!(recording.stage, Stage::Done);
    assert!(recording.note_id.is_some());
}
