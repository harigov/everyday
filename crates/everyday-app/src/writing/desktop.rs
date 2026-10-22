//! Writing help everywhere but Wayland: X11, Windows and macOS.
//!
//! All three let an application find the pointer, place a window and press
//! keys in another application's window, so this is the plain version: a
//! small window opened beside the pointer, and Ctrl+V (Cmd+V on macOS)
//! pressed into the application the text came from, with the clipboard put
//! back afterwards.
//!
//! Reading the selection differs. On X11 the PRIMARY selection already holds
//! it, and is read without pressing anything. Windows and macOS have no such
//! thing, so Ctrl+C is pressed instead -- after letting go of the modifiers
//! the shortcut itself is still holding down -- and the clipboard put back
//! once the copy is read.
//!
//! macOS asks once, in System Settings → Privacy & Security →
//! Accessibility, before it lets an application press keys anywhere.

use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tauri::{AppHandle, LogicalSize, PhysicalPosition, WebviewWindow};

use super::{Captured, Mode, desktop_size, on_main};

/// The modifier a copy and a paste are pressed with.
#[cfg(target_os = "macos")]
const COMMAND: Key = Key::Meta;
#[cfg(not(target_os = "macos"))]
const COMMAND: Key = Key::Control;

/// Press `key` with the command modifier, having first let go of every
/// modifier the shortcut may still be holding.
fn press_with_command(key: char) -> Result<(), String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    for held in [Key::Shift, Key::Control, Key::Alt, Key::Meta] {
        let _ = enigo.key(held, Direction::Release);
    }
    enigo.key(COMMAND, Direction::Press).map_err(|e| e.to_string())?;
    let typed = enigo.key(Key::Unicode(key), Direction::Click).map_err(|e| e.to_string());
    let _ = enigo.key(COMMAND, Direction::Release);
    typed
}

// ---- the clipboard ----------------------------------------------------------
//
// What was on the clipboard is put back after the answer has been pasted --
// text or an image. Anything else (files, say) cannot be read back, and is
// replaced: on X11 by the answer, on Windows and macOS already by the copy
// that read the selection, which no application can avoid.

/// What was on the clipboard, kept to be put back.
pub(crate) enum Saved {
    Text(String),
    /// An image, as GTK hands it over: PNG.
    Png(Vec<u8>),
    /// An image, as `arboard` hands it over on Windows and macOS.
    Rgba(arboard::ImageData<'static>),
    /// Empty, or nothing that can be read back.
    Nothing,
}

impl Saved {
    fn restorable(&self) -> bool {
        !matches!(self, Saved::Nothing)
    }
}

#[cfg(target_os = "linux")]
mod clipboard {
    use super::{Saved, on_main};
    use crate::writing::read_selection;
    use tauri::AppHandle;

    /// The text selected right now, wherever it is.
    pub async fn selection(app: &AppHandle) -> Option<String> {
        read_selection(app, true).await.ok().flatten()
    }

    pub async fn save(app: &AppHandle) -> Saved {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let asked = app.run_on_main_thread(move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            clipboard.request_text(move |clipboard, text| match text {
                Some(text) => {
                    let _ = tx.send(Saved::Text(text.to_string()));
                }
                None => clipboard.request_image(move |_, image| {
                    let png = image.and_then(|p| p.save_to_bufferv("png", &[]).ok());
                    let _ = tx.send(png.map(Saved::Png).unwrap_or(Saved::Nothing));
                }),
            });
        });
        if asked.is_err() {
            return Saved::Nothing;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx).await {
            Ok(Ok(saved)) => saved,
            _ => Saved::Nothing,
        }
    }

    pub async fn set_text(app: &AppHandle, text: String) -> Result<(), String> {
        on_main(app, move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            clipboard.set_text(&text);
            clipboard.store();
        })
        .await
    }

    pub async fn restore(app: &AppHandle, saved: Saved) {
        let _ = on_main(app, move || {
            use gtk::gdk_pixbuf::prelude::*;
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            match saved {
                Saved::Text(text) => clipboard.set_text(&text),
                Saved::Png(png) => {
                    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
                    if loader.write(&png).is_ok()
                        && loader.close().is_ok()
                        && let Some(pixbuf) = loader.pixbuf()
                    {
                        clipboard.set_image(&pixbuf);
                    }
                }
                // Only ever made by `arboard`, which Linux does not use.
                Saved::Rgba(_) | Saved::Nothing => return,
            }
            clipboard.store();
        })
        .await;
    }
}

/// The clipboard through `arboard`, for Windows and macOS. Compiled on Linux
/// too, unused there, so that it is at least built where it is developed.
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod portable {
    use super::Saved;
    use std::time::{Duration, Instant};
    use tauri::AppHandle;

    async fn with<T: Send + 'static>(
        f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error> + Send + 'static,
    ) -> Result<T, String> {
        tauri::async_runtime::spawn_blocking(move || {
            let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
            f(&mut clipboard).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn text() -> Option<String> {
        with(|c| c.get_text()).await.ok().filter(|t| !t.is_empty())
    }

    pub async fn save(_app: &AppHandle) -> Saved {
        if let Some(text) = text().await {
            return Saved::Text(text);
        }
        match with(|c| c.get_image()).await {
            Ok(image) => Saved::Rgba(image.to_owned_img()),
            Err(_) => Saved::Nothing,
        }
    }

    pub async fn set_text(_app: &AppHandle, text: String) -> Result<(), String> {
        with(move |c| c.set_text(text)).await
    }

    pub async fn restore(_app: &AppHandle, saved: Saved) {
        let _ = match saved {
            Saved::Text(text) => with(move |c| c.set_text(text)).await,
            Saved::Rgba(image) => with(move |c| c.set_image(image)).await,
            // Only ever made by GTK's clipboard, which only Linux uses.
            Saved::Png(_) | Saved::Nothing => Ok(()),
        };
    }

    /// The text selected right now: copied with a pressed Ctrl+C (Cmd+C),
    /// with the clipboard as it was put back afterwards.
    pub async fn selection(app: &AppHandle) -> Option<String> {
        let before = save(app).await;
        // A marker the copy has to replace, so a copy that does nothing --
        // no selection, or an application that ignores the key -- reads as
        // nothing rather than as whatever was on the clipboard already.
        // Only where what it replaces can be put back afterwards: never
        // clear what cannot be restored.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let marker = format!("everyday-writing-help-{stamp}");
        if before.restorable() {
            let _ = set_text(app, marker.clone()).await;
        }
        let pressed = super::on_main(app, || super::press_with_command('c')).await;
        let mut copied = None;
        if matches!(pressed, Ok(Ok(()))) {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(600) {
                tokio::time::sleep(Duration::from_millis(30)).await;
                if let Some(text) = text().await.filter(|t| *t != marker) {
                    copied = Some(text);
                    break;
                }
            }
        }
        restore(app, before).await;
        copied
    }
}

#[cfg(not(target_os = "linux"))]
use portable as clipboard;

// ---- open and paste ------------------------------------------------------------

pub(super) async fn open(app: &AppHandle, window: &WebviewWindow) -> Result<Captured, String> {
    let pointer = app.cursor_position().ok();
    let text = clipboard::selection(app).await;

    let (w, h) = desktop_size();
    let _ = window.set_size(LogicalSize::new(w, h));
    if let Some(at) = pointer {
        let _ = window.set_position(place(app, window, at, (w, h)));
    } else {
        let _ = window.center();
    }
    show_focused(app, window).await?;
    // The page, as well as the window, or keys never reach it.
    let _ = AsRef::<tauri::Webview>::as_ref(window).set_focus();
    Ok(Captured { text, app: None, mode: Mode::Window, at: None })
}

/// Show the popup and give it the keyboard.
///
/// On X11, a window that asks for focus without proof of a recent user
/// action is refused it by focus-stealing prevention -- the popup appears
/// and the keys go on to the application behind it, so a "2" meant for the
/// popup replaces the selection with a 2. The X server's own current time is
/// that proof: it is later than anything the person did in the other
/// application.
async fn show_focused(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let shown = window.clone();
        on_main(app, move || -> Result<(), String> {
            use gtk::glib::Cast;
            use gtk::prelude::*;
            let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
            gtk_window.show_all();
            match gtk_window.window().and_then(|w| w.downcast::<gdkx11::X11Window>().ok()) {
                Some(x11) => {
                    gtk_window.present_with_time(gdkx11::functions::x11_get_server_time(&x11))
                }
                None => gtk_window.present(),
            }
            Ok(())
        })
        .await??;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        window.show().map_err(|e| e.to_string())?;
        let _ = window.set_focus();
    }
    Ok(())
}

/// Just below and right of the pointer, kept on the screen the pointer is
/// on.
fn place(
    app: &AppHandle,
    window: &WebviewWindow,
    at: PhysicalPosition<f64>,
    (w, h): (f64, f64),
) -> PhysicalPosition<i32> {
    let monitor = app.monitor_from_point(at.x, at.y).ok().flatten();
    let scale = monitor
        .as_ref()
        .map(|m| m.scale_factor())
        .or_else(|| window.scale_factor().ok())
        .unwrap_or(1.0);
    let (pw, ph) = ((w * scale) as i32, (h * scale) as i32);
    let gap = (12.0 * scale) as i32;
    let (mut x, mut y) = (at.x as i32 + gap, at.y as i32 + gap);
    if let Some(m) = monitor {
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        if x + pw > mx + mw {
            x = (at.x as i32 - gap - pw).max(mx);
        }
        if y + ph > my + mh {
            y = (at.y as i32 - gap - ph).max(my);
        }
    }
    PhysicalPosition::new(x, y)
}

pub(super) async fn paste(
    app: &AppHandle,
    window: &WebviewWindow,
    text: &str,
) -> Result<(), String> {
    let before = clipboard::save(app).await;
    clipboard::set_text(app, text.to_string()).await?;

    // Give the keyboard back to the application the text is in. On macOS
    // hiding one window would hand it to the main window instead, so the
    // whole application steps back.
    let _ = window.hide();
    #[cfg(target_os = "macos")]
    let _ = app.hide();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let pressed = on_main(app, || press_with_command('v')).await?;
    // The application reads the clipboard when it handles the key, which is
    // not necessarily straight away.
    tokio::time::sleep(Duration::from_millis(350)).await;
    if pressed.is_ok() {
        clipboard::restore(app, before).await;
    }
    pressed
}
