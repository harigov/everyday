//! Typing a dictation into the field that has the keyboard.
//!
//! Writing help's paste, without writing help's popup: the text goes on the
//! clipboard, Ctrl+V (Cmd+V) is pressed, and the clipboard is put back.
//!
//! The difference is that nothing of ours should have the keyboard.
//! Everywhere but Wayland that does not matter -- any application may set the
//! clipboard and press a key. On Wayland only the focused window may set the
//! clipboard, so the pill takes the keyboard for that moment (GNOME has
//! usually handed it the keyboard already, when it was shown -- see
//! [`super::pill`]), with the activation token the key press brought, sets it,
//! and goes, handing the keyboard back to the field; then Ctrl+V is pressed
//! through the RemoteDesktop portal, as writing help does.

use tauri::AppHandle;

/// Type `text` into whatever has the keyboard. `token` is the latest
/// activation token the dictation key brought (Wayland). An error says why it
/// could not be typed, in words for the pill.
///
/// On Wayland the pill is hidden by this, and the clipboard is left holding
/// the text, as after writing help's paste: putting the old contents back
/// would need the keyboard again.
pub async fn paste(app: &AppHandle, text: &str, token: Option<String>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) {
        return wayland::paste(app, text, token).await;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = token;
    crate::writing::desktop::paste(app, None, text).await
}

/// Put `text` on the clipboard -- the pill's Copy button, pressed in the pill,
/// which has the keyboard because it was clicked.
pub async fn copy(app: &AppHandle, text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) {
        return wayland::copy(app, text).await;
    }
    crate::writing::desktop::set_clipboard_text(app, text).await
}

/// The Wayland paste: the pill's window takes the keyboard for a moment, sets
/// the clipboard, and goes.
#[cfg(target_os = "linux")]
mod wayland {
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use tauri::{AppHandle, WebviewWindow};

    use crate::writing::on_main;

    use super::super::pill;

    /// How long to wait for the desktop to give the pill the keyboard before
    /// a paste. Writing help waits as long for its overlay.
    const FOCUS_TIMEOUT: Duration = Duration::from_millis(1500);

    /// The same, for the Copy button: the pill was clicked a moment ago, so it
    /// is most likely focused already.
    const COPY_FOCUS_TIMEOUT: Duration = Duration::from_millis(1000);

    /// How long to give the keyboard to find its way back to the field once the
    /// pill is gone, before pressing Ctrl+V. As in writing help.
    const SETTLE: Duration = Duration::from_millis(200);

    const REFUSED: &str = "the desktop would not let Karma Yoga type here";

    /// The pill's window, or an error in words for the pill.
    fn pill_window(app: &AppHandle) -> Result<WebviewWindow, String> {
        pill::existing(app).ok_or_else(|| "the dictation pill is not on screen".to_string())
    }

    /// Ask the desktop to give the pill the keyboard, and wait until it has,
    /// or `timeout` has passed. Whether it was given.
    ///
    /// The pill does not ask for focus the rest of the time (GNOME on Wayland
    /// gives it the keyboard when it is shown regardless); for this moment it
    /// does. The
    /// activation token is what lets a window nobody clicked take the keyboard:
    /// without one, GNOME shows a "ready" notification and leaves focus where
    /// it was.
    async fn take_keyboard(
        app: &AppHandle,
        window: &WebviewWindow,
        token: Option<String>,
        timeout: Duration,
    ) -> Result<bool, String> {
        let shown = window.clone();
        on_main(app, move || -> Result<(), String> {
            let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
            gtk_window.set_accept_focus(true);
            if let Some(token) = token.as_deref() {
                gtk_window.set_startup_id(token);
            }
            gtk_window.present();
            Ok(())
        })
        .await??;
        // The window having the keyboard is not the page having it.
        let _ = AsRef::<tauri::Webview>::as_ref(window).set_focus();

        let started = Instant::now();
        while !window.is_focused().unwrap_or(false) {
            if started.elapsed() > timeout {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        Ok(true)
    }

    /// Put the pill back to not accepting the keyboard.
    async fn refuse_keyboard(app: &AppHandle, window: &WebviewWindow) {
        let window = window.clone();
        let _ = on_main(app, move || {
            if let Ok(gtk_window) = window.gtk_window() {
                gtk_window.set_accept_focus(false);
            }
        })
        .await;
    }

    pub async fn paste(app: &AppHandle, text: &str, token: Option<String>) -> Result<(), String> {
        let window = pill_window(app)?;
        if !take_keyboard(app, &window, token, FOCUS_TIMEOUT).await? {
            tracing::info!("the dictation pill was not given the keyboard; nothing typed");
            refuse_keyboard(app, &window).await;
            return Err(REFUSED.into());
        }
        // Set while the pill has the keyboard, because Wayland accepts a new
        // clipboard only from the focused window; then hidden, which hands the
        // keyboard back to the field the person was typing in.
        let owned = text.to_string();
        let shown = window.clone();
        on_main(app, move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            clipboard.set_text(&owned);
            clipboard.store();
            if let Ok(gtk_window) = shown.gtk_window() {
                gtk_window.hide();
                gtk_window.set_accept_focus(false);
            }
        })
        .await?;

        let restore = crate::writing::load_config().restore_token;
        let token = crate::portal::paste(restore, SETTLE).await?;
        crate::writing::remember_restore_token(token);
        Ok(())
    }

    pub async fn copy(app: &AppHandle, text: &str) -> Result<(), String> {
        let window = pill_window(app)?;
        if !take_keyboard(app, &window, None, COPY_FOCUS_TIMEOUT).await? {
            refuse_keyboard(app, &window).await;
            return Err(REFUSED.into());
        }
        let owned = text.to_string();
        let shown = window.clone();
        // The pill stays up; whoever called this takes it down. Only its
        // refusal to accept the keyboard is put back.
        on_main(app, move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            clipboard.set_text(&owned);
            clipboard.store();
            if let Ok(gtk_window) = shown.gtk_window() {
                gtk_window.set_accept_focus(false);
            }
        })
        .await
    }
}
