//! Writing help over text selected in any application.
//!
//! Select some text anywhere, press the writing shortcut, and a small popup
//! offers to improve it, correct its grammar or make it concise. The choice
//! is sent to the medium model -- `rewrite_selection`, in
//! `everyday_service::domains::writing`, which also keeps a record of every
//! use -- and the answer is pasted over the selection. See
//! `docs/plans/writing-tools.md`.
//!
//! This module is the half that is about the desktop, and the desktop is
//! where it differs:
//!
//! * **Wayland** ([`wayland`]): no application may learn where the pointer
//!   is, place a window, read another's selection while unfocused or press a
//!   key in another's window. So the popup is drawn inside a transparent
//!   window covering the desktop, which learns the pointer's position the
//!   moment it appears under it and, holding the keyboard, may read the
//!   PRIMARY selection -- the text that is selected right now, in whatever
//!   application. The paste goes through the RemoteDesktop portal.
//! * **Everywhere else** ([`desktop`]): an ordinary small window opened at
//!   the pointer; the selection read from PRIMARY on X11 or copied with a
//!   pressed Ctrl+C on Windows and macOS; the answer pasted with a pressed
//!   Ctrl+V and the clipboard put back as it was.
//!
//! # Why the popup is its own window
//!
//! Because the main window may be closed, minimised or on another
//! workspace, and the popup has to appear over the application the text is
//! in. It is created the first time it is wanted and hidden, not destroyed,
//! afterwards, so the second press is as quick as a window can be shown.
//! It shares nothing with the main window but the shell: the rewrite is the
//! same `call` command the main window uses.

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
mod desktop;
#[cfg(target_os = "linux")]
mod wayland;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use everyday_service::ctx::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::Shortcut;

use crate::hotkey::{self, Binding, Hotkeys, KeyStatus};
use crate::state::AppState;

/// The popup window's label.
pub const LABEL: &str = "writing";

/// Tells the popup there is a new selection to offer help with. Its other
/// half is `ui/src/writing/main.ts`.
pub const OPEN_EVENT: &str = "everyday://writing/open";

/// Tells the popup to go back to nothing, because the shortcut was pressed
/// again while it was open.
pub const CLOSE_EVENT: &str = "everyday://writing/close";

// ---- the per-machine setting -------------------------------------------------

/// Writing help's setting on this machine. Not in the vault: the shortcut is
/// a claim on this desktop's keyboard, and a vault opened on two machines
/// may want two different ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    pub shortcut: String,
    /// The RemoteDesktop portal's restore token: what lets a paste happen
    /// without the desktop asking again. Wayland only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_token: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true, shortcut: hotkey::DEFAULT_WRITING.into(), restore_token: None }
    }
}

const FILE: &str = "writing.json";

fn config_path() -> PathBuf {
    everyday_vault::config_dir().join(FILE)
}

pub fn load_config() -> Config {
    std::fs::read(config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    everyday_core::fsutil::write_atomic(&path, &bytes, "writing").map_err(|e| e.to_string())?;
    // The restore token is a standing grant to type into other windows, so
    // the file is the owner's alone.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// The shortcut to claim at startup, or `None` when writing help is off or
/// its setting no longer parses.
pub fn configured_shortcut() -> Option<Shortcut> {
    let config = load_config();
    if !config.enabled {
        return None;
    }
    hotkey::parse(&config.shortcut)
        .map_err(|e| tracing::warn!(error = %e, "writing help's shortcut does not parse"))
        .ok()
}

// ---- the main thread ------------------------------------------------------------

/// Run `f` on the main thread -- where every GTK call must be made, and on
/// macOS anything that posts key events -- and hand back what it returns.
pub(crate) async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "the main thread went away".to_string())
}

/// The text in the PRIMARY selection (`primary`) or on the clipboard,
/// through GTK, on either display server. An error only when it could not be
/// read in time; `None` when there is no text there.
///
/// `primary` is a flag rather than the atom itself because an atom cannot
/// cross threads.
#[cfg(target_os = "linux")]
pub(crate) async fn read_selection(
    app: &AppHandle,
    primary: bool,
) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let atom =
            if primary { gtk::gdk::SELECTION_PRIMARY } else { gtk::gdk::SELECTION_CLIPBOARD };
        gtk::Clipboard::get(&atom).request_text(move |_, text| {
            let _ = tx.send(text.map(str::to_string));
        });
    })
    .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(2), rx)
        .await
        .map_err(|_| "the selection could not be read in time".to_string())?
        .map_err(|_| "the main thread went away".to_string())
}

// ---- which kind of desktop ----------------------------------------------------

static WAYLAND: OnceLock<bool> = OnceLock::new();

/// Find out, once, whether the webview is on Wayland. Must be called on the
/// main thread -- `setup` is -- because GDK is asked.
pub fn detect(_app: &AppHandle) {
    WAYLAND.get_or_init(|| {
        #[cfg(target_os = "linux")]
        {
            use gtk::glib::object::ObjectExt;
            gtk::gdk::Display::default().is_some_and(|d| d.type_().name() == "GdkWaylandDisplay")
        }
        #[cfg(not(target_os = "linux"))]
        false
    });
}

/// Whether the webview is on Wayland -- and so whether keys go through the
/// portal and the popup is drawn on an overlay.
pub fn wayland_session(app: &AppHandle) -> bool {
    detect(app);
    *WAYLAND.get().unwrap_or(&false)
}

// ---- the popup's state -----------------------------------------------------

/// How the popup is drawn.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Inside a transparent window covering the screen, at `x`/`y` in it.
    Overlay,
    /// As the whole of a small window already placed at the pointer.
    Window,
}

/// What the popup is offering help with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    /// Which press of the shortcut this is, so the popup can tell a new
    /// selection from the one it already has.
    pub seq: u64,
    pub text: String,
    /// The application it was selected in, where the desktop says.
    pub app: Option<String>,
    pub mode: Mode,
    /// Where to draw the popup, in CSS pixels of the overlay. `None` when
    /// the pointer could not be found -- the popup centres itself.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Why there is nothing to offer help with, when there is not.
    pub problem: Option<String>,
}

/// What a desktop half captured.
pub(crate) struct Captured {
    text: Option<String>,
    app: Option<String>,
    mode: Mode,
    at: Option<(f64, f64)>,
}

/// The popup's state, for the life of the process.
#[derive(Default)]
pub struct Writing {
    current: Mutex<Option<Selection>>,
    seq: AtomicU64,
    /// A press is being turned into a popup right now. A second press while
    /// one is would capture the popup itself.
    opening: AtomicBool,
}

/// The shortcut was pressed. `token` is the activation token the desktop
/// sent with it, on Wayland.
pub fn trigger(app: &AppHandle, token: Option<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let writing = app.state::<Writing>();
        if writing.opening.swap(true, Ordering::SeqCst) {
            return;
        }
        let shown = app.get_webview_window(LABEL).is_some_and(|w| w.is_visible().unwrap_or(false));
        let result = if shown {
            // The same key again closes it, the way it opened.
            dismiss(&app, None).await.map_err(|e| e.message)
        } else {
            open(&app, token).await
        };
        writing.opening.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            tracing::warn!(error = %e, "writing help could not open");
        }
    });
}

/// The popup, created hidden the first time it is wanted.
fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("writing.html".into()))
        .title("Writing help")
        .inner_size(desktop_size().0, desktop_size().1)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .focused(true)
        .build()
        .map_err(|e| e.to_string())
}

/// The popup's size where it is a window of its own, in logical pixels:
/// three choices, a line of status and the margins around them.
pub(crate) fn desktop_size() -> (f64, f64) {
    (320.0, 212.0)
}

async fn open(app: &AppHandle, token: Option<String>) -> Result<(), String> {
    let window = window(app)?;
    let result = show(app, &window, token).await;
    if result.is_err() {
        // Shown before whatever failed: an invisible window left covering
        // the desktop would swallow every click on it.
        hide(app);
    }
    result
}

async fn show(
    app: &AppHandle,
    window: &WebviewWindow,
    token: Option<String>,
) -> Result<(), String> {
    let window = window.clone();
    #[cfg(target_os = "linux")]
    let captured = if wayland_session(app) {
        wayland::open(app, &window, token).await?
    } else {
        desktop::open(app, &window).await?
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let captured = {
        let _ = token;
        desktop::open(app, &window).await?
    };
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    let captured: Captured = {
        let _ = (token, &window);
        return Err("writing help is not available on this platform".into());
    };

    let writing = app.state::<Writing>();
    let text = captured.text.filter(|t| !t.trim().is_empty());
    let problem = match &text {
        None => Some("Select some text first, then press the shortcut again.".to_string()),
        Some(t) => everyday_core::writing::validate_text(t).err().map(|e| {
            let msg = e.to_string();
            // `Error::Invalid`'s Display carries a prefix of its own.
            msg.strip_prefix("invalid data: ").unwrap_or(&msg).to_string()
        }),
    };
    let selection = Selection {
        seq: writing.seq.fetch_add(1, Ordering::SeqCst) + 1,
        text: text.unwrap_or_default(),
        app: captured.app,
        mode: captured.mode,
        x: captured.at.map(|(x, _)| x),
        y: captured.at.map(|(_, y)| y),
        problem,
    };
    *writing.current.lock().expect("writing lock") = Some(selection.clone());
    window.emit_to(LABEL, OPEN_EVENT, &selection).map_err(|e| e.to_string())
}

/// Hide the popup.
fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        #[cfg(target_os = "linux")]
        if wayland_session(app) {
            wayland::hide(app, &window);
            return;
        }
        let _ = window.hide();
    }
}

async fn settle(app: &AppHandle, assist_id: Option<&str>, outcome: &str) {
    let Some(id) = assist_id else { return };
    let session = app.state::<AppState>().session().as_session();
    let args = json!({ "id": id, "outcome": outcome });
    if let Err(e) = session.call(Ctx::local(), "settle_writing_assist", args).await {
        tracing::debug!(error = %e, outcome, "could not record what became of an assist");
    }
}

async fn dismiss(app: &AppHandle, assist_id: Option<&str>) -> CommandResult<()> {
    tracing::debug!(answered = assist_id.is_some(), "writing help dismissed");
    *app.state::<Writing>().current.lock().expect("writing lock") = None;
    let _ = app.emit_to(LABEL, CLOSE_EVENT, ());
    hide(app);
    settle(app, assist_id, "cancelled").await;
    Ok(())
}

// ---- commands -------------------------------------------------------------------

/// What the popup is offering help with right now, for a popup that has only
/// just loaded and missed the event.
#[tauri::command]
pub async fn writing_selection(app: AppHandle) -> CommandResult<Option<Selection>> {
    Ok(app.state::<Writing>().current.lock().expect("writing lock").clone())
}

/// What became of an answer, for the popup to show.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    /// `replaced`, `copied`, or `cancelled` when the popup had lost the
    /// keyboard and nothing was pasted.
    pub outcome: &'static str,
    /// When it was only copied: why it could not be pasted.
    pub message: Option<String>,
}

/// Put `text` -- the model's answer -- over the selection it was asked about,
/// and record that it was.
///
/// Where it cannot be pasted it is left on the clipboard instead, and the
/// person is told so.
#[tauri::command]
pub async fn writing_apply(
    app: AppHandle,
    assist_id: Option<String>,
    text: String,
) -> CommandResult<Applied> {
    let window = window(&app).map_err(|e| CommandError::new("writing", e))?;
    *app.state::<Writing>().current.lock().expect("writing lock") = None;
    // The paste goes to whichever window the popup hands the keyboard back
    // to, and on Wayland the answer can only be put on the clipboard while
    // the popup has it. If somebody went elsewhere while the model was
    // thinking, that is not the window the text came from: nothing is
    // pasted anywhere.
    if !window.is_focused().unwrap_or(false) {
        hide(&app);
        settle(&app, assist_id.as_deref(), "cancelled").await;
        return Ok(Applied {
            outcome: "cancelled",
            message: Some("you moved to another window, so nothing was pasted".into()),
        });
    }
    #[cfg(target_os = "linux")]
    let pasted = if wayland_session(&app) {
        wayland::paste(&app, &window, &text).await
    } else {
        desktop::paste(&app, &window, &text).await
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let pasted = desktop::paste(&app, &window, &text).await;
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    let pasted: Result<(), String> = {
        let _ = (&window, &text);
        Err("writing help is not available on this platform".into())
    };

    let applied = match pasted {
        Ok(()) => Applied { outcome: "replaced", message: None },
        Err(why) => {
            tracing::info!(error = %why, "writing help could not paste; the answer is on the clipboard");
            notify_copied(&app, &why);
            Applied { outcome: "copied", message: Some(why) }
        }
    };
    hide(&app);
    settle(&app, assist_id.as_deref(), applied.outcome).await;
    Ok(applied)
}

/// The popup was dismissed -- Escape, a click outside it, or the shortcut
/// again -- with or without an answer.
#[tauri::command]
pub async fn writing_dismiss(app: AppHandle, assist_id: Option<String>) -> CommandResult<()> {
    dismiss(&app, assist_id.as_deref()).await
}

/// Close the popup and bring the main window forward -- for a popup that
/// cannot help because the vault is locked or no model is set up.
#[tauri::command]
pub async fn writing_open_main(app: AppHandle) -> CommandResult<()> {
    dismiss(&app, None).await?;
    crate::tray::raise_window(&app);
    Ok(())
}

fn notify_copied(app: &AppHandle, why: &str) {
    use tauri_plugin_notification::NotificationExt;
    let paste = if cfg!(target_os = "macos") { "\u{2318}V" } else { "Ctrl+V" };
    let _ = app
        .notification()
        .builder()
        .title("Rewritten text copied")
        .body(format!("It could not be pasted ({why}). Paste it with {paste}."))
        .show();
}

/// What the settings pane draws for writing help.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritingSettings {
    pub enabled: bool,
    /// The combination as chosen here -- what is offered to the desktop.
    pub shortcut: String,
    pub key: KeyStatus,
    /// `wayland`, `x11`, `windows` or `macos`: what the pane says about how
    /// the selection is read and the answer pasted.
    pub platform: &'static str,
}

fn platform(app: &AppHandle) -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if wayland_session(app) {
        "wayland"
    } else {
        "x11"
    }
}

fn settings(app: &AppHandle) -> WritingSettings {
    let config = load_config();
    WritingSettings {
        enabled: config.enabled,
        shortcut: config.shortcut,
        key: app.state::<Hotkeys>().status(app, Binding::Writing),
        platform: platform(app),
    }
}

#[tauri::command]
pub async fn writing_settings(app: AppHandle) -> CommandResult<WritingSettings> {
    Ok(settings(&app))
}

/// Switch writing help on or off, or give it a different shortcut.
#[tauri::command]
pub async fn set_writing_settings(
    app: AppHandle,
    enabled: bool,
    shortcut: Option<String>,
) -> CommandResult<WritingSettings> {
    let mut config = load_config();
    if let Some(shortcut) = shortcut {
        hotkey::parse(&shortcut).map_err(|e| CommandError::new("invalid", e))?;
        config.shortcut = shortcut.trim().to_string();
    }
    config.enabled = enabled;
    let claim = if enabled {
        Some(hotkey::parse(&config.shortcut).map_err(|e| CommandError::new("invalid", e))?)
    } else {
        None
    };
    save_config(&config).map_err(|e| CommandError::new("writing", e))?;
    let hotkeys = app.state::<Hotkeys>();
    let answer = hotkeys.answer();
    hotkeys.set_writing(&app, claim)?;
    answer.await;
    if !enabled {
        hide(&app);
    }
    Ok(settings(&app))
}

/// Remember the RemoteDesktop portal's latest restore token.
#[cfg(target_os = "linux")]
pub(crate) fn remember_restore_token(token: Option<String>) {
    let mut config = load_config();
    if config.restore_token == token {
        return;
    }
    config.restore_token = token;
    if let Err(e) = save_config(&config) {
        tracing::warn!(error = %e, "could not keep the paste permission's restore token");
    }
}
