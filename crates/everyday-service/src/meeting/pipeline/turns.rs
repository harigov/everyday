use everyday_core::meeting::SAMPLE_RATE;

use crate::meeting::transcribe::Limits;

// ============================================================================
// VAD grouping and the offset map back to recording time
// ============================================================================

/// Silence inserted between two turns folded into the same request, so a
/// recogniser has a real gap to key sentence boundaries off rather than two
/// turns run together. Not audio that was ever recorded -- purely a splice.
pub(crate) const GROUP_SILENCE_MS: u64 = 300;

/// Where one turn, once folded into a [`Grouped`] buffer, actually sits: its
/// own span inside that buffer, and where it really was in the recording.
/// What [`map_segment`] reads to translate a backend's answer back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TurnMap {
    local_start_ms: u64,
    local_end_ms: u64,
    recording_start_ms: u64,
}

/// One request's worth of audio: turns concatenated up to the backend's own
/// [`Limits`], with the map [`map_segment`] needs to undo the concatenation
/// afterwards.
#[derive(Debug, Clone)]
pub(crate) struct Grouped {
    pub(crate) samples: Vec<i16>,
    pub(crate) map: Vec<TurnMap>,
}

pub(crate) fn index_of(ms: u64) -> usize {
    (ms * u64::from(SAMPLE_RATE) / 1000) as usize
}

pub(crate) fn ms_of(samples: usize) -> u64 {
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
pub(crate) fn group_turns(
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
pub(crate) fn map_segment(map: &[TurnMap], local_start_ms: u64, local_end_ms: u64) -> (u64, u64) {
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
