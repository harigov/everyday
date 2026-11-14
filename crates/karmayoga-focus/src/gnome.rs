//! GNOME on Wayland: idle and lock from the session bus, focus from nowhere.
//!
//! Mutter keeps the focused window to itself. `org.gnome.Shell.Introspect`
//! answers only an allowlist of GNOME's own programs, `Eval` has been off
//! since GNOME 41, and Mutter implements no foreign-toplevel protocol -- so
//! [`Watcher::focused`] here is always [`Error::NeedsExtension`], and the
//! Shell extension in `extensions/gnome` reports focus instead. What this
//! watcher can still answer is whether the person is there:
//!
//! - **Idle**: Mutter's `org.gnome.Mutter.IdleMonitor` on the core monitor,
//!   whose `GetIdletime` is the milliseconds since the last input -- the
//!   same clock GNOME's own screen blanking runs on.
//! - **Locked**: `org.gnome.ScreenSaver.GetActive`, which is true while the
//!   Shell's lock screen (or its blanking shield) is up; logind's
//!   `LockedHint` when the Shell does not answer, as during a restart.

use std::time::Duration;

use crate::linux::{Bus, Logind};
use crate::{Backend, Error, Focused, Result, Watcher};

const IDLE_MONITOR: &str = "org.gnome.Mutter.IdleMonitor";
const IDLE_MONITOR_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
const SCREEN_SAVER: &str = "org.gnome.ScreenSaver";
const SCREEN_SAVER_PATH: &str = "/org/gnome/ScreenSaver";

pub(crate) struct Gnome {
    session: Bus,
    logind: Logind,
}

impl Gnome {
    /// Never fails: the buses are opened on first use, so a session bus
    /// that is not up yet costs a failed sample, not the watcher.
    pub(crate) fn new() -> Self {
        Self { session: Bus::session(), logind: Logind::new() }
    }
}

impl Watcher for Gnome {
    fn backend(&self) -> Backend {
        Backend::Gnome
    }

    fn focused(&mut self) -> Result<Option<Focused>> {
        Err(Error::NeedsExtension)
    }

    fn idle(&mut self) -> Result<Duration> {
        let millis: u64 = self.session.call_for(
            IDLE_MONITOR,
            IDLE_MONITOR_PATH,
            IDLE_MONITOR,
            "GetIdletime",
            &(),
        )?;
        Ok(Duration::from_millis(millis))
    }

    fn locked(&mut self) -> Result<bool> {
        match self.session.call_for::<_, bool>(
            SCREEN_SAVER,
            SCREEN_SAVER_PATH,
            SCREEN_SAVER,
            "GetActive",
            &(),
        ) {
            Ok(active) => Ok(active),
            Err(e) => {
                tracing::debug!(error = %e, "GNOME's screen saver did not answer; asking logind");
                self.logind.locked()
            }
        }
    }
}
