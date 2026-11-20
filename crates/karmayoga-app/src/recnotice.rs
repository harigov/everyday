//! The meeting card in the corner of the primary monitor.
//!
//! A system notification cannot show a countdown the shell owns, and on
//! Linux its buttons are the only ones the shell hears. This window is the
//! card: small and always on top everywhere a window can be placed.
//!
//! Wayland lets an application do neither -- keep a window on top, or put it
//! where it asks -- so there the card is drawn by this program run again under
//! X11, through XWayland, which allows both ([`crate::topcard`]). Only without
//! XWayland, or if that fails, is it the window below, wherever the desktop
//! puts it, and on GNOME behind whatever window is in front.
//!
//! It is never a window over the whole desktop with the card drawn in its
//! corner. That was the Wayland fallback once: the rest of it was meant to
//! let clicks through, and when that did not take, nothing else on the
//! screen could be clicked while the card was up.
//!
//! The shell owns what the card says (`offers`). This module only makes the
//! window be there. The page asks the shell for the current card when it
//! loads, so a card decided before the page was listening is not missed.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Set while a window is being created, so a countdown tick does not open a
/// second one before the first has been shown.
static OPENING: AtomicBool = AtomicBool::new(false);

/// The card's window. One for the process; hidden while there is nothing to say.
const LABEL: &str = "recording-notice";

/// The card's size where it is a window of its own, in logical pixels.
const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 176.0;
/// Inset from the corner of the monitor.
const MARGIN: f64 = 16.0;

/// Show the card. A window that is already up is left where it is: the page
/// hears the new contents on its own, and moving it on every countdown tick
/// would drag it out from under the pointer.
pub fn present(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) && crate::topcard::available() {
        crate::topcard::refresh(app);
        return;
    }
    if app.get_webview_window(LABEL).is_some_and(|w| w.is_visible().unwrap_or(false)) {
        return;
    }
    if OPENING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = open(&app);
        OPENING.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            tracing::warn!(error = %e, "the meeting card could not be shown");
        }
    });
}

/// Take the card down. The window stays, hidden, for the next meeting.
pub fn dismiss(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    crate::topcard::dismiss();
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}

fn window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("recording.html".into()))
        .title("Meeting")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .focused(false)
        .on_navigation(crate::navigation::guard(crate::navigation::dev_url(app)))
        .build()
        .map_err(|e| e.to_string())
}

fn open(app: &AppHandle) -> Result<(), String> {
    let window = window(app)?;
    place(app, &window);
    let _ = window.show();
    Ok(())
}

/// The top-right of the primary monitor. A monitor the platform will not
/// name leaves the window wherever the platform puts a new one.
fn place(app: &AppHandle, window: &tauri::WebviewWindow) {
    use tauri::{LogicalPosition, LogicalSize};
    let _ = window.set_size(LogicalSize::new(WIDTH, HEIGHT));
    let Ok(Some(monitor)) = app.primary_monitor() else {
        let _ = window.center();
        return;
    };
    let scale = monitor.scale_factor();
    let pos = monitor.position();
    let size = monitor.size();
    let x = pos.x as f64 / scale + size.width as f64 / scale - WIDTH - MARGIN;
    let y = pos.y as f64 / scale + MARGIN;
    let _ = window.set_position(LogicalPosition::new(x, y));
}
