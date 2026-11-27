//! Asking the person at this computer.
//!
//! The assistant working on its own -- a routine, a project, a job's
//! follow-up -- sometimes needs a yes, a no or an answer. Until now it could
//! only ask in the owner's chat (Matrix, [`crate::chat::ChatOutlet`]), and
//! with no chat link running nobody was asked at all: the call was declined
//! or parked as a proposal. The desktop app hands the service a [`DeskAsker`]
//! as it starts, and a question is then put to the person on the panel too
//! (`jatayu-app/src/askcard.rs`): the first answer, from the panel or from
//! the chat, is the one taken.
//!
//! The race is [`DeskOrChat`], an [`Asker`] like any other, so nothing that
//! asks -- the confirmation gate, `ask_owner` -- knows there are two places:
//! a project's asker (`Service::owner_asker`), a routine's
//! (`scheduler::reach_for`) and a job's follow-up (`jobs::answer`) are each
//! one of these whenever the app has handed a desk over, and exactly what
//! they were before when it has not.
//!
//! # Things decided here
//!
//! - **The first answer wins**, from either place. When the chat's comes
//!   first the panel's card is taken down ([`DeskAsker::withdraw`]). When the
//!   desk's comes first the chat's question is dropped -- a [`ChatOutlet`]
//!   has no way to take back a question it has sent -- and the relay finds
//!   nobody waiting for its answer, and closes the question in the chat.
//! - **A desk that does not put the question** (the person sends questions
//!   to notifications, the vault is locked) is no desk: the chat alone is
//!   asked, and with no chat nobody is, and the answer is what nobody being
//!   there has always meant -- see [`DeskOrChat::park_unasked`].
//! - **The desk waits as long as a chat does**, half an hour
//!   ([`DESK_WAIT`]); silence then means what it means in a chat: later
//!   where that is offered, no otherwise. A job's follow-up waits less
//!   ([`DeskOrChat::waiting`]): its turn is cut off at fifteen minutes, and
//!   a wait longer than that would end in a failed turn, not an answer.
//! - **A desk alone is not a chat**: a run that can ask only at the desk
//!   does not count as asking remotely ([`Asker::asks_in_chat`]), so the
//!   person's own "confirm destructive" setting still decides whether a
//!   delete stops to ask.
//! - **A question nobody wants any more is taken down**: one answered in the
//!   chat, one whose turn was dropped (cancelled, timed out, the vault
//!   closing), and one outstanding when the run is told to stop waiting
//!   ([`DeskOrChat::until`]).
//!
//! [`ChatOutlet`]: crate::chat::ChatOutlet

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::watch;

use crate::agent::{Asker, ConfirmAnswer};

/// A yes-or-no question about a call the assistant wants to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeskQuestion {
    /// Unique to this question: the card's id, and what [`DeskAsker::withdraw`]
    /// names.
    pub id: String,
    /// Who is asking, as the person knows it: the routine's name, "Research:
    /// flights to Lisbon".
    pub from: String,
    /// The question's kind, as `AgentEvent::ConfirmationRequired` names it:
    /// `destructive`, `outward`, `fetch`, ...
    pub kind: String,
    /// What is about to happen.
    pub subject: String,
    /// "Later" may park the call as a proposal.
    pub can_park: bool,
    /// The website "Always allow" would add, where the question has one.
    pub site: Option<String>,
}

/// A question the assistant wants answered in words (`ask_owner`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeskAsk {
    pub id: String,
    pub from: String,
    pub question: String,
    /// Answers to pick from; empty for a free answer.
    pub choices: Vec<String>,
}

/// The desktop's way to put a question to the person at it.
pub trait DeskAsker: Send + Sync {
    /// Put `question`. `None` if it was not put -- the person sends the
    /// assistant's questions elsewhere, or there is nothing to show it on --
    /// and the caller goes on as if there were no desk. Resolves when the
    /// person answers, or the question is withdrawn (then `Decline`).
    fn confirm(&self, question: DeskQuestion) -> BoxFuture<'static, Option<ConfirmAnswer>>;
    /// Put `ask`. The outer `None` if it was not put; the inner `None` if it
    /// was and no answer came (withdrawn, dismissed).
    fn ask(&self, ask: DeskAsk) -> BoxFuture<'static, Option<Option<String>>>;
    /// A question put earlier has been answered elsewhere or is no longer
    /// wanted: take it down. Its future resolves as unanswered.
    fn withdraw(&self, id: &str);
    /// Whether the person wants the assistant's questions here at all. When
    /// they send them elsewhere (to the desktop's notifications, which
    /// carry no answer), the service behaves as if there were no desk:
    /// nobody is asked unless a chat is, `ask_owner` is not offered, and a
    /// call that needed a yes is parked or declined as it was before the
    /// desk existed. [`crate::service::Service::desk_asker`] reads it.
    fn wanted(&self) -> bool {
        true
    }
}

/// How long a question put at the desk waits for an answer before it is
/// taken down: half an hour, as long as the chat waits for one
/// (`jatayu_chat::relay::CONFIRM_TIMEOUT`), and what the run's own prompt
/// promises -- "a question nobody answers within half an hour comes back
/// empty".
pub const DESK_WAIT: Duration = Duration::from_secs(30 * 60);

/// How long the desk may keep one routine run waiting, all its questions
/// together: one question's worth. The run's own clock is paused while it
/// waits, so this is not about the run's time: it stops a routine nobody is
/// watching -- the computer left on overnight -- holding its calls for an
/// hour and more, question after question, when they could be parked for the
/// morning.
pub(crate) const RUN_DESK_BUDGET: Duration = DESK_WAIT;

/// How long the desk may keep one project job waiting, all its questions
/// together: well inside the job's own time (`jobs::project::TIMEOUT`, which
/// keeps counting while it waits), so a project nobody is watching still
/// finishes, with what it could not ask declined.
pub(crate) const PROJECT_DESK_BUDGET: Duration = Duration::from_secs(20 * 60);

/// The person at this computer and their chat, asked at once: the first
/// answer is the one taken. See the module docs for the rules.
pub struct DeskOrChat {
    desk: Arc<dyn DeskAsker>,
    /// The chat's asker, when one is running and the asking run may ask in
    /// it.
    chat: Option<Arc<dyn Asker>>,
    /// Who is asking, as the desk's card says it.
    from: String,
    /// With nobody to ask at all, park what can be parked rather than
    /// decline it.
    park_unasked: bool,
    /// Turns true when nobody will be waiting for an answer any more.
    stop: Option<watch::Receiver<bool>>,
    /// How long the desk waits before its question goes.
    wait: Duration,
    /// How long the desk may keep this asker's run waiting, all its
    /// questions together ([`DeskOrChat::budget`]); `None` is no limit beyond
    /// each question's own `wait`.
    budget: Option<Arc<Mutex<Duration>>>,
}

impl DeskOrChat {
    /// Ask at `desk`, and in `chat` when there is one, as `from`.
    ///
    /// With nobody to ask at all a call that can be parked is parked, until
    /// [`DeskOrChat::park_unasked`] says otherwise; nothing stops it waiting
    /// until [`DeskOrChat::until`] gives it something that does.
    pub fn new(
        desk: Arc<dyn DeskAsker>,
        chat: Option<Arc<dyn Asker>>,
        from: impl Into<String>,
    ) -> DeskOrChat {
        DeskOrChat {
            desk,
            chat,
            from: from.into(),
            park_unasked: true,
            stop: None,
            wait: DESK_WAIT,
            budget: None,
        }
    }

    /// What a call is answered with when the desk did not put the question
    /// and there is no chat: [`ConfirmAnswer::Later`] for one that can be
    /// parked when `park` is true, [`ConfirmAnswer::Decline`] otherwise.
    ///
    /// A routine passes the person's `park_unattended`, so that with their
    /// questions sent to notifications it parks or refuses exactly as it did
    /// before there was a desk; a job's follow-up passes false, because it
    /// always declined with nobody to ask.
    pub fn park_unasked(mut self, park: bool) -> DeskOrChat {
        self.park_unasked = park;
        self
    }

    /// Stop waiting once `stop` turns true -- the scheduler stopping, the
    /// vault closing -- with a no (or no answer), and take the desk's
    /// question down. No, not later: parking writes to a vault that may be
    /// closing.
    pub fn until(mut self, stop: watch::Receiver<bool>) -> DeskOrChat {
        self.stop = Some(stop);
        self
    }

    /// Wait `wait` at the desk rather than [`DESK_WAIT`]: for a question
    /// asked inside a turn that is itself cut off sooner -- a job's
    /// follow-up, `jobs::answer` -- whose silence must come back as an answer
    /// before the turn is stopped for taking too long.
    pub(crate) fn waiting(mut self, wait: Duration) -> DeskOrChat {
        self.wait = wait;
        self
    }

    /// Let the desk keep this run waiting `total` at most, all its questions
    /// together. A run asks at the desk whether or not anybody is there --
    /// at three in the morning nobody is -- and without a limit three
    /// unanswered questions would hold it an hour and a half, past the time a
    /// job or a turn is given. Once the budget is spent the desk is not asked
    /// again: the question goes to the chat alone, or is answered as nobody
    /// being there to ask (parked or declined, as before there was a desk).
    pub(crate) fn budget(mut self, total: Duration) -> DeskOrChat {
        self.budget = Some(Arc::new(Mutex::new(total)));
        self
    }

    /// How long the next question may wait at the desk: its own `wait`, or
    /// what is left of the budget if that is less. Zero: do not ask the desk.
    fn next_wait(&self) -> Duration {
        match &self.budget {
            Some(left) => self.wait.min(*left.lock().unwrap_or_else(|e| e.into_inner())),
            None => self.wait,
        }
    }

    /// `asked`, with the time it spent waiting taken off the budget.
    fn spending<T: Send + 'static>(
        &self,
        asked: impl std::future::Future<Output = T> + Send + 'static,
    ) -> BoxFuture<'static, T> {
        let budget = self.budget.clone();
        Box::pin(async move {
            // Tokio's clock, the one the desk's own wait runs on.
            let started = tokio::time::Instant::now();
            let answer = asked.await;
            if let Some(left) = budget {
                let mut left = left.lock().unwrap_or_else(|e| e.into_inner());
                *left = left.saturating_sub(started.elapsed());
            }
            answer
        })
    }
}

/// The desk's answer to a question the budget leaves no time for: not put.
fn not_put<T: Send + 'static>() -> BoxFuture<'static, Option<T>> {
    Box::pin(async { None })
}

impl Asker for DeskOrChat {
    fn confirm(
        &self,
        kind: &str,
        subject: &str,
        can_park: bool,
        site: Option<&str>,
    ) -> BoxFuture<'static, ConfirmAnswer> {
        let id = question_id();
        let wait = self.next_wait();
        let desk = if wait.is_zero() {
            not_put()
        } else {
            self.desk.confirm(DeskQuestion {
                id: id.clone(),
                from: self.from.clone(),
                kind: kind.to_string(),
                subject: subject.to_string(),
                can_park,
                site: site.map(str::to_string),
            })
        };
        let chat = self.chat.as_ref().map(|chat| chat.confirm(kind, subject, can_park, site));
        let take_down = TakeDown { desk: self.desk.clone(), id, armed: !wait.is_zero() };
        let ends =
            Ends { nobody: unasked(can_park && self.park_unasked), silence: unasked(can_park) };
        let asked = self.spending(first_answer(desk, chat, take_down, wait, ends));
        Box::pin(until(self.stop.clone(), ConfirmAnswer::Decline, asked))
    }

    fn ask(&self, question: String, choices: Vec<String>) -> BoxFuture<'static, Option<String>> {
        let id = question_id();
        let wait = self.next_wait();
        let desk = if wait.is_zero() {
            not_put()
        } else {
            self.desk.ask(DeskAsk {
                id: id.clone(),
                from: self.from.clone(),
                question: question.clone(),
                choices: choices.clone(),
            })
        };
        let chat = self.chat.as_ref().map(|chat| chat.ask(question, choices));
        let take_down = TakeDown { desk: self.desk.clone(), id, armed: !wait.is_zero() };
        let asked = self.spending(first_answer(desk, chat, take_down, wait, Ends::default()));
        Box::pin(until(self.stop.clone(), None, asked))
    }

    /// Only as far as its chat does: the desk alone is the person at this
    /// computer, whose own setting says whether a delete asks first.
    fn asks_in_chat(&self) -> bool {
        self.chat.as_ref().is_some_and(|chat| chat.asks_in_chat())
    }
}

/// A new question's id: unique, so a withdrawal names one card and no other.
fn question_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// What a call is answered with when nobody answers it: later where that is
/// allowed, no otherwise.
fn unasked(park: bool) -> ConfirmAnswer {
    if park { ConfirmAnswer::Later } else { ConfirmAnswer::Decline }
}

/// What a question comes back as without an answer: when nobody at all was
/// asked, and when the desk was asked and said nothing in time.
#[derive(Default)]
struct Ends<T> {
    nobody: T,
    silence: T,
}

/// Takes the desk's question down when dropped -- however the wait ends,
/// including the future being dropped half way -- unless the desk itself
/// answered or never put it.
struct TakeDown {
    desk: Arc<dyn DeskAsker>,
    id: String,
    armed: bool,
}

impl TakeDown {
    /// The desk is done with it by itself: nothing to take down.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Take it down now, once.
    fn now(&mut self) {
        if std::mem::take(&mut self.armed) {
            self.desk.withdraw(&self.id);
        }
    }
}

impl Drop for TakeDown {
    fn drop(&mut self) {
        self.now();
    }
}

/// The first answer from the desk or the chat.
///
/// The desk's `None` is a question it did not put: the chat alone is then
/// waited for, or with no chat `ends.nobody` is the answer. A desk that says
/// nothing for `wait` has its question taken down and is out of it the same
/// way, with `ends.silence` for the answer when there is no chat. The chat
/// keeps its own time and always comes back with something.
async fn first_answer<T: Send>(
    desk: BoxFuture<'static, Option<T>>,
    chat: Option<BoxFuture<'static, T>>,
    mut take_down: TakeDown,
    wait: Duration,
    ends: Ends<T>,
) -> T {
    let desk = tokio::time::timeout(wait, desk);
    let Some(mut chat) = chat else {
        return match desk.await {
            Ok(Some(answer)) => {
                take_down.disarm();
                answer
            }
            Ok(None) => {
                take_down.disarm();
                ends.nobody
            }
            Err(_) => {
                take_down.now();
                ends.silence
            }
        };
    };
    let mut desk = std::pin::pin!(desk);
    tokio::select! {
        // Two answers ready at once: the desk's, the person in front of it
        // having just pressed it.
        biased;
        put = &mut desk => match put {
            Ok(Some(answer)) => {
                take_down.disarm();
                answer
            }
            Ok(None) => {
                take_down.disarm();
                chat.await
            }
            Err(_) => {
                take_down.now();
                chat.await
            }
        },
        answer = &mut chat => {
            take_down.now();
            answer
        }
    }
}

/// `asked`, unless `stop` turns true first: then `stopped`, and `asked` is
/// dropped -- which takes the desk's question down.
async fn until<T>(
    stop: Option<watch::Receiver<bool>>,
    stopped: T,
    asked: impl std::future::Future<Output = T>,
) -> T {
    let Some(stop) = stop else { return asked.await };
    tokio::select! {
        biased;
        answer = asked => answer,
        () = told_to_stop(stop) => stopped,
    }
}

/// Resolve once `stop` turns true, and never otherwise: a sender gone without
/// turning it is "never", not "now".
async fn told_to_stop(mut stop: watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    /// What the fake desk does with a question.
    #[derive(Clone, Copy, Default)]
    enum Desk {
        /// Not put: the person sends questions elsewhere.
        #[default]
        NotPut,
        /// Answered at once.
        Answers(ConfirmAnswer),
        /// Put, and waiting until it is withdrawn.
        Waits,
    }

    #[derive(Default)]
    struct FakeDesk {
        does: Desk,
        /// What a question in words is answered with, when it is.
        says: Option<&'static str>,
        put: Mutex<Vec<DeskQuestion>>,
        asked: Mutex<Vec<DeskAsk>>,
        withdrawn: Mutex<Vec<String>>,
        open: Mutex<HashMap<String, oneshot::Sender<()>>>,
    }

    impl FakeDesk {
        fn new(does: Desk) -> Arc<FakeDesk> {
            Arc::new(FakeDesk { does, ..FakeDesk::default() })
        }

        /// Put a question that waits: resolves once it is withdrawn.
        fn wait(&self, id: &str) -> oneshot::Receiver<()> {
            let (tx, rx) = oneshot::channel();
            self.open.lock().unwrap().insert(id.to_string(), tx);
            rx
        }

        fn withdrawn(&self) -> Vec<String> {
            self.withdrawn.lock().unwrap().clone()
        }

        fn put_ids(&self) -> Vec<String> {
            self.put.lock().unwrap().iter().map(|q| q.id.clone()).collect()
        }
    }

    impl DeskAsker for FakeDesk {
        fn confirm(&self, question: DeskQuestion) -> BoxFuture<'static, Option<ConfirmAnswer>> {
            let id = question.id.clone();
            self.put.lock().unwrap().push(question);
            match self.does {
                Desk::NotPut => Box::pin(async { None }),
                Desk::Answers(answer) => Box::pin(async move { Some(answer) }),
                Desk::Waits => {
                    let gone = self.wait(&id);
                    Box::pin(async move {
                        let _ = gone.await;
                        Some(ConfirmAnswer::Decline)
                    })
                }
            }
        }

        fn ask(&self, ask: DeskAsk) -> BoxFuture<'static, Option<Option<String>>> {
            let id = ask.id.clone();
            self.asked.lock().unwrap().push(ask);
            let says = self.says.map(str::to_string);
            match self.does {
                Desk::NotPut => Box::pin(async { None }),
                Desk::Answers(_) => Box::pin(async move { Some(says) }),
                Desk::Waits => {
                    let gone = self.wait(&id);
                    Box::pin(async move {
                        let _ = gone.await;
                        Some(None)
                    })
                }
            }
        }

        fn withdraw(&self, id: &str) {
            self.withdrawn.lock().unwrap().push(id.to_string());
            self.open.lock().unwrap().remove(id);
        }
    }

    /// The chat's asker: answers at once, or never.
    #[derive(Default)]
    struct FakeChat {
        answers: Option<ConfirmAnswer>,
        says: Option<&'static str>,
        confirms: Mutex<Vec<(String, String, bool)>>,
        asks: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl Asker for FakeChat {
        fn confirm(
            &self,
            kind: &str,
            subject: &str,
            can_park: bool,
            _site: Option<&str>,
        ) -> BoxFuture<'static, ConfirmAnswer> {
            self.confirms.lock().unwrap().push((kind.into(), subject.into(), can_park));
            match self.answers {
                Some(answer) => Box::pin(async move { answer }),
                None => Box::pin(std::future::pending()),
            }
        }

        fn ask(
            &self,
            question: String,
            choices: Vec<String>,
        ) -> BoxFuture<'static, Option<String>> {
            self.asks.lock().unwrap().push((question, choices));
            match self.says {
                Some(said) => Box::pin(async move { Some(said.to_string()) }),
                None => Box::pin(std::future::pending()),
            }
        }
    }

    fn both(desk: &Arc<FakeDesk>, chat: Option<&Arc<FakeChat>>) -> DeskOrChat {
        let chat = chat.map(|c| c.clone() as Arc<dyn Asker>);
        DeskOrChat::new(desk.clone(), chat, "Morning brief")
    }

    #[test]
    fn only_a_chat_beside_the_desk_asks_remotely() {
        let desk = FakeDesk::new(Desk::Waits);
        assert!(!both(&desk, None).asks_in_chat(), "the desk alone is this computer");
        let chat = Arc::new(FakeChat::default());
        assert!(both(&desk, Some(&chat)).asks_in_chat());
        assert!(chat.asks_in_chat(), "any other asker is a chat's");
    }

    #[tokio::test]
    async fn the_desk_answering_first_is_the_answer() {
        let desk = FakeDesk::new(Desk::Answers(ConfirmAnswer::Confirm));
        let chat = Arc::new(FakeChat::default());
        let asker = both(&desk, Some(&chat));

        let answer = asker.confirm("destructive", "the old itinerary", true, None).await;
        assert_eq!(answer, ConfirmAnswer::Confirm);
        let put = desk.put.lock().unwrap().clone();
        assert_eq!(put.len(), 1);
        assert_eq!(put[0].from, "Morning brief");
        assert_eq!(put[0].kind, "destructive");
        assert_eq!(put[0].subject, "the old itinerary");
        assert!(put[0].can_park);
        assert_eq!(put[0].site, None);
        assert_eq!(chat.confirms.lock().unwrap().len(), 1, "the chat was asked as well");
        assert!(desk.withdrawn().is_empty(), "the desk answered: nothing to take down");
    }

    #[tokio::test]
    async fn the_chat_answering_first_takes_the_desks_question_down() {
        let desk = FakeDesk::new(Desk::Waits);
        let chat =
            Arc::new(FakeChat { answers: Some(ConfirmAnswer::Decline), ..Default::default() });
        let asker = both(&desk, Some(&chat));

        let answer = asker.confirm("outward", "to alice@example.com", false, None).await;
        assert_eq!(answer, ConfirmAnswer::Decline);
        assert_eq!(desk.withdrawn(), desk.put_ids(), "the card for exactly that question");
    }

    #[tokio::test]
    async fn a_desk_that_does_not_put_it_leaves_it_to_the_chat() {
        let desk = FakeDesk::new(Desk::NotPut);
        let chat = Arc::new(FakeChat {
            answers: Some(ConfirmAnswer::TrustSite),
            says: Some("the red one"),
            ..Default::default()
        });
        let asker = both(&desk, Some(&chat));

        let answer = asker.confirm("fetch", "https://example.org/a", false, Some("example.org"));
        assert_eq!(answer.await, ConfirmAnswer::TrustSite);
        assert_eq!(asker.ask("Which hotel?".into(), vec![]).await.as_deref(), Some("the red one"));
        assert!(desk.withdrawn().is_empty(), "nothing was put, so nothing is taken down");
    }

    #[tokio::test]
    async fn with_nobody_to_ask_what_can_be_parked_is_parked_and_the_rest_declined() {
        let desk = FakeDesk::new(Desk::NotPut);
        let asker = both(&desk, None);
        assert_eq!(
            asker.confirm("destructive", "the deck", true, None).await,
            ConfirmAnswer::Later
        );
        assert_eq!(
            asker.confirm("fetch", "https://x.dev", false, None).await,
            ConfirmAnswer::Decline
        );
        assert_eq!(asker.ask("Which hotel?".into(), vec![]).await, None);

        // Somebody who has not said a routine may leave things waiting is
        // refused, as before there was a desk.
        let strict = both(&desk, None).park_unasked(false);
        assert_eq!(
            strict.confirm("destructive", "the deck", true, None).await,
            ConfirmAnswer::Decline
        );
        assert!(desk.withdrawn().is_empty());
    }

    #[tokio::test]
    async fn a_question_in_words_offers_its_choices_at_the_desk() {
        let desk = Arc::new(FakeDesk {
            does: Desk::Answers(ConfirmAnswer::Confirm),
            says: Some("The red one"),
            ..Default::default()
        });
        let chat = Arc::new(FakeChat::default());
        let asker = both(&desk, Some(&chat));
        let choices = vec!["The red one".to_string(), "The blue one".to_string()];

        let answer = asker.ask("Which hotel?".into(), choices.clone()).await;
        assert_eq!(answer.as_deref(), Some("The red one"));
        let asked = desk.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].from, "Morning brief");
        assert_eq!(asked[0].question, "Which hotel?");
        assert_eq!(asked[0].choices, choices);
        assert_eq!(*chat.asks.lock().unwrap(), [("Which hotel?".to_string(), choices)]);
        assert!(desk.withdrawn().is_empty());
    }

    #[tokio::test]
    async fn a_question_in_words_answered_in_the_chat_is_taken_down_at_the_desk() {
        let desk = FakeDesk::new(Desk::Waits);
        let chat = Arc::new(FakeChat { says: Some("Friday"), ..Default::default() });
        let asker = both(&desk, Some(&chat));

        assert_eq!(asker.ask("Which day?".into(), vec![]).await.as_deref(), Some("Friday"));
        let asked: Vec<String> = desk.asked.lock().unwrap().iter().map(|a| a.id.clone()).collect();
        assert_eq!(desk.withdrawn(), asked);
    }

    #[tokio::test]
    async fn every_question_has_an_id_of_its_own() {
        let desk = FakeDesk::new(Desk::Answers(ConfirmAnswer::Confirm));
        let asker = both(&desk, None);
        asker.confirm("destructive", "one", false, None).await;
        asker.confirm("destructive", "two", false, None).await;
        asker.ask("Three?".into(), vec![]).await;
        let mut ids = desk.put_ids();
        ids.extend(desk.asked.lock().unwrap().iter().map(|a| a.id.clone()));
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 3, "{ids:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_desk_keeps_a_run_waiting_no_longer_than_its_budget() {
        let desk = FakeDesk::new(Desk::Waits);
        let asker = both(&desk, None).budget(Duration::from_secs(10 * 60));
        let started = tokio::time::Instant::now();

        // The first question gets what is left of the budget, not half an hour.
        assert_eq!(asker.confirm("destructive", "one", true, None).await, ConfirmAnswer::Later);
        let first = started.elapsed();
        assert!(first >= Duration::from_secs(10 * 60) && first < DESK_WAIT, "{first:?}");
        assert_eq!(desk.put_ids().len(), 1);

        // Spent: the desk is not asked again, and the answer is nobody's.
        assert_eq!(asker.confirm("destructive", "two", true, None).await, ConfirmAnswer::Later);
        assert_eq!(asker.ask("Which?".into(), vec![]).await, None);
        assert!(started.elapsed() - first < Duration::from_secs(1), "no more waiting");
        assert_eq!(desk.put_ids().len(), 1, "no second card");
        assert!(desk.asked.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_nobody_answers_at_the_desk_goes_after_half_an_hour() {
        let desk = FakeDesk::new(Desk::Waits);
        let asker = both(&desk, None);
        let started = tokio::time::Instant::now();

        // Silence is later where later is offered, as it is in a chat.
        assert_eq!(
            asker.confirm("destructive", "the deck", true, None).await,
            ConfirmAnswer::Later
        );
        assert!(started.elapsed() >= DESK_WAIT);
        assert_eq!(desk.withdrawn(), desk.put_ids());

        let quick = both(&desk, None).waiting(Duration::from_secs(5));
        assert_eq!(
            quick.confirm("fetch", "https://x.dev", false, None).await,
            ConfirmAnswer::Decline
        );
        assert_eq!(quick.ask("Which day?".into(), vec![]).await, None);
        assert_eq!(desk.withdrawn().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_desk_that_says_nothing_still_leaves_the_chat_to_answer() {
        let desk = FakeDesk::new(Desk::Waits);
        let chat =
            Arc::new(FakeChat { answers: Some(ConfirmAnswer::Confirm), ..Default::default() });
        // A chat slower than the desk's wait: the desk's question goes, and
        // the chat's answer is still the one taken.
        struct Slow(Arc<FakeChat>);
        impl Asker for Slow {
            fn confirm(
                &self,
                kind: &str,
                subject: &str,
                can_park: bool,
                site: Option<&str>,
            ) -> BoxFuture<'static, ConfirmAnswer> {
                let answer = self.0.confirm(kind, subject, can_park, site);
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    answer.await
                })
            }
            fn ask(&self, q: String, c: Vec<String>) -> BoxFuture<'static, Option<String>> {
                self.0.ask(q, c)
            }
        }
        let asker = DeskOrChat::new(desk.clone(), Some(Arc::new(Slow(chat))), "Project")
            .waiting(Duration::from_secs(5));
        assert_eq!(asker.confirm("outward", "to bob", false, None).await, ConfirmAnswer::Confirm);
        assert_eq!(desk.withdrawn(), desk.put_ids());
    }

    #[tokio::test]
    async fn told_to_stop_it_says_no_and_takes_the_question_down() {
        let desk = FakeDesk::new(Desk::Waits);
        let (stop, stopped) = watch::channel(false);
        let asker = both(&desk, None).until(stopped);

        let asked = asker.confirm("destructive", "the deck", true, None);
        stop.send_replace(true);
        assert_eq!(asked.await, ConfirmAnswer::Decline, "no, not later");
        assert_eq!(desk.withdrawn(), desk.put_ids());

        // Already told: answered at once, with nothing.
        assert_eq!(asker.ask("Which day?".into(), vec![]).await, None);
        assert_eq!(desk.withdrawn().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_given_up_on_is_taken_down() {
        let desk = FakeDesk::new(Desk::Waits);
        let asker = both(&desk, None);
        let asked = asker.confirm("destructive", "the deck", true, None);
        // The turn waiting on it was dropped -- cancelled, or out of time.
        assert!(tokio::time::timeout(Duration::from_secs(1), asked).await.is_err());
        assert_eq!(desk.withdrawn(), desk.put_ids());
    }
}
