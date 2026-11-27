//! What is in front of the person right now: which application has focus,
//! how long since they last touched the keyboard or mouse, and whether the
//! screen is locked.
//!
//! One [`Watcher`] per platform, picked by [`detect`]:
//!
//! | backend   | focus                                         | idle                                   | locked                         |
//! |-----------|-----------------------------------------------|----------------------------------------|--------------------------------|
//! | Windows   | `GetForegroundWindow` → the process's exe     | `GetLastInputInfo`                     | the input desktop is not ours  |
//! | macOS     | `CGWindowListCopyWindowInfo`, frontmost layer-0 window's owner; bundle id from `NSRunningApplication` | `CGEventSourceSecondsSinceLastEventType` | `CGSessionCopyCurrentDictionary` |
//! | X11       | `_NET_ACTIVE_WINDOW`, `WM_CLASS`, `_NET_WM_NAME` | the screensaver extension            | logind `LockedHint`            |
//! | wlroots   | `zwlr_foreign_toplevel_manager_v1`, the `activated` toplevel | `ext_idle_notifier_v1`  | logind `LockedHint`            |
//! | KDE       | a KWin script, loaded over D-Bus, calling back on `windowActivated` | `ext_idle_notifier_v1` on Wayland, `org.freedesktop.ScreenSaver` on X11 | `org.freedesktop.ScreenSaver.GetActive` |
//! | GNOME     | none -- [`Error::NeedsExtension`]: Mutter tells nothing outside the Shell; the Shell extension in `extensions/gnome` reports instead | Mutter's `IdleMonitor` | `org.gnome.ScreenSaver.GetActive`, else logind `LockedHint` |
//!
//! Every call is blocking and cheap enough to make every few seconds, and
//! none panics. D-Bus calls give up after two seconds. A backend that needs
//! an event loop (wlroots, KDE) keeps it on a thread of its own -- its own
//! Wayland queue, or zbus's executor -- and answers from what it last heard.

#[cfg(target_os = "linux")]
mod gnome;
#[cfg(target_os = "linux")]
mod kde;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
mod wlroots;
#[cfg(target_os = "linux")]
mod x11;

use std::time::Duration;

/// The window that has focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Focused {
    /// The application's id as the platform names it: a desktop file id
    /// (`org.gnome.Ptyxis`), an X11 class (`firefox`), an executable's file
    /// name (`Code.exe`), a bundle id (`com.apple.Safari`). Never empty.
    pub app_id: String,
    /// A friendlier name when the platform has one (`Visual Studio Code`,
    /// `Safari`), else empty.
    pub name: String,
    /// The window's title. Empty where the platform will not say without a
    /// permission nobody has granted (macOS without Screen Recording).
    pub title: String,
}

/// How focus is watched here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Windows,
    MacOs,
    X11,
    Wlroots,
    Kde,
    /// GNOME on Wayland: the Shell extension reports, not this crate.
    Gnome,
    /// Nothing here knows how to watch this desktop.
    Unsupported,
}

impl Backend {
    /// The word the shell and the interface use: `windows`, `macos`, `x11`,
    /// `wlroots`, `kde`, `gnome`, `unsupported`. Also the heartbeat
    /// collector name this machine's own heartbeats are sent under.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Windows => "windows",
            Backend::MacOs => "macos",
            Backend::X11 => "x11",
            Backend::Wlroots => "wlroots",
            Backend::Kde => "kde",
            Backend::Gnome => "gnome",
            Backend::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// GNOME on Wayland: only code inside the Shell can see focus.
    #[error(
        "GNOME tells nothing outside the Shell which window has focus; the Shell extension reports it instead"
    )]
    NeedsExtension,
    /// The desktop does not offer what this backend needs (no foreign
    /// toplevel protocol, no X server, no session bus).
    #[error("{0}")]
    Unavailable(String),
    /// It was there and the call failed.
    #[error("{0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One platform's way of answering.
pub trait Watcher: Send {
    fn backend(&self) -> Backend;
    /// The focused window, or `None` when nothing has focus (the desktop
    /// itself, a lock screen, a window switcher).
    fn focused(&mut self) -> Result<Option<Focused>>;
    /// Time since the last keyboard or mouse input.
    fn idle(&mut self) -> Result<Duration>;
    /// Is the screen locked?
    fn locked(&mut self) -> Result<bool>;
    /// What this watcher cannot see, when it is a partial answer -- for
    /// the shell to show once. Today only an X11 watcher on XWayland, the
    /// last resort on a Wayland compositor that offers nothing better,
    /// which sees X11 programs' windows and not native Wayland ones.
    fn limitation(&self) -> Option<&str> {
        None
    }
}

/// The watcher for this desktop.
///
/// On Linux, by `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, `WAYLAND_DISPLAY`
/// and `DISPLAY`: GNOME on Wayland is [`Backend::Gnome`]; KDE Plasma is
/// [`Backend::Kde`]; any other Wayland compositor that offers the wlroots
/// foreign-toplevel protocol is [`Backend::Wlroots`]; an X11 session (or
/// XWayland, as a last resort) is [`Backend::X11`]. Never fails: a desktop
/// nothing here understands gets a watcher whose `backend` is
/// [`Backend::Unsupported`] and whose calls answer [`Error::Unavailable`].
///
/// Building a watcher may take a moment (a Wayland roundtrip, loading the
/// KWin script, at most a few seconds when a compositor does not answer),
/// so call this on the thread that will use the watcher.
pub fn detect() -> Box<dyn Watcher> {
    #[cfg(target_os = "linux")]
    let watcher = linux::detect();
    #[cfg(target_os = "windows")]
    let watcher: Box<dyn Watcher> = Box::new(windows::Windows::new());
    #[cfg(target_os = "macos")]
    let watcher: Box<dyn Watcher> = Box::new(macos::MacOs::new());
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    let watcher: Box<dyn Watcher> = Box::new(Unsupported(String::from(
        "watching focus is not supported on this operating system",
    )));
    watcher
}

/// A watcher that can answer nothing, and says why.
pub struct Unsupported(pub String);

impl Watcher for Unsupported {
    fn backend(&self) -> Backend {
        Backend::Unsupported
    }
    fn focused(&mut self) -> Result<Option<Focused>> {
        Err(Error::Unavailable(self.0.clone()))
    }
    fn idle(&mut self) -> Result<Duration> {
        Err(Error::Unavailable(self.0.clone()))
    }
    fn locked(&mut self) -> Result<bool> {
        Err(Error::Unavailable(self.0.clone()))
    }
}
