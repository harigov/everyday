//! The Every Day desktop shell.
//!
//! This crate is deliberately thin. It owns the window, the command bridge
//! and the media protocol; everything about journals, encryption and storage
//! lives in [`everyday_core`]. That split is what lets the same core back a
//! mobile shell later, and it is why the entry point is a library: Tauri
//! builds iOS and Android targets from `run()` rather than from `main`.

mod agent;
mod commands;
mod error;
mod feeds;
mod http;
mod notify;
mod protocol;
mod state;
mod tray;
mod websearch;

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
        // it -- see `notify.rs` for why the routing decision lives on one
        // side of the bridge only.
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::new())
        // Empty until the interface asks for a tray. Nothing is put in the
        // menu bar on a machine where the setting is off, and nothing is put
        // there before the interface knows what belongs in it -- a tray that
        // appears at launch holding only "Quit" is worse than one that
        // appears a moment later holding the actions.
        .manage(Tray::default())
        // Confirmations the assistant is waiting on. Process-wide rather
        // than per turn because the answer arrives as a separate command
        // from the webview, which has no handle on the run that asked.
        .manage(std::sync::Arc::new(agent::Pending::default()))
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
            commands::bootstrap,
            commands::create_vault,
            commands::open_vault,
            commands::unlock,
            commands::lock,
            commands::status,
            commands::change_password,
            commands::set_auto_lock,
            commands::touch,
            commands::poll_auto_lock,
            commands::list_journals,
            commands::new_journal,
            commands::save_journal,
            commands::delete_journal,
            commands::list_entries,
            commands::get_entry,
            commands::new_entry,
            commands::save_entry,
            commands::save_entry_force,
            commands::delete_entry,
            commands::search,
            commands::list_tags,
            commands::list_projects,
            commands::new_project,
            commands::save_project,
            commands::delete_project,
            commands::list_tasks,
            commands::get_task,
            commands::new_task,
            commands::save_task,
            commands::save_tasks,
            commands::delete_task,
            commands::list_blocks,
            commands::new_block,
            commands::save_block,
            commands::delete_block,
            commands::task_tags,
            commands::task_stats,
            commands::list_calendars,
            commands::save_calendar,
            commands::delete_calendar,
            commands::subscribe_calendar,
            commands::import_calendar,
            commands::sync_calendar,
            commands::sync_due_calendars,
            commands::calendar_providers,
            commands::list_events,
            commands::get_event,
            commands::list_kinds,
            commands::new_kind,
            commands::save_kind,
            commands::delete_kind,
            commands::list_items,
            commands::get_item,
            commands::add_item,
            commands::save_item,
            commands::save_items,
            commands::delete_item,
            commands::set_item_status,
            commands::set_item_progress,
            commands::list_logs,
            commands::new_log,
            commands::save_log,
            commands::delete_log,
            commands::library_stats,
            commands::web_search,
            commands::search_sources,
            commands::lookup_metadata,
            commands::apply_metadata,
            commands::fetch_image,
            commands::new_tracker,
            commands::list_readings,
            commands::tracker_days,
            commands::log_reading,
            commands::save_reading,
            commands::delete_reading,
            commands::delete_tracker,
            commands::put_blob,
            commands::collect_garbage,
            commands::vault_stats,
            commands::ready_to_close,
            commands::set_tray_menu,
            commands::hide_tray,
            commands::agent_settings,
            commands::save_agent_settings,
            commands::set_agent_key,
            commands::clear_agent_key,
            commands::list_conversations,
            commands::new_conversation,
            commands::conversation_messages,
            commands::delete_conversation,
            commands::send_message,
            commands::confirm_tool_call,
            commands::list_memories,
            commands::save_memory,
            commands::delete_memory,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
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
                            && let Some(vault) = state.get()
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
                    && let Some(vault) = state.get()
                {
                    vault.lock();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start Every Day");
}
