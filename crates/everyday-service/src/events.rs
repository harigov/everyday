//! What the service says without being asked.
//!
//! Three kinds of thing, one trait. The shell turns them into window events;
//! the server turns them into server-sent events; a test collects them in a
//! `Vec`. Nothing that raises one knows which of those is listening, which is
//! the property that lets the same command body serve a window and a wire.
//!
//! # Notifications
//!
//! The shell does work nobody asked for and nobody is watching -- refreshing
//! a subscribed calendar on a timer, most of all -- and before this existed
//! the only thing it could do about a result was write a log line.
//!
//! It only *emits*. Whether a notification should become a banner in the
//! operating system or a toast inside the window depends on whether the
//! window is in front of the user, on whether the notification carries a
//! button, and on whether permission was ever granted -- and the interface is
//! the only side that knows the first two. Answering that in two places would
//! mean answering it differently within a release. So this states the facts
//! and `ui/src/lib/notify.svelte.ts` decides where they go.
//!
//! # Changes
//!
//! A write that landed, named by what it touched. One window's save is
//! another window's stale list, and this is what lets the second one find
//! out. Stamped with the caller that made it so a client is not told about
//! its own writes and does not flicker.
//!
//! # Lock state
//!
//! The vault locked or unlocked. Not a change to a record and not a
//! notification: every client has to leave the screen it is on.

use serde::{Deserialize, Serialize};

/// How loud a notification is. Mirrors `NotifyLevel` in the interface.
///
/// All four variants exist because this is one half of a wire contract, and a
/// mirror with holes in it is worse than no mirror.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

/// How far a notification has to travel to have done its job.
///
/// `App` is about something the user is looking at and never leaves the
/// window. `User` is for what must reach a person who may not have this app
/// in front of them, and is the only reach allowed out to the operating
/// system. Defaulting to `App` is what keeps a chatty background task from
/// becoming a chatty notification centre.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    App,
    User,
}

/// What the service hands a listener. Mirrors `ShellNotification` in
/// `ui/src/lib/types.ts`.
///
/// No action button, on purpose: the service can say a calendar stopped
/// answering, but "Open settings" is a thing only the interface can do, and a
/// payload that could carry a callback would be a payload that could carry
/// anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub level: Level,
    pub reach: Reach,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// Identity for a condition that can recur, so a fault that is still a
    /// fault replaces its own message rather than stacking a second copy.
    #[serde(default)]
    pub key: Option<String>,
}

impl Notification {
    pub fn new(level: Level, title: impl Into<String>) -> Self {
        Self { level, reach: Reach::App, title: title.into(), body: None, key: None }
    }

    pub fn warning(title: impl Into<String>) -> Self {
        Self::new(Level::Warning, title)
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Mark this as something that must reach the user even if they are not
    /// looking at the window.
    pub fn for_user(mut self) -> Self {
        self.reach = Reach::User;
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Created,
    Updated,
    Deleted,
}

/// What a write touched.
///
/// Coarser than a table on purpose. A listener uses this to decide which list
/// to reload, and the lists in the interface are per app, not per table -- so
/// "something in the task domain moved" is the useful granularity and "row
/// 4f3a of `time_blocks` was updated" is not.
///
/// Adding an app adds a variant here, and the interface's router gains one
/// arm. That is the whole cost of a new domain on this side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Journal,
    Entry,
    /// Writing that is not a day. See `everyday_core::note`.
    Note,
    Project,
    Task,
    Block,
    Calendar,
    Event,
    Shelf,
    Item,
    Log,
    Tracker,
    Reading,
    /// Who you are being. The axis every balance chart is drawn against.
    Role,
    /// An outcome under a role.
    Goal,
    Conversation,
    /// The assistant's standing work.
    Routine,
    /// One run of it. What the count on the app bar is drawn from.
    RoutineRun,
    Memory,
    /// The vault's own settings: auto-lock, the assistant's configuration.
    Settings,
}

/// One write, on its way to everyone who did not make it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub kind: Kind,
    pub op: Op,
    /// The record, when there is exactly one. Absent for a batch -- a board
    /// reorder writes forty tasks and the useful statement is "tasks moved".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Who made it. A client compares this with its own id and ignores a
    /// match, so a save does not make the window that saved it reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl Change {
    pub fn new(kind: Kind, op: Op) -> Self {
        Self { kind, op, id: None, origin: None }
    }
}

/// Where the service's own remarks go.
///
/// Implemented by the shell (window events), the server (server-sent events)
/// and tests (a vector). The default implementations do nothing, so a
/// listener that cares about one kind is not obliged to write three methods.
pub trait EventSink: Send + Sync {
    fn notify(&self, notification: Notification) {
        let _ = notification;
    }

    fn changed(&self, change: Change) {
        let _ = change;
    }

    fn lock_state(&self, locked: bool) {
        let _ = locked;
    }
}

/// A sink that drops everything. What a service runs with until one is set.
pub struct Silent;

impl EventSink for Silent {}
