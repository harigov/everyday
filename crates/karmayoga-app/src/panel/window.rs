//! The panel's own window: in the corner of the primary monitor, or wherever
//! the person dragged it.
//!
//! A system notification cannot show a countdown the shell owns, or what is
//! being heard as it is heard, or take a reply, and on Linux its buttons are
//! the only ones the shell hears. This window is the panel ([`super`]): small
//! and always on top everywhere a window can be placed. The same code runs in
//! the app and, on Wayland, in the panel's child process ([`super::child`]),
//! where the same page draws the same cards.
//!
//! Wayland lets an application do neither -- keep a window on top, or put it
//! where it asks -- so there the panel is drawn by this program run again
//! under X11, through XWayland, which allows both ([`super::child`]). Only
//! without XWayland, or if that fails, is it this window in the app,
//! wherever the desktop puts it, and on GNOME behind whatever window is in
//! front -- and with the keyboard, which GNOME hands any window it shows and
//! an application has no way to refuse (see `voicetyping::pill`).
//!
//! It is never a window over the whole desktop with the cards drawn in its
//! corner. That was the Wayland fallback once: the rest of it was meant to
//! let clicks through, and when that did not take, nothing else on the
//! screen could be clicked while the card was up.
//!
//! # Over a full-screen app
//!
//! A call is often full-screen, and the card about it has to show over it.
//! On a Mac only a non-activating panel can, which `overlay` makes this
//! window into once it is built. After that, nothing here calls
//! `set_always_on_top(false)`, `set_visible_on_all_workspaces(false)` or
//! `set_decorations` on it, nor, on a Mac, the window's `set_focus`: each
//! undoes part of the panel, or activates the app -- which brings the main
//! window forward and switches the Space away from the full-screen one. It
//! is built accepting the first click (`accept_first_mouse`), since a window
//! that is never key would otherwise spend that click on becoming so.
//!
//! # The keyboard
//!
//! The keyboard belongs to whatever the person is typing in -- voice typing's
//! words are going there, and a meeting card appears in the middle of a
//! sentence -- so the window never takes it of its own accord. It is built
//! unable to take focus (`focusable(false)`), and on X11, where the window
//! manager decides from hints on the window, also told not to accept focus
//! nor take it on mapping, just before it is shown.
//!
//! The one exception is the person clicking into a box on a card: a quick
//! reply, a note during a recording. Then the page asks ([`focus`]) and the
//! window takes the keyboard in the one way each platform allows without
//! bringing the app forward, and gives it back when the page is done -- to
//! the window that had it, on a Mac and on Windows; on X11, to wherever the
//! person clicks next. A window that is shown again never has it.
//!
//! # Where
//!
//! The corner -- top right of the primary monitor's work area -- until the
//! person drags the panel by a card's head ([`drag`]); then where they let it
//! go, remembered ([`super::place`]). Only the person's moves are saved: a
//! drag marks the window's moves as theirs while it lasts, and the window's
//! own moves -- back into its place as the cards grow -- never are. On
//! Wayland in this process the desktop places windows and says nothing of
//! where, so nothing is saved there.
//!
//! A drag lasts as long as the button is held, where that can be told: on a
//! Mac, and on Windows when windows are dragged as an outline, the window
//! moves once, as the button comes up, however long after the press; and a
//! drag that pauses with the button down is not over. Where it cannot be
//! told cheaply (X11), a press is the person's for [`FIRST_MOVE`], and the
//! saver asks the X server before it calls a drag that has stopped over.
//!
//! On a Mac the place is in points, not pixels: each screen's pixels are its
//! own there, and a Retina screen beside an ordinary one do not make one
//! space of them ([`place::Screen::in_points`]).
//!
//! # How tall
//!
//! The shell owns what the cards say ([`super::Panel`]) and the page draws
//! them; neither knows how tall that comes to until it is laid out. So the
//! page says (`panel_resize`), and the window is that tall, never more than
//! most of the work area, and always with its top edge where it was, so a
//! card that grows grows downwards -- until it would pass the bottom of the
//! screen, when the window moves up rather than run off it. A height the page
//! reported is kept while the window is hidden and used again when it is
//! shown, so the page's report -- which may arrive before or after the window
//! is shown -- is never undone by showing it.
//!
//! The page asks for the stack when it loads, so cards put up before the page
//! was listening are not missed.
//!
//! # Never waiting for the main thread
//!
//! [`present`] and [`dismiss`] are called, through the hub's worker, for every
//! change to the stack, and the stack is changed by parts of the shell that
//! hold locks of their own while they do it. Neither asks the window anything
//! (`is_visible` waits for the main thread to answer, and on macOS and Windows
//! the main thread may be waiting for one of those locks). They say what is
//! wanted ([`Gate`]), and one task at a time brings the window in line with it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow};

use super::place::{self, Place, Screen};

/// Whether the window is wanted up, and whether it is: what [`present`] and
/// [`dismiss`] say, and what the one task that acts on it has done.
static GATE: Gate = Gate::new();

/// The panel's window. One for the process; hidden while there is nothing to
/// say.
///
/// Crate-visible because `guard` names it: the window's capability
/// (`capabilities/panel.json`) and the list of what it may `call` are both
/// keyed by this label, and a test holds the three together.
pub(crate) const LABEL: &str = "panel";

/// The panel's width, in logical pixels: a card 376 wide and 12 either side
/// for its shadow. Its height until the page says how tall its cards are.
const WIDTH: f64 = 400.0;
const HEIGHT: f64 = 176.0;
/// Inset of the corner from the top right of the work area.
const MARGIN: f64 = 16.0;
/// The shortest the window is made: a card is never less than this, and a
/// page that has measured nothing yet reports less.
const MIN_HEIGHT: f64 = 48.0;
/// The most of the work area's height the window is made. A stack taller than
/// that is the page's to scroll; the window never runs off the screen.
const MOST_OF_AREA: f64 = 0.8;
/// The most it is made where no monitor can be named to say what that is.
const MOST_UNKNOWN: f64 = 720.0;

/// The height the page last asked for, as the bits of an `f64`.
static ASKED: AtomicU64 = AtomicU64::new(HEIGHT.to_bits());

fn asked() -> f64 {
    f64::from_bits(ASKED.load(Ordering::Relaxed))
}

/// Whether this window is what draws the panel just now: asked for and not
/// taken down. The shell only sends it levels while it is.
pub fn drawing() -> bool {
    GATE.wanted()
}

/// Bring whatever draws the panel in line with the stack: shown while there
/// is a card (`any`), taken down when there is none.
pub fn refresh(app: &AppHandle, any: bool) {
    #[cfg(target_os = "linux")]
    if !super::child::is_child()
        && crate::writing::wayland_session(app)
        && super::child::available()
    {
        super::child::refresh(app);
        return;
    }
    if any { present(app) } else { dismiss(app) }
}

/// The page says its cards are `height` logical pixels tall.
///
/// A page with no cards has nothing to measure and says so with nothing, or
/// with zero: that is no height, and the last one stays. The window is made
/// that tall whether or not it is shown, and put back in its place, so that
/// it is right the moment it is. A height the same as the last is applied all
/// the same: the page says it again after a spell with no cards, and the
/// window must be that tall whatever has been done to it since.
pub fn resize(app: &AppHandle, height: f64) {
    if !height.is_finite() || height <= 0.0 {
        return;
    }
    let (screens, primary) = screens(app);
    let size = (WIDTH, clamp_height(height, None));
    let work = place::screen_for(place::get(), &screens, primary, size)
        .map(|screen| screen.work.height / screen.scale);
    let height = clamp_height(height, work);
    ASKED.store(height.to_bits(), Ordering::Relaxed);
    let Some(window) = app.get_webview_window(LABEL) else { return };
    set_height(&window, height);
    put(&window, &screens, primary);
}

/// Show the panel. A window that is already up is left where it is: the page
/// hears the new contents on its own, and moving it on every countdown tick
/// would drag it out from under the pointer. Returns at once; the window is
/// made and shown on another task.
pub fn present(app: &AppHandle) {
    // A window that is gone -- destroyed with the main window -- is not up,
    // whatever was last done to it.
    if app.get_webview_window(LABEL).is_none() {
        GATE.forget();
    }
    if kept_closed(app) {
        return;
    }
    if GATE.want(true) {
        reconcile(app);
    }
}

/// Take the panel down. The window stays, hidden, for the next card. Returns
/// at once; the window is hidden on another task.
pub fn dismiss(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    super::child::dismiss();
    // No cards: the next one is new, whatever was closed.
    *lock(&CLOSED) = None;
    dismiss_here(app);
}

/// Take down this process's window only, leaving whatever else draws the
/// panel alone: on Wayland, the window put up here when the panel's child
/// could not draw the cards, once the child draws them again or they have
/// gone.
pub fn dismiss_here(app: &AppHandle) {
    if GATE.want(false) {
        reconcile(app);
    }
}

/// The cards that were up when the person closed the panel's window, if they
/// did: it stays down until a card that was not among them goes up, or every
/// card has gone.
static CLOSED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// The person closed the panel's window -- Alt+F4 once a box on a card had
/// the keyboard, or the desktop's own close: take it down until the next
/// card. Taken down through [`GATE`], as [`dismiss`] does, so that the next
/// card brings it back; a window hidden behind the gate's back would stay
/// hidden while the gate thought it up, and every card after it unseen.
pub fn closed(app: &AppHandle) {
    let up: Vec<String> = super::current(app).into_iter().map(|card| card.id).collect();
    *lock(&CLOSED) = Some(up);
    KEYBOARD.store(false, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
    if GATE.want(false) {
        reconcile(app);
    }
}

/// Whether the panel stays down, closed by the person, because every card up
/// now was up then. A new card ends that.
fn kept_closed(app: &AppHandle) -> bool {
    if lock(&CLOSED).is_none() {
        return false;
    }
    let up: Vec<String> = super::current(app).into_iter().map(|card| card.id).collect();
    let mut closed = lock(&CLOSED);
    let stays = closed.as_deref().is_some_and(|then| stays_closed(then, &up));
    if !stays {
        *closed = None;
    }
    stays
}

/// Whether a panel closed with the cards `then` up stays closed with the
/// cards `now`: none of them is new.
fn stays_closed(then: &[String], now: &[String]) -> bool {
    now.iter().all(|id| then.contains(id))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start the task that brings the window in line with [`GATE`]: the one task
/// there is, which goes on until the window is as wanted, so a card put up and
/// taken down again while the window is still being made leaves it hidden,
/// and one put up again after leaves it shown.
fn reconcile(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Some(up) = GATE.next() {
                let result = if up { open(&app).await } else { close(&app) };
                match result {
                    Ok(()) => GATE.did(up),
                    Err(e) => {
                        tracing::warn!(error = %e, "the panel could not be shown");
                        GATE.failed();
                    }
                }
            }
            if GATE.rest() {
                break;
            }
        }
    });
}

// ---- the keyboard ---------------------------------------------------------------

/// Whether the page has the keyboard: a box on a card was clicked into and
/// has not let it go. A drag does not take the keyboard back from a box that
/// has it.
static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// On Windows, the window that was in front when the page took the keyboard,
/// to give it back to.
#[cfg(windows)]
static BEFORE_KEYBOARD: Mutex<Option<isize>> = Mutex::new(None);

/// The page wants the keyboard for a box on a card (`want`), or is done with
/// it. The window takes it without bringing the app forward, and when the
/// page is done is made unable to take it again and gives it back.
///
/// * macOS: the window made focusable -- which tao records in the instance
///   variable `overlay`'s panel class reads -- and then made key on the main
///   thread, never `set_focus`, which activates the whole app. Given back
///   by `overlay::give_back_key`: being made unable to take the keyboard
///   does not take it from a window that has it.
/// * Windows: focusable (which lifts `WS_EX_NOACTIVATE`), then focused; the
///   window that was in front before is noted, and put in front again when
///   the page is done, if the panel still is.
/// * X11, in the app or the child: told to accept focus, then presented with
///   the X server's own time, the proof of a recent action focus-stealing
///   prevention asks for (as writing help's popup is). Told not to accept
///   it when the page is done; the keyboard goes back to wherever the person
///   clicks next -- under XWayland the window that had it is usually a
///   Wayland one, which nothing here can hand it to.
/// * Wayland, in the app: GNOME has given the window the keyboard already.
///
/// The page's half -- the webview inside the window -- is focused too, or
/// the keys reach the window and go no further.
pub fn focus(app: &AppHandle, want: bool) {
    let Some(window) = app.get_webview_window(LABEL) else { return };
    KEYBOARD.store(want, Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    {
        let _ = window.set_focusable(want);
        if want {
            crate::overlay::make_key(&window);
            let _ = AsRef::<tauri::Webview>::as_ref(&window).set_focus();
        } else {
            crate::overlay::give_back_key(&window);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if want {
            win::remember(&BEFORE_KEYBOARD, &window);
            let _ = window.set_focusable(true);
            let _ = window.set_focus();
            let _ = AsRef::<tauri::Webview>::as_ref(&window).set_focus();
        } else {
            let _ = window.set_focusable(false);
            win::give_back(app, &window, &BEFORE_KEYBOARD);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if desktop_places_it(app) {
            if want {
                let _ = AsRef::<tauri::Webview>::as_ref(&window).set_focus();
            }
            return;
        }
        let shown = window.clone();
        let _ = app.run_on_main_thread(move || {
            use gtk::glib::Cast;
            use gtk::prelude::*;
            let Ok(gtk_window) = shown.gtk_window() else { return };
            gtk_window.set_accept_focus(want);
            if !want {
                return;
            }
            match gtk_window.window().and_then(|w| w.downcast::<gdkx11::X11Window>().ok()) {
                Some(x11) => {
                    gtk_window.present_with_time(gdkx11::functions::x11_get_server_time(&x11))
                }
                None => gtk_window.present(),
            }
            let _ = AsRef::<tauri::Webview>::as_ref(&shown).set_focus();
        });
    }
}

/// Whether the desktop, not this program, decides where the window is: on
/// Wayland, in the app (the child is on X11 even there).
#[cfg(target_os = "linux")]
fn desktop_places_it(app: &AppHandle) -> bool {
    !super::child::is_child() && crate::writing::wayland_session(app)
}

#[cfg(not(target_os = "linux"))]
fn desktop_places_it(_app: &AppHandle) -> bool {
    false
}

// ---- moving it ------------------------------------------------------------------

/// The person's drag of the window, while it lasts.
static DRAG: Drag = Drag::new();

/// Wakes the thread that saves where a drag ended.
static SAVER: OnceLock<Sender<PhysicalPosition<i32>>> = OnceLock::new();

/// Where the window last said it was (`WindowEvent::Moved`), whoever moved
/// it.
static LAST_AT: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// On Windows, the window that was in front when a grip was pressed, while
/// the page did not have the keyboard: moving a window focuses its webview
/// (wry does, as the move begins), which takes the keyboard from it.
#[cfg(windows)]
static BEFORE_DRAG: Mutex<Option<isize>> = Mutex::new(None);

/// How long after a press on a grip the window's first move is still taken
/// for the person's, where the button cannot be watched (X11). A press that
/// never moves the window -- a click, the first of a double-click -- is over
/// by then.
const FIRST_MOVE: Duration = Duration::from_secs(5);
/// How long after the button comes up the window's first move is still the
/// person's, where the button is watched: on a Mac, and on Windows when
/// windows are dragged as an outline, the window moves once, as it comes up.
const LET_GO: Duration = Duration::from_secs(1);
/// How often the button is looked at while a grip is pressed, where it can
/// be.
const POLL: Duration = Duration::from_millis(30);
/// The longest a press is believed to go on. Past this the button is taken
/// to be up, whatever is said: a drag held still this long has been let go
/// by anybody, and under XWayland the X server may not hear the button come
/// up while the desktop has the pointer.
const HELD_AT_MOST: Duration = Duration::from_secs(30);
/// How long the saver waits for the main thread to say where the button is.
const ASK_WAIT: Duration = Duration::from_millis(200);
/// How long the window has to be still before where it is is saved: the
/// person has let go, or paused. The last move wins.
const SETTLE: Duration = Duration::from_millis(400);
/// How long it has to be still, with the button up, before the drag is over
/// and the window's moves are its own again. Longer than [`SETTLE`], so a
/// drag that pauses and goes on is still the person's.
const DONE: Duration = Duration::from_millis(1200);
const _: () = assert!(SETTLE.as_nanos() < DONE.as_nanos(), "the saver waits DONE - SETTLE");

/// Move the window with the pointer: a press on a card's grip. Where it is let
/// go is where the panel opens from then on.
pub fn drag(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else { return };
    let pressed = Instant::now();
    let tracked = !desktop_places_it(app);
    if tracked {
        DRAG.begin(pressed);
    }
    let started = app.run_on_main_thread(move || {
        #[cfg(windows)]
        if !KEYBOARD.load(Ordering::SeqCst) {
            win::remember(&BEFORE_DRAG, &window);
        }
        if let Err(e) = window.start_dragging() {
            tracing::debug!(error = %e, "the panel could not be moved");
        }
    });
    if let Err(e) = started {
        tracing::debug!(error = %e, "the panel could not be moved");
    }
    if tracked {
        watch_button(app, pressed);
    }
}

/// The window has moved to `at` (`WindowEvent::Moved`). Saved, once it stops,
/// if the person moved it; the window's own moves are not, nor a "move" to
/// where it already was -- X11 says the window moved whenever it is resized.
pub fn moved(app: &AppHandle, at: PhysicalPosition<i32>) {
    #[cfg(target_os = "macos")]
    let at = in_points(app, at);
    if !changed(&mut lock(&LAST_AT), (at.x, at.y)) || !DRAG.heard(Instant::now()) {
        return;
    }
    let _ = SAVER.get_or_init(|| saver(app.clone())).send(at);
}

/// Whether `at` is somewhere else than `last`, which becomes `at`.
fn changed(last: &mut Option<(i32, i32)>, at: (i32, i32)) -> bool {
    last.replace(at) != Some(at)
}

/// `at`, which tao gives in the pixels of the screen the window is on --
/// points times that screen's scale -- in points, the space the place is
/// worked out in on a Mac (see [`screens`]). Carried in a `PhysicalPosition`
/// all the same: to the place it is a pair of whole numbers.
#[cfg(target_os = "macos")]
fn in_points(app: &AppHandle, at: PhysicalPosition<i32>) -> PhysicalPosition<i32> {
    let scale = app
        .get_webview_window(LABEL)
        .and_then(|window| window.scale_factor().ok())
        .filter(|scale| scale.is_normal() && *scale > 0.0)
        .unwrap_or(1.0);
    let points = at.to_logical::<f64>(scale);
    PhysicalPosition::new(points.x.round() as i32, points.y.round() as i32)
}

/// The thread [`moved`] tells where the window is during a drag. It saves the
/// last place once the window has been still for [`SETTLE`], and once it has
/// been still for [`DONE`] with the button up ends the drag and puts the
/// window in its place -- which is where it was let go, kept on the screen.
fn saver(app: AppHandle) -> Sender<PhysicalPosition<i32>> {
    let (tx, rx) = channel::<PhysicalPosition<i32>>();
    let spawned = std::thread::Builder::new().name("panel-place".into()).spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut last = first;
            let mut since = Instant::now();
            let mut saved = false;
            loop {
                let wait = if saved { DONE - SETTLE } else { SETTLE };
                match rx.recv_timeout(wait) {
                    Ok(at) => {
                        last = at;
                        since = Instant::now();
                        saved = false;
                    }
                    Err(RecvTimeoutError::Timeout) if !saved => {
                        place::save(Place { x: last.x, y: last.y, moved: true });
                        saved = true;
                    }
                    // Still, but with the button held: a pause, not the end.
                    Err(RecvTimeoutError::Timeout)
                        if pausing(since.elapsed(), || button_asked(&app)) => {}
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            DRAG.end();
            after_drag(&app);
            if let Some(window) = app.get_webview_window(LABEL) {
                let (screens, primary) = screens(&app);
                put(&window, &screens, primary);
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "the thread that remembers where the panel is could not start");
    }
    tx
}

/// Whether a drag that has been still for `still` is only pausing: the
/// `button` is held, and not for longer than anybody holds one still.
fn pausing(still: Duration, button: impl FnOnce() -> Option<bool>) -> bool {
    still < HELD_AT_MOST && button() == Some(true)
}

/// Watch the button that pressed the grip at `pressed`, where it can be
/// watched from any thread ([`button_now`]: on a Mac and on Windows), until
/// it comes up, telling [`DRAG`] as it goes; then give the keyboard back if
/// the drag took it. Elsewhere nothing is watched.
fn watch_button(app: &AppHandle, pressed: Instant) {
    if button_now().is_none() {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("panel-press".into()).spawn(move || {
        loop {
            std::thread::sleep(POLL);
            let now = Instant::now();
            let button = match button_now() {
                Some(true) if now.saturating_duration_since(pressed) < HELD_AT_MOST => Button::Down,
                Some(false) => Button::Up(now),
                _ => Button::Unknown,
            };
            if !DRAG.button(pressed, button) {
                // Another press, or the drag is over: its watcher, or the
                // saver, deals with the rest.
                return;
            }
            if button != Button::Down {
                after_drag(&app);
                return;
            }
        }
    });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "the panel's drag is timed instead of watched");
    }
}

/// Whether the button that drags windows is down now, where any thread may
/// ask and the answer is cheap: a Mac, Windows. None where it cannot be.
#[cfg(target_os = "macos")]
fn button_now() -> Option<bool> {
    Some(crate::overlay::primary_button_down())
}

#[cfg(windows)]
fn button_now() -> Option<bool> {
    Some(win::button_down())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn button_now() -> Option<bool> {
    None
}

/// Whether the button that drags windows is down now, asked wherever it can
/// be: on X11 (in the app or the child) of the X server, on the main thread,
/// waiting [`ASK_WAIT`] at most. For the saver's thread, never the main one.
#[cfg(target_os = "linux")]
fn button_asked(app: &AppHandle) -> Option<bool> {
    let (tx, rx) = channel::<Option<bool>>();
    let asked = app.run_on_main_thread(move || {
        use gtk::prelude::*;
        let down = gtk::gdk::Display::default().and_then(|display| {
            let pointer = display.default_seat()?.pointer()?;
            let root = display.default_screen().root_window()?;
            let (_, _, _, mask) = root.device_position(&pointer);
            Some(mask.contains(gtk::gdk::ModifierType::BUTTON1_MASK))
        });
        let _ = tx.send(down);
    });
    asked.ok()?;
    rx.recv_timeout(ASK_WAIT).ok().flatten()
}

#[cfg(not(target_os = "linux"))]
fn button_asked(_app: &AppHandle) -> Option<bool> {
    button_now()
}

/// A drag, or a press that might have begun one, is over. On Windows the
/// keyboard goes back to the window that was in front at the press, if the
/// panel took it (see `BEFORE_DRAG`) and the page has not asked for it.
fn after_drag(app: &AppHandle) {
    #[cfg(windows)]
    if !KEYBOARD.load(Ordering::SeqCst)
        && let Some(window) = app.get_webview_window(LABEL)
    {
        win::give_back(app, &window, &BEFORE_DRAG);
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Put the window back in its corner and forget where it was moved to: a
/// double-click on a card's grip, or Settings.
pub fn reset_place(app: &AppHandle) {
    DRAG.end();
    place::forget();
    #[cfg(target_os = "linux")]
    if !super::child::is_child() {
        super::child::reset_place();
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        let (screens, primary) = screens(app);
        put(&window, &screens, primary);
    }
}

// ---- the window -----------------------------------------------------------------

/// Hide the window, if it was ever made.
fn close(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
    Ok(())
}

/// The window, made hidden on first use. It never asks for the keyboard: it
/// is shown over whatever the person is doing, and a window that took the
/// keyboard would take their next words.
///
/// In the child its webview keeps what a webview keeps -- in memory only,
/// and in a folder of its own -- rather than share the app's, which the app
/// has open.
fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }
    let builder =
        crate::overlay::popup_builder(app, LABEL, "panel.html", "Karma Yoga", (WIDTH, asked()))
            .always_on_top(true)
            // A card that is not seen is no use on another desktop or Space.
            .visible_on_all_workspaces(true)
            .visible(false)
            .focused(false)
            // Where the platform has a way to say so, this also keeps a click
            // on a card's buttons from activating the window.
            .focusable(false)
            // The first click on a window that is never key is the page's,
            // not spent on making it key.
            .accept_first_mouse(true);
    #[cfg(target_os = "linux")]
    let builder = match super::child::webview_dir(app) {
        Some(dir) if super::child::is_child() => builder.data_directory(dir).incognito(true),
        _ => builder,
    };
    builder
        .build()
        // A call is often full-screen, and the offer to take notes on it has
        // to show over it. See `overlay`, and the module doc for what must
        // not be done to the window after this.
        .inspect(crate::overlay::float_over_full_screen)
        .map_err(|e| e.to_string())
}

async fn open(app: &AppHandle) -> Result<(), String> {
    let window = window(app)?;
    set_height(&window, asked());
    let (screens, primary) = screens(app);
    if !put(&window, &screens, primary) {
        let _ = window.center();
    }
    reveal(app, &window).await
}

/// Show the window where it is, without the keyboard. On X11 the window
/// manager decides who gets the keyboard from hints on the window, so it is
/// told twice -- not on mapping, not at all -- before the window appears.
///
/// The child's window is also told it is a utility window that belongs on
/// every workspace and in no taskbar or pager: there are no other windows in
/// that program for the window manager to judge it by. A utility window, not
/// a notification -- Mutter never gives a notification the keyboard, and a
/// reply box on a card needs it.
async fn reveal(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    // Whatever the page had when the window was last hidden -- a box with
    // the keyboard as the last card went, or as the window was closed -- it
    // has nothing now.
    KEYBOARD.store(false, Ordering::SeqCst);
    #[cfg(target_os = "linux")]
    {
        let shown = window.clone();
        let child = super::child::is_child();
        crate::writing::on_main(app, move || -> Result<(), String> {
            use gtk::prelude::*;
            let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
            gtk_window.set_accept_focus(false);
            gtk_window.set_focus_on_map(false);
            gtk_window.set_keep_above(true);
            if child {
                gtk_window.stick();
                gtk_window.set_skip_taskbar_hint(true);
                gtk_window.set_skip_pager_hint(true);
                gtk_window.set_type_hint(gtk::gdk::WindowTypeHint::Utility);
            }
            gtk_window.show_all();
            Ok(())
        })
        .await??;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        // Unable to take the keyboard before it is shown: a Mac makes a
        // window that may be key the key window by showing it, and Windows
        // brings forward one without `WS_EX_NOACTIVATE`.
        let _ = window.set_focusable(false);
        window.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Make the window `height` tall.
///
/// On Linux this is the window's size request, not a resize. GTK holds a
/// window that cannot be resized by the person to what its contents ask for
/// and ignores a resize once it is up -- the window is built that way so that
/// its edges are not drag handles -- and a size request is what it does heed.
fn set_height(window: &WebviewWindow, height: f64) {
    #[cfg(target_os = "linux")]
    {
        let shown = window.clone();
        let size = (WIDTH as i32, height.ceil() as i32);
        let _ = window.run_on_main_thread(move || {
            use gtk::prelude::*;
            if let Ok(gtk_window) = shown.gtk_window() {
                gtk_window.set_size_request(size.0, size.1);
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = window.set_size(tauri::LogicalSize::new(WIDTH, height));
}

/// Put the window in its place ([`place::position`]): where the person left
/// it, kept on the monitor it is on, or the corner. False, and the window
/// left where it is, where no monitor can be named. Left alone while the
/// person is dragging it: a card growing under the pointer must not snatch
/// the window back to where the drag began.
///
/// The position is in physical pixels, worked out with the scale of the
/// monitor it is going to -- on a Mac, in points, which is how it is set
/// there: tao turns physical pixels into points with the scale of the screen
/// the window is on now, not of the one it is going to. The size is logical,
/// because it is what the page measured.
fn put(window: &WebviewWindow, screens: &[Screen], primary: usize) -> bool {
    if DRAG.under_way(Instant::now()) {
        return true;
    }
    let Some((x, y)) = place::position(place::get(), screens, primary, (WIDTH, asked()), MARGIN)
    else {
        return false;
    };
    #[cfg(target_os = "macos")]
    let _ = window.set_position(tauri::LogicalPosition::new(f64::from(x), f64::from(y)));
    #[cfg(not(target_os = "macos"))]
    let _ = window.set_position(PhysicalPosition::new(x, y));
    true
}

/// Every monitor, and which of them is the primary. The primary is the first
/// where the platform will not name one; a platform that names a primary but
/// lists nothing has that one. On a Mac, in points
/// ([`place::Screen::in_points`]).
///
/// Asks the main thread, so never called by the hub's worker: only by the
/// page's commands and the task that shows the window.
fn screens(app: &AppHandle) -> (Vec<Screen>, usize) {
    #[cfg(target_os = "macos")]
    let describe = Screen::in_points;
    #[cfg(not(target_os = "macos"))]
    let describe = Screen::new;
    let screen = |monitor: &tauri::Monitor| {
        let (position, size, work) = (monitor.position(), monitor.size(), monitor.work_area());
        describe(
            (position.x, position.y, size.width, size.height),
            (work.position.x, work.position.y, work.size.width, work.size.height),
            monitor.scale_factor(),
        )
    };
    let all = app.available_monitors().unwrap_or_default();
    let primary = app.primary_monitor().ok().flatten();
    if all.is_empty() {
        return (primary.iter().map(screen).collect(), 0);
    }
    let at = primary
        .and_then(|p| all.iter().position(|m| m.position() == p.position() && m.size() == p.size()))
        .unwrap_or(0);
    (all.iter().map(screen).collect(), at)
}

// ---- what is wanted, without a window -----------------------------------------

/// Whether the window is wanted up and whether it is up, and whether a task is
/// bringing the one in line with the other.
///
/// Three flags and no lock: the callers are the hub's worker and the task, and
/// neither may wait for the other.
struct Gate {
    wanted: AtomicBool,
    actual: AtomicBool,
    working: AtomicBool,
}

impl Gate {
    const fn new() -> Gate {
        Gate {
            wanted: AtomicBool::new(false),
            actual: AtomicBool::new(false),
            working: AtomicBool::new(false),
        }
    }

    fn wanted(&self) -> bool {
        self.wanted.load(Ordering::SeqCst)
    }

    /// Say whether the window is wanted up. True if the caller must start the
    /// task that brings it in line: it is not as wanted, and no task is
    /// already at it.
    fn want(&self, up: bool) -> bool {
        self.wanted.store(up, Ordering::SeqCst);
        if self.actual.load(Ordering::SeqCst) == up && !self.working.load(Ordering::SeqCst) {
            return false;
        }
        !self.working.swap(true, Ordering::SeqCst)
    }

    /// What the task is to do next: `Some(up)` if the window is not as wanted.
    fn next(&self) -> Option<bool> {
        let wanted = self.wanted.load(Ordering::SeqCst);
        (self.actual.load(Ordering::SeqCst) != wanted).then_some(wanted)
    }

    /// The task has made the window as wanted.
    fn did(&self, up: bool) {
        self.actual.store(up, Ordering::SeqCst);
    }

    /// The task could not make the window as wanted. It gives up on that: the
    /// next [`Gate::want`] says it again, and tries again.
    fn failed(&self) {
        self.wanted.store(self.actual.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// The task has nothing left to do. True if it may end; false if
    /// something was wanted after it last looked and no one else has taken
    /// that on -- then it goes on.
    fn rest(&self) -> bool {
        self.working.store(false, Ordering::SeqCst);
        if self.next().is_none() {
            return true;
        }
        self.working.swap(true, Ordering::SeqCst)
    }

    /// The window is known to be gone, so it is not up. Left alone while a
    /// task is at work, which knows better.
    fn forget(&self) {
        if !self.working.load(Ordering::SeqCst) {
            self.actual.store(false, Ordering::SeqCst);
        }
    }
}

/// The person's drag of the window: from the press on a grip until the
/// window has been still a while with the button up ([`DONE`], ended by the
/// saver), or, if it never moved, until the press is over ([`Press::open`]).
/// While it lasts the window's moves are the person's, and the window is not
/// put back in its place.
struct Drag {
    state: Mutex<Option<Press>>,
}

/// A press on a grip, and what has come of it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Press {
    /// When the grip was pressed.
    pressed: Instant,
    /// When the window last moved as the person's, since.
    last: Option<Instant>,
    /// What is known of the button since.
    button: Button,
}

/// What is known of the button that pressed a grip.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Button {
    /// Nothing: it is not watched here ([`button_now`]), or not any more.
    Unknown,
    /// Still down: the press goes on, however long before the window moves.
    Down,
    /// Come up, at that moment.
    Up(Instant),
}

impl Press {
    /// Whether a first move at `now` is still the person's: the press is
    /// going on, or has only just ended -- or, where the button is not
    /// watched, was made less than [`FIRST_MOVE`] ago.
    fn open(&self, now: Instant) -> bool {
        match self.button {
            Button::Down => true,
            Button::Up(at) => now.saturating_duration_since(at) < LET_GO,
            Button::Unknown => now.saturating_duration_since(self.pressed) < FIRST_MOVE,
        }
    }

    fn under_way(&self, now: Instant) -> bool {
        self.last.is_some() || self.open(now)
    }
}

impl Drag {
    const fn new() -> Drag {
        Drag { state: Mutex::new(None) }
    }

    /// A grip was pressed at `now`.
    fn begin(&self, now: Instant) {
        *lock(&self.state) = Some(Press { pressed: now, last: None, button: Button::Unknown });
    }

    /// The window moved at `now`: true if that is the person's doing, a move
    /// during a drag.
    fn heard(&self, now: Instant) -> bool {
        let mut state = lock(&self.state);
        match state.as_mut() {
            Some(press) if press.under_way(now) => {
                press.last = Some(now);
                true
            }
            _ => false,
        }
    }

    /// Whether a drag is under way at `now`.
    fn under_way(&self, now: Instant) -> bool {
        lock(&self.state).is_some_and(|press| press.under_way(now))
    }

    /// What the button that pressed the grip at `pressed` is doing. False if
    /// that press is over, or another has been made since: then nobody need
    /// watch it any more.
    fn button(&self, pressed: Instant, button: Button) -> bool {
        let mut state = lock(&self.state);
        match state.as_mut() {
            Some(press) if press.pressed == pressed => {
                press.button = button;
                true
            }
            _ => false,
        }
    }

    /// The drag is over.
    fn end(&self) {
        *lock(&self.state) = None;
    }
}

/// Windows' half of handing the keyboard back: which window was in front
/// before the panel took it, and putting that one in front again -- which
/// Windows allows a program that is in front itself, as this one is while
/// the panel has the keyboard. And the state of the mouse's buttons, which
/// any thread may ask for.
///
/// Declared here rather than through the `windows` crate, which this crate
/// does not otherwise need: five functions of `user32`, plain numbers in and
/// out.
#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::sync::Mutex;

    use tauri::{AppHandle, WebviewWindow};

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetForegroundWindow() -> *mut c_void;
        fn SetForegroundWindow(window: *mut c_void) -> i32;
        fn IsWindow(window: *mut c_void) -> i32;
        fn GetAsyncKeyState(key: i32) -> i16;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    const VK_LBUTTON: i32 = 0x01;
    const VK_RBUTTON: i32 = 0x02;
    const SM_SWAPBUTTON: i32 = 23;

    /// The panel's own window, as a number.
    fn own(window: &WebviewWindow) -> Option<isize> {
        window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
    }

    /// Note in `slot` the window in front now, unless that is the panel.
    pub fn remember(slot: &Mutex<Option<isize>>, window: &WebviewWindow) {
        // SAFETY: takes nothing; answers a window, or null.
        let front = unsafe { GetForegroundWindow() } as isize;
        let before = (front != 0 && Some(front) != own(window)).then_some(front);
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = before;
    }

    /// Put the window noted in `slot` in front again, on the main thread --
    /// if the panel is in front now, and only then: a person who has clicked
    /// somewhere else since has put the keyboard there, and it stays.
    pub fn give_back(app: &AppHandle, window: &WebviewWindow, slot: &'static Mutex<Option<isize>>) {
        let window = window.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(before) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                return;
            };
            let Some(panel) = own(&window) else { return };
            // SAFETY: plain handles. `IsWindow` answers for any number, and
            // `SetForegroundWindow` is only given one it vouched for.
            unsafe {
                if GetForegroundWindow() as isize == panel && IsWindow(before as *mut c_void) != 0 {
                    SetForegroundWindow(before as *mut c_void);
                }
            }
        });
    }

    /// Whether the button that drags windows is down: the left, or the right
    /// where the two are swapped -- `GetAsyncKeyState` reads the buttons as
    /// they are, not as they are mapped.
    pub fn button_down() -> bool {
        // SAFETY: plain numbers in and out.
        unsafe {
            let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 { VK_RBUTTON } else { VK_LBUTTON };
            GetAsyncKeyState(key) < 0
        }
    }
}

/// `height`, whole logical pixels rounded up so nothing is cut off, kept to
/// what a window can sensibly be: no shorter than a card, and no taller than
/// most of the work area's `work` height (logical pixels), where that is
/// known.
fn clamp_height(height: f64, work: Option<f64>) -> f64 {
    let most = work.map_or(MOST_UNKNOWN, |h| h * MOST_OF_AREA).max(MIN_HEIGHT);
    height.ceil().clamp(MIN_HEIGHT, most.floor())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a task does with a gate, one job at a time: the jobs it was given,
    /// until it rests.
    fn run(gate: &Gate) -> Vec<bool> {
        let mut done = Vec::new();
        loop {
            if let Some(up) = gate.next() {
                gate.did(up);
                done.push(up);
            }
            if gate.rest() {
                return done;
            }
        }
    }

    #[test]
    fn a_window_wanted_up_is_brought_up_by_one_task() {
        let gate = Gate::new();
        assert!(gate.want(true), "this caller starts the task");
        assert!(!gate.want(true), "a second does not: one is at it");
        assert!(gate.wanted());
        assert_eq!(run(&gate), [true]);
        assert_eq!(gate.next(), None);
    }

    #[test]
    fn a_window_that_is_as_wanted_starts_nothing() {
        let gate = Gate::new();
        assert!(!gate.want(false), "never up, and wanted down");
        assert!(gate.want(true));
        assert_eq!(run(&gate), [true]);
        for _ in 0..5 {
            assert!(!gate.want(true), "every tick of a countdown asks, and nothing is done");
        }
        assert!(gate.want(false));
        assert_eq!(run(&gate), [false]);
        assert!(!gate.want(false));
    }

    #[test]
    fn the_last_thing_wanted_is_what_the_window_ends_as() {
        let gate = Gate::new();
        assert!(gate.want(true));
        // Up, down and up again before the task has looked.
        assert!(!gate.want(false));
        assert!(!gate.want(true));
        assert_eq!(run(&gate), [true]);

        let gate = Gate::new();
        assert!(gate.want(true));
        assert!(!gate.want(false));
        assert_eq!(run(&gate), Vec::<bool>::new(), "never shown at all");
        assert_eq!(gate.next(), None);
    }

    #[test]
    fn a_change_of_mind_during_the_task_is_acted_on_before_it_ends() {
        let gate = Gate::new();
        assert!(gate.want(true));
        // The task shows the window...
        assert_eq!(gate.next(), Some(true));
        gate.did(true);
        // ...and while it is still going, the card goes.
        assert!(!gate.want(false));
        // So it does not end: it takes the work back and hides the window.
        assert!(!gate.rest());
        assert_eq!(gate.next(), Some(false));
        gate.did(false);
        assert!(gate.rest());
    }

    #[test]
    fn a_want_that_comes_after_the_task_rested_starts_another() {
        let gate = Gate::new();
        assert!(gate.want(true));
        assert_eq!(run(&gate), [true]);
        assert!(gate.want(false), "the task has ended; this starts one");
        assert_eq!(run(&gate), [false]);
    }

    #[test]
    fn a_window_that_could_not_be_made_is_tried_again_by_the_next_want() {
        let gate = Gate::new();
        assert!(gate.want(true));
        assert_eq!(gate.next(), Some(true));
        gate.failed();
        assert_eq!(gate.next(), None, "no hot loop");
        assert!(gate.rest());
        assert!(!gate.wanted(), "nothing is drawing");
        assert!(gate.want(true), "the next card tries again");
        assert_eq!(run(&gate), [true]);
    }

    #[test]
    fn a_window_that_has_gone_is_made_again() {
        let gate = Gate::new();
        assert!(gate.want(true));
        assert_eq!(run(&gate), [true]);
        assert!(!gate.want(true));
        gate.forget();
        assert!(gate.want(true), "it is not up any more");
        assert_eq!(run(&gate), [true]);
    }

    #[test]
    fn a_task_at_work_is_not_second_guessed() {
        let gate = Gate::new();
        assert!(gate.want(true));
        gate.did(true);
        gate.forget();
        assert_eq!(gate.next(), None, "left alone while the task is at it");
    }

    #[test]
    fn a_move_without_a_drag_is_the_windows_own() {
        let drag = Drag::new();
        let now = Instant::now();
        assert!(!drag.heard(now), "the window put back in its place");
        assert!(!drag.under_way(now));
    }

    #[test]
    fn moves_during_a_drag_are_the_persons() {
        let drag = Drag::new();
        let pressed = Instant::now();
        drag.begin(pressed);
        assert!(drag.under_way(pressed));
        assert!(drag.heard(pressed + Duration::from_millis(30)));
        assert!(drag.heard(pressed + Duration::from_millis(60)));
        // However long the drag goes on: it ends when the window is still.
        assert!(drag.heard(pressed + FIRST_MOVE * 3));
        assert!(drag.under_way(pressed + FIRST_MOVE * 3));
        drag.end();
        assert!(!drag.heard(pressed + FIRST_MOVE * 3), "after the drag, the window's own again");
        assert!(!drag.under_way(pressed + FIRST_MOVE * 3));
    }

    #[test]
    fn a_press_that_never_moves_the_window_is_over_after_a_while() {
        let drag = Drag::new();
        let pressed = Instant::now();
        drag.begin(pressed);
        assert!(drag.under_way(pressed + FIRST_MOVE / 2));
        assert!(!drag.under_way(pressed + FIRST_MOVE));
        assert!(!drag.heard(pressed + FIRST_MOVE), "a click, not a drag");
    }

    #[test]
    fn a_new_press_starts_the_wait_for_a_first_move_again() {
        let drag = Drag::new();
        let pressed = Instant::now();
        drag.begin(pressed);
        assert!(drag.heard(pressed + Duration::from_millis(10)));
        let again = pressed + FIRST_MOVE * 2;
        drag.begin(again);
        assert!(!drag.under_way(again + FIRST_MOVE), "the earlier move does not carry over");
        assert!(drag.heard(again + Duration::from_millis(10)));
    }

    #[test]
    fn a_press_held_down_is_the_persons_however_long_before_the_window_moves() {
        let drag = Drag::new();
        let pressed = Instant::now();
        drag.begin(pressed);
        assert!(drag.button(pressed, Button::Down));
        let late = pressed + FIRST_MOVE * 3;
        assert!(drag.under_way(late), "the window is left where the pointer has it");
        assert!(drag.heard(late));
    }

    #[test]
    fn the_one_move_as_the_button_comes_up_is_the_persons() {
        // A Mac, or Windows dragging an outline: no move until the button
        // comes up, six seconds after the press.
        let drag = Drag::new();
        let pressed = Instant::now();
        drag.begin(pressed);
        assert!(drag.button(pressed, Button::Down));
        let up = pressed + Duration::from_secs(6);
        assert!(drag.button(pressed, Button::Up(up)));
        assert!(drag.heard(up + Duration::from_millis(40)));
        assert!(drag.under_way(up + LET_GO * 2), "moving: the saver ends it");
    }

    #[test]
    fn a_click_is_over_soon_after_the_button_comes_up() {
        let drag = Drag::new();
        let pressed = Instant::now();
        drag.begin(pressed);
        let up = pressed + Duration::from_millis(90);
        assert!(drag.button(pressed, Button::Up(up)));
        assert!(drag.under_way(up + LET_GO / 2));
        assert!(!drag.under_way(up + LET_GO));
        assert!(!drag.heard(up + LET_GO), "the window put back in its place, not a drag");
    }

    #[test]
    fn a_watcher_of_a_press_that_is_over_changes_nothing() {
        let drag = Drag::new();
        let first = Instant::now();
        drag.begin(first);
        let second = first + Duration::from_millis(300);
        drag.begin(second);
        assert!(!drag.button(first, Button::Down), "the first press's watcher stops");
        assert!(!drag.under_way(second + FIRST_MOVE), "and did not hold the second open");
        drag.end();
        assert!(!drag.button(second, Button::Down));
        assert!(!drag.under_way(second));
    }

    #[test]
    fn a_move_to_where_the_window_already_is_is_no_move() {
        let mut last = None;
        assert!(changed(&mut last, (100, 200)), "the first heard");
        assert!(!changed(&mut last, (100, 200)), "resized, not moved");
        assert!(changed(&mut last, (101, 200)));
        assert_eq!(last, Some((101, 200)));
    }

    #[test]
    fn a_drag_held_still_with_the_button_down_is_pausing() {
        assert!(pausing(DONE, || Some(true)));
        assert!(!pausing(DONE, || Some(false)), "let go");
        assert!(!pausing(DONE, || None), "nobody can say: over, as it always was");
        assert!(!pausing(HELD_AT_MOST, || Some(true)), "not for ever");
        let mut asked = false;
        assert!(!pausing(HELD_AT_MOST * 2, || {
            asked = true;
            Some(true)
        }));
        assert!(!asked, "past the longest, the button is not asked about");
    }

    #[test]
    fn a_closed_panel_stays_closed_until_a_new_card() {
        let then = ["recording:1".to_string(), "mail:1".to_string()];
        assert!(stays_closed(&then, &["recording:1".to_string()]));
        assert!(stays_closed(&then, &["mail:1".to_string(), "recording:1".to_string()]));
        assert!(!stays_closed(&then, &["recording:1".to_string(), "mail:2".to_string()]));
    }

    #[test]
    fn a_height_is_rounded_up_to_a_whole_pixel() {
        assert_eq!(clamp_height(211.2, Some(1000.0)), 212.0);
        assert_eq!(clamp_height(212.0, Some(1000.0)), 212.0);
    }

    #[test]
    fn a_height_is_never_less_than_a_card() {
        assert_eq!(clamp_height(1.0, Some(1000.0)), 48.0);
        assert_eq!(clamp_height(47.2, None), 48.0);
    }

    #[test]
    fn a_height_is_never_more_than_most_of_the_work_area() {
        assert_eq!(clamp_height(5000.0, Some(1000.0)), 800.0);
        assert_eq!(clamp_height(799.0, Some(1000.0)), 799.0);
        assert_eq!(clamp_height(5000.0, Some(777.0)), 621.0);
    }

    #[test]
    fn a_height_without_a_monitor_is_capped_by_a_default() {
        assert_eq!(clamp_height(5000.0, None), 720.0);
        assert_eq!(clamp_height(300.0, None), 300.0);
    }

    #[test]
    fn a_tiny_work_area_cannot_make_the_most_less_than_the_least() {
        // 80% of 40 is below a card; the window is a card tall, not a panic.
        assert_eq!(clamp_height(200.0, Some(40.0)), 48.0);
    }
}
