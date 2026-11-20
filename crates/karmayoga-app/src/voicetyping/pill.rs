//! The pill at the bottom of the screen that says a dictation is happening.
//!
//! Its own window, which does not ask for the keyboard: the keyboard belongs
//! to the field the words are going to. Everywhere a window can be placed it
//! is a small always-on-top window near the bottom middle of the screen the
//! pointer is on. On Wayland, which places windows itself, it is a
//! transparent window over the desktop with the pill drawn at its bottom
//! middle and clicks everywhere else going through. The meeting card was once
//! drawn this way too, with the region that lets clicks through worked out
//! once, before the window had its final size, and nothing else on the screen
//! could be clicked while it was up; here the region follows every size
//! change (`cover`).
//!
//! # The keyboard on Wayland
//!
//! On X11 the window manager reads "do not focus me" from the window's hints
//! and leaves the keyboard where it is. GNOME on Wayland has no such hint for
//! an application to give, and hands every newly shown window the keyboard
//! (checked against GNOME 50: keys typed while the pill listens reach the
//! pill, and Escape cancels the dictation). So there the pill holds the
//! keyboard while it is up, and hiding it gives the keyboard back to the field
//! it was taken from -- which is the moment the paste needs, see
//! [`super::paste`]. Keys typed meanwhile are not typed anywhere. Only a pill
//! drawn by the shell itself, through an extension, could do better.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The pill's window label.
pub const LABEL: &str = "voice-typing";

/// Set while the window is being created or shown, so a second call before the
/// first has finished does not open another.
static OPENING: AtomicBool = AtomicBool::new(false);

/// Whether the overlay's input region follows the window's size yet. Reset
/// when the window is built, because the signal handler belongs to the window.
#[cfg(target_os = "linux")]
static WATCHING: AtomicBool = AtomicBool::new(false);

/// The pill's hit box, in logical pixels. The page draws exactly this much:
/// as the whole window in [`PillMode::Window`], and inside the overlay in
/// [`PillMode::Overlay`]. `ui/src/voicetyping/` holds the other half of this
/// contract; change neither alone.
const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 72.0;
/// How far the box's bottom edge is above the bottom of the screen's work area
/// (or of the overlay).
const MARGIN: f64 = 32.0;
/// Where the work area is not known, extra room to keep the pill clear of a
/// dock or taskbar, in logical pixels.
const DOCK_ALLOWANCE: f64 = 48.0;

/// How the pill is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PillMode {
    /// At the bottom middle of a transparent window covering the work area.
    Overlay,
    /// As the whole of a small window already placed.
    Window,
}

/// How the pill is drawn on this desktop.
pub fn mode(app: &AppHandle) -> PillMode {
    if crate::writing::wayland_session(app) { PillMode::Overlay } else { PillMode::Window }
}

/// Show the pill, without asking for the keyboard (which GNOME on Wayland
/// hands it anyway -- see the module doc). One already up stays where it
/// is, so a second call -- the state changing under it -- does not move it out
/// from under the pointer. Returns at once; the window is made and shown on
/// another task.
pub fn present(app: &AppHandle) {
    if existing(app).is_some_and(|w| w.is_visible().unwrap_or(false)) {
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
            tracing::warn!(error = %e, "the voice typing pill could not be shown");
        }
    });
}

/// Take the pill down. The window stays, hidden, for the next time. On
/// Wayland it is hidden through GTK, as writing help's overlay is: hiding it
/// is also what hands the keyboard back, should it have had it for a paste.
pub fn dismiss(app: &AppHandle) {
    let Some(window) = existing(app) else { return };
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) {
        hide_overlay(app, &window);
        return;
    }
    let _ = window.hide();
}

/// The pill's window, if it has been made.
pub(super) fn existing(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Hide the overlay through GTK. Does not wait for the main thread.
#[cfg(target_os = "linux")]
pub(super) fn hide_overlay(app: &AppHandle, window: &WebviewWindow) {
    use gtk::prelude::*;
    let window = window.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(gtk_window) = window.gtk_window() {
            gtk_window.hide();
        }
    });
}

/// The window, made hidden on first use. It never asks for the keyboard: the
/// words are going to the field that has it, and a pill that took it would
/// send them nowhere.
fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = existing(app) {
        return Ok(window);
    }
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("voice-typing.html".into()))
        .title("Voice typing")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .focused(false)
        // Where the platform has a way to say so, this also keeps a click on
        // the pill's buttons from activating it.
        .focusable(false)
        .on_navigation(crate::navigation::guard(crate::navigation::dev_url(app)))
        .build()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    WATCHING.store(false, Ordering::SeqCst);
    Ok(window)
}

async fn open(app: &AppHandle) -> Result<(), String> {
    let window = window(app)?;
    if crate::writing::wayland_session(app) {
        cover(app, &window).await
    } else {
        place(app, &window);
        reveal(app, &window).await
    }
}

/// The bottom middle of the monitor the pointer is on, above the dock or
/// taskbar. Where the pointer cannot be found the primary monitor does, and
/// where no monitor can be named the window is centred.
fn place(app: &AppHandle, window: &WebviewWindow) {
    use tauri::{LogicalSize, PhysicalPosition};
    let _ = window.set_size(LogicalSize::new(WIDTH, HEIGHT));
    let under_pointer =
        app.cursor_position().ok().and_then(|at| app.monitor_from_point(at.x, at.y).ok().flatten());
    let Some(monitor) = under_pointer.or_else(|| app.primary_monitor().ok().flatten()) else {
        let _ = window.center();
        return;
    };
    let scale = monitor.scale_factor();
    // The work area is the monitor minus the dock, the panel and the
    // taskbar. A platform that cannot say reports an empty one, and then the
    // pill is lifted by an allowance instead.
    let work = monitor.work_area();
    let (left, top, width, height) = if work.size.width > 0 && work.size.height > 0 {
        (work.position.x, work.position.y, work.size.width as f64, work.size.height as f64)
    } else {
        let pos = monitor.position();
        let size = monitor.size();
        let lift = DOCK_ALLOWANCE * scale;
        (pos.x, pos.y, size.width as f64, size.height as f64 - lift)
    };
    let x = left as f64 + (width - WIDTH * scale) / 2.0;
    let y = top as f64 + height - (HEIGHT + MARGIN) * scale;
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

/// Show the window where it is, without the keyboard. On X11 the window
/// manager decides who gets the keyboard from hints on the window, so it is
/// told twice -- not on mapping, not at all -- before the window appears.
async fn reveal(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let shown = window.clone();
        crate::writing::on_main(app, move || -> Result<(), String> {
            use gtk::prelude::*;
            let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
            gtk_window.set_accept_focus(false);
            gtk_window.set_focus_on_map(false);
            gtk_window.set_keep_above(true);
            gtk_window.show_all();
            Ok(())
        })
        .await??;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        window.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Wayland: a transparent window over the work area, the pill drawn at its
/// bottom middle, and clicks anywhere else going through to what is behind it:
/// Wayland puts windows where it likes.
#[cfg(target_os = "linux")]
async fn cover(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let shown = window.clone();
    crate::writing::on_main(app, move || -> Result<(), String> {
        use gtk::prelude::*;
        let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
        gtk_window.set_keep_above(true);
        gtk_window.set_focus_on_map(false);
        gtk_window.set_accept_focus(false);
        // A window that cannot be resized cannot be maximized either.
        gtk_window.set_resizable(true);
        gtk_window.maximize();
        // The window's size right after `maximize` is usually not the final
        // one: the desktop answers a moment later. The region that lets clicks
        // through is worked out from the size, so it is worked out again every
        // time the size changes, not once.
        if !WATCHING.swap(true, Ordering::SeqCst) {
            gtk_window.connect_size_allocate(|window, allocation| {
                if let Some(gdk_window) = window.window() {
                    pass_clicks_through(&gdk_window, allocation.width(), allocation.height());
                }
            });
        }
        gtk_window.show_all();
        if let Some(gdk_window) = gtk_window.window() {
            pass_clicks_through(
                &gdk_window,
                gtk_window.allocated_width(),
                gtk_window.allocated_height(),
            );
        }
        Ok(())
    })
    .await??;
    Ok(())
}

/// Let every click on the overlay but those on the pill's box through. GDK's
/// coordinates here are logical pixels, as the box is.
#[cfg(target_os = "linux")]
fn pass_clicks_through(gdk_window: &gtk::gdk::Window, width: i32, height: i32) {
    if width <= 1 || height <= 1 {
        return;
    }
    let (box_w, box_h) = (WIDTH as i32, HEIGHT as i32);
    let x = ((width - box_w) / 2).max(0);
    let y = (height - MARGIN as i32 - box_h).max(0);
    let rect = gtk::cairo::RectangleInt::new(x, y, box_w.min(width), box_h.min(height));
    gdk_window.input_shape_combine_region(&gtk::cairo::Region::create_rectangle(&rect), 0, 0);
}

#[cfg(not(target_os = "linux"))]
async fn cover(_app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let _ = window.show();
    Ok(())
}
