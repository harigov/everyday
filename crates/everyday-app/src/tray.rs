//! The tray icon and its quick-action menu.
//!
//! The shell owns the icon; the interface owns what is in the menu. That
//! split is the whole design. A quick action is "add a task", "start an
//! entry" -- things that are defined by an app inside the window, know
//! whether they are currently possible, and have to run where the state
//! lives. Hard-coding them here would mean a Rust change, a capability
//! change and a rebuild every time one of the apps in the window grew
//! another thing worth doing from the menu bar -- which is exactly what
//! adding the library did, and it cost this file nothing.
//!
//! So the interface sends a menu *description* ([`TrayItem`]) and the shell
//! renders it. Choosing an item emits [`TRAY_ACTION`] carrying the item's
//! id, and the interface runs the handler it registered under that id. See
//! `ui/src/lib/tray.svelte.ts` for the other half, which is where a new
//! action is actually added.
//!
//! Two items are *not* the interface's to supply: the shell appends "Open"
//! and "Quit" itself, below a separator. A tray whose only way back to the
//! application is a menu built by a webview that might be wedged is a way
//! to lose a running program, and on Linux -- where a click on the icon
//! raises no event at all -- it is the only way.
//!
//! The icon itself is built at most once per process and hidden rather than
//! destroyed. Both handlers below are registered on the `Builder` instead of
//! on the icon, for the same reason: see [`Tray::hide`].

use everyday_core::calendar::{Calendar, Event, EventStatus};
use everyday_core::meeting::detect;
use everyday_core::store::calendars::EventQuery;
use jiff::{Span, Timestamp, tz::TimeZone};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

use everyday_service::error::{CommandError, CommandResult};

/// Emitted with the id of the menu item that was chosen.
pub const TRAY_ACTION: &str = "everyday://tray-action";

/// Namespace for the items the shell adds itself. Ids from the interface may
/// not use it, so a quick action can never shadow "Quit".
const RESERVED: &str = "everyday:";
const SHOW_ID: &str = "everyday:show";
const QUIT_ID: &str = "everyday:quit";
const TRAY_ID: &str = "everyday";

/// Chosen by the shell, not the interface -- see [`Tray::set_recording`].
pub const STOP_RECORDING_ID: &str = "everyday:stop-recording";

/// The optional "current/next meeting" line -- see [`meeting_line`] -- and
/// its "Join" item, both chosen by the shell for the same reason
/// `STOP_RECORDING_ID` is: there is no quick action behind either, so there
/// is nothing for the interface to have registered a handler for. Handled
/// directly in [`on_menu_event`].
const OPEN_MEETING_ID: &str = "everyday:open-meeting";
const JOIN_MEETING_ID: &str = "everyday:join-meeting";

/// Raised when [`OPEN_MEETING_ID`] is chosen, after the window is already
/// raised. The interface's own half -- switching to the calendar app -- is
/// `ui/src/lib/tray.svelte.ts`'s `onOpenCalendar` handler; the shell does
/// not reach into the interface's app-section state itself, the same
/// division of labour [`TRAY_ACTION`] keeps for every other click.
pub const OPEN_CALENDAR_EVENT: &str = "everyday://open-calendar";

/// What the assistant is up to, in a line, or nothing to say.
///
/// Nothing to say is the ordinary case: a vault with no routines on it has no
/// assistant to report on, and a menu that said "Assistant: off" to everybody
/// would be an advertisement rather than a status.
///
/// Built here rather than sent by the interface for the reason the whole
/// feature exists: the window may be hidden, and its stores dropped, at
/// exactly the moment somebody opens this menu to ask what the process is
/// still doing.
fn assistant_line(app: &AppHandle) -> Option<String> {
    let state = app.try_state::<crate::state::AppState>()?;
    let service = state.service();
    if let Some(name) = service.running_routine() {
        return Some(format!("Assistant: running \u{201c}{name}\u{201d}"));
    }
    let vault = service.get()?;
    if !vault.is_unlocked() {
        // Worth saying, because it is the answer to "why did nothing happen
        // this morning".
        return Some("Assistant: waiting for the password".into());
    }
    if !vault.supports_routines() {
        return None;
    }
    let routines = vault.routines().ok()?;
    let live = routines.iter().filter(|r| r.enabled).count();
    match (routines.len(), live) {
        (0, _) => None,
        (_, 0) => Some("Assistant: every routine is switched off".into()),
        (_, 1) => Some("Assistant: idle, 1 routine".into()),
        (_, n) => Some(format!("Assistant: idle, {n} routines")),
    }
}

// ---- the optional "current/next meeting" line ----------------------------
//
// Off by default -- see `ui/src/components/SettingsView.svelte`'s second
// tray toggle -- and computed here rather than sent by the interface, for
// the same reason `assistant_line` is: the window may be hidden, and what
// this says has to keep up with the clock and with a calendar sync neither
// of which the interface is necessarily awake to drive. See
// `Tray::refresh`, `lib.rs`'s once-a-minute timer, and `events.rs`'s own
// calls on a calendar change or a lock.

/// How far ahead an upcoming meeting is still worth naming in the tray.
///
/// Three hours: long enough that a glance at the tray mid-morning still
/// shows something useful about the day, short enough that what it names is
/// never so far off that "next meeting" reads as stale noise for most of
/// the afternoon. `pick_meeting` caps this further at the end of the local
/// day, so a meeting after midnight does not show up as "next" at eleven
/// at night.
const MEETING_HORIZON_SECS: i64 = 3 * 60 * 60;

/// Below this many minutes away, the line counts down ("in 12 min") rather
/// than naming a clock time -- the point past which "soon" stops being more
/// useful than "at".
const SOON_MINUTES: i64 = 60;

/// The tray title's longest readable length, in characters. A meeting title
/// is free text a calendar server handed us, and the menu bar is not a
/// place to let one run on -- see [`truncate_chars`].
const TRAY_TITLE_MAX_CHARS: usize = 40;

/// The current or next meeting worth naming in the tray, computed fresh on
/// every [`Tray::rebuild`] -- see this section's own doc for why nothing
/// here is cached across calls.
#[derive(Debug, Clone, PartialEq)]
struct MeetingLine {
    /// "Standup · in 12 min" -- used for the title (truncated, see
    /// [`TRAY_TITLE_MAX_CHARS`]), the tooltip and the first menu entry.
    text: String,
    /// The event's own title, unembellished, for the "Join" item's label.
    title: String,
    /// A meeting-host link found on the event, if any -- see
    /// [`detect::join_link`]. `Some` is what adds the "Join" item.
    join_url: Option<String>,
}

/// Does `event`, on `calendar`, belong in the tray's reckoning at all?
///
/// All-day: carries no "now" to be in progress or imminent about. Cancelled:
/// "the meeting was cancelled" is drawn struck through on the grid, not
/// announced in the menu bar. Free (`!busy` -- the feed's own "transparent"
/// flag): marked as not a clash by whoever made it, so it is not treated as
/// one here either. A calendar unticked from the grid (`Calendar::visible`):
/// hidden there for a reason, and the tray keeps it hidden rather than being
/// the one place it still shows through.
///
/// What this does *not* check, because nothing synced from a calendar feed
/// carries it: whether the vault's own owner declined the invitation.
/// [`Event::attendees`] is free text copied from the feed, with no
/// per-attendee RSVP captured -- see
/// `everyday_core::mail::records::AttendeeResponse` for the one place this
/// vault *does* track a decline, which is an invite read over mail, not a
/// synced calendar event.
fn event_is_showable(event: &Event, calendar: &Calendar) -> bool {
    !event.all_day && event.status != EventStatus::Cancelled && event.busy && calendar.visible
}

/// Choose which of `events` -- already filtered to [`event_is_showable`] --
/// belongs in the tray: the one in progress, or else the one starting
/// soonest within [`MEETING_HORIZON_SECS`] of `now`, capped at
/// `end_of_day`.
///
/// Not assumed sorted: this finds the earliest start among whichever
/// candidates qualify, which is what matters when two meetings overlap --
/// back-to-back calls, a double-booking -- so the one that started first,
/// and so has been running longer, is the one named.
fn pick_meeting(events: &[Event], now: Timestamp, end_of_day: Timestamp) -> Option<&Event> {
    let now_s = now.as_second();
    let horizon_s = (now_s + MEETING_HORIZON_SECS).min(end_of_day.as_second());

    let in_progress = events
        .iter()
        .filter(|e| e.start.as_second() <= now_s && now_s < e.end.as_second())
        .min_by_key(|e| e.start.as_second());
    if in_progress.is_some() {
        return in_progress;
    }

    events
        .iter()
        .filter(|e| e.start.as_second() > now_s && e.start.as_second() <= horizon_s)
        .min_by_key(|e| e.start.as_second())
}

/// "Standup · in 12 min", "Design review · now, until 11:30", "1:1 with
/// Sam · 14:00" -- the tray's own vocabulary for when a meeting is.
///
/// Always 24-hour. There is no crate in this build that reads the desktop's
/// locale or its 12/24-hour preference -- `ui/src/lib/format.ts`'s
/// `Intl.DateTimeFormat` is a browser API the shell has no equivalent of --
/// and a clock written one way on every machine is simpler to get right
/// than one that would otherwise have to guess. `tz` is the system zone,
/// passed in rather than read here so this stays pure and testable; see
/// `meeting_line`, its one caller.
fn format_meeting_text(event: &Event, now: Timestamp, tz: &TimeZone) -> String {
    if event.start.as_second() <= now.as_second() {
        let until = event.end.to_zoned(tz.clone()).strftime("%H:%M");
        return format!("{} \u{b7} now, until {until}", event.title);
    }
    // Rounded up, so a meeting 30 seconds away reads "in 1 min" rather than
    // "in 0 min". By hand: `div_ceil` on a signed integer is not stable.
    let minutes = ((event.start.as_second() - now.as_second() + 59) / 60).max(1);
    if minutes < SOON_MINUTES {
        format!("{} \u{b7} in {minutes} min", event.title)
    } else {
        let at = event.start.to_zoned(tz.clone()).strftime("%H:%M");
        format!("{} \u{b7} {at}", event.title)
    }
}

/// Midnight at the start of the day after `day`, in `zone` -- the cap
/// `pick_meeting` applies to [`MEETING_HORIZON_SECS`] so a meeting after
/// midnight is never named as "next" late in the evening before it. `None`
/// only if `zone` cannot place a date at all, which does not happen for
/// [`TimeZone::system`].
fn end_of_local_day(day: jiff::civil::Date, zone: &TimeZone) -> Option<Timestamp> {
    Some(day.tomorrow().ok()?.to_zoned(zone.clone()).ok()?.timestamp())
}

/// `s`, cut to at most `max` characters, with an ellipsis standing in for
/// whatever was cut. Counts characters rather than bytes, and cuts on a
/// character boundary, because a meeting title is free text a calendar
/// server handed us and may hold anything UTF-8 allows.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// The current or next meeting worth naming in the tray, or nothing to
/// say.
///
/// `None` while the vault is locked (checked before anything else is read,
/// so a locked vault never gets as far as a title worth clearing a moment
/// later -- see [`Tray::rebuild`]'s own privacy note), while its backend
/// has no calendar domain, or while there is nothing to report.
fn meeting_line(app: &AppHandle) -> Option<MeetingLine> {
    let state = app.try_state::<crate::state::AppState>()?;
    let service = state.service();
    let vault = service.get()?;
    if !vault.is_unlocked() || !vault.supports_calendars() {
        return None;
    }

    let now = Timestamp::now();
    let zone = TimeZone::system();
    let today = now.to_zoned(zone.clone()).date();
    let end_of_day = end_of_local_day(today, &zone).unwrap_or_else(|| {
        Timestamp::from_second(now.as_second() + MEETING_HORIZON_SECS).unwrap_or(now)
    });

    let calendars = vault.calendars().ok()?;
    // A day either side of today, the same margin `meeting::watch::tick_inner`
    // gives its own query: an event starting just after midnight, looked at
    // just before it, must not fall outside the window.
    let events = vault
        .events(&EventQuery {
            from: today.checked_sub(Span::new().days(1)).ok(),
            to: today.checked_add(Span::new().days(1)).ok(),
            ..Default::default()
        })
        .ok()?;

    let candidates: Vec<Event> = events
        .into_iter()
        .filter(|e| {
            calendars
                .iter()
                .find(|c| c.id == e.calendar_id)
                .is_some_and(|c| event_is_showable(e, c))
        })
        .collect();
    let chosen = pick_meeting(&candidates, now, end_of_day)?;

    Some(MeetingLine {
        text: format_meeting_text(chosen, now, &zone),
        title: chosen.title.clone(),
        join_url: detect::join_link(chosen),
    })
}

/// One entry in the tray menu, as the interface describes it.
///
/// Deliberately a small language rather than a general one: an item that
/// does something, a rule between groups, and a submenu for an app with
/// more than a couple of actions. No icons -- a tray menu is read as a list
/// of verbs -- and no accelerators, because a menu-bar accelerator is not
/// registered globally on any of the three platforms, so displaying one
/// would advertise a shortcut that does nothing unless the window already
/// has focus.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TrayItem {
    Action {
        id: String,
        label: String,
        enabled: bool,
        /// `Some` renders a checkbox -- for an action that is also a state,
        /// like a timer that is running.
        #[serde(default)]
        checked: Option<bool>,
        /// Bring the window forward before the handler runs. True for
        /// anything that puts a cursor somewhere; false for "lock now",
        /// which is precisely a thing you do without coming back.
        raise: bool,
    },
    Separator,
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<TrayItem>,
    },
}

/// The tray icon, if one is showing, and what to do with a click on it.
#[derive(Default)]
pub struct Tray {
    icon: Mutex<Option<TrayIcon>>,
    /// Item id to "raise the window first", for the menu currently set.
    raise: Mutex<HashMap<String, bool>>,
    /// Whether an icon is actually on screen right now.
    ///
    /// Not derivable from `icon`, which stays `Some` across a `hide` -- see
    /// that method for why the handle is kept. What reads this is the decision
    /// to keep the process alive with no window: a tray icon is the way back
    /// to a hidden window, and on a desktop that has nowhere to put one there
    /// is no way back at all.
    showing: Mutex<bool>,
    /// The interface's own items from the last `show`, kept so
    /// [`Tray::set_recording`] can rebuild the menu without the interface
    /// having to resend them every time a recording starts or stops.
    last_items: Mutex<Vec<TrayItem>>,
    /// `Some(title)` while a meeting is being recorded. Not the interface's
    /// to set -- see `capture.rs`, which is the one caller -- so it goes
    /// through its own method rather than through [`Tray::show`]'s `items`,
    /// the same way `SHOW_ID`/`QUIT_ID` are the shell's own rather than
    /// something a quick action could collide with.
    recording: Mutex<Option<String>>,
    /// The "show the next meeting" preference -- off by default, like every
    /// `bool` field this struct's `#[derive(Default)]` gives a fresh value.
    /// Set by [`Tray::set_meeting_enabled`], the one caller, which is
    /// `commands::set_tray_meeting`.
    meeting_enabled: Mutex<bool>,
    /// The [`MeetingLine`] the menu currently up was built with, kept so
    /// [`on_menu_event`] can answer "open the calendar" or "join this call"
    /// without recomputing it -- a click can land well after the rebuild
    /// that drew the line it is clicking on, and what it acts on should be
    /// exactly what was on screen, not whatever is true a moment later.
    meeting: Mutex<Option<MeetingLine>>,
    /// Set while a [`Tray::schedule_refresh`] is queued and has not started
    /// yet, so a burst of calendar changes -- a feed sync announces one per
    /// calendar it touched -- costs one rebuild rather than one each.
    refresh_queued: AtomicBool,
}

impl Tray {
    /// Show the tray icon with `items` above the shell's own two entries,
    /// creating the icon if this is the first call.
    ///
    /// Returns `false` when the platform has no tray to put it in -- a Linux
    /// desktop with no StatusNotifier host is the ordinary case -- so the
    /// interface can say so in Settings rather than leaving a switch on that
    /// does nothing.
    pub fn show(&self, app: &AppHandle, items: &[TrayItem]) -> CommandResult<bool> {
        for item in items {
            check_ids(item)?;
        }
        *self.last_items.lock().unwrap() = items.to_vec();
        self.rebuild(app, items)
    }

    /// Put a "Stop recording" item above "Open"/"Quit", and mark the
    /// tooltip and title, while `title` is `Some`; take both away when it
    /// is `None`. Rebuilds around whatever quick actions the interface last
    /// sent, so starting or stopping a recording never depends on the
    /// interface resending its own menu.
    ///
    /// A tray icon this asks to show one that was never shown -- an
    /// automatic recording starting before Settings has ever called `show`
    /// -- builds one with no quick actions rather than staying silent: the
    /// stop button is the one thing in this menu that must always be
    /// reachable while a microphone is live.
    pub fn set_recording(&self, app: &AppHandle, title: Option<&str>) -> CommandResult<bool> {
        *self.recording.lock().unwrap() = title.map(str::to_string);
        let items = self.last_items.lock().unwrap().clone();
        self.rebuild(app, &items)
    }

    fn rebuild(&self, app: &AppHandle, items: &[TrayItem]) -> CommandResult<bool> {
        // Built before either lock is taken. Every menu call here hops to the
        // main thread and blocks until it answers, so holding a lock across
        // one would deadlock against the click handler below -- which runs on
        // that same main thread and wants `raise`.
        let mut raise = HashMap::new();
        let quick = build(app, items, &mut raise)?;

        let separator = PredefinedMenuItem::separator(app).map_err(menu_error)?;
        let meeting_separator = PredefinedMenuItem::separator(app).map_err(menu_error)?;
        let recording = self.recording.lock().unwrap().clone();
        let stop_recording = match &recording {
            Some(title) => Some(
                MenuItem::with_id(
                    app,
                    STOP_RECORDING_ID,
                    format!("Stop recording \u{201c}{title}\u{201d}"),
                    true,
                    NO_ACCEL,
                )
                .map_err(menu_error)?,
            ),
            None => None,
        };
        // Computed fresh every rebuild, same as `assistant_line` below --
        // see this file's "optional 'current/next meeting' line" section.
        // `None` outright when the preference is off, so a vault that
        // happens to be unlocked never has its calendar read for a feature
        // nobody turned on.
        let meeting = if *self.meeting_enabled.lock().unwrap() { meeting_line(app) } else { None };
        *self.meeting.lock().unwrap() = meeting.clone();
        let meeting_item = match &meeting {
            Some(m) => Some(
                MenuItem::with_id(app, OPEN_MEETING_ID, &m.text, true, NO_ACCEL)
                    .map_err(menu_error)?,
            ),
            None => None,
        };
        let join_item = match &meeting {
            Some(m) if m.join_url.is_some() => Some(
                MenuItem::with_id(
                    app,
                    JOIN_MEETING_ID,
                    format!("Join \u{201c}{}\u{201d}", m.title),
                    true,
                    NO_ACCEL,
                )
                .map_err(menu_error)?,
            ),
            _ => None,
        };
        // Disabled, because it is a statement rather than a thing to press.
        let assistant = match assistant_line(app) {
            Some(line) => Some(MenuItem::new(app, line, false, NO_ACCEL).map_err(menu_error)?),
            None => None,
        };
        let show = MenuItem::with_id(app, SHOW_ID, "Open Every Day", true, NO_ACCEL)
            .map_err(menu_error)?;
        let quit = MenuItem::with_id(app, QUIT_ID, "Quit Every Day", true, NO_ACCEL)
            .map_err(menu_error)?;

        // The meeting line and its "Join" sit above everything else,
        // including the interface's own quick actions -- see this struct's
        // module doc, "As the first line of the tray menu". A separator
        // follows only when there is something below to separate them from;
        // `has_quick_items` folds the interface's own items together with
        // "Stop recording" because both are the same kind of thing next to
        // the meeting line, and the trailing separator before
        // assistant/show/quit is skipped when the meeting block already put
        // one there and nothing else did.
        let mut all: Vec<&dyn IsMenuItem<Wry>> = Vec::new();
        let has_meeting_items = meeting_item.is_some();
        if let Some(item) = &meeting_item {
            all.push(item);
        }
        if let Some(item) = &join_item {
            all.push(item);
        }
        let quick_start = all.len();
        all.extend(quick.iter().map(Box::as_ref));
        if let Some(item) = &stop_recording {
            all.push(item);
        }
        let has_quick_items = all.len() > quick_start;
        if has_meeting_items && has_quick_items {
            all.insert(quick_start, &meeting_separator);
        }
        if !all.is_empty() {
            all.push(&separator);
        }
        if let Some(line) = &assistant {
            all.push(line);
        }
        all.push(&show);
        all.push(&quit);
        let menu = Menu::with_items(app, &all).map_err(menu_error)?;

        *self.raise.lock().unwrap() = raise;
        // `recording` takes priority over the meeting line: a live
        // microphone is the more urgent thing to say, and the two are rare
        // enough together (recording something unrelated while another
        // meeting looms) that showing only one is no real loss.
        let tooltip = match &recording {
            Some(title) => format!("Every Day \u{2014} Recording \u{201c}{title}\u{201d}"),
            None => match &meeting {
                Some(m) => format!("Every Day \u{2014} {}", m.text),
                None => "Every Day".to_string(),
            },
        };
        // The title: beside the icon on macOS, a label in the panel on
        // Linux (only once an icon exists, which this tray always has --
        // see the mark comment below), and a documented no-op on Windows,
        // where the tooltip above and the menu's own first line are what
        // stand in instead. `None` -- the preference is off, the vault is
        // locked, or there is nothing to say -- clears whatever title was
        // there, which is also what makes a lock take a decrypted title off
        // the screen the moment [`crate::events::WindowSink::lock_state`]
        // calls `refresh`.
        let title = meeting.as_ref().map(|m| truncate_chars(&m.text, TRAY_TITLE_MAX_CHARS));

        let mut slot = self.icon.lock().unwrap();
        if let Some(tray) = slot.as_ref() {
            tray.set_menu(Some(menu)).map_err(menu_error)?;
            tray.set_tooltip(Some(&tooltip)).map_err(menu_error)?;
            tray.set_title(title.as_deref()).map_err(menu_error)?;
            // Puts it back if the setting was switched off and on again.
            tray.set_visible(true).map_err(menu_error)?;
            *self.showing.lock().unwrap() = true;
            return Ok(true);
        }

        // No `on_menu_event` or `on_tray_icon_event` here: they are the
        // `Builder`'s, in `lib.rs`. Registering a menu handler on an icon
        // appends it to a process-wide list that nothing ever removes, so an
        // icon built a second time would leave every menu pick firing its
        // handler twice -- two journal entries from one click.
        let mut builder = TrayIconBuilder::with_id(TRAY_ID).menu(&menu).tooltip(tooltip);
        if let Some(t) = &title {
            builder = builder.title(t);
        }
        // The application mark, not a monochrome silhouette of it. A macOS
        // template icon would be the more idiomatic choice in the menu bar,
        // but the mark is a filled tile and a template renders only its
        // alpha -- which would put a solid black square up there. The mark is
        // already drawn to survive 16 pixels (see `icons/icon.svg`), and it
        // is the same thing the dock, the taskbar and the panel show.
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }

        match builder.build(app) {
            Ok(tray) => {
                *slot = Some(tray);
                *self.showing.lock().unwrap() = true;
                Ok(true)
            }
            // Not an error to report: the application is fine, this desktop
            // simply has nowhere to put an icon.
            Err(e) => {
                tracing::info!(error = %e, "no system tray available");
                Ok(false)
            }
        }
    }

    /// Take the icon down, keeping it in hand for the next `show`.
    ///
    /// Hidden rather than dropped, and the difference is not stylistic.
    /// Dropping this handle does not remove anything: `TrayIconBuilder::build`
    /// files a second copy in Tauri's resource table, so the icon would stay
    /// on screen and the switch in Settings would appear to do nothing. The
    /// route that *does* remove it, `remove_tray_by_id`, leaves the id behind
    /// in Tauri's own index and so works exactly once -- and building a
    /// replacement afterwards is what duplicates the menu handler described
    /// above. Visibility is the operation this actually wants: one icon, one
    /// handler, for the life of the process.
    /// Is an icon on screen right now?
    pub fn is_showing(&self) -> bool {
        *self.showing.lock().unwrap()
    }

    pub fn hide(&self) {
        *self.showing.lock().unwrap() = false;
        if let Some(tray) = self.icon.lock().unwrap().as_ref() {
            // Nothing to do about a failure but carry on: the icon is a
            // convenience and the vault is not involved either way.
            if let Err(e) = tray.set_visible(false) {
                tracing::warn!(error = %e, "could not hide the tray icon");
            }
        }
        // A hidden menu cannot be clicked, so nothing needs raising.
        self.raise.lock().unwrap().clear();
    }

    /// Recompute and re-show whatever depends on the moment rather than on
    /// anything the interface sent -- today, only the meeting line's text
    /// and the clock it counts down against -- around whichever menu and
    /// recording state were already in place.
    ///
    /// The one thing a tray already showing needs on a plain tick of the
    /// clock, with no change of its own to report: see `lib.rs`'s
    /// once-a-minute timer, and `events.rs`'s calls on a calendar change or
    /// a lock.
    ///
    /// A no-op while there is no icon on screen, which matters here more
    /// than it does for [`Tray::set_recording`]: unlike a recording
    /// starting -- which must be reachable from the tray even on a machine
    /// that never asked for one, see that method's own doc -- nothing about
    /// a clock ticking or a calendar syncing is reason enough to put an
    /// icon up on a desktop that was never asked for one.
    pub fn refresh(&self, app: &AppHandle) -> CommandResult<bool> {
        if !self.is_showing() {
            return Ok(false);
        }
        let items = self.last_items.lock().unwrap().clone();
        self.rebuild(app, &items)
    }

    /// [`Tray::refresh`], for the callers that are not answering a person:
    /// the minute's tick, a calendar change, a lock.
    ///
    /// Three differences, each for one of those callers. Nothing at all
    /// happens unless the meeting line is turned on, because it is the only
    /// thing in the menu that depends on the clock or the calendar, and a
    /// rebuild is not free: every menu call in it waits on the main thread.
    /// The work goes to a blocking thread, because it reads the vault and
    /// then waits on that main thread, and the event sink that calls this
    /// runs on the async runtime's own workers. And a refresh already queued
    /// absorbs the next one, because a feed sync announces its calendars one
    /// change at a time. The flag is cleared *before* the rebuild reads
    /// anything, so a change that lands mid-rebuild queues another rather
    /// than being lost.
    pub fn schedule_refresh(&self, app: &AppHandle) {
        if !*self.meeting_enabled.lock().unwrap() || !self.is_showing() {
            return;
        }
        if self.refresh_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let tray = app.state::<Tray>();
            tray.refresh_queued.store(false, Ordering::Release);
            if let Err(e) = tray.refresh(&app) {
                tracing::warn!(error = %e.message, "could not refresh the tray's meeting line");
            }
        });
    }

    /// Turn the "show the next meeting" preference on or off, and show the
    /// effect immediately rather than waiting for the next minute's tick or
    /// the next calendar change. The one caller is
    /// `commands::set_tray_meeting`.
    pub fn set_meeting_enabled(&self, app: &AppHandle, on: bool) -> CommandResult<bool> {
        *self.meeting_enabled.lock().unwrap() = on;
        self.refresh(app)
    }

    fn raises(&self, id: &str) -> bool {
        self.raise.lock().unwrap().get(id).copied().unwrap_or(true)
    }
}

/// `MenuItem::with_id` is generic over the accelerator type even when there
/// is no accelerator, so `None` needs one to infer.
const NO_ACCEL: Option<&str> = None;

fn menu_error(e: tauri::Error) -> CommandError {
    CommandError::new("tray", format!("the tray menu could not be built: {e}"))
}

/// Reject ids in the shell's own namespace, at any depth.
fn check_ids(item: &TrayItem) -> CommandResult<()> {
    match item {
        TrayItem::Action { id, .. } if id.starts_with(RESERVED) => Err(CommandError::new(
            "tray",
            format!("tray item id {id:?} uses the reserved {RESERVED:?} prefix"),
        )),
        TrayItem::Submenu { items, .. } => items.iter().try_for_each(check_ids),
        _ => Ok(()),
    }
}

/// Turn the description into real menu items, recording which ids raise the
/// window as it goes.
fn build(
    app: &AppHandle,
    items: &[TrayItem],
    raise: &mut HashMap<String, bool>,
) -> CommandResult<Vec<Box<dyn IsMenuItem<Wry>>>> {
    let mut built: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::with_capacity(items.len());
    for item in items {
        let one: Box<dyn IsMenuItem<Wry>> = match item {
            TrayItem::Separator => {
                Box::new(PredefinedMenuItem::separator(app).map_err(menu_error)?)
            }
            TrayItem::Action { id, label, enabled, checked, raise: raises } => {
                raise.insert(id.clone(), *raises);
                match checked {
                    Some(on) => Box::new(
                        CheckMenuItem::with_id(app, id, label, *enabled, *on, NO_ACCEL)
                            .map_err(menu_error)?,
                    ),
                    None => Box::new(
                        MenuItem::with_id(app, id, label, *enabled, NO_ACCEL)
                            .map_err(menu_error)?,
                    ),
                }
            }
            TrayItem::Submenu { label, enabled, items } => {
                let children = build(app, items, raise)?;
                let refs: Vec<&dyn IsMenuItem<Wry>> = children.iter().map(Box::as_ref).collect();
                Box::new(Submenu::with_items(app, label, *enabled, &refs).map_err(menu_error)?)
            }
        };
        built.push(one);
    }
    Ok(built)
}

/// Raise the main window: undo a minimise, undo a hide, and take focus.
///
/// All three, in that order, because "the window is not in front of me" has
/// three different causes and only doing one of them fixes one of them. Also
/// what a second launch does -- see the single-instance plugin in `lib.rs`.
pub fn raise_window(app: &AppHandle) {
    let Some(window) = app.webview_windows().values().next().cloned() else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    match id {
        SHOW_ID => raise_window(app),
        STOP_RECORDING_ID => crate::meeting::stop_from_tray(app.clone()),
        OPEN_MEETING_ID => {
            raise_window(app);
            // The interface's own half -- switching to the calendar app --
            // lives in `ui/src/lib/tray.svelte.ts`'s `onOpenCalendar`
            // handler; the shell has no notion of app sections to switch to
            // itself. Not `TRAY_ACTION`: that id space is the interface's
            // quick actions, and this is a line the interface never
            // registered a handler for.
            let _ = app.emit(OPEN_CALENDAR_EVENT, ());
        }
        JOIN_MEETING_ID => {
            // Read back rather than recomputed: `meeting_line` could answer
            // differently by the time a click lands (the next minute's
            // tick, a calendar sync), and what this opens should be exactly
            // the link the menu showed, not whatever is current now.
            let url = app
                .state::<Tray>()
                .meeting
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|m| m.join_url.clone());
            if let Some(url) = url
                && let Err(e) = app.opener().open_url(url, None::<&str>)
            {
                tracing::warn!(error = %e, "could not open the meeting link");
            }
        }
        QUIT_ID => {
            // A close *request*, not a teardown: it goes through the window's
            // own `CloseRequested` handler, so quitting from the tray saves
            // and locks exactly the way closing the window does. The three
            // second fallback there also covers an interface too wedged to
            // answer, which is the case where a tray quit matters most.
            if let Some(window) = app.webview_windows().values().next() {
                let _ = window.close();
            }
        }
        _ => {
            if app.state::<Tray>().raises(id) {
                raise_window(app);
            }
            // The window is still alive whether or not it is on screen, so
            // the interface receives this either way.
            let _ = app.emit(TRAY_ACTION, id);
        }
    }
}

pub fn on_tray_icon_event(app: &AppHandle, event: TrayIconEvent) {
    // Double click, and only on the left button: the Windows convention for
    // "open the application", where a single left click is already spoken
    // for by the menu. macOS shows the menu on any click and Linux reports
    // no click at all, so on those two this is dead code and the menu's
    // "Open Every Day" is the way back -- which is why the shell appends it
    // rather than trusting the interface to.
    if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
        raise_window(app);
    }
}

#[cfg(test)]
mod tests {
    //! The pure half of the "current/next meeting" line: picking which
    //! event qualifies, and turning the chosen one into words. `meeting_line`
    //! itself -- the glue that reads a vault through an `AppHandle` -- is
    //! untested here for the same reason `assistant_line` beside it is: it
    //! has nothing left to decide once these functions are right, and
    //! proving it would mean standing up a whole Tauri app for what is, at
    //! that point, wiring.
    use super::*;
    use everyday_core::id::{CalendarId, EventId};
    use jiff::civil::date;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    fn calendar() -> Calendar {
        Calendar::subscribed("Work", "https://example.com/work.ics")
    }

    fn event(calendar_id: CalendarId, start: Timestamp, end: Timestamp) -> Event {
        Event {
            id: EventId::new(),
            calendar_id,
            uid: format!("evt-{}", EventId::new()),
            title: "Standup".into(),
            description: String::new(),
            location: String::new(),
            start,
            end,
            local_date: date(2026, 9, 16),
            end_date: date(2026, 9, 16),
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: Vec::new(),
            url: String::new(),
            busy: true,
            series: None,
            updated_at: start,
        }
    }

    // ---- event_is_showable -------------------------------------------------

    #[test]
    fn an_ordinary_busy_event_on_a_visible_calendar_is_showable() {
        let cal = calendar();
        let e = event(cal.id, at(0), at(1_800));
        assert!(event_is_showable(&e, &cal));
    }

    #[test]
    fn an_all_day_event_is_not_showable() {
        let cal = calendar();
        let mut e = event(cal.id, at(0), at(1_800));
        e.all_day = true;
        assert!(!event_is_showable(&e, &cal));
    }

    #[test]
    fn a_cancelled_event_is_not_showable() {
        let cal = calendar();
        let mut e = event(cal.id, at(0), at(1_800));
        e.status = EventStatus::Cancelled;
        assert!(!event_is_showable(&e, &cal));
    }

    #[test]
    fn a_free_event_is_not_showable() {
        let cal = calendar();
        let mut e = event(cal.id, at(0), at(1_800));
        e.busy = false;
        assert!(!event_is_showable(&e, &cal));
    }

    #[test]
    fn an_event_on_a_calendar_hidden_from_the_grid_is_not_showable() {
        let mut cal = calendar();
        cal.visible = false;
        let e = event(cal.id, at(0), at(1_800));
        assert!(!event_is_showable(&e, &cal));
    }

    // ---- pick_meeting -------------------------------------------------------

    #[test]
    fn a_meeting_in_progress_is_preferred_over_an_upcoming_one() {
        let cal_id = CalendarId::new();
        let now = at(10_000);
        let in_progress = event(cal_id, at(9_000), at(11_000));
        let upcoming = event(cal_id, at(10_500), at(12_000));
        let events = [upcoming, in_progress.clone()];
        let chosen = pick_meeting(&events, now, at(100_000));
        assert_eq!(chosen.unwrap().id, in_progress.id);
    }

    #[test]
    fn two_overlapping_meetings_pick_the_one_that_started_first() {
        let cal_id = CalendarId::new();
        let now = at(10_000);
        let earlier = event(cal_id, at(9_000), at(11_000));
        let later = event(cal_id, at(9_500), at(11_500));
        let events = [later, earlier.clone()];
        let chosen = pick_meeting(&events, now, at(100_000));
        assert_eq!(chosen.unwrap().id, earlier.id);
    }

    #[test]
    fn an_upcoming_meeting_within_the_horizon_is_picked() {
        let cal_id = CalendarId::new();
        let now = at(0);
        let soon = event(cal_id, at(600), at(1_800)); // ten minutes away
        let events = [soon.clone()];
        let chosen = pick_meeting(&events, now, at(100_000));
        assert_eq!(chosen.unwrap().id, soon.id);
    }

    #[test]
    fn an_upcoming_meeting_past_the_horizon_is_not_picked() {
        let cal_id = CalendarId::new();
        let now = at(0);
        let far = event(cal_id, at(MEETING_HORIZON_SECS + 60), at(MEETING_HORIZON_SECS + 1_800));
        assert!(pick_meeting(&[far], now, at(1_000_000)).is_none());
    }

    #[test]
    fn an_upcoming_meeting_is_capped_at_the_end_of_the_day_even_within_the_horizon() {
        let cal_id = CalendarId::new();
        let now = at(0);
        // Well within the three-hour horizon, but past the artificially
        // early "end of day" this test hands `pick_meeting` -- standing in
        // for a meeting after midnight that must not read as "next" late
        // the evening before.
        let after_midnight = event(cal_id, at(1_800), at(3_600));
        assert!(pick_meeting(&[after_midnight], now, at(1_000)).is_none());
    }

    #[test]
    fn nothing_qualifying_picks_nothing() {
        assert!(pick_meeting(&[], at(0), at(100_000)).is_none());
    }

    // ---- format_meeting_text -------------------------------------------------

    #[test]
    fn an_in_progress_meeting_reads_now_until() {
        let cal_id = CalendarId::new();
        let now = at(10_000);
        let mut e = event(cal_id, at(9_000), at(11_400));
        e.title = "Design review".into();
        let text = format_meeting_text(&e, now, &TimeZone::UTC);
        assert!(text.starts_with("Design review \u{b7} now, until "), "got {text:?}");
    }

    #[test]
    fn a_meeting_starting_soon_counts_down_in_minutes() {
        let cal_id = CalendarId::new();
        let now = at(0);
        let mut e = event(cal_id, at(12 * 60), at(30 * 60)); // twelve minutes away
        e.title = "Standup".into();
        assert_eq!(format_meeting_text(&e, now, &TimeZone::UTC), "Standup \u{b7} in 12 min");
    }

    #[test]
    fn a_meeting_further_out_shows_a_clock_time_instead() {
        let cal_id = CalendarId::new();
        let now = at(0);
        let mut e = event(cal_id, at(14 * 3_600), at(15 * 3_600)); // 14:00 UTC
        e.title = "1:1 with Sam".into();
        assert_eq!(format_meeting_text(&e, now, &TimeZone::UTC), "1:1 with Sam \u{b7} 14:00");
    }

    #[test]
    fn a_meeting_starting_within_the_next_minute_still_counts_up_from_one() {
        let cal_id = CalendarId::new();
        let now = at(0);
        let mut e = event(cal_id, at(10), at(1_800)); // ten seconds away
        e.title = "Standup".into();
        assert_eq!(format_meeting_text(&e, now, &TimeZone::UTC), "Standup \u{b7} in 1 min");
    }

    // ---- truncate_chars -------------------------------------------------------

    #[test]
    fn a_short_string_is_left_alone() {
        assert_eq!(truncate_chars("Standup", 40), "Standup");
    }

    #[test]
    fn a_long_string_is_cut_with_an_ellipsis() {
        let long = "a".repeat(50);
        let out = truncate_chars(&long, 40);
        assert_eq!(out.chars().count(), 40);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // Every character here is multiple bytes in UTF-8; a byte-indexed
        // cut would panic or corrupt the string.
        let long = "\u{e9}".repeat(50);
        let out = truncate_chars(&long, 10);
        assert_eq!(out.chars().count(), 10);
    }
}
