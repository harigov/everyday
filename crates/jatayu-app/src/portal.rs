//! The desktop portals Wayland leaves as the only way to do two things.
//!
//! An application on Wayland cannot claim a key for the whole desktop, and
//! cannot press a key in another application's window. Both are deliberate,
//! and both have a sanctioned door: the `GlobalShortcuts` portal, which asks
//! the person once which key and then tells this process when it is pressed,
//! and the `RemoteDesktop` portal, which asks once whether this process may
//! type and then lets it. Everything here is those two doors, and the
//! `Clipboard` portal that rides on the second: a remote-desktop session may
//! also offer the clipboard's contents, which a window of ours would
//! otherwise have to be focused to do, and take what another window copied.
//!
//! The first door reports a key being let go as well as pressed, which is
//! what hold-to-talk voice typing needs; the palette and writing help ignore
//! the release.
//!
//! # Which application is asking
//!
//! A portal files what it was told under the asking application's id, and a
//! process outside a sandbox has none until it registers one -- which only
//! succeeds for an id with a desktop file installed under that name. So
//! [`register`] tries the names Jatayu might be installed under and keeps
//! going without one if none matches: unregistered, the portals still work,
//! the dialogs just do not say whose they are, and what was granted is
//! filed under the empty id.
//!
//! Checked against GNOME 50's portal in a headless shell: the shortcut dialog
//! appears once, and a second session binding the same ids is answered
//! without one; the remote-desktop dialog appears once, and its restore
//! token starts every later session without one.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ashpd::desktop::clipboard::{Clipboard, RequestClipboardOptions, SetSelectionOptions};
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::{PersistMode, Session};
use ashpd::enumflags2::BitFlags;
use futures_util::{FutureExt, StreamExt};
use tokio::net::unix::pipe;
use tokio::sync::{Mutex, OnceCell, mpsc, watch};

/// Register this process under the first application id that has a desktop
/// file. Once per process, before any other portal call: the registry
/// refuses a process that has already talked to a portal unregistered.
pub async fn register() {
    static DONE: OnceCell<()> = OnceCell::const_new();
    DONE.get_or_init(|| async {
        for id in app_ids() {
            let Ok(app_id) = id.parse::<ashpd::AppID>() else { continue };
            match ashpd::register_host_app(app_id).await {
                Ok(()) => {
                    tracing::info!(app_id = %id, "registered with the desktop portals");
                    return;
                }
                Err(e) => tracing::debug!(app_id = %id, error = %e, "not this app id"),
            }
        }
        tracing::info!(
            "no installed desktop file names a valid app id; using the portals unregistered"
        );
    })
    .await;
}

/// The ids Jatayu might be installed under: whatever desktop file
/// launched it, then the bundle identifier. `jatayu-app` and `Jatayu`,
/// the other two names its desktop files go by, are not valid ids.
fn app_ids() -> Vec<String> {
    let launched = std::env::var("GIO_LAUNCHED_DESKTOP_FILE").ok().and_then(|path| {
        let name = std::path::Path::new(&path).file_name()?.to_str()?.to_string();
        name.strip_suffix(".desktop").map(str::to_string)
    });
    launched.into_iter().chain(std::iter::once("app.jatayu.Jatayu".to_string())).collect()
}

// ---- GlobalShortcuts --------------------------------------------------------

/// One key to claim: an id the portal files it under, what the dialog says
/// it does, and the trigger offered as a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub id: &'static str,
    pub description: &'static str,
    /// In the XDG shortcuts format: `CTRL+ALT+space`.
    pub preferred: String,
}

/// What the desktop said it bound, by id: the trigger as it describes it --
/// "Press <Control><Alt>space" from GNOME -- or nothing yet.
pub type Bound = Vec<(String, String)>;

/// A portal session holding this process's global keys, kept for the life of
/// the process: the keys only work while it is open.
pub struct Shortcuts {
    wanted: mpsc::UnboundedSender<Vec<Wanted>>,
    bound: watch::Receiver<Option<Result<Bound, String>>>,
    /// Whether the desktop says when a key is let go, as well as pressed.
    releases: bool,
}

impl Shortcuts {
    /// Connect to the portal, or `None` where there is no GlobalShortcuts
    /// portal to connect to. `on_activate` is called with the id of the key,
    /// whether it was pressed (`true`) or let go (`false`), and the activation
    /// token the desktop sent with it, which is what lets the window that
    /// answers take focus. A release is only heard where the desktop sends
    /// one, and its token -- GNOME may or may not send one then -- is often
    /// `None`.
    ///
    /// `parent` is the window the desktop's dialog -- the first time, when
    /// it asks which keys -- should open over; see [`parent_of`].
    pub async fn connect(
        parent: Option<ashpd::WindowIdentifier>,
        on_activate: impl Fn(&str, bool, Option<String>, Duration) + Send + Sync + 'static,
    ) -> Option<Self> {
        register().await;
        let portal = match GlobalShortcuts::new().await {
            Ok(p) => p,
            Err(e) => {
                tracing::info!(error = %e, "this desktop has no GlobalShortcuts portal");
                return None;
            }
        };
        let activated = match portal.receive_activated().await {
            Ok(s) => s,
            Err(e) => {
                tracing::info!(error = %e, "could not listen to the GlobalShortcuts portal");
                return None;
            }
        };
        // Without releases hold-to-talk degrades to press-to-toggle; that is
        // not worth giving up the presses for.
        let deactivated = match portal.receive_deactivated().await {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::info!(error = %e, "this desktop does not report keys being let go");
                None
            }
        };
        let releases = deactivated.is_some();
        let changed = portal.receive_shortcuts_changed().await.ok();
        let (wanted_tx, wanted_rx) = mpsc::unbounded_channel();
        let (bound_tx, bound_rx) = watch::channel(None);
        tauri::async_runtime::spawn(run(
            portal,
            parent,
            wanted_rx,
            bound_tx,
            Signals {
                activated: Box::pin(activated),
                deactivated: deactivated.map(|d| Box::pin(d) as _),
                changed: changed.map(|c| Box::pin(c) as _),
            },
            Arc::new(on_activate),
        ));
        Some(Self { wanted: wanted_tx, bound: bound_rx, releases })
    }

    /// Whether a key being let go is reported -- what holding a key to talk
    /// needs.
    pub fn reports_releases(&self) -> bool {
        self.releases
    }

    /// Claim exactly these keys, giving back any others.
    pub fn want(&self, keys: Vec<Wanted>) {
        let _ = self.wanted.send(keys);
    }

    /// What was last bound, or why it was not. `None` while the first
    /// binding is still waiting on the person.
    pub fn bound(&self) -> Option<Result<Bound, String>> {
        self.bound.borrow().clone()
    }

    /// Something to wait on for the next answer: taken *before* [`want`]
    /// is called, so the answer to that call is the one waited for.
    ///
    /// [`want`]: Shortcuts::want
    pub fn next_answer(&self) -> watch::Receiver<Option<Result<Bound, String>>> {
        let mut rx = self.bound.clone();
        rx.borrow_and_update();
        rx
    }
}

type Stream<T> = std::pin::Pin<Box<dyn futures_util::Stream<Item = T> + Send>>;
type Activated = Stream<ashpd::desktop::global_shortcuts::Activated>;
type Deactivated = Stream<ashpd::desktop::global_shortcuts::Deactivated>;
type Changed = Stream<ashpd::desktop::global_shortcuts::ShortcutsChanged>;
/// Called with the id of a key, whether it was pressed or let go, the
/// activation token sent with it, and when the desktop says it happened.
type OnActivate = Arc<dyn Fn(&str, bool, Option<String>, Duration) + Send + Sync>;

/// What the portal says on its own: a key pressed, a key let go -- where the
/// desktop reports it -- and a key changed in its settings.
struct Signals {
    activated: Activated,
    deactivated: Option<Deactivated>,
    changed: Option<Changed>,
}

async fn run(
    portal: GlobalShortcuts,
    parent: Option<ashpd::WindowIdentifier>,
    mut wanted: mpsc::UnboundedReceiver<Vec<Wanted>>,
    bound: watch::Sender<Option<Result<Bound, String>>>,
    signals: Signals,
    on_activate: OnActivate,
) {
    let Signals { mut activated, mut deactivated, mut changed } = signals;
    let mut session: Option<ashpd::desktop::Session<GlobalShortcuts>> = None;
    loop {
        tokio::select! {
            next = wanted.recv() => {
                let Some(mut keys) = next else { break };
                // A burst of changes -- the palette switched off and writing
                // help on, at startup -- is one rebinding, not two dialogs.
                while let Ok(newer) = wanted.try_recv() {
                    keys = newer;
                }
                // A session binds once, so a different set of keys is a new
                // session; the old one gives its keys back as it closes.
                if let Some(old) = session.take() {
                    let _ = old.close().await;
                }
                if keys.is_empty() {
                    let _ = bound.send(Some(Ok(Vec::new())));
                    continue;
                }
                match bind(&portal, &keys, parent.as_ref()).await {
                    Ok((s, b)) => {
                        session = Some(s);
                        let _ = bound.send(Some(Ok(b)));
                    }
                    Err(e) => {
                        tracing::info!(error = %e, "the desktop did not bind the keys");
                        let _ = bound.send(Some(Err(e)));
                    }
                }
            }
            Some(a) = activated.next() => {
                let token = a
                    .options()
                    .get("activation_token")
                    .and_then(|v| v.downcast_ref::<String>().ok());
                on_activate(a.shortcut_id(), true, token, a.timestamp());
            }
            Some(d) = async {
                match deactivated.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                let token = d
                    .options()
                    .get("activation_token")
                    .and_then(|v| v.downcast_ref::<String>().ok());
                on_activate(d.shortcut_id(), false, token, d.timestamp());
            }
            Some(c) = async {
                match changed.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                // Somebody changed the key in the desktop's own settings.
                let b = c
                    .shortcuts()
                    .iter()
                    .map(|s| (s.id().to_string(), s.trigger_description().to_string()))
                    .collect();
                let _ = bound.send(Some(Ok(b)));
            }
        }
    }
}

async fn bind(
    portal: &GlobalShortcuts,
    keys: &[Wanted],
    parent: Option<&ashpd::WindowIdentifier>,
) -> Result<(ashpd::desktop::Session<GlobalShortcuts>, Bound), String> {
    let session = portal.create_session(Default::default()).await.map_err(|e| e.to_string())?;
    let shortcuts: Vec<NewShortcut> = keys
        .iter()
        .map(|k| NewShortcut::new(k.id, k.description).preferred_trigger(k.preferred.as_str()))
        .collect();
    let answered = portal
        .bind_shortcuts(&session, &shortcuts, parent, Default::default())
        .await
        .and_then(|r| r.response())
        .map_err(|e| e.to_string())?;
    let bound = answered
        .shortcuts()
        .iter()
        .map(|s| (s.id().to_string(), s.trigger_description().to_string()))
        .collect();
    Ok((session, bound))
}

/// The main window as a portal names a parent: its Wayland surface,
/// exported. Without one the desktop's dialog is nobody's, and GNOME opens
/// it behind the window that was just raised.
///
/// Must be called on the main thread, where the window's surface is read;
/// the export itself is awaited off it.
pub fn parent_of(window: &tauri::WebviewWindow) -> Option<ParentHandle> {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    let surface = match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Wayland(h) => h.surface.as_ptr() as usize,
        _ => return None,
    };
    let display = match window.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Wayland(h) => h.display.as_ptr() as usize,
        _ => return None,
    };
    Some(ParentHandle { surface, display })
}

/// A window's Wayland surface and display, as addresses, so they can cross
/// to the task that exports them. See [`parent_of`].
pub struct ParentHandle {
    surface: usize,
    display: usize,
}

impl ParentHandle {
    pub async fn export(self) -> Option<ashpd::WindowIdentifier> {
        // SAFETY: both were read from a live window's raw handles by
        // `parent_of`. The display is GTK's, open for the life of the
        // process; the surface is the main window's, which is destroyed only
        // on the way out.
        unsafe {
            ashpd::WindowIdentifier::from_wayland_raw(
                self.surface as *mut std::ffi::c_void,
                self.display as *mut std::ffi::c_void,
            )
            .await
        }
    }
}

/// A Tauri-style accelerator -- `Ctrl+Alt+Space` -- in the XDG shortcuts
/// format a portal is offered it in: `CTRL+ALT+space`.
pub fn xdg_trigger(accelerator: &str) -> String {
    accelerator
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmdorctrl" | "commandorcontrol" => "CTRL".to_string(),
            "alt" | "option" => "ALT".to_string(),
            "shift" => "SHIFT".to_string(),
            "super" | "meta" | "cmd" | "command" | "logo" => "LOGO".to_string(),
            "space" => "space".to_string(),
            "enter" | "return" => "Return".to_string(),
            "esc" | "escape" => "Escape".to_string(),
            "tab" => "Tab".to_string(),
            key if key.len() == 1 => key.to_string(),
            key => {
                // F1..F24 and the like keep their keysym spelling.
                let mut c = key.chars();
                c.next()
                    .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// "Press <Control><Alt>space" -- how GNOME describes a trigger -- as a
/// person would write it: "Ctrl+Alt+Space".
pub fn readable_trigger(description: &str) -> String {
    let rest = description.trim().strip_prefix("Press ").unwrap_or(description.trim());
    let mut parts = Vec::new();
    let mut tail = rest;
    while let Some(start) = tail.find('<')
        && let Some(end) = tail[start..].find('>')
    {
        let name = &tail[start + 1..start + end];
        parts.push(match name.to_ascii_lowercase().as_str() {
            "control" | "primary" | "ctrl" => "Ctrl".to_string(),
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "super" | "logo" | "meta" => "Super".to_string(),
            other => other.to_string(),
        });
        tail = &tail[start + end + 1..];
    }
    let key = tail.trim();
    if !key.is_empty() {
        let mut c = key.chars();
        parts.push(
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default(),
        );
    }
    if parts.is_empty() { rest.to_string() } else { parts.join("+") }
}

// ---- RemoteDesktop ----------------------------------------------------------

/// The XKB keysyms for the keys a paste (Ctrl+V) and a copy (Ctrl+Insert) are.
const KEYSYM_CONTROL_L: i32 = 0xffe3;
const KEYSYM_V: i32 = 0x76;
const KEYSYM_INSERT: i32 = 0xff63;

/// How much longer to wait before pressing anything when the desktop had to
/// ask first: its dialog has to close and the keyboard find its way back.
const ASKED_SETTLE: Duration = Duration::from_millis(600);

/// A session that took this long to start waited on the person: the
/// desktop's dialog was up, and has only just handed the keyboard back to the
/// window behind it.
const ASKED_AFTER: Duration = Duration::from_millis(400);

/// Whether a session that took `started_in` to start had to ask the person.
fn asked_the_person(started_in: Duration) -> bool {
    started_in > ASKED_AFTER
}

/// How long to wait before pressing anything: `settle`, and the time the
/// desktop's dialog needs to go away when it was `asked`.
fn settle_before_press(asked: bool, settle: Duration) -> Duration {
    if asked { settle + ASKED_SETTLE } else { settle }
}

/// Press Ctrl and `key` together, then let go of both, in whatever window has
/// the keyboard.
async fn press_with_control(
    portal: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    key: i32,
) -> Result<(), String> {
    for (keysym, state) in [
        (KEYSYM_CONTROL_L, KeyState::Pressed),
        (key, KeyState::Pressed),
        (key, KeyState::Released),
        (KEYSYM_CONTROL_L, KeyState::Released),
    ] {
        portal
            .notify_keyboard_keysym(session, keysym, state, Default::default())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Press Ctrl+V, in whatever window has the keyboard.
async fn press_paste(
    portal: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
) -> Result<(), String> {
    press_with_control(portal, session, KEYSYM_V).await
}

/// Press Ctrl+V in whatever window has the keyboard, through a
/// RemoteDesktop session of its own.
///
/// `restore` is the token the last session handed back; with it the
/// session starts without asking again. The first time, the desktop asks --
/// "Allow remote interaction" -- and the answer is what comes back as the
/// next token. `settle` is how long to let the keyboard find its way back to
/// the window the text came from before pressing anything.
pub async fn paste(restore: Option<String>, settle: Duration) -> Result<Option<String>, String> {
    register().await;
    let portal = RemoteDesktop::new().await.map_err(|e| e.to_string())?;
    let session = portal.create_session(Default::default()).await.map_err(|e| e.to_string())?;
    let result = async {
        portal
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(BitFlags::from(DeviceType::Keyboard))
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(restore.as_deref()),
            )
            .await
            .map_err(|e| e.to_string())?;
        let asking = Instant::now();
        let started = portal
            .start(&session, None, Default::default())
            .await
            .and_then(|r| r.response())
            .map_err(|e| match e {
                ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
                    "typing into other applications was not allowed".to_string()
                }
                other => other.to_string(),
            })?;
        if !started.devices().contains(DeviceType::Keyboard) {
            return Err("the desktop did not hand over a keyboard".to_string());
        }
        let asked = asked_the_person(asking.elapsed());
        tokio::time::sleep(settle_before_press(asked, settle)).await;
        press_paste(&portal, &session).await?;
        Ok(started.restore_token().map(str::to_string))
    }
    .await;
    // Closed straight away: an open session is a "being controlled" badge in
    // the top bar, and there is nothing more to type.
    let _ = session.close().await;
    result
}

// ---- Clipboard --------------------------------------------------------------

/// The types the text is offered as, the one a window of today asks for
/// first: GTK and Qt ask for `text/plain;charset=utf-8`, applications on X
/// (through XWayland) for `UTF8_STRING`, and old ones for `STRING` or `TEXT`.
const TEXT_MIME_TYPES: [&str; 5] =
    ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"];

/// How long to wait, after Ctrl+V, for the window to ask for the text. An
/// application that has not asked by then has not taken the paste at all.
const TRANSFER_WAIT: Duration = Duration::from_secs(2);

/// How long to go on answering once the first request is answered. Some
/// applications ask again -- for another type, or after a first read that
/// came back short -- and a session closed in between would fail the second.
const REPEAT_GRACE: Duration = Duration::from_millis(300);

/// One paste with the clipboard at a time: two sessions each offering their
/// own text as the clipboard's contents would answer each other's windows.
static ONE_AT_A_TIME: Mutex<()> = Mutex::const_new(());

/// Why [`paste_text`] did not type the text, and whether another way of typing
/// it is still safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteTextError {
    /// Nothing was pressed, because the desktop cannot do it this way -- it
    /// has no clipboard portal, or will not share the clipboard or a keyboard
    /// with the session. The text can be pasted another way. The words are
    /// for the log.
    Unavailable(String),
    /// Not to be tried again another way: the person said no to the
    /// desktop's question, or Ctrl+V was pressed and no window took the text
    /// -- an application that was only slow would then type it twice. The
    /// words are fit for the card.
    Stopped(String),
}

impl PasteTextError {
    /// Whether the text may still be pasted another way.
    pub fn falls_back(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }

    /// What went wrong, in words.
    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(words) | Self::Stopped(words) => words,
        }
    }
}

impl std::fmt::Display for PasteTextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// What [`paste_text`] did. The restore token is kept apart from the result
/// because it is the session's, not the paste's: the desktop may hand out a
/// new one with every session it starts, retiring the old, so an error after
/// the start must not lose it.
#[derive(Debug, PartialEq, Eq)]
pub struct TextPaste {
    /// The restore token the session ended with, when it got as far as
    /// starting and the desktop sent one. Keep it for the next paste.
    pub restore_token: Option<String>,
    pub result: Result<(), PasteTextError>,
}

/// How many windows asked for the text during a paste, and how many were
/// given it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Answers {
    asked: u32,
    taken: u32,
}

impl Answers {
    fn add(&mut self, taken: bool) {
        self.asked += 1;
        if taken {
            self.taken += 1;
        }
    }

    /// Whether the text got where Ctrl+V was pressed. A window that asked for
    /// it once and was given it is enough; asking again is only thoroughness.
    fn verdict(self) -> Result<(), PasteTextError> {
        match self {
            Self { taken: 1.., .. } => Ok(()),
            Self { asked: 0, .. } => Err(PasteTextError::Stopped(
                "the application in front did not take the text, so nothing was typed".into(),
            )),
            Self { .. } => Err(PasteTextError::Stopped(
                "the application in front asked for the text but it could not be handed over"
                    .into(),
            )),
        }
    }
}

/// The bytes to hand a window that asked for `mime`: the text itself, in
/// UTF-8, for any of the types it was offered as. (`STRING` is Latin-1 by
/// convention, but plain ASCII reads the same either way, and a window that
/// asks for it after `UTF8_STRING` was on offer takes what it is given.)
/// Nothing for a type it was not offered as.
fn payload_for<'a>(text: &'a str, mime: &str) -> Option<&'a [u8]> {
    text_type_index(mime).map(|_| text.as_bytes())
}

/// Where `mime` is in [`TEXT_MIME_TYPES`], however it is cased -- GTK is not
/// always careful about the case of a charset. `None` for a type that is not
/// text.
fn text_type_index(mime: &str) -> Option<usize> {
    TEXT_MIME_TYPES.iter().position(|text| text.eq_ignore_ascii_case(mime))
}

/// Write all of `bytes` to `fd`, then close it, which is how the reader knows
/// the text has ended. The portal's end may be a non-blocking pipe, so a full
/// one is waited on rather than failed, until `deadline`.
fn write_all_and_close(fd: OwnedFd, mut bytes: &[u8], deadline: Instant) -> std::io::Result<()> {
    use std::io::ErrorKind;
    let mut pipe = std::fs::File::from(fd);
    while !bytes.is_empty() {
        match pipe.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Hand one request its text: the portal's `SelectionWrite` gives a pipe,
/// the text goes down it, and `SelectionWriteDone` says how it went. Whether
/// it went well.
async fn answer(
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    mime: &str,
    serial: u32,
    text: &str,
) -> bool {
    // The pipe is asked for only when there is something to put in it: one
    // opened and closed empty can be read as an empty paste that succeeded.
    let written = match payload_for(text, mime) {
        None => {
            tracing::debug!(mime, "a window asked for a type the text was not offered as");
            false
        }
        Some(bytes) => match clipboard.selection_write(session, serial).await {
            Ok(fd) => {
                let bytes = bytes.to_vec();
                let fd: OwnedFd = fd.into();
                let deadline = Instant::now() + TRANSFER_WAIT;
                let wrote = tauri::async_runtime::spawn_blocking(move || {
                    write_all_and_close(fd, &bytes, deadline)
                })
                .await;
                match wrote {
                    Ok(Ok(())) => true,
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, mime, "could not write the text to the window");
                        false
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, mime, "the write to the window did not finish");
                        false
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, mime, "the desktop would not open a pipe for the text");
                false
            }
        },
    };
    // Said whatever happened: a window left waiting on a transfer that is
    // never ended waits for good.
    if let Err(e) = clipboard.selection_write_done(session, serial, written).await {
        tracing::debug!(error = %e, "could not tell the desktop the text was written");
        return false;
    }
    written
}

/// Answer every request for the text, one at a time, with `answer` -- given
/// the type asked for and the serial -- and say on `answered` how each went,
/// but only of those that came once Ctrl+V was `pressed`.
///
/// Requests are `(session, type, serial)`; those of other sessions are not
/// for this text and are left alone. The ones before the press are answered
/// all the same, because a window left waiting on a transfer waits for good, and
/// not counted: a clipboard manager (GPaste, Klipper) reads whatever the
/// clipboard becomes as soon as it does, and its being given the text says
/// nothing of the window being typed into. Does not end until `transfers`
/// does.
async fn serve<Fut: std::future::Future<Output = bool>>(
    mut transfers: impl futures_util::Stream<Item = (String, String, u32)> + Unpin,
    ours: &str,
    pressed: &AtomicBool,
    answered: &mpsc::UnboundedSender<bool>,
    mut answer: impl FnMut(String, u32) -> Fut,
) {
    while let Some((from, mime, serial)) = transfers.next().await {
        if from != ours {
            continue;
        }
        // Read on arrival, not when the answer is done: a clipboard manager's
        // slow read that began before the press is still before it.
        let after_the_press = pressed.load(Ordering::Acquire);
        let taken = answer(mime, serial).await;
        if after_the_press {
            let _ = answered.send(taken);
        }
    }
}

/// Start `session` with a keyboard and the clipboard, which a paste and a copy
/// both need: the keyboard chosen -- remembered with `restore`, which starts it
/// without asking again -- the clipboard asked for, the session started.
/// Whether the person had to be asked.
///
/// `refused` is what is said when they answered no. `token` is filled with the
/// session's restore token as soon as the session has one, so that an error
/// after it does not lose it.
async fn start_with_clipboard(
    portal: &RemoteDesktop,
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    restore: Option<&str>,
    refused: &str,
    token: &mut Option<String>,
) -> Result<bool, PasteTextError> {
    use PasteTextError::{Stopped, Unavailable};
    let unavailable = |e: ashpd::Error| Unavailable(e.to_string());

    portal
        .select_devices(
            session,
            SelectDevicesOptions::default()
                .set_devices(BitFlags::from(DeviceType::Keyboard))
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(restore),
        )
        .await
        .map_err(unavailable)?;
    // After the devices are chosen and before the session starts: the
    // portal refuses it at any other time.
    clipboard.request(session, RequestClipboardOptions::default()).await.map_err(unavailable)?;
    let asking = Instant::now();
    let started =
        portal.start(session, None, Default::default()).await.and_then(|r| r.response()).map_err(
            |e| match e {
                ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
                    Stopped(refused.to_string())
                }
                other => unavailable(other),
            },
        )?;
    // Taken now, before the calls below add their own time to the start's.
    let asked = asked_the_person(asking.elapsed());
    *token = started.restore_token().map(str::to_string);
    if !started.devices().contains(DeviceType::Keyboard) {
        return Err(Unavailable("the desktop did not hand over a keyboard".into()));
    }
    if !started.is_clipboard_enabled() {
        return Err(Unavailable("the desktop did not share its clipboard with the session".into()));
    }
    Ok(asked)
}

/// Everything [`paste_text`] does inside its session, up to the point where
/// it is closed. `token` is filled with the session's restore token as soon as
/// the session has one.
async fn paste_text_in(
    portal: &RemoteDesktop,
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    text: &str,
    restore: Option<&str>,
    settle: Duration,
    token: &mut Option<String>,
) -> Result<(), PasteTextError> {
    use PasteTextError::{Stopped, Unavailable};
    let unavailable = |e: ashpd::Error| Unavailable(e.to_string());

    let asked = start_with_clipboard(
        portal,
        clipboard,
        session,
        restore,
        "typing into other applications was not allowed",
        token,
    )
    .await?;

    // Listened for before the text is offered, so no request can come first.
    let ours = format!("{session:?}");
    let transfers = Box::pin(
        clipboard.receive_selection_transfer::<RemoteDesktop>().await.map_err(unavailable)?,
    )
    .map(|(from, mime, serial)| (format!("{from:?}"), mime, serial));
    clipboard
        .set_selection(session, SetSelectionOptions::default().set_mime_types(&TEXT_MIME_TYPES))
        .await
        .map_err(unavailable)?;

    let pressed = AtomicBool::new(false);
    let (answered, mut answers_rx) = mpsc::unbounded_channel();
    let pressing = async {
        tokio::time::sleep(settle_before_press(asked, settle)).await;
        // Marked before the keys, so that only what is asked for because of
        // them is counted (see [`serve`]).
        pressed.store(true, Ordering::Release);
        press_paste(portal, session).await.map_err(Stopped)?;
        let mut answers = Answers::default();
        if let Ok(Some(taken)) = tokio::time::timeout(TRANSFER_WAIT, answers_rx.recv()).await {
            answers.add(taken);
            tokio::time::sleep(REPEAT_GRACE).await;
            while let Ok(taken) = answers_rx.try_recv() {
                answers.add(taken);
            }
        }
        Ok::<_, PasteTextError>(answers)
    };
    let serving = serve(transfers, &ours, &pressed, &answered, move |mime, serial| async move {
        answer(clipboard, session, &mime, serial, text).await
    });
    let answers = tokio::select! {
        done = pressing => done?,
        () = serving => {
            return Err(Stopped("the desktop stopped listening for the text".into()));
        }
    };
    answers.verdict()
}

/// Type `text` into whatever window has the keyboard without taking the
/// keyboard from it: a RemoteDesktop session that also asks for the
/// clipboard (the `Clipboard` portal), offers `text` as the clipboard's
/// contents, presses Ctrl+V, and hands the text over when the window asks
/// for it. No window of ours is shown, so no activation token is needed --
/// what typing from a conversation the wake word began has to do without.
///
/// `restore` and `settle` are [`paste`]'s. The clipboard is the session's
/// while it lasts, and goes with it: what was on it before is not put back,
/// nor is the text left on it.
///
/// The result says whether the caller may still paste another way
/// ([`PasteTextError::falls_back`]): it may when nothing was pressed, and
/// must not when Ctrl+V was pressed and no window took the text.
pub async fn paste_text(text: &str, restore: Option<String>, settle: Duration) -> TextPaste {
    use PasteTextError::Unavailable;
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    register().await;
    let mut token = None;
    let started = async {
        let clipboard = Clipboard::new().await.map_err(|e| Unavailable(e.to_string()))?;
        let portal = RemoteDesktop::new().await.map_err(|e| Unavailable(e.to_string()))?;
        let session = portal
            .create_session(Default::default())
            .await
            .map_err(|e| Unavailable(e.to_string()))?;
        let result = paste_text_in(
            &portal,
            &clipboard,
            &session,
            text,
            restore.as_deref(),
            settle,
            &mut token,
        )
        .await;
        // Closed straight away, as in [`paste`]: the badge in the top bar and
        // the session's hold on the clipboard are let go of at once.
        let _ = session.close().await;
        result
    }
    .await;
    TextPaste { restore_token: token, result: started }
}

/// How long to wait, after Ctrl+Insert, for the window to copy. One that has
/// not by then either has nothing selected or never took the key, and either
/// way there is nothing to read.
const COPY_WAIT: Duration = Duration::from_millis(1500);

/// How long the window has to hand over the text once it has been asked.
const READ_WAIT: Duration = Duration::from_secs(2);

/// How long to let the desktop finish starting the session before pressing,
/// when it did not have to ask the person anything.
const COPY_SETTLE: Duration = Duration::from_millis(100);

/// The most bytes of a selection to take. Over that is refused, not cut:
/// rewriting the front of a selection and pasting it over the whole would
/// lose the rest. (A rewrite takes 8,000 characters, at most 32 KB of them.)
const SELECTION_LIMIT: usize = 64 * 1024;

/// The type VS Code files its own account of a copy under, in a JSON object
/// whose `isFromEmptySelection` says the copy was of the line the caret was on
/// because nothing was selected.
const EDITOR_DATA_TYPE: &str = "vscode-editor-data";

/// Where a window built on Chromium (VS Code is one, as are Electron's
/// applications) keeps the types a page sets by hand -- `vscode-editor-data`
/// among them -- when it copies: Chromium does not offer them to the desktop
/// as types of their own, but pickled together under this one.
const CUSTOM_DATA_TYPE: &str = "chromium/x-web-custom-data";

/// What [`copy_selection`] came to: the session's restore token, kept apart
/// from the outcome for [`TextPaste`]'s reason, and what was selected.
pub struct CopiedText {
    /// The restore token the session ended with, when it got as far as
    /// starting and the desktop sent one. Keep it for the next session.
    pub restore_token: Option<String>,
    /// The selected text -- empty when nothing was selected -- or why it could
    /// not be read, in words fit for the person.
    pub result: Result<String, String>,
}

/// A failure to read the selection, as the sentence the person is told.
fn unread(why: impl std::fmt::Display) -> String {
    format!("Couldn't read what is selected ({why}).")
}

/// Why the bytes of a copy could not all be had.
#[derive(Debug, PartialEq, Eq)]
enum Unread {
    /// More than [`SELECTION_LIMIT`].
    TooLong,
    /// The window did not finish handing it over in time -- and what had come
    /// is not a shorter selection.
    TooSlow,
    /// The pipe could not be read.
    Failed(String),
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong => f.write_str("it is far too long"),
            Self::TooSlow => f.write_str("the application took too long to hand it over"),
            Self::Failed(why) => write!(f, "the clipboard could not be read: {why}"),
        }
    }
}

/// What the `Clipboard` portal says when the selection changes hands: whose
/// session it is told to, the types now on offer, and whether that session is
/// the one offering them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerChange {
    session: String,
    mime_types: Vec<String>,
    session_is_owner: Option<bool>,
}

/// A window's copy of text: the type to read it as, and the type to read an
/// editor's own account of the copy from, when the window gave one.
#[derive(Debug, PartialEq, Eq)]
struct WindowCopy {
    text_type: String,
    editor_type: Option<String>,
}

/// Whether `mime` is a type that says it is UTF-8.
fn names_utf8(mime: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    mime.contains("utf-8") || mime.contains("utf8")
}

/// The type to read text offered as `offered` as, spelt as it was offered:
/// one of [`TEXT_MIME_TYPES`] -- those that say they are UTF-8 first, in the
/// list's order, then the rest, since `text/plain` does not say what it is
/// and `STRING` and `TEXT` are Latin-1 and the locale's. `None` for an offer
/// with no text in it: an image, a list of files.
fn text_type_to_read(offered: &[String]) -> Option<&str> {
    offered
        .iter()
        .filter_map(|o| text_type_index(o).map(|index| (o.as_str(), index)))
        .min_by_key(|&(o, index)| (!names_utf8(o), index))
        .map(|(o, _)| o)
}

/// The type to read an editor's account of a copy from, if one was offered:
/// the type itself, else the container that may hold it.
fn editor_data_type(offered: &[String]) -> Option<&str> {
    [EDITOR_DATA_TYPE, CUSTOM_DATA_TYPE].iter().find_map(|wanted| {
        offered.iter().map(String::as_str).find(|o| o.eq_ignore_ascii_case(wanted))
    })
}

/// What `change` is, when it is a window copying text to the session `ours`.
/// Not an offer the session made itself (it makes none, but one of its own
/// would not be a window's copy), not another session's, and not an offer
/// with no text in it.
fn window_copy(ours: &str, change: &OwnerChange) -> Option<WindowCopy> {
    if change.session != ours || change.session_is_owner == Some(true) {
        return None;
    }
    Some(WindowCopy {
        text_type: text_type_to_read(&change.mime_types)?.to_string(),
        editor_type: editor_data_type(&change.mime_types).map(str::to_string),
    })
}

/// Wait for a window to copy text. `None` when the changes end without one.
async fn next_copy(
    changes: &mut (impl futures_util::Stream<Item = OwnerChange> + Unpin),
    ours: &str,
) -> Option<WindowCopy> {
    while let Some(change) = changes.next().await {
        if let Some(copy) = window_copy(ours, &change) {
            return Some(copy);
        }
    }
    None
}

/// Read a pipe a copy is handed over on to its end, and no further than
/// `limit` bytes or `within` the time. Waits on the pipe rather than blocking
/// a thread on it: a window that never closes its end would hold the thread
/// for good.
async fn read_capped(fd: OwnedFd, limit: usize, within: Duration) -> Result<Vec<u8>, Unread> {
    use std::io::ErrorKind;
    let failed = |e: std::io::Error| Unread::Failed(e.to_string());
    let pipe = pipe::Receiver::from_owned_fd(fd).map_err(failed)?;
    let reading = async {
        let mut got = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            pipe.readable().await.map_err(failed)?;
            match pipe.try_read(&mut chunk) {
                // The window closed its end: that is all of it.
                Ok(0) => return Ok(got),
                Ok(n) if got.len() + n > limit => return Err(Unread::TooLong),
                Ok(n) => got.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(e) => return Err(failed(e)),
            }
        }
    };
    tokio::time::timeout(within, reading).await.unwrap_or(Err(Unread::TooSlow))
}

/// The text of a copy read as `mime`, without the NULs some old X clients end
/// their text with.
///
/// UTF-8, any byte that is not valid turned into the replacement character --
/// except `STRING`, which is Latin-1 by convention and so is read as that,
/// unless it is valid UTF-8 as it stands: what a client that sends UTF-8 under
/// that name sends, and what Latin-1 text with an accent in it almost never is.
fn decode(bytes: &[u8], mime: &str) -> String {
    let text = match std::str::from_utf8(bytes) {
        Ok(utf8) => utf8.to_string(),
        Err(_) if mime.eq_ignore_ascii_case("STRING") => {
            bytes.iter().map(|&byte| char::from(byte)).collect()
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    };
    text.trim_end_matches('\0').to_string()
}

/// A little-endian `u32` at `at` in `data`, moving `at` past it.
fn read_u32(data: &[u8], at: &mut usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let number = u32::from_le_bytes(data.get(*at..end)?.try_into().ok()?);
    *at = end;
    Some(number)
}

/// A string in Chromium's pickle: its length in UTF-16 units, the units
/// themselves, and padding to the next four bytes.
fn read_string16(data: &[u8], at: &mut usize) -> Option<String> {
    let units = usize::try_from(read_u32(data, at)?).ok()?;
    let end = at.checked_add(units.checked_mul(2)?)?;
    let units: Vec<u16> =
        data.get(*at..end)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    *at = end.next_multiple_of(4);
    Some(String::from_utf16_lossy(&units))
}

/// What a Chromium-built window filed under `wanted` in the pickle of the
/// types a page set by hand when it copied: a size, a count, then each type
/// and its data as a string.
fn custom_data_entry(data: &[u8], wanted: &str) -> Option<String> {
    // Past the size the pickle begins with.
    let mut at = 4;
    for _ in 0..read_u32(data, &mut at)? {
        let name = read_string16(data, &mut at)?;
        let value = read_string16(data, &mut at)?;
        if name == wanted {
            return Some(value);
        }
    }
    None
}

/// Whether `data`, read as the editor's type `mime`, says that what was
/// copied was a line taken because nothing was selected.
fn marks_empty_selection(mime: &str, data: &[u8]) -> bool {
    let editor_data = if mime.eq_ignore_ascii_case(CUSTOM_DATA_TYPE) {
        custom_data_entry(data, EDITOR_DATA_TYPE)
    } else {
        Some(String::from_utf8_lossy(data).into_owned())
    };
    editor_data
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|editor| editor.get("isFromEmptySelection")?.as_bool())
        == Some(true)
}

/// Whether the editor that copied says it had nothing selected and copied
/// the line the caret was on instead, as VS Code and the editors built on it
/// do on Ctrl+Insert. A rewrite of that line pasted at the caret would type
/// the line a second time, so it is taken as nothing selected. An editor
/// whose account cannot be read is taken to have given none.
async fn copied_a_line_for_nothing(
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    copy: &WindowCopy,
) -> bool {
    let Some(mime) = &copy.editor_type else { return false };
    let Ok(fd) = clipboard.selection_read(session, mime).await else { return false };
    let Ok(data) = read_capped(fd.into(), SELECTION_LIMIT, READ_WAIT).await else { return false };
    marks_empty_selection(mime, &data)
}

/// Everything [`copy_selection`] does inside its session, up to the point
/// where it is closed. `token` is filled with the session's restore token as
/// soon as the session has one.
async fn copy_selection_in(
    portal: &RemoteDesktop,
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    restore: Option<&str>,
    token: &mut Option<String>,
) -> Result<String, String> {
    let asked = start_with_clipboard(
        portal,
        clipboard,
        session,
        restore,
        "reading from other applications was not allowed",
        token,
    )
    .await
    .map_err(unread)?;

    // Listened for before the key is pressed, so the copy cannot come first.
    let ours = format!("{session:?}");
    let mut changes = Box::pin(
        clipboard.receive_selection_owner_changed::<RemoteDesktop>().await.map_err(unread)?,
    )
    .map(|(from, change)| OwnerChange {
        session: format!("{from:?}"),
        mime_types: change.mime_types().to_vec(),
        session_is_owner: change.session_is_owner(),
    });
    tokio::time::sleep(settle_before_press(asked, COPY_SETTLE)).await;
    // Whatever the clipboard did on its own meanwhile -- a clipboard manager
    // taking it over, say -- is not the window copying its selection.
    while changes.next().now_or_never().flatten().is_some() {}
    press_with_control(portal, session, KEYSYM_INSERT).await.map_err(unread)?;

    let copy = match tokio::time::timeout(COPY_WAIT, next_copy(&mut changes, &ours)).await {
        Ok(Some(copy)) => copy,
        // Nothing copied: nothing was selected.
        Err(_) => return Ok(String::new()),
        Ok(None) => return Err(unread("the desktop stopped listening for the clipboard")),
    };
    if copied_a_line_for_nothing(clipboard, session, &copy).await {
        return Ok(String::new());
    }
    let fd = clipboard.selection_read(session, &copy.text_type).await.map_err(unread)?;
    let bytes = read_capped(fd.into(), SELECTION_LIMIT, READ_WAIT).await.map_err(unread)?;
    Ok(decode(&bytes, &copy.text_type))
}

/// Read what is selected in whatever window has the keyboard, without taking
/// the keyboard from it: a RemoteDesktop session that asks for the clipboard,
/// presses Ctrl+Insert, waits for the clipboard to change hands, and reads the
/// text back through the `Clipboard` portal. The window keeps its selection,
/// so a paste after this goes over it. No window of ours is shown, so no
/// activation token is needed -- what reading it from a conversation the wake
/// word began has to do without.
///
/// Ctrl+Insert, not Ctrl+C: it is copy in GTK, Qt, browsers, Electron and VTE
/// terminals alike, and in a terminal Ctrl+C would interrupt the program
/// running there.
///
/// `restore` is [`paste`]'s. Where nothing was selected the window copies
/// nothing and the text comes back empty. The clipboard is left holding the
/// copy, as any copy does; what it held before is not put back.
pub async fn copy_selection(restore: Option<String>) -> CopiedText {
    // With the pastes: a copy and a paste racing would each answer the
    // other's reading of the clipboard.
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    register().await;
    let mut token = None;
    let result = async {
        let clipboard = Clipboard::new().await.map_err(unread)?;
        let portal = RemoteDesktop::new().await.map_err(unread)?;
        let session = portal.create_session(Default::default()).await.map_err(unread)?;
        let result =
            copy_selection_in(&portal, &clipboard, &session, restore.as_deref(), &mut token).await;
        // Closed straight away, as in [`paste`]: nothing more is needed of
        // the keyboard, and the badge in the top bar goes with it.
        let _ = session.close().await;
        result
    }
    .await;
    CopiedText { restore_token: token, result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_become_xdg_triggers() {
        assert_eq!(xdg_trigger("Ctrl+Alt+Space"), "CTRL+ALT+space");
        assert_eq!(xdg_trigger("CommandOrControl+Shift+K"), "CTRL+SHIFT+k");
        assert_eq!(xdg_trigger("Super+F9"), "LOGO+F9");
    }

    #[test]
    fn gnome_trigger_descriptions_read_as_shortcuts() {
        assert_eq!(readable_trigger("Press <Control><Alt>space"), "Ctrl+Alt+Space");
        assert_eq!(readable_trigger("Press <Shift><Control>space"), "Shift+Ctrl+Space");
        assert_eq!(readable_trigger("Alt+W"), "Alt+W");
    }

    #[test]
    fn the_text_is_offered_as_every_type_a_window_asks_for() {
        assert_eq!(TEXT_MIME_TYPES[0], "text/plain;charset=utf-8");
        for wanted in ["text/plain", "UTF8_STRING", "STRING", "TEXT"] {
            assert!(TEXT_MIME_TYPES.contains(&wanted), "{wanted} is not offered");
        }
    }

    #[test]
    fn only_the_types_offered_are_given_the_text() {
        for mime in TEXT_MIME_TYPES {
            assert_eq!(payload_for("héllo", mime), Some("héllo".as_bytes()));
        }
        // GTK is not always careful about the case of the charset.
        assert_eq!(payload_for("hi", "text/plain;charset=UTF-8"), Some("hi".as_bytes()));
        assert_eq!(payload_for("hi", "text/html"), None);
        assert_eq!(payload_for("hi", "image/png"), None);
        assert_eq!(payload_for("hi", ""), None);
    }

    #[test]
    fn only_an_unavailable_desktop_falls_back_to_another_way() {
        assert!(PasteTextError::Unavailable("no portal".into()).falls_back());
        // Pressed, and nobody took it: another try would type it twice if the
        // application was only slow.
        assert!(!PasteTextError::Stopped("not taken".into()).falls_back());
        assert_eq!(PasteTextError::Stopped("not taken".into()).message(), "not taken");
        assert_eq!(PasteTextError::Unavailable("no portal".into()).to_string(), "no portal");
    }

    #[test]
    fn a_paste_counts_only_when_a_window_was_given_the_text() {
        let mut answers = Answers::default();
        // Ctrl+V pressed and nothing asked: stopped, not a fall back.
        let none = answers.verdict().unwrap_err();
        assert!(!none.falls_back());
        assert!(none.message().contains("did not take the text"));
        // Asked, but the text did not get there.
        answers.add(false);
        let failed = answers.verdict().unwrap_err();
        assert!(!failed.falls_back());
        assert!(failed.message().contains("could not be handed over"));
        // A second request that went through is enough.
        answers.add(true);
        assert_eq!(answers.verdict(), Ok(()));
        // And one that went through is enough on its own.
        let mut once = Answers::default();
        once.add(true);
        assert_eq!(once.verdict(), Ok(()));
    }

    #[test]
    fn a_question_from_the_desktop_lengthens_the_wait_before_pressing() {
        let settle = Duration::from_millis(200);
        assert!(!asked_the_person(Duration::from_millis(30)));
        assert!(!asked_the_person(ASKED_AFTER));
        assert!(asked_the_person(Duration::from_secs(8)));
        assert_eq!(settle_before_press(false, settle), settle);
        assert_eq!(settle_before_press(true, settle), settle + ASKED_SETTLE);
    }

    /// A pipe, as `SelectionWrite` hands one over: the end to write to, and
    /// the end the window reads.
    fn pipe() -> (OwnedFd, std::io::PipeReader) {
        let (reader, writer) = std::io::pipe().unwrap();
        (writer.into(), reader)
    }

    #[test]
    fn the_text_goes_down_the_pipe_and_the_pipe_is_closed() {
        use std::io::Read;
        let (writer, mut reader) = pipe();
        let deadline = Instant::now() + Duration::from_secs(2);
        write_all_and_close(writer, "type this, héllo".as_bytes(), deadline).unwrap();
        // Closed, or the read would wait for more.
        let mut got = String::new();
        reader.read_to_string(&mut got).unwrap();
        assert_eq!(got, "type this, héllo");
    }

    #[test]
    fn a_text_longer_than_the_pipe_is_written_while_the_window_reads() {
        use std::io::Read;
        let text = "word ".repeat(60_000);
        let (writer, mut reader) = pipe();
        let reading = std::thread::spawn(move || {
            let mut got = String::new();
            reader.read_to_string(&mut got).unwrap();
            got
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        write_all_and_close(writer, text.as_bytes(), deadline).unwrap();
        assert_eq!(reading.join().unwrap(), text);
    }

    #[test]
    fn a_window_that_stopped_reading_is_a_failed_write() {
        let (writer, reader) = pipe();
        drop(reader);
        let deadline = Instant::now() + Duration::from_secs(2);
        let err = write_all_and_close(writer, b"too late", deadline).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn only_requests_made_after_ctrl_v_are_counted_but_all_are_answered() {
        use futures_util::stream;
        let pressed = AtomicBool::new(false);
        let (answered, mut counted) = mpsc::unbounded_channel();
        let answered_serials = std::sync::Mutex::new(Vec::new());
        let me = || "ours".to_string();
        let transfers = stream::iter([
            // A clipboard manager reading the new clipboard as it lands.
            (me(), "text/plain".to_string(), 1),
            (me(), "UTF8_STRING".to_string(), 2),
            // Another session's, whenever.
            ("other".to_string(), "text/plain".to_string(), 3),
            // Then Ctrl+V, and the window typed into asking for the text.
            (me(), "text/plain;charset=utf-8".to_string(), 4),
            (me(), "STRING".to_string(), 5),
        ]);
        serve(transfers, "ours", &pressed, &answered, |_mime, serial| {
            answered_serials.lock().unwrap().push(serial);
            if serial == 2 {
                // Ctrl+V comes after the second answer.
                pressed.store(true, Ordering::Release);
            }
            std::future::ready(serial != 5)
        })
        .await;
        drop(answered);
        assert_eq!(*answered_serials.lock().unwrap(), [1, 2, 4, 5]);
        let mut said = Vec::new();
        while let Some(taken) = counted.recv().await {
            said.push(taken);
        }
        assert_eq!(said, [true, false]);
    }

    fn offered(types: &[&str]) -> Vec<String> {
        types.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn the_best_text_type_on_offer_is_read() {
        let all =
            offered(&["TEXT", "STRING", "text/plain", "UTF8_STRING", "text/plain;charset=utf-8"]);
        assert_eq!(text_type_to_read(&all), Some("text/plain;charset=utf-8"));
        // Whatever order they come in, and whatever else is offered.
        let x11 = offered(&["TARGETS", "TEXT", "STRING", "UTF8_STRING", "text/html"]);
        assert_eq!(text_type_to_read(&x11), Some("UTF8_STRING"));
        let plain = offered(&["image/png", "STRING", "text/plain"]);
        assert_eq!(text_type_to_read(&plain), Some("text/plain"));
        assert_eq!(text_type_to_read(&offered(&["TEXT", "STRING"])), Some("STRING"));
        assert_eq!(text_type_to_read(&offered(&["TEXT"])), Some("TEXT"));
    }

    #[test]
    fn everything_read_is_something_that_is_offered_and_the_other_way_round() {
        // One list of types, so that what is typed and what is read cannot
        // part ways.
        for mime in TEXT_MIME_TYPES {
            assert_eq!(text_type_to_read(&offered(&[mime])), Some(mime));
        }
        assert!(text_type_index("text/html").is_none());
    }

    #[test]
    fn a_type_is_read_as_the_window_spelt_it() {
        let gtk = offered(&["text/html", "text/plain;charset=UTF-8"]);
        assert_eq!(text_type_to_read(&gtk), Some("text/plain;charset=UTF-8"));
    }

    #[test]
    fn an_offer_with_no_text_has_nothing_to_read() {
        assert_eq!(text_type_to_read(&[]), None);
        assert_eq!(text_type_to_read(&offered(&["image/png", "text/uri-list"])), None);
        // Text, but not as anything a window of today asks for or gives.
        assert_eq!(text_type_to_read(&offered(&["text/html", "text/rtf"])), None);
    }

    fn change(session: &str, types: &[&str], owner: Option<bool>) -> OwnerChange {
        OwnerChange {
            session: session.to_string(),
            mime_types: offered(types),
            session_is_owner: owner,
        }
    }

    /// The copy a window made of text, as read as `text_type`.
    fn copy_of(text_type: &str, editor_type: Option<&str>) -> WindowCopy {
        WindowCopy { text_type: text_type.to_string(), editor_type: editor_type.map(String::from) }
    }

    #[test]
    fn only_another_windows_copy_of_text_is_a_copy() {
        let text = ["text/plain;charset=utf-8", "text/html"];
        let read_as = Some(copy_of("text/plain;charset=utf-8", None));
        // The window copied: owner is not this session, or the desktop did
        // not say.
        assert_eq!(window_copy("ours", &change("ours", &text, Some(false))), read_as);
        assert_eq!(window_copy("ours", &change("ours", &text, None)), read_as);
        // The session's own offer.
        assert_eq!(window_copy("ours", &change("ours", &text, Some(true))), None);
        // Another session's.
        assert_eq!(window_copy("ours", &change("other", &text, Some(false))), None);
        // Something copied that is not text.
        assert_eq!(window_copy("ours", &change("ours", &["image/png"], Some(false))), None);
        assert_eq!(window_copy("ours", &change("ours", &[], Some(false))), None);
    }

    #[test]
    fn an_editors_own_account_of_a_copy_is_read_when_it_gives_one() {
        let direct = change("ours", &["text/plain", "vscode-editor-data"], Some(false));
        assert_eq!(
            window_copy("ours", &direct),
            Some(copy_of("text/plain", Some("vscode-editor-data")))
        );
        // As Chromium offers it: pickled with the page's other types.
        let chromium = change("ours", &["UTF8_STRING", "chromium/x-web-custom-data"], Some(false));
        assert_eq!(
            window_copy("ours", &chromium),
            Some(copy_of("UTF8_STRING", Some("chromium/x-web-custom-data")))
        );
        // The type itself first, when both are there.
        let both =
            change("ours", &["chromium/x-web-custom-data", "vscode-editor-data", "STRING"], None);
        assert_eq!(window_copy("ours", &both), Some(copy_of("STRING", Some("vscode-editor-data"))));
        // And no account for a window that gives none.
        let plain = change("ours", &["text/plain"], Some(false));
        assert_eq!(window_copy("ours", &plain), Some(copy_of("text/plain", None)));
    }

    #[tokio::test]
    async fn the_wait_for_a_copy_skips_what_is_not_one() {
        use futures_util::stream;
        let mut changes = stream::iter([
            change("other", &["text/plain"], Some(false)),
            change("ours", &["UTF8_STRING"], Some(true)),
            // A window clearing the clipboard before it sets it.
            change("ours", &[], Some(false)),
            change("ours", &["image/png"], Some(false)),
            change("ours", &["STRING", "UTF8_STRING"], Some(false)),
            change("ours", &["text/plain;charset=utf-8"], Some(false)),
        ]);
        assert_eq!(next_copy(&mut changes, "ours").await, Some(copy_of("UTF8_STRING", None)));
        // The next is the one after it, not the same again.
        assert_eq!(
            next_copy(&mut changes, "ours").await,
            Some(copy_of("text/plain;charset=utf-8", None))
        );
        assert_eq!(next_copy(&mut changes, "ours").await, None);
    }

    #[tokio::test]
    async fn the_wait_for_a_copy_that_never_comes_ends_with_the_changes() {
        use futures_util::stream;
        let mut none = stream::iter([change("ours", &["image/png"], Some(false))]);
        assert_eq!(next_copy(&mut none, "ours").await, None);
    }

    /// A pipe as `SelectionRead` hands one over: the end the copy is read
    /// from, and the end the window writes.
    fn read_pipe() -> (OwnedFd, std::io::PipeWriter) {
        let (reader, writer) = std::io::pipe().unwrap();
        (reader.into(), writer)
    }

    #[tokio::test]
    async fn a_copy_is_read_to_where_the_window_closes_its_end() {
        let (fd, mut writer) = read_pipe();
        writer.write_all("select me, héllo".as_bytes()).unwrap();
        drop(writer);
        let got = read_capped(fd, 1024, Duration::from_secs(2)).await.unwrap();
        assert_eq!(decode(&got, "UTF8_STRING"), "select me, héllo");
    }

    #[tokio::test]
    async fn a_copy_of_nothing_is_empty() {
        let (fd, writer) = read_pipe();
        drop(writer);
        assert_eq!(read_capped(fd, 1024, Duration::from_secs(2)).await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn a_copy_that_arrives_in_pieces_is_read_whole() {
        let (fd, mut writer) = read_pipe();
        let text = "word ".repeat(10_000);
        let sent = text.clone();
        let writing = std::thread::spawn(move || {
            for piece in sent.as_bytes().chunks(1000) {
                writer.write_all(piece).unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let got = read_capped(fd, SELECTION_LIMIT, Duration::from_secs(5)).await.unwrap();
        writing.join().unwrap();
        assert_eq!(decode(&got, "text/plain;charset=utf-8"), text);
    }

    #[tokio::test]
    async fn a_copy_over_the_limit_is_refused_and_not_cut() {
        let (fd, mut writer) = read_pipe();
        writer.write_all(&[b'a'; 100]).unwrap();
        drop(writer);
        assert_eq!(read_capped(fd, 99, Duration::from_secs(2)).await, Err(Unread::TooLong));
        // Exactly the limit is fine.
        let (fd, mut writer) = read_pipe();
        writer.write_all(&[b'a'; 99]).unwrap();
        drop(writer);
        assert_eq!(read_capped(fd, 99, Duration::from_secs(2)).await.unwrap().len(), 99);
    }

    #[tokio::test]
    async fn a_copy_that_never_finishes_is_not_a_shorter_one() {
        let (fd, mut writer) = read_pipe();
        writer.write_all(b"the first half").unwrap();
        // Held open past the wait: the window is still writing, or stuck.
        let got = read_capped(fd, 1024, Duration::from_millis(150)).await;
        assert_eq!(got, Err(Unread::TooSlow));
        drop(writer);
    }

    #[tokio::test]
    async fn something_that_is_not_a_pipe_is_not_read() {
        let fd: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        assert!(matches!(
            read_capped(fd, 1024, Duration::from_secs(1)).await,
            Err(Unread::Failed(_))
        ));
    }

    #[test]
    fn a_copy_is_decoded_as_utf8_and_loses_its_nuls() {
        for mime in ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT"] {
            assert_eq!(decode("héllo\nwörld".as_bytes(), mime), "héllo\nwörld");
            // Not UTF-8: the replacement character, not an error or a gap.
            assert_eq!(decode(b"caf\xe9", mime), "caf\u{fffd}");
            // A trailing NUL, as an old X client sends one.
            assert_eq!(decode(b"text\0", mime), "text");
            assert_eq!(decode(b"", mime), "");
        }
    }

    #[test]
    fn a_copy_read_as_string_is_latin_1_unless_it_is_utf8() {
        // "café" as ISO 8859-1 has a lone 0xe9, which is not UTF-8.
        assert_eq!(decode(b"caf\xe9", "STRING"), "café");
        assert_eq!(decode(b"caf\xe9", "string"), "café");
        assert_eq!(decode(b"\xa3 5\xb0\0", "STRING"), "£ 5°");
        // A client that sends UTF-8 under the name is read as what it sent.
        assert_eq!(decode("café".as_bytes(), "STRING"), "café");
        assert_eq!(decode(b"plain ascii", "STRING"), "plain ascii");
    }

    /// Chromium's pickle of the types a page set by hand: its size, a count,
    /// and each type and its data as UTF-16 with its length and padding.
    fn custom_data(entries: &[(&str, &str)]) -> Vec<u8> {
        fn string16(out: &mut Vec<u8>, text: &str) {
            let units: Vec<u16> = text.encode_utf16().collect();
            out.extend((units.len() as u32).to_le_bytes());
            out.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
            out.resize(out.len().next_multiple_of(4), 0);
        }
        let mut payload = (entries.len() as u32).to_le_bytes().to_vec();
        for (name, value) in entries {
            string16(&mut payload, name);
            string16(&mut payload, value);
        }
        let mut pickle = (payload.len() as u32).to_le_bytes().to_vec();
        pickle.extend(payload);
        pickle
    }

    #[test]
    fn an_editor_that_copied_a_line_for_nothing_says_so() {
        let empty = br#"{"version":1,"isFromEmptySelection":true,"multicursorText":null,"mode":"markdown"}"#;
        let selected = br#"{"version":1,"isFromEmptySelection":false,"mode":"rust"}"#;
        assert!(marks_empty_selection("vscode-editor-data", empty));
        assert!(!marks_empty_selection("vscode-editor-data", selected));
        // No such key, the wrong kind of value, and what is not JSON at all.
        assert!(!marks_empty_selection("vscode-editor-data", br#"{"mode":"rust"}"#));
        assert!(!marks_empty_selection(
            "vscode-editor-data",
            br#"{"isFromEmptySelection":"true"}"#
        ));
        assert!(!marks_empty_selection("vscode-editor-data", b"isFromEmptySelection"));
        assert!(!marks_empty_selection("vscode-editor-data", b""));
    }

    #[test]
    fn the_account_is_found_among_the_types_chromium_pickles() {
        let json = r#"{"version":1,"isFromEmptySelection":true,"mode":"plaintext"}"#;
        let pickled = custom_data(&[("text/x-other", "ünrelated"), ("vscode-editor-data", json)]);
        assert!(marks_empty_selection("chromium/x-web-custom-data", &pickled));
        let selected = custom_data(&[("vscode-editor-data", r#"{"isFromEmptySelection":false}"#)]);
        assert!(!marks_empty_selection("chromium/x-web-custom-data", &selected));
        // Some other page's custom data, with no account of a copy in it.
        let other = custom_data(&[("text/x-other", json)]);
        assert!(!marks_empty_selection("chromium/x-web-custom-data", &other));
        assert!(!marks_empty_selection("chromium/x-web-custom-data", &custom_data(&[])));
    }

    #[test]
    fn a_pickle_that_ends_early_or_lies_about_its_size_is_not_read() {
        let pickled = custom_data(&[("vscode-editor-data", r#"{"isFromEmptySelection":true}"#)]);
        assert_eq!(
            custom_data_entry(&pickled, "vscode-editor-data").as_deref(),
            Some(r#"{"isFromEmptySelection":true}"#)
        );
        // Cut anywhere short of the end: nothing, and no panic.
        // (Not into the padding after the last string, which is not read.)
        for end in 0..pickled.len() - 3 {
            assert_eq!(custom_data_entry(&pickled[..end], "vscode-editor-data"), None, "{end}");
        }
        // A count, and a length, far past the data.
        let mut lying = custom_data(&[]);
        lying[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(custom_data_entry(&lying, "vscode-editor-data"), None);
        let huge = [0u8, 0, 0, 0, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(custom_data_entry(&huge, "vscode-editor-data"), None);
    }

    #[test]
    fn a_failure_to_read_is_a_sentence_for_the_person() {
        assert_eq!(
            unread("the desktop did not hand over a keyboard"),
            "Couldn't read what is selected (the desktop did not hand over a keyboard)."
        );
        assert_eq!(unread(Unread::TooLong), "Couldn't read what is selected (it is far too long).");
    }
}
