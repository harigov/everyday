//! The Every Day desktop shell.
//!
//! This crate is deliberately thin, and is thinner than it was. It owns the
//! window, the tray, the media protocol and eleven commands; everything a vault
//! can be asked to do lives in [`everyday_service`], which knows nothing about
//! Tauri. That split is what lets the same commands back this window, a server
//! answering other machines, and a mobile shell later -- and it is why the entry
//! point is a library: Tauri builds iOS and Android targets from `run()` rather
//! than from `main`.

mod badge;
pub mod capture;
mod commands;
mod events;
mod fanout;
mod hotkey;
mod listener;
mod mcp;
// `pub`: `meeting::begin_headless` is the window-free core of
// `meeting_start`/`start_automatic` -- see its own doc -- and an E2E test
// (`tests/meeting_e2e.rs`) drives a recording through it with no Tauri
// window at all. Everything else in this module stays private.
pub mod meeting;
mod micwatch;
mod navigation;
mod notice;
mod offers;
#[cfg(target_os = "linux")]
mod portal;
mod protocol;
mod remote;
mod remotes;
mod screentime;
mod sharing;
mod state;
mod transfer;
mod tray;
mod web;
mod windows;
mod writing;

// Re-exported, not the whole (otherwise private) `state` module: the one
// piece of it `meeting::begin_headless`'s public signature needs a test to
// be able to name. See that function's own doc.
pub use state::SessionHandle;

use badge::Badge;
use state::AppState;
use tauri::{Emitter, Manager, WindowEvent};
use tray::Tray;

/// Asks the interface to write pending edits and then close the window.
/// Its other half is `commands::ready_to_close`.
pub const SAVE_AND_CLOSE: &str = "everyday://save-and-close";

/// How long the window waits for the interface to finish saving before it
/// closes anyway. Generous for a handful of local writes, and short enough
/// that a wedged interface does not read as an app that will not quit.
pub(crate) const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

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
        // Counts nothing until the interface says what to count -- off by
        // default, see `badge.rs`.
        .manage(Badge::default())
        // Which global keys are wanted and how they are claimed -- see
        // `hotkey` -- and writing help's popup, which one of them opens.
        .manage(hotkey::Hotkeys::default())
        .manage(writing::Writing::default())
        // Apps opened in windows of their own, while they close -- see
        // `windows`.
        .manage(windows::AppWindows::default())
        // Offers to take notes, from the calendar or the microphone -- see
        // `offers` and `micwatch`.
        .manage(offers::Offers::default())
        .manage(micwatch::MicWatch::default())
        // This machine's screen time -- see `screentime`.
        .manage(screentime::ScreenTime::default())
        .setup(|app| {
            // The window, from its entry in `tauri.conf.json` -- marked
            // `create: false` so that it is built here, where it can be told
            // to go without the system's title bar everywhere but macOS.
            //
            // That bar was a strip of nothing above the interface's own top
            // bar, which already holds the search field and is where the
            // window is dragged from, so the interface draws the minimise,
            // maximise and close buttons at its right end instead -- see
            // `ui/src/lib/window.svelte.ts`. macOS keeps its decorations:
            // there the overlay title bar puts the traffic lights over the
            // top bar's left end, with no strip of its own. Resizing an
            // undecorated window by its edges is Tauri's own on Linux and
            // Windows. Built rather than switched off after the fact,
            // because a window shown with a bar and then stripped of it
            // flashes the bar at every launch.
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .cloned()
                .ok_or("tauri.conf.json has no main window")?;
            tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
                .decorations(cfg!(target_os = "macos"))
                // Never anywhere but the interface itself -- see `navigation`.
                .on_navigation(navigation::guard(navigation::dev_url(app.handle())))
                .on_new_window(navigation::new_window(app.handle().clone()))
                .build()?;

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
            writing::detect(&handle);
            hotkey::install(&handle);
            // Another application taking the microphone is the second way a
            // call is noticed, beside the calendar. See `micwatch`.
            micwatch::spawn(&handle);
            // Which app has focus, sent as screen time, and the door the
            // browser and GNOME extensions send theirs through. See
            // `screentime`.
            screentime::spawn(&handle);
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
            tauri::async_runtime::spawn(everyday_service::scheduler::run(
                service.clone(),
                listen.clone(),
            ));
            // Background jobs, beside the scheduler rather than inside it --
            // see `everyday_service::jobs` -- on the same stop signal.
            tauri::async_runtime::spawn(everyday_service::jobs::run(service.clone(), listen));
            // The mail phase's per-account sync tasks, tied to this vault's
            // lock through `Service::locked` and `Service::unlocked` -- see
            // `everyday_service::supervisor`'s module doc. Constructed here,
            // after `set_sink` above, so its announcements reach the window
            // from its very first task; nothing has registered one yet.
            service.set_supervisor(std::sync::Arc::new(
                everyday_service::supervisor::Supervisor::new(service.events()),
            ));
            // Chat links -- the assistant as a bot on the person's own Matrix
            // server -- run as supervised tasks too, so they are installed
            // after the supervisor they will be registered on. Nothing starts
            // until a vault is unlocked; see `everyday_service::chat`.
            service.set_chat_runner(std::sync::Arc::new(everyday_chat::Runner));
            // Keeps the tray's optional "current/next meeting" line current
            // between whatever else would rebuild it -- a calendar sync
            // (`events::WindowSink::changed`), a lock
            // (`events::WindowSink::lock_state`) -- since nothing else
            // notices a minute of plain clock time passing. `Tray::refresh`
            // is the no-op this costs on every other tick: it does nothing
            // at all unless an icon is on screen and the line is turned on.
            //
            // The icon's badge rides the same tick, for the same reason and
            // two of its own: "due today" rolls over at midnight, and a
            // background mail sync writes without announcing a change. Also
            // a no-op unless something is chosen to count -- see
            // `Badge::tick`.
            let tray_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
                // `Burst`, the default, would fire a minute's worth of
                // rebuilds back to back after the process was asleep for a
                // while -- a laptop closed overnight -- for no benefit over
                // the one rebuild that actually matters once it wakes.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    tray_handle.state::<Tray>().schedule_refresh(&tray_handle);
                    tray_handle.state::<Badge>().tick(&tray_handle);
                }
            });
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
            // Letting another program use this vault's tools over MCP.
            commands::mcp_status,
            commands::web_status,
            commands::web_start,
            commands::web_stop,
            commands::mcp_start,
            commands::mcp_stop,
            commands::mcp_set_destructive,
            commands::mcp_issue_token,
            // A notification the interface has decided should leave the
            // window. See `notice.rs` for why the plugin does not post it.
            notice::post_notification,
            // The OS-wide key to the palette.
            commands::hotkey_status,
            commands::set_hotkey,
            // Writing help over text selected in any application: its popup
            // and its setting. See `writing.rs`.
            writing::writing_selection,
            writing::writing_apply,
            writing::writing_dismiss,
            writing::writing_open_main,
            writing::writing_settings,
            writing::set_writing_settings,
            // The two that are not JSON in and JSON out: one takes a raw
            // body, the other answers with a stream. Reading bytes back is
            // neither -- that is the `everyday://` handler in `protocol.rs`.
            commands::put_blob,
            commands::send_message,
            // A save dialog and a file picker. The archive between them never
            // touches the webview: see `transfer.rs`.
            transfer::save_export,
            transfer::open_import,
            transfer::save_mail_part,
            // This process's own furniture.
            commands::set_opens_itself,
            commands::ready_to_close,
            commands::set_tray_menu,
            commands::hide_tray,
            commands::set_tray_meeting,
            commands::set_badge,
            // An app in a window of its own, and that window handing what is
            // not its app back to the main one. See `windows.rs`.
            windows::open_app_window,
            windows::show_in_main,
            // Meeting notes: hearing a call. See `capture.rs` and
            // `meeting.rs`.
            meeting::meeting_start,
            meeting::meeting_stop,
            meeting::meeting_status,
            meeting::voice_enrol,
            offers::meeting_offer_answer,
            offers::meeting_offers,
            // Screen time on this machine. See `screentime.rs`.
            screentime::screentime_status,
            screentime::screentime_set_tracking,
            screentime::screentime_set_device,
            screentime::screentime_install_gnome_extension,
            screentime::screentime_connect_browser,
        ])
        .on_window_event(|window, event| {
            // Writing help's popup is not the application: closing it is
            // dismissing it, and it goes when the main window does, or a
            // hidden popup would keep the process alive with no way back.
            if window.label() == writing::LABEL {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
                return;
            }
            // An app in a window of its own is not the application either:
            // closing it saves and closes that window and nothing else.
            if windows::is_app_window(window.label()) {
                windows::on_window_event(window, event);
                return;
            }
            if let WindowEvent::Destroyed = event {
                if let Some(popup) = window.app_handle().get_webview_window(writing::LABEL) {
                    let _ = popup.destroy();
                }
                // Asked to save and close on the way here; whatever has not
                // by now goes with the main window, or the process would
                // stay up behind a window of one app with no tray to quit
                // it from.
                windows::destroy_all(window.app_handle());
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing the window is not quitting. Quit is, from the tray
                // (or the platform's own quit), and it still saves and locks
                // on the way out.
                //
                // So the window hides and the process stays, with the vault
                // as it was: the seven o'clock brief happens whether or not
                // anybody opened anything, mail and reminders still arrive, a
                // vault served to another machine goes on being served, the
                // tray's quick actions still work, and an app open in a
                // window of its own (see `windows`) stays open. The window
                // comes back from the tray, the hotkey, a second launch or
                // the Dock.
                //
                // Which means an unlocked vault stays unlocked: its key, and
                // the search index, stay in memory behind a hidden window,
                // exactly as behind a window left open on a desk. The lock
                // timers go on running in it and decide when that ends, the
                // way they would there; Quit, or a lock, ends it at once.
                //
                // Hiding rather than destroying also keeps the webview, which
                // is what lets a notification raised at seven reach the
                // notification centre: the routing that decides banner or
                // toast lives in the interface, and a destroyed webview cannot
                // run it. See `events.rs`.
                // ...but only while the tray icon is up, because that is
                // where Quit is -- or, failing that, while there is work here
                // that needs no window and the hotkey is a way back to it.
                //
                // The hotkey brings a hidden window back but cannot quit it,
                // and the window has no Quit of its own -- its close button is
                // this. So with the tray switched off, or on a desktop with no
                // StatusNotifier host to put it in, hiding an ordinary vault's
                // window would leave a process that can only be quit from the
                // command line, and closing the window is the last thing the
                // person can do to this application: it has to mean what it
                // says. A vault with a routine due, or being served, is the
                // exception it always was (see `AppState::stays_resident`):
                // stopping that is not what closing a window asks for. With
                // neither tray nor hotkey, nothing hides.
                //
                // And never for the tray's Quit, which arrives here as this
                // same close request: hiding is what it is asking not to do.
                let handle = window.app_handle();
                let hides = window.try_state::<AppState>().is_some_and(|s| {
                    !s.is_quitting()
                        && (handle.state::<Tray>().is_showing()
                            || (hotkey::is_registered(handle) && s.stays_resident()))
                });
                if hides {
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
                    // To this window alone: an app open in a window of its
                    // own listens for the same event, and is asked on its
                    // own just below.
                    let _ = window.emit_to(window.label(), SAVE_AND_CLOSE, ());
                    // The windows of one app go with this one, after
                    // writing what was typed in them -- which
                    // `ready_to_close` waits for before it locks.
                    windows::close_all(window.app_handle());

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

                // Lock on the way out so a process that is going never
                // lingers with a decrypted key in memory, and so the search
                // index -- which holds plaintext -- is dropped.
                if let Some(state) = window.try_state::<AppState>()
                    && let Some(vault) = state.service().get()
                {
                    vault.lock();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start Every Day")
        .run(|_app, _event| {
            // A click on the Dock icon, or opening the app again, while the
            // window is hidden -- which closing it does now, see the close
            // handler above. macOS hands the running instance a reopen
            // rather than starting a second one, so the single-instance
            // callback never hears of it, and without this the window would
            // only come back from the menu bar. Asked of the main window
            // rather than read from `has_visible_windows`: an app open in a
            // window of its own is a visible window, and the main one is
            // still the one a click on the Dock is asking for.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event
                && _app
                    .get_webview_window(windows::MAIN)
                    .is_some_and(|w| !w.is_visible().unwrap_or(true))
            {
                tray::raise_window(_app);
            }
        });
}
