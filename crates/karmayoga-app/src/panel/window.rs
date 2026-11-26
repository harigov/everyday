//! The panel's own window, in the corner of the primary monitor.
//!
//! A system notification cannot show a countdown the shell owns, or what is
//! being heard as it is heard, and on Linux its buttons are the only ones the
//! shell hears. This window is the panel ([`super`]): small and always on top
//! everywhere a window can be placed.
//!
//! Wayland lets an application do neither -- keep a window on top, or put it
//! where it asks -- so there the panel is drawn by this program run again
//! under X11, through XWayland, which allows both ([`super::topcard`]). Only
//! without XWayland, or if that fails, is it the window below, wherever the
//! desktop puts it, and on GNOME behind whatever window is in front -- and
//! with the keyboard, which GNOME hands any window it shows and an
//! application has no way to refuse (see `voicetyping::pill`).
//!
//! It is never a window over the whole desktop with the cards drawn in its
//! corner. That was the Wayland fallback once: the rest of it was meant to
//! let clicks through, and when that did not take, nothing else on the
//! screen could be clicked while the card was up.
//!
//! # The keyboard
//!
//! The keyboard belongs to whatever the person is typing in -- voice typing's
//! words are going there, and a meeting card appears in the middle of a
//! sentence -- so the window never asks for it. Where the platform has a way
//! to say so, it is built unable to take focus (`focusable(false)`); on X11,
//! where the window manager decides from hints on the window, it is also told
//! not to accept focus nor take it on mapping, just before it is shown, as
//! the voice typing pill is.
//!
//! # How tall
//!
//! The shell owns what the cards say ([`super::Panel`]) and the page draws
//! them; neither knows how tall that comes to until it is laid out. So the
//! page says (`panel_resize`), and the window is that tall, never more than
//! most of the work area, and always with its top right corner where it was,
//! so a card that grows grows downwards. A height the page reported
//! is kept while the window is hidden and used again when it is shown, so
//! the page's report -- which may arrive before or after the window is shown
//! -- is never undone by showing it.
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

use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

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

/// The panel's width where it is a window of its own, and its height until
/// the page says how tall its cards are, in logical pixels.
const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 176.0;
/// Inset from the corner of the work area.
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
    if crate::writing::wayland_session(app) && super::topcard::available() {
        super::topcard::refresh(app);
        return;
    }
    if any { present(app) } else { dismiss(app) }
}

/// The page says its cards are `height` logical pixels tall.
///
/// A page with no cards has nothing to measure and says so with nothing, or
/// with zero: that is no height, and the last one stays. The window is made
/// that tall whether or not it is shown, and put back in its corner, so that
/// it is right the moment it is. A height the same as the last is applied all
/// the same: the page says it again after a spell with no cards, and the
/// window must be that tall whatever has been done to it since.
pub fn resize(app: &AppHandle, height: f64) {
    if !height.is_finite() || height <= 0.0 {
        return;
    }
    let work = target(app).map(|t| t.area.height / t.scale);
    let height = clamp_height(height, work);
    ASKED.store(height.to_bits(), Ordering::Relaxed);
    let Some(window) = app.get_webview_window(LABEL) else { return };
    set_height(&window, height);
    anchor(app, &window);
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
    if GATE.want(true) {
        reconcile(app);
    }
}

/// Take the panel down. The window stays, hidden, for the next card. Returns
/// at once; the window is hidden on another task.
pub fn dismiss(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    super::topcard::dismiss();
    if GATE.want(false) {
        reconcile(app);
    }
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
fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("panel.html".into()))
        .title("Karma Yoga")
        .inner_size(WIDTH, asked())
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        // A card that is not seen is no use on another desktop or Space.
        .visible_on_all_workspaces(true)
        .visible(false)
        .focused(false)
        // Where the platform has a way to say so, this also keeps a click on
        // a card's buttons from activating the window.
        .focusable(false)
        .on_navigation(crate::navigation::guard(crate::navigation::dev_url(app)))
        .build()
        // A call is often full-screen, and the offer to take notes on it has
        // to show over it. See `overlay`.
        .inspect(crate::overlay::float_over_full_screen)
        .map_err(|e| e.to_string())
}

async fn open(app: &AppHandle) -> Result<(), String> {
    let window = window(app)?;
    set_height(&window, asked());
    if !anchor(app, &window) {
        let _ = window.center();
    }
    reveal(app, &window).await
}

/// Show the window where it is, without the keyboard. On X11 the window
/// manager decides who gets the keyboard from hints on the window, so it is
/// told twice -- not on mapping, not at all -- before the window appears.
async fn reveal(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let shown = window.clone();
        crate::writing::on_main(app, move || -> Result<(), String> {
            use gtk::prelude::*;
            let gtk_window = shown.gtk_window().map_err(|e| e.to_string())?;
            gtk_window.set_accept_focus(false);
            gtk_window.set_focus_on_map(false);
            gtk_window.set_keep_above(true);
            gtk_window.show_all();
            Ok(())
        })
        .await??;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
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

/// Put the window's top right corner [`MARGIN`] from the top right of the
/// primary monitor's work area -- below a top bar, clear of a dock. False,
/// and the window left where it is, where no monitor can be named.
///
/// The position is in physical pixels, worked out with the scale of the
/// monitor it is going to; the size is logical, because it is what the page
/// measured.
fn anchor(app: &AppHandle, window: &WebviewWindow) -> bool {
    let Some(target) = target(app) else { return false };
    let (x, y) = corner(target.area, target.scale);
    let _ = window.set_position(PhysicalPosition::new(x, y));
    true
}

/// Where the panel goes: a monitor's work area and its scale.
struct Target {
    area: Area,
    scale: f64,
}

/// The primary monitor, or if the platform will not name one the first it
/// can list. None leaves the window wherever the platform puts a new one.
fn target(app: &AppHandle) -> Option<Target> {
    let monitor = app
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| app.available_monitors().ok().and_then(|all| all.into_iter().next()))?;
    let work = monitor.work_area();
    let (position, size) = (monitor.position(), monitor.size());
    Some(Target {
        area: area_of(
            (work.position.x, work.position.y, work.size.width, work.size.height),
            (position.x, position.y, size.width, size.height),
        ),
        scale: monitor.scale_factor(),
    })
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

// ---- the geometry, without a screen --------------------------------------------

/// A rectangle of the desktop in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// A rectangle as the platform gives it: left, top, width, height.
type Rect = (i32, i32, u32, u32);

/// The work area of a monitor -- the monitor less its dock, top bar and
/// taskbar -- or, where the platform cannot say and reports an empty one, the
/// whole monitor.
fn area_of(work: Rect, monitor: Rect) -> Area {
    let (x, y, width, height) = if work.2 > 0 && work.3 > 0 { work } else { monitor };
    Area { x: f64::from(x), y: f64::from(y), width: f64::from(width), height: f64::from(height) }
}

/// The top left of a window [`WIDTH`] wide whose top right corner is
/// [`MARGIN`] logical pixels in from the top right of `area`.
fn corner(area: Area, scale: f64) -> (i32, i32) {
    let x = area.x + area.width - (WIDTH + MARGIN) * scale;
    let y = area.y + MARGIN * scale;
    (x.round() as i32, y.round() as i32)
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
    fn the_work_area_is_used_when_the_platform_has_one() {
        let area = area_of((0, 32, 1920, 1048), (0, 0, 1920, 1080));
        assert_eq!(area, Area { x: 0.0, y: 32.0, width: 1920.0, height: 1048.0 });
    }

    #[test]
    fn a_monitor_with_no_work_area_is_used_whole() {
        let area = area_of((0, 0, 0, 0), (1920, 0, 2560, 1440));
        assert_eq!(area, Area { x: 1920.0, y: 0.0, width: 2560.0, height: 1440.0 });
        let one_side_empty = area_of((10, 10, 0, 500), (0, 0, 800, 600));
        assert_eq!(one_side_empty.width, 800.0);
    }

    #[test]
    fn the_corner_is_a_margin_in_from_the_top_right() {
        let area = area_of((0, 0, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(corner(area, 1.0), (1920 - 380 - 16, 16));
    }

    #[test]
    fn the_corner_keeps_below_a_top_bar() {
        let area = area_of((0, 32, 1920, 1048), (0, 0, 1920, 1080));
        assert_eq!(corner(area, 1.0), (1524, 32 + 16));
    }

    #[test]
    fn the_corner_is_in_physical_pixels_of_the_monitor_it_is_on() {
        // A second monitor to the right, at twice the scale: the window is
        // 380 logical pixels wide, 760 physical ones.
        let area = area_of((1920, 0, 3840, 2160), (1920, 0, 3840, 2160));
        assert_eq!(corner(area, 2.0), (1920 + 3840 - 2 * (380 + 16), 32));
        let fractional = corner(area_of((0, 0, 1920, 1080), (0, 0, 1920, 1080)), 1.25);
        assert_eq!(fractional, (1920 - 495, 20));
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
