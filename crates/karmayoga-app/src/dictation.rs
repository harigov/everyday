//! Dictating from this computer: the microphone, and nothing else.
//!
//! The desktop's transport for [`karmayoga_service::speech::dictate`]: the
//! default microphone on one side, a session on the other, and this module
//! between them. It is `voice` without the speakers -- `dictation_start` opens
//! a session and a pump task that carries what is heard to the window, and
//! `dictation_stop` and `dictation_cancel` are the window's controls. See
//! `docs/plans/listen.md`, "The desktop commands".
//!
//! There is one microphone, so dictation, voice mode and meeting recording
//! refuse each other, and only one dictation runs at a time. Anything being
//! listened to is stopped first: there is no echo cancellation here, and the
//! microphone would hear the speakers. A window looking at another computer's
//! vault has no dictation: its microphone is not the vault's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use karmayoga_service::error::{CommandError, CommandResult, codes};
use karmayoga_service::speech::dictate::{
    DictationEnd, DictationEvent, DictationInput, DictationLink,
};
use tauri::{AppHandle, Manager, State};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
use crate::capture::CaptureSlot;
use crate::state::AppState;
use crate::voice::VoiceHandle;

/// What `AppState` holds while someone is dictating, or a dictation is being
/// started.
///
/// Reserved with [`DictationHandle::Starting`] before the first await of
/// `dictation_start`, so two starts racing each other cannot both see an empty
/// slot -- the same reason `voice` and `meeting.rs` reserve theirs.
pub enum DictationHandle {
    /// A start is in flight: the session is being opened. Anything the window
    /// says meanwhile is held, and applied the moment there is a session.
    Starting(Held),
    Running(Running),
}

/// What the window said while a session was still being opened.
#[derive(Default)]
pub struct Held {
    /// The window that asked, whose closing ends the session.
    window: String,
    stop: bool,
    cancel: bool,
}

/// A session under way.
pub struct Running {
    /// The window that started it. See [`window_closing`].
    window: String,
    /// Which session this is, so the pump that ends it clears only its own
    /// slot and never that of a dictation started since.
    id: u64,
    input: UnboundedSender<DictationInput>,
    engine: Arc<AudioEngine>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl Held {
    fn hold(&mut self, message: DictationInput) {
        match message {
            DictationInput::Stop => self.stop = true,
            DictationInput::Cancel => self.cancel = true,
            DictationInput::Audio(_)
            | DictationInput::Backlog(_)
            | DictationInput::Failed(_)
            | DictationInput::EndAfterPause(_) => {}
        }
    }

    /// Say it to the session, now that there is one. Throwing the words away
    /// wins over keeping them.
    fn deliver(self, input: &UnboundedSender<DictationInput>) {
        if self.cancel {
            let _ = input.send(DictationInput::Cancel);
        } else if self.stop {
            let _ = input.send(DictationInput::Stop);
        }
    }
}

/// Claim the one dictation slot, or say why it cannot be had.
///
/// Lock order is capture, voice, dictation, as in `voice::reserve`.
fn reserve(
    dictation: &Mutex<Option<DictationHandle>>,
    capture: &Mutex<Option<CaptureSlot>>,
    voice: &Mutex<Option<VoiceHandle>>,
    window: &str,
) -> CommandResult<()> {
    let mut recording = capture.lock().unwrap();
    // A recording that ended on its own may not have cleared its slot yet.
    crate::meeting::reclaim_if_ended(&mut recording);
    if recording.is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "A meeting is being recorded, and it has the microphone. Stop recording to dictate.",
        ));
    }
    if voice.lock().unwrap().is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "You are talking to the assistant, which has the microphone. End the conversation to dictate.",
        ));
    }
    let mut slot = dictation.lock().unwrap();
    if slot.is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "You are already dictating, or the last dictation has not quite ended.",
        ));
    }
    *slot = Some(DictationHandle::Starting(Held { window: window.to_string(), ..Held::default() }));
    Ok(())
}

/// Hand `message` to whatever is on the other end of the slot: the session, if
/// there is one; the start in flight, to be delivered when it lands; nothing,
/// if nothing is being dictated.
fn send_or_hold(dictation: &Mutex<Option<DictationHandle>>, message: DictationInput) {
    match dictation.lock().unwrap().as_mut() {
        Some(DictationHandle::Running(running)) => {
            let _ = running.input.send(message);
        }
        Some(DictationHandle::Starting(held)) => held.hold(message),
        None => {}
    }
}

/// Empty the slot if it still holds session `id`.
fn release(dictation: &Mutex<Option<DictationHandle>>, id: u64) {
    let old = {
        let mut slot = dictation.lock().unwrap();
        let ours = matches!(&*slot, Some(DictationHandle::Running(running)) if running.id == id);
        if ours { slot.take() } else { None }
    };
    // Dropped outside the lock: the last reference to the engine goes with it.
    drop(old);
}

/// Claim the one dictation slot for voice typing ([`crate::voicetyping`]),
/// which dictates into other applications rather than into a window here but
/// has the same one microphone: while it holds the slot, voice mode, meeting
/// recording, listening and a dictation here are refused as they are for any
/// dictation, and it is refused while any of them is running. Held as a start
/// under `window`, the pill's label, until [`give_back`] -- no session of this
/// module's is ever made from it.
pub(crate) fn claim_for(state: &AppState, window: &str) -> CommandResult<()> {
    reserve(state.dictation(), state.capture(), state.voice(), window)
}

/// Give back what [`claim_for`] took.
pub(crate) fn give_back(state: &AppState) {
    abandon(state.dictation());
}

/// Empty the slot of a start that failed.
fn abandon(dictation: &Mutex<Option<DictationHandle>>) {
    let old = {
        let mut slot = dictation.lock().unwrap();
        let starting = matches!(&*slot, Some(DictationHandle::Starting(_)));
        if starting { slot.take() } else { None }
    };
    drop(old);
}

/// Start dictating: the microphone goes on, and `channel` hears the words so
/// far, and how it ended, until the session is over.
///
/// Refuses when the window is looking at a vault on another computer, when a
/// meeting is being recorded or a conversation is under way (one microphone),
/// and when there is a dictation already. Anything being listened to stops.
#[tauri::command]
pub async fn dictation_start(
    app: AppHandle,
    webview: tauri::Webview,
    state: State<'_, AppState>,
    channel: tauri::ipc::Channel<DictationEvent>,
) -> CommandResult<()> {
    if state.is_remote() {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            "Dictating uses this computer's microphone, so it is not available while this window is looking at a vault on another computer.",
        ));
    }
    reserve(state.dictation(), state.capture(), state.voice(), webview.label())?;
    // The microphone would hear the speakers.
    crate::listen::stop_listening(&state);

    match try_start(&app, &state, channel).await {
        Ok(()) => Ok(()),
        Err(e) => {
            abandon(state.dictation());
            Err(e)
        }
    }
}

/// Everything `dictation_start` does after reserving the slot, so that every
/// early return in it is an ordinary `?` and the slot's release is in one
/// place.
async fn try_start(
    app: &AppHandle,
    state: &State<'_, AppState>,
    channel: tauri::ipc::Channel<DictationEvent>,
) -> CommandResult<()> {
    let DictationLink { input, events } = state.service().dictation_open().await?;

    // What the microphone hears goes to the session, and the microphone
    // failing is told to it, which ends it with the reason.
    let sink: EngineSink = {
        let input = input.clone();
        Arc::new(move |event| match event {
            EngineEvent::Microphone(audio) => {
                let _ = input.send(DictationInput::Audio(audio));
            }
            EngineEvent::Failed(message) => {
                let _ = input.send(DictationInput::Failed(message));
            }
            EngineEvent::Played(_) => {}
        })
    };
    let options = EngineOptions { microphone: true, speaker: false, echo_cancellation: false };
    let engine = match AudioEngine::start(options, sink).await {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            // The session is already running; let it go.
            let _ = input.send(DictationInput::Cancel);
            return Err(e.into());
        }
    };

    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    // In the slot before the pump exists, so a session that ends at once finds
    // its own slot to clear.
    let held = {
        let mut slot = state.dictation().lock().unwrap();
        let held = match slot.take() {
            Some(DictationHandle::Starting(held)) => held,
            _ => Held::default(),
        };
        *slot = Some(DictationHandle::Running(Running {
            id,
            window: held.window.clone(),
            input: input.clone(),
            engine: engine.clone(),
        }));
        held
    };
    let stopped_early = held.stop || held.cancel;
    held.deliver(&input);
    if stopped_early {
        engine.stop();
    }

    tauri::async_runtime::spawn(pump(app.clone(), id, events, input, engine, channel));
    Ok(())
}

/// Carry a session's events to the window, until it ends.
///
/// Never blocks. The microphone is closed, and the slot cleared, when the
/// session says it has ended -- or has closed its events without saying so,
/// which is reported to the window as a failure of its own. A window that has
/// gone cancels the session: there is nobody to hand the words to.
async fn pump(
    app: AppHandle,
    id: u64,
    mut events: UnboundedReceiver<DictationEvent>,
    input: UnboundedSender<DictationInput>,
    engine: Arc<AudioEngine>,
    channel: tauri::ipc::Channel<DictationEvent>,
) {
    let mut window_gone = false;
    let mut ended = false;
    while let Some(event) = events.recv().await {
        let last = matches!(event, DictationEvent::Ended { .. });
        if channel.send(event).is_err() && !window_gone {
            window_gone = true;
            let _ = input.send(DictationInput::Cancel);
            engine.stop();
        }
        if last {
            ended = true;
            break;
        }
    }
    if !ended {
        let _ = channel.send(DictationEvent::Ended {
            reason: DictationEnd::Failed,
            text: String::new(),
            error: Some("Dictation stopped unexpectedly.".to_string()),
        });
    }
    engine.stop();
    release(app.state::<AppState>().dictation(), id);
}

/// Tell the session, and let go of the microphone now: no more audio is
/// needed, and the session waits for the words it has already heard.
fn finish_now(dictation: &Mutex<Option<DictationHandle>>, message: DictationInput) {
    let engine = match dictation.lock().unwrap().as_ref() {
        Some(DictationHandle::Running(running)) => Some(running.engine.clone()),
        _ => None,
    };
    send_or_hold(dictation, message);
    if let Some(engine) = engine {
        engine.stop();
    }
}

/// Stop listening and keep the words: the session finishes with what it has
/// heard, which follows on the channel as `Ended`. Nothing happens if nothing
/// is being dictated.
#[tauri::command]
pub async fn dictation_stop(state: State<'_, AppState>) -> CommandResult<()> {
    finish_now(state.dictation(), DictationInput::Stop);
    Ok(())
}

/// Stop listening and throw the words away.
#[tauri::command]
pub async fn dictation_cancel(state: State<'_, AppState>) -> CommandResult<()> {
    finish_now(state.dictation(), DictationInput::Cancel);
    Ok(())
}

/// A window is closing. Closing the main one only hides it, and a microphone
/// must not stay open behind a window nobody can see, so the dictation it
/// started is cancelled here: its words have nowhere to go.
pub fn window_closing(app: &AppHandle, label: &str) {
    let state = app.state::<AppState>();
    let ours = match state.dictation().lock().unwrap().as_ref() {
        Some(DictationHandle::Running(running)) => running.window == label,
        Some(DictationHandle::Starting(held)) => held.window == label,
        None => false,
    };
    if ours {
        finish_now(state.dictation(), DictationInput::Cancel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Mutex<Option<DictationHandle>> {
        Mutex::new(None)
    }

    fn no_capture() -> Mutex<Option<CaptureSlot>> {
        Mutex::new(None)
    }

    fn no_voice() -> Mutex<Option<VoiceHandle>> {
        Mutex::new(None)
    }

    #[test]
    fn two_starts_cannot_both_have_the_slot() {
        let dictation = slot();
        reserve(&dictation, &no_capture(), &no_voice(), "main").expect("the first start gets it");
        let second = reserve(&dictation, &no_capture(), &no_voice(), "main")
            .expect_err("the second is turned away");
        assert_eq!(second.code, codes::BUSY);
        assert!(second.message.contains("already dictating"));
    }

    #[test]
    fn a_meeting_being_recorded_keeps_dictation_out() {
        let dictation = slot();
        let capture = Mutex::new(Some(CaptureSlot::Starting));
        let refused = reserve(&dictation, &capture, &no_voice(), "main")
            .expect_err("one microphone, one user");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("meeting"));
        assert!(dictation.lock().unwrap().is_none(), "nothing was reserved");
    }

    #[test]
    fn a_conversation_keeps_dictation_out() {
        let dictation = slot();
        let voice = Mutex::new(Some(VoiceHandle::Starting(crate::voice::Held::default())));
        let refused = reserve(&dictation, &no_capture(), &voice, "main")
            .expect_err("voice has the microphone");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("talking to the assistant"));
        assert!(dictation.lock().unwrap().is_none(), "nothing was reserved");
    }

    #[test]
    fn a_failed_start_frees_the_slot_for_the_next() {
        let dictation = slot();
        reserve(&dictation, &no_capture(), &no_voice(), "main").unwrap();
        abandon(&dictation);
        reserve(&dictation, &no_capture(), &no_voice(), "main").expect("free again");
    }

    #[test]
    fn what_the_window_says_during_a_start_is_kept_and_delivered() {
        let dictation = slot();
        reserve(&dictation, &no_capture(), &no_voice(), "main").unwrap();
        send_or_hold(&dictation, DictationInput::Stop);

        let Some(DictationHandle::Starting(held)) = dictation.lock().unwrap().take() else {
            panic!("still starting");
        };
        assert_eq!(held.window, "main");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        held.deliver(&tx);
        assert!(matches!(rx.try_recv(), Ok(DictationInput::Stop)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_cancel_during_a_start_wins_over_a_stop() {
        let dictation = slot();
        reserve(&dictation, &no_capture(), &no_voice(), "main").unwrap();
        send_or_hold(&dictation, DictationInput::Cancel);
        send_or_hold(&dictation, DictationInput::Stop);
        // Audio and failures mean nothing before there is a session.
        send_or_hold(&dictation, DictationInput::Audio(vec![0; 4]));

        let Some(DictationHandle::Starting(held)) = dictation.lock().unwrap().take() else {
            panic!("still starting");
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        held.deliver(&tx);
        assert!(matches!(rx.try_recv(), Ok(DictationInput::Cancel)));
        assert!(rx.try_recv().is_err(), "the words are thrown away, so nothing else is said");
    }

    #[test]
    fn saying_anything_with_nothing_running_is_harmless() {
        let dictation = slot();
        finish_now(&dictation, DictationInput::Stop);
        finish_now(&dictation, DictationInput::Cancel);
        assert!(dictation.lock().unwrap().is_none());
    }

    #[test]
    fn stopping_a_running_session_tells_it_and_leaves_the_slot_to_the_pump() {
        let dictation = slot();
        let (input, mut heard) = tokio::sync::mpsc::unbounded_channel();
        *dictation.lock().unwrap() = Some(DictationHandle::Running(Running {
            id: 1,
            window: "main".into(),
            input,
            engine: Arc::new(AudioEngine::detached()),
        }));
        finish_now(&dictation, DictationInput::Stop);
        assert!(matches!(heard.try_recv(), Ok(DictationInput::Stop)));
        assert!(dictation.lock().unwrap().is_some(), "free once the session has ended");
    }

    #[test]
    fn a_session_clears_only_its_own_slot() {
        let dictation = slot();
        let (input, _output) = tokio::sync::mpsc::unbounded_channel();
        // A dictation that has been started since the one that is ending.
        *dictation.lock().unwrap() = Some(DictationHandle::Running(Running {
            id: 2,
            window: String::new(),
            input,
            engine: Arc::new(AudioEngine::detached()),
        }));
        release(&dictation, 1);
        assert!(
            dictation.lock().unwrap().is_some(),
            "session 1 had no business with session 2's slot"
        );
        release(&dictation, 2);
        assert!(dictation.lock().unwrap().is_none());
    }
}
