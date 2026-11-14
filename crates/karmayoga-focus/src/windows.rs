//! Windows: the foreground window's program, the last-input clock, and
//! whether the input desktop is still the person's own.
//!
//! - **Focus**: `GetForegroundWindow`, its process (`GetWindowThreadProcessId`),
//!   and that process's executable (`QueryFullProcessImageNameW` on a handle
//!   opened with `PROCESS_QUERY_LIMITED_INFORMATION`, the one right Windows
//!   grants on almost any process, elevated ones included). The executable's
//!   file name (`Code.exe`) is the app id and its stem (`Code`) the name;
//!   `GetWindowTextW` gives the title, and reads the window's caption
//!   without sending a message, so a hung program cannot hang this.
//! - **Idle**: `GetLastInputInfo` against `GetTickCount`. Both are 32-bit
//!   millisecond counters that wrap every 49.7 days, so the difference is
//!   taken with wrapping arithmetic.
//! - **Locked**: while the screen is locked, the input desktop is
//!   Winlogon's secure desktop, which this process may not open --
//!   `OpenInputDesktop` failing, or a desktop not named `Default`, means
//!   locked. The Windows 10/11 lock screen (`LockApp.exe`) is drawn on the
//!   person's own desktop before the password prompt, so a foreground
//!   `LockApp.exe` counts as locked too.
//!
//! Every handle opened here is closed by a guard, so an early return cannot
//! leak one.

use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, HDESK, OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

use crate::{Backend, Error, Focused, Result, Watcher};

/// The lock screen's own program.
const LOCK_APP: &str = "LockApp.exe";

/// The longest path `QueryFullProcessImageNameW` can return, in UTF-16
/// units (the limit of a `UNICODE_STRING`).
const MAX_PATH_UNITS: usize = 32_768;

pub(crate) struct Windows;

impl Windows {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// Closes a process handle when dropped.
struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcess`, is not null, and is
        // closed exactly once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// Closes a desktop handle when dropped.
struct DesktopHandle(HDESK);

impl Drop for DesktopHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenInputDesktop`, is not null, and
        // is closed exactly once, here.
        unsafe { CloseDesktop(self.0) };
    }
}

fn last_error(what: &str) -> Error {
    Error::Failed(format!("{what}: {}", std::io::Error::last_os_error()))
}

/// The foreground window, its process id and its executable's full path.
/// `None` when no window is in the foreground (the moment of a switch, the
/// secure desktop).
fn foreground() -> Result<Option<(HWND, String)>> {
    // SAFETY: no arguments; returns null when there is no foreground window.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return Ok(None);
    }
    let mut pid = 0u32;
    // SAFETY: `window` is a window handle (a stale one makes this return 0
    // and leave `pid` alone), and `pid` is a valid place to write.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == 0 {
        return Ok(None);
    }
    Ok(Some((window, image_path(pid)?)))
}

/// The full path of process `pid`'s executable.
fn image_path(pid: u32) -> Result<String> {
    // SAFETY: plain values in; a null handle out on failure.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(last_error("could not open the foreground process"));
    }
    let process = ProcessHandle(process);
    let mut buffer = vec![0u16; MAX_PATH_UNITS];
    let mut size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: `process` is open with the right access; `buffer` holds
    // `size` UTF-16 units, and `size` is updated to the length written.
    let ok = unsafe {
        QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
    };
    if ok == 0 {
        return Err(last_error("could not read the foreground program's path"));
    }
    let written = buffer.get(..size as usize).unwrap_or(&[]);
    Ok(String::from_utf16_lossy(written))
}

/// The window's title, or empty.
fn window_title(window: HWND) -> String {
    // SAFETY: a window handle in; 0 for a stale handle or no title.
    let length = unsafe { GetWindowTextLengthW(window) };
    let Ok(length) = usize::try_from(length) else {
        return String::new();
    };
    if length == 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length + 1];
    let capacity = i32::try_from(buffer.len()).unwrap_or(i32::MAX);
    // SAFETY: `buffer` holds `capacity` UTF-16 units; the call writes at
    // most that many, the terminating NUL included, and returns how many
    // it wrote before the NUL.
    let copied = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), capacity) };
    let copied = usize::try_from(copied).unwrap_or(0);
    String::from_utf16_lossy(buffer.get(..copied).unwrap_or(&[]))
}

/// The app id and name an executable path gives: the file name and its
/// stem. `None` for a path with no file name.
fn exe_names(path: &str) -> Option<(String, String)> {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path).trim();
    if file.is_empty() {
        return None;
    }
    let stem = match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file,
    };
    Some((file.to_owned(), stem.to_owned()))
}

/// Milliseconds since the last input, from the two 32-bit tick counts.
fn idle_millis(now: u32, last_input: u32) -> u32 {
    now.wrapping_sub(last_input)
}

/// Whether the input desktop's name is the person's own.
fn is_default_desktop(name: &str) -> bool {
    name.eq_ignore_ascii_case("Default")
}

/// The name of the desktop that receives input, or `None` when this process
/// may not even open it -- which is what the secure desktop does.
fn input_desktop_name() -> Option<String> {
    // SAFETY: plain values in; a null handle out on failure.
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desktop.is_null() {
        return None;
    }
    let desktop = DesktopHandle(desktop);
    let mut name = [0u16; 256];
    let mut needed = 0u32;
    let bytes = u32::try_from(std::mem::size_of_val(&name)).unwrap_or(u32::MAX);
    // SAFETY: `desktop` is open; `name` is `bytes` long; `needed` is a
    // valid place to write.
    let ok = unsafe {
        GetUserObjectInformationW(desktop.0, UOI_NAME, name.as_mut_ptr().cast(), bytes, &mut needed)
    };
    if ok == 0 {
        return None;
    }
    let end = name.iter().position(|&unit| unit == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(name.get(..end).unwrap_or(&[])))
}

impl Watcher for Windows {
    fn backend(&self) -> Backend {
        Backend::Windows
    }

    fn focused(&mut self) -> Result<Option<Focused>> {
        let Some((window, path)) = foreground()? else {
            return Ok(None);
        };
        let Some((app_id, name)) = exe_names(&path) else {
            return Ok(None);
        };
        // The lock screen is not something the person is using.
        if app_id.eq_ignore_ascii_case(LOCK_APP) {
            return Ok(None);
        }
        Ok(Some(Focused { app_id, name, title: window_title(window) }))
    }

    fn idle(&mut self) -> Result<Duration> {
        let mut info = LASTINPUTINFO {
            cbSize: u32::try_from(std::mem::size_of::<LASTINPUTINFO>()).unwrap_or(0),
            dwTime: 0,
        };
        // SAFETY: `info` is a `LASTINPUTINFO` with its size filled in.
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return Err(last_error("GetLastInputInfo failed"));
        }
        // SAFETY: no arguments.
        let now = unsafe { GetTickCount() };
        Ok(Duration::from_millis(u64::from(idle_millis(now, info.dwTime))))
    }

    fn locked(&mut self) -> Result<bool> {
        match input_desktop_name() {
            Some(name) if is_default_desktop(&name) => {}
            _ => return Ok(true),
        }
        let lock_app_in_front = matches!(
            foreground(),
            Ok(Some((_, path))) if exe_names(&path).is_some_and(|(app_id, _)| app_id.eq_ignore_ascii_case(LOCK_APP))
        );
        Ok(lock_app_in_front)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exe_file_name_is_the_app_id_and_its_stem_the_name() {
        assert_eq!(
            exe_names(r"C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe"),
            Some((String::from("Code.exe"), String::from("Code")))
        );
        assert_eq!(
            exe_names(r"C:\Windows\SystemApps\Microsoft.LockApp_cw5n1h2txyewy\LockApp.exe"),
            Some((String::from("LockApp.exe"), String::from("LockApp")))
        );
        assert_eq!(exe_names("notepad"), Some((String::from("notepad"), String::from("notepad"))));
        assert_eq!(exe_names(r"C:\dir\"), None);
    }

    #[test]
    fn idle_time_survives_the_tick_count_wrapping() {
        assert_eq!(idle_millis(10_000, 4_000), 6_000);
        // The tick count wrapped between the last input and now.
        assert_eq!(idle_millis(1_000, u32::MAX - 999), 2_000);
        assert_eq!(idle_millis(5, 5), 0);
    }

    #[test]
    fn only_the_default_desktop_is_unlocked() {
        assert!(is_default_desktop("Default"));
        assert!(is_default_desktop("default"));
        assert!(!is_default_desktop("Winlogon"));
        assert!(!is_default_desktop(""));
    }
}
