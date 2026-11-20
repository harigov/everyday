//! The meeting card on Wayland, kept in front of every other window.
//!
//! Wayland gives an application no way to keep a window above the others, or
//! to put one where it likes. The card [`crate::recnotice`] draws in this
//! process went behind the meeting the moment the meeting was clicked, and
//! GNOME puts a window that opens without the keyboard behind the focused one
//! to begin with -- so the countdown was only ever seen over this app's own
//! window, if at all. An X11 window can ask for both, and a Wayland desktop
//! runs X11 programs through XWayland. So on Wayland, with XWayland there,
//! the card is this same program run a second time, as a child process under
//! X11 (`GDK_BACKEND=x11`), drawing a small GTK window and nothing else: no
//! webview, no vault, no Tauri.
//!
//! The two speak in lines of JSON. Down the child's stdin, what the card says:
//! a [`RecordingCard`], or `null` to take it down. Up its stdout, a button
//! pressed: `{"key", "answer"}`, answered by [`crate::offers::on_notice_action`]
//! as a notification's button is. The shell still owns the countdown and every
//! answer -- the child only draws and reports, so losing it cannot start a
//! recording -- and the child ends when its stdin closes: when the card is
//! taken down, or this process ends however it ends.

use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use tauri::AppHandle;

use crate::offers::RecordingCard;

/// The argument that makes this program the card rather than the app.
pub const CHILD_FLAG: &str = "--meeting-card";

/// The child's first line, once it has a window to draw in. Until it says
/// so, nothing sent down its stdin is known to be seen: one that cannot open
/// the display ends at once, and a pipe takes a line or two whether or not
/// anybody will ever read them.
const READY: &str = r#"{"ready":true}"#;

/// How long the child has to say it is ready. XWayland may have to start
/// first.
const READY_WAIT: Duration = Duration::from_secs(5);

// ---- the app's side ---------------------------------------------------------

/// The child drawing the card, while one is up.
struct Running {
    process: Child,
    stdin: ChildStdin,
    /// The last line sent: the countdown asks five times a second, and only a
    /// second gone by changes what the card says.
    last: String,
}

/// Wakes the thread that keeps the card up to date.
static WORKER: OnceLock<Sender<()>> = OnceLock::new();

/// Set when the child could not draw the card it was asked for -- no X server
/// it may talk to, or XWayland slow to start -- so that card is drawn in this
/// process until it goes, rather than a new child being tried five times a
/// second. The next card tries again.
static FAILED: AtomicBool = AtomicBool::new(false);

/// Whether the card can be drawn this way: there is an X server to draw it
/// on, and drawing the card that is up on it has not failed. Asked only in a
/// Wayland session; otherwise the card stays in this process
/// ([`crate::recnotice`]'s own window).
pub fn available() -> bool {
    !FAILED.load(Ordering::Relaxed) && std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Bring the card up to date with what the shell says it shows -- a card, or
/// none. Done on a thread of its own and returns at once: starting the child
/// takes a moment, and the callers are a countdown tick and the meeting
/// watcher's.
pub fn refresh(app: &AppHandle) {
    let _ = WORKER.get_or_init(|| work(app.clone())).send(());
}

/// Take the card down, if this way of drawing it was ever started.
pub fn dismiss() {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(());
    }
}

/// The thread [`refresh`] wakes. One, so a card and the card after it are
/// never drawn out of order, and only it ever touches the child.
fn work(app: AppHandle) -> Sender<()> {
    let (wake, woken) = channel::<()>();
    let spawned = std::thread::Builder::new().name("meeting-card".into()).spawn(move || {
        let mut running: Option<Running> = None;
        while woken.recv().is_ok() {
            // Only what the card says now matters, not every tick on the way.
            while woken.try_recv().is_ok() {}
            match crate::offers::recording_notice(app.clone()) {
                // Drawn in this process until it goes; a child now would be
                // a second card.
                Some(_) if FAILED.load(Ordering::Relaxed) => {}
                Some(card) => {
                    if !show(&app, &mut running, &card) {
                        FAILED.store(true, Ordering::Relaxed);
                        // Drawn in this process instead, now `available` says
                        // no.
                        crate::recnotice::present(&app);
                    }
                }
                None => {
                    FAILED.store(false, Ordering::Relaxed);
                    if let Some(gone) = running.take() {
                        end(gone);
                    }
                }
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "the meeting card's thread could not start");
        FAILED.store(true, Ordering::Relaxed);
    }
    wake
}

/// Show `card`, starting the child if it is not running. False if it could
/// not be shown this way.
fn show(app: &AppHandle, running: &mut Option<Running>, card: &RecordingCard) -> bool {
    let Ok(line) = serde_json::to_string(card) else { return false };
    // Twice: a child that died between being looked at and being written to
    // is started again, as one found dead is.
    for _ in 0..2 {
        // Gone since -- it crashed, or somebody closed it: started again.
        if running.as_mut().is_some_and(|r| !matches!(r.process.try_wait(), Ok(None))) {
            *running = None;
        }
        if running.is_none() {
            match start(app) {
                Ok(started) => *running = Some(started),
                Err(e) => {
                    tracing::warn!(error = %e, "the meeting card could not be drawn on X11");
                    return false;
                }
            }
        }
        let Some(child) = running.as_mut() else { return false };
        if child.last == line {
            return true;
        }
        match writeln!(child.stdin, "{line}").and_then(|()| child.stdin.flush()) {
            Ok(()) => {
                child.last = line;
                return true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "the meeting card stopped listening");
                if let Some(gone) = running.take() {
                    end(gone);
                }
            }
        }
    }
    false
}

/// Close the child's stdin, which ends it, and reap it off this thread.
fn end(running: Running) {
    let Running { mut process, stdin, .. } = running;
    drop(stdin);
    std::thread::spawn(move || {
        let _ = process.wait();
    });
}

/// Run this program again as the card, under X11; wait for it to say it can
/// draw, and listen for its buttons.
fn start(app: &AppHandle) -> std::io::Result<Running> {
    // This very program, even if a newer build has replaced the file on disk
    // since it started: the card and the app must agree on what they say.
    let mut process = Command::new("/proc/self/exe")
        .arg(CHILD_FLAG)
        .env("GDK_BACKEND", "x11")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = process.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
    let stdout = process.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
    let (ready_tx, ready_rx) = channel::<bool>();
    let app = app.clone();
    std::thread::Builder::new().name("meeting-card-buttons".into()).spawn(move || {
        let mut lines = std::io::BufReader::new(stdout).lines();
        // Whatever a library prints first -- GLib's own messages, with
        // G_MESSAGES_DEBUG set -- is not an answer either way.
        let ready = lines.by_ref().map_while(Result::ok).any(|line| line == READY);
        let _ = ready_tx.send(ready);
        if !ready {
            return;
        }
        for line in lines {
            let Ok(line) = line else { break };
            // Anything else on its stdout -- a library that prints -- is not
            // a button.
            let Ok(pressed) = serde_json::from_str::<Pressed>(&line) else { continue };
            if matches!(pressed.answer.as_str(), TAKE | JOIN | NOT_NOW) {
                crate::offers::on_notice_action(&app, &pressed.key, &pressed.answer);
            }
        }
    })?;
    if ready_rx.recv_timeout(READY_WAIT) != Ok(true) {
        let _ = process.kill();
        end(Running { process, stdin, last: String::new() });
        return Err(std::io::Error::other("it could not open a window on the X server"));
    }
    Ok(Running { process, stdin, last: String::new() })
}

/// What the child says when a button is pressed.
#[derive(serde::Serialize, serde::Deserialize)]
struct Pressed {
    key: String,
    answer: String,
}

/// The answers a card can give, as [`crate::offers::on_notice_action`] names
/// them.
const TAKE: &str = "take";
const JOIN: &str = "join";
const NOT_NOW: &str = "notNow";

// ---- the child --------------------------------------------------------------

/// The card's width, and its inset from the corner of the work area, in
/// logical pixels -- as [`crate::recnotice`]'s.
const WIDTH: i32 = 360;
const MARGIN: i32 = 16;
/// Room around the card for its shadow, inside the window.
const SHADOW: i32 = 14;
/// How long a pressed button waits for its answer before it can be pressed
/// again.
const PRESS_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// Be the card: draw what comes down stdin until it closes. Called by `run`
/// before anything of the app starts, and never returns.
pub fn run_child() -> ! {
    if let Err(e) = gtk::init() {
        eprintln!("the meeting card could not start GTK: {e}");
        std::process::exit(1);
    }
    let card = Card::build();
    CARD.with(|slot| *slot.borrow_mut() = Some(card));
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{READY}").and_then(|()| out.flush());
    drop(out);
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let Ok(shown) = serde_json::from_str::<Option<RecordingCard>>(&line) else {
                continue;
            };
            gtk::glib::idle_add_once(move || with_card(|card| card.show(shown)));
        }
        gtk::glib::idle_add_once(gtk::main_quit);
    });
    gtk::main();
    std::process::exit(0);
}

thread_local! {
    /// The card, on GTK's thread: its widgets go nowhere else.
    static CARD: std::cell::RefCell<Option<std::rc::Rc<Card>>> =
        const { std::cell::RefCell::new(None) };
}

fn with_card(f: impl FnOnce(&Card)) {
    let card = CARD.with(|slot| slot.borrow().clone());
    if let Some(card) = card {
        f(&card);
    }
}

struct Card {
    window: gtk::Window,
    kicker: gtk::Label,
    count: gtk::Label,
    title: gtk::Label,
    /// "Not now" before the meeting is joined, "Cancel" while counting down.
    second: gtk::Button,
    /// "Join and start recording" before, "Take notes" -- now, not when the
    /// countdown ends -- while counting down.
    first: gtk::Button,
    /// What the card is showing, for the buttons to answer.
    shown: std::cell::RefCell<Option<RecordingCard>>,
    /// Counts presses and new faces of the card, so the wait after one press
    /// cannot let the buttons go in the middle of the wait after another.
    pressed: std::cell::Cell<u64>,
}

impl Card {
    fn build() -> std::rc::Rc<Card> {
        use gtk::prelude::*;

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Meeting");
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_skip_taskbar_hint(true);
        window.set_skip_pager_hint(true);
        // Above everything, on every workspace, and never taking the keyboard
        // from the meeting: pressing its buttons must not either.
        window.set_keep_above(true);
        window.stick();
        window.set_accept_focus(false);
        window.set_focus_on_map(false);
        window.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
        window.set_app_paintable(true);
        if let Some(screen) = WidgetExt::screen(&window) {
            if let Some(visual) = screen.rgba_visual() {
                window.set_visual(Some(&visual));
            }
            let style = gtk::CssProvider::new();
            if style.load_from_data(css(prefers_dark()).as_bytes()).is_ok() {
                gtk::StyleContext::add_provider_for_screen(
                    &screen,
                    &style,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
        }

        let label = |class: &str| {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.style_context().add_class(class);
            label
        };
        let kicker = label("kicker");
        let count = label("count");
        let title = label("title");
        title.set_line_wrap(true);
        title.set_lines(2);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(1);

        let button = |class: Option<&str>| {
            let button = gtk::Button::new();
            button.set_can_focus(false);
            if let Some(class) = class {
                button.style_context().add_class(class);
            }
            button
        };
        let second = button(None);
        let first = button(Some("primary"));

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.set_valign(gtk::Align::End);
        actions.set_vexpand(true);
        actions.pack_start(&second, false, false, 0);
        actions.pack_start(&first, false, false, 0);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.style_context().add_class("card");
        body.set_size_request(WIDTH, -1);
        body.set_margin_top(SHADOW);
        body.set_margin_bottom(SHADOW);
        body.set_margin_start(SHADOW);
        body.set_margin_end(SHADOW);
        body.pack_start(&kicker, false, false, 0);
        body.pack_start(&count, false, false, 0);
        body.pack_start(&title, false, false, 0);
        body.pack_start(&actions, true, true, 4);
        window.add(&body);

        let card = std::rc::Rc::new(Card {
            window,
            kicker,
            count,
            title,
            second,
            first,
            shown: std::cell::RefCell::new(None),
            pressed: std::cell::Cell::new(0),
        });
        let weak = std::rc::Rc::downgrade(&card);
        card.first.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade() {
                let counting = card.shown.borrow().as_ref().is_some_and(|c| c.phase == "countdown");
                card.press(if counting { TAKE } else { JOIN });
            }
        });
        let weak = std::rc::Rc::downgrade(&card);
        card.second.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade() {
                card.press(NOT_NOW);
            }
        });
        card
    }

    /// Draw `shown`, or take the card down.
    fn show(&self, shown: Option<RecordingCard>) {
        use gtk::prelude::*;

        let Some(card) = shown else {
            self.window.hide();
            self.shown.replace(None);
            return;
        };
        let counting = card.phase == "countdown";
        if counting {
            self.kicker.set_text("RECORDING STARTS IN");
            self.count.set_text(&format!("{}s", card.seconds));
            self.second.set_label("Cancel");
            self.first.set_label("Take notes");
        } else {
            self.kicker.set_text("MEETING");
            self.second.set_label("Not now");
            self.first.set_label(if card.can_join {
                "Join and start recording"
            } else {
                "Start recording"
            });
        }
        self.title.set_text(&card.title);
        // A press is answered by the next card; until it comes, once.
        let changed =
            self.shown.borrow().as_ref().is_none_or(|c| c.key != card.key || c.phase != card.phase);
        if changed {
            self.pressed.set(self.pressed.get().wrapping_add(1));
            self.first.set_sensitive(true);
            self.second.set_sensitive(true);
        }
        self.shown.replace(Some(card));

        if !self.window.is_visible() {
            self.window.show_all();
            place(&self.window);
            // Placed again once the window manager has had it: some place a
            // new window themselves first.
            let window = self.window.clone();
            gtk::glib::idle_add_local_once(move || place(&window));
        }
        self.count.set_visible(counting);
    }

    /// Tell the app a button was pressed. The buttons wait for the card that
    /// answers it -- a countdown, or none -- and come back by themselves if
    /// none comes: an answer that failed is said in the app's window, and the
    /// card has to be pressable again.
    fn press(&self, answer: &str) {
        use gtk::prelude::*;

        let Some(key) = self.shown.borrow().as_ref().map(|c| c.key.clone()) else { return };
        self.first.set_sensitive(false);
        self.second.set_sensitive(false);
        let this = self.pressed.get().wrapping_add(1);
        self.pressed.set(this);
        gtk::glib::timeout_add_local_once(PRESS_WAIT, move || {
            with_card(|card| {
                if card.pressed.get() == this {
                    card.first.set_sensitive(true);
                    card.second.set_sensitive(true);
                }
            });
        });
        let Ok(line) = serde_json::to_string(&Pressed { key, answer: answer.to_string() }) else {
            return;
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}").and_then(|()| out.flush());
    }
}

/// The top-right of the primary monitor's work area -- below a top bar, clear
/// of a dock -- less the shadow's room, so the card itself sits [`MARGIN`]
/// from the corner. Where the X server names no primary monitor, as XWayland
/// need not, the card goes on the one the pointer is on, where the person is.
fn place(window: &gtk::Window) {
    use gtk::prelude::*;

    let Some(display) = gtk::gdk::Display::default() else { return };
    let under_pointer = || {
        let pointer = display.default_seat()?.pointer()?;
        let (_, x, y) = pointer.position();
        display.monitor_at_point(x, y)
    };
    let Some(monitor) =
        display.primary_monitor().or_else(under_pointer).or_else(|| display.monitor(0))
    else {
        return;
    };
    let area = monitor.workarea();
    let width = WIDTH + 2 * SHADOW;
    let x = area.x() + area.width() - width - MARGIN + SHADOW;
    let y = area.y() + MARGIN - SHADOW;
    window.move_(x, y.max(area.y()));
}

/// Whether the desktop is set to prefer dark: GNOME's own setting, read only
/// when its schema is there, since asking for one that is not ends the
/// process.
fn prefers_dark() -> bool {
    use gtk::gio::prelude::*;

    crate::gnome::settings("org.gnome.desktop.interface", &["color-scheme"])
        .is_some_and(|settings| settings.string("color-scheme") == "prefer-dark")
}

/// The card's look: the app's own light or dark theme (`themes.css`), so it
/// reads as the app's even drawn by GTK.
fn css(dark: bool) -> String {
    let (bg, fg, muted, border, hover, accent, shadow) = if dark {
        (
            "#201f24",
            "#eceaf0",
            "#a8a4b0",
            "rgba(255,255,255,0.09)",
            "rgba(255,255,255,0.06)",
            "#e8853f",
            "rgba(0,0,0,0.45)",
        )
    } else {
        (
            "#fffefc",
            "#1c1a17",
            "#605c55",
            "rgba(0,0,0,0.09)",
            "rgba(0,0,0,0.05)",
            "#b4530f",
            "rgba(0,0,0,0.18)",
        )
    };
    format!(
        "window {{ background-color: transparent; }}
         .card {{
           background-color: {bg}; color: {fg};
           border: 1px solid {border}; border-radius: 16px;
           padding: 16px 18px 14px;
           box-shadow: 0 6px 18px {shadow};
           font-family: 'Inter', 'Inter Variable', 'Cantarell', 'Ubuntu', sans-serif;
         }}
         .kicker {{ font-size: 9pt; letter-spacing: 0.04em; color: {muted}; }}
         .count {{ font-size: 21pt; font-weight: 700; font-feature-settings: 'tnum'; }}
         .title {{ font-size: 12pt; font-weight: 600; }}
         button {{
           min-height: 32px; padding: 0 12px; border-radius: 999px;
           border: none; box-shadow: none; text-shadow: none;
           background-image: none; background-color: transparent;
           color: {muted}; font-size: 10pt;
         }}
         button:hover {{ background-color: {hover}; color: {fg}; }}
         button.primary {{ background-color: {accent}; color: #ffffff; }}
         button.primary:hover {{ background-color: shade({accent}, 1.08); color: #ffffff; }}
         button:disabled {{ opacity: 0.5; }}"
    )
}
