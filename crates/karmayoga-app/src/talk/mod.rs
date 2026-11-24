//! Talking to the assistant from anywhere: press the talk key, ask, hear the
//! answer.
//!
//! Voice mode without the window. The talk key (Settings → AI features →
//! Voice assistant) opens a spoken conversation wherever the person is; the
//! panel in the corner of the screen ([`crate::panel`]) shows it -- listening,
//! what was heard, the assistant thinking and answering, a question that
//! needs a click -- and a second press, the assistant being told "that's
//! all", or a while with nothing said ends it. The conversation is an
//! ordinary one, kept in the thread list like any other.
//!
//! It is voice mode's session ([`crate::voice`], `karmayoga_service::voice`)
//! with the panel where the window was, and voice typing's key handling
//! ([`crate::voicetyping`]) for a key that may be pressed or held.
//!
//! "Always ready", on by default once the key is: the speech models are
//! kept loaded while the vault is unlocked (`karmayoga_service::speech::ready`),
//! so a press is heard at once rather than after the recogniser has spent a
//! few seconds loading.
//!
//! # Where things are
//!
//! [`key()`] is what the desktop's key edges arrive at, and the `key` module
//! decides what they mean; `card` says what the card shows for what the
//! session has said; `conversation` picks which conversation a press carries
//! on and when a quiet one is over; `level` is the meter. All four are plain
//! functions of their inputs. This file is what joins them to the desktop:
//! the one conversation there can be at a time, the card it draws, the
//! timers, and the models held ready.
//!
//! # Holding the key to talk
//!
//! In hold mode the key is a push-to-talk, and letting go does not mute the
//! microphone at once. A session that is muted while somebody is being heard
//! forgets what they had said so far, and the person lets go of the key as
//! they finish the last word, not after the pause that tells the detector
//! they have: so the microphone stays open for `key::TAIL` after the
//! release, and if somebody is still being heard then, until they are not
//! (`Hearing` is the session's state from the first words to the end of the
//! pause). Only then is it muted, and what was said is understood and
//! answered as any turn is. A tap of less than `key::TAP`, or a release
//! before the session was even open, is not push-to-talk: the conversation
//! stays open, hands-free, and the next press ends it.
//!
//! The card is drawn from a task on the runtime, never from the key's
//! callback, which on some desktops is the main thread: a draw may wait on
//! the main thread, and a key waiting on a draw would wait for itself.
//!
//! See `docs/plans/panel.md`, "The talk key".

mod card;
mod conversation;
mod key;
mod level;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use karmayoga_core::ConversationId;
use karmayoga_service::agent::Typist;
use karmayoga_service::ctx::Ctx;
use karmayoga_service::error::{CommandError, CommandResult};
use karmayoga_service::speech::ready::SpeechReady;
use karmayoga_service::voice::{VoiceEvent, VoiceState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::Shortcut;

use crate::hotkey::{self, Binding, Hotkeys, KeyStatus};
use crate::panel;
use crate::state::AppState;
use crate::voice::{self, Shell};
use crate::voicetyping::KeyMode;

/// The panel card's id. One conversation at a time.
pub const PANEL_ID: &str = "talk";

/// Tauri event to the main window: a conversation from the talk key began
/// (`true`) or ended (`false`). The window pauses what its player is playing
/// for it and picks it up after, as it does for voice mode in the window --
/// the speakers are the conversation's.
pub const TALK_EVENT: &str = "karmayoga://talk";

/// The key unless somebody chooses another: A for the assistant, beside voice
/// typing's M and writing help's Space.
pub const DEFAULT_SHORTCUT: &str = "Ctrl+Alt+A";

/// The card is redrawn at most this often, however fast the reply streams in.
const DRAW_EVERY: Duration = Duration::from_millis(100);

/// How often a conversation is looked at to see whether it has been left
/// alone long enough to end: often enough that one the wake word began ends
/// close to its fifteen seconds.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// How long "Conversation ended" stays up.
const ENDED_LINGER: Duration = Duration::from_secs(2);

/// How long the first turn of a conversation the wake word began waits for
/// the main window to say what it shows ([`screen_shared`]): "write a snippet
/// of this article here" is about what is open there. The window answers the
/// moment it is told the conversation began, well inside this; a window that
/// is hidden says nothing, and the turn goes ahead without it.
const SCREEN_WAIT: Duration = Duration::from_millis(400);

/// How long a card that says something went wrong stays up: there is a
/// reason to read.
const TROUBLE_LINGER: Duration = Duration::from_secs(6);

/// Why a conversation that was left alone has ended, on its card.
const IDLE_NOTE: &str = "Nothing was said for two minutes";

// ---- the per-machine setting -------------------------------------------------

/// The talk key's setting on this machine, beside voice typing's and for the
/// same reason: the key is a claim on this desktop's keyboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    pub shortcut: String,
    /// Press to start and press again to end, or hold to talk.
    pub mode: KeyMode,
    /// Keep the speech models loaded while the vault is unlocked, so a press
    /// is heard at once.
    pub keep_ready: bool,
    /// A press within this many minutes of the last conversation ending
    /// carries it on; later, a new one begins. `0` always begins a new one.
    pub continue_minutes: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            shortcut: DEFAULT_SHORTCUT.into(),
            mode: KeyMode::Toggle,
            keep_ready: true,
            continue_minutes: 10,
        }
    }
}

const FILE: &str = "talk.json";

fn save_config(config: &Config) -> Result<(), String> {
    crate::machine_config::save(FILE, config)
}

pub fn load_config() -> Config {
    crate::machine_config::load(FILE)
}

/// The shortcut to claim at startup: the talk key's while it is switched on,
/// unless writing help or voice typing has the same one.
pub fn configured_shortcut() -> Option<Shortcut> {
    let config = load_config();
    if !config.enabled {
        return None;
    }
    let ours = hotkey::parse(&config.shortcut)
        .map_err(|e| tracing::warn!(error = %e, "the talk key's shortcut does not parse"))
        .ok()?;
    if crate::writing::configured_shortcut() == Some(ours)
        || crate::voicetyping::claimed_shortcut() == Some(ours)
    {
        tracing::warn!("the talk key's shortcut is another key's too; it is left unclaimed");
        return None;
    }
    Some(ours)
}

// ---- the conversation ----------------------------------------------------------

/// The one conversation there can be, if there is one, and what is remembered
/// of the key and of the last. For the life of the process.
struct Inner {
    /// What the key last said while down, with no release since.
    down: Option<key::Down>,
    /// A task is waiting to see whether a held key's repeats have stopped.
    watching: bool,
    live: Option<Live>,
    /// The last conversation to end, for carrying on.
    last: Option<conversation::Last>,
    /// The newest activation token the key brought (Wayland): what lets the
    /// window that sets the clipboard take the keyboard for the moment a
    /// "type that ..." needs it. See [`Keyboard`].
    token: Option<String>,
}

impl Inner {
    const fn new() -> Self {
        Self { down: None, watching: false, live: None, last: None, token: None }
    }
}

/// A conversation, from the key going down until its card has gone.
struct Live {
    /// Which conversation this is, so that a timer set for one never acts on
    /// the next.
    seq: u64,
    conversation: ConversationId,
    posture: key::Posture,
    card: card::State,
    /// The last time the person did anything, or the session did anything
    /// that was not waiting on them. See [`conversation::idle_expired`].
    quiet_since: Instant,
    drawn_at: Option<Instant>,
    /// A redraw is on its way.
    draw_queued: bool,
    /// Changed whenever the key does something that cancels a mute waiting on
    /// its tail.
    release_gen: u64,
    /// The key was let go while somebody was being heard: shut the microphone
    /// as soon as they are not.
    mute_when_heard: bool,
    /// The main window has been told this conversation began, and not yet that
    /// it ended. See [`announce`].
    announced: bool,
    /// Begun by the wake word rather than the key: over after
    /// [`conversation::WOKEN_IDLE_AFTER`] of quiet (unless a dictation is
    /// going on), without a note, and told on its card to say it is done
    /// rather than to press a key.
    woke: bool,
    /// What came after the wake word, waiting to be the first turn until the
    /// main window has said what it shows -- see [`screen_shared`] -- or
    /// [`SCREEN_WAIT`] has passed.
    first_turn: Option<String>,
}

impl Live {
    fn new(
        seq: u64,
        conversation: ConversationId,
        posture: key::Posture,
        card: card::State,
        now: Instant,
    ) -> Self {
        let mut live = Self {
            seq,
            conversation,
            posture,
            card,
            quiet_since: now,
            drawn_at: None,
            draw_queued: false,
            release_gen: 0,
            mute_when_heard: false,
            announced: false,
            woke: false,
            first_turn: None,
        };
        live.sync_hint();
        live
    }

    /// The main window is to be told the conversation has begun: once, and
    /// only while it is not over -- a session that ended the moment it opened
    /// has already had its ending, and a beginning told after it would never
    /// be undone.
    fn begin_announce(&mut self) -> bool {
        if self.active() && !self.announced {
            self.announced = true;
            return true;
        }
        false
    }

    /// The main window is to be told the conversation has ended, if it was
    /// told it began and has not been told so since. However the conversation
    /// comes down -- it ends, the vault locks, it is replaced -- this is
    /// asked, so that a player waiting to carry on is told exactly once.
    fn end_announce(&mut self) -> bool {
        std::mem::take(&mut self.announced)
    }

    /// Whether the session is open and listening, not still being got ready.
    fn opened(&self) -> bool {
        self.card.phase != VoiceState::Starting
    }

    /// Not over: the card is more than a farewell.
    fn active(&self) -> bool {
        !self.card.is_over()
    }

    /// The card's hint follows how the key is held, and how the conversation
    /// began.
    fn sync_hint(&mut self) {
        self.card.hint = hint_for(self.posture, self.woke);
    }

    /// How long it may be left alone before it ends. See
    /// [`conversation::idle_after`].
    fn idle_limit(&self) -> Duration {
        conversation::idle_after(self.card.phase, self.woke, self.card.dictating())
    }
}

/// What the card's body says to do, for a conversation in `posture` that the
/// wake word began (`woke`) or the key or the button did. Nobody who called
/// the assistant by name has a key in their hand, so they are told what to
/// say instead -- but only while a press is what ends it: a key held or
/// muted is the key's own conversation whatever began it.
fn hint_for(posture: key::Posture, woke: bool) -> card::Hint {
    match posture {
        key::Posture::Toggled | key::Posture::Free if woke => card::Hint::Say,
        key::Posture::Toggled | key::Posture::Free => card::Hint::End,
        key::Posture::Held { .. } => card::Hint::Release,
        key::Posture::Muted => card::Hint::Hold,
    }
}

static TALK: Mutex<Inner> = Mutex::new(Inner::new());

/// Held while the card is changed, so that what is drawn goes up in the order
/// it was made, and a card taken down is not put back by a draw that was
/// already under way. Never taken on the key's callback.
static DRAW: Mutex<()> = Mutex::new(());

static NEXT: AtomicU64 = AtomicU64::new(1);

fn lock() -> MutexGuard<'static, Inner> {
    TALK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The conversation `seq` if it is still the one there is.
fn live_for(inner: &mut Inner, seq: u64) -> Option<&mut Live> {
    inner.live.as_mut().filter(|live| live.seq == seq)
}

/// The conversation, if there is one that is not over.
fn active_mut(inner: &mut Inner) -> Option<&mut Live> {
    inner.live.as_mut().filter(|live| live.active())
}

/// Put `new` in place of the conversation there is -- none, to take it down.
/// Whether the main window is owed word that the old one ended: it was told
/// it began and has not been told otherwise.
fn replace_live(inner: &mut Inner, new: Option<Live>) -> bool {
    std::mem::replace(&mut inner.live, new).is_some_and(|mut old| old.end_announce())
}

/// [`replace_live`], telling the main window the old conversation has ended
/// if it is owed that. Every way a conversation comes down goes through here
/// or through [`on_event`]'s own ending, so the window's player is told
/// exactly once.
fn set_live(app: &AppHandle, inner: &mut Inner, new: Option<Live>) {
    if replace_live(inner, new) {
        announce(app, false);
    }
}

/// What the key, a timer or the session asks the shell to do about a
/// conversation, worked out by a function that touches nothing but the
/// [`Live`] so that it can be tested, and carried out by [`apply`].
#[derive(Debug, Default, PartialEq, Eq)]
struct Effects {
    /// Tell the session to mute or unmute.
    muted: Option<bool>,
    /// End the session.
    stop: bool,
    /// Look at the key again after [`key::TAIL`], for this generation.
    tail: Option<u64>,
    /// The card has changed.
    draw: bool,
    /// The session has said it is over.
    ended: bool,
}

/// What the key asked for, done to the conversation. [`key::Act::Start`] is
/// not here: there is no conversation yet.
fn transition(live: &mut Live, act: key::Act, at: Instant) -> Effects {
    let mut fx = Effects::default();
    match act {
        key::Act::Nothing | key::Act::Start => return fx,
        key::Act::End => return finish(live, None),
        key::Act::Unmute => {
            live.release_gen += 1;
            live.mute_when_heard = false;
            live.posture = key::Posture::Held { since: at };
            live.card.muted = false;
            fx.muted = Some(false);
        }
        key::Act::HandsFree => {
            live.release_gen += 1;
            live.posture = key::Posture::Free;
        }
        key::Act::Mute => {
            live.posture = key::Posture::Muted;
            live.release_gen += 1;
            fx.tail = Some(live.release_gen);
        }
    }
    live.sync_hint();
    fx.draw = true;
    fx
}

/// Ask for the conversation to end, for `note` if there is a reason worth
/// saying.
fn finish(live: &mut Live, note: Option<&str>) -> Effects {
    if !live.active() || live.card.ending {
        return Effects::default();
    }
    live.card.ending = true;
    live.card.note = note.map(str::to_string);
    live.release_gen += 1;
    live.mute_when_heard = false;
    Effects { stop: true, draw: true, ..Effects::default() }
}

/// [`key::TAIL`] has passed since the key was let go: shut the microphone,
/// unless somebody is being heard, or the key has done something since.
fn release_due(live: &mut Live, generation: u64) -> Effects {
    if !live.active() || live.release_gen != generation || live.posture != key::Posture::Muted {
        return Effects::default();
    }
    if key::may_mute_now(live.card.phase) {
        live.card.muted = true;
        Effects { muted: Some(true), draw: true, ..Effects::default() }
    } else {
        live.mute_when_heard = true;
        Effects::default()
    }
}

/// The "Mute" button: the microphone off, or on again.
fn toggle_mute(live: &mut Live) -> Effects {
    if !live.active() {
        return Effects::default();
    }
    let muted = !live.card.muted;
    live.release_gen += 1;
    live.mute_when_heard = false;
    live.card.muted = muted;
    // Hold mode with no key down: muting is what letting go would have done,
    // and unmuting is hands-free.
    live.posture = match (live.posture, muted) {
        (key::Posture::Free, true) => key::Posture::Muted,
        (key::Posture::Muted, false) => key::Posture::Free,
        (posture, _) => posture,
    };
    live.sync_hint();
    Effects { muted: Some(muted), draw: true, ..Effects::default() }
}

/// What the session said, taken in.
fn hear(live: &mut Live, event: &VoiceEvent, now: Instant) -> Effects {
    live.card.hear(event);
    let mut fx = Effects { draw: true, ..Effects::default() };
    match event {
        VoiceEvent::State { state } => {
            // Every change of state is the conversation doing something, and
            // the quiet is counted from the last.
            live.quiet_since = now;
            if live.mute_when_heard && key::may_mute_now(*state) {
                live.mute_when_heard = false;
                live.card.muted = true;
                fx.muted = Some(true);
            }
        }
        // Somebody is there: what they said, or what is being typed from what
        // they said, or a dictation begun or ended on their say-so.
        VoiceEvent::Heard { .. } | VoiceEvent::Typed { .. } | VoiceEvent::Dictation { .. } => {
            live.quiet_since = now;
        }
        VoiceEvent::Ended { .. } => {
            live.mute_when_heard = false;
            fx.ended = true;
        }
        _ => {}
    }
    fx
}

/// Do what a conversation's change called for.
fn apply(app: &AppHandle, live: &mut Live, fx: Effects) {
    if let Some(muted) = fx.muted {
        voice::set_shell_muted(app, muted);
    }
    if fx.stop {
        voice::stop_shell(app);
    }
    if let Some(generation) = fx.tail {
        let (app, seq) = (app.clone(), live.seq);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(key::TAIL).await;
            let mut inner = lock();
            if let Some(live) = live_for(&mut inner, seq) {
                let fx = release_due(live, generation);
                apply(&app, live, fx);
            }
        });
    }
    if fx.draw {
        queue_draw(app, live);
    }
}

// ---- the card -----------------------------------------------------------------

/// Have the card drawn: now, if it has not been for [`DRAW_EVERY`], or once
/// that has passed. What is drawn is what the conversation holds then, so a
/// burst of changes is one draw.
fn queue_draw(app: &AppHandle, live: &mut Live) {
    if live.draw_queued {
        return;
    }
    live.draw_queued = true;
    let wait = live.drawn_at.map_or(Duration::ZERO, |at| DRAW_EVERY.saturating_sub(at.elapsed()));
    let (app, seq) = (app.clone(), live.seq);
    tauri::async_runtime::spawn(async move {
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        draw(&app, seq);
    });
}

fn draw(app: &AppHandle, seq: u64) {
    let _drawing = DRAW.lock().unwrap_or_else(|e| e.into_inner());
    let panel = {
        let mut inner = lock();
        let Some(live) = live_for(&mut inner, seq) else { return };
        live.draw_queued = false;
        live.drawn_at = Some(Instant::now());
        card::build(&live.card)
    };
    panel::show(app, panel);
}

/// Take the card down, and the conversation with it, unless a newer one has
/// begun since `seq`.
fn dismiss_card(app: &AppHandle, seq: u64) {
    let _drawing = DRAW.lock().unwrap_or_else(|e| e.into_inner());
    {
        let mut inner = lock();
        if inner.live.as_ref().is_some_and(|live| live.seq != seq) {
            return;
        }
        set_live(app, &mut inner, None);
    }
    panel::dismiss(app, PANEL_ID);
}

/// Take the card down after `wait`.
async fn linger(app: AppHandle, seq: u64, wait: Duration) {
    tokio::time::sleep(wait).await;
    dismiss_card(&app, seq);
}

// ---- the key and the panel ---------------------------------------------------

/// The talk key went down (`pressed`) or came up. `token` is the activation
/// token the desktop sent with it, on Wayland; `stamp` is when the desktop
/// says it happened, where it says.
pub fn key(app: &AppHandle, pressed: bool, token: Option<String>, stamp: Option<Duration>) {
    // Read before the lock: the hotkeys take a lock of their own.
    let releases = app.state::<Hotkeys>().reports_releases();
    let now = Instant::now();
    // A desktop that stamps nothing may send zero.
    let stamp = stamp.filter(|s| !s.is_zero());
    let mut inner = lock();
    // A press brings no window up, so nothing takes the keyboard with it now;
    // it is kept for typing, the newest being the one most likely still good.
    if token.is_some() {
        inner.token = token;
    }
    let edge =
        key::classify(&mut inner.down, pressed, now, stamp, crate::voicetyping::key_repeats());
    if edge == key::Edge::Repeat {
        watch_for_release(app, &mut inner);
        return;
    }
    edge_happened(app, inner, edge, now, releases);
}

/// A held key, let go modifier first, says nothing of being let go -- but its
/// repeats stop. Once they have started, a pause in them of
/// [`key::release_wait`] -- voice typing's own: an interval and the slack for
/// a busy runtime -- is taken as the release, when the last repeat came.
///
/// Only for a conversation that is waiting on a held key, and only where the
/// desktop is known to repeat. Toggle mode never needs a release, and a key
/// let go before its first repeat leaves the microphone open until the next
/// press, as voice typing's does.
fn watch_for_release(app: &AppHandle, inner: &mut Inner) {
    let Some(wait) = key::release_wait(crate::voicetyping::key_repeats()) else { return };
    let held = inner
        .live
        .as_ref()
        .is_some_and(|live| live.active() && matches!(live.posture, key::Posture::Held { .. }));
    if !held || inner.watching {
        return;
    }
    inner.watching = true;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(wait).await;
            let mut inner = lock();
            // Let go already, or pressed again since: a fresh press has its
            // own repeats to wait for, and its own watch.
            let Some(down) = inner.down.filter(|down| down.repeat) else {
                inner.watching = false;
                return;
            };
            if down.at.elapsed() < wait {
                continue;
            }
            inner.down = None;
            inner.watching = false;
            edge_happened(&app, inner, key::Edge::Release, down.at, true);
            return;
        }
    });
}

/// What the key's edge at `at` asks for, and doing it. `releases` is whether
/// the desktop says when the key is let go.
fn edge_happened(
    app: &AppHandle,
    mut inner: MutexGuard<'static, Inner>,
    edge: key::Edge,
    at: Instant,
    releases: bool,
) {
    let stage = match inner.live.as_ref().filter(|live| live.active()) {
        Some(live) => key::Stage::Talking { posture: live.posture, opened: live.opened() },
        None => key::Stage::Idle,
    };
    let act = key::decide(stage, edge, at);
    if act == key::Act::Start {
        drop(inner);
        begin(app, Begin::Key { at, releases });
        return;
    }
    if let Some(live) = active_mut(&mut inner) {
        // Whatever the key did, somebody is there.
        live.quiet_since = at;
        let fx = transition(live, act, at);
        apply(app, live, fx);
    }
}

/// What opens a conversation.
enum Begin {
    /// The key, going down at `at`.
    Key { at: Instant, releases: bool },
    /// The button in Settings, or the tray's item: a press ends it.
    Command,
    /// The wake word ([`crate::wake`]), with what was said after it, which
    /// is the conversation's first turn unless it is empty.
    Wake { said: String },
}

/// Open a conversation: the card goes up at once, saying it is getting ready,
/// and the session opens behind it.
///
/// The conversation exists before the session does, so a key let go while the
/// models are still loading finds something to be let go of.
fn begin(app: &AppHandle, how: Begin) {
    let config = load_config();
    let said = match &how {
        Begin::Wake { said } => Some(said.clone()),
        _ => None,
    };
    let (posture, now) = match how {
        Begin::Key { at, releases } => {
            if !config.enabled {
                return;
            }
            (key::Posture::starting(config.mode, releases, at), at)
        }
        Begin::Command | Begin::Wake { .. } => (key::Posture::Toggled, Instant::now()),
    };
    let mut inner = lock();
    if inner.live.as_ref().is_some_and(Live::active) {
        return;
    }
    let conversation =
        conversation::choose(inner.last, now, config.continue_minutes).unwrap_or_default();
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut card = card::State::new();
    card.key = config.shortcut.trim().to_string();
    let mut live = Live::new(seq, conversation, posture, card, now);
    live.woke = said.is_some();
    live.sync_hint();
    if live.woke {
        // Only a press brings a token, and a press before this one is not
        // what this conversation types for.
        inner.token = None;
    }
    queue_draw(app, &mut live);
    set_live(app, &mut inner, Some(live));
    drop(inner);
    tauri::async_runtime::spawn(open(app.clone(), seq, conversation, said));
    tauri::async_runtime::spawn(watch_idle(app.clone(), seq));
}

/// Types for a conversation the talk key started -- "type that I'm running
/// late" -- into whatever had the keyboard: the field the person was in, which
/// the panel never takes it from. Voice typing's paste, with the key's newest
/// activation token. See `karmayoga_service::agent::Typist`.
struct Keyboard(AppHandle);

impl Typist for Keyboard {
    fn type_text(
        &self,
        text: String,
    ) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        let app = self.0.clone();
        Box::pin(async move {
            let token = lock().token.clone();
            crate::voicetyping::paste::paste(&app, &text, token).await
        })
    }
}

/// Open the session for conversation `seq`: learn what to call the assistant,
/// then ask [`voice::start_for_shell`], whose events come back to
/// [`on_event`]. If it cannot be had -- the microphone is a meeting's, speech
/// is not set up -- the card says so for a few seconds.
async fn open(app: AppHandle, seq: u64, conversation: ConversationId, said: Option<String>) {
    let woken = said.is_some();
    let name = app
        .state::<AppState>()
        .service()
        .on_vault(|vault| vault.agent_settings())
        .await
        .map(|settings| settings.name)
        .unwrap_or_default();
    let shown = key_text(&app);
    {
        let mut inner = lock();
        let Some(live) = live_for(&mut inner, seq) else { return };
        live.card.name_it(&name);
        if let Some(shown) = shown {
            live.card.key = shown;
        }
        if live.card.ending {
            // Ended before it was open: there is nothing to say goodbye to.
            set_live(&app, &mut inner, None);
            drop(inner);
            let app = app.clone();
            tauri::async_runtime::spawn(async move { dismiss_card(&app, seq) });
            return;
        }
        queue_draw(&app, live);
    }

    let events: voice::EventSink = {
        let app = app.clone();
        Arc::new(move |event| on_event(&app, seq, event))
    };
    let microphone: voice::MicrophoneTap = {
        let app = app.clone();
        let meter = Mutex::new(level::Meter::new(Instant::now()));
        Arc::new(move |block| {
            let shown = meter.lock().unwrap_or_else(|e| e.into_inner()).hear(block, Instant::now());
            if let Some(shown) = shown {
                panel::level(&app, PANEL_ID, shown);
            }
        })
    };
    // Where typing will need it, the window that sets the clipboard is made
    // now, hidden, as voice typing makes it while the microphone opens.
    crate::voicetyping::pill::ensure(&app);
    let typist: Arc<dyn Typist> = Arc::new(Keyboard(app.clone()));
    let shell = Shell { events, microphone: Some(microphone), typist: Some(typist), woken };
    match voice::start_for_shell(&app, conversation, shell).await {
        Ok(()) => {
            let mut inner = lock();
            match live_for(&mut inner, seq).filter(|live| live.active()) {
                Some(live) => {
                    if live.begin_announce() {
                        announce(&app, true);
                    }
                    // An end that was asked for while the session was opening.
                    if live.card.ending {
                        voice::stop_shell(&app);
                    } else {
                        match opening(said) {
                            // What came after the wake word: the first turn.
                            // Held for a moment: the window, just told the
                            // conversation began, sends what it shows, and the
                            // turn is about that.
                            Opening::Turn(said) => {
                                live.quiet_since = Instant::now();
                                live.first_turn = Some(said);
                                let app = app.clone();
                                tauri::async_runtime::spawn(async move {
                                    tokio::time::sleep(SCREEN_WAIT).await;
                                    first_turn(&app, Some(seq));
                                });
                            }
                            // The name alone: the person is not looking at the
                            // corner, so they are told by sound that it is
                            // listening, and the quiet is counted from then.
                            Opening::Chime => {
                                live.quiet_since = Instant::now();
                                voice::shell_chime(&app);
                            }
                            Opening::Nothing => {}
                        }
                    }
                }
                // Taken down while it opened -- the vault locked -- or over
                // already: nobody is looking at a session that is running.
                None => {
                    drop(inner);
                    voice::stop_shell(&app);
                }
            }
        }
        Err(error) => refuse(&app, seq, &error),
    }
}

/// What a conversation does as soon as its session is open.
#[derive(Debug, PartialEq, Eq)]
enum Opening {
    /// The wake word, and what came after it: that is the first turn.
    Turn(String),
    /// The wake word and nothing after it: the listening sound says it has
    /// been heard and is listening.
    Chime,
    /// The key or the button: the person is looking at the card, and the key
    /// in their hand is the sign.
    Nothing,
}

/// What a conversation opened with `said` -- `Some` when the wake word began
/// it, with what came after the name, which may be nothing -- does now it is
/// open.
fn opening(said: Option<String>) -> Opening {
    match said {
        Some(said) if !said.trim().is_empty() => Opening::Turn(said),
        Some(_) => Opening::Chime,
        None => Opening::Nothing,
    }
}

/// Begin the first turn that waits on the main window's screen, if there is
/// one still waiting: conversation `seq`'s, or whichever is going on.
fn first_turn(app: &AppHandle, seq: Option<u64>) {
    let said = {
        let mut inner = lock();
        let live = match seq {
            Some(seq) => live_for(&mut inner, seq),
            None => inner.live.as_mut(),
        };
        let Some(live) = live.filter(|live| live.active()) else { return };
        let Some(said) = live.first_turn.take() else { return };
        live.quiet_since = Instant::now();
        said
    };
    voice::shell_heard(app, said);
}

/// The main window has handed the session what it shows
/// (`voice_set_screen`): a first turn waiting on it begins now, after it. See
/// [`SCREEN_WAIT`].
pub fn screen_shared(app: &AppHandle) {
    first_turn(app, None);
}

/// Tell the main window a conversation began or ended -- see [`TALK_EVENT`].
fn announce(app: &AppHandle, active: bool) {
    use tauri::Emitter;
    if let Err(e) = app.emit_to(crate::windows::MAIN, TALK_EVENT, active) {
        tracing::debug!(error = %e, "the main window was not told about the conversation");
    }
}

/// The session would not open: the card says why for a few seconds.
fn refuse(app: &AppHandle, seq: u64, error: &CommandError) {
    tracing::info!(code = %error.code, "a conversation from the talk key could not begin");
    {
        let mut inner = lock();
        let Some(live) = live_for(&mut inner, seq) else { return };
        live.card.refused(&error.message);
        if live.end_announce() {
            announce(app, false);
        }
        queue_draw(app, live);
    }
    tauri::async_runtime::spawn(linger(app.clone(), seq, TROUBLE_LINGER));
}

/// The key as the desktop names it, where it says: the portal's own answer
/// where the desktop keeps the binding.
fn key_text(app: &AppHandle) -> Option<String> {
    let status = app.state::<Hotkeys>().status(app, Binding::Talk);
    let shown = status.shortcut.trim();
    (!shown.is_empty() && shown != "no key").then(|| shown.to_string())
}

/// The session said something. Returns `false` when there is nobody to say it
/// to -- the conversation was taken down, as the lock does -- which ends the
/// session.
fn on_event(app: &AppHandle, seq: u64, event: VoiceEvent) -> bool {
    let now = Instant::now();
    let mut guard = lock();
    let inner = &mut *guard;
    let Some(live) = live_for(inner, seq) else { return false };
    let failed = matches!(event, VoiceEvent::Ended { error: Some(_) });
    let fx = hear(live, &event, now);
    let ended = fx.ended;
    let conversation = live.conversation;
    apply(app, live, fx);
    if ended {
        if live.end_announce() {
            announce(app, false);
        }
        inner.last = Some(conversation::Last { conversation, at: now });
        let wait = if failed { TROUBLE_LINGER } else { ENDED_LINGER };
        tauri::async_runtime::spawn(linger(app.clone(), seq, wait));
    }
    true
}

/// End a conversation that has been left alone, once it has. Looks every
/// [`IDLE_TICK`] until the conversation is over.
async fn watch_idle(app: AppHandle, seq: u64) {
    loop {
        tokio::time::sleep(IDLE_TICK).await;
        let mut inner = lock();
        let Some(live) = live_for(&mut inner, seq).filter(|live| live.active()) else { return };
        let after = live.idle_limit();
        if conversation::idle_expired(live.card.phase, live.quiet_since, Instant::now(), after) {
            // Woken by name, it was always going to end soon: no note. A
            // dictation that was waited on for the long time has one.
            let note = (!live.woke || live.card.dictating()).then_some(IDLE_NOTE);
            let fx = finish(live, note);
            apply(&app, live, fx);
            return;
        }
    }
}

/// A button on the talk card was pressed. See [`crate::panel::on_action`].
pub fn on_panel_action(app: &AppHandle, action: &str) {
    match action {
        "end" => {
            let mut inner = lock();
            if let Some(live) = active_mut(&mut inner) {
                live.quiet_since = Instant::now();
                let fx = finish(live, None);
                apply(app, live, fx);
            }
        }
        "mute" => {
            let mut inner = lock();
            if let Some(live) = active_mut(&mut inner) {
                live.quiet_since = Instant::now();
                let fx = toggle_mute(live);
                apply(app, live, fx);
            }
        }
        // The main window, on the Assistant: where the thread is. There is no
        // way yet to ask it for one thread in particular; the one just talked
        // in is the most recent.
        "open" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let shown =
                    crate::windows::show_in_main(app, Some("assistant".to_string()), None).await;
                if let Err(e) = shown {
                    tracing::debug!(error = %e, "the assistant could not be shown from the talk card");
                }
            });
        }
        "allow" | "deny" | "always" => answer(app, action),
        other => tracing::debug!(action = other, "a press on the talk card that means nothing"),
    }
}

/// Answer the question the card is asking, through the same command the
/// window's buttons use, and take its buttons away at once. The session sees
/// the call finish, as it does when the window answers.
fn answer(app: &AppHandle, action: &str) {
    let call = {
        let mut inner = lock();
        let Some(live) = active_mut(&mut inner) else { return };
        let Some(call) = live.card.confirm().map(|c| c.call_id.clone()) else { return };
        live.card.answered();
        live.quiet_since = Instant::now();
        queue_draw(app, live);
        call
    };
    let args = json!({
        "callId": call,
        "approved": action != "deny",
        "always": action == "always",
        "later": false,
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let session = app.state::<AppState>().session().as_session();
        if let Err(e) = session.call(Ctx::local(), "confirm_tool_call", args).await {
            tracing::debug!(error = %e, "a spoken question's answer could not be given");
        }
    });
}

/// Begin a conversation because the assistant was called by name: what was
/// said after the name, if anything, is its first turn, and it ends after a
/// short quiet. See [`crate::wake`]. Nothing happens while one is going on.
pub fn wake(app: &AppHandle, said: String) {
    begin(app, Begin::Wake { said });
}

/// Whether a conversation is going on -- what the wake word's listener waits
/// for the end of before it listens again. A card left up after one ended
/// does not count.
pub fn is_talking() -> bool {
    lock().live.as_ref().is_some_and(Live::active)
}

/// The vault locked or unlocked: a conversation ends with the lock, and the
/// models are held ready only while it is unlocked.
///
/// A lock takes the card down at once rather than saying goodbye on it: what
/// was said is on it, and the vault is meant to be closed.
pub fn on_lock_state(app: &AppHandle, locked: bool) {
    if locked {
        let seq = {
            let mut inner = lock();
            inner.last = None;
            inner.down = None;
            let seq = inner.live.as_ref().map(|live| live.seq);
            set_live(app, &mut inner, None);
            seq
        };
        voice::stop_shell(app);
        drop_ready();
        let app = app.clone();
        tauri::async_runtime::spawn(async move { dismiss_card(&app, seq.unwrap_or(0)) });
    } else {
        let config = load_config();
        if config.enabled && config.keep_ready {
            want_ready(app);
        }
    }
}

// ---- keeping the speech models ready -------------------------------------------

/// The models held ready, and which asking they answer.
struct Ready {
    /// Bumped whenever readiness is asked for or given up, so that a load
    /// that was still under way when it was given up is let go on arrival.
    generation: u64,
    held: Option<SpeechReady>,
}

static READY: Mutex<Ready> = Mutex::new(Ready { generation: 0, held: None });

fn ready() -> MutexGuard<'static, Ready> {
    READY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Load the speech models and hold them until [`drop_ready`]. Quiet when it
/// cannot: speech not being set up is the ordinary state of a vault that has
/// not chosen a voice yet.
fn want_ready(app: &AppHandle) {
    let generation = {
        let mut ready = ready();
        ready.generation += 1;
        ready.generation
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let service = app.state::<AppState>().service();
        match service.speech_keep_ready().await {
            Ok(held) => {
                let stale = {
                    let mut ready = ready();
                    if ready.generation == generation {
                        ready.held.replace(held)
                    } else {
                        Some(held)
                    }
                };
                // Let go outside the lock: it may be the last of a model.
                drop(stale);
            }
            Err(e) => tracing::debug!(error = %e, "the speech models are not being kept ready"),
        }
    });
}

/// Let go of the models, and of any load still under way.
fn drop_ready() {
    let held = {
        let mut ready = ready();
        ready.generation += 1;
        ready.held.take()
    };
    drop(held);
}

/// Hold the models ready, or not, as `config` says and the vault allows now.
/// An unlocked vault is the only one that can load them; a locked one is
/// handled when it is unlocked -- see [`on_lock_state`].
fn follow(app: &AppHandle, config: &Config) {
    let unlocked = app.state::<AppState>().service().get().is_some_and(|v| v.is_unlocked());
    if config.enabled && config.keep_ready && unlocked {
        want_ready(app);
    } else {
        drop_ready();
    }
}

// ---- commands ----------------------------------------------------------------

/// What Settings draws for the talk key.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkSettings {
    pub enabled: bool,
    pub shortcut: String,
    pub mode: KeyMode,
    pub keep_ready: bool,
    pub continue_minutes: u32,
    pub key: KeyStatus,
}

fn settings(app: &AppHandle) -> TalkSettings {
    let config = load_config();
    TalkSettings {
        enabled: config.enabled,
        shortcut: config.shortcut,
        mode: config.mode,
        keep_ready: config.keep_ready,
        continue_minutes: config.continue_minutes,
        key: app.state::<Hotkeys>().status(app, Binding::Talk),
    }
}

#[tauri::command]
pub async fn talk_settings(app: AppHandle) -> CommandResult<TalkSettings> {
    Ok(settings(&app))
}

#[tauri::command]
pub async fn set_talk_settings(
    app: AppHandle,
    enabled: bool,
    shortcut: Option<String>,
    mode: Option<KeyMode>,
    keep_ready: Option<bool>,
    continue_minutes: Option<u32>,
) -> CommandResult<TalkSettings> {
    let mut config = load_config();
    if let Some(shortcut) = shortcut {
        hotkey::parse(&shortcut).map_err(|e| CommandError::new("invalid", e))?;
        config.shortcut = shortcut.trim().to_string();
    }
    config.enabled = enabled;
    config.mode = mode.unwrap_or(config.mode);
    config.keep_ready = keep_ready.unwrap_or(config.keep_ready);
    config.continue_minutes = continue_minutes.unwrap_or(config.continue_minutes);
    let claim = if enabled {
        Some(hotkey::parse(&config.shortcut).map_err(|e| CommandError::new("invalid", e))?)
    } else {
        None
    };
    if claim.is_some() && crate::writing::configured_shortcut() == claim {
        return Err(CommandError::new("invalid", "that is already writing help's shortcut"));
    }
    if claim.is_some() && crate::voicetyping::claimed_shortcut() == claim {
        return Err(CommandError::new("invalid", "that is already voice typing's shortcut"));
    }
    save_config(&config).map_err(|e| CommandError::new("talk", e))?;
    // A key switched off ends the conversation it started: nobody has the key
    // to end it with, and the microphone is open.
    if !config.enabled {
        let mut inner = lock();
        if let Some(live) = active_mut(&mut inner) {
            let fx = finish(live, None);
            apply(&app, live, fx);
        }
    }
    follow(&app, &config);
    let hotkeys = app.state::<Hotkeys>();
    let answer = hotkeys.answer();
    hotkeys.set_talk(&app, claim)?;
    answer.await;
    Ok(settings(&app))
}

/// Start a conversation, or end the one under way: the button in Settings
/// and the tray's item, for trying it without the key.
#[tauri::command]
pub async fn talk_toggle(app: AppHandle) -> CommandResult<()> {
    let ended = {
        let mut inner = lock();
        match active_mut(&mut inner) {
            Some(live) => {
                live.quiet_since = Instant::now();
                let fx = finish(live, None);
                apply(&app, live, fx);
                true
            }
            None => false,
        }
    };
    if !ended {
        begin(&app, Begin::Command);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Tone;
    use key::{Act, Posture};

    fn live(posture: Posture, now: Instant) -> Live {
        let mut card = card::State::new();
        card.key = "Ctrl+Alt+A".into();
        Live::new(1, ConversationId::new(), posture, card, now)
    }

    fn state(state: VoiceState) -> VoiceEvent {
        VoiceEvent::State { state }
    }

    #[test]
    fn the_defaults_are_off_toggle_ready_and_ten_minutes() {
        let config = Config::default();
        assert!(!config.enabled);
        assert_eq!(config.shortcut, DEFAULT_SHORTCUT);
        assert_eq!(config.mode, KeyMode::Toggle);
        assert!(config.keep_ready);
        assert_eq!(config.continue_minutes, 10);
        // A file with only what somebody changed is read with the rest as it was.
        let read: Config =
            serde_json::from_str(r#"{"enabled": true, "keepReady": false}"#).unwrap();
        assert!(read.enabled && !read.keep_ready);
        assert_eq!(read.continue_minutes, 10);
        assert_eq!(read.shortcut, DEFAULT_SHORTCUT);
    }

    #[test]
    fn a_toggled_conversation_shows_it_is_ended_by_a_press() {
        let now = Instant::now();
        let live = live(Posture::Toggled, now);
        assert_eq!(live.card.hint, card::Hint::End);
        assert_eq!(hint_for(Posture::Held { since: now }, false), card::Hint::Release);
        assert_eq!(hint_for(Posture::Muted, false), card::Hint::Hold);
        assert_eq!(hint_for(Posture::Free, false), card::Hint::End);
    }

    #[test]
    fn a_conversation_the_name_began_shows_what_to_say_where_the_others_show_the_key() {
        let now = Instant::now();
        let mut live = live(Posture::Toggled, now);
        assert_eq!(live.card.hint, card::Hint::End);
        live.woke = true;
        live.sync_hint();
        assert_eq!(live.card.hint, card::Hint::Say);
        assert_eq!(
            card::build(&live.card).body.as_deref(),
            Some("Say “that's all” when you're done")
        );
        // The mute button changes nothing about it: the card falls back to the
        // key itself, which is the only thing a muted microphone leaves.
        toggle_mute(&mut live);
        assert_eq!(live.card.hint, card::Hint::Say);
        assert_eq!(card::build(&live.card).body.as_deref(), Some("Press Ctrl+Alt+A to end"));

        // The key's own postures keep their own hints, whatever began them.
        assert_eq!(hint_for(Posture::Held { since: now }, true), card::Hint::Release);
        assert_eq!(hint_for(Posture::Muted, true), card::Hint::Hold);
        assert_eq!(hint_for(Posture::Free, true), card::Hint::Say);
        assert_eq!(hint_for(Posture::Toggled, false), card::Hint::End);
    }

    #[test]
    fn only_the_name_alone_is_answered_with_the_listening_sound() {
        let turn = |said: &str| Opening::Turn(said.to_string());
        assert_eq!(opening(None), Opening::Nothing, "the key and the button have the card in view");
        assert_eq!(opening(Some(String::new())), Opening::Chime);
        assert_eq!(opening(Some("  \n ".into())), Opening::Chime, "a breath is not words");
        assert_eq!(opening(Some("type I'm late".into())), turn("type I'm late"));
        assert_eq!(opening(Some(" ok ".into())), turn(" ok "), "what was said, as it was");
    }

    #[test]
    fn letting_go_of_a_hold_shuts_the_microphone_after_its_tail() {
        let down = Instant::now();
        let mut live = live(Posture::Held { since: down }, down);
        live.card.hear(&state(VoiceState::Listening));

        let fx = transition(&mut live, Act::Mute, down + Duration::from_secs(3));
        assert_eq!(live.posture, Posture::Muted);
        assert_eq!(live.card.hint, card::Hint::Hold);
        let generation = fx.tail.expect("a look at the key once its tail has gone");
        assert_eq!(fx.muted, None, "not yet: the last word may be on its way in");

        let fx = release_due(&mut live, generation);
        assert_eq!(fx.muted, Some(true));
        assert!(live.card.muted);
    }

    #[test]
    fn nobody_is_muted_mid_sentence() {
        let down = Instant::now();
        let mut live = live(Posture::Held { since: down }, down);
        let now = down + Duration::from_secs(3);
        hear(&mut live, &state(VoiceState::Hearing), now);
        let generation = transition(&mut live, Act::Mute, now).tail.unwrap();

        // The tail has passed and they are still being heard: wait.
        let fx = release_due(&mut live, generation);
        assert_eq!(fx.muted, None);
        assert!(live.mute_when_heard);
        assert!(!live.card.muted);

        // Still hearing: still waiting.
        let fx = hear(&mut live, &VoiceEvent::Heard { text: "hello".into() }, now);
        assert_eq!(fx.muted, None);

        // The pause that ends the utterance has gone by: what was said is
        // being understood, and the microphone may be shut.
        let fx = hear(&mut live, &state(VoiceState::Understanding), now);
        assert_eq!(fx.muted, Some(true));
        assert!(!live.mute_when_heard);
        assert!(live.card.muted);
    }

    #[test]
    fn pressing_again_before_the_tail_is_up_cancels_the_mute() {
        let down = Instant::now();
        let mut live = live(Posture::Held { since: down }, down);
        let generation = transition(&mut live, Act::Mute, down).tail.unwrap();
        let fx = transition(&mut live, Act::Unmute, down + Duration::from_millis(100));
        assert_eq!(fx.muted, Some(false));
        assert!(matches!(live.posture, Posture::Held { .. }));
        assert_eq!(release_due(&mut live, generation), Effects::default());
        assert!(!live.card.muted);
    }

    #[test]
    fn pressing_again_while_a_mute_waits_on_somebody_speaking_cancels_it_too() {
        let down = Instant::now();
        let mut live = live(Posture::Held { since: down }, down);
        live.card.hear(&state(VoiceState::Hearing));
        let generation = transition(&mut live, Act::Mute, down).tail.unwrap();
        release_due(&mut live, generation);
        assert!(live.mute_when_heard);
        transition(&mut live, Act::Unmute, down);
        assert!(!live.mute_when_heard);
        let fx = hear(&mut live, &state(VoiceState::Thinking), down);
        assert_eq!(fx.muted, None);
    }

    #[test]
    fn a_tap_is_hands_free_and_is_never_muted() {
        let down = Instant::now();
        let mut live = live(Posture::Held { since: down }, down);
        let generation = live.release_gen;
        let fx = transition(&mut live, Act::HandsFree, down);
        assert_eq!(live.posture, Posture::Free);
        assert_eq!(fx.muted, None);
        assert_eq!(fx.tail, None);
        assert_eq!(live.card.hint, card::Hint::End);
        assert_eq!(release_due(&mut live, generation), Effects::default());
    }

    #[test]
    fn the_end_button_and_a_press_say_ending_at_once_and_only_once() {
        let now = Instant::now();
        let mut live = live(Posture::Toggled, now);
        live.card.hear(&state(VoiceState::Listening));
        let fx = transition(&mut live, Act::End, now);
        assert!(fx.stop && fx.draw);
        assert!(live.card.ending);
        assert_eq!(finish(&mut live, None), Effects::default(), "a second ask does nothing");
        assert_eq!(crate::talk::card::build(&live.card).title, "Ending…");
    }

    #[test]
    fn the_idle_ending_leaves_its_reason_on_the_card() {
        let now = Instant::now();
        let mut live = live(Posture::Toggled, now);
        live.card.hear(&state(VoiceState::Listening));
        finish(&mut live, Some(IDLE_NOTE));
        hear(&mut live, &VoiceEvent::Ended { error: None }, now);
        let card = card::build(&live.card);
        assert_eq!(card.title, "Conversation ended");
        assert_eq!(card.body.as_deref(), Some(IDLE_NOTE));
        assert_eq!(card.tone, Tone::Success);
        assert!(!live.active());
    }

    #[test]
    fn the_mute_button_works_whatever_the_key_is_doing() {
        let now = Instant::now();
        let mut live = live(Posture::Toggled, now);
        let fx = toggle_mute(&mut live);
        assert_eq!(fx.muted, Some(true));
        assert_eq!(live.posture, Posture::Toggled, "a press still ends it");
        let fx = toggle_mute(&mut live);
        assert_eq!(fx.muted, Some(false));

        // Hands-free in hold mode: muting is what letting go would have done.
        let mut live = self::live(Posture::Free, now);
        toggle_mute(&mut live);
        assert_eq!(live.posture, Posture::Muted);
        assert_eq!(live.card.hint, card::Hint::Hold);
        toggle_mute(&mut live);
        assert_eq!(live.posture, Posture::Free);
    }

    #[test]
    fn what_the_session_does_counts_the_quiet_from_then() {
        let start = Instant::now();
        let mut live = live(Posture::Toggled, start);
        let later = start + Duration::from_secs(90);
        hear(&mut live, &state(VoiceState::Listening), later);
        assert_eq!(live.quiet_since, later);
        let even_later = later + Duration::from_secs(30);
        hear(&mut live, &VoiceEvent::Heard { text: "hello".into() }, even_later);
        assert_eq!(live.quiet_since, even_later);
        // Everything else the session says is not the person.
        hear(&mut live, &VoiceEvent::Trouble { message: "x".into() }, even_later + IDLE_TICK);
        assert_eq!(live.quiet_since, even_later);
    }

    #[test]
    fn typing_and_dictating_are_the_person_doing_something() {
        let start = Instant::now();
        let mut live = live(Posture::Toggled, start);
        let mut later = start;
        for event in [
            VoiceEvent::Dictation { active: true },
            VoiceEvent::Typed { text: "Hello.".into() },
            VoiceEvent::Typed { text: "Again.".into() },
            VoiceEvent::Dictation { active: false },
        ] {
            later += Duration::from_secs(40);
            let fx = hear(&mut live, &event, later);
            assert_eq!(live.quiet_since, later, "{event:?}");
            assert!(fx.draw, "{event:?}");
            assert!(!fx.ended, "{event:?}");
        }
    }

    #[test]
    fn a_woken_conversation_that_is_dictating_is_not_ended_for_a_short_quiet() {
        let start = Instant::now();
        let mut live = live(Posture::Toggled, start);
        live.woke = true;
        live.sync_hint();
        hear(&mut live, &state(VoiceState::Listening), start);
        assert_eq!(live.idle_limit(), conversation::WOKEN_IDLE_AFTER);

        hear(&mut live, &VoiceEvent::Dictation { active: true }, start);
        assert!(live.card.dictating());
        assert_eq!(
            live.idle_limit(),
            conversation::IDLE_AFTER,
            "a dictation thinks between sentences"
        );
        let pause = start + conversation::WOKEN_IDLE_AFTER + Duration::from_secs(5);
        assert!(!conversation::idle_expired(
            live.card.phase,
            live.quiet_since,
            pause,
            live.idle_limit()
        ));

        // Done, it is a woken conversation again, its quiet counted from then.
        let done = start + Duration::from_secs(60);
        hear(&mut live, &VoiceEvent::Dictation { active: false }, done);
        assert_eq!(live.idle_limit(), conversation::WOKEN_IDLE_AFTER);
        let soon = done + conversation::WOKEN_IDLE_AFTER;
        assert!(conversation::idle_expired(
            live.card.phase,
            live.quiet_since,
            soon,
            live.idle_limit()
        ));

        // A conversation that ended in the middle of one is not dictating.
        hear(&mut live, &VoiceEvent::Dictation { active: true }, done);
        hear(&mut live, &VoiceEvent::Ended { error: None }, done);
        assert!(!live.card.dictating());
    }

    #[test]
    fn a_conversation_that_has_ended_cannot_be_ended_or_muted_again() {
        let now = Instant::now();
        let mut live = live(Posture::Toggled, now);
        hear(&mut live, &VoiceEvent::Ended { error: None }, now);
        assert_eq!(finish(&mut live, None), Effects::default());
        assert_eq!(toggle_mute(&mut live), Effects::default());
        let generation = live.release_gen;
        assert_eq!(release_due(&mut live, generation), Effects::default());
    }

    #[test]
    fn the_main_window_is_told_a_conversation_began_once_and_ended_once() {
        let mut live = live(Posture::Toggled, Instant::now());
        assert!(!live.end_announce(), "never told it began, so nothing to take back");
        assert!(live.begin_announce());
        assert!(!live.begin_announce(), "told once");
        assert!(live.end_announce());
        assert!(!live.end_announce(), "and once");
    }

    #[test]
    fn a_conversation_that_is_already_over_is_not_told_as_begun() {
        // The session ended the moment it opened: its ending came first.
        let now = Instant::now();
        let mut ended = live(Posture::Toggled, now);
        hear(&mut ended, &VoiceEvent::Ended { error: None }, now);
        assert!(!ended.begin_announce());
        assert!(!ended.end_announce());

        let mut refused = live(Posture::Toggled, now);
        refused.card.refused("A meeting is being recorded.");
        assert!(!refused.begin_announce());
    }

    #[test]
    fn taking_a_conversation_down_ends_what_the_main_window_was_told() {
        let now = Instant::now();
        // The vault locked while it was running: no `Ended` has come.
        let mut inner = Inner::new();
        let mut running = live(Posture::Toggled, now);
        assert!(running.begin_announce());
        inner.live = Some(running);
        assert!(replace_live(&mut inner, None), "the player is owed its ending");
        assert!(!replace_live(&mut inner, None), "and only the once");

        // One that ended on its own has had its ending already.
        let mut ended = live(Posture::Toggled, now);
        ended.begin_announce();
        hear(&mut ended, &VoiceEvent::Ended { error: None }, now);
        assert!(ended.end_announce(), "on_event sends it");
        inner.live = Some(ended);
        assert!(!replace_live(&mut inner, None));

        // One that never opened was never told it began.
        inner.live = Some(live(Posture::Toggled, now));
        assert!(!replace_live(&mut inner, None));
    }

    #[test]
    fn a_new_conversation_replacing_an_announced_one_ends_the_old() {
        let now = Instant::now();
        let mut inner = Inner::new();
        let mut old = live(Posture::Toggled, now);
        old.begin_announce();
        inner.live = Some(old);
        assert!(replace_live(&mut inner, Some(live(Posture::Toggled, now))));
        assert!(inner.live.as_ref().is_some_and(|l| !l.announced));
    }
}
