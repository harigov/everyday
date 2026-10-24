//! Where this application's own windows may navigate to.
//!
//! A window that loads the interface never has a reason to leave it. The
//! interface is one document, every link that goes out to the web is meant
//! for the system browser (`ui/src/lib/open-external.ts`), and a message,
//! story or invitation is drawn inside a sandboxed `srcdoc` frame that
//! cannot navigate anything but itself. So a top-level navigation anywhere
//! else is either a link that forgot to go through `openExternal` or
//! somebody else's address doing the steering -- a subscribed calendar's
//! event URL, say -- and neither should replace the window.
//!
//! It matters more than a lost page would suggest, because Tauri decides
//! who may call the shell's commands by the *document's* origin: the app's
//! own and every custom scheme it registers (`everyday:`) count as local
//! and get the main window's full IPC. A stranger's page in the main window
//! is a frameless phishing screen with no address bar and no way back; one
//! of the vault's own blobs loaded as a document would be running with the
//! application's own authority. `protocol.rs` sends every `everyday:`
//! response with a `sandbox` content security policy for the second case;
//! this is the wall in front of both.
//!
//! What stays allowed is exactly what the interface does on its own:
//!
//! * its own origin -- `tauri://localhost`, or `http(s)://tauri.localhost`
//!   where Tauri maps custom schemes onto `http` (Windows, Android) --
//!   and, under `tauri dev`, the dev server's;
//! * `about:` -- `about:srcdoc` is how every sandboxed body frame loads,
//!   and WebKit asks about subframes too;
//! * `blob:` -- an export or an OPML file handed to the person through a
//!   `<a download>` on an object URL the interface made itself;
//! * `everyday:` -- a file attached to a note is a plain `<a>` to its blob,
//!   which opens or downloads it. That response carries the `sandbox` policy,
//!   so whatever the bytes are, nothing in them runs.
//!
//! Anything else is refused and logged. Nothing is opened in its place: a
//! navigation can come from a frame or a script rather than a click, and
//! opening the system browser on a stranger's say-so would only move the
//! problem.

use tauri::Url;

/// The guard both windows are built with. `dev` is the dev server's address
/// when this is a `tauri dev` build, and `None` otherwise.
pub fn guard(dev: Option<Url>) -> impl Fn(&Url) -> bool + Send + 'static {
    move |url| {
        let ok = allowed(url, dev.as_ref());
        if !ok {
            tracing::warn!(%url, "refused to navigate an application window away from the interface");
        }
        ok
    }
}

/// The dev server's address, when this build loads the interface from one.
pub fn dev_url<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<Url> {
    if tauri::is_dev() { app.config().build.dev_url.clone() } else { None }
}

fn allowed(url: &Url, dev: Option<&Url>) -> bool {
    match url.scheme() {
        "tauri" | "about" | "blob" | "everyday" => true,
        "http" | "https" => {
            let host = url.host_str().unwrap_or_default();
            if host.eq_ignore_ascii_case("tauri.localhost")
                || host.eq_ignore_ascii_case("everyday.localhost")
            {
                return true;
            }
            dev.is_some_and(|dev| same_origin(url, dev))
        }
        _ => false,
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("a test URL is well formed")
    }

    #[test]
    fn the_interface_and_its_own_frames_and_downloads_stay_allowed() {
        for ok in [
            "tauri://localhost/",
            "tauri://localhost/writing.html",
            "http://tauri.localhost/index.html",
            "https://tauri.localhost/",
            "about:srcdoc",
            "about:blank",
            "blob:tauri://localhost/5e1b0c4a-8d0e-4b8e-9d61-0c4b3c1f2a77",
            "everyday://localhost/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "http://everyday.localhost/0123456789abcdef",
        ] {
            assert!(allowed(&url(ok), None), "{ok} should be allowed");
        }
    }

    #[test]
    fn the_web_and_other_schemes_are_refused() {
        for bad in [
            "https://evil.example/",
            "http://localhost:5273/",
            "https://tauri.localhost.evil.example/",
            "file:///etc/passwd",
            "data:text/html,<script>alert(1)</script>",
            "javascript:alert(1)",
            "mailto:someone@example.com",
        ] {
            assert!(!allowed(&url(bad), None), "{bad} should be refused");
        }
    }

    #[test]
    fn the_dev_server_is_allowed_only_when_this_build_uses_one() {
        let dev = url("http://localhost:5273");
        assert!(allowed(&url("http://localhost:5273/index.html"), Some(&dev)));
        assert!(!allowed(&url("http://localhost:5274/"), Some(&dev)));
        assert!(!allowed(&url("https://localhost:5273/"), Some(&dev)));
        assert!(!allowed(&url("http://localhost:5273/"), None));
    }
}
