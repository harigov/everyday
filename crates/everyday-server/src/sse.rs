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
//! # A slow reader is dropped, not waited for
//!
//! `broadcast` gives every receiver a bounded buffer and tells a receiver that
//! fell behind that it missed some. That is the right failure: a phone on a bad
//! connection must not be able to stall the window on the machine holding the
//! vault. A client that is told it lagged reloads everything, which is correct
//! and rare.

use axum::response::sse::{Event, KeepAlive, Sse};
use everyday_service::events::{Change, EventSink, Notification};
use futures::stream::Stream;
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
}

impl Outgoing {
    fn name(&self) -> &'static str {
        match self {
            Outgoing::Notify(_) => "notify",
            Outgoing::Changed(_) => "changed",
            Outgoing::LockState { .. } => "lockState",
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

    /// A stream for one client, skipping what that client did itself.
    pub fn stream(
        &self,
        me: String,
    ) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + use<>> {
        let rx = self.tx.subscribe();
        let stream = futures::stream::unfold(rx, move |mut rx| {
            let me = me.clone();
            async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            if event.origin().is_some_and(|origin| origin == me) {
                                continue;
                            }
                            let sse = Event::default()
                                .event(event.name())
                                .json_data(&event)
                                .unwrap_or_else(|_| Event::default().comment("unencodable"));
                            return Some((Ok(sse), rx));
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
        });
        Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE))
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
}
