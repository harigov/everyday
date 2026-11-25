//! The computer somebody is talking to the assistant at, as the assistant
//! reaches it.
//!
//! "Search Amazon for headphones", "pause the music", "turn it down", "set a
//! timer for ten minutes", "open Files": things done to this computer's
//! desktop rather than to the vault. The desktop is the shell's -- only the
//! app has a browser to open, a session bus to find the music player on, a
//! notification to ring a timer with -- so it hands the service a [`Desk`]
//! when it starts ([`Service::set_desk`](crate::service::Service::set_desk)),
//! as the talk key hands a turn a [`Typist`](crate::agent::Typist). What each
//! action *means* -- which site, which application, when a timer rings -- is
//! decided in [`karmayoga_core::agent::desk`], offline; what the model is
//! offered and told is [`tools`]; the timers themselves are [`timers`].
//!
//! # Only at this computer
//!
//! A turn is handed the desk in its [`Reach`](crate::agent::Reach) only when
//! the person is at this computer: typed in the window (a [`Caller::Local`]
//! or [`Caller::Window`] turn), voice mode in it, the talk key or the wake
//! word. Never from a chat or a call through one -- "pause the music" said
//! to a phone on a train has nobody at the desk to hear it -- never from the
//! web over the tailnet, which is somebody at another computer, and never in
//! a routine, a job, a dream or a brief. And only with
//! [`AgentSettings::desktop`](karmayoga_core::AgentSettings::desktop) on.
//!
//! [`Caller::Local`]: crate::ctx::Caller::Local
//! [`Caller::Window`]: crate::ctx::Caller::Window
//!
//! # What asks first
//!
//! Almost nothing, because almost nothing here reaches anybody or loses
//! anything: a song paused is a song to press play on. The exceptions are
//! the two that send words to a stranger's server from the person's own
//! browser -- signed in, with their cookies -- which are held to the same
//! rules as the assistant reading a page itself: an address the model put
//! together after a stranger's words came into the turn is shown first. And
//! text put on the clipboard after a stranger's words is shown before it is,
//! as text typed for them is. See `ConfirmGate::open_question`,
//! `ConfirmGate::search_question` and the `copy` question in
//! [`crate::agent`].

#[cfg(test)]
pub(crate) mod testing;
pub mod timers;
pub mod tools;

use futures::future::BoxFuture;
use jiff::Timestamp;
use karmayoga_core::FileId;
use serde::{Deserialize, Serialize};

pub use karmayoga_core::agent::desk::InstalledApp;

/// What the shell can do to this computer's desktop. Every method answers
/// with an error in words fit to tell the person, as [`crate::agent::Typist`]
/// does: the model reads it and says it.
///
/// [`Desk::abilities`] is asked as a turn is built, and a tool for anything
/// it says this desktop cannot do is not offered -- a model never told
/// about the volume cannot promise to turn it down.
pub trait Desk: Send + Sync + 'static {
    /// What this desktop can do. Cheap: asked as every turn is built.
    fn abilities(&self) -> Abilities;

    /// Open `url` in the person's own browser. Only `http` and `https`
    /// reach here; the tool checks.
    fn open_url(&self, url: String) -> BoxFuture<'static, Result<(), String>>;

    /// Open the file `id`, from the Files app, in whatever application this
    /// computer opens it with -- as Files' own "Open" does, and refused as
    /// that is for anything that would run rather than be shown.
    fn open_file(&self, id: FileId) -> BoxFuture<'static, Result<(), String>>;

    /// Bring Karma Yoga's main window forward on one of its own apps:
    /// `section` is one of [`SECTIONS`].
    fn show_section(&self, section: String) -> BoxFuture<'static, Result<(), String>>;

    /// The applications installed here that a person could launch, as the
    /// desktop's own launcher lists them.
    fn apps(&self) -> BoxFuture<'static, Result<Vec<InstalledApp>, String>>;

    /// Launch the application `id`, one of [`Desk::apps`]'.
    fn launch(&self, id: String) -> BoxFuture<'static, Result<(), String>>;

    /// Do `action` to whatever is playing music or a video, and say what that
    /// is afterwards. `None` when nothing that plays anything is running.
    fn media(&self, action: MediaAction) -> BoxFuture<'static, Result<Option<NowPlaying>, String>>;

    /// Change the volume of what this computer plays through, and say where
    /// it is afterwards.
    fn volume(&self, change: VolumeChange) -> BoxFuture<'static, Result<Volume, String>>;

    /// Switch `setting` on or off (`Some`), or only read it (`None`), and say
    /// whether it is on afterwards.
    fn setting(
        &self,
        setting: DeskSetting,
        on: Option<bool>,
    ) -> BoxFuture<'static, Result<bool, String>>;

    /// Lock the screen.
    fn lock_screen(&self) -> BoxFuture<'static, Result<(), String>>;

    /// Put `text` on the clipboard, to be pasted wherever the person likes.
    fn copy(&self, text: String) -> BoxFuture<'static, Result<(), String>>;

    /// A timer has run out: tell the person, so they notice -- a
    /// notification that stays until it is dismissed, with a sound. Returns at
    /// once; nothing is waiting on it.
    fn ring(&self, timer: Timer);
}

/// What a [`Desk`] can do here. Opening a link and ringing a timer every
/// desk can; the rest depend on the desktop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Abilities {
    /// [`Desk::apps`] and [`Desk::launch`].
    pub apps: bool,
    /// [`Desk::open_file`].
    pub files: bool,
    /// [`Desk::show_section`].
    pub sections: bool,
    /// [`Desk::media`].
    pub media: bool,
    /// [`Desk::volume`].
    pub volume: bool,
    /// Which of [`DeskSetting`] [`Desk::setting`] can switch here.
    pub settings: Vec<DeskSetting>,
    /// [`Desk::lock_screen`].
    pub lock: bool,
    /// [`Desk::copy`].
    pub copy: bool,
}

/// Karma Yoga's own apps, as [`Desk::show_section`] names them -- the
/// sections the main window switches between.
pub const SECTIONS: &[&str] = &[
    "overview",
    "journal",
    "notes",
    "todo",
    "calendar",
    "library",
    "mail",
    "feed",
    "money",
    "screentime",
    "files",
    "health",
    "assistant",
];

/// What to do to what is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaAction {
    Play,
    Pause,
    /// Play if paused, pause if playing.
    Toggle,
    Next,
    Previous,
    /// Change nothing; only say what is playing.
    Status,
}

/// What a player is playing, after a [`MediaAction`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    /// The application playing it: "Spotify", "Firefox", "Karma Yoga".
    pub player: String,
    /// Playing now, as opposed to paused or stopped.
    pub playing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
}

/// How to change the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "percent")]
pub enum VolumeChange {
    /// Up by this many points, or the desk's own step when `None`.
    Up(Option<u8>),
    /// Down by this many points, or the desk's own step when `None`.
    Down(Option<u8>),
    /// To this percentage, 0 to 100.
    Set(u8),
    Mute,
    Unmute,
    /// Change nothing; only say where it is.
    Status,
}

/// Where the volume is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    /// 0 to 100 -- or past 100 on a desktop that allows it.
    pub percent: u32,
    pub muted: bool,
}

/// A switch of the desktop's own the assistant may flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeskSetting {
    /// Notifications held back rather than shown.
    DoNotDisturb,
    /// The desktop's dark style.
    DarkMode,
    /// Warmer colours on the screen.
    NightLight,
}

impl DeskSetting {
    pub const ALL: [DeskSetting; 3] =
        [DeskSetting::DoNotDisturb, DeskSetting::DarkMode, DeskSetting::NightLight];

    /// How a person would say it: "Do Not Disturb".
    pub fn label(self) -> &'static str {
        match self {
            DeskSetting::DoNotDisturb => "Do Not Disturb",
            DeskSetting::DarkMode => "dark mode",
            DeskSetting::NightLight => "Night Light",
        }
    }

    /// The name the tool's schema uses.
    pub fn key(self) -> &'static str {
        match self {
            DeskSetting::DoNotDisturb => "do_not_disturb",
            DeskSetting::DarkMode => "dark_mode",
            DeskSetting::NightLight => "night_light",
        }
    }
}

/// A timer the assistant set. Kept in memory only, by [`timers::Timers`]: a
/// timer is for the pasta, and does not outlive the app being closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timer {
    /// Small and sayable -- "cancel timer 2" -- and not reused while the app
    /// runs.
    pub id: u32,
    /// What it is for, in the person's words: "pasta". `None` when they did
    /// not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// When it was set.
    pub set: Timestamp,
    /// When it rings.
    pub ends: Timestamp,
    /// The person's own time zone, by IANA name, as the timer was set in it:
    /// what its notification tells the time in, so that it agrees with what
    /// `list_timers` said rather than with the machine's clock.
    pub zone: String,
}

/// A length of time as a person says it, to the unit that matters: "10
/// minutes", "1 hour 30 minutes", "45 seconds", "1 minute 30 seconds". Never
/// more than two units -- the seconds of a timer set for an hour are not what
/// anybody remembers setting. `None` for no time at all.
///
/// One wording for what the model is told a timer has left and what its
/// notification says it was set for, so the two never describe the same
/// timer differently.
pub fn spoken_length(seconds: i64) -> Option<String> {
    if seconds <= 0 {
        return None;
    }
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let unit = |n: i64, name: &str| format!("{n} {name}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(unit(hours, "hour"));
        if minutes > 0 {
            parts.push(unit(minutes, "minute"));
        }
    } else if minutes > 0 {
        parts.push(unit(minutes, "minute"));
        if seconds > 0 {
            parts.push(unit(seconds, "second"));
        }
    } else {
        parts.push(unit(seconds, "second"));
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_length_is_said_to_the_unit_that_matters() {
        assert_eq!(spoken_length(0), None);
        assert_eq!(spoken_length(-5), None);
        assert_eq!(spoken_length(1).as_deref(), Some("1 second"));
        assert_eq!(spoken_length(45).as_deref(), Some("45 seconds"));
        assert_eq!(spoken_length(60).as_deref(), Some("1 minute"));
        assert_eq!(spoken_length(570).as_deref(), Some("9 minutes 30 seconds"));
        assert_eq!(spoken_length(600).as_deref(), Some("10 minutes"));
        assert_eq!(spoken_length(3600).as_deref(), Some("1 hour"));
        assert_eq!(spoken_length(5400).as_deref(), Some("1 hour 30 minutes"));
        assert_eq!(spoken_length(2 * 3600 + 5 * 60 + 9).as_deref(), Some("2 hours 5 minutes"));
        assert_eq!(spoken_length(3600 + 59).as_deref(), Some("1 hour"));
    }
}
