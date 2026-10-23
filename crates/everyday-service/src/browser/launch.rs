//! Finding a Chrome or Chromium on this computer, and starting a private,
//! headless copy of it.
//!
//! The browser is the person's own installation, not one this application
//! ships or downloads: a browser is several hundred megabytes that need
//! security updates every few weeks, and the one already installed gets
//! them. What this application controls is how it is started:
//!
//! - **A throwaway profile.** A fresh `--user-data-dir` in a temporary
//!   directory, made private to this user and deleted when the session
//!   ends. Nothing of the person's own browsing -- cookies, saved logins,
//!   history -- is anywhere the assistant's browser can reach, so a page
//!   that talks it into clicking "buy" or "delete account" finds nobody
//!   signed in.
//! - **No way out but the proxy.** `--proxy-server` points at
//!   [`super::proxy`], `<-loopback>` takes away Chrome's own exception for
//!   `localhost`, and WebRTC may not open a socket of its own. See that
//!   module for why.
//! - **No debugging port.** The browser is driven over a pipe
//!   (`--remote-debugging-pipe`) on file descriptors 3 and 4, which only
//!   this process holds; see [`super::cdp`].
//! - **Nothing that would reach the person.** No permission prompts, no
//!   notifications, no new windows (pop-ups are blocked outright), no
//!   extensions, no sync, no background networking of Chrome's own, and a
//!   basic password store so that it never asks the system keyring.
//! - **Gone when this process is.** On Linux the browser is told to die
//!   with its parent, and on every platform the session kills it on drop.
//!
//! Unix only, for now: the pipe is two inherited file descriptors, and
//! Windows passes handles in a different way this does not implement yet.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The environment variable that names a browser to use instead of looking.
pub const BROWSER_ENV: &str = "EVERYDAY_BROWSER";

/// The size of the page the browser draws, in CSS pixels. Fixed, so a
/// screenshot's pixel is a page's pixel -- what a model that points at
/// coordinates needs -- and a page lays out the same way every time.
pub const VIEWPORT: (u32, u32) = (1280, 800);

/// The browser to drive, if this computer has one.
///
/// One that was found is remembered for as long as it is still there; not
/// finding one is not, so a browser installed while the app is open -- what
/// the assistant tells somebody to do when there is none -- is found the next
/// time anything asks.
pub fn executable() -> Option<PathBuf> {
    static FOUND: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut found = FOUND.lock().unwrap();
    if found.as_ref().is_none_or(|p| !p.is_file()) {
        *found = find();
    }
    found.clone()
}

fn find() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os(BROWSER_ENV).map(PathBuf::from) {
        return named.is_file().then_some(named);
    }
    if cfg!(target_os = "macos") {
        return [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file());
    }
    if !cfg!(unix) {
        return None;
    }
    // A snap's Chromium last: it can only see a profile under its own
    // directory, which `profile_root` arranges, but it is the one most
    // likely to surprise.
    let names = [
        "google-chrome-stable",
        "google-chrome",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ];
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let on_path = |name: &str| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
    names
        .iter()
        .filter_map(|n| on_path(n))
        .find(|p| !is_snap(p))
        .or_else(|| names.iter().filter_map(|n| on_path(n)).next())
}

fn is_snap(path: &Path) -> bool {
    path.starts_with("/snap/")
}

/// Where a session's throwaway profile goes. A snap's browser cannot see the
/// system's temporary directory -- it has a private one of its own -- so its
/// profile goes where the snap can write.
fn profile_root(executable: &Path) -> PathBuf {
    if is_snap(executable)
        && let Some(home) = std::env::var_os("HOME")
    {
        let name = executable.file_name().and_then(|n| n.to_str()).unwrap_or("chromium");
        return PathBuf::from(home).join("snap").join(name).join("common");
    }
    std::env::temp_dir()
}

/// The prefix every profile directory's name starts with, so a sweep can
/// find ones a crash left behind.
const PROFILE_PREFIX: &str = "everyday-browser-";

/// The command-line switches every session starts with. See the module doc
/// for what each group is for.
pub fn switches(profile: &Path, proxy: std::net::SocketAddr, headless_shell: bool) -> Vec<String> {
    let (width, height) = VIEWPORT;
    let mut out = vec![
        "--remote-debugging-pipe".to_string(),
        format!("--user-data-dir={}", profile.display()),
        if headless_shell { "--headless".into() } else { "--headless=new".into() },
        format!("--window-size={width},{height}"),
        // No way out but the proxy.
        format!("--proxy-server=http://{proxy}"),
        "--proxy-bypass-list=<-loopback>".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
        "--disable-quic".into(),
        // Nothing that would reach the person, or phone home.
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--deny-permission-prompts".into(),
        "--disable-notifications".into(),
        "--block-new-web-contents".into(),
        "--disable-extensions".into(),
        "--disable-component-extensions-with-background-pages".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-sync".into(),
        "--disable-default-apps".into(),
        "--disable-breakpad".into(),
        "--no-pings".into(),
        "--mute-audio".into(),
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication,\
         InterestFeedContentSuggestions,DialMediaRouteProvider,BackForwardCache"
            .into(),
    ];
    // Chrome will not start as root with its sandbox on, which is what a
    // server in a container usually is. The proxy and the throwaway profile
    // still hold; the sandbox is the one layer lost.
    #[cfg(unix)]
    if is_root() {
        out.push("--no-sandbox".into());
    }
    out.push("about:blank".into());
    out
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// A browser that has been started: the process, its profile directory, and
/// the two ends of its pipe.
pub struct Launched {
    pub child: std::process::Child,
    pub profile: PathBuf,
    pub reader: std::io::PipeReader,
    pub writer: std::io::PipeWriter,
}

/// Start `executable` headless, with a fresh profile, behind `proxy`.
///
/// On a thread kept for nothing else, for the life of the process. Linux's
/// "die with your parent" (`PR_SET_PDEATHSIG`) is about the parent *thread*,
/// not the process: a browser forked from one of tokio's blocking threads
/// would be killed the moment that thread retired, ten idle seconds later.
#[cfg(unix)]
pub fn launch(executable: &Path, proxy: std::net::SocketAddr) -> Result<Launched, String> {
    type Request =
        (PathBuf, std::net::SocketAddr, std::sync::mpsc::Sender<Result<Launched, String>>);
    static LAUNCHER: Mutex<Option<std::sync::mpsc::Sender<Request>>> = Mutex::new(None);
    let (answer, answered) = std::sync::mpsc::channel();
    {
        let mut launcher = LAUNCHER.lock().unwrap();
        let sender = launcher.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Request>();
            std::thread::Builder::new()
                .name("browser-launcher".into())
                .spawn(move || {
                    for (executable, proxy, answer) in rx {
                        let _ = answer.send(launch_here(&executable, proxy));
                    }
                })
                .expect("a thread can be started");
            tx
        });
        sender
            .send((executable.to_path_buf(), proxy, answer))
            .map_err(|_| "the browser launcher has stopped".to_string())?;
    }
    answered.recv().map_err(|_| "the browser launcher has stopped".to_string())?
}

#[cfg(unix)]
fn launch_here(executable: &Path, proxy: std::net::SocketAddr) -> Result<Launched, String> {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let root = profile_root(executable);
    sweep(&root);
    let profile = root.join(format!(
        "{PROFILE_PREFIX}{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&profile)
        .map_err(|e| format!("could not make the browser's profile: {e}"))?;

    // Ours to read what the browser writes, and the browser's to read what
    // we write.
    let (reader, browser_writes) = std::io::pipe().map_err(|e| e.to_string())?;
    let (browser_reads, writer) = std::io::pipe().map_err(|e| e.to_string())?;
    // Moved clear of 3 and 4 first, so the `dup2`s below cannot overwrite
    // one end with the other. Close-on-exec, like every descriptor this
    // process opens: only the two `dup2` copies reach the browser.
    let lift = |fd: OwnedFd| -> Result<OwnedFd, String> {
        // SAFETY: `fcntl(F_DUPFD_CLOEXEC)` on a descriptor we own returns a
        // new descriptor, also ours, or -1.
        let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: `raw` was just returned by the kernel and nothing else
        // owns it.
        Ok(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw) })
    };
    let browser_reads = lift(browser_reads.into())?;
    let browser_writes = lift(browser_writes.into())?;
    let (child_in, child_out) = (browser_reads.as_raw_fd(), browser_writes.as_raw_fd());

    let headless_shell =
        executable.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("headless"));
    let mut command = Command::new(executable);
    command
        .args(switches(&profile, proxy, headless_shell))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe calls between fork and exec: `dup2`,
    // `setpgid` and, on Linux, `prctl`.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(child_in, 3) < 0 || libc::dup2(child_out, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // A process group of its own, so that the browser and every
            // helper it starts -- renderers, the GPU process -- can be ended
            // together. See `end`.
            libc::setpgid(0, 0);
            #[cfg(target_os = "linux")]
            {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                // The launcher could have gone between fork and here.
                if libc::getppid() == 1 {
                    libc::_exit(1);
                }
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|e| {
        let _ = std::fs::remove_dir_all(&profile);
        format!("could not start the browser at {}: {e}", executable.display())
    })?;
    // The browser's ends are its own now; ours are closed so that the pipe
    // ends when the browser does.
    drop(browser_reads);
    drop(browser_writes);
    Ok(Launched { child, profile, reader, writer })
}

/// End a browser [`launch`] started: every process in its group at once,
/// so no helper outlives it to write into the profile while it is removed,
/// and then the profile.
pub fn end(child: &mut std::process::Child, profile: &Path) {
    #[cfg(unix)]
    {
        // SAFETY: `kill` on the group this child leads; the worst a stale id
        // can do is fail.
        if let Ok(group) = i32::try_from(child.id()) {
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    for _ in 0..5 {
        match std::fs::remove_dir_all(profile) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            _ => return,
        }
    }
}

#[cfg(not(unix))]
pub fn launch(_executable: &Path, _proxy: std::net::SocketAddr) -> Result<Launched, String> {
    Err("controlling a browser is not available on this platform yet".into())
}

/// Remove profiles left behind by a process that has gone -- one that
/// crashed, or quit with a browser open -- and any of ours older than a day.
/// A profile's name carries the id of the process that made it.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(PROFILE_PREFIX)) else {
            continue;
        };
        let owner = rest.split('-').next().and_then(|pid| pid.parse::<u32>().ok());
        let orphaned = owner.is_some_and(|pid| pid != std::process::id() && !alive(pid));
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if orphaned || old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether a process with this id is running.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 checks for the process without sending anything.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_whose_process_has_gone_is_swept_and_a_live_ones_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let ours = root.path().join(format!("{PROFILE_PREFIX}{}-a", std::process::id()));
        // No process has this id: Linux's pid_max is far below it.
        let gone = root.path().join(format!("{PROFILE_PREFIX}{}-b", i32::MAX - 1));
        let other = root.path().join("someone-elses");
        for dir in [&ours, &gone, &other] {
            std::fs::create_dir(dir).unwrap();
        }
        sweep(root.path());
        assert!(ours.exists());
        assert!(!gone.exists());
        assert!(other.exists());
    }

    #[test]
    fn every_session_is_proxied_private_and_driven_over_a_pipe() {
        let proxy: std::net::SocketAddr = "127.0.0.1:41000".parse().unwrap();
        let switches = switches(Path::new("/tmp/p"), proxy, false);
        for expected in [
            "--remote-debugging-pipe",
            "--user-data-dir=/tmp/p",
            "--proxy-server=http://127.0.0.1:41000",
            "--proxy-bypass-list=<-loopback>",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
            "--block-new-web-contents",
            "--deny-permission-prompts",
            "--password-store=basic",
        ] {
            assert!(switches.iter().any(|s| s == expected), "missing {expected}");
        }
        assert!(!switches.iter().any(|s| s.starts_with("--remote-debugging-port")));
        assert_eq!(switches.last().map(String::as_str), Some("about:blank"));
    }
}
