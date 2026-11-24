//! The panel on Wayland, kept in front of every other window.
//!
//! Wayland gives an application no way to keep a window above the others, or
//! to put one where it likes. The panel [`super::window`] draws in this
//! process went behind the meeting the moment the meeting was clicked, and
//! GNOME puts a window that opens without the keyboard behind the focused one
//! to begin with -- so a countdown was only ever seen over this app's own
//! window, if at all. An X11 window can ask for both, and a Wayland desktop
//! runs X11 programs through XWayland. So on Wayland, with XWayland there,
//! the panel is this same program run a second time, as a child process under
//! X11 (`GDK_BACKEND=x11`), drawing a small GTK window and nothing else: no
//! webview, no vault, no Tauri.
//!
//! The two speak in lines of JSON. Down the child's stdin, a [`ToChild`]: the
//! whole stack of cards (empty to take the window down), or a level for one
//! card's meter. Up its stdout, a [`Pressed`]: a button or a box, answered by
//! [`super::on_action`]. The shell still owns every answer -- the child only
//! draws and reports, so losing it cannot start a recording -- and the child
//! ends when its stdin closes: when the last card is taken down, or this
//! process ends however it ends. The one thing it keeps after that is the
//! text of a Copy button it was pressed for, for a little while: see
//! `Stack::finish`.
//!
//! It draws the cards [`super::shown`] says, as the page does, and says
//! "+N more" under them for [`super::hidden`]. A card is built once, when it
//! first appears, and each part of it is changed where it is as the card
//! changes -- the countdown's number, the words heard so far -- so a button
//! the pointer is on is never replaced between the pointer going down and
//! coming up, which would lose the click.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use tauri::AppHandle;

use super::{Action, Item, Line, Panel, Section, Tone};

/// The argument that makes this program the panel rather than the app.
pub const CHILD_FLAG: &str = "--panel";

/// What the app tells the child, a line at a time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToChild {
    /// The whole stack, in the order it is drawn -- more than the child draws
    /// ([`super::MAX_SHOWN`]), so that it can say how many wait. Empty hides
    /// the window.
    Panels(Vec<Panel>),
    /// How loud the microphone is for one card's meter, from 0 to 1.
    Level { id: String, level: f32 },
}

/// The child's first line, once it has a window to draw in. Until it says
/// so, nothing sent down its stdin is known to be seen: one that cannot open
/// the display ends at once, and a pipe takes a line or two whether or not
/// anybody will ever read them.
const READY: &str = r#"{"ready":true}"#;

/// How long the child has to say it is ready. XWayland may have to start
/// first.
const READY_WAIT: Duration = Duration::from_secs(5);

// ---- the app's side ---------------------------------------------------------

/// The child drawing the panel, while a card is up.
struct Running {
    process: Child,
    stdin: ChildStdin,
    /// The last line sent: the countdown asks five times a second, and only a
    /// second gone by changes what a card says.
    last: String,
}

/// What wakes the thread that keeps the child up to date.
enum Wake {
    /// The stack changed; send it as it is now.
    Stack,
    /// A level for one card's meter, sent as it comes.
    Level(String, f32),
}

/// Wakes the thread that keeps the panel up to date.
static WORKER: OnceLock<Sender<Wake>> = OnceLock::new();

/// Set when the child could not draw the cards it was asked for -- no X server
/// it may talk to, or XWayland slow to start -- so they are drawn in this
/// process until they go, rather than a new child being tried five times a
/// second. The next card to go up after all are down tries again.
static FAILED: AtomicBool = AtomicBool::new(false);

/// Whether the panel can be drawn this way: there is an X server to draw it
/// on, and drawing the cards that are up on it has not failed. Asked only in a
/// Wayland session; otherwise the panel stays in this process
/// ([`super::window`]'s own window).
pub fn available() -> bool {
    !FAILED.load(Ordering::Relaxed) && std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Bring the panel up to date with what the shell says it shows -- some cards,
/// or none. Done on a thread of its own and returns at once: starting the
/// child takes a moment, and the callers are a countdown tick, the meeting
/// watcher and a microphone's worth of words.
pub fn refresh(app: &AppHandle) {
    let _ = WORKER.get_or_init(|| work(app.clone())).send(Wake::Stack);
}

/// Take the panel down, if this way of drawing it was ever started.
pub fn dismiss() {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(Wake::Stack);
    }
}

/// How loud the microphone is for the card `id`, if the child is drawing it.
pub fn level(id: &str, level: f32) {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(Wake::Level(id.to_string(), level));
    }
}

/// The thread [`refresh`] wakes. One, so a stack and the stack after it are
/// never drawn out of order, and only it ever touches the child.
fn work(app: AppHandle) -> Sender<Wake> {
    let (wake, woken) = channel::<Wake>();
    let spawned = std::thread::Builder::new().name("panel".into()).spawn(move || {
        let mut running: Option<Running> = None;
        while let Ok(first) = woken.recv() {
            // Only what the stack is now matters, not every change on the
            // way; levels go as they come, to a child already drawing.
            let mut stack_changed = matches!(first, Wake::Stack);
            let mut levels = Vec::new();
            if let Wake::Level(id, level) = first {
                levels.push((id, level));
            }
            while let Ok(next) = woken.try_recv() {
                match next {
                    Wake::Stack => stack_changed = true,
                    Wake::Level(id, level) => levels.push((id, level)),
                }
            }
            if !stack_changed {
                if let Some(child) = running.as_mut() {
                    for (id, level) in levels {
                        send_level(child, id, level);
                    }
                }
                continue;
            }
            let stack = super::current(&app);
            match stack.is_empty() {
                // Drawn in this process until it goes; a child now would be
                // a second panel.
                false if FAILED.load(Ordering::Relaxed) => {}
                false => {
                    if !show(&app, &mut running, &ToChild::Panels(stack)) {
                        FAILED.store(true, Ordering::Relaxed);
                        // Drawn in this process instead, now `available` says
                        // no.
                        super::window::present(&app);
                    }
                }
                true => {
                    FAILED.store(false, Ordering::Relaxed);
                    if let Some(gone) = running.take() {
                        end(gone);
                    }
                }
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "the panel's thread could not start");
        FAILED.store(true, Ordering::Relaxed);
    }
    wake
}

/// Send a level to a child already drawing. A failure is left for the next
/// stack to find.
fn send_level(child: &mut Running, id: String, level: f32) {
    let Ok(line) = serde_json::to_string(&ToChild::Level { id, level }) else { return };
    let _ = writeln!(child.stdin, "{line}").and_then(|()| child.stdin.flush());
}

/// Show `message`, starting the child if it is not running. False if it could
/// not be shown this way.
fn show(app: &AppHandle, running: &mut Option<Running>, message: &ToChild) -> bool {
    let Ok(line) = serde_json::to_string(message) else { return false };
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
                    tracing::warn!(error = %e, "the panel could not be drawn on X11");
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
                tracing::warn!(error = %e, "the panel stopped listening");
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

/// Run this program again as the panel, under X11; wait for it to say it can
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
    std::thread::Builder::new().name("panel-buttons".into()).spawn(move || {
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
            super::on_action(&app, &pressed.id, &pressed.action, pressed.copied);
        }
    })?;
    if ready_rx.recv_timeout(READY_WAIT) != Ok(true) {
        let _ = process.kill();
        end(Running { process, stdin, last: String::new() });
        return Err(std::io::Error::other("it could not open a window on the X server"));
    }
    Ok(Running { process, stdin, last: String::new() })
}

/// What the child says when a button is pressed or a box ticked.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pressed {
    /// The card's [`Panel::id`].
    pub id: String,
    /// The button's [`super::Action::id`], or the box's
    /// [`super::Check::action`].
    pub action: String,
    /// The child put the button's [`super::Action::copy`] text on the
    /// clipboard before saying so.
    #[serde(default)]
    pub copied: bool,
}

// ---- the child --------------------------------------------------------------

/// A card's width, and its inset from the corner of the work area, in
/// logical pixels -- as the meeting card's was.
const WIDTH: i32 = 360;
const MARGIN: i32 = 16;
/// Room around the cards for their shadows, inside the window.
const SHADOW: i32 = 14;
/// Between one card and the next, on top of their shadows.
const GAP: i32 = 10;
/// How many of a card's lines are drawn: the newest.
const MAX_LINES: usize = 3;
/// The most of the work area's height the window is made; a stack taller than
/// that scrolls.
const MOST_OF_AREA: f64 = 0.8;
/// How long a pressed button waits for its answer before it can be pressed
/// again.
const PRESS_WAIT: Duration = Duration::from_secs(4);
/// How long the child stays up, hidden, after its last card, to go on
/// offering the text of a Copy button that was pressed. See `Stack::finish`.
const CLIPBOARD_LINGER: Duration = Duration::from_secs(120);
/// What a line that is still coming ends with.
const CARET: &str = "\u{258d}";

/// Be the panel: draw what comes down stdin until it closes. Called by `run`
/// before anything of the app starts, and never returns.
pub fn run_child() -> ! {
    if let Err(e) = gtk::init() {
        eprintln!("the panel could not start GTK: {e}");
        std::process::exit(1);
    }
    let stack = Stack::build();
    STACK.with(|slot| *slot.borrow_mut() = Some(stack));
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{READY}").and_then(|()| out.flush());
    drop(out);
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<ToChild>(&line) {
                Ok(ToChild::Panels(stack)) => {
                    gtk::glib::idle_add_once(move || with_stack(|s| s.show(&stack)));
                }
                Ok(ToChild::Level { id, level }) => {
                    gtk::glib::idle_add_once(move || with_stack(|s| s.level(&id, level)));
                }
                Err(_) => {}
            }
        }
        gtk::glib::idle_add_once(|| with_stack(Stack::finish));
    });
    gtk::main();
    std::process::exit(0);
}

thread_local! {
    /// The stack of cards, on GTK's thread: its widgets go nowhere else.
    static STACK: RefCell<Option<Rc<Stack>>> = const { RefCell::new(None) };
}

fn with_stack(f: impl FnOnce(&Stack)) {
    let stack = STACK.with(|slot| slot.borrow().clone());
    if let Some(stack) = stack {
        f(&stack);
    }
}

/// Everything the child draws.
struct Stack {
    window: gtk::Window,
    scroll: gtk::ScrolledWindow,
    /// What the window holds: the cards, top to bottom, then `more`.
    column: gtk::Box,
    /// "+3 more", under the cards, when more are up than are drawn.
    more: gtk::Label,
    /// The cards drawn, in the order drawn.
    cards: RefCell<Vec<CardView>>,
    /// The cards whose folded sections the person has opened. Remembered
    /// across every redraw, for as long as the card is on the stack.
    opened: RefCell<HashSet<String>>,
    /// The last level heard for each card, so a card drawn afresh -- one that
    /// came up from beneath the others -- starts where its meter was.
    levels: RefCell<HashMap<String, f32>>,
    /// Text put on the clipboard by a button, which this process owns for as
    /// long as it lives.
    holds_clipboard: Cell<bool>,
}

impl Stack {
    fn build() -> Rc<Stack> {
        use gtk::prelude::*;

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Karma Yoga");
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_skip_taskbar_hint(true);
        window.set_skip_pager_hint(true);
        // Above everything, on every workspace, and never taking the keyboard
        // from whatever has it: pressing a button must not either.
        window.set_keep_above(true);
        window.stick();
        window.set_accept_focus(false);
        window.set_focus_on_map(false);
        window.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
        window.set_app_paintable(true);
        let dark = prefers_dark();
        // The parts the stylesheet leaves to the theme -- a box's tick, a
        // scroll bar -- are drawn in the variant that goes with the cards.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(dark);
        }
        if let Some(screen) = WidgetExt::screen(&window) {
            if let Some(visual) = screen.rgba_visual() {
                window.set_visual(Some(&visual));
            }
            // Added even if some of it did not read: GTK keeps what it could.
            let style = gtk::CssProvider::new();
            if let Err(e) = style.load_from_data(css(dark).as_bytes()) {
                eprintln!("the panel's style did not all read: {e}");
            }
            gtk::StyleContext::add_provider_for_screen(
                &screen,
                &style,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let column = gtk::Box::new(gtk::Orientation::Vertical, GAP);
        column.set_margin_top(SHADOW);
        column.set_margin_bottom(SHADOW);
        column.set_margin_start(SHADOW);
        column.set_margin_end(SHADOW);
        let more = gtk::Label::new(None);
        more.style_context().add_class("more");
        more.set_halign(gtk::Align::Center);
        hide(&more);
        column.pack_start(&more, false, false, 0);

        // A stack taller than the screen scrolls, rather than pushing the
        // last card's buttons off the bottom of it.
        let scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_shadow_type(gtk::ShadowType::None);
        scroll.set_propagate_natural_height(true);
        scroll.add(&column);
        window.add(&scroll);
        scroll.show_all();

        let stack = Rc::new(Stack {
            window,
            scroll,
            column,
            more,
            cards: RefCell::new(Vec::new()),
            opened: RefCell::new(HashSet::new()),
            levels: RefCell::new(HashMap::new()),
            holds_clipboard: Cell::new(false),
        });
        // The cards grow and shrink, and a window manager may move a window
        // that does: back to the corner it is anchored to, every time.
        stack.window.connect_size_allocate(|window, _| place(window));
        stack
    }

    /// Draw `stack`, or take the window down if there is nothing in it.
    fn show(&self, stack: &[Panel]) {
        use gtk::prelude::*;

        forget_closed(&mut self.opened.borrow_mut(), stack);
        self.levels.borrow_mut().retain(|id, _| stack.iter().any(|p| &p.id == id));

        let wanted = super::shown(stack);
        if wanted.is_empty() {
            for card in self.cards.take() {
                self.column.remove(&card.root);
            }
            hide(&self.more);
            self.window.hide();
            return;
        }

        // The cards are taken out while they are changed: a widget that
        // answers a change by telling this stack must find it in a state to
        // be told.
        let mut old = self.cards.take();
        let mut next = Vec::with_capacity(wanted.len());
        for panel in wanted {
            let mut card = match old.iter().position(|c| c.id == panel.id) {
                Some(at) => old.remove(at),
                None => CardView::new(&panel.id),
            };
            let open = self.opened.borrow().contains(&panel.id);
            let level = self.levels.borrow().get(&panel.id).copied().unwrap_or(0.0);
            card.apply(panel, open, level);
            next.push(card);
        }
        // What is left is a card that has gone, or one that moved beyond what
        // is drawn: it is drawn afresh if it comes back.
        for gone in old {
            self.column.remove(&gone.root);
        }
        for (at, card) in next.iter().enumerate() {
            if card.root.parent().is_none() {
                self.column.pack_start(&card.root, false, false, 0);
            }
            self.column.reorder_child(&card.root, at as i32);
        }
        let waiting = super::hidden(stack);
        self.more.set_text(&more_text(waiting));
        self.column.reorder_child(&self.more, next.len() as i32);
        show_if(&self.more, waiting > 0);
        self.cards.replace(next);

        if let Some(area) = monitor_area() {
            self.scroll.set_max_content_height(max_content_height(area.3));
        }
        if !self.window.is_visible() {
            self.window.show_all();
            place(&self.window);
            // Placed again once the window manager has had it: some place a
            // new window themselves first.
            let window = self.window.clone();
            gtk::glib::idle_add_local_once(move || place(&window));
        }
    }

    /// How loud the microphone is for the card `id`, from 0 to 1.
    fn level(&self, id: &str, level: f32) {
        use gtk::prelude::*;

        let level = level.clamp(0.0, 1.0);
        let mut levels = self.levels.borrow_mut();
        match levels.get_mut(id) {
            Some(known) => *known = level,
            None => {
                levels.insert(id.to_string(), level);
            }
        }
        drop(levels);
        if let Some(card) = self.cards.borrow().iter().find(|c| c.id == id) {
            card.meter.set_value(f64::from(level));
        }
    }

    /// A button was pressed. Its card's buttons wait for the answer to it --
    /// a card that changes, or none -- and come back by themselves if none
    /// comes: an answer that failed is said in the app's window, and the card
    /// has to be pressable again.
    fn press(&self, id: &str, action: &str, copy: Option<&str>) {
        let copied = copy.is_some_and(|text| self.copy(text));
        let token = self.cards.borrow().iter().find(|c| c.id == id).map(CardView::lock);
        if let Some(token) = token {
            let id = id.to_string();
            gtk::glib::timeout_add_local_once(PRESS_WAIT, move || {
                with_stack(|s| s.unlock(&id, token));
            });
        }
        report(id, action, copied);
    }

    /// The card `id`'s buttons come back, if it has not been pressed again or
    /// changed in the meantime.
    fn unlock(&self, id: &str, token: u64) {
        if let Some(card) = self.cards.borrow().iter().find(|c| c.id == id) {
            card.unlock(token);
        }
    }

    /// Open or fold the card `id`'s folded sections.
    fn fold(&self, id: &str) {
        let open = {
            let mut opened = self.opened.borrow_mut();
            if opened.remove(id) {
                false
            } else {
                opened.insert(id.to_string());
                true
            }
        };
        if let Some(card) = self.cards.borrow().iter().find(|c| c.id == id) {
            card.sync_fold(open);
        }
    }

    /// Put `text` on the clipboard. Stored as well, for a clipboard manager
    /// that will hold it past this process; and this process holds it itself
    /// for a while ([`Stack::finish`]), since on Wayland there is none that
    /// will.
    fn copy(&self, text: &str) -> bool {
        let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        clipboard.set_text(text);
        clipboard.store();
        self.holds_clipboard.set(true);
        true
    }

    /// Stdin is closed: the last card is down, or the app has gone. Ends the
    /// child, unless a Copy button was pressed -- the text it put on the
    /// clipboard lives only as long as the program that put it there, and the
    /// card that said "copied" is usually taken down the moment it is pressed.
    /// Then it waits, hidden, until somebody else copies something or
    /// [`CLIPBOARD_LINGER`] is up.
    fn finish(&self) {
        use gtk::prelude::*;

        if !self.holds_clipboard.get() {
            gtk::main_quit();
            return;
        }
        self.window.hide();
        // The signal by name: the bindings do not wrap its event.
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).connect_local(
            "owner-change",
            false,
            |_| {
                gtk::main_quit();
                None
            },
        );
        gtk::glib::timeout_add_local_once(CLIPBOARD_LINGER, gtk::main_quit);
    }
}

/// One card on the screen. Built once, when the card comes up, and changed
/// part by part as the card does.
struct CardView {
    id: String,
    /// What it shows now; none until it has been given anything to show.
    shown: Option<Panel>,
    /// The card itself.
    root: gtk::Box,
    dot: gtk::Label,
    kicker: gtk::Label,
    spinner: gtk::Spinner,
    count: gtk::Label,
    title: gtk::Label,
    body: gtk::Label,
    /// The lines said, rebuilt when they change.
    lines: gtk::Box,
    meter: gtk::LevelBar,
    /// The sections, rebuilt when they change.
    sections: gtk::Box,
    /// Each section drawn, and whether it is folded.
    blocks: Vec<(bool, gtk::Box)>,
    /// "More" or "Less", for the card's folded sections.
    fold: gtk::Button,
    /// The buttons, rebuilt when they change.
    actions: gtk::Box,
    /// Counts presses and new faces of the card, so the wait after one press
    /// cannot let the buttons go in the middle of the wait after another.
    pressed: Cell<u64>,
}

impl CardView {
    fn new(id: &str) -> CardView {
        use gtk::prelude::*;

        let label = |class: &str| {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_line_wrap(true);
            label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_max_width_chars(1);
            label.style_context().add_class(class);
            label
        };

        let dot = gtk::Label::new(None);
        dot.style_context().add_class("dot");
        let kicker = gtk::Label::new(None);
        kicker.set_xalign(0.0);
        kicker.style_context().add_class("kicker");
        kicker.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let spinner = gtk::Spinner::new();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        head.pack_start(&dot, false, false, 0);
        head.pack_start(&kicker, true, true, 0);
        head.pack_end(&spinner, false, false, 0);

        let count = label("count");
        let title = label("title");
        title.set_lines(2);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let body = label("body");
        let lines = gtk::Box::new(gtk::Orientation::Vertical, 3);

        let meter = gtk::LevelBar::new();
        meter.set_min_value(0.0);
        meter.set_max_value(1.0);
        // The bar's own bands -- low, high, full -- each have a colour of the
        // theme's; one colour is the card's.
        for band in ["low", "high", "full"] {
            meter.remove_offset_value(Some(band));
        }
        meter.style_context().add_class("meter");

        let sections = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let fold = gtk::Button::with_label(fold_label(false));
        fold.set_can_focus(false);
        fold.set_halign(gtk::Align::Start);
        fold.style_context().add_class("fold");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.style_context().add_class("card");
        root.set_size_request(WIDTH, -1);
        root.pack_start(&head, false, false, 0);
        root.pack_start(&count, false, false, 0);
        root.pack_start(&title, false, false, 0);
        root.pack_start(&body, false, false, 0);
        root.pack_start(&lines, false, false, 0);
        root.pack_start(&meter, false, false, 2);
        root.pack_start(&sections, false, false, 0);
        root.pack_start(&fold, false, false, 0);
        root.pack_start(&actions, false, false, 4);

        // What a card changes the visibility of is kept out of `show_all`, so
        // that showing the card does not show what it has not got.
        hide(&dot);
        hide(&count);
        hide(&body);
        hide(&spinner);
        hide(&lines);
        hide(&meter);
        hide(&sections);
        hide(&fold);
        hide(&actions);
        root.show_all();

        let card = CardView {
            id: id.to_string(),
            shown: None,
            root,
            dot,
            kicker,
            spinner,
            count,
            title,
            body,
            lines,
            meter,
            sections,
            blocks: Vec::new(),
            fold,
            actions,
            pressed: Cell::new(0),
        };
        let id = card.id.clone();
        card.fold.connect_clicked(move |_| with_stack(|s| s.fold(&id)));
        card
    }

    /// Make the card say `panel`, changing only what is not already so.
    /// `open` says whether its folded sections are open; `level` is how the
    /// meter stands.
    fn apply(&mut self, panel: &Panel, open: bool, level: f32) {
        use gtk::prelude::*;

        let changes = match &self.shown {
            Some(shown) => Changes::between(shown, panel),
            None => Changes::all(),
        };
        if changes.head {
            let style = self.root.style_context();
            for tone in TONES {
                style.remove_class(&format!("tone-{}", tone_class(tone)));
            }
            style.add_class(&format!("tone-{}", tone_class(panel.tone)));
            self.kicker.set_text(&kicker_text(&panel.kicker));
            match tone_mark(panel.tone) {
                Some(mark) => {
                    self.dot.set_text(mark);
                    show_if(&self.dot, true);
                }
                None => show_if(&self.dot, false),
            }
        }
        if changes.count {
            self.count.set_text(panel.count.as_deref().unwrap_or_default());
            show_if(&self.count, panel.count.is_some());
        }
        if changes.title {
            self.title.set_text(&panel.title);
        }
        if changes.body {
            self.body.set_text(panel.body.as_deref().unwrap_or_default());
            show_if(&self.body, panel.body.as_deref().is_some_and(|b| !b.is_empty()));
        }
        if changes.lines {
            self.build_lines(&panel.lines);
        }
        if changes.meter {
            show_if(&self.meter, panel.meter);
            self.meter.set_value(f64::from(level));
        }
        if changes.sections {
            self.build_sections(panel);
        }
        if changes.sections {
            self.sync_fold(open);
        }
        if changes.actions {
            self.build_actions(panel);
        }
        if changes.busy {
            show_if(&self.spinner, panel.busy);
            if panel.busy {
                self.spinner.start();
            } else {
                self.spinner.stop();
            }
        }
        self.shown = Some(panel.clone());
    }

    /// The last of what has been said, the speaker in bold and a caret after
    /// what is still coming.
    fn build_lines(&self, lines: &[Line]) {
        use gtk::prelude::*;

        clear(&self.lines);
        let drawn = last_lines(lines);
        for line in &drawn {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_line_wrap(true);
            label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_max_width_chars(1);
            label.set_markup(&line_markup(line));
            label.style_context().add_class("line");
            self.lines.pack_start(&label, false, false, 0);
            // Not `show_all` on the box: it is one `show_all` leaves alone.
            label.show();
        }
        show_if(&self.lines, !drawn.is_empty());
    }

    /// Each section with something in it: its heading, then its items.
    fn build_sections(&mut self, panel: &Panel) {
        use gtk::prelude::*;

        clear(&self.sections);
        self.blocks.clear();
        for section in panel.sections.iter().filter(|s| section_drawn(s)) {
            let block = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let heading = gtk::Label::new(Some(&section.heading));
            heading.set_xalign(0.0);
            heading.style_context().add_class("heading");
            block.pack_start(&heading, false, false, 0);
            for item in &section.items {
                block.pack_start(&item_row(&self.id, item), false, false, 0);
            }
            self.sections.pack_start(&block, false, false, 0);
            block.show_all();
            self.blocks.push((section.folded, block));
        }
        show_if(&self.fold, has_fold(panel));
    }

    /// Fold or open the folded sections, and say which the button now does.
    fn sync_fold(&self, open: bool) {
        use gtk::prelude::*;

        self.fold.set_label(fold_label(open));
        for (folded, block) in &self.blocks {
            show_if(block, block_visible(*folded, open));
        }
        // Nothing to hold, nothing to leave room for.
        show_if(&self.sections, self.blocks.iter().any(|(folded, _)| block_visible(*folded, open)));
    }

    /// The buttons, left to right, the primary one rightmost
    /// ([`super::drawn_order`]).
    fn build_actions(&self, panel: &Panel) {
        use gtk::prelude::*;

        clear(&self.actions);
        for action in super::drawn_order(&panel.actions) {
            self.actions.pack_start(&action_button(&self.id, action), false, false, 0);
        }
        for button in self.actions.children() {
            button.show_all();
        }
        show_if(&self.actions, !panel.actions.is_empty());
        // A new face: a press waiting to be undone is of the old one.
        self.pressed.set(self.pressed.get().wrapping_add(1));
    }

    /// Switch the buttons off, and say which press that was.
    fn lock(&self) -> u64 {
        use gtk::prelude::*;

        for button in self.actions.children() {
            button.set_sensitive(false);
        }
        let token = self.pressed.get().wrapping_add(1);
        self.pressed.set(token);
        token
    }

    /// Switch the buttons back on, if `token` is still the last thing that
    /// happened to them.
    fn unlock(&self, token: u64) {
        use gtk::prelude::*;

        if self.pressed.get() == token {
            for button in self.actions.children() {
                button.set_sensitive(true);
            }
        }
    }
}

// ---- the parts of a card ------------------------------------------------------

/// A button of a card. Pressing it tells the stack, which puts its text on the
/// clipboard first if it is a Copy and tells the app.
fn action_button(card: &str, action: &Action) -> gtk::Button {
    use gtk::prelude::*;

    let button = gtk::Button::with_label(&action.label);
    button.set_can_focus(false);
    if action.primary {
        button.style_context().add_class("primary");
    }
    let (card, id, copy) = (card.to_string(), action.id.clone(), action.copy.clone());
    button.connect_clicked(move |_| with_stack(|s| s.press(&card, &id, copy.as_deref())));
    button
}

/// One entry of a section: its text, a box before it to tick if it has one,
/// and under it a quieter line if it has more to say.
fn item_row(card: &str, item: &Item) -> gtk::Box {
    use gtk::prelude::*;

    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let indent = match &item.check {
        Some(check) => {
            let tick = gtk::CheckButton::with_label(&item.text);
            tick.set_can_focus(false);
            // Wrapped like the rest of the card, and the box itself kept
            // off the label's edge.
            if let Some(text) = tick.child().and_then(|w| w.downcast::<gtk::Label>().ok()) {
                text.set_xalign(0.0);
                text.set_line_wrap(true);
                text.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
                text.set_max_width_chars(1);
            }
            // Set before the handler is connected: the box's own state is
            // not a tick.
            tick.set_active(check.done);
            let (card, action, done) = (card.to_string(), check.action.clone(), check.done);
            // Set while the box is put back, which is not a tick either.
            let quiet = Rc::new(Cell::new(false));
            tick.connect_toggled(move |tick| {
                if toggle(done, tick.is_active(), quiet.get()) == Toggle::Clicked {
                    ticked(&card, &action, done, tick, &quiet);
                }
            });
            row.pack_start(&tick, false, false, 0);
            CHECK_INDENT
        }
        None => {
            let text = gtk::Label::new(Some(&item.text));
            text.set_xalign(0.0);
            text.set_line_wrap(true);
            text.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
            text.set_max_width_chars(1);
            text.style_context().add_class("item");
            row.pack_start(&text, false, false, 0);
            0
        }
    };
    if let Some(detail) = item.detail.as_deref().filter(|d| !d.is_empty()) {
        let label = gtk::Label::new(Some(detail));
        label.set_xalign(0.0);
        label.set_line_wrap(true);
        label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_max_width_chars(1);
        label.set_margin_start(indent);
        label.style_context().add_class("detail");
        row.pack_start(&label, false, false, 0);
    }
    row
}

/// How far a detail line under a box is in from the card's edge: past the box.
const CHECK_INDENT: i32 = 26;

/// What a box makes of its state having changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    /// Nobody clicked it: it is being put to what the shell says.
    Ignore,
    /// The person clicked it.
    Clicked,
}

/// Whether a box whose state has just changed to `active` was clicked. The
/// shell says the point is `done`; a box that now shows the same was not
/// clicked but set, and so was one set while `quiet`.
fn toggle(done: bool, active: bool, quiet: bool) -> Toggle {
    if quiet || active == done { Toggle::Ignore } else { Toggle::Clicked }
}

/// A box was clicked, and GTK has already flipped it. Tells the app, which
/// answers by changing the card -- and the box is made again with what the
/// shell now says -- or does not. The box shows the other state meanwhile,
/// switched off, as the page's does, and if no answer comes in [`PRESS_WAIT`]
/// it is put back to what the shell says: what a box shows must end up what
/// the shell says it is, whether the click did anything or not.
fn ticked(card: &str, action: &str, done: bool, tick: &gtk::CheckButton, quiet: &Rc<Cell<bool>>) {
    use gtk::prelude::*;

    tick.set_sensitive(false);
    let weak = tick.downgrade();
    let quiet = quiet.clone();
    gtk::glib::timeout_add_local_once(PRESS_WAIT, move || {
        if let Some(tick) = weak.upgrade() {
            quiet.set(true);
            tick.set_active(done);
            quiet.set(false);
            tick.set_sensitive(true);
        }
    });
    report(card, action, false);
}

/// Tell the app, up stdout, that a button was pressed or a box ticked.
fn report(id: &str, action: &str, copied: bool) {
    let pressed = Pressed { id: id.to_string(), action: action.to_string(), copied };
    let Ok(line) = serde_json::to_string(&pressed) else { return };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

/// Take every child out of `container`.
fn clear(container: &gtk::Box) {
    use gtk::prelude::*;

    for child in container.children() {
        container.remove(&child);
    }
}

/// Show or hide a widget that `show_all` must leave as it is: one a card shows
/// only sometimes.
fn show_if(widget: &impl gtk::prelude::IsA<gtk::Widget>, shown: bool) {
    use gtk::prelude::*;

    widget.set_no_show_all(true);
    widget.set_visible(shown);
}

/// Hide a widget now, and keep `show_all` from showing it before it is meant
/// to be.
fn hide(widget: &impl gtk::prelude::IsA<gtk::Widget>) {
    show_if(widget, false);
}

// ---- what a card says, without a window ---------------------------------------

/// The kicker as drawn: in capitals, written as a sentence.
fn kicker_text(kicker: &str) -> String {
    kicker.to_uppercase()
}

/// Every tone, for taking the old one's class off.
const TONES: [Tone; 6] =
    [Tone::Neutral, Tone::Listening, Tone::Working, Tone::Success, Tone::Attention, Tone::Trouble];

/// The name of the tone's style, so the stylesheet can colour the kicker, its
/// mark and the meter.
fn tone_class(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral => "neutral",
        Tone::Listening => "listening",
        Tone::Working => "working",
        Tone::Success => "success",
        Tone::Attention => "attention",
        Tone::Trouble => "trouble",
    }
}

/// The mark beside the kicker: a dot for a card with something going on, a
/// tick for one that is done, a bang for one that went wrong, nothing for one
/// that is only saying something.
fn tone_mark(tone: Tone) -> Option<&'static str> {
    match tone {
        Tone::Neutral => None,
        Tone::Success => Some("\u{2713}"),
        Tone::Trouble => Some("!"),
        Tone::Listening | Tone::Working | Tone::Attention => Some("\u{25cf}"),
    }
}

/// The lines drawn: the last [`MAX_LINES`] that have anything to show. One
/// with no words is worth drawing only while it is still coming -- voice
/// typing starts with one, and its caret is how the card says it is waiting.
fn last_lines(lines: &[Line]) -> Vec<&Line> {
    let worth: Vec<&Line> =
        lines.iter().filter(|l| l.pending || !l.text.trim().is_empty()).collect();
    let skip = worth.len().saturating_sub(MAX_LINES);
    worth.into_iter().skip(skip).collect()
}

/// A line as Pango markup: who said it in bold, what they said, and a caret
/// if they are still saying it.
fn line_markup(line: &Line) -> String {
    let mut markup = String::new();
    if let Some(who) = line.who.as_deref().filter(|w| !w.is_empty()) {
        markup.push_str("<b>");
        markup.push_str(&escape_markup(who));
        markup.push_str("</b> ");
    }
    markup.push_str(&escape_markup(&line.text));
    if line.pending {
        markup.push_str(CARET);
    }
    markup
}

/// `text` with what Pango reads as markup written out as itself.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Whether a section is drawn at all: one with nothing in it is a heading over
/// nothing.
fn section_drawn(section: &Section) -> bool {
    !section.items.is_empty()
}

/// Whether a section is drawn now: a folded one only once the card is opened.
fn block_visible(folded: bool, open: bool) -> bool {
    !folded || open
}

/// Whether a card has sections folded away, which is what earns it a button
/// to open them.
fn has_fold(panel: &Panel) -> bool {
    panel.sections.iter().any(|s| s.folded && section_drawn(s))
}

/// What the button that opens and folds a card's sections says.
fn fold_label(open: bool) -> &'static str {
    if open { "Less" } else { "More" }
}

/// What is said under the cards when more are up than are drawn.
fn more_text(waiting: usize) -> String {
    format!("+{waiting} more")
}

/// Forget the cards whose folded sections were opened, once they are no
/// longer on the stack at all -- a card that comes back starts folded.
fn forget_closed(opened: &mut HashSet<String>, stack: &[Panel]) {
    opened.retain(|id| stack.iter().any(|p| &p.id == id));
}

/// Which parts of a card say something other than they did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Changes {
    /// The tone and the kicker.
    head: bool,
    count: bool,
    title: bool,
    body: bool,
    lines: bool,
    meter: bool,
    sections: bool,
    actions: bool,
    busy: bool,
}

impl Changes {
    /// Everything: a card that has said nothing yet.
    fn all() -> Changes {
        Changes {
            head: true,
            count: true,
            title: true,
            body: true,
            lines: true,
            meter: true,
            sections: true,
            actions: true,
            busy: true,
        }
    }

    /// What `new` says that `old` did not.
    fn between(old: &Panel, new: &Panel) -> Changes {
        Changes {
            head: old.tone != new.tone || old.kicker != new.kicker,
            count: old.count != new.count,
            title: old.title != new.title,
            body: old.body != new.body,
            lines: old.lines != new.lines,
            meter: old.meter != new.meter,
            sections: old.sections != new.sections,
            actions: old.actions != new.actions,
            busy: old.busy != new.busy,
        }
    }
}

#[cfg(test)]
impl Changes {
    /// Nothing.
    fn none() -> Changes {
        Changes {
            head: false,
            count: false,
            title: false,
            body: false,
            lines: false,
            meter: false,
            sections: false,
            actions: false,
            busy: false,
        }
    }

    fn any(self) -> bool {
        self != Changes::none()
    }
}

/// How tall the window's cards may be before the stack scrolls, from the work
/// area's height.
fn max_content_height(area_height: i32) -> i32 {
    (f64::from(area_height) * MOST_OF_AREA) as i32
}

/// The top left of the window, for a work area of `(x, y, width, height)`:
/// the top right of the area, less the shadow's room, so the cards themselves
/// sit [`MARGIN`] from the corner.
fn corner(area: (i32, i32, i32, i32)) -> (i32, i32) {
    let (x, y, width, _) = area;
    let window = WIDTH + 2 * SHADOW;
    (x + width - window - MARGIN + SHADOW, (y + MARGIN - SHADOW).max(y))
}

/// The work area of the primary monitor -- below a top bar, clear of a dock --
/// as `(x, y, width, height)`. Where the X server names no primary monitor,
/// as XWayland need not, it is the one the pointer is on, where the person is.
fn monitor_area() -> Option<(i32, i32, i32, i32)> {
    use gtk::prelude::*;

    let display = gtk::gdk::Display::default()?;
    let under_pointer = || {
        let pointer = display.default_seat()?.pointer()?;
        let (_, x, y) = pointer.position();
        display.monitor_at_point(x, y)
    };
    let monitor =
        display.primary_monitor().or_else(under_pointer).or_else(|| display.monitor(0))?;
    let area = monitor.workarea();
    Some((area.x(), area.y(), area.width(), area.height()))
}

/// Put the window in its corner, if it is not there.
fn place(window: &gtk::Window) {
    use gtk::prelude::*;

    let Some(area) = monitor_area() else { return };
    let corner = corner(area);
    if window.position() != corner {
        window.move_(corner.0, corner.1);
    }
}

/// Whether the desktop is set to prefer dark: GNOME's own setting, read only
/// when its schema is there, since asking for one that is not ends the
/// process.
fn prefers_dark() -> bool {
    use gtk::gio::prelude::*;

    crate::gnome::settings("org.gnome.desktop.interface", &["color-scheme"])
        .is_some_and(|settings| settings.string("color-scheme") == "prefer-dark")
}

/// The panel's look: the app's own light or dark theme (`themes.css`), so it
/// reads as the app's even drawn by GTK.
fn css(dark: bool) -> String {
    let (bg, fg, muted, border, hover, accent, shadow, blue, green, red) = if dark {
        (
            "#201f24",
            "#eceaf0",
            "#a8a4b0",
            "rgba(255,255,255,0.09)",
            "rgba(255,255,255,0.06)",
            "#e8853f",
            "rgba(0,0,0,0.45)",
            "#6ea8ff",
            "#5cc27a",
            "#ff7a6e",
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
            "#2f6fdd",
            "#2e7d45",
            "#c2362b",
        )
    };
    // The colour a tone gives the kicker, the mark beside it and the meter.
    let tone = |class: &str, colour: &str| {
        format!(
            ".tone-{class} .kicker, .tone-{class} .dot {{ color: {colour}; }}
             .tone-{class} levelbar block.filled {{ background-color: {colour}; }}
             "
        )
    };
    let tones = [
        tone("neutral", muted),
        tone("working", muted),
        tone("listening", blue),
        tone("attention", accent),
        tone("success", green),
        tone("trouble", red),
    ]
    .concat();
    format!(
        "window {{ background-color: transparent; }}
         scrolledwindow, viewport {{
           background-color: transparent; background-image: none;
           border: none; box-shadow: none;
         }}
         scrolledwindow undershoot, scrolledwindow overshoot {{
           background-image: none; background-color: transparent;
         }}
         .card {{
           background-color: {bg}; color: {fg};
           border: 1px solid {border}; border-radius: 16px;
           padding: 16px 18px 14px;
           box-shadow: 0 6px 18px {shadow};
           font-family: 'Inter', 'Inter Variable', 'Cantarell', 'Ubuntu', sans-serif;
         }}
         .kicker {{ font-size: 9pt; letter-spacing: 0.04em; color: {muted}; }}
         .dot {{ font-size: 8pt; font-weight: 700; color: {muted}; }}
         .count {{ font-size: 21pt; font-weight: 700; font-feature-settings: 'tnum'; }}
         .title {{ font-size: 12pt; font-weight: 600; }}
         .body {{ font-size: 10pt; color: {muted}; }}
         .line {{ font-size: 10pt; }}
         .heading {{
           font-size: 9pt; font-weight: 700; letter-spacing: 0.04em; color: {muted};
         }}
         .item {{ font-size: 10pt; }}
         .detail {{ font-size: 9pt; color: {muted}; }}
         spinner {{ color: {muted}; min-width: 14px; min-height: 14px; }}
         levelbar trough {{
           min-height: 4px; padding: 0; border: none; border-radius: 2px;
           background-color: {hover};
         }}
         levelbar block {{ min-height: 4px; border: none; border-radius: 2px; }}
         levelbar block.empty {{ background-color: transparent; }}
         levelbar block.filled {{ background-color: {muted}; }}
         checkbutton {{ padding: 0; min-height: 22px; font-size: 10pt; color: {fg}; }}
         checkbutton label {{ color: {fg}; }}
         checkbutton check {{
           min-width: 16px; min-height: 16px; border-radius: 5px;
           border: 1.5px solid {muted}; box-shadow: none; background-image: none;
           background-color: transparent; color: #ffffff;
         }}
         checkbutton check:checked {{ background-color: {accent}; border-color: {accent}; }}
         checkbutton:disabled {{ opacity: 0.5; }}
         label.more {{
           font-size: 9pt; color: {muted}; background-color: {bg};
           border: 1px solid {border}; border-radius: 999px; padding: 3px 12px;
         }}
         {tones}
         button {{
           min-height: 32px; padding: 0 12px; border-radius: 999px;
           border: none; box-shadow: none; text-shadow: none;
           background-image: none; background-color: transparent;
           color: {muted}; font-size: 10pt;
         }}
         button:hover {{ background-color: {hover}; color: {fg}; }}
         button.primary {{ background-color: {accent}; color: #ffffff; }}
         button.primary:hover {{ background-color: shade({accent}, 1.08); color: #ffffff; }}
         button.fold {{ min-height: 24px; padding: 0 8px; font-size: 9pt; }}
         button:disabled {{ opacity: 0.5; }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{Check, MAX_SHOWN, Owner};

    fn card(id: &str) -> Panel {
        Panel {
            id: id.into(),
            owner: Owner::Meeting,
            tone: Tone::Neutral,
            kicker: "Meeting".into(),
            title: "Design sync".into(),
            count: None,
            body: None,
            lines: Vec::new(),
            meter: false,
            sections: Vec::new(),
            actions: Vec::new(),
            busy: false,
        }
    }

    fn line(who: Option<&str>, text: &str, pending: bool) -> Line {
        Line { who: who.map(str::to_string), text: text.into(), pending }
    }

    fn section(heading: &str, folded: bool, items: usize) -> Section {
        Section {
            heading: heading.into(),
            items: (0..items)
                .map(|n| Item { text: format!("item {n}"), detail: None, check: None })
                .collect(),
            folded,
        }
    }

    // ---- the lines the app and the child speak ----

    #[test]
    fn a_stack_goes_down_the_pipe_and_reads_back() {
        let mut meeting = card("event:1:abc");
        meeting.actions.push(Action {
            id: "take".into(),
            label: "Take notes".into(),
            primary: true,
            copy: None,
        });
        let message = ToChild::Panels(vec![meeting, card("talk")]);
        let line = serde_json::to_string(&message).unwrap();
        assert!(!line.contains('\n'), "one line to a message");
        assert!(line.starts_with(r#"{"panels":["#), "{line}");
        assert_eq!(serde_json::from_str::<ToChild>(&line).unwrap(), message);
    }

    #[test]
    fn an_empty_stack_is_a_message_too() {
        let line = serde_json::to_string(&ToChild::Panels(Vec::new())).unwrap();
        assert_eq!(line, r#"{"panels":[]}"#);
        assert_eq!(serde_json::from_str::<ToChild>(&line).unwrap(), ToChild::Panels(Vec::new()));
    }

    #[test]
    fn a_level_goes_down_the_pipe_and_reads_back() {
        let message = ToChild::Level { id: "talk".into(), level: 0.25 };
        let line = serde_json::to_string(&message).unwrap();
        assert_eq!(line, r#"{"level":{"id":"talk","level":0.25}}"#);
        assert_eq!(serde_json::from_str::<ToChild>(&line).unwrap(), message);
    }

    #[test]
    fn what_the_child_says_that_is_not_a_message_is_not_one() {
        assert!(serde_json::from_str::<ToChild>("not json").is_err());
        assert!(serde_json::from_str::<ToChild>(r#"{"ready":true}"#).is_err());
        assert!(serde_json::from_str::<Pressed>(READY).is_err(), "ready is not a press");
    }

    #[test]
    fn a_press_goes_up_the_pipe_and_reads_back() {
        let pressed = Pressed { id: "voice-typing".into(), action: "copy".into(), copied: true };
        let line = serde_json::to_string(&pressed).unwrap();
        assert_eq!(line, r#"{"id":"voice-typing","action":"copy","copied":true}"#);
        assert_eq!(serde_json::from_str::<Pressed>(&line).unwrap(), pressed);
    }

    #[test]
    fn a_press_that_says_nothing_of_copying_did_not_copy() {
        let pressed: Pressed = serde_json::from_str(r#"{"id":"talk","action":"end"}"#).unwrap();
        assert!(!pressed.copied);
    }

    // ---- which cards are drawn ----

    #[test]
    fn the_child_draws_what_the_page_does() {
        let stack: Vec<Panel> = (0..MAX_SHOWN + 2).map(|n| card(&format!("c{n}"))).collect();
        let drawn: Vec<&str> = crate::panel::shown(&stack).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(drawn, ["c0", "c1", "c2"]);
        assert_eq!(more_text(crate::panel::hidden(&stack)), "+2 more");
    }

    // ---- what a card says ----

    #[test]
    fn the_kicker_is_drawn_in_capitals() {
        assert_eq!(kicker_text("Meeting \u{b7} in 1 min"), "MEETING \u{b7} IN 1 MIN");
    }

    #[test]
    fn every_tone_has_its_own_style_and_the_list_has_them_all() {
        let names: HashSet<&str> = TONES.iter().map(|t| tone_class(*t)).collect();
        assert_eq!(names.len(), TONES.len());
        assert_eq!(tone_class(Tone::Attention), "attention");
        assert_eq!(tone_class(Tone::Trouble), "trouble");
    }

    #[test]
    fn a_card_that_only_says_something_has_no_mark() {
        assert_eq!(tone_mark(Tone::Neutral), None);
        assert!(tone_mark(Tone::Listening).is_some());
        assert_eq!(tone_mark(Tone::Success), Some("\u{2713}"));
        assert_eq!(tone_mark(Tone::Trouble), Some("!"));
    }

    #[test]
    fn only_the_last_three_lines_are_drawn() {
        let lines: Vec<Line> =
            (0..5).map(|n| line(Some("You"), &format!("said {n}"), false)).collect();
        let drawn = last_lines(&lines);
        let said: Vec<&str> = drawn.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(said, ["said 2", "said 3", "said 4"]);
    }

    #[test]
    fn a_line_with_no_words_is_drawn_only_while_it_is_coming() {
        let lines = vec![
            line(Some("You"), "hello", false),
            line(Some("Assistant"), "  ", false),
            line(None, "", true),
        ];
        let drawn = last_lines(&lines);
        assert_eq!(drawn.len(), 2);
        assert_eq!(drawn[0].text, "hello");
        assert!(drawn[1].pending);
    }

    #[test]
    fn who_said_it_is_in_bold_before_the_words() {
        let markup = line_markup(&line(Some("You"), "hello there", false));
        assert_eq!(markup, "<b>You</b> hello there");
    }

    #[test]
    fn a_line_with_nobody_speaking_is_only_its_words() {
        assert_eq!(line_markup(&line(None, "hello", false)), "hello");
        assert_eq!(line_markup(&line(Some(""), "hello", false)), "hello");
    }

    #[test]
    fn a_line_still_coming_ends_with_a_caret() {
        assert_eq!(line_markup(&line(None, "hello", true)), "hello\u{258d}");
        assert_eq!(line_markup(&line(None, "", true)), "\u{258d}");
        assert_eq!(
            line_markup(&line(Some("Assistant"), "so", true)),
            "<b>Assistant</b> so\u{258d}"
        );
    }

    #[test]
    fn what_was_said_cannot_be_read_as_markup() {
        let markup = line_markup(&line(Some("A&B"), r#"<b>5 > 3</b> & "it's""#, false));
        assert_eq!(markup, "<b>A&amp;B</b> &lt;b&gt;5 &gt; 3&lt;/b&gt; &amp; &quot;it&#39;s&quot;");
    }

    // ---- boxes ----

    #[test]
    fn a_box_the_person_clicked_is_one_that_now_shows_the_other_state() {
        assert_eq!(toggle(false, true, false), Toggle::Clicked, "ticked");
        assert_eq!(toggle(true, false, false), Toggle::Clicked, "cleared");
    }

    #[test]
    fn a_box_set_to_what_the_shell_says_was_not_clicked() {
        assert_eq!(toggle(false, false, false), Toggle::Ignore);
        assert_eq!(toggle(true, true, false), Toggle::Ignore);
    }

    #[test]
    fn a_box_put_back_is_not_a_click_whatever_it_shows() {
        // Put back to `done`, with the handler told to be quiet.
        for done in [false, true] {
            for active in [false, true] {
                assert_eq!(toggle(done, active, true), Toggle::Ignore);
            }
        }
    }

    // ---- folded sections ----

    #[test]
    fn a_folded_section_is_drawn_only_once_the_card_is_opened() {
        assert!(block_visible(false, false));
        assert!(block_visible(false, true));
        assert!(!block_visible(true, false));
        assert!(block_visible(true, true));
    }

    #[test]
    fn a_section_with_nothing_in_it_is_never_drawn() {
        assert!(!section_drawn(&section("Empty", false, 0)));
        assert!(!section_drawn(&section("Empty", true, 0)));
        assert!(section_drawn(&section("Agenda", false, 1)));
    }

    #[test]
    fn a_card_has_a_more_button_only_with_something_folded_in_it() {
        let mut panel = card("a");
        assert!(!has_fold(&panel));
        panel.sections = vec![section("Agenda", false, 3)];
        assert!(!has_fold(&panel));
        panel.sections.push(section("Context", true, 0));
        assert!(!has_fold(&panel), "folded, but nothing to show when opened");
        panel.sections.push(section("People", true, 1));
        assert!(has_fold(&panel));
    }

    #[test]
    fn the_button_says_what_it_will_do() {
        assert_eq!(fold_label(false), "More");
        assert_eq!(fold_label(true), "Less");
    }

    #[test]
    fn opened_cards_are_remembered_while_they_are_on_the_stack() {
        let mut opened: HashSet<String> = ["a", "b", "gone"].map(String::from).into();
        // Drawn or not -- `b` is beyond what is drawn -- it is still up.
        forget_closed(&mut opened, &[card("a"), card("b")]);
        let mut left: Vec<&str> = opened.iter().map(String::as_str).collect();
        left.sort_unstable();
        assert_eq!(left, ["a", "b"]);
        forget_closed(&mut opened, &[]);
        assert!(opened.is_empty());
    }

    // ---- what changed ----

    #[test]
    fn a_card_that_says_the_same_changes_nothing() {
        let mut panel = card("a");
        panel.lines.push(line(None, "hello", true));
        panel.sections.push(section("Agenda", false, 2));
        assert!(!Changes::between(&panel, &panel.clone()).any());
    }

    #[test]
    fn a_countdown_changes_the_count_and_nothing_else() {
        let mut before = card("a");
        before.count = Some("12s".into());
        before.actions.push(Action {
            id: "take".into(),
            label: "Take notes".into(),
            primary: true,
            copy: None,
        });
        let mut after = before.clone();
        after.count = Some("11s".into());
        let changes = Changes::between(&before, &after);
        assert!(changes.count);
        assert_eq!(Changes { count: false, ..changes }, Changes::none());
        assert!(!changes.actions, "its buttons are left under the pointer");
    }

    #[test]
    fn each_part_is_noticed_changing_on_its_own() {
        let base = card("a");
        let with = |edit: fn(&mut Panel)| {
            let mut changed = base.clone();
            edit(&mut changed);
            Changes::between(&base, &changed)
        };
        assert!(with(|p| p.tone = Tone::Trouble).head);
        assert!(with(|p| p.kicker = "Listening".into()).head);
        assert!(with(|p| p.title = "Thinking".into()).title);
        assert!(with(|p| p.body = Some("Where".into())).body);
        assert!(with(|p| p.lines.push(line(None, "x", false))).lines);
        assert!(with(|p| p.meter = true).meter);
        assert!(with(|p| p.sections.push(section("S", false, 1))).sections);
        assert!(with(|p| p.busy = true).busy);
        assert!(
            with(|p| p.actions.push(Action {
                id: "x".into(),
                label: "X".into(),
                primary: false,
                copy: None,
            }))
            .actions
        );
        // And only that part.
        let only_title = with(|p| p.title = "Thinking".into());
        assert_eq!(Changes { title: false, ..only_title }, Changes::none());
    }

    #[test]
    fn a_tick_in_a_section_changes_the_sections() {
        let mut before = card("a");
        before.sections.push(Section {
            heading: "To raise".into(),
            items: vec![Item {
                text: "Budget".into(),
                detail: None,
                check: Some(Check { action: "done:7".into(), done: false }),
            }],
            folded: false,
        });
        let mut after = before.clone();
        after.sections[0].items[0].check = Some(Check { action: "done:7".into(), done: true });
        assert!(Changes::between(&before, &after).sections);
    }

    #[test]
    fn a_card_never_drawn_changes_everything() {
        assert!(Changes::all().any());
        assert_ne!(Changes::all(), Changes::none());
    }

    // ---- where it goes ----

    #[test]
    fn the_cards_sit_a_margin_from_the_corner_of_the_work_area() {
        // The window is the card and its shadow's room on each side.
        let (x, y) = corner((0, 0, 1920, 1080));
        assert_eq!(x + SHADOW + WIDTH, 1920 - MARGIN);
        assert_eq!(y + SHADOW, MARGIN);
    }

    #[test]
    fn the_corner_is_below_a_top_bar_and_on_the_monitor_it_is_on() {
        let (x, y) = corner((1920, 32, 1280, 688));
        assert_eq!(x + SHADOW + WIDTH, 1920 + 1280 - MARGIN);
        assert!(y >= 32, "never above the work area");
    }

    #[test]
    fn the_window_never_starts_above_the_work_area() {
        let (_, y) = corner((0, 0, 800, 600));
        assert!(y >= 0);
        let (_, y) = corner((0, 40, 800, 560));
        assert!(y >= 40);
    }

    #[test]
    fn the_stack_scrolls_past_most_of_the_work_area() {
        assert_eq!(max_content_height(1000), 800);
        assert_eq!(max_content_height(768), 614);
    }

    #[test]
    fn the_stylesheet_reads_for_both_themes() {
        // GTK parses it for real; here only that every named part is there.
        for dark in [false, true] {
            let sheet = css(dark);
            for tone in TONES {
                assert!(sheet.contains(&format!(".tone-{} .kicker", tone_class(tone))));
            }
            for part in [".card", ".count", "levelbar", "checkbutton", "button.primary"] {
                assert!(sheet.contains(part), "{part}");
            }
        }
        assert_ne!(css(false), css(true));
    }
}
