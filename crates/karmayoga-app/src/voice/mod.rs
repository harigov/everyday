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
//!
//! # A session with no window
//!
//! A session's events go to an [`Outlet`]: a webview's channel, for
//! `voice_start`, or the shell's own callback, for [`start_for_shell`] -- the
//! talk key (`crate::talk`) opens a conversation wherever the person is
//! looking, with the panel where the window would be. The two are the same
//! session on the same slot and refuse each other, but a shell session
//! belongs to no window: closing the main window does not end it
//! ([`window_closing`]), and the window's own stop and mute commands are not
//! how the shell reaches it ([`stop_shell`], [`set_shell_muted`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use karmayoga_core::ConversationId;
use karmayoga_core::agent::onscreen::OnScreen;
use karmayoga_service::agent::Typist;
use karmayoga_service::error::{CommandError, CommandResult, codes};
use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceOutput};
use tauri::{AppHandle, Manager, State};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
#[cfg(desktop)]
use crate::capture::CaptureSlot;
#[cfg(desktop)]
use crate::dictation::DictationHandle;
use crate::state::{AppState, SessionHandle};

#[cfg(all(debug_assertions, target_os = "android"))]
mod test_microphone;

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
    /// The window that asked, whose closing ends the session. Empty for the
    /// shell's own: see [`Outlet`].
    window: String,
    muted: Option<bool>,
    screen: Option<Option<Box<OnScreen>>>,
    /// Words said before the session opened -- see [`shell_heard`].
    spoken: Option<String>,
    /// The listening sound was asked for -- see [`shell_chime`].
    chime: bool,
    stop: bool,
}

/// A session under way.
pub struct Running {
    /// The window that started it. See [`window_closing`]. Empty for a
    /// session the shell started, which no window's closing ends.
    window: String,
    /// Which session this is, so the pump that ends it clears only its own
    /// slot and never that of a conversation started since.
    id: u64,
    input: UnboundedSender<VoiceInput>,
    engine: Arc<AudioEngine>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Where a session's events go, and who hears the microphone.
#[derive(Clone)]
pub(crate) enum Outlet {
    /// The window that started it, through the channel it opened.
    Window(tauri::ipc::Channel<VoiceEvent>),
    /// The shell, with no window.
    Shell(Shell),
}

/// Where a shell session's events go. See [`Shell::events`].
pub(crate) type EventSink = Arc<dyn Fn(VoiceEvent) -> bool + Send + Sync>;

/// What hears a shell session's microphone. See [`Shell::microphone`].
pub(crate) type MicrophoneTap = Arc<dyn Fn(&[i16]) + Send + Sync>;

/// What the shell hands a session it starts in place of a window's channel.
#[derive(Clone)]
pub(crate) struct Shell {
    /// Told every event. Returns `false` when nobody is listening any more,
    /// as a channel's send fails when its window has gone: the session is then
    /// ended and stops making sound for no one. Called from the pump, which
    /// is a task: it must not wait.
    pub events: EventSink,
    /// Shown every block of microphone audio (about 30 ms of it, echo already
    /// removed) before the session is, for a level meter. Called from the
    /// audio thread, so it must be quick.
    pub microphone: Option<MicrophoneTap>,
    /// What types where the person was typing, so that "type that ..." does
    /// -- see `karmayoga_service::agent::Typist`. The talk key's; a window's
    /// session has none, the field with the keyboard being the window's own.
    pub typist: Option<Arc<dyn Typist>>,
    /// The wake word began it, not the key: what is said in it approves no
    /// change and has more than a line shown before it is typed -- see
    /// `karmayoga_service::agent::Turn::woken`.
    pub woken: bool,
}

impl Outlet {
    /// Tell it `event`. `false` if nobody is listening.
    fn send(&self, event: VoiceEvent) -> bool {
        match self {
            Outlet::Window(channel) => channel.send(event).is_ok(),
            Outlet::Shell(shell) => (shell.events)(event),
        }
    }

    /// A block of the microphone, for whoever draws its level.
    fn heard(&self, audio: &[i16]) {
        if let Outlet::Shell(Shell { microphone: Some(microphone), .. }) = self {
            microphone(audio);
        }
    }
}

impl Running {
    /// Started by the shell, not by a window.
    fn is_shell(&self) -> bool {
        self.window.is_empty()
    }
}

impl Held {
    fn is_shell(&self) -> bool {
        self.window.is_empty()
    }
}

impl Held {
    fn hold(&mut self, message: VoiceInput) {
        match message {
            VoiceInput::Muted(muted) => self.muted = Some(muted),
            VoiceInput::Screen(screen) => self.screen = Some(screen),
            VoiceInput::Stop => self.stop = true,
            VoiceInput::Spoken(text) => self.spoken = Some(text),
            VoiceInput::Chime => self.chime = true,
            // The desktop never asks the session to say something of its own.
            VoiceInput::Audio(_)
            | VoiceInput::Played { .. }
            | VoiceInput::Say(_)
            | VoiceInput::Update(_) => {}
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
        if self.chime {
            let _ = input.send(VoiceInput::Chime);
        }
        if let Some(text) = self.spoken {
            let _ = input.send(VoiceInput::Spoken(text));
        }
        if self.stop {
            let _ = input.send(VoiceInput::Stop);
        }
    }
}

/// Claim the one voice slot, or say why it cannot be had.
#[cfg(desktop)]
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
    claim(voice)
}

/// The voice slot, for a conversation about to start: refused while one is
/// already talking. All a phone checks -- it has no recording or dictation to
/// hold the microphone -- and the last thing the desktop's `reserve` does.
fn claim(voice: &Mutex<Option<VoiceHandle>>) -> CommandResult<()> {
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
/// A window looking at a vault on another computer -- a phone always is --
/// talks to that computer's session over its voice socket, with this
/// machine's microphone and speakers. Refuses when a meeting is being
/// recorded, when someone is dictating, and when there is a conversation
/// already. Anything being listened to is stopped.
#[tauri::command]
pub async fn voice_start(
    app: AppHandle,
    webview: tauri::Webview,
    state: State<'_, AppState>,
    conversation_id: ConversationId,
    on_screen: Option<serde_json::Value>,
    channel: tauri::ipc::Channel<VoiceEvent>,
) -> CommandResult<()> {
    begin(
        &app,
        &state,
        webview.label().to_string(),
        conversation_id,
        on_screen,
        Outlet::Window(channel),
    )
    .await
}

/// Start talking in a conversation with no window: the shell's `shell` hears
/// everything the session says until it ends. How the talk key opens one.
///
/// Refuses as [`voice_start`] does -- a vault on another computer, a meeting
/// being recorded, dictating, a conversation already -- and stops anything
/// being listened to first. Nothing is said of what is on screen: the shell
/// does not know what is.
pub(crate) async fn start_for_shell(
    app: &AppHandle,
    conversation_id: ConversationId,
    shell: Shell,
) -> CommandResult<()> {
    let state = app.state::<AppState>();
    begin(app, &state, String::new(), conversation_id, None, Outlet::Shell(shell)).await
}

/// What both ways of starting do: refuse what cannot be, claim the slot, and
/// open the session. `window` is the label of the window that asked, or empty
/// for the shell.
async fn begin(
    app: &AppHandle,
    state: &State<'_, AppState>,
    window: String,
    conversation_id: ConversationId,
    on_screen: Option<serde_json::Value>,
    outlet: Outlet,
) -> CommandResult<()> {
    // A window looking at a vault on another computer -- a phone always is
    // -- talks to that computer's assistant through it: this machine's
    // microphone and speakers, that one's models. Not the talk key's or the
    // wake word's conversation, which types into this desktop and is opened
    // from it.
    if state.is_remote() && matches!(outlet, Outlet::Shell(_)) {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            "The talk key talks to the assistant on this computer, and this window is looking at a vault on another one. Use the microphone button in the assistant instead.",
        ));
    }
    #[cfg(desktop)]
    reserve(state.voice(), state.capture(), state.dictation())?;
    #[cfg(mobile)]
    claim(state.voice())?;
    if let Some(VoiceHandle::Starting(held)) = state.voice().lock().unwrap().as_mut() {
        held.window = window;
    }
    // The speakers are this conversation's now, and its microphone would
    // otherwise hear whatever was being read.
    #[cfg(desktop)]
    crate::listen::stop_listening(state);

    match try_start(app, state, conversation_id, on_screen, outlet).await {
        Ok(()) => Ok(()),
        Err(e) => {
            abandon(state.voice());
            Err(e)
        }
    }
}

/// Everything `begin` does after reserving the slot, so that every early
/// return in it is an ordinary `?` and the slot's release is in one place.
async fn try_start(
    app: &AppHandle,
    state: &State<'_, AppState>,
    conversation_id: ConversationId,
    on_screen: Option<serde_json::Value>,
    outlet: Outlet,
) -> CommandResult<()> {
    let (typist, woken) = match &outlet {
        Outlet::Shell(shell) => (shell.typist.clone(), shell.woken),
        Outlet::Window(_) => (None, false),
    };
    let link = match (state.session(), typist) {
        // The other computer's session, carried over the connection: see
        // `karmayoga_server::voice`.
        (SessionHandle::Remote(remote), _) => {
            remote.client.voice_open(conversation_id, on_screen.clone()).await?
        }
        (SessionHandle::Local(service), Some(typist)) => {
            service.voice_open_typing(conversation_id, typist, woken).await?
        }
        (SessionHandle::Local(service), None) => service.voice_open(conversation_id).await?,
    };
    let karmayoga_service::voice::VoiceLink { input, output } = link;

    let _ = input
        .send(VoiceInput::Screen(on_screen.as_ref().and_then(OnScreen::lenient).map(Box::new)));

    // What the devices report goes to the session, and a device failing is
    // told to whoever is listening and ends the session.
    let sink: EngineSink = {
        let outlet = outlet.clone();
        let input = input.clone();
        Arc::new(move |event| match event {
            EngineEvent::Microphone(audio) => {
                // A debug build on Android can be handed a recording in place of
                // the microphone, for testing on an emulator: see
                // `test_microphone`.
                #[cfg(all(debug_assertions, target_os = "android"))]
                let audio = test_microphone::replace(audio);
                outlet.heard(&audio);
                let _ = input.send(VoiceInput::Audio(audio));
            }
            EngineEvent::Played(utterance) => {
                let _ = input.send(VoiceInput::Played { utterance });
            }
            EngineEvent::Failed(message) => {
                outlet.send(VoiceEvent::Trouble { message });
                let _ = input.send(VoiceInput::Stop);
            }
        })
    };
    let options = EngineOptions { microphone: true, speaker: true, echo_cancellation: true };
    // A phone runs the ordinary filter alone: the models for the stronger
    // ones are the desktop's. See `microphone`.
    #[cfg(desktop)]
    let cleanup = crate::microphone::cleanup(&state.service()).await;
    #[cfg(mobile)]
    let cleanup = None;
    let engine = match AudioEngine::start(options, cleanup, sink).await {
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

    tauri::async_runtime::spawn(pump(app.clone(), id, output, input, engine, outlet));
    Ok(())
}

/// Carry a session's output to the speakers and to whoever is listening,
/// until it ends.
///
/// Never blocks: the speakers are told through the engine's queue, the
/// listener through the channel or the shell's callback. The audio devices
/// are closed, and the slot cleared, when the session says it has ended -- or
/// has closed its output without saying so, which is told to the listener as
/// an ending of its own.
///
/// The devices and the slot go before the ending is told, not after: whoever
/// hears that the conversation is over -- a window, or a second press of the
/// talk key a moment later -- may start another at once, and must find the
/// slot free.
async fn pump(
    app: AppHandle,
    id: u64,
    mut output: UnboundedReceiver<VoiceOutput>,
    input: UnboundedSender<VoiceInput>,
    engine: Arc<AudioEngine>,
    outlet: Outlet,
) {
    let mut unheard = false;
    let mut ended = false;
    while let Some(message) = output.recv().await {
        match message {
            VoiceOutput::Event(event) => {
                if matches!(event, VoiceEvent::Ended { .. }) {
                    engine.stop();
                    release(app.state::<AppState>().voice(), id);
                    let _ = outlet.send(event);
                    ended = true;
                    break;
                }
                if !outlet.send(event) && !unheard {
                    // Nobody is listening: the window has gone. Say so to the
                    // session, which will end, and stop making sound for no
                    // one.
                    unheard = true;
                    let _ = input.send(VoiceInput::Stop);
                    engine.stop();
                }
            }
            VoiceOutput::Play { utterance, samples, sample_rate } => {
                if !unheard {
                    engine.play(utterance, samples, sample_rate);
                }
            }
            VoiceOutput::Pause => engine.pause(),
            VoiceOutput::Resume => engine.resume(),
            VoiceOutput::Clear => engine.clear(),
            VoiceOutput::Working(on) => {
                if !unheard {
                    engine.working(on);
                }
            }
            VoiceOutput::Chime => {
                if !unheard {
                    engine.chime();
                }
            }
        }
    }
    if !ended {
        engine.stop();
        release(app.state::<AppState>().voice(), id);
        let _ = outlet.send(VoiceEvent::Ended {
            error: Some("The conversation stopped unexpectedly.".to_string()),
        });
    }
}

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

/// [`stop_now`], for a session `which` says is the one meant -- it is told
/// the label of the window that started it, empty for the shell -- and for no
/// other. Whether there was one. The slot is looked at once, so a session
/// that has ended and been replaced between a look and a stop is not stopped
/// by mistake.
fn stop_if(voice: &Mutex<Option<VoiceHandle>>, which: impl Fn(&str) -> bool) -> bool {
    let engine = {
        let mut slot = voice.lock().unwrap();
        match slot.as_mut() {
            Some(VoiceHandle::Running(running)) if which(&running.window) => {
                let _ = running.input.send(VoiceInput::Stop);
                Some(Some(running.engine.clone()))
            }
            Some(VoiceHandle::Starting(held)) if which(&held.window) => {
                held.hold(VoiceInput::Stop);
                Some(None)
            }
            _ => None,
        }
    };
    match engine {
        Some(engine) => {
            if let Some(engine) = engine {
                engine.stop();
            }
            true
        }
        None => false,
    }
}

/// [`send_or_hold`], for a session the shell started and for no other.
/// Whether there was one.
fn send_or_hold_shell(voice: &Mutex<Option<VoiceHandle>>, message: VoiceInput) -> bool {
    match voice.lock().unwrap().as_mut() {
        Some(VoiceHandle::Running(running)) if running.is_shell() => {
            let _ = running.input.send(message);
            true
        }
        Some(VoiceHandle::Starting(held)) if held.is_shell() => {
            held.hold(message);
            true
        }
        _ => false,
    }
}

/// End the conversation the talk key started, and only that one: a window's
/// own is its window's to end. Returns at once; the session's own `Ended`
/// follows on the shell's callback, and the slot is free by then. Whether
/// there was one.
pub(crate) fn stop_shell(app: &AppHandle) -> bool {
    stop_if(app.state::<AppState>().voice(), |window| window.is_empty())
}

/// Hand the conversation the talk key started what the person said before it
/// opened -- the words after the wake word -- to be its first turn. Whether
/// there was one.
pub(crate) fn shell_heard(app: &AppHandle, text: String) -> bool {
    send_or_hold_shell(app.state::<AppState>().voice(), VoiceInput::Spoken(text))
}

/// Have the conversation the talk key started play its listening sound --
/// see `karmayoga_service::voice::VoiceInput::Chime`: the wake word opened it
/// with the name alone, and the person is to know they are heard. Whether
/// there was one.
pub(crate) fn shell_chime(app: &AppHandle) -> bool {
    send_or_hold_shell(app.state::<AppState>().voice(), VoiceInput::Chime)
}

/// Mute or unmute the conversation the talk key started. While muted nothing
/// is listened to, and what is being said goes on; muting while somebody is
/// being heard drops what they had said so far, which is for the caller to
/// remember. Whether there was one.
pub(crate) fn set_shell_muted(app: &AppHandle, muted: bool) -> bool {
    send_or_hold_shell(app.state::<AppState>().voice(), VoiceInput::Muted(muted))
}

/// End the conversation. Returns once the session and the devices have been
/// told; the session's own `Ended` follows on the channel, and the slot is
/// free once it has. Nothing happens if nothing is being talked.
#[tauri::command]
pub async fn voice_stop(state: State<'_, AppState>) -> CommandResult<()> {
    stop_now(state.voice());
    Ok(())
}

/// A window is closing. Closing the main one only hides it, and a microphone
/// must not stay open behind a window nobody can see, so the session it
/// started ends here. A conversation the shell started has no window, so no
/// window's closing ends it: its card on the panel is where it is ended.
pub fn window_closing(app: &AppHandle, label: &str) {
    if label.is_empty() {
        return;
    }
    stop_if(app.state::<AppState>().voice(), |window| window == label);
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
///
/// A conversation the wake word began holds its first turn until this has
/// come, so that turn is about what the window shows -- see
/// `crate::talk::screen_shared`.
#[tauri::command]
pub async fn voice_set_screen(
    app: AppHandle,
    state: State<'_, AppState>,
    on_screen: Option<serde_json::Value>,
) -> CommandResult<()> {
    let screen = on_screen.as_ref().and_then(OnScreen::lenient).map(Box::new);
    send_or_hold(state.voice(), VoiceInput::Screen(screen));
    #[cfg(desktop)]
    crate::talk::screen_shared(&app);
    #[cfg(mobile)]
    let _ = app;
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

    fn running_for(
        voice: &Mutex<Option<VoiceHandle>>,
        window: &str,
    ) -> tokio::sync::mpsc::UnboundedReceiver<VoiceInput> {
        let (input, output) = tokio::sync::mpsc::unbounded_channel();
        *voice.lock().unwrap() = Some(VoiceHandle::Running(Running {
            id: 1,
            window: window.to_string(),
            input,
            engine: Arc::new(AudioEngine::detached()),
        }));
        output
    }

    #[test]
    fn a_windows_closing_does_not_end_a_conversation_the_shell_started() {
        let voice = slot();
        let mut heard = running_for(&voice, "");
        assert!(!stop_if(&voice, |window| window == "main"), "no window's session is running");
        assert!(heard.try_recv().is_err(), "the shell's session was left alone");
        assert!(stop_if(&voice, |window| window.is_empty()), "the shell ends its own");
        assert!(matches!(heard.try_recv(), Ok(VoiceInput::Stop)));
    }

    #[test]
    fn the_shell_cannot_stop_or_mute_a_conversation_a_window_started() {
        let voice = slot();
        let mut heard = running_for(&voice, "main");
        assert!(!send_or_hold_shell(&voice, VoiceInput::Muted(true)));
        assert!(!stop_if(&voice, |window| window.is_empty()));
        assert!(heard.try_recv().is_err());
        assert!(stop_if(&voice, |window| window == "main"));
        assert!(matches!(heard.try_recv(), Ok(VoiceInput::Stop)));
    }

    #[test]
    fn what_the_shell_says_during_its_own_start_is_kept_and_delivered() {
        let voice = slot();
        reserve(&voice, &no_capture(), &no_dictation()).unwrap();
        // `begin` leaves the window empty for the shell.
        assert!(send_or_hold_shell(&voice, VoiceInput::Muted(true)));
        assert!(stop_if(&voice, |window| window.is_empty()));

        let Some(VoiceHandle::Starting(held)) = voice.lock().unwrap().take() else {
            panic!("still starting");
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        held.deliver(&tx);
        assert!(matches!(rx.try_recv(), Ok(VoiceInput::Muted(true))));
        assert!(matches!(rx.try_recv(), Ok(VoiceInput::Stop)));
    }

    #[test]
    fn a_start_in_flight_for_a_window_is_not_the_shells() {
        let voice = slot();
        reserve(&voice, &no_capture(), &no_dictation()).unwrap();
        if let Some(VoiceHandle::Starting(held)) = voice.lock().unwrap().as_mut() {
            held.window = "main".to_string();
        }
        assert!(!send_or_hold_shell(&voice, VoiceInput::Stop));
        assert!(!stop_if(&voice, |window| window.is_empty()));
        assert!(stop_if(&voice, |window| window == "main"));
    }
}
