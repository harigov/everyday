//! The pipeline: one supervised task per recording, from spooled audio to a
//! written note. See `docs/plans/meeting-notes.md`, "The shape of it".
//!
//! Filled in by the pipeline work; the two entry points below are the seam
//! the spool calls through.

use crate::service::Service;
use everyday_core::RecordingId;
use everyday_core::meeting::Track;
use std::sync::Arc;

/// A recording has finished (or been retried) and is owed its note. Starts
/// or wakes the pipeline for it; returns at once.
pub fn enqueue(svc: &Arc<Service>, id: RecordingId) {
    let _ = (svc, id);
    tracing::debug!(recording = %id, "meeting pipeline not wired yet");
}

/// One chunk has been spooled while the call is still going. The pipeline
/// may transcribe it early so a long call is mostly done by its end.
pub fn chunk_closed(svc: &Arc<Service>, id: RecordingId, track: Track, seq: u32) {
    let _ = (svc, id, track, seq);
}
