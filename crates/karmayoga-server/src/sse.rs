//! What the service says, fanned out to everyone connected.
//!
//! One [`Broadcaster`] per server, one stream per client. It is an
//! [`EventSink`], so the service does not learn that it is being watched by
//! anybody: the same `changed` a desktop window turns into a list refresh
//! becomes a line on a wire here.
//!
//! # Why the origin is filtered on this side
//!
//! A client that made a write is told about it by the response to that write.
//! Telling it again over the event stream would make every save reload the list
//! it was made in -- under the cursor, mid-scroll. So a stream drops the changes
//! whose origin is the client reading it. Doing that here rather than in each
//! client means a client that forgets cannot flicker.
//!
//! # What a stream is told depends on what it may read
//!
//! A new-mail notice carries the sender, the subject and the first words of a
//! message; a dose reminder names a medicine; a meeting offer carries a title
//! and a join link. Those are the vault's contents, and a stream that was sent
//! everything would give them to any token that could open one. So every
//! stream is built from the scopes of the caller that opened it, and
//! [`is_for`] decides, event by event, whether those scopes may hear it:
//! a notice only if it names a domain the scopes hold, a change only if its
//! kind belongs to one, a meeting offer only to the calendar or the meetings,
//! and the lock state to everyone, because every client has to leave the
//! screen it is on. What was never classified reaches [`Scope::All`] and
//! nobody narrower. See [`Notification::domain`] and
//! [`karmayoga_service::events::Kind::scope`].
//!
//! # A slow reader is dropped, not waited for
//!
//! `broadcast` gives every receiver a bounded buffer and tells a receiver that
//! fell behind that it missed some. That is the right failure: a phone on a bad
//! connection must not be able to stall the window on the machine holding the
//! vault. A client that is told it lagged reloads everything, which is correct
//! and rare.

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use karmayoga_service::ctx::Scope;
use karmayoga_service::events::{Change, EventSink, MeetingOffer, Notification};
use serde::Serialize;
use std::time::Duration;
use tokio::sync::broadcast;

/// How many events a client may fall behind by before it is told it missed
/// some. A window's worth of a busy sync, and no more.
const BACKLOG: usize = 256;

/// How often to send a comment down an idle stream.
///
/// Server-sent events over a network with a proxy or a phone radio in the way
/// die quietly otherwise, and the client cannot tell "nothing has happened"
/// from "this connection is gone".
const KEEPALIVE: Duration = Duration::from_secs(15);

/// One thing the server has to say, in the shape a client reads.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Outgoing {
    Notify(Notification),
    Changed(Change),
    LockState { locked: bool },
    MeetingOffer(MeetingOffer),
}

impl Outgoing {
    fn name(&self) -> &'static str {
        match self {
            Outgoing::Notify(_) => "notify",
            Outgoing::Changed(_) => "changed",
            Outgoing::LockState { .. } => "lockState",
            Outgoing::MeetingOffer(_) => "meetingOffer",
        }
    }

    /// Who made this happen, if anybody. Used to keep a client from being told
    /// about its own writes.
    fn origin(&self) -> Option<&str> {
        match self {
            Outgoing::Changed(change) => change.origin.as_deref(),
            _ => None,
        }
    }
}

/// Do `scopes` hold `scope`, as [`karmayoga_service::ctx::Ctx::holds`] says
/// it: by name, or through [`Scope::All`].
fn holds(scopes: &[Scope], scope: Scope) -> bool {
    scopes.iter().any(|s| *s == Scope::All || *s == scope)
}

/// May a stream opened by a caller holding `scopes` be told `event`?
///
/// Pure, and the only place that decides it, so that what each kind of
/// caller hears is one table a test can walk rather than a check each send
/// remembers. A notice with no domain, and a change of a kind no scope
/// covers, are answered `false` for everyone but [`Scope::All`]: the
/// default for anything not classified is that it is not delivered.
fn is_for(scopes: &[Scope], event: &Outgoing) -> bool {
    match event {
        Outgoing::LockState { .. } => true,
        Outgoing::Notify(notice) => match notice.domain {
            Some(domain) => holds(scopes, domain),
            None => holds(scopes, Scope::All),
        },
        Outgoing::Changed(change) => match change.kind.scope() {
            Some(domain) => holds(scopes, domain),
            None => holds(scopes, Scope::All),
        },
        // An offer is a calendar event with a recording attached to it: the
        // title, the calendar's name and the way in. Either reading the
        // calendars or running the meetings is a good enough reason to see it.
        Outgoing::MeetingOffer(_) => {
            holds(scopes, Scope::Calendars) || holds(scopes, Scope::Meetings)
        }
    }
}

pub struct Broadcaster {
    tx: broadcast::Sender<Outgoing>,
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Broadcaster {
    pub fn new() -> Self {
        Self { tx: broadcast::channel(BACKLOG).0 }
    }

    /// A stream for one client, skipping what that client did itself and
    /// what its `scopes` do not let it hear -- see [`is_for`].
    pub fn stream(
        &self,
        me: String,
        scopes: Vec<Scope>,
    ) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + use<>> {
        Sse::new(self.events(me, scopes)).keep_alive(KeepAlive::new().interval(KEEPALIVE))
    }

    /// [`Broadcaster::stream`], ending once `until` turns true -- for a
    /// browser, whose stream must end with its session rather than carry on
    /// telling a signed-out page what changed. See `crate::web`.
    ///
    /// Whatever was already sent before `until` turned is delivered first.
    /// That is not a nicety: the vault locking says `lockState` and *then*
    /// ends every session, and a stream that ended on the second without
    /// passing on the first left the page to find out by failing to
    /// reconnect.
    pub fn stream_until(
        &self,
        me: String,
        scopes: Vec<Scope>,
        until: tokio::sync::watch::Receiver<bool>,
    ) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + use<>> {
        Sse::new(self.events_until(me, scopes, until))
            .keep_alive(KeepAlive::new().interval(KEEPALIVE))
    }

    fn events_until(
        &self,
        me: String,
        scopes: Vec<Scope>,
        until: tokio::sync::watch::Receiver<bool>,
    ) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + use<> {
        let rx = self.tx.subscribe();
        futures::stream::unfold(
            (rx, until, me, scopes),
            |(mut rx, mut until, me, scopes)| async move {
                loop {
                    let next = tokio::select! {
                        // Biased, events first: anything already in the channel
                        // when the session ends still goes out.
                        biased;
                        received = rx.recv() => received,
                        _ = until.wait_for(|ended| *ended) => return None,
                    };
                    match next {
                        Ok(event) => {
                            if event.origin().is_some_and(|origin| origin == me)
                                || !is_for(&scopes, &event)
                            {
                                continue;
                            }
                            return Some((Ok(encode(&event)), (rx, until, me, scopes)));
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            let sse = Event::default().event("lagged").data(n.to_string());
                            return Some((Ok(sse), (rx, until, me, scopes)));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        )
    }

    fn events(
        &self,
        me: String,
        scopes: Vec<Scope>,
    ) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + use<> {
        let rx = self.tx.subscribe();
        futures::stream::unfold(rx, move |mut rx| {
            let me = me.clone();
            let scopes = scopes.clone();
            async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            if event.origin().is_some_and(|origin| origin == me)
                                || !is_for(&scopes, &event)
                            {
                                continue;
                            }
                            return Some((Ok(encode(&event)), rx));
                        }
                        // Fell behind. Say so rather than skipping silently: a
                        // client that missed a change has a stale list and must
                        // reload everything.
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            let sse = Event::default().event("lagged").data(n.to_string());
                            return Some((Ok(sse), rx));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        })
    }

    fn send(&self, event: Outgoing) {
        // An error here means nobody is listening, which is the ordinary state
        // of a server with no clients connected.
        let _ = self.tx.send(event);
    }

    /// How many streams are open. For the settings screen, and for tests.
    pub fn listeners(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// One event as a server-sent event: its name, and itself as JSON.
fn encode(event: &Outgoing) -> Event {
    Event::default()
        .event(event.name())
        .json_data(event)
        .unwrap_or_else(|_| Event::default().comment("unencodable"))
}

impl EventSink for Broadcaster {
    fn notify(&self, notification: Notification) {
        self.send(Outgoing::Notify(notification));
    }

    fn changed(&self, change: Change) {
        self.send(Outgoing::Changed(change));
    }

    fn lock_state(&self, locked: bool) {
        self.send(Outgoing::LockState { locked });
    }

    fn meeting_offer(&self, offer: MeetingOffer) {
        self.send(Outgoing::MeetingOffer(offer));
    }
}

/// Send to several sinks at once.
///
/// What the desktop app runs while it is sharing: the window still needs its
/// events, and so does every paired device. Neither can be told to look at the
/// other's.
pub struct Fanout(Vec<std::sync::Arc<dyn EventSink>>);

impl Fanout {
    pub fn new(sinks: Vec<std::sync::Arc<dyn EventSink>>) -> Self {
        Self(sinks)
    }
}

impl EventSink for Fanout {
    fn notify(&self, notification: Notification) {
        for sink in &self.0 {
            sink.notify(notification.clone());
        }
    }

    fn changed(&self, change: Change) {
        for sink in &self.0 {
            sink.changed(change.clone());
        }
    }

    fn lock_state(&self, locked: bool) {
        for sink in &self.0 {
            sink.lock_state(locked);
        }
    }

    fn meeting_offer(&self, offer: MeetingOffer) {
        for sink in &self.0 {
            sink.meeting_offer(offer.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use karmayoga_service::events::{Kind, Level, Op};

    fn notice(domain: Option<Scope>) -> Outgoing {
        let notice = Notification::new(Level::Info, "Ada Lovelace").body("Re: the engine");
        Outgoing::Notify(match domain {
            Some(domain) => notice.domain(domain),
            None => notice,
        })
    }

    fn changed(kind: Kind) -> Outgoing {
        Outgoing::Changed(Change::new(kind, Op::Updated))
    }

    fn offer() -> Outgoing {
        Outgoing::MeetingOffer(MeetingOffer {
            event_id: karmayoga_core::id::EventId::new(),
            calendar_id: karmayoga_core::id::CalendarId::new(),
            uid: "uid".into(),
            series: None,
            title: "Design sync".into(),
            start: jiff::Timestamp::UNIX_EPOCH,
            end: jiff::Timestamp::UNIX_EPOCH,
            calendar_name: "Work".into(),
            automatic: false,
            join_url: Some("https://meet.example/abc".into()),
            ready: true,
            auto_record: false,
        })
    }

    #[test]
    fn a_notice_reaches_only_a_stream_that_holds_its_domain() {
        let mail = notice(Some(Scope::Mail));
        assert!(is_for(&[Scope::Mail], &mail));
        assert!(is_for(&[Scope::Tasks, Scope::Mail], &mail));
        assert!(is_for(&[Scope::All], &mail));
        assert!(!is_for(&[Scope::Tasks], &mail));
        assert!(!is_for(&[Scope::Calendars, Scope::Journals], &mail));
        assert!(!is_for(&[], &mail));
    }

    #[test]
    fn a_notice_nobody_classified_reaches_only_a_stream_holding_everything() {
        let unclassified = notice(None);
        assert!(is_for(&[Scope::All], &unclassified));
        assert!(!is_for(&[Scope::Mail], &unclassified));
        assert!(!is_for(&[Scope::Mail, Scope::Health, Scope::Calendars], &unclassified));
    }

    #[test]
    fn a_change_reaches_only_a_stream_that_holds_its_kinds_domain() {
        assert!(is_for(&[Scope::Tasks], &changed(Kind::Task)));
        assert!(is_for(&[Scope::Health], &changed(Kind::Dose)));
        assert!(is_for(&[Scope::Mail], &changed(Kind::Thread)));
        assert!(is_for(&[Scope::All], &changed(Kind::Txn)));
        assert!(!is_for(&[Scope::Tasks], &changed(Kind::Txn)));
        assert!(!is_for(&[Scope::Tasks], &changed(Kind::Thread)));
        assert!(!is_for(&[Scope::Library, Scope::Notes], &changed(Kind::LabResult)));
    }

    #[test]
    fn a_change_no_scope_covers_reaches_only_a_stream_holding_everything() {
        for kind in [Kind::Settings, Kind::BackgroundTask] {
            assert!(is_for(&[Scope::All], &changed(kind)), "{kind:?}");
            assert!(
                !is_for(&[Scope::Mail, Scope::Agent, Scope::Journals], &changed(kind)),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_meeting_offer_reaches_the_calendar_and_the_meetings_and_nobody_else() {
        assert!(is_for(&[Scope::Calendars], &offer()));
        assert!(is_for(&[Scope::Meetings], &offer()));
        assert!(is_for(&[Scope::All], &offer()));
        assert!(!is_for(&[Scope::Mail, Scope::Tasks, Scope::Notes], &offer()));
        assert!(!is_for(&[], &offer()));
    }

    #[test]
    fn the_lock_state_reaches_every_stream() {
        let locked = Outgoing::LockState { locked: true };
        assert!(is_for(&[], &locked));
        assert!(is_for(&[Scope::Tasks], &locked));
        assert!(is_for(&[Scope::All], &locked));
    }

    /// The whole path, not only the predicate: what a narrow stream is never
    /// sent does not turn up on the wire, and what it is sent does.
    #[tokio::test]
    async fn a_narrow_stream_is_sent_only_what_it_may_read() {
        let broadcaster = Broadcaster::new();
        let (ended, until) = tokio::sync::watch::channel(false);
        let stream = broadcaster.events_until("device".into(), vec![Scope::Tasks], until);
        broadcaster.notify(Notification::new(Level::Info, "Ada Lovelace").domain(Scope::Mail));
        broadcaster
            .notify(Notification::new(Level::Info, "Time for Metformin").domain(Scope::Health));
        broadcaster.notify(Notification::new(Level::Info, "Unclassified"));
        broadcaster.meeting_offer(match offer() {
            Outgoing::MeetingOffer(offer) => offer,
            _ => unreachable!(),
        });
        broadcaster.changed(Change::new(Kind::Thread, Op::Updated));
        broadcaster.changed(Change::new(Kind::Task, Op::Created));
        broadcaster.lock_state(true);
        let _ = ended.send(true);
        let events: Vec<String> =
            stream.map(|e| format!("{:?}", e.unwrap())).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events[0].contains("changed") && events[0].contains("task"), "{events:?}");
        assert!(events[1].contains("lockState"), "{events:?}");
        let said = events.join("\n");
        for secret in ["Ada Lovelace", "Metformin", "Unclassified", "Design sync", "meet.example"] {
            assert!(!said.contains(secret), "{secret} leaked: {said}");
        }
    }

    /// The order the vault locking happens in: `lockState` is said, then the
    /// session ends. The stream must carry the first before it stops for the
    /// second.
    #[tokio::test]
    async fn what_was_said_before_a_session_ended_still_goes_out() {
        let broadcaster = Broadcaster::new();
        let (ended, until) = tokio::sync::watch::channel(false);
        let stream = broadcaster.events_until("web-me".into(), vec![Scope::All], until);
        broadcaster.lock_state(true);
        let _ = ended.send(true);
        let events: Vec<String> =
            stream.map(|e| format!("{:?}", e.unwrap())).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].contains("lockState"), "{events:?}");
    }

    #[tokio::test]
    async fn a_session_that_has_ended_hears_nothing_more() {
        let broadcaster = Broadcaster::new();
        let (ended, until) = tokio::sync::watch::channel(false);
        let stream = broadcaster.events_until("web-me".into(), vec![Scope::All], until);
        let _ = ended.send(true);
        let events: Vec<_> = stream.collect().await;
        assert!(events.is_empty());
    }
}
