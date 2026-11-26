//! Lock the screen.
//!
//! Two ways, the first preferred:
//!
//! 1. `SACLockScreenImmediate`, in the login window's private framework
//!    (`login.framework`). It is what the "Lock Screen" menu item and the
//!    Ctrl+Cmd+Q shortcut call: it locks at once, whatever the setting for
//!    asking for a password after sleep. It is private -- there is no public
//!    call that locks -- so it is looked up with `dlopen` and `dlsym` when
//!    asked, never linked: a Mac that has moved or removed it gets the second
//!    way and nothing worse, and a missing symbol is a `NULL` that is
//!    checked, not a crash.
//! 2. `pmset displaysleepnow`, which puts the display to sleep and is public
//!    and stable. It locks only when the Mac is set to ask for a password
//!    after the display sleeps ("Require password after screen saver begins
//!    or display is turned off", on by default and in every managed setup);
//!    a Mac set to wait would be dark and still open. That is why it is
//!    only the fallback.

use std::ffi::{CStr, c_char, c_int, c_void};

const LOGIN_FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/login.framework/login";
const LOCK_SYMBOL: &CStr = c"SACLockScreenImmediate";

/// `dlopen`'s flag for resolving symbols when they are used.
const RTLD_LAZY: c_int = 1;

// Both are in libSystem, which every program on a Mac links already.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Lock the screen, by whichever way works.
pub(in crate::desk) async fn lock() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(lock_now).await.map_err(|e| {
        tracing::warn!(error = %e, "locking did not finish");
        "That didn't finish.".to_string()
    })?
}

fn lock_now() -> Result<(), String> {
    if lock_with_login_window() {
        return Ok(());
    }
    match super::run("/usr/bin/pmset", &["displaysleepnow"]) {
        Ok(ran) if ran.success => Ok(()),
        Ok(ran) => {
            tracing::warn!(stderr = %ran.stderr.trim(), "pmset would not sleep the display");
            Err("The Mac wouldn't lock.".to_string())
        }
        Err(e) => {
            tracing::warn!(error = %e, "the screen could not be locked");
            Err("The Mac wouldn't lock.".to_string())
        }
    }
}

/// Call the login window's lock. `false` when it is not there to call.
fn lock_with_login_window() -> bool {
    // SAFETY: `dlopen` and `dlsym` are given NUL-terminated strings that
    // outlive the calls, and every pointer they return is checked before it
    // is used. A found `SACLockScreenImmediate` is `int (void)` -- the
    // signature it has had since it was introduced -- and the framework is
    // left loaded: it is in the system's shared cache and costs nothing.
    unsafe {
        let framework = dlopen(LOGIN_FRAMEWORK.as_ptr(), RTLD_LAZY);
        if framework.is_null() {
            tracing::debug!("the login framework is not here");
            return false;
        }
        let symbol = dlsym(framework, LOCK_SYMBOL.as_ptr());
        if symbol.is_null() {
            tracing::debug!("the login framework has no lock call");
            return false;
        }
        let lock: unsafe extern "C" fn() -> c_int = std::mem::transmute(symbol);
        let status = lock();
        if status != 0 {
            tracing::debug!(status, "the login window would not lock");
        }
        status == 0
    }
}
