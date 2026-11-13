use std::sync::Arc;

use everyday_core::meeting::Stage;
use everyday_core::meeting::identify::{self, Candidate, Turn};
use everyday_core::meeting::people::{self, Invitee};
use everyday_core::meeting::template::Facts;
use everyday_core::meeting::{
    Attribution, ChunkMeta, EventRef, MeetingSettings, Recording, Segment, Speaker, Track,
    TrackSegment, Transcript, Voiceprint,
};
use everyday_core::{NoteId, RecordingId, Vault};
use jiff::Timestamp;

use crate::error::CommandResult;
use crate::meeting::transcribe::{Hints, RawSegment, SpeechChunk, Transcriber};
use crate::service::blocking;
use everyday_core::meeting::{echo, hallucination, vocab};

#[cfg(feature = "speech")]
use crate::meeting::speech::SpeechKit;

use super::adapters::*;
use super::turns::*;
// ============================================================================
// Transcribing
// ============================================================================

/// What cuts audio into speech turns: the speech kit, or -- in a build
/// without local speech -- nothing that can ever be present.
#[cfg(feature = "speech")]
type Kit = SpeechKit;
#[cfg(not(feature = "speech"))]
type Kit = ();

/// Every 30-second chunk, cut into speech turns: with the speech kit, real
/// VAD; without it, one turn spanning the whole chunk, so [`group_turns`]
/// and [`map_segment`] serve both paths.
fn turns_of(samples: &[i16], kit: Option<&Kit>) -> Vec<(u64, u64)> {
    #[cfg(feature = "speech")]
    if let Some(kit) = kit {
        return kit.vad(samples);
    }
    #[cfg(not(feature = "speech"))]
    let _ = kit;
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
///
/// # Sentences across a chunk boundary
///
/// The spool cuts each track every 30 seconds whatever is being said, and
/// a sentence cut in two comes back from any recogniser as two broken
/// halves. So when a chunk's last speech turn runs to its end and the next
/// chunk of the same track starts mid-speech, the turn is sent whole, with
/// the start of the next chunk appended, and the next chunk remembers how
/// much of its start was already sent ([`ChunkMeta::head_ms`]). This needs
/// voice detection to find the turns; without the speech kit a chunk is
/// sent whole, as before. While the call is still going, a chunk therefore
/// waits for its successor before it is transcribed -- thirty seconds'
/// delay on an early transcription, never a sentence broken in half.
///
/// # Lines nobody said
///
/// Every line a backend returns goes through
/// [`hallucination::clean_line`]: its prompt read back, stock subtitle
/// lines over a faint sound, noise tags and loops are dropped or collapsed
/// before they ever reach the transcript.
pub(crate) async fn transcribe_stage(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    transcriber: &dyn Transcriber,
    id: RecordingId,
    hints: &Hints,
    #[cfg(feature = "speech")] kit: Option<Arc<SpeechKit>>,
) -> CommandResult<()> {
    let limits = transcriber.limits();
    #[cfg(not(feature = "speech"))]
    let kit: Option<Arc<Kit>> = None;

    loop {
        let recording = {
            let vault = vault.clone();
            blocking(move || Ok(vault.recording(id)?)).await?
        };
        // Only voice detection can carry a sentence over, so only then does
        // a chunk of a live call wait for the one after it.
        let waits = matches!(recording.stage, Stage::Recording) && kit.is_some();
        let Some(meta) = recording
            .chunks
            .iter()
            .find(|c| !c.transcribed && (!waits || successor(&recording.chunks, c).is_some()))
            .cloned()
        else {
            return Ok(());
        };
        let next = successor(&recording.chunks, &meta).filter(|n| !n.transcribed).cloned();

        let source_for_read = source.clone();
        let kit_for_read = kit.clone();
        let meta_for_read = meta.clone();
        let (groups, floor, carried) = blocking(move || {
            let kit = kit_for_read.as_deref();
            let detects_speech = kit.is_some();
            let turns_in = |samples: &[i16]| turns_of(samples, kit);
            let mut samples =
                source_for_read.read_chunk(id, meta_for_read.track, meta_for_read.seq)?;
            let floor = hallucination::noise_floor(&samples);
            let len_ms = ms_of(samples.len());
            let mut turns = after_head(&turns_in(&samples), meta_for_read.head_ms);

            // Only with real voice detection: without it the whole chunk is
            // one turn, which always "runs over" and would carry for ever.
            let mut carried = None;
            if detects_speech
                && runs_over(&turns, len_ms)
                && let Some(next) = next.as_ref()
                && next.start_ms.abs_diff(meta_for_read.start_ms + len_ms) <= CONTIGUOUS_MS
                && let Ok(next_samples) = source_for_read.read_chunk(id, next.track, next.seq)
                && let Some(wanted) = carry_into(&turns_in(&next_samples))
                && let Some(last) = turns.last_mut()
                && let Some(end) = carry_end(&next_samples, len_ms.saturating_sub(last.0), wanted)
            {
                let end_index = index_of(end).min(next_samples.len());
                samples.extend_from_slice(&next_samples[..end_index]);
                last.1 = len_ms + ms_of(end_index);
                carried = Some((next.seq, ms_of(end_index)));
            }
            Ok((group_turns(&turns, &samples, meta_for_read.start_ms, limits), floor, carried))
        })
        .await?;

        let mut new_segments = Vec::new();
        for (gi, group) in groups.iter().enumerate() {
            let speech_chunk = SpeechChunk {
                samples: group.samples.clone(),
                offset_ms: 0,
                scope: meta.seq * 1000 + gi as u32,
            };
            let faint =
                hallucination::clear_speech_ms(&group.samples, floor) < hallucination::MIN_CLEAR_MS;
            let raw: Vec<RawSegment> = transcriber.transcribe(&speech_chunk, hints).await?;
            for seg in raw {
                let heard = hallucination::Heard {
                    text: &seg.text,
                    no_speech_prob: seg.no_speech_prob,
                    avg_logprob: seg.avg_logprob,
                };
                let Some(text) = hallucination::clean_line(heard, &hints.prompt, faint) else {
                    continue;
                };
                let (start_ms, end_ms) = map_segment(&group.map, seg.start_ms, seg.end_ms);
                new_segments.push(TrackSegment {
                    track: meta.track,
                    start_ms,
                    end_ms,
                    text,
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
                if let Some((seq, head_ms)) = carried
                    && let Some(c) = r.chunks.iter_mut().find(|c| (c.track, c.seq) == (key.0, seq))
                {
                    c.head_ms = head_ms;
                }
                r.partial.extend(new_segments);
                r.updated_at = Timestamp::now();
                Ok(())
            })
        })
        .await?;
    }
}

/// How far apart, in ms, the end of one chunk (its start plus its length)
/// and the start of the next may be and the two still count as one
/// unbroken stretch of audio. The capture stamps each chunk with the wall
/// clock when the batch holding its first sample arrived, so chunks cut
/// from one continuous stream disagree by the batch period and by drift
/// between the audio and wall clocks -- tens, at worst a few hundred, ms. A
/// stream that went down and came back leaves a gap of a second or more,
/// and speech is not carried over that.
const CONTIGUOUS_MS: u64 = 1_000;

/// The chunk after `meta` on the same track, if it has arrived.
fn successor<'a>(chunks: &'a [ChunkMeta], meta: &ChunkMeta) -> Option<&'a ChunkMeta> {
    chunks.iter().find(|c| c.track == meta.track && c.seq == meta.seq + 1)
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

/// Who the owner is, as [`people::invitees`] needs it to leave them off the
/// invite: every configured mail account's own address and identities, and
/// the profile's name. Best-effort -- an account read failure just means no
/// address is excluded here, which [`identify::identify`]'s own
/// owner-voiceprint exclusion still backs up whenever the owner also has a
/// voiceprint on file. See this module's doc on "excluding owner addresses"
/// for why this is the closest available answer: nothing in the vault names
/// the owner's own address more directly than the accounts they have signed
/// in with.
pub(crate) fn owner_of(vault: &Vault) -> people::Owner {
    let mut addresses = std::collections::HashSet::new();
    if let Ok(accounts) = vault.accounts() {
        for account in accounts {
            addresses.insert(account.address.to_ascii_lowercase());
            for identity in account.identities {
                addresses.insert(identity.address.to_ascii_lowercase());
            }
        }
    }
    let profile = vault.profile().unwrap_or_default();
    let name = format!("{} {}", profile.first_name.trim(), profile.last_name.trim());
    people::Owner { addresses, name: name.trim().to_string() }
}

/// Everybody on `event`'s invite but the owner -- see [`people::invitees`].
pub(crate) fn invitees_of(event: Option<&EventRef>, owner: &people::Owner) -> Vec<Invitee> {
    event.map(|e| people::invitees(e, owner)).unwrap_or_default()
}

/// The invitees as [`Candidate`]s, with a matching voiceprint attached when
/// `voiceprints_on` and one exists -- matched by email first, then by name,
/// and only among voiceprints whose `model` is this session's embedding
/// model (a voiceprint from a different model is never comparable -- see
/// [`Voiceprint::model`](everyday_core::meeting::Voiceprint::model)).
pub(crate) fn candidates_for(
    invitees: &[Invitee],
    voiceprints: &[Voiceprint],
    voiceprints_on: bool,
    embedding_model: &str,
) -> Vec<Candidate> {
    invitees
        .iter()
        .map(|person| {
            let attendee = person.attendee();
            let voiceprint = if voiceprints_on {
                find_voiceprint(&attendee, voiceprints, embedding_model)
            } else {
                None
            };
            Candidate { attendee, voiceprint, declined: person.declined() }
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
    if let Some(email) = &email
        && let Some(v) = usable.clone().find(|v| v.email.as_deref() == Some(email.as_str()))
    {
        return Some(v.clone());
    }
    if let Some(name) = &name
        && let Some(v) = usable.clone().find(|v| v.name.eq_ignore_ascii_case(name))
    {
        return Some(v.clone());
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
/// to one of them, in time order -- with the invite they were matched
/// against, which [`resolve_speakers`] and the note both read again.
pub(crate) struct Identified {
    pub(crate) speakers: Vec<Speaker>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) invitees: Vec<Invitee>,
    /// The owner's label, as the mic track's lines carry it.
    pub(crate) owner_label: String,
    /// Most likely everybody was heard on the one microphone, and their
    /// voices were not told apart: the owner's label is on every line of
    /// it -- see [`Transcript::room`].
    pub(crate) room: bool,
    /// The meeting's names and words, for the summary's spelling rule.
    pub(crate) words: vocab::MeetingWords,
}

/// The meeting's names and words -- see [`vocab::meeting_words`]. The
/// title only when it is a calendar's: one the app made up says nothing.
///
/// Of the owner's own addresses, only the ones on this meeting's invite
/// give a company: the other accounts in the vault -- a side project, an
/// old job -- have nothing to do with this meeting, and must neither be
/// sent to a transcriber nor become words to respell.
pub(crate) fn words_for(
    vault: &Vault,
    event: Option<&EventRef>,
    settings: &MeetingSettings,
) -> vocab::MeetingWords {
    let owner = owner_of(vault);
    let invitees = invitees_of(event, &owner);
    let title = event.map(|e| e.title.as_str()).unwrap_or_default();
    vocab::meeting_words(title, &invitees, &owner_on_invite(&owner, event), &settings.vocabulary)
}

/// The owner as this meeting knows them: their name, and only those of
/// their addresses on its invite -- see [`words_for`].
pub(crate) fn owner_on_invite(owner: &people::Owner, event: Option<&EventRef>) -> people::Owner {
    let on_invite: std::collections::HashSet<String> = event
        .map(|e| {
            e.guests
                .iter()
                .map(|g| g.email.to_ascii_lowercase())
                .chain(e.attendees.iter().filter_map(|a| identify::parse_attendee(a).1))
                .chain(identify::parse_attendee(&e.organizer).1)
                .collect()
        })
        .unwrap_or_default();
    people::Owner {
        addresses: owner.addresses.intersection(&on_invite).cloned().collect(),
        name: owner.name.clone(),
    }
}

/// Whether echo reached the microphone in this recording, from both
/// tracks' audio -- see [`echo::EchoCheck`]. A chunk that cannot be read
/// counts as silence; with no audio at all there is no evidence, and no
/// line is ever taken for an echo. When one track said nothing, there is no
/// line to judge, and the audio is not read at all.
async fn echo_check(
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    chunks: &[ChunkMeta],
    partial: &[TrackSegment],
) -> CommandResult<echo::EchoCheck> {
    let said = |track: Track| partial.iter().any(|s| s.track == track);
    if !said(Track::Mic) || !said(Track::System) {
        return Ok(echo::EchoCheck::none());
    }
    let source = source.clone();
    let chunks = chunks.to_vec();
    blocking(move || {
        let mut envelopes = echo::Envelopes::default();
        for meta in &chunks {
            if let Ok(samples) = source.read_chunk(id, meta.track, meta.seq) {
                envelopes.push(meta.track, meta.start_ms, &samples);
            }
        }
        Ok(echo::EchoCheck::analyse(envelopes))
    })
    .await
}

/// Echo removal, respelling, mic → owner (or, in a room, → voices), system
/// → turns → clusters → speakers, per the plan's "Who said what".
#[allow(clippy::too_many_arguments)]
pub(crate) async fn identify_stage(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    recording: &Recording,
    settings: &MeetingSettings,
    embedding_model: &str,
    #[cfg(feature = "speech")] kit: Option<Arc<SpeechKit>>,
) -> CommandResult<Identified> {
    let id = recording.id;
    let chunks = recording.chunks.clone();

    let check = echo_check(source, id, &chunks, &recording.partial).await?;
    let (mut kept, _echo_found) =
        everyday_core::meeting::merge::remove_echo(recording.partial.clone(), &check);

    let words = {
        let vault = vault.clone();
        let event = recording.event.clone();
        let settings = settings.clone();
        blocking(move || Ok(words_for(&vault, event.as_ref(), &settings))).await?
    };
    let ordinary = vocab::ordinary_words(kept.iter().map(|s| s.text.as_str()));
    let respeller = vocab::Respeller::new(&words);
    for seg in &mut kept {
        seg.text = respeller.respell(&seg.text, &ordinary);
    }

    let mut mic: Vec<TrackSegment> =
        kept.iter().filter(|s| s.track == Track::Mic).cloned().collect();
    let mut system: Vec<TrackSegment> =
        kept.iter().filter(|s| s.track == Track::System).cloned().collect();
    mic.sort_by_key(|s| s.start_ms);
    system.sort_by_key(|s| s.start_ms);

    #[cfg(feature = "speech")]
    {
        let has_hints = system.iter().any(|s| s.speaker_hint.is_some());
        let embeddable =
            system.iter().filter(|s| s.end_ms.saturating_sub(s.start_ms) >= MIN_EMBED_MS).count();
        if !has_hints
            && embeddable < 2
            && let Some(kit) = &kit
        {
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

    let voiceprints_on = settings.voiceprints;
    let invitees = {
        let vault = vault.clone();
        let event = recording.event.clone();
        blocking(move || Ok(invitees_of(event.as_ref(), &owner_of(&vault)))).await?
    };
    let in_room = everyday_core::meeting::heard_in_one_room(&kept, !invitees.is_empty());
    let label = {
        let vault = vault.clone();
        blocking(move || Ok(owner_label(&vault))).await?
    };
    let (candidates, owner_voiceprint) = {
        let vault = vault.clone();
        let invitees = invitees.clone();
        let embedding_model = embedding_model.to_string();
        blocking(move || {
            let voiceprints = vault.voiceprints()?;
            let owner = voiceprints_on
                .then(|| {
                    voiceprints.iter().find(|v| v.is_owner && v.model == embedding_model).cloned()
                })
                .flatten();
            Ok((candidates_for(&invitees, &voiceprints, voiceprints_on, &embedding_model), owner))
        })
        .await?
    };

    // Every system segment becomes one `Turn`, embedded via the speech kit
    // when the feature is compiled in and the segment is long enough. See
    // `MIN_EMBED_MS`.
    let turns = turns_for(
        vault,
        source,
        id,
        &chunks,
        &system,
        Track::System,
        #[cfg(feature = "speech")]
        kit.clone(),
    )
    .await?;

    // Nobody on the other end can be anyone but an invitee, so there are
    // never more distinct voices than invitees -- see `Params::max_voices`.
    let params = identify::Params {
        max_voices: (!invitees.is_empty()).then_some(invitees.len()),
        ..identify::Params::default()
    };
    let mut clusters = identify::identify(&turns, &candidates, params);

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
        clusters[0].label = people::OTHERS.to_string();
    }

    // In a room, the microphone heard everyone. Its voices are told apart
    // only when the owner can be told from the rest by their voiceprint --
    // otherwise one voice split in two by the clustering would hand half of
    // the owner's words to somebody else -- and only when nothing came over
    // the system track, so a voice on the microphone can never be named
    // after a speaker on the call as well. Anything less leaves the room
    // under the owner's label, and the summary is told so.
    let mut mic_clusters: Vec<identify::Cluster> = Vec::new();
    if in_room
        && system.is_empty()
        && let Some(owner_print) = &owner_voiceprint
    {
        let mic_turns = turns_for(
            vault,
            source,
            id,
            &chunks,
            &mic,
            Track::Mic,
            #[cfg(feature = "speech")]
            kit.clone(),
        )
        .await?;
        if mic_turns.iter().filter(|t| !t.embedding.is_empty()).count() >= 2 {
            let mut room_candidates = candidates.clone();
            room_candidates.push(Candidate {
                attendee: label.clone(),
                voiceprint: Some(owner_print.clone()),
                declined: false,
            });
            let params = identify::Params {
                max_voices: Some(invitees.len() + 1),
                owner_may_speak: true,
                ..identify::Params::default()
            };
            let found = fold_into_owner(
                identify::identify(&mic_turns, &room_candidates, params),
                owner_print,
            );
            if found.len() >= 2 && found.iter().any(|c| c.voiceprint_id == Some(owner_print.id)) {
                mic_clusters = found;
            }
        }
    }

    let voiceprints_used = voiceprints_on;
    let owner_voice = owner_voiceprint.as_ref().map(|v| v.id);
    let owner_speaker = |key: u16| Speaker {
        key,
        label: label.clone(),
        email: None,
        voiceprint_id: None,
        how: Attribution::Owner,
        centroid: Vec::new(),
        embedding_model: String::new(),
    };
    let as_speaker = |key: u16, cluster: &identify::Cluster| -> Speaker {
        if owner_voice.is_some() && cluster.voiceprint_id == owner_voice {
            return owner_speaker(key);
        }
        Speaker {
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
        }
    };

    let mut speakers = Vec::new();
    let mut segments = Vec::new();
    let mut next_key = 0u16;
    if mic_clusters.is_empty() {
        if !mic.is_empty() {
            speakers.push(owner_speaker(next_key));
            for seg in &mic {
                segments.push(Segment {
                    start_ms: seg.start_ms,
                    end_ms: seg.end_ms,
                    speaker: next_key,
                    text: seg.text.clone(),
                });
            }
            next_key += 1;
        }
    } else {
        for cluster in &mic_clusters {
            speakers.push(as_speaker(next_key, cluster));
            for &turn_idx in &cluster.turns {
                let seg = &mic[turn_idx];
                segments.push(Segment {
                    start_ms: seg.start_ms,
                    end_ms: seg.end_ms,
                    speaker: next_key,
                    text: seg.text.clone(),
                });
            }
            next_key += 1;
        }
    }
    for cluster in &clusters {
        speakers.push(as_speaker(next_key, cluster));
        for &turn_idx in &cluster.turns {
            let seg = &system[turn_idx];
            segments.push(Segment {
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                speaker: next_key,
                text: seg.text.clone(),
            });
        }
        next_key += 1;
    }
    segments.sort_by_key(|s| s.start_ms);

    // Unnamed voices numbered afresh. Not yet folded down to what the
    // invite has room for: the model reads them as the audio split them,
    // which is what lets it say two labels are one person -- see
    // `resolve_speakers`, which has the last word.
    people::tidy(&mut speakers, &mut segments, None);

    let room = in_room && mic_clusters.is_empty();
    Ok(Identified { speakers, segments, invitees, owner_label: label, room, words })
}

/// How alike a voice on a room's microphone must be to the owner's
/// voiceprint to be taken for the owner's own, split off by the clustering:
/// well under the match threshold, since anything this close is far more
/// likely the owner on a bad stretch of audio than somebody else.
const OWNER_FOLD: f32 = 0.45;

/// `clusters` with every unnamed one that sounds like the owner folded into
/// the owner's -- the one matched to `owner` -- so the owner's voice split in
/// two can never leave half of it to be named after somebody else. A voice
/// another voiceprint matched stays apart.
pub(crate) fn fold_into_owner(
    clusters: Vec<identify::Cluster>,
    owner: &everyday_core::meeting::Voiceprint,
) -> Vec<identify::Cluster> {
    if !clusters.iter().any(|c| c.voiceprint_id == Some(owner.id)) {
        return clusters;
    }
    let near_owner = |c: &identify::Cluster| {
        c.voiceprint_id != Some(owner.id)
            && matches!(c.how, Attribution::Unknown | Attribution::Inferred)
            && owner.centroids.iter().any(|o| identify::cosine(&c.centroid, o) >= OWNER_FOLD)
    };
    let (folded, mut kept): (Vec<_>, Vec<_>) = clusters.into_iter().partition(near_owner);
    if let Some(owner_cluster) = kept.iter_mut().find(|c| c.voiceprint_id == Some(owner.id)) {
        owner_cluster.turns.extend(folded.into_iter().flat_map(|c| c.turns));
        owner_cluster.turns.sort_unstable();
    }
    kept
}

/// One `Turn` per segment of `track`, embedded via the speech kit when the
/// feature is compiled in and the segment is long enough -- see
/// [`MIN_EMBED_MS`].
async fn turns_for(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    chunks: &[ChunkMeta],
    segments: &[TrackSegment],
    track: Track,
    #[cfg(feature = "speech")] kit: Option<Arc<SpeechKit>>,
) -> CommandResult<Vec<Turn>> {
    let mut turns = Vec::with_capacity(segments.len());
    for seg in segments {
        let hint = seg.speaker_hint.as_ref().map(|h| (seg.hint_scope, h.clone()));
        let long_enough = seg.end_ms.saturating_sub(seg.start_ms) >= MIN_EMBED_MS;
        let embedding = if long_enough {
            embed_segment(
                vault,
                source,
                id,
                chunks,
                seg,
                track,
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
    Ok(turns)
}

/// Ask the model to put names to the voices the audio could not, from what
/// was said -- see [`people`]. Only ever an improvement: a failed request
/// or an answer that does not parse leaves the voices as they were, and is
/// logged rather than failing the note, which is worth writing either way.
pub(crate) async fn resolve_speakers(
    summariser: &dyn Summariser,
    identified: &mut Identified,
    budget: u32,
) {
    let Some(req) = people::resolve_request(
        &identified.invitees,
        &identified.owner_label,
        &identified.speakers,
        &identified.segments,
        budget,
    ) else {
        return;
    };
    match summariser.ask(&req.system, &req.user).await {
        Ok(answer) => {
            let resolutions = people::parse_resolutions(&answer);
            people::apply(
                &mut identified.speakers,
                &mut identified.segments,
                &identified.invitees,
                &resolutions,
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e.message,
                "meeting pipeline: could not ask who the unnamed voices were; keeping them unnamed"
            );
        }
    }
    let invited = (!identified.invitees.is_empty()).then_some(identified.invitees.len());
    people::tidy(&mut identified.speakers, &mut identified.segments, invited);
}

#[allow(clippy::too_many_arguments)]
async fn embed_segment(
    vault: &Arc<Vault>,
    source: &Arc<dyn AudioSource>,
    id: RecordingId,
    chunks: &[ChunkMeta],
    seg: &TrackSegment,
    track: Track,
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
            let samples = read_range(source.as_ref(), id, track, &chunks, start_ms, end_ms)?;
            Ok(kit.embed(&samples).unwrap_or_default())
        })
        .await;
    }
    #[cfg(not(feature = "speech"))]
    {
        let _ = (source, id, chunks, seg, track);
        Ok(Vec::new())
    }
}

// ============================================================================
// Summarising
// ============================================================================

/// Build [`Facts`] for the template: title, event, timing, `present` in
/// order of first speech, and the timezone to write times in.
pub(crate) fn facts_for(recording: &Recording, identified: &Identified, tz: &str) -> Facts {
    let mut facts = facts_from(recording, &identified.speakers, &identified.segments, tz);
    facts.invitees = identified.invitees.clone();
    facts
}

fn facts_from(
    recording: &Recording,
    speakers: &[Speaker],
    segments: &[Segment],
    tz: &str,
) -> Facts {
    let mut present = Vec::new();
    let mut present_emails = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for seg in segments {
        if seen.insert(seg.speaker)
            && let Some(speaker) = speakers.iter().find(|s| s.key == seg.speaker)
        {
            if !speaker.label.is_empty() {
                present.push(speaker.label.clone());
            }
            present_emails.extend(speaker.email.clone());
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
        present_emails,
        invitees: Vec::new(),
        tz: recording
            .event
            .as_ref()
            .map(|e| e.tz.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| tz.to_string()),
    }
}

/// Fill the template and write the note: the whole transcript in one
/// request when it fits `budget`, else notes on each part and the note
/// written from those. A section of the template the answer left out is
/// asked for once more on its own, and anything still missing is marked by
/// [`template::enforce_headings`]. Returns the answer alone -- the details
/// block is the writing stage's to prepend.
///
/// `spelling` is [`vocab::spelling_line`]'s, or empty.
///
/// `pub(crate)`: `domains::transcripts` reuses this directly for
/// `preview_meeting_template` and `rewrite_meeting_note`, so a preview and
/// the real note are always produced by exactly the same code.
///
/// [`template::enforce_headings`]: everyday_core::meeting::template::enforce_headings
pub(crate) async fn summarise_stage(
    summariser: &dyn Summariser,
    transcript: &Transcript,
    template_body: &str,
    facts: &Facts,
    budget: u32,
    spelling: &str,
) -> CommandResult<String> {
    use everyday_core::meeting::{prompt, template};

    let filled = template::fill(template_body, facts);
    let people = prompt::people_block(transcript, &facts.invitees);
    let room = prompt::room_note(transcript);
    let cx = prompt::Context { template: &filled, people: &people, spelling, room: &room };
    let pieces = prompt::pieces(transcript, budget);

    let mut parts = Vec::new();
    let answer = if pieces.len() > 1 {
        let n = pieces.len();
        for (i, piece) in pieces.iter().enumerate() {
            let req = prompt::part_notes(&cx, piece, i + 1, n);
            parts.push(prompt::tidy_answer(&summariser.ask(&req.system, &req.user).await?));
        }
        let req = prompt::from_parts(&cx, &parts);
        prompt::tidy_answer(&summariser.ask(&req.system, &req.user).await?)
    } else {
        let lines = pieces.first().map(String::as_str).unwrap_or_default();
        let req = prompt::summarise(&cx, lines);
        prompt::tidy_answer(&summariser.ask(&req.system, &req.user).await?)
    };

    // A model sometimes stops before the end of the template. One more
    // request for only what it left out is cheaper than a note without its
    // next steps; a failure here keeps what was written.
    let missing = template::missing_headings(&filled, &answer);
    let answer = if missing.is_empty() || answer.trim().is_empty() {
        answer
    } else {
        let source = if parts.is_empty() {
            prompt::Source::Transcript(pieces.first().map(String::as_str).unwrap_or_default())
        } else {
            prompt::Source::Parts(&parts)
        };
        let req = prompt::missing_sections(&cx, source, &answer, &missing);
        match summariser.ask(&req.system, &req.user).await {
            Ok(more) => match prompt::take_sections(&more, &missing) {
                Some(sections) => format!("{}\n\n{sections}", answer.trim_end()),
                None => answer,
            },
            Err(e) => {
                tracing::warn!(
                    error = %e.message,
                    "meeting pipeline: could not ask for the sections the note left out"
                );
                answer
            }
        }
    };

    Ok(template::enforce_headings(&filled, &answer))
}

// ============================================================================
// Writing
// ============================================================================

/// The note's title: the calendar's and what this meeting was about -- see
/// [`prompt::title_of`]. A failed request falls back to the date, never to
/// failing the note.
///
/// [`prompt::title_of`]: everyday_core::meeting::prompt::title_of
pub(crate) async fn title_for(
    summariser: &dyn Summariser,
    event_title: &str,
    note: &str,
    date: &str,
) -> String {
    use everyday_core::meeting::prompt;
    let req = prompt::title_request(event_title, note);
    let topic = match summariser.ask(&req.system, &req.user).await {
        Ok(answer) => prompt::clean_title(&answer),
        Err(e) => {
            tracing::warn!(error = %e.message, "meeting pipeline: could not title the note");
            None
        }
    };
    prompt::title_of(event_title, topic.as_deref(), date)
}

/// Tags for a meeting note: `meeting` on every one, so all of them are one
/// click away, and the meeting's own name once it turns out to repeat -- a
/// weekly meeting's notes, together. A one-off meeting gets no tag of its
/// own; it would only crowd the list.
///
/// "Repeats" means an earlier recording of the same series, finished into
/// a note, of a *different* occurrence: two recordings of one meeting (a
/// restart halfway through) are not a series. The first time that is true,
/// the earlier notes are tagged as well, so the tag finds every week, the
/// first included. Returns the tags and the ids of earlier notes it
/// changed. Only the most recent finished recordings on the calendar are
/// looked at: a series that has not met in that long has its tag already.
pub(crate) fn tags_for(vault: &Vault, recording: &Recording) -> (Vec<String>, Vec<NoteId>) {
    use everyday_core::meeting::detect::series_key_of;
    let mut tags = vec!["meeting".to_string()];
    let Some(event) = &recording.event else { return (tags, Vec::new()) };
    let slug = slug_of(&event.title);
    if slug.is_empty() || slug == "meeting" {
        return (tags, Vec::new());
    }
    let key = series_key_of(&event.uid, event.series.as_deref());
    let earlier: Vec<NoteId> = vault
        .recordings(&everyday_core::RecordingQuery {
            stages: vec![Stage::Done.as_str().to_string()],
            calendar_id: Some(event.calendar_id),
            limit: Some(200),
            ..Default::default()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.id != recording.id)
        .filter(|r| {
            r.event.as_ref().is_some_and(|e| {
                e.start != event.start && series_key_of(&e.uid, e.series.as_deref()) == key
            })
        })
        .filter_map(|r| r.note_id)
        .collect();
    if earlier.is_empty() {
        return (tags, Vec::new());
    }
    let mut changed = Vec::new();
    for id in earlier {
        let Ok(mut note) = vault.note(id) else { continue };
        if note.tags.contains(&slug) {
            continue;
        }
        // Against the copy just read, so an edit made in the meantime is
        // never overwritten -- it just goes without the tag.
        let read_at = note.updated_at;
        note.tags.push(slug.clone());
        note.updated_at = Timestamp::now();
        match vault.save_note(&note, Some(read_at)) {
            Ok(()) => changed.push(id),
            Err(e) => {
                tracing::warn!(note = %id, error = %e, "meeting pipeline: could not tag an earlier note")
            }
        }
    }
    tags.push(slug);
    (tags, changed)
}

/// "Weekly Sync (Eng)" → "weekly-sync-eng".
fn slug_of(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    out.chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}

/// Purpose for a meeting note: the calendar's own role, or none. Delegates
/// to `everyday_core::meeting::filing::purpose_for`, now that it has landed.
pub(crate) fn purpose_for(
    vault: &Vault,
    event: Option<&EventRef>,
) -> Option<everyday_core::Purpose> {
    let calendar = event.and_then(|e| vault.calendar(e.calendar_id).ok());
    everyday_core::meeting::filing::purpose_for(calendar.as_ref())
}
