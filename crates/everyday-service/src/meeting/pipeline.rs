//! The pipeline: one supervised task per recording, from spooled audio to a
//! written note. See `docs/plans/meeting-notes.md`, "The shape of it", "Who
//! said what", "Echo" and "Summarising".
//!
//! # Shape
//!
//! [`enqueue`] and [`chunk_closed`] are the seam the spool calls through --
//! see their own docs. Both, ultimately, ask [`everyday_service::supervisor`]
//! for one task keyed `"meeting:{id}"`; the supervisor's own restart-with-
//! backoff and stop-on-lock machinery (see that module's doc) is what gives
//! this "stops when the vault locks, resumes on unlock" for free, the same
//! way it already does for one mail account's sync task. A transient error
//! talking to a transcriber or the assistant -- a network hiccup, a
//! timeout, a rate limit, a provider having a bad moment, see
//! [`is_transient`] -- is retried in place, with backoff, before this
//! module gives up on it (see [`retry_transient`]); a permanent one (a
//! refused key, a model the endpoint does not know, a chunk too large to
//! send) is never retried at all. Either way, once a stage's own retries
//! are exhausted the recording is marked [`Stage::Failed`] and `Err` is
//! returned so the supervisor stops asking for it; reaching [`Stage::Done`]
//! or [`Stage::Failed`] is reported as [`supervisor::Outcome::Done`],
//! because there is nothing further for that key to do until a retry or a
//! discard re-`enqueue`s it.
//!
//! # The stages
//!
//! [`Stage::Transcribing`] → [`Stage::Identifying`] → [`Stage::Summarising`]
//! → [`Stage::Done`], driven by [`run_recording`]. Each stage saves the
//! recording as it finishes its own work (not before, and not batched with
//! the next), so a crash or a lock mid-pipeline resumes exactly where it
//! left off rather than repeating a stage that already succeeded -- the same
//! argument `ChunkMeta::transcribed` makes at the finer grain of one chunk.
//!
//! # Two seams for testability
//!
//! [`AudioSource`] and [`Summariser`] are the two places this module would
//! otherwise reach for a socket or a decrypted file it cannot get in a unit
//! test. Both are traits with a production implementation and a fake one;
//! see their own docs. [`transcribe::Transcriber`] is already exactly this
//! shape (built elsewhere in this crate), so it needed no seam of its own --
//! tests hand `run_recording` a fake one directly.
//!
//! # One seam, two implementations
//!
//! [`SpoolSource`] is the thin, production [`AudioSource`]: `read_chunk` and
//! `remove_audio` call straight through to `meeting::spool`'s own functions
//! of the same name. It is wired into [`enqueue`] and [`chunk_closed`], but
//! nothing in this crate's own tests exercises it directly -- they all hand
//! `run_recording` [`FakeAudio`] instead, so a stage's logic is tested
//! without a real spool directory on disk.
//!
//! # Saving a recording mid-pipeline
//!
//! Every save this module makes of a recording's row goes through
//! `meeting::spool::mutate_recording`, not a bare `vault.recording` /
//! `vault.save_recording` pair -- the same per-`RecordingId` lock `spool`'s
//! own `append` and `finish` hold for theirs, so a chunk landing while this
//! module is mid-stage can never be dropped by, or drop, a save this module
//! makes at the same moment. [`transcribe_stage`] re-reads the chunk list
//! under that lock on every pass for exactly this reason -- see its own doc.
//!
//! # `Stage::Done` is never overwritten
//!
//! [`do_summarise_and_write`] commits the note, the transcript and the
//! recording's own `Stage::Done` row together, then clears the spool as a
//! last, separate step. If that last step fails, the note already exists --
//! so the failure is logged and swallowed rather than returned, which
//! would otherwise reach [`fail`] and overwrite the `Done` row with
//! `Failed { at: Summarising }`, orphaning the note a retry would then
//! duplicate. [`fail`] itself refuses to touch a `Done` row regardless, as
//! a second line of defence. See [`do_summarise_and_write`]'s own doc for
//! the leftover spool directory this leaves, and
//! `spool::expire_failed_tick`'s `sweep_orphaned_spool` for what clears it.

use std::sync::Arc;
use std::time::Duration;

use everyday_core::meeting::identify::{self, Candidate, Turn};
use everyday_core::meeting::template::Facts;
use everyday_core::meeting::{
    Attribution, ChunkMeta, EventRef, MeetingSettings, Recording, SAMPLE_RATE, Segment, Speaker,
    Stage, Track, TrackSegment, TranscriberConfig, Transcript, Voiceprint,
};
use everyday_core::{Note, RecordingId, TranscriptId, Vault};
use jiff::Timestamp;
use rig_agent::AgentBuilder;
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::prelude::*;

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Notification, Op};
use crate::meeting::transcribe::{Hints, Limits, RawSegment, SpeechChunk, Transcriber};
use crate::service::{Service, blocking};
use crate::supervisor::Outcome;

#[cfg(feature = "speech")]
use crate::meeting::speech::{self, SpeechKit};

// ============================================================================
// The AudioSource seam
// ============================================================================

/// What the pipeline needs from the spool: the samples of one sealed chunk,
/// and permission to erase every chunk of one recording once its note
/// exists. Sync rather than `async` -- both are, in the production
/// implementation, a decrypt off disk, not a network call -- so a caller on
/// the async runtime is the one responsible for running it inside
/// [`blocking`], the same discipline [`crate::meeting::speech`] asks of its
/// own callers.
///
/// Two implementations: [`SpoolSource`], production, a thin adapter over
/// `meeting::spool`; and `FakeAudio`, in [`tests`], an in-memory map used by
/// every test in this file. Neither the transcribing nor the identifying
/// stage below ever names `meeting::spool` directly -- only this trait.
pub trait AudioSource: Send + Sync {
    fn read_chunk(&self, id: RecordingId, track: Track, seq: u32) -> CommandResult<Vec<i16>>;
    fn remove_audio(&self, id: RecordingId) -> CommandResult<()>;
}

/// The production [`AudioSource`]: a thin adapter over `meeting::spool`,
/// which owns the sealed chunk files on disk -- see that module's own doc.
pub struct SpoolSource {
    vault: Arc<Vault>,
}

impl SpoolSource {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }
}

impl AudioSource for SpoolSource {
    fn read_chunk(&self, id: RecordingId, track: Track, seq: u32) -> CommandResult<Vec<i16>> {
        crate::meeting::spool::read_chunk(&self.vault, id, track, seq)
    }

    fn remove_audio(&self, id: RecordingId) -> CommandResult<()> {
        crate::meeting::spool::remove_audio(&self.vault, id)
    }
}

// ============================================================================
// The Summariser seam
// ============================================================================

/// One call to the assistant's model: a system prompt and a user message in,
/// prose out. What [`summarise_stage`] is built against, so a test never
/// needs a network or an API key -- see [`tests::FakeSummariser`].
///
/// Not `async_trait`: this crate has no dependency on it, and the same
/// hand-written `BoxFuture` shape [`crate::meeting::transcribe::Transcriber`]
/// already uses is one pattern fewer to learn.
pub trait Summariser: Send + Sync {
    fn ask<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<String>>;
}

/// How long one summarising request may run before it is abandoned. Three
/// minutes, per the plan: a call transcript is a long document and the
/// assistant's own model may be a slow one somebody chose on purpose, but a
/// note that has not arrived by then has stopped being useful to wait for.
const SUMMARISE_TIMEOUT: Duration = Duration::from_secs(180);

/// The real [`Summariser`]: the assistant's own provider and model --
/// [`everyday_core::agent::AgentSettings::assistant_model`], never the quick
/// model, per the plan's "Summaries use the assistant's model, not the quick
/// model". No tools are registered: a summary is one piece of the transcript
/// in and one piece of prose out, not a turn that should be reading the
/// vault.
pub struct AssistantSummariser {
    vault: Arc<Vault>,
}

impl AssistantSummariser {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }

    async fn attempt(&self, system: &str, user: &str) -> CommandResult<String> {
        let (settings, key) = self.vault.agent_credentials().map_err(|e| {
            CommandError::new(codes::AGENT, format!("the assistant is not usable: {e}"))
        })?;
        let model = settings.assistant_model.clone();
        let client = crate::llm::client(&settings.provider_config, key).map_err(|e| {
            CommandError::new(codes::AGENT, format!("could not start the assistant: {e}"))
        })?;
        let builder = AgentBuilder::new(client.completion_model(&model.model)).preamble(system);
        let builder = crate::llm::configure(builder, &model);
        let agent = builder.build();

        match tokio::time::timeout(SUMMARISE_TIMEOUT, agent.prompt(user).max_turns(1)).await {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => {
                Err(CommandError::new(codes::AGENT, crate::llm::friendly(&e.to_string())))
            }
            Err(_) => Err(CommandError::new(
                codes::TIMED_OUT,
                format!(
                    "the assistant took longer than {}s to summarise",
                    SUMMARISE_TIMEOUT.as_secs()
                ),
            )),
        }
    }
}

/// Whether `message` looks like a transport failure rather than the model
/// itself refusing the request -- the one case the plan's "one retry on
/// network error" applies to. Mirrors the substrings
/// [`crate::llm::friendly`] already keys on for "could not reach the model",
/// rather than depending on it directly: `friendly` turns the error into a
/// sentence for a person, and this has to see the raw text first.
fn looks_like_network_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("connection refused")
        || lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("timed out")
        || lower.contains("timeout")
}

impl Summariser for AssistantSummariser {
    fn ask<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move {
            match self.attempt(system, user).await {
                Ok(text) => Ok(text),
                Err(e) if looks_like_network_error(&e.message) => self.attempt(system, user).await,
                Err(e) => Err(e),
            }
        })
    }
}

// ============================================================================
// Building a transcriber from settings
// ============================================================================

/// `https://api.openai.com/v1`, per the plan's own words for it.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Build the configured transcriber. The one place `TranscriberConfig` turns
/// into a live [`Transcriber`] -- used by the pipeline and by
/// `domains::transcripts::test_transcriber` alike, so the two can never
/// disagree about what "the configured transcriber" means.
pub fn build_transcriber(
    config: &TranscriberConfig,
    key: Option<String>,
) -> CommandResult<Box<dyn Transcriber>> {
    match config {
        TranscriberConfig::OpenAi { model } => {
            Ok(Box::new(crate::meeting::transcribe::openai::OpenAiTranscriber::new(
                OPENAI_BASE_URL,
                key,
                model.clone(),
            )))
        }
        TranscriberConfig::Compatible { base_url, model } => {
            Ok(Box::new(crate::meeting::transcribe::openai::OpenAiTranscriber::new(
                base_url.clone(),
                key,
                model.clone(),
            )))
        }
        TranscriberConfig::Google { model } => {
            let key = key.ok_or_else(|| {
                CommandError::new(codes::UNSUPPORTED, "a Google transcriber needs a key")
            })?;
            Ok(Box::new(crate::meeting::transcribe::gemini::GeminiTranscriber::new(
                key,
                model.clone(),
            )))
        }
        TranscriberConfig::Local { model } => build_local_transcriber(*model),
    }
}

#[cfg(feature = "speech")]
fn build_local_transcriber(
    model: everyday_core::meeting::LocalModel,
) -> CommandResult<Box<dyn Transcriber>> {
    Ok(Box::new(crate::meeting::transcribe::local::LocalTranscriber::new(model)))
}

#[cfg(not(feature = "speech"))]
fn build_local_transcriber(
    _model: everyday_core::meeting::LocalModel,
) -> CommandResult<Box<dyn Transcriber>> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}

// ============================================================================
// VAD grouping and the offset map back to recording time
// ============================================================================

/// Silence inserted between two turns folded into the same request, so a
/// recogniser has a real gap to key sentence boundaries off rather than two
/// turns run together. Not audio that was ever recorded -- purely a splice.
const GROUP_SILENCE_MS: u64 = 300;

/// Where one turn, once folded into a [`Grouped`] buffer, actually sits: its
/// own span inside that buffer, and where it really was in the recording.
/// What [`map_segment`] reads to translate a backend's answer back.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TurnMap {
    local_start_ms: u64,
    local_end_ms: u64,
    recording_start_ms: u64,
}

/// One request's worth of audio: turns concatenated up to the backend's own
/// [`Limits`], with the map [`map_segment`] needs to undo the concatenation
/// afterwards.
#[derive(Debug, Clone)]
struct Grouped {
    samples: Vec<i16>,
    map: Vec<TurnMap>,
}

fn index_of(ms: u64) -> usize {
    (ms * u64::from(SAMPLE_RATE) / 1000) as usize
}

fn ms_of(samples: usize) -> u64 {
    samples as u64 * 1000 / u64::from(SAMPLE_RATE)
}

/// Fold `turns` (chunk-relative `(start_ms, end_ms)` pairs, in order) out of
/// `samples` into as few [`Grouped`] buffers as `limits` allow, concatenating
/// consecutive turns with [`GROUP_SILENCE_MS`] of real silence between them.
///
/// `chunk_offset_ms` is where `samples` itself sits in the recording, so
/// every [`TurnMap::recording_start_ms`] this produces is already in the
/// recording's own clock, not the chunk's.
///
/// Pure and independent of `sherpa-onnx`, so it is tested directly against
/// synthetic turns rather than through a loaded VAD model -- see
/// `tests::group_turns_tests`. The single-turn, whole-chunk case (no VAD at
/// all, feature `speech` off) is the same function called with one turn
/// spanning the whole chunk, which is what keeps [`map_segment`] one
/// function for both paths rather than two.
fn group_turns(
    turns: &[(u64, u64)],
    samples: &[i16],
    chunk_offset_ms: u64,
    limits: Limits,
) -> Vec<Grouped> {
    if turns.is_empty() {
        return Vec::new();
    }
    let silence_samples = index_of(GROUP_SILENCE_MS);
    let max_ms = u64::from(limits.max_seconds) * 1000;
    // Two bytes per sample; the WAV header is a fixed, negligible 44 bytes
    // beside anything this is ever asked to group.
    let max_samples = limits.max_bytes / 2;

    let mut groups = Vec::new();
    let mut cur_samples: Vec<i16> = Vec::new();
    let mut cur_map: Vec<TurnMap> = Vec::new();
    let mut cur_ms: u64 = 0;

    for &(t_start, t_end) in turns {
        let lo = index_of(t_start).min(samples.len());
        let hi = index_of(t_end).min(samples.len());
        if lo >= hi {
            continue;
        }
        let turn_samples = &samples[lo..hi];
        let turn_ms = ms_of(turn_samples.len());
        let joining = !cur_samples.is_empty();
        let pad_ms = if joining { GROUP_SILENCE_MS } else { 0 };
        let projected_ms = cur_ms + pad_ms + turn_ms;
        let projected_samples =
            cur_samples.len() + (if joining { silence_samples } else { 0 }) + turn_samples.len();

        if joining && (projected_ms > max_ms || projected_samples > max_samples) {
            groups.push(Grouped {
                samples: std::mem::take(&mut cur_samples),
                map: std::mem::take(&mut cur_map),
            });
            cur_ms = 0;
        }
        if !cur_samples.is_empty() {
            cur_samples.extend(std::iter::repeat_n(0i16, silence_samples));
            cur_ms += GROUP_SILENCE_MS;
        }
        let local_start_ms = cur_ms;
        cur_samples.extend_from_slice(turn_samples);
        cur_ms += turn_ms;
        cur_map.push(TurnMap {
            local_start_ms,
            local_end_ms: cur_ms,
            recording_start_ms: chunk_offset_ms + t_start,
        });
    }
    if !cur_samples.is_empty() {
        groups.push(Grouped { samples: cur_samples, map: cur_map });
    }
    groups
}

/// Translate one backend segment's `(start_ms, end_ms)`, local to a
/// [`Grouped`] buffer, back into the recording's own clock, using the turn
/// whose span contains the segment's midpoint -- or, if a recogniser's own
/// segmentation drifted slightly past a turn's boundary, the turn nearest
/// it. `map` is never empty for a segment a real backend returned, since a
/// [`Grouped`] with no turns is never sent.
fn map_segment(map: &[TurnMap], local_start_ms: u64, local_end_ms: u64) -> (u64, u64) {
    let mid = local_start_ms + local_end_ms.saturating_sub(local_start_ms) / 2;
    let turn = map
        .iter()
        .find(|t| t.local_start_ms <= mid && mid < t.local_end_ms)
        .or_else(|| {
            map.iter().min_by_key(|t| {
                let d_start = mid.abs_diff(t.local_start_ms);
                let d_end = mid.abs_diff(t.local_end_ms);
                d_start.min(d_end)
            })
        })
        .expect("a Grouped buffer always carries at least one turn");
    let delta = local_start_ms.saturating_sub(turn.local_start_ms);
    let mapped_start = turn.recording_start_ms + delta;
    let duration = local_end_ms.saturating_sub(local_start_ms);
    (mapped_start, mapped_start + duration)
}

// ============================================================================
// Transcribing
// ============================================================================

/// Every 30-second chunk, cut into speech turns: with feature `speech`, real
/// VAD; without it, one turn spanning the whole chunk, so [`group_turns`]
/// and [`map_segment`] serve both paths.
fn turns_of(
    #[allow(unused_variables)] samples: &[i16],
    #[cfg(feature = "speech")] kit: Option<&SpeechKit>,
) -> Vec<(u64, u64)> {
    #[cfg(feature = "speech")]
    if let Some(kit) = kit {
        return kit.vad(samples);
    }
    if samples.is_empty() { Vec::new() } else { vec![(0, ms_of(samples.len()))] }
}

/// Transcribe every chunk not yet marked [`ChunkMeta::transcribed`], saving
/// the recording after each one so a retry never pays twice -- per the
/// plan's "Store results as `TrackSegment`s in `recording.partial`, mark
/// `ChunkMeta::transcribed`, save after each chunk".
///
/// Re-reads the chunk list itself on every pass through the loop, rather
/// than working off one snapshot taken at the top -- `spool::append` still
/// accepts a chunk after `finish` has moved the recording out of
/// `Stage::Recording` and into this one (see that function's own doc), and
/// this is what lets a pass already in progress notice a chunk that landed
/// moments after it started, instead of leaving it marked `transcribed:
/// false` for ever with nothing left in the pipeline that will ever look at
/// it again. Each iteration's "mark this chunk done" save goes through
/// `spool::mutate_recording`, matched by `(track, seq)` rather than by
/// index into whatever was last read, so it merges into whatever the row
/// looks like *now* -- including a chunk `append` added while this one
/// was being transcribed -- rather than overwriting it with a stale copy.
async fn transcribe_stage(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    transcriber: &dyn Transcriber,
    id: RecordingId,
    hints: &Hints,
    #[cfg(feature = "speech")] kit: Option<Arc<SpeechKit>>,
) -> CommandResult<()> {
    let limits = transcriber.limits();

    loop {
        let recording = {
            let vault = vault.clone();
            blocking(move || Ok(vault.recording(id)?)).await?
        };
        let Some(meta) = recording.chunks.iter().find(|c| !c.transcribed).cloned() else {
            return Ok(());
        };

        let source_for_read = source.clone();
        #[cfg(feature = "speech")]
        let kit_for_read = kit.clone();
        let meta_for_read = meta.clone();
        let groups = blocking(move || {
            let samples = source_for_read.read_chunk(id, meta_for_read.track, meta_for_read.seq)?;
            #[cfg(feature = "speech")]
            let turns = turns_of(&samples, kit_for_read.as_deref());
            #[cfg(not(feature = "speech"))]
            let turns = turns_of(&samples);
            Ok(group_turns(&turns, &samples, meta_for_read.start_ms, limits))
        })
        .await?;

        let mut new_segments = Vec::new();
        for (gi, group) in groups.iter().enumerate() {
            let speech_chunk = SpeechChunk {
                samples: group.samples.clone(),
                offset_ms: 0,
                scope: meta.seq * 1000 + gi as u32,
            };
            let raw: Vec<RawSegment> = transcriber.transcribe(&speech_chunk, hints).await?;
            for seg in raw {
                let (start_ms, end_ms) = map_segment(&group.map, seg.start_ms, seg.end_ms);
                new_segments.push(TrackSegment {
                    track: meta.track,
                    start_ms,
                    end_ms,
                    text: seg.text,
                    speaker_hint: seg.speaker_hint,
                    hint_scope: speech_chunk.scope,
                });
            }
        }

        let vault_for_save = vault.clone();
        let key = (meta.track, meta.seq);
        blocking(move || {
            crate::meeting::spool::mutate_recording(&vault_for_save, id, move |r| {
                if let Some(c) = r.chunks.iter_mut().find(|c| (c.track, c.seq) == key) {
                    c.transcribed = true;
                }
                r.partial.extend(new_segments);
                r.updated_at = Timestamp::now();
                Ok(())
            })
        })
        .await?;
    }
}

/// Read back the audio of `[start_ms, end_ms)` on `track`, spanning however
/// many spooled chunks it overlaps. Used by the identifying stage to embed a
/// system-track turn. Pure given `chunks` and a `source`, so it is tested
/// directly against synthetic chunk metadata.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn read_range(
    source: &dyn AudioSource,
    id: RecordingId,
    track: Track,
    chunks: &[ChunkMeta],
    start_ms: u64,
    end_ms: u64,
) -> CommandResult<Vec<i16>> {
    let mut out = Vec::new();
    for meta in chunks.iter().filter(|c| c.track == track) {
        let chunk_start = meta.start_ms;
        let chunk_end = chunk_start + ms_of(meta.samples as usize);
        if chunk_end <= start_ms || chunk_start >= end_ms {
            continue;
        }
        let samples = source.read_chunk(id, track, meta.seq)?;
        let lo_ms = start_ms.saturating_sub(chunk_start);
        let hi_ms = end_ms.saturating_sub(chunk_start).min(ms_of(samples.len()));
        let lo = index_of(lo_ms).min(samples.len());
        let hi = index_of(hi_ms).min(samples.len());
        if lo < hi {
            out.extend_from_slice(&samples[lo..hi]);
        }
    }
    Ok(out)
}

// ============================================================================
// Identifying
// ============================================================================

/// Below this, no embedding is attempted -- too short to be reliable. See
/// the plan's "skip < 1 s".
const MIN_EMBED_MS: u64 = 1_000;

/// Owner and attendee addresses this vault already knows are *not* somebody
/// else on the call: every configured mail account's own address and
/// identities. Best-effort -- an account read failure just means nothing is
/// excluded here, which [`identify::identify`]'s own owner-voiceprint
/// exclusion still backs up whenever the owner also has a voiceprint on
/// file. See this module's doc on "excluding owner addresses" for why this
/// is the closest available answer: nothing in the vault names the owner's
/// own address more directly than the accounts they have signed in with.
fn owner_addresses(vault: &Vault) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Ok(accounts) = vault.accounts() {
        for account in accounts {
            out.insert(account.address.to_ascii_lowercase());
            for identity in account.identities {
                out.insert(identity.address.to_ascii_lowercase());
            }
        }
    }
    out
}

/// The event's attendees as [`Candidate`]s, excluding the owner's own
/// addresses, with a matching voiceprint attached when `voiceprints_on` and
/// one exists -- matched by email first, then by name, and only among
/// voiceprints whose `model` is this session's embedding model (a voiceprint
/// from a different model is never comparable -- see
/// [`Voiceprint::model`](everyday_core::meeting::Voiceprint::model)).
fn candidates_for(
    event: Option<&EventRef>,
    voiceprints: &[Voiceprint],
    voiceprints_on: bool,
    embedding_model: &str,
    owner: &std::collections::HashSet<String>,
) -> Vec<Candidate> {
    let Some(event) = event else { return Vec::new() };
    event
        .attendees
        .iter()
        .filter(|a| {
            let (_, email) = identify::parse_attendee(a);
            !email.is_some_and(|e| owner.contains(&e))
        })
        .map(|attendee| {
            let voiceprint = if voiceprints_on {
                find_voiceprint(attendee, voiceprints, embedding_model)
            } else {
                None
            };
            Candidate { attendee: attendee.clone(), voiceprint }
        })
        .collect()
}

fn find_voiceprint(
    attendee: &str,
    voiceprints: &[Voiceprint],
    embedding_model: &str,
) -> Option<Voiceprint> {
    let (name, email) = identify::parse_attendee(attendee);
    let usable = voiceprints.iter().filter(|v| v.model == embedding_model);
    if let Some(email) = &email {
        if let Some(v) = usable.clone().find(|v| v.email.as_deref() == Some(email.as_str())) {
            return Some(v.clone());
        }
    }
    if let Some(name) = &name {
        if let Some(v) = usable.clone().find(|v| v.name.eq_ignore_ascii_case(name)) {
            return Some(v.clone());
        }
    }
    None
}

/// Label the mic track: the profile's own name, or "You" when nobody has
/// typed one in. No inference -- per the plan, mic is always the owner.
fn owner_label(vault: &Vault) -> String {
    let profile = vault.profile().unwrap_or_default();
    let name = format!("{} {}", profile.first_name.trim(), profile.last_name.trim());
    let name = name.trim();
    if name.is_empty() { "You".to_string() } else { name.to_string() }
}

/// Assign every system-track segment a speaker via local diarisation, when
/// the backend gave no hints of its own to fall back on. One pass per chunk
/// -- `SpeechKit::diarise` only ever promises comparable indices within one
/// call -- so each segment's hint is scoped to its own chunk, exactly the
/// way a backend's own hint would be.
#[cfg(feature = "speech")]
fn diarise_hints(
    kit: &SpeechKit,
    source: &dyn AudioSource,
    id: RecordingId,
    chunks: &[ChunkMeta],
    system: &mut [TrackSegment],
) {
    let mut by_chunk: std::collections::BTreeMap<u32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, seg) in system.iter().enumerate() {
        by_chunk.entry(seg.hint_scope).or_default().push(i);
    }
    for meta in chunks.iter().filter(|c| c.track == Track::System) {
        let Ok(samples) = source.read_chunk(id, Track::System, meta.seq) else { continue };
        let dia = kit.diarise(&samples);
        if dia.is_empty() {
            continue;
        }
        // Every segment whose `hint_scope` names this chunk gets whichever
        // diarised span overlaps it most.
        for &i in by_chunk.values().flatten() {
            let seg = &mut system[i];
            let seg_start = seg.start_ms.saturating_sub(meta.start_ms);
            let seg_end = seg.end_ms.saturating_sub(meta.start_ms);
            if seg_end <= seg_start {
                continue;
            }
            let best = dia
                .iter()
                .map(|&(s, e, speaker)| (overlap(seg_start, seg_end, s, e), speaker))
                .filter(|(overlap, _)| *overlap > 0)
                .max_by_key(|(overlap, _)| *overlap);
            if let Some((_, speaker)) = best {
                seg.speaker_hint = Some(format!("dia{speaker}"));
                seg.hint_scope = meta.seq;
            }
        }
    }
}

#[cfg(feature = "speech")]
fn overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> u64 {
    a_end.min(b_end).saturating_sub(a_start.max(b_start))
}

/// What one call to [`identify_stage`] settles: the owner's speaker record
/// (if the mic ever spoke), the other speakers, and every segment attributed
/// to one of them, in time order.
struct Identified {
    speakers: Vec<Speaker>,
    segments: Vec<Segment>,
}

/// Echo removal, mic → owner, system → turns → clusters → speakers, per the
/// plan's "Who said what". `partial` is `recording.partial`, already sorted
/// by [`merge::remove_echo`](everyday_core::meeting::merge::remove_echo).
#[allow(clippy::too_many_arguments)]
async fn identify_stage(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    recording: &Recording,
    settings: &MeetingSettings,
    embedding_model: &str,
    #[cfg(feature = "speech")] kit: Option<Arc<SpeechKit>>,
) -> CommandResult<Identified> {
    let (kept, _echo_found) =
        everyday_core::meeting::merge::remove_echo(recording.partial.clone(), 0.6);

    let mut mic: Vec<TrackSegment> =
        kept.iter().filter(|s| s.track == Track::Mic).cloned().collect();
    let mut system: Vec<TrackSegment> =
        kept.iter().filter(|s| s.track == Track::System).cloned().collect();
    mic.sort_by_key(|s| s.start_ms);
    system.sort_by_key(|s| s.start_ms);

    let id = recording.id;
    let chunks = recording.chunks.clone();

    #[cfg(feature = "speech")]
    {
        let has_hints = system.iter().any(|s| s.speaker_hint.is_some());
        let embeddable =
            system.iter().filter(|s| s.end_ms.saturating_sub(s.start_ms) >= MIN_EMBED_MS).count();
        if !has_hints && embeddable < 2 {
            if let Some(kit) = &kit {
                let kit = kit.clone();
                let source_for_dia = source.clone();
                let chunks_for_dia = chunks.clone();
                let mut system_for_dia = system.clone();
                system = blocking(move || {
                    diarise_hints(
                        &kit,
                        source_for_dia.as_ref(),
                        id,
                        &chunks_for_dia,
                        &mut system_for_dia,
                    );
                    Ok(system_for_dia)
                })
                .await?;
            }
        }
    }

    let voiceprints_on = settings.voiceprints;
    let owner_addrs = {
        let vault = vault.clone();
        blocking(move || Ok(owner_addresses(&vault))).await?
    };
    let candidates = {
        let vault = vault.clone();
        let event = recording.event.clone();
        let embedding_model = embedding_model.to_string();
        blocking(move || {
            let voiceprints = vault.voiceprints()?;
            Ok(candidates_for(
                event.as_ref(),
                &voiceprints,
                voiceprints_on,
                &embedding_model,
                &owner_addrs,
            ))
        })
        .await?
    };

    // Every system segment becomes one `Turn`, embedded via the speech kit
    // when the feature is compiled in and the segment is long enough. See
    // `MIN_EMBED_MS`.
    let mut turns = Vec::with_capacity(system.len());
    for seg in &system {
        let hint = seg.speaker_hint.as_ref().map(|h| (seg.hint_scope, h.clone()));
        let long_enough = seg.end_ms.saturating_sub(seg.start_ms) >= MIN_EMBED_MS;
        let embedding = if long_enough {
            embed_segment(
                vault,
                source,
                id,
                &chunks,
                seg,
                #[cfg(feature = "speech")]
                kit.clone(),
            )
            .await?
        } else {
            Vec::new()
        };
        turns.push(Turn {
            index: turns.len(),
            duration_ms: seg.end_ms.saturating_sub(seg.start_ms),
            hint,
            embedding,
        });
    }

    let mut clusters = identify::identify(&turns, &candidates, identify::Params::default());

    // Without local speech, every system segment is one undifferentiated
    // turn set (no embeddings, no hints), so `identify` always produces at
    // most one cluster -- see that function's own "everybody an orphan"
    // rule. The plan's fallback label for that shape is "Others" rather
    // than "Unknown 1": there structurally cannot be an "Unknown 2" without
    // diarisation, and saying so would be misleading.
    if cfg!(not(feature = "speech"))
        && clusters.len() == 1
        && matches!(clusters[0].how, Attribution::Unknown)
    {
        clusters[0].label = "Others".to_string();
    }

    let voiceprints_used = voiceprints_on;
    let (owner_speaker, next_key) = if mic.is_empty() {
        (None, 0u16)
    } else {
        let label = {
            let vault = vault.clone();
            blocking(move || Ok(owner_label(&vault))).await?
        };
        (
            Some(Speaker {
                key: 0,
                label,
                email: None,
                voiceprint_id: None,
                how: Attribution::Owner,
                centroid: Vec::new(),
                embedding_model: String::new(),
            }),
            1u16,
        )
    };

    let mut speakers = Vec::new();
    if let Some(owner) = owner_speaker.clone() {
        speakers.push(owner);
    }
    let mut cluster_keys = Vec::with_capacity(clusters.len());
    for (i, cluster) in clusters.iter().enumerate() {
        let key = next_key + i as u16;
        cluster_keys.push(key);
        speakers.push(Speaker {
            key,
            label: cluster.label.clone(),
            email: cluster.email.clone(),
            voiceprint_id: cluster.voiceprint_id,
            how: cluster.how,
            centroid: if voiceprints_used { cluster.centroid.clone() } else { Vec::new() },
            embedding_model: if voiceprints_used && !cluster.centroid.is_empty() {
                embedding_model.to_string()
            } else {
                String::new()
            },
        });
    }

    let mut segments = Vec::new();
    if let Some(owner) = &owner_speaker {
        for seg in &mic {
            segments.push(Segment {
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                speaker: owner.key,
                text: seg.text.clone(),
            });
        }
    }
    for (ci, cluster) in clusters.iter().enumerate() {
        let key = cluster_keys[ci];
        for &turn_idx in &cluster.turns {
            let seg = &system[turn_idx];
            segments.push(Segment {
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                speaker: key,
                text: seg.text.clone(),
            });
        }
    }
    segments.sort_by_key(|s| s.start_ms);

    Ok(Identified { speakers, segments })
}

async fn embed_segment(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    chunks: &[ChunkMeta],
    seg: &TrackSegment,
    #[cfg(feature = "speech")] kit: Option<Arc<SpeechKit>>,
) -> CommandResult<Vec<f32>> {
    let _ = vault;
    #[cfg(feature = "speech")]
    {
        let Some(kit) = kit else { return Ok(Vec::new()) };
        let source = source.clone();
        let chunks = chunks.to_vec();
        let start_ms = seg.start_ms;
        let end_ms = seg.end_ms;
        return blocking(move || {
            let samples =
                read_range(source.as_ref(), id, Track::System, &chunks, start_ms, end_ms)?;
            Ok(kit.embed(&samples).unwrap_or_default())
        })
        .await;
    }
    #[cfg(not(feature = "speech"))]
    {
        let _ = (source, id, chunks, seg);
        Ok(Vec::new())
    }
}

// ============================================================================
// Summarising
// ============================================================================

/// Build [`Facts`] for the template: title, event, timing, `present` in
/// order of first speech, and the timezone to write times in.
fn facts_for(recording: &Recording, identified: &Identified, tz: &str) -> Facts {
    let mut present = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for seg in &identified.segments {
        if seen.insert(seg.speaker) {
            let label = identified
                .speakers
                .iter()
                .find(|s| s.key == seg.speaker)
                .map(|s| s.label.clone())
                .unwrap_or_default();
            if !label.is_empty() {
                present.push(label);
            }
        }
    }
    Facts {
        title: recording
            .event
            .as_ref()
            .map(|e| e.title.clone())
            .unwrap_or_else(|| recording.title.clone()),
        event: recording.event.clone(),
        started_at: Some(recording.started_at),
        ended_at: recording.ended_at,
        present,
        tz: recording
            .event
            .as_ref()
            .map(|e| e.tz.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| tz.to_string()),
    }
}

/// Fill the template, split it into pieces under budget, summarise each and
/// combine, then enforce the template's own headings. Returns the answer
/// alone -- the details block is [`write_stage`]'s to prepend.
///
/// `pub(crate)`: `domains::transcripts` reuses this directly for
/// `preview_meeting_template` and `rewrite_meeting_note`, so a preview and
/// the real note are always produced by exactly the same code.
pub(crate) async fn summarise_stage(
    summariser: &dyn Summariser,
    transcript: &Transcript,
    template_body: &str,
    facts: &Facts,
    budget: u32,
) -> CommandResult<String> {
    let filled = everyday_core::meeting::template::fill(template_body, facts);
    let pieces = everyday_core::meeting::prompt::pieces(transcript, budget);

    let mut answers = Vec::with_capacity(pieces.len());
    let n = pieces.len();
    for (i, piece) in pieces.iter().enumerate() {
        let part = if n > 1 { Some((i + 1, n)) } else { None };
        let req = everyday_core::meeting::prompt::summarise(&filled, piece, part);
        let answer = summariser.ask(&req.system, &req.user).await?;
        answers.push(answer);
    }

    let combined = if answers.len() > 1 {
        let req = everyday_core::meeting::prompt::combine(&filled, &answers);
        summariser.ask(&req.system, &req.user).await?
    } else {
        answers.into_iter().next().unwrap_or_default()
    };

    Ok(everyday_core::meeting::template::enforce_headings(&filled, &combined))
}

// ============================================================================
// Writing
// ============================================================================

/// Purpose for a meeting note: the calendar's own role, or none. Delegates
/// to `everyday_core::meeting::filing::purpose_for`, now that it has landed.
fn purpose_for(vault: &Vault, event: Option<&EventRef>) -> Option<everyday_core::Purpose> {
    let calendar = event.and_then(|e| vault.calendar(e.calendar_id).ok());
    everyday_core::meeting::filing::purpose_for(calendar.as_ref())
}

// ============================================================================
// The driver
// ============================================================================

/// One attempt at driving `id` from wherever its [`Stage`] currently is
/// through to [`Stage::Done`] (or a terminal [`Stage::Failed`]).
///
/// Every branch below saves the recording as its own work finishes -- see
/// this module's doc on why. `Stage::Recording` never advances the stage
/// here -- the spool itself owns that transition, once the call actually
/// ends -- but it is not a no-op: see [`transcribe_live`] for the early
/// transcription this branch attempts while the call is still going.
pub async fn run_recording(
    vault: Arc<Vault>,
    source: Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: Option<Arc<dyn Summariser>>,
    events: Arc<dyn crate::events::EventSink>,
) -> CommandResult<()> {
    loop {
        let recording = {
            let vault = vault.clone();
            blocking(move || Ok(vault.recording(id)?)).await?
        };
        match recording.stage.clone() {
            Stage::Recording => {
                if let Err(e) = transcribe_live(&vault, &source, id).await {
                    // A head start, not a promise: whatever did not get
                    // transcribed now is exactly as `Stage::Transcribing`
                    // will pick it up once `finish` ends the call for
                    // real. Logged, not `fail`ed -- the call is still live,
                    // and a network hiccup reaching a transcriber early
                    // must never be the reason a recording that is still
                    // going gets marked `Failed`.
                    tracing::warn!(
                        recording = %id,
                        error = %e.message,
                        "meeting pipeline: early transcription failed; the ordinary pass at \
                         the end of the call will retry it"
                    );
                }
                // Test-only, and a no-op unless a test has armed `id` -- see
                // `test_hooks`'s own doc for why a test needs a hook here at
                // all rather than a slow real transcriber.
                #[cfg(test)]
                test_hooks::wait(id).await;
                return Ok(());
            }
            Stage::Transcribing => {
                if let Err(e) = retry_transient(|| do_transcribe(&vault, &source, id)).await {
                    fail(&vault, id, Stage::Transcribing, &e, events.as_ref()).await?;
                    return Err(e);
                }
            }
            Stage::Identifying => {
                if let Err(e) = retry_transient(|| do_identify(&vault, &source, id)).await {
                    fail(&vault, id, Stage::Identifying, &e, events.as_ref()).await?;
                    return Err(e);
                }
            }
            Stage::Summarising => {
                let Some(summariser) = summariser.clone() else {
                    let e = CommandError::new(codes::AGENT, "the assistant is not configured");
                    fail(&vault, id, Stage::Summarising, &e, events.as_ref()).await?;
                    return Err(e);
                };
                if let Err(e) = retry_transient(|| {
                    do_summarise_and_write(
                        &vault,
                        &source,
                        id,
                        summariser.as_ref(),
                        events.as_ref(),
                    )
                })
                .await
                {
                    fail(&vault, id, Stage::Summarising, &e, events.as_ref()).await?;
                    return Err(e);
                }
                return Ok(());
            }
            Stage::Done | Stage::Failed { .. } => return Ok(()),
        }
    }
}

/// How many attempts [`retry_transient`] gives a transient error before
/// giving up and letting it through to `fail` -- see [`is_transient`].
/// Bounded on purpose: an endpoint whose network trouble never clears is
/// still worth telling a person about eventually, rather than retrying
/// forever with nothing for them to see or act on.
const TRANSIENT_RETRIES: u32 = 5;

/// The delay before [`retry_transient`]'s *second* attempt (the first
/// retry); each one after that doubles it, capped by
/// [`TRANSIENT_RETRY_CAP`]. Chosen, with [`TRANSIENT_RETRIES`], so the
/// worst case -- every attempt transient, every delay maxed out -- spans
/// roughly ten minutes: long enough to outlast a network blip or a
/// provider's bad minute, short enough that a call's note is not left
/// waiting for the length of the call itself.
const TRANSIENT_RETRY_BASE: Duration = Duration::from_secs(30);

/// The largest gap [`retry_transient`] leaves between attempts.
const TRANSIENT_RETRY_CAP: Duration = Duration::from_secs(4 * 60);

/// Whether `code` names a failure worth retrying -- a network hiccup, a
/// timeout, a rate limit, or a provider having a bad moment -- as opposed to
/// one retrying can never fix: a refused key, a model or endpoint that does
/// not exist, a chunk too large to send, or an assistant that is not
/// configured at all. See `crate::error::codes`, and each `Transcriber`'s
/// own `status_error` (`transcribe/openai.rs`, `transcribe/gemini.rs`) for
/// what maps to which.
fn is_transient(code: &str) -> bool {
    matches!(code, codes::NETWORK | codes::TIMED_OUT | codes::RATE_LIMITED | codes::PROVIDER)
}

/// Run one stage's own operation -- `do_transcribe`, `do_identify`,
/// `do_summarise_and_write` -- retrying it in place while its error is
/// [`is_transient`], up to [`TRANSIENT_RETRIES`] times with growing backoff.
/// A permanent error is returned on the very first attempt: see
/// [`is_transient`] for why retrying one is never worth doing.
///
/// Safe to retry a whole stage rather than only the one request that
/// failed: every stage checkpoints as it goes (`transcribe_stage` saves
/// after each chunk; `do_identify` and `do_summarise_and_write` are cheap,
/// local recomputation until their own final network call -- see
/// `do_identify`'s own doc on why nothing is persisted between identifying
/// and summarising), so re-running one from the top never re-pays for
/// audio already transcribed, only for whatever a network hiccup actually
/// interrupted.
async fn retry_transient<F, Fut>(op: F) -> CommandResult<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = CommandResult<()>>,
{
    retry_transient_from(op, TRANSIENT_RETRY_BASE).await
}

/// [`retry_transient`], with the starting delay broken out so a test can
/// shrink it without a real recording ever needing to -- the same seam
/// `capture.rs`'s `finish_with_retry_from` uses for the same reason.
async fn retry_transient_from<F, Fut>(op: F, mut delay: Duration) -> CommandResult<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = CommandResult<()>>,
{
    let mut last_err = None;
    for attempt in 0..TRANSIENT_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(TRANSIENT_RETRY_CAP);
        }
        match op().await {
            Ok(()) => return Ok(()),
            Err(e) if is_transient(&e.code) => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.expect("looped at least once"))
}

/// Marks `id` [`Stage::Failed`] and tells anyone watching the recordings
/// list -- without this, a call that fails partway through the pipeline
/// (unlike one that reaches [`Stage::Done`], which raises its own
/// `Kind::Recording` change in [`do_summarise_and_write`]) would sit at its
/// last-seen stage in every other window until something else happened to
/// reload it.
///
/// Refuses to touch a row already at [`Stage::Done`] -- defence in depth
/// alongside [`do_summarise_and_write`]'s own care not to call this at all
/// once its note is committed: a stage that starts only after `Done` is
/// reached has nothing left to fail *of*, and overwriting that row would
/// orphan the note it already wrote (a retry recomputes from `partial`,
/// which `Done` has already cleared -- see `spool::retry`'s own guard
/// against exactly that). No change event is raised for a no-op refusal:
/// nothing about the row actually changed for a window to reload.
async fn fail(
    vault: &Arc<Vault>,
    id: RecordingId,
    at: Stage,
    e: &CommandError,
    events: &dyn crate::events::EventSink,
) -> CommandResult<()> {
    let vault = vault.clone();
    let reason = crate::llm::friendly(&e.message);
    let recording = blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, move |recording| {
            if matches!(recording.stage, Stage::Done) {
                return Ok(());
            }
            recording.stage = Stage::Failed { reason, at: Box::new(at) };
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await?;
    if matches!(recording.stage, Stage::Failed { .. }) {
        let mut change = Change::new(Kind::Recording, Op::Updated);
        change.id = Some(id.to_string());
        events.changed(change);
    } else {
        tracing::warn!(
            recording = %id,
            "meeting pipeline: refused to mark a `Done` recording as failed"
        );
    }
    Ok(())
}

/// Build the configured transcriber and its hints, then run
/// [`transcribe_stage`] against `id` -- everything [`do_transcribe`] and
/// [`transcribe_live`] both need before they can each decide what to do
/// once transcription itself is caught up: `do_transcribe` may advance the
/// stage out of `Transcribing`; `transcribe_live`, mid-call, never does.
/// Shared so the two can never disagree about what "the configured
/// transcriber" means, the same reason [`build_transcriber`] itself is
/// shared with `domains::transcripts`.
async fn transcribe_pending(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };
    let Some(config) = settings.transcriber.clone() else {
        return Err(CommandError::new(codes::UNSUPPORTED, "no transcriber is configured"));
    };
    let key = crate::domains::meetings::transcriber_key(vault);
    let transcriber = build_transcriber(&config, key)?;

    let names: Vec<String> =
        recording.event.as_ref().map(|e| e.attendees.clone()).unwrap_or_default();
    let hints = Hints {
        language: settings.language.clone(),
        prompt: everyday_core::meeting::prompt::transcriber_hint(&recording.title, &names),
    };

    #[cfg(feature = "speech")]
    let kit = speech::speech_kit().ok();

    transcribe_stage(
        vault,
        source,
        transcriber.as_ref(),
        id,
        &hints,
        #[cfg(feature = "speech")]
        kit,
    )
    .await
}

/// Transcribe whatever chunks have closed so far, without advancing the
/// recording out of `Stage::Recording` -- the call may still be going, so
/// there is nothing here to checkpoint beyond [`transcribe_stage`]'s own
/// per-chunk save. Only for a remote backend, and only best-effort: see
/// `chunk_closed`'s own doc for why local transcription never runs early,
/// and `run_recording`'s `Stage::Recording` arm for why an error here is
/// logged rather than ever failing the recording.
///
/// Re-reads the setting rather than trusting whoever enqueued this pass:
/// `chunk_closed` already checks `is_remote` before enqueuing, but the
/// person could switch the transcriber to a local one between one chunk
/// closing and this attempt actually running, and a local model has no
/// business being built here regardless of what triggered the call.
async fn transcribe_live(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };
    let remote = settings.transcriber.as_ref().is_some_and(TranscriberConfig::is_remote);
    if !remote {
        return Ok(());
    }
    transcribe_pending(vault, source, id).await
}

async fn do_transcribe(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    transcribe_pending(vault, source, id).await?;

    // Locked, and re-checked: a chunk `append` accepted in the gap between
    // `transcribe_stage`'s own last "nothing pending" reload and this save
    // (see that function's doc) must not be silently skipped over. If one
    // did land, the stage is left at `Transcribing` rather than advanced --
    // `run_recording`'s own loop calls this function again as long as it
    // reads that stage, so the chunk is picked up on the very next pass
    // rather than stranded.
    let vault = vault.clone();
    blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, |recording| {
            if recording.chunks.iter().any(|c| !c.transcribed) {
                return Ok(());
            }
            recording.stage = Stage::Identifying;
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await
    .map(|_| ())
}

/// Compute the identifying stage's result and checkpoint the transition to
/// [`Stage::Summarising`]. Deliberately does *not* persist a [`Transcript`]:
/// one needs a [`everyday_core::NoteId`] to be indexed against, and that id
/// does not exist until [`do_summarise_and_write`] mints it. Instead
/// `recording.partial` is left exactly as the transcribing stage left it
/// (not cleared here), so [`do_summarise_and_write`] -- whether it runs
/// straight after this in the same pass, or on a later retry following a
/// crash between this checkpoint and the note being written -- can call
/// [`identify_stage`] again itself and get the same, deterministic answer.
/// The cost is one redundant pass over the same audio on every recording,
/// even the ones that never fail; the alternative is a second storage shape
/// for "a transcript with no note yet", which is more moving parts than a
/// re-embed of a single call's audio is worth guarding against.
async fn do_identify(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };

    #[cfg(feature = "speech")]
    let kit = speech::speech_kit().ok();
    #[cfg(feature = "speech")]
    let embedding_model = kit.as_ref().map(|k| k.embedding_model().to_string()).unwrap_or_default();
    #[cfg(not(feature = "speech"))]
    let embedding_model = String::new();

    // Only run for the readiness check it gives: a transcriber or speech-kit
    // problem is better caught here than after the assistant has already
    // been asked to summarise nothing.
    let _ = identify_stage(
        vault,
        source,
        &recording,
        &settings,
        &embedding_model,
        #[cfg(feature = "speech")]
        kit,
    )
    .await?;

    let vault = vault.clone();
    blocking(move || {
        crate::meeting::spool::mutate_recording(&vault, id, |recording| {
            recording.stage = Stage::Summarising;
            recording.updated_at = Timestamp::now();
            Ok(())
        })
    })
    .await
    .map(|_| ())
}

fn build_transcript(
    recording: &Recording,
    identified: &Identified,
    settings: &MeetingSettings,
) -> Transcript {
    let backend = settings.transcriber.as_ref().map(TranscriberConfig::label).unwrap_or_default();
    let now = Timestamp::now();
    Transcript {
        id: TranscriptId::new(),
        note_id: everyday_core::NoteId::new(),
        recording_id: Some(recording.id),
        language: settings.language.clone(),
        backend,
        speakers: identified.speakers.clone(),
        segments: identified.segments.clone(),
        created_at: now,
        updated_at: now,
    }
}

async fn do_summarise_and_write(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    summariser: &dyn Summariser,
    events: &dyn crate::events::EventSink,
) -> CommandResult<()> {
    let recording = {
        let vault = vault.clone();
        blocking(move || Ok(vault.recording(id)?)).await?
    };
    let settings = {
        let vault = vault.clone();
        blocking(move || Ok(vault.meeting_settings()?)).await?
    };

    #[cfg(feature = "speech")]
    let kit = speech::speech_kit().ok();
    #[cfg(feature = "speech")]
    let embedding_model = kit.as_ref().map(|k| k.embedding_model().to_string()).unwrap_or_default();
    #[cfg(not(feature = "speech"))]
    let embedding_model = String::new();

    // Recomputed rather than loaded -- see `do_identify`'s doc for why
    // nothing is persisted between the two stages.
    let identified = identify_stage(
        vault,
        source,
        &recording,
        &settings,
        &embedding_model,
        #[cfg(feature = "speech")]
        kit,
    )
    .await?;
    let mut transcript = build_transcript(&recording, &identified, &settings);

    let tz = {
        let vault = vault.clone();
        blocking(move || {
            let agent_settings = vault.agent_settings().unwrap_or_default();
            Ok(agent_settings.timezone.unwrap_or_else(everyday_core::model::system_tz))
        })
        .await?
    };
    let facts = facts_for(&recording, &identified, &tz);
    let template = settings.template(Some(recording.template_id));
    let loopback = {
        let vault = vault.clone();
        blocking(move || {
            let (agent_settings, _) = vault.agent_credentials()?;
            Ok(everyday_core::agent::is_loopback(agent_settings.provider_config.endpoint()))
        })
        .await
        .unwrap_or(false)
    };
    let budget = settings
        .summary_budget
        .unwrap_or_else(|| everyday_core::meeting::prompt::default_budget(loopback));

    let answer = summarise_stage(summariser, &transcript, &template.body, &facts, budget).await?;
    let details = everyday_core::meeting::template::details_block(&facts);
    let body = if details.is_empty() { answer } else { format!("{details}\n\n{answer}") };

    let title = facts.title.clone();
    let title = if title.trim().is_empty() {
        format!("Call on {}", recording.started_at.strftime("%-d %B %Y"))
    } else {
        title
    };

    let note = Note::written(title.clone(), &body);
    let note_id = note.id;
    let purpose = {
        let vault = vault.clone();
        let event = recording.event.clone();
        blocking(move || Ok(purpose_for(&vault, event.as_ref()))).await?
    };
    let mut note = note;
    note.purpose = purpose;

    transcript.note_id = note_id;
    transcript.segments.sort_by_key(|s| s.start_ms);
    transcript.updated_at = Timestamp::now();

    // The note, the transcript and the recording's own `Done` row commit
    // together, in one `blocking` call -- `mutate_recording` for the row so
    // it is still serialised against any other mutator of the same
    // recording (see `spool::mutate_recording`'s own doc), even though
    // nothing should legitimately be racing a recording that has already
    // left `Transcribing`.
    {
        let vault = vault.clone();
        let note = note.clone();
        let transcript = transcript.clone();
        blocking(move || {
            vault.save_note(&note, None)?;
            vault.save_transcript(&transcript)?;
            crate::meeting::spool::mutate_recording(&vault, id, |recording| {
                recording.stage = Stage::Done;
                recording.note_id = Some(note_id);
                recording.chunks.clear();
                recording.partial.clear();
                recording.updated_at = Timestamp::now();
                Ok(())
            })?;
            Ok(())
        })
        .await?;
    }

    // The note is committed; everything from here on only tidies the spool,
    // and a failure here must not be allowed to undo it. Unlike every
    // `?` above, this is deliberately swallowed: propagating it would reach
    // `run_recording`'s caller as an `Err`, which calls `fail` and would
    // overwrite the `Done` row this function just wrote with `Failed { at:
    // Summarising }` -- orphaning the note a retry would then duplicate
    // with a second, near-empty one built from `partial`, which `Done`
    // just cleared. `fail` itself now refuses that overwrite too, as a
    // second line of defence, but the point is not to ask it to. Whatever
    // is left of the spool directory is picked up by
    // `spool::expire_failed_tick`'s `sweep_orphaned_spool`, which deletes a
    // spool directory whose recording is `Done` on exactly this account.
    if let Err(e) = source.remove_audio(id) {
        tracing::warn!(
            recording = %id,
            error = %e.message,
            "meeting pipeline: wrote the note but could not clear its spool; \
             the hourly sweep will pick it up"
        );
    }

    let mut change = Change::new(Kind::Note, Op::Created);
    change.id = Some(note_id.to_string());
    events.changed(change);
    let mut rec_change = Change::new(Kind::Recording, Op::Updated);
    rec_change.id = Some(id.to_string());
    events.changed(rec_change);
    let mut t_change = Change::new(Kind::Transcript, Op::Updated);
    t_change.id = Some(transcript.id.to_string());
    events.changed(t_change);

    events.notify(
        Notification::new(crate::events::Level::Info, format!("Notes from {title} are ready"))
            .for_user()
            .key(format!("meeting:{id}")),
    );

    Ok(())
}

// ============================================================================
// The seam the spool calls through
// ============================================================================

fn task_key(id: RecordingId) -> String {
    format!("meeting:{id}")
}

/// A recording has finished (or been retried), or a chunk of a still-live
/// one has closed and is worth transcribing early. Starts or wakes the
/// pipeline for it; returns at once. See the supervisor's own doc for what
/// "starts or wakes" means: calling this twice for the same recording while
/// it is already running never starts a second attempt underneath the
/// first, but it does ask that attempt to run again once it finishes --
/// which matters here specifically because `finish` calling this the
/// instant a call ends can otherwise land in the narrow gap between an
/// early, `chunk_closed`-triggered attempt seeing `Stage::Recording` and
/// that attempt actually retiring; see `Supervisor::Entry::rerun_requested`.
pub fn enqueue(svc: &Arc<Service>, id: RecordingId) {
    let Ok(vault) = svc.require() else { return };
    let source: Arc<dyn AudioSource> = Arc::new(SpoolSource::new(vault.clone()));
    let summariser: Option<Arc<dyn Summariser>> =
        if vault.agent_settings().map(|s| s.enabled).unwrap_or(false) {
            Some(Arc::new(AssistantSummariser::new(vault.clone())))
        } else {
            None
        };
    let events = svc.events();
    svc.supervisor().ensure(task_key(id), move |_stop| {
        let vault = vault.clone();
        let source = source.clone();
        let summariser = summariser.clone();
        let events = events.clone();
        Box::pin(async move {
            match run_recording(vault, source, id, summariser, events).await {
                Ok(()) => Ok(Outcome::Done),
                Err(e) => {
                    Err(Box::new(std::io::Error::other(e.message)) as crate::supervisor::TaskError)
                }
            }
        }) as crate::supervisor::TaskFuture
    });
}

/// One chunk has been spooled while the call is still going. For a remote
/// transcriber, [`enqueue`] -- and, through it, [`run_recording`]'s
/// `Stage::Recording` arm and [`transcribe_live`] -- transcribes it right
/// away, so a long call is mostly done by its end rather than starting from
/// nothing once it ends.
///
/// Deliberately conservative: local transcription is CPU the same process
/// needs for capture and VAD, so a struggling machine is never asked to do
/// both at once by this path. A local chunk is still transcribed -- just
/// not until the call ends and [`enqueue`] runs from `finish`, which is
/// always safe.
pub fn chunk_closed(svc: &Arc<Service>, id: RecordingId, _track: Track, _seq: u32) {
    let Ok(vault) = svc.require() else { return };
    let Ok(settings) = vault.meeting_settings() else { return };
    let remote = settings.transcriber.as_ref().is_some_and(TranscriberConfig::is_remote);
    if remote {
        enqueue(svc, id);
    }
}

/// A per-recording gate [`run_recording`]'s `Stage::Recording` arm waits on
/// just before returning -- test-only, and a no-op for every recording
/// nothing has armed. Exists for one reason: proving that `enqueue` landing
/// while an early, `chunk_closed`-triggered attempt is genuinely still
/// running -- not yet retired by the supervisor -- still wakes that attempt
/// rather than losing the request. See `supervisor.rs`'s
/// `Entry::rerun_requested` for the mechanism this proves, and
/// `tests::finish_landing_while_the_early_attempt_is_still_running_still_reaches_done`
/// for the test that uses it.
///
/// A slow or gated real transcriber could stand in for this instead, and is
/// what `tests::spawn_flaky_server` gives the *other* retry tests their
/// timing from -- but `chunk_closed`'s own early transcription only runs for
/// a transcriber [`TranscriberConfig::is_remote`] calls remote, and every
/// address a test can actually reach from inside this process is, by that
/// same function's own reading, loopback. This gate holds the early attempt
/// "still running" a different way, exactly where a slow network call would
/// sit if one were reachable.
#[cfg(test)]
mod test_hooks {
    use everyday_core::RecordingId;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    pub(super) struct Gate {
        reached: AtomicBool,
        notify: tokio::sync::Notify,
    }

    impl Gate {
        /// Whether `run_recording` has reached this gate yet -- what a test
        /// polls before assuming the early attempt is truly parked, rather
        /// than assuming any particular amount of scheduling has happened.
        pub(super) fn reached(&self) -> bool {
            self.reached.load(Ordering::SeqCst)
        }

        pub(super) fn release(&self) {
            self.notify.notify_one();
        }
    }

    fn gates() -> &'static Mutex<HashMap<RecordingId, Arc<Gate>>> {
        static GATES: OnceLock<Mutex<HashMap<RecordingId, Arc<Gate>>>> = OnceLock::new();
        GATES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Arm the gate for `id`. One test at a time per id -- trivially true in
    /// practice, since every test mints its own fresh `RecordingId`.
    pub(super) fn arm(id: RecordingId) -> Arc<Gate> {
        let gate =
            Arc::new(Gate { reached: AtomicBool::new(false), notify: tokio::sync::Notify::new() });
        gates().lock().unwrap().insert(id, gate.clone());
        gate
    }

    /// What [`super::run_recording`] calls. A no-op unless a test has armed
    /// `id` first.
    pub(super) async fn wait(id: RecordingId) {
        let gate = gates().lock().unwrap().get(&id).cloned();
        if let Some(gate) = gate {
            gate.reached.store(true, Ordering::SeqCst);
            gate.notify.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::meeting::{Recording, Stage};
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
        let (s1, e1) =
            map_segment(&groups[0].map, turn2_local_start + 100, turn2_local_start + 300);
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
        ChunkMeta {
            track,
            seq,
            start_ms,
            samples: ms_to_samples(dur_ms) as u64,
            transcribed: false,
        }
    }

    #[test]
    fn read_range_spans_two_chunks() {
        let id = RecordingId::new();
        let audio = FakeAudio::new();
        audio.put(id, Track::System, 0, samples(ms_to_samples(30_000)));
        audio.put(id, Track::System, 1, samples(ms_to_samples(30_000)));
        let chunks =
            vec![meta(Track::System, 0, 0, 30_000), meta(Track::System, 1, 30_000, 30_000)];
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
                Ok(vec![RawSegment {
                    start_ms: 0,
                    end_ms: duration,
                    text: reply,
                    speaker_hint: None,
                }])
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
            Box::pin(
                async move { Err(CommandError::new(codes::AGENT, "the assistant is not usable")) },
            )
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
        do_summarise_and_write(&vault, &source, id, &FakeSummariser, events.as_ref())
            .await
            .unwrap();

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
            ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<Vec<RawSegment>>>
            {
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
        assert_eq!(
            identified.segments.iter().filter(|s| s.text.contains("ship on Friday")).count(),
            1
        );
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
        do_summarise_and_write(&vault, &source, id, &FakeSummariser, events.as_ref())
            .await
            .unwrap();

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
        let (base, calls) =
            spawn_flaky_server(2, axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        let mut settings = vault.meeting_settings().unwrap();
        settings.transcriber =
            Some(TranscriberConfig::Compatible { base_url: base, model: "whisper-1".into() });
        vault.save_meeting_settings(&settings).unwrap();
        let source: Arc<dyn AudioSource> = audio.clone();

        let result =
            retry_transient_from(|| do_transcribe(&vault, &source, id), Duration::from_millis(1))
                .await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "two 500s, then a third request that succeeded"
        );
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
        let (base, calls) =
            spawn_flaky_server(usize::MAX, axum::http::StatusCode::UNAUTHORIZED).await;
        let mut settings = vault.meeting_settings().unwrap();
        settings.transcriber =
            Some(TranscriberConfig::Compatible { base_url: base, model: "whisper-1".into() });
        vault.save_meeting_settings(&settings).unwrap();
        let source: Arc<dyn AudioSource> = audio.clone();

        let result =
            retry_transient_from(|| do_transcribe(&vault, &source, id), Duration::from_millis(1))
                .await;

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
        assert_eq!(
            recording.stage,
            Stage::Recording,
            "the call is still live; nothing advances it"
        );
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
        settings.transcriber = Some(TranscriberConfig::Local {
            model: everyday_core::meeting::LocalModel::ParakeetV3,
        });
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
                        Err(Box::new(std::io::Error::other(e.message))
                            as crate::supervisor::TaskError)
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
}
