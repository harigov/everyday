//! The meeting card in the corner of the primary monitor.
//!
//! A system notification cannot show a countdown the shell owns, and on
//! Linux its buttons are the only ones the shell hears. This window is the
//! card: small and always on top everywhere a window can be placed, and, on
//! Wayland, a transparent window over the desktop with the card drawn in its
//! top-right corner -- the same constraint writing help works under, because
//! Wayland will not put a window where an application asks.
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
/// Inset from the corner of the monitor, and of the Wayland overlay.
const MARGIN: f64 = 16.0;

/// Show the card. A window that is already up is left where it is: the page
/// hears the new contents on its own, and moving it on every countdown tick
/// would drag it out from under the pointer.
pub fn present(app: &AppHandle) {
    if app.get_webview_window(LABEL).is_some_and(|w| w.is_visible().unwrap_or(false)) {
        return;
    }
    if OPENING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = open(&app).await;
        OPENING.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            tracing::warn!(error = %e, "the meeting card could not be shown");
        }
    });
}

/// Take the card down. The window stays, hidden, for the next meeting.
pub fn dismiss(app: &AppHandle) {
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

async fn open(app: &AppHandle) -> Result<(), String> {
    let window = window(app)?;
    if crate::writing::wayland_session(app) {
        cover(app, &window).await?;
    } else {
        place(app, &window);
        let _ = window.show();
    }
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

/// Wayland: a transparent window over the work area, the card in its
/// top-right, and clicks outside the card going through to whatever is
/// behind it so joining the meeting is still possible.
#[cfg(target_os = "linux")]
async fn cover(app: &AppHandle, window: &tauri::WebviewWindow) -> Result<(), String> {
    let shown = window.clone();
    crate::writing::on_main(app, move || -> Result<(), String> {
        use gtk::prelude::*;
        let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
        gtk_window.set_keep_above(true);
        gtk_window.set_focus_on_map(false);
        gtk_window.set_resizable(true);
        gtk_window.maximize();
        gtk_window.show_all();
        // The card sits in the top-right of this window. Everything else
        // must not take the click, or the person cannot press Join in the
        // meeting itself.
        if let Some(gdk_window) = gtk_window.window() {
            let scale = gdk_window.scale_factor();
            let width = gdk_window.width();
            let card_w = (WIDTH * scale as f64) as i32;
            let card_h = (HEIGHT * scale as f64) as i32;
            let margin = (MARGIN * scale as f64) as i32;
            let x = (width - card_w - margin).max(0);
            let rect = gtk::cairo::RectangleInt::new(x, margin, card_w, card_h);
            let region = gtk::cairo::Region::create_rectangle(&rect);
            gdk_window.input_shape_combine_region(&region, 0, 0);
        }
        Ok(())
    })
    .await??;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn cover(_app: &AppHandle, window: &tauri::WebviewWindow) -> Result<(), String> {
    let _ = window.show();
    Ok(())
}
