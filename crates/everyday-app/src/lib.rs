//! The Every Day desktop shell.
//!
//! This crate is deliberately thin. It owns the window, the command bridge
//! and the media protocol; everything about journals, encryption and storage
//! lives in [`everyday_core`]. That split is what lets the same core back a
//! mobile shell later, and it is why the entry point is a library: Tauri
//! builds iOS and Android targets from `run()` rather than from `main`.

mod commands;
mod error;
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
