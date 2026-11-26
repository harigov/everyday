//! The window that lets voice typing set the clipboard on Wayland, and nothing
//! else.
//!
//! It was once the pill at the bottom of the screen that said a dictation was
//! happening. What voice typing shows is now a card in the panel
//! ([`crate::panel`]; the card is built by [`super::card`]), drawn by the panel
//! and never taking the keyboard, so this window draws nothing: its page
//! (`ui/src/voicetyping/`) is empty and calls nothing, and it is never shown
//! for anybody to look at.
//!
//! It is here because of what typing a dictation takes. The text goes on the
//! clipboard and Ctrl+V is pressed in the field that has the keyboard; on
//! Wayland only a window that has the keyboard may set the clipboard, and the
//! panel's windows must never have it. So when a dictation starts on Wayland
//! the window is made, hidden ([`ensure`]); [`super::paste`] shows it for the
//! moment the clipboard is set, which is also when it asks the desktop for the
//! keyboard, and hides it again, which hands the keyboard back to the field it
//! was taken from. Anywhere else any application may set the clipboard and
//! press a key, nothing needs the window, and it is never made.
//!
//! # The keyboard on Wayland
//!
//! GNOME on Wayland has no hint an application can give to say "do not focus
//! me", and hands every newly shown window the keyboard (checked against GNOME
//! 50: keys typed while the old pill listened reached the pill, and Escape
//! cancelled the dictation). That is why this window is not shown while the
//! person is dictating, as the pill was: they are typing into somebody else's
//! window at that moment, or about to. It is shown only when a paste wants the
//! keyboard on purpose.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The window's label. The dictation slot is claimed under it
/// (`crate::dictation::claim_for`), and the application's window handling
/// treats it as a window that is hidden, not closed.
pub const LABEL: &str = "voice-typing";

/// Set while the window is being made, so a second call before the first has
/// finished does not make another.
static OPENING: AtomicBool = AtomicBool::new(false);

/// How big the window is, in logical pixels. It is transparent and draws
/// nothing, so the size matters only for the moment it is on screen: the
/// smaller it is, the less of the desktop it covers -- a window that has the
/// keyboard also takes the clicks that land on it -- while it is there.
const SIZE: f64 = 32.0;

/// Make the window, hidden, if this is Wayland and it has not been made yet,
/// so that it is there by the time a paste wants it. Returns at once; the
/// window is made on another task.
pub fn ensure(app: &AppHandle) {
    if !crate::writing::wayland_session(app) || existing(app).is_some() {
        return;
    }
    if OPENING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let made = window(&app);
        OPENING.store(false, Ordering::SeqCst);
        if let Err(e) = made {
            tracing::warn!(error = %e, "voice typing's paste window could not be made");
        }
    });
}

/// The window, if it has been made.
pub(super) fn existing(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Hide the window and stop it accepting the keyboard, if it is up: a
/// dictation cancelled in the middle of its paste must hand the keyboard back
/// to the field it was taken from. Hidden through GTK, as the paste hides it,
/// and without waiting for the main thread.
pub fn release(app: &AppHandle) {
    let Some(window) = existing(app) else { return };
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        let _ = app.run_on_main_thread(move || {
            if let Ok(gtk_window) = window.gtk_window() {
                gtk_window.hide();
                gtk_window.set_accept_focus(false);
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = window.hide();
}

/// The window, made hidden on first use. It does not ask for the keyboard, not
/// even when it is shown: [`super::paste`] asks, for the moment it needs it,
/// and until then the words are going to the field that has it.
fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = existing(app) {
        return Ok(window);
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("voice-typing.html".into()))
        .title("Voice typing")
        .inner_size(SIZE, SIZE)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .visible(false)
        .focused(false)
        .focusable(false)
        .on_navigation(crate::navigation::guard(crate::navigation::dev_url(app)))
        .build()
        // Over the app being typed into, full-screen or not. See `overlay`.
        .inspect(crate::overlay::float_over_full_screen)
        .map_err(|e| e.to_string())
}
