//! The panel: the cards in a corner of the screen that Jatayu talks to the
//! person through while they are looking at something else.
//!
//! It is the one surface every part of the shell uses for that: a meeting
//! about to start, with who is coming and what to raise; one being recorded,
//! with a box for the person's own notes and questions; new mail, answered on
//! the card; the action items a meeting's notes found, taken or left one by
//! one; the assistant asking for a yes or an answer; voice typing listening;
//! a conversation started from the talk key. A part of the shell -- an owner
//! -- draws nothing itself. It says what its card holds, a [`Panel`], and is
//! told when something on the card is pressed, ticked or sent
//! ([`on_action`]).
//!
//! # What a card holds
//!
//! A head drawn the same way on every card -- the app's mark and the kind's,
//! a kicker, a title, a count, a meter -- and under it a list of [`Block`]s,
//! each a structured thing drawn as itself: an event as an event, a task as a
//! task, a note as a note, who said what as a conversation with the person on
//! one side and the assistant on the other, the mode and the tool at work as
//! an activity. The page has no code for any one owner; it draws blocks. A
//! new kind of card is an owner putting blocks together, not a new renderer.
//!
//! # Several at once
//!
//! The panel is a stack of cards, one per [`Panel::id`], in the order they
//! were first shown: a meeting offer can arrive while the person is talking
//! to the assistant, and neither should push the other off. Showing a card
//! with an id already up replaces it where it is. The stack is drawn as one
//! window, never taking the keyboard from whatever has it unless the person
//! clicks into a box on a card.
//!
//! # Where it is drawn
//!
//! One page draws the panel everywhere, `ui/src/panel/`, in a webview window
//! of its own ([`window`]), in the theme the main window shows ([`set_theme`]).
//! Everywhere a window can be kept on top and put where it asks, that window
//! is in this process. Wayland allows neither, so there it is this program
//! run again under X11 through XWayland ([`child`]): a small Tauri app with
//! that one window and that same page. Only without XWayland, or if that
//! fails, is it the window in this process, wherever the desktop puts it.
//! Neither decides anything: a press is reported back here, and the owner
//! answers it.
//!
//! The panel opens in the top right of the primary monitor until the person
//! drags it by a card's head; from then on it opens where they left it
//! ([`place`]).
//!
//! # How many are drawn
//!
//! A renderer draws the first [`MAX_SHOWN`] cards of the stack, top to bottom
//! in the order the stack holds them, and under them a quiet "+N more" when
//! the stack is longer ([`shown`] and [`hidden`] say which and how many). The
//! cards that wait are not lost or squeezed in: they are still in the stack,
//! and the first of them is drawn the moment a card above it is taken down --
//! with all its time still to run, if it is one that goes by itself.
//! The newest card goes to the bottom, so a meeting offer arriving in the
//! middle of a conversation with the assistant does not move the card the
//! person is reading.
//!
//! # Going by itself
//!
//! A card with [`Panel::expires`] goes after a while: new mail nobody
//! answered, action items nobody decided. The hub keeps that time -- not the
//! owner, and not the page, of which there may be two in two processes. One
//! clock per card: counting from when its expiry first appears on a card that
//! is drawn, started again when the owner changes it, stopped while the person
//! is reading the card ([`hold`]: the pointer on it, or typing in it) and
//! while it waits under "+N more" -- a card nobody has seen must not go by
//! itself, since its owner may have kept the desktop's notification back for
//! it -- and dropped with the card.
//! When it runs out the owner is told the expiry's action, as if it had been
//! pressed, and then the card is taken down -- unless the owner, told, kept
//! it by giving it another expiry or none. One thread waits for the earliest
//! clock that is counting. What the renderers are sent has the time left
//! filled in, so the page can draw it draining; what owners put up, and what
//! is compared to see whether anything changed, is the owner's own copy.
//!
//! # Calling it
//!
//! [`show`], [`update`], [`dismiss`], [`dismiss_owner`], [`level`], [`hold`]
//! and [`set_theme`] never wait: not for a window, not for the main thread,
//! not for another thread that is drawing. They change the stack under its
//! own lock, which is held for the length of a copy and a comparison, and
//! tell a worker thread, which reads the stack as it is when it gets to it
//! and draws that. So they may be called with a lock of the caller's own held
//! -- the shell's global-key handlers run on the main thread on macOS and
//! Windows, and take their owner's lock to answer a key -- without a window
//! that is waiting for that thread being able to hold it up. The one thing to
//! keep out of them is a call to the panel from inside the closure given to
//! [`update`], which runs with the stack locked; and a lock that the closure
//! takes must not be one that a caller of the panel holds while calling it.
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
pub mod child;
pub mod place;
pub mod settings;
pub mod window;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use jatayu_service::error::CommandResult;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

/// Tauri event: the whole stack, on every change. Payload: `Vec<Panel>`. The
/// panel window listens, and nothing else.
pub const STATE_EVENT: &str = "jatayu://panel/state";

/// Tauri event: how loud the microphone is for one card. Payload:
/// [`PanelLevel`].
pub const LEVEL_EVENT: &str = "jatayu://panel/level";

/// Tauri event: the theme the main window is showing, resolved (`"dusk"`,
/// never `"system"`), so the panel -- which may be another process with no
/// storage of its own -- is drawn in it. Payload: `Option<String>`.
pub const THEME_EVENT: &str = "jatayu://panel/theme";

/// The most cards drawn at once. Older ones beyond it wait, still in the
/// stack, until one above them goes.
pub const MAX_SHOWN: usize = 3;

// ---- the contract with the page ------------------------------------------------
//
// Everything below is mirrored field for field in `ui/src/panel/panel.ts`.
// Every type is camelCase on the wire; an optional field is absent, never
// null; a list that is empty is absent.

/// Which part of the shell a card belongs to, and so who hears its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Owner {
    /// A meeting about to start, its brief -- [`crate::offers`].
    Meeting,
    /// Voice typing into another application -- [`crate::voicetyping`].
    VoiceTyping,
    /// A conversation started from the talk key -- [`crate::talk`].
    Talk,
    /// New mail and invitations, answered on the card -- [`crate::mailcard`].
    Mail,
    /// A meeting's notes written, and the action items found in them --
    /// [`crate::notescard`].
    Notes,
    /// A meeting being recorded: your own notes, questions about what has
    /// been said -- [`crate::recordcard`].
    Recording,
    /// The assistant needs a yes, a no or an answer -- [`crate::askcard`].
    Ask,
}

/// What a card is about: its mark (the app symbol beside the kicker) and its
/// accent colour (that app's `--app-*`). Independent of [`Owner`]: mail's
/// owner draws both [`Kind::Mail`] and [`Kind::Invite`] cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    #[default]
    Notice,
    /// A meeting about to start, or one being recorded: the calendar's.
    Meeting,
    /// A message: mail's.
    Mail,
    /// An invitation to answer: the calendar's.
    Invite,
    /// Notes written, action items: notes'.
    Notes,
    /// The assistant talking or asking: the assistant's.
    Assistant,
    /// Voice typing: the assistant's, with a microphone.
    VoiceTyping,
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
///
/// The head (kicker, title, count, body) is always drawn the same way; what is
/// under it is a list of [`Block`]s, each a structured thing drawn as itself --
/// an event as an event, a task as a task, who said what as a conversation --
/// rather than as lines of text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    /// Which card this is: `event:{calendarId}:{uid}` for a meeting offer,
    /// `mail:{messageId}`, `notes:{recordingId}`, `recording:{id}`,
    /// `ask:{questionId}`, `voice-typing`, `talk`. Showing a card with the same
    /// id replaces it.
    pub id: String,
    pub owner: Owner,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub tone: Tone,
    /// The small line above the title: "Meeting · in 5 min", "New mail".
    /// Written as a sentence; the renderer decides its case.
    pub kicker: String,
    pub title: String,
    /// A large number beside the title: the countdown's "12s".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<String>,
    /// A line under the title: why something failed, which key ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// What the card holds, top to bottom.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Block>,
    /// Show a level meter in the head, fed by [`level`].
    #[serde(default)]
    pub meter: bool,
    /// The buttons along the foot, in order of importance: the primary one --
    /// at most one is -- first. Renderers draw them with the primary one
    /// rightmost ([`drawn_order`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Something is under way that the card is waiting on: a spinner, and the
    /// buttons stay pressable.
    #[serde(default)]
    pub busy: bool,
    /// The card goes by itself after a while, unless the person is reading it
    /// (the pointer on it, or typing in it, holds it: [`hold`]). The hub keeps
    /// the time; see [`Expiry`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Expiry>,
}

/// When a card goes by itself.
///
/// The owner sets `after_ms` and `action`. The hub counts from when the card
/// (or this expiry) was first drawn -- one of the first [`MAX_SHOWN`] -- and
/// stops counting while the card is held or waits under "+N more",
/// and when the time is up tells the owner `action` through [`on_action`] and
/// then takes the card down if the owner has not. `remaining_ms` and `held`
/// are the hub's, filled in on the copy sent to the renderer so it can draw
/// the time draining; what an owner puts there is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expiry {
    pub after_ms: u64,
    pub action: String,
    #[serde(default)]
    pub remaining_ms: u64,
    #[serde(default)]
    pub held: bool,
}

/// One structured part of a card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Block {
    /// What is happening right now: listening, thinking, speaking,
    /// recording -- and the tool at work, with its mark.
    Activity {
        mode: Mode,
        /// Said instead of the mode's own word: "Working out what you said…".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<ToolUse>,
    },
    /// Who said what, in turn: the person on one side, the assistant on the
    /// other, never run together.
    Conversation { turns: Vec<Turn> },
    /// A calendar event.
    Event(EventCard),
    /// A mail message.
    Mail(MailCard),
    /// People: who is coming, and who they are.
    People {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading: Option<String>,
        people: Vec<Person>,
        /// Under "More" until the card is opened.
        #[serde(default)]
        folded: bool,
    },
    /// A task.
    Task(TaskCard),
    /// A note.
    Note(NoteCard),
    /// A paragraph, perhaps headed: an agenda, a description, a summary.
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading: Option<String>,
        text: String,
        #[serde(default)]
        folded: bool,
    },
    /// Text shown whole and exactly, in a well: what is about to be typed or
    /// copied, the thing a question is about.
    Quote {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        text: String,
    },
    /// A titled list.
    List(Section),
    /// Things to take or leave one by one: a meeting's action items. A press
    /// sends `accept:{id}` or `reject:{id}`.
    Proposals {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading: Option<String>,
        items: Vec<Proposal>,
    },
    /// Answers to pick from, one press each: a question's choices.
    Choices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        choices: Vec<Action>,
    },
    /// A box to type in: a quick reply, a note, a question. Typing in it takes
    /// the keyboard (the window asks for it then and gives it back after).
    Input(Input),
    /// A running clock, counted by the renderer from `since_ms` (Unix ms), so
    /// it ticks without the stack being sent every second.
    Clock {
        since_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// What an [`Block::Activity`] is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    Idle,
    Starting,
    /// The microphone is open, waiting for words.
    Listening,
    /// Words are coming.
    Hearing,
    Thinking,
    /// A tool is at work.
    Working,
    Speaking,
    /// What is said is typed where the person was typing.
    Dictating,
    Muted,
    /// Waiting for the person: a yes or no, an answer.
    Waiting,
    /// A meeting is being recorded.
    Recording,
    Sending,
    Done,
    Failed,
}

/// A tool at work, as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUse {
    /// The tool's own name: `search_mail`.
    pub name: String,
    /// What it is doing, in words: "Searching your mail…".
    pub label: String,
    /// Which mark it gets.
    #[serde(default)]
    pub icon: ToolIcon,
}

/// The mark a tool gets: what it touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolIcon {
    #[default]
    Generic,
    Mail,
    Calendar,
    Tasks,
    Notes,
    Journal,
    Web,
    Files,
    Image,
    Typing,
    Memory,
    Money,
    Health,
    News,
}

/// One turn of a [`Block::Conversation`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub who: Speaker,
    /// The name shown over it: the assistant's own name. The renderer says
    /// "You" for the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
    /// Still coming: still being heard, or still being written. A caret.
    #[serde(default)]
    pub pending: bool,
}

/// Who said a [`Turn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Speaker {
    /// The person, to the assistant.
    You,
    /// The assistant.
    Assistant,
    /// What voice typing heard, which is going elsewhere.
    Heard,
    /// What has been typed where the person was typing.
    Typed,
}

/// A calendar event, as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCard {
    pub title: String,
    /// "10:00–10:30", or "All day".
    pub when: String,
    /// "Today", "Tomorrow", "Thu 27 Nov".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The service a join link is for: "Zoom", "Google Meet".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<String>,
    /// The calendar it is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
    /// Who is coming, as faces in a row; a [`Block::People`] says who they
    /// are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<Person>,
    /// The person's own answer to it, where it is an invitation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsvp: Option<Rsvp>,
}

/// Somebody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Who they are, in a line: "Design lead at Acme".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Their answer to the invitation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Rsvp>,
    #[serde(default)]
    pub organizer: bool,
    /// The person themself.
    #[serde(default)]
    pub you: bool,
}

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Rsvp {
    Accepted,
    Tentative,
    Declined,
    NeedsAction,
}

/// A mail message, as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailCard {
    pub from: Person,
    pub subject: String,
    /// The first words of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// "2:14 pm".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received: Option<String>,
    /// Which of the person's addresses it came to, where they have several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default)]
    pub attachments: u32,
}

/// A task, as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCard {
    pub title: String,
    /// A quieter line: the project, who it is with, "Added to your tasks".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// "Fri", "Tomorrow".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(default)]
    pub done: bool,
    /// A box to tick, sending this action. None draws no box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
}

/// A note, as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteCard {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One thing to take or leave in a [`Block::Proposals`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: String,
    pub title: String,
    /// Who it is for, where it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(default)]
    pub state: Decision,
}

/// Where a [`Proposal`] stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    #[default]
    Pending,
    Accepted,
    Rejected,
}

/// A box to type in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    /// The action a submit sends, with the text, when there are no
    /// [`Input::modes`].
    pub id: String,
    pub placeholder: String,
    /// The submit button's word: "Send", "Add", "Ask".
    pub submit: String,
    #[serde(default)]
    pub multiline: bool,
    /// Ready-made texts: a press puts one in the box, to send or change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// Ways to use the box, one at a time -- "Note" or "Ask" -- picked above
    /// it; a submit sends the picked mode's id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<InputMode>,
    /// What was sent is being dealt with: the box waits.
    #[serde(default)]
    pub busy: bool,
    /// What went wrong with the last submit. The renderer puts the text back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A ready-made text for an [`Input`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    /// A few words on the chip: "Sounds good".
    pub label: String,
    /// What goes in the box.
    pub text: String,
}

/// One way to use an [`Input`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMode {
    pub id: String,
    pub label: String,
    pub placeholder: String,
    pub submit: String,
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
    /// Drawn as a warning: delete, decline.
    #[serde(default)]
    pub danger: bool,
    /// A mark beside the label, from a fixed set the renderer knows:
    /// `reply`, `archive`, `read`, `open`, `close`, `check`, `x`, `copy`,
    /// `join`, `mic`, `micOff`, `stop`, `send`, `sparkle`, `later`. One it
    /// does not know draws no mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Text the renderer puts on the clipboard itself when the button is
    /// pressed, before reporting it: on Wayland only a window that is pressed
    /// may set the clipboard, and that is the renderer's, not the shell's.
    /// [`on_action`] is told whether it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<String>,
}

impl Action {
    /// A plain button.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Action {
        Action {
            id: id.into(),
            label: label.into(),
            primary: false,
            danger: false,
            icon: None,
            copy: None,
        }
    }

    /// The button that goes ahead.
    pub fn primary(mut self) -> Action {
        self.primary = true;
        self
    }

    pub fn danger(mut self) -> Action {
        self.danger = true;
        self
    }

    pub fn icon(mut self, icon: &str) -> Action {
        self.icon = Some(icon.to_string());
        self
    }
}

impl Panel {
    /// A card with nothing but its head, for an owner to fill in.
    pub fn new(
        id: impl Into<String>,
        owner: Owner,
        kind: Kind,
        kicker: impl Into<String>,
        title: impl Into<String>,
    ) -> Panel {
        Panel {
            id: id.into(),
            owner,
            kind,
            tone: Tone::Neutral,
            kicker: kicker.into(),
            title: title.into(),
            count: None,
            body: None,
            blocks: Vec::new(),
            meter: false,
            actions: Vec::new(),
            busy: false,
            expires: None,
        }
    }
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
    /// The cards, and the clocks of those that go by themselves: under one
    /// lock, so a clock is never out of step with its card.
    stack: Mutex<Hub>,
    /// The last level sent on, whole per cents, so one that has not moved is
    /// not sent again. Kept apart from the stack: levels arrive twenty times a
    /// second and must not queue behind a renderer being told about a change.
    last_level: Mutex<Option<(String, u8)>>,
    /// The theme the main window shows, for the panel to be drawn in.
    theme: Mutex<Option<String>>,
}

/// What the stack's lock guards.
#[derive(Default)]
struct Hub {
    cards: Vec<Panel>,
    clocks: Clocks,
}

impl Panels {
    /// Change the cards. The clocks are brought in line with whatever `f`
    /// did before the lock is let go.
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Panel>) -> R) -> R {
        let mut hub = self.stack.lock().expect("panel lock");
        let out = f(&mut hub.cards);
        let Hub { cards, clocks } = &mut *hub;
        clocks.sync(cards, Instant::now());
        out
    }

    /// Look at the cards.
    fn read<R>(&self, f: impl FnOnce(&[Panel]) -> R) -> R {
        f(&self.stack.lock().expect("panel lock").cards)
    }

    /// Whether a level of `step` per cent for the card `id` is worth sending:
    /// the card is up with a meter, and the meter is not already there.
    fn heard(&self, id: &str, step: u8) -> bool {
        if !self.read(|stack| has_meter(stack, id)) {
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

    /// The card `id` is being read (`held`), or is not any more. True if that
    /// changes what the renderers are told: the card is up and has a clock
    /// that was not already so.
    fn hold(&self, id: &str, held: bool, now: Instant) -> bool {
        let mut hub = self.stack.lock().expect("panel lock");
        if !hub.cards.iter().any(|p| p.id == id) {
            return false;
        }
        hub.clocks.hold(id, held, now)
    }

    /// Whether the card `id` is held.
    fn held(&self, id: &str) -> bool {
        self.stack.lock().expect("panel lock").clocks.held.contains(id)
    }

    /// Let go of every hold. True if one was on a card with a clock.
    fn release_holds(&self, now: Instant) -> bool {
        self.stack.lock().expect("panel lock").clocks.release_all(now)
    }

    /// The cards as the renderers are sent them, at `now`.
    fn drawn(&self, now: Instant) -> Vec<Panel> {
        let hub = self.stack.lock().expect("panel lock");
        let mut cards = hub.cards.clone();
        for card in &mut cards {
            hub.clocks.fill(card, now);
        }
        cards
    }

    /// When the next clock runs out, of those counting.
    fn next_deadline(&self) -> Option<Instant> {
        self.stack.lock().expect("panel lock").clocks.next()
    }

    /// The cards whose time is up at `now`, each with its expiry's action;
    /// each is told once.
    fn due(&self, now: Instant) -> Vec<(String, String)> {
        self.stack.lock().expect("panel lock").clocks.due(now)
    }

    /// Whether the card `id` ran out of time and its owner, told, left it as
    /// it was.
    fn ran_out(&self, id: &str) -> bool {
        self.stack.lock().expect("panel lock").clocks.ran_out(id)
    }

    /// Keep `theme`. True if it is not the one kept already.
    fn set_theme(&self, theme: Option<String>) -> bool {
        let mut kept = self.theme.lock().expect("panel theme lock");
        if *kept == theme {
            return false;
        }
        *kept = theme;
        true
    }

    fn theme(&self) -> Option<String> {
        self.theme.lock().expect("panel theme lock").clone()
    }
}

fn panels(app: &AppHandle) -> tauri::State<'_, Panels> {
    app.state::<Panels>()
}

/// Whether this process is the panel's child ([`child`]), which only draws:
/// the app sends it the stack with the time already filled in, and keeps the
/// clocks itself.
fn in_child() -> bool {
    #[cfg(target_os = "linux")]
    {
        child::is_child()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
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

/// Make the stack `new`, whole: what the panel's child does with the stack
/// the app sends it. True if it is not what it was.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn replace(stack: &mut Vec<Panel>, new: Vec<Panel>) -> bool {
    if *stack == new {
        return false;
    }
    *stack = new;
    true
}

/// Whether the stack has a card `id` with a meter.
fn has_meter(stack: &[Panel], id: &str) -> bool {
    stack.iter().any(|p| p.id == id && p.meter)
}

/// A card's buttons in the order they are drawn, left to right: as listed,
/// except that the primary one goes last, rightmost. Owners list the primary
/// first, as the most important; the page does the same (`ui/src/panel/
/// cards.ts`'s `drawnOrder`), and this is the rule written down where the
/// owners can test their cards against it.
#[allow(dead_code)]
pub fn drawn_order(actions: &[Action]) -> Vec<&Action> {
    let (primary, rest): (Vec<&Action>, Vec<&Action>) = actions.iter().partition(|a| a.primary);
    rest.into_iter().chain(primary).collect()
}

/// The cards a renderer draws: the first [`MAX_SHOWN`], top to bottom. The
/// page's rule (`ui/src/panel/cards.ts`'s `visible`), written down here for
/// the same reason as [`drawn_order`], and kept by the clocks: only a card
/// drawn counts down.
pub fn shown(stack: &[Panel]) -> &[Panel] {
    &stack[..stack.len().min(MAX_SHOWN)]
}

/// How many cards wait beyond [`shown`]: the "+N more".
#[allow(dead_code)]
pub fn hidden(stack: &[Panel]) -> usize {
    stack.len().saturating_sub(MAX_SHOWN)
}

/// A level as a whole number of per cents, the finest the meter shows. One
/// that is not a number is silence, as it is when it is below nothing.
fn per_cent(level: f32) -> u8 {
    if level.is_nan() { 0 } else { (level.clamp(0.0, 1.0) * 100.0).round() as u8 }
}

// ---- the clocks, without an app ---------------------------------------------------

/// The hub's count of each card's [`Expiry`], which cards are being read, and
/// which wait undrawn.
#[derive(Debug, Default)]
struct Clocks {
    /// One per card on the stack that has an expiry.
    running: HashMap<String, Clock>,
    /// The cards the person is reading, whether or not they have a clock: a
    /// card held when its expiry appears starts with its clock stopped.
    held: HashSet<String>,
    /// The cards beyond the first [`MAX_SHOWN`], under "+N more": not drawn,
    /// so not seen, so not counting down. A card only ever moves up the
    /// stack, so one of these starts counting, with the whole of its time,
    /// when a card above it goes; should one ever move down, it stops.
    waiting: HashSet<String>,
}

/// One card's count down.
#[derive(Debug, Clone, PartialEq)]
struct Clock {
    /// What the owner asked for. Either changing starts the count again.
    after_ms: u64,
    action: String,
    /// What was left when the clock was last started or stopped.
    left: Duration,
    /// When it was last started; none while the card is held.
    since: Option<Instant>,
    /// It ran out and the owner has been told.
    rang: bool,
}

impl Clock {
    fn new(expiry: &Expiry, held: bool, now: Instant) -> Clock {
        Clock {
            after_ms: expiry.after_ms,
            action: expiry.action.clone(),
            left: Duration::from_millis(expiry.after_ms),
            since: (!held).then_some(now),
            rang: false,
        }
    }

    /// Whether this is the clock for `expiry`, as the owner set it.
    fn counts(&self, expiry: &Expiry) -> bool {
        self.after_ms == expiry.after_ms && self.action == expiry.action
    }

    /// What is left at `now`.
    fn left_at(&self, now: Instant) -> Duration {
        match self.since {
            _ if self.rang => Duration::ZERO,
            Some(since) => self.left.saturating_sub(now.saturating_duration_since(since)),
            None => self.left,
        }
    }

    /// When it runs out, if it is counting.
    fn deadline(&self) -> Option<Instant> {
        if self.rang { None } else { self.since.map(|since| since + self.left) }
    }

    fn stop(&mut self, now: Instant) {
        if self.since.is_some() {
            self.left = self.left_at(now);
            self.since = None;
        }
    }

    fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            self.since = Some(now);
        }
    }
}

impl Clocks {
    /// Bring the clocks in line with `stack` at `now`: a clock for every card
    /// with an expiry, new for one that is new or was changed, and none for
    /// anything else; counting for the cards drawn, and stopped for those
    /// held or waiting beyond them.
    fn sync(&mut self, stack: &[Panel], now: Instant) {
        self.held.retain(|id| stack.iter().any(|p| p.id == *id));
        self.running.retain(|id, clock| {
            stack.iter().any(|p| p.id == *id && p.expires.as_ref().is_some_and(|e| clock.counts(e)))
        });
        self.waiting = stack[shown(stack).len()..].iter().map(|p| p.id.clone()).collect();
        for panel in stack {
            if let Some(expiry) = &panel.expires
                && !self.running.contains_key(&panel.id)
            {
                let stopped = self.held.contains(&panel.id) || self.waiting.contains(&panel.id);
                self.running.insert(panel.id.clone(), Clock::new(expiry, stopped, now));
            }
        }
        for (id, clock) in &mut self.running {
            if self.held.contains(id) || self.waiting.contains(id) {
                clock.stop(now);
            } else {
                clock.start(now);
            }
        }
    }

    /// The person is reading the card `id` (`held`), or has stopped. Its
    /// clock, if it has one, stops or goes on from where it was -- unless the
    /// card waits undrawn, when it stays stopped. True if the hold changed on
    /// a card with a clock.
    fn hold(&mut self, id: &str, held: bool, now: Instant) -> bool {
        let changed = if held { self.held.insert(id.to_string()) } else { self.held.remove(id) };
        let waiting = self.waiting.contains(id);
        match self.running.get_mut(id) {
            Some(clock) if changed => {
                if held || waiting {
                    clock.stop(now)
                } else {
                    clock.start(now)
                }
                true
            }
            _ => false,
        }
    }

    /// Let go of every hold at `now`: whatever was holding them is gone. True
    /// if a hold was let go on a card with a clock.
    fn release_all(&mut self, now: Instant) -> bool {
        let held: Vec<String> = self.held.iter().cloned().collect();
        let mut changed = false;
        for id in held {
            changed |= self.hold(&id, false, now);
        }
        changed
    }

    /// The earliest moment a clock that is counting runs out.
    fn next(&self) -> Option<Instant> {
        self.running.values().filter_map(Clock::deadline).min()
    }

    /// The cards whose clocks have run out at `now`, with the action each
    /// owner is to be told, in order of id. Each is marked, so it is told
    /// once.
    fn due(&mut self, now: Instant) -> Vec<(String, String)> {
        let mut due: Vec<(String, String)> = self
            .running
            .iter_mut()
            .filter(|(_, clock)| clock.deadline().is_some_and(|at| at <= now))
            .map(|(id, clock)| {
                clock.rang = true;
                (id.clone(), clock.action.clone())
            })
            .collect();
        due.sort();
        due
    }

    /// Whether the card `id`'s clock ran out and is still the one that did:
    /// the owner, told, neither changed its expiry nor took it away.
    fn ran_out(&self, id: &str) -> bool {
        self.running.get(id).is_some_and(|clock| clock.rang)
    }

    /// Fill in `panel`'s expiry as a renderer is sent it: the time left at
    /// `now`, and whether the card is held.
    fn fill(&self, panel: &mut Panel, now: Instant) {
        let Some(expiry) = panel.expires.as_mut() else { return };
        expiry.held = self.held.contains(&panel.id);
        expiry.remaining_ms = match self.running.get(&panel.id) {
            Some(clock) => u64::try_from(clock.left_at(now).as_millis()).unwrap_or(u64::MAX),
            None => expiry.after_ms,
        };
    }
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

/// Make the stack `stack`, whole: the panel's child, told the stack by the
/// app.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn replace_all(app: &AppHandle, stack: Vec<Panel>) {
    if panels(app).with(|cards| replace(cards, stack)) {
        publish(app);
    }
}

/// The card `id`, if it is up.
pub fn get(app: &AppHandle, id: &str) -> Option<Panel> {
    panels(app).read(|stack| stack.iter().find(|p| p.id == id).cloned())
}

/// The whole stack, in the order it is drawn, as the owners put it up.
pub fn current(app: &AppHandle) -> Vec<Panel> {
    panels(app).read(<[Panel]>::to_vec)
}

/// The whole stack as the renderers are sent it: each expiry with the time
/// left and whether the card is held filled in by the hub. In the child the
/// stack came that way from the app, and goes on as it came.
fn for_renderers(app: &AppHandle) -> Vec<Panel> {
    if in_child() { current(app) } else { panels(app).drawn(Instant::now()) }
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

/// The stack has changed: tell whoever draws the panel, in time, and the
/// thread that keeps the cards' time.
fn publish(app: &AppHandle) {
    panels(app).forget_level();
    ring(app);
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
        let stack = for_renderers(app);
        let _ = app.emit_to(window::LABEL, STATE_EVENT, &stack);
        window::refresh(app, !stack.is_empty());
    }
    for (id, level) in batch.levels {
        if window::drawing() {
            let _ = app.emit_to(window::LABEL, LEVEL_EVENT, PanelLevel { id: &id, level });
        }
        #[cfg(target_os = "linux")]
        child::level(&id, level);
    }
}

// ---- the clocks' thread -----------------------------------------------------------

/// What wakes the thread that keeps the cards' time: a change to the stack,
/// or a hold, either of which may have moved the earliest deadline. A count
/// of rings, so a ring between the thread looking at the clocks and its
/// starting to wait is not missed.
struct Alarm {
    rung: Mutex<u64>,
    bell: Condvar,
}

impl Alarm {
    fn new() -> Alarm {
        Alarm { rung: Mutex::new(0), bell: Condvar::new() }
    }

    fn rung(&self) -> u64 {
        *self.rung.lock().expect("panel alarm lock")
    }

    fn ring(&self) {
        *self.rung.lock().expect("panel alarm lock") += 1;
        self.bell.notify_all();
    }

    /// Wait until the alarm has rung since it had rung `seen` times, or until
    /// `until`, whichever comes first; with no `until`, for a ring alone.
    fn wait(&self, seen: u64, until: Option<Instant>) {
        let mut rung = self.rung.lock().expect("panel alarm lock");
        while *rung == seen {
            match until {
                None => rung = self.bell.wait(rung).expect("panel alarm lock"),
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        return;
                    }
                    rung = self.bell.wait_timeout(rung, at - now).expect("panel alarm lock").0;
                }
            }
        }
    }
}

/// Wakes the thread that keeps the cards' time.
static ALARM: OnceLock<Arc<Alarm>> = OnceLock::new();

/// Tell the thread that keeps the cards' time to look at the clocks again,
/// starting it the first time. Not in the child, where the app keeps them.
fn ring(app: &AppHandle) {
    if in_child() {
        return;
    }
    ALARM.get_or_init(|| keep_time(app.clone())).ring();
}

/// The thread [`ring`] wakes. It tells the owners of the cards whose time is
/// up, then waits for the earliest clock that is counting to run out, or for
/// a ring -- whichever comes first -- and looks again.
fn keep_time(app: AppHandle) -> Arc<Alarm> {
    let alarm = Arc::new(Alarm::new());
    let ringing = alarm.clone();
    let spawned = std::thread::Builder::new().name("panel-expiry".into()).spawn(move || {
        loop {
            let seen = ringing.rung();
            for (id, action) in panels(&app).due(Instant::now()) {
                expire(&app, &id, &action);
            }
            let next = panels(&app).next_deadline();
            ringing.wait(seen, next);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "the thread that keeps the panel's time could not start");
    }
    alarm
}

/// The card `id`'s time is up: tell its owner `action`, as a press, and take
/// the card down -- unless the owner kept it, by giving it another expiry or
/// none, or took it down itself.
fn expire(app: &AppHandle, id: &str, action: &str) {
    on_action(app, id, action, false, None);
    if panels(app).ran_out(id) {
        dismiss(app, id);
    }
}

// ---- presses, holds and the theme -------------------------------------------------

/// A button on a card was pressed, a box ticked, or text submitted from an
/// [`Input`] -- or a card's time ran out ([`Expiry::action`]): tell the
/// card's owner. `copied` says the renderer has already put the button's
/// [`Action::copy`] text on the clipboard; `text` is what was typed, for a
/// submit.
///
/// In the panel's child process ([`child`]) nothing here owns a card: the
/// press goes up the pipe to the app, which calls this there.
pub fn on_action(app: &AppHandle, id: &str, action: &str, copied: bool, text: Option<String>) {
    #[cfg(target_os = "linux")]
    if child::is_child() {
        child::pressed(id, action, copied, text);
        return;
    }
    let Some(owner) = get(app, id).map(|p| p.owner) else {
        tracing::debug!(id, action, "a press on a card that is no longer up");
        return;
    };
    match owner {
        Owner::Meeting => crate::offers::on_panel_action(app, id, action),
        Owner::VoiceTyping => crate::voicetyping::on_panel_action(app, action, copied),
        Owner::Talk => crate::talk::on_panel_action(app, action, text),
        Owner::Mail => crate::mailcard::on_panel_action(app, id, action, text),
        Owner::Notes => crate::notescard::on_panel_action(app, id, action, text),
        Owner::Recording => crate::recordcard::on_panel_action(app, id, action, text),
        Owner::Ask => crate::askcard::on_panel_action(app, id, action, text),
    }
}

/// The person is reading the card `id` -- the pointer is on it, or they are
/// typing in it -- or has stopped (`held` false). A card that is held does not
/// go by itself ([`Expiry`]); its clock stops, and goes on from where it was
/// when the card is let go.
///
/// In the child the hold goes up the pipe, to the app that keeps the clocks.
pub fn hold(app: &AppHandle, id: &str, held: bool) {
    #[cfg(target_os = "linux")]
    if child::is_child() {
        child::held(id, held);
        return;
    }
    if panels(app).hold(id, held, Instant::now()) {
        publish(app);
    }
}

/// Whether the card `id` is being read right now: the pointer on it, or a
/// box in it being typed in ([`hold`]). For an owner that ends a card by its
/// own reckoning rather than by an [`Expiry`] -- a conversation on the talk
/// card is not left alone while the person is writing in it. Always false in
/// the child, whose holds go up to the app.
pub fn held(app: &AppHandle, id: &str) -> bool {
    panels(app).held(id)
}

/// A renderer has just (re)loaded its page -- the in-process window's page
/// asking for the stack, or a new child saying it is ready. Whatever held a
/// card before was the renderer that came before it -- a page that reloaded,
/// a child that died -- and will never let go now, so every hold is let go:
/// a card held by nothing would never go by itself.
pub fn renderer_loaded(app: &AppHandle) {
    if panels(app).release_holds(Instant::now()) {
        publish(app);
    }
}

/// The theme the main window is showing, resolved, for the panel to be drawn
/// in. Sent to whatever draws the panel, now and whenever it starts.
pub fn set_theme(app: &AppHandle, theme: Option<String>) {
    if !panels(app).set_theme(theme.clone()) {
        return;
    }
    #[cfg(target_os = "linux")]
    if !child::is_child() {
        child::theme(theme.clone());
    }
    let _ = app.emit_to(window::LABEL, THEME_EVENT, theme);
}

/// The theme last set by [`set_theme`].
pub fn theme(app: &AppHandle) -> Option<String> {
    panels(app).theme()
}

// ---- commands ---------------------------------------------------------------

/// The stack, for a panel page that has only just loaded. In the child, the
/// first time, that is the child ready: its page is up and listening.
#[tauri::command]
pub fn panel_state(app: AppHandle) -> Vec<Panel> {
    #[cfg(target_os = "linux")]
    if child::is_child() {
        child::page_loaded();
        return for_renderers(&app);
    }
    renderer_loaded(&app);
    for_renderers(&app)
}

/// A button pressed, a box ticked or text submitted in the panel page.
#[tauri::command]
pub async fn panel_action(
    app: AppHandle,
    id: String,
    action: String,
    copied: Option<bool>,
    text: Option<String>,
) -> CommandResult<()> {
    on_action(&app, &id, &action, copied.unwrap_or(false), text);
    Ok(())
}

/// The panel page saying how tall its cards are, in logical pixels, so the
/// window can be that tall and no taller -- the space below it stays the
/// desktop's.
#[tauri::command]
pub fn panel_resize(app: AppHandle, height: f64) {
    window::resize(&app, height);
}

/// The page wants the keyboard (`want`: a box on a card was clicked into) or
/// is done with it (the box lost it, or was sent). The panel never takes the
/// keyboard otherwise.
#[tauri::command]
pub fn panel_focus(app: AppHandle, want: bool) {
    window::focus(&app, want);
}

/// The pointer is on the card `id`, or someone is typing in it (`held`), or
/// neither any more. See [`hold`].
#[tauri::command]
pub fn panel_hold(app: AppHandle, id: String, held: bool) {
    hold(&app, &id, held);
}

/// A press on a card's grip: move the window with the pointer. Where it is
/// let go is where the panel opens from then on.
#[tauri::command]
pub fn panel_drag(app: AppHandle) {
    window::drag(&app);
}

/// Put the panel back in its corner and forget where it was moved to.
#[tauri::command]
pub fn panel_reset_place(app: AppHandle) {
    window::reset_place(&app);
}

/// The theme to draw in, for a page that has only just loaded.
#[tauri::command]
pub fn panel_theme(app: AppHandle) -> Option<String> {
    theme(&app)
}

/// The main window saying which theme it shows, resolved. Called by the main
/// window only.
#[tauri::command]
pub fn set_panel_theme(app: AppHandle, theme: Option<String>) {
    set_theme(&app, theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str) -> Panel {
        Panel::new(id, Owner::Meeting, Kind::Meeting, "Meeting", "Design sync")
    }

    #[test]
    fn the_primary_button_is_drawn_rightmost() {
        let listed = [
            Action::new("take", "Take notes").primary(),
            Action::new("notNow", "Not now"),
            Action::new("open", "Open"),
        ];
        let ids: Vec<&str> = drawn_order(&listed).iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["notNow", "open", "take"]);
        let none = [Action::new("end", "End"), Action::new("mute", "Mute")];
        let ids: Vec<&str> = drawn_order(&none).iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["end", "mute"], "without a primary the order is the card's own");
    }

    #[test]
    fn a_card_reads_back_as_the_page_expects() {
        let mut panel = card("event:1:abc");
        panel.actions.push(Action::new("take", "Take notes").primary().icon("join"));
        let json = serde_json::to_value(&panel).unwrap();
        assert_eq!(json["owner"], "meeting");
        assert_eq!(json["kind"], "meeting");
        assert_eq!(json["tone"], "neutral");
        assert_eq!(json["actions"][0]["primary"], true);
        assert_eq!(json["actions"][0]["icon"], "join");
        assert!(json.get("count").is_none(), "absent, not null");
        assert!(json.get("blocks").is_none(), "an empty list is absent");
        assert!(json.get("expires").is_none());
        assert!(json["actions"][0].get("copy").is_none());
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
        assert_eq!(panel.kind, Kind::Notice);
        assert!(panel.blocks.is_empty() && panel.actions.is_empty());
        assert!(!panel.meter && !panel.busy);
        assert!(panel.count.is_none() && panel.body.is_none() && panel.expires.is_none());
        assert_eq!(panel.owner, Owner::Talk);
    }

    #[test]
    fn owners_kinds_and_tones_use_the_names_the_page_does() {
        let names = |value: serde_json::Value| value.as_str().unwrap().to_string();
        let owners = [
            Owner::Meeting,
            Owner::VoiceTyping,
            Owner::Talk,
            Owner::Mail,
            Owner::Notes,
            Owner::Recording,
            Owner::Ask,
        ]
        .map(|o| names(serde_json::to_value(o).unwrap()));
        assert_eq!(owners, ["meeting", "voiceTyping", "talk", "mail", "notes", "recording", "ask"]);
        let kinds = [Kind::Notice, Kind::Invite, Kind::VoiceTyping, Kind::Assistant]
            .map(|k| names(serde_json::to_value(k).unwrap()));
        assert_eq!(kinds, ["notice", "invite", "voiceTyping", "assistant"]);
        assert_eq!(serde_json::to_value(Tone::Attention).unwrap(), "attention");
        assert_eq!(serde_json::to_value(Rsvp::NeedsAction).unwrap(), "needsAction");
        assert_eq!(serde_json::to_value(ToolIcon::Calendar).unwrap(), "calendar");
        assert_eq!(serde_json::to_value(Decision::Rejected).unwrap(), "rejected");
    }

    #[test]
    fn an_event_block_is_tagged_and_its_fields_are_beside_the_tag() {
        let block = Block::Event(EventCard {
            title: "Design sync".into(),
            when: "10:00–10:30".into(),
            date: Some("Today".into()),
            place: None,
            join: Some("Zoom".into()),
            calendar: None,
            people: vec![Person {
                name: "Ana".into(),
                email: Some("ana@example.com".into()),
                detail: None,
                status: Some(Rsvp::Tentative),
                organizer: true,
                you: false,
            }],
            rsvp: Some(Rsvp::NeedsAction),
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "event");
        assert_eq!(json["title"], "Design sync");
        assert_eq!(json["join"], "Zoom");
        assert_eq!(json["rsvp"], "needsAction");
        assert_eq!(json["people"][0]["status"], "tentative");
        assert_eq!(json["people"][0]["organizer"], true);
        assert!(json.get("place").is_none() && json.get("calendar").is_none());
        assert!(json["people"][0].get("detail").is_none());
        assert_eq!(serde_json::from_value::<Block>(json).unwrap(), block);
    }

    #[test]
    fn an_input_with_modes_and_suggestions_reads_back() {
        let block = Block::Input(Input {
            id: "note".into(),
            placeholder: "Add a note".into(),
            submit: "Add".into(),
            multiline: true,
            suggestions: vec![Suggestion {
                label: "Sounds good".into(),
                text: "Sounds good!".into(),
            }],
            modes: vec![
                InputMode {
                    id: "note".into(),
                    label: "Note".into(),
                    placeholder: "Add a note".into(),
                    submit: "Add".into(),
                },
                InputMode {
                    id: "ask".into(),
                    label: "Ask".into(),
                    placeholder: "Ask about the meeting so far".into(),
                    submit: "Ask".into(),
                },
            ],
            busy: false,
            error: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "input");
        assert_eq!(json["multiline"], true);
        assert_eq!(json["modes"][1]["id"], "ask");
        assert_eq!(json["suggestions"][0]["label"], "Sounds good");
        assert!(json.get("error").is_none());
        assert_eq!(serde_json::from_value::<Block>(json).unwrap(), block);

        let bare: Block = serde_json::from_value(serde_json::json!({
            "type": "input", "id": "reply", "placeholder": "Reply", "submit": "Send",
        }))
        .unwrap();
        let Block::Input(input) = bare else { panic!("not an input") };
        assert!(input.modes.is_empty() && input.suggestions.is_empty());
        assert!(!input.multiline && !input.busy && input.error.is_none());
    }

    #[test]
    fn every_block_is_named_as_the_page_names_it() {
        let blocks = [
            (Block::Activity { mode: Mode::Working, label: None, tool: None }, "activity"),
            (Block::Conversation { turns: Vec::new() }, "conversation"),
            (Block::People { heading: None, people: Vec::new(), folded: false }, "people"),
            (Block::Text { heading: None, text: "x".into(), folded: false }, "text"),
            (Block::Quote { label: None, text: "x".into() }, "quote"),
            (
                Block::List(Section {
                    heading: "To raise".into(),
                    items: Vec::new(),
                    folded: true,
                }),
                "list",
            ),
            (Block::Proposals { heading: None, items: Vec::new() }, "proposals"),
            (Block::Choices { prompt: None, choices: Vec::new() }, "choices"),
            (Block::Clock { since_ms: 1_700_000_000_000, label: None }, "clock"),
            (
                Block::Task(TaskCard {
                    title: "Send the deck".into(),
                    detail: None,
                    due: Some("Fri".into()),
                    done: false,
                    check: None,
                }),
                "task",
            ),
            (Block::Note(NoteCard { title: "Notes".into(), excerpt: None, detail: None }), "note"),
        ];
        for (block, name) in blocks {
            let json = serde_json::to_value(&block).unwrap();
            assert_eq!(json["type"], name);
            assert_eq!(serde_json::from_value::<Block>(json).unwrap(), block, "{name}");
        }
        let clock = serde_json::to_value(Block::Clock { since_ms: 5, label: None }).unwrap();
        assert_eq!(clock, serde_json::json!({ "type": "clock", "sinceMs": 5 }));
        let activity = serde_json::to_value(Block::Activity {
            mode: Mode::Working,
            label: None,
            tool: Some(ToolUse {
                name: "search_mail".into(),
                label: "Searching your mail…".into(),
                icon: ToolIcon::Mail,
            }),
        })
        .unwrap();
        assert_eq!(activity["mode"], "working");
        assert_eq!(activity["tool"]["icon"], "mail");
    }

    #[test]
    fn a_conversation_keeps_who_said_what() {
        let block = Block::Conversation {
            turns: vec![
                Turn { who: Speaker::You, name: None, text: "Move it to 3".into(), pending: false },
                Turn {
                    who: Speaker::Assistant,
                    name: Some("Ada".into()),
                    text: "Done".into(),
                    pending: true,
                },
            ],
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["turns"][0]["who"], "you");
        assert!(json["turns"][0].get("name").is_none());
        assert_eq!(json["turns"][1]["who"], "assistant");
        assert_eq!(json["turns"][1]["pending"], true);
        assert_eq!(serde_json::from_value::<Block>(json).unwrap(), block);
    }

    #[test]
    fn an_expiry_is_written_in_the_names_the_page_reads() {
        let mut panel = card("mail:1");
        panel.expires = Some(Expiry {
            after_ms: 45_000,
            action: "expire".into(),
            remaining_ms: 12_000,
            held: true,
        });
        let json = serde_json::to_value(&panel).unwrap();
        assert_eq!(
            json["expires"],
            serde_json::json!({ "afterMs": 45000, "action": "expire", "remainingMs": 12000, "held": true })
        );
        let owned: Expiry =
            serde_json::from_value(serde_json::json!({ "afterMs": 30000, "action": "expire" }))
                .unwrap();
        assert_eq!((owned.remaining_ms, owned.held), (0, false), "the hub's to fill in");
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
    fn the_childs_stack_is_replaced_whole() {
        let mut stack = vec![card("a"), card("b")];
        assert!(!replace(&mut stack, vec![card("a"), card("b")]), "the same: nothing to draw");
        assert!(replace(&mut stack, vec![card("b"), card("c")]));
        assert_eq!(ids(&stack), ["b", "c"], "in the order the app sent");
        assert!(replace(&mut stack, Vec::new()));
        assert!(stack.is_empty());
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

    #[test]
    fn the_theme_is_kept_and_a_repeat_is_no_change() {
        let panels = Panels::default();
        assert_eq!(panels.theme(), None);
        assert!(panels.set_theme(Some("dusk".into())));
        assert!(!panels.set_theme(Some("dusk".into())), "nothing to send");
        assert_eq!(panels.theme().as_deref(), Some("dusk"));
        assert!(panels.set_theme(None));
        assert_eq!(panels.theme(), None);
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
        for round in 0..20 {
            let panels = Arc::new(Panels::default());
            let drawn: Arc<Mutex<Vec<Panel>>> = Arc::default();
            let tx = {
                let (panels, drawn) = (panels.clone(), drawn.clone());
                start(move |batch| {
                    if batch.stack {
                        *drawn.lock().unwrap() = panels.read(<[Panel]>::to_vec);
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
            let last = panels.read(<[Panel]>::to_vec);
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

    // ---- the clocks ----

    fn expiring(id: &str, after_ms: u64, action: &str) -> Panel {
        let mut panel = card(id);
        panel.expires =
            Some(Expiry { after_ms, action: action.into(), remaining_ms: 0, held: false });
        panel
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_clock_counts_from_when_its_expiry_first_appears() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![card("plain"), expiring("mail:1", 1000, "expire")];
        clocks.sync(&stack, t0);
        assert_eq!(clocks.next(), Some(t0 + ms(1000)));
        // The card changed in other ways since: the count goes on.
        stack[1].title = "Re: lunch".into();
        clocks.sync(&stack, t0 + ms(400));
        assert_eq!(clocks.next(), Some(t0 + ms(1000)));
        // An expiry given to a card already up counts from then.
        stack[0].expires = expiring("plain", 300, "expire").expires;
        clocks.sync(&stack, t0 + ms(500));
        assert_eq!(clocks.next(), Some(t0 + ms(800)));
    }

    #[test]
    fn a_changed_expiry_starts_the_count_again() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![expiring("notes:1", 1000, "expire")];
        clocks.sync(&stack, t0);
        stack[0].expires.as_mut().unwrap().after_ms = 2000;
        clocks.sync(&stack, t0 + ms(500));
        assert_eq!(clocks.next(), Some(t0 + ms(2500)), "a new time");
        stack[0].expires.as_mut().unwrap().action = "goAway".into();
        clocks.sync(&stack, t0 + ms(700));
        assert_eq!(clocks.next(), Some(t0 + ms(2700)), "a new action");
        // What the hub fills in is not the owner's to change: it does not
        // start the count again.
        stack[0].expires.as_mut().unwrap().remaining_ms = 5;
        clocks.sync(&stack, t0 + ms(900));
        assert_eq!(clocks.next(), Some(t0 + ms(2700)));
    }

    #[test]
    fn a_clock_goes_with_its_expiry_or_its_card() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![expiring("a", 1000, "expire"), expiring("b", 2000, "expire")];
        clocks.sync(&stack, t0);
        stack[0].expires = None;
        clocks.sync(&stack, t0);
        assert_eq!(clocks.next(), Some(t0 + ms(2000)), "a's expiry was taken away");
        assert!(take(&mut stack, "b"));
        clocks.sync(&stack, t0);
        assert_eq!(clocks.next(), None, "b was taken down");
        assert!(clocks.running.is_empty());
    }

    #[test]
    fn a_renderer_that_reloads_lets_go_of_every_hold() {
        let start = Instant::now();
        let mut clocks = Clocks::default();
        let mut card = card("mail:1");
        card.expires = Some(Expiry {
            after_ms: 10_000,
            action: "expire".into(),
            remaining_ms: 0,
            held: false,
        });
        clocks.sync(std::slice::from_ref(&card), start);
        assert!(clocks.hold("mail:1", true, start + Duration::from_secs(2)));
        assert_eq!(clocks.next(), None, "held: nothing counts down");
        assert!(clocks.release_all(start + Duration::from_secs(60)));
        assert_eq!(
            clocks.next(),
            Some(start + Duration::from_secs(68)),
            "counting again from where it stopped, with eight seconds left"
        );
        assert!(!clocks.release_all(start + Duration::from_secs(61)), "nothing left to let go");
    }

    #[test]
    fn holding_a_card_stops_its_clock_and_the_time_is_given_back() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let stack = vec![expiring("mail:1", 1000, "expire")];
        clocks.sync(&stack, t0);
        assert!(clocks.hold("mail:1", true, t0 + ms(300)));
        assert_eq!(clocks.next(), None, "nothing runs out while it is read");
        assert!(!clocks.hold("mail:1", true, t0 + ms(400)), "already held");
        assert!(clocks.hold("mail:1", false, t0 + ms(800)));
        // 700 were left when it was held, counted from when it was let go.
        assert_eq!(clocks.next(), Some(t0 + ms(1500)));
        assert!(!clocks.hold("mail:1", false, t0 + ms(900)), "already let go");
        assert_eq!(clocks.next(), Some(t0 + ms(1500)));
    }

    #[test]
    fn a_card_held_when_its_expiry_appears_starts_with_its_clock_stopped() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![card("ask:1")];
        clocks.sync(&stack, t0);
        assert!(!clocks.hold("ask:1", true, t0), "no clock to stop yet");
        stack[0].expires = expiring("ask:1", 500, "later").expires;
        clocks.sync(&stack, t0 + ms(100));
        assert_eq!(clocks.next(), None);
        assert!(clocks.hold("ask:1", false, t0 + ms(1000)));
        assert_eq!(clocks.next(), Some(t0 + ms(1500)));
    }

    #[test]
    fn a_card_taken_down_while_held_is_not_held_when_it_comes_back() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![expiring("mail:1", 1000, "expire")];
        clocks.sync(&stack, t0);
        clocks.hold("mail:1", true, t0);
        stack.clear();
        clocks.sync(&stack, t0);
        stack.push(expiring("mail:1", 1000, "expire"));
        clocks.sync(&stack, t0 + ms(10));
        assert_eq!(clocks.next(), Some(t0 + ms(1010)));
    }

    #[test]
    fn the_next_deadline_is_the_earliest_of_the_clocks_counting() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let stack = vec![
            expiring("a", 1000, "expire"),
            expiring("b", 500, "expire"),
            expiring("c", 3000, "expire"),
        ];
        clocks.sync(&stack, t0);
        assert_eq!(clocks.next(), Some(t0 + ms(500)));
        clocks.hold("b", true, t0 + ms(100));
        assert_eq!(clocks.next(), Some(t0 + ms(1000)), "b is being read");
        clocks.hold("a", true, t0 + ms(100));
        assert_eq!(clocks.next(), Some(t0 + ms(3000)));
    }

    #[test]
    fn a_clock_that_ran_out_tells_its_owner_once() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let stack = vec![expiring("b", 500, "later"), expiring("a", 500, "expire")];
        clocks.sync(&stack, t0);
        assert!(clocks.due(t0 + ms(499)).is_empty());
        assert_eq!(
            clocks.due(t0 + ms(500)),
            [("a".to_string(), "expire".to_string()), ("b".to_string(), "later".to_string())]
        );
        assert!(clocks.due(t0 + ms(5000)).is_empty(), "told once");
        assert_eq!(clocks.next(), None, "nothing left counting");
        assert!(clocks.ran_out("a") && clocks.ran_out("b"));
        // Synced again unchanged, it is still the clock that ran out.
        clocks.sync(&stack, t0 + ms(600));
        assert!(clocks.ran_out("a"));
        assert!(clocks.due(t0 + ms(5000)).is_empty());
    }

    #[test]
    fn an_owner_that_gives_the_card_more_time_keeps_it() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![expiring("notes:1", 500, "expire")];
        clocks.sync(&stack, t0);
        clocks.due(t0 + ms(500));
        stack[0].expires = expiring("notes:1", 4000, "close").expires;
        clocks.sync(&stack, t0 + ms(510));
        assert!(!clocks.ran_out("notes:1"), "not to be taken down");
        assert_eq!(clocks.next(), Some(t0 + ms(4510)));
        stack[0].expires = None;
        clocks.sync(&stack, t0 + ms(520));
        assert!(!clocks.ran_out("notes:1"), "kept up with no expiry at all");
    }

    #[test]
    fn a_held_card_is_never_due() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.sync(&[expiring("a", 100, "expire")], t0);
        clocks.hold("a", true, t0 + ms(50));
        assert!(clocks.due(t0 + ms(10_000)).is_empty());
    }

    #[test]
    fn the_renderers_are_told_the_time_left_and_the_owners_copy_is_untouched() {
        let t0 = Instant::now();
        let panels = Panels::default();
        panels.with(|stack| {
            put(stack, expiring("mail:1", 1000, "expire"));
            put(stack, card("plain"));
        });
        let drawn = panels.drawn(Instant::now() + ms(1));
        let expiry = drawn[0].expires.as_ref().unwrap();
        assert!(expiry.remaining_ms <= 1000 && expiry.remaining_ms > 0);
        assert!(!expiry.held);
        assert!(drawn[1].expires.is_none(), "a card that does not go has nothing filled in");
        // The owner's copy is as the owner put it, so putting it again is no
        // change and does not start the count again.
        assert_eq!(panels.read(|stack| stack[0].expires.as_ref().unwrap().remaining_ms), 0);
        let before = panels.next_deadline();
        assert!(!panels.with(|stack| put(stack, expiring("mail:1", 1000, "expire"))));
        assert_eq!(panels.next_deadline(), before);

        assert!(panels.hold("mail:1", true, t0 + ms(400)));
        assert!(!panels.hold("plain", true, t0 + ms(400)), "no clock: nothing to tell");
        assert!(!panels.hold("gone", true, t0 + ms(400)), "not up");
        let drawn = panels.drawn(t0 + ms(5000));
        let expiry = drawn[0].expires.as_ref().unwrap();
        assert!(expiry.held);
        // The clock started a moment after `t0` and stopped at 400 ms past it.
        assert!((600..=1000).contains(&expiry.remaining_ms), "{}", expiry.remaining_ms);
        let later = panels.drawn(t0 + ms(9000));
        assert_eq!(later[0].expires, drawn[0].expires, "no time passes while it is held");
    }

    #[test]
    fn the_clocks_follow_the_stack_however_it_is_changed() {
        let panels = Panels::default();
        panels.with(|stack| put(stack, expiring("a", 60_000, "expire")));
        assert!(panels.next_deadline().is_some());
        panels.with(|stack| take(stack, "a"));
        assert_eq!(panels.next_deadline(), None);
        panels.with(|stack| replace(stack, vec![expiring("b", 60_000, "expire")]));
        assert!(panels.next_deadline().is_some());
        assert!(panels.due(Instant::now()).is_empty());
    }

    #[test]
    fn a_card_waiting_under_more_does_not_count_down_until_it_is_drawn() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack: Vec<Panel> =
            (0..4).map(|n| expiring(&format!("mail:{n}"), 1000, "expire")).collect();
        clocks.sync(&stack, t0);
        let due = clocks.due(t0 + ms(1000));
        assert_eq!(due.len(), MAX_SHOWN, "the three drawn ran out: {due:?}");
        assert!(due.iter().all(|(id, _)| id != "mail:3"), "the fourth was never drawn");
        assert_eq!(clocks.next(), None, "nothing else is counting");
        assert!(clocks.due(t0 + ms(60_000)).is_empty(), "however long it waits");
        // The first goes, and the one that waited is drawn, with all its time.
        assert!(take(&mut stack, "mail:0"));
        clocks.sync(&stack, t0 + ms(61_000));
        assert_eq!(clocks.next(), Some(t0 + ms(62_000)));
        assert_eq!(clocks.due(t0 + ms(62_000)), [("mail:3".to_string(), "expire".to_string())]);
    }

    #[test]
    fn a_waiting_card_let_go_stays_stopped_and_shows_its_whole_time() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        let mut stack = vec![card("talk"), card("recording"), card("ask"), card("mail:1")];
        stack[3].expires = expiring("mail:1", 45_000, "expire").expires;
        clocks.sync(&stack, t0);
        // A stray hold, or every hold let go when a page reloads.
        clocks.hold("mail:1", true, t0 + ms(10));
        clocks.hold("mail:1", false, t0 + ms(20));
        assert!(!clocks.release_all(t0 + ms(30)));
        assert_eq!(clocks.next(), None);
        let mut drawn = stack[3].clone();
        clocks.fill(&mut drawn, t0 + ms(30_000));
        assert_eq!(drawn.expires.as_ref().map(|e| e.remaining_ms), Some(45_000));
        // Drawn, it counts; held there, it stops as any card does.
        assert!(take(&mut stack, "ask"));
        clocks.sync(&stack, t0 + ms(40_000));
        assert_eq!(clocks.next(), Some(t0 + ms(85_000)));
        assert!(clocks.hold("mail:1", true, t0 + ms(41_000)));
        assert_eq!(clocks.next(), None);
    }

    #[test]
    fn a_card_is_held_while_it_is_read_and_not_once_it_goes() {
        let panels = Panels::default();
        panels.with(|stack| put(stack, card("talk")));
        assert!(!panels.held("talk"));
        panels.hold("talk", true, Instant::now());
        assert!(panels.held("talk"), "held, with no clock to stop");
        assert!(!panels.held("mail:1"));
        panels.hold("talk", false, Instant::now());
        assert!(!panels.held("talk"));
        panels.hold("talk", true, Instant::now());
        panels.with(|stack| take(stack, "talk"));
        assert!(!panels.held("talk"), "a card taken down is held by nobody");
    }

    #[test]
    fn the_alarm_wakes_for_a_ring_or_a_deadline() {
        let alarm = Arc::new(Alarm::new());
        // A deadline that has passed: no wait at all.
        let seen = alarm.rung();
        alarm.wait(seen, Some(Instant::now()));
        // A ring before the wait began is not missed.
        alarm.ring();
        alarm.wait(seen, None);
        // A ring from elsewhere ends a wait with no deadline.
        let seen = alarm.rung();
        let ringer = {
            let alarm = alarm.clone();
            std::thread::spawn(move || {
                std::thread::sleep(ms(20));
                alarm.ring();
            })
        };
        let began = Instant::now();
        alarm.wait(seen, Some(began + Duration::from_secs(30)));
        assert!(began.elapsed() < Duration::from_secs(30), "woken by the ring");
        ringer.join().unwrap();
        // And a deadline ends one with no ring.
        let seen = alarm.rung();
        let began = Instant::now();
        alarm.wait(seen, Some(began + ms(30)));
        assert!(began.elapsed() >= ms(30));
    }
}
