//! Where the service's remarks go in a desktop window.
//!
//! Three events, one per thing the service has to say. The interface listens
//! for all three; see `ui/src/lib/api.ts`.
//!
//! # Why this only emits
//!
//! There is a perfectly good `tauri-plugin-notification` API in Rust, and this
//! module deliberately does not call it. Whether a notification should become a
//! banner in the operating system or a toast inside the window depends on
//! whether the window is in front of the user, on whether the notification
//! carries a button, and on whether permission was ever granted -- and the
//! interface is the only side that knows the first two. Answering that question
//! in two places would mean answering it differently within a release. So the
//! shell states the *facts* and `ui/src/lib/notify.svelte.ts` decides where
//! they go. The plugin is registered so the webview can reach it; this side
//! never posts directly.

use everyday_service::events::{Change, EventSink, Notification};
use tauri::{AppHandle, Emitter};

/// A notification the interface should route. Its other half is
/// `onShellNotification` in `ui/src/lib/api.ts`.
pub const NOTIFY_EVENT: &str = "everyday://notify";

/// A write that landed, so other windows can reload the list it was in.
pub const CHANGED_EVENT: &str = "everyday://changed";

/// The vault locked or unlocked.
pub const LOCK_EVENT: &str = "everyday://lock-state";

pub struct WindowSink {
    app: AppHandle,
}

impl WindowSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    /// Failing to emit is not worth failing whatever produced the event: the
    /// window may simply have gone during a background pass, and a calendar
    /// sync that returned its report should not turn into an error because the
    /// remark about it could not be delivered.
    fn emit<T: serde::Serialize + Clone>(&self, name: &str, payload: T) {
        if let Err(e) = self.app.emit(name, payload) {
            tracing::debug!(error = %e, event = name, "an event could not reach the interface");
        }
    }
}

impl EventSink for WindowSink {
    fn notify(&self, notification: Notification) {
        self.emit(NOTIFY_EVENT, notification);
    }

    fn changed(&self, change: Change) {
        self.emit(CHANGED_EVENT, change);
    }

    fn lock_state(&self, locked: bool) {
        self.emit(LOCK_EVENT, locked);
    }
}
