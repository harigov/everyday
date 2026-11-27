//! One chat's worker: the turn it runs, the questions it asks and the routines it delivers.

use super::*;

// ---- the worker -------------------------------------------------------------

/// One chat's worker: take what is queued, one item at a time, until the relay
/// ends.
pub(super) async fn worker(
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
pub(super) enum Gathered {
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
pub(super) fn gather(held: &mut Vec<Sent>, item: Job) -> Gathered {
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
        RoutineJob::Confirm { from, kind, subject, can_park, site, mut answer } => {
            // Nobody is waiting for it any more -- it was answered on the
            // computer's panel, or the run that asked it stopped, while it
            // sat in this queue behind other work: it is never put.
            if answer.is_closed() {
                return;
            }
            running(&from);
            let asked = ask(
                context,
                chat,
                shared,
                &kind,
                &subject,
                can_park,
                site.as_deref(),
                Asking::Routine(&from.routine),
            );
            // The same question may be up on the computer's panel too
            // (`jatayu_service::asking`), and whichever is answered first
            // is the answer; and the run that asked may stop waiting for
            // other reasons (cancelled, out of time). Either way the question
            // must not stay open in the chat, where the owner's next message
            // would be taken for its answer. The owner's own answer, there at
            // the same moment, is the one taken.
            let given = tokio::select! {
                biased;
                given = asked => Some(given),
                () = answer.closed() => None,
            };
            shared.lock().unwrap().running = None;
            match given {
                Some(given) => {
                    let _ = answer.send(given);
                }
                None => no_longer_wanted(context, chat, shared).await,
            }
        }
        RoutineJob::Ask { from, question, choices, mut answer } => {
            // As for a yes or no, above.
            if answer.is_closed() {
                return;
            }
            running(&from);
            let asked = ask_free(context, chat, shared, &from.routine, &question, &choices);
            let given = tokio::select! {
                biased;
                given = asked => Some(given),
                () = answer.closed() => None,
            };
            shared.lock().unwrap().running = None;
            match given {
                Some(given) => {
                    let _ = answer.send(given);
                }
                None => no_longer_wanted(context, chat, shared).await,
            }
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

/// What the chat is told when the run that put a question here stops
/// waiting for its answer. The relay is not told why -- the person answered
/// it on the computer's panel, or the run was cancelled, ran out of time or
/// was stopped with the vault -- so it says only what is true of them all.
const NO_LONGER_WANTED: &str = "I don't need an answer to that any more.";

/// A routine's question is no longer wanted: close it here, so the owner's
/// next message is a message again, and say so if it had reached the chat.
/// One still being sent when the run stopped waiting is closed without a
/// word: a line about a question the owner may never have seen says nothing
/// to them.
async fn no_longer_wanted(context: &Context, chat: &ChatId, shared: &Mutex<Shared>) {
    let waiting = shared.lock().unwrap().waiting.take();
    if waiting.is_some_and(|waiting| waiting.question.is_some()) {
        context.tell(chat, NO_LONGER_WANTED).await;
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
pub(super) async fn on_event(
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
pub(super) enum Asking<'a> {
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
pub(super) async fn ask(
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
