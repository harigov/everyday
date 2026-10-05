//! The number on the application's icon: the Dock's badge on macOS, the
//! launcher's count on Linux, an overlay on the taskbar button on Windows.
//!
//! What it counts is the person's choice, made in Settings and off by
//! default -- `ui/src/lib/badge.svelte.ts` keeps the choice, and
//! `commands::set_badge` hands it here. Everything chosen is added up into
//! one number: an icon has room for one, and the question it answers is "how
//! much is waiting for me", not "how much of which kind".
//!
//! Counted here rather than sent by the interface, for the reason the tray's
//! meeting line is (see `tray.rs`): the window can be hidden with the process
//! still resident, and mail arriving at three in the afternoon has to reach
//! the icon whether or not a webview is awake to count it. The same three
//! things keep it current as keep that line current: a change the service
//! announces (`events::WindowSink::changed`), a lock or an unlock
//! (`events::WindowSink::lock_state`), and `lib.rs`'s once-a-minute timer --
//! which is also what notices "due today" rolling over at midnight, and mail
//! a background sync wrote without announcing it.
//!
//! # Each count is one already on screen
//!
//! A badge that disagreed with the app it opens would be worse than none, so
//! every source is defined as a number the interface already draws:
//!
//! - **Tasks due today**: `TaskStats::due_today`, beside "Today" in the todo
//!   sidebar -- open tasks due today or already overdue.
//! - **Every open task**: `TaskStats::open_tasks`, beside "All tasks".
//! - **Unread mail**: unread messages in each mail account's inbox, leaving
//!   out threads snoozed away from it -- the mail sidebar's Inbox rows. See
//!   [`unread_inbox`].
//! - **The assistant**: unseen runs plus unseen proposals, the number on the
//!   assistant's button in the app bar.
//!
//! # Three platforms, three mechanisms
//!
//! macOS has the real thing, `NSDockTile`'s badge, and Tauri's
//! `set_badge_count` drives it. Windows gives a desktop application no count
//! at all, only an overlay icon on its taskbar button, so the number is drawn
//! here into a small image ([`overlay_rgba`]) -- the way chat clients on
//! Windows do it. Linux has the `com.canonical.Unity.LauncherEntry` D-Bus
//! signal, which Ubuntu's dock, Dash to Dock, KDE Plasma's task manager and
//! Plank listen for; see [`desktop_ids`] for why this sends it itself rather
//! than through Tauri.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use everyday_core::model::today_local;
use everyday_core::store::mail::ThreadFilter;
use everyday_core::{MailboxRole, Vault};
use everyday_service::Service;
use serde::Deserialize;
use tauri::{AppHandle, Manager};

/// Which tasks the badge counts, if any.
///
/// One three-way choice rather than two switches, because the tasks due
/// today are a subset of the open ones: with both switched on, every task
/// due today would be counted twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskCount {
    #[default]
    Off,
    /// Open tasks due today or already overdue.
    Due,
    /// Every open task.
    Open,
}

/// What the badge adds up, as Settings sends it. Everything off -- the
/// default -- means no badge at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BadgeSources {
    pub tasks: TaskCount,
    pub mail: bool,
    pub assistant: bool,
}

impl BadgeSources {
    fn any(&self) -> bool {
        self.tasks != TaskCount::Off || self.mail || self.assistant
    }
}

/// The badge's choice of sources, and what it last put on the icon.
#[derive(Default)]
pub struct Badge {
    sources: Mutex<BadgeSources>,
    /// The number on the icon now, `None` before the first refresh.
    ///
    /// Held for the whole of [`Badge::refresh`], which is what stops two
    /// refreshes -- a scheduled one and a command's -- from interleaving and
    /// leaving the older count up.
    shown: Mutex<Option<u64>>,
    /// Put the number on the icon again even if it has not changed. Set by
    /// the minute's tick, so anything that lost the badge out from under us
    /// gets it back within a minute: a dock that restarted, or a Windows
    /// taskbar button rebuilt when a hidden window is shown again.
    reannounce: AtomicBool,
    /// Set while a [`Badge::schedule_refresh`] is queued and has not started,
    /// so a burst of changes -- a sync touching forty threads -- costs one
    /// recount rather than forty.
    refresh_queued: AtomicBool,
    /// The session bus, opened on first use and then kept for the life of
    /// the process. A dock forgets a badge when the connection that sent it
    /// goes away -- which is what clears it when Every Day quits -- so a
    /// connection per signal would take the badge down as soon as it went
    /// up.
    #[cfg(target_os = "linux")]
    bus: Mutex<Option<zbus::blocking::Connection>>,
}

impl Badge {
    /// Replace what the badge counts, and show the result at once. The one
    /// caller is `commands::set_badge`, which runs this off the async
    /// runtime: it reads the vault.
    pub fn set_sources(&self, app: &AppHandle, sources: BadgeSources) {
        *self.sources.lock().unwrap() = sources;
        self.refresh(app);
    }

    /// Recount for a change the service announced, or a lock, off the
    /// caller's thread -- the event sink runs on the async runtime's
    /// workers -- and absorbed by a recount already queued.
    ///
    /// Nothing at all with no source chosen: the badge was cleared when the
    /// last one was switched off, and a lock has nothing to take off it.
    pub fn schedule_refresh(&self, app: &AppHandle) {
        if !self.sources.lock().unwrap().any() {
            return;
        }
        if self.refresh_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let badge = app.state::<Badge>();
            // Cleared before counting, so a change that lands mid-count
            // queues another rather than being lost.
            badge.refresh_queued.store(false, Ordering::Release);
            badge.refresh(&app);
        });
    }

    /// The minute's tick: recount, and put the number back on the icon even
    /// if it has not changed. See [`Badge::reannounce`].
    pub fn tick(&self, app: &AppHandle) {
        if !self.sources.lock().unwrap().any() {
            return;
        }
        self.reannounce.store(true, Ordering::Release);
        self.schedule_refresh(app);
    }

    fn refresh(&self, app: &AppHandle) {
        let mut shown = self.shown.lock().unwrap();
        let sources = *self.sources.lock().unwrap();
        let n = match app.try_state::<crate::state::AppState>() {
            Some(state) => count(&state.service(), sources),
            None => 0,
        };
        let again = self.reannounce.swap(false, Ordering::AcqRel);
        if *shown == Some(n) && !again {
            return;
        }
        self.apply(app, n);
        *shown = Some(n);
    }

    /// Put `count` on the icon, or take the badge off for zero.
    ///
    /// A failure is logged and otherwise ignored: the badge is a convenience,
    /// and nothing about the vault depends on it.
    fn apply(&self, app: &AppHandle, count: u64) {
        #[cfg(target_os = "macos")]
        if let Some(window) = app.get_webview_window("main") {
            let n = (count > 0).then(|| i64::try_from(count).unwrap_or(i64::MAX));
            if let Err(e) = window.set_badge_count(n) {
                tracing::warn!(error = %e, "could not set the Dock badge");
            }
        }

        #[cfg(target_os = "windows")]
        if let Some(window) = app.get_webview_window("main") {
            let icon = label(count).map(|text| {
                tauri::image::Image::new_owned(overlay_rgba(&text), OVERLAY_SIZE, OVERLAY_SIZE)
            });
            if let Err(e) = window.set_overlay_icon(icon) {
                tracing::warn!(error = %e, "could not set the taskbar badge");
            }
        }

        #[cfg(target_os = "linux")]
        self.announce(app, count);
    }

    /// Send the LauncherEntry `Update` signal for every desktop file id this
    /// process might be known by. See [`desktop_ids`].
    #[cfg(target_os = "linux")]
    fn announce(&self, app: &AppHandle, count: u64) {
        use std::collections::HashMap;
        use zbus::zvariant::Value;

        let mut bus = self.bus.lock().unwrap();
        if bus.is_none() {
            match zbus::blocking::Connection::session() {
                Ok(connection) => *bus = Some(connection),
                Err(e) => {
                    tracing::debug!(error = %e, "no session bus to put a launcher badge on");
                    return;
                }
            }
        }
        let Some(connection) = bus.as_ref() else {
            return;
        };

        let info = app.package_info();
        let launched = std::env::var("GIO_LAUNCHED_DESKTOP_FILE").ok();
        for id in desktop_ids(launched.as_deref(), &info.name, info.crate_name) {
            let properties: HashMap<&str, Value> = HashMap::from([
                ("count", Value::from(i64::try_from(count).unwrap_or(i64::MAX))),
                ("count-visible", Value::from(count > 0)),
            ]);
            let body = (format!("application://{id}"), properties);
            if let Err(e) = connection.emit_signal(
                None::<&str>,
                LAUNCHER_PATH,
                LAUNCHER_INTERFACE,
                "Update",
                &body,
            ) {
                tracing::debug!(error = %e, "could not announce the launcher badge");
                // Dropped, so the next refresh opens a fresh connection
                // rather than failing on this one forever.
                *bus = None;
                return;
            }
        }
    }
}

/// What the badge should say: `sources` added up, or 0 -- no badge -- with
/// nothing chosen, no vault open, or the vault locked.
fn count(service: &Service, sources: BadgeSources) -> u64 {
    if !sources.any() {
        return 0;
    }
    let Some(vault) = service.get() else {
        return 0;
    };
    if !vault.is_unlocked() {
        return 0;
    }

    let mut total = 0;
    if vault.supports_tasks() {
        total += match sources.tasks {
            TaskCount::Off => 0,
            TaskCount::Due => read("tasks", vault.task_stats(today_local()).map(|s| s.due_today)),
            TaskCount::Open => read("tasks", vault.task_stats(today_local()).map(|s| s.open_tasks)),
        };
    }
    if sources.mail && vault.supports_mail() && vault.supports_accounts() {
        total += unread_inbox(service, &vault);
    }
    if sources.assistant {
        if vault.supports_routines() {
            total += read("runs", vault.unseen_runs());
        }
        if vault.supports_proposals() {
            total += read("proposals", vault.unseen_proposals());
        }
    }
    total
}

/// A count, or nothing for a source that could not be read. One domain
/// failing to answer must not take the rest of the number with it, and must
/// not put an error anywhere a person would see it.
fn read(what: &str, n: everyday_core::Result<u64>) -> u64 {
    n.unwrap_or_else(|e| {
        tracing::debug!(error = %e, what, "the badge could not count this");
        0
    })
}

/// How many of an inbox's snoozed threads [`unread_inbox`] reads, at most.
const SNOOZED_PAGE: u32 = 200;

/// Unread messages in every mail account's inbox, leaving out threads
/// snoozed away from it.
///
/// Two reads per inbox rather than one list of its unread threads: the
/// service's unread total (a clear-column `SUM`, cached until the next mail
/// write -- see `mailsync::unread_cache`), less what the inbox's snoozed
/// threads hold. The first is exact and costs nothing between writes; the
/// second decrypts only the snoozed threads, which are a handful, where
/// listing every unread thread would decrypt all of them once a minute.
///
/// The sidebar leaves snoozed threads out of the Inbox row's number -- see
/// `mail.svelte.ts`'s `refreshUnreadCounts` -- and so does this.
fn unread_inbox(service: &Service, vault: &Vault) -> u64 {
    let accounts = match vault.accounts() {
        Ok(accounts) => accounts,
        Err(e) => {
            tracing::debug!(error = %e, "the badge could not list mail accounts");
            return 0;
        }
    };
    let snoozed = ThreadFilter { unread: Some(true), snoozed: Some(true), ..Default::default() };

    let mut total = 0;
    for account in accounts.iter().filter(|a| a.services.mail) {
        let Ok(mailboxes) = vault.mailboxes(account.id) else {
            continue;
        };
        let inboxes: Vec<_> =
            mailboxes.iter().filter(|m| m.role == MailboxRole::Inbox).map(|m| m.id).collect();
        if inboxes.is_empty() {
            continue;
        }
        // Through the service's cache once mail is open, and straight from
        // the vault in the moment after an unlock before it is.
        let counts = match service.mail_unread_counts(account.id) {
            Ok(counts) => counts,
            Err(_) => match vault.mail_unread_counts(account.id) {
                Ok(counts) => counts,
                Err(e) => {
                    tracing::debug!(error = %e, "the badge could not count unread mail");
                    continue;
                }
            },
        };
        for inbox in inboxes {
            let unread: u64 = counts.iter().filter(|(id, _)| *id == inbox).map(|(_, n)| n).sum();
            let held: u64 = vault
                .list_threads(inbox, &snoozed, None, SNOOZED_PAGE)
                .map(|page| page.threads.iter().map(|t| u64::from(t.unread_count)).sum())
                .unwrap_or(0);
            total += unread.saturating_sub(held);
        }
    }
    total
}

// ---- Linux: the LauncherEntry signal ---------------------------------------

#[cfg(target_os = "linux")]
const LAUNCHER_PATH: &str = "/app/everyday/LauncherEntry";
#[cfg(target_os = "linux")]
const LAUNCHER_INTERFACE: &str = "com.canonical.Unity.LauncherEntry";

/// The desktop file ids this process might be known to a dock by. All of
/// them are told: a dock ignores an id it has no icon for.
///
/// There are three ways Every Day gets onto a desktop, and each names its
/// desktop file differently: whatever launched it, if the launcher said so
/// (`GIO_LAUNCHED_DESKTOP_FILE`, which GLib sets for anything started through
/// a `GDesktopAppInfo` -- GNOME Shell's launchers among them); the `.deb`,
/// whose file `tauri-bundler` names after the product (`Every Day.desktop`);
/// and `make desktop-entry`, which names it after the binary
/// (`everyday-app.desktop`) for a build run from the checkout.
///
/// Tauri's own `set_badge_count` names only the second, which is one reason
/// this sends the signal itself. The other is that Tauri sends it through
/// libunity, loaded at run time, which few distributions besides Ubuntu
/// install. The signal needs neither: it is one D-Bus message.
#[cfg(any(target_os = "linux", test))]
fn desktop_ids(launched: Option<&str>, product: &str, binary: &str) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(name) =
        launched.and_then(|path| std::path::Path::new(path).file_name()).and_then(|n| n.to_str())
        && name.ends_with(".desktop")
    {
        ids.push(name.to_string());
    }
    for id in [format!("{product}.desktop"), format!("{binary}.desktop")] {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

// ---- Windows: drawing the number --------------------------------------------

/// The side of the overlay image, in pixels. Windows shows an overlay at 16
/// logical pixels; drawing at twice that keeps it sharp on a high-density
/// screen, and the glyphs below are bold enough to survive being halved.
#[cfg(any(target_os = "windows", test))]
const OVERLAY_SIZE: u32 = 32;

/// `--accent` from `ui/src/styles/theme.css`, the app bar badge's own
/// colour, so the taskbar's badge is the same as the one inside the window.
#[cfg(any(target_os = "windows", test))]
const BADGE_RGB: [u8; 3] = [0xb4, 0x53, 0x0f];

/// The text an overlay carries: the count up to nine, then "9+" -- the app
/// bar's own rule (`AppBar.svelte`), and all a 16-pixel disc has room for.
#[cfg(any(target_os = "windows", test))]
fn label(count: u64) -> Option<String> {
    match count {
        0 => None,
        1..=9 => Some(count.to_string()),
        _ => Some("9+".into()),
    }
}

/// Digits and "+" on a 3x5 grid, one row per byte, leftmost pixel in the
/// highest of the three bits. Drawn at a whole multiple of a pixel, so every
/// stroke stays a solid square however far it is scaled.
#[cfg(any(target_os = "windows", test))]
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        _ => return None,
    })
}

/// `text` in white on an accent-coloured disc, as [`OVERLAY_SIZE`]-square
/// straight (not premultiplied) RGBA -- what `tauri::image::Image` takes.
#[cfg(any(target_os = "windows", test))]
fn overlay_rgba(text: &str) -> Vec<u8> {
    let size = OVERLAY_SIZE as usize;
    let mut px = vec![0u8; size * size * 4];

    // The disc, its edge anti-aliased by how much of each pixel it covers.
    let radius = OVERLAY_SIZE as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - radius;
            let dy = y as f32 + 0.5 - radius;
            let cover = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            let i = (y * size + x) * 4;
            px[i..i + 3].copy_from_slice(&BADGE_RGB);
            px[i + 3] = (cover * 255.0).round() as u8;
        }
    }

    // The text, centred. Larger for one character than for two, which have
    // to share the disc; one font pixel of space between characters.
    let glyphs: Vec<[u8; 5]> = text.chars().filter_map(glyph).collect();
    let scale = if glyphs.len() > 1 { 3 } else { 4 };
    let advance = 4 * scale;
    let width = (glyphs.len() * advance).saturating_sub(scale);
    let left = size.saturating_sub(width) / 2;
    let top = size.saturating_sub(5 * scale) / 2;
    for (n, rows) in glyphs.iter().enumerate() {
        for (row, bits) in rows.iter().enumerate() {
            for col in (0..3).filter(|col| bits & (0b100 >> col) != 0) {
                let x0 = left + n * advance + col * scale;
                let y0 = top + row * scale;
                for y in y0..(y0 + scale).min(size) {
                    for x in x0..(x0 + scale).min(size) {
                        let i = (y * size + x) * 4;
                        px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::task::{Task, TaskStatus};
    use everyday_core::{VaultConfig, crypto::KdfParams};
    use jiff::ToSpan;
    use std::sync::Arc;

    // ---- the choice, as Settings sends it ----------------------------------

    #[test]
    fn nothing_chosen_is_the_default_and_counts_nothing() {
        let sources: BadgeSources = serde_json::from_str("{}").unwrap();
        assert_eq!(sources, BadgeSources::default());
        assert!(!sources.any());
    }

    #[test]
    fn the_interfaces_shape_parses() {
        let sources: BadgeSources =
            serde_json::from_str(r#"{"tasks":"due","mail":true,"assistant":false}"#).unwrap();
        assert_eq!(sources, BadgeSources { tasks: TaskCount::Due, mail: true, assistant: false });
        assert!(sources.any());
    }

    // ---- counting against a real vault -------------------------------------

    /// A fresh encrypted vault in a temporary directory, behind a service.
    /// The directory goes with the returned guard.
    fn vault() -> (tempfile::TempDir, Arc<Service>, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            &dir.path().join("vault"),
            VaultConfig {
                password: Some("badge".into()),
                kdf: KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let service = Arc::new(Service::new());
        let vault = service.set(vault);
        (dir, service, vault)
    }

    fn task(title: &str, due: Option<jiff::civil::Date>, status: TaskStatus) -> Task {
        let mut task = Task::new(title);
        task.due_date = due;
        task.status = status;
        task
    }

    fn tasks_only(tasks: TaskCount) -> BadgeSources {
        BadgeSources { tasks, ..Default::default() }
    }

    #[test]
    fn tasks_count_what_the_sidebar_shows() {
        let (_dir, service, vault) = vault();
        let today = today_local();
        vault
            .save_tasks(&[
                task("overdue", Some(today - 2.days()), TaskStatus::Todo),
                task("due today", Some(today), TaskStatus::Doing),
                task("next week", Some(today + 7.days()), TaskStatus::Todo),
                task("someday", None, TaskStatus::Backlog),
                task("finished", Some(today), TaskStatus::Done),
            ])
            .unwrap();

        // "Today": the overdue one and today's, not the finished one.
        assert_eq!(count(&service, tasks_only(TaskCount::Due)), 2);
        // "All tasks": every one still open.
        assert_eq!(count(&service, tasks_only(TaskCount::Open)), 4);
        assert_eq!(count(&service, tasks_only(TaskCount::Off)), 0);
    }

    #[test]
    fn sources_are_added_together() {
        let (_dir, service, vault) = vault();
        vault.save_tasks(&[task("due today", Some(today_local()), TaskStatus::Todo)]).unwrap();
        // A vault with no mail account and an assistant that has done
        // nothing adds nothing to the task, and does not fail to.
        let all = BadgeSources { tasks: TaskCount::Due, mail: true, assistant: true };
        assert_eq!(count(&service, all), 1);
    }

    #[test]
    fn a_locked_vault_shows_no_badge() {
        let (_dir, service, vault) = vault();
        vault.save_tasks(&[task("due today", Some(today_local()), TaskStatus::Todo)]).unwrap();
        vault.lock();
        assert_eq!(count(&service, tasks_only(TaskCount::Due)), 0);
    }

    #[test]
    fn no_vault_shows_no_badge() {
        let service = Arc::new(Service::new());
        let all = BadgeSources { tasks: TaskCount::Open, mail: true, assistant: true };
        assert_eq!(count(&service, all), 0);
    }

    // ---- Linux's desktop file ids --------------------------------------------

    #[test]
    fn both_installed_names_are_told_when_the_launcher_said_nothing() {
        assert_eq!(
            desktop_ids(None, "Every Day", "everyday-app"),
            ["Every Day.desktop", "everyday-app.desktop"],
        );
    }

    #[test]
    fn the_launching_desktop_file_comes_first_and_is_not_repeated() {
        assert_eq!(
            desktop_ids(
                Some("/usr/share/applications/Every Day.desktop"),
                "Every Day",
                "everyday-app"
            ),
            ["Every Day.desktop", "everyday-app.desktop"],
        );
        assert_eq!(
            desktop_ids(
                Some("/home/sam/.local/share/applications/my-everyday.desktop"),
                "Every Day",
                "everyday-app"
            ),
            ["my-everyday.desktop", "Every Day.desktop", "everyday-app.desktop"],
        );
    }

    #[test]
    fn a_launch_path_that_is_not_a_desktop_file_is_ignored() {
        assert_eq!(
            desktop_ids(Some("/usr/bin/everyday-app"), "Every Day", "everyday-app"),
            ["Every Day.desktop", "everyday-app.desktop"],
        );
    }

    // ---- Windows's drawn overlay ----------------------------------------------

    #[test]
    fn the_label_stops_at_nine_like_the_app_bar() {
        assert_eq!(label(0), None);
        assert_eq!(label(1).as_deref(), Some("1"));
        assert_eq!(label(9).as_deref(), Some("9"));
        assert_eq!(label(10).as_deref(), Some("9+"));
        assert_eq!(label(u64::MAX).as_deref(), Some("9+"));
    }

    fn alpha(px: &[u8], x: usize, y: usize) -> u8 {
        px[(y * OVERLAY_SIZE as usize + x) * 4 + 3]
    }

    fn white(px: &[u8]) -> Vec<(usize, usize)> {
        let size = OVERLAY_SIZE as usize;
        (0..size * size)
            .filter(|i| px[i * 4..i * 4 + 4] == [255, 255, 255, 255])
            .map(|i| (i % size, i / size))
            .collect()
    }

    #[test]
    fn the_overlay_is_a_disc_in_a_transparent_square() {
        let px = overlay_rgba("1");
        let size = OVERLAY_SIZE as usize;
        assert_eq!(px.len(), size * size * 4);
        assert_eq!(alpha(&px, 0, 0), 0);
        assert_eq!(alpha(&px, size - 1, size - 1), 0);
        assert_eq!(alpha(&px, size / 2, size / 2), 255);
    }

    #[test]
    fn a_single_digit_is_drawn_at_four_times_its_grid() {
        // "1" has eight pixels set on its 3x5 grid, each drawn 4x4.
        assert_eq!(white(&overlay_rgba("1")).len(), 8 * 16);
    }

    #[test]
    fn two_characters_stay_inside_the_disc() {
        let radius = OVERLAY_SIZE as f32 / 2.0;
        let drawn = white(&overlay_rgba("9+"));
        // "9" has twelve pixels set and "+" five, each drawn 3x3.
        assert_eq!(drawn.len(), (12 + 5) * 9);
        for (x, y) in drawn {
            let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
            assert!((dx * dx + dy * dy).sqrt() < radius - 1.0, "({x}, {y}) spills off the disc");
        }
    }
}
