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
//! otherwise have to be focused to do.
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
//! [`register`] tries the names Karma Yoga might be installed under and keeps
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
use futures_util::StreamExt;
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

/// The ids Karma Yoga might be installed under: whatever desktop file
/// launched it, then the bundle identifier. `karmayoga-app` and `Karma Yoga`,
/// the other two names its desktop files go by, are not valid ids.
fn app_ids() -> Vec<String> {
    let launched = std::env::var("GIO_LAUNCHED_DESKTOP_FILE").ok().and_then(|path| {
        let name = std::path::Path::new(&path).file_name()?.to_str()?.to_string();
        name.strip_suffix(".desktop").map(str::to_string)
    });
    launched.into_iter().chain(std::iter::once("app.karmayogi.KarmaYoga".to_string())).collect()
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

/// The XKB keysyms for the two keys a paste is.
const KEYSYM_CONTROL_L: i32 = 0xffe3;
const KEYSYM_V: i32 = 0x76;

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

/// Press Ctrl+V, in whatever window has the keyboard.
async fn press_paste(
    portal: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
) -> Result<(), String> {
    for (keysym, state) in [
        (KEYSYM_CONTROL_L, KeyState::Pressed),
        (KEYSYM_V, KeyState::Pressed),
        (KEYSYM_V, KeyState::Released),
        (KEYSYM_CONTROL_L, KeyState::Released),
    ] {
        portal
            .notify_keyboard_keysym(session, keysym, state, Default::default())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
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
    TEXT_MIME_TYPES
        .iter()
        .any(|offered| offered.eq_ignore_ascii_case(mime))
        .then_some(text.as_bytes())
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
                    Stopped("typing into other applications was not allowed".to_string())
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
}
