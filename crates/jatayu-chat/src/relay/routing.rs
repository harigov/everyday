//! Deciding what an item is for, and which conversation a chat is on.

use super::*;

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
    pub(super) id: ConversationId,
    pub(super) last_used: Timestamp,
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
pub(super) struct Memory {
    pub(super) current: Option<Current>,
    /// Whether the vault has been searched for this chat's conversation yet.
    /// Once is enough: from then on this worker is the only thing creating
    /// them.
    pub(super) looked: bool,
}

impl Memory {
    /// Forget the conversation, for good: `!new`. `looked` is set so the
    /// forgotten one is not simply found again.
    pub(super) fn forget(&mut self) {
        self.current = None;
        self.looked = true;
    }

    pub(super) async fn conversation(
        &mut self,
        context: &Context,
        chat: &ChatId,
    ) -> ConversationId {
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
    pub(super) fn adopt(&mut self, conversation: ConversationId) {
        self.current = Some(Current { id: conversation, last_used: Timestamp::now() });
        self.looked = true;
    }

    /// A turn on `conversation` has just ended.
    pub(super) fn used(&mut self, conversation: ConversationId) {
        if let Some(current) = &mut self.current
            && current.id == conversation
        {
            current.last_used = Timestamp::now();
        }
    }
}
