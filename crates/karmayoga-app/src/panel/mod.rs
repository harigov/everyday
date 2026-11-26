//! The panel: the small card in the corner of the screen that Karma Yoga
//! talks to the person through when they are looking at something else.
//!
//! It began as the meeting card ("Recording starts in 12s"). It is now the
//! one surface every part of the shell uses for that, whatever it has to
//! say: a meeting about to start, with its brief and a button to take notes;
//! voice typing listening, with what it has heard so far; a conversation with
//! the assistant started from the talk key, with what was said on both sides.
//! A part of the shell does not draw anything itself. It says what its card
//! holds -- a [`Panel`] -- and is told when one of the card's buttons is
//! pressed ([`on_action`]).
//!
//! # Several at once
//!
//! The panel is a stack of cards, one per [`Panel::id`], in the order they
//! were first shown: a meeting offer can arrive while the person is talking
//! to the assistant, and neither should push the other off. Showing a card
//! with an id already up replaces it where it is. The stack is drawn as one
//! window, top-right, never taking the keyboard from whatever has it.
//!
//! # Where it is drawn
//!
//! Everywhere a window can be kept on top and put where it asks, the panel is
//! a webview window of its own ([`window`], the page in `ui/src/panel/`). On
//! Wayland neither is allowed, so there it is this program run again under
//! X11 through XWayland, drawing the stack with GTK ([`topcard`]); only
//! without XWayland, or if that fails, is it the webview window, wherever the
//! desktop puts it. Both draw the same [`Panel`]s, so a card looks the same
//! whichever draws it, and neither decides anything: a press is reported
//! back here, and the owner answers it.
//!
//! # How many are drawn
//!
//! A renderer draws the first [`MAX_SHOWN`] cards of the stack, top to bottom
//! in the order the stack holds them, and under them a quiet "+N more" when
//! the stack is longer ([`shown`] and [`hidden`] say which and how many). The
//! cards that wait are not lost or squeezed in: they are still in the stack,
//! and the first of them is drawn the moment a card above it is taken down.
//! The newest card goes to the bottom, so a meeting offer arriving in the
//! middle of a conversation with the assistant does not move the card the
//! person is reading.
//!
//! # Calling it
//!
//! [`show`], [`update`], [`dismiss`], [`dismiss_owner`] and [`level`] never
//! wait: not for a window, not for the main thread, not for another thread
//! that is drawing. They change the stack under its own lock, which is held
//! for the length of a copy and a comparison, and tell a worker thread, which
//! reads the stack as it is when it gets to it and draws that. So they may be
//! called with a lock of the caller's own held -- the shell's global-key
//! handlers run on the main thread on macOS and Windows, and take their
//! owner's lock to answer a key -- without a window that is waiting for that
//! thread being able to hold it up. The one thing to keep out of them is a
//! call to the panel from inside the closure given to [`update`], which runs
//! with the stack locked; and a lock that the closure takes must not be one
//! that a caller of the panel holds while calling it.
//!
//! Because the worker reads the stack when it runs rather than being handed a
//! copy, the renderers converge on the latest stack however many callers
//! change it at once: a change made after the worker read the stack wakes it
//! again.
//!
//! # Levels
//!
//! A card with [`Panel::meter`] set shows how loud the microphone is. Levels
//! come about twenty times a second; they go to the renderer on their own
//! ([`level`], [`LEVEL_EVENT`]) rather than as a whole new stack each time,
//! and only when the level has moved a whole per cent -- a quiet room says the
//! same thing twenty times a second.

#[cfg(target_os = "linux")]
pub mod topcard;
pub mod window;

use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use karmayoga_service::error::CommandResult;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

/// Tauri event: the whole stack, on every change. Payload: `Vec<Panel>`. The
/// panel window listens, and nothing else.
pub const STATE_EVENT: &str = "karmayoga://panel/state";

/// Tauri event: how loud the microphone is for one card. Payload:
/// [`PanelLevel`].
pub const LEVEL_EVENT: &str = "karmayoga://panel/level";

/// The most cards drawn at once. Older ones beyond it wait, still in the
/// stack, until one above them goes.
// This and the three functions after `drawn_order` serve the X11 renderer
// (`topcard`); elsewhere the page draws the cards and counts them itself.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const MAX_SHOWN: usize = 3;

/// Which part of the shell a card belongs to, and so who hears its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Owner {
    /// A meeting offer and its brief -- [`crate::offers`].
    Meeting,
    /// Voice typing into another application -- [`crate::voicetyping`].
    VoiceTyping,
    /// A conversation started from the talk key -- [`crate::talk`].
    Talk,
}

/// What a card is doing, for its accent colour and the mark beside its
/// kicker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    #[default]
    Neutral,
    /// Hearing the person: the microphone is open.
    Listening,
    /// Working something out.
    Working,
    /// Done, and fine.
    Success,
    /// Needs the person: a question, a countdown.
    Attention,
    /// Something went wrong.
    Trouble,
}

/// One card. Sent whole, in [`STATE_EVENT`]'s stack, on every change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    /// Which card this is: `event:{calendarId}:{uid}` for a meeting offer,
    /// `voice-typing`, `talk`. Showing a card with the same id replaces it.
    pub id: String,
    pub owner: Owner,
    #[serde(default)]
    pub tone: Tone,
    /// The small line above the title: "MEETING · 10:00–10:30", "LISTENING".
    /// Drawn in capitals by the renderer; written here as a sentence.
    pub kicker: String,
    pub title: String,
    /// A large number beside the title: the countdown's "12s".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<String>,
    /// A line under the title: where a meeting is, which key stops
    /// listening, why something failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// What is being said, newest last: what voice typing has heard, or the
    /// last exchange with the assistant. The renderer shows the last few.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
    /// Show a level meter, fed by [`level`].
    #[serde(default)]
    pub meter: bool,
    /// Lists under the lines: a meeting's agenda, who is coming, what to
    /// raise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    /// The buttons, in order of importance: the primary one -- at most one is
    /// -- first. Renderers draw them left to right with the primary one
    /// rightmost, where a desktop's dialogs put the button that goes ahead
    /// ([`drawn_order`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Something is under way that the card is waiting on: a spinner, and the
    /// buttons stay pressable.
    #[serde(default)]
    pub busy: bool,
}

/// A line of speech, or of anything else said in turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// Who said it -- "You", the assistant's name -- or nobody, for voice
    /// typing's single running line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
    pub text: String,
    /// Still coming: still being heard, or still being written. Drawn with a
    /// caret.
    #[serde(default)]
    pub pending: bool,
}

/// A titled list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub heading: String,
    pub items: Vec<Item>,
    /// Folded under "More" until the person opens the card: the long parts
    /// of a brief.
    #[serde(default)]
    pub folded: bool,
}

/// One entry in a [`Section`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub text: String,
    /// A second, quieter line: who somebody is, where a fact came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A box to tick. Ticking it sends `check.action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// The action [`on_action`] is told when the box is ticked.
    pub action: String,
    pub done: bool,
}

/// A button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    /// What [`on_action`] is told when it is pressed.
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub primary: bool,
    /// Text the renderer puts on the clipboard itself when the button is
    /// pressed, before reporting it: on Wayland only a window that is pressed
    /// may set the clipboard, and that is the renderer's, not the shell's.
    /// [`on_action`] is told whether it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<String>,
}

/// How loud the microphone is for the card `id`, from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelLevel<'a> {
    #[serde(borrow)]
    pub id: &'a str,
    pub level: f32,
}

/// The stack, for the life of the process.
#[derive(Default)]
pub struct Panels {
    stack: Mutex<Vec<Panel>>,
    /// The last level sent on, whole per cents, so one that has not moved is
    /// not sent again. Kept apart from the stack: levels arrive twenty times a
    /// second and must not queue behind a renderer being told about a change.
    last_level: Mutex<Option<(String, u8)>>,
}

impl Panels {
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Panel>) -> R) -> R {
        f(&mut self.stack.lock().expect("panel lock"))
    }

    /// Whether a level of `step` per cent for the card `id` is worth sending:
    /// the card is up with a meter, and the meter is not already there.
    fn heard(&self, id: &str, step: u8) -> bool {
        if !self.with(|stack| has_meter(stack, id)) {
            return false;
        }
        let mut last = self.last_level.lock().expect("panel level lock");
        if last.as_ref().is_some_and(|(sent, was)| sent == id && *was == step) {
            return false;
        }
        *last = Some((id.to_string(), step));
        true
    }

    /// Whatever is drawn is about to be told the whole stack, which may draw
    /// a meter afresh: the next level goes out even if it is the last again.
    fn forget_level(&self) {
        self.last_level.lock().expect("panel level lock").take();
    }
}

fn panels(app: &AppHandle) -> tauri::State<'_, Panels> {
    app.state::<Panels>()
}

// ---- the stack, without an app ----------------------------------------------
//
// The operations below are the whole of what the stack does. They are plain
// functions on a `Vec<Panel>`, and say whether they changed it, so what they
// promise -- order kept, nothing published when nothing changed -- is tested
// without a window or an `AppHandle`.

/// Put `panel` on the stack: at the bottom, or in place of the card with its
/// id, where that is. True if the stack is not what it was.
fn put(stack: &mut Vec<Panel>, panel: Panel) -> bool {
    match stack.iter_mut().find(|p| p.id == panel.id) {
        Some(shown) if *shown == panel => false,
        Some(shown) => {
            *shown = panel;
            true
        }
        None => {
            stack.push(panel);
            true
        }
    }
}

/// Let `edit` alter the card `id`, where it is. `None` if it is not on the
/// stack; otherwise whether `edit` altered anything.
///
/// A card's id is what it is, so one `edit` rewrites is put back: two cards
/// with the same id would answer for each other's buttons.
fn change(stack: &mut [Panel], id: &str, edit: impl FnOnce(&mut Panel)) -> Option<bool> {
    let shown = stack.iter_mut().find(|p| p.id == id)?;
    let before = shown.clone();
    edit(shown);
    shown.id.clear();
    shown.id.push_str(id);
    Some(*shown != before)
}

/// Take the card `id` off the stack. True if it was there.
fn take(stack: &mut Vec<Panel>, id: &str) -> bool {
    let before = stack.len();
    stack.retain(|p| p.id != id);
    stack.len() != before
}

/// Take every card `owner` has off the stack. True if there were any.
fn take_owner(stack: &mut Vec<Panel>, owner: Owner) -> bool {
    let before = stack.len();
    stack.retain(|p| p.owner != owner);
    stack.len() != before
}

/// Whether the stack has a card `id` with a meter.
fn has_meter(stack: &[Panel], id: &str) -> bool {
    stack.iter().any(|p| p.id == id && p.meter)
}

/// A card's buttons in the order they are drawn, left to right: as listed,
/// except that the primary one goes last, rightmost. Owners list the primary
/// first, as the most important; the page does the same (`ui/src/panel/
/// cards.ts`'s `drawnOrder`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn drawn_order(actions: &[Action]) -> Vec<&Action> {
    let (primary, rest): (Vec<&Action>, Vec<&Action>) = actions.iter().partition(|a| a.primary);
    rest.into_iter().chain(primary).collect()
}

/// The cards a renderer draws: the first [`MAX_SHOWN`], top to bottom.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn shown(stack: &[Panel]) -> &[Panel] {
    &stack[..stack.len().min(MAX_SHOWN)]
}

/// How many cards wait beyond [`shown`]: the "+N more".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn hidden(stack: &[Panel]) -> usize {
    stack.len().saturating_sub(MAX_SHOWN)
}

/// A level as a whole number of per cents, the finest the meter shows. One
/// that is not a number is silence, as it is when it is below nothing.
fn per_cent(level: f32) -> u8 {
    if level.is_nan() { 0 } else { (level.clamp(0.0, 1.0) * 100.0).round() as u8 }
}

// ---- the stack, in the app ----------------------------------------------------

/// Put `panel` up, or replace the card with its id where it is.
pub fn show(app: &AppHandle, panel: Panel) {
    if panels(app).with(|stack| put(stack, panel)) {
        publish(app);
    }
}

/// Change the card `id` in place. False, and nothing done, if it is not up.
///
/// `edit` runs with the stack locked: it changes the card and nothing else.
pub fn update(app: &AppHandle, id: &str, edit: impl FnOnce(&mut Panel)) -> bool {
    match panels(app).with(|stack| change(stack, id, edit)) {
        Some(true) => {
            publish(app);
            true
        }
        Some(false) => true,
        None => false,
    }
}

/// Take the card `id` down, if it is up.
pub fn dismiss(app: &AppHandle, id: &str) {
    if panels(app).with(|stack| take(stack, id)) {
        publish(app);
    }
}

/// Take down every card `owner` has up.
pub fn dismiss_owner(app: &AppHandle, owner: Owner) {
    if panels(app).with(|stack| take_owner(stack, owner)) {
        publish(app);
    }
}

/// The card `id`, if it is up.
pub fn get(app: &AppHandle, id: &str) -> Option<Panel> {
    panels(app).with(|stack| stack.iter().find(|p| p.id == id).cloned())
}

/// The whole stack, in the order it is drawn.
pub fn current(app: &AppHandle) -> Vec<Panel> {
    panels(app).with(|stack| stack.clone())
}

/// How loud the microphone is for the card `id`, from 0 to 1. Dropped if the
/// card is not up or has no meter, or if the meter is already there.
///
/// Called about twenty times a second, so it takes two short locks and
/// allocates only when the level has moved.
pub fn level(app: &AppHandle, id: &str, level: f32) {
    let step = per_cent(level);
    if panels(app).heard(id, step) {
        wake(app, Wake::Level(id.to_string(), f32::from(step) / 100.0));
    }
}

/// The stack has changed: tell whoever draws the panel, in time.
fn publish(app: &AppHandle) {
    panels(app).forget_level();
    wake(app, Wake::Stack);
}

// ---- the worker -------------------------------------------------------------------

/// What wakes the thread that keeps the renderers up to date.
#[derive(Debug, Clone, PartialEq)]
enum Wake {
    /// The stack changed; draw it as it is now.
    Stack,
    /// A level for one card's meter.
    Level(String, f32),
}

/// What the worker has been woken for since it last drew: the stack changed,
/// and the latest level of each meter. Many wakes are one job, and the job is
/// always the newest state, never one in the middle of the way to it.
#[derive(Debug, Default, PartialEq)]
struct Batch {
    stack: bool,
    /// Latest level per card, in the order the cards were first heard from.
    levels: Vec<(String, f32)>,
}

impl Batch {
    fn add(&mut self, wake: Wake) {
        match wake {
            Wake::Stack => self.stack = true,
            Wake::Level(id, level) => {
                match self.levels.iter_mut().find(|(known, _)| *known == id) {
                    Some(heard) => heard.1 = level,
                    None => self.levels.push((id, level)),
                }
            }
        }
    }
}

/// Wakes the thread that keeps the renderers up to date.
static WORKER: OnceLock<Sender<Wake>> = OnceLock::new();

fn wake(app: &AppHandle, wake: Wake) {
    let _ = WORKER.get_or_init(|| work(app.clone())).send(wake);
}

/// The thread [`wake`] wakes. One, so what is drawn is drawn in order, and the
/// only thing that ever waits on a window: the callers of the panel never do.
///
/// It reads the stack when it runs, not when it was woken. A change made after
/// it read the stack has woken it again by the time it is done, so the last
/// change is always the last thing drawn -- whichever caller made it, and
/// whichever order the callers got to `wake` in.
fn work(app: AppHandle) -> Sender<Wake> {
    start(move |batch| draw(&app, batch))
}

/// Start a thread that gives `draw` what it has been woken for, a batch at a
/// time, until every sender is gone.
fn start(mut draw: impl FnMut(Batch) + Send + 'static) -> Sender<Wake> {
    let (tx, rx) = channel::<Wake>();
    let spawned = std::thread::Builder::new().name("panel-hub".into()).spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch = Batch::default();
            batch.add(first);
            while let Ok(next) = rx.try_recv() {
                batch.add(next);
            }
            draw(batch);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "the panel's thread could not start");
    }
    tx
}

/// Tell the renderers what the batch says: the stack as it is now, then the
/// levels, so a card drawn afresh has its meter set after it exists.
fn draw(app: &AppHandle, batch: Batch) {
    if batch.stack {
        let stack = current(app);
        let _ = app.emit_to(window::LABEL, STATE_EVENT, &stack);
        window::refresh(app, !stack.is_empty());
    }
    for (id, level) in batch.levels {
        if window::drawing() {
            let _ = app.emit_to(window::LABEL, LEVEL_EVENT, PanelLevel { id: &id, level });
        }
        #[cfg(target_os = "linux")]
        topcard::level(&id, level);
    }
}

/// A button on a card was pressed, or a box ticked: tell the card's owner.
/// `copied` says the renderer has already put the button's
/// [`Action::copy`] text on the clipboard.
pub fn on_action(app: &AppHandle, id: &str, action: &str, copied: bool) {
    let Some(owner) = get(app, id).map(|p| p.owner) else {
        tracing::debug!(id, action, "a press on a card that is no longer up");
        return;
    };
    match owner {
        Owner::Meeting => crate::offers::on_panel_action(app, id, action),
        Owner::VoiceTyping => crate::voicetyping::on_panel_action(app, action, copied),
        Owner::Talk => crate::talk::on_panel_action(app, action),
    }
}

// ---- commands ---------------------------------------------------------------

/// The stack, for a panel page that has only just loaded.
#[tauri::command]
pub fn panel_state(app: AppHandle) -> Vec<Panel> {
    current(&app)
}

/// A button pressed, or a box ticked, in the panel page.
#[tauri::command]
pub async fn panel_action(
    app: AppHandle,
    id: String,
    action: String,
    copied: Option<bool>,
) -> CommandResult<()> {
    on_action(&app, &id, &action, copied.unwrap_or(false));
    Ok(())
}

/// The panel page saying how tall its cards are, in logical pixels, so the
/// window can be that tall and no taller -- the space below it stays the
/// desktop's.
#[tauri::command]
pub fn panel_resize(app: AppHandle, height: f64) {
    window::resize(&app, height);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_button_is_drawn_rightmost() {
        let action = |id: &str, primary: bool| Action {
            id: id.into(),
            label: id.into(),
            primary,
            copy: None,
        };
        let listed = [action("take", true), action("notNow", false), action("open", false)];
        let ids: Vec<&str> = drawn_order(&listed).iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["notNow", "open", "take"]);
        let none = [action("end", false), action("mute", false)];
        let ids: Vec<&str> = drawn_order(&none).iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["end", "mute"], "without a primary the order is the card's own");
    }

    fn card(id: &str) -> Panel {
        Panel {
            id: id.into(),
            owner: Owner::Meeting,
            tone: Tone::Neutral,
            kicker: "Meeting".into(),
            title: "Design sync".into(),
            count: None,
            body: None,
            lines: Vec::new(),
            meter: false,
            sections: Vec::new(),
            actions: Vec::new(),
            busy: false,
        }
    }

    #[test]
    fn a_card_reads_back_as_the_page_expects() {
        let mut panel = card("event:1:abc");
        panel.actions.push(Action {
            id: "take".into(),
            label: "Take notes".into(),
            primary: true,
            copy: None,
        });
        let json = serde_json::to_value(&panel).unwrap();
        assert_eq!(json["owner"], "meeting");
        assert_eq!(json["tone"], "neutral");
        assert_eq!(json["actions"][0]["primary"], true);
        assert!(json.get("count").is_none(), "absent, not null");
        let back: Panel = serde_json::from_value(json).unwrap();
        assert_eq!(back, panel);
    }

    #[test]
    fn a_card_the_page_sends_back_without_the_optional_parts_still_reads() {
        let panel: Panel = serde_json::from_value(serde_json::json!({
            "id": "talk",
            "owner": "talk",
            "kicker": "Assistant",
            "title": "Listening",
        }))
        .unwrap();
        assert_eq!(panel.tone, Tone::Neutral);
        assert!(panel.lines.is_empty() && panel.sections.is_empty() && panel.actions.is_empty());
        assert!(!panel.meter && !panel.busy);
        assert_eq!(panel.owner, Owner::Talk);
    }

    #[test]
    fn parts_of_a_card_use_the_names_the_page_does() {
        let mut panel = card("event:1:abc");
        panel.tone = Tone::Attention;
        panel.owner = Owner::VoiceTyping;
        panel.lines.push(Line { who: Some("You".into()), text: "hello".into(), pending: true });
        panel.sections.push(Section {
            heading: "To raise".into(),
            items: vec![Item {
                text: "Budget".into(),
                detail: Some("with Josh".into()),
                check: Some(Check { action: "done:7".into(), done: false }),
            }],
            folded: true,
        });
        panel.actions.push(Action {
            id: "copy".into(),
            label: "Copy".into(),
            primary: false,
            copy: Some("words".into()),
        });
        let json = serde_json::to_value(&panel).unwrap();
        assert_eq!(json["owner"], "voiceTyping");
        assert_eq!(json["tone"], "attention");
        assert_eq!(json["lines"][0]["pending"], true);
        assert_eq!(json["sections"][0]["folded"], true);
        assert_eq!(json["sections"][0]["items"][0]["check"]["action"], "done:7");
        assert_eq!(json["actions"][0]["copy"], "words");
        let back: Panel = serde_json::from_value(json).unwrap();
        assert_eq!(back, panel);
    }

    #[test]
    fn a_level_goes_out_under_the_names_the_page_listens_for() {
        let json = serde_json::to_value(PanelLevel { id: "talk", level: 0.5 }).unwrap();
        assert_eq!(json, serde_json::json!({ "id": "talk", "level": 0.5 }));
    }

    fn ids(stack: &[Panel]) -> Vec<&str> {
        stack.iter().map(|p| p.id.as_str()).collect()
    }

    #[test]
    fn a_new_card_goes_to_the_bottom() {
        let mut stack = Vec::new();
        assert!(put(&mut stack, card("a")));
        assert!(put(&mut stack, card("b")));
        assert!(put(&mut stack, card("c")));
        assert_eq!(ids(&stack), ["a", "b", "c"]);
    }

    #[test]
    fn a_card_shown_again_is_replaced_where_it_is() {
        let mut stack = vec![card("a"), card("b"), card("c")];
        let mut again = card("b");
        again.title = "Moved to 11".into();
        assert!(put(&mut stack, again));
        assert_eq!(ids(&stack), ["a", "b", "c"], "the order is the order they were first shown");
        assert_eq!(stack[1].title, "Moved to 11");
    }

    #[test]
    fn a_card_shown_again_unchanged_is_no_change() {
        let mut stack = vec![card("a"), card("b")];
        assert!(!put(&mut stack, card("a")), "nothing to publish");
        assert_eq!(ids(&stack), ["a", "b"]);
    }

    #[test]
    fn changing_a_card_that_is_not_up_does_nothing() {
        let mut stack = vec![card("a")];
        let mut called = false;
        assert_eq!(change(&mut stack, "gone", |_| called = true), None);
        assert!(!called);
        assert_eq!(stack, vec![card("a")]);
    }

    #[test]
    fn changing_a_card_says_whether_anything_changed() {
        let mut stack = vec![card("a"), card("b")];
        assert_eq!(change(&mut stack, "b", |p| p.count = Some("12s".into())), Some(true));
        assert_eq!(stack[1].count.as_deref(), Some("12s"));
        assert_eq!(change(&mut stack, "b", |p| p.count = Some("12s".into())), Some(false));
        assert_eq!(change(&mut stack, "b", |_| {}), Some(false));
        assert_eq!(ids(&stack), ["a", "b"]);
    }

    #[test]
    fn a_card_cannot_change_its_own_id() {
        let mut stack = vec![card("a"), card("b")];
        let outcome = change(&mut stack, "a", |p| {
            p.id = "b".into();
            p.title = "Not the same".into();
        });
        assert_eq!(outcome, Some(true));
        assert_eq!(ids(&stack), ["a", "b"]);
        assert_eq!(stack[0].title, "Not the same");
        // Only the id was tried: that alone is no change.
        assert_eq!(change(&mut stack, "a", |p| p.id = "z".into()), Some(false));
        assert_eq!(ids(&stack), ["a", "b"]);
    }

    #[test]
    fn taking_a_card_down_keeps_the_rest_in_order() {
        let mut stack = vec![card("a"), card("b"), card("c")];
        assert!(take(&mut stack, "b"));
        assert_eq!(ids(&stack), ["a", "c"]);
        assert!(!take(&mut stack, "b"), "already down: nothing to publish");
        assert!(!take(&mut stack, "nobody"));
        assert_eq!(ids(&stack), ["a", "c"]);
    }

    #[test]
    fn taking_down_an_owners_cards_leaves_the_others() {
        let mut talk = card("talk");
        talk.owner = Owner::Talk;
        let mut typing = card("voice-typing");
        typing.owner = Owner::VoiceTyping;
        let mut stack = vec![card("a"), talk, card("b"), typing];
        assert!(take_owner(&mut stack, Owner::Meeting));
        assert_eq!(ids(&stack), ["talk", "voice-typing"]);
        assert!(!take_owner(&mut stack, Owner::Meeting), "none left: nothing to publish");
        assert!(take_owner(&mut stack, Owner::Talk));
        assert_eq!(ids(&stack), ["voice-typing"]);
    }

    #[test]
    fn at_most_three_are_drawn_and_the_rest_are_counted() {
        let stack: Vec<Panel> = (0..5).map(|n| card(&format!("c{n}"))).collect();
        assert_eq!(ids(shown(&stack)), ["c0", "c1", "c2"]);
        assert_eq!(hidden(&stack), 2);
        assert_eq!(ids(shown(&stack[..2])), ["c0", "c1"]);
        assert_eq!(hidden(&stack[..2]), 0);
        assert_eq!(hidden(&stack[..MAX_SHOWN]), 0);
        assert!(shown(&[]).is_empty());
        assert_eq!(hidden(&[]), 0);
    }

    #[test]
    fn taking_the_top_card_down_draws_the_one_that_waited() {
        let mut stack: Vec<Panel> = (0..4).map(|n| card(&format!("c{n}"))).collect();
        assert_eq!(hidden(&stack), 1);
        assert!(take(&mut stack, "c0"));
        assert_eq!(ids(shown(&stack)), ["c1", "c2", "c3"]);
        assert_eq!(hidden(&stack), 0);
    }

    #[test]
    fn a_level_is_whole_per_cents_and_never_leaves_the_meter() {
        assert_eq!(per_cent(0.0), 0);
        assert_eq!(per_cent(0.5), 50);
        assert_eq!(per_cent(0.504), 50);
        assert_eq!(per_cent(0.506), 51);
        assert_eq!(per_cent(1.0), 100);
        assert_eq!(per_cent(7.0), 100);
        assert_eq!(per_cent(-3.0), 0);
        assert_eq!(per_cent(f32::NAN), 0);
        assert_eq!(per_cent(f32::INFINITY), 100);
        assert_eq!(per_cent(f32::NEG_INFINITY), 0);
    }

    fn metered(id: &str) -> Panel {
        let mut panel = card(id);
        panel.meter = true;
        panel
    }

    #[test]
    fn only_a_card_with_a_meter_hears_levels() {
        let panels = Panels::default();
        panels.with(|stack| {
            put(stack, card("plain"));
            put(stack, metered("talk"));
        });
        assert!(!panels.heard("plain", 40), "no meter");
        assert!(!panels.heard("gone", 40), "not up");
        assert!(panels.heard("talk", 40));
    }

    #[test]
    fn a_level_that_has_not_moved_is_not_sent_again() {
        let panels = Panels::default();
        panels.with(|stack| {
            put(stack, metered("talk"));
        });
        assert!(panels.heard("talk", 0), "the first, even if it is silence");
        assert!(!panels.heard("talk", 0));
        assert!(panels.heard("talk", 1));
        assert!(!panels.heard("talk", 1));
        assert!(panels.heard("talk", 0));
    }

    #[test]
    fn a_new_stack_lets_the_same_level_through_once_more() {
        let panels = Panels::default();
        panels.with(|stack| {
            put(stack, metered("talk"));
        });
        assert!(panels.heard("talk", 30));
        assert!(!panels.heard("talk", 30));
        panels.forget_level();
        assert!(panels.heard("talk", 30), "a meter drawn afresh starts at nothing");
    }

    #[test]
    fn many_wakes_are_one_job() {
        let mut batch = Batch::default();
        assert_eq!(batch, Batch { stack: false, levels: Vec::new() });
        batch.add(Wake::Stack);
        batch.add(Wake::Stack);
        batch.add(Wake::Stack);
        assert!(batch.stack);
        assert!(batch.levels.is_empty());
    }

    #[test]
    fn only_the_latest_level_of_each_meter_is_kept() {
        let mut batch = Batch::default();
        batch.add(Wake::Level("talk".into(), 0.1));
        batch.add(Wake::Level("typing".into(), 0.5));
        batch.add(Wake::Level("talk".into(), 0.3));
        batch.add(Wake::Level("talk".into(), 0.2));
        assert!(!batch.stack);
        assert_eq!(batch.levels, [("talk".to_string(), 0.2), ("typing".to_string(), 0.5)]);
    }

    /// Many threads changing the stack and waking the worker as `publish`
    /// does -- change, then wake, with nothing between that orders them --
    /// and the worker drawing whatever the stack is when it gets to it. The
    /// last thing drawn is the final stack, however they interleave: the
    /// order in which two callers' wakes arrive is not the order in which
    /// their changes were made, and does not have to be.
    #[test]
    fn the_last_state_drawn_is_the_last_state_there_is() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        for round in 0..20 {
            let panels = Arc::new(Panels::default());
            let drawn: Arc<Mutex<Vec<Panel>>> = Arc::default();
            let tx = {
                let (panels, drawn) = (panels.clone(), drawn.clone());
                start(move |batch| {
                    if batch.stack {
                        *drawn.lock().unwrap() = panels.with(|stack| stack.clone());
                    }
                })
            };
            let threads: Vec<_> = (0..4)
                .map(|t| {
                    let (panels, tx) = (panels.clone(), tx.clone());
                    std::thread::spawn(move || {
                        for n in 0..100 {
                            let id = format!("card{}", (t + n) % 3);
                            let changed = panels.with(|stack| {
                                if (n + round) % 3 == 0 {
                                    take(stack, &id)
                                } else {
                                    let mut panel = card(&id);
                                    panel.title = format!("{t}:{n}");
                                    put(stack, panel)
                                }
                            });
                            if changed {
                                let _ = tx.send(Wake::Stack);
                            }
                        }
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            let last = panels.with(|stack| stack.clone());
            let wait = Instant::now();
            while *drawn.lock().unwrap() != last {
                assert!(
                    wait.elapsed() < Duration::from_secs(10),
                    "round {round}: the last state drawn is not the last there is"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn two_meters_do_not_hide_each_others_levels() {
        let panels = Panels::default();
        panels.with(|stack| {
            put(stack, metered("a"));
            put(stack, metered("b"));
        });
        assert!(panels.heard("a", 30));
        assert!(panels.heard("b", 30));
        assert!(panels.heard("a", 30), "b was heard since");
    }
}
