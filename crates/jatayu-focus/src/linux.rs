//! What every Linux backend shares: choosing the backend from the
//! environment, the D-Bus plumbing, logind's lock state, and turning an
//! application id into the name its desktop file gives it.
//!
//! # Choosing a backend
//!
//! Linux has no one answer to "which window has focus"; each desktop keeps
//! it to itself in its own way. [`plan`] reads the environment the session
//! hands every program and lists, in order, the backends worth trying;
//! [`detect`] tries them and keeps the first that starts. The decision is a
//! pure function of the environment so that it can be tested without a
//! desktop of each kind to hand:
//!
//! - **GNOME on Wayland**: only [`gnome`](crate::gnome) -- Mutter tells
//!   nothing to ordinary programs, the Shell extension reports focus, and
//!   falling back to XWayland here would quietly count only the X11 apps.
//! - **KDE Plasma**: the KWin script ([`kde`](crate::kde)), then XWayland or
//!   X11 if KWin will not load it.
//! - **Any other Wayland compositor**: the wlroots foreign-toplevel protocol
//!   ([`wlroots`](crate::wlroots)), then XWayland as a last resort -- which
//!   sees only the windows of X11 programs, so the watcher says so through
//!   [`Watcher::limitation`].
//! - **An X11 session**: [`x11`](crate::x11).
//!
//! # Lock state
//!
//! logind's `LockedHint` on the session object is the one lock signal every
//! desktop is expected to set (GNOME, KDE, and the `swaylock` family through
//! `loginctl lock-session`/their own logind integration). It lives on the
//! system bus; [`Logind`] finds this process's session once and reads the
//! property on each call.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::export::serde::Serialize;
use zbus::export::serde::de::DeserializeOwned;
use zbus::message::Message;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Type};

use crate::{Error, Result, Unsupported, Watcher};

/// How long any one D-Bus call may take before it counts as failed. The
/// shell asks every few seconds from a thread of its own; a desktop service
/// that has hung must cost it a missed sample, not a stuck thread.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// One backend worth trying, in the order [`plan`] lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attempt {
    Gnome,
    Kde,
    Wlroots,
    /// An X11 session's own server.
    X11,
    /// The X server a Wayland compositor runs for older programs: it sees
    /// only their windows, so the watcher is partial.
    XWayland,
}

/// Which backends to try, best first, for a session with this environment.
/// Empty when there is no display at all (a text console, a service).
///
/// `XDG_SESSION_TYPE` decides between Wayland and X11 when it says one or
/// the other; without it, a `WAYLAND_DISPLAY` means Wayland and a `DISPLAY`
/// alone means X11. `XDG_CURRENT_DESKTOP` is a colon-separated list
/// (`ubuntu:GNOME`, `KDE`, `sway`), matched case-insensitively.
pub(crate) fn plan(env: &HashMap<String, String>) -> Vec<Attempt> {
    let get = |key: &str| env.get(key).map(|value| value.trim()).filter(|value| !value.is_empty());
    let has_wayland = get("WAYLAND_DISPLAY").is_some();
    let has_x = get("DISPLAY").is_some();
    let wayland = match get("XDG_SESSION_TYPE").map(str::to_ascii_lowercase).as_deref() {
        Some("wayland") => true,
        Some("x11") => false,
        _ => has_wayland,
    };
    let desktops: Vec<String> = get("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(|desktop| desktop.trim().to_ascii_lowercase())
        .collect();
    let is = |name: &str| desktops.iter().any(|desktop| desktop == name);

    let mut attempts = Vec::new();
    if wayland {
        if is("gnome") {
            attempts.push(Attempt::Gnome);
            return attempts;
        }
        // KWin implements no wlroots protocol, so on Plasma the script is
        // the only full answer.
        attempts.push(if is("kde") { Attempt::Kde } else { Attempt::Wlroots });
        if has_x {
            attempts.push(Attempt::XWayland);
        }
    } else if has_x || get("XDG_SESSION_TYPE").is_some_and(|kind| kind.eq_ignore_ascii_case("x11"))
    {
        if is("kde") {
            attempts.push(Attempt::Kde);
        }
        attempts.push(Attempt::X11);
    }
    attempts
}

/// What [`Watcher::limitation`] says of an X11 watcher on XWayland.
pub(crate) const XWAYLAND_LIMITATION: &str = "Watching through XWayland: only windows of X11 programs are seen, so time in native Wayland apps is not counted";

/// The watcher for this Linux desktop. See the module docs for the order.
pub(crate) fn detect() -> Box<dyn Watcher> {
    // `vars_os`, not `vars`: one variable that is not UTF-8 would make
    // `vars` panic, and nothing here needs such a variable.
    let env: HashMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let attempts = plan(&env);
    let mut failures = Vec::new();
    for attempt in attempts {
        let started: Result<Box<dyn Watcher>> = match attempt {
            Attempt::Gnome => Ok(Box::new(crate::gnome::Gnome::new())),
            Attempt::Kde => {
                crate::kde::Kde::start().map(|watcher| Box::new(watcher) as Box<dyn Watcher>)
            }
            Attempt::Wlroots => crate::wlroots::Wlroots::connect()
                .map(|watcher| Box::new(watcher) as Box<dyn Watcher>),
            Attempt::X11 => {
                crate::x11::X11::connect(None).map(|watcher| Box::new(watcher) as Box<dyn Watcher>)
            }
            Attempt::XWayland => crate::x11::X11::connect(Some(XWAYLAND_LIMITATION))
                .map(|watcher| Box::new(watcher) as Box<dyn Watcher>),
        };
        match started {
            Ok(watcher) => {
                tracing::info!(backend = watcher.backend().as_str(), ?attempt, "watching focus");
                return watcher;
            }
            Err(e) => {
                tracing::info!(?attempt, error = %e, "focus backend did not start");
                failures.push(format!("{attempt:?}: {e}"));
            }
        }
    }
    let why = if failures.is_empty() {
        String::from("no Wayland or X11 display to watch")
    } else {
        format!("no way to watch this desktop ({})", failures.join("; "))
    };
    Box::new(Unsupported(why))
}

/// A failed call, as the crate's error.
pub(crate) fn failed(e: impl std::fmt::Display) -> Error {
    Error::Failed(e.to_string())
}

/// Which bus a [`Bus`] talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusKind {
    Session,
    System,
}

/// A lazily opened, self-healing blocking D-Bus connection.
///
/// Opened on first use rather than up front, so a watcher can be built
/// before a bus it may never need is reachable; dropped after a failure
/// that was not the far end's answer, so a restarted bus (or a stale
/// socket after suspend) costs one failed sample rather than every sample
/// from then on.
pub(crate) struct Bus {
    kind: BusKind,
    connection: Option<Connection>,
}

impl Bus {
    pub(crate) fn session() -> Self {
        Self { kind: BusKind::Session, connection: None }
    }

    pub(crate) fn system() -> Self {
        Self { kind: BusKind::System, connection: None }
    }

    /// Calls over a session connection already open (a clone of it). If
    /// it fails, this lets go of its clone and opens a fresh one; the
    /// original is left to whoever owns it.
    pub(crate) fn session_with(connection: Connection) -> Self {
        Self { kind: BusKind::Session, connection: Some(connection) }
    }

    /// The open connection, opening it first if need be.
    pub(crate) fn connection(&mut self) -> Result<&Connection> {
        if self.connection.is_none() {
            self.connection = Some(open(self.kind)?);
        }
        self.connection
            .as_ref()
            .ok_or_else(|| Error::Unavailable(String::from("no D-Bus connection")))
    }

    /// Call `method` and hand back the reply.
    pub(crate) fn call<B>(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> Result<Message>
    where
        B: Serialize + DynamicType,
    {
        let connection = self.connection()?;
        match connection.call_method(Some(destination), path, Some(interface), method, body) {
            Ok(reply) => Ok(reply),
            Err(e) => {
                // An error reply means the bus is fine and the service said
                // no; anything else may mean the connection is gone.
                if !matches!(e, zbus::Error::MethodError(..) | zbus::Error::FDO(_)) {
                    self.connection = None;
                }
                Err(failed(format!("{destination} {method}: {e}")))
            }
        }
    }

    /// Call `method` and read its single return value.
    pub(crate) fn call_for<B, T>(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> Result<T>
    where
        B: Serialize + DynamicType,
        T: Type + DeserializeOwned,
    {
        let reply = self.call(destination, path, interface, method, body)?;
        read::<T>(&reply)
    }

    /// Read one property with `org.freedesktop.DBus.Properties.Get`.
    pub(crate) fn property<T>(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        name: &str,
    ) -> Result<T>
    where
        T: TryFrom<OwnedValue>,
        <T as TryFrom<OwnedValue>>::Error: std::fmt::Display,
    {
        let value: OwnedValue =
            self.call_for(destination, path, PROPERTIES, "Get", &(interface, name))?;
        T::try_from(value).map_err(|e| failed(format!("{interface}.{name}: {e}")))
    }
}

/// The reply's body as one value of type `T`.
pub(crate) fn read<T: Type + DeserializeOwned>(reply: &Message) -> Result<T> {
    let body = reply.body();
    body.deserialize::<T>().map_err(failed)
}

fn open(kind: BusKind) -> Result<Connection> {
    let builder = match kind {
        BusKind::Session => zbus::blocking::connection::Builder::session(),
        BusKind::System => zbus::blocking::connection::Builder::system(),
    };
    builder.and_then(|builder| builder.method_timeout(CALL_TIMEOUT).build()).map_err(|e| {
        let which = match kind {
            BusKind::Session => "session",
            BusKind::System => "system",
        };
        Error::Unavailable(format!("no D-Bus {which} bus: {e}"))
    })
}

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIND_SESSION: &str = "org.freedesktop.login1.Session";
/// logind resolves this path to the caller's own session -- or, for a
/// process outside any session (a user service), to the user's display
/// session. The last resort when neither the pid nor `XDG_SESSION_ID` names
/// one.
const LOGIND_AUTO_SESSION: &str = "/org/freedesktop/login1/session/auto";

/// logind's `LockedHint` for this process's session.
pub(crate) struct Logind {
    bus: Bus,
    /// The session object, found once. Forgotten after a failure, so a
    /// session that ended and was replaced is found again.
    session: Option<String>,
}

impl Logind {
    pub(crate) fn new() -> Self {
        Self { bus: Bus::system(), session: None }
    }

    /// Is the session locked?
    pub(crate) fn locked(&mut self) -> Result<bool> {
        let session = match &self.session {
            Some(session) => session.clone(),
            None => {
                let session = self.find_session();
                self.session = Some(session.clone());
                session
            }
        };
        let locked = self.bus.property::<bool>(LOGIND, &session, LOGIND_SESSION, "LockedHint");
        if locked.is_err() {
            self.session = None;
        }
        locked
    }

    /// This process's session object: by pid, then by `XDG_SESSION_ID`,
    /// then logind's `auto`.
    fn find_session(&mut self) -> String {
        let by_pid: Result<OwnedObjectPath> = self.bus.call_for(
            LOGIND,
            LOGIND_PATH,
            LOGIND_MANAGER,
            "GetSessionByPID",
            &(std::process::id(),),
        );
        if let Ok(path) = by_pid {
            return path.as_str().to_owned();
        }
        if let Some(id) = std::env::var("XDG_SESSION_ID").ok().filter(|id| !id.trim().is_empty()) {
            let by_id: Result<OwnedObjectPath> =
                self.bus.call_for(LOGIND, LOGIND_PATH, LOGIND_MANAGER, "GetSession", &(id.trim(),));
            if let Ok(path) = by_id {
                return path.as_str().to_owned();
            }
        }
        String::from(LOGIND_AUTO_SESSION)
    }
}

/// The `Name` desktop files give application ids, looked up once each.
///
/// An X11 class (`firefox`), a Wayland app id (`org.gnome.Nautilus`) and a
/// KWin desktop file name are all, usually, the stem of a `.desktop` file
/// in one of the XDG data directories -- and that file's `Name` is what the
/// person knows the program as ("Files", not `org.gnome.Nautilus`). Cached,
/// misses included, so the disk is read once per application per run.
pub(crate) struct DesktopNames {
    dirs: Vec<PathBuf>,
    cache: HashMap<String, Option<String>>,
}

/// Enough for every application anyone runs in a day; past it the cache is
/// cleared rather than grown, so a program that invents a new id per
/// window cannot make it grow without bound.
const DESKTOP_CACHE_LIMIT: usize = 512;

impl DesktopNames {
    pub(crate) fn new() -> Self {
        let var = |key: &str| std::env::var(key).ok();
        Self {
            dirs: application_dirs(
                var("XDG_DATA_HOME").as_deref(),
                var("HOME").as_deref(),
                var("XDG_DATA_DIRS").as_deref(),
            ),
            cache: HashMap::new(),
        }
    }

    /// The desktop file's `Name` for `app_id`, if there is such a file.
    pub(crate) fn name(&mut self, app_id: &str) -> Option<String> {
        if app_id.is_empty() {
            return None;
        }
        if let Some(known) = self.cache.get(app_id) {
            return known.clone();
        }
        let found = desktop_file_names(app_id).iter().find_map(|file| {
            self.dirs.iter().find_map(|dir| {
                let text = std::fs::read_to_string(dir.join(file)).ok()?;
                desktop_entry_name(&text)
            })
        });
        if self.cache.len() >= DESKTOP_CACHE_LIMIT {
            self.cache.clear();
        }
        self.cache.insert(app_id.to_owned(), found.clone());
        found
    }
}

/// The `applications` directories to look in, most personal first, as the
/// XDG base directory specification lays them out.
fn application_dirs(
    data_home: Option<&str>,
    home: Option<&str>,
    data_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match data_home.filter(|dir| !dir.is_empty()) {
        Some(dir) => dirs.push(PathBuf::from(dir)),
        None => {
            if let Some(home) = home.filter(|home| !home.is_empty()) {
                dirs.push(Path::new(home).join(".local/share"));
            }
        }
    }
    let data_dirs =
        data_dirs.filter(|dirs| !dirs.is_empty()).unwrap_or("/usr/local/share:/usr/share");
    dirs.extend(data_dirs.split(':').filter(|dir| !dir.is_empty()).map(PathBuf::from));
    dirs.into_iter().map(|dir| dir.join("applications")).collect()
}

/// The file names an application id might have: itself, then lower-cased
/// (X11 classes are often capitalised, `Gimp`, where the file is not).
fn desktop_file_names(app_id: &str) -> Vec<String> {
    // Never a path: an id comes from other programs, and `../` in one must
    // not make this read files outside the applications directories.
    if app_id.contains('/') || app_id.contains('\0') {
        return Vec::new();
    }
    let stem = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    let mut names = vec![format!("{stem}.desktop")];
    let lower = stem.to_lowercase();
    if lower != stem {
        names.push(format!("{lower}.desktop"));
    }
    names
}

/// The unlocalised `Name` of a desktop file's `[Desktop Entry]` group.
fn desktop_entry_name(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "Name" {
            let value = value.trim();
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn gnome_on_wayland_is_the_extensions_alone() {
        let env = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
        ]);
        assert_eq!(plan(&env), vec![Attempt::Gnome]);
    }

    #[test]
    fn gnome_on_x11_is_plain_x11() {
        let env = env(&[
            ("XDG_SESSION_TYPE", "x11"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
            ("DISPLAY", ":0"),
        ]);
        assert_eq!(plan(&env), vec![Attempt::X11]);
    }

    #[test]
    fn plasma_tries_the_kwin_script_first() {
        let wayland = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":1"),
        ]);
        assert_eq!(plan(&wayland), vec![Attempt::Kde, Attempt::XWayland]);
        let x11 =
            env(&[("XDG_SESSION_TYPE", "x11"), ("XDG_CURRENT_DESKTOP", "KDE"), ("DISPLAY", ":0")]);
        assert_eq!(plan(&x11), vec![Attempt::Kde, Attempt::X11]);
    }

    #[test]
    fn other_wayland_compositors_try_wlroots_then_xwayland() {
        let sway = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "sway"),
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("DISPLAY", ":0"),
        ]);
        assert_eq!(plan(&sway), vec![Attempt::Wlroots, Attempt::XWayland]);
        // No XWayland running: nothing to fall back to.
        let river = env(&[("XDG_CURRENT_DESKTOP", "river"), ("WAYLAND_DISPLAY", "wayland-1")]);
        assert_eq!(plan(&river), vec![Attempt::Wlroots]);
    }

    #[test]
    fn the_session_type_wins_over_the_display_variables() {
        // An X11 session that inherited a stray WAYLAND_DISPLAY.
        let env = env(&[
            ("XDG_SESSION_TYPE", "x11"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
        ]);
        assert_eq!(plan(&env), vec![Attempt::X11]);
    }

    #[test]
    fn without_a_session_type_the_display_variables_decide() {
        assert_eq!(plan(&env(&[("DISPLAY", ":0")])), vec![Attempt::X11]);
        assert_eq!(
            plan(&env(&[
                ("XDG_SESSION_TYPE", "tty"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("DISPLAY", ":0")
            ])),
            vec![Attempt::Wlroots, Attempt::XWayland]
        );
    }

    #[test]
    fn no_display_is_nothing_to_try() {
        assert!(plan(&env(&[])).is_empty());
        assert!(plan(&env(&[("XDG_SESSION_TYPE", "tty"), ("DISPLAY", "  ")])).is_empty());
    }

    #[test]
    fn desktop_names_read_the_entry_group_only() {
        let text = "\
# a comment
[Desktop Entry]
Type=Application
Name[de]=Dateien
Name = Files
GenericName=File Manager

[Desktop Action new-window]
Name=New Window
";
        assert_eq!(desktop_entry_name(text).as_deref(), Some("Files"));
        let action_only = "[Desktop Action x]\nName=Not this\n";
        assert_eq!(desktop_entry_name(action_only), None);
        assert_eq!(desktop_entry_name("[Desktop Entry]\nName=\n"), None);
    }

    #[test]
    fn desktop_file_names_try_the_id_then_lower_case_and_refuse_paths() {
        assert_eq!(
            desktop_file_names("org.gnome.Nautilus"),
            ["org.gnome.Nautilus.desktop", "org.gnome.nautilus.desktop"]
        );
        assert_eq!(desktop_file_names("firefox"), ["firefox.desktop"]);
        assert_eq!(desktop_file_names("code.desktop"), ["code.desktop"]);
        assert!(desktop_file_names("../../etc/passwd").is_empty());
    }

    #[test]
    fn application_dirs_follow_the_base_directory_spec() {
        assert_eq!(
            application_dirs(None, Some("/home/a"), None),
            [
                PathBuf::from("/home/a/.local/share/applications"),
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ]
        );
        assert_eq!(
            application_dirs(
                Some("/data"),
                Some("/home/a"),
                Some("/flatpak/exports/share::/usr/share")
            ),
            [
                PathBuf::from("/data/applications"),
                PathBuf::from("/flatpak/exports/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ]
        );
    }
}
