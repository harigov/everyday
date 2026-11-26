//! The panel on Wayland, kept in front of every other window: this program run
//! again under X11 (through XWayland), as a small Tauri app with one webview
//! window drawing the same page (`ui/src/panel/`) the panel is drawn with
//! everywhere else.
//!
//! Wayland gives an application no way to keep a window above the others, or
//! to put one where it likes. The panel's window in the app went behind the
//! meeting the moment the meeting was clicked, and GNOME puts a window that
//! opens without the keyboard behind the focused one to begin with -- so a
//! countdown was only ever seen over this app's own window, if at all. An X11
//! window can ask for both, and a Wayland desktop runs X11 programs through
//! XWayland. So on Wayland, with XWayland there, the panel is this same
//! program run a second time as a child process under X11
//! (`GDK_BACKEND=x11`): the panel's window ([`super::window`]), its page and
//! the panel's own commands, and nothing else -- no vault, no tray, no keys,
//! no second-instance check, no logging (its stdout is how it talks).
//!
//! # Talking
//!
//! The two speak in lines of JSON. Down the child's stdin, a [`ToChild`]: the
//! whole stack of cards, with the hub's expiry times filled in (empty hides
//! the window), a level for one card's meter, the theme, or "back to the
//! corner". Up its stdout, a [`FromChild`]: ready, a press, a hold. The app
//! still owns every answer -- the child only draws and reports, so losing it
//! cannot start a recording or send a reply -- and its stdout carries other
//! lines too (GLib and WebKit write there), which are not JSON and are
//! passed over.
//!
//! # Its life
//!
//! Started when the first card goes up. It says it is ready when its page
//! first asks for the stack; one that has not within [`READY_WAIT`] -- no X
//! server it may talk to, XWayland slow to start -- is given up on, and the
//! cards that are up are drawn by the window in the app until they have all
//! gone ([`FAILED`]), when that window is taken down again. One that dies or
//! stops listening is started again, once while cards are up: the second
//! time, it is given up on as one that never said it was ready is.
//! After the last card goes the child stays, hidden, for [`KEEP_HIDDEN`]: a
//! webview takes a moment to start, and the next card should not wait for
//! one. Then its stdin is closed, which ends it -- as does the app ending,
//! however it ends -- unless a Copy button was pressed on it in the last
//! [`CLIPBOARD_LINGER`]: text put on an X11 clipboard lives only as long as
//! the program that put it there, so it waits, hidden, that long, or until
//! somebody else copies something.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use super::Panel;

/// The argument that makes this program the panel rather than the app.
pub const CHILD_FLAG: &str = "--panel";

/// How long the child has to say it is ready: XWayland may have to start,
/// and then a webview.
const READY_WAIT: Duration = Duration::from_secs(10);

/// How long the child stays, hidden, after the last card goes.
const KEEP_HIDDEN: Duration = Duration::from_secs(10 * 60);

/// How long the child outlives its stdin after a Copy button was pressed on
/// it, holding the clipboard.
const CLIPBOARD_LINGER: Duration = Duration::from_secs(120);

/// What the app tells the child, a line of JSON at a time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToChild {
    /// The whole stack, with the hub's expiry times filled in. Empty hides the
    /// window (the child stays, hidden, a while, for the next card).
    Panels(Vec<Panel>),
    /// How loud the microphone is for one card's meter, from 0 to 1.
    Level { id: String, level: f32 },
    /// The theme to draw in.
    Theme(Option<String>),
    /// Put the window back in its corner and forget where it was moved to.
    ResetPlace,
}

/// What the child tells the app, a line of JSON at a time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FromChild {
    /// The page has loaded and asked for the stack: from now on what is sent
    /// is seen.
    Ready,
    /// A button, a box or a submit; answered by [`super::on_action`].
    Pressed {
        id: String,
        action: String,
        #[serde(default)]
        copied: bool,
        #[serde(default)]
        text: Option<String>,
    },
    /// See [`super::hold`].
    Hold { id: String, held: bool },
}

// ---- the app's side ---------------------------------------------------------

/// The child drawing the panel.
struct Running {
    process: Child,
    stdin: ChildStdin,
    /// The last stack sent: a countdown asks several times a second, and only
    /// a second gone by changes what a card says.
    last: String,
}

/// What wakes the thread that keeps the child up to date.
#[derive(Debug, Clone, PartialEq)]
enum Wake {
    /// The stack changed; send it as it is now.
    Stack,
    /// A level for one card's meter.
    Level(String, f32),
    /// The theme to draw in.
    Theme(Option<String>),
    /// Back to the corner.
    ResetPlace,
}

/// What the thread has been woken for since it last looked: only the latest
/// of each thing matters.
#[derive(Debug, Default, PartialEq)]
struct Batch {
    stack: bool,
    /// Latest level per card, in the order the cards were first heard from.
    levels: Vec<(String, f32)>,
    /// The latest theme, if one was set.
    theme: Option<Option<String>>,
    reset: bool,
}

impl Batch {
    fn add(&mut self, wake: Wake) {
        match wake {
            Wake::Stack => self.stack = true,
            Wake::Level(id, level) => {
                match self.levels.iter_mut().find(|(known, _)| *known == id) {
                    Some(heard) => heard.1 = level,
                    None => self.levels.push((id, level)),
                }
            }
            Wake::Theme(theme) => self.theme = Some(theme),
            Wake::ResetPlace => self.reset = true,
        }
    }
}

/// Wakes the thread that keeps the child up to date.
static WORKER: OnceLock<Sender<Wake>> = OnceLock::new();

/// Set when the child could not draw the cards it was asked for, so they are
/// drawn in the app until they go, rather than a new child being tried for
/// every tick of a countdown. The next card to go up after all are down tries
/// again.
static FAILED: AtomicBool = AtomicBool::new(false);

/// Whether the panel can be drawn this way: there is an X server to draw it
/// on, and drawing the cards that are up on it has not failed. Asked only in a
/// Wayland session; otherwise the panel stays in this process
/// ([`super::window`]'s own window).
pub fn available() -> bool {
    !FAILED.load(Ordering::Relaxed) && std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Bring the child up to date with the stack, and start it if a card is up.
/// Done on a thread of its own and returns at once: starting the child takes
/// a moment, and the callers are a countdown tick, the meeting watcher and a
/// microphone's worth of words.
pub fn refresh(app: &AppHandle) {
    let _ = WORKER.get_or_init(|| work(app.clone())).send(Wake::Stack);
}

/// Take the panel down, if this way of drawing it was ever started.
pub fn dismiss() {
    tell_worker(Wake::Stack);
}

/// How loud the microphone is for the card `id`, if the child is drawing it.
pub fn level(id: &str, level: f32) {
    tell_worker(Wake::Level(id.to_string(), level));
}

/// The theme to draw in, if the child is running. One started later is told
/// the theme as it is then.
pub fn theme(theme: Option<String>) {
    tell_worker(Wake::Theme(theme));
}

/// Put the child's window back in its corner, if it is running. One started
/// later reads the place afresh.
pub fn reset_place() {
    tell_worker(Wake::ResetPlace);
}

fn tell_worker(wake: Wake) {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(wake);
    }
}

/// The thread [`refresh`] wakes. One, so a stack and the stack after it are
/// never sent out of order, and only it ever touches the child.
fn work(app: AppHandle) -> Sender<Wake> {
    let (wake, woken) = channel::<Wake>();
    let spawned = std::thread::Builder::new().name("panel-child".into()).spawn(move || {
        let mut running: Option<Running> = None;
        // When the child, hidden with no card to draw, is let go.
        let mut idle_until: Option<Instant> = None;
        // A child has died, or stopped listening, and been started again
        // since the stack was last empty.
        let mut restarted = false;
        loop {
            let first = match idle_until {
                Some(until) => {
                    match woken.recv_timeout(until.saturating_duration_since(Instant::now())) {
                        Ok(first) => first,
                        Err(RecvTimeoutError::Timeout) => {
                            idle_until = None;
                            if let Some(gone) = running.take() {
                                end(gone);
                            }
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match woken.recv() {
                    Ok(first) => first,
                    Err(_) => break,
                },
            };
            let mut batch = Batch::default();
            batch.add(first);
            while let Ok(next) = woken.try_recv() {
                batch.add(next);
            }
            // A child not running is told the theme when it starts, and reads
            // its place then: nothing is lost by sending these to nobody.
            if let Some(child) = running.as_mut() {
                if let Some(theme) = batch.theme.take() {
                    let _ = send(&mut child.stdin, &ToChild::Theme(theme));
                }
                if batch.reset {
                    let _ = send(&mut child.stdin, &ToChild::ResetPlace);
                }
            }
            if batch.stack {
                let stack = super::for_renderers(&app);
                if stack.is_empty() {
                    FAILED.store(false, Ordering::Relaxed);
                    restarted = false;
                    // Whatever this process drew when the child failed goes
                    // with the cards.
                    take_down_here(&app);
                    // The child may have been moved, or put back, since this
                    // process last read where the panel goes.
                    super::place::reload();
                    if let Some(child) = running.as_mut() {
                        if show(child, Vec::new()) {
                            idle_until = Some(Instant::now() + KEEP_HIDDEN);
                        } else if let Some(gone) = running.take() {
                            end(gone);
                        }
                    }
                } else if !FAILED.load(Ordering::Relaxed) {
                    idle_until = None;
                    if draw(&app, &mut running, stack, &mut restarted) {
                        // Drawn by the child, so not here as well.
                        take_down_here(&app);
                    } else {
                        FAILED.store(true, Ordering::Relaxed);
                        super::place::reload();
                        // Drawn in this process instead, now `available`
                        // says no -- if there is still anything to draw: a
                        // stack emptied since has woken this thread again,
                        // which takes the failure back.
                        if !super::for_renderers(&app).is_empty() {
                            super::window::present(&app);
                        }
                    }
                }
            }
            if let Some(child) = running.as_mut()
                && !FAILED.load(Ordering::Relaxed)
            {
                for (id, level) in batch.levels {
                    let _ = send(&mut child.stdin, &ToChild::Level { id, level });
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

/// Take down the window this process put up when the child could not draw
/// the cards, if it is up. The hub asks for that window only while
/// `available` says no, and asks the child otherwise -- so once the child
/// draws again, or there is nothing to draw, nothing else would ever take it
/// down; and a hub that asked for it just as the child recovered would have
/// the cards drawn twice.
fn take_down_here(app: &AppHandle) {
    if super::window::drawing() {
        super::window::dismiss_here(app);
    }
}

/// One line of JSON down the child's stdin.
fn send(stdin: &mut ChildStdin, message: &ToChild) -> std::io::Result<()> {
    let line = serde_json::to_string(message).map_err(std::io::Error::other)?;
    write_line(stdin, &line)
}

fn write_line(stdin: &mut ChildStdin, line: &str) -> std::io::Result<()> {
    writeln!(stdin, "{line}").and_then(|()| stdin.flush())
}

/// Send a running child `stack`, unless it is what it was last sent. False if
/// it could not be sent.
fn show(child: &mut Running, stack: Vec<Panel>) -> bool {
    let Ok(line) = serde_json::to_string(&ToChild::Panels(stack)) else { return false };
    if child.last == line {
        return true;
    }
    match write_line(&mut child.stdin, &line) {
        Ok(()) => {
            child.last = line;
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "the panel stopped listening");
            false
        }
    }
}

/// Have the child draw `stack`, starting it if it is not running. False if it
/// could not be drawn this way.
///
/// A child that died -- it crashed, or somebody closed it -- or stopped
/// listening is started again once while cards are up (`restarted`, which
/// the caller clears when the stack empties). One that goes again is not: a
/// child that cannot draw these cards would otherwise be started afresh,
/// with all a webview's starting, on every tick of a countdown.
fn draw(
    app: &AppHandle,
    running: &mut Option<Running>,
    stack: Vec<Panel>,
    restarted: &mut bool,
) -> bool {
    let mut lost = false;
    if running.as_mut().is_some_and(|r| !matches!(r.process.try_wait(), Ok(None)))
        && let Some(gone) = running.take()
    {
        end(gone);
        lost = true;
    }
    if let Some(child) = running.as_mut() {
        if show(child, stack.clone()) {
            return true;
        }
        if let Some(gone) = running.take() {
            end(gone);
        }
        lost = true;
    }
    if !may_start(lost, restarted) {
        tracing::warn!("the panel's child stopped again; the panel is drawn in the app");
        return false;
    }
    match start(app, stack) {
        Ok(started) => {
            *running = Some(started);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "the panel could not be drawn on X11");
            false
        }
    }
}

/// Whether a child may be started: always where none was lost (the first of
/// a run of cards, or one let go while hidden), and once where one was --
/// which `restarted` keeps count of.
fn may_start(lost: bool, restarted: &mut bool) -> bool {
    if !lost {
        return true;
    }
    !std::mem::replace(restarted, true)
}

/// Close the child's stdin, which ends it, and reap it off this thread.
fn end(running: Running) {
    let Running { mut process, stdin, .. } = running;
    drop(stdin);
    std::thread::spawn(move || {
        let _ = process.wait();
    });
}

/// Run this program again as the panel, under X11; tell it the theme and the
/// cards, wait for its page to say it has them, and listen for its presses.
///
/// The cards go before the wait, not after: the child's window is made when
/// it has a card to show, and it is that window's page that says it is
/// ready.
fn start(app: &AppHandle, stack: Vec<Panel>) -> std::io::Result<Running> {
    // This very program, even if a newer build has replaced the file on disk
    // since it started: the child and the app must agree on what they say.
    let mut process = Command::new("/proc/self/exe")
        .arg(CHILD_FLAG)
        .env("GDK_BACKEND", "x11")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let (Some(mut stdin), Some(stdout)) = (process.stdin.take(), process.stdout.take()) else {
        let _ = process.kill();
        let _ = process.wait();
        return Err(std::io::Error::other("the panel was started without its pipes"));
    };
    let (ready_tx, ready_rx) = channel::<()>();
    let listening = app.clone();
    let spawned = std::thread::Builder::new()
        .name("panel-presses".into())
        .spawn(move || listen(listening, stdout, ready_tx));
    if let Err(e) = spawned {
        let _ = process.kill();
        end(Running { process, stdin, last: String::new() });
        return Err(e);
    }
    let line = serde_json::to_string(&ToChild::Panels(stack)).map_err(std::io::Error::other)?;
    let sent = send(&mut stdin, &ToChild::Theme(super::theme(app)))
        .and_then(|()| write_line(&mut stdin, &line));
    let ready = sent.and_then(|()| {
        ready_rx
            .recv_timeout(READY_WAIT)
            .map_err(|_| std::io::Error::other("it did not open its window in time"))
    });
    if let Err(e) = ready {
        let _ = process.kill();
        end(Running { process, stdin, last: String::new() });
        return Err(e);
    }
    Ok(Running { process, stdin, last: line })
}

/// Read what the child says until it closes its stdout: that it is ready,
/// once, then presses and holds, which are answered here as if the page in
/// this process had sent them. Anything that is not one of those -- a
/// library printing -- is passed over.
fn listen(app: AppHandle, stdout: ChildStdout, ready: Sender<()>) {
    let mut ready = Some(ready);
    for line in std::io::BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Some(said) = heard(&line) else { continue };
        match said {
            FromChild::Ready => {
                // A new child's page: holds the last one left are nobody's.
                super::renderer_loaded(&app);
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
            }
            FromChild::Pressed { id, action, copied, text } => {
                super::on_action(&app, &id, &action, copied, text);
            }
            FromChild::Hold { id, held } => super::hold(&app, &id, held),
        }
    }
}

/// A line of the child's stdout, if it is the child talking.
fn heard(line: &str) -> Option<FromChild> {
    serde_json::from_str(line.trim()).ok()
}

// ---- the child ------------------------------------------------------------------

/// Set at the start of [`run_child`]: this process is the child.
static IS_CHILD: AtomicBool = AtomicBool::new(false);

/// When a Copy button was last pressed here: the clipboard is this process's
/// for as long as it lives.
static COPIED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether this process is the child: then a press goes up the pipe rather
/// than to an owner ([`super::on_action`]).
pub fn is_child() -> bool {
    IS_CHILD.load(Ordering::Relaxed)
}

/// Where the child's webview keeps what a webview keeps: a folder of its own
/// in the cache, never the app's, which the app has open.
pub fn webview_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().cache_dir().ok().map(|dir| dir.join("karmayoga").join("panel-webview"))
}

/// Say `said` to the app: one line on stdout.
fn tell(said: &FromChild) {
    let Ok(line) = serde_json::to_string(said) else { return };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

/// The page has asked for the stack: the child is ready -- the first time --
/// or its page has loaded again (WebKit's web process restarted it). Said
/// every time: the app lets go of every hold when it hears it
/// (`renderer_loaded`), and a page that reloaded has forgotten what it held.
pub fn page_loaded() {
    tell(&FromChild::Ready);
}

/// A press on the child's page, sent up to the app, which owns the cards.
pub fn pressed(id: &str, action: &str, copied: bool, text: Option<String>) {
    if copied {
        *COPIED_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
    tell(&FromChild::Pressed { id: id.to_string(), action: action.to_string(), copied, text });
}

/// A card held or let go on the child's page, sent up to the app, which keeps
/// the cards' time.
pub fn held(id: &str, held: bool) {
    tell(&FromChild::Hold { id: id.to_string(), held });
}

/// Run as the child: the Tauri app drawing the panel, until stdin closes.
/// Called by `run` before anything of the app starts, logging included, and
/// never returns.
pub fn run_child() -> ! {
    IS_CHILD.store(true, Ordering::SeqCst);
    let built = tauri::Builder::default()
        .manage(super::Panels::default())
        .setup(|app| {
            // On the main thread, as finding out what the display is must be.
            crate::writing::detect(app.handle());
            let handle = app.handle().clone();
            std::thread::Builder::new().name("panel-orders".into()).spawn(move || obey(handle))?;
            Ok(())
        })
        // The panel's page's commands, and nothing else: the same functions
        // the app registers, which send presses and holds up the pipe here.
        .invoke_handler(tauri::generate_handler![
            super::panel_state,
            super::panel_action,
            super::panel_resize,
            super::panel_focus,
            super::panel_hold,
            super::panel_drag,
            super::panel_reset_place,
            super::panel_theme,
        ])
        .on_window_event(|window, event| {
            if window.label() != super::window::LABEL {
                return;
            }
            match event {
                tauri::WindowEvent::Moved(at) => super::window::moved(window.app_handle(), *at),
                // Closing the panel is taking it down until the next card.
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    super::window::closed(window.app_handle());
                }
                _ => {}
            }
        })
        .build(crate::context());
    match built {
        Ok(app) => app.run(|_, event| {
            // The window is only ever hidden; but should it go, the child
            // still ends when the app says, not when it has no window.
            if let tauri::RunEvent::ExitRequested { code: None, api, .. } = event {
                api.prevent_exit();
            }
        }),
        Err(e) => {
            eprintln!("the panel could not start: {e}");
            std::process::exit(1);
        }
    }
    std::process::exit(0)
}

/// Do what the app says, a line of stdin at a time, until it closes.
fn obey(app: AppHandle) {
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(order) = serde_json::from_str::<ToChild>(&line) else { continue };
        match order {
            ToChild::Panels(stack) => super::replace_all(&app, stack),
            ToChild::Level { id, level } => super::level(&app, &id, level),
            ToChild::Theme(theme) => super::set_theme(&app, theme),
            ToChild::ResetPlace => super::window::reset_place(&app),
        }
    }
    finish(&app);
}

/// Stdin is closed: the app has let the child go, or has gone. Ends the
/// child, unless a Copy button was pressed lately -- the text it put on the
/// clipboard lives only as long as this process, and the card that said
/// "Copied" is usually taken down the moment it is pressed. Then the window
/// goes and the child waits until somebody else copies something or
/// [`CLIPBOARD_LINGER`] is up.
fn finish(app: &AppHandle) {
    let copied = COPIED_AT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|at| at.elapsed() < CLIPBOARD_LINGER);
    if !copied {
        app.exit(0);
        return;
    }
    super::replace_all(app, Vec::new());
    let quit = app.clone();
    let _ = app.run_on_main_thread(move || {
        use gtk::prelude::*;
        // The signal by name: the bindings do not wrap its event.
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).connect_local(
            "owner-change",
            false,
            move |_| {
                quit.exit(0);
                None
            },
        );
    });
    std::thread::sleep(CLIPBOARD_LINGER);
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{Kind, Owner};

    #[test]
    fn what_the_app_says_reads_back_as_it_was_said() {
        let panel = Panel::new("talk", Owner::Talk, Kind::Assistant, "Assistant", "Listening");
        for message in [
            ToChild::Panels(vec![panel]),
            ToChild::Panels(Vec::new()),
            ToChild::Level { id: "talk".into(), level: 0.25 },
            ToChild::Theme(Some("dusk".into())),
            ToChild::Theme(None),
            ToChild::ResetPlace,
        ] {
            let line = serde_json::to_string(&message).unwrap();
            assert!(!line.contains('\n'), "one message, one line: {line}");
            assert_eq!(serde_json::from_str::<ToChild>(&line).unwrap(), message);
        }
        assert_eq!(serde_json::to_string(&ToChild::ResetPlace).unwrap(), r#""resetPlace""#);
        assert_eq!(
            serde_json::to_string(&ToChild::Theme(Some("mica".into()))).unwrap(),
            r#"{"theme":"mica"}"#
        );
    }

    #[test]
    fn what_the_child_says_is_heard_and_nothing_else_is() {
        assert_eq!(heard(r#""ready""#), Some(FromChild::Ready));
        assert_eq!(
            heard(r#"{"pressed":{"id":"mail:1","action":"reply","text":"Thanks"}}"#),
            Some(FromChild::Pressed {
                id: "mail:1".into(),
                action: "reply".into(),
                copied: false,
                text: Some("Thanks".into()),
            })
        );
        assert_eq!(
            heard(r#"{"hold":{"id":"notes:2","held":true}}"#),
            Some(FromChild::Hold { id: "notes:2".into(), held: true })
        );
        // What GLib and WebKit print there.
        assert_eq!(heard("(WebKitWebProcess:4242): GLib-GIO-WARNING **: no bus"), None);
        assert_eq!(heard(""), None);
        assert_eq!(heard(r#"{"ready":true}"#), None, "the GTK child's old greeting");
    }

    #[test]
    fn a_press_reads_back_as_it_was_written() {
        let said = FromChild::Pressed {
            id: "voice-typing".into(),
            action: "copy".into(),
            copied: true,
            text: None,
        };
        let line = serde_json::to_string(&said).unwrap();
        assert_eq!(heard(&line), Some(said));
    }

    #[test]
    fn only_the_latest_of_each_thing_is_sent() {
        let mut batch = Batch::default();
        batch.add(Wake::Level("talk".into(), 0.1));
        batch.add(Wake::Theme(Some("dusk".into())));
        batch.add(Wake::Level("rec".into(), 0.4));
        batch.add(Wake::Level("talk".into(), 0.3));
        batch.add(Wake::Theme(None));
        assert_eq!(
            batch,
            Batch {
                stack: false,
                levels: vec![("talk".into(), 0.3), ("rec".into(), 0.4)],
                theme: Some(None),
                reset: false,
            }
        );
        batch.add(Wake::Stack);
        batch.add(Wake::ResetPlace);
        batch.add(Wake::Stack);
        assert!(batch.stack && batch.reset);
    }

    #[test]
    fn a_child_that_dies_is_started_again_once_while_cards_are_up() {
        let mut restarted = false;
        assert!(may_start(false, &mut restarted), "the first of a run of cards");
        assert!(!restarted, "that is no restart");
        assert!(may_start(true, &mut restarted), "it died: once more");
        assert!(may_start(false, &mut restarted), "running again, a card more");
        assert!(!may_start(true, &mut restarted), "died again: given up on");
        // The stack empties, which clears it; the next run may restart once.
        restarted = false;
        assert!(may_start(true, &mut restarted));
    }

    #[test]
    fn nothing_but_the_child_is_the_child() {
        assert!(!is_child(), "a test, like the app, owns its cards");
    }
}
