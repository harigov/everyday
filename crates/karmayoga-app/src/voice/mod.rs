//! Talking to the assistant from this computer.
//!
//! The desktop's transport for [`karmayoga_service::voice`]: the default
//! microphone and speakers on one side, a session on the other, and this module
//! between them. `crate::audio` owns the devices (and the echo canceller, which
//! needs to know what was actually played), `voice_start` opens a session and a
//! pump task that carries its output to them and to the window, and the other
//! three commands are the window's controls. See `docs/plans/voice.md`, "The desktop
//! transport".
//!
//! Voice, dictation and meeting recording refuse each other -- there is one
//! microphone -- and a conversation has the speakers too, so it stops anything
//! being listened to before it opens them (`crate::listen`). A window looking
//! at another computer's vault has no voice mode: its microphone is not the
//! vault's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use karmayoga_core::ConversationId;
use karmayoga_core::agent::onscreen::OnScreen;
use karmayoga_service::error::{CommandError, CommandResult, codes};
use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceOutput};
use tauri::{AppHandle, Manager, State};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
use crate::capture::CaptureSlot;
use crate::dictation::DictationHandle;
use crate::state::AppState;

/// What `AppState` holds while a conversation is being talked, or being
/// started.
///
/// Reserved with [`VoiceHandle::Starting`] before the first await of
/// `voice_start`, so two starts racing each other cannot both see an empty
/// slot -- the same reason `meeting.rs` reserves `CaptureSlot::Starting`.
pub enum VoiceHandle {
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
    muted: Option<bool>,
    screen: Option<Option<OnScreen>>,
    stop: bool,
}

/// A session under way.
pub struct Running {
    /// The window that started it. See [`window_closing`].
    window: String,
    /// Which session this is, so the pump that ends it clears only its own
    /// slot and never that of a conversation started since.
    id: u64,
    input: UnboundedSender<VoiceInput>,
    engine: Arc<AudioEngine>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl Held {
    fn hold(&mut self, message: VoiceInput) {
        match message {
            VoiceInput::Muted(muted) => self.muted = Some(muted),
            VoiceInput::Screen(screen) => self.screen = Some(screen),
            VoiceInput::Stop => self.stop = true,
            // The desktop never asks the session to say something of its own.
            VoiceInput::Audio(_) | VoiceInput::Played { .. } | VoiceInput::Say(_) => {}
        }
    }

    /// Say it all to the session, now that there is one.
    fn deliver(self, input: &UnboundedSender<VoiceInput>) {
        if let Some(muted) = self.muted {
            let _ = input.send(VoiceInput::Muted(muted));
        }
        if let Some(screen) = self.screen {
            let _ = input.send(VoiceInput::Screen(screen));
        }
        if self.stop {
            let _ = input.send(VoiceInput::Stop);
        }
    }
}

/// Claim the one voice slot, or say why it cannot be had.
fn reserve(
    voice: &Mutex<Option<VoiceHandle>>,
    capture: &Mutex<Option<CaptureSlot>>,
    dictation: &Mutex<Option<DictationHandle>>,
) -> CommandResult<()> {
    let mut recording = capture.lock().unwrap();
    // A recording that ended on its own may not have cleared its slot yet.
    crate::meeting::reclaim_if_ended(&mut recording);
    if recording.is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "A meeting is being recorded, and it has the microphone. Stop recording to talk to the assistant.",
        ));
    }
    if dictation.lock().unwrap().is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "You are dictating, and it has the microphone. Finish dictating to talk to the assistant.",
        ));
    }
    let mut slot = voice.lock().unwrap();
    if slot.is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "You are already talking to the assistant, or the last conversation has not quite ended.",
        ));
    }
    *slot = Some(VoiceHandle::Starting(Held::default()));
    Ok(())
}

/// Hand `message` to whatever is on the other end of the slot: the session, if
/// there is one; the start in flight, to be delivered when it lands; nothing,
/// if nothing is being talked.
fn send_or_hold(voice: &Mutex<Option<VoiceHandle>>, message: VoiceInput) {
    match voice.lock().unwrap().as_mut() {
        Some(VoiceHandle::Running(running)) => {
            let _ = running.input.send(message);
        }
        Some(VoiceHandle::Starting(held)) => held.hold(message),
        None => {}
    }
}

/// Empty the slot if it still holds session `id`.
fn release(voice: &Mutex<Option<VoiceHandle>>, id: u64) {
    let old = {
        let mut slot = voice.lock().unwrap();
        let ours = matches!(&*slot, Some(VoiceHandle::Running(running)) if running.id == id);
        if ours { slot.take() } else { None }
    };
    // Dropped outside the lock: the last reference to the engine goes with it.
    drop(old);
}

/// Empty the slot of a start that failed.
fn abandon(voice: &Mutex<Option<VoiceHandle>>) {
    let old = {
        let mut slot = voice.lock().unwrap();
        let starting = matches!(&*slot, Some(VoiceHandle::Starting(_)));
        if starting { slot.take() } else { None }
    };
    drop(old);
}

/// Start talking in a conversation: the microphone and speakers go on, and
/// `channel` hears everything the session says until it ends.
///
/// Refuses when the window is looking at a vault on another computer, when a
/// meeting is being recorded, when someone is dictating, and when there is a
/// conversation already. Anything being listened to is stopped.
#[tauri::command]
pub async fn voice_start(
    app: AppHandle,
    webview: tauri::Webview,
    state: State<'_, AppState>,
    conversation_id: ConversationId,
    on_screen: Option<serde_json::Value>,
    channel: tauri::ipc::Channel<VoiceEvent>,
) -> CommandResult<()> {
    if state.is_remote() {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            "Talking to the assistant uses this computer's microphone and speakers, so it is not available while this window is looking at a vault on another computer.",
        ));
    }
    reserve(state.voice(), state.capture(), state.dictation())?;
    if let Some(VoiceHandle::Starting(held)) = state.voice().lock().unwrap().as_mut() {
        held.window = webview.label().to_string();
    }
    // The speakers are this conversation's now, and its microphone would
    // otherwise hear whatever was being read.
    crate::listen::stop_listening(&state);

    match try_start(&app, &state, conversation_id, on_screen, channel).await {
        Ok(()) => Ok(()),
        Err(e) => {
            abandon(state.voice());
            Err(e)
        }
    }
}

/// Everything `voice_start` does after reserving the slot, so that every early
/// return in it is an ordinary `?` and the slot's release is in one place.
async fn try_start(
    app: &AppHandle,
    state: &State<'_, AppState>,
    conversation_id: ConversationId,
    on_screen: Option<serde_json::Value>,
    channel: tauri::ipc::Channel<VoiceEvent>,
) -> CommandResult<()> {
    let link = state.service().voice_open(conversation_id).await?;
    let karmayoga_service::voice::VoiceLink { input, output } = link;

    let _ = input.send(VoiceInput::Screen(on_screen.as_ref().and_then(OnScreen::lenient)));

    // What the devices report goes to the session, and a device failing is
    // told to the window and ends the session.
    let sink: EngineSink = {
        let channel = channel.clone();
        let input = input.clone();
        Arc::new(move |event| match event {
            EngineEvent::Microphone(audio) => {
                let _ = input.send(VoiceInput::Audio(audio));
            }
            EngineEvent::Played(utterance) => {
                let _ = input.send(VoiceInput::Played { utterance });
            }
            EngineEvent::Failed(message) => {
                let _ = channel.send(VoiceEvent::Trouble { message });
                let _ = input.send(VoiceInput::Stop);
            }
        })
    };
    let options = EngineOptions { microphone: true, speaker: true, echo_cancellation: true };
    let engine = match AudioEngine::start(options, sink).await {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            // The session is already running; let it go.
            let _ = input.send(VoiceInput::Stop);
            return Err(e.into());
        }
    };

    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    // In the slot before the pump exists, so a session that ends at once finds
    // its own slot to clear.
    let held = {
        let mut slot = state.voice().lock().unwrap();
        let held = match slot.take() {
            Some(VoiceHandle::Starting(held)) => held,
            _ => Held::default(),
        };
        *slot = Some(VoiceHandle::Running(Running {
            id,
            window: held.window.clone(),
            input: input.clone(),
            engine: engine.clone(),
        }));
        held
    };
    held.deliver(&input);

    tauri::async_runtime::spawn(pump(app.clone(), id, output, input, engine, channel));
    Ok(())
}

/// Carry a session's output to the speakers and to the window, until it ends.
///
/// Never blocks: the speakers are told through the engine's queue, the window
/// through the channel. The audio devices are closed, and the slot cleared,
/// when the session says it has ended -- or has closed its output without
/// saying so, which is reported to the window as an ending of its own.
async fn pump(
    app: AppHandle,
    id: u64,
    mut output: UnboundedReceiver<VoiceOutput>,
    input: UnboundedSender<VoiceInput>,
    engine: Arc<AudioEngine>,
    channel: tauri::ipc::Channel<VoiceEvent>,
) {
    let mut window_gone = false;
    let mut ended = false;
    while let Some(message) = output.recv().await {
        match message {
            VoiceOutput::Event(event) => {
                let last = matches!(event, VoiceEvent::Ended { .. });
                if channel.send(event).is_err() && !window_gone {
                    // Nobody is listening: the window has gone. Say so to the
                    // session, which will end, and stop making sound for no
                    // one.
                    window_gone = true;
                    let _ = input.send(VoiceInput::Stop);
                    engine.stop();
                }
                if last {
                    ended = true;
                    break;
                }
            }
            VoiceOutput::Play { utterance, samples, sample_rate } => {
                if !window_gone {
                    engine.play(utterance, samples, sample_rate);
                }
            }
            VoiceOutput::Pause => engine.pause(),
            VoiceOutput::Resume => engine.resume(),
            VoiceOutput::Clear => engine.clear(),
            VoiceOutput::Working(on) => {
                if !window_gone {
                    engine.working(on);
                }
            }
        }
    }
    if !ended {
        let _ = channel.send(VoiceEvent::Ended {
            error: Some("The conversation stopped unexpectedly.".to_string()),
        });
    }
    engine.stop();
    release(app.state::<AppState>().voice(), id);
}

/// End the conversation. Returns once the session and the devices have been
/// told; the session's own `Ended` follows on the channel, and the slot is
/// free once it has. Nothing happens if nothing is being talked.
/// End the session, and let go of the microphone and speakers now rather than
/// when the session has finished saying goodbye.
fn stop_now(voice: &Mutex<Option<VoiceHandle>>) {
    let engine = match voice.lock().unwrap().as_ref() {
        Some(VoiceHandle::Running(running)) => Some(running.engine.clone()),
        _ => None,
    };
    send_or_hold(voice, VoiceInput::Stop);
    if let Some(engine) = engine {
        engine.stop();
    }
}

#[tauri::command]
pub async fn voice_stop(state: State<'_, AppState>) -> CommandResult<()> {
    stop_now(state.voice());
    Ok(())
}

/// A window is closing. Closing the main one only hides it, and a microphone
/// must not stay open behind a window nobody can see, so the session it
/// started ends here.
pub fn window_closing(app: &AppHandle, label: &str) {
    let state = app.state::<AppState>();
    let ours = match state.voice().lock().unwrap().as_ref() {
        Some(VoiceHandle::Running(running)) => running.window == label,
        Some(VoiceHandle::Starting(held)) => held.window == label,
        None => false,
    };
    if ours {
        stop_now(state.voice());
    }
}

/// While muted nothing is listened to, and what is being said goes on.
#[tauri::command]
pub async fn voice_set_muted(state: State<'_, AppState>, muted: bool) -> CommandResult<()> {
    send_or_hold(state.voice(), VoiceInput::Muted(muted));
    Ok(())
}

/// What the person has on screen now, for the assistant's next turn. Read as
/// leniently as `send_message` reads it: a part that does not parse costs only
/// itself.
#[tauri::command]
pub async fn voice_set_screen(
    state: State<'_, AppState>,
    on_screen: Option<serde_json::Value>,
) -> CommandResult<()> {
    let screen = on_screen.as_ref().and_then(OnScreen::lenient);
    send_or_hold(state.voice(), VoiceInput::Screen(screen));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Mutex<Option<VoiceHandle>> {
        Mutex::new(None)
    }

    fn no_capture() -> Mutex<Option<CaptureSlot>> {
        Mutex::new(None)
    }

    fn no_dictation() -> Mutex<Option<DictationHandle>> {
        Mutex::new(None)
    }

    #[test]
    fn two_starts_cannot_both_have_the_slot() {
        let voice = slot();
        let capture = no_capture();
        reserve(&voice, &capture, &no_dictation()).expect("the first start gets it");
        let second =
            reserve(&voice, &capture, &no_dictation()).expect_err("the second is turned away");
        assert_eq!(second.code, codes::BUSY);
    }

    #[test]
    fn a_meeting_being_recorded_keeps_voice_out() {
        let voice = slot();
        let capture = Mutex::new(Some(CaptureSlot::Starting));
        let refused =
            reserve(&voice, &capture, &no_dictation()).expect_err("one microphone, one user");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("meeting"));
        assert!(voice.lock().unwrap().is_none(), "nothing was reserved");
    }

    #[test]
    fn dictating_keeps_voice_out() {
        let voice = slot();
        let dictation =
            Mutex::new(Some(DictationHandle::Starting(crate::dictation::Held::default())));
        let refused =
            reserve(&voice, &no_capture(), &dictation).expect_err("one microphone, one user");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("dictating"));
        assert!(voice.lock().unwrap().is_none(), "nothing was reserved");
    }

    #[test]
    fn a_failed_start_frees_the_slot_for_the_next() {
        let voice = slot();
        let capture = no_capture();
        reserve(&voice, &capture, &no_dictation()).unwrap();
        abandon(&voice);
        reserve(&voice, &capture, &no_dictation()).expect("free again");
    }

    #[test]
    fn what_the_window_says_during_a_start_is_kept_and_delivered_in_order() {
        let voice = slot();
        reserve(&voice, &no_capture(), &no_dictation()).unwrap();
        send_or_hold(&voice, VoiceInput::Muted(true));
        send_or_hold(&voice, VoiceInput::Muted(false));
        send_or_hold(&voice, VoiceInput::Screen(None));
        send_or_hold(&voice, VoiceInput::Stop);

        let Some(VoiceHandle::Starting(held)) = voice.lock().unwrap().take() else {
            panic!("still starting");
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        held.deliver(&tx);
        assert!(matches!(rx.try_recv(), Ok(VoiceInput::Muted(false))), "the last word on muting");
        assert!(matches!(rx.try_recv(), Ok(VoiceInput::Screen(None))));
        assert!(matches!(rx.try_recv(), Ok(VoiceInput::Stop)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn saying_anything_with_nothing_running_is_harmless() {
        let voice = slot();
        send_or_hold(&voice, VoiceInput::Stop);
        send_or_hold(&voice, VoiceInput::Muted(true));
        assert!(voice.lock().unwrap().is_none());
    }

    #[test]
    fn a_session_clears_only_its_own_slot() {
        let voice = slot();
        let (input, _output) = tokio::sync::mpsc::unbounded_channel();
        // A conversation that has been started since the one that is ending.
        *voice.lock().unwrap() = Some(VoiceHandle::Running(Running {
            id: 2,
            window: String::new(),
            input: input.clone(),
            engine: Arc::new(AudioEngine::detached()),
        }));
        release(&voice, 1);
        assert!(voice.lock().unwrap().is_some(), "session 1 had no business with session 2's slot");
        release(&voice, 2);
        assert!(voice.lock().unwrap().is_none());
    }
}
