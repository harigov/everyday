//! wlroots compositors (Sway, Hyprland, river, Wayfire, labwc, niri…):
//! focus from the foreign-toplevel protocol, idleness from ext-idle-notify,
//! the lock from logind.
//!
//! # Focus
//!
//! `zwlr_foreign_toplevel_manager_v1` is how taskbars learn about windows:
//! the compositor announces a handle per toplevel and keeps sending its
//! `app_id`, `title` and `state`, each batch closed by `done`. The state is
//! an array of native-endian `u32`s, and the toplevel whose array holds
//! `activated` (2) is the one with focus. Being a stream of events, it needs
//! an event loop: [`Wayland`] runs one on a thread of its own, with its own
//! connection and queue, and keeps what it last heard in a mutex that the
//! watcher's calls read.
//!
//! GNOME and KDE do not offer the protocol (they have their own backends);
//! when the global is missing, [`Wlroots::connect`] says so after the first
//! roundtrip, so [`crate::detect`] can fall back without waiting.
//!
//! # Idle
//!
//! `ext_idle_notifier_v1` does not tell how long the seat has been idle,
//! only that it has *become* idle after a timeout the client chose, and
//! later that it resumed. So a notification is asked for at a short timeout
//! ([`IDLE_TIMEOUT`]), the instant it fires is kept, and the idle time is
//! that timeout plus the time since -- or zero while input keeps coming.
//! Version 2's `get_input_idle_notification` is preferred where offered: it
//! ignores idle inhibitors, so a video player keeping the screen awake does
//! not count as the person typing (the browser extension is what counts a
//! playing video as attention, per the plan).
//!
//! KDE's backend borrows this half alone ([`Wayland::connect`] with
//! [`Want::IdleOnly`]): KWin implements ext-idle-notify, and its D-Bus
//! screen saver will not report idle time on Wayland.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};

use crate::linux::{DesktopNames, Logind};
use crate::{Backend, Error, Focused, Result, Watcher};

/// How long without input before the compositor says "idle". Short, so the
/// idle time the shell sees is right to within this; the shell's own
/// threshold is minutes.
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the compositor has to answer the first roundtrip before the
/// backend counts as unavailable.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);

/// `zwlr_foreign_toplevel_handle_v1.state.activated`.
const STATE_ACTIVATED: u32 = 2;

/// The watcher for wlroots compositors.
pub(crate) struct Wlroots {
    wayland: Wayland,
    logind: Logind,
    names: DesktopNames,
}

impl Wlroots {
    /// Connect, bind the foreign-toplevel manager, and hear the windows
    /// already open. [`Error::Unavailable`] when there is no Wayland display
    /// or the compositor does not offer the protocol.
    pub(crate) fn connect() -> Result<Self> {
        Ok(Self {
            wayland: Wayland::connect(Want::Toplevels)?,
            logind: Logind::new(),
            names: DesktopNames::new(),
        })
    }
}

impl Watcher for Wlroots {
    fn backend(&self) -> Backend {
        Backend::Wlroots
    }

    fn focused(&mut self) -> Result<Option<Focused>> {
        let Some(window) = self.wayland.focused()? else {
            return Ok(None);
        };
        let name = self.names.name(&window.app_id).unwrap_or_default();
        Ok(Some(Focused { app_id: window.app_id, name, title: window.title }))
    }

    fn idle(&mut self) -> Result<Duration> {
        self.wayland.idle()
    }

    fn locked(&mut self) -> Result<bool> {
        self.logind.locked()
    }
}

/// What a [`Wayland`] connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    /// Focus and idleness: the foreign-toplevel manager is required.
    Toplevels,
    /// Idleness alone, for a compositor with its own way to report focus.
    IdleOnly,
}

/// The focused toplevel as the compositor names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) app_id: String,
    pub(crate) title: String,
}

/// What the event loop last heard, read by the watcher's calls.
#[derive(Debug)]
struct Shared {
    focused: Option<Window>,
    idle: Idle,
    /// Why the event loop stopped, once it has: the compositor went away
    /// or withdrew the toplevel manager.
    ended: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Idle {
    /// The compositor offers no ext-idle-notify (or no seat to watch).
    NotOffered,
    /// Input within the last [`IDLE_TIMEOUT`].
    Active,
    /// The notification fired at this instant.
    IdledAt(Instant),
}

impl Idle {
    /// The time since the last input, as far as the notifications tell.
    fn elapsed(self, now: Instant) -> Option<Duration> {
        match self {
            Idle::NotOffered => None,
            Idle::Active => Some(Duration::ZERO),
            Idle::IdledAt(at) => Some(IDLE_TIMEOUT + now.saturating_duration_since(at)),
        }
    }
}

/// A Wayland connection with an event loop on its own thread.
pub(crate) struct Wayland {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    /// Kept to wake the loop when this is dropped (see [`Drop`]).
    connection: Connection,
    queue: QueueHandle<Loop>,
}

/// What the loop thread hands back once it has started.
type Started = Result<(Connection, QueueHandle<Loop>)>;

impl Wayland {
    /// Connect to `WAYLAND_DISPLAY`, bind what `want` needs, and wait for
    /// the first roundtrip -- at most [`STARTUP_TIMEOUT`].
    pub(crate) fn connect(want: Want) -> Result<Self> {
        let shared =
            Arc::new(Mutex::new(Shared { focused: None, idle: Idle::NotOffered, ended: None }));
        let stop = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::channel::<Started>();
        let thread =
            std::thread::Builder::new().name(String::from("everyday-focus-wayland")).spawn({
                let shared = Arc::clone(&shared);
                let stop = Arc::clone(&stop);
                move || run(want, shared, stop, started_tx)
            });
        if let Err(e) = thread {
            return Err(Error::Failed(format!("could not start the Wayland thread: {e}")));
        }
        match started_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok((connection, queue))) => Ok(Self { shared, stop, connection, queue }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // The thread is stuck in its first roundtrip; tell it to
                // give up should the compositor ever answer.
                stop.store(true, Ordering::Relaxed);
                Err(Error::Unavailable(String::from(
                    "the Wayland compositor did not answer in time",
                )))
            }
        }
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn focused(&self) -> Result<Option<Window>> {
        let shared = self.shared();
        if let Some(why) = &shared.ended {
            return Err(Error::Failed(why.clone()));
        }
        Ok(shared.focused.clone())
    }

    pub(crate) fn idle(&self) -> Result<Duration> {
        let shared = self.shared();
        if let Some(why) = &shared.ended {
            return Err(Error::Failed(why.clone()));
        }
        shared.idle.elapsed(Instant::now()).ok_or_else(|| {
            Error::Unavailable(String::from(
                "this compositor offers no ext-idle-notify to tell idleness by",
            ))
        })
    }

    /// Whether the compositor offered idle notifications.
    pub(crate) fn idle_offered(&self) -> bool {
        self.shared().idle != Idle::NotOffered
    }
}

impl Drop for Wayland {
    /// Stop the loop. It is blocked waiting for events, so a `sync` is sent
    /// for the compositor to answer: its `done` wakes the loop, which sees
    /// the flag, tidies up and returns. Not joined -- a compositor that
    /// never answers must not hang whoever drops this.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _callback = self.connection.display().sync(&self.queue, ());
        let _ = self.connection.flush();
    }
}

/// The loop thread: connect, bind, roundtrip, report, then dispatch until
/// told to stop or the connection fails.
fn run(
    want: Want,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    started: mpsc::Sender<Started>,
) {
    let connection = match Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(e) => {
            let _ = started.send(Err(Error::Unavailable(format!("no Wayland display: {e}"))));
            return;
        }
    };
    let (globals, mut queue) = match registry_queue_init::<Loop>(&connection) {
        Ok(registry) => registry,
        Err(e) => {
            let _ =
                started.send(Err(Error::Unavailable(format!("the Wayland registry failed: {e}"))));
            return;
        }
    };
    let qh = queue.handle();

    let manager = match want {
        Want::IdleOnly => None,
        Want::Toplevels => match globals
            .bind::<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, _, _>(
            &qh,
            1..=3,
            (),
        ) {
            Ok(manager) => Some(manager),
            Err(e) => {
                let _ = started.send(Err(Error::Unavailable(format!(
                    "this compositor offers no wlr foreign-toplevel protocol ({e})"
                ))));
                return;
            }
        },
    };

    let seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ()).ok();
    let notifier =
        globals.bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(&qh, 1..=2, ()).ok();
    let timeout = u32::try_from(IDLE_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    let notification = match (&notifier, &seat) {
        (Some(notifier), Some(seat)) if notifier.version() >= 2 => {
            Some(notifier.get_input_idle_notification(timeout, seat, &qh, ()))
        }
        (Some(notifier), Some(seat)) => {
            Some(notifier.get_idle_notification(timeout, seat, &qh, ()))
        }
        _ => None,
    };
    if notification.is_some() {
        lock(&shared).idle = Idle::Active;
    }

    let mut state = Loop { shared: Arc::clone(&shared), toplevels: HashMap::new(), focused: None };
    // The toplevels already open are announced now, so the first call
    // after this returns already knows what has focus.
    if let Err(e) = queue.roundtrip(&mut state) {
        let _ =
            started.send(Err(Error::Unavailable(format!("the Wayland compositor failed: {e}"))));
        return;
    }
    if started.send(Ok((connection.clone(), qh.clone()))).is_err() {
        // Nobody is waiting any more (the start timed out).
        return;
    }

    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = queue.blocking_dispatch(&mut state) {
            tracing::info!(error = %e, "the Wayland connection ended");
            lock(&shared).ended = Some(format!("the Wayland connection ended: {e}"));
            return;
        }
    }

    if let Some(notification) = notification {
        notification.destroy();
    }
    if let Some(notifier) = notifier {
        notifier.destroy();
    }
    for handle in state.toplevels.into_values().map(|toplevel| toplevel.handle) {
        handle.destroy();
    }
    if let Some(manager) = manager {
        manager.stop();
    }
    let _ = connection.flush();
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The event loop's own state.
struct Loop {
    shared: Arc<Mutex<Shared>>,
    toplevels: HashMap<ObjectId, Toplevel>,
    /// The toplevel that most recently became activated and still is.
    focused: Option<ObjectId>,
}

/// One toplevel as last reported, and what has arrived since its last
/// `done` (the protocol applies a batch atomically at `done`).
struct Toplevel {
    handle: zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
    app_id: String,
    title: String,
    activated: bool,
    pending: Pending,
}

#[derive(Default)]
struct Pending {
    app_id: Option<String>,
    title: Option<String>,
    activated: Option<bool>,
}

impl Loop {
    /// Apply `id`'s pending batch and work out which toplevel has focus.
    fn done(&mut self, id: &ObjectId) {
        let Some(toplevel) = self.toplevels.get_mut(id) else {
            return;
        };
        let was_activated = toplevel.activated;
        let pending = std::mem::take(&mut toplevel.pending);
        if let Some(app_id) = pending.app_id {
            toplevel.app_id = app_id;
        }
        if let Some(title) = pending.title {
            toplevel.title = title;
        }
        if let Some(activated) = pending.activated {
            toplevel.activated = activated;
        }
        if toplevel.activated && !was_activated {
            self.focused = Some(id.clone());
        } else if !toplevel.activated && self.focused.as_ref() == Some(id) {
            self.focused = self.any_activated();
        }
        self.publish();
    }

    fn closed(&mut self, id: &ObjectId) {
        if let Some(toplevel) = self.toplevels.remove(id) {
            toplevel.handle.destroy();
        }
        if self.focused.as_ref() == Some(id) {
            self.focused = self.any_activated();
        }
        self.publish();
    }

    /// Another activated toplevel, if any (several seats can each have one).
    fn any_activated(&self) -> Option<ObjectId> {
        self.toplevels.iter().find(|(_, toplevel)| toplevel.activated).map(|(id, _)| id.clone())
    }

    fn publish(&self) {
        let window = self
            .focused
            .as_ref()
            .and_then(|id| self.toplevels.get(id))
            // A toplevel that has not said its app id yet is not one that
            // can be counted against anything.
            .filter(|toplevel| !toplevel.app_id.is_empty())
            .map(|toplevel| Window {
                app_id: toplevel.app_id.clone(),
                title: toplevel.title.clone(),
            });
        lock(&self.shared).focused = window;
    }
}

/// Whether a toplevel's `state` array says it is activated.
fn is_activated(state: &[u8]) -> bool {
    state.as_chunks::<4>().0.iter().any(|bytes| u32::from_ne_bytes(*bytes) == STATE_ACTIVATED)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Loop {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Globals coming and going later (an output plugged in) change
        // nothing watched here.
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Loop {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Loop {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Only ever the wake-up sent by `Drop`; its arrival is the point.
    }
}

impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for Loop {
    fn event(
        state: &mut Self,
        _: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                state.toplevels.insert(
                    toplevel.id(),
                    Toplevel {
                        handle: toplevel,
                        app_id: String::new(),
                        title: String::new(),
                        activated: false,
                        pending: Pending::default(),
                    },
                );
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                state.focused = None;
                state.publish();
                lock(&state.shared).ended =
                    Some(String::from("the compositor stopped reporting its windows"));
            }
            _ => {}
        }
    }

    event_created_child!(Loop, zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for Loop {
    fn event(
        state: &mut Self,
        handle: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Done => state.done(&id),
            zwlr_foreign_toplevel_handle_v1::Event::Closed => state.closed(&id),
            event => {
                let Some(toplevel) = state.toplevels.get_mut(&id) else {
                    return;
                };
                match event {
                    zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                        toplevel.pending.app_id = Some(app_id)
                    }
                    zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                        toplevel.pending.title = Some(title)
                    }
                    zwlr_foreign_toplevel_handle_v1::Event::State { state: words } => {
                        toplevel.pending.activated = Some(is_activated(&words));
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, ()> for Loop {
    fn event(
        _: &mut Self,
        _: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The notifier itself sends nothing.
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for Loop {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let idle = match event {
            ext_idle_notification_v1::Event::Idled => Idle::IdledAt(Instant::now()),
            ext_idle_notification_v1::Event::Resumed => Idle::Active,
            _ => return,
        };
        lock(&state.shared).idle = idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_ne_bytes()).collect()
    }

    #[test]
    fn activated_is_the_value_two_among_native_endian_words() {
        assert!(is_activated(&state(&[2])));
        assert!(is_activated(&state(&[0, 2, 3])));
        assert!(!is_activated(&state(&[0, 1, 3])));
        assert!(!is_activated(&[]));
        // A trailing partial word is ignored rather than misread.
        let mut ragged = state(&[1]);
        ragged.extend_from_slice(&[2, 0]);
        assert!(!is_activated(&ragged));
    }

    #[test]
    fn idle_time_is_the_timeout_plus_the_time_since_it_fired() {
        let now = Instant::now();
        assert_eq!(Idle::Active.elapsed(now), Some(Duration::ZERO));
        assert_eq!(Idle::NotOffered.elapsed(now), None);
        let fired = Idle::IdledAt(now);
        assert_eq!(
            fired.elapsed(now + Duration::from_secs(60)),
            Some(IDLE_TIMEOUT + Duration::from_secs(60))
        );
        // A clock read before the event's own instant does not underflow.
        assert_eq!(Idle::IdledAt(now + Duration::from_secs(1)).elapsed(now), Some(IDLE_TIMEOUT));
    }
}
