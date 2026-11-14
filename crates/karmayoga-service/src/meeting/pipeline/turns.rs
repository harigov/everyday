use karmayoga_core::meeting::SAMPLE_RATE;

use crate::meeting::transcribe::Limits;

// ============================================================================
// VAD grouping and the offset map back to recording time
// ============================================================================

/// Silence inserted between two turns folded into the same request, so a
/// recogniser has a real gap to key sentence boundaries off rather than two
/// turns run together. Not audio that was ever recorded -- purely a splice.
pub(crate) const GROUP_SILENCE_MS: u64 = 300;

/// How close to a chunk's edge a speech turn must reach to be taken as
/// running across it. Voice detection pads every turn by 150 ms and clamps
/// the pad to the clip, so speech that runs to the boundary ends exactly at
/// it; a little more than the pad covers a last word that stopped just
/// short.
pub(crate) const EDGE_MS: u64 = 250;

/// The longest one turn may grow to by being carried across a chunk
/// boundary. Whisper reads 30 seconds at a time and drops what is past it,
/// so a sentence that runs on is cut -- at its quietest moment, see
/// [`quietest_cut`] -- rather than handed over whole.
pub(crate) const MAX_CARRIED_TURN_MS: u64 = 28_000;

/// How far back from [`MAX_CARRIED_TURN_MS`] to look for a quiet moment to
/// cut a sentence that runs on at.
const CUT_SEARCH_MS: u64 = 3_000;

/// Below this, a stub of speech left over at a chunk's start once its
/// carried part is trimmed away is not worth a request of its own.
const MIN_TURN_MS: u64 = 250;

/// The chunk's turns once the part of it already sent with the previous
/// chunk (its first `head_ms`) is taken off: a turn wholly inside it goes,
/// and one that runs past it starts where it ends.
pub(crate) fn after_head(turns: &[(u64, u64)], head_ms: u64) -> Vec<(u64, u64)> {
    turns
        .iter()
        .filter(|(_, end)| *end > head_ms)
        .map(|&(start, end)| (start.max(head_ms), end))
        .filter(|(start, end)| end.saturating_sub(*start) >= MIN_TURN_MS)
        .collect()
}

/// Does the chunk's last turn run to its end -- somebody still talking when
/// the chunk closed?
pub(crate) fn runs_over(turns: &[(u64, u64)], len_ms: u64) -> bool {
    turns.last().is_some_and(|&(_, end)| end + EDGE_MS >= len_ms)
}

/// How far into the next chunk a sentence running over the boundary goes:
/// to the end of the next chunk's first turn, when that turn starts at its
/// very beginning. `None` when the next chunk starts quiet -- the speech
/// stopped at the boundary after all.
pub(crate) fn carry_into(next_turns: &[(u64, u64)]) -> Option<u64> {
    next_turns.first().filter(|&&(start, _)| start <= EDGE_MS).map(|&(_, end)| end)
}

/// Where in the next chunk a sentence carried over from the previous one
/// stops: [`carry_into`]'s end, unless that would make the turn longer than
/// [`MAX_CARRIED_TURN_MS`], when it is cut at the quietest moment before
/// that limit instead. `carried_ms` is how much of the turn the previous
/// chunk holds. `None` when there is no room left to carry anything.
pub(crate) fn carry_end(next: &[i16], carried_ms: u64, wanted_ms: u64) -> Option<u64> {
    if carried_ms + wanted_ms <= MAX_CARRIED_TURN_MS {
        return (wanted_ms > 0).then_some(wanted_ms);
    }
    let limit = MAX_CARRIED_TURN_MS.saturating_sub(carried_ms).min(wanted_ms);
    if limit < 1_000 {
        return None;
    }
    Some(quietest_cut(next, limit.saturating_sub(CUT_SEARCH_MS), limit))
}

/// The middle of the quietest 100 ms of `samples` between `from_ms` and
/// `to_ms` -- the least bad place to cut speech that does not pause. The
/// first quietest window wins a tie; `to_ms` when the range is too short to
/// hold a window.
pub(crate) fn quietest_cut(samples: &[i16], from_ms: u64, to_ms: u64) -> u64 {
    const WINDOW_MS: u64 = 100;
    const STEP_MS: u64 = 10;
    let to_ms = to_ms.min(ms_of(samples.len()));
    let mut best: Option<(f64, u64)> = None;
    let mut at = from_ms;
    while at + WINDOW_MS <= to_ms {
        let window = &samples[index_of(at)..index_of(at + WINDOW_MS)];
        let energy: f64 = window.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        if best.is_none_or(|(e, _)| energy < e) {
            best = Some((energy, at + WINDOW_MS / 2));
        }
        at += STEP_MS;
    }
    best.map_or(to_ms, |(_, mid)| mid)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_part_already_sent_is_taken_off_the_chunks_start() {
        let turns = [(0, 2_000), (2_500, 4_000), (6_000, 9_000)];
        assert_eq!(after_head(&turns, 0), turns.to_vec());
        // The first turn was the end of the previous chunk's sentence.
        assert_eq!(after_head(&turns, 2_000), vec![(2_500, 4_000), (6_000, 9_000)]);
        // Cut short at a quiet moment inside a turn: the rest still goes.
        assert_eq!(after_head(&turns, 3_000), vec![(3_000, 4_000), (6_000, 9_000)]);
        // A stub too short to send is dropped.
        assert_eq!(after_head(&turns, 3_900), vec![(6_000, 9_000)]);
    }

    #[test]
    fn speech_at_both_sides_of_a_boundary_is_one_sentence() {
        let len = 30_000;
        assert!(runs_over(&[(1_000, 5_000), (27_000, 30_000)], len));
        assert!(runs_over(&[(27_000, 29_800)], len), "a last word just short of the edge");
        assert!(!runs_over(&[(27_000, 29_000)], len));
        assert!(!runs_over(&[], len));

        assert_eq!(carry_into(&[(0, 1_800), (5_000, 7_000)]), Some(1_800));
        assert_eq!(carry_into(&[(200, 1_800)]), Some(1_800));
        assert_eq!(carry_into(&[(900, 1_800)]), None, "the next chunk starts quiet");
        assert_eq!(carry_into(&[]), None);
    }

    #[test]
    fn a_carried_sentence_stops_where_it_ends_or_at_a_quiet_moment() {
        let next = vec![1_000i16; index_of(30_000)];
        assert_eq!(carry_end(&next, 4_000, 2_000), Some(2_000));
        assert_eq!(carry_end(&next, 4_000, 0), None);

        // Too long to carry whole: cut at the quietest 100 ms before the limit.
        let mut loud = vec![8_000i16; index_of(30_000)];
        let quiet_at = 20_000;
        for s in &mut loud[index_of(quiet_at)..index_of(quiet_at + 100)] {
            *s = 10;
        }
        let end = carry_end(&loud, 6_000, 30_000).unwrap();
        assert!(end.abs_diff(quiet_at + 50) <= 10, "{end}");
        assert!(6_000 + end <= MAX_CARRIED_TURN_MS);

        // No room left at all: nothing is carried.
        assert_eq!(carry_end(&loud, MAX_CARRIED_TURN_MS - 500, 10_000), None);
    }

    #[test]
    fn the_quietest_moment_is_found_and_a_short_range_falls_back_to_its_end() {
        let mut samples = vec![5_000i16; index_of(5_000)];
        for s in &mut samples[index_of(3_000)..index_of(3_100)] {
            *s = 0;
        }
        assert_eq!(quietest_cut(&samples, 1_000, 5_000), 3_050);
        assert_eq!(quietest_cut(&samples, 1_000, 1_050), 1_050);
    }
}
