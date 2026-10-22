//! The keys that reach Every Day from anywhere on the desktop.
//!
//! Two of them. One raises the window with the palette open: the difference
//! between "the app has a palette" and "there is somewhere to put a
//! thought", because the second one has to work while somebody is looking at
//! a different application entirely. The other is writing help over text
//! selected in whatever application that is -- see [`crate::writing`].
//!
//! # Why the palette raises the window rather than drawing its own
//!
//! A palette in a window of its own -- appearing over whatever is in front,
//! never taking focus from it -- is the better end state and is deliberately
//! not what this does yet. Its own window would need its own copy of the
//! action table and its own answer to what "the open shelf" means. That is a
//! feature, not a detail, and it is worth building once the in-window palette
//! has settled what belongs in it. (Writing help does have a window of its
//! own, because it has nothing to share with the main one: a selection in,
//! a rewrite out.)
//!
//! # Two ways of claiming a key
//!
//! Everywhere but Wayland, `tauri-plugin-global-shortcut` claims the
//! combination directly: a hotkey on Windows and macOS, a passive grab on
//! X11.
//!
//! Wayland has no protocol for an application to claim a global key at all.
//! The plugin's X11 grab does "succeed" there -- on Xwayland -- and then only
//! fires while an X11 window happens to have the keyboard, which is to say
//! almost never. So on Wayland the keys go through the desktop's
//! `GlobalShortcuts` portal instead ([`crate::portal::Shortcuts`]), which
//! asks the person once, in the desktop's own dialog, and then tells this
//! process whenever they are pressed. The desktop keeps the binding from
//! then on: a different combination is chosen in its settings, not here,
//! and what this module reports as the key is what the desktop says it is.
//!
//! A desktop without the portal leaves the keys unclaimed. That is reported
//! to the settings pane as a fact about this desktop, and the tray remains
//! the way in. Nothing else depends on a key being claimed.

use std::sync::Mutex;

use everyday_service::error::{CommandError, CommandResult};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutEvent, ShortcutState,
};

/// The event the interface listens for. Its other half is `onPalette` in
/// `ui/src/lib/api.ts`.
pub const PALETTE_EVENT: &str = "everyday://palette";

/// A key this module claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Binding {
    Palette,
    Writing,
}

impl Binding {
    /// The id a portal files it under, and the name it has in a log line.
    fn id(self) -> &'static str {
        match self {
            Binding::Palette => "palette",
            Binding::Writing => "writing",
        }
    }

    /// What the desktop's shortcut dialog says it does.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn description(self) -> &'static str {
        match self {
            Binding::Palette => "Open Every Day's command palette",
            Binding::Writing => "Improve, correct or shorten the selected text",
        }
    }
}

/// What is claimed unless somebody chooses otherwise.
///
/// `Ctrl`/`Cmd` and `Shift` and space. Space because the gesture it borrows
/// from is Spotlight's, and the two modifiers because the unshifted version is
/// taken by an input-method switcher on most desktops -- claiming that one
/// would break typing in another language, which is a worse outcome than
/// having no hotkey.
fn palette_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
}

/// Writing help's key unless somebody chooses another: `Ctrl+Alt+Space`.
///
/// Beside the palette's, for the same reason that one has two modifiers. Not
/// `Cmd+Option` anything on macOS, where that is "close every window" in half
/// the applications somebody would select text in.
pub const DEFAULT_WRITING: &str = "Ctrl+Alt+Space";

/// Parse a writing-help accelerator -- `Ctrl+Alt+Space` -- or say why not.
///
/// At least one of Ctrl, Alt or Super, because a letter claimed for the
/// whole desktop -- bare or shifted -- is a letter that can no longer be
/// typed.
pub fn parse(accelerator: &str) -> Result<Shortcut, String> {
    let shortcut: Shortcut = accelerator
        .trim()
        .parse()
        .map_err(|e| format!("{accelerator:?} is not a key combination: {e}"))?;
    // Shift alone does not count: Shift+A is how a capital A is typed.
    if !shortcut.mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER) {
        return Err("a shortcut for the whole desktop needs Ctrl, Alt or Super".into());
    }
    if shortcut == palette_shortcut() {
        return Err("that is already the palette's shortcut".into());
    }
    Ok(shortcut)
}

/// How a shortcut should be written on screen: `Ctrl+Alt+Space`, or the
/// macOS glyphs.
pub fn describe(shortcut: &Shortcut) -> String {
    let mac = cfg!(target_os = "macos");
    let mut parts: Vec<String> = Vec::new();
    let m = shortcut.mods;
    if m.contains(Modifiers::CONTROL) {
        parts.push(if mac { "\u{2303}" } else { "Ctrl" }.into());
    }
    if m.contains(Modifiers::ALT) {
        parts.push(if mac { "\u{2325}" } else { "Alt" }.into());
    }
    if m.contains(Modifiers::SHIFT) {
        parts.push(if mac { "\u{21e7}" } else { "Shift" }.into());
    }
    if m.contains(Modifiers::SUPER) {
        parts.push(if mac { "\u{2318}" } else { "Super" }.into());
    }
    let key = shortcut.key.to_string();
    let key = key.strip_prefix("Key").or_else(|| key.strip_prefix("Digit")).unwrap_or(&key);
    parts.push(key.to_string());
    parts.join(if mac { "" } else { "+" })
}

/// How the palette's key should be written on screen. On macOS the palette
/// is `Cmd` rather than `Ctrl` -- the plugin maps `CONTROL` to it there.
fn describe_palette() -> String {
    if cfg!(target_os = "macos") {
        "\u{2318} \u{21e7} Space".into()
    } else {
        "Ctrl Shift Space".into()
    }
}

// ---- the state --------------------------------------------------------------

/// Which keys are wanted, and how they are being claimed.
#[derive(Default)]
pub struct Hotkeys {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    palette: bool,
    writing: Option<Shortcut>,
    /// Set once `install` has found a GlobalShortcuts portal; every claim
    /// goes through it from then on.
    #[cfg(target_os = "linux")]
    portal: Option<crate::portal::Shortcuts>,
    /// Whether the portal has been asked for the keys yet. It is asked once
    /// per process, for both -- see `apply`.
    #[cfg(target_os = "linux")]
    portal_asked: bool,
}

/// What the settings pane draws for one key.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    /// Somebody wants it claimed.
    pub on: bool,
    /// The desktop says it is.
    pub registered: bool,
    /// The combination, as it should be written on screen -- the desktop's
    /// own answer where it keeps the binding.
    pub shortcut: String,
    /// `true` where the desktop keeps the binding and a different key is
    /// chosen in its settings rather than here.
    pub desktop_managed: bool,
    /// Why it is not claimed, when somebody wants it and it is not.
    pub problem: Option<String>,
}

impl Inner {
    /// Whether somebody wants `binding` to do something.
    fn wanted(&self, binding: Binding) -> bool {
        match binding {
            Binding::Palette => self.palette,
            Binding::Writing => self.writing.is_some(),
        }
    }

    /// The combination `binding` is claimed with through the plugin.
    fn shortcut(&self, binding: Binding) -> Option<Shortcut> {
        match binding {
            Binding::Palette => Some(palette_shortcut()),
            Binding::Writing => self.writing,
        }
    }

    /// How `binding`'s combination is written on screen, where this process
    /// rather than the desktop decides it.
    fn described(&self, binding: Binding) -> String {
        match binding {
            Binding::Palette => describe_palette(),
            Binding::Writing => self.writing.as_ref().map(describe).unwrap_or_default(),
        }
    }

    /// Whether the portal still has to be asked for the keys: never asked
    /// yet, or asked and refused -- the dialog cancelled, say -- in which
    /// case the next switch in Settings is the person asking again.
    #[cfg(target_os = "linux")]
    fn portal_needs_asking(&self) -> bool {
        let refused = self.portal.as_ref().is_some_and(|p| matches!(p.bound(), Some(Err(_))));
        !self.portal_asked || refused
    }
}

/// What `apply` decided to do through the plugin, done once the lock is
/// released: registering and unregistering wait on the main thread, which
/// may itself be waiting on the lock.
enum PluginStep {
    Nothing,
    Claim(Shortcut, Binding),
    Release(Shortcut),
}

impl Hotkeys {
    #[cfg(target_os = "linux")]
    fn portal_status(&self, binding: Binding, inner: &Inner) -> Option<KeyStatus> {
        let portal = inner.portal.as_ref()?;
        let on = inner.wanted(binding);
        let fallback = inner.described(binding);
        let (registered, shortcut, problem) = match portal.bound() {
            None if on => {
                (false, fallback, Some("waiting for the desktop to confirm the key".into()))
            }
            None => (false, fallback, None),
            Some(Err(e)) => (false, fallback, on.then_some(e)),
            Some(Ok(bound)) => match bound.iter().find(|(id, _)| id == binding.id()) {
                // Registered only while it is also switched on: a key that
                // is switched off stays bound -- see `apply` -- but does
                // nothing, and nothing should count on it as a way back.
                Some((_, trigger)) if trigger.trim().is_empty() => (
                    false,
                    "no key".into(),
                    on.then(|| "the desktop has this shortcut with no key assigned".into()),
                ),
                Some((_, trigger)) => (on, crate::portal::readable_trigger(trigger), None),
                None => (false, fallback, on.then(|| "the desktop did not bind it".into())),
            },
        };
        Some(KeyStatus { on, registered, shortcut, desktop_managed: true, problem })
    }

    pub fn status(&self, app: &AppHandle, binding: Binding) -> KeyStatus {
        let (on, shortcut, shortcut_text) = {
            let inner = self.inner.lock().expect("hotkeys lock");
            #[cfg(target_os = "linux")]
            if let Some(status) = self.portal_status(binding, &inner) {
                return status;
            }
            (inner.wanted(binding), inner.shortcut(binding), inner.described(binding))
        };
        // Asked of the plugin with the lock released, as everything the
        // plugin is asked is -- see `PluginStep`.
        let registered = on && shortcut.is_some_and(|s| app.global_shortcut().is_registered(s));
        KeyStatus {
            on,
            registered,
            shortcut: shortcut_text,
            desktop_managed: false,
            problem: (on && !registered)
                .then(|| "this desktop did not grant that key; the tray is the way in".into()),
        }
    }

    /// Wait -- briefly -- for the desktop to answer whatever the next
    /// `set_*` call asks of it. Call before that call, await after it.
    ///
    /// Only the portal answers later than the call itself; the plugin has
    /// answered by the time `set_*` returns. A rebinding the person has
    /// already agreed to is answered at once, so the wait is short except
    /// the first time, when the desktop's dialog is waiting on them -- and
    /// that is not worth holding a settings pane up for.
    pub fn answer(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        #[cfg(target_os = "linux")]
        let waiter = {
            let inner = self.inner.lock().expect("hotkeys lock");
            // Asked already: a switch now only changes what a press does.
            if inner.portal_needs_asking() {
                inner.portal.as_ref().map(crate::portal::Shortcuts::next_answer)
            } else {
                None
            }
        };
        async move {
            #[cfg(target_os = "linux")]
            if let Some(mut waiter) = waiter {
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(2), waiter.changed()).await;
            }
        }
    }

    pub fn is_registered(&self, app: &AppHandle, binding: Binding) -> bool {
        self.status(app, binding).registered
    }

    /// Claim the palette's key, or give it back.
    pub fn set_palette(&self, app: &AppHandle, on: bool) -> CommandResult<()> {
        let step = {
            let mut inner = self.inner.lock().expect("hotkeys lock");
            inner.palette = on;
            Self::plan(&mut inner, Binding::Palette)
        };
        Self::run(app, step)
    }

    /// Claim writing help's key -- `None` gives it back.
    pub fn set_writing(&self, app: &AppHandle, shortcut: Option<Shortcut>) -> CommandResult<()> {
        let (old, step) = {
            let mut inner = self.inner.lock().expect("hotkeys lock");
            let old = inner.writing;
            inner.writing = shortcut;
            #[cfg(target_os = "linux")]
            let through_plugin = inner.portal.is_none();
            #[cfg(not(target_os = "linux"))]
            let through_plugin = true;
            // The plugin claims by combination, so a changed one is the old
            // given back before the new is claimed.
            let old = old.filter(|old| through_plugin && Some(*old) != shortcut);
            (old, Self::plan(&mut inner, Binding::Writing))
        };
        if let Some(old) = old {
            Self::run(app, PluginStep::Release(old))?;
        }
        Self::run(app, step)
    }

    /// Decide what claiming `binding` as `inner` now wants takes. Through
    /// the portal that is done here, and quickly -- sending a request, not
    /// waiting on it; through the plugin it is handed back to be done once
    /// the lock is released.
    fn plan(inner: &mut Inner, binding: Binding) -> PluginStep {
        // Through the portal, both keys are asked for once, whatever is
        // switched on, and switching one off only stops it doing anything.
        //
        // Because the desktop remembers the last set of keys it was asked
        // for and nothing else: binding the palette's alone forgets writing
        // help's -- and whatever key somebody chose for it in the desktop's
        // settings -- and asking for it again brings the desktop's dialog
        // back. A switch in this application should not do either. (Checked
        // against GNOME 50.) Asked again only if the last asking was
        // refused.
        #[cfg(target_os = "linux")]
        if inner.portal.is_some() {
            if inner.portal_needs_asking()
                && let Some(portal) = inner.portal.as_ref()
            {
                let preferred = crate::writing::load_config().shortcut;
                portal.want(vec![
                    crate::portal::Wanted {
                        id: Binding::Palette.id(),
                        description: Binding::Palette.description(),
                        preferred: "CTRL+SHIFT+space".into(),
                    },
                    crate::portal::Wanted {
                        id: Binding::Writing.id(),
                        description: Binding::Writing.description(),
                        preferred: crate::portal::xdg_trigger(&preferred),
                    },
                ]);
                inner.portal_asked = true;
            }
            return PluginStep::Nothing;
        }
        match inner.shortcut(binding) {
            None => PluginStep::Nothing,
            Some(shortcut) if inner.wanted(binding) => PluginStep::Claim(shortcut, binding),
            Some(shortcut) => PluginStep::Release(shortcut),
        }
    }

    fn run(app: &AppHandle, step: PluginStep) -> CommandResult<()> {
        let plugin = app.global_shortcut();
        match step {
            PluginStep::Nothing => Ok(()),
            PluginStep::Release(shortcut) => {
                if plugin.is_registered(shortcut) {
                    plugin
                        .unregister(shortcut)
                        .map_err(|e| CommandError::new("hotkey", e.to_string()))?;
                }
                Ok(())
            }
            PluginStep::Claim(shortcut, binding) => {
                if plugin.is_registered(shortcut) {
                    return Ok(());
                }
                let handle = app.clone();
                let outcome =
                    plugin.on_shortcut(shortcut, move |_app, _shortcut, event: ShortcutEvent| {
                        // Press, not release. Both arrive, and acting on each would
                        // open the palette and immediately close it again.
                        if event.state() != ShortcutState::Pressed {
                            return;
                        }
                        activate(&handle, binding, None);
                    });
                if let Err(e) = outcome {
                    tracing::info!(
                        key = binding.id(),
                        error = %e,
                        "this desktop did not grant the global shortcut; the tray is the way in"
                    );
                }
                Ok(())
            }
        }
    }
}

/// A key was pressed. `token` is the activation token a portal sent with
/// it: proof the person asked, which is what lets a window take focus on
/// Wayland.
fn activate(app: &AppHandle, binding: Binding, token: Option<String>) {
    // Through the portal a key switched off is still bound -- see `apply` --
    // and its presses are dropped here.
    if !app.state::<Hotkeys>().inner.lock().expect("hotkeys lock").wanted(binding) {
        return;
    }
    match binding {
        Binding::Palette => {
            raise_main(app, token);
            if let Err(e) = app.emit(PALETTE_EVENT, ()) {
                tracing::debug!(error = %e, "the palette could not be raised");
            }
        }
        Binding::Writing => crate::writing::trigger(app, token),
    }
}

/// Raise the main window, passing a portal's activation token on so the
/// desktop lets it take focus.
fn raise_main(app: &AppHandle, token: Option<String>) {
    #[cfg(target_os = "linux")]
    if let Some(token) = token
        && let Some(window) = app.get_webview_window("main")
    {
        let _ = app.run_on_main_thread(move || {
            use gtk::prelude::GtkWindowExt;
            if let Ok(gtk_window) = window.gtk_window() {
                gtk_window.set_startup_id(&token);
            }
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        });
        return;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = token;
    crate::tray::raise_window(app);
}

/// Decide how keys are claimed on this desktop, and claim the ones wanted at
/// startup: the palette's, and writing help's if it is switched on.
///
/// The portal is looked for only on Wayland. It answers asynchronously -- the
/// first time, only once the person has answered its dialog -- so this
/// returns before anything is claimed; the settings pane reads the outcome
/// when it is drawn.
pub fn install(app: &AppHandle) {
    let writing = crate::writing::configured_shortcut();
    #[cfg(target_os = "linux")]
    if crate::writing::wayland_session(app) {
        let handle = app.clone();
        // Read here, on the main thread, where `setup` runs.
        let parent = app.get_webview_window("main").and_then(|w| crate::portal::parent_of(&w));
        tauri::async_runtime::spawn(async move {
            // The export's future holds the raw surface across its awaits,
            // so it is run to completion on a thread of its own.
            let parent = match parent {
                Some(p) => tauri::async_runtime::spawn_blocking(move || {
                    tauri::async_runtime::block_on(p.export())
                })
                .await
                .ok()
                .flatten(),
                None => None,
            };
            let on_activate = {
                let handle = handle.clone();
                move |id: &str, token: Option<String>| {
                    let binding = match id {
                        "palette" => Binding::Palette,
                        "writing" => Binding::Writing,
                        _ => return,
                    };
                    activate(&handle, binding, token);
                }
            };
            let portal = crate::portal::Shortcuts::connect(parent, on_activate).await;
            let hotkeys = handle.state::<Hotkeys>();
            hotkeys.inner.lock().expect("hotkeys lock").portal = portal;
            let _ = hotkeys.set_palette(&handle, true);
            // Through the portal the line above asked for both, and this one
            // only records whether writing help is on. Without a portal -- a
            // compositor with no GlobalShortcuts -- each key is the plugin's
            // to try, as on X11, however little that grab is worth here.
            let _ = hotkeys.set_writing(&handle, writing);
        });
        return;
    }
    let hotkeys = app.state::<Hotkeys>();
    let _ = hotkeys.set_palette(app, true);
    let _ = hotkeys.set_writing(app, writing);
}

/// Whether the palette's key is claimed right now -- whether a hidden window
/// has a way back.
pub fn is_registered(app: &AppHandle) -> bool {
    app.state::<Hotkeys>().is_registered(app, Binding::Palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writing_shortcut_needs_a_modifier_and_must_not_be_the_palettes() {
        assert!(parse("Ctrl+Alt+Space").is_ok());
        assert!(parse("Super+W").is_ok());
        assert!(parse("W").is_err());
        assert!(parse("Shift+W").is_err(), "Shift alone is how a capital is typed");
        assert!(parse("Ctrl+Shift+W").is_ok());
        assert!(parse("Ctrl+Shift+Space").is_err());
        assert!(parse("Ctrl+Nonsense").is_err());
    }

    #[test]
    fn shortcuts_are_written_the_way_people_write_them() {
        if cfg!(target_os = "macos") {
            return;
        }
        assert_eq!(describe(&parse("Ctrl+Alt+Space").unwrap()), "Ctrl+Alt+Space");
        assert_eq!(describe(&parse("alt+shift+w").unwrap()), "Alt+Shift+W");
        assert_eq!(describe(&parse("Super+5").unwrap()), "Super+5");
    }

    #[test]
    fn the_default_writing_shortcut_parses() {
        assert!(parse(DEFAULT_WRITING).is_ok());
    }
}
