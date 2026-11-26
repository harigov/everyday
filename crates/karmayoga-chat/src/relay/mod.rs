//! Everything between a chat and the assistant that is the same on every
//! service.
//!
//! A [`Connector`](crate::connector::Connector) hands this module what the
//! owner said, already vetted: it came from the link's owner, in a chat nobody
//! else can read. What happens next is the same whether the chat is on Matrix
//! or anything later -- pick the conversation it belongs to, run a turn,
//! answer the assistant's questions, send one reply -- and is written once
//! here, against the trait, so a second service is a connector and nothing
//! more. See `docs/plans/chat.md`, "The relay", for the behaviour this is
//! built to.
//!
//! # Shape
//!
//! ```text
//!   inbox ──▶ dispatcher ──▶ per-chat queue ──▶ worker ──▶ Service::chat_turn
//!                 │                                 ▲
//!                 └──── `!stop`, answers ───────────┘   (through the chat's Shared)
//! ```
//!
//! **One dispatcher** reads the inbox and never waits on anything slow. It
//! decides, with [`route`], what each item is for, and acts at once on the
//! two things that must not queue behind a turn that is running: `!stop`, and
//! an answer to a question the assistant is waiting on. Everything else goes to
//! the chat's queue.
//!
//! **One worker per chat** takes that queue one item at a time, so a chat
//! never has two turns at once -- the second message waits for the first's
//! reply, as it would with a person. Different chats do not wait for each
//! other. A worker is started the first time its chat is heard from and lives
//! until the relay ends.
//!
//! **Routines reach the owner through the same workers.** While the relay
//! runs it registers a [`ChatOutlet`] for its link; a routine's message, or
//! its question, arrives through it as a [`RoutineJob`] and is queued on the
//! owner's private chat like anything else, so it waits for a turn the owner
//! started and their answer is routed exactly as an answer to the
//! assistant's own question is.
//!
//! **A background job's update does not.** A post that names a job and no
//! conversation -- a watch saying its build has finished -- is sent the moment
//! it is made, from [`RelayOutlet`] straight to the owner's private chat, not
//! queued on its worker: an update that waited for an hour-long call to end
//! would be no update. The relay remembers which job each such message came
//! from ([`Posts`]), so that when the owner answers it with a reply -- Matrix's
//! own, on that message -- the turn it starts is told which job it is about.
//!
//! **Calls go through the same workers.** A call the owner makes to the
//! assistant is delivered as [`Incoming::Called`] and queued on its chat's
//! worker as one more piece of work, so a chat never has a call and a typed turn
//! at once. What cannot wait for the worker -- the caller's keys, the owner
//! hanging up, `!stop` -- is found by the dispatcher in the chat's
//! [`Shared`]. See [`crate::calling`], and `docs/plans/calls.md`.
//!
//! **A [`Shared`] per chat** is the only thing the dispatcher and a worker both
//! touch: which turn is running (so `!stop` knows what to cancel) and which
//! question is waiting (so an answer knows where to go). It is a plain
//! `std::sync::Mutex`, held for a few lines and never across an `.await`, and
//! everything the dispatcher does with it is synchronous -- which is what
//! keeps the dispatcher from ever stalling on a network call.
//!
//! # What is pure
//!
//! Every decision that could be wrong without anybody noticing -- routing an
//! item, reading a command, choosing the conversation, deciding a message is
//! too old to run, wording a confirmation, choosing the one message a turn
//! ends with -- is a small function of its inputs with a test beside it. The
//! rest is plumbing, and is tested end to end in `tests/relay.rs` against a
//! fake connector and a fake model.

use crate::call::{CallEnd, ToCall};
use crate::calling::{
    self, CallOffer, CallSlot, Confirmation, Confirming, Line, Offered, Transcript,
};
use crate::connector::{
    CallId, CallKey, ChatId, Choice, Connector, Document, HangUp, Incoming, MessageRef, Picture,
    Question, Sent, Shown,
};
use futures::FutureExt;
use futures::future::BoxFuture;
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::chat::{ChatLink, Via};
use karmayoga_core::{
    ChatLinkId, Conversation, ConversationId, ConversationQuery, FileId, JobId, Vault,
};
use karmayoga_service::agent::{AgentEvent, ConfirmAnswer, Pending, Sink, Turned};
use karmayoga_service::chat::{CallDelivered, ChatOutlet, ChatTurnInput, FromRun, escape_markdown};
use karmayoga_service::error::CommandResult;
use karmayoga_service::service::Service;
use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceLink};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

pub use crate::calling::{CallBackend, Media, RealCalls};

mod calls;
mod dispatch;
mod outlet;
mod routing;
mod say;
mod shared;
#[cfg(test)]
mod tests;
mod worker;

pub use self::calls::RING_FOR;
pub(crate) use self::outlet::stopped;
use self::{calls::*, dispatch::*, outlet::*, routing::*, say::*, shared::*, worker::*};

/// How long a question may go unanswered before the relay answers it itself.
///
/// Half an hour: long enough to finish a meeting before replying to "delete
/// the deck?", short enough that a turn is not parked on a phone that went
/// into a drawer until tomorrow.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How old a message may be when it arrives and still be acted on.
///
/// A message that old was sent while the bot was offline. "Reply to Sam and
/// say yes" is not something to do twelve hours late on the strength of a
/// sentence nobody remembers writing, so it is not run; the owner is told, and
/// can send it again.
pub const STALE_AFTER: SignedDuration = SignedDuration::from_hours(12);

/// How long a picture or a file sent with nothing written beside it waits for
/// the words that go with it.
///
/// A phone sends a picture and the question about it as two messages more
/// often than as one: the photo goes, then "what is this plant?" is typed
/// under it. Answering the picture the moment it lands would answer a
/// question nobody asked yet, and then the real one separately. So it is
/// held, and goes with whatever is said next -- or alone, once a minute has
/// passed with nothing more, which is ample time to type a question and
/// short enough that a picture sent on its own is not left unanswered.
/// Further pictures and files sent meanwhile are gathered with it, in the
/// order they came.
pub const PICTURE_WAIT: Duration = Duration::from_secs(60);

/// How long the relay gives its workers to finish when it is stopped, before
/// abandoning them. Inside the supervisor's own five seconds, so the relay
/// ends cleanly rather than by being aborted.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The relay's clocks, gathered so a test can shorten them. Everything else
/// about the relay's behaviour is fixed; these are the numbers that would
/// otherwise make a test wait half an hour or fake a twelve-hour-old
/// message.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// See [`CONFIRM_TIMEOUT`].
    pub confirm_timeout: Duration,
    /// See [`STALE_AFTER`].
    pub stale_after: SignedDuration,
    /// See [`SHUTDOWN_GRACE`].
    pub shutdown_grace: Duration,
    /// See [`PICTURE_WAIT`].
    pub picture_wait: Duration,
    /// See [`RING_FOR`].
    pub ring_for: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            confirm_timeout: CONFIRM_TIMEOUT,
            stale_after: STALE_AFTER,
            shutdown_grace: SHUTDOWN_GRACE,
            picture_wait: PICTURE_WAIT,
            ring_for: RING_FOR,
        }
    }
}

const NEW_CONVERSATION: &str = "Starting a new conversation.";
const STOPPED: &str = "Stopped.";
const NOTHING_RUNNING: &str = "Nothing is running.";
const NO_REPLY: &str = "(no reply)";
const UNREADABLE_REPLY: &str = "I couldn't read that message \u{2014} it was encrypted for a session I don't have. Try sending it again.";
const NO_ANSWER: &str = "No answer, so I'll carry on with my best guess.";
/// Leads a routine's result that was to be said on a call, written instead
/// because the owner was on a call already; and why, for the run's log.
const BUSY_INSTEAD: &str = "I'd have called with this, but you were already on a call.";
const BUSY_WHY: &str = "another call had the line";

/// Answer what arrives in `inbox` until `stop` turns true or the inbox
/// closes.
///
/// [`run_with`] with the real clocks.
pub async fn run(
    service: Arc<Service>,
    vault: Arc<Vault>,
    link: ChatLink,
    connector: Arc<dyn Connector>,
    inbox: mpsc::Receiver<Incoming>,
    stop: watch::Receiver<bool>,
) {
    run_with(service, vault, link, connector, inbox, stop, Timing::default()).await;
}

/// [`run`], with the clocks given.
///
/// Ends in one of two ways, and they differ on purpose.
///
/// * **`stop` turns true** -- the vault is locking, or the link is being
///   restarted. Every running turn is cancelled, every waiting question is
///   declined, workers get [`Timing::shutdown_grace`] to unwind, and nothing
///   more is sent: the owner is not told "Stopped." about a turn the vault
///   itself is stopping.
/// * **The inbox closes** -- nothing more can arrive, so nothing more can be
///   answered either. Questions are declined (there is no one to ask), but
///   turns already running or queued finish, and their replies go out. This is
///   how a test drains the relay, and what a connector that ends cleanly would
///   expect.
pub async fn run_with(
    service: Arc<Service>,
    vault: Arc<Vault>,
    link: ChatLink,
    connector: Arc<dyn Connector>,
    inbox: mpsc::Receiver<Incoming>,
    stop: watch::Receiver<bool>,
    timing: Timing,
) {
    run_with_calls(service, vault, link, connector, inbox, stop, timing, Arc::new(RealCalls)).await;
}

/// [`run_with`], with what answers a call given: [`RealCalls`] in the app, and
/// a stand-in in a test that has no speech models or sockets to answer with.
///
/// A call ends as the relay does, whichever of the two ways that is: a call
/// is a conversation with somebody who can be reached only through this
/// relay, so once it cannot be answered or hung up there is nothing for it
/// to wait for. The call is told to end and hung up with
/// [`HangUp::Finished`], within [`Timing::shutdown_grace`] when the relay is
/// stopped.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_with_calls(
    service: Arc<Service>,
    vault: Arc<Vault>,
    link: ChatLink,
    connector: Arc<dyn Connector>,
    mut inbox: mpsc::Receiver<Incoming>,
    mut stop: watch::Receiver<bool>,
    timing: Timing,
    calls: Arc<dyn CallBackend>,
) {
    tracing::info!(service = link.service.label(), "a chat relay is up");
    let (outbound_tx, mut outbound) = mpsc::unbounded_channel();
    let posts = JobPosts::default();
    let outlet: Arc<dyn ChatOutlet> = Arc::new(RelayOutlet {
        connector: connector.clone(),
        outbound: outbound_tx,
        posts: posts.clone(),
        stop: stop.clone(),
    });
    let _open = OpenOutlet::new(service.clone(), link.id, outlet);
    let context = Arc::new(Context {
        service,
        vault,
        link,
        connector,
        timing,
        stop: stop.clone(),
        calls,
        line: Mutex::new(Line::default()),
        posts,
    });
    let mut chats = Chats::new(context);

    let ended = loop {
        tokio::select! {
            biased;
            () = stopped(&mut stop) => break End::Stopped,
            item = inbox.recv() => match item {
                Some(item) => chats.dispatch(item),
                None => break End::InboxClosed,
            },
            Some((chat, job)) = outbound.recv() => chats.routine(chat, job),
        }
    };

    chats.wind_down(ended).await;
    tracing::info!("a chat relay is down");
}
