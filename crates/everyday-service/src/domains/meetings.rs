//! Meeting notes: settings, the recording history, transcripts and
//! voiceprints, and -- since the spool work -- recording itself:
//! `begin`/`append`/`finish`/`discard`/`retry` and `dismiss_meeting_offer`,
//! each a thin wire wrapper over `everyday_service::meeting::spool` and
//! `everyday_service::meeting::watch`, which hold the actual behaviour.
//! `everyday_core::meeting` decides what a call needs to be one;
//! `everyday_core::vault::Vault` holds it; this is the surface a client --
//! the interface, the shell's capture code, the assistant -- reaches both
//! through.
//!
//! What is *still not* here: `name_speaker`, `rewrite_meeting_note`,
//! `preview_meeting_template`, `enrol_voice`, and the speech-model download
//! commands. Those need the pipeline, the transcribers and the speech kit,
//! all of which are somebody else's file in this same change -- see
//! `docs/plans/meeting-notes.md`.
//!
//! # A stub two files away
//!
//! [`usable`] decides whether the switch may be turned on without calling
//! [`crate::meeting::models::installed`] or
//! [`crate::meeting::models::speech_kit_installed`] itself -- both are
//! `todo!()` until the speech agent's work lands. Instead it takes what they
//! would have answered as plain booleans, so it can be unit tested today and
//! [`view`] is the one place, reached only once a transcriber is actually
//! configured, that calls the real thing.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::meeting::{spool, watch};
use crate::service::{Service, blocking};
use base64::Engine;
use everyday_core::id::EventId;
use everyday_core::meeting::{
    MeetingSettings, NoteTemplate, Recording, Stage, Track, TranscriberConfig, Transcript,
    Voiceprint,
};
use everyday_core::store::meetings::RecordingQuery;
use everyday_core::{Error, NoteId, RecordingId, TemplateId, Vault, VoiceprintId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

// ---- wire shapes ----------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMeetingSettings {
    pub settings: MeetingSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTranscriberKey {
    pub key: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListRecordings {
    #[serde(default)]
    pub query: RecordingQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingRef {
    pub id: RecordingId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginRecording {
    #[serde(default)]
    pub event_id: Option<EventId>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub template_id: Option<TemplateId>,
    #[serde(default)]
    pub automatic: bool,
}

/// One spooled chunk, over the wire. `pcm` is little-endian `i16` samples,
/// base64-encoded -- `everyday-app`'s `capture::RecordingSink::send_once` is
/// the one place that builds one of these, and it is the shape this struct
/// exists to mirror exactly.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendRecordingChunk {
    pub id: RecordingId,
    pub track: Track,
    pub seq: u32,
    pub start_ms: u64,
    pub pcm: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DismissMeetingOffer {
    pub event_id: EventId,
    pub never: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTranscript {
    pub note_id: NoteId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceprintRef {
    pub id: VoiceprintId,
}

/// A voice, without its vectors -- what a settings pane draws a list from.
/// Mirrors the TS `VoiceprintInfo`.
///
/// `Deserialize` too, not only `Serialize`: `everyday-app`'s `voice_enrol`
/// command gets one of these back from the service command `enrol_voice`
/// over the same JSON `Session::call` every other command answers through,
/// and has to read it back out of the `Value` to hand it on to the
/// interface typed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceprintInfo {
    pub id: VoiceprintId,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub is_owner: bool,
    pub model: String,
    pub samples: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl From<Voiceprint> for VoiceprintInfo {
    fn from(v: Voiceprint) -> Self {
        Self {
            id: v.id,
            name: v.name,
            email: v.email,
            is_owner: v.is_owner,
            model: v.model,
            samples: v.samples,
            created_at: v.created_at,
            updated_at: v.updated_at,
        }
    }
}

/// What `meeting_settings` answers: the settings, and what they add up to.
/// Mirrors the TS `MeetingSettingsView`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSettingsView {
    pub settings: MeetingSettings,
    /// A transcription key is stored. The key itself never leaves the vault.
    pub has_key: bool,
    /// The transcriber is chosen and can run; the switch may be turned on.
    pub usable: bool,
    /// Why not, in words, when `usable` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The assistant talks to api.openai.com with a key, so it could be
    /// reused.
    pub assistant_key_available: bool,
    /// Audio leaves this machine with the chosen transcriber.
    pub remote: bool,
    /// Local speech is compiled into this build.
    pub local_speech: bool,
}

// ---- the pure decision ----------------------------------------------------

/// Is this transcriber set up well enough to switch on? See the module
/// docs for why the installation checks arrive as booleans rather than
/// being made here.
///
/// Conservative on purpose: every branch that cannot say yes says no with a
/// reason, rather than falling through to a default that might be "usable".
fn usable(
    transcriber: &TranscriberConfig,
    use_assistant_key: bool,
    has_transcriber_key: bool,
    assistant_key_available: bool,
    local_recogniser_installed: bool,
    speech_kit_installed: bool,
) -> Result<(), String> {
    match transcriber {
        TranscriberConfig::Local { model } => {
            if !cfg!(feature = "speech") {
                return Err("this build does not include local speech".into());
            }
            if !local_recogniser_installed {
                return Err(format!("{} has not been downloaded yet", model.as_str()));
            }
            if !speech_kit_installed {
                return Err("the speech kit has not been downloaded yet".into());
            }
            Ok(())
        }
        TranscriberConfig::OpenAi { .. } => {
            if !(has_transcriber_key || (use_assistant_key && assistant_key_available)) {
                return Err("add an OpenAI key, or turn on \"use the assistant's key\"".into());
            }
            if !speech_kit_installed {
                return Err("the speech kit has not been downloaded yet -- it is needed to detect speech before any audio is sent".into());
            }
            Ok(())
        }
        TranscriberConfig::Google { .. } => {
            if !has_transcriber_key {
                return Err("add a Google API key".into());
            }
            if !speech_kit_installed {
                return Err("the speech kit has not been downloaded yet -- it is needed to detect speech before any audio is sent".into());
            }
            Ok(())
        }
        TranscriberConfig::Compatible { base_url, model } => {
            if base_url.trim().is_empty() {
                return Err("a compatible server needs a base URL".into());
            }
            if model.trim().is_empty() {
                return Err("a compatible server needs a model name".into());
            }
            if transcriber.needs_key() && !has_transcriber_key {
                return Err("add a key for this server".into());
            }
            if transcriber.is_remote() && !speech_kit_installed {
                return Err("the speech kit has not been downloaded yet -- it is needed to detect speech before any audio is sent".into());
            }
            Ok(())
        }
    }
}

/// What [`usable`] needs from `meeting::models` for one transcriber choice --
/// the one place this module calls into the speech agent's stubs. Only
/// reached once a transcriber is actually configured, so the common "not set
/// up yet" case never touches them.
fn installation_state(transcriber: &TranscriberConfig) -> (bool, bool) {
    match transcriber {
        TranscriberConfig::Local { model } => (
            crate::meeting::models::installed(*model),
            crate::meeting::models::speech_kit_installed(),
        ),
        _ => (false, crate::meeting::models::speech_kit_installed()),
    }
}

/// Build the view a settings pane draws: the settings themselves, plus
/// everything derived from them and from the vault.
///
/// `pub(crate)` rather than private: `everyday_service::meeting::spool::begin`
/// reuses this exact check to refuse starting a recording the same way
/// `save_meeting_settings` refuses turning the switch on -- see that
/// module's own docs on why it does not reimplement [`usable`] by hand.
pub(crate) fn view(
    vault: &Vault,
    settings: MeetingSettings,
) -> everyday_core::Result<MeetingSettingsView> {
    let has_key = vault.has_transcriber_key()?;
    let assistant_key_available = vault.assistant_key_if_openai()?.is_some();

    let (usable, problem) = match &settings.transcriber {
        None => (false, Some("choose a transcriber before turning meeting notes on".to_string())),
        Some(t) => {
            let (local_installed, speech_kit) = installation_state(t);
            match self::usable(
                t,
                settings.use_assistant_key,
                has_key,
                assistant_key_available,
                local_installed,
                speech_kit,
            ) {
                Ok(()) => (true, None),
                Err(reason) => (false, Some(reason)),
            }
        }
    };
    let remote = settings.transcriber.as_ref().is_some_and(TranscriberConfig::is_remote);

    Ok(MeetingSettingsView {
        settings,
        has_key,
        usable,
        problem,
        assistant_key_available,
        remote,
        local_speech: cfg!(feature = "speech"),
    })
}

/// Non-empty names, non-empty bodies, and no two templates sharing an id.
/// `everyday_core::meeting::template::lint` -- which would also check the
/// placeholders a template uses -- is `todo!()` until the template work
/// lands, so this is only the part of validation that does not need it.
fn validate_templates(templates: &[NoteTemplate]) -> everyday_core::Result<()> {
    let mut ids = HashSet::new();
    for t in templates {
        if t.name.trim().is_empty() {
            return Err(Error::Invalid("a template needs a name".into()));
        }
        if t.body.trim().is_empty() {
            return Err(Error::Invalid(format!("{:?} has no body", t.name)));
        }
        if !ids.insert(t.id) {
            return Err(Error::Invalid("two templates cannot share an id".into()));
        }
    }
    Ok(())
}

/// The key a transcriber request should carry, whichever backend is
/// configured.
///
/// The one door the pipeline (`everyday_service::meeting::pipeline`) reaches
/// for on its way to building a request, rather than making it resolve
/// `use_assistant_key` itself: an `OpenAi` transcriber set to reuse the
/// assistant's key is answered from
/// [`Vault::assistant_key_if_openai`](everyday_core::Vault::assistant_key_if_openai)
/// -- which is `None` unless the assistant is actually pointed at OpenAI's
/// hosted endpoint with a key stored -- and everything else, including an
/// `OpenAi` transcriber that has its own key instead, is answered from
/// [`Vault::transcriber_key`](everyday_core::Vault::transcriber_key).
///
/// `None` covers both "no key needed" (a loopback `Compatible` server, or
/// `Local`) and "no key available yet" alike; a caller that cares which is
/// which already has the settings in hand to ask
/// [`TranscriberConfig::needs_key`].
pub fn transcriber_key(vault: &Vault) -> Option<String> {
    let settings = vault.meeting_settings().ok()?;
    if settings.use_assistant_key
        && matches!(settings.transcriber, Some(TranscriberConfig::OpenAi { .. }))
        && let Ok(Some(key)) = vault.assistant_key_if_openai()
    {
        return Some(key);
    }
    vault.transcriber_key().ok().flatten()
}

// ---- commands ---------------------------------------------------------

async fn meeting_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<MeetingSettingsView> {
    let vault = svc.require()?;
    blocking(move || Ok(view(&vault, vault.meeting_settings()?)?)).await
}

async fn save_meeting_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveMeetingSettings,
) -> CommandResult<MeetingSettingsView> {
    let vault = svc.require()?;
    blocking(move || {
        let mut settings = args.settings;
        validate_templates(&settings.templates)?;

        if settings.enabled {
            let checked = view(&vault, settings.clone())?;
            if !checked.usable {
                let why = checked.problem.unwrap_or_else(|| "the transcriber is not usable".into());
                return Err(Error::Invalid(format!("cannot turn meeting notes on: {why}")).into());
            }
        }
        // Else: a disabled feature is always a valid thing to save, whatever
        // state the transcriber is in -- somebody mid-way through
        // configuring it must still be able to save a draft.

        vault.save_meeting_settings(&settings)?;
        // Read back rather than echoing what was sent: `has_key` and the
        // rest of the view are derived from the vault, not from the
        // argument, so a stale flag on the way in cannot leak back out.
        settings = vault.meeting_settings()?;
        Ok(view(&vault, settings)?)
    })
    .await
}

async fn set_transcriber_key(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SetTranscriberKey,
) -> CommandResult<MeetingSettingsView> {
    let vault = svc.require()?;
    blocking(move || {
        vault.set_transcriber_key(args.key.as_deref())?;
        Ok(view(&vault, vault.meeting_settings()?)?)
    })
    .await
}

/// Mint a template without saving it. The id is the core's to allocate, for
/// the reason `new_routine` and `new_note` already give.
///
/// Calls `template::starter`, which is `todo!()` until the template work
/// lands -- fine at runtime once it does, but why this command has no test
/// of its own here.
async fn new_meeting_template(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<NoteTemplate> {
    let starter = everyday_core::meeting::template::starter();
    Ok(NoteTemplate { id: TemplateId::new(), name: "New template".into(), body: starter.body })
}

async fn list_recordings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ListRecordings,
) -> CommandResult<Vec<Recording>> {
    svc.on_vault(move |vault| vault.recordings(&args.query)).await
}

async fn get_recording(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RecordingRef,
) -> CommandResult<Recording> {
    svc.on_vault(move |vault| vault.recording(args.id)).await
}

/// The one recording actually running, if there is one -- what the pill in
/// the corner is drawn from on a window that was not the one recording.
async fn active_recording(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Option<Recording>> {
    svc.on_vault(move |vault| {
        let query = RecordingQuery {
            stages: vec![Stage::Recording.as_str().to_string()],
            limit: Some(1),
            ..Default::default()
        };
        Ok(vault.recordings(&query)?.into_iter().next())
    })
    .await
}

/// Delete one history row. Only once the pipeline is done with it: still
/// recording, transcribing, identifying or summarising is not this
/// command's to interrupt -- `everyday_service::meeting::pipeline` owns
/// that, through `discard`.
async fn delete_recording(svc: Arc<Service>, _ctx: Ctx, args: RecordingRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        let recording = vault.recording(args.id)?;
        if !matches!(recording.stage, Stage::Done | Stage::Failed { .. }) {
            return Err(Error::Invalid(
                "only a finished or failed recording can be deleted from the history".into(),
            )
            .into());
        }
        Ok(vault.delete_recording(args.id)?)
    })
    .await
}

async fn get_transcript(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: GetTranscript,
) -> CommandResult<Option<Transcript>> {
    svc.on_vault(move |vault| vault.transcript_for_note(args.note_id)).await
}

async fn list_voiceprints(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<VoiceprintInfo>> {
    svc.on_vault(move |vault| {
        Ok(vault.voiceprints()?.into_iter().map(VoiceprintInfo::from).collect())
    })
    .await
}

async fn delete_voiceprint(svc: Arc<Service>, _ctx: Ctx, args: VoiceprintRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_voiceprint(args.id)).await
}

async fn delete_all_voiceprints(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_all_voiceprints()).await
}

// ---- recording: begin / append / finish / discard / retry ----------------
//
// Thin wire wrappers over `everyday_service::meeting::spool`, which holds
// the actual behaviour -- refusals, the spool cap, idempotency, recovery.
// `blocking` rather than `svc.on_vault`, because every one of these needs
// `svc` itself (to reach the pipeline seam and the session's append-time
// tracking), not only the vault `on_vault`'s closure is handed.

async fn begin_recording(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: BeginRecording,
) -> CommandResult<Recording> {
    blocking(move || {
        spool::begin(
            &svc,
            spool::BeginArgs {
                event_id: args.event_id,
                title: args.title,
                template_id: args.template_id,
                automatic: args.automatic,
            },
        )
    })
    .await
}

/// `pcm` is decoded here, off the async runtime -- a 30 s chunk is up to
/// about 960 KB raw, 1.3 MB as the base64 this arrived over, comfortably
/// under both the server's `MAX_JSON_BYTES` (32 MB, `everyday-server`'s
/// `routes.rs`) and the in-process path, which has no limit at all.
async fn append_recording_chunk(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AppendRecordingChunk,
) -> CommandResult<()> {
    blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&args.pcm)
            .map_err(|e| CommandError::new("invalid", format!("pcm was not valid base64: {e}")))?;
        if bytes.len() % 2 != 0 {
            return Err(CommandError::new("invalid", "pcm must be an even number of bytes"));
        }
        let samples: Vec<i16> =
            bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        spool::append(&svc, args.id, args.track, args.seq, args.start_ms, &samples)
    })
    .await
}

async fn finish_recording(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RecordingRef,
) -> CommandResult<Recording> {
    blocking(move || spool::finish(&svc, args.id)).await
}

async fn discard_recording(svc: Arc<Service>, _ctx: Ctx, args: RecordingRef) -> CommandResult<()> {
    blocking(move || spool::discard(&svc, args.id)).await
}

async fn retry_recording(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RecordingRef,
) -> CommandResult<Recording> {
    blocking(move || spool::retry(&svc, args.id)).await
}

async fn dismiss_meeting_offer(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: DismissMeetingOffer,
) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || watch::dismiss(&svc, &vault, args.event_id, args.never)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "meeting_settings", scope: Meetings, effect: Read,
        args: Nothing, returns: "MeetingSettingsView", signature: &[],
        run: meeting_settings,
    },
    command! {
        name: "save_meeting_settings", scope: Meetings, effect: Write,
        change: Settings / Updated,
        args: SaveMeetingSettings, returns: "MeetingSettingsView",
        signature: &[("settings", "MeetingSettings", true)],
        run: save_meeting_settings,
    },
    command! {
        name: "set_transcriber_key", scope: Meetings, effect: Write,
        change: Settings / Updated,
        args: SetTranscriberKey, returns: "MeetingSettingsView",
        // `Option<String>` accepts the key being left out of the object
        // entirely, not only `null` -- serde does not require an `Option`
        // field's key to be present -- so `required` here has to say `false`
        // to match what `SetTranscriberKey` actually accepts, the same
        // check `tests/surface.rs` runs against every command's struct.
        signature: &[("key", "string | null", false)],
        run: set_transcriber_key,
    },
    command! {
        name: "new_meeting_template", scope: Meetings, effect: Read,
        args: Nothing, returns: "NoteTemplate", signature: &[],
        run: new_meeting_template,
    },
    command! {
        name: "list_recordings", scope: Meetings, effect: Read,
        args: ListRecordings, returns: "Recording[]",
        signature: &[("query", "RecordingQuery", false)],
        run: list_recordings,
    },
    command! {
        name: "get_recording", scope: Meetings, effect: Read,
        args: RecordingRef, returns: "Recording",
        signature: &[("id", "RecordingId", true)],
        run: get_recording,
    },
    command! {
        name: "active_recording", scope: Meetings, effect: Read,
        args: Nothing, returns: "Recording | null", signature: &[],
        run: active_recording,
    },
    command! {
        name: "delete_recording", scope: Meetings, effect: Destructive,
        change: Recording / Deleted,
        id: |a: &RecordingRef| Some(a.id.to_string()),
        args: RecordingRef, returns: "void",
        signature: &[("id", "RecordingId", true)],
        run: delete_recording,
    },
    command! {
        name: "begin_recording", scope: Meetings, effect: Write,
        // No `id:`: the id is minted inside `spool::begin`, not carried in
        // the arguments -- see `command.rs`'s own doc on why `id`/`ids` can
        // only ever read what a save or a delete's *arguments* already
        // name. `subscribe_calendar` and the rest of this table's own
        // `Created` commands are the same shape.
        change: Recording / Created,
        args: BeginRecording, returns: "Recording",
        signature: &[
            ("eventId", "EventId | null", false),
            ("title", "string | null", false),
            ("templateId", "TemplateId | null", false),
            ("automatic", "boolean", false),
        ],
        run: begin_recording,
    },
    command! {
        name: "append_recording_chunk", scope: Meetings, effect: Write,
        // No `change:`: a chunk landing is not something any list reloads
        // for -- `meetings.svelte.ts` polls the recordings list on its own
        // timer while a call is live, per that module's own doc on why
        // recordings are not yet in `ChangeKind`.
        args: AppendRecordingChunk, returns: "void",
        signature: &[
            ("id", "RecordingId", true),
            ("track", "Track", true),
            ("seq", "number", true),
            ("startMs", "number", true),
            ("pcm", "string", true),
        ],
        run: append_recording_chunk,
    },
    command! {
        name: "finish_recording", scope: Meetings, effect: Write,
        change: Recording / Updated,
        id: |a: &RecordingRef| Some(a.id.to_string()),
        args: RecordingRef, returns: "Recording",
        signature: &[("id", "RecordingId", true)],
        run: finish_recording,
    },
    command! {
        name: "discard_recording", scope: Meetings, effect: Destructive,
        change: Recording / Deleted,
        id: |a: &RecordingRef| Some(a.id.to_string()),
        args: RecordingRef, returns: "void",
        signature: &[("id", "RecordingId", true)],
        run: discard_recording,
    },
    command! {
        name: "retry_recording", scope: Meetings, effect: Write,
        change: Recording / Updated,
        id: |a: &RecordingRef| Some(a.id.to_string()),
        args: RecordingRef, returns: "Recording",
        signature: &[("id", "RecordingId", true)],
        run: retry_recording,
    },
    command! {
        name: "dismiss_meeting_offer", scope: Meetings, effect: Write,
        args: DismissMeetingOffer, returns: "void",
        signature: &[("eventId", "EventId", true), ("never", "boolean", true)],
        run: dismiss_meeting_offer,
    },
    command! {
        name: "get_transcript", scope: Meetings, effect: Read,
        args: GetTranscript, returns: "Transcript | null",
        signature: &[("noteId", "NoteId", true)],
        run: get_transcript,
    },
    command! {
        name: "list_voiceprints", scope: Meetings, effect: Read,
        args: Nothing, returns: "VoiceprintInfo[]", signature: &[],
        run: list_voiceprints,
    },
    command! {
        name: "delete_voiceprint", scope: Meetings, effect: Destructive,
        change: Voiceprint / Deleted,
        id: |a: &VoiceprintRef| Some(a.id.to_string()),
        args: VoiceprintRef, returns: "void",
        signature: &[("id", "VoiceprintId", true)],
        run: delete_voiceprint,
    },
    command! {
        name: "delete_all_voiceprints", scope: Meetings, effect: Destructive,
        change: Voiceprint / Deleted,
        args: Nothing, returns: "void", signature: &[],
        run: delete_all_voiceprints,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::meeting::LocalModel;

    fn openai(model: &str) -> TranscriberConfig {
        TranscriberConfig::OpenAi { model: model.into() }
    }

    #[test]
    fn a_transcriber_is_not_usable_until_it_has_a_key() {
        let t = openai("gpt-4o-transcribe-diarize");
        let err = usable(&t, false, false, false, false, true).unwrap_err();
        assert!(err.contains("key"), "{err}");
    }

    #[test]
    fn use_assistant_key_stands_in_for_a_stored_one() {
        let t = openai("gpt-4o-transcribe-diarize");
        assert!(usable(&t, true, false, true, false, true).is_ok());
        // Without the flag on, the assistant's key does not count.
        assert!(usable(&t, false, false, true, false, true).is_err());
    }

    #[test]
    fn a_remote_backend_also_needs_the_speech_kit() {
        let t = openai("gpt-4o-transcribe-diarize");
        let err = usable(&t, false, true, false, false, false).unwrap_err();
        assert!(err.contains("speech kit"), "{err}");
        assert!(usable(&t, false, true, false, false, true).is_ok());
    }

    #[test]
    fn local_needs_the_recogniser_and_the_speech_kit_and_the_feature() {
        let t = TranscriberConfig::Local { model: LocalModel::ParakeetV3 };
        assert!(usable(&t, false, false, false, false, false).is_err());
        assert!(usable(&t, false, false, false, true, false).is_err(), "still no speech kit");
        // Whether this is `Ok` from here depends on whether the `speech`
        // feature is compiled in -- see `usable`'s own `cfg!` check -- so
        // the strongest claim a feature-independent test can make is that
        // the two installation checks above are each necessary.
    }

    #[test]
    fn a_compatible_server_needs_a_url_and_a_model_before_anything_else() {
        let t =
            TranscriberConfig::Compatible { base_url: String::new(), model: "whisper-1".into() };
        assert!(usable(&t, false, true, false, false, true).is_err());
        let t = TranscriberConfig::Compatible {
            base_url: "http://localhost:8080".into(),
            model: String::new(),
        };
        assert!(usable(&t, false, true, false, false, true).is_err());
    }

    #[test]
    fn a_loopback_compatible_server_needs_no_key_and_no_speech_kit() {
        let t = TranscriberConfig::Compatible {
            base_url: "http://127.0.0.1:8080".into(),
            model: "whisper-1".into(),
        };
        assert!(usable(&t, false, false, false, false, false).is_ok());
    }

    #[test]
    fn a_non_loopback_compatible_server_needs_both() {
        let t = TranscriberConfig::Compatible {
            base_url: "https://transcribe.example.com".into(),
            model: "whisper-1".into(),
        };
        assert!(usable(&t, false, false, false, false, true).is_err(), "no key");
        assert!(usable(&t, false, true, false, false, false).is_err(), "no speech kit");
        assert!(usable(&t, false, true, false, false, true).is_ok());
    }

    #[test]
    fn google_needs_only_its_own_key_and_the_speech_kit() {
        let t = TranscriberConfig::Google { model: "gemini-2.5-flash".into() };
        assert!(usable(&t, false, false, false, false, true).is_err());
        assert!(usable(&t, false, true, false, false, false).is_err());
        assert!(usable(&t, false, true, false, false, true).is_ok());
    }

    #[test]
    fn templates_need_a_name_a_body_and_a_unique_id() {
        let a = NoteTemplate {
            id: TemplateId::new(),
            name: "Standup".into(),
            body: "# {{title}}".into(),
        };
        let mut b = a.clone();
        b.id = TemplateId::new();
        validate_templates(&[a.clone(), b]).expect("two distinct templates are fine");

        let blank_name = NoteTemplate { name: String::new(), ..a.clone() };
        assert!(validate_templates(&[blank_name]).is_err());

        let blank_body = NoteTemplate { body: String::new(), ..a.clone() };
        assert!(validate_templates(&[blank_body]).is_err());

        let dup = a.clone();
        assert!(validate_templates(&[a, dup]).is_err());
    }
}
