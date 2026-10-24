//! Offers to take notes, held by the shell: one hub for every way a
//! meeting is noticed -- a calendar event starting
//! (`everyday_service::meeting::watch`, by way of
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

use std::collections::HashMap;
use std::sync::Mutex;

use everyday_core::id::{CalendarId, EventId, RecordingId};
use everyday_core::meeting::Recording;
use everyday_service::Ctx;
use everyday_service::domains::meetings::MeetingNow;
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::MeetingOffer;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<RecordingId>,
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
/// would only fail.
fn notice_text(p: &OfferPayload) -> NoticeText {
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
            body,
            actions: vec![("setup", "Set up meeting notes"), ("notNow", "Not now")],
        };
    }
    let actions = if p.join_url.is_some() {
        vec![("join", "Join & take notes"), ("take", "Take notes"), ("notNow", "Not now")]
    } else {
        vec![("take", "Take notes"), ("notNow", "Not now")]
    };
    NoticeText { summary, body, actions }
}

/// The notification for an offer that was not a question: "Always" mode has
/// already started the recording. The one button is how to take it back.
fn automatic_notice_text(p: &OfferPayload) -> NoticeText {
    NoticeText {
        summary: format!("Taking notes for {}", p.title),
        body: "Started automatically".to_string(),
        actions: vec![("stop", "Stop recording")],
    }
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
    offer_event(app, payload, offer.end);
}

/// The part [`on_calendar_offer`] and a microphone offer tied to an event
/// share: not twice, automatically or as a question.
fn offer_event(app: &AppHandle, payload: OfferPayload, end: jiff::Timestamp) {
    let now = jiff::Timestamp::now();
    let first = offers(app).with(|o| o.book.mark_offered(&payload.key, end, now));
    if !first {
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
        let text = automatic_notice_text(&payload);
        offers(app).with(|o| o.shown.push(payload.key.clone()));
        raise_notice(app, &payload.key, &text);
        return;
    }
    raise(app, payload, Some(end));
}

/// Put an offer up everywhere: pending, the interface, a notification
/// unless the window is in front already, and the tray.
fn raise(app: &AppHandle, payload: OfferPayload, expires: Option<jiff::Timestamp>) {
    let key = payload.key.clone();
    offers(app).with(|o| o.book.push(payload.clone(), expires));
    emit(app, OFFER_EVENT, &payload);
    raise_notice(app, &key, &notice_text(&payload));
    refresh_tray(app);
    if let Some(end) = expires {
        schedule_expiry(app, key, end);
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
    let removed = offers(app).with(|o| o.book.remove(key));
    if removed.is_some() {
        announce_settled(app, key);
        refresh_tray(app);
    }
    removed.is_some()
}

/// Settle every pending offer `pick` chooses. One tray update for the lot.
fn settle_where(app: &AppHandle, pick: impl Fn(&Pending) -> bool) {
    let taken = offers(app).with(|o| o.book.take_where(pick));
    if taken.is_empty() {
        return;
    }
    for p in &taken {
        announce_settled(app, &p.payload.key);
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
        Err(e) => {
            tracing::debug!(error = %e, "no microphone offer: the service did not answer");
            return;
        }
    };
    if !now.offers || !now.mic_offer {
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
            offer_event(app, payload, event.end);
        }
        None => {
            let payload =
                offers(app).with(|o| mic_payload(o.book.next_mic_number(), app_name, now.ready));
            raise(app, payload, None);
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
        raise(app, p.payload, p.expires);
    }
}

/// The vault locked: every offer names a meeting, which is decrypted
/// contents of the vault, so every one goes -- pending, held back, on the
/// tray and in the notification centre -- the way a toast quoting a title
/// does in the window. The interface empties its own list on lock already.
pub(crate) fn on_locked(app: &AppHandle) {
    let shown = offers(app).with(|o| {
        o.book.deferred.clear();
        std::mem::take(&mut o.shown)
    });
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
        if let Err(e) = self::answer(&app, &key, answer).await {
            report_failure(&app, &key, &e);
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
    if let Err(e) = crate::meeting::begin(app.clone(), None, None, None, false).await {
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
        OfferAnswer::Take => Some(take(app, &payload).await?),
        OfferAnswer::Join => {
            if let Some(url) = payload.join_url.clone()
                && let Err(e) = app.opener().open_url(url, None::<&str>)
            {
                // Not a reason to withhold the notes: the person pressed
                // "Join & take notes", and the second half still stands.
                tracing::warn!(error = %e, "could not open the meeting link");
            }
            Some(take(app, &payload).await?)
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
    // After a recording starts `on_recording_started` has already settled
    // this offer, along with the rest; this finds nothing to do then.
    settle(app, key);
    Ok(recording)
}

async fn take(app: &AppHandle, payload: &OfferPayload) -> CommandResult<Recording> {
    crate::meeting::begin(app.clone(), payload.event_id, Some(payload.title.clone()), None, false)
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
        let text = notice_text(&p);
        assert_eq!(text.summary, "Take notes for Design sync?");
        assert_eq!(text.body, "Work \u{b7} starting now");
        assert_eq!(keys(&text), ["take", "notNow"]);
    }

    #[test]
    fn a_join_link_puts_join_first() {
        let mut offer = meeting(false, true);
        offer.join_url = Some("https://meet.example.com/abc".into());
        let text = notice_text(&calendar_payload(&offer, OfferSource::Calendar, None));
        assert_eq!(keys(&text), ["join", "take", "notNow"]);
        assert_eq!(text.actions[0].1, "Join & take notes");
    }

    #[test]
    fn a_meeting_the_microphone_noticed_says_which_application() {
        let p =
            calendar_payload(&meeting(false, true), OfferSource::Microphone, Some("Zoom".into()));
        let text = notice_text(&p);
        assert_eq!(text.summary, "Take notes for Design sync?");
        assert_eq!(text.body, "Zoom is using your microphone");
        let anon = calendar_payload(&meeting(false, true), OfferSource::Microphone, None);
        assert_eq!(notice_text(&anon).body, "Another app is using your microphone");
    }

    #[test]
    fn a_call_with_no_event_asks_whether_it_is_one() {
        let text = notice_text(&mic_payload(1, Some("Zoom".into()), true));
        assert_eq!(text.summary, "Taking a call? Take notes");
        assert_eq!(text.body, "Zoom is using your microphone.");
        assert_eq!(keys(&text), ["take", "notNow"]);
        let anon = notice_text(&mic_payload(2, None, true));
        assert_eq!(anon.body, "Another app is using your microphone.");
    }

    #[test]
    fn an_offer_that_is_not_ready_asks_to_set_up_instead() {
        let calendar =
            notice_text(&calendar_payload(&meeting(false, false), OfferSource::Calendar, None));
        assert_eq!(calendar.body, "Work \u{b7} starting now. Meeting notes aren't set up yet.");
        assert_eq!(keys(&calendar), ["setup", "notNow"]);
        assert_eq!(calendar.actions[0].1, "Set up meeting notes");

        let mic = notice_text(&mic_payload(1, Some("Zoom".into()), false));
        assert_eq!(mic.body, "Zoom is using your microphone. Meeting notes aren't set up yet.");
        assert_eq!(keys(&mic), ["setup", "notNow"]);

        // A join link does not bring "Join" back: nothing could be taken.
        let mut offer = meeting(false, false);
        offer.join_url = Some("https://meet.example.com/abc".into());
        let linked = notice_text(&calendar_payload(&offer, OfferSource::Calendar, None));
        assert_eq!(keys(&linked), ["setup", "notNow"]);
    }

    #[test]
    fn an_automatic_start_offers_to_stop() {
        let p = calendar_payload(&meeting(true, true), OfferSource::Calendar, None);
        let text = automatic_notice_text(&p);
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
}
