//! A loaded model, kept in memory while it is used and let go of once it is
//! not.
//!
//! A model on this computer is hundreds of megabytes to several gigabytes
//! once loaded, and loading one again takes a few seconds. Holding every
//! model the app has used since it started -- a recogniser from this
//! morning's call, a decision model from the last mail sync -- would add
//! those up for no reason; loading one afresh for every request would spend
//! those seconds on every message. [`Resident`] is the middle: the first
//! request loads it, later ones share it, and a sweep lets it go once
//! [`IDLE_AFTER`] has passed with nobody asking for it.
//!
//! # Never while it is being used
//!
//! Callers get an [`Arc`]. A model somebody still holds -- a pipeline an hour
//! into transcribing a long call, say -- counts as in use however long ago it
//! was handed out: the sweep sees the extra reference and leaves it alone.
//! Letting go of it then would free nothing (the caller's `Arc` keeps it
//! alive) and the next request would load a second copy beside it.
//!
//! # One load per model, and only that model waits
//!
//! A model being loaded has its own slot, marked as loading, and the list's
//! lock is not held while it loads: asking for another model -- or whether
//! one is loaded -- never waits seconds behind somebody else's load. A
//! second caller for the same model waits for that load rather than starting
//! one of its own.
//!
//! # Failures are not kept
//!
//! A load that fails -- the file is not downloaded yet, say -- is not
//! remembered. The next request tries again, so downloading the model is
//! enough, with no restart.
//!
//! # The sweep
//!
//! One thread for the whole process, started the first time anything is
//! loaded, wakes every [`SWEEP_EVERY`] and asks each [`Resident`] that has
//! ever loaded something to let go of what has sat idle. It owns nothing and
//! sleeps through a process with no model loaded at all.

use std::fmt::Debug;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::CommandResult;

/// How long a loaded model may go unused before it is let go of.
pub const IDLE_AFTER: Duration = Duration::from_secs(15 * 60);
/// How often the sweep looks. A model is let go of within this long after
/// [`IDLE_AFTER`] has passed.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// Models of one kind, by key -- which recogniser, which decision model --
/// each loaded on first use and let go of once idle. Built in a `static`, so
/// the sweep can hold on to it.
pub struct Resident<K, T> {
    /// What these are, for the log.
    what: &'static str,
    idle_after: Duration,
    /// A handful at most -- one per model somebody has picked -- so a list,
    /// which unlike a map can be built in a `static`.
    slots: Mutex<Vec<Slot<K, T>>>,
}

struct Slot<K, T> {
    key: K,
    state: State<T>,
    last_used: Instant,
}

enum State<T> {
    /// Somebody is loading it; wait on this for them to finish.
    Loading(Arc<Loading>),
    Ready(Arc<T>),
}

/// A load under way, for those waiting on it. Finished once the slot says how
/// it went: ready, or gone for a load that failed.
#[derive(Default)]
struct Loading {
    finished: Mutex<bool>,
    done: Condvar,
}

impl Loading {
    fn wait(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        while !*finished {
            finished = self.done.wait(finished).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn finish(&self) {
        *self.finished.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.done.notify_all();
    }
}

/// The end of one load, whichever way it ends -- a panic in `load` included:
/// the slot is made ready, or taken out if nothing was published, and
/// whoever waits is woken to look again.
struct Finish<'a, K: PartialEq + Clone, T> {
    resident: &'a Resident<K, T>,
    key: &'a K,
    loading: Arc<Loading>,
    published: bool,
}

impl<K: PartialEq + Clone, T> Finish<'_, K, T> {
    fn publish(&mut self, model: Arc<T>) {
        let mut slots = self.resident.lock();
        match slots.iter_mut().find(|s| s.key == *self.key) {
            Some(slot) => {
                slot.state = State::Ready(model);
                slot.last_used = Instant::now();
            }
            // Not reached -- nothing takes out a slot being loaded but its
            // loader -- but a model that loaded is kept either way.
            None => slots.push(Slot {
                key: self.key.clone(),
                state: State::Ready(model),
                last_used: Instant::now(),
            }),
        }
        self.published = true;
    }
}

impl<K: PartialEq + Clone, T> Drop for Finish<'_, K, T> {
    fn drop(&mut self) {
        if !self.published {
            let loading = &self.loading;
            self.resident.lock().retain(|s| {
                !(s.key == *self.key
                    && matches!(&s.state, State::Loading(l) if Arc::ptr_eq(l, loading)))
            });
        }
        self.loading.finish();
    }
}

impl<K, T> Resident<K, T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot<K, T>>> {
        // A poisoned lock means something panicked while holding it, which
        // nothing here does across a load. The list is still sound -- a
        // model is only made ready once it has loaded -- so the next caller
        // simply goes on.
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<K, T> Resident<K, T>
where
    K: PartialEq + Clone + Debug + Send + 'static,
    T: Send + Sync + 'static,
{
    pub const fn new(what: &'static str) -> Self {
        Self::with_idle(what, IDLE_AFTER)
    }

    const fn with_idle(what: &'static str, idle_after: Duration) -> Self {
        Self { what, idle_after, slots: Mutex::new(Vec::new()) }
    }

    /// The model for `key`, loading it with `load` if it is not in memory.
    /// Blocking, and `load` usually is too: call from a blocking thread.
    ///
    /// A second caller for a model being loaded waits for that load rather
    /// than starting one of its own -- and, if it failed, tries it again
    /// itself. Callers for other models do not wait at all.
    pub fn get(
        &'static self,
        key: K,
        load: impl FnOnce() -> CommandResult<T>,
    ) -> CommandResult<Arc<T>> {
        let loading = loop {
            let waiting = {
                let mut slots = self.lock();
                match slots.iter_mut().find(|s| s.key == key) {
                    Some(slot) => match &slot.state {
                        State::Ready(model) => {
                            slot.last_used = Instant::now();
                            return Ok(model.clone());
                        }
                        State::Loading(loading) => loading.clone(),
                    },
                    None => {
                        let loading = Arc::new(Loading::default());
                        slots.push(Slot {
                            key: key.clone(),
                            state: State::Loading(loading.clone()),
                            last_used: Instant::now(),
                        });
                        break loading;
                    }
                }
            };
            waiting.wait();
        };
        let mut finish = Finish { resident: self, key: &key, loading, published: false };
        let started = Instant::now();
        let model = Arc::new(load()?);
        tracing::info!(
            what = self.what,
            key = ?key,
            ms = started.elapsed().as_millis() as u64,
            "loaded a local model"
        );
        finish.publish(model.clone());
        drop(finish);
        watch(self);
        Ok(model)
    }

    /// Let go of `key` now rather than once it is idle -- before its file is
    /// deleted, say. Answers whether it is gone: one somebody is still using
    /// is kept, and `false` says so, since letting go of it would free
    /// nothing and leave its file open underneath whoever deletes it. Waits
    /// for a load of it in progress, whose caller is then using it.
    pub fn forget(&self, key: &K) -> bool {
        loop {
            let waiting = {
                let mut slots = self.lock();
                let Some(at) = slots.iter().position(|s| s.key == *key) else {
                    return true;
                };
                match &slots[at].state {
                    State::Loading(loading) => loading.clone(),
                    State::Ready(model) => {
                        if Arc::strong_count(model) > 1 {
                            return false;
                        }
                        slots.remove(at);
                        return true;
                    }
                }
            };
            waiting.wait();
        }
    }

    /// Whether `key` is in memory right now. Never waits for a load: one
    /// still loading is not in memory yet.
    pub fn is_loaded(&self, key: &K) -> bool {
        self.lock().iter().any(|s| &s.key == key && matches!(s.state, State::Ready(_)))
    }

    /// Whether any model of this kind is in memory right now.
    pub fn any_loaded(&self) -> bool {
        self.lock().iter().any(|s| matches!(s.state, State::Ready(_)))
    }

    /// The model for `key` if it is in memory, without loading it or
    /// counting as a use of it.
    pub fn peek(&self, key: &K) -> Option<Arc<T>> {
        self.lock().iter().find(|s| &s.key == key).and_then(|s| match &s.state {
            State::Ready(model) => Some(model.clone()),
            State::Loading(_) => None,
        })
    }

    /// Let go of every model nobody has asked for in [`IDLE_AFTER`] as of
    /// `now`. One a caller still holds counts as used just now; one being
    /// loaded is left to its loader.
    fn sweep_at(&self, now: Instant) {
        self.lock().retain_mut(|slot| {
            let State::Ready(model) = &slot.state else { return true };
            if Arc::strong_count(model) > 1 {
                slot.last_used = now;
                return true;
            }
            if now.saturating_duration_since(slot.last_used) < self.idle_after {
                return true;
            }
            tracing::info!(what = self.what, key = ?slot.key, "let go of an idle local model");
            false
        });
    }
}

/// What the sweep asks of every [`Resident`], whatever it holds.
trait Sweep: Sync {
    fn sweep_at(&self, now: Instant);
}

impl<K, T> Sweep for Resident<K, T>
where
    K: PartialEq + Clone + Debug + Send + 'static,
    T: Send + Sync + 'static,
{
    fn sweep_at(&self, now: Instant) {
        Resident::sweep_at(self, now);
    }
}

/// Every [`Resident`] that has loaded something, for the sweep.
fn watched() -> &'static Mutex<Vec<&'static dyn Sweep>> {
    static WATCHED: Mutex<Vec<&'static dyn Sweep>> = Mutex::new(Vec::new());
    &WATCHED
}

/// Add `resident` to what the sweep looks at, starting the sweep if this is
/// the first.
fn watch(resident: &'static dyn Sweep) {
    {
        let mut watched = watched().lock().unwrap_or_else(|e| e.into_inner());
        let ptr = resident as *const dyn Sweep as *const ();
        if !watched.iter().any(|w| std::ptr::eq(*w as *const dyn Sweep as *const (), ptr)) {
            watched.push(resident);
        }
    }
    static SWEEPER: OnceLock<()> = OnceLock::new();
    SWEEPER.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("local-model-sweep".into()).spawn(|| {
            loop {
                std::thread::sleep(SWEEP_EVERY);
                let now = Instant::now();
                let watched = watched().lock().unwrap_or_else(|e| e.into_inner()).clone();
                for resident in watched {
                    resident.sweep_at(now);
                }
            }
        });
        if let Err(e) = spawned {
            // Models then stay loaded until the app quits, as they did
            // before there was a sweep; nothing else depends on it.
            tracing::warn!(error = %e, "could not start the sweep that unloads idle local models");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CommandError, codes};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    const IDLE: Duration = Duration::from_secs(60);

    fn leak<T: Send + Sync + 'static>(what: &'static str) -> &'static Resident<u8, T> {
        Box::leak(Box::new(Resident::with_idle(what, IDLE)))
    }

    #[test]
    fn a_model_is_loaded_once_and_shared() {
        let resident = leak::<String>("test");
        let loads = AtomicUsize::new(0);
        let load = || {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok("weights".to_string())
        };
        let a = resident.get(1, load).unwrap();
        let b = resident.get(1, || panic!("already loaded")).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_idle_model_is_let_go_of_and_loaded_again_when_asked() {
        let resident = leak::<String>("test");
        let at = Instant::now();
        drop(resident.get(1, || Ok("weights".into())).unwrap());

        resident.sweep_at(at + IDLE / 2);
        assert!(resident.is_loaded(&1), "not idle long enough yet");
        resident.sweep_at(at + IDLE * 2);
        assert!(!resident.is_loaded(&1));

        let again = resident.get(1, || Ok("reloaded".into())).unwrap();
        assert_eq!(*again, "reloaded");
    }

    /// The case the reference count is there for: a caller an hour into
    /// using it. Letting go would free nothing and load a second copy.
    #[test]
    fn a_model_somebody_still_holds_is_never_let_go_of() {
        let resident = leak::<String>("test");
        let at = Instant::now();
        let held = resident.get(1, || Ok("weights".into())).unwrap();

        resident.sweep_at(at + IDLE * 10);
        assert!(resident.is_loaded(&1));
        drop(held);
        // Seen in use at +10 idle periods; idle counts from there.
        resident.sweep_at(at + IDLE * 10 + IDLE / 2);
        assert!(resident.is_loaded(&1));
        resident.sweep_at(at + IDLE * 12);
        assert!(!resident.is_loaded(&1));
    }

    #[test]
    fn a_failed_load_is_not_remembered() {
        let resident = leak::<String>("test");
        let missing =
            resident.get(1, || Err(CommandError::new(codes::NOT_FOUND, "not downloaded")));
        assert!(missing.is_err());
        assert!(!resident.is_loaded(&1));
        assert!(resident.get(1, || Ok("downloaded since".into())).is_ok());
    }

    #[test]
    fn a_model_in_use_is_not_forgotten() {
        let resident = leak::<String>("test");
        let held = resident.get(1, || Ok("weights".into())).unwrap();
        assert!(!resident.forget(&1), "still in use");
        assert!(resident.is_loaded(&1));
        drop(held);
        assert!(resident.forget(&1));
        assert!(!resident.is_loaded(&1));
        assert!(resident.forget(&2), "nothing loaded is nothing to keep");
    }

    /// A loader for `value` that says when it has started and finishes only
    /// when told to.
    fn held_load(
        value: CommandResult<String>,
    ) -> (impl FnOnce() -> CommandResult<String>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let load = move || {
            started.send(()).unwrap();
            released.recv().unwrap();
            value
        };
        (load, has_started, release)
    }

    #[test]
    fn loading_one_model_keeps_nobody_waiting_for_another() {
        let resident = leak::<String>("test");
        let (load, has_started, release) = held_load(Ok("one".into()));
        let slow = thread::spawn(move || resident.get(1, load).unwrap());
        has_started.recv().unwrap();

        assert_eq!(*resident.get(2, || Ok("two".into())).unwrap(), "two");
        assert!(!resident.is_loaded(&1), "still loading");
        assert!(resident.peek(&1).is_none());
        assert!(resident.is_loaded(&2) && resident.any_loaded());

        release.send(()).unwrap();
        assert_eq!(*slow.join().unwrap(), "one");
        assert!(resident.is_loaded(&1));
    }

    #[test]
    fn two_callers_for_one_model_load_it_once() {
        let resident = leak::<String>("test");
        let (load, has_started, release) = held_load(Ok("one".into()));
        let first = thread::spawn(move || resident.get(1, load).unwrap());
        has_started.recv().unwrap();
        let second = thread::spawn(move || resident.get(1, || panic!("loaded twice")).unwrap());
        thread::sleep(Duration::from_millis(50));

        release.send(()).unwrap();
        let (a, b) = (first.join().unwrap(), second.join().unwrap());
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn a_caller_waiting_on_a_load_that_fails_tries_it_itself() {
        let resident = leak::<String>("test");
        let (load, has_started, release) =
            held_load(Err(CommandError::new(codes::NOT_FOUND, "not downloaded")));
        let first = thread::spawn(move || resident.get(1, load));
        has_started.recv().unwrap();
        let second = thread::spawn(move || resident.get(1, || Ok("downloaded since".into())));
        thread::sleep(Duration::from_millis(50));

        release.send(()).unwrap();
        assert!(first.join().unwrap().is_err());
        assert_eq!(*second.join().unwrap().unwrap(), "downloaded since");
    }

    #[test]
    fn a_load_that_panics_leaves_nothing_behind() {
        let resident = leak::<String>("test");
        let panicked = std::panic::catch_unwind(|| resident.get(1, || panic!("bad file")));
        assert!(panicked.is_err());
        assert!(!resident.is_loaded(&1));
        assert_eq!(*resident.get(1, || Ok("weights".into())).unwrap(), "weights");
    }

    #[test]
    fn each_key_is_its_own_model() {
        let resident = leak::<String>("test");
        let at = Instant::now();
        drop(resident.get(1, || Ok("one".into())).unwrap());
        resident.sweep_at(at + IDLE / 2);
        drop(resident.get(2, || Ok("two".into())).unwrap());
        assert_eq!(*resident.get(1, || panic!("loaded")).unwrap(), "one");
        // Both were used just now (1 by the get above).
        resident.sweep_at(Instant::now() + IDLE / 2);
        assert!(resident.is_loaded(&1) && resident.is_loaded(&2));
        resident.sweep_at(Instant::now() + IDLE * 2);
        assert!(!resident.is_loaded(&1) && !resident.is_loaded(&2));
    }
}
