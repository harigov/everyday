//! Every name this application went by before it was Jatayu, and the
//! few places data written under those names is still read.
//!
//! The application was called Every Day, and then Karma Yoga (from November
//! 14 2026), and is Jatayu now (November 26 2026). Each rename changed every
//! name a person or another program sees -- the product, the crates, the
//! command line, the bundle id, the folders, the URL scheme, the extensions --
//! and a first launch of a renamed build carries an existing installation
//! across (`jatayu_vault::legacy`, `jatayu-app`'s own `legacy`, the
//! interface's `lib/legacy.ts`, and [`crate::files::disk::Folder::carry_over`]
//! for the files folder's own folder). What is left is data that was *written*
//! under an old name and is still on disk, on a mail server or in a file
//! somebody exported: this module names each of those once, per name the
//! application had ([`Former`]), so that the day an old name goes entirely,
//! deleting its entry is how every last reader of it is found.
//!
//! # Fan-in, not a chain
//!
//! An installation is carried from whichever former name it has straight to
//! the current one, newest first ([`FORMERS`]) -- never Every Day to Karma
//! Yoga to Jatayu. A machine that went through both renames has links left by
//! the first (`~/.config/everyday -> ~/.config/karmayoga`); a name whose path
//! is a link into the family is an alias of another, never a thing to move
//! itself. A reader of old data (a scheme in a stored body, an exported file,
//! a header from a paired copy) accepts every former name.
//!
//! # What this module does not hold, because it never changes
//!
//! The labels records are sealed under -- `everyday.<kind>.v1:<id>`, the key
//! wrapping label, the blob address key's label, and the `karmayoga.*`
//! family added while the application was Karma Yoga (backups, health, the
//! Karma book, connections, files and Feed settings, the documents cache,
//! file tokens) -- and the names the mail and documents indexes registered
//! their tokenizers under (`everyday_*`, `karmayoga_docs`). None of them is a
//! name anybody sees: each is an input to the cipher, or part of a schema
//! already written into an index, and changing one would mean decrypting and
//! resealing every record (or rebuilding the index) to change bytes no
//! person reads. `tests/pinned_strings.rs` keeps them as they are.

use std::borrow::Cow;

/// One name the application went by, and everything that name was spelled
/// into: what a first launch of a Jatayu build looks for, reads, moves or
/// retires. A field a generation never had is empty (`""` or `&[]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Former {
    /// The product's name then, for messages that tell somebody what
    /// happened to their old installation.
    pub product: &'static str,
    /// What the platform directories crate was given -- the `ProjectDirs`
    /// triple behind `~/.config/<name>` and `~/.local/share/<name>` on Linux.
    pub project_dirs: (&'static str, &'static str, &'static str),
    /// The fallback folder names used where the platform names none
    /// (`~/.<name>`, `~/.<name>-config`).
    pub home_fallbacks: (&'static str, &'static str),
    /// The bundle identifier: the keychain service, the webview's data
    /// folder and the app id the desktop portals knew.
    pub bundle_id: &'static str,
    /// The binary the desktop app was built as, which is also the Wayland
    /// app id and the name of the desktop file `make desktop-entry` wrote.
    pub app_binary: &'static str,
    /// The command line's name.
    pub cli_binary: &'static str,
    /// The GNOME Shell screen-time extension's uuid.
    pub gnome_extension_uuid: &'static str,
    /// The SQLite backend's database file, inside a vault's `store/`.
    pub sqlite_db_filename: &'static str,
    /// The schema a Postgres vault's tables went in when none was named.
    pub postgres_schema: &'static str,
    /// The prefix of the environment variables the application read.
    pub env_prefix: &'static str,
    /// The environment variable a Postgres vault's connection URL came from.
    /// Refused, not read, when set without the new one -- see
    /// `jatayu_store_postgres`.
    pub database_url_env: &'static str,
    /// The root manifest of an export archive.
    pub export_manifest: &'static str,
    /// The folder beside an exported Markdown note that holds its exact tree.
    pub export_doc_sidecar: &'static str,
    /// The properties an exported `.ics` carried the publisher's own uid and
    /// the record's zone in.
    pub ics_uid: &'static str,
    pub ics_tz: &'static str,
    /// The local host name a server's certificate named. A certificate made
    /// under an old name is kept, not regenerated: paired devices pin it.
    pub local_host_name: &'static str,
    /// The headers one copy of the app spoke to another in: its protocol
    /// version, an idempotent request's key, the client a write came from.
    pub protocol_header: &'static str,
    pub request_header: &'static str,
    pub client_header: &'static str,
    /// How a pairing link began.
    pub pairing_link: &'static str,
    /// The Matrix device name the assistant's bot signed in under.
    pub matrix_device_name: &'static str,
    /// The media scheme, as a stored body spells an address in it, and the
    /// same scheme as Windows and Android spell it (Tauri maps a custom
    /// scheme to an `http://<scheme>.localhost` origin there).
    pub scheme: &'static str,
    pub scheme_origin: &'static str,
    /// The prefix the interface's stored preferences had (`<prefix>.theme`,
    /// `<prefix>:assistant-open`). Read by `ui/src/lib/legacy.ts`, which
    /// keeps its own copy; listed here so the two can be compared.
    pub storage_prefix: &'static str,
    /// The file, in the settings folder, that says this generation's webview
    /// folder has been carried across already.
    pub webview_marker: &'static str,
    /// The folder of the application's own files inside the person's files
    /// folder (trash, uploads in progress, the folder's marker). Only Karma
    /// Yoga had a files folder.
    pub files_internal_dir: &'static str,
    /// The name of the files folder a new setup made in the home folder
    /// (`~/Karma Yoga`). A setup's own path is stored and kept, so this is
    /// only for recognising that default where it is recorded -- a backup's
    /// files folder, restored to where it was. Only Karma Yoga had one.
    pub default_files_folder: &'static str,
    /// A backup repository's marker file and format name. Only Karma Yoga
    /// made backups.
    pub backup_marker: &'static str,
    pub backup_format: &'static str,
    /// The name readings typed in by hand were stored under as their source,
    /// in health data. Only Karma Yoga had health data.
    pub manual_health_source: &'static str,
    /// Prefixes of the scratch files and folders the application made in the
    /// system's temporary or cache folders, so ones left behind under an old
    /// name are still swept up (some hold decrypted copies).
    pub scratch_prefixes: &'static [&'static str],
    /// App ids the desktop app was seen under by screen time, each with the
    /// id the same app has now on the same platform, so the hours spent in it
    /// before the rename count as the same app's: Wayland's app id (the
    /// binary), an X11 class, a Flatpak-style id, and macOS's bundle id, all
    /// lower-cased as screen time keys them.
    pub screen_app_ids: &'static [(&'static str, &'static str)],
}

/// Karma Yoga, November 14 to 26 2026.
pub const KARMA_YOGA: Former = Former {
    product: "Karma Yoga",
    project_dirs: ("app", "Karma Yoga", "KarmaYoga"),
    home_fallbacks: (".karmayoga", ".karmayoga-config"),
    bundle_id: "app.karmayogi.KarmaYoga",
    app_binary: "karmayoga-app",
    cli_binary: "karmayoga",
    gnome_extension_uuid: "screentime@karmayogi.app",
    sqlite_db_filename: "karmayoga.db",
    postgres_schema: "karmayoga",
    env_prefix: "KARMAYOGA_",
    database_url_env: "KARMAYOGA_DATABASE_URL",
    export_manifest: "karmayoga.json",
    export_doc_sidecar: ".karmayoga/",
    ics_uid: "X-KARMAYOGA-UID",
    ics_tz: "X-KARMAYOGA-TZ",
    local_host_name: "karmayoga.local",
    protocol_header: "x-karmayoga-protocol",
    request_header: "x-karmayoga-request",
    client_header: "x-karmayoga-client",
    pairing_link: "karmayoga://pair?",
    matrix_device_name: "Karma Yoga assistant",
    scheme: "karmayoga://",
    scheme_origin: "http://karmayoga.localhost",
    storage_prefix: "karmayoga",
    webview_marker: "webview-carried-over-from-karmayoga",
    files_internal_dir: ".karmayoga",
    default_files_folder: "Karma Yoga",
    backup_marker: "karmayoga-backup.json",
    backup_format: "karmayoga-backup",
    manual_health_source: "Karma Yoga",
    scratch_prefixes: &[
        "karmayoga-browser-",
        "karmayoga-open-",
        "karmayoga-staging-",
        "karmayoga-docs-",
        "karmayoga-transcripts",
        "karmayoga-downloads",
    ],
    screen_app_ids: &[
        ("karmayoga-app", "jatayu-app"),
        ("karmayoga", "jatayu"),
        ("app.karmayogi.karmayoga", "app.jatayu.jatayu"),
    ],
};

/// Every Day, until November 14 2026.
pub const EVERY_DAY: Former = Former {
    product: "Every Day",
    project_dirs: ("app", "Every Day", "EveryDay"),
    home_fallbacks: (".everyday", ".everyday-config"),
    bundle_id: "app.everyday.journal",
    app_binary: "everyday-app",
    cli_binary: "everyday",
    gnome_extension_uuid: "screentime@everyday.journal",
    sqlite_db_filename: "everyday.db",
    postgres_schema: "everyday",
    env_prefix: "EVERYDAY_",
    database_url_env: "EVERYDAY_DATABASE_URL",
    export_manifest: "everyday.json",
    export_doc_sidecar: ".everyday/",
    ics_uid: "X-EVERYDAY-UID",
    ics_tz: "X-EVERYDAY-TZ",
    local_host_name: "everyday.local",
    protocol_header: "x-everyday-protocol",
    request_header: "x-everyday-request",
    client_header: "x-everyday-client",
    pairing_link: "everyday://pair?",
    matrix_device_name: "Every Day assistant",
    scheme: "everyday://",
    scheme_origin: "http://everyday.localhost",
    storage_prefix: "everyday",
    // The name the first rename's marker already has on disk.
    webview_marker: "webview-carried-over",
    files_internal_dir: "",
    default_files_folder: "",
    backup_marker: "",
    backup_format: "",
    manual_health_source: "",
    scratch_prefixes: &["everyday-browser-"],
    screen_app_ids: &[
        ("everyday-app", "jatayu-app"),
        ("everyday", "jatayu"),
        ("org.everyday.app", "jatayu-app"),
        ("app.everyday.journal", "app.jatayu.jatayu"),
    ],
};

/// Every former name, newest first: the order an installation is looked for
/// in, and the order a value found under two of them is preferred in.
pub const FORMERS: [&Former; 2] = [&KARMA_YOGA, &EVERY_DAY];

/// Environment variables set under an old prefix, which nothing reads any
/// more: for a warning at startup, so a setting that silently stopped
/// working is at least said out loud. Each comes with the name it has now.
pub fn old_environment() -> Vec<(String, String)> {
    old_names(std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()))
}

/// [`old_environment`] over the variable names given, so the mapping can be
/// tested without changing the process's environment.
fn old_names(names: impl IntoIterator<Item = String>) -> Vec<(String, String)> {
    names
        .into_iter()
        .filter_map(|name| {
            let rest = FORMERS
                .iter()
                .filter(|f| !f.env_prefix.is_empty())
                .find_map(|f| name.strip_prefix(f.env_prefix))?;
            let now = format!("JATAYU_{rest}");
            Some((name, now))
        })
        .collect()
}

/// `id`, lower-cased, or the id the same app has now if it is one a former
/// name was seen under ([`Former::screen_app_ids`]).
pub fn screen_app_id(id: &str) -> String {
    let id = id.trim().to_lowercase();
    FORMERS
        .iter()
        .flat_map(|f| f.screen_app_ids.iter())
        .find(|(old, _)| *old == id)
        .map(|(_, new)| (*new).to_string())
        .unwrap_or(id)
}

/// `html` with every address in a former media scheme moved to the current
/// one.
///
/// Mail bodies, drafts and Feed stories were stored with their pictures
/// already pointed at this app's own scheme -- `everyday://mail/img/…`,
/// `karmayoga://news/img/…`, `karmayoga://localhost/<blob>` for a picture
/// pasted into a draft -- because the sanitiser rewrites them once, at sync
/// time. Rewriting gigabytes of sealed bodies to change a scheme name would
/// put every message at risk for no gain, so they are moved here instead, as
/// they are read. The prefixes are distinct from anything the rest of a body
/// can mean, and a body that holds none comes back borrowed.
pub fn current_scheme(html: &str) -> Cow<'_, str> {
    let old = |f: &&Former| html.contains(f.scheme) || html.contains(f.scheme_origin);
    if !FORMERS.iter().any(old) {
        return Cow::Borrowed(html);
    }
    let mut moved = html.to_string();
    for former in FORMERS {
        moved = moved
            .replace(former.scheme, "jatayu://")
            .replace(former.scheme_origin, "http://jatayu.localhost");
    }
    Cow::Owned(moved)
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
        let html = r#"<p>Hi <img src="jatayu://mail/img/m/t"></p>"#;
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
                r#"<img src="jatayu://mail/img/m%2D1/tok">"#,
                r#"<img src="jatayu://localhost/abc" data-blob="abc">"#,
                r#"<img src="http://jatayu.localhost/news/img/s/t">"#,
            )
        );
    }

    #[test]
    fn the_app_under_an_old_id_is_the_same_screen_time_item() {
        use crate::screentime::{SpanKind, item_key};
        let whole = |_: &str| false;
        let key = |app: &str| item_key(SpanKind::App, app, "", &whole);
        assert_eq!(key("everyday-app"), key("jatayu-app"));
        assert_eq!(key("everyday-app"), "app:jatayu-app");
        assert_eq!(key(" Everyday "), "app:jatayu");
        assert_eq!(key("app.everyday.journal"), key("app.jatayu.Jatayu"));
        assert_eq!(key("firefox"), "app:firefox");
    }

    /// Mail, drafts and Feed stories synced while the application was Karma
    /// Yoga hold `karmayoga://` addresses (and the Windows and Android
    /// spelling, `http://karmayoga.localhost`); they move straight to
    /// Jatayu's, never by way of another former name, and a body with both
    /// generations' addresses comes back with neither.
    #[test]
    fn karma_yogas_addresses_move_to_the_current_scheme_too() {
        let html = concat!(
            r#"<img src="karmayoga://mail/img/m/tok">"#,
            r#"<img src="karmayoga://localhost/abc" data-blob="abc">"#,
            r#"<img src="http://karmayoga.localhost/news/img/s/t">"#,
            r#"<img src="everyday://news/img/s/u">"#,
        );
        let moved = current_scheme(html);
        assert_eq!(
            moved,
            concat!(
                r#"<img src="jatayu://mail/img/m/tok">"#,
                r#"<img src="jatayu://localhost/abc" data-blob="abc">"#,
                r#"<img src="http://jatayu.localhost/news/img/s/t">"#,
                r#"<img src="jatayu://news/img/s/u">"#,
            )
        );
        for former in FORMERS {
            assert!(!moved.contains(former.scheme), "{}", former.product);
            assert!(!moved.contains(former.scheme_origin), "{}", former.product);
        }
    }

    /// Every id either former name was seen under by screen time is the
    /// same item as the app's id now, whichever platform it came from.
    #[test]
    fn both_generations_screen_ids_are_the_current_app() {
        use crate::screentime::{SpanKind, item_key};
        let whole = |_: &str| false;
        let key = |app: &str| item_key(SpanKind::App, app, "", &whole);
        assert_eq!(key("karmayoga-app"), "app:jatayu-app");
        assert_eq!(key("KarmaYoga"), "app:jatayu");
        assert_eq!(key("app.karmayogi.KarmaYoga"), key("app.jatayu.Jatayu"));
        assert_eq!(key("org.everyday.app"), "app:jatayu-app");
        for former in FORMERS {
            for (old, now) in former.screen_app_ids {
                assert_eq!(screen_app_id(old), *now, "{}", former.product);
                assert_eq!(screen_app_id(&old.to_uppercase()), *now, "{}", former.product);
                // The current id is never itself renamed: no chain.
                assert_eq!(screen_app_id(now), *now);
                assert!(
                    FORMERS.iter().flat_map(|f| f.screen_app_ids).all(|(o, _)| o != now),
                    "{now} is both a former id and a current one"
                );
            }
        }
    }

    /// A variable set under either former prefix is named with the one it
    /// has now; anything else, `JATAYU_*` included, is not mentioned.
    #[test]
    fn old_environment_names_both_prefixes_and_nothing_else() {
        let names = [
            "KARMAYOGA_VAULT",
            "EVERYDAY_PASSWORD",
            "KARMAYOGA_DATABASE_URL",
            "JATAYU_VAULT",
            "HOME",
            "XKARMAYOGA_VAULT",
            "karmayoga_vault",
        ]
        .map(String::from);
        assert_eq!(
            old_names(names),
            vec![
                ("KARMAYOGA_VAULT".to_string(), "JATAYU_VAULT".to_string()),
                ("EVERYDAY_PASSWORD".to_string(), "JATAYU_PASSWORD".to_string()),
                ("KARMAYOGA_DATABASE_URL".to_string(), "JATAYU_DATABASE_URL".to_string()),
            ]
        );
        for former in FORMERS {
            assert!(former.database_url_env.starts_with(former.env_prefix), "{}", former.product);
        }
    }

    /// The names every reader fans in from are listed newest first, and a
    /// field one generation never had is empty rather than a guess, so that
    /// the readers' "skip empty" rule is what keeps it out.
    #[test]
    fn formers_are_newest_first_and_distinct() {
        assert_eq!(FORMERS[0].product, "Karma Yoga");
        assert_eq!(FORMERS[1].product, "Every Day");
        assert_ne!(KARMA_YOGA.files_internal_dir, "");
        assert_eq!(EVERY_DAY.files_internal_dir, "");
        assert_eq!(EVERY_DAY.manual_health_source, "");
        let pairs = |f: &Former| {
            [
                f.bundle_id,
                f.app_binary,
                f.sqlite_db_filename,
                f.export_manifest,
                f.export_doc_sidecar,
                f.ics_uid,
                f.scheme,
                f.webview_marker,
            ]
        };
        for (a, b) in pairs(&KARMA_YOGA).iter().zip(pairs(&EVERY_DAY)) {
            assert_ne!(*a, b, "both former names spell this the same");
            assert!(!a.contains("jatayu") && !b.contains("jatayu"));
        }
    }

    #[test]
    fn moving_in_place_leaves_a_current_body_alone() {
        let mut html = "<p>plain</p>".to_string();
        move_to_current_scheme(&mut html);
        assert_eq!(html, "<p>plain</p>");
        let mut old = r#"<img src="everyday://mail/part/m/1">"#.to_string();
        move_to_current_scheme(&mut old);
        assert_eq!(old, r#"<img src="jatayu://mail/part/m/1">"#);
    }
}
