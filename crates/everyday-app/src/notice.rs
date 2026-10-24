//! A desktop notification with buttons, for an offer to take notes.
//!
//! An offer that only ever shows inside the window is no offer to somebody
//! who is in another application -- which, for a call starting, is exactly
//! where they are. The service used to send a plain `Notification` beside
//! the offer; the interface's `notify.svelte.ts` turned that into an
//! operating-system banner when the window was out of sight, and a banner
//! with no buttons can only say "a meeting is starting", never ask the
//! question. This module is the shell raising the notification itself, so
//! it can carry "Take notes" and "Not now" and hear which was pressed. See
//! [`crate::offers`], the one caller, for what is said and when.
//!
//! # Buttons are Linux-only, for now
//!
//! The XDG notification service reports a button press as a signal on the
//! session bus, which a process can simply listen for. The other two
//! platforms report one only through an activation callback that
//! `tauri-plugin-notification` does not surface on desktop, so elsewhere
//! this is the plugin's plain title-and-body notification: pressing it
//! does nothing the shell hears about. The in-window banner
//! (`MeetingOfferBanner.svelte`) has the buttons everywhere, and so does the
//! tray's "Take notes for ..." item; the notification is the reminder, not
//! the only way to answer.
//!
//! # Why one worker, not a task per call
//!
//! A notification raised and then withdrawn a moment later -- the call ended
//! before anyone looked -- must arrive at the daemon in that order. Two
//! tasks spawned one after the other on a multi-threaded runtime make no
//! such promise, and a withdrawal that overtakes its own notification
//! leaves the notification up for good. So on Linux every call is a
//! message to one worker, which owns the connection, runs them in the order
//! they were sent, and is also the one place the daemon's `ActionInvoked`
//! and `NotificationClosed` signals are read.

use tauri::AppHandle;

/// Show (or replace) the notification for offer `key`. `actions` are
/// `(action key, label)` pairs; a press comes back through
/// [`crate::offers::on_notice_action`].
///
/// Returns at once: the notification is raised on a worker. Failures --
/// no notification daemon, a refused call -- are logged and otherwise
/// silent, because the offer is also in the window and the tray, and a
/// desktop with no daemon has nowhere to put this at all.
pub fn show(app: &AppHandle, key: &str, summary: &str, body: &str, actions: &[(&str, &str)]) {
    #[cfg(target_os = "linux")]
    linux::send(
        app,
        linux::Cmd::Show {
            key: key.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            actions: actions.iter().map(|(k, l)| (k.to_string(), l.to_string())).collect(),
        },
    );
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_notification::NotificationExt;
        // No buttons to give: see the module doc. `key` and `actions` have
        // nothing to attach to.
        let _ = (key, actions);
        if let Err(e) = app.notification().builder().title(summary).body(body).show() {
            tracing::debug!(error = %e, "a notification could not be shown");
        }
    }
}

/// Take the notification for `key` down, if it is up. A no-op off Linux,
/// where the plain notification this module raises cannot be recalled.
pub fn withdraw(app: &AppHandle, key: &str) {
    #[cfg(target_os = "linux")]
    linux::send(app, linux::Cmd::Withdraw { key: key.to_string() });
    #[cfg(not(target_os = "linux"))]
    let _ = (app, key);
}

/// `org.freedesktop.Notifications` `Notify`'s flat action list: key, label,
/// key, label ..., then the `"default"` action with no label, which is how a
/// daemon is told to report a click on the notification's body -- most
/// draw no button for an action with an empty label, and the rest draw one
/// that does what the body click does.
#[cfg(any(target_os = "linux", test))]
fn flatten_actions(actions: &[(String, String)]) -> Vec<&str> {
    let mut flat: Vec<&str> = Vec::with_capacity(actions.len() * 2 + 2);
    for (key, label) in actions {
        flat.push(key);
        flat.push(label);
    }
    flat.push("default");
    flat.push("");
    flat
}

/// The desktop file's name, minus `.desktop`, a notification's
/// `desktop-entry` hint wants: so the shell groups it under Every Day and
/// shows its icon. Whatever launched this process, when it said so (GLib's
/// `GIO_LAUNCHED_DESKTOP_FILE`, set for anything started through a
/// `GDesktopAppInfo`, GNOME Shell's launchers among them); otherwise the
/// binary's name, which is what `make desktop-entry` names its file and
/// its icon. See `badge::desktop_ids`, which tells a dock about all the
/// names this process might be known by -- a notification can only name
/// one.
#[cfg(any(target_os = "linux", test))]
fn desktop_entry_name(launched: Option<&str>, binary: &str) -> String {
    launched
        .and_then(|path| std::path::Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".desktop"))
        .filter(|stem| !stem.is_empty())
        .unwrap_or(binary)
        .to_string()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use futures_util::StreamExt;
    use futures_util::stream::{self, BoxStream};
    use tauri::AppHandle;
    use tokio::sync::mpsc;
    use zbus::zvariant::Value;

    const SERVICE: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    const INTERFACE: &str = "org.freedesktop.Notifications";

    pub(super) enum Cmd {
        Show { key: String, summary: String, body: String, actions: Vec<(String, String)> },
        Withdraw { key: String },
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
                while rx.recv().await.is_some() {}
                return;
            }
        };
        let entry = super::desktop_entry_name(
            std::env::var("GIO_LAUNCHED_DESKTOP_FILE").ok().as_deref(),
            app.package_info().crate_name,
        );
        serve(proxy, rx, entry, move |key, action| {
            crate::offers::on_notice_action(&app, key, action)
        })
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
        let closed: BoxStream<'static, Wake> =
            match proxy.receive_signal("NotificationClosed").await {
                Ok(s) => s.map(Wake::Closed).boxed(),
                Err(_) => stream::empty().boxed(),
            };
        let cmds: BoxStream<'static, Wake> =
            stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Wake::Cmd).boxed();
        let mut wakes = stream::select(cmds, stream::select(actions, closed));

        let mut ids = Ids::default();

        while let Some(wake) = wakes.next().await {
            match wake {
                Wake::Cmd(Cmd::Show { key, summary, body, actions }) => {
                    let replaces = ids.by_key.get(&key).copied().unwrap_or(0);
                    let flat = super::flatten_actions(&actions);
                    let hints: HashMap<&str, Value<'_>> = HashMap::from([
                        ("desktop-entry", Value::from(entry.as_str())),
                        // Normal. Not critical, which some shells refuse to
                        // time out, and not low, which some never show.
                        ("urgency", Value::U8(1)),
                    ]);
                    let reply: zbus::Result<u32> = proxy
                        .call(
                            "Notify",
                            &(
                                "Every Day",
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
                    match reply {
                        Ok(id) => ids.set(&key, id),
                        Err(e) => tracing::debug!(error = %e, "a notification could not be shown"),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_action_is_always_last() {
        let actions = vec![("take".to_string(), "Take notes".to_string())];
        assert_eq!(flatten_actions(&actions), ["take", "Take notes", "default", ""]);
        assert_eq!(flatten_actions(&[]), ["default", ""]);
    }

    #[test]
    fn the_launching_desktop_file_names_the_entry() {
        assert_eq!(
            desktop_entry_name(
                Some("/home/sam/.local/share/applications/my-everyday.desktop"),
                "everyday-app"
            ),
            "my-everyday"
        );
        assert_eq!(desktop_entry_name(None, "everyday-app"), "everyday-app");
        assert_eq!(
            desktop_entry_name(Some("/usr/bin/everyday-app"), "everyday-app"),
            "everyday-app"
        );
    }
}
