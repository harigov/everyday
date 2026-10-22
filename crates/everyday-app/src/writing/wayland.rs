//! Writing help on Wayland: an overlay, the PRIMARY selection, and a paste
//! through the RemoteDesktop portal.
//!
//! # The overlay
//!
//! Wayland gives an application no way to ask where the pointer is, and no
//! way to put a window anywhere in particular. What it does do is tell a
//! window where the pointer is *inside it* -- the moment the window appears
//! under the pointer. So the popup's window is made transparent and
//! maximized: it appears under the pointer whatever else is on the screen,
//! learns where in it the pointer is, and the popup is drawn at that spot
//! inside it. Everything around the popup is see-through, and a click on it
//! dismisses the popup, the way a click outside a menu does.
//!
//! It finds the pointer, not the text: after selecting with the mouse they
//! are the same place; after selecting with the keyboard they may not be.
//!
//! # The selection
//!
//! Holding the keyboard, the overlay may read the PRIMARY selection -- on
//! Wayland as on X11, whatever text is selected right now, in whatever
//! application. Nothing is copied, and the clipboard is not touched to read
//! it.
//!
//! # The paste
//!
//! The answer is put on the clipboard while the overlay still holds the
//! keyboard -- Wayland lets only the focused application set it -- and the
//! overlay is hidden, which hands the keyboard back to the application the
//! text is in, still with the text selected. Then Ctrl+V is pressed there
//! through the RemoteDesktop portal: the desktop asks the person once
//! whether Every Day may, and its restore token means it does not ask again.
//!
//! The clipboard is left holding the answer afterwards: putting the old
//! contents back would need the keyboard again.

use std::time::Duration;

use gtk::prelude::*;
use tauri::{AppHandle, WebviewWindow};

use super::{Captured, Mode, on_main, read_selection};

/// How long to wait for the desktop to give the overlay the keyboard. The
/// PRIMARY selection is offered to the focused window only.
const FOCUS_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long to wait for the pointer to be found on the overlay. Longer only
/// when it is over the top bar or the dock, which the overlay does not
/// cover -- and then the popup is centred instead.
const POINTER_TIMEOUT: Duration = Duration::from_millis(600);

/// Where the pointer was found on the overlay last time. See [`open`].
static LAST_POINTER: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);

/// How long to give the keyboard to find its way back to the application
/// the text is in, once the overlay is gone, before pressing Ctrl+V.
const SETTLE: Duration = Duration::from_millis(200);

pub(super) async fn open(
    app: &AppHandle,
    window: &WebviewWindow,
    token: Option<String>,
) -> Result<Captured, String> {
    let shown = window.clone();
    on_main(app, move || -> Result<(), String> {
        let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
        // The token is what lets a window from an application nobody is
        // looking at take the keyboard; without one, GNOME shows a
        // "ready" notification and leaves focus where it was.
        if let Some(token) = token.as_deref() {
            gtk_window.set_startup_id(token);
        }
        gtk_window.set_keep_above(true);
        // A window that cannot be resized cannot be maximized either.
        gtk_window.set_resizable(true);
        // Maximized, not fullscreen: GNOME draws black behind a fullscreen
        // window whatever its transparency, and a maximized one is
        // composited like any other. It leaves the top bar and the dock
        // uncovered, and a pointer over either is a pointer this window
        // cannot see -- the popup centres itself then.
        gtk_window.maximize();
        gtk_window.show_all();
        gtk_window.present();
        Ok(())
    })
    .await??;
    // The window having the keyboard is not the page having it: without
    // this, keys go to GTK and the popup's 1, 2, 3 and Escape do nothing.
    let _ = AsRef::<tauri::Webview>::as_ref(window).set_focus();

    // Focus arrives a round trip or two later.
    let started = std::time::Instant::now();
    while !window.is_focused().unwrap_or(false) {
        if started.elapsed() > FOCUS_TIMEOUT {
            tracing::info!("the overlay was not given the keyboard; reading the selection anyway");
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    // Then the pointer: GDK knows where it is on the overlay once the
    // desktop has said it entered -- after the window has been maximized
    // under it. Until then it answers with wherever the pointer last was on
    // any of this process's windows, which is why the answer is only taken
    // once GDK says the pointer is over this one.
    //
    // And not taken if it is the same answer as last time. GTK keeps the
    // overlay's surface while it is hidden, so GDK can go on believing the
    // pointer is over it, where it was when it was last open; a desktop that
    // has said where it is now has said something different. A pointer that
    // really is on the very same pixel as last time is centred instead,
    // which is the safe way to be wrong.
    let stale = *LAST_POINTER.lock().expect("pointer lock");
    let started = std::time::Instant::now();
    let at = loop {
        let read = window.clone();
        let now = on_main(app, move || pointer_in(&read)).await?;
        if now.is_some() && now != stale {
            break now;
        }
        if started.elapsed() > POINTER_TIMEOUT {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    if at.is_some() {
        *LAST_POINTER.lock().expect("pointer lock") = at;
    }

    let text = read_selection(app, true).await?;

    tracing::debug!(
        pointer = ?at,
        selected = text.as_deref().map(str::len),
        focused = window.is_focused().unwrap_or(false),
        "the overlay read the selection"
    );
    Ok(Captured { text, app: None, mode: Mode::Overlay, at })
}

/// Where the pointer is on the overlay, in its own logical pixels -- which
/// are the webview's CSS pixels -- once the overlay covers the desktop and
/// the pointer is over it.
fn pointer_in(window: &WebviewWindow) -> Option<(f64, f64)> {
    let gtk_window = window.gtk_window().ok()?;
    if !gtk_window.is_maximized() {
        return None;
    }
    let ours = gtk_window.window()?;
    let pointer = ours.display().default_seat()?.pointer()?;
    let (under, _, _) = pointer.window_at_position_double();
    if under?.toplevel() != ours {
        return None;
    }
    let (_, x, y, _) = ours.device_position_double(&pointer);
    Some((x, y))
}

pub(super) fn hide(app: &AppHandle, window: &WebviewWindow) {
    let window = window.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(gtk_window) = window.gtk_window() {
            gtk_window.hide();
        }
    });
}

pub(super) async fn paste(
    app: &AppHandle,
    window: &WebviewWindow,
    text: &str,
) -> Result<(), String> {
    // Set while the overlay still has the keyboard: Wayland accepts a new
    // clipboard only from the focused window.
    let owned = text.to_string();
    on_main(app, move || {
        let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        clipboard.set_text(&owned);
    })
    .await?;

    hide(app, window);

    let restore = super::load_config().restore_token;
    let token = crate::portal::paste(restore, SETTLE).await?;
    super::remember_restore_token(token);
    Ok(())
}
