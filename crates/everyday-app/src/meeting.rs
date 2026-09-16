//! The three commands the interface calls to record a meeting, and the
//! hooks later wiring uses to start or watch one without a press.
//!
//! Everything about *how* a call is heard is `capture.rs`; this is the
//! Tauri-facing glue around it, in the shape `commands.rs` uses for
//! everything else: a small function per verb, `State<'_, AppState>` for
//! what this process holds, and `crate::remote::Session::call` for the one
//! service command (`begin_recording`) this module makes directly. Every
//! chunk after that goes through `capture::RecordingSink`, over the same
//! `Session`, so a recording made against a remote vault sends its audio
//! exactly the way this module sends `begin_recording` itself.

use std::sync::{Arc, Mutex};

use base64::Engine;
use everyday_core::id::{CalendarId, EventId, RecordingId, TemplateId};
use everyday_core::meeting::Recording;
use everyday_service::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::{EventSink, MeetingOffer};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::capture::{
    self, CaptureHandle, CaptureSlot, CaptureStatus, RecordingHandle, RecordingSink,
};
use crate::remote::Session;
use crate::state::{AppState, SessionHandle};
use crate::tray::Tray;

/// The Tauri event carrying a `MeetingOfferPayload` -- what
/// `everyday_service::meeting::watch` raised as a [`MeetingOffer`], turned
/// into the shape `onMeetingOffer` in `ui/src/lib/api.ts` reads. Its other
/// half is that function.
pub const OFFER_EVENT: &str = "meeting-offer";

/// Mirrors the TS `MeetingOfferPayload`, field for field. `calendarId`,
/// `uid` and `series` ride along beside `eventId` for `dismissMeetingOffer`
/// to use -- see [`MeetingOffer`]'s own doc on why that command is keyed by
/// the triple that survives a feed resync rather than by `eventId`, which
/// does not, and `series`'s own doc on why `calendarId`/`uid` alone are not
/// enough for a recurring Google or Graph event.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OfferPayload {
    event_id: EventId,
    calendar_id: CalendarId,
    uid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    series: Option<String>,
    title: String,
    start: jiff::Timestamp,
    end: jiff::Timestamp,
    calendar_name: String,
    automatic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    recording_id: Option<RecordingId>,
}

impl From<MeetingOffer> for OfferPayload {
    fn from(offer: MeetingOffer) -> Self {
        Self {
            event_id: offer.event_id,
            calendar_id: offer.calendar_id,
            uid: offer.uid,
            series: offer.series,
            title: offer.title,
            start: offer.start,
            end: offer.end,
            calendar_name: offer.calendar_name,
            automatic: offer.automatic,
            recording_id: None,
        }
    }
}

/// What a [`MeetingOffer`] from the service becomes in this window.
///
/// For "Always" (`automatic: true`), the offer is not a question: the call
/// has already been recorded without asking on every other window watching
/// this vault, so this one starts capturing too -- through
/// [`start_automatic`], the same hook a press of "Take notes" goes through
/// -- and forwards the event so `meetings.svelte.ts`'s handler can show its
/// toast and refresh the recordings list. For an ordinary offer, nothing
/// starts on its own and the event is simply forwarded, for
/// `MeetingOfferBanner.svelte` to ask about.
///
/// `recordingId` is left empty either way: `start_automatic` begins the
/// microphone on its own spawned task rather than blocking this call on it,
/// and the one place in the interface that reads an automatic offer today
/// (`meetings.svelte.ts`'s own `onMeetingOffer`) does not need it -- it
/// re-polls `active_recording` instead, which is how the pill finds the
/// recording that resulted.
pub fn on_meeting_offer(app: &AppHandle, offer: MeetingOffer) {
    if starts_automatically(&offer) {
        start_automatic(app.clone(), offer.event_id);
    }
    let payload = OfferPayload::from(offer);
    if let Err(e) = app.emit(OFFER_EVENT, payload) {
        tracing::debug!(error = %e, "a meeting offer could not reach the interface");
    }
}

/// Whether an offer should start capture on its own, without a press --
/// the one condition [`on_meeting_offer`] checks before calling
/// [`start_automatic`]. Broken out so it is a plain, window-free fact to
/// assert against both an "Always" and an "Ask" offer, rather than
/// something only provable by driving a whole Tauri window.
fn starts_automatically(offer: &MeetingOffer) -> bool {
    offer.automatic
}

/// Begin recording, on the service and then the microphone. `eventId` ties
/// the recording to a calendar event (auto-stop watches its end);
/// `templateId` picks a note shape, defaulting the way
/// `MeetingSettings::template` does. Refuses if a recording is already
/// running, or starting -- one microphone, one call at a time.
///
/// The refusal itself happens inside [`begin_headless`], which reserves the
/// slot with [`CaptureSlot::Starting`] *before* the `begin_recording` await
/// -- see that function's own doc for why a check made here instead, with
/// nothing reserved until the await returns, would let two starts pressed
/// together both pass it.
#[tauri::command]
pub async fn meeting_start(
    app: AppHandle,
    event_id: Option<EventId>,
    title: Option<String>,
    template_id: Option<TemplateId>,
) -> CommandResult<Recording> {
    begin(app, event_id, title, template_id, false).await
}

/// Same as [`meeting_start`], but for a call detected on a calendar the
/// person has set to "Always" -- see `docs/plans/meeting-notes.md`'s
/// "'Always' mode". Not a command: nothing in the interface asks for this
/// by name; [`on_meeting_offer`] calls it directly when the service's
/// offer says `automatic`. Failures are logged rather than surfaced, the
/// way a routine's are: there is no dialog to put them in, and a call that
/// could not be recorded automatically is still a call the person can
/// record by hand from the notes app.
///
/// The `state_already_recording` peek below is only ever a cheap early
/// exit -- skip spawning a task, and the `begin_recording` round trip it
/// would make, when the slot is obviously taken already. It is not what
/// keeps two starts from racing; [`begin_headless`]'s own reservation is.
pub fn start_automatic(app: AppHandle, event_id: EventId) {
    tauri::async_runtime::spawn(async move {
        if state_already_recording(&app) {
            return;
        }
        if let Err(e) = begin(app, Some(event_id), None, None, true).await {
            tracing::warn!(error = %e, "automatic recording could not start");
        }
    });
}

fn state_already_recording(app: &AppHandle) -> bool {
    app.state::<AppState>().capture().lock().unwrap().is_some()
}

async fn begin(
    app: AppHandle,
    event_id: Option<EventId>,
    title: Option<String>,
    template_id: Option<TemplateId>,
    automatic: bool,
) -> CommandResult<Recording> {
    let state = app.state::<AppState>();
    let recording = begin_headless(
        state.session(),
        state.sink(),
        state.capture(),
        event_id,
        title,
        template_id,
        automatic,
        Some(app.clone()),
    )
    .await?;
    if let Some(tray) = app.try_state::<Tray>() {
        let _ = tray.set_recording(&app, Some(&recording.title));
    }
    Ok(recording)
}

/// The window-free core of [`begin`]: on the service and then the
/// microphone, exactly the same two steps and the same order, but taking
/// everything `begin` would otherwise read off an `AppHandle`'s `AppState`
/// as plain arguments instead -- a `SessionHandle` rather than a window to
/// fetch one from, `capture_slot` rather than `AppState::capture()`, and
/// `app` only for [`capture::start`]'s own optional status emission (see
/// its doc), never for a tray update, which is `begin`'s alone to do once
/// this returns `Ok`.
///
/// This is what makes an E2E harness with no Tauri window able to start a
/// recording through the identical path `meeting_start` and
/// `start_automatic` do: both ultimately call this, `begin` for the
/// former and, by way of `begin` itself, for the latter too.
///
/// # The slot is reserved before the first await, not after
///
/// The "already recording" refusal and the write that actually claims the
/// slot used to be two different moments: `meeting_start` checked
/// `capture_slot.lock().unwrap().is_some()` before calling this function at
/// all, and nothing wrote to the slot until [`capture::start`] returned --
/// on the far side of the `begin_recording` await below, a real network (or
/// at least IPC) round trip. Two starts pressed together (a person
/// double-clicking "Take notes", or a press racing an "Always" calendar's
/// own automatic start) could both read the slot as empty before either had
/// written to it, both go on to call `begin_recording` and open the
/// microphone, and whichever finished last would overwrite the slot, silently
/// dropping the first `CaptureHandle` -- and, before that handle grew a
/// `Drop` impl of its own (see `capture.rs`), leaking its capture thread
/// forever, still recording to a row nothing would ever call `finish` on.
///
/// The fix is to reserve the slot -- write [`CaptureSlot::Starting`] into
/// it -- *before* the `begin_recording` await, atomically with the
/// "already recording" check, both under the same lock acquisition below.
/// A second start racing this one now sees `Starting` rather than an empty
/// slot and is refused immediately, not after its own round trip. Every
/// path out of this function after that reservation -- success, a failed
/// `begin_recording`, a failed `capture::start` -- clears or replaces it in
/// [`finish_reservation`], so a refusal is never permanent.
#[allow(clippy::too_many_arguments)]
pub async fn begin_headless(
    session: SessionHandle,
    events: Option<Arc<dyn EventSink>>,
    capture_slot: &Mutex<Option<CaptureSlot>>,
    event_id: Option<EventId>,
    title: Option<String>,
    template_id: Option<TemplateId>,
    automatic: bool,
    app: Option<AppHandle>,
) -> CommandResult<Recording> {
    {
        let mut slot = capture_slot.lock().unwrap();
        if slot.is_some() {
            return Err(CommandError::new("already_running", "a recording is already in progress"));
        }
        *slot = Some(CaptureSlot::Starting);
    }

    let result = try_begin(session, events, event_id, title, template_id, automatic, app).await;
    finish_reservation(capture_slot, result)
}

/// Everything [`begin_headless`] does *after* reserving the slot: the
/// `begin_recording` call, then [`capture::start`]. Split out so the
/// reservation and its release -- [`finish_reservation`] -- are the only
/// things touching `capture_slot` directly, and every early return in here
/// (a failed service call, a failed `capture::start`) is just an ordinary
/// `?`/`Err` rather than something that also has to remember to clear a
/// lock.
async fn try_begin(
    session: SessionHandle,
    events: Option<Arc<dyn EventSink>>,
    event_id: Option<EventId>,
    title: Option<String>,
    template_id: Option<TemplateId>,
    automatic: bool,
    app: Option<AppHandle>,
) -> CommandResult<(Recording, CaptureHandle)> {
    let call_session = session.as_session();

    let value = call_session
        .call(
            Ctx::local(),
            "begin_recording",
            json!({ "eventId": event_id, "title": title, "templateId": template_id, "automatic": automatic }),
        )
        .await?;
    let recording: Recording = serde_json::from_value(value).map_err(|e| {
        CommandError::new("invalid", format!("begin_recording answered oddly: {e}"))
    })?;

    let auto_stop_quiet =
        if read_auto_stop(&call_session).await { Some(capture::AUTO_STOP_QUIET) } else { None };
    let sink_session = session.as_session();
    let call: Box<capture::CommandCaller> = Box::new(move |name, args| {
        tauri::async_runtime::block_on(sink_session.call(Ctx::local(), name, args))
    });
    let sink = Box::new(RecordingSink::new(recording.id, call));

    let handle = RecordingHandle {
        id: recording.id,
        title: recording.title.clone(),
        automatic,
        template_id: Some(recording.template_id),
        event_end: recording.event.as_ref().map(|e| e.end),
    };

    match capture::start(handle, sink, events, app, auto_stop_quiet) {
        Ok(capture_handle) => Ok((recording, capture_handle)),
        Err(e) => {
            // The record exists on the service but the microphone never
            // opened -- a machine with no input device at all, the one
            // failure worth checking for before spawning anything. Leaving
            // a `Recording` stuck at `Stage::Recording` with no audio ever
            // coming would be a row recovery has to find and explain;
            // discarding it here means the error the person sees is the
            // only trace it left.
            let _ = call_session
                .call(Ctx::local(), "discard_recording", json!({ "id": recording.id }))
                .await;
            Err(e.into())
        }
    }
}

/// Release [`begin_headless`]'s reservation: on success, replace
/// `Starting` with the real [`CaptureSlot::Recording`]; on failure, clear
/// the slot back to empty so the refusal a racing start saw was only ever
/// "someone else is starting right now", never permanent.
fn finish_reservation(
    capture_slot: &Mutex<Option<CaptureSlot>>,
    result: CommandResult<(Recording, CaptureHandle)>,
) -> CommandResult<Recording> {
    match result {
        Ok((recording, capture_handle)) => {
            *capture_slot.lock().unwrap() = Some(CaptureSlot::Recording(capture_handle));
            Ok(recording)
        }
        Err(e) => {
            *capture_slot.lock().unwrap() = None;
            Err(e)
        }
    }
}

/// `MeetingSettings::auto_stop`, by way of a service command the shell does
/// not otherwise know about. Defaults to on -- [`everyday_core::meeting::MeetingSettings::default`]'s
/// own default -- if the command fails or does not exist yet; the shell
/// only reads this once, at the start of a recording, so an outage here
/// costs one recording's auto-stop rather than turning the setting off
/// silently for good.
async fn read_auto_stop(session: &Session) -> bool {
    match session.call(Ctx::local(), "meeting_settings", json!({})).await {
        Ok(value) => value
            .get("settings")
            .and_then(|s| s.get("autoStop"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// Stop recording. `discard` throws the audio and the record away instead
/// of finishing the note pipeline -- the notes app's "discard this
/// recording" action, not the ordinary "stop and write the note" one.
#[tauri::command]
pub async fn meeting_stop(app: AppHandle, discard: bool) -> CommandResult<()> {
    stop(&app, discard).await
}

/// The tray's "Stop recording" item. Not a command -- see `tray.rs`'s
/// `on_menu_event`, which calls this directly the way it does `QUIT_ID` --
/// but the same stop underneath, always without discarding: a tray click is
/// "I'm done", never "throw this away".
pub fn stop_from_tray(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = stop(&app, false).await {
            tracing::warn!(error = %e, "could not stop recording from the tray");
        }
    });
}

async fn stop(app: &AppHandle, discard: bool) -> CommandResult<()> {
    let state = app.state::<AppState>();
    // A slot still `CaptureSlot::Starting` (a start's `begin_recording`
    // round trip has not returned yet) has no `CaptureHandle` to stop --
    // leave the reservation alone rather than clearing it, so the start
    // still in flight lands normally instead of racing a fresh start in
    // underneath it. This window is one IPC round trip long; a stop
    // pressed inside it is rare enough that "the start finishes, then this
    // stop is simply a no-op" costs nothing a person would notice.
    let handle = {
        let mut slot = state.capture().lock().unwrap();
        if matches!(*slot, Some(CaptureSlot::Recording(_))) { slot.take() } else { None }
    }
    .and_then(|slot| match slot {
        CaptureSlot::Recording(handle) => Some(handle),
        CaptureSlot::Starting => None,
    });
    let Some(handle) = handle else { return Ok(()) };
    if let Some(tray) = app.try_state::<Tray>() {
        let _ = tray.set_recording(app, None);
    }
    // Joins the capture thread, which runs the sink's `finish`/`discard` to
    // completion -- disk and, for a remote vault, network, so this belongs
    // off the async runtime the way every other command that touches the
    // vault does.
    everyday_service::service::blocking(move || {
        handle.stop(discard);
        Ok(())
    })
    .await
}

/// What the notes app polls, and what `meeting-status` repeats every
/// quarter second while a recording runs. `None` means nothing is being
/// recorded right now.
#[tauri::command]
pub async fn meeting_status(state: State<'_, AppState>) -> CommandResult<Option<CaptureStatus>> {
    Ok(state
        .capture()
        .lock()
        .unwrap()
        .as_ref()
        .and_then(CaptureSlot::handle)
        .and_then(|h| h.status()))
}

/// Record the microphone for `seconds` and turn it into a voiceprint.
///
/// Native, like the three commands above: the webview has no microphone
/// permission. `capture::record_mic_only` is the mic-only half of the same
/// capture engine `meeting_start` uses -- no system track, so whoever else
/// is on a call at the time is not folded into the sample -- and this
/// command is the whole of the glue between that and `enrol_voice`, the
/// service command the pipeline agent's work adds. If that command does not
/// exist yet in this build, its rejection is returned exactly as the
/// service raised it; `meetings.svelte.ts`'s `enrolVoice` already turns
/// *any* failure of this Tauri command into one plain sentence for the
/// person, so there is nothing this needs to do to soften it.
#[tauri::command]
pub async fn voice_enrol(
    state: State<'_, AppState>,
    seconds: u32,
) -> CommandResult<everyday_service::domains::meetings::VoiceprintInfo> {
    let session = state.session().as_session();
    let pcm = everyday_service::service::blocking(move || {
        capture::record_mic_only(seconds).map_err(CommandError::from)
    })
    .await?;

    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let pcm_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);

    let value = session.call(Ctx::local(), "enrol_voice", json!({ "pcm": pcm_b64 })).await?;
    serde_json::from_value(value)
        .map_err(|e| CommandError::new("invalid", format!("enrol_voice answered oddly: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(automatic: bool) -> MeetingOffer {
        MeetingOffer {
            event_id: EventId::new(),
            calendar_id: CalendarId::new(),
            uid: "evt-1@example.com".into(),
            series: None,
            title: "Design sync".into(),
            start: jiff::Timestamp::now(),
            end: jiff::Timestamp::now(),
            calendar_name: "Work".into(),
            automatic,
        }
    }

    #[test]
    fn an_always_offer_starts_automatically() {
        assert!(starts_automatically(&offer(true)));
    }

    #[test]
    fn an_ask_offer_does_not_start_on_its_own() {
        assert!(!starts_automatically(&offer(false)));
    }

    // ---- the reservation race -------------------------------------------

    /// A local service and vault, meeting notes left off -- the same
    /// pattern `everyday-service`'s own `spool.rs` tests use, and for the
    /// same reason (see that module's own `env` doc): `begin_recording`
    /// against this vault refuses for a plain, predictable reason ("turned
    /// off") without needing a real transcriber on disk. That refusal is
    /// not what this test is about; it only needs `begin_recording` to be a
    /// real service call worth racing two starts against.
    fn local_session() -> (SessionHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            dir.path(),
            everyday_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let service = Arc::new(everyday_service::Service::new());
        service.set(vault);
        (SessionHandle::Local(service), dir)
    }

    /// The regression this whole finding is about: two starts pressed
    /// together must not both pass the "already recording" check. Before
    /// `begin_headless` reserved its slot ahead of the `begin_recording`
    /// await, both of these could observe an empty `capture_slot` and both
    /// go on to call `begin_recording` -- see this function's own doc.
    /// `tokio::join!` polls both futures on the same task, so each runs its
    /// synchronous prefix (the reservation) before either can be pre-empted
    /// by real parallelism; that is enough to prove the property, because
    /// the guarantee comes from the `Mutex` never being released between
    /// the check and the write, not from timing.
    #[tokio::test]
    async fn two_starts_pressed_together_only_let_one_through() {
        let (session, _dir) = local_session();
        let capture: Mutex<Option<CaptureSlot>> = Mutex::new(None);

        let (a, b) = tokio::join!(
            begin_headless(session.clone(), None, &capture, None, None, None, false, None),
            begin_headless(session.clone(), None, &capture, None, None, None, false, None),
        );

        let codes: Vec<String> =
            [&a, &b].into_iter().map(|r| r.as_ref().unwrap_err().code.clone()).collect();
        assert_eq!(
            codes.iter().filter(|c| c.as_str() == "already_running").count(),
            1,
            "exactly one of two simultaneous starts must be refused as already running; got {codes:?}"
        );
        // The one that was not refused for racing still refuses -- this
        // vault has meeting notes turned off -- but for a different
        // reason, proving it genuinely reached `begin_recording` rather
        // than being refused twice for the same thing.
        assert!(
            codes.iter().any(|c| c.as_str() != "already_running"),
            "the start that reserved the slot should fail for the vault's own reason, not \
             the race guard; got {codes:?}"
        );

        // Nothing is left reserved: both branches clear the slot on
        // failure (`finish_reservation`), so a third start is never
        // blocked by this race's own bookkeeping.
        assert!(capture.lock().unwrap().is_none());
    }
}
