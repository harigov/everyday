//! The tray icon and its quick-action menu.
//!
//! The shell owns the icon; the interface owns what is in the menu. That
//! split is the whole design. A quick action is "add a task", "start an
//! entry" -- things that are defined by an app inside the window, know
//! whether they are currently possible, and have to run where the state
//! lives. Hard-coding them here would mean a Rust change, a capability
//! change and a rebuild every time one of the apps in the window grew
//! another thing worth doing from the menu bar -- which is exactly what
//! adding the library did, and it cost this file nothing.
//!
//! So the interface sends a menu *description* ([`TrayItem`]) and the shell
//! renders it. Choosing an item emits [`TRAY_ACTION`] carrying the item's
//! id, and the interface runs the handler it registered under that id. See
//! `ui/src/lib/tray.svelte.ts` for the other half, which is where a new
//! action is actually added.
//!
//! Two items are *not* the interface's to supply: the shell appends "Open"
//! and "Quit" itself, below a separator. A tray whose only way back to the
//! application is a menu built by a webview that might be wedged is a way
//! to lose a running program, and on Linux -- where a click on the icon
//! raises no event at all -- it is the only way.
//!
//! The icon itself is built at most once per process and hidden rather than
//! destroyed. Both handlers below are registered on the `Builder` instead of
//! on the icon, for the same reason: see [`Tray::hide`].

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use everyday_service::error::{CommandError, CommandResult};

/// Emitted with the id of the menu item that was chosen.
pub const TRAY_ACTION: &str = "everyday://tray-action";

/// Namespace for the items the shell adds itself. Ids from the interface may
/// not use it, so a quick action can never shadow "Quit".
const RESERVED: &str = "everyday:";
const SHOW_ID: &str = "everyday:show";
const QUIT_ID: &str = "everyday:quit";
const TRAY_ID: &str = "everyday";

/// What the assistant is up to, in a line, or nothing to say.
///
/// Nothing to say is the ordinary case: a vault with no routines on it has no
/// assistant to report on, and a menu that said "Assistant: off" to everybody
/// would be an advertisement rather than a status.
///
/// Built here rather than sent by the interface for the reason the whole
/// feature exists: the window may be hidden, and its stores dropped, at
/// exactly the moment somebody opens this menu to ask what the process is
/// still doing.
fn assistant_line(app: &AppHandle) -> Option<String> {
    let state = app.try_state::<crate::state::AppState>()?;
    let service = state.service();
    if let Some(name) = service.running_routine() {
        return Some(format!("Assistant: running \u{201c}{name}\u{201d}"));
    }
    let vault = service.get()?;
    if !vault.is_unlocked() {
        // Worth saying, because it is the answer to "why did nothing happen
        // this morning".
        return Some("Assistant: waiting for the password".into());
    }
    if !vault.supports_routines() {
        return None;
    }
    let routines = vault.routines().ok()?;
    let live = routines.iter().filter(|r| r.enabled).count();
    match (routines.len(), live) {
        (0, _) => None,
        (_, 0) => Some("Assistant: every routine is switched off".into()),
        (_, 1) => Some("Assistant: idle, 1 routine".into()),
        (_, n) => Some(format!("Assistant: idle, {n} routines")),
    }
}

/// One entry in the tray menu, as the interface describes it.
///
/// Deliberately a small language rather than a general one: an item that
/// does something, a rule between groups, and a submenu for an app with
/// more than a couple of actions. No icons -- a tray menu is read as a list
/// of verbs -- and no accelerators, because a menu-bar accelerator is not
/// registered globally on any of the three platforms, so displaying one
/// would advertise a shortcut that does nothing unless the window already
/// has focus.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TrayItem {
    Action {
        id: String,
        label: String,
        enabled: bool,
        /// `Some` renders a checkbox -- for an action that is also a state,
        /// like a timer that is running.
        #[serde(default)]
        checked: Option<bool>,
        /// Bring the window forward before the handler runs. True for
        /// anything that puts a cursor somewhere; false for "lock now",
        /// which is precisely a thing you do without coming back.
        raise: bool,
    },
    Separator,
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<TrayItem>,
    },
}

/// The tray icon, if one is showing, and what to do with a click on it.
#[derive(Default)]
pub struct Tray {
    icon: Mutex<Option<TrayIcon>>,
    /// Item id to "raise the window first", for the menu currently set.
    raise: Mutex<HashMap<String, bool>>,
}

impl Tray {
    /// Show the tray icon with `items` above the shell's own two entries,
    /// creating the icon if this is the first call.
    ///
    /// Returns `false` when the platform has no tray to put it in -- a Linux
    /// desktop with no StatusNotifier host is the ordinary case -- so the
    /// interface can say so in Settings rather than leaving a switch on that
    /// does nothing.
    pub fn show(&self, app: &AppHandle, items: &[TrayItem]) -> CommandResult<bool> {
        for item in items {
            check_ids(item)?;
        }

        // Built before either lock is taken. Every menu call here hops to the
        // main thread and blocks until it answers, so holding a lock across
        // one would deadlock against the click handler below -- which runs on
        // that same main thread and wants `raise`.
        let mut raise = HashMap::new();
        let quick = build(app, items, &mut raise)?;
        let mut all: Vec<&dyn IsMenuItem<Wry>> = quick.iter().map(Box::as_ref).collect();

        let separator = PredefinedMenuItem::separator(app).map_err(menu_error)?;
        let show = MenuItem::with_id(app, SHOW_ID, "Open Every Day", true, NO_ACCEL)
            .map_err(menu_error)?;
        let quit = MenuItem::with_id(app, QUIT_ID, "Quit Every Day", true, NO_ACCEL)
            .map_err(menu_error)?;
        if !all.is_empty() {
            all.push(&separator);
        }
        all.push(&show);
        all.push(&quit);
        let menu = Menu::with_items(app, &all).map_err(menu_error)?;

        *self.raise.lock().unwrap() = raise;

        let mut slot = self.icon.lock().unwrap();
        if let Some(tray) = slot.as_ref() {
            tray.set_menu(Some(menu)).map_err(menu_error)?;
            // Puts it back if the setting was switched off and on again.
            tray.set_visible(true).map_err(menu_error)?;
            return Ok(true);
        }

        // No `on_menu_event` or `on_tray_icon_event` here: they are the
        // `Builder`'s, in `lib.rs`. Registering a menu handler on an icon
        // appends it to a process-wide list that nothing ever removes, so an
        // icon built a second time would leave every menu pick firing its
        // handler twice -- two journal entries from one click.
        let mut builder = TrayIconBuilder::with_id(TRAY_ID).menu(&menu).tooltip("Every Day");
        // The application mark, not a monochrome silhouette of it. A macOS
        // template icon would be the more idiomatic choice in the menu bar,
        // but the mark is a filled tile and a template renders only its
        // alpha -- which would put a solid black square up there. The mark is
        // already drawn to survive 16 pixels (see `icons/icon.svg`), and it
        // is the same thing the dock, the taskbar and the panel show.
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }

        match builder.build(app) {
            Ok(tray) => {
                *slot = Some(tray);
                Ok(true)
            }
            // Not an error to report: the application is fine, this desktop
            // simply has nowhere to put an icon.
            Err(e) => {
                tracing::info!(error = %e, "no system tray available");
                Ok(false)
            }
        }
    }

    /// Take the icon down, keeping it in hand for the next `show`.
    ///
    /// Hidden rather than dropped, and the difference is not stylistic.
    /// Dropping this handle does not remove anything: `TrayIconBuilder::build`
    /// files a second copy in Tauri's resource table, so the icon would stay
    /// on screen and the switch in Settings would appear to do nothing. The
    /// route that *does* remove it, `remove_tray_by_id`, leaves the id behind
    /// in Tauri's own index and so works exactly once -- and building a
    /// replacement afterwards is what duplicates the menu handler described
    /// above. Visibility is the operation this actually wants: one icon, one
    /// handler, for the life of the process.
    pub fn hide(&self) {
        if let Some(tray) = self.icon.lock().unwrap().as_ref() {
            // Nothing to do about a failure but carry on: the icon is a
            // convenience and the vault is not involved either way.
            if let Err(e) = tray.set_visible(false) {
                tracing::warn!(error = %e, "could not hide the tray icon");
            }
        }
        // A hidden menu cannot be clicked, so nothing needs raising.
        self.raise.lock().unwrap().clear();
    }

    fn raises(&self, id: &str) -> bool {
        self.raise.lock().unwrap().get(id).copied().unwrap_or(true)
    }
}

/// `MenuItem::with_id` is generic over the accelerator type even when there
/// is no accelerator, so `None` needs one to infer.
const NO_ACCEL: Option<&str> = None;

fn menu_error(e: tauri::Error) -> CommandError {
    CommandError::new("tray", format!("the tray menu could not be built: {e}"))
}

/// Reject ids in the shell's own namespace, at any depth.
fn check_ids(item: &TrayItem) -> CommandResult<()> {
    match item {
        TrayItem::Action { id, .. } if id.starts_with(RESERVED) => Err(CommandError::new(
            "tray",
            format!("tray item id {id:?} uses the reserved {RESERVED:?} prefix"),
        )),
        TrayItem::Submenu { items, .. } => items.iter().try_for_each(check_ids),
        _ => Ok(()),
    }
}

/// Turn the description into real menu items, recording which ids raise the
/// window as it goes.
fn build(
    app: &AppHandle,
    items: &[TrayItem],
    raise: &mut HashMap<String, bool>,
) -> CommandResult<Vec<Box<dyn IsMenuItem<Wry>>>> {
    let mut built: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::with_capacity(items.len());
    for item in items {
        let one: Box<dyn IsMenuItem<Wry>> = match item {
            TrayItem::Separator => {
                Box::new(PredefinedMenuItem::separator(app).map_err(menu_error)?)
            }
            TrayItem::Action { id, label, enabled, checked, raise: raises } => {
                raise.insert(id.clone(), *raises);
                match checked {
                    Some(on) => Box::new(
                        CheckMenuItem::with_id(app, id, label, *enabled, *on, NO_ACCEL)
                            .map_err(menu_error)?,
                    ),
                    None => Box::new(
                        MenuItem::with_id(app, id, label, *enabled, NO_ACCEL)
                            .map_err(menu_error)?,
                    ),
                }
            }
            TrayItem::Submenu { label, enabled, items } => {
                let children = build(app, items, raise)?;
                let refs: Vec<&dyn IsMenuItem<Wry>> = children.iter().map(Box::as_ref).collect();
                Box::new(Submenu::with_items(app, label, *enabled, &refs).map_err(menu_error)?)
            }
        };
        built.push(one);
    }
    Ok(built)
}

/// Raise the main window: undo a minimise, undo a hide, and take focus.
///
/// All three, in that order, because "the window is not in front of me" has
/// three different causes and only doing one of them fixes one of them. Also
/// what a second launch does -- see the single-instance plugin in `lib.rs`.
pub fn raise_window(app: &AppHandle) {
    let Some(window) = app.webview_windows().values().next().cloned() else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    match id {
        SHOW_ID => raise_window(app),
        QUIT_ID => {
            // A close *request*, not a teardown: it goes through the window's
            // own `CloseRequested` handler, so quitting from the tray saves
            // and locks exactly the way closing the window does. The three
            // second fallback there also covers an interface too wedged to
            // answer, which is the case where a tray quit matters most.
            if let Some(window) = app.webview_windows().values().next() {
                let _ = window.close();
            }
        }
        _ => {
            if app.state::<Tray>().raises(id) {
                raise_window(app);
            }
            // The window is still alive whether or not it is on screen, so
            // the interface receives this either way.
            let _ = app.emit(TRAY_ACTION, id);
        }
    }
}

pub fn on_tray_icon_event(app: &AppHandle, event: TrayIconEvent) {
    // Double click, and only on the left button: the Windows convention for
    // "open the application", where a single left click is already spoken
    // for by the menu. macOS shows the menu on any click and Linux reports
    // no click at all, so on those two this is dead code and the menu's
    // "Open Every Day" is the way back -- which is why the shell appends it
    // rather than trusting the interface to.
    if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
        raise_window(app);
    }
}
