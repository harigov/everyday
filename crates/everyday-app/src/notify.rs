//! Notifications raised by the shell.
//!
//! The shell does work nobody asked for and nobody is watching -- refreshing
//! a subscribed calendar on a timer, most of all -- and until this existed
//! the only thing it could do about a result was write a log line. A feed
//! that had stopped answering was visible in the calendar sidebar and
//! nowhere else, so somebody who spent the week in the journal was simply
//! not told.
//!
//! # Why this only emits
//!
//! There is a perfectly good `tauri-plugin-notification` API in Rust, and
//! this module deliberately does not call it. Whether a notification should
//! become a banner in the operating system or a toast inside the window
//! depends on whether the window is in front of the user, on whether the
//! notification carries a button, and on whether permission was ever
//! granted -- and the interface is the only side that knows the first two.
//! Answering that question in two places would mean answering it differently
//! within a release. So the shell states the *facts* -- what happened, how
//! serious it is, how far it needs to reach -- and `ui/src/lib/notify.svelte.ts`
//! decides where it goes. The plugin is registered so the webview can reach
//! it; this side never posts directly.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// The event the interface listens on. Its other half is `onShellNotification`
/// in `ui/src/lib/api.ts`.
pub const NOTIFY_EVENT: &str = "everyday://notify";

/// How loud a notification is. Mirrors `NotifyLevel` in the interface.
///
/// All four variants exist because this is one half of a wire contract, and
/// a mirror with holes in it is worse than no mirror: the next person to want
/// `Level::Error` from the shell should find it here rather than discover
/// that the two sides have drifted. Only `Warning` has a caller today.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
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
/// window. `User` is for the things that must reach a person who may not
/// have this app in front of them, and is the only reach allowed out to the
/// operating system. Defaulting to `App` is what keeps a chatty background
/// task from becoming a chatty notification centre.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    App,
    User,
}

/// What the shell hands the interface. Mirrors `ShellNotification` in
/// `ui/src/lib/types.ts`.
///
/// No action button, on purpose: the shell can say a calendar stopped
/// answering, but "Open settings" is a thing only the interface can do, and a
/// payload that could carry a callback across the bridge would be a payload
/// that could carry anything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub level: Level,
    pub reach: Reach,
    pub title: String,
    pub body: Option<String>,
    /// Identity for a condition that can recur, so a fault that is still a
    /// fault replaces its own message rather than stacking a second copy.
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

/// Send a notification to the interface.
///
/// Failing to emit is not worth failing whatever produced the notification:
/// the window may simply have gone during a background pass, and a calendar
/// sync that returned its report should not turn into an error because the
/// remark about it could not be delivered.
pub fn notify(app: &AppHandle, notification: Notification) {
    if let Err(e) = app.emit(NOTIFY_EVENT, &notification) {
        tracing::debug!(error = %e, "a notification could not be delivered to the interface");
    }
}
