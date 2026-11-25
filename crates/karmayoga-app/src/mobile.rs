//! The phone shell.
//!
//! Always a remote client. The vault, its search index, the models and every
//! background task -- mail sync, the routines, the briefs -- stay on the
//! computer this phone has paired with; this process holds a connection to it
//! and one full-screen window. So the setup screen offers the vaults it has
//! paired with and a box for a pairing link, never a vault of its own, and
//! every command the interface runs goes to the other computer through
//! [`crate::remote`], exactly as it does for a desktop window looking at a
//! vault elsewhere.
//!
//! What the desktop shell has and this does not: the tray, the OS-wide keys,
//! the small windows that work over other applications, a second instance to
//! turn away, app windows, serving a vault, screen time, recording a meeting.
//! The interface is told (`Bootstrap::mobile`) and leaves them alone.
//!
//! See `docs/mobile.md` for building and testing it.

use std::sync::Arc;
use tauri::Manager;

use crate::state::AppState;
use crate::{commands, events, navigation, notice, protocol};

/// The one window's label: the desktop's main window's, so the interface and
/// `capabilities/` need not tell them apart.
pub const MAIN: &str = "main";

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::new())
        .setup(|app| {
            // Before anything reads a setting: the paired computers and their
            // tokens live in the app's private storage, which only the
            // platform can name. See `karmayoga_vault::set_app_dirs`.
            let path = app.path();
            karmayoga_vault::set_app_dirs(path.app_config_dir()?, path.app_data_dir()?);

            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == MAIN)
                .cloned()
                .ok_or("tauri.conf.json has no main window")?;
            tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
                // Never anywhere but the interface itself -- see `navigation`.
                .on_navigation(navigation::guard(navigation::dev_url(app.handle())))
                .on_new_window(navigation::new_window(app.handle().clone()))
                .build()?;

            // The connection's events task re-raises the server's remarks
            // through this, as the desktop's does. See `remote`.
            app.state::<AppState>()
                .set_sink(Arc::new(events::WindowSink::new(app.handle().clone())));
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("karmayoga", |ctx, request, responder| {
            protocol::handle(ctx.app_handle(), request, responder);
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::call,
            commands::list_remotes,
            commands::connect_remote,
            commands::reconnect_remote,
            commands::disconnect_remote,
            commands::forget_remote,
            commands::put_blob,
            commands::send_message,
            notice::post_notification,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Karma Yoga");
}

/// Where this process's log goes on Android: logcat, under the tag
/// `KarmaYoga`. An app's standard output goes nowhere there, so without this
/// `adb logcat` would show nothing of ours.
#[cfg(target_os = "android")]
pub struct Logcat;

#[cfg(target_os = "android")]
mod logcat {
    use std::ffi::{CString, c_char, c_int};
    use tracing::{Level, Metadata};
    use tracing_subscriber::fmt::MakeWriter;

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    /// One event's line, written to logcat when the formatter is done with
    /// it.
    pub struct Line {
        priority: c_int,
        bytes: Vec<u8>,
    }

    impl std::io::Write for Line {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Line {
        fn drop(&mut self) {
            let text = String::from_utf8_lossy(&self.bytes);
            let text = text.trim_end().replace('\0', " ");
            if text.is_empty() {
                return;
            }
            let Ok(text) = CString::new(text) else { return };
            // SAFETY: both are NUL-terminated strings that outlive the call,
            // which copies them.
            unsafe {
                __android_log_write(self.priority, c"KarmaYoga".as_ptr(), text.as_ptr());
            }
        }
    }

    /// Android's own priorities: `android/log.h`.
    fn priority(level: &Level) -> c_int {
        match *level {
            Level::ERROR => 6,
            Level::WARN => 5,
            Level::INFO => 4,
            Level::DEBUG => 3,
            Level::TRACE => 2,
        }
    }

    impl<'a> MakeWriter<'a> for super::Logcat {
        type Writer = Line;

        fn make_writer(&'a self) -> Line {
            Line { priority: priority(&Level::INFO), bytes: Vec::new() }
        }

        fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Line {
            Line { priority: priority(meta.level()), bytes: Vec::new() }
        }
    }
}
