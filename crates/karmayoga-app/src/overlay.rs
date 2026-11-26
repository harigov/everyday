//! The windows that float over whatever the person is doing -- the panel's
//! cards, the voice-typing pill, writing help -- over a full-screen app too.
//!
//! Since macOS 10.14 an ordinary window of an app that is not in front cannot
//! appear over another app's full-screen space, however it is flagged: a call
//! offer came up on the desktop and not over the full-screen call it was
//! about, and voice typing's pill and its "set up a model" card never showed
//! over the app being typed into. A panel can -- Spotlight and every launcher
//! like it are panels -- so on a Mac each of these windows, once built,
//! becomes a non-activating `NSPanel`: it floats on every space, full-screen
//! ones included, without bringing the app to the front. (Electron's answer
//! is to hide the Dock icon, which this app, with its badge, does not want.)
//!
//! Tauri builds an `NSWindow` -- tao's `TaoWindow` -- and making it a panel
//! means changing its class, as tauri-nspanel does. The class here is
//! `NSPanel`'s, given tao's `focusable` instance variable and the two methods
//! tao answers from it, so tao keeps working on the window it made; and it is
//! only swapped in when it fits in the memory the window already has.
//! Elsewhere this does nothing: GTK's and Windows' always-on-top windows
//! already show over a full-screen app.
//!
//! A panel made so can still be given the keyboard when the person clicks
//! into a box on it -- a reply on the panel's mail card -- by making it the
//! key window (`make_key`), which a non-activating panel may be while
//! another app stays the active one; and it gives it back to that app when
//! the box is done (`give_back_key`).

/// The builder every floating window starts from: a frameless, transparent,
/// shadowless window of a fixed size that stays out of the taskbar, loads one
/// page of the interface and never navigates away from it (see `navigation`).
/// What differs between them -- whether it stays on top, is shown, takes the
/// keyboard -- is the caller's to add; and each calls [`float_over_full_screen`]
/// on what it builds.
pub fn popup_builder<'a>(
    app: &'a tauri::AppHandle,
    label: &str,
    page: &str,
    title: &str,
    (width, height): (f64, f64),
) -> tauri::WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle> {
    tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App(page.into()))
        .title(title)
        .inner_size(width, height)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .on_navigation(crate::navigation::guard(crate::navigation::dev_url(app)))
}

/// Make `window` float over every space, full-screen apps' included. Call
/// once, right after building it; it is done on the main thread.
#[cfg(target_os = "macos")]
pub fn float_over_full_screen(window: &tauri::WebviewWindow) {
    let ns_window = match window.ns_window() {
        // As an address, to cross to the main thread; it is the window's own
        // and lives as long as the window, which outlives this call.
        Ok(pointer) => pointer as usize,
        Err(e) => {
            tracing::warn!(error = %e, "an overlay window has no NSWindow to make a panel of");
            return;
        }
    };
    let made = window.run_on_main_thread(move || {
        // SAFETY: on the main thread, as AppKit wants, with the window's own
        // NSWindow; see `macos::make_panel` for the class change.
        unsafe { macos::make_panel(ns_window as *mut objc2::runtime::AnyObject) }
    });
    if let Err(e) = made {
        tracing::warn!(error = %e, "an overlay window could not be made a panel");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn float_over_full_screen(_window: &tauri::WebviewWindow) {}

/// Give `window` -- one [`float_over_full_screen`] has made a panel, and
/// made focusable since (`set_focusable(true)`) -- the keyboard, without
/// bringing the app forward. On the main thread.
///
/// What a box on the panel's cards does when it is clicked into. Tao's own
/// `set_focus` would be wrong here: it activates the whole application
/// (`activateIgnoringOtherApps`), which brings the main window forward and,
/// over a full-screen app, switches the Space away from it. A non-activating
/// panel made key takes the keys while the other application stays the
/// active one.
#[cfg(target_os = "macos")]
pub fn make_key(window: &tauri::WebviewWindow) {
    let ns_window = match window.ns_window() {
        Ok(pointer) => pointer as usize,
        Err(e) => {
            tracing::warn!(error = %e, "an overlay window has no NSWindow to give the keyboard");
            return;
        }
    };
    let made = window.run_on_main_thread(move || {
        // SAFETY: on the main thread, with the window's own NSWindow, which
        // outlives this call as it does `float_over_full_screen`'s.
        unsafe { macos::make_key(ns_window as *mut objc2::runtime::AnyObject) }
    });
    if let Err(e) = made {
        tracing::warn!(error = %e, "an overlay window could not be given the keyboard");
    }
}

/// Give the keyboard back from `window` -- a panel [`make_key`] made the key
/// window, and made unable to be key again since (`set_focusable(false)`)
/// -- to the application that is active, which it never stopped being. On
/// the main thread.
///
/// Being made unable to be key does not take the keyboard from a window that
/// has it: AppKit asks a window whether it may be key only as it is about to
/// become so. Nor may a window simply resign: AppKit says never to call
/// `resignKeyWindow`. Ordered out, the panel is not key, and the window
/// server hands the keyboard to the active application's own key window;
/// ordered straight back in front -- without being made key, which it may no
/// longer be anyway -- it is still up, over a full-screen app too.
#[cfg(target_os = "macos")]
pub fn give_back_key(window: &tauri::WebviewWindow) {
    let ns_window = match window.ns_window() {
        Ok(pointer) => pointer as usize,
        Err(e) => {
            tracing::warn!(error = %e, "an overlay window has no NSWindow to take the keyboard from");
            return;
        }
    };
    let given = window.run_on_main_thread(move || {
        // SAFETY: on the main thread, with the window's own NSWindow, which
        // outlives this call as it does `make_key`'s.
        unsafe { macos::give_back_key(ns_window as *mut objc2::runtime::AnyObject) }
    });
    if let Err(e) = given {
        tracing::warn!(error = %e, "an overlay window could not give the keyboard back");
    }
}

/// Whether the primary mouse button is down now. Any thread may ask: it is
/// the window server's state, not that of an event being handled -- what the
/// panel's drag goes by while the window server, not the app, moves it.
#[cfg(target_os = "macos")]
pub fn primary_button_down() -> bool {
    // SAFETY: a class method of NSEvent that takes nothing and answers an
    // NSUInteger, a bit per button down, the primary button's first.
    let pressed: usize = unsafe { objc2::msg_send![objc2::class!(NSEvent), pressedMouseButtons] };
    pressed & 1 != 0
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::sync::OnceLock;

    use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2::{class, msg_send, sel};

    /// `NSWindowStyleMaskNonactivatingPanel`: clicking it does not bring the
    /// app forward.
    const NONACTIVATING_PANEL: usize = 1 << 7;
    /// `NSWindowCollectionBehaviorCanJoinAllSpaces`.
    const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    /// `NSWindowCollectionBehaviorFullScreenAuxiliary`: may share a space
    /// with a full-screen window.
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

    /// tao's instance variable for whether the window may take the keyboard,
    /// which its `set_focusable` writes and its key-window methods read.
    const FOCUSABLE: &CStr = c"focusable";

    extern "C-unwind" fn focusable(this: &AnyObject, _: Sel) -> Bool {
        match this.class().instance_variable(FOCUSABLE) {
            // SAFETY: declared below as a `Bool`, on this very class.
            Some(ivar) => unsafe { *ivar.load::<Bool>(this) },
            None => Bool::NO,
        }
    }

    fn panel_class() -> &'static AnyClass {
        static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
        CLASS.get_or_init(|| {
            let mut builder = ClassBuilder::new(c"KarmaYogaOverlayPanel", class!(NSPanel))
                .expect("the overlay panel class is declared once");
            builder.add_ivar::<Bool>(FOCUSABLE);
            // SAFETY: both are `- (BOOL)name` on NSWindow, and the function
            // matches that signature.
            unsafe {
                builder.add_method(
                    sel!(canBecomeKeyWindow),
                    focusable as extern "C-unwind" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(canBecomeMainWindow),
                    focusable as extern "C-unwind" fn(_, _) -> _,
                );
            }
            builder.register()
        })
    }

    /// Make the NSWindow at `ns_window` a non-activating panel on every
    /// space.
    ///
    /// # Safety
    ///
    /// On the main thread, with a live NSWindow that tao made.
    pub(super) unsafe fn make_panel(ns_window: *mut AnyObject) {
        if ns_window.is_null() {
            return;
        }
        // SAFETY: the caller's: a live NSWindow.
        let window = unsafe { &mut *ns_window };
        let panel = panel_class();
        let current = window.class();
        if std::ptr::eq(current, panel) {
            return;
        }
        // The object keeps the memory it was made with: a class that needs
        // more would write past it.
        if panel.instance_size() > current.instance_size() {
            tracing::warn!(
                panel = panel.instance_size(),
                window = current.instance_size(),
                "an overlay window is too small to become a panel; it stays a window"
            );
            return;
        }
        let was_focusable = current
            .instance_variable(FOCUSABLE)
            // SAFETY: tao declares it as a `Bool`.
            .map(|ivar| unsafe { *ivar.load::<Bool>(window) })
            .unwrap_or(Bool::NO);
        // SAFETY: an NSPanel is an NSWindow, the new class fits the object
        // (checked above), and the one instance variable tao reads is given
        // its value again just below, at its offset in the new class.
        unsafe {
            objc2::ffi::object_setClass(ns_window, panel);
            if let Some(ivar) = panel.instance_variable(FOCUSABLE) {
                *ivar.load_mut::<Bool>(&mut *ns_window) = was_focusable;
            }
            let window = &*ns_window;
            let mask: usize = msg_send![window, styleMask];
            let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
            // Above ordinary windows, and kept up when the app is not in
            // front -- a panel hides itself then by default.
            let _: () = msg_send![window, setFloatingPanel: Bool::YES];
            let _: () = msg_send![window, setHidesOnDeactivate: Bool::NO];
            // And kept up when the app hides: writing help hides the whole
            // app to hand the keyboard back (`app.hide()`), which would take
            // the panel with it.
            let _: () = msg_send![window, setCanHide: Bool::NO];
            let behavior: usize = msg_send![window, collectionBehavior];
            let _: () = msg_send![
                window,
                setCollectionBehavior: behavior | CAN_JOIN_ALL_SPACES | FULL_SCREEN_AUXILIARY
            ];
        }
    }

    /// Make the NSWindow at `ns_window` the key window, which a panel may be
    /// without its app becoming the active one.
    ///
    /// # Safety
    ///
    /// On the main thread, with a live NSWindow.
    pub(super) unsafe fn make_key(ns_window: *mut AnyObject) {
        if ns_window.is_null() {
            return;
        }
        // SAFETY: the caller's: a live NSWindow, on the main thread.
        unsafe {
            let window = &*ns_window;
            let _: () = msg_send![window, makeKeyWindow];
        }
    }

    /// Take the keyboard from the NSWindow at `ns_window`, if it has it, by
    /// ordering it out and straight back in front; see `super::give_back_key`.
    ///
    /// # Safety
    ///
    /// On the main thread, with a live NSWindow.
    pub(super) unsafe fn give_back_key(ns_window: *mut AnyObject) {
        if ns_window.is_null() {
            return;
        }
        // SAFETY: the caller's: a live NSWindow, on the main thread.
        unsafe {
            let window = &*ns_window;
            let key: bool = msg_send![window, isKeyWindow];
            if !key {
                return;
            }
            let visible: bool = msg_send![window, isVisible];
            let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
            if visible {
                let _: () = msg_send![window, orderFrontRegardless];
            }
        }
    }
}
