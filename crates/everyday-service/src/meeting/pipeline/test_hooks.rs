use everyday_core::RecordingId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) struct Gate {
    reached: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Gate {
    /// Whether `run_recording` has reached this gate yet -- what a test
    /// polls before assuming the early attempt is truly parked, rather
    /// than assuming any particular amount of scheduling has happened.
    pub(crate) fn reached(&self) -> bool {
        self.reached.load(Ordering::SeqCst)
    }

    pub(crate) fn release(&self) {
        self.notify.notify_one();
    }
}

fn gates() -> &'static Mutex<HashMap<RecordingId, Arc<Gate>>> {
    static GATES: OnceLock<Mutex<HashMap<RecordingId, Arc<Gate>>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Arm the gate for `id`. One test at a time per id -- trivially true in
/// practice, since every test mints its own fresh `RecordingId`.
pub(crate) fn arm(id: RecordingId) -> Arc<Gate> {
    let gate =
        Arc::new(Gate { reached: AtomicBool::new(false), notify: tokio::sync::Notify::new() });
    gates().lock().unwrap().insert(id, gate.clone());
    gate
}

fn without_vad() -> &'static Mutex<std::collections::HashSet<RecordingId>> {
    static IDS: OnceLock<Mutex<std::collections::HashSet<RecordingId>>> = OnceLock::new();
    IDS.get_or_init(Default::default)
}

/// Send `id`'s chunks whole, as a build without local speech would.
pub(crate) fn skip_vad(id: RecordingId) {
    without_vad().lock().unwrap().insert(id);
}

#[cfg_attr(not(feature = "speech"), allow(dead_code))]
pub(crate) fn skips_vad(id: RecordingId) -> bool {
    without_vad().lock().unwrap().contains(&id)
}

/// What [`super::drivers::run_recording`] calls. A no-op unless a test has
/// armed `id` first.
pub(crate) async fn wait(id: RecordingId) {
    let gate = gates().lock().unwrap().get(&id).cloned();
    if let Some(gate) = gate {
        gate.reached.store(true, Ordering::SeqCst);
        gate.notify.notified().await;
    }
}
