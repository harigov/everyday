//! The Linux desk: GNOME first, and whatever else answers the same calls.
//!
//! Each piece asks the desktop the way the desktop itself is asked, and no
//! more than that:
//!
//! | | |
//! |---|---|
//! | [`apps`] | GIO's list of desktop files, launched through a launch context from the window system |
//! | [`media`] | MPRIS on the session bus, which every player worth the name speaks |
//! | [`volume`] | `wpctl` (PipeWire's own tool), or `pactl` where there is no WirePlumber |
//! | [`settings`] | GSettings: Do Not Disturb, dark style and Night Light are three keys |
//! | [`lock`] | the screen saver's `Lock`, or logind's, or `loginctl` |
//!
//! # Say only what is true
//!
//! The assistant is offered a tool only for what [`abilities`] says this
//! desktop can do, and is told nothing of the rest: a model that never heard
//! of the volume cannot promise to turn it down. So the answer is worked out
//! once, as the app starts, from what is actually here -- a `wpctl` on the
//! path, a schema installed with the key in it, a session bus to talk on --
//! rather than from the name of the desktop. That is also why nothing here
//! assumes: a GNOME with no Night Light plugin reports every other setting
//! and not that one.
//!
//! # What is handed to the programs started
//!
//! Every program started from here is first cleaned of what would break it
//! -- see [`super::environment`].

pub(super) mod apps;
pub(super) mod lock;
pub(super) mod media;
pub(super) mod settings;
pub(super) mod volume;

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jatayu_service::desk::Abilities;

/// What this desktop can do, found out by looking.
pub(super) fn abilities() -> Abilities {
    let bus = session_bus_present(
        std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR").as_deref().map(Path::new),
        |path| path.exists(),
    );
    Abilities {
        // The list is GIO's own, and launching may be a D-Bus activation.
        apps: bus,
        files: true,
        sections: true,
        media: bus,
        volume: volume::available(),
        settings: settings::available(),
        lock: bus,
        copy: true,
    }
}

/// Is there a session bus to talk on?
///
/// The same two places a D-Bus client looks: the address in
/// `DBUS_SESSION_BUS_ADDRESS`, and failing that the socket every systemd
/// session has at `$XDG_RUNTIME_DIR/bus`. `exists` is whether a path is
/// there, so this can be tried without one.
fn session_bus_present(
    address: Option<&str>,
    runtime_dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> bool {
    address.is_some_and(|a| !a.trim().is_empty())
        || runtime_dir.is_some_and(|dir| exists(&dir.join("bus")))
}

/// Is `program` an executable file in one of the directories of `search`
/// (a `PATH`)? Not asking a shell, which would take it from a different
/// `PATH` than the one a program started from here is given.
pub(super) fn on_path(program: &str, search: &OsStr) -> bool {
    std::env::split_paths(search).any(|dir| is_executable(&dir.join(program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// What a finished program printed.
pub(super) struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// How long a command-line tool gets. They answer in milliseconds, or the
/// sound server or the session is not there to answer at all, and a turn
/// must not wait on a program that is hung.
const TOOL_TIME: Duration = Duration::from_secs(5);

/// Run `program` with `args` -- cleaned of what would break it -- and wait
/// for it, up to [`TOOL_TIME`]. Blocking: a call for the blocking pool.
///
/// An error only when the program could not be started or did not finish,
/// in words to log, not to say: a program that ran and failed is a
/// [`Ran`] with `success` false, and what it said is for the caller to read.
pub(super) fn run(program: &str, args: &[&str]) -> Result<Ran, String> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    super::environment::clean(&mut command);
    let mut child = command.spawn().map_err(|e| format!("{program} could not be started: {e}"))?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > TOOL_TIME => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} did not finish in {TOOL_TIME:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("{program} could not be waited for: {e}")),
        }
    };
    // What these print fits a pipe's buffer many times over, so the program
    // was never blocked writing it, and reading it afterwards loses nothing.
    let stdout = slurp(child.stdout.take());
    let stderr = slurp(child.stderr.take());
    Ok(Ran { success: status.success(), stdout, stderr })
}

/// Everything `pipe` holds, or nothing if it cannot be read.
fn slurp(pipe: Option<impl std::io::Read>) -> String {
    let mut text = String::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_string(&mut text);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_bus_is_an_address_or_a_socket() {
        let none = |_: &Path| false;
        let present = |p: &Path| p == Path::new("/run/user/1000/bus");
        assert!(session_bus_present(Some("unix:path=/run/user/1000/bus"), None, none));
        assert!(session_bus_present(None, Some(Path::new("/run/user/1000")), present));
        assert!(!session_bus_present(None, Some(Path::new("/run/user/1001")), present));
        assert!(!session_bus_present(Some("  "), None, none));
        assert!(!session_bus_present(None, None, present));
    }

    #[cfg(unix)]
    #[test]
    fn a_program_is_on_the_path_when_an_executable_file_is() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("wpctl");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let data = dir.path().join("pactl");
        std::fs::write(&data, "not a program").unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644)).unwrap();

        let search = std::env::join_paths([Path::new("/nonexistent"), dir.path()]).unwrap();
        assert!(on_path("wpctl", &search));
        assert!(!on_path("pactl", &search), "a file that cannot be run is not a program");
        assert!(!on_path("missing", &search));
        assert!(!on_path("wpctl", OsStr::new("")));
    }
}
