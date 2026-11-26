//! A call, from ringing to hanging up.

use super::*;

// ---- a call -------------------------------------------------------------------

/// Hang up `call`, saying why. A failure is logged and goes no further: the
/// call is over from this side whatever the server thinks of it.
pub(super) async fn hang_up(context: &Context, chat: &ChatId, call: &CallId, why: HangUp) {
    if let Err(e) = context.connector.end_call(chat, call, why).await {
        tracing::warn!(chat = %chat.0, error = %e, ?why, "could not hang up a call");
    }
}

/// Refuse a call because another has the line, and say so.
pub(super) async fn refuse_busy(context: &Context, chat: &ChatId, call: &CallId) {
    hang_up(context, chat, call, HangUp::Refused).await;
    context.tell(chat, calling::BUSY).await;
}

/// The owner is calling: answer, carry the call, hang up. The chat's worker
/// does this in turn, so no typed turn runs in this chat until it is over.
pub(super) async fn take_call(
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
    pub(super) conversation: Option<ConversationId>,
    /// What is said when the owner picks up.
    pub(super) spoken: String,
    /// The same, as written: what the chat shows of it, as it shows
    /// everything said on a call.
    pub(super) written: String,
    /// Told that the run's result was given, once the call is under way.
    pub(super) delivered: oneshot::Sender<Result<CallDelivered, String>>,
}

/// How long the owner's devices ring before the call is given up and the
/// result written instead. A little past the ring's own lifetime on their
/// devices, so a pick-up at the last moment is still heard.
pub const RING_FOR: Duration = Duration::from_secs(50);

/// A run's message to the owner, and the chat carried on in the run's
/// conversation from then, as a reply to it would be. A message from a
/// background job is remembered as that job's, so a reply to it is about it.
pub(super) async fn post_for_run(
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
pub(super) fn instead(lead: &str, markdown: &str) -> String {
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
pub(super) async fn call_owner(
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
pub(super) type Showing = tokio::task::JoinHandle<()>;

/// How long a turn's reply waits for its pictures to have gone first: an
/// upload of a few megabytes, or a few of them, to a server that answers.
pub(super) const SHOW_FOR: Duration = Duration::from_secs(60);

/// Send a picture kept in Files to `chat`, as a picture: `Ok(false)` for one
/// that is no longer a picture in Files, `Err` for one that could not be read
/// or sent, logged here. The picture is in Files whether or not it arrives.
pub(super) async fn show_picture(
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
pub(super) enum Written {
    Line(String),
    Picture(FileId),
}

/// What `event` puts into the chat: the transcript's lines, then any picture
/// a tool that finished just made.
pub(super) fn written_of(transcript: &mut Transcript, event: &VoiceEvent) -> Vec<Written> {
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
pub(super) fn withdraw<F>(
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
