//! The Every Day desktop shell.
//!
//! This crate is deliberately thin. It owns the window, the command bridge
//! and the media protocol; everything about journals, encryption and storage
//! lives in [`everyday_core`]. That split is what lets the same core back a
//! mobile shell later, and it is why the entry point is a library: Tauri
//! builds iOS and Android targets from `run()` rather than from `main`.

mod commands;
mod error;
mod feeds;
mod protocol;
mod state;

use state::AppState;
use tauri::{Manager, WindowEvent};

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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
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
            commands::put_blob,
            commands::collect_garbage,
            commands::vault_stats,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
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
