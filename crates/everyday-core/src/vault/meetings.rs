//! Meeting notes: recordings, transcripts, voiceprints, and the settings that
//! decide what gets recorded.
//!
//! On the same terms every optional domain in this module is: an optional
//! store, reached through [`Vault::with_domain`], refusing to mutate when
//! this process holds no write claim. Two things are not that plain
//! forwarding, and both are here because they reach outside
//! `MeetingStore` itself:
//!
//! * **The transcriber's own key** is not part of [`MeetingSettings`] at
//!   all -- see [`Vault::set_transcriber_key`] for why, the same argument
//!   [`Vault::set_agent_key`] makes for the assistant's.
//! * **A transcript's words belong in search**, and search is built from
//!   entries and notes, neither of which knows a transcript exists. See
//!   [`reindex_note`] for how the two are kept in step.
//! * **Deleting a note is a cascade into a different domain.** A note owns
//!   nothing about the call that produced it -- the pointer runs the other
//!   way, `Recording::note_id` -- so `Vault::delete_note` cannot express
//!   "and take its transcript with it" through `NoteStore` alone. See
//!   [`detach_note_from_meetings`].
//! * **The spool is not `MeetingStore`'s to keep.** A recording's audio
//!   lives in plain files beside the vault directory -- `spool/recordings/`,
//!   found again by id, track and sequence, never by content hash -- rather
//!   than in the backend's own blob store, because it has to exist and be
//!   findable the same way whether the backend is SQLite or Postgres, and a
//!   Postgres vault has nowhere on this machine to put a `BYTEA` a person
//!   never asked it to keep past the note being written. See
//!   [`Vault::seal_spool_bytes`] and `everyday_service::meeting::spool`,
//!   which is the only caller.

use super::Vault;
use super::session::{Domain, Unlocked, pick_domain};
use crate::error::Result;
use crate::id::{NoteId, RecordingId, TranscriptId, VoiceprintId};
use crate::meeting::{MeetingSettings, Recording, Stage, Transcript, Voiceprint};
use crate::store::meetings::{MeetingStore, RecordingQuery};
use crate::store::secrets::SecretStore;

/// Owner kind the transcriber's own key is sealed under in the per-record
/// secret store -- see [`crate::store::secrets`]. There is one transcriber
/// per vault, so the owner id is a fixed string rather than a record's id,
/// the same shape `agent_secret` used before per-record secrets existed at
/// all.
pub const TRANSCRIBER_SECRET_OWNER_KIND: &str = "transcriber";
const TRANSCRIBER_SECRET_OWNER_ID: &str = "singleton";

impl Vault {
    /// Does this vault's backend hold meeting notes at all?
    pub fn supports_meetings(&self) -> bool {
        self.with_meetings(|_| Ok(())).is_ok()
    }

    fn with_meetings<T>(&self, f: impl FnOnce(&dyn MeetingStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Meetings, |s| s.meetings().map(f))
    }

    /// Named apart from `accounts.rs`'s own `with_secrets`: item privacy in
    /// Rust is per *module*, and each file under this one is its own module,
    /// so a name declared there is not reachable from here even though both
    /// are `impl Vault` for the same type.
    fn with_meeting_secrets<T>(&self, f: impl FnOnce(&dyn SecretStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Secrets, |s| s.secrets().map(f))
    }

    // ---- settings ---------------------------------------------------------

    pub fn meeting_settings(&self) -> Result<MeetingSettings> {
        self.with_meetings(|m| m.meeting_settings())
    }

    pub fn save_meeting_settings(&self, settings: &MeetingSettings) -> Result<()> {
        self.writable()?;
        self.with_meetings(|m| m.put_meeting_settings(settings))
    }

    // ---- the transcriber's own key -----------------------------------------

    /// Store the transcriber's key, replacing any previous one, or forget it
    /// when `key` is `None` or blank.
    ///
    /// Kept apart from [`MeetingSettings`] for the reason
    /// [`crate::store::agent::AgentStore::put_secret`] gives about the
    /// assistant's: a credential has a different shape from configuration
    /// and must not be handed back out by accident. There is no getter on
    /// this type; the only reader is
    /// [`everyday_service`](../../everyday_service/index.html)'s pipeline, on
    /// its way to building a request.
    pub fn set_transcriber_key(&self, key: Option<&str>) -> Result<()> {
        self.writable()?;
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => self.with_meeting_secrets(|s| {
                s.put_secret(
                    TRANSCRIBER_SECRET_OWNER_KIND,
                    TRANSCRIBER_SECRET_OWNER_ID,
                    k.as_bytes(),
                )
            }),
            None => self.with_meeting_secrets(|s| {
                s.delete_secret(TRANSCRIBER_SECRET_OWNER_KIND, TRANSCRIBER_SECRET_OWNER_ID)
            }),
        }
    }

    /// The transcriber's own key, if one is stored. Not the assistant's --
    /// see [`Vault::assistant_key_if_openai`] for that half of
    /// `use_assistant_key`, and `everyday_service::meeting::transcriber_key`
    /// for the function that picks between the two.
    pub fn transcriber_key(&self) -> Result<Option<String>> {
        let bytes = self.with_meeting_secrets(|s| {
            s.get_secret(TRANSCRIBER_SECRET_OWNER_KIND, TRANSCRIBER_SECRET_OWNER_ID)
        })?;
        Ok(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    pub fn has_transcriber_key(&self) -> Result<bool> {
        Ok(self.transcriber_key()?.is_some())
    }

    // ---- the spool's own bytes ---------------------------------------------

    /// Seal `plaintext`, bound to `aad`, under this vault's own key.
    ///
    /// What the spool uses to write one chunk file. Deliberately not
    /// [`Vault::put_blob`]: that store is content-addressed and belongs to
    /// whichever backend the vault chose, while a spool chunk is always a
    /// plain file beside the vault directory (see [`Vault::path`]), found
    /// again by recording id, track and sequence -- and gone, files and all,
    /// the moment the note exists. Requires only that the vault be
    /// unlocked; the write claim is [`everyday_service::meeting::spool`]'s
    /// own to check, since only it knows which calls actually touch disk.
    pub fn seal_spool_bytes(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        self.read(|u| u.cipher.seal(aad, plaintext))
    }

    /// The other half of [`Vault::seal_spool_bytes`].
    pub fn open_spool_bytes(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        self.read(|u| u.cipher.open(aad, sealed))
    }

    // ---- recordings ---------------------------------------------------------

    pub fn recordings(&self, query: &RecordingQuery) -> Result<Vec<Recording>> {
        self.with_meetings(|m| m.list_recordings(query))
    }

    pub fn recording(&self, id: RecordingId) -> Result<Recording> {
        self.with_meetings(|m| m.get_recording(id))
    }

    pub fn save_recording(&self, recording: &Recording) -> Result<()> {
        self.writable()?;
        self.with_meetings(|m| m.put_recording(recording))
    }

    /// Delete one history row. Not for a recording still in progress -- the
    /// pipeline owns that lifecycle -- but this does not check the stage
    /// itself; `everyday_service::domains::meetings::delete_recording` is
    /// where "only `done` or `failed`" is enforced, the same split
    /// `Vault::delete_run` and its own caller make.
    pub fn delete_recording(&self, id: RecordingId) -> Result<()> {
        self.writable()?;
        self.with_meetings(|m| m.delete_recording(id))
    }

    // ---- transcripts --------------------------------------------------------

    pub fn transcript(&self, id: TranscriptId) -> Result<Transcript> {
        self.with_meetings(|m| m.get_transcript(id))
    }

    pub fn transcript_for_note(&self, note_id: NoteId) -> Result<Option<Transcript>> {
        self.with_meetings(|m| m.transcript_for_note(note_id))
    }

    /// Save a transcript, and fold its words into the note's search entry.
    ///
    /// The two happen together because they must never be seen apart: a
    /// transcript saved without its note being reindexed is a transcript
    /// nobody can find by searching for what was said in it, which is the
    /// whole point of keeping one. See [`reindex_note`].
    pub fn save_transcript(&self, transcript: &Transcript) -> Result<()> {
        self.writable()?;
        let note_id = transcript.note_id;
        self.write(|u| {
            let meetings = pick_domain(u.store.as_ref(), Domain::Meetings, |s| s.meetings())?;
            meetings.put_transcript(transcript)?;
            reindex_note(u, note_id);
            Ok(())
        })
    }

    /// Delete a transcript, and drop its words back out of search.
    pub fn delete_transcript(&self, id: TranscriptId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let meetings = pick_domain(u.store.as_ref(), Domain::Meetings, |s| s.meetings())?;
            // Read first: once the row is gone there is no way to ask which
            // note it belonged to.
            let note_id = meetings.get_transcript(id)?.note_id;
            meetings.delete_transcript(id)?;
            reindex_note(u, note_id);
            Ok(())
        })
    }

    // ---- voiceprints --------------------------------------------------------

    pub fn voiceprints(&self) -> Result<Vec<Voiceprint>> {
        self.with_meetings(|m| m.list_voiceprints())
    }

    pub fn voiceprint(&self, id: VoiceprintId) -> Result<Voiceprint> {
        self.with_meetings(|m| m.get_voiceprint(id))
    }

    pub fn save_voiceprint(&self, voiceprint: &Voiceprint) -> Result<()> {
        self.writable()?;
        self.with_meetings(|m| m.put_voiceprint(voiceprint))
    }

    /// Delete a voice.
    ///
    /// Does **not** walk every transcript clearing `Speaker::voiceprint_id`
    /// where it named this one. That cascade is real -- a transcript can
    /// point at a voiceprint that no longer exists -- and is handled lazily
    /// instead: `Speaker` is drawn from whatever the transcript actually
    /// says, and anything that resolves a `voiceprint_id` against the
    /// current list of voices (naming someone from a chip, say) already has
    /// to cope with the id pointing at nothing, the same way
    /// `Recording::note_id` copes with a deleted note. A full scan of every
    /// transcript in the vault, every time a voice is deleted, would cost
    /// more than the dangling pointer it prevents: nothing renders a
    /// `voiceprint_id` directly, only the `label` and `how` beside it, so a
    /// stale id changes nothing anybody sees.
    pub fn delete_voiceprint(&self, id: VoiceprintId) -> Result<()> {
        self.writable()?;
        self.with_meetings(|m| m.delete_voiceprint(id))
    }

    pub fn delete_all_voiceprints(&self) -> Result<()> {
        self.writable()?;
        self.with_meetings(|m| m.delete_all_voiceprints())
    }
}

/// Re-derive one note's search entry from what is actually stored: the note
/// itself, plus its transcript's words if it has one.
///
/// Looked up fresh rather than handed the transcript that was just saved or
/// deleted, so [`Vault::save_transcript`] and [`Vault::delete_transcript`]
/// can share it, and so [`rebuild_meeting_index`] -- the unlock and
/// `Vault::reindex` path -- can call the same function a single save does.
/// A note with no backend, or no longer there, is left alone or dropped from
/// the index respectively; neither is an error, because a search index that
/// cannot be rebuilt for a stale id would be worse than one that is briefly
/// behind.
fn reindex_note(u: &mut Unlocked, note_id: NoteId) {
    let Some(notes) = u.store.notes() else { return };
    let note = match notes.get_note(note_id) {
        Ok(note) => note,
        Err(e) if e.code() == "not_found" => {
            u.index.remove_note(note_id);
            return;
        }
        Err(_) => return,
    };
    let extra = u
        .store
        .meetings()
        .and_then(|m| m.transcript_for_note(note_id).ok().flatten())
        .map(|t| t.searchable_text());
    match extra {
        Some(text) => u.index.insert_note_with_extra(&note, &text),
        None => u.index.insert_note(&note),
    }
}

/// Fold every meeting note's transcript into an already-built search index.
///
/// Called after [`crate::search::SearchIndex::build`], which only knows
/// entries and notes: a transcript lives in its own table, reached through
/// [`Recording::note_id`], so the notes it belongs to have to be found and
/// re-indexed as a second pass. Recordings, not notes, are what is walked --
/// there are as many of these as there have been meetings, which in every
/// vault is far fewer than the notes it holds, and only a recording that
/// reached `Done` carries a `note_id` at all.
pub(super) fn rebuild_meeting_index(
    store: &dyn crate::store::JournalStore,
    index: &mut crate::search::SearchIndex,
) -> Result<()> {
    let (Some(meetings), Some(notes)) = (store.meetings(), store.notes()) else { return Ok(()) };
    for recording in meetings.list_recordings(&RecordingQuery::default())? {
        let Some(note_id) = recording.note_id else { continue };
        let Ok(note) = notes.get_note(note_id) else { continue };
        if let Some(t) = meetings.transcript_for_note(note_id)? {
            index.insert_note_with_extra(&note, &t.searchable_text());
        }
    }
    Ok(())
}

/// What `Vault::delete_note` reaches for after taking the note itself: the
/// transcript, which is nothing without the note it was kept beside, and the
/// finished recording's own pointer back to it, which
/// [`Recording::note_id`]'s own doc already promises is "cleared if the note
/// is deleted".
///
/// Only recordings at [`Stage::Done`] are walked -- `note_id` is `None`
/// everywhere else in the pipeline, per that field's own doc, so nothing
/// earlier in the pipeline can be pointing at this note in the first place.
/// `RecordingQuery` has no way to filter by `note_id` itself (there is no
/// clear column for it -- see the SQL schema's own reasoning), so this reads
/// every finished recording and checks in Rust; a vault's history of
/// finished calls is small enough that this costs nothing a person would
/// notice, and it is the same shape `list_recordings` itself is built to
/// finish its own filters in.
pub(super) fn detach_note_from_meetings(u: &mut Unlocked, note_id: NoteId) -> Result<()> {
    let Some(meetings) = u.store.meetings() else { return Ok(()) };

    if let Some(transcript) = meetings.transcript_for_note(note_id)? {
        meetings.delete_transcript(transcript.id)?;
    }

    let done = meetings.list_recordings(&RecordingQuery {
        stages: vec![Stage::Done.as_str().to_string()],
        ..Default::default()
    })?;
    for mut recording in done {
        if recording.note_id == Some(note_id) {
            recording.note_id = None;
            meetings.put_recording(&recording)?;
        }
    }
    Ok(())
}
