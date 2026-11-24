//! Typing a dictation into the field that has the keyboard.
//!
//! Writing help's paste, without writing help's popup: the text goes on the
//! clipboard, Ctrl+V (Cmd+V) is pressed, and the clipboard is put back.
//!
//! The difference is that nothing of ours should have the keyboard.
//! Everywhere but Wayland that does not matter -- any application may set the
//! clipboard and press a key. On Wayland only the focused window may set the
//! clipboard, so the helper window ([`super::pill`]: transparent, drawing
//! nothing, made hidden when the dictation began) is shown for that moment
//! and asks for the keyboard, with the activation token the key press
//! brought, sets it, and goes, handing the keyboard back to the field; then
//! Ctrl+V is pressed through the RemoteDesktop portal, as writing help does.
//! What the person sees meanwhile is the panel's card, which never has the
//! keyboard.

use tauri::AppHandle;

/// Type `text` into whatever has the keyboard. `token` is the latest
/// activation token the dictation key brought (Wayland). An error says why it
/// could not be typed, in words for the card.
///
/// On Wayland the helper window is shown and hidden again by this, and the
/// clipboard is left holding the text, as after writing help's paste: putting
/// the old contents back would need the keyboard again.
pub async fn paste(app: &AppHandle, text: &str, token: Option<String>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) {
        return wayland::paste(app, text, token).await;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = token;
    crate::writing::desktop::paste(app, None, text).await
}

/// Put `text` on the clipboard, for a Copy button whose card could not do it
/// itself (the card does, where it can: on Wayland it is the window that was
/// clicked, so it has the keyboard, and says so -- see
/// [`crate::panel::Action::copy`]).
pub async fn copy(app: &AppHandle, text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) {
        return wayland::copy(app, text).await;
    }
    crate::writing::desktop::set_clipboard_text(app, text).await
}

/// The Wayland paste: the helper window takes the keyboard for a moment, sets
/// the clipboard, and goes.
#[cfg(target_os = "linux")]
mod wayland {
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use tauri::{AppHandle, WebviewWindow};

    use crate::writing::on_main;

    use super::super::pill;

    /// How long to wait for the desktop to give the helper window the
    /// keyboard before a paste. Writing help waits as long for its overlay.
    const FOCUS_TIMEOUT: Duration = Duration::from_millis(1500);

    /// The same, for a copy. Nothing was clicked and there is no token, so
    /// this is only a try: the card's own Copy button is what normally does it.
    const COPY_FOCUS_TIMEOUT: Duration = Duration::from_millis(1000);

    /// How long to give the keyboard to find its way back to the field once the
    /// window is gone, before pressing Ctrl+V. As in writing help.
    const SETTLE: Duration = Duration::from_millis(200);

    const REFUSED: &str = "the desktop would not let Karma Yoga type here";
    const REFUSED_COPY: &str = "the desktop would not let Karma Yoga copy it";

    /// The helper window, or an error in words for the card.
    fn helper_window(app: &AppHandle) -> Result<WebviewWindow, String> {
        pill::existing(app)
            .ok_or_else(|| "voice typing's paste window has not been made".to_string())
    }

    /// Show the helper window and ask the desktop to give it the keyboard, and
    /// wait until it has, or `timeout` has passed. Whether it was given.
    ///
    /// The window is not shown at any other time (it was made hidden, and
    /// GNOME on Wayland hands every window it shows the keyboard, which is
    /// the one thing the person dictating must not lose); for this moment it
    /// is, and it accepts the keyboard. The activation token is what lets a
    /// window nobody clicked take the keyboard: without one, GNOME shows a
    /// "ready" notification and leaves focus where it was.
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
            // Made hidden, and shown for the first time here: the keyboard on
            // mapping is wanted now, and the page is shown with the window.
            gtk_window.set_focus_on_map(true);
            if let Some(token) = token.as_deref() {
                gtk_window.set_startup_id(token);
            }
            gtk_window.show_all();
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

    /// Put the helper window away again: hidden, which hands the keyboard back
    /// to the field it was taken from, and not accepting the keyboard.
    async fn refuse_keyboard(app: &AppHandle, window: &WebviewWindow) {
        let window = window.clone();
        let _ = on_main(app, move || {
            if let Ok(gtk_window) = window.gtk_window() {
                gtk_window.hide();
                gtk_window.set_accept_focus(false);
            }
        })
        .await;
    }

    pub async fn paste(app: &AppHandle, text: &str, token: Option<String>) -> Result<(), String> {
        let window = helper_window(app)?;
        if !take_keyboard(app, &window, token, FOCUS_TIMEOUT).await? {
            tracing::info!("voice typing's paste window was not given the keyboard; nothing typed");
            refuse_keyboard(app, &window).await;
            return Err(REFUSED.into());
        }
        // Set while the window has the keyboard, because Wayland accepts a new
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
        let window = helper_window(app)?;
        if !take_keyboard(app, &window, None, COPY_FOCUS_TIMEOUT).await? {
            refuse_keyboard(app, &window).await;
            return Err(REFUSED_COPY.into());
        }
        let owned = text.to_string();
        let shown = window.clone();
        // Nothing is typed afterwards, so the keyboard goes back at once.
        on_main(app, move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            clipboard.set_text(&owned);
            clipboard.store();
            if let Ok(gtk_window) = shown.gtk_window() {
                gtk_window.hide();
                gtk_window.set_accept_focus(false);
            }
        })
        .await
    }
}
