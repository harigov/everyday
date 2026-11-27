//! Listening to mail and articles from this computer.
//!
//! The desktop's transport for [`jatayu_service::speech::listen`]: the
//! default speakers on one side, a session on the other, and this module
//! between them. It is `voice` without the microphone -- `crate::audio` owns the
//! speakers, `listen_start` opens a session and a pump task that carries its
//! output to them and to the window, and the other commands are the player's
//! controls. See `docs/plans/listen.md`, "The desktop commands".
//!
//! One thing is listened to at a time, and starting another replaces it: the
//! player in the top bar has one place to be, and "Listen" on a second thing
//! means "this one instead". The replaced session's window hears its `Ended`
//! like any other. Listening refuses to start during a voice conversation,
//! which has the speakers; voice mode and dictation stop any listening before
//! they take the speakers or the microphone, because there is no echo
//! cancellation here and the microphone would hear what is being read. A window
//! looking at another computer's vault has no listening: the speakers are not
//! the vault's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jatayu_service::error::{CommandError, CommandResult, codes};
use jatayu_service::listen::{ListenOpened, ListenSource};
use jatayu_service::speech::listen::{ListenEvent, ListenInput, ListenOutput};
use tauri::{AppHandle, Manager, State};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
use crate::capture::CaptureSlot;
use crate::dictation::DictationHandle;
use crate::state::AppState;
use crate::voice::VoiceHandle;

/// What `AppState` holds while something is being listened to, or being
/// started.
///
/// Reserved with [`ListenHandle::Starting`] before the first await of
/// `listen_start`, for the same reason `voice` reserves its slot: two starts
/// racing each other must not both find it empty.
pub enum ListenHandle {
    /// A start is in flight: the session is being opened. What the window says
    /// meanwhile is held, and applied the moment there is a session.
    Starting {
        id: u64,
        held: Held,
    },
    Running(Running),
}

/// What the window said while a session was still being opened, in order.
#[derive(Default)]
pub struct Held {
    /// The window that asked, whose closing ends the session.
    window: String,
    inputs: Vec<ListenInput>,
}

/// A session under way.
pub struct Running {
    /// The window that started it. See [`window_closing`].
    window: String,
    /// Which session this is, so the pump that ends it clears only its own
    /// slot and never that of a session started since.
    id: u64,
    input: UnboundedSender<ListenInput>,
    engine: Arc<AudioEngine>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl Held {
    fn hold(&mut self, message: ListenInput) {
        match message {
            ListenInput::Pause | ListenInput::Resume | ListenInput::Skip(_) => {
                self.inputs.push(message);
            }
            // Only the last word on speed matters, and stopping makes
            // everything before it moot.
            ListenInput::Speed(_) => {
                self.inputs.retain(|held| !matches!(held, ListenInput::Speed(_)));
                self.inputs.push(message);
            }
            ListenInput::Stop => {
                self.inputs.clear();
                self.inputs.push(ListenInput::Stop);
            }
            ListenInput::Played { .. } => {}
        }
    }

    /// Say it all to the session, now that there is one.
    fn deliver(self, input: &UnboundedSender<ListenInput>) {
        for message in self.inputs {
            let _ = input.send(message);
        }
    }
}

/// Claim the one listening slot for a new session, ending whatever was in it,
/// or say why it cannot be had. Returns the new session's id.
///
/// Nothing that listens may hear it: there is no echo cancellation here, so a
/// meeting being recorded or something being dictated keeps listening out.
///
/// Lock order is capture, voice, dictation, then listening, as everywhere that
/// takes more than one.
fn reserve(
    listen: &Mutex<Option<ListenHandle>>,
    capture: &Mutex<Option<CaptureSlot>>,
    voice: &Mutex<Option<VoiceHandle>>,
    dictation: &Mutex<Option<DictationHandle>>,
    window: &str,
) -> CommandResult<u64> {
    let mut recording = capture.lock().unwrap();
    // A recording that ended on its own may not have cleared its slot yet.
    crate::meeting::reclaim_if_ended(&mut recording);
    if recording.is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "A meeting is being recorded, and it would record this too. Stop recording to listen to something.",
        ));
    }
    let voice = voice.lock().unwrap();
    if voice.is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "You are talking to the assistant, which has the speakers. End the conversation to listen to something.",
        ));
    }
    if dictation.lock().unwrap().is_some() {
        return Err(CommandError::new(
            codes::BUSY,
            "You are dictating, and the microphone would hear it. Finish dictating to listen to something.",
        ));
    }
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let old = listen.lock().unwrap().replace(ListenHandle::Starting {
        id,
        held: Held { window: window.to_string(), inputs: Vec::new() },
    });
    drop(voice);
    drop(recording);
    // Outside the locks: dropping the last reference to an engine.
    if let Some(old) = old {
        end(old);
    }
    Ok(id)
}

/// End a session that has been taken out of the slot: tell it, and let go of
/// the speakers at once. A start still in flight needs nothing here -- it finds
/// its slot gone when it lands, and ends itself.
fn end(handle: ListenHandle) {
    if let ListenHandle::Running(running) = handle {
        let _ = running.input.send(ListenInput::Stop);
        running.engine.stop();
    }
}

/// Hand `message` to whatever is on the other end of the slot: the session, if
/// there is one; the start in flight, to be delivered when it lands; nothing,
/// if nothing is being listened to.
fn send_or_hold(listen: &Mutex<Option<ListenHandle>>, message: ListenInput) {
    match listen.lock().unwrap().as_mut() {
        Some(ListenHandle::Running(running)) => {
            let _ = running.input.send(message);
        }
        Some(ListenHandle::Starting { held, .. }) => held.hold(message),
        None => {}
    }
}

/// Empty the slot if it still holds session `id`.
fn release(listen: &Mutex<Option<ListenHandle>>, id: u64) {
    let old = {
        let mut slot = listen.lock().unwrap();
        let ours = matches!(&*slot, Some(ListenHandle::Running(running)) if running.id == id);
        if ours { slot.take() } else { None }
    };
    // Dropped outside the lock: the last reference to the engine goes with it.
    drop(old);
}

/// Empty the slot of a start that failed, if it is still that start's.
fn abandon(listen: &Mutex<Option<ListenHandle>>, id: u64) {
    let old = {
        let mut slot = listen.lock().unwrap();
        let ours =
            matches!(&*slot, Some(ListenHandle::Starting { id: starting, .. }) if *starting == id);
        if ours { slot.take() } else { None }
    };
    drop(old);
}

/// Start listening to `source`, at `speed` times the voice's own: the
/// speakers go on, and `channel` hears everything the session says until it
/// ends.
///
/// Refuses when the window is looking at a vault on another computer and when
/// a voice conversation is under way. Listening to something else already
/// replaces it.
#[tauri::command]
pub async fn listen_start(
    app: AppHandle,
    webview: tauri::Webview,
    state: State<'_, AppState>,
    source: ListenSource,
    speed: f32,
    channel: tauri::ipc::Channel<ListenEvent>,
) -> CommandResult<()> {
    if state.is_remote() {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            "Listening uses this computer's speakers, so it is not available while this window is looking at a vault on another computer.",
        ));
    }
    let id = reserve(
        state.listen(),
        state.capture(),
        state.voice(),
        state.dictation(),
        webview.label(),
    )?;
    match try_start(&app, &state, id, source, speed, channel).await {
        Ok(()) => Ok(()),
        Err(e) => {
            abandon(state.listen(), id);
            Err(e)
        }
    }
}

/// Everything `listen_start` does after reserving the slot, so that every
/// early return in it is an ordinary `?` and the slot's release is in one
/// place.
async fn try_start(
    app: &AppHandle,
    state: &State<'_, AppState>,
    id: u64,
    source: ListenSource,
    speed: f32,
    channel: tauri::ipc::Channel<ListenEvent>,
) -> CommandResult<()> {
    let ListenOpened { link, title: _, notice } =
        state.service().listen_open(source, speed).await?;
    let jatayu_service::speech::listen::ListenLink { input, output } = link;

    if let Some(message) = notice {
        let _ = channel.send(ListenEvent::Trouble { message });
    }

    // What the speakers report goes to the session, and a device failing is
    // told to the window and ends the session.
    let sink: EngineSink = {
        let channel = channel.clone();
        let input = input.clone();
        Arc::new(move |event| match event {
            EngineEvent::Played(utterance) => {
                let _ = input.send(ListenInput::Played { utterance });
            }
            EngineEvent::Failed(message) => {
                let _ = channel.send(ListenEvent::Trouble { message });
                let _ = input.send(ListenInput::Stop);
            }
            EngineEvent::Microphone(_) => {}
        })
    };
    let options = EngineOptions { microphone: false, speaker: true, echo_cancellation: false };
    let engine = match AudioEngine::start(options, None, sink).await {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            // The session is already running; let it go.
            let _ = input.send(ListenInput::Stop);
            return Err(e.into());
        }
    };

    // In the slot before the pump exists, so a session that ends at once finds
    // its own slot to clear. A start that was replaced while it was opening
    // finds its slot gone or someone else's: it ends itself, and the pump
    // below still tells its window.
    let held = {
        let mut slot = state.listen().lock().unwrap();
        match slot.take() {
            Some(ListenHandle::Starting { id: starting, held }) if starting == id => {
                *slot = Some(ListenHandle::Running(Running {
                    id,
                    window: held.window.clone(),
                    input: input.clone(),
                    engine: engine.clone(),
                }));
                Some(held)
            }
            other => {
                *slot = other;
                None
            }
        }
    };
    match held {
        Some(held) => held.deliver(&input),
        None => {
            let _ = input.send(ListenInput::Stop);
            engine.stop();
        }
    }

    tauri::async_runtime::spawn(pump(app.clone(), id, output, input, engine, channel));
    Ok(())
}

/// Carry a session's output to the speakers and to the window, until it ends.
///
/// Never blocks: the speakers are told through the engine's queue, the window
/// through the channel. The speakers are closed, and the slot cleared, when the
/// session says it has ended -- or has closed its output without saying so,
/// which is reported to the window as an ending of its own.
async fn pump(
    app: AppHandle,
    id: u64,
    mut output: UnboundedReceiver<ListenOutput>,
    input: UnboundedSender<ListenInput>,
    engine: Arc<AudioEngine>,
    channel: tauri::ipc::Channel<ListenEvent>,
) {
    let mut window_gone = false;
    let mut ended = false;
    while let Some(message) = output.recv().await {
        match message {
            ListenOutput::Event(event) => {
                let last = matches!(event, ListenEvent::Ended { .. });
                if channel.send(event).is_err() && !window_gone {
                    // Nobody is listening: the window has gone. Say so to the
                    // session, which will end, and stop making sound for no
                    // one.
                    window_gone = true;
                    let _ = input.send(ListenInput::Stop);
                    engine.stop();
                }
                if last {
                    ended = true;
                    break;
                }
            }
            ListenOutput::Play { utterance, samples, sample_rate } => {
                if !window_gone {
                    engine.play(utterance, samples, sample_rate);
                }
            }
            ListenOutput::Pause => engine.pause(),
            ListenOutput::Resume => engine.resume(),
            ListenOutput::Clear => engine.clear(),
        }
    }
    if !ended {
        let _ = channel.send(ListenEvent::Ended {
            error: Some("Listening stopped unexpectedly.".to_string()),
        });
    }
    engine.stop();
    release(app.state::<AppState>().listen(), id);
}

/// End the session, and let go of the speakers now rather than when the
/// session has finished saying goodbye. A stop during a start is held, and is
/// the last thing the new session hears.
fn stop_now(listen: &Mutex<Option<ListenHandle>>) {
    let engine = match listen.lock().unwrap().as_ref() {
        Some(ListenHandle::Running(running)) => Some(running.engine.clone()),
        _ => None,
    };
    send_or_hold(listen, ListenInput::Stop);
    if let Some(engine) = engine {
        engine.stop();
    }
}

/// Stop whatever is being listened to, if anything is. For voice mode and
/// dictation, which must not start while the speakers are playing.
pub fn stop_listening(state: &AppState) {
    stop_now(state.listen());
}

#[tauri::command]
pub async fn listen_stop(state: State<'_, AppState>) -> CommandResult<()> {
    stop_now(state.listen());
    Ok(())
}

#[tauri::command]
pub async fn listen_pause(state: State<'_, AppState>) -> CommandResult<()> {
    send_or_hold(state.listen(), ListenInput::Pause);
    Ok(())
}

#[tauri::command]
pub async fn listen_resume(state: State<'_, AppState>) -> CommandResult<()> {
    send_or_hold(state.listen(), ListenInput::Resume);
    Ok(())
}

/// Go back (negative) or forward (positive) by `by` sentences from the one
/// playing.
#[tauri::command]
pub async fn listen_skip(state: State<'_, AppState>, by: i32) -> CommandResult<()> {
    send_or_hold(state.listen(), ListenInput::Skip(by));
    Ok(())
}

/// Read at `speed` times the voice's own, from the sentence playing.
#[tauri::command]
pub async fn listen_speed(state: State<'_, AppState>, speed: f32) -> CommandResult<()> {
    send_or_hold(state.listen(), ListenInput::Speed(speed));
    Ok(())
}

/// A window is closing. Closing the main one only hides it, and a voice must
/// not go on reading behind a window nobody can see, so the session it started
/// ends here.
pub fn window_closing(app: &AppHandle, label: &str) {
    let state = app.state::<AppState>();
    let ours = match state.listen().lock().unwrap().as_ref() {
        Some(ListenHandle::Running(running)) => running.window == label,
        Some(ListenHandle::Starting { held, .. }) => held.window == label,
        None => false,
    };
    if ours {
        stop_now(state.listen());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Mutex<Option<ListenHandle>> {
        Mutex::new(None)
    }

    fn no_voice() -> Mutex<Option<VoiceHandle>> {
        Mutex::new(None)
    }

    fn no_capture() -> Mutex<Option<CaptureSlot>> {
        Mutex::new(None)
    }

    fn no_dictation() -> Mutex<Option<DictationHandle>> {
        Mutex::new(None)
    }

    #[test]
    fn a_meeting_being_recorded_keeps_listening_out() {
        let listen = slot();
        let capture = Mutex::new(Some(CaptureSlot::Starting));
        let refused = reserve(&listen, &capture, &no_voice(), &no_dictation(), "main")
            .expect_err("the recording would hear it");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("meeting"));
        assert!(listen.lock().unwrap().is_none(), "nothing was reserved");
    }

    #[test]
    fn dictating_keeps_listening_out() {
        let listen = slot();
        let dictation = Mutex::new(Some(DictationHandle::Starting(Default::default())));
        let refused = reserve(&listen, &no_capture(), &no_voice(), &dictation, "main")
            .expect_err("the microphone would hear it");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("dictating"));
        assert!(listen.lock().unwrap().is_none(), "nothing was reserved");
    }

    fn running(
        id: u64,
        window: &str,
    ) -> (ListenHandle, UnboundedReceiver<ListenInput>, Arc<AudioEngine>) {
        let (input, heard) = tokio::sync::mpsc::unbounded_channel();
        let engine = Arc::new(AudioEngine::detached());
        let handle = ListenHandle::Running(Running {
            id,
            window: window.to_string(),
            input,
            engine: engine.clone(),
        });
        (handle, heard, engine)
    }

    #[test]
    fn a_conversation_keeps_listening_out_of_the_speakers() {
        let listen = slot();
        let voice = Mutex::new(Some(VoiceHandle::Starting(crate::voice::Held::default())));
        let refused = reserve(&listen, &no_capture(), &voice, &no_dictation(), "main")
            .expect_err("voice has the speakers");
        assert_eq!(refused.code, codes::BUSY);
        assert!(refused.message.contains("talking to the assistant"));
        assert!(listen.lock().unwrap().is_none(), "nothing was reserved");
    }

    #[test]
    fn a_second_start_replaces_the_first_and_stops_it() {
        let listen = slot();
        let voice = no_voice();
        let (handle, mut heard, _engine) = running(1, "main");
        *listen.lock().unwrap() = Some(handle);

        let id = reserve(&listen, &no_capture(), &voice, &no_dictation(), "app-mail")
            .expect("starting another replaces it");
        assert!(matches!(heard.try_recv(), Ok(ListenInput::Stop)), "the old session was told");
        assert!(
            matches!(&*listen.lock().unwrap(), Some(ListenHandle::Starting { id: now, .. }) if *now == id),
            "the slot is the new start's"
        );
        // The old session finishing must not take the new one's slot.
        release(&listen, 1);
        assert!(listen.lock().unwrap().is_some());
    }

    #[test]
    fn two_starts_in_a_row_each_get_their_own_id() {
        let listen = slot();
        let voice = no_voice();
        let first = reserve(&listen, &no_capture(), &voice, &no_dictation(), "main").unwrap();
        let second = reserve(&listen, &no_capture(), &voice, &no_dictation(), "main").unwrap();
        assert_ne!(first, second);
        // The first start, replaced, cannot empty the second's slot.
        abandon(&listen, first);
        assert!(listen.lock().unwrap().is_some());
        abandon(&listen, second);
        assert!(listen.lock().unwrap().is_none());
    }

    #[test]
    fn a_failed_start_frees_the_slot_for_the_next() {
        let listen = slot();
        let voice = no_voice();
        let id = reserve(&listen, &no_capture(), &voice, &no_dictation(), "main").unwrap();
        abandon(&listen, id);
        assert!(listen.lock().unwrap().is_none());
        reserve(&listen, &no_capture(), &voice, &no_dictation(), "main").expect("free again");
    }

    #[test]
    fn what_the_window_says_during_a_start_is_kept_and_delivered_in_order() {
        let listen = slot();
        reserve(&listen, &no_capture(), &no_voice(), &no_dictation(), "main").unwrap();
        send_or_hold(&listen, ListenInput::Pause);
        send_or_hold(&listen, ListenInput::Skip(-1));
        send_or_hold(&listen, ListenInput::Speed(1.5));
        send_or_hold(&listen, ListenInput::Speed(2.0));
        send_or_hold(&listen, ListenInput::Resume);

        let Some(ListenHandle::Starting { held, .. }) = listen.lock().unwrap().take() else {
            panic!("still starting");
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        held.deliver(&tx);
        assert!(matches!(rx.try_recv(), Ok(ListenInput::Pause)));
        assert!(matches!(rx.try_recv(), Ok(ListenInput::Skip(-1))));
        assert!(
            matches!(rx.try_recv(), Ok(ListenInput::Speed(s)) if s == 2.0),
            "only the last word on speed"
        );
        assert!(matches!(rx.try_recv(), Ok(ListenInput::Resume)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_stop_during_a_start_is_all_the_new_session_hears() {
        let listen = slot();
        reserve(&listen, &no_capture(), &no_voice(), &no_dictation(), "main").unwrap();
        send_or_hold(&listen, ListenInput::Pause);
        stop_now(&listen);

        let Some(ListenHandle::Starting { held, .. }) = listen.lock().unwrap().take() else {
            panic!("still starting");
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        held.deliver(&tx);
        assert!(matches!(rx.try_recv(), Ok(ListenInput::Stop)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn stopping_a_running_session_tells_it() {
        let listen = slot();
        let (handle, mut heard, _engine) = running(3, "main");
        *listen.lock().unwrap() = Some(handle);
        stop_now(&listen);
        assert!(matches!(heard.try_recv(), Ok(ListenInput::Stop)));
        // The slot is the pump's to clear, when the session has ended.
        assert!(listen.lock().unwrap().is_some());
    }

    #[test]
    fn saying_anything_with_nothing_running_is_harmless() {
        let listen = slot();
        send_or_hold(&listen, ListenInput::Stop);
        send_or_hold(&listen, ListenInput::Pause);
        stop_now(&listen);
        assert!(listen.lock().unwrap().is_none());
    }

    #[test]
    fn a_session_clears_only_its_own_slot() {
        let listen = slot();
        let (handle, _heard, _engine) = running(2, "main");
        // Something started since the one that is ending.
        *listen.lock().unwrap() = Some(handle);
        release(&listen, 1);
        assert!(
            listen.lock().unwrap().is_some(),
            "session 1 had no business with session 2's slot"
        );
        release(&listen, 2);
        assert!(listen.lock().unwrap().is_none());
    }
}
