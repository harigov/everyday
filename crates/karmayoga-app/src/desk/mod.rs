//! The computer the assistant is talking to somebody at, as the app reaches it.
//!
//! "Search Amazon for headphones", "pause the music", "turn it down", "Do Not
//! Disturb on", "lock the screen", "copy that", "set a timer for ten minutes",
//! "open Files": the assistant acting on the desktop rather than on the
//! vault. What each of them *means* -- which site, which application, when a
//! timer rings -- is decided offline in `karmayoga_core::agent::desk`, and
//! what the model is offered is `karmayoga_service::desk`'s. The service
//! cannot open a browser or find the music player on a session bus, though; it
//! has no desktop. This is the other half of the contract it is written to,
//! the [`Desk`] the app hands it as it starts ([`install`]).
//! `docs/plans/desk.md` has the whole design.
//!
//! # What is the same everywhere and what is not
//!
//! Opening a link, a file, one of Karma Yoga's own apps, copying text and
//! ringing a timer are the shell's own, and work on every platform: they go
//! through the opener plugin, the files catalog, the main window, the clipboard
//! and the notification module, all of which already do. The rest -- other
//! applications, the music, the volume, Do Not Disturb, the lock -- is the
//! desktop's, and is `linux`'s (GNOME first) and `macos`'s. Elsewhere the desk
//! says so honestly: [`Abilities`] names only what is there, the assistant is
//! told about nothing else, and a method that is called anyway answers that it
//! "isn't available on this computer yet". Windows has its own ways to do
//! every one of these and is the obvious next module.
//!
//! # Who calls these
//!
//! Only a turn taken *at this computer*, by the rule `karmayoga_service::desk`
//! documents, and only with the local session's service: [`install`] is given
//! that one and no other, so a window looking at a vault on another computer
//! has a service with no desk in it, and so does a server. Every method
//! answers in words to be said to the person, as [`karmayoga_service::agent::Typist`]
//! does -- the model reads the error and tells them -- and logs the detail
//! for whoever is looking at why.

#[cfg(any(target_os = "linux", test))]
mod environment;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod words;

use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use karmayoga_core::FileId;
use karmayoga_service::Service;
use karmayoga_service::desk::{
    Abilities, Desk, DeskSetting, InstalledApp, MediaAction, NowPlaying, Timer, Volume,
    VolumeChange,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

/// Hand the local service this computer's desk. Once, as the app starts, from
/// `setup`: [`AppDesk::new`] looks at what the desktop has, which is cheap and
/// done once so that every turn's [`Desk::abilities`] is only a copy.
pub fn install(app: &AppHandle, service: &Service) {
    service.set_desk(Arc::new(AppDesk::new(app.clone())));
}

/// The desk of the computer this app is running on.
pub struct AppDesk {
    app: AppHandle,
    /// What was found at start. A tool installed later is not seen until the
    /// app is restarted, which is the price of a turn never waiting to find
    /// out.
    abilities: Abilities,
}

impl AppDesk {
    pub fn new(app: AppHandle) -> Self {
        let abilities = abilities();
        tracing::debug!(?abilities, "what the assistant can do at this desk");
        Self { app, abilities }
    }
}

/// What this desktop can do.
fn abilities() -> Abilities {
    #[cfg(target_os = "linux")]
    {
        linux::abilities()
    }
    #[cfg(target_os = "macos")]
    {
        macos::abilities()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Abilities { files: true, sections: true, copy: true, ..Abilities::default() }
    }
}

impl Desk for AppDesk {
    fn abilities(&self) -> Abilities {
        self.abilities.clone()
    }

    fn open_url(&self, url: String) -> BoxFuture<'static, Result<(), String>> {
        let app = self.app.clone();
        async move {
            let url = url.trim().to_string();
            if !is_web_address(&url) {
                return Err("Only web addresses can be opened.".to_string());
            }
            // A process to start, and on Linux a call to the desktop over
            // D-Bus made with zbus's own blocking runtime, which panics when
            // it is started from inside an async task: a thread of its own,
            // as `files::open_file_externally` runs the same call.
            off_thread(move || {
                app.opener().open_url(&url, None::<&str>).map_err(|e| {
                    tracing::warn!(error = %e, "a link could not be opened");
                    "The browser wouldn't open that link.".to_string()
                })
            })
            .await
        }
        .boxed()
    }

    fn open_file(&self, id: FileId) -> BoxFuture<'static, Result<(), String>> {
        let app = self.app.clone();
        async move {
            // This computer's own vault, which is the one a turn at it is
            // about -- see the module doc.
            let service = app.state::<crate::state::AppState>().service();
            crate::files::open_in_program(app, service, id).await.map_err(|e| e.message)
        }
        .boxed()
    }

    fn show_section(&self, section: String) -> BoxFuture<'static, Result<(), String>> {
        let app = self.app.clone();
        async move { crate::windows::show_section(&app, &section).map_err(|e| e.message) }.boxed()
    }

    fn apps(&self) -> BoxFuture<'static, Result<Vec<InstalledApp>, String>> {
        #[cfg(target_os = "linux")]
        {
            async { off_thread(|| Ok(linux::apps::installed())).await }.boxed()
        }
        #[cfg(target_os = "macos")]
        {
            async { off_thread(|| Ok(macos::apps::installed())).await }.boxed()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            unavailable("Listing applications")
        }
    }

    fn launch(&self, id: String) -> BoxFuture<'static, Result<(), String>> {
        #[cfg(target_os = "linux")]
        {
            let app = self.app.clone();
            async move { linux::apps::launch(&app, id).await }.boxed()
        }
        #[cfg(target_os = "macos")]
        {
            async move { off_thread(move || macos::apps::launch(&id)).await }.boxed()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = id;
            unavailable("Opening applications")
        }
    }

    fn media(&self, action: MediaAction) -> BoxFuture<'static, Result<Option<NowPlaying>, String>> {
        #[cfg(target_os = "linux")]
        {
            linux::media::control(action).boxed()
        }
        #[cfg(target_os = "macos")]
        {
            let app = self.app.clone();
            async move { macos::media::control(&app, action).await }.boxed()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = action;
            unavailable("Controlling the music")
        }
    }

    fn volume(&self, change: VolumeChange) -> BoxFuture<'static, Result<Volume, String>> {
        #[cfg(target_os = "linux")]
        {
            async move { off_thread(move || linux::volume::change(change)).await }.boxed()
        }
        #[cfg(target_os = "macos")]
        {
            async move { off_thread(move || macos::volume::change(change)).await }.boxed()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = change;
            unavailable("Changing the volume")
        }
    }

    fn setting(
        &self,
        setting: DeskSetting,
        on: Option<bool>,
    ) -> BoxFuture<'static, Result<bool, String>> {
        #[cfg(target_os = "linux")]
        {
            async move { off_thread(move || linux::settings::apply(setting, on)).await }.boxed()
        }
        #[cfg(target_os = "macos")]
        {
            async move { off_thread(move || macos::settings::apply(setting, on)).await }.boxed()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (setting, on);
            unavailable("Changing desktop settings")
        }
    }

    fn lock_screen(&self) -> BoxFuture<'static, Result<(), String>> {
        #[cfg(target_os = "linux")]
        {
            linux::lock::lock().boxed()
        }
        #[cfg(target_os = "macos")]
        {
            macos::lock::lock().boxed()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            unavailable("Locking the screen")
        }
    }

    fn copy(&self, text: String) -> BoxFuture<'static, Result<(), String>> {
        let app = self.app.clone();
        async move {
            // Wayland has no clipboard a program may simply write to: this
            // goes through the portal, which may refuse, and says so in words
            // for the person -- see `paste::copy`.
            crate::voicetyping::paste::copy(&app, &text).await.inspect_err(|e| {
                tracing::warn!(error = %e, "text could not be copied for the assistant");
            })
        }
        .boxed()
    }

    fn ring(&self, timer: Timer) {
        let app = self.app.clone();
        // The notification is queued on its own worker, which is quick; on
        // macOS and Windows it is the plugin's, which is a call to the
        // system. Neither is for the timer's task, which has a clock to keep.
        tauri::async_runtime::spawn_blocking(move || {
            let (title, body) = ring_words(&timer);
            crate::notice::alarm(&app, &format!("timer:{}", timer.id), &title, &body);
        });
    }
}

/// What a timer's notification says, from the timer and the person's clock
/// -- their own zone, as the timer was set in it, so the notification agrees
/// with what `list_timers` said; the machine's when that zone is unknown.
fn ring_words(timer: &Timer) -> (String, String) {
    let set_at = timer
        .set
        .in_tz(&timer.zone)
        .unwrap_or_else(|_| timer.set.to_zoned(jiff::tz::TimeZone::system()))
        .time();
    let seconds = timer.ends.as_second() - timer.set.as_second();
    words::timer_done(timer.label.as_deref(), seconds, set_at)
}

/// Run `work` on a thread that may block, and say what it answered.
///
/// For the calls here that wait on something outside this process -- a tool
/// that has to start, the settings daemon, the desktop's own opener -- and so
/// are not for a thread of the async runtime. A worker that dies is a plain
/// refusal, not a panic carried back.
pub(crate) async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = %e, "a desk call did not finish");
            Err("That didn't finish.".to_string())
        }
    }
}

/// The answer of a call this platform cannot make, in words for the person.
///
/// Not what is offered -- [`Abilities`] says that, and an assistant is never
/// offered what it cannot do -- but what a call that arrives regardless is
/// answered with, rather than a panic or a silence.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unavailable<T: Send + 'static>(what: &str) -> BoxFuture<'static, Result<T, String>> {
    let message = format!("{what} isn't available on this computer yet.");
    async move { Err(message) }.boxed()
}

/// Is `url` something the browser should be given: a web address, and
/// nothing a desktop would take for a command? `open_url` is handed what the
/// model wrote, so this is checked here and not only where it is said to be.
///
/// Only `http` and `https`, as a scheme and not as the start of something
/// else, and no control characters -- a newline in an address is how one
/// argument becomes two to a program that is handed it.
fn is_web_address(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|rest| !rest.is_empty()) && !url.chars().any(|c| c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    #[test]
    fn only_web_addresses_are_opened() {
        assert!(is_web_address("https://www.amazon.com/s?k=headphones"));
        assert!(is_web_address("http://localhost:8080/"));
        assert!(is_web_address("HTTPS://EXAMPLE.COM"));
        for refused in [
            "",
            "https://",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "ftp://example.com",
            "mailto:a@b.c",
            "example.com",
            "xhttps://example.com",
            " https://example.com",
            "https://example.com\n--help",
            "https://example.com\0",
        ] {
            assert!(!is_web_address(refused), "{refused:?}");
        }
    }

    #[test]
    fn a_timer_rings_with_its_label_and_how_long_it_was() {
        let set: Timestamp = "2026-11-24T22:05:00Z".parse().unwrap();
        let timer = Timer {
            id: 3,
            label: Some("Pasta".to_string()),
            set,
            ends: set + jiff::SignedDuration::from_secs(600),
            zone: "America/Los_Angeles".to_string(),
        };
        let (title, body) = ring_words(&timer);
        assert_eq!(title, "Pasta \u{2014} timer done");
        // In the person's zone the timer was set in, whatever the machine's.
        assert_eq!(body, "10 minutes, set at 2:05 pm");
        let elsewhere = Timer { zone: "Asia/Kolkata".to_string(), ..timer.clone() };
        assert_eq!(ring_words(&elsewhere).1, "10 minutes, set at 3:35 am");
    }
}
