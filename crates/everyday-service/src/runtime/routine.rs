//! The scheduler's bookkeeping about routines this process is running or
//! has already reported trouble for.

use std::collections::HashSet;
use std::sync::RwLock;

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
    /// The routine running right now, by name, if one is.
    ///
    /// Here rather than derived from a `Running` row, because a row is also
    /// what a run abandoned by a dead process looks like. This is in
    /// memory and therefore cannot lie about the present.
    running: RwLock<Option<String>>,
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

    pub(crate) fn set_running(&self, name: Option<String>) {
        *self.running.write().unwrap() = name;
    }

    pub(crate) fn running(&self) -> Option<String> {
        self.running.read().unwrap().clone()
    }

    // ---- what a full vault close clears ----------------------------------
    //
    // `Service::close` calls these three by hand, in this order: reported
    // failures, then claimed runs, then the running routine's name. Not
    // called by `Service::locked` -- a lock screen leaves a routine's claim
    // and the "already reported" flags alone, only a full close resets
    // them. See `tests/runtime_lifecycle.rs`.

    pub(crate) fn forget_reported(&self) {
        self.reported.write().unwrap().clear();
    }

    pub(crate) fn forget_claimed(&self) {
        self.claimed.write().unwrap().clear();
    }

    pub(crate) fn forget_running(&self) {
        self.running.write().unwrap().take();
    }
}
