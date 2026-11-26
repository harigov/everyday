//! Where the service's remarks go in a desktop window.
//!
//! Three events, one per thing the service has to say. The interface listens
//! for all three; see `ui/src/lib/api.ts`.
//!
//! # Why this only emits
//!
//! Whether a notification should become a banner in the operating system or a
//! toast inside the window depends on whether the window is in front of the
//! user, on whether the notification carries a button, and on whether
//! permission was ever granted -- and the interface is the only side that knows
//! the first two. Answering that question in two places would mean answering it
//! differently within a release. So the shell states the *facts* and
//! `ui/src/lib/notify.svelte.ts` decides where they go, handing what should
//! leave the window back to [`crate::notice::post_notification`]. This side
//! never decides for itself -- with the two exceptions below.
//!
//! # The fourth event
//!
//! A meeting offer is not a fact this window merely states, unlike the
//! three above, and it does not go to the interface alone. It is handed to
//! [`crate::offers`], which owns every offer to take notes: this one, from
//! the calendar, and the ones [`crate::micwatch`] raises when another
//! application takes the microphone. For an automatic ("Always") offer that
//! is also where the shell decides to start capturing, before the interface
//! ever hears about it. For any other it is where the banner's event, the
//! tray's item and a desktop notification all come from -- the
//! notification raised by the shell itself ([`crate::notice`]) rather than
//! routed through `notify.svelte.ts`, because it carries buttons, and
//! whether to raise one at all is decided there from whether the window is
//! already in front. The service therefore sends no `Notification` beside
//! an offer. See [`crate::offers::on_calendar_offer`].
//!
//! # Notifications the panel answers
//!
//! The other exception is a notification that carries what it is about,
//! structured (`Notification::card`): one new message, or a meeting's notes
//! with the action items found in them. Those are offered to the panel first
//! -- [`crate::mailcard`], [`crate::notescard`] -- which shows them as cards
//! that can be answered where they are: a reply typed and sent, an
//! invitation accepted, an action item taken. A notification the panel takes
//! is not emitted here, so it does not become a banner as well; one it does
//! not (the person routed that kind to the desktop's notifications in
//! Settings → Panel) goes to the interface as any other does. The mail card
//! hands its notification back to the window after all when the window
//! turns out to be in front, where a toast in it is the right answer.
//!
//! Both cards also give way to the desktop's Do Not Disturb
//! ([`desktop_quiet`]): a card floats over every window, a full-screen one
//! or a shared screen included, where the notification it stands in for
//! would have waited quietly in the desktop's list. So while the desktop is
//! holding notifications back, the notification goes on to the interface
//! as it would without the panel, and the desktop holds it. And a card that
//! goes by itself before the person did anything with it hands its
//! notification on then, so that what it announced is still in the list
//! when they come back to the computer.

use karmayoga_service::events::{Change, EventSink, MeetingOffer, Notification};
#[cfg(desktop)]
use karmayoga_service::events::{Kind, NoticeCard};
#[cfg(desktop)]
use tauri::Manager;
use tauri::{AppHandle, Emitter};

/// A notification the interface should route. Its other half is
/// `onShellNotification` in `ui/src/lib/api.ts`.
pub const NOTIFY_EVENT: &str = "karmayoga://notify";

/// A write that landed, so other windows can reload the list it was in.
pub const CHANGED_EVENT: &str = "karmayoga://changed";

/// The vault locked or unlocked.
pub const LOCK_EVENT: &str = "karmayoga://lock-state";

/// Whether the desktop is holding notifications back just now -- its Do Not
/// Disturb -- so that nothing should float over what the person is doing.
/// Asked by the panel's mail and notes cards before one goes up; see the
/// module docs' "notifications the panel answers".
///
/// On Linux this is GNOME's switch, read where `desk::linux::settings` reads
/// it: `show-banners` in `org.gnome.desktop.notifications`, off meaning Do Not
/// Disturb is on. Only under GNOME, and only where the key is installed --
/// GSettings ends the process when asked for a schema that is not, see
/// [`crate::gnome`] -- and no otherwise.
///
/// A Mac has no public way to ask whether a Focus is on, and Windows' own
/// question (`SHQueryUserNotificationState`) needs a binding this crate does
/// not build, so both answer no: there, the person who shares a screen
/// routes mail and notes to the desktop's notifications in Settings → Panel.
///
/// Blocking: GSettings waits on the settings daemon, so this is asked from a
/// thread that may wait, never from the service's event sink itself.
#[cfg(desktop)]
pub(crate) fn desktop_quiet() -> bool {
    #[cfg(target_os = "linux")]
    {
        use gtk::gio::prelude::SettingsExt;
        const SCHEMA: &str = "org.gnome.desktop.notifications";
        const KEY: &str = "show-banners";
        crate::gnome::running()
            && crate::gnome::settings(SCHEMA, &[KEY]).is_some_and(|s| !s.boolean(KEY))
    }
    // Windows: the shell's own answer to "may a notification be shown now",
    // the one its toasts follow -- no in a presentation, under Focus assist's
    // quiet hours, or with an app full screen. Asked through shell32 itself,
    // so the crate needs no Windows binding for one call.
    #[cfg(windows)]
    {
        windows_quiet()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        false
    }
}

/// Whether Windows says notifications should wait: `SHQueryUserNotificationState`
/// answering busy (an app full screen), a Direct3D game, presentation mode,
/// or quiet time. Any other answer, or a failed call, is not quiet.
#[cfg(windows)]
fn windows_quiet() -> bool {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHQueryUserNotificationState(state: *mut i32) -> i32;
    }
    // QUERY_USER_NOTIFICATION_STATE: QUNS_BUSY 2, QUNS_RUNNING_D3D_FULL_SCREEN
    // 3, QUNS_PRESENTATION_MODE 4, QUNS_QUIET_TIME 6.
    let mut state = 0i32;
    // SAFETY: one out-parameter, written by the call; an HRESULT back.
    let result = unsafe { SHQueryUserNotificationState(&mut state) };
    result >= 0 && matches!(state, 2 | 3 | 4 | 6)
}

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
        // A phone has no panel: what would be a card there is a notification.
        #[cfg(desktop)]
        if self.offer_to_panel(&notification) {
            return;
        }
        self.emit(NOTIFY_EVENT, notification);
    }

    fn changed(&self, change: Change) {
        #[cfg(desktop)]
        let kind = change.kind;
        self.emit(CHANGED_EVENT, change);
        // The rest is the tray and the icon's badge, neither of which a phone
        // has: a phone's badge is its notifications.
        #[cfg(desktop)]
        self.refresh_furniture(kind);
    }

    fn lock_state(&self, locked: bool) {
        self.emit(LOCK_EVENT, locked);
        #[cfg(desktop)]
        self.furniture_lock_state(locked);
    }

    // A phone is never offered to take notes: it records nothing. See `offers`.
    #[cfg(mobile)]
    fn meeting_offer(&self, _offer: MeetingOffer) {}

    #[cfg(mobile)]
    fn meeting_brief(&self, _brief: karmayoga_core::meeting::brief::MeetingBrief) {}

    #[cfg(desktop)]
    fn meeting_offer(&self, offer: MeetingOffer) {
        crate::offers::on_calendar_offer(&self.app, offer);
    }

    #[cfg(desktop)]
    fn meeting_brief(&self, brief: karmayoga_core::meeting::brief::MeetingBrief) {
        crate::offers::on_meeting_brief(&self.app, brief);
    }
}

#[cfg(desktop)]
impl WindowSink {
    /// Hand a notification that says what it is about to the panel's owner
    /// for that kind. True if it took it: the notification then goes no
    /// further. See the module docs' "notifications the panel answers".
    /// Never waits -- the owners decide what they can without a window and
    /// do the rest on threads of their own.
    fn offer_to_panel(&self, notification: &Notification) -> bool {
        match &notification.card {
            Some(NoticeCard::Mail(_)) => crate::mailcard::offer(&self.app, notification),
            Some(NoticeCard::MeetingNotes(_)) => crate::notescard::offer(&self.app, notification),
            _ => false,
        }
    }

    fn refresh_furniture(&self, kind: Kind) {
        // A calendar sync can change what the tray's optional meeting line
        // should say -- a new event landed, one moved, one was cancelled --
        // and nothing else would notice before the next minute's tick. See
        // `tray::Tray::refresh` and `tray::meeting_line`.
        if matches!(kind, Kind::Calendar | Kind::Event)
            && let Some(tray) = self.app.try_state::<crate::tray::Tray>()
        {
            tray.schedule_refresh(&self.app);
        }
        // Anything the icon's badge can count: a task done or added, a
        // project (and its tasks) deleted, a thread read or archived, an
        // account's mail switched off, a run or a proposal arriving or being
        // looked at. See `badge.rs`.
        if matches!(
            kind,
            Kind::Task
                | Kind::Project
                | Kind::Thread
                | Kind::Mailbox
                | Kind::Account
                | Kind::RoutineRun
                | Kind::Proposal
        ) && let Some(badge) = self.app.try_state::<crate::badge::Badge>()
        {
            badge.schedule_refresh(&self.app);
        }
    }

    fn furniture_lock_state(&self, locked: bool) {
        // A conversation from the talk key ends with the lock, and its models
        // are held ready only while the vault is open. See `talk`.
        crate::talk::on_lock_state(&self.app, locked);
        // Locking must take a decrypted meeting title off the tray at once,
        // not on the next tick -- `tray::meeting_line` already answers
        // `None` once `vault.is_unlocked()` is false, so this call is what
        // actually acts on that rather than leaving the old one up to stale
        // for up to a minute. Unlocking is refreshed here too, for the
        // symmetric reason: a vault that just became readable should not
        // wait a minute to say what it is, either.
        if let Some(tray) = self.app.try_state::<crate::tray::Tray>() {
            tray.schedule_refresh(&self.app);
        }
        // The badge, for the same pair of reasons: a locked vault has no
        // count to show (`badge::count` answers 0), and an unlocked one
        // should not wait a minute to show its own.
        if let Some(badge) = self.app.try_state::<crate::badge::Badge>() {
            badge.schedule_refresh(&self.app);
        }
        // Offers to take notes name meetings, so they go with the key too:
        // pending and held-back ones, the tray's item, the notifications.
        // So do the panel's cards that show what the vault holds: a message
        // and its first words, a meeting's notes and its action items, the
        // assistant's questions (which go unanswered).
        if locked {
            crate::offers::on_locked(&self.app);
            crate::mailcard::on_locked(&self.app);
            crate::notescard::on_locked(&self.app);
            crate::askcard::on_locked(&self.app);
        }
    }
}
