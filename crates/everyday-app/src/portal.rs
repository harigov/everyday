//! The desktop portals Wayland leaves as the only way to do two things.
//!
//! An application on Wayland cannot claim a key for the whole desktop, and
//! cannot press a key in another application's window. Both are deliberate,
//! and both have a sanctioned door: the `GlobalShortcuts` portal, which asks
//! the person once which key and then tells this process when it is pressed,
//! and the `RemoteDesktop` portal, which asks once whether this process may
//! type and then lets it. Everything here is those two doors.
//!
//! # Which application is asking
//!
//! A portal files what it was told under the asking application's id, and a
//! process outside a sandbox has none until it registers one -- which only
//! succeeds for an id with a desktop file installed under that name. So
//! [`register`] tries the names Every Day might be installed under and keeps
//! going without one if none matches: unregistered, the portals still work,
//! the dialogs just do not say whose they are, and what was granted is
//! filed under the empty id.
//!
//! Checked against GNOME 50's portal in a headless shell: the shortcut dialog
//! appears once, and a second session binding the same ids is answered
//! without one; the remote-desktop dialog appears once, and its restore
//! token starts every later session without one.

use std::sync::Arc;
use std::time::Duration;

use ashpd::desktop::PersistMode;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::enumflags2::BitFlags;
use futures_util::StreamExt;
use tokio::sync::{OnceCell, mpsc, watch};

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

/// The ids Every Day might be installed under: whatever desktop file
/// launched it, then the bundle identifier. `everyday-app` and `Every Day`,
/// the other two names its desktop files go by, are not valid ids.
fn app_ids() -> Vec<String> {
    let launched = std::env::var("GIO_LAUNCHED_DESKTOP_FILE").ok().and_then(|path| {
        let name = std::path::Path::new(&path).file_name()?.to_str()?.to_string();
        name.strip_suffix(".desktop").map(str::to_string)
    });
    launched.into_iter().chain(std::iter::once("app.everyday.journal".to_string())).collect()
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
}

impl Shortcuts {
    /// Connect to the portal, or `None` where there is no GlobalShortcuts
    /// portal to connect to. `on_activate` is called with the id of the key
    /// pressed and the activation token the desktop sent with it, which is
    /// what lets the window that answers take focus.
    ///
    /// `parent` is the window the desktop's dialog -- the first time, when
    /// it asks which keys -- should open over; see [`parent_of`].
    pub async fn connect(
        parent: Option<ashpd::WindowIdentifier>,
        on_activate: impl Fn(&str, Option<String>) + Send + Sync + 'static,
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
        let changed = portal.receive_shortcuts_changed().await.ok();
        let (wanted_tx, wanted_rx) = mpsc::unbounded_channel();
        let (bound_tx, bound_rx) = watch::channel(None);
        tauri::async_runtime::spawn(run(
            portal,
            parent,
            wanted_rx,
            bound_tx,
            Box::pin(activated),
            changed.map(|c| Box::pin(c) as _),
            Arc::new(on_activate),
        ));
        Some(Self { wanted: wanted_tx, bound: bound_rx })
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
type Changed = Stream<ashpd::desktop::global_shortcuts::ShortcutsChanged>;
/// Called with the id of a key pressed and the activation token sent with it.
type OnActivate = Arc<dyn Fn(&str, Option<String>) + Send + Sync>;

async fn run(
    portal: GlobalShortcuts,
    parent: Option<ashpd::WindowIdentifier>,
    mut wanted: mpsc::UnboundedReceiver<Vec<Wanted>>,
    bound: watch::Sender<Option<Result<Bound, String>>>,
    mut activated: Activated,
    mut changed: Option<Changed>,
    on_activate: OnActivate,
) {
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
                on_activate(a.shortcut_id(), token);
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
        let asking = std::time::Instant::now();
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
        // A start that took this long waited on the person: the desktop's
        // dialog was up, and has only just handed the keyboard back to the
        // window behind it.
        let asked = asking.elapsed() > Duration::from_millis(400);
        tokio::time::sleep(if asked { settle + ASKED_SETTLE } else { settle }).await;
        for (keysym, state) in [
            (KEYSYM_CONTROL_L, KeyState::Pressed),
            (KEYSYM_V, KeyState::Pressed),
            (KEYSYM_V, KeyState::Released),
            (KEYSYM_CONTROL_L, KeyState::Released),
        ] {
            portal
                .notify_keyboard_keysym(&session, keysym, state, Default::default())
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(started.restore_token().map(str::to_string))
    }
    .await;
    // Closed straight away: an open session is a "being controlled" badge in
    // the top bar, and there is nothing more to type.
    let _ = session.close().await;
    result
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
}
