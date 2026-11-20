//! A call from the owner, carried by the relay: answered in the chat's turn,
//! talked through a voice session on the chat's conversation, and hung up.
//! See `docs/plans/calls.md`.
//!
//! The relay (`crate::relay`) owns the doing: a call is a piece of work on its
//! chat's worker, as a message is, so a chat never has a call and a typed turn
//! at once. This module owns what that work is made of, and what can be
//! decided without a runtime:
//!
//! - [`Line`]: the relay answers one call at a time, whichever chat it is
//!   in. A second is refused at once rather than queued -- a phone ringing
//!   for a minute while the owner is already talking to the assistant helps
//!   nobody.
//! - [`CallSlot`]: what the dispatcher keeps in a chat's `Shared` about its
//!   call, so a key or a hang-up that arrives while the worker is busy
//!   finds it: buffered until the worker has started the call, forwarded to
//!   the running call after.
//! - [`Confirming`]: when a confirmation the voice session cannot take by
//!   voice is to be put to the owner in the chat, and when it is no longer
//!   wanted.
//! - [`CallBackend`]: opening the voice session and joining the media server
//!   need speech models and real sockets, so a test substitutes them. The
//!   real thing is [`RealCalls`].
//! - [`Transcript`]: what is said on the call, written into the chat as it
//!   goes, as the thread at the desk shows a conversation being talked.
//! - What is said to the owner, and which [`HangUp`] each [`CallEnd`] is.

use crate::call::{self, Addresses, CallEnd, CallError, ToCall};
use crate::connector::{CallId, CallJoin, CallKey, HangUp};
use futures::future::BoxFuture;
use jiff::Timestamp;
use karmayoga_core::ConversationId;
use karmayoga_core::chat::Via;
use karmayoga_service::agent::AgentEvent;
use karmayoga_service::chat::escape_markdown;
use karmayoga_service::error::CommandResult;
use karmayoga_service::service::Service;
use karmayoga_service::voice::{VoiceEvent, VoiceLink, VoiceState};
use std::sync::Arc;
use tokio::sync::mpsc;

// ---- the seam ----------------------------------------------------------------

/// What a call needs that touches speech models and sockets, so that a test
/// can stand something else in its place.
///
/// [`RealCalls`] is the default. The only other implementations are tests'.
pub trait CallBackend: Send + Sync + 'static {
    /// Open a voice session on `conversation`, whose turns run as turns from
    /// the chat `via` -- [`Service::voice_open_via`]. The error is a sentence
    /// fit for the owner: the speech model or voice is not chosen, the vault
    /// is locked.
    fn open_voice(
        &self,
        service: Arc<Service>,
        conversation: ConversationId,
        via: Via,
    ) -> BoxFuture<'static, CommandResult<VoiceLink>>;

    /// Join the call's room on its media server, as `join` says, with what
    /// has arrived so far of the caller's `keys`.
    fn connect(
        &self,
        join: CallJoin,
        keys: Vec<CallKey>,
    ) -> BoxFuture<'static, Result<Box<dyn Media>, CallError>>;
}

/// One joined call, not yet running. [`call::Call`] is the real one.
pub trait Media: Send + 'static {
    /// Carry the call until it ends. As [`call::Call::run`] says: the
    /// caller's audio to `voice` and what it says back, the session's events
    /// to `events`, and the session told to stop however the call ends unless
    /// it ended by itself.
    fn run(
        self: Box<Self>,
        voice: VoiceLink,
        to_call: mpsc::UnboundedReceiver<ToCall>,
        events: mpsc::UnboundedSender<VoiceEvent>,
    ) -> BoxFuture<'static, CallEnd>;
}

impl Media for call::Call {
    fn run(
        self: Box<Self>,
        voice: VoiceLink,
        to_call: mpsc::UnboundedReceiver<ToCall>,
        events: mpsc::UnboundedSender<VoiceEvent>,
    ) -> BoxFuture<'static, CallEnd> {
        Box::pin((*self).run(voice, to_call, events))
    }
}

/// The real thing: a voice session from the service, and the media server
/// over this machine's own interfaces.
pub struct RealCalls;

impl CallBackend for RealCalls {
    fn open_voice(
        &self,
        service: Arc<Service>,
        conversation: ConversationId,
        via: Via,
    ) -> BoxFuture<'static, CommandResult<VoiceLink>> {
        Box::pin(async move { service.voice_open_via(conversation, Some(via)).await })
    }

    fn connect(
        &self,
        join: CallJoin,
        keys: Vec<CallKey>,
    ) -> BoxFuture<'static, Result<Box<dyn Media>, CallError>> {
        Box::pin(async move {
            let call = call::connect(&join, keys, Addresses::Interfaces).await?;
            Ok(Box::new(call) as Box<dyn Media>)
        })
    }
}

// ---- one call at a time ------------------------------------------------------

/// The relay's one telephone line: which call, if any, is taking it.
///
/// A call holds it from the moment it is offered. While it is still ringing --
/// queued behind a turn, not yet taken up by its worker -- it holds it only
/// for as long as it could still be answered: the owner hanging up, or the
/// call stopping ringing, frees the line at once, so that a redial is not
/// refused as busy by a call nobody is making any more.
#[derive(Debug, Default)]
pub(crate) struct Line {
    held: Option<Held>,
}

#[derive(Debug)]
struct Held {
    call: CallId,
    /// When it stops ringing, while it is still ringing; `None` once the
    /// worker has taken it up.
    rings_until: Option<Timestamp>,
}

/// What [`Line::offer`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offered {
    /// Free: the call has the line now.
    Taken,
    /// This very call is the one that has it: the service said it twice.
    /// Neither refused nor started again -- refusing would hang up the
    /// call that is being answered.
    Again,
    /// Another call has it.
    Busy,
}

impl Line {
    /// `call`, which stops ringing at `expires`, is offered at `now`.
    pub(crate) fn offer(&mut self, call: &CallId, expires: Timestamp, now: Timestamp) -> Offered {
        let free = match &self.held {
            None => true,
            Some(held) if held.call == *call => return Offered::Again,
            // Rang out while it waited: nobody can answer it any more, and
            // the worker will refuse it when it gets there.
            Some(Held { rings_until: Some(until), .. }) => has_expired(*until, now),
            Some(_) => false,
        };
        if !free {
            return Offered::Busy;
        }
        self.held = Some(Held { call: call.clone(), rings_until: Some(expires) });
        Offered::Taken
    }

    /// The worker has taken `call` up, so it holds the line until it is over,
    /// however long it has been ringing.
    pub(crate) fn started(&mut self, call: &CallId) {
        if let Some(held) = self.held.as_mut().filter(|held| held.call == *call) {
            held.rings_until = None;
        }
    }

    /// `call` was ended while still ringing -- the owner hung up: free the
    /// line if it is the one that has it. A call already taken up keeps it
    /// until the worker is done with it.
    pub(crate) fn give_up(&mut self, call: &CallId) {
        if self.held.as_ref().is_some_and(|h| h.call == *call && h.rings_until.is_some()) {
            self.held = None;
        }
    }

    /// Whether a call has the line at `now`: one running, or one still
    /// ringing that could yet be answered.
    pub(crate) fn is_busy(&self, now: Timestamp) -> bool {
        match &self.held {
            None => false,
            Some(Held { rings_until: Some(until), .. }) => !has_expired(*until, now),
            Some(_) => true,
        }
    }

    /// `call` is over. Only the call that holds the line can free it.
    pub(crate) fn release(&mut self, call: &CallId) {
        if self.held.as_ref().is_some_and(|held| held.call == *call) {
            self.held = None;
        }
    }
}

// ---- what the dispatcher knows of a call -------------------------------------

/// A call as the dispatcher queues it for its chat's worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallOffer {
    pub call: CallId,
    /// When the call stops ringing: answering later than this is answering
    /// nothing.
    pub expires: Timestamp,
}

/// What a chat's `Shared` knows of its call, from the moment it is queued to
/// the moment the worker has finished with it.
pub(crate) struct CallSlot {
    pub(crate) id: CallId,
    stage: Stage,
    /// Whether the call is over as far as the owner is concerned: they hung
    /// up, `!stop` was typed, or the relay is stopping.
    ended: bool,
}

enum Stage {
    /// Queued behind whatever the worker is doing. The caller's keys so far,
    /// to be handed to the media when it connects.
    Queued { keys: Vec<CallKey> },
    /// The worker has taken it up: later keys, and the end, go down this.
    Started { to_call: mpsc::UnboundedSender<ToCall> },
}

impl CallSlot {
    pub(crate) fn queued(id: CallId) -> Self {
        Self { id, stage: Stage::Queued { keys: Vec::new() }, ended: false }
    }

    /// More of the caller's keys, for this call: buffered while it waits to
    /// start, forwarded once it has.
    pub(crate) fn heard(&mut self, keys: Vec<CallKey>) {
        if self.ended || keys.is_empty() {
            return;
        }
        match &mut self.stage {
            Stage::Queued { keys: held } => held.extend(keys),
            // The call ending an instant ago closes the channel; nothing
            // depends on what it would have been told.
            Stage::Started { to_call } => {
                let _ = to_call.send(ToCall::Keys(keys));
            }
        }
    }

    /// The call is over: tell it if it is running, and make sure the worker
    /// does not start it if it is not. Whether that was news.
    pub(crate) fn end(&mut self) -> bool {
        let news = !self.ended;
        self.ended = true;
        if let Stage::Started { to_call } = &self.stage {
            let _ = to_call.send(ToCall::End);
        }
        news
    }

    pub(crate) fn is_ended(&self) -> bool {
        self.ended
    }

    /// Whether the worker has taken the call up, as opposed to it waiting its
    /// turn. `!stop` ends a call that has; one still waiting is not what the
    /// owner meant to stop, which is the turn it is waiting behind.
    pub(crate) fn is_started(&self) -> bool {
        matches!(self.stage, Stage::Started { .. })
    }

    /// The worker takes the call up: what was buffered, and the channel the
    /// call will be told the rest down. `None` if the call ended while it
    /// waited, in which case it is not to be answered.
    ///
    /// The channel exists from here, before the media does, so that a key
    /// that arrives while the call is being joined is not lost: the media is
    /// given what was buffered, and the call gets the rest.
    pub(crate) fn start(&mut self) -> Option<(Vec<CallKey>, mpsc::UnboundedReceiver<ToCall>)> {
        if self.ended {
            return None;
        }
        let Stage::Queued { keys } = &mut self.stage else { return None };
        let heard = std::mem::take(keys);
        let (to_call, rx) = mpsc::unbounded_channel();
        self.stage = Stage::Started { to_call };
        Some((heard, rx))
    }
}

/// Whether a call that stopped ringing at `expires` can still be answered at
/// `now`.
pub(crate) fn has_expired(expires: Timestamp, now: Timestamp) -> bool {
    now >= expires
}

// ---- a confirmation only a tap can answer -----------------------------------

/// A confirmation the assistant is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Confirmation {
    pub call_id: String,
    pub subject: String,
    pub kind: String,
    pub can_park: bool,
}

/// A question put in the chat that is not wanted any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withdraw {
    /// The confirmation it was about.
    pub call_id: String,
    /// Whether the voice session has settled that confirmation itself -- its
    /// tool started or finished -- so that nothing is waiting for an answer.
    /// When not, the turn may still be waiting on it, and whoever withdraws
    /// the question must answer it, or it waits for ever.
    pub settled: bool,
}

/// What to do about the chat's question, after one more event from the
/// session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Step {
    /// The question that was put is not wanted any more.
    pub withdraw: Option<Withdraw>,
    /// Put this to the owner in the chat.
    pub ask: Option<Confirmation>,
}

/// Follows the session's events to decide when the chat is asked.
///
/// A voice session answers some confirmations by voice itself and says so;
/// the rest it cannot, and tells the owner "That needs your go-ahead in our
/// chat" and reports [`VoiceState::WaitingForClick`]. Only those are asked in
/// the chat: a question there that the owner has just answered aloud would be
/// a second, stale yes-or-no under the call.
///
/// The session reports the state only when it changes, so a second
/// confirmation that arrives while the first is still a card is asked on its
/// own arrival. A state that moves away (the owner is talking, say) and back
/// does not ask again: the question is still there in the chat.
#[derive(Debug, Default)]
pub(crate) struct Confirming {
    /// The latest confirmation the turn is waiting on, until it is resolved.
    waiting: Option<Confirmation>,
    /// Whether the session's last reported state was
    /// [`VoiceState::WaitingForClick`].
    clicking: bool,
    /// The one that has been put to the owner.
    asked: Option<String>,
}

impl Confirming {
    pub(crate) fn on_event(&mut self, event: &VoiceEvent) -> Step {
        match event {
            VoiceEvent::Agent {
                event: AgentEvent::ConfirmationRequired { call_id, subject, kind, can_park, .. },
            } => {
                let confirmation = Confirmation {
                    call_id: call_id.clone(),
                    subject: subject.clone(),
                    kind: kind.clone(),
                    can_park: *can_park,
                };
                self.waiting = Some(confirmation);
                if self.clicking {
                    // The state will not be reported again: this is the
                    // moment, and an earlier question is out of date -- and
                    // its confirmation unanswered.
                    let withdraw =
                        self.asked.take().map(|call_id| Withdraw { call_id, settled: false });
                    return Step { withdraw, ask: self.ask() };
                }
                Step::default()
            }
            VoiceEvent::State { state: VoiceState::WaitingForClick } => {
                self.clicking = true;
                Step { withdraw: None, ask: self.ask() }
            }
            VoiceEvent::State { .. } => {
                self.clicking = false;
                Step::default()
            }
            VoiceEvent::Agent {
                event:
                    AgentEvent::ToolStarted { call_id, .. } | AgentEvent::ToolFinished { call_id, .. },
            } => self.resolved(Some(call_id)),
            // The turn is over, or the session is: whatever it was waiting on
            // was forgotten with it, but the session did not say it settled
            // it, so the caller declines it anyway, which is harmless.
            VoiceEvent::Agent {
                event: AgentEvent::Finished { .. } | AgentEvent::Failed { .. },
            }
            | VoiceEvent::Ended { .. } => self.resolved(None),
            _ => Step::default(),
        }
    }

    /// The question to put now, if there is one that has not been.
    fn ask(&mut self) -> Option<Confirmation> {
        let waiting = self.waiting.as_ref()?;
        if self.asked.as_deref() == Some(waiting.call_id.as_str()) {
            return None;
        }
        self.asked = Some(waiting.call_id.clone());
        Some(waiting.clone())
    }

    /// `call_id` -- or, for `None`, whatever there was -- is resolved. Settled
    /// by the session only when it says so of that very call.
    fn resolved(&mut self, call_id: Option<&str>) -> Step {
        let ours = |id: &str| call_id.is_none_or(|call_id| call_id == id);
        if self.waiting.as_ref().is_some_and(|w| ours(&w.call_id)) {
            self.waiting = None;
        }
        let settled = call_id.is_some();
        let withdraw =
            self.asked.take_if(|asked| ours(asked)).map(|call_id| Withdraw { call_id, settled });
        Step { withdraw, ask: None }
    }
}

// ---- the call, written down -------------------------------------------------

/// Follows the session's events to write the call into the chat as it goes:
/// each thing the owner said, quoted as it was heard, and each reply once it
/// is settled -- the same words the thread at the desk shows for a
/// conversation being talked, and the vault keeps.
///
/// A reply is settled when what was heard of it is known: it has been played
/// out (`ReplyHeard`, before anything read aloud after it), it was cut short
/// where the owner talked over it (`ReplyCut`, which is what the vault keeps
/// of it -- nothing, when nothing was heard), the owner has moved on to the
/// next thing, or the call is over. Written whole and once, since a message in the chat cannot
/// be taken back the way the desk redraws one.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    reply: Option<Reply>,
}

#[derive(Debug, Default)]
struct Reply {
    message_id: Option<String>,
    text: String,
    /// The turn is over: nothing more of it is coming.
    finished: bool,
    /// It failed, saying this.
    failed: Option<String>,
}

impl Transcript {
    /// What to write in the chat after `event`, in order.
    pub(crate) fn on_event(&mut self, event: &VoiceEvent) -> Vec<String> {
        match event {
            VoiceEvent::Heard { text } => {
                let mut out = self.settle();
                if !text.trim().is_empty() {
                    out.push(said_by_owner(text));
                }
                self.reply = Some(Reply::default());
                out
            }
            VoiceEvent::Agent { event } => {
                let reply = self.reply.get_or_insert_with(Reply::default);
                match event {
                    AgentEvent::Started { message_id } => {
                        reply.message_id.get_or_insert_with(|| message_id.clone());
                    }
                    AgentEvent::Delta { text } => reply.text.push_str(text),
                    AgentEvent::Retrying { text } => reply.text.clone_from(text),
                    AgentEvent::Finished { .. } => reply.finished = true,
                    AgentEvent::Failed { message } => {
                        reply.finished = true;
                        reply.failed = Some(message.clone());
                    }
                    _ => {}
                }
                Vec::new()
            }
            VoiceEvent::ReplyCut { message_id, text } => {
                match self.reply.as_mut() {
                    Some(reply) if reply.message_id.as_deref() == Some(message_id.as_str()) => {
                        reply.text.clone_from(text);
                        reply.failed = None;
                    }
                    _ => return Vec::new(),
                }
                self.settle()
            }
            VoiceEvent::ReplyHeard { message_id } => {
                let ours = self
                    .reply
                    .as_ref()
                    .is_some_and(|reply| reply.message_id.as_deref() == Some(message_id.as_str()));
                if ours { self.settle() } else { Vec::new() }
            }
            VoiceEvent::State { state: VoiceState::Listening } => {
                if self.reply.as_ref().is_some_and(|reply| reply.finished) {
                    self.settle()
                } else {
                    Vec::new()
                }
            }
            VoiceEvent::Ended { .. } => self.settle(),
            _ => Vec::new(),
        }
    }

    /// What is left to write once the call is over.
    pub(crate) fn finish(&mut self) -> Vec<String> {
        self.settle()
    }

    /// The reply under way, written down, if there is anything to write.
    fn settle(&mut self) -> Vec<String> {
        let Some(reply) = self.reply.take() else { return Vec::new() };
        let text = reply.text.trim();
        if !text.is_empty() {
            return vec![text.to_string()];
        }
        match reply.failed {
            Some(why) => vec![format!("Something went wrong: {}", why.trim())],
            None => Vec::new(),
        }
    }
}

/// What the owner said on a call, as the chat shows it: quoted, since the
/// bot is the one writing it, behind a microphone.
pub(crate) fn said_by_owner(text: &str) -> String {
    let lines: Vec<String> = text
        .trim()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| escape_markdown(line.trim()))
        .collect();
    format!("> \u{1f399}\u{fe0f} {}", lines.join("\n> "))
}

// ---- what is said ------------------------------------------------------------

/// Said in the chat, and the call refused, when another call has the line.
pub(crate) const BUSY: &str = "I'm already on a call.";

/// Said in the chat when a call waited so long behind other work that it had
/// stopped ringing by the time it could be answered.
pub(crate) const MISSED: &str =
    "I couldn't pick up in time \u{2014} I was busy. Call again when you're ready.";

/// Said when the voice session would not open. `why` is the service's own
/// sentence about what to set up.
pub(crate) fn cannot_take_calls(why: &str) -> String {
    format!("I can't take calls yet: {}", why.trim())
}

/// Said when the call's media server could not be joined.
pub(crate) fn could_not_answer(why: &str) -> String {
    format!("I couldn't answer that call: {}", why.trim())
}

/// Said when the chat service would not let this side join the call.
pub(crate) fn could_not_join(why: &str) -> String {
    format!(
        "I couldn't answer that call: the chat server wouldn't let me join it ({}).",
        why.trim()
    )
}

/// How a call that ended is hung up.
///
/// The owner hanging up first, or a goodbye, are both finished: ending a call
/// that has ended is harmless, so there is no reason to tell the two apart.
pub(crate) fn hang_up_for(end: CallEnd) -> HangUp {
    match end {
        CallEnd::SessionEnded | CallEnd::Ended => HangUp::Finished,
        CallEnd::Unreachable | CallEnd::Lost => HangUp::Unreachable,
        CallEnd::Failed => HangUp::Failed,
    }
}

/// How a call ended, given what its voice session said as it went.
///
/// A session that ended with an error ends the call, and the media reports
/// that as the session having ended, which on its own reads as a goodbye. It
/// was not one: the call failed. If the owner hung up, or the relay ended the
/// call, that is what it was, whatever the session said while it was being
/// stopped.
pub(crate) fn settle_end(end: CallEnd, session_error: Option<&str>) -> CallEnd {
    match (end, session_error) {
        (CallEnd::SessionEnded, Some(_)) => CallEnd::Failed,
        _ => end,
    }
}

/// Said when the voice session ended in an error.
pub(crate) fn session_failed(error: &str) -> String {
    format!("That call ended because something went wrong on my side: {}", error.trim())
}

/// What the owner is told when a call ended badly, if anything is to be said.
/// A call that ended as calls do is not followed by a message.
pub(crate) fn after_call(end: CallEnd) -> Option<&'static str> {
    match end {
        CallEnd::SessionEnded | CallEnd::Ended => None,
        CallEnd::Unreachable => Some(
            "That call couldn't connect. Your phone has to be able to reach this computer \
             directly \u{2014} over your tailnet, for example \u{2014} because calls don't go through a \
             relay server.",
        ),
        CallEnd::Lost => Some(
            "We lost each other on that call. Your phone has to keep a direct route to this \
             computer \u{2014} over your tailnet, for example \u{2014} because calls don't go through a \
             relay server.",
        ),
        CallEnd::Failed => Some("That call broke off because something went wrong on my side."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> CallId {
        CallId(s.into())
    }

    fn key(index: u8) -> CallKey {
        CallKey { participant: "@owner:hs:PHONE".into(), index, key: vec![index; 16] }
    }

    // ---- the line ----

    fn t(seconds: i64) -> Timestamp {
        Timestamp::from_second(2_000_000_000 + seconds).unwrap()
    }

    #[test]
    fn a_free_line_is_taken_and_a_second_call_finds_it_busy() {
        let mut line = Line::default();
        assert_eq!(line.offer(&id("a"), t(60), t(0)), Offered::Taken);
        assert_eq!(line.offer(&id("b"), t(60), t(1)), Offered::Busy);
    }

    #[test]
    fn the_line_is_busy_while_a_call_has_it_or_could_still_be_answered() {
        let mut line = Line::default();
        assert!(!line.is_busy(t(0)));
        line.offer(&id("a"), t(60), t(0));
        assert!(line.is_busy(t(30)), "ringing");
        assert!(!line.is_busy(t(60)), "rang out");
        line.started(&id("a"));
        assert!(line.is_busy(t(600)), "taken up, however long ago it rang");
        line.release(&id("a"));
        assert!(!line.is_busy(t(600)));
    }

    #[test]
    fn the_same_call_offered_twice_is_not_refused() {
        let mut line = Line::default();
        line.offer(&id("a"), t(60), t(0));
        assert_eq!(line.offer(&id("a"), t(60), t(1)), Offered::Again);
    }

    #[test]
    fn the_line_is_free_again_only_when_its_own_call_is_over() {
        let mut line = Line::default();
        line.offer(&id("a"), t(60), t(0));
        line.release(&id("b"));
        assert_eq!(
            line.offer(&id("c"), t(60), t(1)),
            Offered::Busy,
            "another call's end frees nothing"
        );
        line.release(&id("a"));
        assert_eq!(line.offer(&id("c"), t(60), t(2)), Offered::Taken);
    }

    #[test]
    fn a_call_hung_up_while_it_rings_frees_the_line_for_a_redial() {
        let mut line = Line::default();
        line.offer(&id("a"), t(60), t(0));
        line.give_up(&id("b"));
        assert_eq!(line.offer(&id("c"), t(60), t(1)), Offered::Busy, "not its call");
        line.give_up(&id("a"));
        assert_eq!(line.offer(&id("c"), t(60), t(2)), Offered::Taken);
    }

    #[test]
    fn a_call_that_rang_out_while_it_waited_no_longer_holds_the_line() {
        let mut line = Line::default();
        line.offer(&id("a"), t(60), t(0));
        assert_eq!(line.offer(&id("b"), t(120), t(59)), Offered::Busy, "still ringing");
        assert_eq!(line.offer(&id("b"), t(120), t(60)), Offered::Taken, "rang out");
    }

    #[test]
    fn a_call_being_carried_holds_the_line_however_long_ago_it_rang() {
        let mut line = Line::default();
        line.offer(&id("a"), t(60), t(0));
        line.started(&id("a"));
        assert_eq!(line.offer(&id("b"), t(900), t(600)), Offered::Busy);
        line.give_up(&id("a"));
        assert_eq!(
            line.offer(&id("b"), t(900), t(601)),
            Offered::Busy,
            "a hang-up is not news now"
        );
    }

    // ---- the slot ----

    #[test]
    fn keys_are_buffered_until_the_call_starts_and_forwarded_after() {
        let mut slot = CallSlot::queued(id("a"));
        slot.heard(vec![key(1)]);
        slot.heard(vec![key(2)]);
        let (heard, mut rx) = slot.start().expect("it was not ended");
        assert_eq!(heard, vec![key(1), key(2)]);

        slot.heard(vec![key(3)]);
        assert_eq!(rx.try_recv().unwrap(), ToCall::Keys(vec![key(3)]));
        assert!(rx.try_recv().is_err(), "and nothing is sent twice");
    }

    #[test]
    fn a_call_ended_while_it_waits_is_not_started() {
        let mut slot = CallSlot::queued(id("a"));
        assert!(slot.end(), "the first hang-up is news");
        assert!(!slot.end(), "the second is not");
        assert!(slot.start().is_none());
    }

    #[test]
    fn ending_a_running_call_tells_it() {
        let mut slot = CallSlot::queued(id("a"));
        let (_, mut rx) = slot.start().unwrap();
        slot.end();
        assert_eq!(rx.try_recv().unwrap(), ToCall::End);
        assert!(slot.is_ended());
    }

    #[test]
    fn keys_after_the_end_are_dropped() {
        let mut slot = CallSlot::queued(id("a"));
        let (_, mut rx) = slot.start().unwrap();
        slot.end();
        slot.heard(vec![key(9)]);
        assert_eq!(rx.try_recv().unwrap(), ToCall::End);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn an_empty_batch_of_keys_says_nothing() {
        let mut slot = CallSlot::queued(id("a"));
        let (_, mut rx) = slot.start().unwrap();
        slot.heard(Vec::new());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_call_stops_ringing_at_its_expiry() {
        let at = Timestamp::from_second(2_000_000_000).unwrap();
        assert!(!has_expired(at, at - jiff::SignedDuration::from_secs(1)));
        assert!(has_expired(at, at));
        assert!(has_expired(at, at + jiff::SignedDuration::from_secs(1)));
    }

    // ---- hanging up ----

    #[test]
    fn every_way_a_call_ends_is_hung_up_with_the_reason_that_fits() {
        assert_eq!(hang_up_for(CallEnd::SessionEnded), HangUp::Finished);
        assert_eq!(hang_up_for(CallEnd::Ended), HangUp::Finished);
        assert_eq!(hang_up_for(CallEnd::Unreachable), HangUp::Unreachable);
        assert_eq!(hang_up_for(CallEnd::Lost), HangUp::Unreachable);
        assert_eq!(hang_up_for(CallEnd::Failed), HangUp::Failed);
    }

    #[test]
    fn only_a_call_that_ended_badly_is_followed_by_a_message() {
        assert!(after_call(CallEnd::SessionEnded).is_none());
        assert!(after_call(CallEnd::Ended).is_none());
        let unreachable = after_call(CallEnd::Unreachable).unwrap();
        assert!(unreachable.contains("tailnet"), "{unreachable}");
        assert!(unreachable.contains("couldn't connect"), "{unreachable}");
        assert!(after_call(CallEnd::Lost).unwrap().contains("tailnet"));
        assert!(after_call(CallEnd::Failed).is_some());
    }

    #[test]
    fn a_refusal_carries_the_services_own_sentence() {
        assert_eq!(
            cannot_take_calls(" Choose a speech model in Settings. "),
            "I can't take calls yet: Choose a speech model in Settings."
        );
    }

    #[test]
    fn a_session_that_ended_in_error_failed_the_call_whatever_the_media_thought() {
        assert_eq!(settle_end(CallEnd::SessionEnded, Some("the model broke")), CallEnd::Failed);
        assert_eq!(settle_end(CallEnd::SessionEnded, None), CallEnd::SessionEnded);
        assert_eq!(
            settle_end(CallEnd::Ended, Some("stopped mid-sentence")),
            CallEnd::Ended,
            "the owner hanging up is a hang-up"
        );
        assert_eq!(settle_end(CallEnd::Unreachable, Some("x")), CallEnd::Unreachable);
        assert!(session_failed(" the model broke ").ends_with("the model broke"));
    }

    // ---- confirmations ----

    fn requires(call_id: &str) -> VoiceEvent {
        VoiceEvent::Agent {
            event: AgentEvent::ConfirmationRequired {
                call_id: call_id.into(),
                name: "delete_task".into(),
                subject: "Book the dentist".into(),
                arguments: serde_json::Value::Null,
                kind: "destructive".into(),
                site: None,
                can_park: true,
            },
        }
    }

    fn state(state: VoiceState) -> VoiceEvent {
        VoiceEvent::State { state }
    }

    fn started(call_id: &str) -> VoiceEvent {
        VoiceEvent::Agent {
            event: AgentEvent::ToolStarted {
                call_id: call_id.into(),
                name: "delete_task".into(),
                arguments: serde_json::Value::Null,
            },
        }
    }

    fn asked(step: &Step) -> Option<&str> {
        step.ask.as_ref().map(|c| c.call_id.as_str())
    }

    #[test]
    fn a_confirmation_that_waits_for_a_click_is_asked_in_the_chat() {
        let mut c = Confirming::default();
        assert_eq!(c.on_event(&requires("c1")), Step::default(), "not until it is a click");
        let step = c.on_event(&state(VoiceState::WaitingForClick));
        assert_eq!(asked(&step), Some("c1"));
        let confirmation = step.ask.unwrap();
        assert_eq!(confirmation.subject, "Book the dentist");
        assert_eq!(confirmation.kind, "destructive");
        assert!(confirmation.can_park);
        assert_eq!(step.withdraw, None);
    }

    #[test]
    fn a_confirmation_the_voice_can_answer_is_never_asked_in_the_chat() {
        let mut c = Confirming::default();
        c.on_event(&requires("c1"));
        c.on_event(&state(VoiceState::Asking));
        let step = c.on_event(&started("c1"));
        assert_eq!(step, Step::default(), "nothing was asked, so nothing is withdrawn");
        assert!(c.on_event(&state(VoiceState::WaitingForClick)).ask.is_none());
    }

    #[test]
    fn the_state_arriving_first_still_asks_when_the_confirmation_follows() {
        let mut c = Confirming::default();
        assert_eq!(c.on_event(&state(VoiceState::WaitingForClick)), Step::default());
        assert_eq!(asked(&c.on_event(&requires("c1"))), Some("c1"));
    }

    #[test]
    fn a_tool_starting_or_the_turn_ending_withdraws_the_question() {
        for resolved in [
            started("c1"),
            VoiceEvent::Agent {
                event: AgentEvent::ToolFinished {
                    call_id: "c1".into(),
                    name: "delete_task".into(),
                    ok: false,
                    summary: String::new(),
                    mail_link: None,
                    files: Vec::new(),
                },
            },
            VoiceEvent::Agent { event: AgentEvent::Finished { message_id: "m".into() } },
            VoiceEvent::Agent { event: AgentEvent::Failed { message: "no".into() } },
            VoiceEvent::Ended { error: None },
        ] {
            let mut c = Confirming::default();
            c.on_event(&requires("c1"));
            c.on_event(&state(VoiceState::WaitingForClick));
            let step = c.on_event(&resolved);
            let withdrawn = step.withdraw.expect("withdrawn");
            assert_eq!(withdrawn.call_id, "c1", "{resolved:?}");
            assert_eq!(
                withdrawn.settled,
                matches!(
                    resolved,
                    VoiceEvent::Agent {
                        event: AgentEvent::ToolStarted { .. } | AgentEvent::ToolFinished { .. }
                    }
                ),
                "only the tool itself says the session settled it: {resolved:?}"
            );
            assert!(step.ask.is_none());
            assert_eq!(c.on_event(&state(VoiceState::WaitingForClick)).withdraw, None);
        }
    }

    #[test]
    fn another_calls_tool_starting_leaves_the_question_alone() {
        let mut c = Confirming::default();
        c.on_event(&requires("c1"));
        c.on_event(&state(VoiceState::WaitingForClick));
        assert_eq!(c.on_event(&started("other")), Step::default());
    }

    #[test]
    fn the_owner_talking_over_it_does_not_ask_again() {
        let mut c = Confirming::default();
        c.on_event(&requires("c1"));
        assert!(c.on_event(&state(VoiceState::WaitingForClick)).ask.is_some());
        assert_eq!(c.on_event(&state(VoiceState::Hearing)), Step::default());
        assert_eq!(
            c.on_event(&state(VoiceState::WaitingForClick)),
            Step::default(),
            "the question is still there in the chat"
        );
    }

    // ---- the transcript ----

    fn heard(text: &str) -> VoiceEvent {
        VoiceEvent::Heard { text: text.into() }
    }

    fn agent(event: AgentEvent) -> VoiceEvent {
        VoiceEvent::Agent { event }
    }

    fn reply(id: &str, text: &str) -> Vec<VoiceEvent> {
        vec![
            agent(AgentEvent::Started { message_id: id.into() }),
            agent(AgentEvent::Delta { text: text.into() }),
            agent(AgentEvent::Finished { message_id: id.into() }),
        ]
    }

    fn written(t: &mut Transcript, events: &[VoiceEvent]) -> Vec<String> {
        events.iter().flat_map(|e| t.on_event(e)).collect()
    }

    #[test]
    fn what_was_said_is_written_and_each_reply_once_it_has_been_heard() {
        let mut t = Transcript::default();
        let mut events = vec![heard("what's on today?")];
        events.extend(reply("m1", "Just the dentist at three."));
        assert_eq!(
            written(&mut t, &events),
            [said_by_owner("what's on today?")],
            "the reply waits until it has been heard"
        );
        assert_eq!(
            written(&mut t, &[state(VoiceState::Speaking), state(VoiceState::Listening)]),
            ["Just the dentist at three."]
        );
        assert!(t.finish().is_empty(), "and only once");
        assert!(said_by_owner("what's on today?").starts_with("> "));
        assert!(said_by_owner("what's on today?").contains("what's on today?"));
    }

    #[test]
    fn a_reply_heard_whole_is_written_before_anything_read_after_it_ends() {
        let mut t = Transcript::default();
        let mut events = vec![heard("read me the Morning Brew")];
        events.extend(reply("m1", "Here is the Morning Brew."));
        events.push(state(VoiceState::Speaking));
        assert_eq!(written(&mut t, &events), [said_by_owner("read me the Morning Brew")]);
        assert_eq!(
            t.on_event(&VoiceEvent::ReplyHeard { message_id: "m1".into() }),
            ["Here is the Morning Brew."],
            "while the reading goes on"
        );
        assert!(t.on_event(&VoiceEvent::ReplyHeard { message_id: "m0".into() }).is_empty());
        assert!(written(&mut t, &[state(VoiceState::Listening)]).is_empty(), "and only once");
    }

    #[test]
    fn a_reply_talked_over_is_written_as_far_as_it_was_heard() {
        let mut t = Transcript::default();
        let mut events = vec![heard("tell me about the week")];
        events.extend(reply("m1", "Monday is busy. Tuesday is quiet."));
        events
            .push(VoiceEvent::ReplyCut { message_id: "m1".into(), text: "Monday is busy.".into() });
        events.push(heard("what about Friday?"));
        assert_eq!(
            written(&mut t, &events),
            [
                said_by_owner("tell me about the week"),
                "Monday is busy.".to_string(),
                said_by_owner("what about Friday?"),
            ]
        );
    }

    #[test]
    fn a_reply_nobody_heard_is_not_written() {
        let mut t = Transcript::default();
        let mut events = vec![heard("hello")];
        events.extend(reply("m1", "Hi there."));
        events.push(VoiceEvent::ReplyCut { message_id: "m1".into(), text: String::new() });
        assert_eq!(written(&mut t, &events), [said_by_owner("hello")]);
    }

    #[test]
    fn a_retry_replaces_what_was_said_and_a_failure_is_written() {
        let mut t = Transcript::default();
        let events = vec![
            heard("hello"),
            agent(AgentEvent::Started { message_id: "m1".into() }),
            agent(AgentEvent::Delta { text: "Half a th".into() }),
            agent(AgentEvent::Retrying { text: String::new() }),
            agent(AgentEvent::Delta { text: "Hello!".into() }),
            agent(AgentEvent::Finished { message_id: "m1".into() }),
            heard("and?"),
            agent(AgentEvent::Started { message_id: "m2".into() }),
            agent(AgentEvent::Failed { message: "the model is down".into() }),
            VoiceEvent::Ended { error: None },
        ];
        assert_eq!(
            written(&mut t, &events),
            [
                said_by_owner("hello"),
                "Hello!".to_string(),
                said_by_owner("and?"),
                "Something went wrong: the model is down".to_string(),
            ]
        );
    }

    #[test]
    fn a_reply_still_being_said_when_the_call_ends_is_written() {
        let mut t = Transcript::default();
        let events = vec![
            heard("read me the news"),
            agent(AgentEvent::Started { message_id: "m1".into() }),
            agent(AgentEvent::Delta { text: "Here it is.".into() }),
        ];
        written(&mut t, &events);
        assert_eq!(t.finish(), ["Here it is."]);
    }

    #[test]
    fn a_second_confirmation_while_clicking_replaces_the_first() {
        let mut c = Confirming::default();
        c.on_event(&requires("c1"));
        c.on_event(&state(VoiceState::WaitingForClick));
        let step = c.on_event(&requires("c2"));
        assert_eq!(
            step.withdraw,
            Some(Withdraw { call_id: "c1".into(), settled: false }),
            "the first is still waiting to be answered"
        );
        assert_eq!(asked(&step), Some("c2"));
    }
}
