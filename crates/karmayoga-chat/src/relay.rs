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

/// The outlet a running relay offers the rest of the service, registered for
/// exactly as long as the relay runs -- dropped with it, however it ends.
struct OpenOutlet {
    service: Arc<Service>,
    link: ChatLinkId,
    outlet: Arc<dyn ChatOutlet>,
}

impl OpenOutlet {
    fn new(service: Arc<Service>, link: ChatLinkId, outlet: Arc<dyn ChatOutlet>) -> Self {
        service.open_chat_outlet(link, outlet.clone());
        Self { service, link, outlet }
    }
}

impl Drop for OpenOutlet {
    fn drop(&mut self) {
        self.service.close_chat_outlet(self.link, &self.outlet);
    }
}

/// What a routine needs from the owner: [`ChatOutlet`]'s three calls, queued
/// on the owner's private chat with the half that answers them.
pub(crate) enum RoutineJob {
    Post {
        from: FromRun,
        markdown: String,
        done: oneshot::Sender<Result<(), String>>,
    },
    Confirm {
        from: FromRun,
        kind: String,
        subject: String,
        can_park: bool,
        /// The website "always allow" would add, when that is on offer.
        site: Option<String>,
        answer: oneshot::Sender<ConfirmAnswer>,
    },
    Ask {
        from: FromRun,
        question: String,
        choices: Vec<String>,
        answer: oneshot::Sender<Option<String>>,
    },
    /// Call the owner with a run's result: `spoken` when they pick up, and
    /// `markdown` posted instead if they do not.
    Call {
        from: FromRun,
        markdown: String,
        spoken: String,
        delivered: oneshot::Sender<Result<CallDelivered, String>>,
    },
    /// Send the pictures a run made, after its result. Answers how many went.
    Show {
        pictures: Vec<FileId>,
        done: oneshot::Sender<Result<usize, String>>,
    },
}

/// What the owner is told when a message for their private chat has nowhere
/// to go yet.
const NOT_JOINED: &str = "the owner has not joined the bot's private room yet";

/// [`ChatOutlet`] over a running relay: finds the owner's private chat, and
/// hands the job to the dispatcher, which queues it on that chat's worker --
/// except a background job's update, which it sends itself, at once.
struct RelayOutlet {
    connector: Arc<dyn Connector>,
    outbound: mpsc::UnboundedSender<(ChatId, RoutineJob)>,
    /// Which job each message sent for one came from, shared with the relay's
    /// own sending: a reply to any of them is looked up in the same list.
    posts: JobPosts,
    /// The relay's own stop, for the one thing this sends without a worker.
    stop: watch::Receiver<bool>,
}

impl RelayOutlet {
    /// Queue a job for the owner's private chat. `Err` says why it could not
    /// be: there is no such chat yet, or the relay has stopped.
    async fn queue(
        connector: Arc<dyn Connector>,
        outbound: mpsc::UnboundedSender<(ChatId, RoutineJob)>,
        job: RoutineJob,
    ) -> Result<(), String> {
        let chat = connector.home().await.ok_or(NOT_JOINED)?;
        outbound.send((chat, job)).map_err(|_| "the chat link has stopped".to_string())
    }

    /// Send a background job's update to the owner's private chat now, and
    /// remember which job it was from.
    ///
    /// The update is not queued, because a chat's worker is one thing at a
    /// time: behind a turn the owner started it waits for the reply, and
    /// behind a call it waits for the hang-up -- and the watch that said the
    /// build had failed was most useful an hour ago. So it goes out from here,
    /// beside whatever the worker is doing, and nothing about the chat's
    /// conversation changes: it is not a reply to anything, and the chat does
    /// not carry on from it. What the owner types next is still a message in
    /// the conversation they were in -- unless they reply to this one, which
    /// [`Posts`] is for.
    ///
    /// Refused once the relay is stopping, as everything is: the vault may be
    /// locking, and the one thing a worker does when it is stopped is send
    /// nothing more.
    async fn post_at_once(
        connector: Arc<dyn Connector>,
        posts: JobPosts,
        stop: watch::Receiver<bool>,
        job: JobId,
        markdown: String,
    ) -> Result<(), String> {
        if *stop.borrow() {
            return Err("the chat link is stopping".to_string());
        }
        let chat = connector.home().await.ok_or(NOT_JOINED)?;
        let sent = connector.send(&chat, &markdown).await.map_err(|e| e.to_string())?;
        posts.record(sent, job);
        Ok(())
    }
}

/// The job a post is an update from, when it is to be sent at once rather
/// than in turn: it names a job, and no conversation to carry on in.
///
/// A job's *answer* names its conversation, which the owner's reply then
/// carries on, so it takes the worker's turn like a routine's result does. An
/// update has no conversation of its own to give -- a watch's looks are not a
/// thread the owner talks in -- and is not worth waiting for.
pub(crate) fn immediate(from: &FromRun) -> Option<JobId> {
    match (from.conversation, from.job) {
        (None, Some(job)) => Some(job),
        _ => None,
    }
}

/// How many messages the relay remembers the job of: enough for several days
/// of a busy watch, and a few kilobytes -- a reply to something older is
/// answered like any other message, which is the safe direction to be wrong in.
const POSTS_REMEMBERED: usize = 500;

/// Which background job each message this side sent came from, for the last
/// [`POSTS_REMEMBERED`] of them, oldest forgotten first.
///
/// Matrix names a reply by the event it answers, and nothing else says which
/// of the owner's jobs that was, so the relay writes down what it sent.
/// Only messages that came from a job are here: a reply to anything else is
/// not about one.
#[derive(Debug)]
pub(crate) struct Posts {
    /// In the order they were recorded, to know which to forget.
    order: VecDeque<MessageRef>,
    jobs: HashMap<MessageRef, JobId>,
    limit: usize,
}

impl Default for Posts {
    fn default() -> Self {
        Self::remembering(POSTS_REMEMBERED)
    }
}

impl Posts {
    /// A list that forgets the oldest once it holds `limit`.
    pub(crate) fn remembering(limit: usize) -> Self {
        Self { order: VecDeque::new(), jobs: HashMap::new(), limit: limit.max(1) }
    }

    /// Remember that `message` came from `job`. A message named twice is
    /// kept once, with the later word, and does not use up a second place.
    pub(crate) fn record(&mut self, message: MessageRef, job: JobId) {
        if self.jobs.insert(message.clone(), job).is_none() {
            self.order.push_back(message);
        }
        while self.order.len() > self.limit {
            if let Some(oldest) = self.order.pop_front() {
                self.jobs.remove(&oldest);
            }
        }
    }

    /// The job `message` came from, if it is one of the remembered.
    pub(crate) fn job_of(&self, message: &MessageRef) -> Option<JobId> {
        self.jobs.get(message).copied()
    }

    /// The job a message the owner sent as a reply to `reply_to` is about:
    /// the one the message it answers came from. `None` for a message that
    /// is not a reply, and for a reply to anything else this side sent --
    /// its own answers, its questions, a routine's result.
    pub(crate) fn about(&self, reply_to: Option<&MessageRef>) -> Option<JobId> {
        self.job_of(reply_to?)
    }
}

/// The relay's [`Posts`], shared by what sends (the outlet, the workers) and
/// what reads (the workers). Locked for a line at a time, never across an
/// `.await`.
#[derive(Debug, Clone, Default)]
pub(crate) struct JobPosts(Arc<Mutex<Posts>>);

impl JobPosts {
    pub(crate) fn record(&self, message: MessageRef, job: JobId) {
        self.0.lock().unwrap().record(message, job);
    }

    pub(crate) fn about(&self, reply_to: Option<&MessageRef>) -> Option<JobId> {
        self.0.lock().unwrap().about(reply_to)
    }
}

impl ChatOutlet for RelayOutlet {
    fn post(&self, from: FromRun, markdown: String) -> BoxFuture<'static, Result<(), String>> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
        if let Some(job) = immediate(&from) {
            let (posts, stop) = (self.posts.clone(), self.stop.clone());
            return Box::pin(Self::post_at_once(connector, posts, stop, job, markdown));
        }
        Box::pin(async move {
            let (done, sent) = oneshot::channel();
            Self::queue(connector, outbound, RoutineJob::Post { from, markdown, done }).await?;
            sent.await.map_err(|_| "the chat link stopped before it was sent".to_string())?
        })
    }

    fn confirm(
        &self,
        from: FromRun,
        kind: String,
        subject: String,
        can_park: bool,
        site: Option<String>,
    ) -> BoxFuture<'static, ConfirmAnswer> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
        Box::pin(async move {
            let (answer, answered) = oneshot::channel();
            let job = RoutineJob::Confirm { from, kind, subject, can_park, site, answer };
            if let Err(why) = Self::queue(connector, outbound, job).await {
                tracing::info!(%why, "a routine's question could not be put to the chat");
                return ConfirmAnswer::Decline;
            }
            answered.await.unwrap_or(ConfirmAnswer::Decline)
        })
    }

    fn ask(
        &self,
        from: FromRun,
        question: String,
        choices: Vec<String>,
    ) -> BoxFuture<'static, Option<String>> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
        Box::pin(async move {
            let (answer, answered) = oneshot::channel();
            let job = RoutineJob::Ask { from, question, choices, answer };
            if let Err(why) = Self::queue(connector, outbound, job).await {
                tracing::info!(%why, "a routine's question could not be put to the chat");
                return None;
            }
            answered.await.ok().flatten()
        })
    }

    fn call(
        &self,
        from: FromRun,
        markdown: String,
        spoken: String,
    ) -> BoxFuture<'static, Result<CallDelivered, String>> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
        Box::pin(async move {
            let (delivered, done) = oneshot::channel();
            let job = RoutineJob::Call { from, markdown, spoken, delivered };
            Self::queue(connector, outbound, job).await?;
            done.await.map_err(|_| "the chat link stopped before it called".to_string())?
        })
    }

    fn show(
        &self,
        _from: FromRun,
        pictures: Vec<FileId>,
    ) -> BoxFuture<'static, Result<usize, String>> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
        Box::pin(async move {
            let (done, shown) = oneshot::channel();
            Self::queue(connector, outbound, RoutineJob::Show { pictures, done }).await?;
            shown.await.map_err(|_| "the chat link stopped before they were sent".to_string())?
        })
    }
}

/// Resolve once `stop` has been flipped, and never otherwise.
///
/// A sender that has gone away without flipping it is "never", not "now": the
/// relay then ends when its inbox does. The same rule the agent's own turns
/// use for their stop switch.
pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Why the dispatcher stopped reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Stopped,
    InboxClosed,
}

/// What every worker and the dispatcher share, and never changes.
struct Context {
    service: Arc<Service>,
    vault: Arc<Vault>,
    link: ChatLink,
    connector: Arc<dyn Connector>,
    timing: Timing,
    /// The relay's own stop. Read by workers so they do not start new work, or
    /// send replies, once the relay is being stopped.
    stop: watch::Receiver<bool>,
    /// What opens a call's voice session and answers its media.
    calls: Arc<dyn CallBackend>,
    /// Which call has the relay's one line. Locked for a line at a time, and
    /// only ever after a chat's `Shared`, never before.
    line: Mutex<Line>,
    /// Which background job each message sent for one came from: written when
    /// one is sent, read when the owner replies. See [`Posts`].
    posts: JobPosts,
}

impl Context {
    fn stopping(&self) -> bool {
        *self.stop.borrow()
    }

    fn pending(&self) -> Arc<Pending> {
        self.service.pending()
    }

    /// Send `markdown`. A failure is logged and goes no further: a chat
    /// server that is briefly unreachable must not take the relay down with
    /// it, and there is nobody to tell.
    async fn tell(&self, chat: &ChatId, markdown: &str) {
        if let Err(e) = self.connector.send(chat, markdown).await {
            tracing::warn!(chat = %chat.0, error = %e, "could not send a message to the chat");
        }
    }

    /// Send a picture kept in Files, as a picture: `Ok(false)` for one that
    /// is no longer a picture in Files, `Err` for one that could not be read
    /// or sent, logged here. The picture is in Files whether or not it
    /// arrives, so a chat turn carries on as it does past [`Self::tell`].
    async fn show(&self, chat: &ChatId, file: FileId) -> Result<bool, String> {
        show_picture(&self.vault, &self.connector, chat, file).await
    }

    /// A task that sends the pictures it is handed to `chat`, one after
    /// another in the order they came, so that whoever hands them over -- a
    /// turn's event loop, a call's -- never waits on an upload. Ends once the
    /// sender is dropped and the last has gone.
    fn showing(&self, chat: &ChatId) -> (mpsc::UnboundedSender<FileId>, Showing) {
        let (to_show, mut queued) = mpsc::unbounded_channel::<FileId>();
        let (vault, connector, chat) = (self.vault.clone(), self.connector.clone(), chat.clone());
        let task = tokio::spawn(async move {
            while let Some(file) = queued.recv().await {
                let _ = show_picture(&vault, &connector, &chat, file).await;
            }
        });
        (to_show, task)
    }

    /// Show, or stop showing, that the bot is typing. Best effort, like
    /// [`Self::tell`].
    async fn typing(&self, chat: &ChatId, on: bool) {
        if let Err(e) = self.connector.typing(chat, on).await {
            tracing::warn!(chat = %chat.0, error = %e, "could not change the typing notice");
        }
    }

    /// What the person named their assistant, or empty. Read when it is
    /// wanted rather than once at the start: it is a setting, and the owner
    /// may have just changed it on the desktop.
    async fn assistant_name(&self) -> String {
        let vault = self.vault.clone();
        karmayoga_service::blocking(move || Ok(vault.agent_settings()?.name))
            .await
            .map(|name| name.trim().to_string())
            .unwrap_or_default()
    }

    /// Mark a routine run's conversation as one this chat carries on, so that
    /// a relay started afresh -- which remembers nothing -- still finds it for
    /// the owner's reply. Best effort: without it the reply starts a new
    /// conversation, which is where a chat that has forgotten would be anyway.
    async fn adopt_conversation(&self, chat: &ChatId, conversation: ConversationId) {
        let vault = self.vault.clone();
        let via = Via {
            link: self.link.id,
            service: self.link.service.label().to_string(),
            chat: chat.0.clone(),
        };
        let marked = karmayoga_service::blocking(move || {
            let mut thread = vault.conversation(conversation)?;
            if thread.via.is_none() {
                thread.via = Some(via);
                thread.updated_at = Timestamp::now();
                vault.save_conversation(&thread)?;
            }
            Ok(())
        })
        .await;
        if let Err(e) = marked {
            tracing::warn!(chat = %chat.0, error = %e.message, "could not mark a routine's conversation as this chat's");
        }
    }

    /// The newest conversation this link already has in `chat`, for a relay
    /// that has just started and so remembers nothing. A vault that cannot be
    /// read is the same as one with no such conversation: the chat starts a
    /// fresh one, which is the safe direction to be wrong in.
    async fn find_conversation(&self, chat: &ChatId) -> Option<Current> {
        let vault = self.vault.clone();
        let all = karmayoga_service::blocking(move || {
            Ok(vault.conversations(&ConversationQuery::default())?)
        })
        .await;
        match all {
            Ok(all) => newest_conversation(&all, self.link.id, chat),
            Err(e) => {
                tracing::warn!(chat = %chat.0, error = %e, "could not look for this chat's conversation");
                None
            }
        }
    }
}

// ---- the dispatcher ---------------------------------------------------------

/// Every chat the relay has heard from.
struct Chats {
    context: Arc<Context>,
    handles: HashMap<ChatId, ChatHandle>,
    /// The workers. Held in a set rather than detached so that dropping the
    /// relay -- the supervisor aborting it -- aborts them too, instead of
    /// leaving turns running for a link that is gone.
    workers: JoinSet<()>,
}

/// How the dispatcher reaches one chat's worker.
struct ChatHandle {
    queue: mpsc::UnboundedSender<Job>,
    shared: Arc<Mutex<Shared>>,
}

/// One thing for a chat's worker to do, in order: what the owner asked for,
/// or what a routine needs from them.
enum Job {
    Owner(Work),
    Routine(RoutineJob),
    /// A call the owner is making. See [`CallWork`].
    Call(CallWork),
}

/// A call, queued: what the worker needs to answer it, and the relay's line,
/// held for as long as this exists.
struct CallWork {
    offer: CallOffer,
    _line: LineHeld<Arc<Context>>,
}

/// The relay's one line, held by one call. Given back when this is dropped,
/// however the call ends -- finished, refused, its worker panicking, or the
/// queue it sat in being thrown away -- so a call that goes wrong cannot
/// leave the line engaged for the rest of the relay's life.
///
/// Over the context however it is held: shared by the dispatcher, borrowed by
/// a worker.
struct LineHeld<C: std::ops::Deref<Target = Context>> {
    context: C,
    call: CallId,
}

impl<C: std::ops::Deref<Target = Context>> Drop for LineHeld<C> {
    fn drop(&mut self) {
        // Not `unwrap`: a panic while unwinding aborts the process.
        if let Ok(mut line) = self.context.line.lock() {
            line.release(&self.call);
        }
    }
}

impl ChatHandle {
    /// Queue `work`. A worker only goes away when the relay does, so a send
    /// that fails is the relay winding down, and dropping the item is right.
    fn enqueue(&self, work: Work) {
        let _ = self.queue.send(Job::Owner(work));
    }

    /// Queue a call. As [`Self::enqueue`]: a send that fails is the relay
    /// winding down.
    fn enqueue_call(&self, call: CallWork) {
        let _ = self.queue.send(Job::Call(call));
    }
}

impl Chats {
    fn new(context: Arc<Context>) -> Self {
        Self { context, handles: HashMap::new(), workers: JoinSet::new() }
    }

    /// The chat's worker, started if this is the first it has been heard from
    /// -- or if the last one is gone. A worker catches its own panics, so
    /// that second case should never happen; if it does, the chat gets a new
    /// worker rather than a queue nobody reads.
    fn handle_for(&mut self, chat: &ChatId) -> &ChatHandle {
        let alive = self.handles.get(chat).is_some_and(|handle| !handle.queue.is_closed());
        if !alive {
            let (queue, work) = mpsc::unbounded_channel();
            let shared = Arc::new(Mutex::new(Shared::default()));
            self.workers.spawn(worker(self.context.clone(), chat.clone(), shared.clone(), work));
            self.handles.insert(chat.clone(), ChatHandle { queue, shared });
        }
        &self.handles[chat]
    }

    /// Route one thing the owner did.
    ///
    /// Synchronous on purpose, and the whole of it under the chat's lock: the
    /// route is decided from the chat's state and acted on without that state
    /// changing in between, so an answer cannot arrive for a question that was
    /// just withdrawn. The one network call it would make -- the reply to
    /// `!stop` -- is spawned rather than awaited, so the dispatcher is back at
    /// the inbox at once.
    fn dispatch(&mut self, item: Incoming) {
        let context = self.context.clone();
        let chat = item.chat().clone();
        let now = Timestamp::now();
        let handle = self.handle_for(&chat);
        let mut shared = handle.shared.lock().unwrap();

        match route(item, shared.asked(), now, context.timing.stale_after) {
            Route::Ignore => {}
            Route::Queue(work) => handle.enqueue(work),
            Route::Answer(choice) => shared.answer(choice),
            Route::AnswerText(text) => shared.answer_text(text),
            Route::Hold { question, choice } => shared.hold(question, choice),
            Route::DeclineThen(work) => {
                shared.decline();
                handle.enqueue(work);
            }
            Route::Stop => {
                let reply = shared.stop(&context.pending());
                drop(shared);
                tokio::spawn(async move { context.tell(&chat, reply).await });
            }
            Route::Call(offer) => {
                // The owner picking up a call this side placed: the worker
                // that rang is waiting for exactly this, and holds the line.
                let offer = match shared.ringing.take() {
                    Some(answer) => {
                        shared.call = Some(CallSlot::queued(offer.call.clone()));
                        match answer.send(offer) {
                            Ok(()) => return,
                            // It gave up ringing a moment ago: an ordinary
                            // call, then.
                            Err(offer) => {
                                shared.call = None;
                                offer
                            }
                        }
                    }
                    None => offer,
                };
                // Under the chat's lock, like every route, and the line
                // taken inside it: a hang-up for this call cannot arrive
                // between the line being taken and the call being findable.
                let offered = context.line.lock().unwrap().offer(&offer.call, offer.expires, now);
                match offered {
                    Offered::Taken => {
                        shared.call = Some(CallSlot::queued(offer.call.clone()));
                        let line = LineHeld { context: context.clone(), call: offer.call.clone() };
                        handle.enqueue_call(CallWork { offer, _line: line });
                    }
                    Offered::Again => {}
                    Offered::Busy => {
                        drop(shared);
                        tokio::spawn(
                            async move { refuse_busy(&context, &chat, &offer.call).await },
                        );
                    }
                }
            }
            Route::CallKeys { call, keys } => shared.call_keys(&call, keys),
            Route::CallEnded(call) => {
                shared.call_ended(&call);
                // A call that was still ringing -- waiting behind a turn --
                // is not holding the line any more: a redial must be taken.
                context.line.lock().unwrap().give_up(&call);
            }
        }
    }

    /// Queue what a routine needs on `chat`'s worker. Dropped, if the relay
    /// is winding down, with its answering half -- which the routine reads
    /// as the chat having gone.
    ///
    /// A call while the owner is on one already is written instead, then
    /// and there: queued, it would wait for their call to end and ring them
    /// the moment they hung up.
    fn routine(&mut self, chat: ChatId, job: RoutineJob) {
        match job {
            RoutineJob::Call { from, markdown, delivered, .. }
                if self.context.line.lock().unwrap().is_busy(Timestamp::now()) =>
            {
                // Sent beside the call rather than queued behind it, and
                // without taking the chat's conversation over from the call.
                let connector = self.context.connector.clone();
                let posts = self.context.posts.clone();
                tokio::spawn(async move {
                    let sent = connector.send(&chat, &instead(BUSY_INSTEAD, &markdown)).await;
                    if let (Ok(message), Some(job)) = (&sent, from.job) {
                        posts.record(message.clone(), job);
                    }
                    let _ = delivered.send(
                        sent.map(|_| CallDelivered::Messaged { why: BUSY_WHY.into() })
                            .map_err(|e| e.to_string()),
                    );
                });
            }
            job => {
                let _ = self.handle_for(&chat).queue.send(Job::Routine(job));
            }
        }
    }

    /// End the workers: see [`run_with`] for the two ways this goes.
    async fn wind_down(self, ended: End) {
        let Chats { context, mut handles, mut workers } = self;

        let pending = context.pending();
        for handle in handles.values() {
            let mut shared = handle.shared.lock().unwrap();
            match ended {
                End::Stopped => shared.halt(&pending),
                End::InboxClosed => shared.close(),
            }
            // A call that is running is told to end, and one that is queued
            // is not answered, whichever way the relay is ending.
            shared.end_call();
        }
        // Closing each queue is what lets its worker, once it has finished
        // what it is doing, see that there is no more and end.
        handles.clear();

        let drained = async {
            while let Some(joined) = workers.join_next().await {
                if let Err(e) = joined {
                    tracing::warn!(error = %e, "a chat worker ended abnormally");
                }
            }
        };
        match ended {
            End::InboxClosed => drained.await,
            End::Stopped => {
                if tokio::time::timeout(context.timing.shutdown_grace, drained).await.is_err() {
                    // Whatever is left is aborted when the set is dropped.
                    tracing::warn!("a chat worker did not stop in time and was abandoned");
                }
            }
        }
    }
}

// ---- what is shared per chat ------------------------------------------------

/// What the dispatcher and one chat's worker both need to see.
#[derive(Default)]
struct Shared {
    /// The turn the worker has in hand, from the moment it takes a message up
    /// until the turn has ended.
    running: Option<Running>,
    /// The question the turn is waiting on the owner for.
    waiting: Option<Waiting>,
    /// Set once nothing more can be answered. A question asked after that is
    /// declined on the spot rather than left to wait out its timeout.
    closed: bool,
    /// The call this chat has, queued or running.
    call: Option<CallSlot>,
    /// The worker is ringing the owner, and their picking up -- which arrives
    /// as a call like any other -- goes to it rather than being queued.
    ringing: Option<oneshot::Sender<CallOffer>>,
}

/// The turn a worker has in hand.
#[derive(Default)]
struct Running {
    /// Known once the worker has chosen it; before that there is nothing to
    /// cancel yet.
    conversation: Option<ConversationId>,
    /// Whether the owner has said `!stop` (or the relay is stopping) since
    /// this turn was taken up. Three things follow from it, all in the worker:
    /// a turn not yet started is not started, a stop that landed before the
    /// turn existed is applied the moment it does, and the turn's own
    /// "Stopped." is not sent a second time.
    stop_requested: bool,
    /// Whether this is a routine's run waiting on a question here rather than
    /// a turn the owner started. `!stop` stops either; the relay stopping
    /// stops only its own -- the routine runs elsewhere, and is merely told
    /// nobody will answer.
    routine: bool,
}

/// A question the turn is waiting on.
struct Waiting {
    /// The message the question went out as. `None` while it is still being
    /// sent -- the window in which a typed answer must already count, and a
    /// tap on a message we have not been told the name of yet can only be
    /// held.
    question: Option<MessageRef>,
    /// Taps that arrived before `question` was known, waiting to be matched
    /// to it.
    early: Vec<(MessageRef, Choice)>,
    answer: Answer,
}

/// Who is waiting for the answer, and what kind of answer they want.
enum Answer {
    /// Yes, no or -- when `can_park` -- later, and -- when `can_trust` --
    /// always. Without `can_park`, "later" is read as "no"; without
    /// `can_trust`, so is "always".
    Confirm { can_park: bool, can_trust: bool, tx: oneshot::Sender<ConfirmAnswer> },
    /// Whatever the owner types next: a routine's question in its own words.
    Free { tx: oneshot::Sender<Option<String>> },
}

impl Answer {
    /// Answer with a tap.
    fn chose(self, choice: Choice) {
        match self {
            Answer::Confirm { can_park, can_trust, tx } => {
                let _ = tx.send(answer_for(choice, can_park, can_trust));
            }
            Answer::Free { tx } => {
                let word = match choice {
                    Choice::Yes => "yes",
                    Choice::No => "no",
                    Choice::Later => "later",
                    Choice::Always => "always",
                };
                let _ = tx.send(Some(word.to_string()));
            }
        }
    }

    /// Answer with what they typed. Only a free question is routed typed
    /// text this way; a confirmation reads it as a choice first.
    fn said(self, text: String) {
        match self {
            Answer::Free { tx } => {
                let _ = tx.send(Some(text));
            }
            Answer::Confirm { can_park, can_trust, tx } => {
                let choice = Choice::from_text(&text).unwrap_or(Choice::No);
                let _ = tx.send(answer_for(choice, can_park, can_trust));
            }
        }
    }

    /// No answer is coming: no, or nothing.
    fn decline(self) {
        match self {
            Answer::Confirm { tx, .. } => {
                let _ = tx.send(ConfirmAnswer::Decline);
            }
            Answer::Free { tx } => {
                let _ = tx.send(None);
            }
        }
    }
}

/// Whether a question is waiting, as [`route`] needs to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked<'a> {
    /// No question is waiting.
    Nothing,
    /// One is, and is still being sent.
    Unsent,
    /// One is, and went out as this message.
    Sent(&'a MessageRef),
    /// A routine's free question is waiting: whatever the owner types next
    /// is the answer.
    Open,
}

impl Shared {
    fn asked(&self) -> Asked<'_> {
        match &self.waiting {
            None => Asked::Nothing,
            Some(Waiting { answer: Answer::Free { .. }, .. }) => Asked::Open,
            Some(waiting) => match &waiting.question {
                None => Asked::Unsent,
                Some(question) => Asked::Sent(question),
            },
        }
    }

    /// Give the waiting question `choice` as its answer.
    fn answer(&mut self, choice: Choice) {
        // A closed receiver means the worker stopped waiting an instant ago;
        // the answer has nowhere to go and nothing depends on it.
        if let Some(waiting) = self.waiting.take() {
            waiting.answer.chose(choice);
        }
    }

    /// Give the waiting question what the owner typed.
    fn answer_text(&mut self, text: String) {
        if let Some(waiting) = self.waiting.take() {
            waiting.answer.said(text);
        }
    }

    /// Hold a tap on a message the question has not been matched to yet.
    fn hold(&mut self, question: MessageRef, choice: Choice) {
        if let Some(waiting) = &mut self.waiting {
            waiting.early.push((question, choice));
        }
    }

    /// Say no to the waiting question, if there is one.
    fn decline(&mut self) {
        if let Some(waiting) = self.waiting.take() {
            waiting.answer.decline();
        }
    }

    /// The question has gone out as `reference`: remember it, and count a tap
    /// that beat it here.
    fn question_sent(&mut self, reference: MessageRef) {
        let Some(waiting) = &mut self.waiting else { return };
        let early = std::mem::take(&mut waiting.early)
            .into_iter()
            .find(|(question, _)| *question == reference)
            .map(|(_, choice)| choice);
        waiting.question = Some(reference);
        if let Some(choice) = early {
            self.answer(choice);
        }
    }

    /// More of the caller's keys, for `call` if it is the one this chat
    /// has. For any other they are nobody's.
    fn call_keys(&mut self, call: &CallId, keys: Vec<CallKey>) {
        if let Some(slot) = self.call.as_mut().filter(|slot| slot.id == *call) {
            slot.heard(keys);
        }
    }

    /// The worker takes `call` up: see [`CallSlot::start`]. `None` if this chat
    /// no longer has it, or the owner ended it while it waited.
    fn call_start(
        &mut self,
        call: &CallId,
    ) -> Option<(Vec<CallKey>, mpsc::UnboundedReceiver<ToCall>)> {
        self.call.as_mut().filter(|slot| slot.id == *call).and_then(CallSlot::start)
    }

    /// Whether `call` is over as far as the owner is concerned.
    fn call_is_over(&self, call: &CallId) -> bool {
        self.call.as_ref().filter(|slot| slot.id == *call).is_none_or(CallSlot::is_ended)
    }

    /// The owner hung up `call`.
    fn call_ended(&mut self, call: &CallId) {
        if let Some(slot) = self.call.as_mut().filter(|slot| slot.id == *call) {
            slot.end();
        }
    }

    /// End the call this chat has, whichever stage it is at. Whether there was
    /// one to end.
    fn end_call(&mut self) -> bool {
        self.call.as_mut().is_some_and(CallSlot::end)
    }

    /// `!stop`'s half for a call: end it if the worker has taken it up. A call
    /// still queued is left to ring -- what the owner stopped is the turn it
    /// is waiting behind. Whether a call was ended.
    fn stop_call(&mut self) -> bool {
        self.call.as_mut().is_some_and(|slot| slot.is_started() && slot.end())
    }

    /// Cancel the running turn and withdraw the waiting question, and nothing
    /// more is going to be answered. What the relay stopping does.
    fn halt(&mut self, pending: &Pending) {
        if let Some(running) = &mut self.running {
            running.stop_requested = true;
            if let Some(conversation) = running.conversation
                && !running.routine
            {
                pending.cancel(conversation);
            }
        }
        self.close();
    }

    /// Nothing more can be answered: decline what is waiting, and decline
    /// whatever asks from now on.
    fn close(&mut self) {
        self.closed = true;
        self.decline();
        // A ring the worker is waiting on stops, and the worker takes it down.
        self.ringing = None;
    }

    /// The owner's `!stop`: cancel the turn that is running, and decline the
    /// question it may be waiting on. What to say back.
    fn stop(&mut self, pending: &Pending) -> &'static str {
        // A call is stopped by ending it, and is something running as far as
        // what to say back goes.
        // So is a ring: the worker that placed it stops ringing.
        let calling = self.stop_call() | self.ringing.take().is_some();
        let Some(running) = &mut self.running else {
            return if calling { STOPPED } else { NOTHING_RUNNING };
        };
        running.stop_requested = true;
        // False when the turn has not registered yet -- the worker is still
        // choosing its conversation, or `chat_turn` is a few lines from
        // `start_turn`. `stop_requested` is how the worker finds out.
        if let Some(conversation) = running.conversation {
            pending.cancel(conversation);
        }
        self.decline();
        STOPPED
    }
}

// ---- deciding what an item is for -------------------------------------------

/// A command the owner can type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    New,
    Stop,
    Help,
}

/// Read `text` as a command.
///
/// Only the whole message counts, case aside: `!stop the presses` is a
/// sentence for the assistant, and so is any other `!word`. `!` rather than
/// `/` because Element keeps `/` for its own commands.
pub(crate) fn parse_command(text: &str) -> Option<Command> {
    match text.trim().to_lowercase().as_str() {
        "!new" => Some(Command::New),
        "!stop" => Some(Command::Stop),
        "!help" => Some(Command::Help),
        _ => None,
    }
}

/// Something for a chat's worker to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Work {
    /// Run this as a turn. `reply_to` is the message of the relay's own the
    /// owner sent it as a reply to, when they did: if that was a background
    /// job's update, the turn is about that job.
    Say { text: String, reply_to: Option<MessageRef> },
    /// A picture, and what was written with it. Held by the worker until
    /// what is said next, when nothing was -- see [`PICTURE_WAIT`] -- and
    /// otherwise run as a turn at once, with anything already held.
    Show { picture: Picture, caption: String },
    /// A file that is not a picture to look at, and what was written with it.
    /// Held and run exactly as [`Work::Show`] is.
    Hand { document: Document, caption: String },
    /// `!new`.
    New,
    /// `!help`.
    Help,
    /// Send this, and nothing else happens. Queued rather than sent by the
    /// dispatcher so that a chat's replies come out in the order its messages
    /// came in.
    Reply(String),
}

impl Work {
    /// Words that are not a reply to anything.
    pub(crate) fn say(text: impl Into<String>) -> Self {
        Work::Say { text: text.into(), reply_to: None }
    }
}

/// What the dispatcher does with one thing the owner did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// `!stop`: act now, not in turn.
    Stop,
    /// It answers the waiting question.
    Answer(Choice),
    /// It answers a routine's free question, in the owner's own words.
    AnswerText(String),
    /// A tap on a message the question has not been matched to yet.
    Hold { question: MessageRef, choice: Choice },
    /// It is not an answer: withdraw the question, then carry on with this.
    DeclineThen(Work),
    /// Do this in turn.
    Queue(Work),
    /// A call: answer it in turn, if the line is free -- which only the
    /// dispatcher, with the relay's line in hand, can say.
    Call(CallOffer),
    /// More of the caller's keys. For the call this chat has, they are at
    /// once buffered or forwarded; for any other, nothing.
    CallKeys { call: CallId, keys: Vec<CallKey> },
    /// The owner hung up, now, not in turn.
    CallEnded(CallId),
    /// It means nothing.
    Ignore,
}

/// Decide what one thing the owner did is for, given whether a question is
/// waiting on them.
///
/// * A tap answers the question it was made on, and nothing else: a tap on an
///   old question, or on a message that was never one, is not an answer to
///   this.
/// * Typed text answers a waiting question if it reads as an answer. If it
///   does not, the owner has moved on: the question is declined, and the text
///   is taken as a new message rather than swallowed.
/// * A message older than `stale_after` is not run, whatever it says --
///   `!new` and `!stop` included: a command from last night is not an
///   instruction for now.
/// * `!stop` is never queued.
/// * A picture or a file never answers a question. It is the owner moving
///   on, as typed text that is not an answer is.
/// * A call, its keys and its hang-up never answer a question either,
///   and never decline one: the owner phoning is not the owner replying.
///
/// Takes the item rather than borrowing it, so that a file of two hundred
/// megabytes is moved into the work it becomes and not copied.
pub(crate) fn route(
    item: Incoming,
    asked: Asked<'_>,
    now: Timestamp,
    stale_after: SignedDuration,
) -> Route {
    match item {
        Incoming::Unreadable { .. } => Route::Queue(Work::Reply(UNREADABLE_REPLY.to_string())),
        // How old a call is does not matter here: it stops ringing at its
        // own `expires`, which the worker checks when it comes to answer, and
        // a call is never "sent last night". The keys and the hang-up are
        // acted on at once, as `!stop` is, for the same reason: they must not
        // wait behind the turn that is running.
        Incoming::Called { call, expires, .. } => Route::Call(CallOffer { call, expires }),
        Incoming::CallKeys { call, keys, .. } => Route::CallKeys { call, keys },
        Incoming::CallEnded { call, .. } => Route::CallEnded(call),
        Incoming::Chose { question, choice, .. } => match asked {
            Asked::Sent(waiting_on) if *waiting_on == question => Route::Answer(choice),
            Asked::Unsent => Route::Hold { question, choice },
            Asked::Sent(_) | Asked::Nothing | Asked::Open => Route::Ignore,
        },
        Incoming::Showed { picture, caption, at, .. } => {
            let work = Work::Show { picture, caption: caption.trim().to_string() };
            route_sent(work, at, asked, now, stale_after)
        }
        Incoming::Handed { document, caption, at, .. } => {
            let work = Work::Hand { document, caption: caption.trim().to_string() };
            route_sent(work, at, asked, now, stale_after)
        }
        Incoming::Said { text, at, reply_to, .. } => {
            let text = text.trim();
            if text.is_empty() {
                return Route::Ignore;
            }
            if let Some(hours) = stale_hours(at, now, stale_after) {
                return Route::Queue(Work::Reply(catch_up_reply(hours)));
            }
            let command = parse_command(text);
            if command == Some(Command::Stop) {
                return Route::Stop;
            }
            let work = match command {
                Some(Command::New) => Work::New,
                Some(Command::Help) => Work::Help,
                _ => Work::Say { text: text.to_string(), reply_to },
            };
            match asked {
                Asked::Nothing => Route::Queue(work),
                Asked::Unsent | Asked::Sent(_) => match Choice::from_text(text) {
                    Some(choice) => Route::Answer(choice),
                    None => Route::DeclineThen(work),
                },
                // Anything they write answers a free question -- except a
                // command, which is them doing something else.
                Asked::Open => match work {
                    Work::Say { text, .. } => Route::AnswerText(text),
                    other => Route::DeclineThen(other),
                },
            }
        }
    }
}

/// What a picture or a file is for: not run if it is too old, and otherwise
/// queued -- after declining a question it did not answer, if one was
/// waiting.
fn route_sent(
    work: Work,
    at: Timestamp,
    asked: Asked<'_>,
    now: Timestamp,
    stale_after: SignedDuration,
) -> Route {
    if let Some(hours) = stale_hours(at, now, stale_after) {
        return Route::Queue(Work::Reply(catch_up_reply(hours)));
    }
    match asked {
        Asked::Nothing => Route::Queue(work),
        // A picture or a file answers no question: the owner has moved on.
        Asked::Unsent | Asked::Sent(_) | Asked::Open => Route::DeclineThen(work),
    }
}

/// How many whole hours old a message sent at `at` is, if that is more than
/// `stale_after`. A message dated in the future (two clocks that disagree) is
/// not old.
pub(crate) fn stale_hours(
    at: Timestamp,
    now: Timestamp,
    stale_after: SignedDuration,
) -> Option<i64> {
    let age = now.duration_since(at);
    (age > stale_after).then(|| age.as_hours())
}

/// What the owner is told instead of a message that was too old to run.
pub(crate) fn catch_up_reply(hours: i64) -> String {
    format!(
        "This arrived while I was offline {hours} hours ago \u{2014} send it again if you still want it."
    )
}

// ---- which conversation -----------------------------------------------------

/// The conversation a chat is on, and when it last had a turn in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Current {
    id: ConversationId,
    last_used: Timestamp,
}

/// What to do about the next message's conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Which {
    Continue(ConversationId),
    Fresh,
}

/// Carry on with the chat's conversation, or start a new one.
///
/// New when there is none, or when it has been quiet for longer than
/// `fresh_after_minutes` -- so a morning's questions are one thread and the
/// evening's another. Zero means never: the chat is one conversation until
/// somebody types `!new`.
pub(crate) fn which_conversation(
    current: Option<Current>,
    now: Timestamp,
    fresh_after_minutes: u32,
) -> Which {
    match current {
        None => Which::Fresh,
        Some(current) => {
            let quiet = now.duration_since(current.last_used);
            let limit = SignedDuration::from_mins(i64::from(fresh_after_minutes));
            if fresh_after_minutes > 0 && quiet > limit {
                Which::Fresh
            } else {
                Which::Continue(current.id)
            }
        }
    }
}

/// The most recently updated conversation that arrived through `chat` on
/// `link`. A conversation started in the desktop's rail has no `via`, and one
/// from another chat or another link names a different pair, so neither is
/// ever picked up.
pub(crate) fn newest_conversation(
    conversations: &[Conversation],
    link: ChatLinkId,
    chat: &ChatId,
) -> Option<Current> {
    conversations
        .iter()
        .filter(|c| c.via.as_ref().is_some_and(|via| via.link == link && via.chat == chat.0))
        .max_by_key(|c| c.updated_at)
        .map(|c| Current { id: c.id, last_used: c.updated_at })
}

/// What a chat's worker remembers between messages.
#[derive(Default)]
struct Memory {
    current: Option<Current>,
    /// Whether the vault has been searched for this chat's conversation yet.
    /// Once is enough: from then on this worker is the only thing creating
    /// them.
    looked: bool,
}

impl Memory {
    /// Forget the conversation, for good: `!new`. `looked` is set so the
    /// forgotten one is not simply found again.
    fn forget(&mut self) {
        self.current = None;
        self.looked = true;
    }

    async fn conversation(&mut self, context: &Context, chat: &ChatId) -> ConversationId {
        if !self.looked {
            self.looked = true;
            self.current = context.find_conversation(chat).await;
        }
        let now = Timestamp::now();
        match which_conversation(self.current, now, context.link.fresh_after_minutes) {
            Which::Continue(id) => id,
            Which::Fresh => {
                let id = ConversationId::new();
                self.current = Some(Current { id, last_used: now });
                id
            }
        }
    }

    /// Carry on with `conversation` from here: a routine has just written to
    /// the owner from it, and their reply is to that.
    fn adopt(&mut self, conversation: ConversationId) {
        self.current = Some(Current { id: conversation, last_used: Timestamp::now() });
        self.looked = true;
    }

    /// A turn on `conversation` has just ended.
    fn used(&mut self, conversation: ConversationId) {
        if let Some(current) = &mut self.current
            && current.id == conversation
        {
            current.last_used = Timestamp::now();
        }
    }
}

// ---- the worker -------------------------------------------------------------

/// One chat's worker: take what is queued, one item at a time, until the relay
/// ends.
async fn worker(
    context: Arc<Context>,
    chat: ChatId,
    shared: Arc<Mutex<Shared>>,
    mut work: mpsc::UnboundedReceiver<Job>,
) {
    let mut memory = Memory::default();
    // Pictures and files sent with nothing written beside them, waiting for
    // the words that go with them. See `PICTURE_WAIT`.
    let mut held: Vec<Sent> = Vec::new();
    loop {
        let next = if held.is_empty() {
            work.recv().await
        } else {
            match tokio::time::timeout(context.timing.picture_wait, work.recv()).await {
                Ok(next) => next,
                // Nothing more came: what was sent is the whole message.
                Err(_) => Some(Job::Owner(Work::say(""))),
            }
        };
        let Some(item) = next else { break };
        // Stopped with messages still queued: they are not run, and nobody is
        // told. The owner's next message, after the vault unlocks, is a new
        // one.
        if context.stopping() {
            // A call that is still ringing is not left to ring: the owner's
            // phone would go on until the invite ran out. Everything queued
            // is in the queue already -- the dispatcher has stopped -- so
            // hang those up and go.
            hang_up_queued(&context, &chat, item).await;
            while let Ok(item) = work.try_recv() {
                hang_up_queued(&context, &chat, item).await;
            }
            break;
        }
        let (item, sent) = match gather(&mut held, item) {
            Gathered::Hold => continue,
            Gathered::Run(item, sent) => (item, sent),
            // Something else to do while pictures or files wait: they go
            // first, on their own, so they are answered in the order they
            // were sent.
            Gathered::First(sent, item) => {
                let alone = Job::Owner(Work::say(""));
                run_caught(&context, &chat, &shared, &mut memory, alone, sent).await;
                (item, Vec::new())
            }
        };
        run_caught(&context, &chat, &shared, &mut memory, item, sent).await;
    }
}

/// Hang up `job` if it is a call: the relay is stopping, so it will not be
/// answered. Anything else is dropped without a word, as it always was.
async fn hang_up_queued(context: &Context, chat: &ChatId, job: Job) {
    if let Job::Call(call) = job {
        hang_up(context, chat, &call.offer.call, HangUp::Refused).await;
    }
}

/// What a worker does with the next thing queued, given the pictures and
/// files it is holding. See [`gather`].
enum Gathered {
    /// A picture or a file with nothing said: hold it, and wait for more.
    Hold,
    /// Run this, with these.
    Run(Job, Vec<Sent>),
    /// Run what is held, alone, and then this.
    First(Vec<Sent>, Job),
}

/// Fold `item` into what is `held`, or run it.
///
/// A picture or a file with nothing written beside it is held. Anything the
/// owner says -- a caption included -- takes everything held with it.
/// Anything else (a command, a routine's message) finds what is held sent on
/// its own first.
fn gather(held: &mut Vec<Sent>, item: Job) -> Gathered {
    match item {
        Job::Owner(Work::Show { picture, caption }) => {
            hold_or_run(held, Sent::Picture(picture), caption)
        }
        Job::Owner(Work::Hand { document, caption }) => {
            hold_or_run(held, Sent::Document(document), caption)
        }
        // Whatever words come take what is held with them, and the message
        // they replied to, if any, goes on with them.
        Job::Owner(say @ Work::Say { .. }) => Gathered::Run(Job::Owner(say), std::mem::take(held)),
        other if held.is_empty() => Gathered::Run(other, Vec::new()),
        other => Gathered::First(std::mem::take(held), other),
    }
}

/// Add `sent` to what is `held`: and wait for more if nothing was written
/// with it, or run it as a turn with everything held and its `caption` as
/// the words.
fn hold_or_run(held: &mut Vec<Sent>, sent: Sent, caption: String) -> Gathered {
    held.push(sent);
    if caption.is_empty() {
        Gathered::Hold
    } else {
        Gathered::Run(Job::Owner(Work::say(caption)), std::mem::take(held))
    }
}

/// [`do_work`], with a panic in it caught. A panic in one turn must not take
/// the chat with it: the typing notice would stay on for ever, and every
/// message after it would queue behind a worker that is gone.
async fn run_caught(
    context: &Arc<Context>,
    chat: &ChatId,
    shared: &Arc<Mutex<Shared>>,
    memory: &mut Memory,
    item: Job,
    sent: Vec<Sent>,
) {
    // Whose call this is, if it is one: what is cleared and hung up after a
    // panic is that call, and not one queued behind it.
    let own_call = match &item {
        Job::Call(work) => Some(work.offer.call.clone()),
        _ => None,
    };
    let done =
        AssertUnwindSafe(do_work(context, chat, shared, memory, item, sent)).catch_unwind().await;
    if done.is_err() {
        tracing::error!(chat = %chat.0, "a chat turn panicked; carrying on with the next message");
        {
            let mut shared = shared.lock().unwrap();
            shared.running = None;
            shared.waiting = None;
            if let Some(call) = &own_call
                && shared.call.as_ref().is_some_and(|slot| slot.id == *call)
            {
                shared.call = None;
            }
        }
        // The panic dropped the call's voice session and media with it -- a
        // session whose input has closed stops -- but nothing told the owner's
        // phone.
        if let Some(call) = &own_call {
            hang_up(context, chat, call, HangUp::Failed).await;
        }
        context.typing(chat, false).await;
        if !context.stopping() {
            context.tell(chat, "Something went wrong: the assistant stopped unexpectedly.").await;
        }
    }
}

/// Do one thing queued for a chat. `sent` -- pictures and files -- go with a
/// turn: the owner's words, or a caption; and with nothing else; [`gather`]
/// sees that they only ever come with one.
async fn do_work(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    job: Job,
    sent: Vec<Sent>,
) {
    let work = match job {
        Job::Owner(work) => work,
        Job::Routine(job) => return for_routine(context, chat, shared, memory, job).await,
        Job::Call(call) => return take_call(context, chat, shared, memory, call).await,
    };
    match work {
        Work::Reply(text) => context.tell(chat, &text).await,
        Work::New => {
            memory.forget();
            context.tell(chat, NEW_CONVERSATION).await;
        }
        Work::Help => {
            let name = context.assistant_name().await;
            context.tell(chat, &help_text(&name)).await;
        }
        Work::Say { text, reply_to } => {
            // Looked up now, when the turn is about to run, and not when the
            // message arrived: the update it answers was recorded as soon as
            // the server named it, which is long before the owner's reply
            // could reach the front of the queue.
            let about = context.posts.about(reply_to.as_ref());
            converse(context, chat, shared, memory, text, sent, about).await
        }
        Work::Show { picture, caption } => {
            let sent = sent.into_iter().chain([Sent::Picture(picture)]).collect();
            converse(context, chat, shared, memory, caption, sent, None).await
        }
        Work::Hand { document, caption } => {
            let sent = sent.into_iter().chain([Sent::Document(document)]).collect();
            converse(context, chat, shared, memory, caption, sent, None).await
        }
    }
}

/// What a routine needs from the owner, done in turn with the chat's own
/// work.
///
/// While a routine's question waits, the run it came from is this chat's
/// running turn as far as `!stop` is concerned: stopping it stops the run.
async fn for_routine(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    job: RoutineJob,
) {
    let running = |from: &FromRun| {
        shared.lock().unwrap().running =
            Some(Running { conversation: from.conversation, stop_requested: false, routine: true });
    };
    match job {
        RoutineJob::Post { from, markdown, done } => {
            let _ = done.send(post_for_run(context, chat, memory, &from, &markdown).await);
        }
        RoutineJob::Confirm { from, kind, subject, can_park, site, answer } => {
            running(&from);
            let given = ask(
                context,
                chat,
                shared,
                &kind,
                &subject,
                can_park,
                site.as_deref(),
                Asking::Routine(&from.routine),
            )
            .await;
            shared.lock().unwrap().running = None;
            let _ = answer.send(given);
        }
        RoutineJob::Ask { from, question, choices, answer } => {
            running(&from);
            let given = ask_free(context, chat, shared, &from.routine, &question, &choices).await;
            shared.lock().unwrap().running = None;
            let _ = answer.send(given);
        }
        RoutineJob::Call { from, markdown, spoken, delivered } => {
            call_owner(context, chat, shared, memory, from, markdown, spoken, delivered).await;
        }
        RoutineJob::Show { pictures, done } => {
            let (mut sent, mut why) = (0, None);
            for file in pictures {
                match context.show(chat, file).await {
                    Ok(true) => sent += 1,
                    Ok(false) => {}
                    Err(e) => why = Some(e),
                }
            }
            let _ = done.send(match why {
                Some(why) if sent == 0 => Err(why),
                _ => Ok(sent),
            });
        }
    }
}

/// One turn: the owner's message in, the assistant's one reply out.
///
/// The steps are the design's "A turn": typing on; the turn itself, with its
/// events read as it goes so a question can be put to the owner while the
/// turn waits; typing off; one message.
///
/// `about_job` is the background job the message is a reply to an update of,
/// when it is one. It changes what the turn is told and nothing else: the
/// conversation is the chat's own, chosen as for any message, because a
/// reply to a watch's update is the owner talking to their assistant, and
/// what they say next belongs with what they said before.
async fn converse(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    text: String,
    sent: Vec<Sent>,
    about_job: Option<JobId>,
) {
    // Claimed before anything slow, so a `!stop` that lands while the
    // conversation is being looked for finds something to stop.
    shared.lock().unwrap().running = Some(Running::default());

    let conversation = memory.conversation(context, chat).await;

    let go = {
        let mut shared = shared.lock().unwrap();
        match &mut shared.running {
            Some(running) if !running.stop_requested => {
                running.conversation = Some(conversation);
                true
            }
            _ => false,
        }
    };
    if !go {
        // Stopped before it began. The owner has been told so already.
        shared.lock().unwrap().running = None;
        return;
    }

    let via = Via {
        link: context.link.id,
        service: context.link.service.label().to_string(),
        chat: chat.0.clone(),
    };
    // The sink is a synchronous `Fn` called from inside the turn; an unbounded
    // channel is how it hands events to this task without ever waiting.
    let (events_tx, mut events) = mpsc::unbounded_channel::<AgentEvent>();
    let sink: Sink = Arc::new(move |event| {
        let _ = events_tx.send(event);
    });

    context.typing(chat, true).await;

    let mut told = String::new();
    // What the assistant makes goes out as it is made, ahead of the words
    // about it, by a task of its own: the turn is not held up by an upload.
    let (to_show, showing) = context.showing(chat);
    // Pictures are shown to the model, and every one is also kept as a file;
    // anything else is only kept, and the assistant told. The service takes
    // the two apart and does not care what order they came in.
    let (mut pictures, mut documents) = (Vec::new(), Vec::new());
    for item in sent {
        match item {
            Sent::Picture(picture) => pictures.push((picture.bytes, picture.filename)),
            Sent::Document(document) => documents.push((document.bytes, document.filename)),
        }
    }
    let input = ChatTurnInput { via, conversation, prompt: text, pictures, documents, about_job };
    let mut turn = Box::pin(context.service.chat_turn_input(input, sink));
    let result = loop {
        tokio::select! {
            biased;
            Some(event) = events.recv() => {
                on_event(context, chat, shared, conversation, &mut told, &to_show, event).await;
            }
            result = &mut turn => break result,
        }
    };
    drop(turn);
    // Whatever the turn said in its last breath is in the channel still.
    while let Ok(event) = events.try_recv() {
        match event {
            AgentEvent::Delta { text } => told.push_str(&text),
            AgentEvent::Retrying { text } => told = text,
            AgentEvent::ToolFinished { ok: true, files, .. } => {
                for file in files {
                    let _ = to_show.send(file);
                }
            }
            _ => {}
        }
    }
    // The pictures before the reply that says what they are.
    drop(to_show);
    let mut showing = showing;
    if tokio::time::timeout(SHOW_FOR, &mut showing).await.is_err() {
        showing.abort();
        tracing::warn!(chat = %chat.0, "a turn's pictures did not all go before its reply");
    }

    let owner_said_stopped =
        shared.lock().unwrap().running.take().is_some_and(|running| running.stop_requested);
    memory.used(conversation);

    context.typing(chat, false).await;
    if context.stopping() {
        return;
    }
    if let Some(reply) = final_reply(&result, &told, owner_said_stopped) {
        context.tell(chat, &reply).await;
    }
}

/// What one event from the running turn means to the relay. Most mean
/// nothing: the owner gets the reply, not the working.
async fn on_event(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    conversation: ConversationId,
    told: &mut String,
    to_show: &mpsc::UnboundedSender<FileId>,
    event: AgentEvent,
) {
    match event {
        AgentEvent::Delta { text } => told.push_str(&text),
        // A picture the assistant made goes out the moment it is made, as the
        // panel draws it: ahead of the words about it, which come when the
        // turn ends -- and a picture a local model took minutes over is not
        // held back for the rest of the turn. Handed to `Context::showing`'s
        // task, so the turn's events are not held up by the upload.
        AgentEvent::ToolFinished { ok: true, files, .. } => {
            for file in files {
                let _ = to_show.send(file);
            }
        }
        // A step asked for again: what it had begun to say is said again.
        AgentEvent::Retrying { text } => *told = text,
        AgentEvent::Started { .. } => {
            // The turn exists now, and can be cancelled. A `!stop` that came
            // before that could not cancel it; this is where it takes effect.
            let stop_wanted = shared
                .lock()
                .unwrap()
                .running
                .as_ref()
                .is_some_and(|running| running.stop_requested);
            if stop_wanted {
                context.pending().cancel(conversation);
            }
        }
        AgentEvent::ConfirmationRequired { call_id, subject, kind, can_park, site, .. } => {
            let answer = ask(
                context,
                chat,
                shared,
                &kind,
                &subject,
                can_park,
                site.as_deref(),
                Asking::Turn,
            )
            .await;
            // False when the turn was cancelled in the meantime and nothing
            // is waiting for this any more -- not worth mentioning.
            context.pending().answer(&call_id, answer);
        }
        _ => {}
    }
}

/// Who is putting a question to the owner.
#[derive(Clone, Copy)]
enum Asking<'a> {
    /// A turn the owner started by typing.
    Turn,
    /// A routine's run, by name.
    Routine(&'a str),
    /// A call's voice session, which the owner is talking to.
    Call,
}

/// Put a confirmation to the owner and wait for what they say.
///
/// The turn is waiting too, inside its own confirmation gate, so this runs
/// to an answer whatever happens: the owner's, the dispatcher declining on
/// their behalf (`!stop`, a message that was not an answer, the relay
/// stopping), or the timeout.
///
/// `site`, when there is one, is the website "always" would let the assistant
/// open from now on; the answer to that is [`ConfirmAnswer::TrustSite`], and
/// the gate that asked does the saving. Silence is never that, nor yes.
///
/// `who` says who is asking. A routine's question says whose it is, and no
/// typing notice is shown around it, because nothing in this chat is typing;
/// nor around a call's, because the owner is on the phone.
#[allow(clippy::too_many_arguments)]
async fn ask(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    kind: &str,
    subject: &str,
    can_park: bool,
    site: Option<&str>,
    who: Asking<'_>,
) -> ConfirmAnswer {
    let mut question = confirmation_question(kind, subject, can_park, site);
    // Read off the question rather than off `site`, so what is offered and
    // what is accepted cannot drift apart.
    let can_trust = question.choices.contains(&Choice::Always);
    if let Asking::Routine(routine) = who {
        question.markdown = format!("{}{}", from_routine(routine), question.markdown);
    }
    let in_turn = matches!(who, Asking::Turn);

    // Registered before the question is sent, not after: sending it takes as
    // long as the server takes, and a typed "yes" in that time must already
    // count.
    let (answer, mut answered) = oneshot::channel();
    let closed = {
        let mut shared = shared.lock().unwrap();
        if !shared.closed {
            shared.waiting = Some(Waiting {
                question: None,
                early: Vec::new(),
                answer: Answer::Confirm { can_park, can_trust, tx: answer },
            });
        }
        shared.closed
    };
    if closed {
        return ConfirmAnswer::Decline;
    }

    // Nobody is typing while we wait for them.
    if in_turn {
        context.typing(chat, false).await;
    }
    match context.connector.ask(chat, &question).await {
        Ok(reference) => shared.lock().unwrap().question_sent(reference),
        Err(e) => {
            // The question never reached them, so no answer will come. The
            // safe reading of silence is no.
            tracing::warn!(chat = %chat.0, error = %e, "could not put a question to the chat");
            shared.lock().unwrap().waiting = None;
            if in_turn {
                context.typing(chat, true).await;
            }
            return ConfirmAnswer::Decline;
        }
    }

    let given = tokio::select! {
        answer = &mut answered => Some(answer.unwrap_or(ConfirmAnswer::Decline)),
        () = tokio::time::sleep(context.timing.confirm_timeout) => None,
    };
    let answer = match given {
        Some(answer) => answer,
        None => {
            let still_waiting = shared.lock().unwrap().waiting.take().is_some();
            if still_waiting {
                let answer = timeout_answer(can_park);
                context.tell(chat, timeout_notice(answer)).await;
                answer
            } else {
                // They answered in the instant the wait ran out; the answer
                // is in the channel, and is theirs to give.
                answered.try_recv().unwrap_or(ConfirmAnswer::Decline)
            }
        }
    };
    if in_turn {
        context.typing(chat, true).await;
    }
    answer
}

/// Put a routine's own question to the owner and wait for what they type.
///
/// The free half of [`ask`]: no reactions to tap, because the answer is
/// words, but the same waiting slot, so the dispatcher routes their next
/// message here rather than starting a turn with it -- and the same timeout,
/// after which the routine is told nothing came and carries on.
async fn ask_free(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    routine: &str,
    question: &str,
    choices: &[String],
) -> Option<String> {
    let (answer, mut answered) = oneshot::channel();
    let closed = {
        let mut shared = shared.lock().unwrap();
        if !shared.closed {
            shared.waiting = Some(Waiting {
                question: None,
                early: Vec::new(),
                answer: Answer::Free { tx: answer },
            });
        }
        shared.closed
    };
    if closed {
        return None;
    }

    match context.connector.send(chat, &free_question(routine, question, choices)).await {
        Ok(reference) => shared.lock().unwrap().question_sent(reference),
        Err(e) => {
            tracing::warn!(chat = %chat.0, error = %e, "could not put a question to the chat");
            shared.lock().unwrap().waiting = None;
            return None;
        }
    }

    let given = tokio::select! {
        answer = &mut answered => Some(answer.ok().flatten()),
        () = tokio::time::sleep(context.timing.confirm_timeout) => None,
    };
    let said = match given {
        Some(said) => said,
        None => {
            let still_waiting = shared.lock().unwrap().waiting.take().is_some();
            if still_waiting {
                context.tell(chat, NO_ANSWER).await;
                None
            } else {
                answered.try_recv().ok().flatten()
            }
        }
    };
    said.map(|said| pick_choice(&said, choices))
}

// ---- a call -------------------------------------------------------------------

/// Hang up `call`, saying why. A failure is logged and goes no further: the
/// call is over from this side whatever the server thinks of it.
async fn hang_up(context: &Context, chat: &ChatId, call: &CallId, why: HangUp) {
    if let Err(e) = context.connector.end_call(chat, call, why).await {
        tracing::warn!(chat = %chat.0, error = %e, ?why, "could not hang up a call");
    }
}

/// Refuse a call because another has the line, and say so.
async fn refuse_busy(context: &Context, chat: &ChatId, call: &CallId) {
    hang_up(context, chat, call, HangUp::Refused).await;
    context.tell(chat, calling::BUSY).await;
}

/// The owner is calling: answer, carry the call, hang up. The chat's worker
/// does this in turn, so no typed turn runs in this chat until it is over.
async fn take_call(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    work: CallWork,
) {
    let CallWork { offer, _line } = work;
    let call = offer.call.clone();
    let _ = carry_call(context, chat, shared, memory, offer, None).await;
    // The call is not findable any more, and then -- as `_line` drops -- the
    // line is free for the next. In that order, so a call that comes the
    // instant the line is free finds the slot it is about to replace empty
    // rather than clearing the new one.
    let mut shared = shared.lock().unwrap();
    if shared.call.as_ref().is_some_and(|slot| slot.id == call) {
        shared.call = None;
    }
}

/// [`take_call`], without the tidying-up after it, so that every way out of
/// it can simply return.
///
/// The steps, each of which can end the call and says how:
///
/// 1. Taken up. A call the owner ended while it waited is not answered; it is
///    hung up as refused all the same, which is harmless if they hung up
///    themselves and is what makes `!stop` stop the phone ringing. A call
///    that stopped ringing while it waited is refused, and the owner told.
/// 2. The conversation, chosen as a message would, so typing after the call
///    carries on the same thread.
/// 3. The voice session. If it will not open the owner is told why, in the
///    service's own words, and the call is refused.
/// 4. The call joined on the chat service -- this side said to be in it,
///    and its key sent to the caller -- and then its room on the media
///    server. A failure on either tells the owner, hangs up as failed, and
///    stops the voice session.
/// 5. The call, run; and while it runs, any confirmation only a tap can
///    answer is put to the owner in the chat.
/// 6. Hung up, with the reason that fits how it ended.
async fn carry_call(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    offer: CallOffer,
    opening: Option<Opening>,
) -> Option<Opening> {
    let CallOffer { call, expires } = offer;

    let started = {
        // The line is told under the chat's lock, as the dispatcher frees it
        // under that lock, so a hang-up cannot free the line of a call that
        // is just now being taken up.
        let mut shared = shared.lock().unwrap();
        let started = shared.call_start(&call);
        if started.is_some() {
            context.line.lock().unwrap().started(&call);
        }
        started
    };
    let Some((heard, to_call)) = started else {
        hang_up(context, chat, &call, HangUp::Refused).await;
        return opening;
    };
    if calling::has_expired(expires, Timestamp::now()) {
        hang_up(context, chat, &call, HangUp::Refused).await;
        context.tell(chat, calling::MISSED).await;
        return opening;
    }

    // A call this side placed for a run is in that run's conversation, as a
    // reply to its message would be; so is what is typed after it.
    let conversation = match opening.as_ref().and_then(|o| o.conversation) {
        Some(conversation) => {
            memory.adopt(conversation);
            context.adopt_conversation(chat, conversation).await;
            conversation
        }
        None => memory.conversation(context, chat).await,
    };
    let via = Via {
        link: context.link.id,
        service: context.link.service.label().to_string(),
        chat: chat.0.clone(),
    };

    let voice = match context.calls.open_voice(context.service.clone(), conversation, via).await {
        Ok(voice) => voice,
        Err(e) => {
            context.tell(chat, &calling::cannot_take_calls(&e.message)).await;
            hang_up(context, chat, &call, HangUp::Refused).await;
            return opening;
        }
    };
    // Opening the voice can take seconds: the owner may have given up in
    // them, or the call rung out. Joining now would show them a join and a
    // leave for nothing.
    if let Some(over) = given_up(shared, &call, expires) {
        let _ = voice.input.send(VoiceInput::Stop);
        hang_up(context, chat, &call, HangUp::Refused).await;
        if !over {
            context.tell(chat, calling::MISSED).await;
        }
        return opening;
    }
    let join = match context.connector.join_call(chat, &call).await {
        Ok(join) => join,
        Err(e) => {
            let _ = voice.input.send(VoiceInput::Stop);
            // The owner hung up in the moments it took to get here, and the
            // chat service has forgotten the call: that is why this failed,
            // and it is not a failure to tell them about.
            if shared.lock().unwrap().call_is_over(&call) {
                hang_up(context, chat, &call, HangUp::Refused).await;
                return opening;
            }
            context.tell(chat, &calling::could_not_join(&e.to_string())).await;
            hang_up(context, chat, &call, HangUp::Failed).await;
            return opening;
        }
    };
    let media = match context.calls.connect(join, heard).await {
        Ok(media) => media,
        Err(e) => {
            context.tell(chat, &calling::could_not_answer(&e.to_string())).await;
            hang_up(context, chat, &call, HangUp::Failed).await;
            let _ = voice.input.send(VoiceInput::Stop);
            return opening;
        }
    };

    // Setting up takes seconds, and the owner may have given up in them, or
    // the call have stopped ringing: carrying on now would be talking to a
    // phone nobody is holding.
    if let Some(over) = given_up(shared, &call, expires) {
        let _ = voice.input.send(VoiceInput::Stop);
        hang_up(context, chat, &call, HangUp::Refused).await;
        if !over {
            context.tell(chat, calling::MISSED).await;
        }
        return opening;
    }

    let mut first_written = None;
    if let Some(opening) = opening {
        // Said first, before the owner says anything: why the call. From
        // here the run's result is the call's to give.
        let _ = voice.input.send(VoiceInput::Say(opening.spoken));
        let _ = opening.delivered.send(Ok(CallDelivered::PickedUp));
        first_written = Some(opening.written);
    }
    let (end, session_error, writing) =
        run_call(context, chat, shared, media, voice, to_call, first_written).await;
    memory.used(conversation);

    // A session that ended in an error was a failed call, not a goodbye.
    let end = calling::settle_end(end, session_error.as_deref());
    // Hung up first: the call is over whether or not the chat has caught up.
    hang_up(context, chat, &call, calling::hang_up_for(end)).await;
    finish_writing(chat, writing).await;
    if !context.stopping() {
        let note = match (&session_error, end) {
            (Some(error), CallEnd::Failed) => Some(calling::session_failed(error)),
            _ => calling::after_call(end).map(str::to_string),
        };
        if let Some(note) = note {
            context.tell(chat, &note).await;
        }
    }
    None
}

/// Whether `call` is no longer to be answered: `Some(true)` when the owner
/// ended it, `Some(false)` when it rang out, `None` while it can still be.
fn given_up(shared: &Mutex<Shared>, call: &CallId, expires: Timestamp) -> Option<bool> {
    if shared.lock().unwrap().call_is_over(call) {
        Some(true)
    } else if calling::has_expired(expires, Timestamp::now()) {
        Some(false)
    } else {
        None
    }
}

/// What a call this side placed opens with.
struct Opening {
    /// The run it was placed for, whose conversation it carries on.
    conversation: Option<ConversationId>,
    /// What is said when the owner picks up.
    spoken: String,
    /// The same, as written: what the chat shows of it, as it shows
    /// everything said on a call.
    written: String,
    /// Told that the run's result was given, once the call is under way.
    delivered: oneshot::Sender<Result<CallDelivered, String>>,
}

/// How long the owner's devices ring before the call is given up and the
/// result written instead. A little past the ring's own lifetime on their
/// devices, so a pick-up at the last moment is still heard.
pub const RING_FOR: Duration = Duration::from_secs(50);

/// A run's message to the owner, and the chat carried on in the run's
/// conversation from then, as a reply to it would be. A message from a
/// background job is remembered as that job's, so a reply to it is about it.
async fn post_for_run(
    context: &Context,
    chat: &ChatId,
    memory: &mut Memory,
    from: &FromRun,
    markdown: &str,
) -> Result<(), String> {
    let sent = context.connector.send(chat, markdown).await.map_err(|e| e.to_string())?;
    if let Some(job) = from.job {
        context.posts.record(sent, job);
    }
    if let Some(conversation) = from.conversation {
        memory.adopt(conversation);
        context.adopt_conversation(chat, conversation).await;
    }
    Ok(())
}

/// A result that was to be said, written instead, led by why.
fn instead(lead: &str, markdown: &str) -> String {
    format!("_{lead}_\n\n{markdown}")
}

/// How a ring ended.
enum Rang {
    PickedUp(CallOffer),
    /// Nobody picked up in time.
    Unanswered,
    /// `!stop`, or the relay stopping.
    Stopped,
}

/// Call the owner with a run's result, in turn with the chat's own work.
///
/// 1. The run must still be waiting for it: a job that sat queued past the
///    run's patience is written rather than rung through, late.
/// 2. The line, held while it rings: a call already running means the
///    result is written instead.
/// 3. Ring. Their picking up arrives as an ordinary call, which the
///    dispatcher hands here (`Shared::ringing`) rather than queueing.
///    `!stop` stops the ringing.
/// 4. Picked up: the call is carried as any other, opening with `spoken`, in
///    the run's conversation; the run hears it was delivered once the call is
///    under way. A call that never gets that far, a ring nobody picks up in
///    [`Timing::ring_for`], and a stopped ring all end the ringing and write
///    the result, saying so.
#[allow(clippy::too_many_arguments)]
async fn call_owner(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    from: FromRun,
    markdown: String,
    spoken: String,
    delivered: oneshot::Sender<Result<CallDelivered, String>>,
) {
    let write = async |memory: &mut Memory, lead: &str, why: &str| {
        post_for_run(context, chat, memory, &from, &instead(lead, &markdown))
            .await
            .map(|()| CallDelivered::Messaged { why: why.to_string() })
    };

    if delivered.is_closed() {
        let lead = "This was to be a call, but I couldn't get to it in time.";
        let _ = write(memory, lead, "it waited too long to call").await;
        return;
    }

    let now = Timestamp::now();
    let ringing_id = CallId(format!("ringing:{}", chat.0));
    let ring_for = context.timing.ring_for;
    let until = now + SignedDuration::try_from(ring_for).unwrap_or(SignedDuration::from_secs(50));
    let offered = context.line.lock().unwrap().offer(&ringing_id, until, now);
    if offered != Offered::Taken {
        let _ = delivered.send(write(memory, BUSY_INSTEAD, BUSY_WHY).await);
        return;
    }
    let ringing_line = LineHeld { context, call: ringing_id };

    let (answer, mut answered) = oneshot::channel();
    shared.lock().unwrap().ringing = Some(answer);
    let ring = match context.connector.ring(chat).await {
        Ok(ring) => ring,
        Err(e) => {
            shared.lock().unwrap().ringing = None;
            drop(ringing_line);
            tracing::warn!(chat = %chat.0, error = %e, "could not call the owner");
            let why = format!("the call could not be placed ({e})");
            let lead = "I tried to call you with this, but the call couldn't be placed.";
            let _ = delivered.send(write(memory, lead, &why).await);
            return;
        }
    };

    let rang = match tokio::time::timeout(ring_for, &mut answered).await {
        Ok(Ok(offer)) => Rang::PickedUp(offer),
        Ok(Err(_)) => Rang::Stopped,
        Err(_) => {
            // The dispatcher hands a pick-up over under the chat's lock: once
            // the ring is taken back under it, either nothing came or it is
            // already here.
            let taken_back = shared.lock().unwrap().ringing.take().is_some();
            match answered.try_recv() {
                Ok(offer) if !taken_back => Rang::PickedUp(offer),
                _ => Rang::Unanswered,
            }
        }
    };
    shared.lock().unwrap().ringing = None;
    drop(ringing_line);
    let offer = match rang {
        Rang::PickedUp(offer) => offer,
        Rang::Unanswered => {
            hang_up(context, chat, &ring, HangUp::Unreachable).await;
            let lead = "I called you with this, but you didn't pick up.";
            let _ = delivered.send(write(memory, lead, "they did not pick up").await);
            return;
        }
        Rang::Stopped => {
            hang_up(context, chat, &ring, HangUp::Finished).await;
            let lead = "I stopped calling. Here it is instead.";
            let _ = delivered.send(write(memory, lead, "the call was stopped").await);
            return;
        }
    };

    // The line, now for the call they picked up.
    let call = offer.call.clone();
    let taken = context.line.lock().unwrap().offer(&call, offer.expires, Timestamp::now());
    if taken != Offered::Taken {
        // Another call took the line in the instant between: this one cannot
        // be carried, and the membership that rang them is still standing.
        shared.lock().unwrap().call = None;
        refuse_busy(context, chat, &call).await;
        hang_up(context, chat, &ring, HangUp::Refused).await;
        let _ = delivered.send(write(memory, BUSY_INSTEAD, BUSY_WHY).await);
        return;
    }
    let call_line = LineHeld { context, call: call.clone() };
    let opening =
        Opening { conversation: from.conversation, spoken, written: markdown.clone(), delivered };
    let unsaid = carry_call(context, chat, shared, memory, offer, Some(opening)).await;
    {
        let mut shared = shared.lock().unwrap();
        if shared.call.as_ref().is_some_and(|slot| slot.id == call) {
            shared.call = None;
        }
    }
    drop(call_line);
    // Joining the call they picked up takes over the ring's membership and
    // forgets the ring. If the call never got that far, that membership is
    // still in the room, and ending the ring is what takes it down. Ending
    // a ring that joining already forgot does nothing.
    hang_up(context, chat, &ring, HangUp::Finished).await;
    if let Some(Opening { delivered, .. }) = unsaid {
        let lead = "You picked up, but the call couldn't go ahead, so here it is.";
        let _ =
            delivered.send(write(memory, lead, "the call they picked up did not go ahead").await);
    }
}

/// Run an answered call to its end, reading what the voice session reports as
/// it goes so that a confirmation only a tap can answer is asked in the chat.
///
/// The question is the one a typed turn would put -- [`ask`], the same words,
/// reactions and timeout -- and what is waiting is registered in `shared` as
/// it is for a turn, so the dispatcher routes the owner's tap or typed yes or
/// no to it. The answer goes to the same place a turn's does. If the voice
/// answers it first, or the turn moves on, the question is withdrawn the way
/// the relay withdraws one when the owner does something else.
/// A question asked in the chat on a call, until the owner answers it.
type CallQuestion<'a> = Pin<Box<dyn Future<Output = ConfirmAnswer> + Send + 'a>>;

async fn run_call(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    media: Box<dyn calling::Media>,
    voice: VoiceLink,
    to_call: mpsc::UnboundedReceiver<ToCall>,
    first_written: Option<String>,
) -> (CallEnd, Option<String>, Writing) {
    let (events_tx, mut events) = mpsc::unbounded_channel::<VoiceEvent>();
    let mut run = media.run(voice, to_call, events_tx);
    let mut confirming = Confirming::default();
    // What is said, written into the chat as it goes -- by a task of its own,
    // in order, since the call's audio is paced by this loop and must never
    // wait on the chat server.
    let mut transcript = Transcript::default();
    let (writes, writing) = mpsc::unbounded_channel::<Written>();
    let writer = tokio::spawn(write_down(
        context.vault.clone(),
        context.connector.clone(),
        chat.clone(),
        writing,
    ));
    if let Some(first) = first_written {
        let _ = writes.send(Written::Line(first));
    }
    // The question in the chat, and the confirmation it is about.
    let mut asking: Option<(String, CallQuestion<'_>)> = None;
    // Why the voice session ended, if it ended in an error.
    let mut session_error: Option<String> = None;

    let end = loop {
        tokio::select! {
            biased;
            Some(event) = events.recv() => {
                note_session_error(&event, &mut session_error);
                for written in written_of(&mut transcript, &event) {
                    let _ = writes.send(written);
                }
                let step = confirming.on_event(&event);
                if let Some(withdrawn) = step.withdraw {
                    withdraw(context, shared, asking.take(), withdrawn.settled);
                }
                if let Some(Confirmation { call_id, subject, kind, can_park, site }) = step.ask {
                    let id = call_id.clone();
                    asking = Some((id, Box::pin(async move {
                        ask(
                            context,
                            chat,
                            shared,
                            &kind,
                            &subject,
                            can_park,
                            site.as_deref(),
                            Asking::Call,
                        )
                        .await
                    })));
                }
            }
            (call_id, answer) = async {
                match asking.as_mut() {
                    Some((call_id, asking)) => (call_id.clone(), asking.await),
                    None => std::future::pending().await,
                }
            }, if asking.is_some() => {
                asking = None;
                // False when the turn moved on in the meantime and nothing is
                // waiting for this any more -- not worth mentioning.
                context.pending().answer(&call_id, answer);
            }
            end = &mut run => break end,
        }
    };

    // The media may have said its last word as it returned: the session's
    // ending is in the channel still.
    while let Ok(event) = events.try_recv() {
        note_session_error(&event, &mut session_error);
        for written in written_of(&mut transcript, &event) {
            let _ = writes.send(written);
        }
    }
    for line in transcript.finish() {
        let _ = writes.send(Written::Line(line));
    }
    // The call is over, and so is any question about it.
    withdraw(context, shared, asking.take(), false);
    // Nothing more is coming; the writer finishes with what it has.
    drop(writes);
    (end, session_error, writer)
}

/// What is still being written into the chat of a call that has ended.
type Writing = tokio::task::JoinHandle<()>;

/// Pictures still on their way to a chat -- see [`Context::showing`].
type Showing = tokio::task::JoinHandle<()>;

/// How long a turn's reply waits for its pictures to have gone first: an
/// upload of a few megabytes, or a few of them, to a server that answers.
const SHOW_FOR: Duration = Duration::from_secs(60);

/// Send a picture kept in Files to `chat`, as a picture: `Ok(false)` for one
/// that is no longer a picture in Files, `Err` for one that could not be read
/// or sent, logged here. The picture is in Files whether or not it arrives.
async fn show_picture(
    vault: &Arc<Vault>,
    connector: &Arc<dyn Connector>,
    chat: &ChatId,
    file: FileId,
) -> Result<bool, String> {
    let vault = vault.clone();
    let read = karmayoga_service::blocking(move || {
        let entry = vault.file(file)?;
        if entry.kind != karmayoga_core::files::FileKind::Image
            || entry.status.is_trashed()
            || entry.size > crate::connector::MAX_FILE_BYTES
        {
            return Ok(None);
        }
        let bytes = vault.read_file_range(file, 0, entry.size)?;
        Ok(Some(Shown {
            filename: entry.name,
            mime: entry.mime,
            bytes,
            width: entry.media.width,
            height: entry.media.height,
        }))
    })
    .await;
    let picture = match read {
        Ok(Some(picture)) => picture,
        Ok(None) => return Ok(false),
        Err(e) => {
            tracing::warn!(%file, error = %e.message, "could not read a picture to send");
            return Err(format!("a picture could not be read: {}", e.message));
        }
    };
    match connector.show(chat, &picture).await {
        Ok(_) => Ok(true),
        Err(e) => {
            tracing::warn!(chat = %chat.0, error = %e, "could not send a picture to the chat");
            Err(e.to_string())
        }
    }
}

/// How long the end of a call waits for what was said on it to be written
/// into the chat.
const WRITE_DOWN_FOR: Duration = Duration::from_secs(15);

/// Let what is being written of a call finish -- for as long as a chat
/// server that answers at all takes -- and stop it there, so that anything
/// said about how the call ended comes after it, never in the middle.
async fn finish_writing(chat: &ChatId, mut writing: Writing) {
    if tokio::time::timeout(WRITE_DOWN_FOR, &mut writing).await.is_err() {
        writing.abort();
        tracing::warn!(chat = %chat.0, "the end of a call was not all written into the chat");
    }
}

/// One thing a call puts into its chat: a line of what was said, or a
/// picture the assistant made while it was said.
enum Written {
    Line(String),
    Picture(FileId),
}

/// What `event` puts into the chat: the transcript's lines, then any picture
/// a tool that finished just made.
fn written_of(transcript: &mut Transcript, event: &VoiceEvent) -> Vec<Written> {
    let mut out: Vec<Written> = transcript.on_event(event).into_iter().map(Written::Line).collect();
    if let VoiceEvent::Agent { event: AgentEvent::ToolFinished { ok: true, files, .. } } = event {
        out.extend(files.iter().copied().map(Written::Picture));
    }
    out
}

/// Send each of `written` to `chat`, in order, until there are no more. A
/// failure is logged and the rest still sent: one message the chat server
/// refused is no reason to lose the others.
async fn write_down(
    vault: Arc<Vault>,
    connector: Arc<dyn Connector>,
    chat: ChatId,
    mut written: mpsc::UnboundedReceiver<Written>,
) {
    while let Some(next) = written.recv().await {
        match next {
            Written::Line(markdown) => {
                if let Err(e) = connector.send(&chat, &markdown).await {
                    tracing::warn!(chat = %chat.0, error = %e, "could not write a call into the chat");
                }
            }
            Written::Picture(file) => {
                let _ = show_picture(&vault, &connector, &chat, file).await;
            }
        }
    }
}

/// Remember why the voice session ended, if it ended in an error.
fn note_session_error(event: &VoiceEvent, error: &mut Option<String>) {
    if let VoiceEvent::Ended { error: Some(why) } = event {
        *error = Some(why.clone());
    }
}

/// Stop asking `asked` -- the question in the chat, and the confirmation it
/// was about, if there was one still being asked.
///
/// The question is withdrawn as the relay withdraws any: what is waiting is
/// declined. And unless the voice session has settled the confirmation itself
/// (`settled`: its tool started or finished), the turn may be waiting on
/// it still, and nothing else will answer it now -- the question that would
/// have carried the answer is gone -- so it is declined, here, by name. No
/// path out of a question leaves a confirmation in [`Pending`] unanswered.
fn withdraw<F>(
    context: &Context,
    shared: &Mutex<Shared>,
    asked: Option<(String, F)>,
    settled: bool,
) {
    let Some((call_id, question)) = asked else { return };
    // The question first, so that nothing is left waiting on an answer that
    // is no longer being read.
    drop(question);
    shared.lock().unwrap().decline();
    if !settled {
        context.pending().answer(&call_id, ConfirmAnswer::Decline);
    }
}

// ---- what is said -----------------------------------------------------------

/// The one message a finished turn ends with, if it should send one.
///
/// What the assistant said; failing that, everything it streamed; failing
/// that, "Stopped." for a turn that was stopped, and "(no reply)" for one that
/// simply said nothing. A turn that failed says so, in the model's or the
/// harness's own words -- including a failure before it started, such as the
/// assistant being switched off.
///
/// `owner_said_stopped` is the one case that sends nothing: the owner typed
/// `!stop`, was told "Stopped." the moment they did, and a second one would be
/// noise. If the turn had something to say before it was stopped, that is still
/// sent.
pub(crate) fn final_reply(
    result: &CommandResult<Turned>,
    told: &str,
    owner_said_stopped: bool,
) -> Option<String> {
    let turned = match result {
        Ok(turned) => turned,
        Err(e) => return Some(format!("Something went wrong: {}", e.message)),
    };
    let text = turned.text.trim();
    if !text.is_empty() {
        return Some(text.to_string());
    }
    let told = told.trim();
    if !told.is_empty() {
        return Some(told.to_string());
    }
    if turned.stopped {
        return (!owner_said_stopped).then(|| STOPPED.to_string());
    }
    Some(NO_REPLY.to_string())
}

/// What the bot says about itself: what it is, what to type.
pub(crate) fn help_text(assistant_name: &str) -> String {
    let name = assistant_name.trim();
    let who = if name.is_empty() {
        "your assistant".to_string()
    } else {
        format!("{}, your assistant", escape_markdown(name))
    };
    format!(
        "I'm {who} from Karma Yoga \u{2014} the same one that lives on your computer, answering \
         from this chat. Ask for what you would ask for there: what's on today, a reply to \
         someone, a task added.\n\n\
         - `!new` starts a new conversation\n\
         - `!stop` stops what I'm doing\n\
         - `!help` shows this message\n\n\
         Before I delete anything, or send something to someone else, I'll ask you here first. \
         If I ask before opening a web page, answer \"always\" and I'll open pages on that \
         website without asking from then on. \
         Routines you have sent here post what they did, and may ask you something while they \
         work \u{2014} answer them here too."
    )
}

/// The question put to the owner for a call that needs their say.
///
/// `subject` is what the assistant's tools name the thing as -- the record's
/// own title for a delete, "to alice@example.com \u{2014} Re: dinner \u{2014} \u{2026}" for a send,
/// the query for a search, the address for a fetch -- so each kind has its own
/// words around it, the same distinction the desktop's card draws. The choices
/// are Yes and No, Later where the call can be saved as a proposal, and Always
/// where `site` names the website that would be allowed from now on.
pub(crate) fn confirmation_question(
    kind: &str,
    subject: &str,
    can_park: bool,
    site: Option<&str>,
) -> Question {
    let subject = subject.trim();
    let site = site.map(one_line).filter(|site| !site.is_empty());
    let mut markdown = match kind {
        "destructive" if subject.is_empty() => {
            "**I want to delete something, but I can't say what.** That can't be undone."
                .to_string()
        }
        "destructive" => {
            format!("**Delete {}?** That can't be undone.", escape_markdown(&one_line(subject)))
        }
        "outward" => with_quote("**This reaches someone outside your vault.**", subject),
        "search" => with_quote(
            "**Search the web for this?** I've read your mail in this conversation, so I'm \
             checking before words from it leave your machine.",
            subject,
        ),
        "fetch" => {
            // As code, so the address is shown rather than turned into a link
            // that one tap would follow.
            let head = "**Open this address?** I put it together myself rather than finding it, \
                        and whatever is in it reaches that site.";
            if subject.is_empty() {
                head.to_string()
            } else {
                format!("{head}\n\n`{}`", one_line(subject).replace('`', "'"))
            }
        }
        // Typing into a page in the assistant's browser: the text, the field
        // and the site, which the site can read the moment it is typed.
        "type" => with_quote(
            "**Type this into a web page?** The site can read it as soon as it is typed.",
            subject,
        ),
        "click" => with_quote(
            "**Click this in the browser?** I've read your mail in this conversation, so I'm \
             checking that the mail isn't choosing where I go.",
            subject,
        ),
        // A routine whose instructions name an address nobody gave: as code,
        // as `fetch` shows one, so it is read rather than tapped.
        "routine" => {
            let head = "**Save a routine that opens this?** Neither you nor a search gave me \
                        this address, and the routine's runs will open it without asking.";
            if subject.is_empty() {
                head.to_string()
            } else {
                format!("{head}\n\n`{}`", one_line(subject).replace('`', "'"))
            }
        }
        // A money rule, or a change to many transactions, after somebody
        // else's words -- a transfer's memo, a mail -- were read.
        "rewrite" => with_quote(
            "**Change these transactions?** I've read words from someone else in this \
             conversation, so I'm checking before I rewrite many of them at once.",
            subject,
        ),
        // Something to be kept for good, read back as the owner's own wishes
        // -- a memory, a skill, a routine, a task for the assistant -- after
        // somebody else's words were read. The subject's first line says
        // what, and the lines after it are the text that would be saved, in
        // full: that text is what is being agreed to.
        "keep" => with_quote(
            "**Keep this?** I've read words from someone else in this conversation, so I'm \
             checking before I save something that I would read back later as what you want.",
            subject,
        ),
        // A tool of a connected service, which acts there as the owner: the
        // subject's first line names the tool and the connection, and the
        // lines after it are the arguments.
        "connection" => {
            with_quote("**Use this in a connected service?** It acts there as you.", subject)
        }
        _ => with_quote("**Go ahead with this?**", subject),
    };
    markdown.push_str("\n\n");

    let mut choices = vec![Choice::Yes, Choice::No];
    if can_park {
        choices.push(Choice::Later);
    }
    if site.is_some() {
        choices.push(Choice::Always);
    }
    let later = "later saves it for you to decide in the app";
    // As code, like the address above, so a host is read and not made a link.
    let always = site.map(|site| {
        format!(
            "always lets me open pages on `{}` from now on without asking",
            site.replace('`', "'")
        )
    });
    markdown.push_str(&match (can_park, always) {
        (false, None) => "Reply **yes** or **no**.".to_string(),
        (true, None) => format!("Reply **yes**, **no** or **later** ({later})."),
        (false, Some(always)) => format!("Reply **yes**, **no** or **always** ({always})."),
        (true, Some(always)) => {
            format!("Reply **yes**, **no**, **later** or **always** ({later}; {always}).")
        }
    });
    Question { markdown, choices }
}

/// The line a routine's message to the owner starts with, so that a question
/// arriving out of nowhere says whose it is.
fn from_routine(routine: &str) -> String {
    format!("_From your routine **{}**:_\n\n", escape_markdown(&one_line(routine)))
}

/// A routine's own question, as the owner sees it: whose it is, the question
/// in the model's words, and the choices numbered if there are any.
///
/// Said to be the assistant's own words, and set apart as a quotation: a run
/// that has read somebody's mail or a page can be talked into putting a
/// paragraph of it in a question, and a message that arrives in the owner's
/// own chat from a routine they set up reads as the routine's -- so the
/// owner is told that the assistant wrote it, itself, while the routine ran.
/// Escaped as every other name and quotation shown in a chat is, so a link in
/// it is shown as written and not made one to tap.
pub(crate) fn free_question(routine: &str, question: &str, choices: &[String]) -> String {
    let mut out = format!(
        "_Your routine **{}** is running, and the assistant wrote this question itself:_\n\n",
        escape_markdown(&one_line(routine))
    );
    let quoted: Vec<String> = question
        .trim()
        .lines()
        .map(|line| format!("> {}", escape_markdown(line.trim_end())))
        .collect();
    out.push_str(&quoted.join("\n"));
    if choices.is_empty() {
        out.push_str("\n\nReply here.");
    } else {
        out.push_str("\n\n");
        for (n, choice) in choices.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", n + 1, escape_markdown(&one_line(choice))));
        }
        out.push_str("\nReply with a number, or in your own words.");
    }
    out
}

/// What the owner said, with a bare number read as the choice it numbers.
pub(crate) fn pick_choice(said: &str, choices: &[String]) -> String {
    let trimmed = said.trim().trim_end_matches('.');
    match trimmed.parse::<usize>() {
        Ok(n) if (1..=choices.len()).contains(&n) => choices[n - 1].clone(),
        _ => said.trim().to_string(),
    }
}

/// `head`, then `subject` as a quotation, if there is one.
fn with_quote(head: &str, subject: &str) -> String {
    if subject.is_empty() {
        return head.to_string();
    }
    let quoted: Vec<String> =
        subject.lines().map(|line| format!("> {}", escape_markdown(line.trim_end()))).collect();
    format!("{head}\n\n{}", quoted.join("\n"))
}

/// `text` on one line, whitespace runs folded to a space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What an answer means to the turn that asked.
///
/// "Later" without a proposal form to park into is not on offer, so it is
/// read as the one safe thing: no. "Always" on a question that named no site
/// is not on offer either, and is read the same way: a delete, a send or a
/// keep was never put to them as something to say yes to by that word, and a
/// reaction or a phrase that meant nothing before this answer existed must not
/// start doing the thing.
pub(crate) fn answer_for(choice: Choice, can_park: bool, can_trust: bool) -> ConfirmAnswer {
    match choice {
        Choice::Yes => ConfirmAnswer::Confirm,
        Choice::No => ConfirmAnswer::Decline,
        Choice::Later if can_park => ConfirmAnswer::Later,
        Choice::Later => ConfirmAnswer::Decline,
        Choice::Always if can_trust => ConfirmAnswer::TrustSite,
        Choice::Always => ConfirmAnswer::Decline,
    }
}

/// What silence means: save it for later if it can be saved, otherwise no.
/// Never yes.
pub(crate) fn timeout_answer(can_park: bool) -> ConfirmAnswer {
    if can_park { ConfirmAnswer::Later } else { ConfirmAnswer::Decline }
}

/// What the owner is told when a question timed out, so that a missing answer
/// does not look like nothing having happened.
pub(crate) fn timeout_notice(answer: ConfirmAnswer) -> &'static str {
    match answer {
        ConfirmAnswer::Later => {
            "No answer, so I've saved that for later. You can decide in the Assistant app."
        }
        _ => "No answer, so I haven't done it. Ask me again if you still want it.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_service::error::CommandError;

    const NOW_SECONDS: i64 = 2_000_000_000;

    fn now() -> Timestamp {
        Timestamp::from_second(NOW_SECONDS).unwrap()
    }

    fn ago(hours: i64) -> Timestamp {
        now() - SignedDuration::from_hours(hours)
    }

    fn chat() -> ChatId {
        ChatId("!room:test".into())
    }

    fn said(text: &str) -> Incoming {
        Incoming::Said { chat: chat(), text: text.into(), at: now(), reply_to: None }
    }

    fn said_at(text: &str, at: Timestamp) -> Incoming {
        Incoming::Said { chat: chat(), text: text.into(), at, reply_to: None }
    }

    fn chose(question: &str, choice: Choice) -> Incoming {
        Incoming::Chose { chat: chat(), question: MessageRef(question.into()), choice, at: now() }
    }

    fn routed(item: &Incoming, asked: Asked<'_>) -> Route {
        route(item.clone(), asked, now(), STALE_AFTER)
    }

    // ---- pictures ----

    fn picture(name: &str) -> Picture {
        Picture { filename: name.into(), bytes: name.as_bytes().to_vec() }
    }

    fn showed_at(caption: &str, at: Timestamp) -> Incoming {
        Incoming::Showed {
            chat: chat(),
            picture: picture("plant.jpg"),
            caption: caption.into(),
            at,
        }
    }

    #[test]
    fn a_picture_is_queued_with_its_caption_trimmed() {
        assert_eq!(
            routed(&showed_at("  what plant is this?  ", now()), Asked::Nothing),
            Route::Queue(Work::Show {
                picture: picture("plant.jpg"),
                caption: "what plant is this?".into()
            })
        );
    }

    #[test]
    fn a_picture_never_answers_a_waiting_question() {
        let question = MessageRef("$q".into());
        for asked in [Asked::Unsent, Asked::Sent(&question), Asked::Open] {
            assert!(
                matches!(
                    routed(&showed_at("yes", now()), asked),
                    Route::DeclineThen(Work::Show { .. })
                ),
                "{asked:?}"
            );
        }
    }

    #[test]
    fn a_picture_from_last_night_is_not_looked_at() {
        assert!(matches!(
            routed(&showed_at("what is it?", ago(13)), Asked::Nothing),
            Route::Queue(Work::Reply(_))
        ));
    }

    fn owner(work: Work) -> Job {
        Job::Owner(work)
    }

    fn show(name: &str, caption: &str) -> Job {
        owner(Work::Show { picture: picture(name), caption: caption.into() })
    }

    fn names(sent: &[Sent]) -> Vec<&str> {
        sent.iter().map(Sent::filename).collect()
    }

    #[test]
    fn a_picture_with_nothing_said_waits_for_what_is_said_next() {
        let mut held = Vec::new();
        assert!(matches!(gather(&mut held, show("one.jpg", "")), Gathered::Hold));
        assert!(matches!(gather(&mut held, show("two.jpg", "")), Gathered::Hold));
        match gather(&mut held, owner(Work::say("which is ripe?"))) {
            Gathered::Run(Job::Owner(Work::Say { text, .. }), pictures) => {
                assert_eq!(text, "which is ripe?");
                assert_eq!(names(&pictures), ["one.jpg", "two.jpg"]);
            }
            _ => panic!("the words should take the pictures with them"),
        }
        assert!(held.is_empty());
    }

    #[test]
    fn a_captioned_picture_goes_at_once_with_any_held_before_it() {
        let mut held = vec![Sent::Picture(picture("one.jpg"))];
        match gather(&mut held, show("two.jpg", "compare these")) {
            Gathered::Run(Job::Owner(Work::Say { text, .. }), pictures) => {
                assert_eq!(text, "compare these");
                assert_eq!(names(&pictures), ["one.jpg", "two.jpg"], "in the order sent");
            }
            _ => panic!("a caption is something said"),
        }
        assert!(held.is_empty());
    }

    #[test]
    fn a_command_while_pictures_wait_sends_them_first() {
        let mut held = vec![Sent::Picture(picture("one.jpg"))];
        match gather(&mut held, owner(Work::Help)) {
            Gathered::First(pictures, Job::Owner(Work::Help)) => {
                assert_eq!(names(&pictures), ["one.jpg"]);
            }
            _ => panic!("the pictures go first, on their own"),
        }
        assert!(held.is_empty());
        assert!(matches!(gather(&mut held, owner(Work::New)), Gathered::Run(_, p) if p.is_empty()));
    }

    // ---- files ----

    fn document(name: &str) -> Document {
        Document { filename: name.into(), bytes: name.as_bytes().to_vec() }
    }

    fn handed_at(caption: &str, at: Timestamp) -> Incoming {
        Incoming::Handed {
            chat: chat(),
            document: document("minutes.pdf"),
            caption: caption.into(),
            at,
        }
    }

    fn hand(name: &str, caption: &str) -> Job {
        owner(Work::Hand { document: document(name), caption: caption.into() })
    }

    #[test]
    fn a_file_is_queued_with_its_caption_trimmed() {
        assert_eq!(
            routed(&handed_at("  summarise this  ", now()), Asked::Nothing),
            Route::Queue(Work::Hand {
                document: document("minutes.pdf"),
                caption: "summarise this".into()
            })
        );
    }

    #[test]
    fn a_file_never_answers_a_waiting_question() {
        let question = MessageRef("$q".into());
        for asked in [Asked::Unsent, Asked::Sent(&question), Asked::Open] {
            assert!(
                matches!(
                    routed(&handed_at("yes", now()), asked),
                    Route::DeclineThen(Work::Hand { .. })
                ),
                "{asked:?}"
            );
        }
    }

    #[test]
    fn a_file_from_last_night_is_not_taken() {
        assert!(matches!(
            routed(&handed_at("what is it?", ago(13)), Asked::Nothing),
            Route::Queue(Work::Reply(_))
        ));
    }

    #[test]
    fn pictures_and_files_wait_together_and_go_in_the_order_they_were_sent() {
        let mut held = Vec::new();
        assert!(matches!(gather(&mut held, show("one.jpg", "")), Gathered::Hold));
        assert!(matches!(gather(&mut held, hand("two.pdf", "")), Gathered::Hold));
        match gather(&mut held, hand("three.mov", "compare these")) {
            Gathered::Run(Job::Owner(Work::Say { text, .. }), sent) => {
                assert_eq!(text, "compare these");
                assert_eq!(names(&sent), ["one.jpg", "two.pdf", "three.mov"]);
                assert!(matches!(sent[1], Sent::Document(_)), "a file stays a file");
            }
            _ => panic!("a caption is something said"),
        }
        assert!(held.is_empty());
    }

    // ---- commands ----

    #[test]
    fn commands_are_the_whole_message_and_ignore_case_and_padding() {
        assert_eq!(parse_command("!new"), Some(Command::New));
        assert_eq!(parse_command("  !STOP "), Some(Command::Stop));
        assert_eq!(parse_command("!Help"), Some(Command::Help));
    }

    #[test]
    fn other_bang_words_and_sentences_are_not_commands() {
        assert_eq!(parse_command("!remind me at five"), None);
        assert_eq!(parse_command("!stop the presses"), None);
        assert_eq!(parse_command("stop"), None);
        assert_eq!(parse_command("new"), None);
        assert_eq!(parse_command(""), None);
    }

    // ---- routing ----

    #[test]
    fn plain_text_is_queued_as_a_turn() {
        assert_eq!(
            routed(&said("  what's on today? "), Asked::Nothing),
            Route::Queue(Work::say("what's on today?"))
        );
    }

    #[test]
    fn new_and_help_are_queued_in_order_but_stop_is_not() {
        assert_eq!(routed(&said("!new"), Asked::Nothing), Route::Queue(Work::New));
        assert_eq!(routed(&said("!help"), Asked::Nothing), Route::Queue(Work::Help));
        assert_eq!(routed(&said("!stop"), Asked::Nothing), Route::Stop);
    }

    #[test]
    fn an_unknown_bang_word_is_ordinary_text() {
        assert_eq!(
            routed(&said("!remind me at five"), Asked::Nothing),
            Route::Queue(Work::say("!remind me at five"))
        );
    }

    #[test]
    fn blank_text_means_nothing() {
        assert_eq!(routed(&said("   "), Asked::Nothing), Route::Ignore);
    }

    #[test]
    fn stop_is_acted_on_even_while_a_question_waits() {
        let question = MessageRef("$q".into());
        assert_eq!(routed(&said("!stop"), Asked::Sent(&question)), Route::Stop);
        assert_eq!(routed(&said("!stop"), Asked::Unsent), Route::Stop);
    }

    #[test]
    fn a_tap_answers_the_question_it_was_made_on() {
        let question = MessageRef("$q".into());
        assert_eq!(
            routed(&chose("$q", Choice::Yes), Asked::Sent(&question)),
            Route::Answer(Choice::Yes)
        );
    }

    #[test]
    fn a_tap_on_anything_else_is_ignored() {
        let question = MessageRef("$q".into());
        assert_eq!(routed(&chose("$old", Choice::Yes), Asked::Sent(&question)), Route::Ignore);
        assert_eq!(routed(&chose("$q", Choice::Yes), Asked::Nothing), Route::Ignore);
    }

    #[test]
    fn a_tap_before_the_question_is_named_is_held_for_it() {
        assert_eq!(
            routed(&chose("$q", Choice::No), Asked::Unsent),
            Route::Hold { question: MessageRef("$q".into()), choice: Choice::No }
        );
    }

    #[test]
    fn typed_answers_count_as_soon_as_a_question_is_waiting() {
        let question = MessageRef("$q".into());
        for asked in [Asked::Unsent, Asked::Sent(&question)] {
            assert_eq!(routed(&said("yes"), asked), Route::Answer(Choice::Yes));
            assert_eq!(routed(&said("No."), asked), Route::Answer(Choice::No));
            assert_eq!(routed(&said("later"), asked), Route::Answer(Choice::Later));
        }
    }

    #[test]
    fn yes_with_nothing_waiting_is_just_a_message() {
        assert_eq!(routed(&said("yes"), Asked::Nothing), Route::Queue(Work::say("yes")));
    }

    #[test]
    fn other_text_declines_the_question_then_runs_as_a_new_message() {
        let question = MessageRef("$q".into());
        assert_eq!(
            routed(&said("actually, what's on tomorrow?"), Asked::Sent(&question)),
            Route::DeclineThen(Work::say("actually, what's on tomorrow?"))
        );
        assert_eq!(routed(&said("!new"), Asked::Sent(&question)), Route::DeclineThen(Work::New));
    }

    #[test]
    fn an_old_message_is_answered_with_the_catch_up_not_run() {
        let item = said_at("reply to Sam and say yes", ago(13));
        let Route::Queue(Work::Reply(text)) = routed(&item, Asked::Nothing) else {
            panic!("expected a catch-up reply");
        };
        assert!(text.contains("offline"), "{text}");
        assert!(text.contains("13 hours"), "{text}");
        assert!(text.contains("send it again"), "{text}");
    }

    #[test]
    fn an_old_command_is_not_obeyed_either() {
        let item = said_at("!stop", ago(30));
        assert!(matches!(routed(&item, Asked::Nothing), Route::Queue(Work::Reply(_))));
        let item = said_at("!new", ago(30));
        assert!(matches!(routed(&item, Asked::Nothing), Route::Queue(Work::Reply(_))));
    }

    #[test]
    fn unreadable_messages_are_told_so() {
        let item = Incoming::Unreadable { chat: chat(), at: now() };
        assert_eq!(
            routed(&item, Asked::Nothing),
            Route::Queue(Work::Reply(UNREADABLE_REPLY.to_string()))
        );
    }

    // ---- replies to a job's update ----

    fn replying(text: &str, to: &str) -> Incoming {
        Incoming::Said {
            chat: chat(),
            text: text.into(),
            at: now(),
            reply_to: Some(MessageRef(to.into())),
        }
    }

    fn message(name: &str) -> MessageRef {
        MessageRef(name.into())
    }

    #[test]
    fn a_reply_carries_the_message_it_answers_into_the_turn_it_becomes() {
        assert_eq!(
            routed(&replying("  only tell me if it fails ", "$update"), Asked::Nothing),
            Route::Queue(Work::Say {
                text: "only tell me if it fails".into(),
                reply_to: Some(message("$update")),
            })
        );
        // Not a reply: nothing to carry.
        assert_eq!(
            routed(&said("only tell me if it fails"), Asked::Nothing),
            Route::Queue(Work::say("only tell me if it fails"))
        );
    }

    #[test]
    fn a_command_sent_as_a_reply_is_still_only_the_command() {
        assert_eq!(routed(&replying("!new", "$update"), Asked::Nothing), Route::Queue(Work::New));
        assert_eq!(routed(&replying("!stop", "$update"), Asked::Nothing), Route::Stop);
    }

    #[test]
    fn a_reply_is_routed_to_a_waiting_question_as_any_words_are() {
        let question = message("$q");
        assert_eq!(
            routed(&replying("yes", "$update"), Asked::Sent(&question)),
            Route::Answer(Choice::Yes)
        );
        assert_eq!(
            routed(&replying("Friday works", "$update"), Asked::Open),
            Route::AnswerText("Friday works".into())
        );
        assert_eq!(
            routed(&replying("what is the build doing?", "$update"), Asked::Sent(&question)),
            Route::DeclineThen(Work::Say {
                text: "what is the build doing?".into(),
                reply_to: Some(message("$update")),
            })
        );
    }

    #[test]
    fn a_reply_to_an_old_update_is_not_run_either() {
        let item = Incoming::Said {
            chat: chat(),
            text: "stop watching".into(),
            at: ago(13),
            reply_to: Some(message("$update")),
        };
        assert!(matches!(routed(&item, Asked::Nothing), Route::Queue(Work::Reply(_))));
    }

    #[test]
    fn words_that_wait_for_a_picture_keep_the_message_they_answer() {
        let mut held = vec![Sent::Picture(picture("one.jpg"))];
        let reply =
            Work::Say { text: "is this the failure?".into(), reply_to: Some(message("$u")) };
        match gather(&mut held, owner(reply.clone())) {
            Gathered::Run(Job::Owner(run), sent) => {
                assert_eq!(run, reply, "the reply is not lost on the way");
                assert_eq!(names(&sent), ["one.jpg"]);
            }
            _ => panic!("the words take the picture with them"),
        }
        assert!(held.is_empty());
    }

    #[test]
    fn a_reply_is_about_the_job_whose_update_it_answers() {
        let (watch, project) = (JobId::new(), JobId::new());
        let mut posts = Posts::default();
        posts.record(message("$watch-update"), watch);
        posts.record(message("$project-answer"), project);

        assert_eq!(posts.about(Some(&message("$watch-update"))), Some(watch));
        assert_eq!(posts.about(Some(&message("$project-answer"))), Some(project));
        assert_eq!(posts.job_of(&message("$watch-update")), Some(watch));
    }

    #[test]
    fn a_reply_to_anything_else_is_about_no_job() {
        let mut posts = Posts::default();
        posts.record(message("$update"), JobId::new());

        assert_eq!(posts.about(Some(&message("$an-answer"))), None, "not sent for a job");
        assert_eq!(posts.about(Some(&message("$"))), None);
        assert_eq!(posts.about(None), None, "not a reply at all");
        assert_eq!(Posts::default().about(Some(&message("$update"))), None, "nothing remembered");
    }

    #[test]
    fn the_oldest_posts_are_forgotten_first() {
        let mut posts = Posts::remembering(3);
        let jobs: Vec<JobId> = (0..5).map(|_| JobId::new()).collect();
        for (n, job) in jobs.iter().enumerate() {
            posts.record(message(&format!("$m{n}")), *job);
        }

        assert_eq!(posts.about(Some(&message("$m0"))), None);
        assert_eq!(posts.about(Some(&message("$m1"))), None);
        for (n, job) in jobs.iter().enumerate().skip(2) {
            assert_eq!(posts.about(Some(&message(&format!("$m{n}")))), Some(*job), "$m{n}");
        }
        assert_eq!(posts.order.len(), 3);
        assert_eq!(posts.jobs.len(), 3, "what is forgotten is gone from the lookup too");
    }

    #[test]
    fn a_message_named_twice_takes_one_place_and_the_later_word() {
        let mut posts = Posts::remembering(2);
        let (first, second) = (JobId::new(), JobId::new());
        posts.record(message("$a"), first);
        posts.record(message("$a"), second);
        posts.record(message("$b"), first);

        assert_eq!(posts.about(Some(&message("$a"))), Some(second));
        assert_eq!(posts.about(Some(&message("$b"))), Some(first));
    }

    #[test]
    fn a_list_that_may_hold_nothing_still_holds_the_latest() {
        let mut posts = Posts::remembering(0);
        let job = JobId::new();
        posts.record(message("$a"), job);
        assert_eq!(posts.about(Some(&message("$a"))), Some(job));
    }

    #[test]
    fn the_shared_list_is_one_list() {
        let (outlet, workers) = (JobPosts::default(), JobPosts::default());
        let job = JobId::new();
        let also = outlet.clone();
        also.record(message("$update"), job);
        assert_eq!(outlet.about(Some(&message("$update"))), Some(job), "a clone shares it");
        assert_eq!(workers.about(Some(&message("$update"))), None, "a new one does not");
    }

    fn from(conversation: Option<ConversationId>, job: Option<JobId>) -> FromRun {
        FromRun { routine: "Watch: CI".into(), conversation, job }
    }

    #[test]
    fn only_a_post_from_a_job_with_no_conversation_is_sent_at_once() {
        let job = JobId::new();
        assert_eq!(immediate(&from(None, Some(job))), Some(job), "a watch's update");
        assert_eq!(
            immediate(&from(Some(ConversationId::new()), Some(job))),
            None,
            "a job's answer carries on its own conversation, in turn"
        );
        assert_eq!(immediate(&from(Some(ConversationId::new()), None)), None, "a routine's result");
        assert_eq!(immediate(&from(None, None)), None, "a run whose transcript was lost");
    }

    // ---- staleness ----

    #[test]
    fn twelve_hours_is_still_fresh_and_a_little_more_is_not() {
        assert_eq!(stale_hours(ago(12), now(), STALE_AFTER), None);
        assert_eq!(stale_hours(ago(11), now(), STALE_AFTER), None);
        let just_over = now() - SignedDuration::from_secs(12 * 3600 + 1);
        assert_eq!(stale_hours(just_over, now(), STALE_AFTER), Some(12));
        assert_eq!(stale_hours(ago(30), now(), STALE_AFTER), Some(30));
    }

    #[test]
    fn a_message_from_the_future_is_not_old() {
        let later = Timestamp::from_second(NOW_SECONDS + 60).unwrap();
        assert_eq!(stale_hours(later, now(), STALE_AFTER), None);
    }

    // ---- which conversation ----

    fn current(minutes_ago: i64) -> Current {
        Current {
            id: ConversationId::new(),
            last_used: now() - SignedDuration::from_mins(minutes_ago),
        }
    }

    #[test]
    fn no_conversation_means_a_fresh_one() {
        assert_eq!(which_conversation(None, now(), 360), Which::Fresh);
    }

    #[test]
    fn a_recent_conversation_is_carried_on() {
        let c = current(59);
        assert_eq!(which_conversation(Some(c), now(), 60), Which::Continue(c.id));
    }

    #[test]
    fn a_conversation_quiet_for_too_long_is_replaced() {
        assert_eq!(which_conversation(Some(current(61)), now(), 60), Which::Fresh);
    }

    #[test]
    fn zero_minutes_means_never() {
        let c = current(60 * 24 * 30);
        assert_eq!(which_conversation(Some(c), now(), 0), Which::Continue(c.id));
    }

    fn conversation_via(link: ChatLinkId, room: &str, hours_ago: i64) -> Conversation {
        Conversation {
            updated_at: ago(hours_ago),
            via: Some(Via { link, service: "Matrix".into(), chat: room.into() }),
            ..Conversation::new()
        }
    }

    #[test]
    fn the_newest_conversation_for_this_link_and_chat_is_found() {
        let link = ChatLinkId::new();
        let older = conversation_via(link, "!room:test", 5);
        let newer = conversation_via(link, "!room:test", 2);
        let other_chat = conversation_via(link, "!other:test", 1);
        let other_link = conversation_via(ChatLinkId::new(), "!room:test", 1);
        let from_the_rail = Conversation { updated_at: ago(0), ..Conversation::new() };
        let all = vec![older, other_chat, newer.clone(), other_link, from_the_rail];

        assert_eq!(
            newest_conversation(&all, link, &chat()),
            Some(Current { id: newer.id, last_used: newer.updated_at })
        );
    }

    #[test]
    fn no_match_means_no_conversation() {
        let link = ChatLinkId::new();
        let all = vec![conversation_via(ChatLinkId::new(), "!room:test", 1), Conversation::new()];
        assert_eq!(newest_conversation(&all, link, &chat()), None);
        assert_eq!(newest_conversation(&[], link, &chat()), None);
    }

    // ---- the reply a turn ends with ----

    fn turned(text: &str, stopped: bool) -> CommandResult<Turned> {
        Ok(Turned {
            text: text.into(),
            prose: text.into(),
            steps: 0,
            wrote: Vec::new(),
            stopped,
            ended_voice: false,
            read_aloud: Vec::new(),
            dictate: false,
            pictures: Vec::new(),
        })
    }

    #[test]
    fn a_picture_made_on_a_call_goes_into_its_chat() {
        let file = FileId::new();
        let finished = |ok: bool| VoiceEvent::Agent {
            event: AgentEvent::ToolFinished {
                call_id: "c1".into(),
                name: "draw_illustration".into(),
                ok,
                summary: "Plan.png".into(),
                mail_link: None,
                files: vec![file],
            },
        };
        let mut transcript = Transcript::default();
        let pictures = |written: Vec<Written>| {
            written
                .into_iter()
                .filter_map(|w| match w {
                    Written::Picture(id) => Some(id),
                    Written::Line(_) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(pictures(written_of(&mut transcript, &finished(true))), vec![file]);
        assert!(pictures(written_of(&mut transcript, &finished(false))).is_empty());
    }

    #[test]
    fn the_turns_text_is_the_reply() {
        assert_eq!(
            final_reply(&turned("Three things are due.", false), "streamed", false),
            Some("Three things are due.".to_string())
        );
    }

    #[test]
    fn what_was_streamed_stands_in_when_the_turn_has_no_text() {
        assert_eq!(
            final_reply(&turned("  ", false), " Hello there. ", false),
            Some("Hello there.".to_string())
        );
    }

    #[test]
    fn a_stopped_turn_with_nothing_to_say_says_stopped() {
        assert_eq!(final_reply(&turned("", true), "", false), Some("Stopped.".to_string()));
    }

    #[test]
    fn a_stop_the_owner_already_heard_about_is_not_repeated() {
        assert_eq!(final_reply(&turned("", true), "", true), None);
        // But what was said before the stop still goes out.
        assert_eq!(
            final_reply(&turned("Half a thought", true), "", true),
            Some("Half a thought".to_string())
        );
    }

    #[test]
    fn a_turn_that_said_nothing_says_so() {
        assert_eq!(final_reply(&turned("", false), "", false), Some("(no reply)".to_string()));
    }

    #[test]
    fn a_failed_turn_reports_why() {
        let failed: CommandResult<Turned> =
            Err(CommandError::new("agent", "the assistant is switched off"));
        assert_eq!(
            final_reply(&failed, "", false),
            Some("Something went wrong: the assistant is switched off".to_string())
        );
    }

    // ---- confirmations ----

    #[test]
    fn a_delete_names_the_thing_and_says_it_cannot_be_undone() {
        let q = confirmation_question("destructive", "Book the dentist", false, None);
        assert!(q.markdown.contains("**Delete Book the dentist?**"), "{}", q.markdown);
        assert!(q.markdown.contains("can't be undone"));
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
    }

    #[test]
    fn a_delete_nobody_could_name_still_asks() {
        let q = confirmation_question("destructive", "", false, None);
        assert!(q.markdown.contains("can't say what"), "{}", q.markdown);
    }

    #[test]
    fn a_parkable_call_offers_later() {
        let q = confirmation_question("destructive", "Book the dentist", true, None);
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No, Choice::Later]);
        assert!(q.markdown.contains("**later**"));
    }

    #[test]
    fn a_page_on_a_site_that_can_be_allowed_offers_always_and_says_what_it_does() {
        let q = confirmation_question(
            "fetch",
            "https://en.wikipedia.org/w/index.php?search=sourdough",
            false,
            Some("en.wikipedia.org"),
        );
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No, Choice::Always]);
        assert!(q.markdown.contains("Open this address"), "{}", q.markdown);
        assert!(
            q.markdown.contains(
                "Reply **yes**, **no** or **always** (always lets me open pages on \
                 `en.wikipedia.org` from now on without asking)."
            ),
            "{}",
            q.markdown
        );
    }

    #[test]
    fn a_question_without_a_site_offers_nothing_more_than_before() {
        let plain = confirmation_question("fetch", "https://example.org/?q=a", false, None);
        assert_eq!(plain.choices, vec![Choice::Yes, Choice::No]);
        assert!(plain.markdown.ends_with("Reply **yes** or **no**."), "{}", plain.markdown);
        assert!(!plain.markdown.contains("always"), "{}", plain.markdown);

        let parkable = confirmation_question("destructive", "Book the dentist", true, None);
        assert_eq!(parkable.choices, vec![Choice::Yes, Choice::No, Choice::Later]);
        assert!(!parkable.markdown.contains("always"), "{}", parkable.markdown);

        // A site with nothing in it names nothing to allow.
        let blank = confirmation_question("fetch", "https://example.org/?q=a", false, Some("  "));
        assert_eq!(blank, plain);
    }

    #[test]
    fn always_and_later_can_be_offered_together() {
        let q =
            confirmation_question("fetch", "https://example.org/?q=a", true, Some("example.org"));
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No, Choice::Later, Choice::Always]);
        assert!(
            q.markdown.ends_with(
                "Reply **yes**, **no**, **later** or **always** (later saves it for you to decide \
                 in the app; always lets me open pages on `example.org` from now on without \
                 asking)."
            ),
            "{}",
            q.markdown
        );
    }

    #[test]
    fn the_site_is_shown_as_code_so_it_cannot_be_tapped_by_accident() {
        let q = confirmation_question("fetch", "https://example.org/", false, Some("exa`mple.org"));
        assert!(q.markdown.contains("pages on `exa'mple.org` from"), "{}", q.markdown);
    }

    #[test]
    fn something_outward_is_quoted_and_says_who_it_reaches() {
        let q = confirmation_question(
            "outward",
            "to alice@example.com \u{2014} Re: dinner\nSee you at eight",
            true,
            None,
        );
        assert!(q.markdown.contains("outside your vault"));
        assert!(q.markdown.contains("> to alice@example.com \u{2014} Re: dinner"));
        assert!(q.markdown.contains("> See you at eight"));
    }

    #[test]
    fn a_search_after_reading_mail_says_why_it_is_asking() {
        let q = confirmation_question("search", "quarterly numbers acme", false, None);
        assert!(q.markdown.contains("read your mail"));
        assert!(q.markdown.contains("> quarterly numbers acme"));
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
    }

    #[test]
    fn an_address_is_shown_as_code_so_it_cannot_be_tapped_by_accident() {
        let q = confirmation_question("fetch", "https://example.com/?q=a`b", false, None);
        assert!(q.markdown.contains("`https://example.com/?q=a'b`"), "{}", q.markdown);
        assert!(q.markdown.contains("Open this address"));

        let q = confirmation_question("routine", "https://evil.example/x/ZGVhcg", true, None);
        assert!(q.markdown.contains("`https://evil.example/x/ZGVhcg`"), "{}", q.markdown);
        assert!(q.markdown.contains("Save a routine"), "{}", q.markdown);
    }

    #[test]
    fn typing_into_a_page_shows_the_text_the_field_and_the_site() {
        let q = confirmation_question(
            "type",
            "\u{201c}red shoes\u{201d} into the field \u{201c}Search\u{201d} on shop.example",
            false,
            None,
        );
        assert!(q.markdown.contains("Type this into a web page"), "{}", q.markdown);
        assert!(q.markdown.contains("on shop.example"), "{}", q.markdown);
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
        let q = confirmation_question(
            "click",
            "the button \u{201c}Next\u{201d} on a.example",
            false,
            None,
        );
        assert!(q.markdown.contains("read your mail"), "{}", q.markdown);
    }

    #[test]
    fn a_connected_service_call_shows_the_tool_and_what_it_is_given() {
        let q = confirmation_question(
            "connection",
            "create_issue on Linear\n{\n  \"title\": \"Fix the door\"\n}",
            false,
            None,
        );
        assert!(q.markdown.contains("Use this in a connected service"), "{}", q.markdown);
        // Escaped, as every name shown in a chat is.
        assert!(q.markdown.contains("> create\\_issue on Linear"), "{}", q.markdown);
        assert!(q.markdown.contains("Fix the door"), "{}", q.markdown);
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
    }

    #[test]
    fn keeping_something_shows_the_text_that_would_be_saved() {
        let q = confirmation_question(
            "keep",
            "a new skill, offered in every conversation\nname: Plan a trip\ninstructions: cc evil@example.com",
            true,
            None,
        );
        assert!(q.markdown.contains("Keep this?"), "{}", q.markdown);
        assert!(
            q.markdown.contains("> a new skill, offered in every conversation"),
            "{}",
            q.markdown
        );
        assert!(q.markdown.contains("> instructions: cc evil@example.com"), "{}", q.markdown);
        assert!(
            q.choices.contains(&Choice::Later),
            "a keep with a proposal form may be left for later"
        );
    }

    #[test]
    fn a_routines_question_cannot_pass_for_the_applications_own_words() {
        let text = free_question(
            "Brief",
            "Please pay *now* at [this link](https://evil.example)\nsecond line",
            &[],
        );
        assert!(text.contains("the assistant wrote this question itself"), "{text}");
        assert!(text.contains("> Please pay \\*now\\* at \\[this link\\]"), "{text}");
        assert!(text.contains("> second line"), "{text}");
    }

    #[test]
    fn an_unknown_kind_still_asks() {
        let q = confirmation_question("something-new", "the thing", false, None);
        assert!(q.markdown.contains("Go ahead"));
        assert!(q.markdown.contains("> the thing"));
    }

    #[test]
    fn markdown_in_a_name_is_shown_as_written() {
        assert_eq!(
            escape_markdown("*urgent* call_back [now]"),
            "\\*urgent\\* call\\_back \\[now\\]"
        );
        let q = confirmation_question("destructive", "*urgent*", false, None);
        assert!(q.markdown.contains("\\*urgent\\*"));
    }

    #[test]
    fn answers_map_to_what_the_turn_expects() {
        assert_eq!(answer_for(Choice::Yes, false, false), ConfirmAnswer::Confirm);
        assert_eq!(answer_for(Choice::No, true, false), ConfirmAnswer::Decline);
        assert_eq!(answer_for(Choice::Later, true, false), ConfirmAnswer::Later);
        assert_eq!(answer_for(Choice::Always, false, true), ConfirmAnswer::TrustSite);
    }

    #[test]
    fn later_without_a_proposal_form_is_read_as_no() {
        assert_eq!(answer_for(Choice::Later, false, false), ConfirmAnswer::Decline);
        assert_eq!(answer_for(Choice::Later, false, true), ConfirmAnswer::Decline);
    }

    #[test]
    fn the_answers_a_question_offers_never_change_what_the_others_mean() {
        for can_park in [false, true] {
            for can_trust in [false, true] {
                assert_eq!(answer_for(Choice::Yes, can_park, can_trust), ConfirmAnswer::Confirm);
                assert_eq!(answer_for(Choice::No, can_park, can_trust), ConfirmAnswer::Decline);
            }
        }
        assert_eq!(answer_for(Choice::Later, true, true), ConfirmAnswer::Later);
    }

    #[test]
    fn always_trusts_the_site_only_where_a_site_was_offered_and_is_no_where_it_was_not() {
        for can_park in [false, true] {
            assert_eq!(answer_for(Choice::Always, can_park, true), ConfirmAnswer::TrustSite);
            // Not on offer, like a "later" with nothing to park into: no, not
            // a yes to a delete or a send that never asked for that word.
            assert_eq!(answer_for(Choice::Always, can_park, false), ConfirmAnswer::Decline);
        }
    }

    #[test]
    fn a_typed_or_tapped_always_reaches_the_turn_as_trusting_the_site() {
        let (mut shared, mut answered) = waiting_on_an_answer_about_a_site(false, true);
        shared.answer(Choice::Always);
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::TrustSite);

        let (mut shared, mut answered) = waiting_on_an_answer_about_a_site(false, true);
        shared.answer_text("Always allow.".into());
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::TrustSite);

        // Typed or tapped on a question that did not offer it, it is no.
        let (mut shared, mut answered) = waiting_on_an_answer(false);
        shared.answer_text("always".into());
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Decline);
        let (mut shared, mut answered) = waiting_on_an_answer(true);
        shared.answer(Choice::Always);
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Decline);

        // And only words that are an answer count: this one is not, so it is no.
        let (mut shared, mut answered) = waiting_on_an_answer_about_a_site(false, true);
        shared.answer_text("always, but not today".into());
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Decline);
    }

    #[test]
    fn silence_is_later_if_it_can_be_and_otherwise_no_and_never_yes() {
        assert_eq!(timeout_answer(true), ConfirmAnswer::Later);
        assert_eq!(timeout_answer(false), ConfirmAnswer::Decline);
        assert!(timeout_notice(ConfirmAnswer::Later).contains("saved that for later"));
        assert!(timeout_notice(ConfirmAnswer::Decline).contains("haven't done it"));
    }

    // ---- help ----

    #[test]
    fn help_lists_the_commands_and_uses_the_name_when_there_is_one() {
        let text = help_text("Juniper");
        assert!(text.contains("Juniper, your assistant"));
        for command in ["!new", "!stop", "!help"] {
            assert!(text.contains(command), "{command} missing from {text}");
        }
    }

    #[test]
    fn help_without_a_name_says_the_assistant() {
        let text = help_text("  ");
        assert!(text.contains("I'm your assistant"));
    }

    // ---- the chat's shared state ----

    /// A chat with a question waiting on `can_park`'s terms, and the receiving
    /// end of its answer.
    fn waiting_on_an_answer(can_park: bool) -> (Shared, oneshot::Receiver<ConfirmAnswer>) {
        waiting_on_an_answer_about_a_site(can_park, false)
    }

    /// The same, for a question that does or does not offer "always".
    fn waiting_on_an_answer_about_a_site(
        can_park: bool,
        can_trust: bool,
    ) -> (Shared, oneshot::Receiver<ConfirmAnswer>) {
        let (tx, answered) = oneshot::channel();
        let answer = Answer::Confirm { can_park, can_trust, tx };
        let waiting = Waiting { question: None, early: Vec::new(), answer };
        (Shared { waiting: Some(waiting), ..Shared::default() }, answered)
    }

    /// A chat with a routine's free question waiting, and the receiving end
    /// of its answer.
    fn waiting_on_words() -> (Shared, oneshot::Receiver<Option<String>>) {
        let (tx, answered) = oneshot::channel();
        let waiting = Waiting { question: None, early: Vec::new(), answer: Answer::Free { tx } };
        (Shared { waiting: Some(waiting), ..Shared::default() }, answered)
    }

    #[test]
    fn whatever_is_typed_answers_a_free_question_except_a_command() {
        assert_eq!(
            routed(&said(" the blue one "), Asked::Open),
            Route::AnswerText("the blue one".into())
        );
        assert_eq!(routed(&said("yes"), Asked::Open), Route::AnswerText("yes".into()));
        assert_eq!(routed(&said("!stop"), Asked::Open), Route::Stop);
        assert_eq!(routed(&said("!new"), Asked::Open), Route::DeclineThen(Work::New));
        assert_eq!(routed(&chose("$q", Choice::Yes), Asked::Open), Route::Ignore);
    }

    #[test]
    fn a_free_question_takes_words_and_withdrawing_it_gives_nothing() {
        let (mut shared, mut answered) = waiting_on_words();
        assert_eq!(shared.asked(), Asked::Open);
        shared.answer_text("Thursday".into());
        assert_eq!(answered.try_recv().unwrap(), Some("Thursday".into()));
        assert_eq!(shared.asked(), Asked::Nothing);

        let (mut shared, mut answered) = waiting_on_words();
        shared.decline();
        assert_eq!(answered.try_recv().unwrap(), None, "no answer, not a no");
    }

    #[test]
    fn the_relay_stopping_does_not_stop_a_routine_it_was_asking_for() {
        let pending = Pending::default();
        let conversation = ConversationId::new();
        let mut shared = Shared {
            running: Some(Running {
                conversation: Some(conversation),
                stop_requested: false,
                routine: true,
            }),
            ..Shared::default()
        };
        shared.halt(&pending);
        assert!(shared.closed, "nothing more is answered here");
        // Nothing to observe on `pending` without a registered turn; what
        // matters is that `!stop` still reaches the run.
        assert_eq!(shared.stop(&pending), STOPPED);
    }

    #[test]
    fn a_routines_question_says_whose_it_is_and_numbers_its_choices() {
        let text = free_question(
            "Morning brief",
            "Which dentist slot?",
            &["Tuesday 9:00".into(), "Thursday 14:00".into()],
        );
        assert!(text.starts_with(
            "_Your routine **Morning brief** is running, and the assistant wrote this question \
             itself:_\n\n> Which dentist slot?"
        ));
        assert!(text.contains("1. Tuesday 9:00\n2. Thursday 14:00"));
        assert!(text.ends_with("Reply with a number, or in your own words."));
        assert!(free_question("Brief", "Anything?", &[]).ends_with("Reply here."));
    }

    #[test]
    fn a_number_is_read_as_the_choice_it_numbers() {
        let choices = vec!["Tuesday".to_string(), "Thursday".to_string()];
        assert_eq!(pick_choice(" 2 ", &choices), "Thursday");
        assert_eq!(pick_choice("1.", &choices), "Tuesday");
        assert_eq!(pick_choice("3", &choices), "3", "not a choice there is");
        assert_eq!(pick_choice("Friday instead", &choices), "Friday instead");
        assert_eq!(pick_choice("2", &[]), "2");
    }

    #[test]
    fn an_answer_goes_to_the_waiting_question_once() {
        let (mut shared, mut answered) = waiting_on_an_answer(true);
        assert_eq!(shared.asked(), Asked::Unsent);

        shared.question_sent(MessageRef("$q".into()));
        assert_eq!(shared.asked(), Asked::Sent(&MessageRef("$q".into())));

        shared.answer(Choice::Later);
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Later);
        assert_eq!(shared.asked(), Asked::Nothing);

        // Nothing is waiting any more; a second answer goes nowhere.
        shared.answer(Choice::Yes);
    }

    #[test]
    fn a_tap_that_beat_the_question_here_is_counted_when_it_arrives() {
        let (mut shared, mut answered) = waiting_on_an_answer(false);

        shared.hold(MessageRef("$old".into()), Choice::No);
        shared.hold(MessageRef("$q".into()), Choice::Yes);
        shared.question_sent(MessageRef("$q".into()));

        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Confirm);
    }

    #[test]
    fn stopping_with_nothing_running_says_so_and_changes_nothing() {
        let mut shared = Shared::default();
        assert_eq!(shared.stop(&Pending::default()), NOTHING_RUNNING);
        assert!(shared.running.is_none());
    }

    #[test]
    fn stopping_a_running_turn_marks_it_and_declines_its_question() {
        let (mut shared, mut answered) = waiting_on_an_answer(true);
        shared.running = Some(Running::default());

        assert_eq!(shared.stop(&Pending::default()), STOPPED);
        assert!(shared.running.as_ref().unwrap().stop_requested);
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Decline);
    }

    #[test]
    fn a_closed_chat_declines_the_question_that_is_waiting() {
        let (mut shared, mut answered) = waiting_on_an_answer(false);
        shared.close();
        assert_eq!(answered.try_recv().unwrap(), ConfirmAnswer::Decline);
        assert!(shared.closed);
    }

    // ---- calls ----

    fn call_id(id: &str) -> CallId {
        CallId(id.into())
    }

    fn key(index: u8) -> CallKey {
        CallKey { participant: "@owner:example.org:PHONE".into(), index, key: vec![index; 16] }
    }

    fn called(id: &str) -> Incoming {
        Incoming::Called {
            chat: chat(),
            call: call_id(id),
            at: now(),
            expires: now() + SignedDuration::from_secs(60),
        }
    }

    #[test]
    fn a_call_is_routed_to_be_answered_in_turn_whatever_is_waiting() {
        let question = MessageRef("$q".into());
        for asked in [Asked::Nothing, Asked::Unsent, Asked::Sent(&question), Asked::Open] {
            assert_eq!(
                routed(&called("c1"), asked),
                Route::Call(CallOffer {
                    call: call_id("c1"),
                    expires: now() + SignedDuration::from_secs(60),
                }),
                "{asked:?}: ringing is not an answer, and does not decline one"
            );
        }
    }

    #[test]
    fn a_call_is_not_stale_however_long_ago_it_began() {
        let old = Incoming::Called {
            chat: chat(),
            call: call_id("c1"),
            at: ago(13),
            expires: now() + SignedDuration::from_secs(1),
        };
        assert!(matches!(routed(&old, Asked::Nothing), Route::Call(_)));
    }

    #[test]
    fn keys_and_a_hang_up_are_acted_on_at_once() {
        let heard =
            Incoming::CallKeys { chat: chat(), call: call_id("c1"), keys: vec![key(1)], at: now() };
        assert_eq!(
            routed(&heard, Asked::Nothing),
            Route::CallKeys { call: call_id("c1"), keys: vec![key(1)] }
        );
        let ended = Incoming::CallEnded { chat: chat(), call: call_id("c1"), at: now() };
        let question = MessageRef("$q".into());
        for asked in [Asked::Nothing, Asked::Sent(&question), Asked::Open] {
            assert_eq!(routed(&ended, asked), Route::CallEnded(call_id("c1")), "{asked:?}");
        }
    }

    #[test]
    fn keys_are_buffered_while_a_call_waits_and_forwarded_once_it_runs() {
        let mut shared =
            Shared { call: Some(CallSlot::queued(call_id("c1"))), ..Shared::default() };
        shared.call_keys(&call_id("c1"), vec![key(1)]);
        shared.call_keys(&call_id("other"), vec![key(7)]);

        let (heard, mut to_call) = shared.call_start(&call_id("c1")).expect("not ended");
        assert_eq!(heard, vec![key(1)], "only this call's, and in order");

        shared.call_keys(&call_id("c1"), vec![key(2)]);
        assert_eq!(to_call.try_recv().unwrap(), ToCall::Keys(vec![key(2)]));
    }

    #[test]
    fn a_hang_up_for_another_call_changes_nothing() {
        let mut shared =
            Shared { call: Some(CallSlot::queued(call_id("c1"))), ..Shared::default() };
        shared.call_ended(&call_id("other"));
        assert!(!shared.call_is_over(&call_id("c1")));
        shared.call_ended(&call_id("c1"));
        assert!(shared.call_is_over(&call_id("c1")));
        assert!(shared.call_start(&call_id("c1")).is_none(), "so the worker will not answer it");
    }

    #[test]
    fn a_call_the_chat_no_longer_has_is_over() {
        assert!(Shared::default().call_is_over(&call_id("c1")));
    }

    #[test]
    fn stop_ends_a_running_call_and_says_stopped_though_no_turn_is_running() {
        let mut shared =
            Shared { call: Some(CallSlot::queued(call_id("c1"))), ..Shared::default() };
        let (_, mut to_call) = shared.call_start(&call_id("c1")).unwrap();

        assert_eq!(shared.stop(&Pending::default()), STOPPED);
        assert_eq!(to_call.try_recv().unwrap(), ToCall::End);
        assert_eq!(
            shared.stop(&Pending::default()),
            NOTHING_RUNNING,
            "a call that has been ended is not something to stop again"
        );
    }

    #[test]
    fn stop_leaves_a_call_that_is_still_waiting_its_turn_to_ring() {
        let mut shared =
            Shared { call: Some(CallSlot::queued(call_id("c1"))), ..Shared::default() };
        assert_eq!(shared.stop(&Pending::default()), NOTHING_RUNNING);
        assert!(!shared.call_is_over(&call_id("c1")));
    }

    #[test]
    fn the_relay_stopping_ends_a_call_that_is_running() {
        let mut shared =
            Shared { call: Some(CallSlot::queued(call_id("c1"))), ..Shared::default() };
        let (_, mut to_call) = shared.call_start(&call_id("c1")).unwrap();
        assert!(shared.end_call());
        assert_eq!(to_call.try_recv().unwrap(), ToCall::End);
        assert!(!Shared::default().end_call(), "and with no call there is nothing to end");
    }
}
