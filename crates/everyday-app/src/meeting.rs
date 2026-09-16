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

use everyday_core::id::{EventId, TemplateId};
use everyday_core::meeting::Recording;
use everyday_service::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, State};

use crate::capture::{self, CaptureStatus, RecordingHandle, RecordingSink};
use crate::remote::Session;
use crate::state::AppState;
use crate::tray::Tray;

/// Begin recording, on the service and then the microphone. `eventId` ties
/// the recording to a calendar event (auto-stop watches its end);
/// `templateId` picks a note shape, defaulting the way
/// `MeetingSettings::template` does. Refuses if a recording is already
/// running -- one microphone, one call at a time.
#[tauri::command]
pub async fn meeting_start(
    app: AppHandle,
    state: State<'_, AppState>,
    event_id: Option<EventId>,
    title: Option<String>,
    template_id: Option<TemplateId>,
) -> CommandResult<Recording> {
    if state.capture().lock().unwrap().is_some() {
        return Err(CommandError::new("already_running", "a recording is already in progress"));
    }
    begin(app, event_id, title, template_id, false).await
}

/// Same as [`meeting_start`], but for a call detected on a calendar the
/// person has set to "Always" -- see `docs/plans/meeting-notes.md`'s
/// "'Always' mode". Not a command: nothing in the interface asks for this
/// by name, the watcher (not yet wired) will call it directly. Failures are
/// logged rather than surfaced, the way a routine's are: there is no dialog
/// to put them in, and a call that could not be recorded automatically is
/// still a call the person can record by hand from the notes app.
///
/// A hook, not yet called from anywhere in this crate -- `everyday_service`
/// has no watcher wired to a calendar's "Always" setting yet. Kept `pub`
/// and unused rather than deleted, since the shape (`AppHandle`, the one
/// event to record) is the contract the watcher will be built against.
#[allow(dead_code)]
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
    let session = state.session().as_session();

    let value = session
        .call(
            Ctx::local(),
            "begin_recording",
            json!({ "eventId": event_id, "title": title, "templateId": template_id, "automatic": automatic }),
        )
        .await?;
    let recording: Recording = serde_json::from_value(value).map_err(|e| {
        CommandError::new("invalid", format!("begin_recording answered oddly: {e}"))
    })?;

    let auto_stop_enabled = read_auto_stop(&session).await;
    let events = state.sink();
    let call_session = state.session().as_session();
    let call: Box<capture::CommandCaller> = Box::new(move |name, args| {
        tauri::async_runtime::block_on(call_session.call(Ctx::local(), name, args))
    });
    let sink = Box::new(RecordingSink::new(recording.id, call));

    let handle = RecordingHandle {
        id: recording.id,
        title: recording.title.clone(),
        automatic,
        template_id: Some(recording.template_id),
        event_end: recording.event.as_ref().map(|e| e.end),
    };

    match capture::start(handle, sink, events, app.clone(), auto_stop_enabled) {
        Ok(capture_handle) => {
            *state.capture().lock().unwrap() = Some(capture_handle);
            if let Some(tray) = app.try_state::<Tray>() {
                let _ = tray.set_recording(&app, Some(&recording.title));
            }
            Ok(recording)
        }
        Err(e) => {
            // The record exists on the service but the microphone never
            // opened -- a machine with no input device at all, the one
            // failure worth checking for before spawning anything. Leaving
            // a `Recording` stuck at `Stage::Recording` with no audio ever
            // coming would be a row recovery has to find and explain;
            // discarding it here means the error the person sees is the
            // only trace it left.
            let _ = session
                .call(Ctx::local(), "discard_recording", json!({ "id": recording.id }))
                .await;
            Err(e.into())
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
    let handle = state.capture().lock().unwrap().take();
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
    Ok(state.capture().lock().unwrap().as_ref().and_then(|h| h.status()))
}
