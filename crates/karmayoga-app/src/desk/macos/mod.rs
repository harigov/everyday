//! The macOS desk: what a Mac does for a program that asks politely.
//!
//! Only what can be done honestly is here -- with the system's own command
//! line tools and public calls, no private framework that the next release
//! locks (but see [`lock`]), no root, and nothing that asks the person a
//! question they could not understand.
//!
//! | | |
//! |---|---|
//! | [`apps`] | the `.app` bundles in the Applications folders, named from their `Info.plist`, started with `open -b` |
//! | [`media`] | the keyboard's play, next and previous keys, pressed through `enigo` |
//! | [`volume`] | `osascript`'s own `get volume settings` and `set volume`, which need no permission |
//! | [`settings`] | dark mode, through System Events |
//! | [`lock`] | the login window's own lock call, or the display put to sleep |
//!
//! # What is left off, and why
//!
//! Do Not Disturb and Night Shift are not here. Focus and Night Shift have no
//! public interface at all: scripts that flip them press buttons in Control
//! Centre or write private preferences, and both break with the next macOS.
//! The assistant is told of nothing that is not offered ([`abilities`]), so
//! it never promises them. Nor does the music report what is playing:
//! `MediaRemote`, the only thing that knows, is private and has been locked
//! to Apple's own programs since macOS 15.4.
//!
//! # Say only what is true
//!
//! As on Linux, what is offered is worked out once at start from what is
//! actually here -- `osascript` for the volume and dark mode -- and not from
//! the version of the operating system.

pub(super) mod apps;
pub(super) mod lock;
pub(super) mod media;
pub(super) mod settings;
pub(super) mod volume;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use karmayoga_service::desk::{Abilities, DeskSetting};

/// Where Apple keeps the scripting tool. A fixed path, not the `PATH`: an
/// app started from the Dock is given almost none.
const OSASCRIPT: &str = "/usr/bin/osascript";

/// What this Mac can do, found out by looking.
pub(super) fn abilities() -> Abilities {
    let scripting = Path::new(OSASCRIPT).exists();
    Abilities {
        apps: Path::new("/usr/bin/open").exists(),
        files: true,
        sections: true,
        // The keys are pressed by `enigo`, which is always built in; whether
        // the person has allowed it to is only known when one is pressed, and
        // is answered then, in words.
        media: true,
        volume: scripting,
        settings: if scripting { vec![DeskSetting::DarkMode] } else { Vec::new() },
        // Either the login window's call or `pmset`, which ships with the system.
        lock: true,
        copy: true,
    }
}

/// What a finished program printed.
pub(super) struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// How long a tool gets. `osascript` answers in milliseconds, unless it is
/// waiting on a permission prompt that nobody is looking at, and a turn must
/// not wait on that for ever.
const TOOL_TIME: Duration = Duration::from_secs(10);

/// Run `program` with `args` and wait for it, up to [`TOOL_TIME`]. Blocking:
/// a call for the blocking pool.
///
/// An error only when the program could not be started or did not finish,
/// in words to log, not to say: a program that ran and failed is a [`Ran`]
/// with `success` false, and what it said is for the caller to read.
pub(super) fn run(program: &str, args: &[&str]) -> Result<Ran, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program} could not be started: {e}"))?;

    // Read while it runs, not after: a pipe holds 64 KB, and a program that
    // prints more -- Xcode's Info.plist as JSON is 73 KB -- waits on the
    // write until it is killed at TOOL_TIME, and the app it describes was
    // left out of the list.
    let stdout = slurp(child.stdout.take());
    let stderr = slurp(child.stderr.take());

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
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Ran { success: status.success(), stdout, stderr })
}

/// Read everything `pipe` gives, on a thread of its own, until it closes --
/// when the program exits, or is killed.
fn slurp(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

/// Run one line of AppleScript. Blocking.
pub(super) fn script(line: &str) -> Result<Ran, String> {
    run(OSASCRIPT, &["-e", line])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_fails_is_an_answer_and_one_that_is_missing_is_an_error() {
        let ran = run("/bin/sh", &["-c", "echo no >&2; exit 3"]).unwrap();
        assert!(!ran.success);
        assert_eq!(ran.stderr.trim(), "no");
        assert_eq!(run("/bin/echo", &["hi"]).unwrap().stdout.trim(), "hi");
        assert!(run("/nonexistent/tool", &[]).is_err());
    }

    #[test]
    fn dark_mode_is_the_only_setting_offered() {
        let offered = abilities();
        assert!(offered.settings.iter().all(|s| *s == DeskSetting::DarkMode));
    }
}
