//! Is another application using the microphone? -- the second way a call
//! is noticed, beside the calendar. See [`crate::offers`].
//!
//! The calendar knows about the calls somebody scheduled. It does not know
//! about the one a colleague starts with "got a minute?" in a Zoom link
//! pasted into chat, or the Meet a client opens on their side. Those have
//! one thing in common that is visible from outside the call application
//! without reading anything of it: some other program has asked the sound
//! server for the microphone. So that is what this watches. When one has
//! held it for a few seconds the offers hub is told ([`crate::offers::on_mic_started`])
//! and may offer to take notes; when every other program has let go for long
//! enough, it is told the call is over ([`crate::offers::on_mic_ended`]).
//!
//! Nothing is recorded, opened or listened to here. The watcher is a
//! bystander on the sound server's list of what is connected to what, the
//! same list `pw-top` prints; it never touches audio.
//!
//! # Three pieces, two of them pure
//!
//! - [`mic_user`] decides whether one object on the sound server counts as
//!   "another program is using the microphone", given its properties. Most
//!   of what is wrong with a naive version of this lives here: our own
//!   recording is a microphone user, so is the volume meter in every
//!   sound settings panel, and so is anything recording what the speakers
//!   play. None of those is a call.
//! - [`MicTracker`] turns a once-a-second reading of "who has the
//!   microphone" into at most one `Started` and one `Ended` per call. It
//!   holds the two timers -- [`SETTLE`] before a call begins, [`RELEASE_GRACE`]
//!   before it ends -- and takes the time as an argument, so a test can run
//!   a call that lasts an hour in no time at all.
//! - The Linux backend (everything under `backend`) keeps a connection to
//!   PipeWire open on its own thread, feeds [`mic_user`] each stream that
//!   appears, and shares the list of those that are *running* with a second
//!   thread that runs the tracker and calls the hub. Running, not merely
//!   present: a stream can sit open and idle -- GNOME Settings keeps several
//!   of its level meters that way, and an application can open its input
//!   long before it uses it -- and only one actually moving audio is
//!   "using the microphone", the same question macOS answers with
//!   `kAudioDevicePropertyDeviceIsRunningSomewhere`. So each candidate
//!   stream is bound, and its state followed through its info events. They are separate threads because the
//!   PipeWire objects are `!Send` and must live and die on the thread that
//!   made them, while the tracker has to keep its clock running when
//!   PipeWire is not answering.
//!
//! # Why a gap does not end the call
//!
//! Call applications let go of the microphone more often than the call
//! ends: switching headset reopens the stream, a browser tab that reloads
//! its call page drops it for a second, Teams does something with it when
//! someone shares a screen. Ending the session on the first empty reading
//! would offer the same call twice. [`RELEASE_GRACE`] is deliberately long
//! (45 seconds) because the cost is lopsided: a call noticed to be over 45
//! seconds late costs nothing, a call split in two costs a second set of
//! notes and a second offer.
//!
//! # Other platforms
//!
//! Only Linux is implemented. Elsewhere [`spawn`] does nothing, because neither of the others can be built or
//! tried on the machines this is written on, and an untested detector that
//! offers notes during a person's private call is worse than none. For whoever
//! does it later:
//!
//! - **macOS**: CoreAudio's `kAudioDevicePropertyDeviceIsRunningSomewhere`
//!   on the default input device is true while any process, this one
//!   included, has it running, with a property listener to be told when it
//!   changes. It does not say *which* program, so there would be no name to
//!   offer, and "any process" includes ours -- the answer would have to be
//!   ignored while our own recording is running.
//! - **Windows**: each program that has used the microphone has a key under
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone`
//!   (packaged apps directly, desktop programs under `NonPackaged`) with a
//!   `LastUsedTimeStart` and a `LastUsedTimeStop`; a `LastUsedTimeStop` of 0
//!   means it is using the microphone now. The key name is the program's
//!   path, which is a name to offer. Polling the registry once a second is
//!   enough; `RegNotifyChangeKeyValue` would make it event-driven.
//!
//! Either would feed [`MicTracker`] the same way the Linux backend does.
//!
//! # Switching it off
//!
//! `EVERYDAY_NO_MICWATCH`, set to anything, stops [`spawn`] from starting
//! anything. It is for test harnesses -- a headless session with a real
//! PipeWire would otherwise start offering notes for whatever the machine
//! happens to be doing -- and for anyone whose sound server misbehaves when
//! it is watched.

// Everything below the platform split is exercised by the tests on every
// platform but only called on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tauri::AppHandle;

/// How long another program must hold the microphone, without a break,
/// before that is taken for a call. Long enough that something that opens
/// the microphone for a moment and shuts it -- a voice-search button, a
/// speech-to-text dictation key, a browser's permission prompt checking the
/// device -- never gets as far as an offer; short enough that notes begin
/// before the first sentence of a call is over.
pub const SETTLE: Duration = Duration::from_secs(4);

/// How long the microphone must stay free of other programs, without a
/// break, before the call is over. See "Why a gap does not end the call" in
/// the module docs.
pub const RELEASE_GRACE: Duration = Duration::from_secs(45);

/// How often the policy thread reads the list and asks the tracker.
const TICK: Duration = Duration::from_secs(1);

/// How long the Linux backend waits before it tries PipeWire again, after
/// failing to connect or after losing the connection. A sound server that
/// is not there is not about to be, and one that restarted is back within
/// seconds; half a minute costs nothing when the answer is "still no".
const RETRY: Duration = Duration::from_secs(30);

/// One program that has the microphone open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUser {
    /// What to call it, when the sound server knows: the name the program
    /// gave itself, else its executable's name, else its stream's name.
    pub app: Option<String>,
    /// Its process id, when it said.
    pub pid: Option<u32>,
}

/// What a reading of the microphone's users, over time, adds up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicEvent {
    /// Another program has held the microphone for [`SETTLE`]. `app` is the
    /// name of the one that had it longest, when it has a name.
    Started { app: Option<String> },
    /// The microphone has been free of other programs for [`RELEASE_GRACE`].
    Ended,
}

/// Where [`MicTracker`] is in one call's life.
#[derive(Debug)]
enum Phase {
    /// Nobody else has the microphone, or not for long enough yet to say.
    Idle,
    /// Somebody has had it since `since`, and has not yet had it for
    /// [`SETTLE`].
    Settling { since: Instant },
    /// `Started` has been reported. `free_since` is when the last reading
    /// with nobody on the microphone began, if the latest reading is such a
    /// one.
    Active { free_since: Option<Instant> },
}

/// The policy: turns a once-a-second reading of who has the microphone into
/// at most one [`MicEvent::Started`] and then one [`MicEvent::Ended`] per
/// call. Knows nothing of PipeWire, threads or the clock; the caller says
/// what time it is.
///
/// The two timers are measured against the *readings*, not against
/// wall-clock time between calls, so a caller that stalls (a laptop that
/// slept for an hour) sees the tracker behave as if the gap had been one
/// long reading of whatever it saw next -- a call that was over before the
/// lid closed ends on the first reading after it opens, and a microphone
/// that was free then and busy now starts the settle clock from zero.
#[derive(Debug)]
pub struct MicTracker {
    phase: Phase,
}

impl Default for MicTracker {
    fn default() -> Self {
        Self { phase: Phase::Idle }
    }
}

impl MicTracker {
    /// Feed one reading: `users` is everyone with the microphone open at
    /// `now`, oldest first, and may be empty. Returns the event this
    /// reading completes, if any.
    ///
    /// A reading that is empty before `Started` resets the settle clock: two
    /// blips three seconds apart are two blips, not a six-second call.
    pub fn observe(&mut self, now: Instant, users: &[MicUser]) -> Option<MicEvent> {
        match self.phase {
            Phase::Idle => {
                if !users.is_empty() {
                    self.phase = Phase::Settling { since: now };
                }
                None
            }
            Phase::Settling { since } => {
                if users.is_empty() {
                    self.phase = Phase::Idle;
                    None
                } else if now.saturating_duration_since(since) >= SETTLE {
                    self.phase = Phase::Active { free_since: None };
                    // The oldest user is the one that was there first, which
                    // is the call application rather than something that
                    // joined it; the first that has a name stands in when it
                    // is the one without.
                    Some(MicEvent::Started { app: users.iter().find_map(|u| u.app.clone()) })
                } else {
                    None
                }
            }
            Phase::Active { free_since } => {
                if !users.is_empty() {
                    self.phase = Phase::Active { free_since: None };
                    None
                } else {
                    let since = free_since.unwrap_or(now);
                    if now.saturating_duration_since(since) >= RELEASE_GRACE {
                        self.phase = Phase::Idle;
                        Some(MicEvent::Ended)
                    } else {
                        self.phase = Phase::Active { free_since: Some(since) };
                        None
                    }
                }
            }
        }
    }
}

/// Programs that open the microphone to show a level or to route audio, not
/// to talk. Matched case-insensitively, as a substring, against a stream's
/// executable, application id and application name -- substring because
/// the same program is `pavucontrol` as an executable, `org.pulseaudio.pavucontrol`
/// as an id and "PulseAudio Volume Control" as a name, and the executable
/// is the one that is reliably present.
///
/// `plasmashell` is on the list because KDE's sound applet draws its
/// microphone meter from inside the shell, and the shell has no stream of
/// its own that means anything else.
const PLUMBING: &[&str] = &[
    "pavucontrol",
    "pwvucontrol",
    "easyeffects",
    "pulseeffects",
    "helvum",
    "qpwgraph",
    "coppwr",
    "gnome-control-center",
    "plasma-pa",
    "plasmashell",
    "kmix",
];

/// Decide whether one object on the sound server is another program using
/// the microphone. `props` looks up a PipeWire property by name; `own_pid`
/// is this process's id.
///
/// Counts only a stream that *captures* (`Stream/Input/Audio`: a program
/// reading from an input), and not:
///
/// - ours -- the meeting recording opens the microphone through cpal, and
///   would otherwise start a call about itself the moment it began. A
///   stream's own properties do not always carry the process id (that lives
///   on the client object, and only some clients copy it down), so cpal's
///   own mark is checked too: it puts every stream it opens in the node
///   group `cpal-{pid}`;
/// - one reading what a sink plays rather than a microphone
///   (`stream.capture.sink`, `stream.monitor`, or `node.passive`, which is
///   what a program sets on a stream it only wants while something else
///   is already running) -- that is a screen recorder or a loopback, not a
///   person speaking;
/// - a volume meter or an audio routing tool ([`PLUMBING`], or a stream
///   named "Peak detect", which is what PipeWire's own level metering
///   calls its streams whoever opens them).
pub fn mic_user(props: &dyn Fn(&str) -> Option<String>, own_pid: u32) -> Option<MicUser> {
    if props("media.class").as_deref() != Some("Stream/Input/Audio") {
        return None;
    }
    let pid = props("application.process.id").and_then(|p| p.trim().parse::<u32>().ok());
    if pid == Some(own_pid) || props("node.group") == Some(format!("cpal-{own_pid}")) {
        return None;
    }
    let is_true = |key: &str| props(key).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    if is_true("stream.capture.sink") || is_true("stream.monitor") || is_true("node.passive") {
        return None;
    }
    if props("media.name").is_some_and(|n| n.trim().eq_ignore_ascii_case("Peak detect")) {
        return None;
    }
    let lowered = |key: &str| props(key).map(|v| v.to_lowercase());
    let identities = [
        lowered("application.process.binary"),
        lowered("application.id"),
        lowered("application.name"),
    ];
    let is_plumbing =
        identities.iter().flatten().any(|id| PLUMBING.iter().any(|tool| id.contains(tool)));
    if is_plumbing {
        return None;
    }
    let named = |key: &str| props(key).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let app = named("application.name")
        .or_else(|| named("application.process.binary"))
        .or_else(|| named("node.name"));
    Some(MicUser { app, pid })
}

/// What the backend thread and the policy thread share.
#[derive(Default)]
struct Shared {
    /// Streams that count, by PipeWire object id. Ids are handed out in
    /// increasing order, so iterating is oldest first.
    users: Mutex<BTreeMap<u32, MicUser>>,
    /// Between `Started` and `Ended`.
    active: AtomicBool,
}

impl Shared {
    fn users(&self) -> MutexGuard<'_, BTreeMap<u32, MicUser>> {
        // A panic in a PipeWire callback must not turn every later reading
        // into another panic; the map is valid whenever the lock is free.
        self.users.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn snapshot(&self) -> Vec<MicUser> {
        self.users().values().cloned().collect()
    }
}

/// Held in Tauri state.
#[derive(Default)]
pub struct MicWatch {
    shared: Arc<Shared>,
}

impl MicWatch {
    /// Whether another application's use of the microphone currently
    /// counts as a call: [`crate::offers::on_mic_started`] has been called
    /// and [`crate::offers::on_mic_ended`] has not since.
    pub fn session_active(&self) -> bool {
        self.shared.active.load(Ordering::Relaxed)
    }
}

/// Start watching, for the life of the process. A no-op off Linux
/// (see "Other platforms" in the module docs), and wherever `EVERYDAY_NO_MICWATCH` is
/// set (see the module docs).
///
/// Starts two threads and returns at once. Neither is ever joined: they
/// stop when the process does.
pub fn spawn(app: &AppHandle) {
    if std::env::var_os("EVERYDAY_NO_MICWATCH").is_some() {
        tracing::debug!("micwatch: switched off by EVERYDAY_NO_MICWATCH");
        return;
    }
    #[cfg(target_os = "linux")]
    {
        use tauri::Manager;
        let shared = app.state::<MicWatch>().shared.clone();
        backend::spawn(shared.clone());
        spawn_policy(app.clone(), shared);
    }
    #[cfg(not(target_os = "linux"))]
    {
        // See "Other platforms" in the module docs.
        let _ = app;
    }
}

/// The thread that reads the shared list every [`TICK`], runs the tracker
/// and tells the hub. The flag is set before the hub is told, and cleared
/// before it is told, so that anything the hub does in response -- asking
/// [`MicWatch::session_active`] -- sees the state it was told about.
#[cfg(target_os = "linux")]
fn spawn_policy(app: AppHandle, shared: Arc<Shared>) {
    let spawned = std::thread::Builder::new()
        .name("micwatch-policy".into())
        .spawn(move || {
            let mut tracker = MicTracker::default();
            loop {
                std::thread::sleep(TICK);
                let users = shared.snapshot();
                match tracker.observe(Instant::now(), &users) {
                    Some(MicEvent::Started { app: name }) => {
                        tracing::info!(app = ?name, "micwatch: another application is using the microphone");
                        shared.active.store(true, Ordering::Relaxed);
                        crate::offers::on_mic_started(&app, name);
                    }
                    Some(MicEvent::Ended) => {
                        tracing::info!("micwatch: the microphone is free again");
                        shared.active.store(false, Ordering::Relaxed);
                        crate::offers::on_mic_ended(&app);
                    }
                    None => {}
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "micwatch: could not start the policy thread");
    }
}

/// The PipeWire side. Everything here runs on one thread, because the
/// objects are `!Send`.
#[cfg(target_os = "linux")]
mod backend {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;

    use pipewire::context::ContextRc;
    use pipewire::core::PW_ID_CORE;
    use pipewire::main_loop::MainLoopRc;
    use pipewire::node::{Node, NodeListener};
    use pipewire::types::ObjectType;

    use super::{MicUser, RETRY, Shared, mic_user};

    /// `PW_NODE_STATE_RUNNING` in `pipewire/node.h`: the node is moving
    /// audio, not merely open.
    const PW_NODE_STATE_RUNNING: i32 = 3;

    pub fn spawn(shared: Arc<Shared>) {
        let spawned =
            std::thread::Builder::new().name("micwatch-pipewire".into()).spawn(move || run(shared));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "micwatch: could not start the PipeWire thread");
        }
    }

    /// Connect, watch until the connection ends, forget everything seen,
    /// wait, and again. Never returns and never panics on a PipeWire
    /// failure.
    fn run(shared: Arc<Shared>) {
        pipewire::init();
        loop {
            match watch_once(&shared) {
                Ok(()) => tracing::warn!("micwatch: lost the connection to PipeWire"),
                Err(e) => tracing::debug!(error = %e, "micwatch: PipeWire is not available"),
            }
            // What was seen on a connection that is gone is not to be
            // trusted: the streams' removals may never have arrived. The
            // new connection announces everything again from scratch.
            shared.users().clear();
            std::thread::sleep(RETRY);
        }
    }

    /// One connection's life: returns when the core reports that it is gone
    /// (PipeWire stopped or restarted), or with the error that stopped it
    /// from starting.
    fn watch_once(shared: &Arc<Shared>) -> Result<(), pipewire::Error> {
        let main_loop = MainLoopRc::new(None)?;
        let context = ContextRc::new(&main_loop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;
        tracing::debug!("micwatch: connected to PipeWire");

        let own_pid = std::process::id();

        // Weak, so the closure does not keep the loop alive in a cycle
        // through the listener it is stored in.
        let loop_weak = main_loop.downgrade();
        let _core_listener = core
            .add_listener_local()
            .error(move |id, seq, res, message| {
                tracing::debug!(id, seq, res, error = message, "micwatch: PipeWire core error");
                // Errors on other objects are theirs to deal with; an error
                // on the core itself (id 0) is the connection breaking.
                if id == PW_ID_CORE
                    && let Some(main_loop) = loop_weak.upgrade()
                {
                    main_loop.quit();
                }
            })
            .register();

        // Every candidate stream, bound so its state can be followed, kept
        // alive here -- a proxy or listener dropped stops reporting -- and
        // dropped when the stream goes. Only the running ones are in
        // `shared`'s list.
        let bound: Rc<RefCell<HashMap<u32, (Node, NodeListener)>>> = Rc::default();
        let registry_weak = registry.downgrade();
        let on_global = shared.clone();
        let on_global_bound = bound.clone();
        let on_remove = shared.clone();
        let on_remove_bound = bound.clone();
        let _registry_listener = registry
            .add_listener_local()
            .global(move |global| {
                if global.type_ != ObjectType::Node {
                    return;
                }
                // The announcement carries only a handful of properties --
                // enough to know a capture stream, not whose it is: the
                // process id and binary `mic_user` needs arrive with the
                // node's own info. So every capture stream is bound, and
                // judged on that.
                let class = global.props.and_then(|p| p.get("media.class"));
                if class != Some("Stream/Input/Audio") {
                    return;
                }
                let Some(registry) = registry_weak.upgrade() else { return };
                let node: Node = match registry.bind(global) {
                    Ok(node) => node,
                    Err(e) => {
                        tracing::debug!(id = global.id, error = %e, "micwatch: could not follow a stream");
                        return;
                    }
                };
                let id = global.id;
                let users = on_global.clone();
                // Whether this stream counts, judged on every property it has
                // reported so far: an info event after the first carries
                // only the properties that changed, and judging on those
                // alone would find no `media.class` and drop the stream.
                let seen: RefCell<HashMap<String, String>> = RefCell::default();
                let judged: RefCell<Option<MicUser>> = RefCell::new(None);
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        if let Some(props) = info.props() {
                            let mut seen = seen.borrow_mut();
                            for (key, value) in props.iter() {
                                seen.insert(key.to_owned(), value.to_owned());
                            }
                            let lookup = |key: &str| seen.get(key).cloned();
                            *judged.borrow_mut() = mic_user(&lookup, own_pid);
                        }
                        // Read raw rather than through `NodeInfoRef::state`,
                        // which panics on a state number it does not know --
                        // and a panic in a callback from C takes the process
                        // with it.
                        let running = info.as_raw().state == PW_NODE_STATE_RUNNING;
                        match (&*judged.borrow(), running) {
                            (Some(user), true) => {
                                if users.users().insert(id, user.clone()).is_none() {
                                    tracing::debug!(id, ?user, "micwatch: a stream is capturing");
                                }
                            }
                            _ => {
                                if users.users().remove(&id).is_some() {
                                    tracing::debug!(id, "micwatch: a stream stopped capturing");
                                }
                            }
                        }
                    })
                    .register();
                on_global_bound.borrow_mut().insert(id, (node, listener));
            })
            .global_remove(move |id| {
                on_remove_bound.borrow_mut().remove(&id);
                if on_remove.users().remove(&id).is_some() {
                    tracing::debug!(id, "micwatch: a capturing stream went away");
                }
            })
            .register();

        main_loop.run();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(app: &str) -> MicUser {
        MicUser { app: Some(app.into()), pid: Some(100) }
    }

    /// A tracker, a clock starting at an arbitrary instant, and a way to
    /// say "t seconds in".
    struct Clock(Instant);
    impl Clock {
        fn new() -> Self {
            Clock(Instant::now())
        }
        fn at(&self, secs: u64) -> Instant {
            self.0 + Duration::from_secs(secs)
        }
    }

    // --- the tracker ---------------------------------------------------

    #[test]
    fn a_call_starts_once_the_microphone_has_been_held_for_the_settle_time() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let zoom = [user("zoom")];
        assert_eq!(t.observe(c.at(0), &zoom), None);
        assert_eq!(t.observe(c.at(3), &zoom), None);
        assert_eq!(t.observe(c.at(4), &zoom), Some(MicEvent::Started { app: Some("zoom".into()) }));
    }

    #[test]
    fn a_blip_shorter_than_the_settle_time_starts_nothing() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        assert_eq!(t.observe(c.at(0), &[user("dictation")]), None);
        assert_eq!(t.observe(c.at(2), &[user("dictation")]), None);
        assert_eq!(t.observe(c.at(3), &[]), None);
        // Ten minutes of nothing, and still nothing.
        assert_eq!(t.observe(c.at(600), &[]), None);
    }

    #[test]
    fn an_empty_reading_before_the_start_resets_the_settle_clock() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let app = [user("meet")];
        t.observe(c.at(0), &app);
        t.observe(c.at(3), &[]);
        // Held again from 4 s; 4 s after the *first* reading, but only
        // moments after the second.
        assert_eq!(t.observe(c.at(4), &app), None);
        assert_eq!(t.observe(c.at(7), &app), None);
        assert!(matches!(t.observe(c.at(8), &app), Some(MicEvent::Started { .. })));
    }

    #[test]
    fn a_gap_shorter_than_the_grace_keeps_the_session() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let app = [user("teams")];
        t.observe(c.at(0), &app);
        assert!(t.observe(c.at(4), &app).is_some());
        // The app reopens its stream: 30 s with nothing, then it is back.
        assert_eq!(t.observe(c.at(10), &[]), None);
        assert_eq!(t.observe(c.at(40), &[]), None);
        assert_eq!(t.observe(c.at(41), &app), None);
        // The gap's clock starts over: another 44 s of nothing is still short.
        assert_eq!(t.observe(c.at(42), &[]), None);
        assert_eq!(t.observe(c.at(86), &[]), None);
        // And no second Started when it comes back.
        assert_eq!(t.observe(c.at(87), &app), None);
        assert_eq!(t.observe(c.at(120), &app), None);
    }

    #[test]
    fn a_gap_as_long_as_the_grace_ends_the_session() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let app = [user("zoom")];
        t.observe(c.at(0), &app);
        assert!(t.observe(c.at(4), &app).is_some());
        assert_eq!(t.observe(c.at(100), &[]), None);
        assert_eq!(t.observe(c.at(144), &[]), None);
        assert_eq!(t.observe(c.at(145), &[]), Some(MicEvent::Ended));
    }

    #[test]
    fn each_session_yields_exactly_one_started_and_one_ended() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let app = [user("zoom")];
        let mut events = Vec::new();
        for s in 0..=400u64 {
            // On the microphone from 0 to 100 s, then free.
            let users: &[MicUser] = if s <= 100 { &app } else { &[] };
            events.extend(t.observe(c.at(s), users));
        }
        assert_eq!(events, vec![MicEvent::Started { app: Some("zoom".into()) }, MicEvent::Ended]);
    }

    #[test]
    fn a_new_session_can_follow_an_ended_one() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        t.observe(c.at(0), &[user("zoom")]);
        assert!(t.observe(c.at(4), &[user("zoom")]).is_some());
        t.observe(c.at(10), &[]);
        assert_eq!(t.observe(c.at(55), &[]), Some(MicEvent::Ended));
        // Silence, then a different app.
        assert_eq!(t.observe(c.at(60), &[]), None);
        assert_eq!(t.observe(c.at(100), &[user("meet")]), None);
        assert_eq!(
            t.observe(c.at(104), &[user("meet")]),
            Some(MicEvent::Started { app: Some("meet".into()) })
        );
    }

    #[test]
    fn the_oldest_user_names_the_call() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let users = [user("zoom"), user("obs")];
        t.observe(c.at(0), &users);
        assert_eq!(
            t.observe(c.at(4), &users),
            Some(MicEvent::Started { app: Some("zoom".into()) })
        );
    }

    #[test]
    fn a_nameless_oldest_user_gives_way_to_a_named_one() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        let users = [MicUser { app: None, pid: None }, user("zoom")];
        t.observe(c.at(0), &users);
        assert_eq!(
            t.observe(c.at(4), &users),
            Some(MicEvent::Started { app: Some("zoom".into()) })
        );
        // And a call with no names at all is still a call.
        let mut t = MicTracker::default();
        let nameless = [MicUser { app: None, pid: None }];
        t.observe(c.at(0), &nameless);
        assert_eq!(t.observe(c.at(5), &nameless), Some(MicEvent::Started { app: None }));
    }

    #[test]
    fn one_user_leaving_while_another_stays_does_not_end_the_session() {
        let (mut t, c) = (MicTracker::default(), Clock::new());
        t.observe(c.at(0), &[user("zoom"), user("obs")]);
        assert!(t.observe(c.at(4), &[user("zoom"), user("obs")]).is_some());
        for s in 5..300 {
            assert_eq!(t.observe(c.at(s), &[user("obs")]), None);
        }
    }

    // --- the filter ----------------------------------------------------

    const OWN: u32 = 4242;

    /// Look properties up in a list of pairs.
    fn props(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_owned())
    }

    #[test]
    fn a_browsers_webrtc_stream_counts() {
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.name", "Firefox"),
            ("application.process.binary", "firefox"),
            ("application.process.id", "9001"),
            ("node.name", "Firefox"),
            ("media.name", "AudioStream"),
        ]);
        assert_eq!(
            mic_user(&p, OWN),
            Some(MicUser { app: Some("Firefox".into()), pid: Some(9001) })
        );
    }

    #[test]
    fn the_name_falls_back_to_the_binary_then_the_node() {
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.process.binary", "zoom"),
            ("node.name", "ZOOM VoiceEngine"),
        ]);
        assert_eq!(mic_user(&p, OWN).unwrap().app.as_deref(), Some("zoom"));
        let p = props(&[("media.class", "Stream/Input/Audio"), ("node.name", "ZOOM VoiceEngine")]);
        assert_eq!(mic_user(&p, OWN).unwrap().app.as_deref(), Some("ZOOM VoiceEngine"));
        let p = props(&[("media.class", "Stream/Input/Audio")]);
        assert_eq!(mic_user(&p, OWN), Some(MicUser { app: None, pid: None }));
    }

    #[test]
    fn our_own_recording_is_not_another_program() {
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.name", "Every Day"),
            ("application.process.id", "4242"),
        ]);
        assert_eq!(mic_user(&p, OWN), None);
        // No process id on the stream, only cpal's own group.
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("node.name", "everyday-app"),
            ("node.group", "cpal-4242"),
        ]);
        assert_eq!(mic_user(&p, OWN), None);
        // Another program's cpal stream is still another program.
        let p = props(&[("media.class", "Stream/Input/Audio"), ("node.group", "cpal-77")]);
        assert!(mic_user(&p, OWN).is_some());
    }

    #[test]
    fn a_volume_meter_is_not_a_call() {
        for (binary, id, name) in [
            ("pavucontrol", "org.pulseaudio.pavucontrol", "PulseAudio Volume Control"),
            ("pwvucontrol", "", "Pipewire Volume Control"),
            ("easyeffects", "com.github.wwmm.easyeffects", "Easy Effects"),
            ("gnome-control-center", "", "Settings"),
            ("Plasma-PA", "", ""),
        ] {
            // Any one of the three is enough, and case does not matter.
            let p = |key: &str| match key {
                "media.class" => Some("Stream/Input/Audio".to_owned()),
                "application.process.binary" => Some(binary.to_owned()),
                "application.id" => Some(id.to_owned()),
                "application.name" => Some(name.to_owned()),
                _ => None,
            };
            assert_eq!(mic_user(&p, OWN), None, "{binary}");
        }
        // By id alone.
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.id", "org.pipewire.Helvum"),
        ]);
        assert_eq!(mic_user(&p, OWN), None);
    }

    #[test]
    fn a_peak_detect_stream_is_not_a_call() {
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.name", "Some Mixer"),
            ("media.name", "Peak detect"),
        ]);
        assert_eq!(mic_user(&p, OWN), None);
    }

    #[test]
    fn a_stream_reading_a_sink_is_not_a_microphone() {
        for key in ["stream.capture.sink", "stream.monitor", "node.passive"] {
            let p = |k: &str| match k {
                "media.class" => Some("Stream/Input/Audio".to_owned()),
                "application.name" => Some("OBS".to_owned()),
                k if k == key => Some("true".to_owned()),
                _ => None,
            };
            assert_eq!(mic_user(&p, OWN), None, "{key}");
        }
        // "false" is a way of saying nothing.
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.name", "OBS"),
            ("stream.capture.sink", "false"),
        ]);
        assert!(mic_user(&p, OWN).is_some());
    }

    #[test]
    fn only_capturing_streams_count() {
        for class in [
            "Stream/Output/Audio",
            "Audio/Source",
            "Audio/Sink",
            "Video/Source",
            "Stream/Input/Video",
        ] {
            let p = |k: &str| match k {
                "media.class" => Some(class.to_owned()),
                "application.name" => Some("Spotify".to_owned()),
                _ => None,
            };
            assert_eq!(mic_user(&p, OWN), None, "{class}");
        }
        assert_eq!(mic_user(&|_: &str| None, OWN), None);
    }

    #[test]
    fn a_pid_that_is_not_a_number_is_not_ours() {
        let p = props(&[
            ("media.class", "Stream/Input/Audio"),
            ("application.name", "Weird"),
            ("application.process.id", "not-a-number"),
        ]);
        assert_eq!(mic_user(&p, OWN), Some(MicUser { app: Some("Weird".into()), pid: None }));
    }

    // --- the state ------------------------------------------------------

    #[test]
    fn the_watch_starts_out_idle() {
        let w = MicWatch::default();
        assert!(!w.session_active());
        w.shared.active.store(true, Ordering::Relaxed);
        assert!(w.session_active());
    }

    #[test]
    fn the_snapshot_lists_the_oldest_stream_first() {
        let s = Shared::default();
        s.users().insert(90, user("later"));
        s.users().insert(12, user("first"));
        let apps: Vec<_> = s.snapshot().into_iter().filter_map(|u| u.app).collect();
        assert_eq!(apps, ["first", "later"]);
    }
}
