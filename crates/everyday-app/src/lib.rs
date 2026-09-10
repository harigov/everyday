//! The Every Day desktop shell.
//!
//! This crate is deliberately thin, and is thinner than it was. It owns the
//! window, the tray, the media protocol and eleven commands; everything a vault
//! can be asked to do lives in [`everyday_service`], which knows nothing about
//! Tauri. That split is what lets the same commands back this window, a server
//! answering other machines, and a mobile shell later -- and it is why the entry
//! point is a library: Tauri builds iOS and Android targets from `run()` rather
//! than from `main`.

mod commands;
mod events;
mod hotkey;
mod protocol;
mod remote;
mod remotes;
mod sharing;
mod state;
mod tray;

use state::AppState;
use tauri::{Emitter, Manager, WindowEvent};
use tray::Tray;

/// Asks the interface to write pending edits and then close the window.
/// Its other half is `commands::ready_to_close`.
pub const SAVE_AND_CLOSE: &str = "everyday://save-and-close";

/// How long the window waits for the interface to finish saving before it
/// closes anyway. Generous for a handful of local writes, and short enough
/// that a wedged interface does not read as an app that will not quit.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Entry point shared by the desktop binary and the mobile harnesses.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("EVERYDAY_LOG")
                .unwrap_or_else(|_| "everyday_app=info,everyday_core=info".into()),
        )
        .with_target(false)
        .init();

    tauri::Builder::default()
        // Must be the first plugin registered: a second launch has to be
        // turned away before it can build a window or open a vault.
        //
        // Without it, launching the app twice gave two windows on one vault,
        // each with its own in-memory copy of whatever was open, and the
        // second to autosave silently overwrote the first. The vault's own
        // write lock now catches that too -- the second process would open
        // read-only -- but a read-only second window is a confusing thing to
        // be handed when what you wanted was the window you already had. So
        // the second launch raises the first and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::raise_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // Registered for the *webview's* sake. The shell never posts through
        // it -- see `events.rs` for why the routing decision lives on one
        // side of the bridge only.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState::new())
        // Empty until the interface asks for a tray. Nothing is put in the
        // menu bar on a machine where the setting is off, and nothing is put
        // there before the interface knows what belongs in it -- a tray that
        // appears at launch holding only "Quit" is worse than one that
        // appears a moment later holding the actions.
        .manage(Tray::default())
        .setup(|app| {
            let handle = app.handle().clone();
            // The service exists before Tauri does -- `AppState` builds it --
            // but it cannot emit anywhere until there is an app to emit
            // through. This is the earliest moment that handle exists.
            app.state::<AppState>()
                .set_sink(std::sync::Arc::new(events::WindowSink::new(handle.clone())));
            // The OS-wide key to the palette. A desktop that refuses it is the
            // ordinary case on Wayland, so this is not a failure to report:
            // the settings pane says whether it was granted, and the tray is
            // the way in regardless. See `hotkey`.
            hotkey::install(&handle);
            // The assistant's routines. On Tauri's own runtime, never a second
            // one: two runtimes would double the blocking pool and, worse,
            // would mean the vault's single-writer rule was being kept by two
            // sets of threads that know nothing about each other.
            //
            // Spawned once for the life of the process rather than per vault.
            // It does nothing at all while there is no vault or the vault is
            // locked, which is most of the time and is the point: a window
            // that has never been opened must not be what decides whether the
            // seven o'clock brief happens.
            let service = app.state::<AppState>().service();
            let (_stop, listen) = tokio::sync::watch::channel(false);
            // Held for the life of the process. The scheduler stops when the
            // process does, and nothing else should be able to stop it.
            std::mem::forget(_stop);
            tauri::async_runtime::spawn(everyday_service::scheduler::run(service, listen));
            Ok(())
        })
        // Registered here rather than on the icon, and once rather than per
        // icon: Tauri appends a menu handler given to `TrayIconBuilder` to a
        // process-wide list it never prunes, and dispatches every menu event
        // to all of them. See `tray::Tray::hide`.
        .on_menu_event(tray::on_menu_event)
        .on_tray_icon_event(tray::on_tray_icon_event)
        .register_asynchronous_uri_scheme_protocol("everyday", |ctx, request, responder| {
            protocol::handle(ctx.app_handle(), request, responder);
        })
        .invoke_handler(tauri::generate_handler![
            // Deciding what this session is about. Not commands, because a
            // service is handed a vault rather than going to look for one.
            commands::bootstrap,
            commands::create_vault,
            commands::open_vault,
            // Everything else a vault can be asked to do.
            commands::call,
            // Looking at a vault on another computer.
            commands::list_remotes,
            commands::connect_remote,
            commands::reconnect_remote,
            commands::disconnect_remote,
            commands::forget_remote,
            // Serving this vault to other machines.
            commands::share_status,
            commands::share_start,
            commands::share_stop,
            commands::new_pairing_code,
            commands::cancel_pairing,
            commands::revoke_device,
            commands::set_remote_unlock,
            // The OS-wide key to the palette.
            commands::hotkey_status,
            commands::set_hotkey,
            // The three that are not JSON in and JSON out.
            commands::put_blob,
            commands::send_message,
            // This process's own furniture.
            commands::ready_to_close,
            commands::set_tray_menu,
            commands::hide_tray,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing the window is not always quitting any more.
                //
                // A vault with a routine on it, or one being served to another
                // machine, has work to do with no window in front of it: the
                // seven o'clock brief has to happen whether or not anybody
                // opened anything. So in that case the window hides and the
                // process stays, reachable from the tray. Quit -- from the
                // tray, or the platform's own quit -- still quits, and still
                // locks on the way out.
                //
                // Hiding rather than destroying also keeps the webview, which
                // is what lets a notification raised at seven reach the
                // notification centre: the routing that decides banner or
                // toast lives in the interface, and a destroyed webview cannot
                // run it. See `events.rs`.
                if window.try_state::<AppState>().is_some_and(|s| s.stays_resident()) {
                    api.prevent_close();
                    let _ = window.hide();
                    return;
                }

                // Hold the window open for one round trip.
                //
                // The interface autosaves on a timer, so at the moment a
                // close arrives there is routinely a few hundred milliseconds
                // of typing that has not been written yet. Firing a flush and
                // letting the close proceed does not save it: the flush is an
                // async call across the IPC boundary, and it loses the race
                // against teardown essentially always. So the close is
                // cancelled, the interface is asked to finish its writes, and
                // it closes the window itself when they have landed.
                if let Some(state) = window.try_state::<AppState>()
                    && state.begin_closing()
                {
                    api.prevent_close();
                    let _ = window.emit(SAVE_AND_CLOSE, ());

                    // ...but never at the cost of a window that will not
                    // shut. A wedged or crashed interface never answers, and
                    // quitting must not depend on it, so the close is made
                    // unconditional shortly after. The delay is what a save
                    // takes, generously: the writes are local.
                    let window = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(CLOSE_GRACE);
                        if let Some(state) = window.try_state::<AppState>()
                            && let Some(vault) = state.service().get()
                        {
                            vault.lock();
                        }
                        let _ = window.destroy();
                    });
                    return;
                }

                // Lock on the way out so the process never lingers with a
                // decrypted key in memory, and so the search index -- which
                // holds plaintext -- is dropped.
                if let Some(state) = window.try_state::<AppState>()
                    && let Some(vault) = state.service().get()
                {
                    vault.lock();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start Every Day");
}
