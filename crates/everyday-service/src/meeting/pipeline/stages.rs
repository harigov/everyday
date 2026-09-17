use std::sync::Arc;

use everyday_core::meeting::identify::{self, Candidate, Turn};
use everyday_core::meeting::template::Facts;
use everyday_core::meeting::{
    Attribution, ChunkMeta, EventRef, MeetingSettings, Recording, Segment, Speaker, Track,
    TrackSegment, Transcript, Voiceprint,
};
use everyday_core::{RecordingId, Vault};
use jiff::Timestamp;

use crate::error::CommandResult;
use crate::meeting::transcribe::{Hints, RawSegment, SpeechChunk, Transcriber};
use crate::service::blocking;

#[cfg(feature = "speech")]
use crate::meeting::speech::SpeechKit;

use super::adapters::*;
use super::turns::*;
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
pub(crate) async fn transcribe_stage(
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
pub(crate) fn read_range(
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
pub(crate) fn candidates_for(
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

pub(crate) fn find_voiceprint(
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
pub(crate) struct Identified {
    pub(crate) speakers: Vec<Speaker>,
    pub(crate) segments: Vec<Segment>,
}

/// Echo removal, mic → owner, system → turns → clusters → speakers, per the
/// plan's "Who said what". `partial` is `recording.partial`, already sorted
/// by [`merge::remove_echo`](everyday_core::meeting::merge::remove_echo).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn identify_stage(
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
pub(crate) fn facts_for(recording: &Recording, identified: &Identified, tz: &str) -> Facts {
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
pub(crate) fn purpose_for(
    vault: &Vault,
    event: Option<&EventRef>,
) -> Option<everyday_core::Purpose> {
    let calendar = event.and_then(|e| vault.calendar(e.calendar_id).ok());
    everyday_core::meeting::filing::purpose_for(calendar.as_ref())
}
