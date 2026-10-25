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

use crate::connector::{ChatId, Choice, Connector, Incoming, MessageRef, Picture, Question};
use everyday_core::chat::{ChatLink, Via};
use everyday_core::{ChatLinkId, Conversation, ConversationId, ConversationQuery, Vault};
use everyday_service::agent::{AgentEvent, ConfirmAnswer, Pending, Sink, Turned};
use everyday_service::chat::{ChatOutlet, FromRun};
use everyday_service::error::CommandResult;
use everyday_service::service::Service;
use futures::FutureExt;
use futures::future::BoxFuture;
use jiff::{SignedDuration, Timestamp};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

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

/// How long a picture sent with nothing written beside it waits for the
/// words that go with it.
///
/// A phone sends a picture and the question about it as two messages more
/// often than as one: the photo goes, then "what is this plant?" is typed
/// under it. Answering the picture the moment it lands would answer a
/// question nobody asked yet, and then the real one separately. So it is
/// held, and goes with whatever is said next -- or alone, once a minute has
/// passed with nothing more, which is ample time to type a question and
/// short enough that a picture sent on its own is not left unanswered.
/// Further pictures sent meanwhile are gathered with it.
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
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            confirm_timeout: CONFIRM_TIMEOUT,
            stale_after: STALE_AFTER,
            shutdown_grace: SHUTDOWN_GRACE,
            picture_wait: PICTURE_WAIT,
        }
    }
}

const NEW_CONVERSATION: &str = "Starting a new conversation.";
const STOPPED: &str = "Stopped.";
const NOTHING_RUNNING: &str = "Nothing is running.";
const NO_REPLY: &str = "(no reply)";
const UNREADABLE_REPLY: &str = "I couldn't read that message \u{2014} it was encrypted for a session I don't have. Try sending it again.";
const NO_ANSWER: &str = "No answer, so I'll carry on with my best guess.";

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
    mut inbox: mpsc::Receiver<Incoming>,
    mut stop: watch::Receiver<bool>,
    timing: Timing,
) {
    tracing::info!(service = link.service.label(), "a chat relay is up");
    let (outbound_tx, mut outbound) = mpsc::unbounded_channel();
    let outlet: Arc<dyn ChatOutlet> =
        Arc::new(RelayOutlet { connector: connector.clone(), outbound: outbound_tx });
    let _open = OpenOutlet::new(service.clone(), link.id, outlet);
    let context = Arc::new(Context { service, vault, link, connector, timing, stop: stop.clone() });
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
        answer: oneshot::Sender<ConfirmAnswer>,
    },
    Ask {
        from: FromRun,
        question: String,
        choices: Vec<String>,
        answer: oneshot::Sender<Option<String>>,
    },
}

/// [`ChatOutlet`] over a running relay: finds the owner's private chat, and
/// hands the job to the dispatcher, which queues it on that chat's worker.
struct RelayOutlet {
    connector: Arc<dyn Connector>,
    outbound: mpsc::UnboundedSender<(ChatId, RoutineJob)>,
}

impl RelayOutlet {
    /// Queue a job for the owner's private chat. `Err` says why it could not
    /// be: there is no such chat yet, or the relay has stopped.
    async fn queue(
        connector: Arc<dyn Connector>,
        outbound: mpsc::UnboundedSender<(ChatId, RoutineJob)>,
        job: RoutineJob,
    ) -> Result<(), String> {
        let chat =
            connector.home().await.ok_or("the owner has not joined the bot's private room yet")?;
        outbound.send((chat, job)).map_err(|_| "the chat link has stopped".to_string())
    }
}

impl ChatOutlet for RelayOutlet {
    fn post(&self, from: FromRun, markdown: String) -> BoxFuture<'static, Result<(), String>> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
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
    ) -> BoxFuture<'static, ConfirmAnswer> {
        let (connector, outbound) = (self.connector.clone(), self.outbound.clone());
        Box::pin(async move {
            let (answer, answered) = oneshot::channel();
            let job = RoutineJob::Confirm { from, kind, subject, can_park, answer };
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
        everyday_service::blocking(move || Ok(vault.agent_settings()?.name))
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
        let marked = everyday_service::blocking(move || {
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
        let all = everyday_service::blocking(move || {
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
}

impl ChatHandle {
    /// Queue `work`. A worker only goes away when the relay does, so a send
    /// that fails is the relay winding down, and dropping the item is right.
    fn enqueue(&self, work: Work) {
        let _ = self.queue.send(Job::Owner(work));
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

        match route(&item, shared.asked(), now, context.timing.stale_after) {
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
        }
    }

    /// Queue what a routine needs on `chat`'s worker. Dropped, if the relay
    /// is winding down, with its answering half -- which the routine reads
    /// as the chat having gone.
    fn routine(&mut self, chat: ChatId, job: RoutineJob) {
        let _ = self.handle_for(&chat).queue.send(Job::Routine(job));
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
    /// Yes, no or -- when `can_park` -- later. Without `can_park`, "later" is
    /// read as "no".
    Confirm { can_park: bool, tx: oneshot::Sender<ConfirmAnswer> },
    /// Whatever the owner types next: a routine's question in its own words.
    Free { tx: oneshot::Sender<Option<String>> },
}

impl Answer {
    /// Answer with a tap.
    fn chose(self, choice: Choice) {
        match self {
            Answer::Confirm { can_park, tx } => {
                let _ = tx.send(answer_for(choice, can_park));
            }
            Answer::Free { tx } => {
                let word = match choice {
                    Choice::Yes => "yes",
                    Choice::No => "no",
                    Choice::Later => "later",
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
            Answer::Confirm { can_park, tx } => {
                let choice = Choice::from_text(&text).unwrap_or(Choice::No);
                let _ = tx.send(answer_for(choice, can_park));
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
    }

    /// The owner's `!stop`: cancel the turn that is running, and decline the
    /// question it may be waiting on. What to say back.
    fn stop(&mut self, pending: &Pending) -> &'static str {
        let Some(running) = &mut self.running else { return NOTHING_RUNNING };
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
    /// Run this as a turn.
    Say(String),
    /// A picture, and what was written with it. Held by the worker until
    /// what is said next, when nothing was -- see [`PICTURE_WAIT`] -- and
    /// otherwise run as a turn at once, with any pictures already held.
    Show { picture: Picture, caption: String },
    /// `!new`.
    New,
    /// `!help`.
    Help,
    /// Send this, and nothing else happens. Queued rather than sent by the
    /// dispatcher so that a chat's replies come out in the order its messages
    /// came in.
    Reply(String),
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
/// * A picture never answers a question. It is the owner moving on, as
///   typed text that is not an answer is.
pub(crate) fn route(
    item: &Incoming,
    asked: Asked<'_>,
    now: Timestamp,
    stale_after: SignedDuration,
) -> Route {
    match item {
        Incoming::Unreadable { .. } => Route::Queue(Work::Reply(UNREADABLE_REPLY.to_string())),
        Incoming::Chose { question, choice, .. } => match asked {
            Asked::Sent(waiting_on) if waiting_on == question => Route::Answer(*choice),
            Asked::Unsent => Route::Hold { question: question.clone(), choice: *choice },
            Asked::Sent(_) | Asked::Nothing | Asked::Open => Route::Ignore,
        },
        Incoming::Showed { picture, caption, at, .. } => {
            if let Some(hours) = stale_hours(*at, now, stale_after) {
                return Route::Queue(Work::Reply(catch_up_reply(hours)));
            }
            let work = Work::Show { picture: picture.clone(), caption: caption.trim().to_string() };
            match asked {
                Asked::Nothing => Route::Queue(work),
                // A picture answers no question: the owner has moved on.
                Asked::Unsent | Asked::Sent(_) | Asked::Open => Route::DeclineThen(work),
            }
        }
        Incoming::Said { text, at, .. } => {
            let text = text.trim();
            if text.is_empty() {
                return Route::Ignore;
            }
            if let Some(hours) = stale_hours(*at, now, stale_after) {
                return Route::Queue(Work::Reply(catch_up_reply(hours)));
            }
            let command = parse_command(text);
            if command == Some(Command::Stop) {
                return Route::Stop;
            }
            let work = match command {
                Some(Command::New) => Work::New,
                Some(Command::Help) => Work::Help,
                _ => Work::Say(text.to_string()),
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
                    Work::Say(text) => Route::AnswerText(text),
                    other => Route::DeclineThen(other),
                },
            }
        }
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
    // Pictures sent with nothing written beside them, waiting for the words
    // that go with them. See `PICTURE_WAIT`.
    let mut held: Vec<Picture> = Vec::new();
    loop {
        let next = if held.is_empty() {
            work.recv().await
        } else {
            match tokio::time::timeout(context.timing.picture_wait, work.recv()).await {
                Ok(next) => next,
                // Nothing more came: the pictures are the whole message.
                Err(_) => Some(Job::Owner(Work::Say(String::new()))),
            }
        };
        let Some(item) = next else { break };
        // Stopped with messages still queued: they are not run, and nobody is
        // told. The owner's next message, after the vault unlocks, is a new
        // one.
        if context.stopping() {
            break;
        }
        let (item, pictures) = match gather(&mut held, item) {
            Gathered::Hold => continue,
            Gathered::Run(item, pictures) => (item, pictures),
            // Something else to do while pictures wait: they go first, on
            // their own, so they are answered in the order they were sent.
            Gathered::First(pictures, item) => {
                let alone = Job::Owner(Work::Say(String::new()));
                run_caught(&context, &chat, &shared, &mut memory, alone, pictures).await;
                (item, Vec::new())
            }
        };
        run_caught(&context, &chat, &shared, &mut memory, item, pictures).await;
    }
}

/// What a worker does with the next thing queued, given the pictures it is
/// holding. See [`gather`].
enum Gathered {
    /// A picture with nothing said: hold it, and wait for more.
    Hold,
    /// Run this, with these pictures.
    Run(Job, Vec<Picture>),
    /// Run the pictures held, alone, and then this.
    First(Vec<Picture>, Job),
}

/// Fold `item` into the pictures `held`, or run it.
///
/// A picture with nothing written beside it is held. Anything the owner
/// says -- a picture's caption included -- takes every picture held with
/// it. Anything else (a command, a routine's message) finds them sent on
/// their own first.
fn gather(held: &mut Vec<Picture>, item: Job) -> Gathered {
    match item {
        Job::Owner(Work::Show { picture, caption }) if caption.is_empty() => {
            held.push(picture);
            Gathered::Hold
        }
        Job::Owner(Work::Show { picture, caption }) => {
            held.push(picture);
            Gathered::Run(Job::Owner(Work::Say(caption)), std::mem::take(held))
        }
        Job::Owner(Work::Say(text)) => {
            Gathered::Run(Job::Owner(Work::Say(text)), std::mem::take(held))
        }
        other if held.is_empty() => Gathered::Run(other, Vec::new()),
        other => Gathered::First(std::mem::take(held), other),
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
    pictures: Vec<Picture>,
) {
    let done = AssertUnwindSafe(do_work(context, chat, shared, memory, item, pictures))
        .catch_unwind()
        .await;
    if done.is_err() {
        tracing::error!(chat = %chat.0, "a chat turn panicked; carrying on with the next message");
        {
            let mut shared = shared.lock().unwrap();
            shared.running = None;
            shared.waiting = None;
        }
        context.typing(chat, false).await;
        if !context.stopping() {
            context.tell(chat, "Something went wrong: the assistant stopped unexpectedly.").await;
        }
    }
}

/// Do one thing queued for a chat. `pictures` go with a turn -- the owner's
/// words, or a picture's caption -- and with nothing else; [`gather`] sees
/// that they only ever come with one.
async fn do_work(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    job: Job,
    pictures: Vec<Picture>,
) {
    let work = match job {
        Job::Owner(work) => work,
        Job::Routine(job) => return for_routine(context, chat, shared, memory, job).await,
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
        Work::Say(text) => converse(context, chat, shared, memory, text, pictures).await,
        Work::Show { picture, caption } => {
            let pictures = pictures.into_iter().chain([picture]).collect();
            converse(context, chat, shared, memory, caption, pictures).await
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
            let sent = context.connector.send(chat, &markdown).await;
            if sent.is_ok()
                && let Some(conversation) = from.conversation
            {
                memory.adopt(conversation);
                context.adopt_conversation(chat, conversation).await;
            }
            let _ = done.send(sent.map(|_| ()).map_err(|e| e.to_string()));
        }
        RoutineJob::Confirm { from, kind, subject, can_park, answer } => {
            running(&from);
            let given =
                ask(context, chat, shared, &kind, &subject, can_park, Some(&from.routine)).await;
            shared.lock().unwrap().running = None;
            let _ = answer.send(given);
        }
        RoutineJob::Ask { from, question, choices, answer } => {
            running(&from);
            let given = ask_free(context, chat, shared, &from.routine, &question, &choices).await;
            shared.lock().unwrap().running = None;
            let _ = answer.send(given);
        }
    }
}

/// One turn: the owner's message in, the assistant's one reply out.
///
/// The steps are the design's "A turn": typing on; the turn itself, with its
/// events read as it goes so a question can be put to the owner while the
/// turn waits; typing off; one message.
async fn converse(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    memory: &mut Memory,
    text: String,
    pictures: Vec<Picture>,
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
    let pictures = pictures.into_iter().map(|p| (p.bytes, p.filename)).collect();
    let mut turn = Box::pin(context.service.chat_turn(via, conversation, text, pictures, sink));
    let result = loop {
        tokio::select! {
            biased;
            Some(event) = events.recv() => {
                on_event(context, chat, shared, conversation, &mut told, event).await;
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
            _ => {}
        }
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
    event: AgentEvent,
) {
    match event {
        AgentEvent::Delta { text } => told.push_str(&text),
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
        AgentEvent::ConfirmationRequired { call_id, subject, kind, can_park, .. } => {
            let answer = ask(context, chat, shared, &kind, &subject, can_park, None).await;
            // False when the turn was cancelled in the meantime and nothing
            // is waiting for this any more -- not worth mentioning.
            context.pending().answer(&call_id, answer);
        }
        _ => {}
    }
}

/// Put a confirmation to the owner and wait for what they say.
///
/// The turn is waiting too, inside its own confirmation gate, so this runs
/// to an answer whatever happens: the owner's, the dispatcher declining on
/// their behalf (`!stop`, a message that was not an answer, the relay
/// stopping), or the timeout.
///
/// `routine` names the routine asking, when it is one rather than a turn the
/// owner started here: the question then says whose it is, and no typing
/// notice is shown around it, because nothing in this chat is typing.
async fn ask(
    context: &Context,
    chat: &ChatId,
    shared: &Mutex<Shared>,
    kind: &str,
    subject: &str,
    can_park: bool,
    routine: Option<&str>,
) -> ConfirmAnswer {
    let mut question = confirmation_question(kind, subject, can_park);
    if let Some(routine) = routine {
        question.markdown = format!("{}{}", from_routine(routine), question.markdown);
    }
    let in_turn = routine.is_none();

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
                answer: Answer::Confirm { can_park, tx: answer },
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
        "I'm {who} from Every Day \u{2014} the same one that lives on your computer, answering \
         from this chat. Ask for what you would ask for there: what's on today, a reply to \
         someone, a task added.\n\n\
         - `!new` starts a new conversation\n\
         - `!stop` stops what I'm doing\n\
         - `!help` shows this message\n\n\
         Before I delete anything, or send something to someone else, I'll ask you here first. \
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
/// are Yes and No, and Later where the call can be saved as a proposal.
pub(crate) fn confirmation_question(kind: &str, subject: &str, can_park: bool) -> Question {
    let subject = subject.trim();
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
            let head = "**Open this address?** Neither you nor a search gave it to me, and opening \
                        it tells that site it was asked for.";
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
        _ => with_quote("**Go ahead with this?**", subject),
    };
    markdown.push_str("\n\n");

    let mut choices = vec![Choice::Yes, Choice::No];
    if can_park {
        choices.push(Choice::Later);
        markdown.push_str(
            "Reply **yes**, **no** or **later** (later saves it for you to decide in the app).",
        );
    } else {
        markdown.push_str("Reply **yes** or **no**.");
    }
    Question { markdown, choices }
}

/// The line a routine's message to the owner starts with, so that a question
/// arriving out of nowhere says whose it is.
fn from_routine(routine: &str) -> String {
    format!("_From your routine **{}**:_\n\n", escape_markdown(&one_line(routine)))
}

/// A routine's own question, as the owner sees it: whose it is, the question
/// in the model's words, and the choices numbered if there are any.
pub(crate) fn free_question(routine: &str, question: &str, choices: &[String]) -> String {
    let mut out = from_routine(routine);
    out.push_str(question.trim());
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

/// Backslash-escape what Markdown would otherwise act on, so that a task
/// called `*urgent* call_back` is shown as written rather than as emphasis.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What an answer means to the turn that asked.
///
/// "Later" without a proposal form to park into is not on offer, so it is
/// read as the one safe thing: no.
pub(crate) fn answer_for(choice: Choice, can_park: bool) -> ConfirmAnswer {
    match choice {
        Choice::Yes => ConfirmAnswer::Confirm,
        Choice::No => ConfirmAnswer::Decline,
        Choice::Later if can_park => ConfirmAnswer::Later,
        Choice::Later => ConfirmAnswer::Decline,
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
    use everyday_service::error::CommandError;

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
        Incoming::Said { chat: chat(), text: text.into(), at: now() }
    }

    fn said_at(text: &str, at: Timestamp) -> Incoming {
        Incoming::Said { chat: chat(), text: text.into(), at }
    }

    fn chose(question: &str, choice: Choice) -> Incoming {
        Incoming::Chose { chat: chat(), question: MessageRef(question.into()), choice, at: now() }
    }

    fn routed(item: &Incoming, asked: Asked<'_>) -> Route {
        route(item, asked, now(), STALE_AFTER)
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

    fn names(pictures: &[Picture]) -> Vec<&str> {
        pictures.iter().map(|p| p.filename.as_str()).collect()
    }

    #[test]
    fn a_picture_with_nothing_said_waits_for_what_is_said_next() {
        let mut held = Vec::new();
        assert!(matches!(gather(&mut held, show("one.jpg", "")), Gathered::Hold));
        assert!(matches!(gather(&mut held, show("two.jpg", "")), Gathered::Hold));
        match gather(&mut held, owner(Work::Say("which is ripe?".into()))) {
            Gathered::Run(Job::Owner(Work::Say(text)), pictures) => {
                assert_eq!(text, "which is ripe?");
                assert_eq!(names(&pictures), ["one.jpg", "two.jpg"]);
            }
            _ => panic!("the words should take the pictures with them"),
        }
        assert!(held.is_empty());
    }

    #[test]
    fn a_captioned_picture_goes_at_once_with_any_held_before_it() {
        let mut held = vec![picture("one.jpg")];
        match gather(&mut held, show("two.jpg", "compare these")) {
            Gathered::Run(Job::Owner(Work::Say(text)), pictures) => {
                assert_eq!(text, "compare these");
                assert_eq!(names(&pictures), ["one.jpg", "two.jpg"], "in the order sent");
            }
            _ => panic!("a caption is something said"),
        }
        assert!(held.is_empty());
    }

    #[test]
    fn a_command_while_pictures_wait_sends_them_first() {
        let mut held = vec![picture("one.jpg")];
        match gather(&mut held, owner(Work::Help)) {
            Gathered::First(pictures, Job::Owner(Work::Help)) => {
                assert_eq!(names(&pictures), ["one.jpg"]);
            }
            _ => panic!("the pictures go first, on their own"),
        }
        assert!(held.is_empty());
        assert!(matches!(gather(&mut held, owner(Work::New)), Gathered::Run(_, p) if p.is_empty()));
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
            Route::Queue(Work::Say("what's on today?".into()))
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
            Route::Queue(Work::Say("!remind me at five".into()))
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
        assert_eq!(routed(&said("yes"), Asked::Nothing), Route::Queue(Work::Say("yes".into())));
    }

    #[test]
    fn other_text_declines_the_question_then_runs_as_a_new_message() {
        let question = MessageRef("$q".into());
        assert_eq!(
            routed(&said("actually, what's on tomorrow?"), Asked::Sent(&question)),
            Route::DeclineThen(Work::Say("actually, what's on tomorrow?".into()))
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
        Ok(Turned { text: text.into(), steps: 0, wrote: Vec::new(), stopped })
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
        let q = confirmation_question("destructive", "Book the dentist", false);
        assert!(q.markdown.contains("**Delete Book the dentist?**"), "{}", q.markdown);
        assert!(q.markdown.contains("can't be undone"));
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
    }

    #[test]
    fn a_delete_nobody_could_name_still_asks() {
        let q = confirmation_question("destructive", "", false);
        assert!(q.markdown.contains("can't say what"), "{}", q.markdown);
    }

    #[test]
    fn a_parkable_call_offers_later() {
        let q = confirmation_question("destructive", "Book the dentist", true);
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No, Choice::Later]);
        assert!(q.markdown.contains("**later**"));
    }

    #[test]
    fn something_outward_is_quoted_and_says_who_it_reaches() {
        let q = confirmation_question(
            "outward",
            "to alice@example.com \u{2014} Re: dinner\nSee you at eight",
            true,
        );
        assert!(q.markdown.contains("outside your vault"));
        assert!(q.markdown.contains("> to alice@example.com \u{2014} Re: dinner"));
        assert!(q.markdown.contains("> See you at eight"));
    }

    #[test]
    fn a_search_after_reading_mail_says_why_it_is_asking() {
        let q = confirmation_question("search", "quarterly numbers acme", false);
        assert!(q.markdown.contains("read your mail"));
        assert!(q.markdown.contains("> quarterly numbers acme"));
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
    }

    #[test]
    fn an_address_is_shown_as_code_so_it_cannot_be_tapped_by_accident() {
        let q = confirmation_question("fetch", "https://example.com/?q=a`b", false);
        assert!(q.markdown.contains("`https://example.com/?q=a'b`"), "{}", q.markdown);
        assert!(q.markdown.contains("Open this address"));

        let q = confirmation_question("routine", "https://evil.example/x/ZGVhcg", true);
        assert!(q.markdown.contains("`https://evil.example/x/ZGVhcg`"), "{}", q.markdown);
        assert!(q.markdown.contains("Save a routine"), "{}", q.markdown);
    }

    #[test]
    fn typing_into_a_page_shows_the_text_the_field_and_the_site() {
        let q = confirmation_question(
            "type",
            "\u{201c}red shoes\u{201d} into the field \u{201c}Search\u{201d} on shop.example",
            false,
        );
        assert!(q.markdown.contains("Type this into a web page"), "{}", q.markdown);
        assert!(q.markdown.contains("on shop.example"), "{}", q.markdown);
        assert_eq!(q.choices, vec![Choice::Yes, Choice::No]);
        let q =
            confirmation_question("click", "the button \u{201c}Next\u{201d} on a.example", false);
        assert!(q.markdown.contains("read your mail"), "{}", q.markdown);
    }

    #[test]
    fn an_unknown_kind_still_asks() {
        let q = confirmation_question("something-new", "the thing", false);
        assert!(q.markdown.contains("Go ahead"));
        assert!(q.markdown.contains("> the thing"));
    }

    #[test]
    fn markdown_in_a_name_is_shown_as_written() {
        assert_eq!(
            escape_markdown("*urgent* call_back [now]"),
            "\\*urgent\\* call\\_back \\[now\\]"
        );
        let q = confirmation_question("destructive", "*urgent*", false);
        assert!(q.markdown.contains("\\*urgent\\*"));
    }

    #[test]
    fn answers_map_to_what_the_turn_expects() {
        assert_eq!(answer_for(Choice::Yes, false), ConfirmAnswer::Confirm);
        assert_eq!(answer_for(Choice::No, true), ConfirmAnswer::Decline);
        assert_eq!(answer_for(Choice::Later, true), ConfirmAnswer::Later);
    }

    #[test]
    fn later_without_a_proposal_form_is_read_as_no() {
        assert_eq!(answer_for(Choice::Later, false), ConfirmAnswer::Decline);
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
        let (tx, answered) = oneshot::channel();
        let waiting =
            Waiting { question: None, early: Vec::new(), answer: Answer::Confirm { can_park, tx } };
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
        assert!(text.starts_with("_From your routine **Morning brief**:_\n\nWhich dentist slot?"));
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
}
