//! An app in a window of its own.
//!
//! The todo list beside the editor you are writing in, the notes next to a
//! call: one app of the window's, opened again by itself in a second window,
//! while the main window goes on showing every app as it always did.
//!
//! A second window is a second webview, with its own copy of the interface
//! and its own stores, talking to the same service over the same commands as
//! the main window does -- the interface finds out which app it is pinned to
//! from its window's label (`app-todo`), see `ui/src/lib/appwindow.ts`. What
//! keeps the two in step is what already kept a phone and a laptop in step:
//! every write is announced, and a window reloads what somebody else
//! changed. That is why a window here calls with [`Ctx::window`] rather than
//! [`Ctx::local`]: the main window drops `local` changes as its own, so a
//! task added here has to be stamped as somebody else's to reach it.
//!
//! Which is also why these are for a vault in this process only. A vault on
//! another computer stamps every write with this machine's device id, the
//! window's own origin never reaches it, and its server sends nothing back to
//! the device that wrote -- so two windows on one remote vault would never
//! hear of each other's writes. [`open_app_window`] refuses there, and
//! connecting to one closes any that are open (`commands::attach`).
//!
//! What stays the main window's alone: the tray, the badge, notifications,
//! the palette's hotkey, meeting offers, Settings. Each is one thing per
//! process, and two webviews each pushing a tray menu or posting the same
//! notification is two of it. A window here asks the main window to show
//! anything that is not its own app -- see [`show_in_main`] -- and only the
//! app: whatever the window meant to open there (a note, an event) has no
//! store here to be opened in, so that is left to the person.
//!
//! Closing one of these closes it, never the application: it saves first,
//! through the same handshake as the main window's, and nothing is locked
//! or hidden. Closing the main window hides it when it can (see `lib.rs`'s
//! close handler: with the tray up, or with work to do and the hotkey to
//! come back by), and these stay. When the application quits instead --
//! from the tray, or because closing the main window had to -- these close
//! too, after they have saved; see `commands::ready_to_close`.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use everyday_service::ctx::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent};

use crate::SAVE_AND_CLOSE;

/// Every app window's label starts with this, and nothing else's does.
const PREFIX: &str = "app-";

/// The main window's label, from `tauri.conf.json`.
pub const MAIN: &str = "main";

/// Asks the main window to show an app, or Settings. Its other half is
/// `onShowInMain` in `ui/src/lib/api.ts`.
pub const SHOW_IN_MAIN_EVENT: &str = "everyday://show-in-main";

/// The apps a window may be opened on, with the name its title bar gives it.
///
/// The interface's `APP_ORDER` in `ui/src/lib/apps.ts`, copied rather than
/// trusted: a label -- and so a window -- is never made from whatever string
/// arrived.
const APPS: &[(&str, &str)] = &[
    ("overview", "Overview"),
    ("notes", "Notes"),
    ("todo", "Todo"),
    ("calendar", "Calendar"),
    ("library", "Library"),
    ("mail", "Mail"),
    ("feed", "Feed"),
    ("money", "Money"),
    ("screentime", "Screen time"),
    ("assistant", "Assistant"),
    ("journal", "Journal"),
];

/// The app called `section`, with its name -- or a refusal, for a name
/// that is not one.
fn app_named(section: &str) -> CommandResult<(&'static str, &'static str)> {
    APPS.iter()
        .find(|(s, _)| *s == section)
        .copied()
        .ok_or_else(|| CommandError::new("invalid", format!("there is no app called {section}")))
}

/// Is this one of the windows opened on a single app?
pub fn is_app_window(label: &str) -> bool {
    label.starts_with(PREFIX)
}

/// Who a call from the window labelled `label` comes from. See the module
/// doc for why it is not always [`Ctx::local`].
pub fn ctx_for(label: &str) -> Ctx {
    if is_app_window(label) { Ctx::window(label) } else { Ctx::local() }
}

/// The app windows that have been asked to save and close and have not gone
/// yet, by label.
///
/// One flag per window rather than `AppState`'s one for the main window: a
/// second close while the first is still saving is somebody who does not
/// want to wait, and is let through -- for that window, not for every other.
#[derive(Default)]
pub struct AppWindows {
    closing: Mutex<HashSet<String>>,
}

impl AppWindows {
    fn begin_closing(&self, label: &str) -> bool {
        self.closing.lock().unwrap().insert(label.to_string())
    }

    fn closed(&self, label: &str) {
        self.closing.lock().unwrap().remove(label);
    }

    fn is_closing(&self, label: &str) -> bool {
        self.closing.lock().unwrap().contains(label)
    }
}

/// Open `section` in a window of its own, or bring forward the one already
/// showing it.
#[tauri::command]
pub async fn open_app_window(
    app: AppHandle,
    state: tauri::State<'_, crate::state::AppState>,
    section: String,
) -> CommandResult<()> {
    let (section, name) = app_named(&section)?;
    if state.is_remote() {
        return Err(CommandError::new(
            "unsupported",
            "a vault on another computer can only be open in one window",
        ));
    }
    let label = format!("{PREFIX}{section}");
    // One that was closed a moment ago may still be saving, and is going
    // whatever happens next: bringing it forward would show a window about to
    // vanish. Its replacement waits for it, which is never longer than the
    // close's own grace.
    if app.state::<AppWindows>().is_closing(&label) {
        let deadline =
            tokio::time::Instant::now() + crate::CLOSE_GRACE + Duration::from_millis(500);
        while app.get_webview_window(&label).is_some() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    // The main window's settings, less the ones that are about being the
    // main window: no centring over it, and a size for one app rather than
    // for an app bar, a sidebar and an app. Not a smaller minimum than the
    // main window's, though: every app's layout was made to fit that, and
    // without an app bar beside it there is only a little more room.
    let builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .title(format!("{name} \u{2014} Every Day"))
        .inner_size(960.0, 720.0)
        .min_inner_size(720.0, 520.0)
        .resizable(true)
        // Without the system's title bar off macOS, as the main window is --
        // the interface draws its own controls. See `lib.rs`'s setup.
        .decorations(cfg!(target_os = "macos"))
        // The main window's `dragDropEnabled: false`: a picture dropped on a
        // message is the interface's to take, not the webview's to open.
        .disable_drag_drop_handler()
        .on_navigation(crate::navigation::guard(crate::navigation::dev_url(&app)))
        .on_new_window(crate::navigation::new_window(app.clone()));
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    builder.build().map_err(|e| CommandError::new("window_failed", e.to_string()))?;
    Ok(())
}

/// What [`SHOW_IN_MAIN_EVENT`] carries: an app to switch to, or a tab of
/// Settings to open. The interface checks the tab against its own list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShowInMain {
    section: Option<&'static str>,
    settings: Option<String>,
}

/// Bring the main window forward showing `section`, or Settings at the tab
/// named `settings`.
///
/// What a window of one app does with anything that is not that app: a
/// link to an event from a task, "Go to Mail" from the palette, the key for
/// Settings. It cannot show them itself without becoming a second main
/// window, so it hands them over.
#[tauri::command]
pub async fn show_in_main(
    app: AppHandle,
    section: Option<String>,
    settings: Option<String>,
) -> CommandResult<()> {
    let section = section.map(|asked| app_named(&asked).map(|(s, _)| s)).transpose()?;
    crate::tray::raise_window(&app);
    app.emit_to(MAIN, SHOW_IN_MAIN_EVENT, ShowInMain { section, settings })
        .map_err(|e| CommandError::new("internal", e.to_string()))
}

/// An app window's own events: the save before it closes, and forgetting it
/// once it has.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let Some(windows) = window.try_state::<AppWindows>() else { return };
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            if windows.begin_closing(window.label()) {
                api.prevent_close();
                ask_to_close(window);
            }
        }
        WindowEvent::Destroyed => windows.closed(window.label()),
        _ => {}
    }
}

/// Ask every app window to save and close: the application is quitting, or is
/// about to look at a vault on another computer.
pub fn close_all(app: &AppHandle) {
    let Some(windows) = app.try_state::<AppWindows>() else { return };
    for (label, window) in app.webview_windows() {
        if is_app_window(&label) && windows.begin_closing(&label) {
            ask_to_close(&window.as_ref().window());
        }
    }
}

/// The main window's half of the handshake, for one of these: ask the
/// interface to write what it holds and close itself, and close it anyway if
/// it has not after `CLOSE_GRACE` -- a wedged interface must not leave a
/// window that will not shut.
fn ask_to_close(window: &Window) {
    // To this window alone. The interface listens on its own window -- see
    // `onSaveAndClose` -- so the main window does not hear it and close too.
    let _ = window.emit_to(window.label(), SAVE_AND_CLOSE, ());
    // This window, not whichever has its label in three seconds: one closed
    // and opened again in that time is a new window, and is not to go.
    let window = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(crate::CLOSE_GRACE);
        let _ = window.destroy();
    });
}

/// Destroy what is left of the app windows, unasked: the main window has
/// gone, and the application with it.
pub fn destroy_all(app: &AppHandle) {
    for (label, window) in app.webview_windows() {
        if is_app_window(&label) {
            let _ = window.destroy();
        }
    }
}

/// Wait, a little, for the app windows to finish closing.
///
/// What the main window's close does before it locks the vault: each app
/// window is writing out what was typed in it, and dropping the key under
/// those writes would lose them. Bounded, because a wedged window is closed
/// on its own timer anyway and quitting must not wait on it.
pub async fn settle(app: &AppHandle, at_most: Duration) {
    let deadline = tokio::time::Instant::now() + at_most;
    while app.webview_windows().keys().any(|label| is_app_window(label))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_app_window_calls_as_a_window_of_its_own() {
        assert_eq!(ctx_for("app-todo").caller.origin(), Some("window:app-todo"));
        assert_eq!(ctx_for(MAIN).caller.origin(), Some("local"));
        // Writing help's popup calls as the process, as it always has.
        assert_eq!(ctx_for(crate::writing::LABEL).caller.origin(), Some("local"));
    }

    #[test]
    fn every_app_makes_an_app_window_label() {
        for (section, _) in APPS {
            assert!(is_app_window(&format!("{PREFIX}{section}")));
        }
        assert!(!is_app_window(MAIN));
        assert!(!is_app_window(crate::writing::LABEL));
    }
}
