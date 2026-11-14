//! Every name this application went by before it was Karma Yoga, and the
//! few places data written under those names is still read.
//!
//! The application was called Every Day. The rename changed every name a
//! person or another program sees -- the product, the crates, the command
//! line, the bundle id, the folders, the URL scheme, the extensions -- and a
//! first launch of a renamed build carries an existing installation across
//! (`karmayoga_vault::legacy`, `karmayoga-app`'s own `legacy`, and the
//! interface's `lib/legacy.ts`). What is left is data that was *written*
//! under an old name and is still on disk, on a mail server or in a file
//! somebody exported: this module names each of those once, so that the day
//! the old name goes entirely, deleting this module is how every last reader
//! of it is found -- the compiler lists them.
//!
//! # What this module does not hold, because it never changes
//!
//! The labels records are sealed under -- `everyday.<kind>.v1:<id>`, the key
//! wrapping label, the blob address key's label -- and the names the mail
//! index registered its tokenizers under. None of them is a name anybody
//! sees: each is an input to the cipher, or part of a schema already written
//! into an index, and changing one would mean decrypting and resealing every
//! record (or rebuilding the index) to change bytes no person reads.
//! `tests/pinned_strings.rs` keeps them as they are.

use std::borrow::Cow;

/// The product's old name, for messages that tell somebody what happened to
/// their old installation.
pub const PRODUCT: &str = "Every Day";

/// What the platform directories crate was given for the old name -- the
/// `ProjectDirs` triple behind `~/.config/everyday` and
/// `~/.local/share/everyday` on Linux.
pub const PROJECT_DIRS: (&str, &str, &str) = ("app", "Every Day", "EveryDay");

/// The old bundle identifier: the keychain service, the webview's data
/// folder and the app id the desktop portals knew.
pub const BUNDLE_ID: &str = "app.everyday.journal";

/// The binary the desktop app was built as, which is also the Wayland app id
/// and the name of the desktop file `make desktop-entry` wrote.
pub const APP_BINARY: &str = "everyday-app";

/// The command line's old name.
pub const CLI_BINARY: &str = "everyday";

/// The GNOME Shell screen-time extension's old uuid.
pub const GNOME_EXTENSION_UUID: &str = "screentime@everyday.journal";

/// The SQLite backend's database file, inside a vault's `store/`.
pub const SQLITE_DB_FILENAME: &str = "everyday.db";

/// The schema a Postgres vault's tables went in when none was named.
pub const POSTGRES_SCHEMA: &str = "everyday";

/// The environment variable a Postgres vault's connection URL could come
/// from. Refused, not read, when set without the new one -- see
/// `karmayoga_store_postgres`.
pub const DATABASE_URL_ENV: &str = "EVERYDAY_DATABASE_URL";

/// Environment variables set under the old prefix, which nothing reads any
/// more: for a warning at startup, so a setting that silently stopped
/// working is at least said out loud. Each comes with the name it has now.
pub fn old_environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, _)| {
            let name = name.into_string().ok()?;
            let rest = name.strip_prefix("EVERYDAY_")?;
            Some((name.clone(), format!("KARMAYOGA_{rest}")))
        })
        .collect()
}

/// The root manifest of an export archive.
pub const EXPORT_MANIFEST: &str = "everyday.json";

/// The folder beside an exported Markdown note that holds its exact tree.
pub const EXPORT_DOC_SIDECAR: &str = ".everyday/";

/// The properties an exported `.ics` carried the publisher's own uid and the
/// record's zone in.
pub const ICS_UID: &str = "X-EVERYDAY-UID";
pub const ICS_TZ: &str = "X-EVERYDAY-TZ";

/// The local host name a server's certificate named. A certificate made
/// before the rename is kept, not regenerated: paired devices pin it.
pub const LOCAL_HOST_NAME: &str = "everyday.local";

/// The header a copy of the app says its protocol version in, to another
/// copy: accepted from, and sent to, a copy from before the rename, so a
/// mismatched pair is told which side to update.
pub const PROTOCOL_HEADER: &str = "x-everyday-protocol";

/// How the assistant's throwaway browser profiles were named, so one a
/// crash left behind before the rename is still swept up.
pub const BROWSER_PROFILE_PREFIX: &str = "everyday-browser-";

/// How a pairing link from a copy before the rename begins.
pub const PAIRING_LINK: &str = "everyday://pair?";

/// The Matrix device name the assistant's bot signed in under.
pub const MATRIX_DEVICE_NAME: &str = "Every Day assistant";

/// App ids the desktop app was seen under by screen time, each with the id
/// the same app has now on the same platform, so the hours spent in it
/// before the rename count as the same app's: Wayland's app id (the
/// binary), an X11 class, a Flatpak-style id, and macOS's bundle id, all
/// lower-cased as screen time keys them.
pub const SCREEN_APP_IDS: [(&str, &str); 4] = [
    ("everyday-app", "karmayoga-app"),
    ("everyday", "karmayoga"),
    ("org.everyday.app", "karmayoga-app"),
    ("app.everyday.journal", "app.karmayogi.karmayoga"),
];

/// `id`, lower-cased, or the id the same app has now if it is one of
/// [`SCREEN_APP_IDS`].
pub fn screen_app_id(id: &str) -> String {
    let id = id.trim().to_lowercase();
    SCREEN_APP_IDS
        .iter()
        .find(|(old, _)| *old == id)
        .map(|(_, new)| (*new).to_string())
        .unwrap_or(id)
}

/// The old media scheme, as a stored body spells an address in it.
const SCHEME: &str = "everyday://";

/// The same scheme as Windows and Android spell it: Tauri maps a custom
/// scheme to an `http://<scheme>.localhost` origin there.
const SCHEME_ORIGIN: &str = "http://everyday.localhost";

/// `html` with every address in the old media scheme moved to the current
/// one.
///
/// Mail bodies, drafts and Feed stories were stored with their pictures
/// already pointed at this app's own scheme -- `everyday://mail/img/…`,
/// `everyday://news/img/…`, `everyday://localhost/<blob>` for a picture
/// pasted into a draft -- because the sanitiser rewrites them once, at sync
/// time. Rewriting gigabytes of sealed bodies to change a scheme name would
/// put every message at risk for no gain, so they are moved here instead, as
/// they are read. The two prefixes are distinct from anything the rest of a
/// body can mean, and a body that holds neither comes back borrowed.
pub fn current_scheme(html: &str) -> Cow<'_, str> {
    if !html.contains(SCHEME) && !html.contains(SCHEME_ORIGIN) {
        return Cow::Borrowed(html);
    }
    Cow::Owned(
        html.replace(SCHEME, "karmayoga://").replace(SCHEME_ORIGIN, "http://karmayoga.localhost"),
    )
}

/// [`current_scheme`], in place.
pub fn move_to_current_scheme(html: &mut String) {
    if let Cow::Owned(moved) = current_scheme(html) {
        *html = moved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_with_no_old_address_is_borrowed() {
        let html = r#"<p>Hi <img src="karmayoga://mail/img/m/t"></p>"#;
        assert!(matches!(current_scheme(html), Cow::Borrowed(_)));
    }

    #[test]
    fn old_addresses_move_to_the_current_scheme() {
        let html = concat!(
            r#"<img src="everyday://mail/img/m%2D1/tok">"#,
            r#"<img src="everyday://localhost/abc" data-blob="abc">"#,
            r#"<img src="http://everyday.localhost/news/img/s/t">"#,
        );
        assert_eq!(
            current_scheme(html),
            concat!(
                r#"<img src="karmayoga://mail/img/m%2D1/tok">"#,
                r#"<img src="karmayoga://localhost/abc" data-blob="abc">"#,
                r#"<img src="http://karmayoga.localhost/news/img/s/t">"#,
            )
        );
    }

    #[test]
    fn the_app_under_an_old_id_is_the_same_screen_time_item() {
        use crate::screentime::{SpanKind, item_key};
        let whole = |_: &str| false;
        let key = |app: &str| item_key(SpanKind::App, app, "", &whole);
        assert_eq!(key("everyday-app"), key("karmayoga-app"));
        assert_eq!(key("everyday-app"), "app:karmayoga-app");
        assert_eq!(key(" Everyday "), "app:karmayoga");
        assert_eq!(key("app.everyday.journal"), key("app.karmayogi.KarmaYoga"));
        assert_eq!(key("firefox"), "app:firefox");
    }

    #[test]
    fn moving_in_place_leaves_a_current_body_alone() {
        let mut html = "<p>plain</p>".to_string();
        move_to_current_scheme(&mut html);
        assert_eq!(html, "<p>plain</p>");
        let mut old = r#"<img src="everyday://mail/part/m/1">"#.to_string();
        move_to_current_scheme(&mut old);
        assert_eq!(old, r#"<img src="karmayoga://mail/part/m/1">"#);
    }
}
