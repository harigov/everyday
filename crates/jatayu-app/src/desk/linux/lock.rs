//! Lock the screen.
//!
//! There is no one way, and which a desktop answers depends on who runs its
//! lock screen, so this asks in order and stops at the first that takes it:
//!
//! 1. `org.gnome.ScreenSaver.Lock`, which GNOME Shell answers on the session
//!    bus -- what its own lock button calls;
//! 2. `org.freedesktop.ScreenSaver.Lock`, the name KDE, Cinnamon, MATE and
//!    most others answer, and the one every screen saver is meant to;
//! 3. `loginctl lock-session`, which asks logind to say "lock" to every
//!    program in the session that listens for it -- the Shell and the others
//!    all do -- and works where nobody owns either bus name.
//!
//! A bus name nobody owns is an answer, not a failure, so each call is
//! given a few seconds and the next is tried.

use std::time::Duration;

use zbus::Connection;

/// How long one way gets before the next is tried.
const PATIENCE: Duration = Duration::from_secs(3);

/// The two screen saver interfaces on the session bus, and where each lives.
const SCREEN_SAVERS: [(&str, &str); 2] = [
    ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver"),
    ("org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver"),
];

/// Lock the screen, by whichever way works.
pub(in crate::desk) async fn lock() -> Result<(), String> {
    match Connection::session().await {
        Ok(connection) => {
            for (name, path) in SCREEN_SAVERS {
                let call = connection.call_method(Some(name), path, Some(name), "Lock", &());
                match tokio::time::timeout(PATIENCE, call).await {
                    Ok(Ok(_)) => return Ok(()),
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, name, "this screen saver would not lock")
                    }
                    Err(_) => tracing::debug!(name, "this screen saver did not answer"),
                }
            }
        }
        Err(e) => tracing::debug!(error = %e, "no session bus to lock the screen over"),
    }
    let ran = tauri::async_runtime::spawn_blocking(|| super::run("loginctl", &["lock-session"]))
        .await
        .map_err(|e| e.to_string())
        .and_then(|ran| ran);
    match ran {
        Ok(ran) if ran.success => Ok(()),
        Ok(ran) => {
            tracing::warn!(stderr = %ran.stderr.trim(), "loginctl would not lock the session");
            Err("I couldn't lock the screen.".to_string())
        }
        Err(e) => {
            tracing::warn!(error = %e, "the screen could not be locked");
            Err("I couldn't lock the screen.".to_string())
        }
    }
}
