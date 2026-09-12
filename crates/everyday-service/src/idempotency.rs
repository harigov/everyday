//! Answering a retry from the record rather than doing it twice.
//!
//! "From anywhere" means dropped connections, and a dropped connection has a
//! failure mode a local call does not: the write landed and the answer did
//! not. The client cannot tell that from a write that never arrived, so it
//! retries -- and the interface's autosave is *built* to retry, because a full
//! disk or a busy database is exactly what it is there for.
//!
//! What the retry then meets is the conditional save. Every entry save carries
//! the `updated_at` it loaded and is refused if that is no longer what is
//! stored. The first attempt stored it, so the second attempt is refused as a
//! conflict -- against its own earlier write. The editor then offers "keep
//! mine" or "discard mine" for a change that has already been saved, which is
//! an alarming thing to be shown and is entirely this layer's fault.
//!
//! So a write may carry a request id. The first call with a given id runs; a
//! second with the same id is answered with the first one's result, including
//! its error if it failed. Ids are the client's to mint and are scoped to the
//! caller, so two devices cannot collide.
//!
//! # A retry that arrives while the first is still running
//!
//! The common case, in fact: the client gave up waiting, which is why it
//! retried. So a slot is claimed before the command runs and the second caller
//! waits on it rather than starting a second copy. Without that, the whole
//! mechanism would only cover retries that arrive after the original finished,
//! which is the easy half.

use crate::error::{CommandError, codes};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a result is worth keeping. Long enough to cover a client's own
/// retry budget and a reconnect; short enough that this is a cache and not a
/// log of everything anybody wrote.
const TTL: Duration = Duration::from_secs(300);

/// Most results to keep. A bound on memory rather than a policy: the TTL is
/// what usually retires an entry.
const CAPACITY: usize = 512;

type Answer = Result<Value, CommandError>;

pub struct Slot {
    /// `None` until the command that claimed this slot has finished.
    answer: Mutex<Option<Answer>>,
    /// Woken when it has.
    ready: tokio::sync::Notify,
    claimed_at: Instant,
}

#[derive(Default)]
pub struct Idempotency {
    slots: Mutex<HashMap<String, Arc<Slot>>>,
}

/// What a caller got when it asked for a slot.
pub enum Claim {
    /// This caller runs the command and must call [`Idempotency::fulfil`].
    Mine(Arc<Slot>),
    /// Somebody else has it. Await this.
    Theirs(Arc<Slot>),
}

impl Idempotency {
    /// Hold a claimed slot, answering it or abandoning it on the way out.
    ///
    /// The `Drop` is the point. A command's future can be *dropped* rather than
    /// completed -- a client that hung up, a server shutting down, a timeout
    /// somewhere above -- and a slot left claimed with no answer in it parks
    /// every retry of that request for ever. There is no code path that
    /// notices; the symptom is one client that stops responding.
    pub fn guard<'a>(&'a self, key: &str, slot: Arc<Slot>) -> Guard<'a> {
        Guard { cache: self, key: key.to_string(), slot, answered: false }
    }

    /// Take the slot for `key`, or find out who has it.
    pub fn claim(&self, key: &str) -> Claim {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        slots.retain(|_, slot| now.duration_since(slot.claimed_at) < TTL);
        if slots.len() >= CAPACITY {
            // Oldest first, which is the only ordering that means anything
            // here: a slot's usefulness is entirely its age.
            let mut ages: Vec<(String, Instant)> =
                slots.iter().map(|(k, s)| (k.clone(), s.claimed_at)).collect();
            ages.sort_by_key(|(_, at)| *at);
            for (key, _) in ages.into_iter().take(slots.len() - CAPACITY + 1) {
                slots.remove(&key);
            }
        }

        if let Some(slot) = slots.get(key) {
            return Claim::Theirs(slot.clone());
        }
        let slot = Arc::new(Slot {
            answer: Mutex::new(None),
            ready: tokio::sync::Notify::new(),
            claimed_at: now,
        });
        slots.insert(key.to_string(), slot.clone());
        Claim::Mine(slot)
    }

    /// Record what the command answered, and wake anyone waiting.
    pub fn fulfil(&self, slot: &Arc<Slot>, answer: Answer) {
        *slot.answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
        slot.ready.notify_waiters();
    }

    /// Give up a slot whose command never finished, so a retry is not answered
    /// with a wait that never ends.
    ///
    /// The slot is *answered*, not merely dropped. A waiter that is told
    /// nothing has no way to distinguish "gone" from "not yet", and the whole
    /// point of this layer is that a caller is never left guessing whether its
    /// write landed.
    pub fn abandon(&self, key: &str, slot: &Arc<Slot>) {
        {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if slots.get(key).is_some_and(|held| Arc::ptr_eq(held, slot)) {
                slots.remove(key);
            }
        }
        self.fulfil(
            slot,
            Err(CommandError::new(
                codes::RETRY,
                "the first attempt at this request did not finish; \
                 ask again with a new request id",
            )),
        );
    }
}

/// A claimed slot, held for the duration of the command that claimed it.
pub struct Guard<'a> {
    cache: &'a Idempotency,
    key: String,
    slot: Arc<Slot>,
    answered: bool,
}

impl Guard<'_> {
    /// Record what the command answered. Consumes the guard's obligation, so
    /// the `Drop` below does nothing.
    pub fn fulfil(mut self, answer: Result<Value, CommandError>) {
        self.answered = true;
        self.cache.fulfil(&self.slot, answer);
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if !self.answered {
            self.cache.abandon(&self.key, &self.slot);
        }
    }
}

/// Wait for whoever claimed this slot to finish, and answer as they did.
///
/// `enable()` is the whole of the correctness here, and it is easy to leave out.
/// `Notify::notified()` merely *builds* a future; it registers nothing until it
/// is first polled, and `notify_waiters` stores no permit for a waiter that has
/// not registered yet. So the obvious spelling -- build the future, check the
/// answer, then await -- parks for ever whenever the answer arrives in the gap
/// between the check and the await, which is exactly the case this is for.
/// `enable()` registers up front, so a completion in that gap is caught.
pub async fn wait(slot: Arc<Slot>) -> Answer {
    let answer = || slot.answer.lock().unwrap_or_else(|e| e.into_inner()).clone();
    loop {
        let notified = slot.ready.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(answer) = answer() {
            return answer;
        }
        notified.await;
        if let Some(answer) = answer() {
            return answer;
        }
        // A wakeup with nothing behind it. Both `fulfil` and `abandon` set the
        // answer before they notify, so this cannot be a completion that was
        // missed -- go round again rather than invent a result.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_repeat_is_answered_from_the_first_result() {
        let cache = Idempotency::default();
        let Claim::Mine(slot) = cache.claim("device-a:1") else { panic!("first claim") };
        cache.fulfil(&slot, Ok(serde_json::json!("done")));

        let Claim::Theirs(again) = cache.claim("device-a:1") else {
            panic!("a repeat must not get the slot")
        };
        assert_eq!(wait(again).await.unwrap(), serde_json::json!("done"));
    }

    #[tokio::test]
    async fn a_repeat_that_arrives_first_waits_rather_than_running() {
        let cache = Arc::new(Idempotency::default());
        let Claim::Mine(slot) = cache.claim("device-a:2") else { panic!("first claim") };

        let Claim::Theirs(waiting) = cache.claim("device-a:2") else { panic!("second claim") };
        let reader = tokio::spawn(async move { wait(waiting).await });

        // Nothing has been recorded yet, so the second caller is parked.
        tokio::task::yield_now().await;
        cache.fulfil(&slot, Ok(serde_json::json!(7)));
        assert_eq!(reader.await.unwrap().unwrap(), serde_json::json!(7));
    }

    #[tokio::test]
    async fn a_failure_is_replayed_as_a_failure() {
        let cache = Idempotency::default();
        let Claim::Mine(slot) = cache.claim("k") else { panic!() };
        cache.fulfil(&slot, Err(CommandError::new(codes::CONFLICT, "changed since you loaded it")));
        let Claim::Theirs(again) = cache.claim("k") else { panic!() };
        assert_eq!(wait(again).await.unwrap_err().code, "conflict");
    }

    #[tokio::test]
    async fn an_abandoned_slot_does_not_park_a_retry_for_ever() {
        let cache = Idempotency::default();
        let Claim::Mine(slot) = cache.claim("k") else { panic!() };
        let Claim::Theirs(again) = cache.claim("k") else { panic!() };
        cache.abandon("k", &slot);
        assert_eq!(wait(again).await.unwrap_err().code, "retry");
        // ...and the key is free, so a fresh attempt may take it.
        assert!(matches!(cache.claim("k"), Claim::Mine(_)));
    }

    #[test]
    fn different_callers_do_not_collide() {
        let cache = Idempotency::default();
        assert!(matches!(cache.claim("device-a:1"), Claim::Mine(_)));
        assert!(matches!(cache.claim("device-b:1"), Claim::Mine(_)));
    }
}
