//! The scheduler's bookkeeping about routines this process is running or
//! has already reported trouble for.

use karmayoga_core::RoutineId;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Default)]
pub(crate) struct RoutineRuntime {
    /// Routines whose failure has already been reported this session. See
    /// [`RoutineRuntime::failed`].
    reported: RwLock<HashSet<String>>,
    /// Runs this process is carrying out, by id.
    ///
    /// A row in the vault reading `Running` says one of two things and
    /// cannot tell them apart on its own: a run happening now, or one a
    /// process that died left behind. This says which, because it exists
    /// only in memory -- a run this process is not carrying out is not in
    /// here, whatever the row says.
    claimed: RwLock<HashSet<String>>,
    /// The routines running right now, by name, oldest first. The tray
    /// names the newest.
    ///
    /// Here rather than derived from a `Running` row, because a row is also
    /// what a run abandoned by a dead process looks like. This is in
    /// memory and therefore cannot lie about the present. A list because a
    /// run that can ask its person runs beside the tick rather than in it
    /// -- see [`RoutineRuntime::detach`] -- so two can be going at once.
    running: RwLock<Vec<String>>,
    /// Runs carried out beside the tick rather than in it: the ones that may
    /// wait half an hour on somebody's answer, which must not hold up the
    /// meeting watcher, the snooze sweep and every other routine meanwhile.
    detached: Mutex<Vec<JoinHandle<()>>>,
    /// The routines one of those runs belongs to, so the tick does not
    /// start the same slot again -- or record it missed -- while it is still
    /// going.
    in_flight: Arc<Mutex<HashSet<RoutineId>>>,
    /// The model's passes over mail are going on beside the tick.
    mail_passes: Arc<AtomicBool>,
    /// They were asked for again while they were going -- new mail landed --
    /// so they run once more as soon as they end.
    mail_passes_again: Arc<AtomicBool>,
    /// Flipped to tell every run waiting on an answer to stop waiting: the
    /// scheduler is stopping, or the vault is closing. Replaced by a fresh
    /// one each time, so a run started afterwards is not born withdrawn.
    withdraw: Mutex<Option<watch::Sender<bool>>>,
}

impl RoutineRuntime {
    /// Note that a routine's run failed. True the first time, so a routine
    /// whose endpoint has been unreachable every morning for a week says so
    /// once rather than seven times.
    pub(crate) fn failed(&self, id: String) -> bool {
        self.reported.write().unwrap().insert(id)
    }

    /// Note that a routine ran, so its next failure is news.
    pub(crate) fn recovered(&self, id: &str) {
        self.reported.write().unwrap().remove(id);
    }

    pub(crate) fn claim(&self, id: String) {
        self.claimed.write().unwrap().insert(id);
    }

    pub(crate) fn release(&self, id: &str) {
        self.claimed.write().unwrap().remove(id);
    }

    /// Is this process carrying out that run?
    pub(crate) fn claims(&self, id: &str) -> bool {
        self.claimed.read().unwrap().contains(id)
    }

    /// `Some` adds a running routine; `None` forgets them all.
    pub(crate) fn set_running(&self, name: Option<String>) {
        let mut running = self.running.write().unwrap();
        match name {
            Some(name) => running.push(name),
            None => running.clear(),
        }
    }

    /// One routine has finished, whichever others are still going.
    pub(crate) fn finished_running(&self, name: &str) {
        let mut running = self.running.write().unwrap();
        if let Some(at) = running.iter().position(|n| n == name) {
            running.remove(at);
        }
    }

    pub(crate) fn running(&self) -> Option<String> {
        self.running.read().unwrap().last().cloned()
    }

    /// Whether a detached run of `routine` is still going.
    pub(crate) fn in_flight(&self, routine: RoutineId) -> bool {
        self.in_flight.lock().unwrap().contains(&routine)
    }

    /// Carry `run` out beside the tick. `routine`, when given, is held in
    /// flight until it finishes, however it finishes -- including an abort.
    pub(crate) fn detach(
        &self,
        routine: Option<RoutineId>,
        run: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        struct Landed(Arc<Mutex<HashSet<RoutineId>>>, Option<RoutineId>);
        impl Drop for Landed {
            fn drop(&mut self) {
                if let Some(id) = self.1 {
                    self.0.lock().unwrap().remove(&id);
                }
            }
        }
        if let Some(id) = routine {
            self.in_flight.lock().unwrap().insert(id);
        }
        let landed = Landed(self.in_flight.clone(), routine);
        let handle = tokio::spawn(async move {
            let _landed = landed;
            run.await;
        });
        let mut detached = self.detached.lock().unwrap();
        detached.retain(|h| !h.is_finished());
        detached.push(handle);
    }

    /// [`RoutineRuntime::detach`] the model's passes over mail. When the last
    /// ones are still going, `again` has them run once more as soon as they
    /// end -- new mail that lands during a pass is asked about straight after
    /// it rather than at the next tick -- and without it the ask is dropped,
    /// as the scheduler's own minute tick wants: on a slow endpoint it would
    /// otherwise run them back to back whether or not anything arrived.
    /// Marked done however they finish, an abort on lock and a panic among
    /// them, or they would never run again.
    ///
    /// An ask that lands in the moment between the last run ending and the
    /// flag going down is left to the next tick, a minute at most.
    pub(crate) fn detach_mail_passes<F, Fut>(&self, again: bool, passes: F)
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        struct Landed(Arc<AtomicBool>);
        impl Drop for Landed {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        if self.mail_passes.swap(true, Ordering::SeqCst) {
            if again {
                self.mail_passes_again.store(true, Ordering::SeqCst);
            }
            return;
        }
        let landed = Landed(self.mail_passes.clone());
        let again = self.mail_passes_again.clone();
        self.detach(None, async move {
            let _landed = landed;
            loop {
                again.store(false, Ordering::SeqCst);
                passes().await;
                if !again.load(Ordering::SeqCst) {
                    break;
                }
            }
        });
    }

    /// Wait for every detached run to finish.
    pub(crate) async fn settle(&self) {
        loop {
            let next = self.detached.lock().unwrap().pop();
            let Some(handle) = next else { return };
            let _ = handle.await;
        }
    }

    /// A signal that turns true when waiting runs are told to stop waiting.
    pub(crate) fn withdrawn(&self) -> watch::Receiver<bool> {
        let mut withdraw = self.withdraw.lock().unwrap();
        withdraw.get_or_insert_with(|| watch::channel(false).0).subscribe()
    }

    /// Tell every run waiting on an answer to stop waiting, now.
    pub(crate) fn withdraw(&self) {
        if let Some(withdraw) = self.withdraw.lock().unwrap().take() {
            withdraw.send_replace(true);
        }
    }

    // ---- what a full vault close clears ----------------------------------
    //
    // `on_lock` bundles these four, in this order: detached runs, reported
    // failures, claimed runs, then the running routines' names. Not called by
    // `Service::locked` -- a lock screen leaves a routine's claim and the
    // "already reported" flags alone, only a full close resets them. See
    // `tests/runtime_lifecycle.rs`.

    fn forget_reported(&self) {
        self.reported.write().unwrap().clear();
    }

    fn forget_claimed(&self) {
        self.claimed.write().unwrap().clear();
    }

    fn forget_running(&self) {
        self.running.write().unwrap().clear();
    }

    /// Detached runs are stopped where they stand: the vault they write to is
    /// going. A row one leaves `Running` is closed by the next unlock's sweep,
    /// like any other a stopped process left.
    fn forget_detached(&self) {
        self.withdraw();
        for handle in self.detached.lock().unwrap().drain(..) {
            handle.abort();
        }
        self.in_flight.lock().unwrap().clear();
    }

    /// Called once, by [`Service::close`](crate::service::Service::close),
    /// in place of the `forget_*` calls above written out by hand, in the
    /// exact order they happened there -- with the detached runs first,
    /// which are newer than that list.
    pub(crate) fn on_lock(&self) {
        self.forget_detached();
        self.forget_reported();
        self.forget_claimed();
        self.forget_running();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// New mail that lands while the model's passes are going has them go
    /// once more when they end -- once, however many times it was asked.
    #[tokio::test]
    async fn mail_passes_asked_for_while_going_go_once_more() {
        let runtime = RoutineRuntime::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let (release, released) = watch::channel(false);
        let passes = {
            let runs = runs.clone();
            move || {
                let runs = runs.clone();
                let mut released = released.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    let _ = released.wait_for(|r| *r).await;
                }
            }
        };
        runtime.detach_mail_passes(false, passes.clone());
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        runtime.detach_mail_passes(false, passes.clone());
        assert!(!runtime.mail_passes_again.load(Ordering::SeqCst), "the tick's ask is dropped");
        runtime.detach_mail_passes(true, passes.clone());
        runtime.detach_mail_passes(true, passes.clone());
        release.send_replace(true);
        runtime.settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);

        runtime.detach_mail_passes(false, passes);
        runtime.settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 3, "and they run again when asked after");
    }
}
