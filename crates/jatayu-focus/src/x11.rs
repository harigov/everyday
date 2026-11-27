//! X11: the window manager's word for the active window, the screensaver
//! extension's idle clock, and logind for the lock.
//!
//! - **Focus**: `_NET_ACTIVE_WINDOW` on the root window, which every EWMH
//!   window manager keeps pointing at the client window with focus (0 when
//!   nothing has it). That window's `WM_CLASS` names the program -- two
//!   NUL-terminated strings, the instance and the class, of which the class
//!   (`firefox`, `Code`, `Gimp`) is the stable one -- and `_NET_WM_NAME`
//!   (UTF-8) or the older `WM_NAME` is its title.
//! - **Idle**: the MIT-SCREEN-SAVER extension's `QueryInfo`, whose
//!   `ms_since_user_input` is the server's own count since the last key or
//!   pointer event.
//! - **Locked**: logind's `LockedHint` -- X11 lockers (`light-locker`,
//!   `xss-lock` with anything) set it, and the screensaver extension's own
//!   state says only whether the screen is blanked, not locked.
//!
//! The pure-Rust connection is opened once, when the watcher is made, so
//! that [`crate::detect`] can fall back when there is no X server; if it
//! breaks later (the server restarted, XWayland was stopped for being idle)
//! it is dropped and opened again on the next call.
//!
//! The same watcher serves XWayland as a last resort on Wayland
//! compositors that offer nothing better, with a
//! [`limitation`](Watcher::limitation) saying that native Wayland windows
//! are invisible to it.

use std::time::Duration;

use x11rb::connection::Connection as _;
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::screensaver;
use x11rb::protocol::xproto::{self, Atom, AtomEnum, Window};
use x11rb::rust_connection::RustConnection;

use crate::linux::{DesktopNames, Logind, failed};
use crate::{Backend, Error, Focused, Result, Watcher};

pub(crate) struct X11 {
    connection: Option<Connected>,
    limitation: Option<&'static str>,
    logind: Logind,
    names: DesktopNames,
}

/// An open connection and what was looked up on it once.
struct Connected {
    connection: RustConnection,
    root: Window,
    net_active_window: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
}

/// Longest property read, in the 32-bit units X counts in: 4 KB, past any
/// class name and any title worth keeping.
const PROPERTY_LIMIT: u32 = 1024;

impl X11 {
    /// Connect to the X server `DISPLAY` names. `limitation` is what
    /// [`Watcher::limitation`] will say -- set for XWayland.
    pub(crate) fn connect(limitation: Option<&'static str>) -> Result<Self> {
        let connection = Connected::open()?;
        Ok(Self {
            connection: Some(connection),
            limitation,
            logind: Logind::new(),
            names: DesktopNames::new(),
        })
    }

    fn connected(&mut self) -> Result<&Connected> {
        if self.connection.is_none() {
            self.connection = Some(Connected::open()?);
        }
        self.connection.as_ref().ok_or_else(|| Error::Unavailable(String::from("no X server")))
    }

    /// Turn a failed request into the crate's error, dropping the
    /// connection when it is the connection that failed.
    fn trouble(&mut self, e: ReplyError) -> Error {
        match e {
            ReplyError::ConnectionError(ConnectionError::UnsupportedExtension) => {
                Error::Unavailable(String::from("the X server has no screensaver extension"))
            }
            ReplyError::ConnectionError(e) => {
                self.connection = None;
                failed(format!("the X connection failed: {e}"))
            }
            ReplyError::X11Error(e) => failed(format!("the X server refused: {e:?}")),
        }
    }
}

impl Connected {
    fn open() -> Result<Self> {
        let (connection, screen) = RustConnection::connect(None)
            .map_err(|e| Error::Unavailable(format!("no X server: {e}")))?;
        let root =
            connection.setup().roots.get(screen).map(|screen| screen.root).ok_or_else(|| {
                Error::Unavailable(String::from("the X server named no such screen"))
            })?;
        let intern = |name: &[u8]| -> Result<Atom> {
            let cookie = xproto::intern_atom(&connection, false, name).map_err(failed)?;
            Ok(cookie.reply().map_err(failed)?.atom)
        };
        let net_active_window = intern(b"_NET_ACTIVE_WINDOW")?;
        let net_wm_name = intern(b"_NET_WM_NAME")?;
        let utf8_string = intern(b"UTF8_STRING")?;
        Ok(Self { connection, root, net_active_window, net_wm_name, utf8_string })
    }

    fn property(
        &self,
        window: Window,
        property: Atom,
        kind: impl Into<Atom>,
    ) -> std::result::Result<xproto::GetPropertyReply, ReplyError> {
        xproto::get_property(&self.connection, false, window, property, kind, 0, PROPERTY_LIMIT)?
            .reply()
    }

    /// The active window's class and title, or `None` when nothing has
    /// focus or the window gives no class.
    fn focused(&self) -> std::result::Result<Option<Seen>, ReplyError> {
        let active = self.property(self.root, self.net_active_window, AtomEnum::WINDOW)?;
        let window = active.value32().and_then(|mut values| values.next()).unwrap_or(0);
        if window == 0 {
            return Ok(None);
        }
        let class = match self.property(window, AtomEnum::WM_CLASS.into(), AtomEnum::ANY) {
            Ok(class) => class,
            // The window closed between the two reads: for this instant,
            // nothing has focus.
            Err(ReplyError::X11Error(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let (instance, class) = parse_wm_class(&class.value);
        let title = match self.title(window) {
            Ok(title) => title,
            Err(ReplyError::X11Error(_)) => String::new(),
            Err(e) => return Err(e),
        };
        Ok(WmClass { instance, class }.into_seen(title))
    }

    fn title(&self, window: Window) -> std::result::Result<String, ReplyError> {
        let utf8 = self.property(window, self.net_wm_name, self.utf8_string)?;
        if !utf8.value.is_empty() {
            return Ok(String::from_utf8_lossy(&utf8.value).into_owned());
        }
        let legacy = self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::ANY)?;
        Ok(if legacy.type_ == Atom::from(AtomEnum::STRING) {
            latin1(&legacy.value)
        } else {
            String::from_utf8_lossy(&legacy.value).into_owned()
        })
    }

    fn idle(&self) -> std::result::Result<Duration, ReplyError> {
        let info = screensaver::query_info(&self.connection, self.root)?.reply()?;
        Ok(Duration::from_millis(u64::from(info.ms_since_user_input)))
    }
}

/// What one look at the active window found, before the friendly name.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    app_id: String,
    class: String,
    title: String,
}

/// `WM_CLASS`, split.
#[derive(Debug, PartialEq, Eq)]
struct WmClass {
    instance: String,
    class: String,
}

impl WmClass {
    /// The app id is the class lower-cased (falling back to the instance
    /// for the odd program that sets only that); a window with neither is
    /// not one anything can be counted against.
    fn into_seen(self, title: String) -> Option<Seen> {
        let class = if self.class.is_empty() { self.instance } else { self.class };
        if class.is_empty() {
            return None;
        }
        Some(Seen { app_id: class.to_lowercase(), class, title })
    }
}

/// `WM_CLASS`'s two NUL-terminated strings, instance then class. Tolerates
/// a missing final NUL and a missing class.
fn parse_wm_class(value: &[u8]) -> (String, String) {
    let mut parts =
        value.split(|&byte| byte == 0).map(|part| String::from_utf8_lossy(part).trim().to_owned());
    let instance = parts.next().unwrap_or_default();
    let class = parts.next().unwrap_or_default();
    (instance, class)
}

/// X's `STRING` type is ISO 8859-1, whose bytes are the first 256 code
/// points.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

impl Watcher for X11 {
    fn backend(&self) -> Backend {
        Backend::X11
    }

    fn limitation(&self) -> Option<&str> {
        self.limitation
    }

    fn focused(&mut self) -> Result<Option<Focused>> {
        // Bound first, so the borrow of the connection ends before
        // `trouble` may drop it.
        let result = self.connected()?.focused();
        let seen = match result {
            Ok(seen) => seen,
            Err(e) => return Err(self.trouble(e)),
        };
        Ok(seen.map(|seen| {
            let name = self.names.name(&seen.app_id).unwrap_or(seen.class);
            Focused { app_id: seen.app_id, name, title: seen.title }
        }))
    }

    fn idle(&mut self) -> Result<Duration> {
        let result = self.connected()?.idle();
        match result {
            Ok(idle) => Ok(idle),
            Err(e) => Err(self.trouble(e)),
        }
    }

    fn locked(&mut self) -> Result<bool> {
        self.logind.locked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wm_class(value: &[u8]) -> WmClass {
        let (instance, class) = parse_wm_class(value);
        WmClass { instance, class }
    }

    #[test]
    fn wm_class_is_instance_then_class() {
        assert_eq!(
            parse_wm_class(b"Navigator\0firefox\0"),
            (String::from("Navigator"), String::from("firefox"))
        );
        assert_eq!(parse_wm_class(b"code\0Code\0"), (String::from("code"), String::from("Code")));
        // No final NUL, as some clients write it.
        assert_eq!(parse_wm_class(b"gimp\0Gimp"), (String::from("gimp"), String::from("Gimp")));
        assert_eq!(parse_wm_class(b""), (String::new(), String::new()));
    }

    #[test]
    fn the_class_lower_cased_is_the_app_id() {
        let seen = wm_class(b"code\0Code\0").into_seen("main.rs - Visual Studio Code".into());
        assert_eq!(
            seen,
            Some(Seen {
                app_id: "code".into(),
                class: "Code".into(),
                title: "main.rs - Visual Studio Code".into()
            })
        );
    }

    #[test]
    fn a_window_with_only_an_instance_uses_it() {
        let seen = wm_class(b"xterm").into_seen(String::new());
        assert_eq!(seen.map(|seen| seen.app_id), Some(String::from("xterm")));
    }

    #[test]
    fn a_window_without_a_class_is_not_counted() {
        assert_eq!(wm_class(b"").into_seen("untitled".into()), None);
        assert_eq!(wm_class(b"\0\0").into_seen("untitled".into()), None);
    }

    #[test]
    fn latin1_titles_decode_every_byte() {
        assert_eq!(latin1(b"caf\xe9"), "café");
    }
}
