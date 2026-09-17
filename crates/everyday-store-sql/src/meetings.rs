//! Meeting notes: recordings, transcripts, voiceprints, and the settings
//! that decide what gets recorded.
//!
//! One singleton, and three ordinary tables. `meeting_settings` follows
//! `agent_settings`'s own shape exactly -- see `agent.rs` beside this file.
//! The rest are unremarkable by this crate's standards: `Recording` and
//! `Voiceprint` fit [`Record`] and go through [`SqlStore::get`] /
//! [`SqlStore::upsert`] / [`SqlStore::delete_by_id`] like most tables in this
//! crate do; `Transcript` almost does, and keeps one method of its own --
//! [`MeetingStore::transcript_for_note`] -- because the generic helpers only
//! know how to look a row up by its own id, and this table's second most
//! common question is asked by the note beside it instead.

use everyday_core::error::Result;
use everyday_core::id::{NoteId, RecordingId, TranscriptId, VoiceprintId};
use everyday_core::meeting::{
    MeetingSettings, Recording, SETTINGS_AAD, Transcript, Voiceprint, recording_aad,
    transcript_aad, voiceprint_aad,
};
use everyday_core::store::meetings::{MeetingStore, RecordingQuery};

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::record::Record;
use crate::{SqlStore, to_us, vals};

impl Record for Recording {
    const TABLE: &'static str = "recordings";
    const KIND: &'static str = "recording";
    type Id = RecordingId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        recording_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("stage", self.stage.as_str().to_value()),
            ("calendar_id", self.calendar_id().map(|c| c.to_string()).to_value()),
            ("started_us", to_us(self.started_at).to_value()),
        ]
    }
}

impl Record for Transcript {
    const TABLE: &'static str = "transcripts";
    const KIND: &'static str = "transcript";
    type Id = TranscriptId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        transcript_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![("note_id", self.note_id.to_string().to_value())]
    }
}

impl Record for Voiceprint {
    const TABLE: &'static str = "voiceprints";
    const KIND: &'static str = "voiceprint";
    type Id = VoiceprintId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        voiceprint_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![("created_us", to_us(self.created_at).to_value())]
    }
}

impl MeetingStore for SqlStore {
    // ---- settings ---------------------------------------------------------

    fn meeting_settings(&self) -> Result<MeetingSettings> {
        let sealed = self.read().sealed("SELECT data FROM meeting_settings WHERE id = 1", &[])?;
        match sealed {
            Some(sealed) => self.unseal(SETTINGS_AAD, &sealed),
            // Never configured. The default -- the feature is off -- is a
            // real answer, the same argument `AgentStore::settings` makes.
            None => Ok(MeetingSettings::default()),
        }
    }

    fn put_meeting_settings(&self, settings: &MeetingSettings) -> Result<()> {
        let data = self.seal(SETTINGS_AAD, settings)?;
        self.write().execute(
            "INSERT INTO meeting_settings (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    // ---- recordings ---------------------------------------------------------

    fn list_recordings(&self, query: &RecordingQuery) -> Result<Vec<Recording>> {
        // What an index can answer is pushed down -- the stage set, the
        // calendar and the lower time bound; the rest (the exclusive upper
        // bound, the exact sort and the cap) is finished in Rust by
        // `RecordingQuery::apply`, the same split `RunQuery::apply` makes for
        // the assistant's log, and for the same reason: two backends must
        // not be able to disagree about where a tie goes.
        let mut w = Where::new();
        if query.stages.len() == 1 {
            w = w.eq("stage", query.stages[0].as_str());
        } else if !query.stages.is_empty() {
            w = w.in_list("stage", query.stages.iter().map(String::as_str));
        }
        if let Some(cal) = query.calendar_id {
            w = w.eq("calendar_id", cal.to_string());
        }
        if let Some(from) = query.from {
            w = w.gte("started_us", to_us(from));
        }
        let (where_sql, args) = w.finish();
        let sql =
            format!("SELECT id, data FROM recordings WHERE {where_sql} ORDER BY started_us DESC");
        let rows = self.read().records(&sql, &args)?;
        Ok(query.apply(self.collect(rows, recording_aad)?))
    }

    fn get_recording(&self, id: RecordingId) -> Result<Recording> {
        self.get(id)
    }

    fn put_recording(&self, recording: &Recording) -> Result<()> {
        self.upsert(recording)
    }

    fn delete_recording(&self, id: RecordingId) -> Result<()> {
        self.delete_by_id::<Recording>(id)?;
        Ok(())
    }

    // ---- transcripts --------------------------------------------------------

    fn get_transcript(&self, id: TranscriptId) -> Result<Transcript> {
        self.get(id)
    }

    fn transcript_for_note(&self, note_id: NoteId) -> Result<Option<Transcript>> {
        // Not `SqlStore::get`: that looks a row up by its own id, and the
        // question here is asked by the note beside it instead. `id` is
        // still selected, because unsealing needs it -- the associated data
        // a transcript is sealed under is bound to its own id, not to the
        // note's, so the row has to be read before it can be opened.
        let rows = self.read().records(
            "SELECT id, data FROM transcripts WHERE note_id = ?1",
            &vals![note_id.to_string()],
        )?;
        let Some((id, sealed)) = rows.into_iter().next() else { return Ok(None) };
        let id = id
            .parse::<TranscriptId>()
            .map_err(|e| everyday_core::error::Error::Invalid(e.to_string()))?;
        Ok(Some(self.unseal(&transcript_aad(id), &sealed)?))
    }

    fn put_transcript(&self, transcript: &Transcript) -> Result<()> {
        self.upsert(transcript)
    }

    fn delete_transcript(&self, id: TranscriptId) -> Result<()> {
        self.delete_by_id::<Transcript>(id)?;
        Ok(())
    }

    // ---- voiceprints --------------------------------------------------------

    fn list_voiceprints(&self) -> Result<Vec<Voiceprint>> {
        let rows =
            self.read().records("SELECT id, data FROM voiceprints ORDER BY created_us ASC", &[])?;
        self.collect(rows, voiceprint_aad)
    }

    fn get_voiceprint(&self, id: VoiceprintId) -> Result<Voiceprint> {
        self.get(id)
    }

    fn put_voiceprint(&self, voiceprint: &Voiceprint) -> Result<()> {
        self.upsert(voiceprint)
    }

    fn delete_voiceprint(&self, id: VoiceprintId) -> Result<()> {
        self.delete_by_id::<Voiceprint>(id)?;
        Ok(())
    }

    fn delete_all_voiceprints(&self) -> Result<()> {
        self.write().execute("DELETE FROM voiceprints", &[])?;
        Ok(())
    }
}
