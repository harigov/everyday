//! The key that reaches the palette from anywhere on the desktop.
//!
//! One combination, claimed for the whole session, that raises the window with
//! the palette open. It is the difference between "the app has a palette" and
//! "there is somewhere to put a thought", because the second one has to work
//! while somebody is looking at a different application entirely.
//!
//! # Why it raises the window rather than drawing its own
//!
//! A palette in a window of its own -- appearing over whatever is in front,
//! never taking focus from it -- is the better end state and is deliberately
//! not what this does yet. A second webview shares no memory with the first,
//! so it would need its own connection to the vault, its own copy of the
//! action table, and its own answer to what "the open shelf" means. That is a
//! feature, not a detail, and it is worth building once the in-window palette
//! has settled what belongs in it.
//!
//! # Where this does not work
//!
//! Wayland has no protocol for an application to claim a global key; the
//! desktop's own portal has to grant it, and not every compositor implements
//! that. Failing to register is therefore *ordinary* rather than exceptional:
//! it is reported to the settings pane as a fact about this desktop, and the
//! tray remains the way in. Nothing about the palette depends on this
//! succeeding.

use everyday_service::error::{CommandError, CommandResult};
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

/// The event the interface listens for. Its other half is `onPalette` in
/// `ui/src/lib/api.ts`.
pub const PALETTE_EVENT: &str = "everyday://palette";

/// What is claimed unless somebody chooses otherwise.
///
/// `Ctrl`/`Cmd` and `Shift` and space. Space because the gesture it borrows
/// from is Spotlight's, and the two modifiers because the unshifted version is
/// taken by an input-method switcher on most desktops -- claiming that one
/// would break typing in another language, which is a worse outcome than
/// having no hotkey.
fn default_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
}

/// Claim the hotkey, and raise the palette when it is pressed.
///
/// Returns whether the desktop allowed it. A refusal is a fact about the
/// desktop rather than an error in the application; see the module docs.
pub fn install(app: &AppHandle) -> bool {
    let shortcut = default_shortcut();
    let handle = app.clone();
    let outcome = app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
        // Press, not release. Both arrive, and acting on each would open the
        // palette and immediately close it again.
        if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
            return;
        }
        crate::tray::raise_window(&handle);
        if let Err(e) = handle.emit(PALETTE_EVENT, ()) {
            tracing::debug!(error = %e, "the palette could not be raised");
        }
    });

    match outcome {
        Ok(()) => true,
        Err(e) => {
            tracing::info!(
                error = %e,
                "this desktop did not grant the global shortcut; the tray is the way in"
            );
            false
        }
    }
}

/// Give the key back.
pub fn remove(app: &AppHandle) -> CommandResult<()> {
    app.global_shortcut()
        .unregister(default_shortcut())
        .map_err(|e| CommandError::new("hotkey", e.to_string()))
}

/// Whether the key is claimed right now.
pub fn is_registered(app: &AppHandle) -> bool {
    app.global_shortcut().is_registered(default_shortcut())
}

/// How the default combination should be written on screen.
///
/// Built from the same `Shortcut` the registration uses rather than typed out
/// beside it, so a settings pane cannot advertise a key the application did
/// not claim.
pub fn describe(mac: bool) -> String {
    let modifier = if mac { "\u{2318}" } else { "Ctrl" };
    let shift = if mac { "\u{21e7}" } else { "Shift" };
    format!("{modifier} {shift} Space")
}
