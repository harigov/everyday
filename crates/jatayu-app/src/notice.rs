//! A desktop notification, with buttons when it is an offer to take notes.
//!
//! Every notification this application raises goes through here, because
//! `tauri-plugin-notification` cannot deliver one on Linux: its `show` spawns
//! a task on the tokio runtime and calls `notify_rust`'s blocking `show` from
//! inside it, which is `zbus::block_on`, and with zbus's `tokio` feature on
//! (`ashpd` switches it on, and features are per build, not per crate) that
//! panics with "Cannot start a runtime from within a runtime". Tokio catches
//! the panic, the plugin reports success, and nothing arrives. So
//! `notify.svelte.ts` still decides *whether* something leaves the window, and
//! [`post_notification`] is how it goes.
//!
//! An offer that only ever shows inside the window is no offer to somebody
//! who is in another application -- which, for a call starting, is exactly
//! where they are. The service used to send a plain `Notification` beside
//! the offer; the interface's `notify.svelte.ts` turned that into an
//! operating-system banner when the window was out of sight, and a banner
//! with no buttons can only say "a meeting is starting", never ask the
//! question. This module is the shell raising the notification itself, so
//! it can carry "Take notes" and "Not now" and hear which was pressed. See
//! [`crate::offers`] for what is said and when.
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

use std::sync::atomic::{AtomicU64, Ordering};

use jatayu_service::error::{CommandError, CommandResult, codes};
use tauri::AppHandle;

/// A plain notification the interface has decided should leave the window.
///
/// `key` is the notification's identity, so a condition that recurs replaces
/// its own banner. One with no identity gets one of its own, because the same
/// map is what stops two notifications being taken for the same one. A click
/// raises the window: see [`crate::offers::on_notice_action`].
///
/// Answers once the desktop has, and with an error if it would not take it --
/// a session with no notification service is an ordinary one -- because
/// `notify.svelte.ts` puts up a toast instead, and that is the difference
/// between the message being late and being lost.
#[tauri::command]
pub async fn post_notification(
    app: AppHandle,
    title: String,
    body: Option<String>,
    key: Option<String>,
) -> CommandResult<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let key = key.unwrap_or_else(|| format!("plain:{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    if deliver(&app, &key, &title, body.as_deref().unwrap_or_default()).await {
        Ok(())
    } else {
        Err(CommandError::new(codes::UNSUPPORTED, "the desktop did not take the notification"))
    }
}

/// [`show`] with no buttons, answering whether the desktop took it.
async fn deliver(app: &AppHandle, key: &str, summary: &str, body: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let (done, answer) = tokio::sync::oneshot::channel();
        linux::send(
            app,
            linux::Cmd::Show {
                key: key.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                actions: Vec::new(),
                alert: false,
                done: Some(done),
            },
        );
        answer.await.unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_notification::NotificationExt;
        let _ = key;
        app.notification().builder().title(summary).body(body).show().is_ok()
    }
}

#[cfg(desktop)]
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
            alert: false,
            done: None,
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

#[cfg(desktop)]
/// [`show`] for something that has to be noticed -- a timer that has run out
/// -- rather than something that may be: on Linux the notification is
/// critical, so it stays up until it is dismissed and is shown through Do Not
/// Disturb, and it asks the shell to play the alarm sound (see [`Alert`]).
///
/// No buttons: the press that matters is dismissing it. Elsewhere this is
/// the plain notification [`show`] raises, which the platform decides the
/// manners of.
pub fn alarm(app: &AppHandle, key: &str, summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    linux::send(
        app,
        linux::Cmd::Show {
            key: key.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            actions: Vec::new(),
            alert: true,
            done: None,
        },
    );
    #[cfg(not(target_os = "linux"))]
    show(app, key, summary, body, &[]);
}

/// How loudly a notification asks to be noticed, as the freedesktop
/// `urgency` and `sound-name` hints say it.
///
/// The three levels of urgency are 0 low, 1 normal and 2 critical. A server
/// treats critical as "keep this until somebody dismisses it", and GNOME
/// Shell shows it through Do Not Disturb -- which is the whole point of a
/// timer, and why nothing else here uses it. `sound-name` is a name in the
/// sound theme; GNOME Shell plays the sound of a notification that carries
/// one, and a server that does not simply ignores the hint.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Alert {
    urgency: u8,
    sound: Option<&'static str>,
}

#[cfg(any(target_os = "linux", test))]
impl Alert {
    /// What every notification but an alarm asks for. Normal, not critical,
    /// which some shells refuse to time out, and not low, which some never
    /// show.
    const NORMAL: Alert = Alert { urgency: 1, sound: None };

    /// A timer's: critical, with the sound theme's alarm.
    const ALARM: Alert = Alert { urgency: 2, sound: Some("alarm-clock-elapsed") };

    fn new(alarm: bool) -> Alert {
        if alarm { Alert::ALARM } else { Alert::NORMAL }
    }
}

#[cfg(desktop)]
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
/// `desktop-entry` hint wants: so the shell groups it under Jatayu and
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

/// `text` made safe for a notification server that reads markup.
///
/// The freedesktop specification lets a server render a small HTML subset in
/// a notification's body (`<b>`, `<a href>`, `<img src>`) -- KDE shows a
/// linked picture from a local path. What goes in a notification is often
/// somebody else's words: a mail's first lines, an invitation's place, a
/// feed item. Escaped, `<img src="file:///...">` is a line of text, which is
/// all it ever was meant to be. Only the body: the summary is plain text to
/// GNOME and KDE, which would show the escapes themselves.
#[cfg(any(target_os = "linux", test))]
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing but this file may ask the plugin to post: on Linux that reports
    /// success and delivers nothing, and the only sign of it is a panic line in
    /// the log, which is how every notification went missing for two weeks
    /// without a test noticing. See the module docs.
    #[test]
    fn notification_text_cannot_carry_markup() {
        assert_eq!(
            escape_markup(r#"<img src="file:///etc/passwd"> & <a href="x">pay</a>"#),
            "&lt;img src=\"file:///etc/passwd\"&gt; &amp; &lt;a href=\"x\"&gt;pay&lt;/a&gt;"
        );
        assert_eq!(
            escape_markup("Lunch at 12:30 \u{2014} caf\u{e9}"),
            "Lunch at 12:30 \u{2014} caf\u{e9}"
        );
        // The ampersand first, or the others would be escaped twice.
        assert_eq!(escape_markup("&lt;"), "&amp;lt;");
    }

    #[test]
    fn nothing_else_posts_through_the_plugin() {
        fn walk(dir: &std::path::Path, offenders: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, offenders);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|n| n != "notice.rs")
                    && std::fs::read_to_string(&path).unwrap().contains(".notification()")
                {
                    offenders.push(path.display().to_string());
                }
            }
        }
        let mut offenders = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut offenders);
        assert_eq!(offenders, Vec::<String>::new(), "use `notice::show`");
    }

    /// A notification that is not an alarm asks for nothing it did not
    /// before; an alarm is critical and has the alarm sound.
    #[test]
    fn only_an_alarm_is_critical_and_has_a_sound() {
        assert_eq!(Alert::new(false), Alert { urgency: 1, sound: None });
        assert_eq!(Alert::new(true), Alert { urgency: 2, sound: Some("alarm-clock-elapsed") });
    }

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
                Some("/home/sam/.local/share/applications/my-jatayu.desktop"),
                "jatayu-app"
            ),
            "my-jatayu"
        );
        assert_eq!(desktop_entry_name(None, "jatayu-app"), "jatayu-app");
        assert_eq!(desktop_entry_name(Some("/usr/bin/jatayu-app"), "jatayu-app"), "jatayu-app");
    }
}
