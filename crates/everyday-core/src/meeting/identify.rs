//! Which voice is whose, given embeddings. Pure: the service computes the
//! vectors, this decides.

use super::{Attribution, Voiceprint};
use crate::timestamped::Timestamped;
use std::collections::BTreeMap;

/// One turn on the system track, with its embedding.
#[derive(Debug, Clone)]
pub struct Turn {
    /// Index into the caller's segment list.
    pub index: usize,
    pub duration_ms: u64,
    /// The backend's hint, scoped (see `TrackSegment::hint_scope`).
    pub hint: Option<(u32, String)>,
    /// Empty when no embedding could be made (too short).
    pub embedding: Vec<f32>,
}

/// A candidate person: an attendee string, and their voiceprint if any.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// As the calendar gave it: "Priya Raman <priya@x.com>", or just an address.
    pub attendee: String,
    pub voiceprint: Option<Voiceprint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Indices of the turns in this cluster.
    pub turns: Vec<usize>,
    pub centroid: Vec<f32>,
    pub label: String,
    pub email: Option<String>,
    pub voiceprint_id: Option<crate::id::VoiceprintId>,
    pub how: Attribution,
}

/// Tuning, with defaults that favour "Unknown" over a wrong name.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Cosine similarity at or above which two turns are one voice.
    pub cluster_threshold: f32,
    /// Cosine similarity at or above which a cluster is a known voice.
    pub match_threshold: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self { cluster_threshold: 0.55, match_threshold: 0.62 }
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

fn normalise(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 { v.to_vec() } else { v.iter().map(|x| x / norm).collect() }
}

/// Cluster the turns across the whole meeting (agglomerative, average
/// linkage, stopping at `cluster_threshold`), using backend hints to keep
/// turns with the same scoped hint together when embeddings are missing.
/// Then name clusters: voiceprint match among candidates only, then the
/// one-left-over inference, then "Unknown N" in order of first speech.
/// Turns without embeddings and without hints join the nearest-in-time
/// cluster.
///
/// `turns` is assumed to be in chronological order -- it carries no
/// timestamp of its own, so "nearest in time" for a turn with neither an
/// embedding nor a hint is read as nearest by position in this slice.
///
/// Clustering is a full pairwise comparison across however many distinct
/// voices the hints have not already collapsed together, which is fine for
/// the few hundred turns one call produces; a transcript with thousands of
/// turns would want a cheaper linkage than this, which this does not
/// attempt.
pub fn identify(turns: &[Turn], candidates: &[Candidate], params: Params) -> Vec<Cluster> {
    if turns.is_empty() {
        return Vec::new();
    }

    // 1. Group every turn by its scoped hint; hint-less turns are handled
    //    individually below.
    let mut hint_groups: BTreeMap<(u32, String), Vec<usize>> = BTreeMap::new();
    let mut no_hint: Vec<usize> = Vec::new();
    for (pos, turn) in turns.iter().enumerate() {
        match &turn.hint {
            Some(key) => hint_groups.entry(key.clone()).or_default().push(pos),
            None => no_hint.push(pos),
        }
    }

    // 2. Split into clusters the agglomerative pass can compare (at least
    //    one embedded member), clusters it cannot (a hint group with no
    //    embedding anywhere in it, which stays exactly as the hint left
    //    it), and orphans (no hint, no embedding) for the nearest-in-time
    //    pass at the end.
    let has_embedding = |pos: usize| !turns[pos].embedding.is_empty();

    let mut mergeable: Vec<Vec<usize>> = Vec::new();
    let mut settled: Vec<Vec<usize>> = Vec::new();
    let mut orphans: Vec<usize> = Vec::new();

    for members in hint_groups.into_values() {
        if members.iter().any(|&p| has_embedding(p)) {
            mergeable.push(members);
        } else {
            settled.push(members);
        }
    }
    for pos in no_hint {
        if has_embedding(pos) {
            mergeable.push(vec![pos]);
        } else {
            orphans.push(pos);
        }
    }

    // 3. Average-linkage agglomerative clustering: repeatedly merge the two
    //    clusters whose members are, on average, most alike, stopping once
    //    the best remaining pair falls under `cluster_threshold`.
    loop {
        if mergeable.len() < 2 {
            break;
        }
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..mergeable.len() {
            for j in (i + 1)..mergeable.len() {
                let Some(score) = avg_pairwise_similarity(&mergeable[i], &mergeable[j], turns)
                else {
                    continue;
                };
                let better = match best {
                    None => true,
                    Some((_, _, current)) => score > current,
                };
                if better {
                    best = Some((i, j, score));
                }
            }
        }
        match best {
            Some((i, j, score)) if score >= params.cluster_threshold => {
                let removed = mergeable.remove(j);
                mergeable[i].extend(removed);
            }
            _ => break,
        }
    }

    let mut final_clusters: Vec<Vec<usize>> = mergeable;
    final_clusters.extend(settled);

    // 4. Orphans join the nearest-in-time cluster. If there is nothing at
    //    all to be near -- every turn was an orphan -- they all become one
    //    cluster: with no signal to tell them apart, one "Unknown" is more
    //    honest than several.
    let mut position_owner: Vec<Option<usize>> = vec![None; turns.len()];
    for (ci, members) in final_clusters.iter().enumerate() {
        for &pos in members {
            position_owner[pos] = Some(ci);
        }
    }
    orphans.sort_unstable();
    for pos in orphans {
        match nearest_owned_position(pos, &position_owner) {
            Some(owner) => {
                final_clusters[owner].push(pos);
                position_owner[pos] = Some(owner);
            }
            None => {
                let ci = final_clusters.len();
                final_clusters.push(vec![pos]);
                position_owner[pos] = Some(ci);
            }
        }
    }
    for members in &mut final_clusters {
        members.sort_unstable();
    }

    // 5. A centroid per cluster: the L2-normalised mean of its embedded
    //    members. Empty when a cluster (only ever a hint-only one) has no
    //    embedded member to offer a voiceprint match.
    let centroids: Vec<Vec<f32>> =
        final_clusters.iter().map(|members| centroid_of(members, turns)).collect();

    name_clusters(&final_clusters, &centroids, candidates, params)
}

fn avg_pairwise_similarity(a: &[usize], b: &[usize], turns: &[Turn]) -> Option<f32> {
    let mut sum = 0.0f32;
    let mut count = 0u32;
    for &i in a {
        if turns[i].embedding.is_empty() {
            continue;
        }
        for &j in b {
            if turns[j].embedding.is_empty() {
                continue;
            }
            sum += cosine(&turns[i].embedding, &turns[j].embedding);
            count += 1;
        }
    }
    if count == 0 { None } else { Some(sum / count as f32) }
}

fn centroid_of(members: &[usize], turns: &[Turn]) -> Vec<f32> {
    let embedded: Vec<&Vec<f32>> =
        members.iter().map(|&p| &turns[p].embedding).filter(|e| !e.is_empty()).collect();
    let Some(dim) = embedded.first().map(|e| e.len()) else {
        return Vec::new();
    };
    let mut mean = vec![0.0f32; dim];
    for e in &embedded {
        for (m, v) in mean.iter_mut().zip(e.iter()) {
            *m += v;
        }
    }
    let n = embedded.len() as f32;
    for m in &mut mean {
        *m /= n;
    }
    normalise(&mean)
}

/// The nearest turn position that already belongs to a cluster, searching
/// outward from `pos` and preferring the earlier side on a tie.
fn nearest_owned_position(pos: usize, owners: &[Option<usize>]) -> Option<usize> {
    let n = owners.len();
    let mut delta = 1usize;
    loop {
        let left = pos.checked_sub(delta);
        if let Some(owner) = left.and_then(|p| owners[p]) {
            return Some(owner);
        }
        let right = pos + delta;
        if right < n {
            if let Some(owner) = owners[right] {
                return Some(owner);
            }
        }
        if left.is_none() && right >= n {
            return None;
        }
        delta += 1;
    }
}

/// Name every cluster: a voiceprint match first, then the one-left-over
/// inference, then "Unknown N" for whatever is left, numbered by first
/// speech.
fn name_clusters(
    final_clusters: &[Vec<usize>],
    centroids: &[Vec<f32>],
    candidates: &[Candidate],
    params: Params,
) -> Vec<Cluster> {
    // The owner is mic-track, always; their voiceprint is never a candidate
    // for a system-track cluster.
    let usable: Vec<usize> = (0..candidates.len())
        .filter(|&i| !candidates[i].voiceprint.as_ref().is_some_and(|v| v.is_owner))
        .collect();

    let mut attribution: Vec<Option<Attribution>> = vec![None; final_clusters.len()];
    let mut label: Vec<Option<String>> = vec![None; final_clusters.len()];
    let mut email: Vec<Option<String>> = vec![None; final_clusters.len()];
    let mut voiceprint_id: Vec<Option<crate::id::VoiceprintId>> = vec![None; final_clusters.len()];

    // Every (cluster, candidate) pair at or above the match threshold,
    // best first, so a contested voiceprint or cluster goes to whichever
    // pairing is strongest, not whichever is considered first.
    let mut scored: Vec<(f32, usize, usize)> = Vec::new();
    for (ci, centroid) in centroids.iter().enumerate() {
        if centroid.is_empty() {
            continue;
        }
        for &cand_idx in &usable {
            let Some(vp) = &candidates[cand_idx].voiceprint else { continue };
            let score =
                vp.centroids.iter().map(|c| cosine(centroid, c)).fold(f32::NEG_INFINITY, f32::max);
            if score >= params.match_threshold {
                scored.push((score, ci, cand_idx));
            }
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut candidate_used = vec![false; candidates.len()];
    for (score, ci, cand_idx) in scored {
        if attribution[ci].is_some() || candidate_used[cand_idx] {
            continue;
        }
        let vp = candidates[cand_idx].voiceprint.as_ref().expect("scored only voiceprints");
        attribution[ci] = Some(Attribution::Matched { score });
        label[ci] = Some(vp.name.clone());
        email[ci] = vp.email.clone().or_else(|| parse_attendee(&candidates[cand_idx].attendee).1);
        voiceprint_id[ci] = Some(vp.id);
        candidate_used[cand_idx] = true;
    }

    // The one-left-over inference: exactly one usable attendee unmatched,
    // and exactly one cluster still unnamed.
    let unmatched: Vec<usize> = usable.iter().copied().filter(|&i| !candidate_used[i]).collect();
    let unnamed: Vec<usize> =
        (0..final_clusters.len()).filter(|&i| attribution[i].is_none()).collect();
    if unmatched.len() == 1 && unnamed.len() == 1 {
        let cand_idx = unmatched[0];
        let ci = unnamed[0];
        let candidate = &candidates[cand_idx];
        let (name, addr) = parse_attendee(&candidate.attendee);
        attribution[ci] = Some(Attribution::Inferred);
        label[ci] =
            Some(name.or_else(|| addr.clone()).unwrap_or_else(|| candidate.attendee.clone()));
        email[ci] = addr.or_else(|| candidate.voiceprint.as_ref().and_then(|v| v.email.clone()));
        voiceprint_id[ci] = candidate.voiceprint.as_ref().map(|v| v.id);
    }

    // Whatever is left: "Unknown N", numbered by first speech.
    let mut remaining: Vec<usize> =
        (0..final_clusters.len()).filter(|&i| attribution[i].is_none()).collect();
    remaining.sort_by_key(|&i| final_clusters[i].first().copied().unwrap_or(usize::MAX));
    for (n, ci) in remaining.into_iter().enumerate() {
        attribution[ci] = Some(Attribution::Unknown);
        label[ci] = Some(format!("Unknown {}", n + 1));
    }

    (0..final_clusters.len())
        .map(|ci| Cluster {
            turns: final_clusters[ci].clone(),
            centroid: centroids[ci].clone(),
            label: label[ci].clone().unwrap_or_default(),
            email: email[ci].clone(),
            voiceprint_id: voiceprint_id[ci],
            how: attribution[ci].unwrap_or(Attribution::Unknown),
        })
        .collect()
}

/// Fold a new centroid into a voiceprint: merge into the nearest existing
/// centroid when similar enough (running mean weighted by samples), else
/// add one, keeping at most `MAX_CENTROIDS`.
pub fn fold_into(voiceprint: &mut Voiceprint, centroid: &[f32], params: Params) {
    if centroid.is_empty() {
        return;
    }

    let mut nearest: Option<(usize, f32)> = None;
    for (i, existing) in voiceprint.centroids.iter().enumerate() {
        let score = cosine(existing, centroid);
        let better = match nearest {
            None => true,
            Some((_, current)) => score > current,
        };
        if better {
            nearest = Some((i, score));
        }
    }

    if let Some((i, score)) = nearest {
        if score >= params.match_threshold {
            let n = voiceprint.samples as f32;
            let merged: Vec<f32> = voiceprint.centroids[i]
                .iter()
                .zip(centroid)
                .map(|(existing, new)| (existing * n + new) / (n + 1.0))
                .collect();
            voiceprint.centroids[i] = normalise(&merged);
            voiceprint.samples += 1;
            voiceprint.touch();
            return;
        }
    }

    if voiceprint.centroids.len() >= super::MAX_CENTROIDS {
        // No room for a new voice of the same person: replace whichever
        // existing centroid is closest to this one, since that is the
        // direction already best covered and the one whose loss teaches
        // the voiceprint the least.
        if let Some((i, _)) = nearest {
            voiceprint.centroids[i] = centroid.to_vec();
        }
    } else {
        voiceprint.centroids.push(centroid.to_vec());
    }
    voiceprint.samples += 1;
    voiceprint.touch();
}

/// Split an attendee string into (name, email). Either may be absent.
///
/// Handles a display name with an address in angle brackets (quoted or
/// not), a bare `mailto:` address, a bare address, and a bare name.
pub fn parse_attendee(attendee: &str) -> (Option<String>, Option<String>) {
    let s = attendee.trim();
    if s.is_empty() {
        return (None, None);
    }

    if let Some(lt) = s.find('<') {
        if let Some(gt_rel) = s[lt..].find('>') {
            let email_part = s[lt + 1..lt + gt_rel].trim();
            let name_part = s[..lt].trim().trim_matches('"').trim();
            let email = email_part.strip_prefix("mailto:").unwrap_or(email_part).trim();
            return (
                if name_part.is_empty() { None } else { Some(name_part.to_string()) },
                if email.is_empty() { None } else { Some(email.to_ascii_lowercase()) },
            );
        }
    }

    if let Some(rest) = s.strip_prefix("mailto:") {
        let email = rest.trim();
        return (None, if email.is_empty() { None } else { Some(email.to_ascii_lowercase()) });
    }

    if s.contains('@') && !s.chars().any(char::is_whitespace) {
        return (None, Some(s.to_ascii_lowercase()));
    }

    (Some(s.to_string()), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::VoiceprintId;
    use jiff::Timestamp;

    // A tiny deterministic PRNG so tests do not depend on `rand`'s exact
    // algorithm across versions, and so a failure reproduces byte for byte.
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((self.0 >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    const DIM: usize = 8;

    fn base_vector(seed: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; DIM];
        v[seed % DIM] = 1.0;
        v[(seed + 1) % DIM] = 0.5;
        v
    }

    /// A noisy copy of `base`: close enough to cluster with its siblings,
    /// far enough that it is not a bit-for-bit duplicate.
    fn noisy(base: &[f32], rng: &mut Lcg, amount: f32) -> Vec<f32> {
        let v: Vec<f32> = base.iter().map(|x| x + rng.next_f32() * amount).collect();
        normalise(&v)
    }

    fn turn(index: usize, embedding: Vec<f32>) -> Turn {
        Turn { index, duration_ms: 2_000, hint: None, embedding }
    }

    fn hinted_turn(index: usize, scope: u32, hint: &str, embedding: Vec<f32>) -> Turn {
        Turn { index, duration_ms: 2_000, hint: Some((scope, hint.to_string())), embedding }
    }

    fn voiceprint(name: &str, centroid: Vec<f32>) -> Voiceprint {
        Voiceprint {
            id: VoiceprintId::new(),
            name: name.to_string(),
            email: Some(format!("{}@example.com", name.to_ascii_lowercase().replace(' ', "."))),
            is_owner: false,
            model: "test".into(),
            centroids: vec![centroid],
            samples: 1,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn cosine_of_identical_vectors_is_one_and_orthogonal_is_zero() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert_eq!(cosine(&[], &[1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn parse_attendee_handles_every_shape() {
        assert_eq!(
            parse_attendee("Priya Raman <priya@x.com>"),
            (Some("Priya Raman".to_string()), Some("priya@x.com".to_string()))
        );
        assert_eq!(
            parse_attendee("\"Raman, Priya\" <p@x.com>"),
            (Some("Raman, Priya".to_string()), Some("p@x.com".to_string()))
        );
        assert_eq!(parse_attendee("mailto:p@x.com"), (None, Some("p@x.com".to_string())));
        assert_eq!(parse_attendee("p@x.com"), (None, Some("p@x.com".to_string())));
        assert_eq!(parse_attendee("Priya Raman"), (Some("Priya Raman".to_string()), None));
        assert_eq!(parse_attendee(""), (None, None));
    }

    #[test]
    fn empty_input_produces_no_clusters() {
        assert!(identify(&[], &[], Params::default()).is_empty());
    }

    #[test]
    fn three_voices_of_noisy_embeddings_cluster_separately_and_match_voiceprints() {
        let mut rng = Lcg(42);
        let a = base_vector(0);
        let b = base_vector(3);
        let c = base_vector(6);

        let mut turns = Vec::new();
        // A, B, C, A, B, C -- interleaved, the way a real conversation is.
        for (i, base) in [&a, &b, &c, &a, &b, &c].into_iter().enumerate() {
            turns.push(turn(i, noisy(base, &mut rng, 0.05)));
        }

        let candidates = vec![
            Candidate {
                attendee: "Alice <alice@x.com>".into(),
                voiceprint: Some(voiceprint("Alice", a.clone())),
            },
            Candidate {
                attendee: "Bob <bob@x.com>".into(),
                voiceprint: Some(voiceprint("Bob", b.clone())),
            },
            Candidate {
                attendee: "Carol <carol@x.com>".into(),
                voiceprint: Some(voiceprint("Carol", c.clone())),
            },
        ];

        let clusters = identify(&turns, &candidates, Params::default());
        assert_eq!(clusters.len(), 3, "{clusters:#?}");

        let labels: std::collections::BTreeSet<String> =
            clusters.iter().map(|c| c.label.clone()).collect();
        assert_eq!(
            labels,
            ["Alice", "Bob", "Carol"].into_iter().map(String::from).collect(),
            "{clusters:#?}"
        );
        for cluster in &clusters {
            assert!(matches!(cluster.how, Attribution::Matched { .. }), "{cluster:#?}");
        }

        // Every turn is accounted for exactly once.
        let mut all_turns: Vec<usize> = clusters.iter().flat_map(|c| c.turns.clone()).collect();
        all_turns.sort_unstable();
        assert_eq!(all_turns, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn no_voiceprint_is_reused_across_two_clusters() {
        // Two clearly different voices, but only one voiceprint on file.
        // Both should not claim "Matched" against the same voiceprint.
        let mut rng = Lcg(7);
        let a = base_vector(0);
        let b = base_vector(4);
        let turns = vec![
            turn(0, noisy(&a, &mut rng, 0.02)),
            turn(1, noisy(&a, &mut rng, 0.02)),
            turn(2, noisy(&b, &mut rng, 0.02)),
            turn(3, noisy(&b, &mut rng, 0.02)),
        ];
        // A voiceprint that (implausibly) matches both clusters reasonably
        // well; a genuine cosine match against a's noisy copies scores
        // highest, so it should be claimed by that cluster only.
        let candidates = vec![Candidate {
            attendee: "Alice <alice@x.com>".into(),
            voiceprint: Some(voiceprint("Alice", a.clone())),
        }];
        let clusters = identify(&turns, &candidates, Params::default());
        let matched: Vec<&Cluster> =
            clusters.iter().filter(|c| matches!(c.how, Attribution::Matched { .. })).collect();
        assert_eq!(matched.len(), 1, "{clusters:#?}");
        assert_eq!(matched[0].voiceprint_id, Some(candidates[0].voiceprint.as_ref().unwrap().id));
    }

    #[test]
    fn hints_alone_group_turns_with_no_embeddings_at_all() {
        let turns = vec![
            hinted_turn(0, 1, "speaker_1", Vec::new()),
            hinted_turn(1, 1, "speaker_2", Vec::new()),
            hinted_turn(2, 1, "speaker_1", Vec::new()),
        ];
        let clusters = identify(&turns, &[], Params::default());
        assert_eq!(clusters.len(), 2, "{clusters:#?}");
        let mut by_size: Vec<usize> = clusters.iter().map(|c| c.turns.len()).collect();
        by_size.sort_unstable();
        assert_eq!(by_size, vec![1, 2]);
        for cluster in &clusters {
            assert!(matches!(cluster.how, Attribution::Unknown));
        }
        assert!(clusters.iter().any(|c| c.label == "Unknown 1"));
        assert!(clusters.iter().any(|c| c.label == "Unknown 2"));
    }

    #[test]
    fn turns_with_neither_hint_nor_embedding_join_the_nearest_in_time_cluster() {
        let mut rng = Lcg(3);
        let a = base_vector(0);
        let turns = vec![
            turn(0, noisy(&a, &mut rng, 0.02)),
            turn(1, Vec::new()), // no hint, no embedding: should join turn 0's cluster
        ];
        let clusters = identify(&turns, &[], Params::default());
        assert_eq!(clusters.len(), 1, "{clusters:#?}");
        assert_eq!(clusters[0].turns, vec![0, 1]);
    }

    #[test]
    fn everybody_an_orphan_becomes_one_cluster_rather_than_many() {
        let turns = vec![turn(0, Vec::new()), turn(1, Vec::new()), turn(2, Vec::new())];
        let clusters = identify(&turns, &[], Params::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].turns, vec![0, 1, 2]);
    }

    #[test]
    fn the_inference_rule_names_the_only_cluster_left_after_one_match() {
        let mut rng = Lcg(11);
        let a = base_vector(0);
        let b = base_vector(4);
        let turns = vec![
            turn(0, noisy(&a, &mut rng, 0.02)),
            turn(1, noisy(&a, &mut rng, 0.02)),
            turn(2, noisy(&b, &mut rng, 0.02)),
            turn(3, noisy(&b, &mut rng, 0.02)),
        ];
        let candidates = vec![
            Candidate {
                attendee: "Alice <alice@x.com>".into(),
                voiceprint: Some(voiceprint("Alice", a.clone())),
            },
            // Dev has no voiceprint at all: the only attendee left over
            // once Alice is matched, for the only cluster left unnamed.
            Candidate { attendee: "Dev <dev@x.com>".into(), voiceprint: None },
        ];
        let clusters = identify(&turns, &candidates, Params::default());
        assert_eq!(clusters.len(), 2, "{clusters:#?}");
        let inferred: Vec<&Cluster> =
            clusters.iter().filter(|c| matches!(c.how, Attribution::Inferred)).collect();
        assert_eq!(inferred.len(), 1, "{clusters:#?}");
        assert_eq!(inferred[0].label, "Dev");
        assert_eq!(inferred[0].email.as_deref(), Some("dev@x.com"));
    }

    #[test]
    fn the_inference_rule_does_not_fire_with_two_unmatched_candidates() {
        let mut rng = Lcg(23);
        let a = base_vector(0);
        let b = base_vector(4);
        let turns = vec![turn(0, noisy(&a, &mut rng, 0.02)), turn(1, noisy(&b, &mut rng, 0.02))];
        // Neither Dev nor Eve has a voiceprint, and there is no matched
        // cluster to eliminate down to one -- both clusters stay Unknown.
        let candidates = vec![
            Candidate { attendee: "Dev <dev@x.com>".into(), voiceprint: None },
            Candidate { attendee: "Eve <eve@x.com>".into(), voiceprint: None },
        ];
        let clusters = identify(&turns, &candidates, Params::default());
        assert!(clusters.iter().all(|c| matches!(c.how, Attribution::Unknown)), "{clusters:#?}");
    }

    #[test]
    fn an_owner_voiceprint_is_never_a_candidate() {
        let mut rng = Lcg(5);
        let a = base_vector(0);
        let turns = vec![turn(0, noisy(&a, &mut rng, 0.02)), turn(1, noisy(&a, &mut rng, 0.02))];
        let mut owner_vp = voiceprint("Me", a.clone());
        owner_vp.is_owner = true;
        let candidates =
            vec![Candidate { attendee: "Me <me@x.com>".into(), voiceprint: Some(owner_vp) }];
        let clusters = identify(&turns, &candidates, Params::default());
        assert_eq!(clusters.len(), 1);
        assert!(matches!(clusters[0].how, Attribution::Unknown), "{:#?}", clusters[0]);
    }

    #[test]
    fn a_low_similarity_score_does_not_match() {
        // Two candidates with voiceprints that do not sound like the one
        // cluster actually heard -- with two left unmatched, the
        // one-left-over inference must not fire either, so this is a clean
        // test of the match threshold alone.
        let mut rng = Lcg(99);
        let a = base_vector(0);
        let unrelated_one = base_vector(4);
        let unrelated_two = base_vector(2);
        let turns = vec![turn(0, noisy(&a, &mut rng, 0.02)), turn(1, noisy(&a, &mut rng, 0.02))];
        let candidates = vec![
            Candidate {
                attendee: "Stranger <s@x.com>".into(),
                voiceprint: Some(voiceprint("Stranger", unrelated_one)),
            },
            Candidate {
                attendee: "Other <o@x.com>".into(),
                voiceprint: Some(voiceprint("Other", unrelated_two)),
            },
        ];
        let clusters = identify(&turns, &candidates, Params::default());
        assert_eq!(clusters.len(), 1);
        assert!(matches!(clusters[0].how, Attribution::Unknown), "{:#?}", clusters[0]);
    }

    #[test]
    fn fold_into_merges_a_similar_sample_and_adds_a_dissimilar_one() {
        let a = base_vector(0);
        let b = base_vector(4);
        let mut vp = voiceprint("Alice", a.clone());
        let params = Params::default();

        // Similar: merges into the existing centroid rather than adding one.
        fold_into(&mut vp, &a, params);
        assert_eq!(vp.centroids.len(), 1);
        assert_eq!(vp.samples, 2);

        // Dissimilar: a second centroid, a second voice for the same person.
        fold_into(&mut vp, &b, params);
        assert_eq!(vp.centroids.len(), 2);
        assert_eq!(vp.samples, 3);
    }

    #[test]
    fn fold_into_never_exceeds_max_centroids() {
        let mut vp = voiceprint("Alice", base_vector(0));
        let params = Params::default();
        for i in 1..(super::super::MAX_CENTROIDS + 5) {
            fold_into(&mut vp, &base_vector(i), params);
        }
        assert!(vp.centroids.len() <= super::super::MAX_CENTROIDS);
    }

    #[test]
    fn fold_into_ignores_an_empty_centroid() {
        let mut vp = voiceprint("Alice", base_vector(0));
        let before = vp.clone();
        fold_into(&mut vp, &[], Params::default());
        assert_eq!(vp, before);
    }
}
