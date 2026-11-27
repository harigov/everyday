//! The macOS side of [`super`]: CoreAudio's own list of who is capturing.
//!
//! Two questions can be put to CoreAudio, and which one is asked depends on
//! the system:
//!
//! - **macOS 14.4 and later** keeps a *process object* for every program that
//!   talks to the audio server, and each says whether it has input running
//!   (`kAudioProcessPropertyIsRunningInput`), its process id and its bundle
//!   id. That is a list of users, like the Linux backend's, and it settles
//!   both problems of the older question: our own process is left out by its
//!   id, whatever recorder inside it is running, and the one that has the
//!   microphone can be named.
//! - **Before that** there is only `kAudioDevicePropertyDeviceIsRunningSomewhere`
//!   on the default input device: true while *any* process has it running,
//!   this one included, with no word of which. It counts as one nameless
//!   user when none of our own recorders is open ([`own_recording`]); while
//!   one is, the reading says nothing new and the last one stands, since
//!   otherwise every meeting we recorded would be offered back to us, or a
//!   call being recorded would seem to end. macOS 14.0 to 14.3 list the
//!   processes but say nothing about them, and are asked this way too.
//!
//! Nothing here opens an input device, so nothing here raises the system's
//! microphone prompt: these are reads of the audio server's bookkeeping.
//! They are polled once a [`TICK`] rather than followed with a property
//! listener. A listener would say "something changed" and still need the
//! reading, the readings cost microseconds, and the tracker wants one per
//! second regardless. Polling also follows a change of default input device
//! for free, since the device is looked up again each time.
//!
//! The CoreAudio calls are declared here with `extern "C"` rather than taken
//! from a crate. `objc2-core-audio` has them (it is in the build already, for
//! cpal), but adding it to this crate's manifest changes `Cargo.lock`, and
//! six functions are not worth that.

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use super::{MicUser, Shared, TICK};

/// A four-character code as CoreAudio writes them, e.g. `'gone'`.
const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// `kAudioObjectSystemObject`: the object that knows about all the others.
const SYSTEM_OBJECT: u32 = 1;
/// `kAudioObjectPropertyScopeGlobal`, `'glob'`: the property of the object
/// itself rather than of one of its inputs or outputs.
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
/// `kAudioObjectPropertyElementMain`: the whole object, not one channel.
const ELEMENT_MAIN: u32 = 0;
/// `kAudioHardwarePropertyDefaultInputDevice`, `'dIn '`: the device that is
/// the microphone now, as System Settings has it.
const DEFAULT_INPUT_DEVICE: u32 = fourcc(b"dIn ");
/// `kAudioDevicePropertyDeviceIsRunningSomewhere`, `'gone'`: nonzero while
/// any process has IO running on the device.
const DEVICE_IS_RUNNING_SOMEWHERE: u32 = fourcc(b"gone");
/// `kAudioHardwarePropertyProcessObjectList`, `'prs#'` (macOS 14.0 and
/// later; its per-process properties below arrived in 14.4): the audio
/// objects of the processes that use the audio server.
const PROCESS_OBJECT_LIST: u32 = fourcc(b"prs#");
/// `kAudioProcessPropertyPID`, `'ppid'`: the process id, an `i32`.
const PROCESS_PID: u32 = fourcc(b"ppid");
/// `kAudioProcessPropertyBundleID`, `'pbid'`: a `CFString`, which is ours to
/// release; empty for a program that is not in a bundle.
const PROCESS_BUNDLE_ID: u32 = fourcc(b"pbid");
/// `kAudioProcessPropertyIsRunningInput`, `'piri'`: nonzero while the
/// process has input running.
const PROCESS_IS_RUNNING_INPUT: u32 = fourcc(b"piri");

/// `kCFStringEncodingUTF8`.
const CF_UTF8: u32 = 0x0800_0100;

/// `AudioObjectPropertyAddress`.
#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(object: *const c_void);
}

fn address(selector: u32) -> PropertyAddress {
    PropertyAddress { selector, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN }
}

/// Read one fixed-size property: `None` when the object does not have it (an
/// older system, a process that exited since the list was read) or CoreAudio
/// says anything else is wrong.
fn get_scalar<T: Copy + Default>(object: u32, selector: u32) -> Option<T> {
    let addr = address(selector);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    // SAFETY: `addr` and `size` live through the call; `value` is a `T` and
    // `size` says so, so CoreAudio writes at most `size_of::<T>()` bytes into
    // it, and the types used here (`u32`, `i32`) are valid for any bits.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut T).cast(),
        )
    };
    (status == 0).then_some(value)
}

/// The processes that use the audio server, as audio object ids; `None`
/// when the system has no such list (before macOS 14).
fn process_objects() -> Option<Vec<u32>> {
    let addr = address(PROCESS_OBJECT_LIST);
    let mut size = 0u32;
    // SAFETY: as for `get_scalar`; only `size` is written.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), &mut size)
    };
    if status != 0 {
        return None;
    }
    let mut ids = vec![0u32; size as usize / std::mem::size_of::<u32>()];
    let mut size = (ids.len() * std::mem::size_of::<u32>()) as u32;
    // SAFETY: `ids` holds `size` bytes and CoreAudio is told that; the
    // process list can grow between the two calls, in which case it fills
    // what fits, and shortens `size` to what it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return None;
    }
    ids.truncate(size as usize / std::mem::size_of::<u32>());
    Some(ids)
}

/// A process's bundle id, empty when it has none or cannot be read.
fn bundle_id(object: u32) -> String {
    let addr = address(PROCESS_BUNDLE_ID);
    let mut string: *const c_void = std::ptr::null();
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    // SAFETY: the property is a `CFStringRef`, written into `string`, which
    // is pointer-sized as `size` says.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            (&mut string as *mut *const c_void).cast(),
        )
    };
    if status != 0 || string.is_null() {
        return String::new();
    }
    let mut buffer = [0 as c_char; 256];
    // SAFETY: `string` is a live `CFString` that this function owns (the
    // property hands out a retained reference); the buffer's length is
    // passed, and the call NUL-terminates what it writes. Released once,
    // after the last use.
    let (ok, text) = unsafe {
        let ok = CFStringGetCString(string, buffer.as_mut_ptr(), buffer.len() as isize, CF_UTF8);
        CFRelease(string);
        (ok != 0, std::ffi::CStr::from_ptr(buffer.as_ptr()))
    };
    if ok { text.to_string_lossy().into_owned() } else { String::new() }
}

/// One process on the audio server, as far as it matters here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// Its audio object id; handed out in increasing order, so a process
    /// that came to the audio server earlier has the smaller one.
    pub object: u32,
    pub pid: i32,
    pub bundle_id: String,
}

/// Every process that has input running right now, or `None` where the
/// system cannot say.
fn capturing_processes() -> Option<Vec<Process>> {
    let objects = process_objects()?;
    let mut found = Vec::new();
    let mut answered = false;
    for object in objects.iter().copied() {
        // A process that is gone since the list was read, or that does not
        // answer, is not capturing as far as anyone can tell.
        let Some(running) = get_scalar::<u32>(object, PROCESS_IS_RUNNING_INPUT) else {
            continue;
        };
        answered = true;
        if running == 0 {
            continue;
        }
        let pid = get_scalar::<i32>(object, PROCESS_PID).unwrap_or(-1);
        found.push(Process { object, pid, bundle_id: bundle_id(object) });
    }
    // macOS 14.0 to 14.3 list the processes but answer nothing about them:
    // that is "cannot say", for the device-level reading to answer instead,
    // not "nobody", which would never notice a call there at all.
    if !objects.is_empty() && !answered {
        return None;
    }
    Some(found)
}

/// Whether any process has the default input device running. `None` when
/// there is no default input device, or it cannot be read.
fn default_input_running() -> Option<bool> {
    let device = get_scalar::<u32>(SYSTEM_OBJECT, DEFAULT_INPUT_DEVICE).filter(|d| *d != 0)?;
    Some(get_scalar::<u32>(device, DEVICE_IS_RUNNING_SOMEWHERE)? != 0)
}

/// Bundle ids that are not programs a call happens in, though they have the
/// microphone: audio routing and metering tools that open the input to
/// route or show it. Matched case-insensitively as a prefix of the bundle
/// id, and this program's own, in case a second copy of it is open.
const PLUMBING: &[&str] = &[
    "com.rogueamoeba.",
    "com.existentialaudio.",
    "com.eqmac.",
    "com.sound-source.",
    "app.jatayu.",
];

/// The `com.apple.` programs that are a place for calls (or may be a
/// browser's). Everything else Apple signs that captures is the system's
/// own -- dictation, Siri, Voice Memos, the speech and accessibility
/// services -- and not another program's call.
const APPLE_CALL_PROGRAMS: &[&str] =
    &["com.apple.FaceTime", "com.apple.Safari", "com.apple.WebKit"];

/// Names for the bundle ids people take calls in. A bundle id matches when
/// it is the id or starts with it and a dot, so a program's helper
/// processes (`com.google.Chrome.helper`) are named for the program.
const KNOWN_APPS: &[(&str, &str)] = &[
    ("us.zoom.xos", "Zoom"),
    ("com.microsoft.teams2", "Microsoft Teams"),
    ("com.microsoft.teams", "Microsoft Teams"),
    ("com.apple.FaceTime", "FaceTime"),
    ("com.apple.Safari", "Safari"),
    ("com.google.Chrome", "Google Chrome"),
    ("org.mozilla.firefox", "Firefox"),
    ("com.microsoft.edgemac", "Microsoft Edge"),
    ("com.brave.Browser", "Brave"),
    ("company.thebrowser.Browser", "Arc"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.hnc.Discord", "Discord"),
    ("com.cisco.webexmeetingsapp", "Webex"),
    ("Cisco-Systems.Spark", "Webex"),
    ("com.skype.skype", "Skype"),
    ("com.loom.desktop", "Loom"),
    ("com.openai.chat", "ChatGPT"),
];

fn has_prefix_id(bundle_id: &str, id: &str) -> bool {
    // As bytes: the ids here are ASCII, and a bundle id need not be, so a
    // string slice at `id.len()` could land inside a character and panic.
    let (bundle, id) = (bundle_id.as_bytes(), id.as_bytes());
    bundle.eq_ignore_ascii_case(id)
        || (bundle.len() > id.len()
            && bundle[..id.len()].eq_ignore_ascii_case(id)
            && bundle[id.len()] == b'.')
}

/// What to call the program with this bundle id, when it is one of the
/// well-known ones. Not guessed for the rest: a wrong name in an offer is
/// worse than saying "a call".
pub fn app_name(bundle_id: &str) -> Option<String> {
    KNOWN_APPS
        .iter()
        .find(|(id, _)| has_prefix_id(bundle_id, id))
        .map(|(_, name)| (*name).to_owned())
}

/// Whether a capturing process with this bundle id is a program a person
/// is on a call in. A process with no bundle (a daemon, a command-line
/// tool) is not, and neither is the system's own or an audio tool.
fn is_call_program(bundle_id: &str) -> bool {
    if bundle_id.is_empty() {
        return false;
    }
    let lowered = bundle_id.to_lowercase();
    if PLUMBING.iter().any(|p| lowered.starts_with(p)) {
        return false;
    }
    !lowered.starts_with("com.apple.")
        || APPLE_CALL_PROGRAMS.iter().any(|p| has_prefix_id(bundle_id, p))
}

/// From the processes that have input running, the ones that count as
/// "another program is using the microphone": not `own_pid`, and
/// [`is_call_program`]. Oldest on the audio server first, and named where
/// the name is known.
// The watcher itself asks `pick_users_naming`, knowing whether Safari is
// open; the tests ask this.
#[cfg(test)]
pub fn pick_users(processes: &[Process], own_pid: u32) -> Vec<(u32, MicUser)> {
    pick_users_naming(processes, own_pid, false)
}

/// [`pick_users`], told whether Safari is running. Safari takes a call's
/// microphone in WebKit's GPU process, not its own, and that process is
/// any WebKit program's: it is named Safari only while Safari is open, and
/// otherwise not at all -- a wrong name is worse than "a call".
fn pick_users_naming(processes: &[Process], own_pid: u32, safari: bool) -> Vec<(u32, MicUser)> {
    let mut users: Vec<(u32, MicUser)> = processes
        .iter()
        .filter(|p| u32::try_from(p.pid).ok() != Some(own_pid) && is_call_program(&p.bundle_id))
        .map(|p| {
            let pid = u32::try_from(p.pid).ok();
            let app = app_name(&p.bundle_id)
                .or_else(|| (safari && is_webkit(&p.bundle_id)).then(|| "Safari".to_owned()));
            // The object is CoreAudio's per *process*, not per stream: two
            // calls in one browser share it, so only a change of process
            // reads as a handover (see [`MicUser::id`]).
            (p.object, MicUser { app, pid, id: Some(p.object) })
        })
        .collect();
    users.sort_by_key(|(object, _)| *object);
    users
}

fn is_webkit(bundle_id: &str) -> bool {
    has_prefix_id(bundle_id, "com.apple.WebKit")
}

/// Whether Safari is open. Asked only when a WebKit process is capturing.
fn safari_running() -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // On this watcher thread, which has no pool of its own for what AppKit
    // hands back autoreleased.
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: `stringWithUTF8String:` is given a NUL-terminated literal,
        // and `runningApplicationsWithBundleIdentifier:` an NSString; both
        // answers are checked before use and only counted.
        unsafe {
            let id: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: c"com.apple.Safari".as_ptr()];
            if id.is_null() {
                return false;
            }
            let apps: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationsWithBundleIdentifier: id
            ];
            if apps.is_null() {
                return false;
            }
            let count: usize = msg_send![apps, count];
            count > 0
        }
    })
}

/// Whether one of our own recorders -- meeting notes, voice to the
/// assistant, voice typing, a listen -- has the microphone open. Only the
/// older systems' answer needs this, as they cannot tell whose it is.
fn own_recording(app: &AppHandle) -> bool {
    let state = app.state::<crate::state::AppState>();
    state.capture().lock().is_ok_and(|s| s.is_some())
        || state.voice().lock().is_ok_and(|s| s.is_some())
        || state.dictation().lock().is_ok_and(|s| s.is_some())
        || state.listen().lock().is_ok_and(|s| s.is_some())
        || crate::wake::hearing()
}

/// One reading of who has the microphone, or `None` when this moment cannot
/// say and the last reading should stand.
///
/// That is the older systems' answer while a recorder of ours has the
/// device: "running somewhere" is then true whether or not a call is on
/// too. Reading it as nobody ended a call 45 seconds into the notes that
/// were taken on it -- the recording itself made the call look over -- so
/// it is read as nothing new instead. (The wake word listening all the time
/// is a recorder of ours too: there, on those systems, no call is noticed
/// while it listens, which is the most they can be asked.)
fn read(app: &AppHandle, own_pid: u32) -> Option<Vec<(u32, MicUser)>> {
    match capturing_processes() {
        Some(processes) => {
            let safari = processes.iter().any(|p| is_webkit(&p.bundle_id)) && safari_running();
            Some(pick_users_naming(&processes, own_pid, safari))
        }
        None => match default_input_running() {
            Some(true) if own_recording(app) => None,
            Some(true) => Some(vec![(0, MicUser { app: None, pid: None, id: None })]),
            _ => Some(Vec::new()),
        },
    }
}

pub fn spawn(app: AppHandle, shared: Arc<Shared>) {
    let spawned = std::thread::Builder::new().name("micwatch-coreaudio".into()).spawn(move || {
        let own_pid = std::process::id();
        match process_objects() {
            Some(_) => tracing::debug!("micwatch: reading CoreAudio's process list"),
            None => tracing::debug!(
                "micwatch: no process list on this macOS; reading the default input device"
            ),
        }
        loop {
            if let Some(users) = read(&app, own_pid) {
                *shared.users() = users.into_iter().collect();
            }
            std::thread::sleep(TICK);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "micwatch: could not start the CoreAudio thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 4242;

    fn proc(object: u32, pid: i32, bundle: &str) -> Process {
        Process { object, pid, bundle_id: bundle.into() }
    }

    #[test]
    fn four_character_codes_are_spelled_out() {
        assert_eq!(DEVICE_IS_RUNNING_SOMEWHERE, 0x676f6e65);
        assert_eq!(PROCESS_OBJECT_LIST, 0x70727323);
        assert_eq!(PROCESS_IS_RUNNING_INPUT, 0x70697269);
        assert_eq!(PROCESS_BUNDLE_ID, 0x70626964);
        assert_eq!(PROCESS_PID, 0x70706964);
        assert_eq!(DEFAULT_INPUT_DEVICE, 0x64496e20);
        assert_eq!(SCOPE_GLOBAL, 0x676c6f62);
    }

    #[test]
    fn our_own_process_is_not_another_program() {
        let users = pick_users(&[proc(7, OWN as i32, "app.example.Other")], OWN);
        assert!(users.is_empty());
        let users = pick_users(&[proc(7, 99, "us.zoom.xos")], OWN);
        assert_eq!(
            users,
            vec![(7, MicUser { app: Some("Zoom".into()), pid: Some(99), id: Some(7) })]
        );
    }

    #[test]
    fn the_longest_on_the_audio_server_comes_first() {
        let users =
            pick_users(&[proc(90, 3, "com.hnc.Discord"), proc(12, 2, "com.microsoft.teams2")], OWN);
        let apps: Vec<_> = users.into_iter().filter_map(|(_, u)| u.app).collect();
        assert_eq!(apps, ["Microsoft Teams", "Discord"]);
    }

    #[test]
    fn helper_processes_are_named_for_their_program() {
        assert_eq!(app_name("com.google.Chrome.helper").as_deref(), Some("Google Chrome"));
        assert_eq!(app_name("com.microsoft.teams2.modulehost").as_deref(), Some("Microsoft Teams"));
        // A longer id that merely starts the same is another program.
        assert_eq!(app_name("us.zoom.xosplugin"), None);
        assert_eq!(app_name("US.ZOOM.XOS").as_deref(), Some("Zoom"));
        assert_eq!(app_name(""), None);
        assert_eq!(app_name("com.apple.FaceTime").as_deref(), Some("FaceTime"));
    }

    #[test]
    fn webkit_is_named_safari_only_while_safari_is_open() {
        let gpu = [proc(5, 50, "com.apple.WebKit.GPU")];
        assert_eq!(pick_users_naming(&gpu, OWN, true)[0].1.app.as_deref(), Some("Safari"));
        assert_eq!(pick_users_naming(&gpu, OWN, false)[0].1.app, None);
        // Another program keeps its own name, Safari open or not.
        let zoom = [proc(6, 51, "us.zoom.xos")];
        assert_eq!(pick_users_naming(&zoom, OWN, true)[0].1.app.as_deref(), Some("Zoom"));
    }

    #[test]
    fn a_bundle_id_that_is_not_ascii_is_compared_without_panicking() {
        // "us.zoom.xo" and then an é, two bytes across where a slice of the
        // id's length would cut.
        assert_eq!(app_name("us.zoom.xo\u{e9}x"), None);
        assert_eq!(app_name("\u{e9}"), None);
    }

    #[test]
    fn an_unknown_program_still_counts_but_has_no_name() {
        let users = pick_users(&[proc(5, 50, "com.example.NewCallApp")], OWN);
        assert_eq!(users, vec![(5, MicUser { app: None, pid: Some(50), id: Some(5) })]);
    }

    #[test]
    fn the_systems_own_services_and_audio_tools_are_not_calls() {
        for bundle in [
            "",
            "com.apple.speech.recognitionserver",
            "com.apple.VoiceMemos",
            "com.apple.assistant_service",
            "com.apple.QuickTimePlayerX",
            "com.rogueamoeba.audiohijack",
            "com.existentialaudio.BlackHole",
            "app.jatayu.Jatayu",
        ] {
            assert!(pick_users(&[proc(5, 50, bundle)], OWN).is_empty(), "{bundle:?}");
        }
        // Apple's own call programs, and Safari's web process, are.
        for bundle in ["com.apple.FaceTime", "com.apple.Safari", "com.apple.WebKit.GPU"] {
            assert_eq!(pick_users(&[proc(5, 50, bundle)], OWN).len(), 1, "{bundle}");
        }
    }

    #[test]
    fn a_process_with_no_valid_pid_is_kept_without_one() {
        let users = pick_users(&[proc(5, -1, "us.zoom.xos")], OWN);
        assert_eq!(users, vec![(5, MicUser { app: Some("Zoom".into()), pid: None, id: Some(5) })]);
    }

    /// Reads CoreAudio once, for a person to look at: `cargo test ... --
    /// --ignored --nocapture`. Opens no microphone.
    #[test]
    #[ignore]
    fn live_reading() {
        println!("process list available: {}", process_objects().is_some());
        let processes = capturing_processes();
        println!("processes with input running: {processes:?}");
        println!("default input running somewhere: {:?}", default_input_running());
        if let Some(p) = processes {
            println!("counted as users: {:?}", pick_users(&p, std::process::id()));
        }
    }
}
