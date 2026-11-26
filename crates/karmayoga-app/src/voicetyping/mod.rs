//! Voice typing: dictation into any text field on the desktop.
//!
//! Press the voice typing key (or hold it), say what you want to type, and it is
//! typed into whatever text field had the keyboard: heard by the speech model
//! chosen in Settings → Models, written up on the medium model -- filler out,
//! the speaker's own corrections applied, punctuated -- and pasted there. A
//! card in the panel ([`crate::panel`]) says what is happening: listening,
//! with the words as they are heard and a level meter; writing; done; or, when
//! nothing could be typed, why, with the text and a Copy button. See
//! `docs/plans/voice-typing.md` and `docs/plans/panel.md`.
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
//! keyboard while it listens: the text goes to the field that has it. The card
//! is the panel's, which never takes the keyboard; the one window of voice
//! typing's own is the helper that sets the clipboard on Wayland, shown only
//! for that moment (see [`pill`]).

pub mod paste;
pub mod pill;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use karmayoga_service::ctx::Ctx;
use karmayoga_service::error::{CommandError, CommandResult, codes};
use karmayoga_service::speech::detect::Tuning;
use karmayoga_service::speech::dictate::{
    DictationEnd, DictationEvent, DictationInput, DictationLink, DictationState,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::Shortcut;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
use crate::hotkey::{self, Binding, Hotkeys, KeyStatus};
use crate::panel::{self, Action, Block, Kind, Mode, Owner, Panel, Speaker, Tone, Turn};
use crate::state::AppState;

/// The id of voice typing's card in the panel. There is one microphone, so one
/// dictation at a time and never more than one card.
const CARD: &str = "voice-typing";

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
    /// Listening hands-free (toggle mode, or hold mode after a tap), stop
    /// and type once the person has been quiet this many seconds after
    /// saying something. `0` waits for the key. Never while the key is held:
    /// letting go is the end then.
    pub finish_after_pause: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            shortcut: DEFAULT_SHORTCUT.into(),
            mode: KeyMode::Toggle,
            clean: true,
            finish_after_pause: DEFAULT_PAUSE_SECONDS,
        }
    }
}

/// How long a pause finishes a dictation unless somebody chooses otherwise:
/// long enough for a breath and a comma, short enough that stopping talking
/// feels like the end of it.
const DEFAULT_PAUSE_SECONDS: f32 = 2.0;

/// The longest pause that may be chosen. Past this, pressing the key is
/// quicker, and the session's own quiet limit is not far off.
const MAX_PAUSE_SECONDS: f32 = 10.0;

/// What the dictation session is asked to wait after somebody's last word
/// is heard to end, for a pause of `seconds` in all -- `None` for none. The
/// detector reports the end of what was said only after its own quiet
/// ([`Tuning::DICTATION`]), so that much of the pause has passed already; a
/// pause shorter than that is the detector's.
fn pause_after_speech(seconds: f32) -> Option<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    let after = seconds.min(MAX_PAUSE_SECONDS) - Tuning::DICTATION.end_of_turn_seconds;
    Some(Duration::from_secs_f32(after.max(0.0)))
}

const FILE: &str = "voice-typing.json";

fn save_config(config: &Config) -> Result<(), String> {
    crate::machine_config::save(FILE, config)
}

pub fn load_config() -> Config {
    crate::machine_config::load(FILE)
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

/// The talk key's shortcut while the talk key is switched on, or `None` when
/// it is off or its setting no longer parses. What voice typing's and writing
/// help's setters check a new key against: the talk key's own
/// [`crate::talk::configured_shortcut`] gives way to theirs at startup, so it
/// is not the answer to "does somebody have this key" -- and asking it would
/// have the three ask one another.
pub(crate) fn talk_shortcut() -> Option<Shortcut> {
    talk_shortcut_of(&crate::talk::load_config())
}

fn talk_shortcut_of(config: &crate::talk::Config) -> Option<Shortcut> {
    if !config.enabled {
        return None;
    }
    hotkey::parse(&config.shortcut).ok()
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

// ---- what the card shows -------------------------------------------------------

/// Where a dictation has got to, in the order it gets there: a phase never
/// goes back to an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    /// The microphone is opening and the speech model loading. What is said
    /// meanwhile is kept.
    Starting,
    /// Hearing what is said.
    Listening,
    /// Stopped listening; the last words are being worked out.
    Transcribing,
    /// The medium model is writing it up.
    Writing,
    /// Typed. The card goes a moment later.
    Done,
    /// Nothing typed: `message` says why, and `text` holds whatever there
    /// was to type, for the card's Copy button.
    Failed,
}

/// What a dictation's card says: everything [`card`] draws it from, and kept
/// whole so that anything that changes -- the phase, the words heard, how the
/// key works -- can redraw it.
#[derive(Debug, Clone, PartialEq)]
struct Typing {
    /// Which dictation this is, counted from 1 for the life of the process,
    /// so that a late word of one is not taken for the next.
    seq: u64,
    phase: Phase,
    /// How the key works right now: in hold mode the card says "let go to
    /// finish"; hands-free after a tap, it says "press to finish".
    mode: KeyMode,
    /// Listening hands-free (toggle mode, or hold mode after a tap).
    hands_free: bool,
    /// Hands-free, a pause finishes it ([`Config::finish_after_pause`]).
    pauses: bool,
    /// The key, as it should be written on screen -- the desktop's own
    /// answer where it keeps the binding -- for "press Ctrl+Alt+M to finish".
    key: String,
    /// For `Failed`, why.
    message: Option<String>,
    /// For `Failed`, the text that could not be typed, when there was some.
    text: Option<String>,
    /// Everything the speech model has made of what was said so far.
    heard: String,
}

/// The card for a dictation, as a function of nothing but where it has got
/// to, so that every phase can be tested.
///
/// From the top: what is happening (listening, with the meter in the head;
/// working out the words; writing them up; typed; or not), then what was
/// heard, as one turn that is still coming while the microphone is open --
/// heard, not said to anyone: it is going into another application -- and,
/// when nothing could be typed, the text, whole, to be copied.
fn card(typing: &Typing) -> Panel {
    let cancel = || Action::new("cancel", "Cancel").icon("x");
    let mut panel = Panel {
        tone: Tone::Working,
        actions: vec![cancel()],
        busy: true,
        ..Panel::new(CARD, Owner::VoiceTyping, Kind::VoiceTyping, "Voice typing", "")
    };
    let doing = |mode: Mode, label: Option<&str>| Block::Activity {
        mode,
        label: label.map(str::to_string),
        tool: None,
    };
    // What is happening, then what was heard as one running turn while there
    // is anything. Still coming only while the microphone is open.
    let and_said = |first: Block, pending: bool| {
        let mut blocks = vec![first];
        let words = typing.heard.trim();
        if !words.is_empty() {
            blocks.push(Block::Conversation {
                turns: vec![Turn { who: Speaker::Heard, name: None, text: words.into(), pending }],
            });
        }
        blocks
    };
    match typing.phase {
        Phase::Starting => {
            panel.title = "Getting ready…".into();
            panel.blocks = vec![doing(Mode::Starting, None)];
        }
        Phase::Listening => {
            panel.tone = Tone::Listening;
            panel.title = "Listening".into();
            panel.body = Some(hint(typing));
            panel.blocks = and_said(doing(Mode::Listening, None), true);
            panel.meter = true;
            panel.busy = false;
            panel.actions = vec![Action::new("stop", "Stop").primary().icon("stop"), cancel()];
        }
        Phase::Transcribing => {
            panel.title = "Working out the words…".into();
            panel.blocks = and_said(doing(Mode::Working, Some("Working out the words…")), false);
        }
        Phase::Writing => {
            panel.title = "Writing…".into();
            panel.blocks = and_said(doing(Mode::Thinking, Some("Writing it up…")), false);
        }
        Phase::Done => {
            panel.tone = Tone::Success;
            panel.title = "Typed".into();
            panel.blocks = vec![doing(Mode::Done, None)];
            panel.busy = false;
            panel.actions.clear();
        }
        Phase::Failed => {
            let message = typing.message.as_deref().map(str::trim).filter(|m| !m.is_empty());
            let text = typing.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
            panel.tone = Tone::Trouble;
            panel.title = message.unwrap_or("Nothing was typed.").into();
            panel.blocks = vec![doing(Mode::Failed, None)];
            panel.busy = false;
            panel.actions.clear();
            if let Some(text) = text {
                panel.blocks.push(Block::Quote { label: None, text: text.into() });
                let copy = Action { copy: Some(text.into()), ..Action::new("copy", "Copy") };
                panel.actions.push(copy.primary().icon("copy"));
            } else if message.is_some_and(needs_the_window) {
                panel.actions.push(Action::new("open", "Open Karma Yoga").primary().icon("open"));
            }
            panel.actions.push(Action::new("dismiss", "Dismiss").icon("close"));
        }
    }
    panel
}

/// The small line under "Listening": how to end it, for how the key works
/// right now.
fn hint(typing: &Typing) -> String {
    if typing.mode == KeyMode::Hold && !typing.hands_free {
        "Let go to finish".into()
    } else if typing.pauses {
        format!("Pause or press {} to finish", typing.key)
    } else {
        format!("Press {} to finish", typing.key)
    }
}

/// Whether a failure is one only the main window can put right: the vault is
/// locked, or no model is set up and Settings is where to do it.
fn needs_the_window(message: &str) -> bool {
    let message = message.to_lowercase();
    ["settings", "locked", "unlock"].iter().any(|word| message.contains(word))
}

// ---- the session ------------------------------------------------------------------

/// A key held for less than this was a tap: in hold mode it starts listening
/// hands-free instead of ending a dictation the moment it is let go.
const TAP: Duration = Duration::from_millis(400);

/// How long the last words are waited for once the session has been told to
/// stop. The session gives up on a recogniser sooner than this (its own
/// `FINISH_WAIT`); this is only so a session that never answers cannot hold
/// the card up. Waiting for the speech model to load is not limited by it.
const FINISH_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the microphone stays open after the key says stop: a device hands
/// audio over a little after it hears it, and the end of the last word is in
/// that little.
const TAIL: Duration = Duration::from_millis(150);

/// How long the card stays after what it shows has no button to press.
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

/// The level goes to the card this often, however often the microphone
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
    /// The key or the card's Stop button says that is enough.
    Finish,
    /// The key was tapped in hold mode: listening goes on hands-free, and a
    /// pause may finish it now.
    HandsFree,
}

/// A dictation under way, from the key going down until it is typed or has
/// failed.
struct Session {
    seq: u64,
    /// How the key works for this dictation, as it was when it began.
    mode: KeyMode,
    phase: Phase,
    hands_free: bool,
    /// Hands-free, a pause finishes it ([`Config::finish_after_pause`]).
    pauses: bool,
    /// Hold mode: when the key went down and has not come up since.
    down_since: Option<Instant>,
    /// The configured key, for the card when the desktop says nothing.
    shortcut: String,
    inbox: UnboundedSender<Msg>,
    /// Ends the dictation where it stands. `None` for the moment between
    /// the session existing and its task being spawned.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

/// What could not be typed, kept so the card's Copy button has something to
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
    /// What the card shows, `None` once it has gone.
    last: Option<Typing>,
    /// The latest activation token the key brought. Wayland only.
    token: Option<String>,
    pending: Option<Pending>,
    /// What the key last said while down -- pressed, or the keyboard
    /// repeating it -- with no release since. See [`first_press`].
    down: Option<Edge>,
    /// How many times the key has said anything, so a check made later can
    /// tell whether it has spoken since.
    edges: u64,
}

/// One press of the key, or the keyboard repeating it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edge {
    /// When it got here.
    pub(crate) at: Instant,
    /// When the desktop says it happened, where it says: the portal's own
    /// timestamp, which a busy runtime does not delay.
    pub(crate) stamp: Option<Duration>,
    /// It was the keyboard repeating the key, not the press.
    pub(crate) repeat: bool,
}

/// How the desktop repeats a held key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Only GNOME's setting is read, so elsewhere it is always `Unknown`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum Repeats {
    /// Not known: not GNOME, or GNOME's setting not read yet.
    Unknown,
    /// Switched off: a held key says it is down once.
    Off,
    /// The first repeat `delay` after the press, then one every `interval`.
    Every { delay: Duration, interval: Duration },
}

/// Whether this edge of the key means anything, keeping `down` up to date:
/// a press that comes when the keyboard would repeat a held key -- GNOME
/// sends a held key's activation again and again, the first time after its
/// repeat delay and then every 30 ms or so -- is the repeat, and not a new
/// press. See [`is_repeat`].
///
/// A press at any other time with no release between is a new press whose
/// release went missing. GNOME (50) sends no release when a modifier is let
/// go before the key -- Ctrl+Alt+M with Ctrl up first -- which is how people
/// often let go; taking the key to be down until a release came ignored every
/// press after the first.
pub(crate) fn first_press(
    down: &mut Option<Edge>,
    pressed: bool,
    at: Instant,
    stamp: Option<Duration>,
    repeats: Repeats,
) -> bool {
    if !pressed {
        *down = None;
        return true;
    }
    let repeat = down.as_ref().is_some_and(|last| is_repeat(last, at, stamp, repeats));
    *down = Some(Edge { at, stamp, repeat });
    !repeat
}

/// How far a repeat's timestamp may be from when the keyboard would repeat
/// the key. GNOME's are within a millisecond or two; this is for a desktop
/// under load.
const STAMPED_SLACK: Duration = Duration::from_millis(150);

/// The same, timed by when presses got here rather than by the desktop's
/// own timestamps: either may have waited on a busy runtime.
pub(crate) const ARRIVAL_SLACK: Duration = Duration::from_millis(400);

/// How soon after the key last said it was down a press is taken for a
/// repeat where the desktop's way of repeating is not known: past the
/// common delays before the first repeat (GNOME's 500 ms, KDE's 600 ms).
const UNKNOWN_REPEAT_WITHIN: Duration = Duration::from_millis(1_000);

/// Whether a press at `at` (`stamp`, where the desktop gave one) is the
/// keyboard repeating the key that last said it was down at `last`: it
/// comes one repeat delay after the press, or one interval after the last
/// repeat.
fn is_repeat(last: &Edge, at: Instant, stamp: Option<Duration>, repeats: Repeats) -> bool {
    let (gap, slack) = match (last.stamp, stamp) {
        (Some(then), Some(now)) => (now.saturating_sub(then), STAMPED_SLACK),
        _ => (at.saturating_duration_since(last.at), ARRIVAL_SLACK),
    };
    let expected = match repeats {
        Repeats::Unknown => return gap < UNKNOWN_REPEAT_WITHIN,
        Repeats::Off => return false,
        Repeats::Every { interval, .. } if last.repeat => interval,
        Repeats::Every { delay, .. } => delay,
    };
    gap + slack >= expected && gap <= expected + slack
}

/// How this desktop repeats a held key, as last read. Kept up to date by
/// [`watch_key_repeat`], so that a key repeating thirty times a second reads
/// no settings.
static REPEATS: Mutex<Repeats> = Mutex::new(Repeats::Unknown);

pub(crate) fn key_repeats() -> Repeats {
    *REPEATS.lock().expect("key repeat lock")
}

/// Read GNOME's keyboard repeat setting, and read it again whenever it
/// changes. Called once, on the main thread, whose loop delivers the
/// changes. Elsewhere than GNOME the repeat stays [`Repeats::Unknown`].
#[cfg(target_os = "linux")]
pub fn watch_key_repeat() {
    use gtk::gio::prelude::*;

    const KEYS: [&str; 3] = ["repeat", "delay", "repeat-interval"];
    if !crate::gnome::running() {
        return;
    }
    let Some(settings) = crate::gnome::settings("org.gnome.desktop.peripherals.keyboard", &KEYS)
    else {
        return;
    };
    fn read(settings: &gtk::gio::Settings) {
        let repeats = if settings.boolean("repeat") {
            Repeats::Every {
                delay: Duration::from_millis(settings.uint("delay").into()),
                interval: Duration::from_millis(settings.uint("repeat-interval").into()),
            }
        } else {
            Repeats::Off
        };
        *REPEATS.lock().expect("key repeat lock") = repeats;
    }
    read(&settings);
    settings.connect_changed(None, |settings, key| {
        if KEYS.contains(&key) {
            read(settings);
        }
    });
    // Watched for the life of the process: the settings object must outlive
    // this call for its changes to be heard.
    std::mem::forget(settings);
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
        // Hands-free, the next press ends it. Held, a press comes after a
        // release that went missing, or is a repeat that [`first_press`] did
        // not recognise (a desktop repeating out of time); either way it is
        // the end, as it would be in toggle mode.
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
/// activation token the desktop sent with it, on Wayland: what lets the paste
/// window take the keyboard for the moment a paste needs it. `stamp` is when the
/// desktop says it happened, where it says.
pub fn key(app: &AppHandle, pressed: bool, token: Option<String>, stamp: Option<Duration>) {
    // Read before the lock: the hotkeys take a lock of their own.
    let releases = app.state::<Hotkeys>().reports_releases();
    let repeats = key_repeats();
    let now = Instant::now();
    // A desktop that stamps nothing may send zero.
    let stamp = stamp.filter(|s| !s.is_zero());
    let dictation = app.state::<VoiceTyping>();
    let mut inner = dictation.lock();
    inner.edges += 1;
    if !first_press(&mut inner.down, pressed, now, stamp, repeats) {
        watch_for_release(app, &inner, repeats);
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
    carry_out(app, &dictation, inner, act, now, releases);
}

/// A key held in hold mode, let go modifier first, says nothing of being let
/// go -- but its repeats stop. Once they have started, a pause in them longer
/// than an interval is taken as the release, when the last repeat came.
///
/// Only once they have started: a desktop that does not repeat, or a key let
/// go before the first repeat, leaves the dictation listening until the next
/// press, as before.
fn watch_for_release(app: &AppHandle, inner: &Inner, repeats: Repeats) {
    let Repeats::Every { interval, .. } = repeats else { return };
    let Some(session) = inner.session.as_ref() else { return };
    let held =
        matches!(session.stage(), Stage::Listening { hands_free: false, down_since: Some(_) });
    if session.mode != KeyMode::Hold || !held {
        return;
    }
    let edge = inner.edges;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(interval + ARRIVAL_SLACK).await;
        let dictation = app.state::<VoiceTyping>();
        let mut inner = dictation.lock();
        if inner.edges != edge {
            return;
        }
        let Some(last) = inner.down.take() else { return };
        inner.edges += 1;
        let stage = inner.session.as_ref().map_or(Stage::Idle, Session::stage);
        let mode = inner.session.as_ref().map_or(KeyMode::Toggle, |s| s.mode);
        let act = decide(mode, stage, false, last.at);
        carry_out(&app, &dictation, inner, act, last.at, true);
    });
}

/// Do what the key asked for. `now` is when it asked; `releases`, whether
/// the desktop says when the key is let go.
fn carry_out(
    app: &AppHandle,
    dictation: &VoiceTyping,
    mut inner: std::sync::MutexGuard<'_, Inner>,
    act: Act,
    now: Instant,
    releases: bool,
) {
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
            let _ = session.inbox.send(Msg::HandsFree);
            let (seq, phase) = (session.seq, session.phase);
            drop(inner);
            emit(app, seq, phase, None, None);
        }
        Act::Start => {
            // Read only for a press that starts something, not for every
            // repeat of a held key.
            let mut config = load_config();
            if !config.enabled {
                return;
            }
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
                pauses: pause_after_speech(config.finish_after_pause).is_some(),
                down_since: held.then_some(now),
                shortcut: config.shortcut.clone(),
                inbox: inbox.clone(),
                task: None,
            });
            // A failure still on the card is over: its text is not going to
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

// ---- what the card shows -------------------------------------------------------------

/// Show `phase` on the card and remember it, for a dictation that is still
/// the current one. `false` means it is not -- it was cancelled, or another
/// has begun -- and whatever the caller was doing is no longer wanted.
///
/// A phase earlier than the one the session is in is not shown: the key may
/// already have asked for the end while the microphone was still opening.
/// Done and Failed end the session.
///
/// The panel is told with the lock held. A cancel takes the card down under
/// the same lock, so the card cannot be put back after it by a dictation that
/// was cancelled between being checked and being shown. The panel itself
/// shows nothing that has not changed, so a phase said again -- the key
/// working differently once a tap has made it hands-free -- redraws only when
/// the card reads differently.
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
    let dictation = app.state::<VoiceTyping>();
    let mut inner = dictation.lock();
    let Some(session) = inner.session.as_mut().filter(|s| s.seq == seq) else { return false };
    if phase < session.phase {
        return true;
    }
    session.phase = phase;
    let (mode, hands_free, pauses) = (session.mode, session.hands_free, session.pauses);
    let key = if key.is_empty() { session.shortcut.clone() } else { key };
    // What was heard stays with the dictation from one phase to the next.
    let heard = inner.last.as_ref().filter(|t| t.seq == seq).map(|t| t.heard.clone());
    let typing = Typing {
        seq,
        phase,
        mode,
        hands_free,
        pauses,
        key,
        message,
        text,
        heard: heard.unwrap_or_default(),
    };
    if matches!(phase, Phase::Done | Phase::Failed) {
        inner.session = None;
    }
    panel::show(app, card(&typing));
    inner.last = Some(typing);
    true
}

/// Show what the speech model has made of what was said so far, on the card
/// of dictation `seq` if it is still up and has not been typed. Said once per
/// utterance, so this is rare; the same words again change nothing.
fn heard(app: &AppHandle, seq: u64, words: String) {
    let mut inner = locked(app);
    let Some(typing) = inner.last.as_mut().filter(|t| t.seq == seq && t.phase <= Phase::Writing)
    else {
        return;
    };
    if typing.heard == words {
        return;
    }
    typing.heard = words;
    panel::show(app, card(typing));
}

/// Take the card down, if it is still showing dictation `seq`. Under the lock,
/// as [`emit`] shows it: a dictation begun a moment later has the same card
/// id, and must not have its card taken down by the last one's.
fn dismiss(app: &AppHandle, seq: u64) {
    let mut inner = locked(app);
    if inner.last.as_ref().is_some_and(|s| s.seq == seq) {
        inner.last = None;
        panel::dismiss(app, CARD);
    }
}

/// Show that nothing was typed, and why; then take the card down after
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

/// Thins what the microphone reports to what the card can use, keeping the
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
    let hands_free = config.mode != KeyMode::Hold;
    let pause = pause_after_speech(config.finish_after_pause);
    let heard = match hear(&app, seq, inbox, messages, pause, hands_free).await {
        Ok(Some(heard)) => heard,
        Ok(None) => return,
        // Said only now that `hear` has let go of the microphone: the card
        // lingers on a failure, and the slot held that long refused the next
        // press ("already dictating"), voice mode and a recording.
        Err(why) => {
            fail(&app, seq, &why, None, FAILURE_LINGER).await;
            return;
        }
    };
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
            // nothing on a card; it goes to the log, and the card says what it
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
            fail(&app, seq, &why, Some(result), COPY_LINGER).await;
        }
    }
}

/// Everything from the key to the words: open the microphone, listen, stop,
/// and take what was heard. `None` when it was cancelled or another has
/// begun; an error, for the card, when it failed.
///
/// The listening is the same dictation session mail's write-by-voice uses
/// (`karmayoga_service::speech::dictate`): it ends on Stop, after 15 seconds
/// of quiet or after three minutes, and keeps the last words of somebody cut
/// off mid-sentence. This adds the microphone opening before the session, so
/// that what is said while the speech model loads is kept, and the meter --
/// and, while `hands_free`, asks the session to end on a pause of `pause`
/// after somebody's words ([`pause_after_speech`]): stopping talking is then
/// the end, as the key would be.
async fn hear(
    app: &AppHandle,
    seq: u64,
    inbox: UnboundedSender<Msg>,
    mut messages: UnboundedReceiver<Msg>,
    pause: Option<Duration>,
    mut hands_free: bool,
) -> Result<Option<String>, String> {
    if !emit(app, seq, Phase::Starting, None, None) {
        return Ok(None);
    }
    // Where the paste will need it, the window that sets the clipboard is made
    // now, while the microphone opens, and not when it is wanted.
    pill::ensure(app);
    let state = app.state::<AppState>();
    if state.is_remote() {
        let refusal = "Voice typing uses this computer's microphone and speech model, so it is not available while this window is looking at a vault on another computer.";
        return Err(refusal.to_string());
    }
    // One microphone: the slot mail's write-by-voice takes, so that voice
    // mode, a recording, listening or a dictation here and this refuse each
    // other both ways. Given back when this is over, however it ends -- a
    // cancel drops this future, and the claim with it.
    crate::dictation::claim_for(&state, pill::LABEL).map_err(|e| e.message)?;
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
    let cleanup = crate::microphone::cleanup(&state.service()).await;
    let mut engine = match AudioEngine::start(options, cleanup, sink).await {
        Ok(engine) => Some(engine),
        Err(e) => return Err(e.to_string()),
    };
    if !emit(app, seq, Phase::Listening, None, None) {
        return Ok(None);
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
            Ok(Some(message)) => message,
            Ok(None) => return Ok(None),
            Err(_) if engine.is_some() => {
                // The tail is in: close the microphone and hand on whatever
                // it delivered before it closed -- with the microphone's
                // models on, once they have handed over all they hold.
                if let Some(engine) = engine.take() {
                    engine.finish(crate::audio::engine::FINISH_WITHIN).await;
                }
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
                return Err("The speech model did not finish in time.".to_string());
            }
        };
        match message {
            Msg::Finish => {
                if close_at.is_none() {
                    close_at = Some(tokio::time::Instant::now() + TAIL);
                    if !emit(app, seq, Phase::Transcribing, None, None) {
                        return Ok(None);
                    }
                }
            }
            Msg::Audio(block) => {
                if engine.is_some()
                    && close_at.is_none()
                    && let Some(loudness) = meter.hear(&block, Instant::now())
                {
                    panel::level(app, CARD, loudness);
                }
                send_audio(&session, &mut backlog, block);
            }
            Msg::Opened(Ok(input)) => {
                if !backlog.is_empty() {
                    let _ = input.send(DictationInput::Backlog(std::mem::take(&mut backlog)));
                }
                if hands_free && pause.is_some() {
                    let _ = input.send(DictationInput::EndAfterPause(pause));
                }
                // The key said stop while the model loaded, and the
                // microphone has closed since: what was said is all there.
                if close_at.is_some() && engine.is_none() {
                    let _ = input.send(DictationInput::Stop);
                    stop_sent = true;
                }
                session = Some(input);
            }
            Msg::HandsFree => {
                hands_free = true;
                if let (Some(input), Some(_)) = (&session, pause) {
                    let _ = input.send(DictationInput::EndAfterPause(pause));
                }
            }
            Msg::Opened(Err(e)) => return Err(e.message),
            Msg::Trouble(message) => return Err(message),
            Msg::Session(DictationEvent::Trouble { message }) => {
                tracing::warn!(error = %message, "a piece of a voice typing was not transcribed");
            }
            Msg::Session(DictationEvent::Heard { text }) => heard(app, seq, text),
            // It stopped listening by itself -- a pause after the words, a
            // long quiet, the time limit -- and hears nothing more: the card
            // stops saying it is listening now, not once the last words are
            // in, and the microphone closes as if the key had been pressed.
            Msg::Session(DictationEvent::State { state: DictationState::Finishing }) => {
                if close_at.is_none() {
                    close_at = Some(tokio::time::Instant::now());
                    if !emit(app, seq, Phase::Transcribing, None, None) {
                        return Ok(None);
                    }
                }
            }
            // Ended because it was told to, or by itself -- a pause after
            // the words, quiet for long enough, or long enough altogether --
            // in which case what it heard is typed as if the key had been
            // pressed.
            Msg::Session(DictationEvent::Ended { reason, text, error }) => {
                break (reason, text, error);
            }
            Msg::Session(_) => {}
        }
    };
    drop(engine);
    let (reason, text, error) = ended;
    match reason {
        DictationEnd::Stopped
        | DictationEnd::Paused
        | DictationEnd::Silence
        | DictationEnd::TimeLimit => Ok(Some(text)),
        DictationEnd::Cancelled => Ok(None),
        DictationEnd::Failed => Err(error.unwrap_or_else(|| "The speech model failed.".into())),
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

// ---- the panel ------------------------------------------------------------------

/// What a press on one of the card's buttons asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    /// Stop listening and type what was said.
    Stop,
    /// Throw the dictation away, typing nothing. Also what dismissing a
    /// failure is: what could not be typed is let go.
    Cancel,
    /// Put what could not be typed on the clipboard. `copied`: the panel has
    /// done it already, as it must on Wayland, where only the window that was
    /// pressed may.
    Copy { copied: bool },
    /// Close the card and bring the main window forward.
    Open,
    /// A button the card does not have, or a stale press.
    Nothing,
}

/// What `action`, pressed on the card, asks for. The ids are those [`card`]
/// gives its buttons.
fn press(action: &str, copied: bool) -> Press {
    match action {
        "stop" => Press::Stop,
        "cancel" | "dismiss" => Press::Cancel,
        "copy" => Press::Copy { copied },
        "open" => Press::Open,
        _ => Press::Nothing,
    }
}

/// A button on voice typing's card was pressed. `copied` says the panel has
/// put the button's text on the clipboard itself. See
/// [`crate::panel::on_action`].
pub fn on_panel_action(app: &AppHandle, action: &str, copied: bool) {
    match press(action, copied) {
        Press::Stop => stop(app),
        Press::Cancel => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { cancel(&app).await });
        }
        Press::Copy { copied } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { copy(&app, copied).await });
        }
        Press::Open => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                cancel(&app).await;
                crate::tray::raise_window(&app);
            });
        }
        Press::Nothing => {
            tracing::debug!(action, "a press on voice typing's card it has no use for")
        }
    }
}

/// Stop listening and type what was said -- the card's Stop button.
fn stop(app: &AppHandle) {
    if let Some(session) = locked(app).session.as_mut()
        && session.phase <= Phase::Listening
    {
        session.finish();
    }
}

/// End whatever there is, wherever it has got to, and take the card down.
async fn cancel(app: &AppHandle) {
    let (task, pending) = {
        let mut inner = locked(app);
        inner.last = None;
        // Down before the lock is let go, so that a dictation begun the
        // instant after has its own card and not this one's going.
        panel::dismiss(app, CARD);
        (inner.session.take().and_then(|s| s.task), inner.pending.take())
    };
    if let Some(task) = task {
        task.abort();
    }
    // A paste cancelled half way has the helper window up with the keyboard.
    pill::release(app);
    if let Some(Pending { assist, .. }) = pending {
        settle(app, assist.as_deref(), "cancelled").await;
    }
}

/// Put a failed dictation's text on the clipboard -- the card's Copy button
/// -- and be done with it. `copied`: the panel has put it there already.
async fn copy(app: &AppHandle, copied: bool) {
    let wanted = {
        let inner = locked(app);
        inner.pending.as_ref().map(|p| (p.seq, p.text.clone()))
    };
    let Some((seq, text)) = wanted else { return };
    if !copied && let Err(why) = paste::copy(app, &text).await {
        tracing::info!(error = %why, "a dictation's text could not be copied");
        // Said on the card, which stays up for the person to try again or to
        // select the text from.
        let inner = locked(app);
        if inner.last.as_ref().is_some_and(|t| t.seq == seq) {
            let note = format!("Could not copy it: {why}.");
            panel::update(app, CARD, |shown| shown.body = Some(note));
        }
        return;
    }
    let taken = locked(app).pending.take_if(|p| p.seq == seq);
    if let Some(Pending { assist, .. }) = taken {
        settle(app, assist.as_deref(), "copied").await;
    }
    dismiss(app, seq);
}

// ---- commands -------------------------------------------------------------------

/// What the settings pane draws for dictation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTypingSettings {
    pub enabled: bool,
    /// The combination as chosen here -- what is offered to the desktop.
    pub shortcut: String,
    pub mode: KeyMode,
    pub clean: bool,
    pub finish_after_pause: f32,
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
        finish_after_pause: config.finish_after_pause,
        key: app.state::<Hotkeys>().status(app, Binding::VoiceTyping),
        platform: platform(app),
    }
}

#[tauri::command]
pub async fn voice_typing_settings(app: AppHandle) -> CommandResult<VoiceTypingSettings> {
    Ok(settings(&app))
}

/// Switch dictation on or off, or change its key, how the key works,
/// whether what is heard is written up, or how long a pause finishes it.
#[tauri::command]
pub async fn set_voice_typing_settings(
    app: AppHandle,
    enabled: bool,
    shortcut: Option<String>,
    mode: Option<KeyMode>,
    clean: Option<bool>,
    finish_after_pause: Option<f32>,
) -> CommandResult<VoiceTypingSettings> {
    let mut config = load_config();
    if let Some(shortcut) = shortcut {
        hotkey::parse(&shortcut).map_err(|e| CommandError::new("invalid", e))?;
        config.shortcut = shortcut.trim().to_string();
    }
    config.enabled = enabled;
    config.mode = mode.unwrap_or(config.mode);
    config.clean = clean.unwrap_or(config.clean);
    if let Some(seconds) = finish_after_pause {
        if !seconds.is_finite() || !(0.0..=MAX_PAUSE_SECONDS).contains(&seconds) {
            return Err(CommandError::new(
                "invalid",
                format!("a pause is between 0 and {MAX_PAUSE_SECONDS} seconds"),
            ));
        }
        config.finish_after_pause = seconds;
    }
    let claim = if enabled {
        Some(hotkey::parse(&config.shortcut).map_err(|e| CommandError::new("invalid", e))?)
    } else {
        None
    };
    // One key cannot start two things -- while both are switched on.
    if claim.is_some() && crate::writing::configured_shortcut() == claim {
        return Err(CommandError::new("invalid", "that is already writing help's shortcut"));
    }
    // The talk key is a fourth that cannot share it. Where the combination is
    // already registered the desktop's claim succeeds without registering
    // anything, so without this the key would go on starting a conversation
    // while this setting said it was voice typing's.
    if claim.is_some() && talk_shortcut() == claim {
        return Err(CommandError::new("invalid", "that is already the talk key's shortcut"));
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

    /// A dictation at `phase`, in toggle mode, with nothing heard yet.
    fn typing(phase: Phase) -> Typing {
        Typing {
            seq: 1,
            phase,
            mode: KeyMode::Toggle,
            hands_free: true,
            pauses: false,
            key: "Ctrl+Alt+M".into(),
            message: None,
            text: None,
            heard: String::new(),
        }
    }

    fn failed(message: Option<&str>, text: Option<&str>) -> Typing {
        Typing {
            message: message.map(str::to_string),
            text: text.map(str::to_string),
            ..typing(Phase::Failed)
        }
    }

    #[test]
    fn a_pause_that_finishes_it_says_so_only_while_nothing_is_held() {
        let pausing = Typing { pauses: true, ..typing(Phase::Listening) };
        assert_eq!(hint(&pausing), "Pause or press Ctrl+Alt+M to finish");
        let held = Typing { mode: KeyMode::Hold, hands_free: false, ..pausing.clone() };
        assert_eq!(hint(&held), "Let go to finish");
        let tapped = Typing { mode: KeyMode::Hold, hands_free: true, ..pausing };
        assert_eq!(hint(&tapped), "Pause or press Ctrl+Alt+M to finish");
    }

    #[test]
    fn the_pause_asked_of_the_session_is_what_the_detector_has_not_waited_already() {
        let detector = Tuning::DICTATION.end_of_turn_seconds;
        let after = |seconds: f32| pause_after_speech(seconds).map(|d| d.as_secs_f32());
        assert_eq!(after(0.0), None);
        assert_eq!(after(-1.0), None);
        assert_eq!(after(f32::NAN), None);
        assert!((after(2.0).unwrap() - (2.0 - detector)).abs() < 1e-3);
        // Shorter than the detector's own wait: that wait is the pause.
        assert_eq!(after(0.5), Some(0.0));
        // Longer than the longest that may be chosen: the longest.
        assert!((after(60.0).unwrap() - (MAX_PAUSE_SECONDS - detector)).abs() < 1e-3);
    }

    #[test]
    fn a_setting_saved_before_pauses_finished_it_gets_the_default() {
        let old: Config = serde_json::from_str(
            r#"{"enabled": true, "shortcut": "Ctrl+Alt+M", "mode": "toggle", "clean": true}"#,
        )
        .unwrap();
        assert_eq!(old.finish_after_pause, DEFAULT_PAUSE_SECONDS);
        let off: Config = serde_json::from_str(r#"{"finishAfterPause": 0}"#).unwrap();
        assert_eq!(pause_after_speech(off.finish_after_pause), None);
    }

    /// The ids of a card's buttons, left to right.
    fn buttons(panel: &Panel) -> Vec<&str> {
        panel.actions.iter().map(|a| a.id.as_str()).collect()
    }

    /// The marks on a card's buttons, left to right.
    fn marks(panel: &Panel) -> Vec<Option<&str>> {
        panel.actions.iter().map(|a| a.icon.as_deref()).collect()
    }

    /// What the card says is happening, and in what words: its first block.
    fn activity(panel: &Panel) -> (Mode, Option<&str>) {
        match panel.blocks.first() {
            Some(Block::Activity { mode, label, tool: None }) => (*mode, label.as_deref()),
            other => panic!("an activity first, with no tool, got {other:?}"),
        }
    }

    /// The turns of the card's conversation, none if it has none.
    fn turns(panel: &Panel) -> Vec<&Turn> {
        panel
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Conversation { turns } => Some(turns.iter()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// What the card quotes, whole, if anything.
    fn quoted(panel: &Panel) -> Option<&str> {
        panel.blocks.iter().find_map(|b| match b {
            Block::Quote { label: None, text } => Some(text.as_str()),
            _ => None,
        })
    }

    fn heard_turn(text: &str, pending: bool) -> Turn {
        Turn { who: Speaker::Heard, name: None, text: text.into(), pending }
    }

    #[test]
    fn the_talk_key_has_its_shortcut_only_while_it_is_on_and_parses() {
        let talk = |enabled, shortcut: &str| crate::talk::Config {
            enabled,
            shortcut: shortcut.into(),
            ..Default::default()
        };
        let default = hotkey::parse(crate::talk::DEFAULT_SHORTCUT).unwrap();
        assert_eq!(talk_shortcut_of(&talk(true, crate::talk::DEFAULT_SHORTCUT)), Some(default));
        assert_eq!(talk_shortcut_of(&talk(true, " Ctrl+Alt+A ")), Some(default));
        assert_eq!(talk_shortcut_of(&talk(false, crate::talk::DEFAULT_SHORTCUT)), None);
        assert_eq!(talk_shortcut_of(&talk(true, "not a key")), None);
        // And it is not voice typing's own.
        assert_ne!(Some(hotkey::parse(DEFAULT_SHORTCUT).unwrap()), Some(default));
    }

    #[test]
    fn the_card_is_voice_typings_and_has_the_id_the_panel_knows_it_by() {
        for phase in [
            Phase::Starting,
            Phase::Listening,
            Phase::Transcribing,
            Phase::Writing,
            Phase::Done,
            Phase::Failed,
        ] {
            let panel = card(&typing(phase));
            assert_eq!(panel.id, "voice-typing");
            assert_eq!(panel.owner, Owner::VoiceTyping);
            assert_eq!(panel.kind, Kind::VoiceTyping);
            assert_eq!(panel.kicker, "Voice typing");
            assert!(panel.count.is_none());
            // Every phase says what is happening, first.
            activity(&panel);
        }
    }

    #[test]
    fn starting_says_so_and_can_only_be_cancelled() {
        let panel = card(&typing(Phase::Starting));
        assert_eq!(panel.tone, Tone::Working);
        assert_eq!(panel.title, "Getting ready…");
        assert_eq!(activity(&panel), (Mode::Starting, None));
        assert!(panel.busy && !panel.meter);
        assert_eq!(buttons(&panel), ["cancel"]);
        assert_eq!(marks(&panel), [Some("x")]);
        assert!(panel.actions.iter().all(|a| !a.primary));
    }

    #[test]
    fn listening_shows_the_meter_how_to_finish_and_stop_beside_cancel() {
        let panel = card(&typing(Phase::Listening));
        assert_eq!(panel.tone, Tone::Listening);
        assert_eq!(panel.title, "Listening");
        assert_eq!(activity(&panel), (Mode::Listening, None));
        assert_eq!(panel.body.as_deref(), Some("Press Ctrl+Alt+M to finish"));
        assert!(panel.meter && !panel.busy);
        assert_eq!(buttons(&panel), ["stop", "cancel"]);
        assert_eq!(marks(&panel), [Some("stop"), Some("x")]);
        let primary: Vec<_> = panel.actions.iter().filter(|a| a.primary).map(|a| &a.id).collect();
        assert_eq!(primary, ["stop"]);
    }

    #[test]
    fn listening_says_nothing_of_words_until_there_are_some() {
        assert!(turns(&card(&typing(Phase::Listening))).is_empty());
        let heard = Typing { heard: "  ".into(), ..typing(Phase::Listening) };
        let panel = card(&heard);
        assert!(turns(&panel).is_empty());
        assert_eq!(panel.blocks.len(), 1, "only what is happening: {:?}", panel.blocks);
    }

    #[test]
    fn listening_shows_everything_heard_as_one_line_still_coming() {
        let heard = Typing { heard: "Dear Sam, thanks for".into(), ..typing(Phase::Listening) };
        let panel = card(&heard);
        assert_eq!(turns(&panel), [&heard_turn("Dear Sam, thanks for", true)]);
    }

    #[test]
    fn the_hint_follows_how_the_key_works() {
        let hold =
            |hands_free| Typing { mode: KeyMode::Hold, hands_free, ..typing(Phase::Listening) };
        // Held down: let go. Tapped, so hands-free: press the key again.
        assert_eq!(hint(&hold(false)), "Let go to finish");
        assert_eq!(hint(&hold(true)), "Press Ctrl+Alt+M to finish");
        // Toggle mode is hands-free from the start.
        assert_eq!(hint(&typing(Phase::Listening)), "Press Ctrl+Alt+M to finish");
        // The key as the desktop writes it, whatever that is.
        let other = Typing { key: "Shift+Ctrl+4".into(), ..typing(Phase::Listening) };
        assert_eq!(card(&other).body.as_deref(), Some("Press Shift+Ctrl+4 to finish"));
    }

    #[test]
    fn working_out_and_writing_keep_the_words_and_stop_the_caret() {
        for (phase, title, doing) in [
            (
                Phase::Transcribing,
                "Working out the words…",
                (Mode::Working, Some("Working out the words…")),
            ),
            (Phase::Writing, "Writing…", (Mode::Thinking, Some("Writing it up…"))),
        ] {
            let heard = Typing { heard: "Dear Sam".into(), ..typing(phase) };
            let panel = card(&heard);
            assert_eq!(panel.tone, Tone::Working);
            assert_eq!(panel.title, title);
            assert_eq!(activity(&panel), doing, "{phase:?}");
            assert!(panel.busy && !panel.meter && panel.body.is_none());
            assert_eq!(turns(&panel), [&heard_turn("Dear Sam", false)]);
            assert_eq!(buttons(&panel), ["cancel"]);
        }
    }

    #[test]
    fn done_is_a_success_with_nothing_left_to_press() {
        let heard = Typing { heard: "Dear Sam".into(), ..typing(Phase::Done) };
        let panel = card(&heard);
        assert_eq!(panel.tone, Tone::Success);
        assert_eq!(panel.title, "Typed");
        assert_eq!(activity(&panel), (Mode::Done, None));
        assert!(!panel.busy && !panel.meter);
        assert!(panel.actions.is_empty() && turns(&panel).is_empty());
    }

    #[test]
    fn a_failure_says_why_and_offers_the_text_to_copy() {
        let why = "the desktop would not let Karma Yoga type here";
        let panel = card(&failed(Some(why), Some("Dear Sam,")));
        assert_eq!(panel.tone, Tone::Trouble);
        assert_eq!(panel.title, why);
        assert_eq!(activity(&panel), (Mode::Failed, None));
        assert!(!panel.busy && !panel.meter);
        assert_eq!(quoted(&panel), Some("Dear Sam,"), "the text, whole, in a well");
        assert!(turns(&panel).is_empty());
        assert_eq!(buttons(&panel), ["copy", "dismiss"]);
        assert_eq!(marks(&panel), [Some("copy"), Some("close")]);
        let copy = &panel.actions[0];
        assert!(copy.primary);
        assert_eq!(copy.copy.as_deref(), Some("Dear Sam,"));
        assert!(!panel.actions[1].primary && panel.actions[1].copy.is_none());
    }

    #[test]
    fn a_failure_with_nothing_to_copy_can_only_be_dismissed() {
        let panel = card(&failed(Some("Nothing was heard."), None));
        assert_eq!(panel.title, "Nothing was heard.");
        assert_eq!(quoted(&panel), None);
        assert_eq!(buttons(&panel), ["dismiss"]);
        assert_eq!(marks(&panel), [Some("close")]);
        // Text that is only space is nothing.
        assert_eq!(buttons(&card(&failed(Some("Nothing to type."), Some("  \n")))), ["dismiss"]);
        // And a failure that said nothing still says something.
        assert_eq!(card(&failed(None, None)).title, "Nothing was typed.");
        assert_eq!(card(&failed(Some("  "), None)).title, "Nothing was typed.");
    }

    #[test]
    fn a_failure_the_main_window_can_put_right_offers_to_open_it() {
        for message in [
            "The vault is locked.",
            "Unlock Karma Yoga to dictate.",
            "No medium model is set up. Choose one in Settings.",
        ] {
            let panel = card(&failed(Some(message), None));
            assert_eq!(buttons(&panel), ["open", "dismiss"], "{message}");
            assert!(panel.actions[0].primary);
            assert_eq!(panel.actions[0].icon.as_deref(), Some("open"));
        }
        // Text to copy is the better offer; one primary button at most.
        let both = card(&failed(Some("The vault is locked."), Some("Dear Sam,")));
        assert_eq!(buttons(&both), ["copy", "dismiss"]);
        assert!(!needs_the_window("The microphone failed."));
    }

    #[test]
    fn a_card_has_at_most_one_primary_button_and_every_button_does_something() {
        let mut every = vec![
            typing(Phase::Starting),
            typing(Phase::Listening),
            typing(Phase::Transcribing),
            typing(Phase::Writing),
            typing(Phase::Done),
            failed(None, None),
            failed(Some("Nothing was heard."), None),
            failed(Some("The vault is locked."), None),
            failed(Some("could not type"), Some("Dear Sam,")),
        ];
        every.push(Typing { mode: KeyMode::Hold, hands_free: false, ..typing(Phase::Listening) });
        for typing in &every {
            let panel = card(typing);
            assert!(panel.actions.iter().filter(|a| a.primary).count() <= 1, "{typing:?}");
            for action in &panel.actions {
                assert_ne!(press(&action.id, false), Press::Nothing, "{} in {typing:?}", action.id);
            }
        }
    }

    #[test]
    fn each_button_asks_for_what_it_says() {
        assert_eq!(press("stop", false), Press::Stop);
        assert_eq!(press("cancel", false), Press::Cancel);
        assert_eq!(press("open", false), Press::Open);
        assert_eq!(press("copy", false), Press::Copy { copied: false });
        assert_eq!(press("nonsense", false), Press::Nothing);
        assert_eq!(press("", true), Press::Nothing);
    }

    #[test]
    fn dismissing_a_failure_lets_the_text_go_as_cancelling_does() {
        assert_eq!(press("dismiss", false), press("cancel", false));
        assert_eq!(press("dismiss", true), Press::Cancel);
    }

    #[test]
    fn a_copy_the_panel_made_is_not_made_again() {
        assert_eq!(press("copy", true), Press::Copy { copied: true });
        // Only a copy says whether it was made; no other button cares.
        for action in ["stop", "cancel", "dismiss", "open"] {
            assert_eq!(press(action, true), press(action, false));
        }
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

    /// GNOME's defaults: the first repeat 500 ms after the press, then one
    /// every 30 ms.
    const GNOME: Repeats =
        Repeats::Every { delay: Duration::from_millis(500), interval: Duration::from_millis(30) };

    /// A press, then whatever follows, each at `ms` after the first -- with the
    /// desktop's timestamps when `stamped`, by arrival when not. Says which
    /// of them counted as presses.
    fn presses(repeats: Repeats, stamped: bool, edges: &[(u64, bool)]) -> Vec<bool> {
        let t = Instant::now();
        let mut down = None;
        edges
            .iter()
            .map(|&(ms, pressed)| {
                let at = Duration::from_millis(ms);
                let stamp = stamped.then_some(Duration::from_secs(1_000) + at);
                first_press(&mut down, pressed, t + at, stamp, repeats)
            })
            .collect()
    }

    /// A key held for `ms`, as `repeats` says the desktop repeats it.
    fn held_for(repeats: Repeats, ms: u64) -> Vec<(u64, bool)> {
        let Repeats::Every { delay, interval } = repeats else { return vec![(0, true)] };
        let (delay, interval) = (delay.as_millis() as u64, interval.as_millis() as u64);
        let mut edges = vec![(0, true)];
        let mut next = delay;
        while next < ms {
            edges.push((next, true));
            next += interval;
        }
        edges
    }

    #[test]
    fn a_held_keys_repeats_are_not_presses() {
        for stamped in [true, false] {
            let counted = presses(GNOME, stamped, &held_for(GNOME, 60_000));
            assert!(counted[0], "the press counts");
            assert!(counted[1..].iter().all(|&c| !c), "no repeat does (stamped: {stamped})");
        }
    }

    #[test]
    fn a_release_ends_the_key_and_the_next_press_counts() {
        let counted = presses(GNOME, true, &[(0, true), (500, true), (560, false), (700, true)]);
        assert_eq!(counted, [true, false, true, true]);
    }

    #[test]
    fn a_press_after_a_release_that_never_came_counts() {
        // Ctrl let go before M: GNOME says nothing of the key coming up.
        for stamped in [true, false] {
            let counted = presses(GNOME, stamped, &[(0, true), (4_000, true)]);
            assert_eq!(counted, [true, true], "stamped: {stamped}");
        }
    }

    #[test]
    fn a_quick_second_press_after_a_release_that_never_came_counts() {
        // Toggle mode, "yes", and the key again 250 ms on: no repeat comes
        // then, so it is a press. So are quick taps after it.
        let counted = presses(GNOME, true, &[(0, true), (250, true), (1_000, true), (1_250, true)]);
        assert_eq!(counted, [true, true, true, true]);
    }

    #[test]
    fn slow_repeats_are_still_repeats() {
        // Repeat Keys set slow, for somebody who needs it.
        let slow = Repeats::Every {
            delay: Duration::from_millis(500),
            interval: Duration::from_millis(1_000),
        };
        let counted = presses(slow, true, &held_for(slow, 5_000));
        assert!(counted[0] && counted[1..].iter().all(|&c| !c), "{counted:?}");
        // And a long delay before the first, however long.
        let long = Repeats::Every {
            delay: Duration::from_millis(4_000),
            interval: Duration::from_millis(30),
        };
        let counted = presses(long, true, &held_for(long, 6_000));
        assert!(counted[0] && counted[1..].iter().all(|&c| !c), "{counted:?}");
    }

    #[test]
    fn stamps_keep_a_late_repeat_a_repeat() {
        // The first repeat reached the app 300 ms late, behind a busy runtime,
        // but the desktop's stamp says when it happened.
        let t = Instant::now();
        let mut down = None;
        let stamp = |ms: u64| Some(Duration::from_millis(1_000_000 + ms));
        assert!(first_press(&mut down, true, t, stamp(0), GNOME));
        let late = t + Duration::from_millis(800 + ARRIVAL_SLACK.as_millis() as u64);
        assert!(!first_press(&mut down, true, late, stamp(500), GNOME));
    }

    #[test]
    fn a_repeat_out_of_time_by_arrival_is_still_within_its_slack() {
        let (delay, slack) = (500, ARRIVAL_SLACK.as_millis() as u64);
        let counted = presses(GNOME, false, &[(0, true), (delay + slack - 1, true)]);
        assert_eq!(counted, [true, false]);
        let counted = presses(GNOME, false, &[(0, true), (delay + slack + 1, true)]);
        assert_eq!(counted, [true, true]);
    }

    #[test]
    fn without_repeats_every_press_counts() {
        let counted = presses(Repeats::Off, true, &[(0, true), (500, true), (530, true)]);
        assert_eq!(counted, [true, true, true]);
    }

    #[test]
    fn unknown_repeats_take_anything_soon_for_a_repeat() {
        let within = UNKNOWN_REPEAT_WITHIN.as_millis() as u64;
        let counted = presses(
            Repeats::Unknown,
            false,
            &[(0, true), (within - 1, true), (2 * within + 10, true)],
        );
        assert_eq!(counted, [true, false, true]);
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
