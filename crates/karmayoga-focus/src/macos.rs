//! macOS: the frontmost window's owner, the event source's idle clock, and
//! the window server's session dictionary.
//!
//! - **Focus**: `CGWindowListCopyWindowInfo` lists on-screen windows front
//!   to back; the first at layer 0 (ordinary windows -- the menu bar, the
//!   Dock and overlays sit on higher layers) belongs to the application in
//!   front. Its owner pid gives an `NSRunningApplication`, whose bundle id
//!   (`com.apple.Safari`) is the app id and whose localised name is the
//!   name; the window list's own owner name stands in for either when the
//!   program has no bundle. The title is the window's `kCGWindowName`,
//!   which macOS leaves out unless the app has been granted Screen
//!   Recording -- so it is usually empty, as [`Focused::title`] allows.
//!   Unlike `NSWorkspace.frontmostApplication`, none of this needs the main
//!   thread's run loop to be turning, so it answers correctly from the
//!   shell's watcher thread.
//! - **Idle**: `CGEventSourceSecondsSinceLastEventType` for any input
//!   event on the combined session state -- the same clock the screen saver
//!   uses.
//! - **Locked**: `CGSessionCopyCurrentDictionary`'s
//!   `CGSSessionScreenIsLocked`, present and true while the lock screen is
//!   up (and absent otherwise).

use std::time::Duration;

use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::{
    CGEventSource, CGEventSourceStateID, CGEventType, CGSessionCopyCurrentDictionary,
    CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID, kCGWindowLayer, kCGWindowName,
    kCGWindowOwnerName, kCGWindowOwnerPID,
};

use crate::{Backend, Error, Focused, Result, Watcher};

/// `kCGAnyInputEventType`: `~0` in the C headers, which the bindings leave
/// out.
const ANY_INPUT_EVENT: CGEventType = CGEventType(u32::MAX);

/// The key in the session dictionary that is true while the screen is
/// locked. Not in the public headers, but what every macOS idle and lock
/// tool reads.
const SCREEN_IS_LOCKED: &str = "CGSSessionScreenIsLocked";

pub(crate) struct MacOs;

impl MacOs {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// A window dictionary from the window list.
type WindowInfo = CFDictionary<CFString, CFType>;

/// The frontmost ordinary window, as the window list describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrontWindow {
    pid: i32,
    owner: String,
    title: String,
}

fn front_window() -> Option<FrontWindow> {
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID)?;
    // SAFETY: `CGWindowListCopyWindowInfo` is documented to return an array
    // of window dictionaries keyed by `CFString`s. Values are only ever
    // read through checked downcasts.
    let list: CFRetained<CFArray<WindowInfo>> = unsafe { CFRetained::cast_unchecked(list) };
    // SAFETY: CoreGraphics' constant keys, valid for the life of the
    // process.
    let (layer_key, pid_key, owner_key, name_key) =
        unsafe { (kCGWindowLayer, kCGWindowOwnerPID, kCGWindowOwnerName, kCGWindowName) };
    list.iter().find_map(|window| {
        if number(&window, layer_key)?.as_i64()? != 0 {
            return None;
        }
        Some(FrontWindow {
            pid: number(&window, pid_key)?.as_i32()?,
            owner: string(&window, owner_key).unwrap_or_default(),
            title: string(&window, name_key).unwrap_or_default(),
        })
    })
}

fn number(window: &WindowInfo, key: &CFString) -> Option<CFRetained<CFNumber>> {
    window.get(key)?.downcast::<CFNumber>().ok()
}

fn string(window: &WindowInfo, key: &CFString) -> Option<String> {
    window.get(key)?.downcast::<CFString>().ok().map(|text| text.to_string())
}

/// The app id and name for a window: the bundle id and localised name when
/// the running application has them, else the window list's owner name for
/// both. `None` when there is nothing to call it.
fn identify(
    bundle_id: Option<String>,
    localized_name: Option<String>,
    owner: &str,
) -> Option<(String, String)> {
    let owner = owner.trim();
    let bundle_id = bundle_id.map(|id| id.trim().to_owned()).filter(|id| !id.is_empty());
    let name = localized_name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty());
    let app_id = bundle_id.or_else(|| (!owner.is_empty()).then(|| owner.to_owned()))?;
    let name = name.unwrap_or_else(|| owner.to_owned());
    Some((app_id, name))
}

/// Seconds as a duration, with nonsense (negative, not a number) as zero.
fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value).unwrap_or(Duration::ZERO)
}

impl Watcher for MacOs {
    fn backend(&self) -> Backend {
        Backend::MacOs
    }

    fn focused(&mut self) -> Result<Option<Focused>> {
        // A pool of its own: this runs on a thread AppKit did not make, and
        // the strings handed back are autoreleased.
        objc2::rc::autoreleasepool(|_| {
            let Some(window) = front_window() else {
                return Ok(None);
            };
            let app = NSRunningApplication::runningApplicationWithProcessIdentifier(window.pid);
            let bundle_id =
                app.as_ref().and_then(|app| app.bundleIdentifier()).map(|id| id.to_string());
            let localized_name =
                app.as_ref().and_then(|app| app.localizedName()).map(|name| name.to_string());
            Ok(identify(bundle_id, localized_name, &window.owner).map(|(app_id, name)| Focused {
                app_id,
                name,
                title: window.title,
            }))
        })
    }

    fn idle(&mut self) -> Result<Duration> {
        let idle = CGEventSource::seconds_since_last_event_type(
            CGEventSourceStateID::CombinedSessionState,
            ANY_INPUT_EVENT,
        );
        Ok(seconds(idle))
    }

    fn locked(&mut self) -> Result<bool> {
        let Some(session) = CGSessionCopyCurrentDictionary() else {
            return Err(Error::Unavailable(String::from(
                "this process is not in a window server session",
            )));
        };
        // SAFETY: the session dictionary is keyed by `CFString`s; values
        // are only read through checked downcasts.
        let session: CFRetained<CFDictionary<CFString, CFType>> =
            unsafe { CFRetained::cast_unchecked(session) };
        let key = CFString::from_static_str(SCREEN_IS_LOCKED);
        let Some(value) = session.get(&key) else {
            return Ok(false);
        };
        let value = match value.downcast::<CFBoolean>() {
            Ok(flag) => return Ok(flag.as_bool()),
            Err(value) => value,
        };
        Ok(value
            .downcast::<CFNumber>()
            .ok()
            .and_then(|number| number.as_i64())
            .is_some_and(|number| number != 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_id_and_localized_name_win() {
        assert_eq!(
            identify(Some("com.apple.Safari".into()), Some("Safari".into()), "Safari"),
            Some((String::from("com.apple.Safari"), String::from("Safari")))
        );
    }

    #[test]
    fn a_program_without_a_bundle_goes_by_its_owner_name() {
        assert_eq!(
            identify(None, None, "python3"),
            Some((String::from("python3"), String::from("python3")))
        );
        assert_eq!(
            identify(Some(String::new()), Some("Helper".into()), "helper"),
            Some((String::from("helper"), String::from("Helper")))
        );
    }

    #[test]
    fn nothing_to_call_it_is_no_app() {
        assert_eq!(identify(None, None, "  "), None);
    }

    #[test]
    fn idle_seconds_tolerate_nonsense() {
        assert_eq!(seconds(1.5), Duration::from_millis(1500));
        assert_eq!(seconds(-1.0), Duration::ZERO);
        assert_eq!(seconds(f64::NAN), Duration::ZERO);
    }
}
