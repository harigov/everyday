//! The note a call leaves behind the moment it ends: the meeting's title and
//! details, a line saying the summary is being written, and the transcript
//! so far -- so the person can see at once that notes were taken, and read
//! what was said, while the model works. The same note is then filled in
//! (`ensure_draft` -> `refresh_draft` -> the final write in `drivers`), and
//! never replaced by a second one.
//!
//! # Whose words are in the note
//!
//! The pipeline writes the note, and so may the person: they can open the
//! draft and start typing. Every write here is made against the `updated_at`
//! it read ([`Vault::save_note`]'s `expect`), and a note is only replaced
//! outright while its `updated_at` still equals the one the pipeline stamped
//! on the recording when it last wrote it
//! ([`Recording::note_written_at`]). One that moved has the person's words
//! in it, and a later write puts the summary above them instead.
//!
//! Everything here that is not the final write is best-effort: a draft is
//! there to be looked at, and a failure to make one is logged by the caller,
//! never allowed to fail a recording.

use std::sync::Arc;

use jatayu_core::meeting::template::{self, Facts};
use jatayu_core::meeting::{
    Attribution, Recording, Segment, Speaker, Stage, Track, TrackSegment, Transcript, people,
};
use jatayu_core::{Note, NoteId, NoteOrigin, Purpose, RecordingId, RichDoc, TranscriptId, Vault};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, EventSink, Kind, Op};
use crate::service::blocking;

use super::stages::{
    Identified, facts_for, facts_from, invitees_of, owner_of, purpose_for, tags_for,
};

/// What the note says until the summary is in it. One italic paragraph,
/// found again by [`swap_marker`] -- the final write takes it out, a failure
/// replaces it with [`failure_line`].
pub(crate) const MARKER: &str = "_Writing the summary of this call. The transcript is below; \
this note fills in by itself when the summary is ready._";

/// What the note says when its recording was thrown away before the summary
/// was written, and the person had written in the note, so it stays.
pub(crate) const DISCARDED: &str =
    "_This call's recording was discarded before its summary was written._";

const FAILED_PREFIX: &str = "The summary could not be written: ";
const FAILED_SUFFIX: &str = ". Retry it from the recording in the notes list.";

/// What the note says when the summary could not be written, for `reason`.
pub(crate) fn failure_line(reason: &str) -> String {
    let reason = reason.trim().trim_end_matches('.');
    format!("_{FAILED_PREFIX}{reason}{FAILED_SUFFIX}_")
}

/// A line with its emphasis marks and escapes taken off, so the marker is
/// still found after the editor has had the note and written it back in its
/// own Markdown.
fn squash(line: &str) -> String {
    line.chars().filter(|c| !matches!(c, '_' | '*' | '\\')).collect::<String>().trim().to_string()
}

fn is_marker_line(line: &str) -> bool {
    squash(line) == squash(MARKER)
}

fn is_failure_line(line: &str) -> bool {
    let line = squash(line);
    line.starts_with(&squash(FAILED_PREFIX)) && line.ends_with(squash(FAILED_SUFFIX).trim())
}

/// Does `markdown` carry the "summary being written" paragraph?
pub(crate) fn has_marker(markdown: &str) -> bool {
    markdown.lines().any(is_marker_line)
}

fn has_failure_line(markdown: &str) -> bool {
    markdown.lines().any(is_failure_line)
}

/// `markdown` with the marker and any failure line taken out, and `with`
/// put where the first of them stood. The second part is whether there was
/// one.
pub(crate) fn swap_marker(markdown: &str, with: Option<&str>) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut found = false;
    for line in markdown.lines() {
        if is_marker_line(line) || is_failure_line(line) {
            if !found && let Some(with) = with {
                out.push(with);
            }
            found = true;
            continue;
        }
        out.push(line);
    }
    // Taking a paragraph out leaves its blank lines side by side.
    let mut lines: Vec<&str> = Vec::new();
    for line in out {
        if line.trim().is_empty() && lines.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        lines.push(line);
    }
    let text = lines.join("\n");
    (text.trim().to_string(), found)
}

/// Is `note` as the pipeline last wrote it? `written_at` is the
/// `updated_at` it stamped on the recording then.
pub(crate) fn untouched(note: &Note, written_at: Option<Timestamp>) -> bool {
    written_at == Some(note.updated_at)
}

/// A moment later than `after`, for the `updated_at` of a note written now.
fn later_than(after: Timestamp) -> Timestamp {
    let now = Timestamp::now();
    if now > after {
        now
    } else {
        after.checked_add(jiff::SignedDuration::from_micros(1)).unwrap_or(now)
    }
}

fn change(kind: Kind, op: Op, id: impl std::fmt::Display) -> Change {
    let mut change = Change::new(kind, op);
    change.id = Some(id.to_string());
    change
}

fn tz_of(vault: &Vault) -> String {
    vault
        .agent_settings()
        .unwrap_or_default()
        .timezone
        .unwrap_or_else(jatayu_core::model::system_tz)
}

/// The label the microphone's lines carry: the profile's name, or "You".
/// The same rule `stages::identify_stage` follows, for a transcript made
/// before voices are told apart.
fn owner_name(vault: &Vault) -> String {
    let profile = vault.profile().unwrap_or_default();
    let name = format!("{} {}", profile.first_name.trim(), profile.last_name.trim());
    let name = name.trim();
    if name.is_empty() { "You".to_string() } else { name.to_string() }
}

/// How the transcript is labelled: the speech model, and its language.
fn speech_of(vault: &Vault) -> (String, Option<String>) {
    match vault.speech_and_keyed() {
        Ok((Some(speech), _)) => (speech.transcriber.label(), speech.language.clone()),
        _ => (String::new(), None),
    }
}

fn blank_transcript(vault: &Vault, note_id: NoteId, recording: RecordingId) -> Transcript {
    let (backend, language) = speech_of(vault);
    let now = Timestamp::now();
    Transcript {
        id: TranscriptId::new(),
        note_id,
        recording_id: Some(recording),
        language,
        backend,
        speakers: Vec::new(),
        segments: Vec::new(),
        room: false,
        created_at: now,
        updated_at: now,
    }
}

/// What was said so far, as a transcript can say it before voices are told
/// apart: the microphone is the owner and everything the computer played is
/// "Others". A meeting heard in one room is all the owner's.
fn provisional(vault: &Vault, recording: &Recording) -> (Vec<Speaker>, Vec<Segment>, bool) {
    let invited = !invitees_of(recording.event.as_ref(), &owner_of(vault)).is_empty();
    let room = jatayu_core::meeting::heard_in_one_room(&recording.partial, invited);
    let mut lines: Vec<&TrackSegment> =
        recording.partial.iter().filter(|s| !s.text.trim().is_empty()).collect();
    lines.sort_by_key(|s| s.start_ms);

    let (owner_key, others_key) = (0u16, 1u16);
    let (mut owner_spoke, mut others_spoke) = (false, false);
    let segments: Vec<Segment> = lines
        .into_iter()
        .map(|s| {
            let owner = room || s.track == Track::Mic;
            if owner {
                owner_spoke = true;
            } else {
                others_spoke = true;
            }
            Segment {
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                speaker: if owner { owner_key } else { others_key },
                text: s.text.trim().to_string(),
            }
        })
        .collect();
    let speaker = |key: u16, label: String, how: Attribution| Speaker {
        key,
        label,
        email: None,
        voiceprint_id: None,
        how,
        centroid: Vec::new(),
        embedding_model: String::new(),
    };
    let mut speakers = Vec::new();
    if owner_spoke {
        speakers.push(speaker(owner_key, owner_name(vault), Attribution::Owner));
    }
    if others_spoke {
        speakers.push(speaker(others_key, people::OTHERS.to_string(), Attribution::Unknown));
    }
    (speakers, segments, room)
}

/// The facts a draft can state: what the calendar says, and who was invited.
/// Nobody has been heard yet.
fn draft_facts(vault: &Vault, recording: &Recording, tz: &str) -> Facts {
    let mut facts = facts_from(recording, &[], &[], tz);
    facts.invitees = invitees_of(recording.event.as_ref(), &owner_of(vault));
    facts
}

/// The draft's body: the details, the marker, and what the person wrote
/// down on the panel -- which is theirs, and in the note from the start.
fn draft_body(facts: &Facts, recording: &Recording) -> String {
    let details = template::details_block(facts);
    let mut parts: Vec<String> = Vec::new();
    if !details.is_empty() {
        parts.push(details);
    }
    parts.push(MARKER.to_string());
    let notes = jatayu_core::meeting::owner_notes_section(&recording.notes);
    if !notes.is_empty() {
        parts.push(notes);
    }
    parts.join("\n\n")
}

fn draft_title(facts: &Facts, recording: &Recording) -> String {
    [facts.title.trim(), recording.title.trim(), "Meeting"]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or("Meeting")
        .to_string()
}

fn is_conflict(e: &jatayu_core::Error) -> bool {
    matches!(e, jatayu_core::Error::Conflict { .. })
}

fn is_missing(e: &jatayu_core::Error) -> bool {
    e.code() == codes::NOT_FOUND
}

/// Set `note_written_at` on every recording writing into `note_id` -- a
/// meeting put back together has more than one, and whichever writes next
/// must find the others agree on what was last written.
///
/// Only the recordings near the meeting's first one are looked at (see
/// [`crate::meeting::spool::recordings_near`]); every part sharing a note
/// started within a meeting and its rejoins of the transcript's own origin,
/// and listing the whole history decrypts every row of it on each write.
pub(crate) fn stamp(vault: &Vault, note_id: NoteId, at: Option<Timestamp>) -> CommandResult<()> {
    let Some(anchor) = note_anchor(vault, note_id) else { return Ok(()) };
    for recording in crate::meeting::spool::recordings_near(vault, anchor)? {
        if recording.note_id == Some(note_id) && recording.note_written_at != at {
            crate::meeting::spool::mutate_recording(vault, recording.id, |r| {
                r.note_written_at = at;
                Ok(())
            })?;
        }
    }
    Ok(())
}

/// When the first recording writing into `note_id` started: the one its
/// transcript was first made for, and failing that -- a note with no
/// transcript yet -- when the note itself was made, which is within a meeting
/// of it. `None` when there is no such note, so nothing writes into it.
fn note_anchor(vault: &Vault, note_id: NoteId) -> Option<Timestamp> {
    let origin = vault
        .transcript_for_note(note_id)
        .ok()
        .flatten()
        .and_then(|t| t.recording_id)
        .and_then(|r| vault.recording(r).ok());
    match origin {
        Some(r) => Some(crate::meeting::spool::chain_origin_start(vault, &r)),
        None => vault.note(note_id).ok().map(|n| n.created_at),
    }
}

/// Save `note` after `edit`, against the `updated_at` it was read with, and
/// stamp it as the pipeline's own write. `None` when the note moved in the
/// meantime -- the person is typing in it, and a draft's tidying is never
/// worth getting in their way for.
fn save_edited(
    vault: &Vault,
    note: &Note,
    edit: impl FnOnce(&mut Note),
) -> CommandResult<Option<Timestamp>> {
    let read_at = note.updated_at;
    let mut next = note.clone();
    edit(&mut next);
    next.updated_at = later_than(read_at);
    match vault.save_note(&next, Some(read_at)) {
        Ok(()) => {
            stamp(vault, next.id, Some(next.updated_at))?;
            Ok(Some(next.updated_at))
        }
        Err(e) if is_conflict(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ============================================================================
// The draft
// ============================================================================

/// Make sure the recording has its note: the draft written when the call
/// ends. Cheap when it already does. See the module doc.
///
/// * No note yet: a new one, with the title and details, the marker, and a
///   transcript of what has been transcribed so far.
/// * A recording that `continues` an earlier one whose note exists: that
///   note is the one, with this part's words added to its transcript and the
///   marker put back on it (only if the person has not edited it). A draft
///   of its own, made before the meeting was known to be a continuation, is
///   folded in -- deleted, if the person never touched it, else kept, and
///   the recording stops being a continuation: their words are never lost.
pub(crate) async fn ensure_draft(
    vault: &Arc<Vault>,
    id: RecordingId,
    events: &dyn EventSink,
) -> CommandResult<()> {
    let vault = vault.clone();
    let changes = blocking(move || ensure_draft_now(&vault, id)).await?;
    for change in changes {
        events.changed(change);
    }
    Ok(())
}

/// The earlier recording `recording` carries on and its note, when both are
/// still there.
pub(crate) fn earlier_note(vault: &Vault, recording: &Recording) -> Option<(Recording, Note)> {
    let earlier_id = recording.continues.filter(|e| *e != recording.id)?;
    let earlier = vault.recording(earlier_id).ok()?;
    let note = vault.note(earlier.note_id?).ok()?;
    Some((earlier, note))
}

fn ensure_draft_now(vault: &Vault, id: RecordingId) -> CommandResult<Vec<Change>> {
    let recording = vault.recording(id)?;
    if !recording.stage.is_pending() {
        return Ok(Vec::new());
    }
    let own = recording.note_id.and_then(|n| vault.note(n).ok());
    let earlier = earlier_note(vault, &recording);
    let mut changes = Vec::new();
    match (own, earlier) {
        (Some(own), Some((earlier, note))) if own.id != note.id => {
            if !untouched(&own, recording.note_written_at) {
                // The person has been writing in the draft this recording
                // made for itself: it stays theirs, and is not folded.
                crate::meeting::spool::mutate_recording(vault, id, |r| {
                    r.continues = None;
                    Ok(())
                })?;
                return Ok(changes);
            }
            vault.delete_note(own.id)?;
            changes.push(change(Kind::Note, Op::Deleted, own.id));
            adopt(vault, &recording, &earlier, note, &mut changes)?;
        }
        (Some(own), _) => keep(vault, &recording, own, &mut changes)?,
        (None, Some((earlier, note))) => adopt(vault, &recording, &earlier, note, &mut changes)?,
        (None, None) => create(vault, &recording, &mut changes)?,
    }
    Ok(changes)
}

/// The draft is there already. Put right what a retry or a half-made draft
/// left: a failure line that no longer applies, and a missing transcript.
fn keep(
    vault: &Vault,
    recording: &Recording,
    note: Note,
    changes: &mut Vec<Change>,
) -> CommandResult<()> {
    let markdown = note.body.to_markdown();
    if has_failure_line(&markdown) && untouched(&note, recording.note_written_at) {
        let (body, _) = swap_marker(&markdown, Some(MARKER));
        if save_edited(vault, &note, |n| n.body = RichDoc::from_markdown(&body))?.is_some() {
            changes.push(change(Kind::Note, Op::Updated, note.id));
        }
    }
    if vault.transcript_for_note(note.id)?.is_none() {
        let (speakers, segments, room) = provisional(vault, recording);
        let mut transcript = blank_transcript(vault, note.id, recording.id);
        transcript.speakers = speakers;
        transcript.segments = segments;
        transcript.room = room;
        vault.save_transcript(&transcript)?;
        changes.push(change(Kind::Transcript, Op::Updated, transcript.id));
    }
    Ok(())
}

/// A new draft of the recording's own.
fn create(vault: &Vault, recording: &Recording, changes: &mut Vec<Change>) -> CommandResult<()> {
    let tz = tz_of(vault);
    let facts = draft_facts(vault, recording, &tz);
    let mut note = Note::written_by(
        NoteOrigin::Meeting,
        draft_title(&facts, recording),
        &draft_body(&facts, recording),
    );
    let (tags, retagged) = tags_for(vault, recording);
    note.tags = tags;
    note.purpose = purpose_for(vault, recording.event.as_ref());
    vault.save_note(&note, None)?;
    let (note_id, written) = (note.id, note.updated_at);
    // Pointed at before the transcript is saved, so a draft whose transcript
    // could not be is found again, not made a second time.
    crate::meeting::spool::mutate_recording(vault, recording.id, |r| {
        r.note_id = Some(note_id);
        r.note_written_at = Some(written);
        r.updated_at = Timestamp::now();
        Ok(())
    })?;
    changes.push(change(Kind::Note, Op::Created, note_id));
    for id in retagged {
        changes.push(change(Kind::Note, Op::Updated, id));
    }
    changes.push(change(Kind::Recording, Op::Updated, recording.id));

    let (speakers, segments, room) = provisional(vault, recording);
    let mut transcript = blank_transcript(vault, note_id, recording.id);
    transcript.speakers = speakers;
    transcript.segments = segments;
    transcript.room = room;
    vault.save_transcript(&transcript)?;
    changes.push(change(Kind::Transcript, Op::Updated, transcript.id));
    Ok(())
}

/// How long after `origin` `at` is, in milliseconds; none before it.
pub(crate) fn ms_after(origin: Timestamp, at: Timestamp) -> u64 {
    u64::try_from(at.as_millisecond() - origin.as_millisecond()).unwrap_or(0)
}

/// The recording whose start the clock of `transcript` runs from: the one
/// it was first made for, and failing that `fallback`.
pub(crate) fn transcript_origin(
    vault: &Vault,
    transcript: Option<&Transcript>,
    fallback: &Recording,
) -> Timestamp {
    transcript
        .and_then(|t| t.recording_id)
        .and_then(|r| vault.recording(r).ok())
        .map_or(fallback.started_at, |r| r.started_at)
}

/// Write `recording`'s words into `earlier`'s note, which is where this
/// meeting's notes are.
fn adopt(
    vault: &Vault,
    recording: &Recording,
    earlier: &Recording,
    note: Note,
    changes: &mut Vec<Change>,
) -> CommandResult<()> {
    let base = vault.transcript_for_note(note.id)?;
    let offset = ms_after(transcript_origin(vault, base.as_ref(), earlier), recording.started_at);
    let (speakers, segments, _) = provisional(vault, recording);
    let mut transcript = base.unwrap_or_else(|| blank_transcript(vault, note.id, earlier.id));
    // Whatever this recording put in before comes out first: the draft is
    // adopted again after every wake, and the final write adds the part once more.
    transcript.replace_part(&speakers, &segments, offset);
    vault.save_transcript(&transcript)?;
    changes.push(change(Kind::Transcript, Op::Updated, transcript.id));

    // The summary is being written again, over both parts -- said so, unless
    // the person has been writing in the note, which is never made to look
    // unfinished.
    let mut written = earlier.note_written_at;
    if untouched(&note, written) {
        let (body, _) = swap_marker(&note.body.to_markdown(), None);
        let marked = format!("{MARKER}\n\n{body}");
        if let Some(at) = save_edited(vault, &note, |n| n.body = RichDoc::from_markdown(&marked))? {
            written = Some(at);
            changes.push(change(Kind::Note, Op::Updated, note.id));
        }
    }
    let note_id = note.id;
    crate::meeting::spool::mutate_recording(vault, recording.id, |r| {
        r.note_id = Some(note_id);
        r.note_written_at = written;
        r.updated_at = Timestamp::now();
        Ok(())
    })?;
    changes.push(change(Kind::Recording, Op::Updated, recording.id));
    Ok(())
}

/// Voices are told apart: put them in the draft's transcript, and, while the
/// person has not touched the note, bring its title and details up to date
/// -- the recording may have been filed under its calendar meeting by now,
/// and who spoke is known.
///
/// A recording carrying on an earlier one leaves both alone: the note and
/// the transcript are the joined meeting's, and the final write puts the two
/// parts together.
pub(crate) async fn refresh_draft(
    vault: &Arc<Vault>,
    id: RecordingId,
    identified: &Identified,
    events: &dyn EventSink,
) -> CommandResult<()> {
    let vault = vault.clone();
    let identified = identified.clone();
    let changes = blocking(move || refresh_draft_now(&vault, id, &identified)).await?;
    for change in changes {
        events.changed(change);
    }
    Ok(())
}

fn refresh_draft_now(
    vault: &Vault,
    id: RecordingId,
    identified: &Identified,
) -> CommandResult<Vec<Change>> {
    let recording = vault.recording(id)?;
    let mut changes = Vec::new();
    let Some(note_id) = recording.note_id else { return Ok(changes) };
    let Ok(note) = vault.note(note_id) else { return Ok(changes) };
    if earlier_note(vault, &recording).is_some_and(|(_, earlier_note)| earlier_note.id == note_id) {
        return Ok(changes);
    }

    // The same transcript record as the draft's -- there is one per note.
    let mut transcript =
        vault.transcript_for_note(note_id)?.unwrap_or_else(|| blank_transcript(vault, note_id, id));
    transcript.speakers = identified.speakers.clone();
    transcript.segments = identified.segments.clone();
    transcript.room = identified.room;
    transcript.updated_at = Timestamp::now();
    vault.save_transcript(&transcript)?;
    changes.push(change(Kind::Transcript, Op::Updated, transcript.id));

    if untouched(&note, recording.note_written_at) {
        let tz = tz_of(vault);
        let facts = facts_for(&recording, identified, &tz);
        let title = draft_title(&facts, &recording);
        let body = draft_body(&facts, &recording);
        let (tags, retagged) = tags_for(vault, &recording);
        let purpose = purpose_for(vault, recording.event.as_ref());
        let changed = note.title != title
            || note.tags != tags
            || note.purpose != purpose
            || note.body.to_markdown().trim() != RichDoc::from_markdown(&body).to_markdown().trim();
        if changed
            && save_edited(vault, &note, |n| {
                n.title = title;
                n.body = RichDoc::from_markdown(&body);
                n.tags = tags;
                n.purpose = purpose;
            })?
            .is_some()
        {
            changes.push(change(Kind::Note, Op::Updated, note_id));
            for id in retagged {
                changes.push(change(Kind::Note, Op::Updated, id));
            }
        }
    }
    Ok(changes)
}

/// The summary could not be written: say so in the draft where the marker
/// is, so the note does not go on promising it. Only while the person has
/// not touched the note. `None` when there was nothing to change.
pub(crate) fn note_failed(
    vault: &Vault,
    recording: &Recording,
    reason: &str,
) -> CommandResult<Option<Change>> {
    let Some(note_id) = recording.note_id else { return Ok(None) };
    let Ok(note) = vault.note(note_id) else { return Ok(None) };
    let markdown = note.body.to_markdown();
    if !has_marker(&markdown) || !untouched(&note, recording.note_written_at) {
        return Ok(None);
    }
    let (body, _) = swap_marker(&markdown, Some(&failure_line(reason)));
    let saved = save_edited(vault, &note, |n| n.body = RichDoc::from_markdown(&body))?;
    Ok(saved.map(|_| change(Kind::Note, Op::Updated, note_id)))
}

/// A recording that never reached `Done` is being thrown away (discarded, or
/// expired after failing): its draft note must not go on promising a summary
/// or a retry that no longer exist. Unless another recording is written into
/// the same note (a later part of the meeting), a note nobody touched is
/// deleted, with its transcript; one the person wrote in keeps their text,
/// the marker or failure line swapped for [`DISCARDED`].
///
/// A recording that carries on an earlier one and shares its note leaves the
/// note, but not its own mark on it: [`release_follower`]. The changes to
/// announce.
pub(crate) fn release_note(vault: &Vault, recording: &Recording) -> CommandResult<Vec<Change>> {
    if matches!(recording.stage, Stage::Done) {
        return Ok(Vec::new());
    }
    let Some(note_id) = recording.note_id else { return Ok(Vec::new()) };
    let sharing: Vec<Recording> = crate::meeting::spool::related_recordings(vault, recording)?
        .into_iter()
        .filter(|r| r.id != recording.id && r.note_id == Some(note_id))
        .collect();
    if !sharing.is_empty() {
        return match recording.continues {
            Some(earlier) => release_follower(vault, recording, earlier, note_id, &sharing),
            None => Ok(Vec::new()),
        };
    }
    let Ok(note) = vault.note(note_id) else { return Ok(Vec::new()) };
    if untouched(&note, recording.note_written_at) {
        vault.delete_note(note_id)?;
        return Ok(vec![change(Kind::Note, Op::Deleted, note_id)]);
    }
    let markdown = note.body.to_markdown();
    let (body, found) = swap_marker(&markdown, Some(DISCARDED));
    if !found {
        return Ok(Vec::new());
    }
    // Saved directly, not through `save_edited`: that stamps the recordings
    // writing into the note, and the caller already holds this one's lock.
    let mut next = note.clone();
    next.body = RichDoc::from_markdown(&body);
    next.updated_at = later_than(note.updated_at);
    match vault.save_note(&next, Some(note.updated_at)) {
        Ok(()) => Ok(vec![change(Kind::Note, Op::Updated, note_id)]),
        Err(e) if is_conflict(&e) => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// A later part of a meeting is being thrown away, and the earlier part's
/// note is the meeting's: [`adopt`] put this part's provisional lines into
/// the shared transcript and, while the note was untouched, a marker at the
/// top of it. Both are taken out again, or the finished note would go on
/// promising a summary, with lines of a call nobody kept in its transcript.
///
/// The lines go from this part's offset on the shared clock onwards (any
/// part after this one is written again when it next runs). The marker goes
/// only when every other part writing into the note is done -- until then
/// it is the earlier part's own -- and only while the person has not written
/// in the note. The recordings sharing the note are stamped by hand with the
/// note's new time, for the caller already holds this one's lock.
fn release_follower(
    vault: &Vault,
    recording: &Recording,
    earlier: RecordingId,
    note_id: NoteId,
    sharing: &[Recording],
) -> CommandResult<Vec<Change>> {
    let mut changes = Vec::new();

    if let Some(mut transcript) = vault.transcript_for_note(note_id)? {
        let origin = transcript
            .recording_id
            .and_then(|r| vault.recording(r).ok())
            .or_else(|| vault.recording(earlier).ok())
            .map(|r| r.started_at);
        // No origin, or this part at it, would take out every line.
        if let Some(origin) = origin {
            let offset = ms_after(origin, recording.started_at);
            let before = transcript.segments.len();
            if offset > 0 {
                transcript.truncate_from(offset);
                if transcript.segments.len() != before {
                    vault.save_transcript(&transcript)?;
                    changes.push(change(Kind::Transcript, Op::Updated, transcript.id));
                }
            }
        }
    }

    let Ok(note) = vault.note(note_id) else { return Ok(changes) };
    let markdown = note.body.to_markdown();
    let others_done = sharing.iter().all(|r| matches!(r.stage, Stage::Done));
    if others_done && has_marker(&markdown) && untouched(&note, recording.note_written_at) {
        let (body, _) = swap_marker(&markdown, None);
        let mut next = note.clone();
        next.body = RichDoc::from_markdown(body.trim_start());
        next.updated_at = later_than(note.updated_at);
        match vault.save_note(&next, Some(note.updated_at)) {
            Ok(()) => {
                for other in sharing {
                    crate::meeting::spool::mutate_recording(vault, other.id, |r| {
                        r.note_written_at = Some(next.updated_at);
                        Ok(())
                    })?;
                }
                changes.push(change(Kind::Note, Op::Updated, note_id));
            }
            Err(e) if is_conflict(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(changes)
}

// ============================================================================
// The final write
// ============================================================================

/// What the finished notes put into the note.
pub(crate) struct Fill {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) tags: Vec<String>,
    pub(crate) purpose: Option<Purpose>,
}

/// What [`commit`] did to the note.
#[derive(Debug)]
pub(crate) struct Committed {
    pub(crate) note: Note,
    pub(crate) transcript: Transcript,
    /// A new note, not one written into: there was none, or it was deleted.
    pub(crate) created: bool,
    /// The person had written in the note, and their words are still in it,
    /// below the summary.
    pub(crate) kept_edits: bool,
}

/// How many times a write reads the note again after the person saved it
/// in between.
const WRITE_ATTEMPTS: usize = 5;

/// Write `fill` into the note `target`, or into a new one when there is none
/// or it has been deleted since. Never over the person's words: see the
/// module doc.
///
/// Safe to run again after it already ran: a note that already starts with
/// this body is left as it is.
fn write_note(
    vault: &Vault,
    recording: RecordingId,
    target: Option<NoteId>,
    fill: &Fill,
) -> CommandResult<(Note, bool, bool)> {
    if let Some(note_id) = target {
        let wanted = RichDoc::from_markdown(&fill.body).to_markdown();
        for _ in 0..WRITE_ATTEMPTS {
            let note = match vault.note(note_id) {
                Ok(note) => note,
                Err(e) if is_missing(&e) => break,
                Err(e) => return Err(e.into()),
            };
            let written = vault.recording(recording)?.note_written_at;
            let replace = untouched(&note, written);
            let current = note.body.to_markdown();
            if !has_marker(&current) && current.trim_start().starts_with(wanted.trim()) {
                return Ok((note, false, !replace));
            }
            let read_at = note.updated_at;
            let mut next = note.clone();
            if replace {
                next.title = fill.title.clone();
                next.body = RichDoc::from_markdown(&fill.body);
                next.tags = fill.tags.clone();
                next.purpose = fill.purpose;
            } else {
                let (theirs, _) = swap_marker(&current, None);
                let body = if theirs.is_empty() {
                    fill.body.clone()
                } else {
                    format!("{}\n\n---\n\n{theirs}", fill.body.trim_end())
                };
                next.body = RichDoc::from_markdown(&body);
                for tag in &fill.tags {
                    if !next.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                        next.tags.push(tag.clone());
                    }
                }
                if next.purpose.is_none() {
                    next.purpose = fill.purpose;
                }
            }
            next.updated_at = later_than(read_at);
            match vault.save_note(&next, Some(read_at)) {
                Ok(()) => {
                    if replace {
                        stamp(vault, note_id, Some(next.updated_at))?;
                    }
                    return Ok((next, false, !replace));
                }
                Err(e) if is_conflict(&e) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        if vault.note(note_id).is_ok() {
            return Err(CommandError::new(
                codes::CONFLICT,
                "the note kept changing while the summary was being written into it",
            ));
        }
    }
    // A pipeline racing a discard must not bring the recording back as a
    // note nobody can retry or tidy: with its row gone, there is nothing to
    // write for.
    if let Err(e) = vault.recording(recording) {
        return Err(if is_missing(&e) {
            CommandError::new(codes::NOT_FOUND, "the recording was discarded while it was written")
        } else {
            e.into()
        });
    }
    let mut note = Note::written_by(NoteOrigin::Meeting, fill.title.clone(), &fill.body);
    note.tags = fill.tags.clone();
    note.purpose = fill.purpose;
    vault.save_note(&note, None)?;
    Ok((note, true, false))
}

/// The final write: the finished notes into the recording's note, the
/// transcript under the note's one transcript record, and the recording
/// `Done`, its audio's bookkeeping cleared. Runs under `blocking`, and
/// again if a transient error sends the stage round once more, so every step
/// is safe to repeat.
pub(crate) fn commit(
    vault: &Vault,
    id: RecordingId,
    target: Option<NoteId>,
    fill: &Fill,
    mut transcript: Transcript,
) -> CommandResult<Committed> {
    let (note, created, kept_edits) = write_note(vault, id, target, fill)?;

    transcript.note_id = note.id;
    if let Some(existing) = vault.transcript_for_note(note.id)? {
        // One transcript to a note.
        transcript.id = existing.id;
        transcript.created_at = existing.created_at;
    }
    transcript.segments.sort_by_key(|s| s.start_ms);
    transcript.updated_at = Timestamp::now();
    vault.save_transcript(&transcript)?;

    let written = (!kept_edits).then_some(note.updated_at);
    let note_id = note.id;
    crate::meeting::spool::mutate_recording(vault, id, |recording| {
        recording.stage = Stage::Done;
        recording.note_id = Some(note_id);
        recording.note_written_at = written;
        recording.chunks.clear();
        recording.partial.clear();
        recording.updated_at = Timestamp::now();
        Ok(())
    })?;
    Ok(Committed { note, transcript, created, kept_edits })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_found_in_the_editors_own_markdown() {
        let body = format!("- **When:** today\n\n{MARKER}\n\n## Your notes");
        assert!(has_marker(&body));
        // The editor may write emphasis with stars, or escape the underscores.
        assert!(has_marker(
            "*Writing the summary of this call. The transcript is below; this note fills in by itself when the summary is ready.*"
        ));
        assert!(!has_marker("Writing a summary is hard."));
    }

    #[test]
    fn swapping_the_marker_leaves_no_gap_and_no_second_copy() {
        let body = format!("- **When:** today\n\n{MARKER}\n\n## Your notes\n\n- one");
        let (out, found) = swap_marker(&body, None);
        assert!(found);
        assert_eq!(out, "- **When:** today\n\n## Your notes\n\n- one");

        let failed = failure_line("the assistant is not configured");
        let (out, _) = swap_marker(&body, Some(&failed));
        assert!(has_failure_line(&out));
        assert!(!has_marker(&out));
        // A failure line is swapped back for the marker the same way.
        let (out, _) = swap_marker(&out, Some(MARKER));
        assert!(has_marker(&out) && !has_failure_line(&out));
        assert_eq!(swap_marker("nothing here", None), ("nothing here".to_string(), false));
    }

    #[test]
    fn a_failure_line_says_why_and_how_to_retry() {
        let line = failure_line("timed out.");
        assert_eq!(
            line,
            "_The summary could not be written: timed out. Retry it from the recording in the notes list._"
        );
    }
}
