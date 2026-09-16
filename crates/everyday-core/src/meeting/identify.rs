//! Which voice is whose, given embeddings. Pure: the service computes the
//! vectors, this decides.

use super::{Attribution, Voiceprint};

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
    let _ = (a, b);
    todo!("core-logic agent")
}

/// Cluster the turns across the whole meeting (agglomerative, average
/// linkage, stopping at `cluster_threshold`), using backend hints to keep
/// turns with the same scoped hint together when embeddings are missing.
/// Then name clusters: voiceprint match among candidates only, then the
/// one-left-over inference, then "Unknown N" in order of first speech.
/// Turns without embeddings and without hints join the nearest-in-time
/// cluster.
pub fn identify(turns: &[Turn], candidates: &[Candidate], params: Params) -> Vec<Cluster> {
    let _ = (turns, candidates, params);
    todo!("core-logic agent")
}

/// Fold a new centroid into a voiceprint: merge into the nearest existing
/// centroid when similar enough (running mean weighted by samples), else
/// add one, keeping at most `MAX_CENTROIDS`.
pub fn fold_into(voiceprint: &mut Voiceprint, centroid: &[f32], params: Params) {
    let _ = (voiceprint, centroid, params);
    todo!("core-logic agent")
}

/// Split an attendee string into (name, email). Either may be absent.
pub fn parse_attendee(attendee: &str) -> (Option<String>, Option<String>) {
    let _ = attendee;
    todo!("core-logic agent")
}
