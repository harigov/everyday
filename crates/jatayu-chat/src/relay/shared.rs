//! What the dispatcher and one chat's worker both touch: the turn running and the question waiting.

use super::*;

// ---- what is shared per chat ------------------------------------------------

/// What the dispatcher and one chat's worker both need to see.
#[derive(Default)]
pub(super) struct Shared {
    /// The turn the worker has in hand, from the moment it takes a message up
    /// until the turn has ended.
    pub(super) running: Option<Running>,
    /// The question the turn is waiting on the owner for.
    pub(super) waiting: Option<Waiting>,
    /// Set once nothing more can be answered. A question asked after that is
    /// declined on the spot rather than left to wait out its timeout.
    pub(super) closed: bool,
    /// The call this chat has, queued or running.
    pub(super) call: Option<CallSlot>,
    /// The worker is ringing the owner, and their picking up -- which arrives
    /// as a call like any other -- goes to it rather than being queued.
    pub(super) ringing: Option<oneshot::Sender<CallOffer>>,
}

/// The turn a worker has in hand.
#[derive(Default)]
pub(super) struct Running {
    /// Known once the worker has chosen it; before that there is nothing to
    /// cancel yet.
    pub(super) conversation: Option<ConversationId>,
    /// Whether the owner has said `!stop` (or the relay is stopping) since
    /// this turn was taken up. Three things follow from it, all in the worker:
    /// a turn not yet started is not started, a stop that landed before the
    /// turn existed is applied the moment it does, and the turn's own
    /// "Stopped." is not sent a second time.
    pub(super) stop_requested: bool,
    /// Whether this is a routine's run waiting on a question here rather than
    /// a turn the owner started. `!stop` stops either; the relay stopping
    /// stops only its own -- the routine runs elsewhere, and is merely told
    /// nobody will answer.
    pub(super) routine: bool,
}

/// A question the turn is waiting on.
pub(super) struct Waiting {
    /// The message the question went out as. `None` while it is still being
    /// sent -- the window in which a typed answer must already count, and a
    /// tap on a message we have not been told the name of yet can only be
    /// held.
    pub(super) question: Option<MessageRef>,
    /// Taps that arrived before `question` was known, waiting to be matched
    /// to it.
    pub(super) early: Vec<(MessageRef, Choice)>,
    pub(super) answer: Answer,
}

/// Who is waiting for the answer, and what kind of answer they want.
pub(super) enum Answer {
    /// Yes, no or -- when `can_park` -- later, and -- when `can_trust` --
    /// always. Without `can_park`, "later" is read as "no"; without
    /// `can_trust`, so is "always".
    Confirm { can_park: bool, can_trust: bool, tx: oneshot::Sender<ConfirmAnswer> },
    /// Whatever the owner types next: a routine's question in its own words.
    Free { tx: oneshot::Sender<Option<String>> },
}

impl Answer {
    /// Answer with a tap.
    pub(super) fn chose(self, choice: Choice) {
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
    pub(super) fn said(self, text: String) {
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
    pub(super) fn decline(self) {
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
    pub(super) fn asked(&self) -> Asked<'_> {
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
    pub(super) fn answer(&mut self, choice: Choice) {
        // A closed receiver means the worker stopped waiting an instant ago;
        // the answer has nowhere to go and nothing depends on it.
        if let Some(waiting) = self.waiting.take() {
            waiting.answer.chose(choice);
        }
    }

    /// Give the waiting question what the owner typed.
    pub(super) fn answer_text(&mut self, text: String) {
        if let Some(waiting) = self.waiting.take() {
            waiting.answer.said(text);
        }
    }

    /// Hold a tap on a message the question has not been matched to yet.
    pub(super) fn hold(&mut self, question: MessageRef, choice: Choice) {
        if let Some(waiting) = &mut self.waiting {
            waiting.early.push((question, choice));
        }
    }

    /// Say no to the waiting question, if there is one.
    pub(super) fn decline(&mut self) {
        if let Some(waiting) = self.waiting.take() {
            waiting.answer.decline();
        }
    }

    /// The question has gone out as `reference`: remember it, and count a tap
    /// that beat it here.
    pub(super) fn question_sent(&mut self, reference: MessageRef) {
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
    pub(super) fn call_keys(&mut self, call: &CallId, keys: Vec<CallKey>) {
        if let Some(slot) = self.call.as_mut().filter(|slot| slot.id == *call) {
            slot.heard(keys);
        }
    }

    /// The worker takes `call` up: see [`CallSlot::start`]. `None` if this chat
    /// no longer has it, or the owner ended it while it waited.
    pub(super) fn call_start(
        &mut self,
        call: &CallId,
    ) -> Option<(Vec<CallKey>, mpsc::UnboundedReceiver<ToCall>)> {
        self.call.as_mut().filter(|slot| slot.id == *call).and_then(CallSlot::start)
    }

    /// Whether `call` is over as far as the owner is concerned.
    pub(super) fn call_is_over(&self, call: &CallId) -> bool {
        self.call.as_ref().filter(|slot| slot.id == *call).is_none_or(CallSlot::is_ended)
    }

    /// The owner hung up `call`.
    pub(super) fn call_ended(&mut self, call: &CallId) {
        if let Some(slot) = self.call.as_mut().filter(|slot| slot.id == *call) {
            slot.end();
        }
    }

    /// End the call this chat has, whichever stage it is at. Whether there was
    /// one to end.
    pub(super) fn end_call(&mut self) -> bool {
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
    pub(super) fn halt(&mut self, pending: &Pending) {
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
    pub(super) fn close(&mut self) {
        self.closed = true;
        self.decline();
        // A ring the worker is waiting on stops, and the worker takes it down.
        self.ringing = None;
    }

    /// The owner's `!stop`: cancel the turn that is running, and decline the
    /// question it may be waiting on. What to say back.
    pub(super) fn stop(&mut self, pending: &Pending) -> &'static str {
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
