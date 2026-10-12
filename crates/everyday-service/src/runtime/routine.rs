//! The scheduler's bookkeeping about routines this process is running or
//! has already reported trouble for.

use everyday_core::RoutineId;
use std::collections::HashSet;
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
