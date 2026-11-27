//! The dispatcher: what every worker and the dispatcher share, and the one task that reads the inbox and never waits on anything slow.

use super::*;

/// Why the dispatcher stopped reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum End {
    Stopped,
    InboxClosed,
}

/// What every worker and the dispatcher share, and never changes.
pub(super) struct Context {
    pub(super) service: Arc<Service>,
    pub(super) vault: Arc<Vault>,
    pub(super) link: ChatLink,
    pub(super) connector: Arc<dyn Connector>,
    pub(super) timing: Timing,
    /// The relay's own stop. Read by workers so they do not start new work, or
    /// send replies, once the relay is being stopped.
    pub(super) stop: watch::Receiver<bool>,
    /// What opens a call's voice session and answers its media.
    pub(super) calls: Arc<dyn CallBackend>,
    /// Which call has the relay's one line. Locked for a line at a time, and
    /// only ever after a chat's `Shared`, never before.
    pub(super) line: Mutex<Line>,
    /// Which background job each message sent for one came from: written when
    /// one is sent, read when the owner replies. See [`Posts`].
    pub(super) posts: JobPosts,
}

impl Context {
    pub(super) fn stopping(&self) -> bool {
        *self.stop.borrow()
    }

    pub(super) fn pending(&self) -> Arc<Pending> {
        self.service.pending()
    }

    /// Send `markdown`. A failure is logged and goes no further: a chat
    /// server that is briefly unreachable must not take the relay down with
    /// it, and there is nobody to tell.
    pub(super) async fn tell(&self, chat: &ChatId, markdown: &str) {
        if let Err(e) = self.connector.send(chat, markdown).await {
            tracing::warn!(chat = %chat.0, error = %e, "could not send a message to the chat");
        }
    }

    /// Send a picture kept in Files, as a picture: `Ok(false)` for one that
    /// is no longer a picture in Files, `Err` for one that could not be read
    /// or sent, logged here. The picture is in Files whether or not it
    /// arrives, so a chat turn carries on as it does past [`Self::tell`].
    pub(super) async fn show(&self, chat: &ChatId, file: FileId) -> Result<bool, String> {
        show_picture(&self.vault, &self.connector, chat, file).await
    }

    /// A task that sends the pictures it is handed to `chat`, one after
    /// another in the order they came, so that whoever hands them over -- a
    /// turn's event loop, a call's -- never waits on an upload. Ends once the
    /// sender is dropped and the last has gone.
    pub(super) fn showing(&self, chat: &ChatId) -> (mpsc::UnboundedSender<FileId>, Showing) {
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
    pub(super) async fn typing(&self, chat: &ChatId, on: bool) {
        if let Err(e) = self.connector.typing(chat, on).await {
            tracing::warn!(chat = %chat.0, error = %e, "could not change the typing notice");
        }
    }

    /// What the person named their assistant, or empty. Read when it is
    /// wanted rather than once at the start: it is a setting, and the owner
    /// may have just changed it on the desktop.
    pub(super) async fn assistant_name(&self) -> String {
        let vault = self.vault.clone();
        jatayu_service::blocking(move || Ok(vault.agent_settings()?.name))
            .await
            .map(|name| name.trim().to_string())
            .unwrap_or_default()
    }

    /// Mark a routine run's conversation as one this chat carries on, so that
    /// a relay started afresh -- which remembers nothing -- still finds it for
    /// the owner's reply. Best effort: without it the reply starts a new
    /// conversation, which is where a chat that has forgotten would be anyway.
    pub(super) async fn adopt_conversation(&self, chat: &ChatId, conversation: ConversationId) {
        let vault = self.vault.clone();
        let via = Via {
            link: self.link.id,
            service: self.link.service.label().to_string(),
            chat: chat.0.clone(),
        };
        let marked = jatayu_service::blocking(move || {
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
    pub(super) async fn find_conversation(&self, chat: &ChatId) -> Option<Current> {
        let vault = self.vault.clone();
        let all = jatayu_service::blocking(move || {
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
pub(super) struct Chats {
    pub(super) context: Arc<Context>,
    handles: HashMap<ChatId, ChatHandle>,
    /// The workers. Held in a set rather than detached so that dropping the
    /// relay -- the supervisor aborting it -- aborts them too, instead of
    /// leaving turns running for a link that is gone.
    pub(super) workers: JoinSet<()>,
}

/// How the dispatcher reaches one chat's worker.
struct ChatHandle {
    pub(super) queue: mpsc::UnboundedSender<Job>,
    pub(super) shared: Arc<Mutex<Shared>>,
}

/// One thing for a chat's worker to do, in order: what the owner asked for,
/// or what a routine needs from them.
pub(super) enum Job {
    Owner(Work),
    Routine(RoutineJob),
    /// A call the owner is making. See [`CallWork`].
    Call(CallWork),
}

/// A call, queued: what the worker needs to answer it, and the relay's line,
/// held for as long as this exists.
pub(super) struct CallWork {
    pub(super) offer: CallOffer,
    pub(super) _line: LineHeld<Arc<Context>>,
}

/// The relay's one line, held by one call. Given back when this is dropped,
/// however the call ends -- finished, refused, its worker panicking, or the
/// queue it sat in being thrown away -- so a call that goes wrong cannot
/// leave the line engaged for the rest of the relay's life.
///
/// Over the context however it is held: shared by the dispatcher, borrowed by
/// a worker.
pub(super) struct LineHeld<C: std::ops::Deref<Target = Context>> {
    pub(super) context: C,
    pub(super) call: CallId,
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
    pub(super) fn new(context: Arc<Context>) -> Self {
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
    pub(super) fn dispatch(&mut self, item: Incoming) {
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
    pub(super) fn routine(&mut self, chat: ChatId, job: RoutineJob) {
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
    pub(super) async fn wind_down(self, ended: End) {
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
