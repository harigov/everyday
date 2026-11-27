//! Putting a meeting back together when the person left the call and came
//! back: a recording that `continues` an earlier one writes into the
//! earlier one's note, over both parts' words.
//!
//! The later recording is processed as any other until it would write its
//! summary. There it waits for the earlier one to finish -- [`gate`] --
//! since the notes are of the whole meeting and two summaries written at
//! once into one note would only overwrite each other. Whichever recording
//! finishes or fails next wakes the ones waiting on it ([`wake_followers`]).
//! Then [`earlier_of`] hands the summary the earlier part: its transcript
//! to add this one's words to, when the clock started, and everything the
//! person wrote down along the way.

use std::sync::Arc;

use jatayu_core::meeting::{Recording, RecordingNote, Stage, Transcript};
use jatayu_core::{NoteId, RecordingId, RecordingQuery, Vault};
use jiff::Timestamp;

use crate::error::CommandResult;
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};

use super::adapters::AudioSource;
use super::draft::{ms_after, transcript_origin};

/// Whether a recording may write its notes yet.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    Go,
    /// The earlier recording it continues is still being worked on. The
    /// pipeline returns without progress and is woken when that one is done.
    Park,
}

/// Is the earlier recording `id` continues finished with? A `continues`
/// pointing at a recording that has since been deleted is dropped here:
/// there is nothing to wait for and nothing to carry on.
pub(crate) async fn gate(vault: &Arc<Vault>, id: RecordingId) -> CommandResult<Gate> {
    let vault = vault.clone();
    blocking(move || {
        let recording = vault.recording(id)?;
        let Some(earlier) = recording.continues.filter(|e| *e != id) else {
            return Ok(Gate::Go);
        };
        match vault.recording(earlier) {
            Ok(earlier) if matches!(earlier.stage, Stage::Done | Stage::Failed { .. }) => {
                Ok(Gate::Go)
            }
            Ok(_) => Ok(Gate::Park),
            Err(e) if e.code() == crate::error::codes::NOT_FOUND => {
                crate::meeting::spool::mutate_recording(&vault, id, |r| {
                    r.continues = None;
                    Ok(())
                })?;
                Ok(Gate::Go)
            }
            Err(e) => Err(e.into()),
        }
    })
    .await
}

/// What a recording that carries on an earlier one starts from.
pub(crate) struct Earlier {
    pub(crate) recording: Recording,
    /// The note the whole meeting is written into.
    pub(crate) note_id: NoteId,
    /// The transcript so far, if one was kept.
    pub(crate) transcript: Option<Transcript>,
    /// When the transcript's clock started: this recording's words are
    /// added that long after it.
    pub(crate) origin: Timestamp,
    /// What the person wrote down in the earlier parts, on the same clock.
    pub(crate) notes: Vec<RecordingNote>,
}

/// The how-many-earlier-recordings a chain of rejoins is followed for.
const CHAIN_LIMIT: usize = 16;

/// The earlier part of the meeting `recording` carries on, when it has
/// finished (done, or failed) and its note is there. `None` for a recording
/// that stands alone, and for one whose earlier part has no note any more:
/// that is summarised alone, into a note of its own.
pub(crate) fn earlier_of(vault: &Vault, recording: &Recording) -> CommandResult<Option<Earlier>> {
    let Some(earlier_id) = recording.continues.filter(|e| *e != recording.id) else {
        return Ok(None);
    };
    let Ok(earlier) = vault.recording(earlier_id) else { return Ok(None) };
    if !matches!(earlier.stage, Stage::Done | Stage::Failed { .. }) {
        return Ok(None);
    }
    let Some(note_id) = earlier.note_id else { return Ok(None) };
    if vault.note(note_id).is_err() {
        return Ok(None);
    }
    let transcript = vault.transcript_for_note(note_id)?;
    let origin = transcript_origin(vault, transcript.as_ref(), &earlier);

    // Every recording of the chain that came before: a meeting left and
    // rejoined twice is three parts, and the notes the person made in the
    // first are still theirs.
    let mut notes: Vec<RecordingNote> = Vec::new();
    let mut seen: Vec<RecordingId> = vec![recording.id];
    let mut next = Some(earlier.clone());
    while let Some(part) = next.take() {
        if seen.len() > CHAIN_LIMIT || seen.contains(&part.id) {
            break;
        }
        seen.push(part.id);
        let shift = ms_after(origin, part.started_at);
        notes.extend(
            part.notes.iter().map(|n| RecordingNote {
                at_ms: n.at_ms.saturating_add(shift),
                text: n.text.clone(),
            }),
        );
        next = part.continues.and_then(|id| vault.recording(id).ok());
    }
    notes.sort_by_key(|n| n.at_ms);
    Ok(Some(Earlier { recording: earlier, note_id, transcript, origin, notes }))
}

/// This recording's notes on the joined clock, after `earlier`'s.
pub(crate) fn notes_after(earlier: &Earlier, recording: &Recording) -> Vec<RecordingNote> {
    let shift = ms_after(earlier.origin, recording.started_at);
    let mut notes = earlier.notes.clone();
    notes.extend(
        recording
            .notes
            .iter()
            .map(|n| RecordingNote { at_ms: n.at_ms.saturating_add(shift), text: n.text.clone() }),
    );
    notes
}

/// The earlier part has been written into the note, with this one. If it
/// had failed, it is done too, and its audio can go: the follower has
/// already written the joined note, and a retry of the earlier part would
/// only write its words into that note a second time (retry refuses it, so
/// leaving it failed would strand a row with a banner whose Retry always
/// errors). When some of its audio was never transcribed, that audio is
/// dropped with a warning: the note is written and cannot use it any more.
/// The change to announce, when there was one.
pub(crate) fn settle_earlier(
    vault: &Vault,
    source: &dyn AudioSource,
    earlier: &Recording,
    note_id: NoteId,
    written: Option<Timestamp>,
) -> CommandResult<Option<Change>> {
    let latest = vault.recording(earlier.id)?;
    let Stage::Failed { .. } = latest.stage else { return Ok(None) };
    if latest.chunks.iter().any(|c| !c.transcribed) {
        tracing::warn!(
            recording = %earlier.id,
            follower_note = %note_id,
            "meeting pipeline: untranscribed audio of an earlier part was dropped because its \
             follower already wrote the meeting's note"
        );
    }
    crate::meeting::spool::mutate_recording(vault, earlier.id, |r| {
        r.stage = Stage::Done;
        r.note_id = Some(note_id);
        r.note_written_at = written;
        r.chunks.clear();
        r.partial.clear();
        r.updated_at = Timestamp::now();
        Ok(())
    })?;
    if let Err(e) = source.remove_audio(earlier.id) {
        tracing::warn!(
            recording = %earlier.id,
            error = %e.message,
            "meeting pipeline: could not clear the spool of a part folded into its meeting; \
             the hourly sweep will pick it up"
        );
    }
    let mut change = Change::new(Kind::Recording, Op::Updated);
    change.id = Some(earlier.id.to_string());
    Ok(Some(change))
}

/// The recordings waiting on `id`: still owed work, and carrying it on.
pub(crate) fn followers_of(vault: &Vault, id: RecordingId) -> Vec<RecordingId> {
    let pending = [Stage::Transcribing, Stage::Identifying, Stage::Summarising]
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    vault
        .recordings(&RecordingQuery { stages: pending, ..Default::default() })
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.continues == Some(id))
        .map(|r| r.id)
        .collect()
}

/// `id` has been run: if it is finished (done or failed), start every
/// recording that was waiting for it.
pub(crate) fn wake_followers(svc: &Arc<Service>, vault: &Vault, id: RecordingId) {
    let finished = vault
        .recording(id)
        .map(|r| matches!(r.stage, Stage::Done | Stage::Failed { .. }))
        .unwrap_or(false);
    if !finished {
        return;
    }
    for follower in followers_of(vault, id) {
        super::enqueue::enqueue(svc, follower);
    }
}
