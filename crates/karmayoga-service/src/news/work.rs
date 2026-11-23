//! The lists the Feed's background tasks work through, and the loop that works
//! through them. What a transcript ([`super::transcribe`]) and an article
//! turned into audio ([`super::audio`]) have in common: one supervised task a
//! kind of work, one piece of work at a time -- a local recogniser and a local
//! voice each use every core they are given, and two at once would only take
//! twice as long each -- and a list the interface can read.
//!
//! # What a [`Queue`] is
//!
//! A list in memory of what is waiting, what is parked, the one piece under
//! way with how far it has got, and what failed. It lives on
//! [`crate::service::Service`] (`Service::listening`) and goes when the vault
//! locks; what lasts is whatever the work wrote to the files folder.
//!
//! - **Once.** What is waiting or under way is not put on the list again, so
//!   asking twice -- a policy, then a button -- is one piece of work. A task
//!   may say how two asks for the same thing are folded together
//!   ([`Task::absorb`]).
//! - **A failure is kept until it is asked for again.** That is the retry.
//!   Only [`MAX_FAILED`] are kept.
//! - **Parked work** waits for something outside the queue, a download, and
//!   goes back on the list when the worker says it is over.
//! - **An interrupted piece goes back to the front.** The future that was doing
//!   it is gone when the task stops, and nothing else will ever end it.
//! - **A name can wait.** Asking is quick and never reads the vault -- it is
//!   called from places that must not -- so a piece of work may be put on the
//!   list with no title, and the task looks it up on the blocking pool before
//!   it does anything else ([`Worker::name`]).
//!
//! # What a [`Worker`] is
//!
//! The one thing that differs between kinds of work: how one piece is done.
//! [`ensure_task`] starts the supervised task, which looks the vault up each
//! time it starts -- see `news::ensure_task` for why a vault held by a factory
//! the supervisor keeps for good would be wrong -- and [`run`] and [`pass`]
//! are its loop.

use super::{WorkItem, WorkKind, WorkState};
use crate::files;
use crate::service::Service;
use crate::supervisor::{Outcome, TaskResult};
use futures::future::BoxFuture;
use karmayoga_core::Vault;
use karmayoga_core::id::{FileId, StoryId};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, watch};

/// How many failures are kept to be shown before the oldest is forgotten.
pub const MAX_FAILED: usize = 50;

/// How often the task looks again while something is parked.
const PARKED_EVERY: Duration = Duration::from_secs(5);

/// One kind of work: what it is called in a list and when two asks for it are
/// the same.
pub trait Task: Copy + Send + Sync + 'static {
    /// Whether `other` asks for the same thing as `self`.
    fn same(self, other: Self) -> bool;

    /// Fold a second ask for the same thing into `self`, which is waiting.
    /// Nothing by default.
    fn absorb(&mut self, _again: Self) {}

    fn kind(self) -> WorkKind;

    fn story(self) -> Option<StoryId>;

    fn file(self) -> Option<FileId> {
        None
    }

    /// What a list calls it before anybody has looked its name up.
    fn unnamed(self) -> &'static str;

    /// Whether how far a piece has got is worth showing.
    fn shows_progress(self) -> bool {
        true
    }
}

/// How far a piece of work has got, written by the work and read by the list.
pub trait Measured: Default + Send + Sync + 'static {
    /// From 0 to 1, when it can be told.
    fn fraction(&self) -> Option<f32>;
}

/// How a piece of work came out.
#[derive(Debug, Clone)]
pub enum Ended<T> {
    Finished,
    /// Waiting for something outside the queue; goes back on the list when
    /// [`Queue::release_parked`] says so. The work is as it should be asked for
    /// then, which may not be as it was.
    Parked(T),
    /// A sentence fit to show, with no address in it.
    Failed(String),
}

/// What is waiting, parked, under way and failed. See the module doc.
pub struct Queue<T: Task, P: Measured> {
    state: Mutex<State<T, P>>,
    /// Rung when something is added to the list; the task waits on it between
    /// passes. A ring with nobody waiting is kept, so one that lands while a
    /// pass is running is not lost.
    wake: Notify,
}

struct State<T, P> {
    waiting: VecDeque<Waiting<T>>,
    parked: Vec<Waiting<T>>,
    running: Option<Running<T, P>>,
    /// Kept until the same thing is asked for again, or the vault locks.
    failed: VecDeque<Failure<T>>,
}

struct Waiting<T> {
    work: T,
    /// What the interface calls it; empty until it has been looked up.
    title: String,
}

struct Running<T, P> {
    work: T,
    title: String,
    progress: Arc<P>,
}

struct Failure<T> {
    work: T,
    title: String,
    error: String,
}

impl<T: Task, P: Measured> Default for Queue<T, P> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                waiting: VecDeque::new(),
                parked: Vec::new(),
                running: None,
                failed: VecDeque::new(),
            }),
            wake: Notify::new(),
        }
    }
}

impl<T: Task, P: Measured> Queue<T, P> {
    /// Forget everything: the vault locked.
    pub fn clear(&self) {
        let mut state = self.state();
        state.waiting.clear();
        state.parked.clear();
        state.running = None;
        state.failed.clear();
    }

    fn state(&self) -> MutexGuard<'_, State<T, P>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Put `work` on the list. Answers whether it is new: what is already
    /// waiting is left in its place, though a second ask is folded into it
    /// ([`Task::absorb`]) and not lost; and what is under way is left alone.
    /// Asking again for what failed forgets the failure: that is the retry.
    ///
    /// `title` may be empty, to be looked up by the task.
    pub fn queue(&self, work: T, title: String) -> bool {
        let mut guard = self.state();
        // Through a plain reference, so that the two lists are borrowed apart.
        let state = &mut *guard;
        if state.running.as_ref().is_some_and(|r| r.work.same(work)) {
            return false;
        }
        if let Some(there) =
            state.waiting.iter_mut().chain(state.parked.iter_mut()).find(|w| w.work.same(work))
        {
            there.work.absorb(work);
            return false;
        }
        state.failed.retain(|f| !f.work.same(work));
        state.waiting.push_back(Waiting { work, title });
        true
    }

    /// Ring the task: something is on the list.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Take the next piece of work off the list and mark it as under way.
    pub fn start_next(&self) -> Option<(T, String, Arc<P>)> {
        let mut state = self.state();
        let Waiting { work, title } = state.waiting.pop_front()?;
        let progress = Arc::new(P::default());
        state.running = Some(Running { work, title: title.clone(), progress: progress.clone() });
        Some((work, title, progress))
    }

    /// What is under way is, from now, `work`: a transcript that is written and
    /// has its article to write. A failure from here on is the article's.
    pub fn switch_running(&self, work: T) {
        if let Some(running) = self.state().running.as_mut() {
            running.work = work;
        }
    }

    /// Record how a piece of work came out.
    pub fn end(&self, ended: Ended<T>) {
        let mut state = self.state();
        let Some(running) = state.running.take() else { return };
        match ended {
            Ended::Finished => {}
            Ended::Parked(work) => state.parked.push(Waiting { work, title: running.title }),
            Ended::Failed(error) => {
                state.failed.push_back(Failure { work: running.work, title: running.title, error });
                while state.failed.len() > MAX_FAILED {
                    state.failed.pop_front();
                }
            }
        }
    }

    /// Whatever was under way when the task was stopped goes back to the
    /// front of the list.
    pub fn requeue_interrupted(&self) {
        let mut state = self.state();
        if let Some(running) = state.running.take() {
            state.waiting.push_front(Waiting { work: running.work, title: running.title });
        }
    }

    /// Put back on the list every parked piece of work that `still_waiting`
    /// does not say is still waiting.
    pub fn release_parked(&self, still_waiting: impl Fn(T) -> bool) {
        let mut state = self.state();
        for waiting in std::mem::take(&mut state.parked) {
            if still_waiting(waiting.work) {
                state.parked.push(waiting);
            } else {
                state.waiting.push_back(waiting);
            }
        }
    }

    pub fn has_parked(&self) -> bool {
        !self.state().parked.is_empty()
    }

    /// What was asked for with no title, and still has none.
    pub fn unnamed(&self) -> Vec<T> {
        let state = self.state();
        state
            .waiting
            .iter()
            .chain(state.parked.iter())
            .filter(|w| w.title.is_empty())
            .map(|w| w.work)
            .collect()
    }

    /// Give what was asked for with no title its name.
    pub fn name(&self, work: T, title: &str) {
        let mut guard = self.state();
        let state = &mut *guard;
        for waiting in state.waiting.iter_mut().chain(state.parked.iter_mut()) {
            if waiting.work.same(work) && waiting.title.is_empty() {
                waiting.title = title.to_string();
            }
        }
    }

    /// The list as the interface reads it: what is under way, then what is
    /// waiting, then what failed.
    pub fn statuses(&self) -> Vec<WorkItem> {
        let state = self.state();
        let item = |work: T, title: &str, state, progress, error| WorkItem {
            kind: work.kind(),
            story_id: work.story(),
            file_id: work.file(),
            title: if title.is_empty() { work.unnamed() } else { title }.to_string(),
            state,
            progress,
            error,
        };
        let running = state.running.iter().map(|r| {
            let progress = if r.work.shows_progress() { r.progress.fraction() } else { None };
            item(r.work, &r.title, WorkState::Working, progress, None)
        });
        let waiting = state
            .waiting
            .iter()
            .chain(state.parked.iter())
            .map(|w| item(w.work, &w.title, WorkState::Waiting, None, None));
        let failed = state
            .failed
            .iter()
            .map(|f| item(f.work, &f.title, WorkState::Failed, None, Some(f.error.clone())));
        running.chain(waiting).chain(failed).collect()
    }

    /// How many are waiting, for a test to look at.
    #[cfg(test)]
    pub fn waiting_len(&self) -> usize {
        self.state().waiting.len()
    }

    #[cfg(test)]
    pub fn parked_len(&self) -> usize {
        self.state().parked.len()
    }
}

// ── The task ─────────────────────────────────────────────────────────────

/// How one kind of work is done, and where its list is.
pub trait Worker: Clone + Send + Sync + 'static {
    type Task: Task;
    type Progress: Measured;

    /// The supervisor key of the task.
    const KEY: &'static str;

    fn queue(svc: &Service) -> &Queue<Self::Task, Self::Progress>;

    /// Do one piece of work, and say how it came out.
    fn work<'a>(
        &'a self,
        svc: &'a Arc<Service>,
        vault: &'a Arc<Vault>,
        task: Self::Task,
        progress: &'a Arc<Self::Progress>,
    ) -> BoxFuture<'a, Ended<Self::Task>>;

    /// Before each pass, quickly and with no reading of the vault: what is
    /// parked may be put back. Nothing by default.
    fn before_pass(&self, _svc: &Service) {}

    /// The title of a piece of work asked for with none. Nothing by default,
    /// for work that is never asked for unnamed.
    fn name<'a>(
        &'a self,
        _svc: &'a Arc<Service>,
        _vault: &'a Arc<Vault>,
        _task: Self::Task,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
}

/// Whether `vault` is one a task of this kind may work in. Only where this
/// process may write: what comes of the work is linked to a story and kept in
/// the files folder, and a second window that merely reads must not spend
/// minutes making what it cannot keep.
fn can_work(vault: &Vault) -> bool {
    vault.is_writable() && vault.supports_news() && vault.supports_files()
}

/// Start `worker`'s task if it is not already running.
pub fn ensure_task<K: Worker>(svc: &Arc<Service>, vault: &Arc<Vault>, worker: K) {
    if !can_work(vault) {
        return;
    }
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(K::KEY, move |stop| {
        let (svc, worker) = (svc_for_task.clone(), worker.clone());
        Box::pin(async move {
            match svc.get() {
                Some(vault) if can_work(&vault) => run(&worker, svc, vault, stop).await,
                _ => Ok(Outcome::Done),
            }
        })
    });
}

/// The supervised loop: a pass at the start, then one whenever something is
/// asked for, and every few seconds while something is parked. Returns when
/// `stop` turns `true`.
pub async fn run<K: Worker>(
    worker: &K,
    svc: Arc<Service>,
    vault: Arc<Vault>,
    mut stop: watch::Receiver<bool>,
) -> TaskResult {
    K::queue(&svc).requeue_interrupted();
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        // Locked between one pass and the next: nothing can be written and
        // nothing should be worked on. The supervisor stops this task on the
        // lock in any case; until it does, wait.
        if files::still_open(&svc, &vault) && pass(worker, &svc, &vault, &mut stop).await {
            return Ok(Outcome::Done);
        }
        let parked = K::queue(&svc).has_parked();
        tokio::select! {
            () = K::queue(&svc).wake.notified() => {}
            () = tokio::time::sleep(PARKED_EVERY), if parked => {}
            stopping = stop.changed() => {
                if stopping.is_err() {
                    return Ok(Outcome::Done);
                }
            }
        }
    }
}

/// One pass: everything on the list, one at a time. Answers whether the task
/// was asked to stop in the middle of it.
pub async fn pass<K: Worker>(
    worker: &K,
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    let queue = K::queue(svc);
    worker.before_pass(svc);
    for task in queue.unnamed() {
        if let Some(title) = worker.name(svc, vault, task).await {
            queue.name(task, &title);
        }
    }
    while let Some((task, _title, progress)) = queue.start_next() {
        let mut doing = Box::pin(worker.work(svc, vault, task, &progress));
        let ended = loop {
            tokio::select! {
                ended = &mut doing => break ended,
                stopping = stop.changed() => {
                    // Dropping `doing` ends the work at its next await; it is
                    // on the list again, and nobody waits on it.
                    if stopping.is_err() || *stop.borrow() {
                        drop(doing);
                        queue.requeue_interrupted();
                        return true;
                    }
                }
            }
        };
        queue.end(ended);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Work that is a number: the same number is the same work.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Number {
        n: u32,
        /// How many times it was asked for while it waited.
        asked: u32,
    }

    impl Number {
        fn new(n: u32) -> Self {
            Self { n, asked: 1 }
        }
    }

    impl Task for Number {
        fn same(self, other: Self) -> bool {
            self.n == other.n
        }

        fn absorb(&mut self, again: Self) {
            self.asked += again.asked;
        }

        fn kind(self) -> WorkKind {
            WorkKind::Audio
        }

        fn story(self) -> Option<StoryId> {
            None
        }

        fn unnamed(self) -> &'static str {
            "Something"
        }
    }

    #[derive(Default)]
    struct Done(AtomicU32);

    impl Measured for Done {
        fn fraction(&self) -> Option<f32> {
            let thousandths = self.0.load(Ordering::Relaxed);
            (thousandths > 0).then(|| thousandths as f32 / 1000.0)
        }
    }

    type List = Queue<Number, Done>;

    fn titles(list: &List) -> Vec<(String, WorkState)> {
        list.statuses().into_iter().map(|i| (i.title, i.state)).collect()
    }

    #[test]
    fn something_is_on_the_list_once_and_a_second_ask_is_folded_in() {
        let list = List::default();
        assert!(list.queue(Number::new(1), "One".into()));
        assert!(!list.queue(Number::new(1), "One".into()), "asking twice is one piece of work");
        assert!(list.queue(Number::new(2), "Two".into()));
        let (first, title, _) = list.start_next().expect("one is next");
        assert_eq!((first.n, first.asked, title.as_str()), (1, 2, "One"));
        assert!(!list.queue(Number::new(1), "One".into()), "nor while it is under way");
        assert_eq!(list.waiting_len(), 1);
        assert_eq!(
            titles(&list),
            [("One".to_string(), WorkState::Working), ("Two".to_string(), WorkState::Waiting)]
        );
    }

    #[test]
    fn progress_is_shown_only_when_there_is_some_to_show() {
        let list = List::default();
        list.queue(Number::new(1), "One".into());
        let (_, _, progress) = list.start_next().unwrap();
        assert_eq!(list.statuses()[0].progress, None);
        progress.0.store(250, Ordering::Relaxed);
        assert_eq!(list.statuses()[0].progress, Some(0.25));
    }

    #[test]
    fn a_failure_is_kept_until_it_is_asked_for_again_and_a_finish_leaves_nothing() {
        let list = List::default();
        list.queue(Number::new(1), "One".into());
        list.start_next().unwrap();
        list.end(Ended::Failed("It went wrong.".into()));
        let items = list.statuses();
        assert_eq!((items.len(), items[0].state), (1, WorkState::Failed));
        assert_eq!(items[0].error.as_deref(), Some("It went wrong."));

        // Another thing's asking is not this one's to forget; its own is.
        assert!(list.queue(Number::new(2), "Two".into()));
        assert_eq!(list.statuses().iter().filter(|i| i.state == WorkState::Failed).count(), 1);
        assert!(list.queue(Number::new(1), "One".into()), "the retry");
        assert_eq!(list.statuses().iter().filter(|i| i.state == WorkState::Failed).count(), 0);

        list.start_next().unwrap();
        list.end(Ended::Finished);
        assert_eq!(titles(&list), [("One".to_string(), WorkState::Waiting)]);
    }

    #[test]
    fn only_so_many_failures_are_kept() {
        let list = List::default();
        for n in 0..MAX_FAILED as u32 + 5 {
            list.queue(Number::new(n), "x".into());
            list.start_next().unwrap();
            list.end(Ended::Failed("no".into()));
        }
        assert_eq!(list.statuses().len(), MAX_FAILED);
    }

    #[test]
    fn what_was_under_way_when_the_task_stopped_goes_to_the_front() {
        let list = List::default();
        list.queue(Number::new(1), "One".into());
        list.queue(Number::new(2), "Two".into());
        list.start_next().unwrap();
        list.requeue_interrupted();
        assert_eq!(list.start_next().unwrap().0.n, 1);
        list.requeue_interrupted();
        list.requeue_interrupted();
        assert_eq!(list.waiting_len(), 2, "a second stop with nothing under way changes nothing");
        // Ended with nothing under way, it is nothing.
        list.end(Ended::Failed("x".into()));
        assert_eq!(list.statuses().len(), 2);
    }

    #[test]
    fn parked_work_comes_back_when_what_it_waits_for_is_over_and_a_lock_forgets_everything() {
        let list = List::default();
        for n in [1, 2] {
            list.queue(Number::new(n), "x".into());
            list.start_next().unwrap();
            list.end(Ended::Parked(Number::new(n)));
        }
        assert!(list.has_parked());
        list.release_parked(|work| work.n == 1);
        assert_eq!((list.parked_len(), list.waiting_len()), (1, 1));
        assert_eq!(list.statuses().len(), 2, "both are shown as waiting");
        // A second ask for what is parked is folded into it, not added.
        assert!(!list.queue(Number::new(1), "x".into()));
        list.clear();
        assert!(list.statuses().is_empty());
        assert!(!list.has_parked());
    }

    #[test]
    fn what_is_under_way_can_become_other_work() {
        let list = List::default();
        list.queue(Number::new(1), "One".into());
        list.start_next().unwrap();
        list.switch_running(Number::new(7));
        list.end(Ended::Failed("The second part failed.".into()));
        // The failure is the second thing's: asking for the first is a new
        // ask, and forgets nothing.
        assert!(list.queue(Number::new(1), "One".into()));
        let failed =
            |list: &List| list.statuses().iter().filter(|i| i.state == WorkState::Failed).count();
        assert_eq!(failed(&list), 1);
        assert!(list.queue(Number::new(7), "Seven".into()));
        assert_eq!(failed(&list), 0);
    }

    #[test]
    fn a_piece_of_work_asked_for_with_no_title_is_named_later() {
        let list = List::default();
        list.queue(Number::new(1), String::new());
        list.queue(Number::new(2), "Two".into());
        assert_eq!(titles(&list)[0].0, "Something", "until it is looked up");
        assert_eq!(list.unnamed().iter().map(|w| w.n).collect::<Vec<_>>(), [1]);
        list.name(Number::new(1), "One");
        assert!(list.unnamed().is_empty());
        assert_eq!(titles(&list)[0].0, "One");
        // A name is not given over one that is there.
        list.name(Number::new(1), "Other");
        assert_eq!(titles(&list)[0].0, "One");
    }
}
