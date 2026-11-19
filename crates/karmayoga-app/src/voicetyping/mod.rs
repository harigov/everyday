//! Voice typing: dictation into any text field on the desktop.
//!
//! Press the voice typing key (or hold it), say what you want to type, and it is
//! typed into whatever text field had the keyboard: heard by the speech model
//! chosen in Settings → Models, written up on the medium model -- filler out,
//! the speaker's own corrections applied, punctuated -- and pasted there. A
//! small pill at the bottom of the screen says what is happening: listening,
//! with a level meter; writing; done. See `docs/plans/voice-typing.md`.
//!
//! Not to be confused with [`crate::dictation`], mail's write-by-voice inside
//! Karma Yoga's own window, whose listening session this shares.
//!
//! It is writing help with a microphone where the selection was, and shares
//! what it can with it: the key is claimed beside writing help's
//! ([`crate::hotkey`]), the paste is writing help's ([`paste`]), the record
//! is a `WritingAssist` (`dictate`, in `karmayoga_service::domains::writing`),
//! and the audio and the words are voice mode's layers
//! ([`crate::audio::engine`], `karmayoga_service::speech::dictate`).
//!
//! The one thing it must not do that writing help does is ask for the
//! keyboard while it listens: the text goes to the field that has it. (GNOME
//! on Wayland hands the pill the keyboard anyway, and hiding it hands it back
//! to that field -- see [`pill`].)

pub mod paste;
pub mod pill;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use karmayoga_service::ctx::Ctx;
use karmayoga_service::error::{CommandError, CommandResult, codes};
use karmayoga_service::speech::dictate::{
    DictationEnd, DictationEvent, DictationInput, DictationLink,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::Shortcut;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
use crate::hotkey::{self, Binding, Hotkeys, KeyStatus};
use crate::state::AppState;

/// Tells the pill what to show. Its other half is `ui/src/voicetyping/main.ts`.
pub const STATE_EVENT: &str = "karmayoga://voice-typing/state";

/// How loud the microphone is, about twenty times a second while listening:
/// a number from 0 to 1, for the pill's meter.
pub const LEVEL_EVENT: &str = "karmayoga://voice-typing/level";

/// The key unless somebody chooses another. M for the microphone; two
/// modifiers for the reason writing help's has them. Not `Ctrl+Alt+D`, the
/// obvious one: GNOME as Ubuntu ships it hides every window on that.
pub const DEFAULT_SHORTCUT: &str = "Ctrl+Alt+M";

// ---- the per-machine setting -------------------------------------------------

/// How the key starts and stops listening.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyMode {
    /// Press to start, press again to stop.
    #[default]
    Toggle,
    /// Hold to talk; let go to stop. A quick tap starts listening hands-free
    /// until the next press.
    Hold,
}

/// Voice typing's setting on this machine, beside writing help's and for the
/// same reason: the key is a claim on this desktop's keyboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    pub shortcut: String,
    pub mode: KeyMode,
    /// Write up what was heard on the medium model. Off types the transcript
    /// as the speech model heard it.
    pub clean: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            shortcut: DEFAULT_SHORTCUT.into(),
            mode: KeyMode::Toggle,
            clean: true,
        }
    }
}

const FILE: &str = "voice-typing.json";

fn config_path() -> PathBuf {
    karmayoga_vault::config_dir().join(FILE)
}

fn save_config(config: &Config) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        karmayoga_core::fsutil::create_private_dir(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    // Private from the moment it exists, as writing help's file is: the two
    // are kept alike, and nothing here needs anybody else to read it.
    karmayoga_core::fsutil::write_atomic_private(
        &path,
        &bytes,
        &karmayoga_core::fsutil::unique_tag(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_config() -> Config {
    std::fs::read(config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Voice typing's shortcut while it is switched on, or `None` when it is off or
/// its setting no longer parses.
pub fn claimed_shortcut() -> Option<Shortcut> {
    let config = load_config();
    if !config.enabled {
        return None;
    }
    hotkey::parse(&config.shortcut)
        .map_err(|e| tracing::warn!(error = %e, "voice typing's shortcut does not parse"))
        .ok()
}

/// The shortcut to claim at startup: [`claimed_shortcut`], unless writing
/// help is on and has the same one. Both settings are checked against each
/// other when they are saved, but a file edited by hand or a writing shortcut
/// chosen before dictation existed can still clash, and the key would then
/// open writing help while dictation's status said it was claimed.
pub fn configured_shortcut() -> Option<Shortcut> {
    let ours = claimed_shortcut()?;
    if crate::writing::configured_shortcut() == Some(ours) {
        tracing::warn!("voice typing's shortcut is writing help's too; its key is left unclaimed");
        return None;
    }
    Some(ours)
}

// ---- what the pill shows -------------------------------------------------------

/// Where a dictation has got to, in the order it gets there: a phase never
/// goes back to an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// The microphone is opening and the speech model loading. What is said
    /// meanwhile is kept.
    Starting,
    /// Hearing what is said.
    Listening,
    /// Stopped listening; the last words are being worked out.
    Transcribing,
    /// The medium model is writing it up.
    Writing,
    /// Typed. The pill goes a moment later.
    Done,
    /// Nothing typed: `message` says why, and `text` holds whatever there
    /// was to type, for the pill's Copy button.
    Failed,
}

/// What the pill shows. Sent whole on every change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypingState {
    /// Which dictation this is, counted from 1 for the life of the process,
    /// so the pill can tell a new one from the last.
    pub seq: u64,
    pub phase: Phase,
    /// How the key works right now: in hold mode the pill says "let go to
    /// stop"; hands-free after a tap, it says "press to stop".
    pub mode: KeyMode,
    /// Listening hands-free (toggle mode, or hold mode after a tap).
    pub hands_free: bool,
    /// The key, as it should be written on screen -- the desktop's own
    /// answer where it keeps the binding -- for "press Ctrl+Alt+M to stop".
    pub key: String,
    /// For `Failed`, why; for `Done`, a note when there is one ("copied").
    pub message: Option<String>,
    /// For `Failed`, the text that could not be typed, when there was some.
    pub text: Option<String>,
    /// How the pill is drawn -- see [`pill::PillMode`].
    pub pill: pill::PillMode,
}

// ---- the session ------------------------------------------------------------------

/// A key held for less than this was a tap: in hold mode it starts listening
/// hands-free instead of ending a dictation the moment it is let go.
const TAP: Duration = Duration::from_millis(400);

/// How long the last words are waited for once the session has been told to
/// stop. The session gives up on a recogniser sooner than this (its own
/// `FINISH_WAIT`); this is only so a session that never answers cannot hold
/// the pill up. Waiting for the speech model to load is not limited by it.
const FINISH_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the microphone stays open after the key says stop: a device hands
/// audio over a little after it hears it, and the end of the last word is in
/// that little.
const TAIL: Duration = Duration::from_millis(150);

/// How long the pill stays after what it shows has no button to press.
const DONE_LINGER: Duration = Duration::from_millis(700);
const NOTHING_LINGER: Duration = Duration::from_millis(2500);
const FAILURE_LINGER: Duration = Duration::from_secs(8);
/// Longer where there is text to copy: reading it and reaching the button
/// takes a moment.
const COPY_LINGER: Duration = Duration::from_secs(20);

/// The quietest sound the level meter shows and the loudest it tells apart,
/// in dB below full scale. Speech at a desk is around -30.
const QUIET_DB: f32 = -55.0;
const LOUD_DB: f32 = -10.0;

/// The level goes to the pill this often, however often the microphone
/// reports.
const LEVEL_EVERY: Duration = Duration::from_millis(50);

/// What a running dictation is told, through one channel so that the task
/// that runs it has a single thing to wait on.
enum Msg {
    /// About 30 ms of the microphone.
    Audio(Vec<i16>),
    /// The microphone failed; the engine has stopped.
    Trouble(String),
    /// The dictation session is open -- the speech model loaded -- or could
    /// not be.
    Opened(CommandResult<UnboundedSender<DictationInput>>),
    /// What the session says, from the moment it is open.
    Session(DictationEvent),
    /// The key or the pill's stop button says that is enough.
    Finish,
}

/// A dictation under way, from the key going down until it is typed or has
/// failed.
struct Session {
    seq: u64,
    /// How the key works for this dictation, as it was when it began.
    mode: KeyMode,
    phase: Phase,
    hands_free: bool,
    /// Hold mode: when the key went down and has not come up since.
    down_since: Option<Instant>,
    /// The configured key, for the pill when the desktop says nothing.
    shortcut: String,
    inbox: UnboundedSender<Msg>,
    /// Ends the dictation where it stands. `None` for the moment between
    /// the session existing and its task being spawned.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

/// What could not be typed, kept so the pill's Copy button has something to
/// copy and the record is settled whichever way it ends.
struct Pending {
    seq: u64,
    text: String,
    /// The record `dictate` kept, unless the write-up itself failed.
    assist: Option<String>,
}

#[derive(Default)]
struct Inner {
    session: Option<Session>,
    /// What the pill shows, `None` once it has gone.
    last: Option<TypingState>,
    /// The latest activation token the key brought. Wayland only.
    token: Option<String>,
    pending: Option<Pending>,
    /// The key is down, as far as its presses and releases say. Kept only
    /// where releases are reported -- see [`first_press`].
    key_down: bool,
}

/// Whether this edge of the key means anything, keeping `key_down` up to date:
/// a press while the key is already down is the keyboard repeating it -- GNOME
/// sends a held key's activation again and again, the first time after its
/// repeat delay -- and not a new press.
///
/// Only where releases are reported: without them the key would be "down"
/// for ever after the first press. A desktop that does not report releases
/// does not repeat presses either, as far as is known (X11 through the plugin
/// reports both and repeats neither).
fn first_press(key_down: &mut bool, pressed: bool) -> bool {
    if pressed {
        !std::mem::replace(key_down, true)
    } else {
        *key_down = false;
        true
    }
}

/// Voice typing's state, for the life of the process.
#[derive(Default)]
pub struct VoiceTyping {
    inner: Mutex<Inner>,
    seq: AtomicU64,
}

impl VoiceTyping {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("voice typing lock")
    }
}

/// Voice typing's state, locked. Never held across an await.
fn locked(app: &AppHandle) -> std::sync::MutexGuard<'_, Inner> {
    app.state::<VoiceTyping>().inner().lock()
}

// ---- the key ------------------------------------------------------------------------

/// Where a dictation is, as far as the key is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Nothing is under way; a press starts one.
    Idle,
    /// Opening the microphone or listening.
    Listening { hands_free: bool, down_since: Option<Instant> },
    /// Stopped listening; the key has nothing to say until it is over.
    Busy,
}

/// What a press or release of the key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Nothing,
    Start,
    Finish,
    /// The key was let go quickly in hold mode: keep listening until the next
    /// press.
    HandsFree,
}

/// What the key does, as a function of nothing but its inputs so that every
/// case can be tested.
fn decide(mode: KeyMode, stage: Stage, pressed: bool, now: Instant) -> Act {
    let (hands_free, down_since) = match stage {
        Stage::Idle => return if pressed { Act::Start } else { Act::Nothing },
        Stage::Busy => return Act::Nothing,
        Stage::Listening { hands_free, down_since } => (hands_free, down_since),
    };
    match (mode, pressed) {
        (KeyMode::Toggle, true) => Act::Finish,
        (KeyMode::Toggle, false) => Act::Nothing,
        // Hands-free, the next press ends it. Held, a press can only come
        // after a release that went missing -- repeats never get here, see
        // [`first_press`] -- and is the end, as it would be in toggle mode.
        (KeyMode::Hold, true) => Act::Finish,
        (KeyMode::Hold, false) if hands_free => Act::Nothing,
        (KeyMode::Hold, false) => match down_since {
            Some(since) if now.saturating_duration_since(since) < TAP => Act::HandsFree,
            Some(_) => Act::Finish,
            None => Act::Nothing,
        },
    }
}

impl Session {
    fn stage(&self) -> Stage {
        if self.phase <= Phase::Listening {
            Stage::Listening { hands_free: self.hands_free, down_since: self.down_since }
        } else {
            Stage::Busy
        }
    }

    /// Ask the task to stop listening. The phase moves on at once, so a
    /// second press before the task has noticed is not a second finish.
    fn finish(&mut self) {
        self.phase = Phase::Transcribing;
        let _ = self.inbox.send(Msg::Finish);
    }
}

/// The dictation key went down (`pressed`) or came up. `token` is the
/// activation token the desktop sent with it, on Wayland: what lets the pill
/// take the keyboard for the moment a paste needs it.
pub fn key(app: &AppHandle, pressed: bool, token: Option<String>) {
    // Read before the lock: only a press can start anything, and the hotkeys
    // take a lock of their own.
    let config = pressed.then(load_config);
    let releases = app.state::<Hotkeys>().reports_releases();
    let now = Instant::now();
    let dictation = app.state::<VoiceTyping>();
    let mut inner = dictation.lock();
    if releases && !first_press(&mut inner.key_down, pressed) {
        return;
    }
    let stage = inner.session.as_ref().map_or(Stage::Idle, Session::stage);
    let mode = inner.session.as_ref().map_or(KeyMode::Toggle, |s| s.mode);
    let act = decide(mode, stage, pressed, now);
    // The newest token is the one most likely still good. One from an earlier
    // dictation never is, so a dictation starts with its own or none.
    if token.is_some() || act == Act::Start {
        inner.token = token;
    }
    match act {
        Act::Nothing => {}
        Act::Finish => {
            if let Some(session) = inner.session.as_mut() {
                session.finish();
            }
        }
        Act::HandsFree => {
            let Some(session) = inner.session.as_mut() else { return };
            session.hands_free = true;
            session.down_since = None;
            let (seq, phase) = (session.seq, session.phase);
            drop(inner);
            emit(app, seq, phase, None, None);
        }
        Act::Start => {
            let Some(mut config) = config.filter(|c| c.enabled) else { return };
            // Holding a key to talk needs to hear it let go. Where the
            // desktop does not say, the key starts and stops as in toggle.
            if !releases {
                config.mode = KeyMode::Toggle;
            }
            let held = config.mode == KeyMode::Hold;
            let seq = dictation.seq.fetch_add(1, Ordering::SeqCst) + 1;
            let (inbox, messages) = unbounded_channel();
            inner.session = Some(Session {
                seq,
                mode: config.mode,
                phase: Phase::Starting,
                hands_free: !held,
                down_since: held.then_some(now),
                shortcut: config.shortcut.clone(),
                inbox: inbox.clone(),
                task: None,
            });
            // A failure still on the pill is over: its text is not going to
            // be copied now.
            let abandoned = inner.pending.take();
            drop(inner);
            if let Some(Pending { assist: Some(id), .. }) = abandoned {
                let app = app.clone();
                tauri::async_runtime::spawn(
                    async move { settle(&app, Some(&id), "cancelled").await },
                );
            }
            let task = tauri::async_runtime::spawn(run(app.clone(), seq, config, inbox, messages));
            if let Some(session) = dictation.lock().session.as_mut()
                && session.seq == seq
            {
                session.task = Some(task);
            }
        }
    }
}

// ---- what the pill shows -------------------------------------------------------------

/// Show `phase` on the pill and remember it, for a dictation that is still
/// the current one. `false` means it is not -- it was cancelled, or another
/// has begun -- and whatever the caller was doing is no longer wanted.
///
/// A phase earlier than the one the session is in is not shown: the key may
/// already have asked for the end while the microphone was still opening.
/// Done and Failed end the session.
fn emit(
    app: &AppHandle,
    seq: u64,
    phase: Phase,
    message: Option<String>,
    text: Option<String>,
) -> bool {
    // Asked of the other states before the lock is taken, so none of them is
    // ever waited for with it held.
    let key = app.state::<Hotkeys>().status(app, Binding::VoiceTyping).shortcut;
    let pill = pill::mode(app);
    let dictation = app.state::<VoiceTyping>();
    let mut inner = dictation.lock();
    let Some(session) = inner.session.as_mut().filter(|s| s.seq == seq) else { return false };
    if phase < session.phase {
        return true;
    }
    session.phase = phase;
    let state = TypingState {
        seq,
        phase,
        mode: session.mode,
        hands_free: session.hands_free,
        key: if key.is_empty() { session.shortcut.clone() } else { key },
        message,
        text,
        pill,
    };
    if matches!(phase, Phase::Done | Phase::Failed) {
        inner.session = None;
    }
    inner.last = Some(state.clone());
    let _ = app.emit_to(pill::LABEL, STATE_EVENT, &state);
    true
}

/// Take the pill down, if it is still showing dictation `seq`.
fn dismiss(app: &AppHandle, seq: u64) {
    let mine = {
        let mut inner = locked(app);
        let mine = inner.last.as_ref().is_some_and(|s| s.seq == seq);
        if mine {
            inner.last = None;
        }
        mine
    };
    if mine {
        pill::dismiss(app);
    }
}

/// Show that nothing was typed, and why; then take the pill down after
/// `linger` unless it has been dealt with by then.
async fn fail(app: &AppHandle, seq: u64, message: &str, text: Option<String>, linger: Duration) {
    if !emit(app, seq, Phase::Failed, Some(message.to_string()), text) {
        return;
    }
    tokio::time::sleep(linger).await;
    let abandoned = {
        let mut inner = locked(app);
        let still_up =
            inner.last.as_ref().is_some_and(|s| s.seq == seq && s.phase == Phase::Failed);
        if !still_up {
            return;
        }
        inner.pending.take_if(|p| p.seq == seq)
    };
    dismiss(app, seq);
    if let Some(Pending { assist, .. }) = abandoned {
        settle(app, assist.as_deref(), "cancelled").await;
    }
}

/// Keep what could not be typed until the person has chosen what becomes of
/// it. `false` when the dictation is no longer the current one.
fn hold(app: &AppHandle, seq: u64, text: String, assist: Option<String>) -> bool {
    let mut inner = locked(app);
    let current = inner.session.as_ref().is_some_and(|s| s.seq == seq);
    if current {
        inner.pending = Some(Pending { seq, text, assist });
    }
    current
}

async fn settle(app: &AppHandle, assist_id: Option<&str>, outcome: &str) {
    let Some(id) = assist_id else { return };
    let session = app.state::<AppState>().session().as_session();
    let args = json!({ "id": id, "outcome": outcome });
    if let Err(e) = session.call(Ctx::local(), "settle_writing_assist", args).await {
        tracing::debug!(error = %e, outcome, "could not record what became of a dictation");
    }
}

// ---- the level ------------------------------------------------------------------------

/// How loud a block of microphone audio is, from 0 to 1, on a dB scale
/// because that is how loudness is heard: -55 dBFS is nothing, -10 is as
/// loud as the meter gets.
fn level(block: &[i16]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let mean =
        block.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / block.len() as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let db = 10.0 * mean.log10() as f32;
    ((db - QUIET_DB) / (LOUD_DB - QUIET_DB)).clamp(0.0, 1.0)
}

/// Thins what the microphone reports to what the pill can use, keeping the
/// loudest of each stretch so a syllable between two updates still shows.
struct Meter {
    last: Instant,
    peak: f32,
}

impl Meter {
    fn new(now: Instant) -> Self {
        Self { last: now, peak: 0.0 }
    }

    /// The level to send now, if it is time to send one.
    fn hear(&mut self, block: &[i16], now: Instant) -> Option<f32> {
        self.peak = self.peak.max(level(block));
        if now.saturating_duration_since(self.last) < LEVEL_EVERY {
            return None;
        }
        self.last = now;
        Some(std::mem::take(&mut self.peak))
    }
}

// ---- the work -------------------------------------------------------------------------

/// One dictation from the microphone to the field, on a task of its own so
/// that the key never waits for any of it. Cancelling aborts the task, which
/// drops the microphone and the transcription with it.
async fn run(
    app: AppHandle,
    seq: u64,
    config: Config,
    inbox: UnboundedSender<Msg>,
    messages: UnboundedReceiver<Msg>,
) {
    let Some(heard) = hear(&app, seq, inbox, messages).await else { return };
    let text = heard.trim().to_string();
    if text.is_empty() {
        fail(&app, seq, "Nothing was heard.", None, NOTHING_LINGER).await;
        return;
    }
    if !emit(&app, seq, Phase::Writing, None, None) {
        return;
    }
    let session = app.state::<AppState>().session().as_session();
    let args = json!({ "text": text, "clean": config.clean });
    let assist = match session.call(Ctx::local(), "dictate", args).await {
        Ok(assist) => assist,
        Err(e) => {
            // What was heard is still worth having. A model that was asked
            // and failed (`quick`) fails with a line about HTTP that means
            // nothing on a pill; it goes to the log, and the pill says what it
            // means. A locked vault or a medium model that is not set up is
            // said as the service says it, which names where to fix it.
            tracing::info!(error = %e, "a dictation could not be written up");
            let message = if e.code == codes::QUICK {
                "The model could not write it up. You can copy what was heard.".to_string()
            } else {
                e.message
            };
            if hold(&app, seq, text.clone(), None) {
                fail(&app, seq, &message, Some(text), FAILURE_LINGER).await;
            }
            return;
        }
    };
    let id = assist["id"].as_str().map(str::to_string);
    let result = assist["result"].as_str().unwrap_or_default().trim().to_string();
    if result.is_empty() {
        settle(&app, id.as_deref(), "cancelled").await;
        fail(&app, seq, "Nothing to type.", None, NOTHING_LINGER).await;
        return;
    }
    // Held from here, so that cancelling during the paste settles the record.
    if !hold(&app, seq, result.clone(), id.clone()) {
        settle(&app, id.as_deref(), "cancelled").await;
        return;
    }
    let token = locked(&app).token.clone();
    match paste::paste(&app, &result, token).await {
        Ok(()) => {
            let shown = emit(&app, seq, Phase::Done, None, None);
            let typed = locked(&app).pending.take_if(|p| p.seq == seq);
            if typed.is_some() {
                settle(&app, id.as_deref(), "replaced").await;
            }
            if shown {
                tokio::time::sleep(DONE_LINGER).await;
                dismiss(&app, seq);
            }
        }
        Err(why) => {
            tracing::info!(error = %why, "a dictation could not be typed");
            // On Wayland the paste hid the pill to hand the keyboard back
            // before it found it could not press anything; the failure and
            // its Copy button need it up again.
            pill::present(&app);
            fail(&app, seq, &why, Some(result), COPY_LINGER).await;
        }
    }
}

/// Everything from the key to the words: open the microphone, listen, stop,
/// and take what was heard. `None` when it ended some other way -- cancelled,
/// or failed, in which case the pill already says so.
///
/// The listening is the same dictation session mail's write-by-voice uses
/// (`karmayoga_service::speech::dictate`): it ends on Stop, after 15 seconds
/// of quiet or after three minutes, and keeps the last words of somebody cut
/// off mid-sentence. This adds the microphone opening before the session, so
/// that what is said while the speech model loads is kept, and the meter.
async fn hear(
    app: &AppHandle,
    seq: u64,
    inbox: UnboundedSender<Msg>,
    mut messages: UnboundedReceiver<Msg>,
) -> Option<String> {
    if !emit(app, seq, Phase::Starting, None, None) {
        return None;
    }
    pill::present(app);
    let state = app.state::<AppState>();
    if state.is_remote() {
        let refusal = "Voice typing uses this computer's microphone and speech model, so it is not available while this window is looking at a vault on another computer.";
        fail(app, seq, refusal, None, FAILURE_LINGER).await;
        return None;
    }
    // One microphone: the slot mail's write-by-voice takes, so that voice
    // mode, a recording, listening or a dictation here and this refuse each
    // other both ways. Given back when this is over, however it ends -- a
    // cancel drops this future, and the claim with it.
    if let Err(e) = crate::dictation::claim_for(&state, pill::LABEL) {
        fail(app, seq, &e.message, None, FAILURE_LINGER).await;
        return None;
    }
    let _claim = Claim(app.clone());
    // The microphone would hear the speakers.
    crate::listen::stop_listening(&state);

    // The microphone first: what is said while the speech model loads is
    // kept, and the session's opening goes on beside it -- stopped with the
    // rest of this if it is cancelled.
    let service = state.service();
    let opening = inbox.clone();
    let _opening = AbortOnDrop(tauri::async_runtime::spawn(async move {
        let opened = match service.dictation_open().await {
            Ok(DictationLink { input, mut events }) => {
                // What the session says goes through the one inbox, so the
                // loop below waits on one thing. The forwarder ends with the
                // session, or when nobody is listening any more.
                let forward = opening.clone();
                tauri::async_runtime::spawn(async move {
                    while let Some(event) = events.recv().await {
                        if forward.send(Msg::Session(event)).is_err() {
                            break;
                        }
                    }
                });
                Ok(input)
            }
            Err(e) => Err(e),
        };
        let _ = opening.send(Msg::Opened(opened));
    }));
    let sink: EngineSink = Arc::new(move |event| match event {
        EngineEvent::Microphone(block) => {
            let _ = inbox.send(Msg::Audio(block));
        }
        EngineEvent::Failed(message) => {
            let _ = inbox.send(Msg::Trouble(message));
        }
        EngineEvent::Played(_) => {}
    });
    let options = EngineOptions { microphone: true, speaker: false, echo_cancellation: false };
    let mut engine = match AudioEngine::start(options, sink).await {
        Ok(engine) => Some(engine),
        Err(e) => {
            fail(app, seq, &e.to_string(), None, FAILURE_LINGER).await;
            return None;
        }
    };
    if !emit(app, seq, Phase::Listening, None, None) {
        return None;
    }

    let mut session: Option<UnboundedSender<DictationInput>> = None;
    let mut backlog: Vec<i16> = Vec::new();
    let mut meter = Meter::new(Instant::now());
    // When the key said stop, the microphone runs on for `TAIL` -- what the
    // device had heard of the last word and not yet handed over -- and then
    // closes; once it has, and the session is open, the session is told.
    let mut close_at: Option<tokio::time::Instant> = None;
    let mut stop_sent = false;
    let ended = loop {
        let wait = match close_at {
            // Waiting out the tail.
            Some(at) if engine.is_some() => at,
            // Told to stop and waiting for the words: not for ever.
            _ if stop_sent => tokio::time::Instant::now() + FINISH_TIMEOUT,
            // Listening, or waiting for the model to load however long it
            // takes: the key or the session ends that.
            _ => tokio::time::Instant::now() + Duration::from_secs(24 * 60 * 60),
        };
        let message = match tokio::time::timeout_at(wait, messages.recv()).await {
            Ok(message) => message?,
            Err(_) if engine.is_some() => {
                // The tail is in: close the microphone and hand on whatever
                // it delivered before it closed.
                drop(engine.take());
                while let Ok(Msg::Audio(block)) = messages.try_recv() {
                    send_audio(&session, &mut backlog, block);
                }
                if let Some(input) = &session {
                    let _ = input.send(DictationInput::Stop);
                    stop_sent = true;
                }
                continue;
            }
            Err(_) => {
                tracing::warn!("the last of a voice typing was not worked out in time");
                fail(app, seq, "The speech model did not finish in time.", None, FAILURE_LINGER)
                    .await;
                return None;
            }
        };
        match message {
            Msg::Finish => {
                if close_at.is_none() {
                    close_at = Some(tokio::time::Instant::now() + TAIL);
                    if !emit(app, seq, Phase::Transcribing, None, None) {
                        return None;
                    }
                }
            }
            Msg::Audio(block) => {
                if engine.is_some()
                    && close_at.is_none()
                    && let Some(loudness) = meter.hear(&block, Instant::now())
                {
                    let _ = app.emit_to(pill::LABEL, LEVEL_EVENT, loudness);
                }
                send_audio(&session, &mut backlog, block);
            }
            Msg::Opened(Ok(input)) => {
                if !backlog.is_empty() {
                    let _ = input.send(DictationInput::Backlog(std::mem::take(&mut backlog)));
                }
                // The key said stop while the model loaded, and the
                // microphone has closed since: what was said is all there.
                if close_at.is_some() && engine.is_none() {
                    let _ = input.send(DictationInput::Stop);
                    stop_sent = true;
                }
                session = Some(input);
            }
            // The microphone is closed before the pill lingers on the
            // failure: nothing is listening to it any more.
            Msg::Opened(Err(e)) => {
                drop(engine.take());
                fail(app, seq, &e.message, None, FAILURE_LINGER).await;
                return None;
            }
            Msg::Trouble(message) => {
                drop(engine.take());
                fail(app, seq, &message, None, FAILURE_LINGER).await;
                return None;
            }
            Msg::Session(DictationEvent::Trouble { message }) => {
                tracing::warn!(error = %message, "a piece of a voice typing was not transcribed");
            }
            // Ended because it was told to, or by itself -- quiet for long
            // enough, or long enough altogether -- in which case what it
            // heard is typed as if the key had been pressed.
            Msg::Session(DictationEvent::Ended { reason, text, error }) => {
                break (reason, text, error);
            }
            Msg::Session(_) => {}
        }
    };
    drop(engine);
    let (reason, text, error) = ended;
    match reason {
        DictationEnd::Stopped | DictationEnd::Silence | DictationEnd::TimeLimit => Some(text),
        DictationEnd::Cancelled => None,
        DictationEnd::Failed => {
            let message = error.unwrap_or_else(|| "The speech model failed.".to_string());
            fail(app, seq, &message, None, FAILURE_LINGER).await;
            None
        }
    }
}

/// Microphone audio to the session, or kept for it until it is open.
fn send_audio(
    session: &Option<UnboundedSender<DictationInput>>,
    backlog: &mut Vec<i16>,
    block: Vec<i16>,
) {
    match session {
        Some(input) => {
            let _ = input.send(DictationInput::Audio(block));
        }
        None => backlog.extend(block),
    }
}

/// Holds the dictation slot -- the one microphone -- while voice typing
/// listens, and gives it back when dropped. See `crate::dictation::claim_for`.
struct Claim(AppHandle);

impl Drop for Claim {
    fn drop(&mut self) {
        crate::dictation::give_back(&self.0.state::<AppState>());
    }
}

/// A task that stops when this is dropped: the speech model's loading, when a
/// voice typing is cancelled before it is ready.
struct AbortOnDrop(tauri::async_runtime::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---- commands -------------------------------------------------------------------

/// What the pill shows right now, for a pill that has only just loaded.
#[tauri::command]
pub async fn voice_typing_state(app: AppHandle) -> CommandResult<Option<TypingState>> {
    Ok(locked(&app).last.clone())
}

/// Stop listening and type what was said -- the pill's stop button.
#[tauri::command]
pub async fn voice_typing_stop(app: AppHandle) -> CommandResult<()> {
    if let Some(session) = locked(&app).session.as_mut()
        && session.phase <= Phase::Listening
    {
        session.finish();
    }
    Ok(())
}

/// End whatever there is, wherever it has got to, and take the pill down.
async fn cancel(app: &AppHandle) {
    let (task, pending) = {
        let mut inner = locked(app);
        inner.last = None;
        (inner.session.take().and_then(|s| s.task), inner.pending.take())
    };
    if let Some(task) = task {
        task.abort();
    }
    pill::dismiss(app);
    if let Some(Pending { assist, .. }) = pending {
        settle(app, assist.as_deref(), "cancelled").await;
    }
}

/// Throw the dictation away, typing nothing -- the pill's cancel button.
#[tauri::command]
pub async fn voice_typing_cancel(app: AppHandle) -> CommandResult<()> {
    cancel(&app).await;
    Ok(())
}

/// Put a failed dictation's text on the clipboard -- the pill's Copy button.
#[tauri::command]
pub async fn voice_typing_copy(app: AppHandle) -> CommandResult<()> {
    let wanted = {
        let inner = locked(&app);
        inner.pending.as_ref().map(|p| (p.seq, p.text.clone()))
    };
    let Some((seq, text)) = wanted else { return Ok(()) };
    paste::copy(&app, &text).await.map_err(|e| CommandError::new("voice-typing", e))?;
    let copied = locked(&app).pending.take_if(|p| p.seq == seq);
    if let Some(Pending { assist, .. }) = copied {
        settle(&app, assist.as_deref(), "copied").await;
    }
    dismiss(&app, seq);
    Ok(())
}

/// Close the pill and bring the main window forward -- for a dictation that
/// failed because the vault is locked or no model is set up.
#[tauri::command]
pub async fn voice_typing_open_main(app: AppHandle) -> CommandResult<()> {
    cancel(&app).await;
    crate::tray::raise_window(&app);
    Ok(())
}

/// What the settings pane draws for dictation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTypingSettings {
    pub enabled: bool,
    /// The combination as chosen here -- what is offered to the desktop.
    pub shortcut: String,
    pub mode: KeyMode,
    pub clean: bool,
    pub key: KeyStatus,
    /// `wayland`, `x11`, `windows` or `macos`.
    pub platform: &'static str,
}

fn platform(app: &AppHandle) -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if crate::writing::wayland_session(app) {
        "wayland"
    } else {
        "x11"
    }
}

fn settings(app: &AppHandle) -> VoiceTypingSettings {
    let config = load_config();
    VoiceTypingSettings {
        enabled: config.enabled,
        shortcut: config.shortcut,
        mode: config.mode,
        clean: config.clean,
        key: app.state::<Hotkeys>().status(app, Binding::VoiceTyping),
        platform: platform(app),
    }
}

#[tauri::command]
pub async fn voice_typing_settings(app: AppHandle) -> CommandResult<VoiceTypingSettings> {
    Ok(settings(&app))
}

/// Switch dictation on or off, or change its key, how the key works, or
/// whether what is heard is written up.
#[tauri::command]
pub async fn set_voice_typing_settings(
    app: AppHandle,
    enabled: bool,
    shortcut: Option<String>,
    mode: Option<KeyMode>,
    clean: Option<bool>,
) -> CommandResult<VoiceTypingSettings> {
    let mut config = load_config();
    if let Some(shortcut) = shortcut {
        hotkey::parse(&shortcut).map_err(|e| CommandError::new("invalid", e))?;
        config.shortcut = shortcut.trim().to_string();
    }
    config.enabled = enabled;
    config.mode = mode.unwrap_or(config.mode);
    config.clean = clean.unwrap_or(config.clean);
    let claim = if enabled {
        Some(hotkey::parse(&config.shortcut).map_err(|e| CommandError::new("invalid", e))?)
    } else {
        None
    };
    // One key cannot start two things -- while both are switched on.
    if claim.is_some() && crate::writing::configured_shortcut() == claim {
        return Err(CommandError::new("invalid", "that is already writing help's shortcut"));
    }
    save_config(&config).map_err(|e| CommandError::new("voice-typing", e))?;
    let hotkeys = app.state::<Hotkeys>();
    let answer = hotkeys.answer();
    hotkeys.set_voice_typing(&app, claim)?;
    answer.await;
    if !enabled {
        cancel(&app).await;
    }
    Ok(settings(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(hands_free: bool, down_since: Option<Instant>) -> Stage {
        Stage::Listening { hands_free, down_since }
    }

    #[test]
    fn the_level_is_nothing_when_quiet_and_full_when_loud() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0; 480]), 0.0);
        // -55 dBFS is an amplitude of about 58 of 32768.
        assert!(level(&[58; 480]) < 0.02);
        assert_eq!(level(&[i16::MAX; 480]), 1.0);
        // About -32.5 dBFS: halfway on a dB scale.
        let middle = level(&[(32768.0 * 10f32.powf(-32.5 / 20.0)) as i16; 480]);
        assert!((middle - 0.5).abs() < 0.02, "{middle}");
    }

    #[test]
    fn the_meter_reports_the_loudest_of_each_stretch() {
        let start = Instant::now();
        let mut meter = Meter::new(start);
        let loud = [i16::MAX; 480];
        assert_eq!(meter.hear(&loud, start + Duration::from_millis(10)), None);
        let quiet = [0; 480];
        assert_eq!(meter.hear(&quiet, start + Duration::from_millis(60)), Some(1.0));
        assert_eq!(meter.hear(&quiet, start + Duration::from_millis(80)), None);
        assert_eq!(meter.hear(&quiet, start + Duration::from_millis(120)), Some(0.0));
    }

    #[test]
    fn toggle_starts_on_a_press_and_ends_on_the_next() {
        let now = Instant::now();
        assert_eq!(decide(KeyMode::Toggle, Stage::Idle, true, now), Act::Start);
        assert_eq!(decide(KeyMode::Toggle, Stage::Idle, false, now), Act::Nothing);
        assert_eq!(decide(KeyMode::Toggle, held(true, None), true, now), Act::Finish);
        assert_eq!(decide(KeyMode::Toggle, held(true, None), false, now), Act::Nothing);
    }

    #[test]
    fn nothing_the_key_does_changes_a_dictation_that_has_stopped_listening() {
        let now = Instant::now();
        for mode in [KeyMode::Toggle, KeyMode::Hold] {
            for pressed in [true, false] {
                assert_eq!(decide(mode, Stage::Busy, pressed, now), Act::Nothing);
            }
        }
    }

    #[test]
    fn hold_ends_when_the_key_is_let_go_after_a_while() {
        let down = Instant::now();
        let stage = held(false, Some(down));
        assert_eq!(decide(KeyMode::Hold, stage, false, down + TAP), Act::Finish);
        assert_eq!(decide(KeyMode::Hold, stage, false, down + Duration::from_secs(5)), Act::Finish);
    }

    #[test]
    fn hold_tapped_goes_on_listening_until_the_next_press() {
        let down = Instant::now();
        let stage = held(false, Some(down));
        let up = down + Duration::from_millis(120);
        assert_eq!(decide(KeyMode::Hold, stage, false, up), Act::HandsFree);
        let free = held(true, None);
        assert_eq!(decide(KeyMode::Hold, free, false, up), Act::Nothing);
        assert_eq!(decide(KeyMode::Hold, free, true, up), Act::Finish);
    }

    #[test]
    fn a_press_while_the_key_is_down_is_the_keyboard_repeating() {
        let mut down = false;
        assert!(first_press(&mut down, true), "the first press counts");
        assert!(!first_press(&mut down, true), "the repeats do not");
        assert!(!first_press(&mut down, true));
        assert!(first_press(&mut down, false), "the release counts");
        assert!(first_press(&mut down, true), "and so does the next press");
    }

    #[test]
    fn held_after_a_release_that_went_missing_a_press_ends_it() {
        let down = Instant::now();
        let stage = held(false, Some(down));
        assert_eq!(decide(KeyMode::Hold, stage, true, down + Duration::from_secs(3)), Act::Finish);
    }

    #[test]
    fn a_release_nobody_saw_the_press_of_does_nothing() {
        let now = Instant::now();
        assert_eq!(decide(KeyMode::Hold, held(false, None), false, now), Act::Nothing);
    }
}
