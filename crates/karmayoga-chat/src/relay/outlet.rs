//! The outlet a running relay offers the rest of the service, and the record of which background job each posted message came from.

use super::*;

/// The outlet a running relay offers the rest of the service, registered for
/// exactly as long as the relay runs -- dropped with it, however it ends.
pub(super) struct OpenOutlet {
    pub(super) service: Arc<Service>,
    pub(super) link: ChatLinkId,
    pub(super) outlet: Arc<dyn ChatOutlet>,
}

impl OpenOutlet {
    pub(super) fn new(
        service: Arc<Service>,
        link: ChatLinkId,
        outlet: Arc<dyn ChatOutlet>,
    ) -> Self {
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
pub(super) struct RelayOutlet {
    pub(super) connector: Arc<dyn Connector>,
    pub(super) outbound: mpsc::UnboundedSender<(ChatId, RoutineJob)>,
    /// Which job each message sent for one came from, shared with the relay's
    /// own sending: a reply to any of them is looked up in the same list.
    pub(super) posts: JobPosts,
    /// The relay's own stop, for the one thing this sends without a worker.
    pub(super) stop: watch::Receiver<bool>,
}

impl RelayOutlet {
    /// Queue a job for the owner's private chat. `Err` says why it could not
    /// be: there is no such chat yet, or the relay has stopped.
    pub(super) async fn queue(
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
    pub(super) order: VecDeque<MessageRef>,
    pub(super) jobs: HashMap<MessageRef, JobId>,
    pub(super) limit: usize,
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
