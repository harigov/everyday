//! A way for a test script into a window where no WebDriver reaches.
//!
//! Playwright drives the interface on its mock backend in a browser, and the
//! Android app's webview over adb. Neither reaches WKWebView: not the Mac
//! app's window, and not the iOS simulator's -- Tauri's WebDriver is Linux
//! and Windows only. So a build made with `--features drive` listens on a
//! Unix socket named by `JATAYU_DRIVE`, runs the JavaScript each
//! connection sends in a window, and answers with what it returned:
//!
//! ```text
//! client -> "@main\nreturn document.title"   (the "@label" line is optional)
//!           half-closes its side
//! shell  -> {"id":1,"ok":true,"value":"\"Jatayu\""}   and closes
//! ```
//!
//! The script is the body of an async function: `return` a value, `await`
//! freely. `value` is that value as JSON text; a throw comes back as
//! `ok: false` with its stack. The answer travels back as an ordinary event
//! (`plugin:event|emit`, which `core:default` already grants the window),
//! so this adds no command to the app's own list.
//!
//! `ui/scripts/drive.mjs` is the client; see `docs/mobile.md`.
//!
//! **Never ship it.** Whoever can open the socket can do anything the
//! interface can, the unlocked vault included. It is off unless the feature
//! is compiled in *and* the variable is set, the socket is made readable by
//! its owner only, and no build the repository describes -- `make build`,
//! `make dist`, a release -- turns the feature on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tauri::{AppHandle, Listener, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

/// The event a window's answer comes back on.
const EVENT: &str = "jatayu-drive";

/// How long a script may take before the client is told it gave no answer
/// -- one that navigated the page away, say, never will.
const PATIENCE: Duration = Duration::from_secs(60);

type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// Listen, if `JATAYU_DRIVE` names a socket. Call once the windows'
/// app handle exists.
pub fn start(app: &AppHandle) {
    let Some(path) = std::env::var_os("JATAYU_DRIVE") else { return };
    let path = PathBuf::from(path);
    let waiting = Waiting::default();

    let answers = waiting.clone();
    app.listen_any(EVENT, move |event| {
        let Ok(answer) = serde_json::from_str::<Value>(event.payload()) else { return };
        let Some(id) = answer.get("id").and_then(Value::as_u64) else { return };
        if let Some(tx) = answers.lock().expect("drive: waiting list").remove(&id) {
            let _ = tx.send(answer);
        }
    });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match bind(&path) {
            Ok(listener) => listener,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "drive: could not listen");
                return;
            }
        };
        tracing::warn!(path = %path.display(), "drive: listening -- a test build; never ship it");
        let next = Arc::new(AtomicU64::new(1));
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                // Out of descriptors, say, which the next try would be too:
                // a pause, rather than a core spun on it.
                Err(e) => {
                    tracing::warn!(error = %e, "drive: could not accept");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            };
            let (app, waiting, next) = (app.clone(), waiting.clone(), next.clone());
            tauri::async_runtime::spawn(serve(stream, app, waiting, next));
        }
    });
}

/// The socket, readable and writable by its owner only.
fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn serve(mut stream: UnixStream, app: AppHandle, waiting: Waiting, next: Arc<AtomicU64>) {
    let mut request = String::new();
    if stream.read_to_string(&mut request).await.is_err() {
        return;
    }
    let (label, script) = match request.strip_prefix('@') {
        Some(rest) => match rest.split_once('\n') {
            Some((label, script)) => (label.trim().to_owned(), script.to_owned()),
            None => (rest.trim().to_owned(), String::new()),
        },
        None => (crate::MAIN_WINDOW.to_owned(), request),
    };
    let answer = run(&app, &waiting, &next, &label, &script).await;
    let _ = stream.write_all(answer.to_string().as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn run(
    app: &AppHandle,
    waiting: &Waiting,
    next: &AtomicU64,
    label: &str,
    script: &str,
) -> Value {
    let Some(window) = app.get_webview_window(label) else {
        let open: Vec<String> = app.webview_windows().into_keys().collect();
        return json!({ "ok": false, "value": format!("no window {label:?}; open: {open:?}") });
    };
    let id = next.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    waiting.lock().expect("drive: waiting list").insert(id, tx);

    // A script that does not parse never starts, and so would never answer:
    // this, run first and on its own, says so if it has not started within
    // a second. (Parsing it here instead, with `new Function`, is what the
    // page's content security policy forbids.)
    let watch = format!(
        r#"setTimeout(() => {{
  if (!(window.__kyDriveRan || {{}})[{id}]) {{
    window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {{
      event: '{EVENT}',
      payload: {{ id: {id}, ok: false, value: JSON.stringify('the script did not parse') }},
    }})
  }}
}}, 1000)"#
    );
    let wrapped = format!(
        r#"(async () => {{
  (window.__kyDriveRan = window.__kyDriveRan || {{}})[{id}] = true
  let ok = true, value
  try {{
    value = await (async () => {{
{script}
    }})()
  }} catch (e) {{
    ok = false
    // WebKit's `stack` is the frames alone, without the message.
    value = e && e.message ? `${{e.message}}\n${{e.stack || ''}}` : String(e)
  }}
  let text
  try {{ text = JSON.stringify(value === undefined ? null : value) }} catch (e) {{ text = undefined }}
  // A function or a symbol is not JSON at all: say what it is instead.
  if (text === undefined) text = JSON.stringify(String(value))
  await window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {{
    event: '{EVENT}',
    payload: {{ id: {id}, ok, value: text }},
  }})
}})()"#
    );
    let _ = window.eval(&watch);
    if let Err(e) = window.eval(&wrapped) {
        waiting.lock().expect("drive: waiting list").remove(&id);
        return json!({ "ok": false, "value": format!("could not run it: {e}") });
    }
    match tokio::time::timeout(PATIENCE, rx).await {
        Ok(Ok(answer)) => answer,
        _ => {
            waiting.lock().expect("drive: waiting list").remove(&id);
            json!({ "ok": false, "value": format!("no answer in {} s", PATIENCE.as_secs()) })
        }
    }
}
