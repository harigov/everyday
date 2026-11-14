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
use std::sync::{Arc, Mutex, OnceLock};
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
    model: Arc<T>,
    last_used: Instant,
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
    /// than starting one of its own.
    pub fn get(
        &'static self,
        key: K,
        load: impl FnOnce() -> CommandResult<T>,
    ) -> CommandResult<Arc<T>> {
        let mut slots = self.lock();
        if let Some(slot) = slots.iter_mut().find(|s| s.key == key) {
            slot.last_used = Instant::now();
            return Ok(slot.model.clone());
        }
        let started = Instant::now();
        let model = Arc::new(load()?);
        tracing::info!(
            what = self.what,
            key = ?key,
            ms = started.elapsed().as_millis() as u64,
            "loaded a local model"
        );
        slots.push(Slot { key, model: model.clone(), last_used: Instant::now() });
        drop(slots);
        watch(self);
        Ok(model)
    }

    /// Let go of `key` now rather than once it is idle -- before its file is
    /// deleted, say. Answers whether it is gone: one somebody is still using
    /// is kept, and `false` says so, since letting go of it would free
    /// nothing and leave its file open underneath whoever deletes it.
    pub fn forget(&self, key: &K) -> bool {
        let mut slots = self.lock();
        if slots.iter().any(|s| s.key == *key && Arc::strong_count(&s.model) > 1) {
            return false;
        }
        slots.retain(|s| s.key != *key);
        true
    }

    /// Whether `key` is in memory right now.
    pub fn is_loaded(&self, key: &K) -> bool {
        self.lock().iter().any(|s| &s.key == key)
    }

    /// Let go of every model nobody has asked for in [`IDLE_AFTER`] as of
    /// `now`. One a caller still holds counts as used just now.
    fn sweep_at(&self, now: Instant) {
        self.lock().retain_mut(|slot| {
            if Arc::strong_count(&slot.model) > 1 {
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot<K, T>>> {
        // A poisoned lock means a load panicked part-way. The list is still
        // sound -- a model is only pushed once it has loaded -- so the next
        // caller simply tries again.
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
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
