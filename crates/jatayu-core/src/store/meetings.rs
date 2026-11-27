//! Storage for meeting notes: recordings, transcripts, voiceprints, and the
//! settings that decide what gets recorded.
//!
//! Four kinds of row, and one shape running through all of them: `stage`,
//! `calendar_id` and `started_us` stay in the clear on a recording so that
//! the watcher and the pipeline's own recovery can find a stuck one without
//! unsealing every row in the vault, and everything else -- what was said,
//! who said it, whose voice a voiceprint is -- is sealed. See
//! `crate::meeting`'s own module docs for the full account of what is
//! readable and what is not, and `docs/plans/meeting-notes.md` for the
//! design this trait is built against.
//!
//! # Why this is a ninth trait
//!
//! The same reason [`RoutineStore`](super::routines::RoutineStore) is a
//! seventh and [`NoteStore`](super::notes::NoteStore) an eighth: a backend
//! that stores journals as a tree of Markdown files has no good answer for a
//! transcript with timed, attributed segments, and folding this into
//! [`JournalStore`](super::JournalStore) would oblige it to invent one. So
//! meetings are reached through
//! [`JournalStore::meetings`](super::JournalStore::meetings), which returns
//! `None` by default, and the interface reads
//! [`Capabilities::meetings`](super::Capabilities::meetings) to know whether
//! to offer the feature at all.
//!
//! # The settings are a singleton, like the assistant's
//!
//! [`MeetingStore::meeting_settings`] never returns an `Option`: a vault that
//! has never opened Settings → Meetings has a perfectly good answer -- the
//! feature is off -- and [`MeetingSettings::default`] is it. The same
//! argument [`AgentStore::settings`](super::agent::AgentStore::settings)
//! makes, for the same reason.

use crate::Result;
use crate::id::{CalendarId, NoteId, RecordingId, TranscriptId, VoiceprintId};
use crate::meeting::{MeetingSettings, Recording, Transcript, Voiceprint};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Filter and pagination for [`MeetingStore::list_recordings`].
///
/// Mirrors the TS `RecordingQuery` in `ui/src/lib/types.ts` field for field --
/// a client builds one of these and sends it whole, the same way
/// [`RunQuery`](super::routines::RunQuery) does for the assistant's log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingQuery {
    /// [`crate::meeting::Stage::as_str`] values; empty means any.
    pub stages: Vec<String>,
    pub calendar_id: Option<CalendarId>,
    /// `started_at` at or after this.
    pub from: Option<Timestamp>,
    /// `started_at` strictly before this. Exclusive, so a caller paging by
    /// day can ask `[from, to)` for each one without a recording that started
    /// exactly at midnight landing in both.
    pub to: Option<Timestamp>,
    pub limit: Option<u32>,
}

impl RecordingQuery {
    /// One recording's history row, most recent first, capped.
    pub fn recent(limit: u32) -> Self {
        Self { limit: Some(limit), ..Default::default() }
    }

    /// Does this recording pass the filters? The fallback for a backend that
    /// cannot express one natively, so behaviour stays identical across
    /// backends -- see [`RecordingQuery::apply`].
    pub fn matches(&self, r: &Recording) -> bool {
        if !self.stages.is_empty() && !self.stages.iter().any(|s| s == r.stage.as_str()) {
            return false;
        }
        if let Some(cal) = self.calendar_id
            && r.calendar_id() != Some(cal)
        {
            return false;
        }
        if let Some(from) = self.from
            && r.started_at < from
        {
            return false;
        }
        if let Some(to) = self.to
            && r.started_at >= to
        {
            return false;
        }
        true
    }

    /// Filter, sort newest-first and cap. Done here rather than trusted to
    /// SQL so that two backends cannot disagree about where a tie goes --
    /// the same reasoning [`RunQuery::apply`](super::routines::RunQuery::apply)
    /// gives, and the same tie-break: id, which is a UUIDv7 and so agrees
    /// with `started_at` on order whenever the two recordings did not start
    /// in the same microsecond.
    pub fn apply(&self, mut rows: Vec<Recording>) -> Vec<Recording> {
        rows.retain(|r| self.matches(r));
        rows.sort_by(|a, b| {
            b.started_at.cmp(&a.started_at).then_with(|| b.id.to_string().cmp(&a.id.to_string()))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// What a backend must do to hold meeting notes.
pub trait MeetingStore: Send + Sync {
    // ---- settings ---------------------------------------------------------

    /// The configuration, or [`MeetingSettings::default`] if none was ever
    /// saved. Never `Option` -- see the module docs.
    fn meeting_settings(&self) -> Result<MeetingSettings>;

    fn put_meeting_settings(&self, settings: &MeetingSettings) -> Result<()>;

    // ---- recordings ---------------------------------------------------------

    /// The recording history: what is running now, and every past one that
    /// matches `query`, newest first.
    fn list_recordings(&self, query: &RecordingQuery) -> Result<Vec<Recording>>;

    fn get_recording(&self, id: RecordingId) -> Result<Recording>;

    /// Insert or replace. Implementations must be idempotent -- the pipeline
    /// saves a recording's progress repeatedly as chunks arrive.
    fn put_recording(&self, recording: &Recording) -> Result<()>;

    /// Delete one history row. Deleting one that is not there is not an
    /// error. Does *not* touch the transcript or the note a finished
    /// recording points at -- see [`crate::vault::Vault::delete_recording`]
    /// for the cascade a caller actually wants.
    fn delete_recording(&self, id: RecordingId) -> Result<()>;

    // ---- transcripts --------------------------------------------------------

    fn get_transcript(&self, id: TranscriptId) -> Result<Transcript>;

    /// The transcript kept beside one note, if that note is a meeting note.
    ///
    /// `Option` rather than [`Error::NotFound`](crate::Error::NotFound):
    /// asking whether a note has a transcript is the ordinary case -- most
    /// notes do not -- and making every caller match on a "not found" error
    /// to mean "not a meeting note" would be the wrong shape for a question
    /// this common.
    fn transcript_for_note(&self, note_id: NoteId) -> Result<Option<Transcript>>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_transcript(&self, transcript: &Transcript) -> Result<()>;

    fn delete_transcript(&self, id: TranscriptId) -> Result<()>;

    // ---- voiceprints --------------------------------------------------------

    /// Every voice, in no particular order -- there are a handful of these,
    /// the same argument [`RoutineStore::list_routines`](super::routines::RoutineStore::list_routines)
    /// makes, and sorting them by name is the caller's business once it has
    /// decrypted one.
    fn list_voiceprints(&self) -> Result<Vec<Voiceprint>>;

    fn get_voiceprint(&self, id: VoiceprintId) -> Result<Voiceprint>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_voiceprint(&self, voiceprint: &Voiceprint) -> Result<()>;

    fn delete_voiceprint(&self, id: VoiceprintId) -> Result<()>;

    /// Forget every voiceprint in the vault. What "voiceprints: off" in
    /// settings does to what is already stored, and what somebody who never
    /// wants a voice remembered again reaches for directly.
    fn delete_all_voiceprints(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::TemplateId;
    use crate::meeting::Stage;
    use jiff::Timestamp;

    fn recording(started_at_secs: i64) -> Recording {
        let mut r = Recording::new("Design sync", None, TemplateId::new());
        r.started_at = Timestamp::from_second(started_at_secs).unwrap();
        r
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(RecordingQuery::default().matches(&recording(1_000)));
    }

    #[test]
    fn stage_filter_matches_any_of_the_listed_stages() {
        let mut r = recording(1_000);
        r.stage = Stage::Done;
        let q = RecordingQuery { stages: vec!["done".into()], ..Default::default() };
        assert!(q.matches(&r));
        let q = RecordingQuery { stages: vec!["recording".into()], ..Default::default() };
        assert!(!q.matches(&r));
    }

    #[test]
    fn the_upper_bound_is_exclusive() {
        let r = recording(1_000);
        let q = RecordingQuery {
            to: Some(Timestamp::from_second(1_000).unwrap()),
            ..Default::default()
        };
        assert!(!q.matches(&r), "a recording starting exactly at `to` must not match");
        let q = RecordingQuery {
            to: Some(Timestamp::from_second(1_001).unwrap()),
            ..Default::default()
        };
        assert!(q.matches(&r));
    }

    #[test]
    fn apply_sorts_newest_first_and_caps() {
        let rows = vec![recording(1_000), recording(3_000), recording(2_000)];
        let out = RecordingQuery::default().apply(rows);
        assert_eq!(
            out.iter().map(|r| r.started_at.as_second()).collect::<Vec<_>>(),
            vec![3_000, 2_000, 1_000]
        );

        let capped = RecordingQuery::recent(1).apply(out);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].started_at.as_second(), 3_000);
    }
}
