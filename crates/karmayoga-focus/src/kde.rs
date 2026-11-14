//! KDE Plasma: a KWin script reports focus over D-Bus; idleness comes from
//! ext-idle-notify (Wayland) or the screen saver service (X11); the lock
//! from the screen saver service.
//!
//! # Focus, by a script inside KWin
//!
//! KWin keeps the active window to itself on Wayland and implements no
//! wlroots protocol, but it runs JavaScript it is handed over D-Bus, and a
//! script sees `workspace.activeWindow` and hears `windowActivated`. So
//! this watcher:
//!
//! 1. takes a well-known name of its own on the session bus,
//!    `app.karmayogi.Focus.pid<pid>` (an element of a bus name may not start
//!    with a digit, hence the `pid`), and serves [`Receiver`] there;
//! 2. writes [`SCRIPT`] to a file in `$XDG_RUNTIME_DIR`, with that name in
//!    it, and has KWin load it (`org.kde.kwin.Scripting.loadScript`) and run
//!    it (`run` on `/Scripting/Script<id>` in Plasma 6, `/<id>` in Plasma 5);
//! 3. hears `Activated` from the script on every focus change and every
//!    retitling of the focused window, and answers [`Watcher::focused`]
//!    from the last call;
//! 4. unloads the script when dropped.
//!
//! The script sends all four values as strings -- desktop file name,
//! resource class, caption, pid -- because KWin turns a JavaScript number
//! into whichever D-Bus type `QVariant` makes of it, and a method whose
//! signature does not match is refused. Which of the first two becomes the
//! app id is decided here ([`choose_app_id`]), where it can be tested.
//! Calls from anyone but KWin's own bus connection are ignored, so another
//! program on the session bus cannot make up what has focus.
//!
//! The plugin name is fixed ([`PLUGIN`]), so a script left behind by a run
//! that crashed is replaced rather than piling up -- which also means a
//! second instance of the app takes the script over from the first.
//!
//! # Idle: ext-idle-notify, or `GetSessionIdleTime`
//!
//! On Wayland, KWin implements `ext_idle_notifier_v1`, and that is used
//! (through [`crate::wlroots::Wayland`]): kscreenlocker answers
//! `org.freedesktop.ScreenSaver.GetSessionIdleTime` there with a
//! "not supported" error. On X11 that method is what is left, and **KDE
//! answers it in milliseconds** -- kscreenlocker returns
//! `KIdleTime::idleTime()` unchanged, which is milliseconds -- although the
//! freedesktop draft and GNOME's implementation say seconds. Milliseconds
//! are assumed here. Were that wrong, a desktop answering seconds would
//! never look idle (time away would be counted); the opposite mistake would
//! turn three seconds without input into fifty minutes and drop nearly all
//! real use, which is the worse of the two.
//!
//! # Locked
//!
//! `org.freedesktop.ScreenSaver.GetActive`, which kscreenlocker answers
//! with the lock screen's state; logind's `LockedHint` if it does not.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::message::Header;

use crate::linux::{Bus, CALL_TIMEOUT, DesktopNames, Logind, failed};
use crate::wlroots::{Want, Wayland};
use crate::{Backend, Error, Focused, Result, Watcher};

const KWIN: &str = "org.kde.KWin";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING: &str = "org.kde.kwin.Scripting";
const SCRIPT_INTERFACE: &str = "org.kde.kwin.Script";
/// The name KWin knows the script by.
const PLUGIN: &str = "karmayoga-focus";

/// Where [`Receiver`] is served, and the interface the script calls.
const OBJECT_PATH: &str = "/app/karmayogi/Focus";
const INTERFACE: &str = "app.karmayogi.Focus";

const SCREEN_SAVER: &str = "org.freedesktop.ScreenSaver";
const SCREEN_SAVER_PATH: &str = "/ScreenSaver";

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";

/// The KWin script. `@SERVICE@`, `@PATH@` and `@INTERFACE@` are filled in
/// by [`script_for`].
///
/// Plasma 6 names the signal `windowActivated` and the property
/// `activeWindow`; Plasma 5 named them `clientActivated` and
/// `activeClient`. Plasma 5 also hands a script `QByteArray` properties
/// (`resourceClass`, `desktopFileName`) as an `ArrayBuffer`, which `text`
/// decodes. The focused window's `captionChanged` is followed too, so a
/// browser switching tabs is heard without a focus change. Plain ES5
/// (`var`, no arrow functions): older Plasma 5 releases ran scripts in
/// QtScript, which knows nothing newer.
pub(crate) const SCRIPT: &str = r#"// Karma Yoga: tell the app which window has focus.
// Loaded over D-Bus by the app (karmayoga-focus) and unloaded when it stops.
var SERVICE = "@SERVICE@";
var PATH = "@PATH@";
var INTERFACE = "@INTERFACE@";

function text(value) {
    if (value === undefined || value === null) {
        return "";
    }
    if (typeof value === "string") {
        return value;
    }
    if (typeof ArrayBuffer !== "undefined" && value instanceof ArrayBuffer) {
        return String.fromCharCode.apply(null, new Uint8Array(value));
    }
    return String(value);
}

var watched = null;

function report() {
    var window = watched;
    callDBus(SERVICE, PATH, INTERFACE, "Activated",
        window ? text(window.desktopFileName) : "",
        window ? text(window.resourceClass) : "",
        window ? text(window.caption) : "",
        window ? text(window.pid) : "");
}

function activated(window) {
    if (watched && watched.captionChanged) {
        try { watched.captionChanged.disconnect(report); } catch (e) {}
    }
    watched = window || null;
    if (watched && watched.captionChanged) {
        try { watched.captionChanged.connect(report); } catch (e) {}
    }
    report();
}

if (workspace.windowActivated) {
    workspace.windowActivated.connect(activated);
    activated(workspace.activeWindow);
} else {
    workspace.clientActivated.connect(activated);
    activated(workspace.activeClient);
}
"#;

/// The script with this process's bus name in it.
fn script_for(service: &str) -> String {
    SCRIPT
        .replace("@SERVICE@", service)
        .replace("@PATH@", OBJECT_PATH)
        .replace("@INTERFACE@", INTERFACE)
}

/// This process's bus name.
fn service_name(pid: u32) -> String {
    format!("app.karmayogi.Focus.pid{pid}")
}

/// The app id for a KWin window: its desktop file name (without a
/// `.desktop` some programs leave on), else its resource class
/// lower-cased, as an X11 class would be. `None` when it has neither, or
/// when nothing has focus (the script sends empty strings).
fn choose_app_id(desktop_file: &str, resource_class: &str) -> Option<String> {
    let desktop_file = desktop_file.trim();
    let desktop_file = desktop_file.strip_suffix(".desktop").unwrap_or(desktop_file);
    if !desktop_file.is_empty() {
        return Some(desktop_file.to_owned());
    }
    let class = resource_class.trim();
    (!class.is_empty()).then(|| class.to_lowercase())
}

/// What the script last said.
#[derive(Debug, Default)]
struct Heard {
    /// KWin's unique name on the bus: the only sender listened to.
    kwin: Option<String>,
    /// The focused window, once the script has spoken; `None` inside means
    /// it said nothing has focus.
    window: Option<Reported>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    app_id: String,
    resource_class: String,
    title: String,
}

fn lock(heard: &Mutex<Heard>) -> MutexGuard<'_, Heard> {
    heard.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the script calls. Served on zbus's own thread.
struct Receiver {
    heard: Arc<Mutex<Heard>>,
}

#[zbus::interface(name = "app.karmayogi.Focus")]
impl Receiver {
    /// The focused window changed or was retitled. Empty strings when
    /// nothing has focus. `pid` is a decimal string, or empty.
    fn activated(
        &self,
        #[zbus(header)] header: Header<'_>,
        desktop_file: String,
        resource_class: String,
        caption: String,
        pid: String,
    ) {
        let mut heard = lock(&self.heard);
        let sender = header.sender().map(|sender| sender.as_str());
        if heard.kwin.as_deref().is_some_and(|kwin| Some(kwin) != sender) {
            tracing::debug!(?sender, "ignored a focus report not from KWin");
            return;
        }
        tracing::trace!(%desktop_file, %resource_class, %pid, "KWin reported focus");
        heard.window = choose_app_id(&desktop_file, &resource_class).map(|app_id| Reported {
            app_id,
            resource_class: resource_class.trim().to_owned(),
            title: caption,
        });
    }
}

pub(crate) struct Kde {
    /// The connection that owns the bus name and serves [`Receiver`]: KWin
    /// calls back on it for as long as it is open.
    _serving: Connection,
    /// Calls out, over a clone of the same connection.
    calls: Bus,
    heard: Arc<Mutex<Heard>>,
    script: PathBuf,
    /// ext-idle-notify on Wayland, when KWin offers it.
    wayland: Option<Wayland>,
    logind: Logind,
    names: DesktopNames,
}

impl Kde {
    /// Take the bus name, load the script into KWin and run it.
    /// [`Error::Unavailable`] when there is no session bus or no KWin that
    /// will run it.
    pub(crate) fn start() -> Result<Self> {
        let heard = Arc::new(Mutex::new(Heard::default()));
        let service = service_name(std::process::id());
        let serving = zbus::blocking::connection::Builder::session()
            .and_then(|builder| builder.method_timeout(CALL_TIMEOUT).name(service.as_str()))
            .and_then(|builder| {
                builder.serve_at(OBJECT_PATH, Receiver { heard: Arc::clone(&heard) })
            })
            .and_then(|builder| builder.build())
            .map_err(|e| {
                Error::Unavailable(format!(
                    "could not serve KWin's reports on the session bus: {e}"
                ))
            })?;
        let mut calls = Bus::session_with(serving.clone());

        let kwin: String = calls
            .call_for(DBUS, DBUS_PATH, DBUS, "GetNameOwner", &(KWIN,))
            .map_err(|e| Error::Unavailable(format!("KWin is not on the session bus: {e}")))?;
        lock(&heard).kwin = Some(kwin);

        let script = script_path(&service);
        std::fs::write(&script, script_for(&service)).map_err(|e| {
            failed(format!("could not write the KWin script to {}: {e}", script.display()))
        })?;
        if let Err(e) = load_script(&mut calls, &script) {
            let _ = std::fs::remove_file(&script);
            return Err(e);
        }

        let wayland = std::env::var_os("WAYLAND_DISPLAY")
            .filter(|display| !display.is_empty())
            .and_then(|_| match Wayland::connect(Want::IdleOnly) {
                Ok(wayland) => Some(wayland),
                Err(e) => {
                    tracing::info!(error = %e, "no Wayland idle notifications; asking the screen saver instead");
                    None
                }
            });

        Ok(Self {
            _serving: serving,
            calls,
            heard,
            script,
            wayland,
            logind: Logind::new(),
            names: DesktopNames::new(),
        })
    }
}

/// Where the script is written: the runtime directory, private to the user
/// and cleared at logout, else the temporary directory.
fn script_path(service: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("{service}.kwinscript.js"))
}

/// Load the script into KWin and start it, replacing one a previous run
/// left behind.
fn load_script(calls: &mut Bus, script: &Path) -> Result<()> {
    let unavailable =
        |e: Error| Error::Unavailable(format!("KWin would not load the focus script: {e}"));
    let loaded: bool = calls
        .call_for(KWIN, SCRIPTING_PATH, SCRIPTING, "isScriptLoaded", &(PLUGIN,))
        .unwrap_or(false);
    if loaded {
        let _: Result<bool> =
            calls.call_for(KWIN, SCRIPTING_PATH, SCRIPTING, "unloadScript", &(PLUGIN,));
    }
    let path = script.to_string_lossy().into_owned();
    let id: i32 = calls
        .call_for(KWIN, SCRIPTING_PATH, SCRIPTING, "loadScript", &(path.as_str(), PLUGIN))
        .map_err(unavailable)?;
    if id < 0 {
        return Err(Error::Unavailable(String::from("KWin refused the focus script")));
    }
    // Plasma 6, then Plasma 5.
    for object in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        if calls.call(KWIN, &object, SCRIPT_INTERFACE, "run", &()).is_ok() {
            return Ok(());
        }
    }
    // Neither object answered: `start` runs every loaded script that is
    // not running yet.
    calls.call(KWIN, SCRIPTING_PATH, SCRIPTING, "start", &()).map(drop).map_err(unavailable)
}

impl Drop for Kde {
    fn drop(&mut self) {
        let unloaded: Result<bool> =
            self.calls.call_for(KWIN, SCRIPTING_PATH, SCRIPTING, "unloadScript", &(PLUGIN,));
        if let Err(e) = unloaded {
            tracing::debug!(error = %e, "could not unload the KWin focus script");
        }
        let _ = std::fs::remove_file(&self.script);
    }
}

impl Watcher for Kde {
    fn backend(&self) -> Backend {
        Backend::Kde
    }

    fn focused(&mut self) -> Result<Option<Focused>> {
        let Some(window) = lock(&self.heard).window.clone() else {
            return Ok(None);
        };
        let name = self.names.name(&window.app_id).unwrap_or(window.resource_class);
        Ok(Some(Focused { app_id: window.app_id, name, title: window.title }))
    }

    fn idle(&mut self) -> Result<Duration> {
        if let Some(wayland) = &self.wayland
            && wayland.idle_offered()
        {
            match wayland.idle() {
                Ok(idle) => return Ok(idle),
                Err(e) => {
                    tracing::debug!(error = %e, "Wayland idle failed; asking the screen saver")
                }
            }
        }
        // Milliseconds from KDE; see the module docs.
        let millis: u32 = self.calls.call_for(
            SCREEN_SAVER,
            SCREEN_SAVER_PATH,
            SCREEN_SAVER,
            "GetSessionIdleTime",
            &(),
        )?;
        Ok(Duration::from_millis(u64::from(millis)))
    }

    fn locked(&mut self) -> Result<bool> {
        match self.calls.call_for::<_, bool>(
            SCREEN_SAVER,
            SCREEN_SAVER_PATH,
            SCREEN_SAVER,
            "GetActive",
            &(),
        ) {
            Ok(active) => Ok(active),
            Err(e) => {
                tracing::debug!(error = %e, "KDE's screen saver did not answer; asking logind");
                self.logind.locked()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_calls_this_process_back() {
        let script = script_for("app.karmayogi.Focus.pid42");
        assert!(script.contains(r#"var SERVICE = "app.karmayogi.Focus.pid42";"#));
        assert!(script.contains(r#"var PATH = "/app/karmayogi/Focus";"#));
        assert!(script.contains(r#"var INTERFACE = "app.karmayogi.Focus";"#));
        assert!(script.contains(r#"callDBus(SERVICE, PATH, INTERFACE, "Activated","#));
        assert!(!script.contains('@'), "a placeholder was left unfilled");
    }

    #[test]
    fn the_script_speaks_both_plasma_versions() {
        assert!(SCRIPT.contains("workspace.windowActivated.connect(activated)"));
        assert!(SCRIPT.contains("activated(workspace.activeWindow)"));
        assert!(SCRIPT.contains("workspace.clientActivated.connect(activated)"));
        assert!(SCRIPT.contains("activated(workspace.activeClient)"));
    }

    #[test]
    fn the_script_sends_four_strings() {
        // The receiver's signature is four strings; the call must match it.
        let call = SCRIPT
            .split("callDBus(")
            .nth(1)
            .and_then(|rest| rest.split(");").next())
            .unwrap_or_default();
        assert_eq!(
            call.split(',').count(),
            4 + 4,
            "service, path, interface, method, then four values"
        );
        assert_eq!(call.matches("text(").count(), 4);
    }

    #[test]
    fn the_interface_constant_matches_the_served_one() {
        // `#[zbus::interface(name = ...)]` takes a literal; keep the two in step.
        assert_eq!(INTERFACE, "app.karmayogi.Focus");
    }

    #[test]
    fn the_bus_name_never_starts_an_element_with_a_digit() {
        let name = service_name(1234);
        assert_eq!(name, "app.karmayogi.Focus.pid1234");
        assert!(name.split('.').all(|element| !element.starts_with(|c: char| c.is_ascii_digit())));
    }

    #[test]
    fn the_desktop_file_name_wins_over_the_resource_class() {
        assert_eq!(choose_app_id("org.kde.dolphin", "dolphin").as_deref(), Some("org.kde.dolphin"));
        assert_eq!(choose_app_id("firefox.desktop", "firefox").as_deref(), Some("firefox"));
    }

    #[test]
    fn the_resource_class_is_the_fallback_lower_cased() {
        assert_eq!(choose_app_id("", "Steam").as_deref(), Some("steam"));
        assert_eq!(choose_app_id("  ", " XTerm ").as_deref(), Some("xterm"));
    }

    #[test]
    fn nothing_focused_is_no_app() {
        assert_eq!(choose_app_id("", ""), None);
    }
}
