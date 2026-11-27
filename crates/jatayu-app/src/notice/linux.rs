//! The Linux side of [`super`]: notifications posted straight to the session bus with
//! `zbus`, so the buttons can be heard, in place of the plugin that cannot deliver here.

use std::collections::HashMap;
use std::sync::OnceLock;

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use tauri::AppHandle;
use tokio::sync::{mpsc, oneshot};
use zbus::zvariant::Value;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

pub(super) enum Cmd {
    Show {
        key: String,
        summary: String,
        body: String,
        actions: Vec<(String, String)>,
        /// Critical, with a sound: see [`super::Alert`].
        alert: bool,
        /// Told whether the desktop took it, by whoever wants to know.
        done: Option<oneshot::Sender<bool>>,
    },
    Withdraw {
        key: String,
    },
}

/// What the worker wakes for.
enum Wake {
    Cmd(Cmd),
    Action(zbus::Message),
    Closed(zbus::Message),
}

/// The sending half of the worker's queue, created on the first call.
static WORKER: OnceLock<mpsc::UnboundedSender<Cmd>> = OnceLock::new();

pub(super) fn send(app: &AppHandle, cmd: Cmd) {
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        tauri::async_runtime::spawn(run(app.clone(), rx));
        tx
    });
    // Only fails if the worker is gone, which it never is while the
    // process lives -- and there would be nothing to do about it.
    let _ = tx.send(cmd);
}

/// Which id the daemon gave each key, both ways: a key to replace or
/// close its notification, an id to find the key a button press was on.
#[derive(Default)]
struct Ids {
    by_key: HashMap<String, u32>,
    by_id: HashMap<u32, String>,
}

impl Ids {
    fn set(&mut self, key: &str, id: u32) {
        if let Some(old) = self.by_key.insert(key.to_string(), id) {
            self.by_id.remove(&old);
        }
        self.by_id.insert(id, key.to_string());
    }

    fn forget_id(&mut self, id: u32) {
        if let Some(key) = self.by_id.remove(&id) {
            self.by_key.remove(&key);
        }
    }
}

/// Own the connection and the queue for the life of the process.
///
/// The connection is made here, on the async runtime, the first time
/// anything is shown, and kept: opening one per notification would
/// leave each button press nowhere to be heard, since a signal goes
/// only to the connection that asked for it. If there is no session bus
/// the queue is still drained, so a desktop without one costs a log
/// line and not a queue that grows for ever.
async fn run(app: AppHandle, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let proxy = match connect().await {
        Ok(proxy) => proxy,
        Err(e) => {
            tracing::debug!(error = %e, "no notification service to talk to");
            while let Some(cmd) = rx.recv().await {
                if let Cmd::Show { done: Some(done), .. } = cmd {
                    let _ = done.send(false);
                }
            }
            return;
        }
    };
    let entry = super::desktop_entry_name(
        std::env::var("GIO_LAUNCHED_DESKTOP_FILE").ok().as_deref(),
        app.package_info().crate_name,
    );
    serve(proxy, rx, entry, move |key, action| crate::offers::on_notice_action(&app, key, action))
        .await;
}

/// [`run`]'s loop, given the connection: everything but where a press
/// goes, so it can be run against a stand-in notification server.
async fn serve(
    proxy: zbus::Proxy<'static>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    entry: String,
    on_action: impl Fn(&str, &str),
) {
    let actions: BoxStream<'static, Wake> = match proxy.receive_signal("ActionInvoked").await {
        Ok(s) => s.map(Wake::Action).boxed(),
        Err(e) => {
            tracing::debug!(error = %e, "notification buttons will not be heard");
            stream::empty().boxed()
        }
    };
    let closed: BoxStream<'static, Wake> = match proxy.receive_signal("NotificationClosed").await {
        Ok(s) => s.map(Wake::Closed).boxed(),
        Err(_) => stream::empty().boxed(),
    };
    let cmds: BoxStream<'static, Wake> =
        stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Wake::Cmd).boxed();
    let mut wakes = stream::select(cmds, stream::select(actions, closed));

    let mut ids = Ids::default();

    // Whether this server reads markup, which decides whether the text
    // is escaped: one that does would show `<b>` as bold and `<a>` as a
    // link, and one that does not would show an escaped `&lt;` as the
    // five characters it is. A server that cannot be asked is taken to
    // read it, since a stray `&amp;` is the milder mistake.
    let capabilities: zbus::Result<Vec<String>> = proxy.call("GetCapabilities", &()).await;
    let markup = capabilities.map_or(true, |caps| caps.iter().any(|c| c == "body-markup"));
    let shown = |text: &str| if markup { super::escape_markup(text) } else { text.to_string() };

    while let Some(wake) = wakes.next().await {
        match wake {
            Wake::Cmd(Cmd::Show { key, summary, body, actions, alert, done }) => {
                let replaces = ids.by_key.get(&key).copied().unwrap_or(0);
                let flat = super::flatten_actions(&actions);
                let alert = super::Alert::new(alert);
                let mut hints: HashMap<&str, Value<'_>> = HashMap::from([
                    ("desktop-entry", Value::from(entry.as_str())),
                    ("urgency", Value::U8(alert.urgency)),
                ]);
                if let Some(sound) = alert.sound {
                    hints.insert("sound-name", Value::from(sound));
                }
                // The body only: the specification gives markup to the
                // body alone, and a summary escaped would read `Q&amp;A`.
                let body = shown(&body);
                let reply: zbus::Result<u32> = proxy
                    .call(
                        "Notify",
                        &(
                            "Jatayu",
                            replaces,
                            entry.as_str(),
                            summary.as_str(),
                            body.as_str(),
                            flat,
                            hints,
                            -1i32,
                        ),
                    )
                    .await;
                let took = reply.is_ok();
                match reply {
                    Ok(id) => ids.set(&key, id),
                    Err(e) => tracing::debug!(error = %e, "a notification could not be shown"),
                }
                if let Some(done) = done {
                    let _ = done.send(took);
                }
            }
            Wake::Cmd(Cmd::Withdraw { key }) => {
                if let Some(id) = ids.by_key.get(&key).copied() {
                    let reply: zbus::Result<()> = proxy.call("CloseNotification", &(id,)).await;
                    if let Err(e) = reply {
                        tracing::debug!(error = %e, "a notification could not be closed");
                    }
                    ids.forget_id(id);
                }
            }
            Wake::Action(message) => {
                if let Ok((id, action)) = message.body().deserialize::<(u32, String)>()
                    && let Some(key) = ids.by_id.get(&id).cloned()
                {
                    on_action(&key, &action);
                }
            }
            Wake::Closed(message) => {
                if let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() {
                    ids.forget_id(id);
                }
            }
        }
    }
}

async fn connect() -> zbus::Result<zbus::Proxy<'static>> {
    let connection = zbus::Connection::session().await?;
    zbus::Proxy::new(&connection, SERVICE, PATH, INTERFACE).await
}
