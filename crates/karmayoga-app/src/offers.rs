//! Offers to take notes, held by the shell: one hub for every way a
//! meeting is noticed -- a calendar event starting
//! (`karmayoga_service::meeting::watch`, by way of
//! [`crate::events::WindowSink`]) and another application taking the
//! microphone ([`crate::micwatch`]) -- and every way one is answered: the
//! in-window banner (`MeetingOfferBanner.svelte`), the desktop
//! notification's buttons ([`crate::notice`]) and the tray.
//!
//! # Why the shell holds them
//!
//! An offer used to be an event the window either heard or missed: raised
//! once by the service, shown as a banner, and gone from everywhere the
//! moment the banner was. That is fine for somebody looking at the window
//! and no use at all to somebody on a call, who is looking at the call. So
//! an offer is now a thing with a life -- raised, perhaps answered in one
//! place and moot in the others, perhaps withdrawn because the call ended
//! first -- and this module is where that life is kept. Every surface
//! (the banner, the notification, the tray item) is a view of the pending
//! set below, and every answer goes through [`answer`], so what one surface
//! does is what the others are told.
//!
//! # Keys
//!
//! `event:{calendarId}:{uid}` names anything tied to a calendar event,
//! however it was noticed -- the watcher saw it start, or the microphone
//! went live during it -- so the same meeting is never offered twice, once
//! by each. `mic:{n}` names a microphone offer with no event behind it.
//! Calendar and `uid` are what `dismiss_meeting_offer` is keyed by, not
//! `eventId`: see [`MeetingOffer`]'s own doc on why the latter does not
//! survive a feed resync.
//!
//! # One microphone, one call
//!
//! Once a recording runs ([`on_recording_started`]) every other pending
//! offer is moot and is withdrawn; and an offer made because the
//! microphone went live is withdrawn when it goes quiet again
//! ([`on_mic_ended`]), which is also what stops a recording that was
//! started during that call, when "stop when the call ends" is on.
//!
//! # The corner card
//!
//! An offer that is ready is a card in the corner of the screen -- the
//! panel ([`crate::panel`]) -- rather than a system notification, whether or
//! not recording starts by itself. Which face it wears depends on whether the
//! meeting has been joined. Before it is, the card offers to join and record,
//! which a press does at once. Once the call is joined some other way -- the
//! microphone goes live -- the card either counts down fifteen seconds and then
//! starts recording (with auto-record on), unless it is cancelled, the vault
//! locks, or the offer expires, or asks "Take notes" and waits (with it off:
//! nothing starts by itself). "Take notes" on the card starts recording
//! without the wait. The window only draws what it is told, so closing it
//! cannot start a recording.
//!
//! # The brief on the card
//!
//! The service prepares a brief a little before a meeting with other people
//! in it -- fifteen minutes, unless the person changed that -- and says so
//! (`karmayoga_service::meeting::brief`, [`on_meeting_brief`]); it is kept
//! here by the offer's key, so the card that goes up when the meeting starts
//! already has it, and one that goes up without ([`fetch_brief`]) asks the
//! service once for a brief it has already prepared. A brief that arrives
//! while the card is up redraws it. The brief's
//! talking points are boxes on the card; ticking one completes its task
//! ([`on_panel_action`]). A brief is dropped with the offer it was kept for,
//! and all of them when the vault locks: they name people and say what is
//! being raised with them.

use std::collections::HashMap;
use std::sync::Mutex;

use jiff::tz::TimeZone;
use karmayoga_core::TaskStatus;
use karmayoga_core::id::{CalendarId, EventId, RecordingId, TaskId};
use karmayoga_core::meeting::Recording;
use karmayoga_core::meeting::brief::{MeetingBrief, join_service};
use karmayoga_service::Ctx;
use karmayoga_service::domains::meetings::MeetingNow;
use karmayoga_service::error::{CommandError, CommandResult};
use karmayoga_service::events::MeetingOffer;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

/// Tauri event: an offer to show. Payload: [`OfferPayload`].
pub const OFFER_EVENT: &str = "meeting-offer";
/// Tauri event: an offer is over -- answered somewhere, withdrawn because
/// the call ended, or expired. Payload: `{ key }`.
pub const SETTLED_EVENT: &str = "meeting-offer-settled";
/// Tauri event: answering an offer from outside the window (the
/// notification, the tray) failed. Payload: `{ key, message }`.
pub const FAILED_EVENT: &str = "meeting-offer-failed";
/// Tauri event: the person asked to set meeting notes up from an offer
/// that was not `ready`. Payload: `{ key }`. The interface opens
/// Settings → Meetings.
pub const SETUP_EVENT: &str = "meeting-offer-setup";
/// Tauri event: the auto-record countdown, for the in-app banner.
/// Payload: `{ key, seconds }`. `seconds: null` clears it.
pub const COUNTDOWN_EVENT: &str = "meeting-countdown";

/// How long after joining before recording starts, unless cancelled.
const COUNTDOWN: std::time::Duration = std::time::Duration::from_secs(15);

/// The notification's key for "the call ended and the recording stopped
/// with it". Not an offer's key; there is only ever one of these up.
const CALL_ENDED_KEY: &str = "call-ended";

/// How long after a recording starts a call that begins can still be the
/// call it is recording -- see [`on_mic_started`].
const LINK_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Where an offer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OfferSource {
    Calendar,
    Microphone,
}

/// Mirrors the TS `MeetingOfferPayload` in `ui/src/lib/api.ts`, field for
/// field.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferPayload {
    /// `event:{calendarId}:{uid}` for an offer about a calendar event
    /// (whichever way it was noticed), `mic:{n}` for a microphone offer
    /// with no event.
    pub key: String,
    pub source: OfferSource,
    pub event_id: Option<EventId>,
    pub calendar_id: Option<CalendarId>,
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    pub title: String,
    pub start: Option<jiff::Timestamp>,
    pub end: Option<jiff::Timestamp>,
    pub calendar_name: Option<String>,
    /// The application using the microphone, for a microphone offer.
    pub app: Option<String>,
    pub join_url: Option<String>,
    pub ready: bool,
    pub automatic: bool,
    /// Recording starts by itself, after a countdown, once the call is
    /// joined: the setting is on and this offer is asking. Without it the
    /// card asks instead and nothing starts until it is answered.
    pub auto_record: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<RecordingId>,
}

/// The corner card, when one is up: whose it is and which face it wears.
/// What it says is not kept: [`meeting_panel`] makes that from the offer, the
/// face and the brief each time it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    key: String,
    phase: CardPhase,
    /// Seconds left on the countdown. Unused on the other faces.
    seconds: u32,
}

/// An answer to an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OfferAnswer {
    /// Start recording.
    Take,
    /// Open the join link, then start recording.
    Join,
    /// Not this time.
    NotNow,
    /// Calendar: never for this meeting's series. Microphone: stop offering
    /// when the microphone turns on (`MeetingSettings::mic_offer = false`).
    Never,
}

// ---- keys and payloads ------------------------------------------------------

/// The key of an offer about a calendar event, however it was noticed.
fn event_key(calendar_id: CalendarId, uid: &str) -> String {
    format!("event:{calendar_id}:{uid}")
}

/// Whether an offer should start capture on its own, without a press.
///
/// "Always" mode and a transcriber that can actually run: the service never
/// sets `automatic` on an offer that is not `ready`, but this checks both
/// anyway, so a version skew between a remote vault and this shell cannot
/// turn a "set meeting notes up" prompt into a recording that fails
/// silently.
fn starts_automatically(offer: &MeetingOffer) -> bool {
    offer.automatic && offer.ready
}

/// What a [`MeetingOffer`] looks like on the wire to the interface, noticed
/// by `source` -- and, for the microphone, whichever application had it.
fn calendar_payload(
    offer: &MeetingOffer,
    source: OfferSource,
    app_name: Option<String>,
) -> OfferPayload {
    OfferPayload {
        key: event_key(offer.calendar_id, &offer.uid),
        source,
        event_id: Some(offer.event_id),
        calendar_id: Some(offer.calendar_id),
        uid: Some(offer.uid.clone()),
        series: offer.series.clone(),
        title: offer.title.clone(),
        start: Some(offer.start),
        end: Some(offer.end),
        calendar_name: Some(offer.calendar_name.clone()),
        app: app_name,
        join_url: offer.join_url.clone(),
        ready: offer.ready,
        automatic: starts_automatically(offer),
        auto_record: offer.auto_record,
        recording_id: None,
    }
}

/// An offer for a call nobody put on a calendar: the microphone went live
/// and there is no event to tie it to.
fn mic_payload(n: u64, app_name: Option<String>, ready: bool) -> OfferPayload {
    OfferPayload {
        key: format!("mic:{n}"),
        source: OfferSource::Microphone,
        event_id: None,
        calendar_id: None,
        uid: None,
        series: None,
        title: match &app_name {
            Some(name) => format!("Call on {name}"),
            None => "Call".to_string(),
        },
        start: None,
        end: None,
        calendar_name: None,
        app: app_name,
        join_url: None,
        ready,
        automatic: false,
        auto_record: false,
        recording_id: None,
    }
}

// ---- what the notification says ---------------------------------------------

/// A desktop notification's words, for [`crate::notice::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct NoticeText {
    summary: String,
    body: String,
    /// `(action key, label)`, in the order the buttons are drawn.
    actions: Vec<(&'static str, &'static str)>,
}

/// The notification for an offer that is a question.
///
/// A calendar offer says which calendar the meeting is on and that it is
/// starting; one the microphone noticed says which application has it
/// instead, since "starting now" is not what the shell knows. A call with
/// no event has nothing to name but the application. A "Join" button is
/// offered first when there is a link to join -- the usual next thing a
/// person does with a meeting that is starting -- and an offer that is not
/// `ready` swaps its buttons for the one that fixes that: taking notes
/// would only fail. When the meeting's brief is known its first line goes on
/// the end of the body ([`brief_line`]).
fn notice_text(p: &OfferPayload, brief: Option<&MeetingBrief>) -> NoticeText {
    let in_call_with_no_event = p.source == OfferSource::Microphone && p.event_id.is_none();
    let summary = if in_call_with_no_event {
        "Taking a call? Take notes".to_string()
    } else {
        format!("Take notes for {}?", p.title)
    };
    let mut body = match p.source {
        OfferSource::Calendar => match &p.calendar_name {
            Some(name) => format!("{name} \u{b7} starting now"),
            None => "Starting now".to_string(),
        },
        OfferSource::Microphone => {
            let who = p.app.as_deref().unwrap_or("Another app");
            if in_call_with_no_event {
                format!("{who} is using your microphone.")
            } else {
                format!("{who} is using your microphone")
            }
        }
    };
    if !p.ready {
        if !body.ends_with('.') {
            body.push('.');
        }
        body.push_str(" Meeting notes aren't set up yet.");
        return NoticeText {
            summary,
            body: with_brief(body, brief),
            actions: vec![("setup", "Set up meeting notes"), ("notNow", "Not now")],
        };
    }
    let actions = if p.join_url.is_some() {
        vec![("join", "Join & take notes"), ("take", "Take notes"), ("notNow", "Not now")]
    } else {
        vec![("take", "Take notes"), ("notNow", "Not now")]
    };
    NoticeText { summary, body: with_brief(body, brief), actions }
}

/// The notification for an offer that was not a question: "Always" mode has
/// already started the recording. The one button is how to take it back.
fn automatic_notice_text(p: &OfferPayload, brief: Option<&MeetingBrief>) -> NoticeText {
    NoticeText {
        summary: format!("Taking notes for {}", p.title),
        body: with_brief("Started automatically".to_string(), brief),
        actions: vec![("stop", "Stop recording")],
    }
}

/// `body` with the brief's [`brief_line`] under it, if there is a brief with
/// anything to say.
fn with_brief(body: String, brief: Option<&MeetingBrief>) -> String {
    match brief.and_then(brief_line) {
        Some(line) => format!("{body}\n{line}"),
        None => body,
    }
}

/// The most people a notification names before "+2".
const NOTICE_PEOPLE: usize = 3;
/// The longest a notification's brief line is, in characters. A banner shows
/// a line or two of its body, and what it cuts off is not read.
const NOTICE_LINE_CHARS: usize = 100;
/// The longest the topic in that line is, so one long agenda item cannot push
/// the people off the end of it.
const NOTICE_TOPIC_CHARS: usize = 56;

/// The one line of a brief a notification has room for: who is coming and the
/// first thing worth knowing -- "With Josh, Priya · Agenda: Q3 budget".
///
/// What the person promised to raise comes before the agenda, because it is
/// theirs; the agenda before the summary, because it is what the meeting is
/// to cover. `None` for a brief that names nobody and says nothing.
fn brief_line(brief: &MeetingBrief) -> Option<String> {
    let mut parts = Vec::new();
    let names: Vec<&str> =
        brief.people.iter().map(|p| p.name.trim()).filter(|n| !n.is_empty()).collect();
    if !names.is_empty() {
        let shown = names[..names.len().min(NOTICE_PEOPLE)].join(", ");
        parts.push(match names.len().saturating_sub(NOTICE_PEOPLE) {
            0 => format!("With {shown}"),
            more => format!("With {shown} +{more}"),
        });
    }
    let topic = brief
        .talking_points
        .iter()
        .find(|p| !p.done)
        .map(|p| format!("To raise: {}", p.text.trim()))
        .or_else(|| {
            let first = brief.agenda.iter().map(|l| l.trim()).find(|l| !l.is_empty());
            first.map(|a| format!("Agenda: {a}"))
        })
        .or_else(|| {
            let summary = brief.summary.as_deref().map(str::trim);
            summary.filter(|s| !s.is_empty()).map(str::to_owned)
        });
    parts.extend(topic.map(|t| shorten(&t, NOTICE_TOPIC_CHARS)));
    (!parts.is_empty()).then(|| shorten(&parts.join(" \u{b7} "), NOTICE_LINE_CHARS))
}

/// `text` cut to `max` characters, on a character boundary, with an ellipsis
/// where anything was cut.
fn shorten(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('\u{2026}');
    out
}

/// What to tell somebody whose recording stopped because the call did.
fn call_ended_text(title: &str, app_name: Option<&str>) -> NoticeText {
    NoticeText {
        summary: "Call ended \u{2014} writing your notes".to_string(),
        body: format!(
            "Recording of {title} stopped when {} let go of the microphone.",
            app_name.unwrap_or("the call")
        ),
        actions: Vec::new(),
    }
}

// ---- the bookkeeping --------------------------------------------------------

/// An offer waiting for an answer.
#[derive(Debug, Clone)]
struct Pending {
    payload: OfferPayload,
    /// When the offer stops being worth making -- a calendar meeting's end.
    /// A microphone offer with no event has none: it lasts as long as the
    /// call does, which [`on_mic_ended`] is told about.
    expires: Option<jiff::Timestamp>,
}

/// The pending set and what has already been offered, with no window or
/// clock in it so it can be tested as plain data.
#[derive(Default)]
struct Book {
    /// Oldest first.
    pending: Vec<Pending>,
    /// Event keys offered by this process, with when that offer expires.
    ///
    /// Remembered past the answer, not only while pending: a "Not now"
    /// must not let the other way of noticing the same meeting offer it
    /// again a minute later. Forgotten once the meeting is over, because
    /// an event with a shared `uid` -- a repeating one, on a feed that does
    /// not give each occurrence its own -- is a different offer next week.
    offered: HashMap<String, jiff::Timestamp>,
    /// Source of the `n` in `mic:{n}`.
    mic_count: u64,
    /// Offers that arrived while a recording was running, held back until
    /// it ends: one microphone, one call, but a meeting that starts while
    /// the last one is still being recorded is still a meeting to offer --
    /// back-to-back calls are exactly when that happens.
    deferred: Vec<Pending>,
}

impl Book {
    /// Record that `key` is being offered, for an event ending at `until`.
    /// `false` if it already was, and so is not to be offered again.
    fn mark_offered(&mut self, key: &str, until: jiff::Timestamp, now: jiff::Timestamp) -> bool {
        self.offered.retain(|_, end| *end > now);
        if self.offered.contains_key(key) {
            return false;
        }
        self.offered.insert(key.to_string(), until);
        true
    }

    /// Let `key` be offered again: an offer about a meeting that the
    /// microphone noticed early and then withdrew when that session ended
    /// (somebody testing their microphone before the call) must not keep
    /// the calendar's own offer at the meeting's start from being made.
    fn forget_offered(&mut self, key: &str) {
        self.offered.remove(key);
    }

    /// Hold an offer back until the running recording ends.
    fn defer(&mut self, payload: OfferPayload, expires: Option<jiff::Timestamp>) {
        self.deferred.retain(|p| p.payload.key != payload.key);
        self.deferred.push(Pending { payload, expires });
    }

    /// The held-back offers still worth making, oldest first; the rest are
    /// dropped.
    fn take_deferred(&mut self, now: jiff::Timestamp) -> Vec<Pending> {
        std::mem::take(&mut self.deferred).into_iter().filter(|p| Self::live(p, now)).collect()
    }

    fn next_mic_number(&mut self) -> u64 {
        self.mic_count += 1;
        self.mic_count
    }

    fn push(&mut self, payload: OfferPayload, expires: Option<jiff::Timestamp>) {
        // A key is only ever pending once; a repeat replaces, keeping its
        // place, rather than showing two banners for one thing.
        match self.pending.iter_mut().find(|p| p.payload.key == payload.key) {
            Some(existing) => *existing = Pending { payload, expires },
            None => self.pending.push(Pending { payload, expires }),
        }
    }

    fn live(p: &Pending, now: jiff::Timestamp) -> bool {
        p.expires.is_none_or(|end| end > now)
    }

    fn find(&self, key: &str, now: jiff::Timestamp) -> Option<&Pending> {
        self.pending.iter().find(|p| p.payload.key == key && Self::live(p, now))
    }

    fn remove(&mut self, key: &str) -> Option<Pending> {
        let at = self.pending.iter().position(|p| p.payload.key == key)?;
        Some(self.pending.remove(at))
    }

    /// Take every pending offer `pick` chooses, oldest first.
    fn take_where(&mut self, pick: impl Fn(&Pending) -> bool) -> Vec<Pending> {
        let (taken, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.pending).into_iter().partition(|p| pick(p));
        self.pending = kept;
        taken
    }

    /// Pending offers that have not expired, oldest first.
    fn payloads(&self, now: jiff::Timestamp) -> Vec<OfferPayload> {
        self.pending.iter().filter(|p| Self::live(p, now)).map(|p| p.payload.clone()).collect()
    }

    /// The oldest pending offer a recording could start from -- what the
    /// tray's "Take notes for ..." names. One that is not `ready` is
    /// skipped: a menu item that can only fail is worse than none.
    fn oldest_ready(&self, now: jiff::Timestamp) -> Option<&OfferPayload> {
        self.pending
            .iter()
            .filter(|p| Self::live(p, now) && p.payload.ready && !p.payload.automatic)
            .map(|p| &p.payload)
            .next()
    }

    fn oldest(&self, now: jiff::Timestamp) -> Option<&OfferPayload> {
        self.pending.iter().filter(|p| Self::live(p, now)).map(|p| &p.payload).next()
    }
}

#[derive(Default)]
struct Inner {
    book: Book,
    /// The recording that is running started during, or was joined by, a
    /// microphone session -- so it stops when that session does, if the
    /// person asked for that.
    mic_linked: bool,
    /// Whichever application held the microphone last, for the words on
    /// "the call ended".
    mic_app: Option<String>,
    /// When the running recording started, if one is running.
    recording_started: Option<std::time::Instant>,
    /// Keys of notifications raised that are not pending offers -- "Taking
    /// notes for ... (Stop)" and "Call ended" -- so they can be taken down
    /// when they stop being true, and when the vault locks.
    shown: Vec<String>,
    /// The corner card, when one is up.
    notice: Option<Card>,
    /// Bumped whenever a countdown is replaced or cancelled, so the task
    /// that was counting stops without starting a recording. The faces that
    /// keep their time up to date ([`spawn_clock`]) answer to it too.
    countdown_gen: u64,
    /// The offer whose recording is being started right now -- by a press,
    /// or by its countdown running out -- so a press in the same instant as
    /// the countdown's end does not start it twice.
    starting: Option<String>,
    /// Briefs by the offer's key, as the service sent them -- before the
    /// meeting, so the card has one the moment it goes up -- with a talking
    /// point ticked on the card marked done here as well. Dropped with the
    /// offer, and all at once on lock.
    briefs: HashMap<String, MeetingBrief>,
}

impl Inner {
    /// The offer the card asks to join, if the card is `key`'s and the offer
    /// is still pending.
    fn asking(&self, key: &str, now: jiff::Timestamp) -> Option<OfferPayload> {
        let shown =
            self.notice.as_ref().is_some_and(|n| n.key == key && n.phase == CardPhase::Join);
        shown.then(|| self.book.find(key, now).map(|p| p.payload.clone())).flatten()
    }

    /// The offer the card is showing as joined -- counting down for, or
    /// asking about -- if it is still pending.
    fn joined(&self, now: jiff::Timestamp) -> Option<OfferPayload> {
        let card = self
            .notice
            .as_ref()
            .filter(|n| matches!(n.phase, CardPhase::Countdown | CardPhase::Ask))?;
        self.book.find(&card.key, now).map(|p| p.payload.clone())
    }

    /// Claim starting `key`'s recording. False if it is being started already.
    fn claim_start(&mut self, key: &str) -> bool {
        if self.starting.as_deref() == Some(key) {
            return false;
        }
        self.starting = Some(key.to_string());
        true
    }

    fn release_start(&mut self, key: &str) {
        if self.starting.as_deref() == Some(key) {
            self.starting = None;
        }
    }

    /// `key`'s card as it is drawn now: its face, from the offer and whatever
    /// brief is kept for it. `None` if the card is not up, or is another
    /// offer's, or the offer is no longer pending.
    fn panel(
        &self,
        key: &str,
        now: jiff::Timestamp,
        zone: &TimeZone,
    ) -> Option<crate::panel::Panel> {
        let card = self.notice.as_ref().filter(|n| n.key == key)?;
        let pending = self.book.find(key, now)?;
        let brief = self.briefs.get(key);
        Some(meeting_panel(card.phase, card.seconds, &pending.payload, brief, now, zone))
    }

    /// The card for one tick of the countdown `generation` is counting:
    /// `seconds` left, drawn from the brief already kept. `None` once
    /// anything has replaced or cancelled that countdown.
    fn tick(
        &mut self,
        key: &str,
        generation: u64,
        seconds: u32,
        now: jiff::Timestamp,
        zone: &TimeZone,
    ) -> Option<crate::panel::Panel> {
        if self.countdown_gen != generation {
            return None;
        }
        self.notice.as_mut().filter(|n| n.key == key)?.seconds = seconds;
        self.panel(key, now, zone)
    }

    /// Keep `brief` for the offer it names, in place of any older one. A brief
    /// arrives before its offer does, so there is no offer to drop it with if
    /// the meeting never gets one; what is over is dropped here instead, the
    /// next time a brief comes, and a brief for a meeting already over is not
    /// kept at all.
    fn keep_brief(&mut self, brief: MeetingBrief, now: jiff::Timestamp) {
        self.briefs.retain(|_, b| b.end > now);
        if brief.end > now {
            self.briefs.insert(brief.key.clone(), brief);
        }
    }

    /// The talking point of `key`'s brief that is `task`, and whether it is
    /// already ticked. `None` if the brief has no such point, which is also
    /// what a press on a card whose brief has gone looks like.
    fn point(&self, key: &str, task: TaskId) -> Option<bool> {
        let brief = self.briefs.get(key)?;
        brief.talking_points.iter().find(|p| p.task_id == task).map(|p| p.done)
    }

    /// Tick `task` off in `key`'s brief.
    fn tick_point(&mut self, key: &str, task: TaskId) {
        let Some(brief) = self.briefs.get_mut(key) else { return };
        for point in brief.talking_points.iter_mut().filter(|p| p.task_id == task) {
            point.done = true;
        }
    }
}

/// Held in Tauri state: the offers still waiting for an answer, and what
/// the hub needs to remember between calls.
#[derive(Default)]
pub struct Offers {
    inner: Mutex<Inner>,
    /// Held while the tray is brought up to date. Updating it is slow (the
    /// menu is built on the main thread) and runs off the async runtime, so
    /// two updates can overlap; this makes the later one -- which reads the
    /// pending set when it gets the lock -- also the last to draw.
    tray: Mutex<()>,
}

impl Offers {
    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }
}

fn offers(app: &AppHandle) -> tauri::State<'_, Offers> {
    app.state::<Offers>()
}

// ---- raising ----------------------------------------------------------------

/// A calendar offer from the service. Called by
/// [`crate::events::WindowSink`].
///
/// For "Always" (`automatic`, and `ready`), the offer is not a question:
/// the call has already been recorded without asking on every other window
/// watching this vault, so this one starts capturing too -- through
/// [`crate::meeting::start_automatic`], the same hook a press of "Take
/// notes" goes through -- and the event is forwarded so
/// `meetings.svelte.ts` can show its toast and refresh the recordings list.
/// `recordingId` is left empty either way: `start_automatic` begins the
/// microphone on its own task rather than blocking this call on it, and
/// the interface re-polls `active_recording` instead, which is how the pill
/// finds the recording that resulted.
///
/// Anything else is a question, and is asked everywhere the person might
/// be: the banner, a notification and the tray. Unless a recording is
/// already running -- one microphone, one call -- in which case there is
/// nothing to ask.
pub fn on_calendar_offer(app: &AppHandle, offer: MeetingOffer) {
    let payload = calendar_payload(&offer, OfferSource::Calendar, None);
    // Already in the call: the countdown or the ask to take notes, not the
    // join card.
    offer_event(app, payload, offer.end, mic_live(app));
}

/// The part [`on_calendar_offer`] and a microphone offer tied to an event
/// share: not twice, automatically or as a question.
fn offer_event(app: &AppHandle, payload: OfferPayload, end: jiff::Timestamp, joined: bool) {
    let now = jiff::Timestamp::now();
    let first = offers(app).with(|o| o.book.mark_offered(&payload.key, end, now));
    if !first {
        if joined {
            note_joined(app, &payload.key);
        }
        return;
    }
    if end <= now {
        // Over before it was heard about.
        return;
    }
    if crate::meeting::state_already_recording(app) {
        // Held back until the recording ends -- see `Book::deferred` -- and
        // then asked, never started on its own: a recording that begins
        // the moment another stops, unasked, is not what "Always" meant.
        let mut payload = payload;
        payload.automatic = false;
        offers(app).with(|o| o.book.defer(payload, Some(end)));
        return;
    }
    if payload.automatic
        && let Some(event_id) = payload.event_id
    {
        crate::meeting::start_automatic(app.clone(), event_id);
        emit(app, OFFER_EVENT, &payload);
        let brief = offers(app).with(|o| o.briefs.get(&payload.key).cloned());
        let text = automatic_notice_text(&payload, brief.as_ref());
        offers(app).with(|o| o.shown.push(payload.key.clone()));
        raise_notice(app, &payload.key, &text);
        return;
    }
    raise(app, payload, Some(end), joined);
}

/// Put an offer up everywhere: pending, the interface, a notification
/// unless the window is in front already, and the tray.
fn raise(app: &AppHandle, payload: OfferPayload, expires: Option<jiff::Timestamp>, joined: bool) {
    let key = payload.key.clone();
    offers(app).with(|o| o.book.push(payload.clone(), expires));
    emit(app, OFFER_EVENT, &payload);
    refresh_tray(app);
    if let Some(end) = expires {
        schedule_expiry(app, key, end);
    }
    // The corner card replaces the system notification for this offer. One
    // that is not ready keeps the notification: it is a prompt to set notes
    // up, which a card with a button to start recording would be no use for.
    let phase = card_phase(payload.auto_record, payload.ready, joined);
    match phase {
        Some(CardPhase::Countdown) => arm_countdown(app, &payload.key),
        Some(phase) => show_still(app, &payload.key, phase),
        None => {
            let brief = offers(app).with(|o| o.briefs.get(&payload.key).cloned());
            raise_notice(app, &payload.key, &notice_text(&payload, brief.as_ref()));
        }
    }
    if phase.is_some() {
        fetch_brief(app, &payload);
    }
}

/// Withdraw an offer at `end`, if nobody has answered it by then: a
/// meeting that is over is not worth taking notes of.
fn schedule_expiry(app: &AppHandle, key: String, end: jiff::Timestamp) {
    let wait = (end.as_second() - jiff::Timestamp::now().as_second()).max(0) as u64;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        settle(&app, &key);
    });
}

/// Which face the corner card wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardPhase {
    /// Before the meeting is joined: join it and record.
    Join,
    /// Joined, with auto-record on: recording starts when the countdown ends.
    Countdown,
    /// Joined, with auto-record off: take notes? Nothing starts by itself.
    Ask,
}

impl CardPhase {
    /// The face the card goes back to after an attempt to record failed:
    /// still asking, never counting down again -- a countdown that failed
    /// would only fail again in fifteen seconds. A meeting that was joined
    /// stays joined, so the ask does not offer to join it a second time.
    fn asking(self) -> CardPhase {
        match self {
            CardPhase::Join => CardPhase::Join,
            CardPhase::Countdown | CardPhase::Ask => CardPhase::Ask,
        }
    }
}

/// `None` keeps the system notification: the offer is not one recording could
/// start from. Otherwise it is a card, and `joined` -- the call is on -- says
/// which face: the ask before it, and after it either the countdown, with
/// auto-record on, or the ask to take notes, with it off. A deferred offer is
/// raised with `joined` false, so that a countdown does not begin the moment
/// another recording stops.
fn card_phase(auto_record: bool, ready: bool, joined: bool) -> Option<CardPhase> {
    if !ready {
        return None;
    }
    Some(match (joined, auto_record) {
        (false, _) => CardPhase::Join,
        (true, true) => CardPhase::Countdown,
        (true, false) => CardPhase::Ask,
    })
}

fn mic_live(app: &AppHandle) -> bool {
    app.try_state::<crate::micwatch::MicWatch>().is_some_and(|m| m.session_active())
}

/// Put `key`'s card up, or change it where it is, wearing `phase`. Does
/// nothing if the offer is no longer pending: a card with no offer behind it
/// would have buttons that answer nothing.
///
/// A card that was another offer's goes down first, and the interface's
/// banner hears the countdown, or that there is none.
fn show_card(app: &AppHandle, key: &str, phase: CardPhase, seconds: u32) {
    let (now, zone) = (jiff::Timestamp::now(), TimeZone::system());
    let shown = offers(app).with(|o| {
        o.book.find(key, now)?;
        let gone = o.notice.replace(Card { key: key.to_string(), phase, seconds }).map(|c| c.key);
        o.panel(key, now, &zone).map(|panel| (gone, panel))
    });
    let Some((gone, panel)) = shown else { return };
    emit(
        app,
        COUNTDOWN_EVENT,
        &json!({ "key": key, "seconds": (phase == CardPhase::Countdown).then_some(seconds) }),
    );
    if let Some(gone) = gone.filter(|g| g != key) {
        crate::panel::dismiss(app, &gone);
    }
    crate::panel::show(app, panel);
}

/// Draw `key`'s card again from what is kept -- the same face, with whatever
/// its brief says now. Nothing if the card is not up, and it is never put up
/// by this: a brief arriving a moment after the card was answered must not
/// bring it back.
fn redraw(app: &AppHandle, key: &str) {
    let (now, zone) = (jiff::Timestamp::now(), TimeZone::system());
    if let Some(panel) = offers(app).with(|o| o.panel(key, now, &zone)) {
        crate::panel::update(app, key, |shown| *shown = panel);
    }
}

/// A card on a face that does not count down: the ask to join, or the ask to
/// take notes.
fn show_still(app: &AppHandle, key: &str, phase: CardPhase) {
    let generation = offers(app).with(|o| {
        o.countdown_gen = o.countdown_gen.wrapping_add(1);
        o.countdown_gen
    });
    show_card(app, key, phase, 0);
    spawn_clock(app, key.to_string(), generation);
}

/// How often a card that does not count down is drawn again, for the "in 2
/// min" in its kicker.
const CLOCK: std::time::Duration = std::time::Duration::from_secs(20);

/// Keep a still card's kicker true -- "in 2 min" does not stay true while the
/// card sits there -- until anything changes the card, which is what
/// `generation` is for.
fn spawn_clock(app: &AppHandle, key: String, generation: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(CLOCK).await;
            if !offers(&app).with(|o| o.countdown_gen == generation) {
                return;
            }
            redraw(&app, &key);
        }
    });
}

/// The meeting card as the panel draws it, made from the face it wears, the
/// offer, and the brief when there is one.
///
/// `seconds` is what is left on the countdown, and is read only on that face.
/// `now` and `zone` are the clock and the person's own time zone, passed in so
/// that this has no clock in it.
///
/// Before the call is joined the kicker says when the meeting is, and the
/// body gives its hours in the person's own time and where it is; with a
/// brief, the first thing under that is what the meeting is for. What to know
/// -- what the person promised to raise, the agenda, who is there, and the
/// rest folded away -- comes after, in that order and with nothing in it that
/// is empty.
fn meeting_panel(
    phase: CardPhase,
    seconds: u32,
    payload: &OfferPayload,
    brief: Option<&MeetingBrief>,
    now: jiff::Timestamp,
    zone: &TimeZone,
) -> crate::panel::Panel {
    use crate::panel::{Action, Owner, Panel, Tone};
    let action = |id: &str, label: &str, primary: bool| Action {
        id: id.into(),
        label: label.into(),
        primary,
        copy: None,
    };
    let counting = phase == CardPhase::Countdown;
    Panel {
        id: payload.key.clone(),
        owner: Owner::Meeting,
        tone: if counting { Tone::Attention } else { Tone::Neutral },
        kicker: kicker(phase, payload, now),
        title: payload.title.clone(),
        count: counting.then(|| format!("{seconds}s")),
        body: card_body(payload, brief, zone),
        lines: Vec::new(),
        meter: false,
        sections: brief.map(brief_sections).unwrap_or_default(),
        actions: match phase {
            CardPhase::Join => {
                let first = if payload.join_url.is_some() {
                    "Join and start recording"
                } else {
                    "Start recording"
                };
                vec![action("join", first, true), action("notNow", "Not now", false)]
            }
            CardPhase::Countdown => {
                vec![action("take", "Take notes", true), action("notNow", "Cancel", false)]
            }
            CardPhase::Ask => {
                vec![action("take", "Take notes", true), action("notNow", "Not now", false)]
            }
        },
        busy: false,
    }
}

/// The small line above the card's title: "Meeting · in 2 min", or, once the
/// countdown is running, what it counts to. A call with no event behind it is
/// a "Call", and has no time to say.
fn kicker(phase: CardPhase, payload: &OfferPayload, now: jiff::Timestamp) -> String {
    if phase == CardPhase::Countdown {
        return "Recording starts in".to_string();
    }
    let what = if payload.event_id.is_none() { "Call" } else { "Meeting" };
    match payload.start {
        Some(start) => format!("{what} \u{b7} {}", timing(start, now)),
        None => what.to_string(),
    }
}

/// When a meeting is, said from where the person stands: "in 2 min", "now",
/// "started 3 min ago". A minute is whole minutes, to the nearest, so "now"
/// is the half-minute either side of the start.
fn timing(start: jiff::Timestamp, now: jiff::Timestamp) -> String {
    let secs = start.as_second() - now.as_second();
    match ((secs.abs() + 30) / 60, secs > 0) {
        (0, _) => "now".to_string(),
        (minutes, true) => format!("in {}", span_words(minutes)),
        (minutes, false) => format!("started {} ago", span_words(minutes)),
    }
}

/// "12 min", "1 h", "2 h 5 min".
fn span_words(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// The card's body: the meeting's hours in the person's own time and where
/// it is, then, on a line of its own, what the brief says it is for.
fn card_body(
    payload: &OfferPayload,
    brief: Option<&MeetingBrief>,
    zone: &TimeZone,
) -> Option<String> {
    let hours = match (payload.start, payload.end) {
        (Some(start), Some(end)) => Some(format!(
            "{}\u{2013}{}",
            start.to_zoned(zone.clone()).strftime("%H:%M"),
            end.to_zoned(zone.clone()).strftime("%H:%M")
        )),
        (Some(start), None) => Some(start.to_zoned(zone.clone()).strftime("%H:%M").to_string()),
        _ => None,
    };
    let place = brief
        .and_then(|b| b.location.as_deref())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .or_else(|| payload.join_url.as_deref().and_then(join_service).map(str::to_owned));
    let first = [hours, place].into_iter().flatten().collect::<Vec<_>>().join(" \u{b7} ");
    let summary = brief.and_then(|b| b.summary.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    let lines: Vec<&str> =
        [Some(first.as_str()).filter(|f| !f.is_empty()), summary].into_iter().flatten().collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The most agenda lines the card shows. The brief keeps more, for the
/// calendar; a card with room for twelve would be a page.
const CARD_AGENDA: usize = 5;
/// More people than this are folded under "More".
const CARD_PEOPLE_OPEN: usize = 3;

/// What the brief adds to the card, in the order that matters walking in:
/// what to raise (the boxes), the agenda, who is there, and the context,
/// which is the longest and so the one folded away. A part with nothing in it
/// is left out.
fn brief_sections(brief: &MeetingBrief) -> Vec<crate::panel::Section> {
    use crate::panel::{Check, Item, Section};
    let plain = |text: &String| Item { text: text.trim().to_string(), detail: None, check: None };
    let lines = |lines: &[String]| -> Vec<Item> {
        lines.iter().filter(|l| !l.trim().is_empty()).map(plain).collect()
    };
    let to_raise: Vec<Item> = brief
        .talking_points
        .iter()
        .map(|point| Item {
            text: point.text.trim().to_string(),
            detail: Some(point.with.trim()).filter(|w| !w.is_empty()).map(|w| format!("with {w}")),
            check: Some(Check { action: format!("done:{}", point.task_id), done: point.done }),
        })
        .collect();
    let mut agenda = lines(&brief.agenda);
    agenda.truncate(CARD_AGENDA);
    let people: Vec<Item> = brief
        .people
        .iter()
        .map(|p| Item {
            text: p.name.trim().to_string(),
            detail: Some(p.line.trim()).filter(|l| !l.is_empty()).map(str::to_owned),
            check: None,
        })
        .collect();
    let folded_people = people.len() > CARD_PEOPLE_OPEN;
    [
        ("To raise", to_raise, false),
        ("Agenda", agenda, false),
        ("With", people, folded_people),
        ("Context", lines(&brief.context), true),
    ]
    .into_iter()
    .filter(|(_, items, _)| !items.is_empty())
    .map(|(heading, items, folded)| Section { heading: heading.to_string(), items, folded })
    .collect()
}

/// A button on a meeting card was pressed, or a box on it ticked. See
/// [`crate::panel::on_action`].
///
/// A button is answered as the notification's would be. A box is a talking
/// point -- `done:{taskId}` -- and ticking it completes the task.
pub fn on_panel_action(app: &AppHandle, key: &str, action: &str) {
    if matches!(action, "take" | "join" | "notNow") {
        on_notice_action(app, key, action);
    } else if let Some(task) = point_of(action) {
        let (app, key) = (app.clone(), key.to_string());
        tauri::async_runtime::spawn(async move { complete_point(&app, &key, task).await });
    } else {
        tracing::debug!(key, action, "a press on a meeting card that means nothing");
    }
}

/// The task a talking point's `done:{taskId}` action names.
fn point_of(action: &str) -> Option<TaskId> {
    action.strip_prefix("done:")?.parse().ok()
}

/// Complete the task behind a talking point and tick the point. A failure
/// leaves it unticked, and the card as it was: the person can try again, and a
/// card that said done about a task that is not would be worse than one that
/// said nothing.
///
/// Only a task that is a point on this card's own brief is completed: the
/// action comes from the page, and what a card may do is what it shows.
async fn complete_point(app: &AppHandle, key: &str, task: TaskId) {
    let Some(done) = offers(app).with(|o| o.point(key, task)) else {
        tracing::debug!(key, task = %task, "a talking point that is not on the card");
        return;
    };
    // Ticked already, in the brief: the page drew the box again, and what is
    // wanted is the card as it was.
    if !done && let Err(e) = finish_task(app, task).await {
        tracing::warn!(error = %e, key, task = %task, "could not complete a talking point's task");
        return;
    }
    offers(app).with(|o| o.tick_point(key, task));
    redraw(app, key);
}

/// Mark task `id` done, the way the interface does: the task as the service
/// has it, made done ([`mark_done`]), and saved back whole.
///
/// The task is edited as the JSON the service sent, not decoded into this
/// build's [`karmayoga_core::Task`] and encoded again, for the reason
/// [`turn_mic_offers_off`] gives: a field a newer vault's task has, which
/// this build does not know, would be saved away. There is no command that
/// changes a task's status alone.
async fn finish_task(app: &AppHandle, id: TaskId) -> CommandResult<()> {
    let session = app.state::<AppState>().session().as_session();
    let mut task = session.call(Ctx::local(), "get_task", json!({ "id": id })).await?;
    mark_done(&mut task, jiff::Timestamp::now())?;
    session.call(Ctx::local(), "save_task", json!({ "task": task })).await?;
    Ok(())
}

/// `task`, as `get_task` sent it, with what finishing it changes and nothing
/// else: `status`, `completedAt` and `updatedAt`. This is what
/// [`karmayoga_core::Completable::set_status`] does to a decoded task --
/// stamped `now` the first time it is done, and left alone if it already was,
/// so "finished on Tuesday" does not become "finished on Friday" -- and what
/// the interface's `toggleDoneOf` writes.
fn mark_done(task: &mut Value, now: jiff::Timestamp) -> CommandResult<()> {
    let Some(fields) = task.as_object_mut() else {
        return Err(CommandError::new("invalid", "get_task answered oddly"));
    };
    let stamped = fields.get("completedAt").filter(|at| !at.is_null()).cloned();
    fields.insert("status".to_string(), json!(TaskStatus::Done));
    fields.insert("completedAt".to_string(), stamped.unwrap_or_else(|| json!(now)));
    fields.insert("updatedAt".to_string(), json!(now));
    Ok(())
}

/// A meeting's brief is ready (`karmayoga_service::meeting::brief`): kept for
/// its offer, and drawn on the card if the card is up.
pub fn on_meeting_brief(app: &AppHandle, brief: MeetingBrief) {
    let key = brief.key.clone();
    offers(app).with(|o| o.keep_brief(brief, jiff::Timestamp::now()));
    redraw(app, &key);
}

/// A card has gone up for `payload` with no brief behind it: ask the service
/// once for one it has already prepared, and draw the card again if there is
/// one. The service answers from what it holds and never waits on a model, so
/// this is quick, but it is a call to a service that may be remote, and the
/// card goes up without waiting for it.
///
/// Once, not on every change of face: the countdown redraws the card five
/// times a second, from the brief kept here.
fn fetch_brief(app: &AppHandle, payload: &OfferPayload) {
    let (Some(calendar_id), Some(uid)) = (payload.calendar_id, payload.uid.clone()) else {
        return;
    };
    if offers(app).with(|o| o.briefs.contains_key(&payload.key)) {
        return;
    }
    let (app, key) = (app.clone(), payload.key.clone());
    tauri::async_runtime::spawn(async move {
        let session = app.state::<AppState>().session().as_session();
        let args = json!({ "calendarId": calendar_id, "uid": uid });
        let brief = match session.call(Ctx::local(), "meeting_brief", args).await {
            Ok(Value::Null) => return,
            Ok(value) => match serde_json::from_value::<MeetingBrief>(value) {
                Ok(brief) => brief,
                Err(e) => {
                    tracing::debug!(error = %e, "meeting_brief answered oddly");
                    return;
                }
            },
            Err(e) => {
                tracing::debug!(error = %e, "no brief for the meeting card");
                return;
            }
        };
        // Answered, or withdrawn, while the service was deciding: a brief
        // kept for an offer that is gone would sit there until its meeting
        // ended.
        if offers(&app).with(|o| o.book.find(&key, jiff::Timestamp::now()).is_none()) {
            return;
        }
        on_meeting_brief(&app, brief);
    });
}

/// The meeting the card asks to join has been joined from the meeting
/// itself -- the microphone went live -- so the card moves on to the joined
/// face: the countdown with auto-record on, the ask to take notes without it.
fn note_joined(app: &AppHandle, key: &str) {
    let Some(payload) = offers(app).with(|o| o.asking(key, jiff::Timestamp::now())) else {
        return;
    };
    match card_phase(payload.auto_record, payload.ready, true) {
        Some(CardPhase::Countdown) => arm_countdown(app, key),
        Some(phase) => show_still(app, key, phase),
        None => {}
    }
}

fn arm_countdown(app: &AppHandle, key: &str) {
    let generation = offers(app).with(|o| {
        o.countdown_gen = o.countdown_gen.wrapping_add(1);
        o.countdown_gen
    });
    let deadline = std::time::Instant::now() + COUNTDOWN;
    show_card(app, key, CardPhase::Countdown, seconds_left(deadline));
    let app = app.clone();
    let key = key.to_string();
    tauri::async_runtime::spawn(async move {
        let zone = TimeZone::system();
        loop {
            let left = seconds_left(deadline);
            if left == 0 {
                break;
            }
            // Drawn from the brief kept, never fetched: this is five times a
            // second, and a card that has been replaced or cancelled stops
            // the count.
            if !tick_card(&app, &key, generation, left, &zone) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if !countdown_live(&app, &key, generation) {
            return;
        }
        if let Err(e) = answer(&app, &key, OfferAnswer::Take).await {
            // Cancel, lock and expiry settle the offer while this is
            // deciding to start. That is not a failure to tell anyone about.
            if e.code == "not_found" {
                return;
            }
            // `answer` has put the card back up, as the ask.
            report_failure(&app, &key, &e);
        }
    });
}

fn seconds_left(deadline: std::time::Instant) -> u32 {
    let ms = deadline.saturating_duration_since(std::time::Instant::now()).as_millis();
    u32::try_from(ms.div_ceil(1000)).unwrap_or(u32::MAX)
}

fn countdown_live(app: &AppHandle, key: &str, generation: u64) -> bool {
    offers(app).with(|o| {
        o.countdown_gen == generation && o.book.find(key, jiff::Timestamp::now()).is_some()
    })
}

/// One tick of `key`'s countdown, drawn in place. `false` if the card is no
/// longer the one `generation` is counting for -- cancelled, answered,
/// replaced -- which ends the count. Only ever changes a card that is up: a
/// tick landing as the card is answered must not put it back.
fn tick_card(app: &AppHandle, key: &str, generation: u64, seconds: u32, zone: &TimeZone) -> bool {
    let now = jiff::Timestamp::now();
    let Some(panel) = offers(app).with(|o| o.tick(key, generation, seconds, now, zone)) else {
        return false;
    };
    emit(app, COUNTDOWN_EVENT, &json!({ "key": key, "seconds": seconds }));
    crate::panel::update(app, key, |shown| *shown = panel);
    true
}

/// Take the card down if it is `key`'s, stopping its countdown. The face it
/// wore, if it was up.
fn drop_notice(app: &AppHandle, key: &str) -> Option<CardPhase> {
    let was = offers(app).with(|o| {
        if o.notice.as_ref().is_none_or(|n| n.key != key) {
            return None;
        }
        o.countdown_gen = o.countdown_gen.wrapping_add(1);
        o.notice.take().map(|n| n.phase)
    })?;
    crate::panel::dismiss(app, key);
    emit(app, COUNTDOWN_EVENT, &json!({ "key": key, "seconds": null }));
    Some(was)
}

/// A notification, unless the window is in front of the person already --
/// visible and focused -- in which case the banner is, and a second
/// announcement of the same thing is noise.
///
/// Asking the window whether it is in front waits on the main thread, and
/// this is reached from the service's event sink in the middle of the
/// meeting watcher's tick, so the asking happens on a blocking thread of its
/// own rather than holding that tick up.
fn raise_notice(app: &AppHandle, key: &str, text: &NoticeText) {
    let (app, key, text) = (app.clone(), key.to_string(), text.clone());
    tauri::async_runtime::spawn_blocking(move || {
        if window_in_front(&app) {
            return;
        }
        crate::notice::show(&app, &key, &text.summary, &text.body, &text.actions);
    });
}

fn window_in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
}

fn emit<S: serde::Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!(error = %e, event, "a meeting offer event could not reach the interface");
    }
}

// ---- settling ---------------------------------------------------------------

/// An offer is over: take it out of the pending set, and take down every
/// view of it. `false` if it was not pending (already answered somewhere,
/// or already withdrawn), in which case nothing is said -- a second
/// `settled` for a key the interface has forgotten is harmless but noise.
fn settle(app: &AppHandle, key: &str) -> bool {
    let removed = offers(app).with(|o| {
        o.briefs.remove(key);
        o.book.remove(key)
    });
    if removed.is_some() {
        announce_settled(app, key);
        drop_notice(app, key);
        refresh_tray(app);
    }
    removed.is_some()
}

/// Settle every pending offer `pick` chooses. One tray update for the lot.
fn settle_where(app: &AppHandle, pick: impl Fn(&Pending) -> bool) {
    let taken = offers(app).with(|o| {
        let taken = o.book.take_where(pick);
        for p in &taken {
            o.briefs.remove(&p.payload.key);
        }
        taken
    });
    if taken.is_empty() {
        return;
    }
    for p in &taken {
        announce_settled(app, &p.payload.key);
        drop_notice(app, &p.payload.key);
    }
    refresh_tray(app);
}

fn announce_settled(app: &AppHandle, key: &str) {
    crate::notice::withdraw(app, key);
    emit(app, SETTLED_EVENT, json!({ "key": key }));
}

/// Bring the tray's "Take notes for ..." item in line with the pending set.
/// On a blocking thread, for the reason [`crate::tray::Tray::schedule_refresh`]
/// gives: building a menu waits on the main thread, and callers here run on
/// the async runtime's own workers.
fn refresh_tray(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(tray) = app.try_state::<crate::tray::Tray>() else { return };
        let offers = offers(&app);
        let _gate = offers.tray.lock().unwrap();
        let title =
            offers.with(|o| o.book.oldest_ready(jiff::Timestamp::now()).map(|p| p.title.clone()));
        if let Err(e) = tray.set_offer(&app, title.as_deref()) {
            tracing::debug!(error = %e.message, "the tray's offer item could not be updated");
        }
    });
}

// ---- the microphone ---------------------------------------------------------

/// Another application has held the microphone long enough to look like a
/// call. `app_name` is its name, when the platform says. Called by
/// [`crate::micwatch`].
///
/// If something is already being recorded, that recording is now tied to
/// this call -- it will stop when the call does -- and nothing is offered.
/// Otherwise the service is asked ([`MeetingNow`]) whether to offer at all
/// and whether a calendar meeting is on, which decides between an offer
/// about that meeting and one about "a call". The service is asked rather
/// than the settings read here so that a remote vault and a local one are
/// treated alike, and so there is one definition of "a meeting is on now".
pub fn on_mic_started(app: &AppHandle, app_name: Option<String>) {
    offers(app).with(|o| o.mic_app = app_name.clone());
    if crate::meeting::state_already_recording(app) {
        // A call joined a few minutes after the notes were started (the
        // calendar's offer was taken, then the person clicked the link) is
        // that recording's call. One that turns up well into a recording is
        // something else -- a voice message sent during an in-room meeting
        // -- and its end must not end the meeting's notes.
        offers(app).with(|o| {
            if o.recording_started.is_some_and(|at| at.elapsed() <= LINK_WINDOW) {
                o.mic_linked = true;
            }
        });
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { mic_started(&app, app_name).await });
}

async fn mic_started(app: &AppHandle, app_name: Option<String>) {
    let session = app.state::<AppState>().session().as_session();
    let now: MeetingNow = match session.call(Ctx::local(), "meeting_now", json!({})).await {
        Ok(value) => match serde_json::from_value(value) {
            Ok(now) => now,
            Err(e) => {
                tracing::debug!(error = %e, "meeting_now answered oddly");
                return;
            }
        },
        // Locked, no vault open, a remote that is away: no offer, and no
        // fuss -- the person has not asked for anything.
        // Logged at info, as is the settings' no below: the journal shows the
        // microphone being noticed, and an offer that then never comes has
        // to say why there too.
        Err(e) => {
            tracing::info!(error = %e, "no microphone offer: the service did not answer");
            return;
        }
    };
    if !now.offers || !now.mic_offer {
        tracing::info!(
            offers = now.offers,
            mic_offer = now.mic_offer,
            "no microphone offer: turned off in Settings, Meetings"
        );
        return;
    }
    // The answer took a moment, and the world moved: somebody may have
    // started a recording by hand, or the call may already be over.
    if crate::meeting::state_already_recording(app) {
        offers(app).with(|o| {
            if o.recording_started.is_some_and(|at| at.elapsed() <= LINK_WINDOW) {
                o.mic_linked = true;
            }
        });
        return;
    }
    let still_on = app.try_state::<crate::micwatch::MicWatch>().is_some_and(|m| m.session_active());
    if !still_on {
        return;
    }
    match now.event {
        Some(event) => {
            let payload = calendar_payload(&event, OfferSource::Microphone, app_name);
            // The microphone going live is the join. A recording already
            // running holds the offer back, and that one is asked rather
            // than counted down the moment the previous recording stops.
            offer_event(app, payload, event.end, true);
        }
        None => {
            let mut payload =
                offers(app).with(|o| mic_payload(o.book.next_mic_number(), app_name, now.ready));
            payload.auto_record = now.auto_record;
            raise(app, payload, None, true);
        }
    }
}

/// The microphone has been free of other applications long enough that the
/// call is over. Called by [`crate::micwatch`].
///
/// Every offer that was made because of the call is withdrawn. A recording
/// that was started during it -- see [`on_recording_started`] -- is
/// stopped, if the person has "stop when the call ends" on
/// (`MeetingSettings::auto_stop`, read the way `meeting::read_auto_stop`
/// does at the start of a recording). Stopped, never discarded: the notes
/// of a call that has ended are exactly what was asked for.
pub fn on_mic_ended(app: &AppHandle) {
    let withdrawn: Vec<String> = offers(app).with(|o| {
        o.book
            .pending
            .iter()
            .filter(|p| p.payload.source == OfferSource::Microphone && p.payload.event_id.is_some())
            .map(|p| p.payload.key.clone())
            .collect()
    });
    settle_where(app, |p| p.payload.source == OfferSource::Microphone);
    offers(app).with(|o| withdrawn.iter().for_each(|key| o.book.forget_offered(key)));
    // A calendar meeting's card that began counting down, or asking, when the
    // call was joined goes back to asking to join: the call is over, and
    // nobody said to record an empty room.
    if let Some(payload) = offers(app).with(|o| o.joined(jiff::Timestamp::now())) {
        show_still(app, &payload.key, CardPhase::Join);
    }
    let (linked, name) =
        offers(app).with(|o| (std::mem::take(&mut o.mic_linked), o.mic_app.take()));
    if !linked || !crate::meeting::state_already_recording(app) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let session = app.state::<AppState>().session().as_session();
        if !crate::meeting::read_auto_stop(&session).await {
            return;
        }
        let title = app
            .state::<AppState>()
            .capture()
            .lock()
            .unwrap()
            .as_ref()
            .and_then(crate::capture::CaptureSlot::handle)
            .and_then(|h| h.status())
            .map(|s| s.title);
        match crate::meeting::stop(&app, false).await {
            Ok(()) => {
                let text = call_ended_text(title.as_deref().unwrap_or("the call"), name.as_deref());
                offers(&app).with(|o| o.shown.push(CALL_ENDED_KEY.to_string()));
                raise_notice(&app, CALL_ENDED_KEY, &text);
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not stop the recording when the call ended")
            }
        }
    });
}

/// A recording has started, by any route. Called by
/// [`crate::meeting::begin`] once it succeeds.
///
/// Remembers whether a call is on, so [`on_mic_ended`] knows this recording
/// belongs to it; and withdraws every pending offer, since there is one
/// microphone and it is spoken for.
pub(crate) fn on_recording_started(app: &AppHandle) {
    let in_call = app.try_state::<crate::micwatch::MicWatch>().is_some_and(|m| m.session_active());
    offers(app).with(|o| {
        o.mic_linked = in_call;
        o.recording_started = Some(std::time::Instant::now());
    });
    settle_where(app, |_| true);
}

/// A recording has stopped, by any route: [`crate::meeting::stop`], or the
/// capture thread ending on its own (`capture.rs`'s `clear_stale_slot`).
///
/// "Taking notes for ..." comes down, since it is no longer true and its
/// Stop button would otherwise stop whatever is recorded next; and the
/// offers held back while this was recording -- see `Book::deferred` -- are
/// made now, if their meetings are not over.
pub(crate) fn on_recording_stopped(app: &AppHandle) {
    let (stale, deferred) = offers(app).with(|o| {
        o.mic_linked = false;
        o.recording_started = None;
        let stale: Vec<String> = o.shown.iter().filter(|k| *k != CALL_ENDED_KEY).cloned().collect();
        o.shown.retain(|k| k == CALL_ENDED_KEY);
        (stale, o.book.take_deferred(jiff::Timestamp::now()))
    });
    for key in stale {
        crate::notice::withdraw(app, &key);
    }
    for p in deferred {
        // Asked, not counted down: a recording that begins the moment the
        // previous one stops was the thing `defer` exists to prevent.
        raise(app, p.payload, p.expires, false);
    }
}

/// The vault locked: every offer names a meeting, which is decrypted
/// contents of the vault, so every one goes -- pending, held back, on the
/// tray and in the notification centre -- the way a toast quoting a title
/// does in the window. So do the briefs, which say who was to be at them and
/// what was to be raised. The interface empties its own list on lock already.
pub(crate) fn on_locked(app: &AppHandle) {
    let shown = offers(app).with(|o| {
        o.book.deferred.clear();
        o.briefs.clear();
        o.countdown_gen = o.countdown_gen.wrapping_add(1);
        o.notice = None;
        std::mem::take(&mut o.shown)
    });
    crate::panel::dismiss_owner(app, crate::panel::Owner::Meeting);
    emit(app, COUNTDOWN_EVENT, &json!({ "key": "", "seconds": null }));
    for key in shown {
        crate::notice::withdraw(app, &key);
    }
    settle_where(app, |_| true);
}

// ---- answering --------------------------------------------------------------

/// A button pressed on a desktop notification raised by [`crate::notice`].
/// `action` is the action key the notice was raised with, or `"default"`
/// for a click on its body.
///
/// A press is answered exactly as the banner's would be; if that fails
/// there is no dialog to put the reason in, so the window is raised and told
/// ([`FAILED_EVENT`]), and the offer stays pending for another try.
pub fn on_notice_action(app: &AppHandle, key: &str, action: &str) {
    let answer = match action {
        "take" => OfferAnswer::Take,
        "join" => OfferAnswer::Join,
        "notNow" => OfferAnswer::NotNow,
        "never" => OfferAnswer::Never,
        "setup" => {
            crate::tray::raise_window(app);
            emit(app, SETUP_EVENT, json!({ "key": key }));
            return;
        }
        "stop" => {
            crate::meeting::stop_from_tray(app.clone());
            return;
        }
        // "default", and anything a daemon invents: a click on the body.
        _ => {
            crate::tray::raise_window(app);
            return;
        }
    };
    let app = app.clone();
    let key = key.to_string();
    tauri::async_runtime::spawn(async move {
        match self::answer(&app, &key, answer).await {
            // Answered already -- by the countdown running out as the button
            // was pressed, or elsewhere. Nothing went wrong.
            Err(e) if e.code == "not_found" => {}
            Err(e) => report_failure(&app, &key, &e),
            Ok(_) => {}
        }
    });
}

/// Raise the window and tell it an answer given outside it failed. `key` is
/// empty for the tray's "Take notes now", which has no offer.
fn report_failure(app: &AppHandle, key: &str, error: &CommandError) {
    tracing::warn!(error = %error, key, "answering a meeting offer failed");
    crate::tray::raise_window(app);
    emit(app, FAILED_EVENT, json!({ "key": key, "message": error.message }));
}

/// The tray's "Take notes for ..." item: answer the oldest pending offer
/// with "take". If it has gone since the menu was drawn, take notes of
/// whatever is happening, as "Take notes now" would.
pub(crate) fn take_oldest(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // The same pick `refresh_tray` named the item after, so the click
        // answers the meeting the menu showed.
        let oldest = offers(&app).with(|o| {
            let now = jiff::Timestamp::now();
            o.book.oldest_ready(now).or_else(|| o.book.oldest(now)).cloned()
        });
        let Some(oldest) = oldest else {
            take_now_inner(&app).await;
            return;
        };
        if !oldest.ready {
            // Taking notes would only fail; the thing to do is set it up.
            crate::tray::raise_window(&app);
            emit(&app, SETUP_EVENT, json!({ "key": oldest.key }));
            return;
        }
        if let Err(e) = answer(&app, &oldest.key, OfferAnswer::Take).await {
            report_failure(&app, &oldest.key, &e);
        }
    });
}

/// The tray's "Take notes now": a recording with no meeting behind it.
pub(crate) fn take_now(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { take_now_inner(&app).await });
}

async fn take_now_inner(app: &AppHandle) {
    if let Err(e) = crate::meeting::begin(app.clone(), Ctx::local(), None, None, None, false).await
    {
        report_failure(app, "", &e);
    }
}

/// Answer an offer from the interface. `Some(recording)` once one started.
///
/// An error leaves the offer pending, so the banner can say what went wrong
/// and let the person try again. On success the offer is settled
/// everywhere: the interface hears `settled`, the notification comes down
/// and the tray item goes.
#[tauri::command]
pub async fn meeting_offer_answer(
    app: AppHandle,
    key: String,
    answer: OfferAnswer,
) -> CommandResult<Option<Recording>> {
    self::answer(&app, &key, answer).await
}

async fn answer(
    app: &AppHandle,
    key: &str,
    answer: OfferAnswer,
) -> CommandResult<Option<Recording>> {
    let pending = offers(app).with(|o| o.book.find(key, jiff::Timestamp::now()).cloned());
    let Some(Pending { payload, .. }) = pending else {
        return Err(CommandError::new("not_found", "that offer has already gone"));
    };
    let session = app.state::<AppState>().session().as_session();

    let recording = match answer {
        OfferAnswer::Take => start(app, &payload).await?,
        OfferAnswer::Join => {
            if let Some(url) = payload.join_url.clone()
                && let Err(e) = app.opener().open_url(url, None::<&str>)
            {
                // Not a reason to withhold the notes: the person pressed
                // "Join & take notes", and the second half still stands.
                tracing::warn!(error = %e, "could not open the meeting link");
            }
            start(app, &payload).await?
        }
        OfferAnswer::NotNow => {
            if let (Some(calendar_id), Some(uid)) = (payload.calendar_id, &payload.uid) {
                dismiss(&session, calendar_id, uid, payload.series.as_deref(), false).await?;
            }
            None
        }
        OfferAnswer::Never => {
            match (payload.source, payload.calendar_id, &payload.uid) {
                (OfferSource::Calendar, Some(calendar_id), Some(uid)) => {
                    dismiss(&session, calendar_id, uid, payload.series.as_deref(), true).await?;
                }
                _ => turn_mic_offers_off(&session).await?,
            }
            None
        }
    };
    // Being started by the other of a press and the countdown, which settles
    // it -- or, if starting fails, leaves it pending to be tried again.
    if recording.is_none() && matches!(answer, OfferAnswer::Take | OfferAnswer::Join) {
        return Ok(None);
    }
    // After a recording starts `on_recording_started` has already settled
    // this offer, along with the rest; this finds nothing to do then.
    settle(app, key);
    Ok(recording)
}

/// Start recording now -- a press, or the countdown running out. The countdown
/// is only the time to cancel in when nobody has said anything, so a press
/// does not wait for it: the card goes at once, and its countdown with it,
/// which would otherwise start a second recording when it ran out.
///
/// `None` if this offer's recording is already being started: the other
/// start answers for both. If starting fails, the card comes back as the ask
/// -- the offer is still pending -- so it can be pressed again.
async fn start(app: &AppHandle, payload: &OfferPayload) -> CommandResult<Option<Recording>> {
    let key = &payload.key;
    if !offers(app).with(|o| o.claim_start(key)) {
        return Ok(None);
    }
    let had_card = drop_notice(app, key);
    let result = take(app, payload).await;
    offers(app).with(|o| o.release_start(key));
    if result.is_err()
        && let Some(face) = had_card
    {
        let pending = offers(app).with(|o| o.book.find(key, jiff::Timestamp::now()).is_some());
        if pending {
            show_still(app, key, face.asking());
        }
    }
    result.map(Some)
}

async fn take(app: &AppHandle, payload: &OfferPayload) -> CommandResult<Recording> {
    crate::meeting::begin(
        app.clone(),
        Ctx::local(),
        payload.event_id,
        Some(payload.title.clone()),
        None,
        false,
    )
    .await
}

async fn dismiss(
    session: &crate::remote::Session,
    calendar_id: CalendarId,
    uid: &str,
    series: Option<&str>,
    never: bool,
) -> CommandResult<()> {
    session
        .call(
            Ctx::local(),
            "dismiss_meeting_offer",
            json!({ "calendarId": calendar_id, "uid": uid, "series": series, "never": never }),
        )
        .await
        .map(|_| ())
}

/// "Never" on a microphone offer: stop offering when the microphone turns
/// on, for good -- `MeetingSettings::mic_offer`.
///
/// The settings are read, one field changed on the JSON as the service
/// sent it, and sent back whole, rather than decoded into a
/// `MeetingSettings` and re-encoded: a field this build does not know,
/// because the vault is a newer one's, would otherwise be saved away.
async fn turn_mic_offers_off(session: &crate::remote::Session) -> CommandResult<()> {
    let view = session.call(Ctx::local(), "meeting_settings", json!({})).await?;
    let mut settings = view
        .get("settings")
        .cloned()
        .ok_or_else(|| CommandError::new("invalid", "meeting_settings answered oddly"))?;
    let Some(map) = settings.as_object_mut() else {
        return Err(CommandError::new("invalid", "meeting_settings answered oddly"));
    };
    map.insert("micOffer".to_string(), Value::Bool(false));
    session.call(Ctx::local(), "save_meeting_settings", json!({ "settings": settings })).await?;
    Ok(())
}

/// Every offer still waiting for an answer, oldest first -- for an
/// interface that started after one was raised.
#[tauri::command]
pub async fn meeting_offers(app: AppHandle) -> CommandResult<Vec<OfferPayload>> {
    Ok(offers(&app).with(|o| o.book.payloads(jiff::Timestamp::now())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Panel;
    use karmayoga_core::Completable;
    use karmayoga_core::meeting::brief::{MeetingBriefPerson, TalkingPoint};

    fn at(secs: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(secs).unwrap()
    }

    fn meeting(automatic: bool, ready: bool) -> MeetingOffer {
        MeetingOffer {
            event_id: EventId::new(),
            calendar_id: CalendarId::new(),
            uid: "evt-1@example.com".into(),
            series: None,
            title: "Design sync".into(),
            start: at(1_000),
            end: at(4_600),
            calendar_name: "Work".into(),
            automatic,
            join_url: None,
            ready,
            auto_record: false,
        }
    }

    // ---- when capture starts by itself ------------------------------------

    #[test]
    fn an_always_offer_starts_automatically() {
        assert!(starts_automatically(&meeting(true, true)));
    }

    #[test]
    fn an_ask_offer_does_not_start_on_its_own() {
        assert!(!starts_automatically(&meeting(false, true)));
    }

    #[test]
    fn a_ready_offer_is_always_a_card_and_counts_down_only_with_auto_record() {
        for auto_record in [false, true] {
            assert!(card_phase(auto_record, false, true).is_none(), "not ready: set up first");
            assert!(card_phase(auto_record, false, false).is_none());
            assert_eq!(
                card_phase(auto_record, true, false),
                Some(CardPhase::Join),
                "before the call is joined it asks to join, whatever the setting"
            );
        }
        assert_eq!(card_phase(true, true, true), Some(CardPhase::Countdown));
        assert_eq!(
            card_phase(false, true, true),
            Some(CardPhase::Ask),
            "joined with auto-record off: asks, and nothing starts by itself"
        );
    }

    #[test]
    fn a_failed_start_goes_back_to_asking_and_never_to_a_second_countdown() {
        assert_eq!(CardPhase::Join.asking(), CardPhase::Join);
        assert_eq!(CardPhase::Countdown.asking(), CardPhase::Ask);
        assert_eq!(CardPhase::Ask.asking(), CardPhase::Ask);
    }

    #[test]
    fn an_offer_that_is_not_ready_never_starts_on_its_own() {
        assert!(!starts_automatically(&meeting(true, false)));
        assert!(!calendar_payload(&meeting(true, false), OfferSource::Calendar, None).automatic);
    }

    // ---- keys and payloads -----------------------------------------------

    #[test]
    fn an_event_offer_is_keyed_by_calendar_and_uid_however_it_was_noticed() {
        let offer = meeting(false, true);
        let by_calendar = calendar_payload(&offer, OfferSource::Calendar, None);
        let by_mic = calendar_payload(&offer, OfferSource::Microphone, Some("Zoom".into()));
        assert_eq!(by_calendar.key, format!("event:{}:evt-1@example.com", offer.calendar_id));
        assert_eq!(by_calendar.key, by_mic.key);
        assert_eq!(by_mic.app.as_deref(), Some("Zoom"));
        assert_eq!(by_mic.source, OfferSource::Microphone);
    }

    #[test]
    fn a_call_with_no_event_is_named_for_the_application() {
        let p = mic_payload(3, Some("Zoom".into()), true);
        assert_eq!(p.key, "mic:3");
        assert_eq!(p.title, "Call on Zoom");
        assert!(p.event_id.is_none() && p.calendar_id.is_none() && p.uid.is_none());
        assert_eq!(mic_payload(4, None, true).title, "Call");
    }

    #[test]
    fn the_payload_keeps_the_wire_names_the_interface_reads() {
        let p = calendar_payload(&meeting(false, true), OfferSource::Calendar, None);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["source"], "calendar");
        assert!(v.get("calendarName").is_some() && v.get("joinUrl").is_some());
        assert!(v.get("recordingId").is_none());
        assert_eq!(serde_json::to_value(OfferSource::Microphone).unwrap(), "microphone");
    }

    #[test]
    fn answers_read_as_the_interface_spells_them() {
        for (text, answer) in [
            ("take", OfferAnswer::Take),
            ("join", OfferAnswer::Join),
            ("notNow", OfferAnswer::NotNow),
            ("never", OfferAnswer::Never),
        ] {
            assert_eq!(serde_json::from_value::<OfferAnswer>(json!(text)).unwrap(), answer);
        }
    }

    // ---- what the notification says ----------------------------------------

    fn keys(text: &NoticeText) -> Vec<&'static str> {
        text.actions.iter().map(|(k, _)| *k).collect()
    }

    #[test]
    fn a_ready_calendar_offer_asks_to_take_notes() {
        let p = calendar_payload(&meeting(false, true), OfferSource::Calendar, None);
        let text = notice_text(&p, None);
        assert_eq!(text.summary, "Take notes for Design sync?");
        assert_eq!(text.body, "Work \u{b7} starting now");
        assert_eq!(keys(&text), ["take", "notNow"]);
    }

    #[test]
    fn a_join_link_puts_join_first() {
        let mut offer = meeting(false, true);
        offer.join_url = Some("https://meet.example.com/abc".into());
        let text = notice_text(&calendar_payload(&offer, OfferSource::Calendar, None), None);
        assert_eq!(keys(&text), ["join", "take", "notNow"]);
        assert_eq!(text.actions[0].1, "Join & take notes");
    }

    #[test]
    fn a_meeting_the_microphone_noticed_says_which_application() {
        let p =
            calendar_payload(&meeting(false, true), OfferSource::Microphone, Some("Zoom".into()));
        let text = notice_text(&p, None);
        assert_eq!(text.summary, "Take notes for Design sync?");
        assert_eq!(text.body, "Zoom is using your microphone");
        let anon = calendar_payload(&meeting(false, true), OfferSource::Microphone, None);
        assert_eq!(notice_text(&anon, None).body, "Another app is using your microphone");
    }

    #[test]
    fn a_call_with_no_event_asks_whether_it_is_one() {
        let text = notice_text(&mic_payload(1, Some("Zoom".into()), true), None);
        assert_eq!(text.summary, "Taking a call? Take notes");
        assert_eq!(text.body, "Zoom is using your microphone.");
        assert_eq!(keys(&text), ["take", "notNow"]);
        let anon = notice_text(&mic_payload(2, None, true), None);
        assert_eq!(anon.body, "Another app is using your microphone.");
    }

    #[test]
    fn an_offer_that_is_not_ready_asks_to_set_up_instead() {
        let calendar = notice_text(
            &calendar_payload(&meeting(false, false), OfferSource::Calendar, None),
            None,
        );
        assert_eq!(calendar.body, "Work \u{b7} starting now. Meeting notes aren't set up yet.");
        assert_eq!(keys(&calendar), ["setup", "notNow"]);
        assert_eq!(calendar.actions[0].1, "Set up meeting notes");

        let mic = notice_text(&mic_payload(1, Some("Zoom".into()), false), None);
        assert_eq!(mic.body, "Zoom is using your microphone. Meeting notes aren't set up yet.");
        assert_eq!(keys(&mic), ["setup", "notNow"]);

        // A join link does not bring "Join" back: nothing could be taken.
        let mut offer = meeting(false, false);
        offer.join_url = Some("https://meet.example.com/abc".into());
        let linked = notice_text(&calendar_payload(&offer, OfferSource::Calendar, None), None);
        assert_eq!(keys(&linked), ["setup", "notNow"]);
    }

    #[test]
    fn an_automatic_start_offers_to_stop() {
        let p = calendar_payload(&meeting(true, true), OfferSource::Calendar, None);
        let text = automatic_notice_text(&p, None);
        assert_eq!(text.summary, "Taking notes for Design sync");
        assert_eq!(text.body, "Started automatically");
        assert_eq!(text.actions, [("stop", "Stop recording")]);
    }

    #[test]
    fn a_call_that_ended_says_so_without_buttons() {
        let text = call_ended_text("Design sync", Some("Zoom"));
        assert_eq!(text.summary, "Call ended \u{2014} writing your notes");
        assert_eq!(
            text.body,
            "Recording of Design sync stopped when Zoom let go of the microphone."
        );
        assert!(text.actions.is_empty());
        assert!(call_ended_text("Call", None).body.contains("when the call let go"));
    }

    // ---- the pending set ----------------------------------------------------

    fn pending_event(book: &mut Book, uid: &str, source: OfferSource, end: i64) -> String {
        let mut offer = meeting(false, true);
        offer.uid = uid.into();
        let mut p = calendar_payload(&offer, source, None);
        // One calendar for all of them, so the same `uid` is the same key.
        p.key = format!("event:cal:{uid}");
        let key = p.key.clone();
        book.push(p, Some(at(end)));
        key
    }

    #[test]
    fn a_meeting_is_offered_once_however_it_was_noticed() {
        let mut book = Book::default();
        assert!(book.mark_offered("event:c:u", at(100), at(0)));
        assert!(!book.mark_offered("event:c:u", at(100), at(1)));
        assert!(book.mark_offered("event:c:other", at(100), at(1)));
    }

    #[test]
    fn a_repeating_meeting_with_one_uid_is_offered_again_after_it_is_over() {
        let mut book = Book::default();
        assert!(book.mark_offered("event:c:u", at(100), at(0)));
        assert!(book.mark_offered("event:c:u", at(700), at(500)));
    }

    #[test]
    fn a_meeting_noticed_early_by_the_microphone_can_be_offered_again_once_withdrawn() {
        let mut book = Book::default();
        assert!(book.mark_offered("event:c:u", at(100), at(0)));
        book.forget_offered("event:c:u");
        assert!(book.mark_offered("event:c:u", at(100), at(10)), "the calendar's own offer");
    }

    #[test]
    fn an_offer_held_back_during_a_recording_is_made_after_it_if_still_on() {
        let mut book = Book::default();
        let mut offer = meeting(false, true);
        offer.uid = "b".into();
        let b = calendar_payload(&offer, OfferSource::Calendar, None);
        offer.uid = "c".into();
        let c = calendar_payload(&offer, OfferSource::Calendar, None);
        book.defer(b.clone(), Some(at(100)));
        book.defer(b.clone(), Some(at(100)));
        book.defer(c, Some(at(50)));
        let due: Vec<_> = book.take_deferred(at(60)).into_iter().map(|p| p.payload.key).collect();
        assert_eq!(due, [b.key], "held once, and the meeting already over is dropped");
        assert!(book.take_deferred(at(60)).is_empty(), "taken, not copied");
    }

    #[test]
    fn mic_offers_are_numbered_from_one() {
        let mut book = Book::default();
        assert_eq!(book.next_mic_number(), 1);
        assert_eq!(book.next_mic_number(), 2);
    }

    #[test]
    fn pending_offers_come_oldest_first_and_a_key_is_pending_once() {
        let mut book = Book::default();
        let a = pending_event(&mut book, "a", OfferSource::Calendar, 100);
        let b = pending_event(&mut book, "b", OfferSource::Calendar, 100);
        pending_event(&mut book, "a", OfferSource::Calendar, 100);
        let keys: Vec<_> = book.payloads(at(0)).into_iter().map(|p| p.key).collect();
        assert_eq!(keys, [a, b]);
    }

    #[test]
    fn an_expired_offer_is_not_answerable_or_listed() {
        let mut book = Book::default();
        let key = pending_event(&mut book, "a", OfferSource::Calendar, 100);
        assert!(book.find(&key, at(99)).is_some());
        assert!(book.find(&key, at(100)).is_none());
        assert!(book.payloads(at(100)).is_empty());
        assert!(book.oldest(at(100)).is_none());
    }

    #[test]
    fn settling_removes_only_that_offer() {
        let mut book = Book::default();
        let a = pending_event(&mut book, "a", OfferSource::Calendar, 100);
        let b = pending_event(&mut book, "b", OfferSource::Calendar, 100);
        assert!(book.remove(&a).is_some());
        assert!(book.remove(&a).is_none());
        assert_eq!(book.payloads(at(0))[0].key, b);
    }

    #[test]
    fn the_end_of_a_call_withdraws_the_offers_it_made_and_no_others() {
        let mut book = Book::default();
        let cal = pending_event(&mut book, "cal", OfferSource::Calendar, 100);
        let mic_event = pending_event(&mut book, "mic", OfferSource::Microphone, 100);
        book.push(mic_payload(1, None, true), None);
        let gone = book.take_where(|p| p.payload.source == OfferSource::Microphone);
        let gone: Vec<_> = gone.into_iter().map(|p| p.payload.key).collect();
        assert_eq!(gone, [mic_event, "mic:1".to_string()]);
        assert_eq!(book.payloads(at(0))[0].key, cal);
        assert_eq!(book.payloads(at(0)).len(), 1);
    }

    #[test]
    fn a_recording_starting_withdraws_everything() {
        let mut book = Book::default();
        pending_event(&mut book, "a", OfferSource::Calendar, 100);
        book.push(mic_payload(1, None, true), None);
        assert_eq!(book.take_where(|_| true).len(), 2);
        assert!(book.payloads(at(0)).is_empty());
    }

    #[test]
    fn the_tray_names_the_oldest_offer_that_could_start_a_recording() {
        let mut book = Book::default();
        book.push(mic_payload(1, Some("Zoom".into()), false), None);
        assert!(book.oldest_ready(at(0)).is_none());
        assert!(book.oldest(at(0)).is_some());
        book.push(mic_payload(2, Some("Meet".into()), true), None);
        assert_eq!(book.oldest_ready(at(0)).unwrap().title, "Call on Meet");
    }

    // ---- the corner card ----------------------------------------------------

    fn card(key: &str, phase: CardPhase) -> Card {
        Card { key: key.into(), phase, seconds: 15 }
    }

    #[test]
    fn joining_the_call_moves_the_ask_on() {
        let mut inner = Inner::default();
        let key = pending_event(&mut inner.book, "u", OfferSource::Calendar, 100);
        inner.notice = Some(card(&key, CardPhase::Join));
        assert_eq!(inner.asking(&key, at(0)).map(|p| p.key), Some(key.clone()));
        assert!(inner.asking("event:cal:other", at(0)).is_none(), "another meeting's card");

        inner.notice = Some(card(&key, CardPhase::Countdown));
        assert!(inner.asking(&key, at(0)).is_none(), "counting down already");
        inner.notice = Some(card(&key, CardPhase::Ask));
        assert!(inner.asking(&key, at(0)).is_none(), "asking to take notes already");

        inner.notice = Some(card(&key, CardPhase::Join));
        inner.book.remove(&key);
        assert!(inner.asking(&key, at(0)).is_none(), "answered since");
    }

    #[test]
    fn the_call_ending_finds_the_joined_card_to_take_back() {
        let mut inner = Inner::default();
        let key = pending_event(&mut inner.book, "u", OfferSource::Calendar, 100);
        assert!(inner.joined(at(0)).is_none(), "no card");
        inner.notice = Some(card(&key, CardPhase::Join));
        assert!(inner.joined(at(0)).is_none(), "still asking to join");
        inner.notice = Some(card(&key, CardPhase::Countdown));
        assert_eq!(inner.joined(at(0)).map(|p| p.key), Some(key.clone()));
        inner.notice = Some(card(&key, CardPhase::Ask));
        assert_eq!(inner.joined(at(0)).map(|p| p.key), Some(key.clone()));
        inner.book.remove(&key);
        assert!(inner.joined(at(0)).is_none(), "answered since");
    }

    #[test]
    fn a_press_as_the_countdown_ends_starts_one_recording() {
        let mut inner = Inner::default();
        assert!(inner.claim_start("event:cal:u"), "the countdown");
        assert!(!inner.claim_start("event:cal:u"), "the press at the same moment");
        inner.release_start("event:cal:other");
        assert!(!inner.claim_start("event:cal:u"), "only its own start releases it");
        inner.release_start("event:cal:u");
        assert!(inner.claim_start("event:cal:u"), "tried again after a failure");
    }

    // ---- what the card says ---------------------------------------------------

    /// Five minutes to the start, on a feed that is an hour long, in a zone
    /// eight hours behind UTC.
    const NOW: i64 = 1_000;

    fn pacific() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(-8))
    }

    fn payload() -> OfferPayload {
        let mut offer = meeting(false, true);
        offer.start = at(NOW + 300);
        offer.end = at(NOW + 300 + 3_600);
        calendar_payload(&offer, OfferSource::Calendar, None)
    }

    fn brief_for(p: &OfferPayload) -> MeetingBrief {
        MeetingBrief {
            key: p.key.clone(),
            event_id: p.event_id.unwrap(),
            calendar_id: p.calendar_id.unwrap(),
            uid: p.uid.clone().unwrap(),
            title: p.title.clone(),
            start: p.start.unwrap(),
            end: p.end.unwrap(),
            location: None,
            summary: None,
            agenda: Vec::new(),
            people: Vec::new(),
            context: Vec::new(),
            talking_points: Vec::new(),
            sources: Vec::new(),
            written: false,
            gaps: Vec::new(),
            prepared_at: at(NOW),
        }
    }

    fn person(name: &str, line: &str) -> MeetingBriefPerson {
        MeetingBriefPerson { name: name.into(), email: None, contact: None, line: line.into() }
    }

    fn point(text: &str, with: &str, done: bool) -> TalkingPoint {
        TalkingPoint { task_id: TaskId::new(), text: text.into(), with: with.into(), done }
    }

    fn draw(
        phase: CardPhase,
        p: &OfferPayload,
        brief: Option<&MeetingBrief>,
    ) -> crate::panel::Panel {
        meeting_panel(phase, 12, p, brief, at(NOW), &pacific())
    }

    fn ids(panel: &crate::panel::Panel) -> Vec<(&str, &str, bool)> {
        panel.actions.iter().map(|a| (a.id.as_str(), a.label.as_str(), a.primary)).collect()
    }

    #[test]
    fn the_join_card_asks_to_join_and_says_when_the_meeting_is() {
        let mut p = payload();
        let card = draw(CardPhase::Join, &p, None);
        assert_eq!(card.id, p.key);
        assert_eq!(card.owner, crate::panel::Owner::Meeting);
        assert_eq!(card.tone, crate::panel::Tone::Neutral);
        assert_eq!(card.kicker, "Meeting \u{b7} in 5 min");
        assert_eq!(card.title, "Design sync");
        assert_eq!(card.count, None);
        // 1_300 and 4_900 seconds after the epoch, eight hours behind it.
        assert_eq!(card.body.as_deref(), Some("16:21\u{2013}17:21"));
        assert!(card.sections.is_empty());
        assert_eq!(ids(&card), [("join", "Start recording", true), ("notNow", "Not now", false)]);

        p.join_url = Some("https://us02web.zoom.us/j/123".into());
        let card = draw(CardPhase::Join, &p, None);
        assert_eq!(card.body.as_deref(), Some("16:21\u{2013}17:21 \u{b7} Zoom"));
        assert_eq!(
            ids(&card),
            [("join", "Join and start recording", true), ("notNow", "Not now", false)]
        );
    }

    #[test]
    fn the_countdown_card_counts_and_asks_for_attention() {
        let p = payload();
        let card = draw(CardPhase::Countdown, &p, None);
        assert_eq!(card.kicker, "Recording starts in");
        assert_eq!(card.count.as_deref(), Some("12s"));
        assert_eq!(card.tone, crate::panel::Tone::Attention);
        assert_eq!(ids(&card), [("take", "Take notes", true), ("notNow", "Cancel", false)]);
    }

    #[test]
    fn the_ask_card_has_no_countdown_and_nothing_starts_by_itself() {
        let p = payload();
        let card = draw(CardPhase::Ask, &p, None);
        assert_eq!(card.kicker, "Meeting \u{b7} in 5 min");
        assert_eq!(card.count, None);
        assert_eq!(card.tone, crate::panel::Tone::Neutral);
        assert_eq!(ids(&card), [("take", "Take notes", true), ("notNow", "Not now", false)]);
    }

    #[test]
    fn a_call_with_no_event_is_a_call_with_no_time() {
        let p = mic_payload(1, Some("Zoom".into()), true);
        let card = draw(CardPhase::Ask, &p, None);
        assert_eq!(card.kicker, "Call");
        assert_eq!(card.title, "Call on Zoom");
        assert_eq!(card.body, None);
        assert!(card.sections.is_empty());
    }

    #[test]
    fn the_kicker_says_when_the_meeting_is_from_where_the_person_stands() {
        let now = at(10_000);
        assert_eq!(timing(at(10_000 + 120), now), "in 2 min");
        assert_eq!(timing(at(10_000 + 20), now), "now");
        assert_eq!(timing(at(10_000), now), "now");
        assert_eq!(timing(at(10_000 - 20), now), "now");
        assert_eq!(timing(at(10_000 + 45), now), "in 1 min");
        assert_eq!(timing(at(10_000 - 180), now), "started 3 min ago");
        assert_eq!(timing(at(10_000 + 3_600), now), "in 1 h");
        assert_eq!(timing(at(10_000 - 3_900), now), "started 1 h 5 min ago");
    }

    #[test]
    fn the_hours_are_the_persons_own() {
        let p = payload();
        let utc = meeting_panel(CardPhase::Join, 0, &p, None, at(NOW), &TimeZone::UTC);
        assert_eq!(utc.body.as_deref(), Some("00:21\u{2013}01:21"));
        let pacific = draw(CardPhase::Join, &p, None);
        assert_eq!(pacific.body.as_deref(), Some("16:21\u{2013}17:21"));
    }

    #[test]
    fn where_it_is_comes_from_the_brief_before_the_link() {
        let mut p = payload();
        p.join_url = Some("https://meet.google.com/abc-defg-hij".into());
        let mut brief = brief_for(&p);
        assert_eq!(
            draw(CardPhase::Join, &p, Some(&brief)).body.as_deref(),
            Some("16:21\u{2013}17:21 \u{b7} Google Meet"),
            "the link's own service when the brief names no room"
        );
        brief.location = Some("  Room 4B  ".into());
        brief.summary = Some("Settle the Q3 budget.".into());
        assert_eq!(
            draw(CardPhase::Join, &p, Some(&brief)).body.as_deref(),
            Some("16:21\u{2013}17:21 \u{b7} Room 4B\nSettle the Q3 budget.")
        );
    }

    #[test]
    fn a_link_with_no_room_names_its_service_and_an_unknown_one_names_nothing() {
        let mut p = payload();
        p.join_url = Some("https://teams.microsoft.com/l/meetup-join/19".into());
        assert_eq!(
            draw(CardPhase::Join, &p, None).body.as_deref(),
            Some("16:21\u{2013}17:21 \u{b7} Microsoft Teams")
        );
        p.join_url = Some("https://evil.example/zoom.us".into());
        assert_eq!(draw(CardPhase::Join, &p, None).body.as_deref(), Some("16:21\u{2013}17:21"));
    }

    #[test]
    fn the_brief_adds_its_parts_in_the_order_that_matters() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.talking_points = vec![point("Talk about the Q3 budget", "Josh", false)];
        brief.agenda = vec!["Budget".into(), "Hiring".into()];
        brief.people = vec![person("Josh", "Head of design at Acme"), person("Priya", "")];
        brief.context = vec!["Last met on 3 November about the launch.".into()];
        let card = draw(CardPhase::Join, &p, Some(&brief));
        let headings: Vec<_> =
            card.sections.iter().map(|s| (s.heading.as_str(), s.folded)).collect();
        assert_eq!(
            headings,
            [("To raise", false), ("Agenda", false), ("With", false), ("Context", true)]
        );

        let raise = &card.sections[0].items[0];
        assert_eq!(raise.text, "Talk about the Q3 budget");
        assert_eq!(raise.detail.as_deref(), Some("with Josh"));
        let check = raise.check.as_ref().unwrap();
        assert_eq!(check.action, format!("done:{}", brief.talking_points[0].task_id));
        assert!(!check.done);

        let with = &card.sections[2].items;
        assert_eq!(with[0].text, "Josh");
        assert_eq!(with[0].detail.as_deref(), Some("Head of design at Acme"));
        assert_eq!(with[1].detail, None, "no line, no detail");
        assert!(with.iter().all(|i| i.check.is_none()));
    }

    #[test]
    fn a_part_with_nothing_in_it_is_left_out() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.agenda = vec!["  ".into(), "Budget".into()];
        let card = draw(CardPhase::Join, &p, Some(&brief));
        assert_eq!(card.sections.len(), 1);
        assert_eq!(card.sections[0].heading, "Agenda");
        assert_eq!(card.sections[0].items.len(), 1, "a blank line is not an item");
        assert!(draw(CardPhase::Join, &p, Some(&brief_for(&p))).sections.is_empty());
    }

    #[test]
    fn the_agenda_is_five_lines_and_many_people_are_folded() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.agenda = (1..=8).map(|n| format!("Item {n}")).collect();
        brief.people = ["A", "B", "C"].iter().map(|n| person(n, "")).collect();
        let card = draw(CardPhase::Join, &p, Some(&brief));
        assert_eq!(card.sections[0].items.len(), 5);
        assert!(!card.sections[0].folded);
        assert!(!card.sections[1].folded, "three people are shown");

        brief.people.push(person("D", ""));
        let card = draw(CardPhase::Join, &p, Some(&brief));
        assert!(card.sections[1].folded, "four are folded");
        assert_eq!(card.sections[1].items.len(), 4);
    }

    #[test]
    fn a_ticked_point_is_drawn_ticked() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.talking_points = vec![point("Budget", "Josh", true), point("Hiring", "Josh", false)];
        let card = draw(CardPhase::Ask, &p, Some(&brief));
        let done: Vec<_> =
            card.sections[0].items.iter().map(|i| i.check.as_ref().unwrap().done).collect();
        assert_eq!(done, [true, false]);
    }

    #[test]
    fn the_brief_is_drawn_on_every_face() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.agenda = vec!["Budget".into()];
        for phase in [CardPhase::Join, CardPhase::Countdown, CardPhase::Ask] {
            assert_eq!(draw(phase, &p, Some(&brief)).sections.len(), 1, "{phase:?}");
        }
    }

    // ---- what the card is made from ---------------------------------------------

    #[test]
    fn a_card_is_drawn_from_the_offer_the_face_and_the_brief_kept() {
        let mut inner = Inner::default();
        let key = pending_event(&mut inner.book, "u", OfferSource::Calendar, 10_000);
        let zone = pacific();
        assert!(inner.panel(&key, at(NOW), &zone).is_none(), "no card up");

        inner.notice = Some(card(&key, CardPhase::Join));
        assert_eq!(inner.panel(&key, at(NOW), &zone).unwrap().id, key);
        assert!(inner.panel("event:cal:other", at(NOW), &zone).is_none(), "another offer's card");
        assert!(inner.panel(&key, at(10_000), &zone).is_none(), "the meeting is over");

        let mut brief = brief_for(&inner.book.find(&key, at(NOW)).unwrap().payload);
        brief.agenda = vec!["Budget".into()];
        brief.key = key.clone();
        inner.keep_brief(brief, at(NOW));
        let panel = inner.panel(&key, at(NOW), &zone).unwrap();
        assert_eq!(panel.sections[0].heading, "Agenda");

        inner.book.remove(&key);
        assert!(inner.panel(&key, at(NOW), &zone).is_none(), "answered since");
    }

    #[test]
    fn a_tick_changes_the_seconds_and_nothing_else_and_only_for_its_own_countdown() {
        let mut inner = Inner::default();
        let key = pending_event(&mut inner.book, "u", OfferSource::Calendar, 10_000);
        let zone = pacific();
        inner.notice = Some(card(&key, CardPhase::Countdown));
        inner.countdown_gen = 7;

        let first = inner.tick(&key, 7, 9, at(NOW), &zone).unwrap();
        assert_eq!(first.count.as_deref(), Some("9s"));
        let second = inner.tick(&key, 7, 8, at(NOW), &zone).unwrap();
        assert_eq!(second.count.as_deref(), Some("8s"));
        assert_eq!(Panel { count: None, ..first }, Panel { count: None, ..second });
        assert_eq!(inner.notice.as_ref().unwrap().seconds, 8, "kept, for a redraw by a brief");

        assert!(inner.tick(&key, 6, 7, at(NOW), &zone).is_none(), "a countdown since replaced");
        assert!(inner.tick("event:cal:other", 7, 7, at(NOW), &zone).is_none());
        inner.notice = None;
        assert!(inner.tick(&key, 7, 7, at(NOW), &zone).is_none(), "cancelled");
    }

    // ---- briefs kept ----------------------------------------------------------------

    #[test]
    fn a_brief_is_kept_for_its_offer_and_a_fresher_one_replaces_it() {
        let mut inner = Inner::default();
        let p = payload();
        let mut brief = brief_for(&p);
        inner.keep_brief(brief.clone(), at(NOW));
        assert!(inner.briefs.contains_key(&p.key));
        brief.agenda = vec!["Budget".into()];
        inner.keep_brief(brief, at(NOW));
        assert_eq!(inner.briefs.len(), 1);
        assert_eq!(inner.briefs[&p.key].agenda, ["Budget"]);
    }

    #[test]
    fn a_brief_for_a_meeting_that_is_over_is_not_kept_and_clears_the_old() {
        let mut inner = Inner::default();
        let p = payload();
        let end = p.end.unwrap().as_second();
        inner.keep_brief(brief_for(&p), at(NOW));
        let mut other = brief_for(&p);
        other.key = "event:cal:other".into();
        other.end = at(NOW + 10);
        inner.keep_brief(other.clone(), at(NOW));
        assert_eq!(inner.briefs.len(), 2);

        // The other meeting has ended by now, and a brief for one that has
        // is not worth keeping either.
        inner.keep_brief(other, at(NOW + 20));
        assert_eq!(inner.briefs.keys().collect::<Vec<_>>(), [&p.key], "only the one still on");
        inner.keep_brief(brief_for(&p), at(end + 1));
        assert!(inner.briefs.is_empty());
    }

    #[test]
    fn a_point_ticked_on_the_card_is_ticked_in_the_brief() {
        let mut inner = Inner::default();
        let p = payload();
        let mut brief = brief_for(&p);
        brief.talking_points = vec![point("Budget", "Josh", false), point("Hiring", "Josh", true)];
        let (budget, hiring) = (brief.talking_points[0].task_id, brief.talking_points[1].task_id);
        inner.keep_brief(brief, at(NOW));

        assert_eq!(inner.point(&p.key, budget), Some(false));
        assert_eq!(inner.point(&p.key, hiring), Some(true));
        assert_eq!(inner.point(&p.key, TaskId::new()), None, "not on this card");
        assert_eq!(inner.point("event:cal:other", budget), None, "not this card's brief");

        inner.tick_point(&p.key, budget);
        assert_eq!(inner.point(&p.key, budget), Some(true));
        inner.tick_point("event:cal:other", budget);
    }

    #[test]
    fn a_talking_points_action_names_its_task() {
        let task = TaskId::new();
        assert_eq!(point_of(&format!("done:{task}")), Some(task));
        assert_eq!(point_of("done:not-an-id"), None);
        assert_eq!(point_of("done:"), None);
        assert_eq!(point_of("take"), None);
    }

    #[test]
    fn completing_a_task_edits_what_the_service_sent_and_loses_nothing_of_it() {
        let open = karmayoga_core::Task::new("Talk about the Q3 budget");
        let mut sent = serde_json::to_value(&open).unwrap();
        // A field a newer vault's task has and this build does not.
        sent["raisedAt"] = json!({ "room": "4B" });

        mark_done(&mut sent, at(5_000)).unwrap();
        assert_eq!(sent["raisedAt"], json!({ "room": "4B" }), "kept");
        assert_eq!(sent["status"], "done");

        // The same task as `set_status` would have made it, but for the time.
        let done: karmayoga_core::Task = serde_json::from_value(sent).unwrap();
        let mut expected = open.clone();
        expected.set_status(TaskStatus::Done);
        assert_eq!(done.status, expected.status);
        assert_eq!(done.completed_at, Some(at(5_000)));
        assert_eq!(done.updated_at, at(5_000));
        let rest = karmayoga_core::Task {
            completed_at: None,
            updated_at: open.updated_at,
            status: open.status,
            ..done
        };
        assert_eq!(rest, open, "nothing else changed");
    }

    #[test]
    fn a_task_that_was_finished_already_keeps_the_day_it_was() {
        let mut sent = serde_json::to_value(karmayoga_core::Task::new("x")).unwrap();
        sent["status"] = json!("done");
        sent["completedAt"] = json!(at(2_000));
        mark_done(&mut sent, at(9_000)).unwrap();
        assert_eq!(sent["completedAt"], json!(at(2_000)));
        assert_eq!(sent["updatedAt"], json!(at(9_000)));
        assert!(mark_done(&mut json!("not a task"), at(9_000)).is_err());
    }

    // ---- what the notification says of a brief ------------------------------------------

    #[test]
    fn a_notification_names_who_is_coming_and_the_first_thing_to_cover() {
        let p = payload();
        let mut brief = brief_for(&p);
        assert_eq!(brief_line(&brief), None, "nothing to say");
        brief.people = vec![person("Josh", "x"), person("Priya", "")];
        assert_eq!(brief_line(&brief).as_deref(), Some("With Josh, Priya"));
        brief.agenda = vec!["".into(), "Q3 budget".into(), "Hiring".into()];
        assert_eq!(
            brief_line(&brief).as_deref(),
            Some("With Josh, Priya \u{b7} Agenda: Q3 budget")
        );
        brief.summary = Some("Settle the budget.".into());
        assert_eq!(
            brief_line(&brief).as_deref(),
            Some("With Josh, Priya \u{b7} Agenda: Q3 budget"),
            "the agenda before the summary"
        );
        brief.agenda.clear();
        assert_eq!(
            brief_line(&brief).as_deref(),
            Some("With Josh, Priya \u{b7} Settle the budget.")
        );
    }

    #[test]
    fn what_the_person_promised_to_raise_comes_before_the_agenda() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.agenda = vec!["Q3 budget".into()];
        brief.talking_points =
            vec![point("Pay rise", "Josh", true), point("Hiring", "Josh", false)];
        assert_eq!(brief_line(&brief).as_deref(), Some("To raise: Hiring"), "the first open one");
        brief.talking_points[1].done = true;
        assert_eq!(brief_line(&brief).as_deref(), Some("Agenda: Q3 budget"), "none left to raise");
    }

    #[test]
    fn a_long_brief_is_cut_to_fit_a_banner() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.people = ["Ann", "Bob", "Cat", "Dee", "Eve"].iter().map(|n| person(n, "")).collect();
        brief.agenda = vec!["word ".repeat(60)];
        let line = brief_line(&brief).unwrap();
        assert!(line.starts_with("With Ann, Bob, Cat +2 \u{b7} Agenda: word"));
        assert!(line.chars().count() <= NOTICE_LINE_CHARS, "{line}");
        assert!(line.ends_with('\u{2026}'));
        assert_eq!(shorten("  short  ", 10), "short");
    }

    #[test]
    fn a_notification_carries_the_brief_under_what_it_already_says() {
        let p = payload();
        let mut brief = brief_for(&p);
        brief.people = vec![person("Josh", ""), person("Priya", "")];
        brief.agenda = vec!["Q3 budget".into()];
        let line = "With Josh, Priya \u{b7} Agenda: Q3 budget";

        let ask = notice_text(&p, Some(&brief));
        assert_eq!(ask.body, format!("Work \u{b7} starting now\n{line}"));
        assert_eq!(keys(&ask), ["take", "notNow"], "the buttons are the same");

        let mut not_ready = meeting(false, false);
        not_ready.start = p.start.unwrap();
        let setup =
            notice_text(&calendar_payload(&not_ready, OfferSource::Calendar, None), Some(&brief));
        assert_eq!(
            setup.body,
            format!("Work \u{b7} starting now. Meeting notes aren't set up yet.\n{line}")
        );

        let auto = automatic_notice_text(&p, Some(&brief));
        assert_eq!(auto.body, format!("Started automatically\n{line}"));

        assert_eq!(notice_text(&p, Some(&brief_for(&p))).body, "Work \u{b7} starting now");
    }
}
