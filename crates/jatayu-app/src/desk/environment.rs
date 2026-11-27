//! What a program started from here must not inherit from this process.
//!
//! The assistant launches applications, runs `wpctl`, locks the screen: each
//! one a child of this process, and each one handed its environment. Most of
//! it is harmless, and some of it breaks the child in ways that are hard to
//! trace to their cause:
//!
//! - **What GIO adds to a program it launched.** `GIO_LAUNCHED_DESKTOP_FILE`
//!   (and `_PID`) say which desktop file started *this* process, and the
//!   shell reads them to tell its own icon from the one a child would
//!   otherwise share. GIO writes its own into the child it launches, so these
//!   are overwritten there -- but a plain `Command` child, or a launcher that
//!   does not, would carry ours as if it were the child's.
//! - **What is ours.** `JATAYU_*` is this program's own switches and
//!   paths, and `WEBKIT_*` is how its web view was told to behave
//!   (`WEBKIT_DISABLE_*` workarounds for a driver that misbehaves with
//!   *this* window). A browser launched with them inherits settings made for
//!   somebody else's window.
//! - **What an AppImage injects.** An AppImage runs the program with its own
//!   copies of GTK, GStreamer and the C library's neighbours first on every
//!   search path: `LD_LIBRARY_PATH`, `GDK_PIXBUF_MODULE_FILE`, `GIO_MODULE_DIR`,
//!   `GST_PLUGIN_*`, `GTK_PATH`, and `PATH` and `XDG_DATA_DIRS` with its own
//!   directories in front. An application launched from inside one picks up
//!   those libraries instead of the system's and fails to start, or starts
//!   and shows no icons. Such a variable is not removed outright when it also
//!   holds something that is the person's: the AppImage's directories are
//!   taken out of it and what is left is kept. `APPIMAGE` and `APPDIR`
//!   themselves would tell a launched AppImage that it is us.
//!
//! [`changes`] is what to do about it, from the environment it is given and
//! nothing else, so it is a function a test can feed any environment to.

use std::collections::BTreeMap;

/// One change to make to the environment of a program about to be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Change {
    Unset(String),
    Set(String, String),
}

/// Variables of this program's own, by prefix.
const OURS: &[&str] = &["JATAYU_", "WEBKIT_"];

/// Which desktop file started this process, as GIO sets it.
const LAUNCHED: &[&str] = &["GIO_LAUNCHED_DESKTOP_FILE", "GIO_LAUNCHED_DESKTOP_FILE_PID"];

/// What the AppImage runtime itself sets to say where it is.
const RUNTIME: &[&str] = &["APPIMAGE", "APPDIR", "ARGV0", "OWD"];

/// The search paths an AppImage's launcher puts its own directories on. Each
/// holds a list, or a single path, and is cleaned of what is under the
/// AppImage and kept if anything else is left.
const SEARCH_PATHS: &[&str] = &[
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "PATH",
    "XDG_DATA_DIRS",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "GIO_MODULE_DIR",
    "GIO_EXTRA_MODULES",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_REGISTRY",
    "GST_REGISTRY_1_0",
    "GTK_PATH",
    "GTK_EXE_PREFIX",
    "GTK_DATA_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GI_TYPELIB_PATH",
    "QT_PLUGIN_PATH",
    "QT_QPA_PLATFORM_PLUGIN_PATH",
    "PYTHONHOME",
    "PYTHONPATH",
    "PERLLIB",
];

/// Variables an AppImage's GTK launcher *forces* to a value, which say
/// nothing about what the program started from it wants. It sets
/// `GDK_BACKEND=x11` because its bundled GTK crashes under Wayland; a
/// launched application should choose for itself.
const FORCED: &[(&str, &str)] = &[("GDK_BACKEND", "x11")];

/// What to change in `env` for a program started from here.
///
/// In a fixed order -- by name -- so that what is done does not depend on how
/// the process happened to be handed its environment.
pub(super) fn changes(env: impl IntoIterator<Item = (String, String)>) -> Vec<Change> {
    let env: BTreeMap<String, String> = env.into_iter().collect();
    let appimage = env.get("APPIMAGE").is_some_and(|v| !v.is_empty());
    let appdir = env.get("APPDIR").map(String::as_str).filter(|v| !v.is_empty());

    let mut out = Vec::new();
    for (name, value) in &env {
        let ours = OURS.iter().any(|prefix| name.starts_with(prefix));
        let launched = LAUNCHED.contains(&name.as_str());
        let runtime = appimage && RUNTIME.contains(&name.as_str());
        let forced = appimage && FORCED.iter().any(|(n, v)| name == n && value == v);
        if ours || launched || runtime || forced {
            out.push(Change::Unset(name.clone()));
            continue;
        }
        if !appimage || !SEARCH_PATHS.contains(&name.as_str()) {
            continue;
        }
        match appdir {
            // Everything that is not the AppImage's stays.
            Some(dir) => match without_directory(value, dir) {
                Cleaned::Unchanged => {}
                // A `PATH` of nothing is not the same as no `PATH` to a
                // shell script: leave it be rather than empty it.
                Cleaned::Nothing if name == "PATH" => {}
                Cleaned::Nothing => out.push(Change::Unset(name.clone())),
                Cleaned::Left(rest) => out.push(Change::Set(name.clone(), rest)),
            },
            // An AppImage that did not say where it is: all of it goes.
            None if name != "PATH" => out.push(Change::Unset(name.clone())),
            None => {}
        }
    }
    out
}

/// What is left of a list of paths once those under a directory are gone.
#[derive(Debug, PartialEq, Eq)]
enum Cleaned {
    /// None of it was under the directory.
    Unchanged,
    /// All of it was.
    Nothing,
    Left(String),
}

/// `value`, a colon-separated list of paths, without those that are
/// `directory` or inside it.
fn without_directory(value: &str, directory: &str) -> Cleaned {
    let directory = directory.trim_end_matches('/');
    let inside = |entry: &str| {
        let entry = entry.trim_end_matches('/');
        entry == directory
            || entry.strip_prefix(directory).is_some_and(|rest| rest.starts_with('/'))
    };
    let entries: Vec<&str> = value.split(':').collect();
    let kept: Vec<&str> = entries.iter().copied().filter(|entry| !inside(entry)).collect();
    if kept.len() == entries.len() {
        Cleaned::Unchanged
    } else if kept.iter().all(|entry| entry.is_empty()) {
        Cleaned::Nothing
    } else {
        Cleaned::Left(kept.join(":"))
    }
}

/// This process's own environment, as [`changes`] takes it. A variable that
/// is not text is not one anything here is named by, so it is passed over.
#[cfg(target_os = "linux")]
pub(super) fn current() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Make a command start without what [`changes`] takes out of this process's
/// environment.
#[cfg(target_os = "linux")]
pub(super) fn clean(command: &mut std::process::Command) {
    for change in changes(current()) {
        match change {
            Change::Unset(name) => {
                command.env_remove(name);
            }
            Change::Set(name, value) => {
                command.env(name, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn unset(name: &str) -> Change {
        Change::Unset(name.to_string())
    }

    fn set(name: &str, value: &str) -> Change {
        Change::Set(name.to_string(), value.to_string())
    }

    #[test]
    fn what_is_ours_is_not_passed_on() {
        let changes = changes(env(&[
            ("HOME", "/home/sam"),
            ("JATAYU_LOG", "debug"),
            ("JATAYU_VAULT", "/v"),
            ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
            ("GIO_LAUNCHED_DESKTOP_FILE", "/usr/share/applications/jatayu.desktop"),
            ("GIO_LAUNCHED_DESKTOP_FILE_PID", "4242"),
            ("LD_LIBRARY_PATH", "/opt/lib"),
        ]));
        assert_eq!(
            changes,
            [
                unset("GIO_LAUNCHED_DESKTOP_FILE"),
                unset("GIO_LAUNCHED_DESKTOP_FILE_PID"),
                unset("JATAYU_LOG"),
                unset("JATAYU_VAULT"),
                unset("WEBKIT_DISABLE_COMPOSITING_MODE"),
            ],
            "outside an AppImage LD_LIBRARY_PATH is the person's own"
        );
    }

    #[test]
    fn nothing_is_asked_of_an_environment_with_nothing_to_remove() {
        assert_eq!(changes(env(&[("HOME", "/home/sam"), ("PATH", "/usr/bin")])), []);
    }

    #[test]
    fn an_appimages_directories_come_out_of_the_search_paths() {
        let appdir = "/tmp/.mount_karmaXYZ";
        let changes = changes(env(&[
            ("APPIMAGE", "/home/sam/Karma.AppImage"),
            ("APPDIR", appdir),
            ("ARGV0", "Karma.AppImage"),
            ("OWD", "/home/sam"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_karmaXYZ/usr/lib:/tmp/.mount_karmaXYZ/usr/lib64"),
            ("GDK_PIXBUF_MODULE_FILE", "/tmp/.mount_karmaXYZ/usr/lib/loaders.cache"),
            ("GST_PLUGIN_SYSTEM_PATH_1_0", "/tmp/.mount_karmaXYZ/usr/lib/gstreamer-1.0"),
            ("GIO_MODULE_DIR", "/tmp/.mount_karmaXYZ/usr/lib/gio/modules"),
            ("XDG_DATA_DIRS", "/tmp/.mount_karmaXYZ/usr/share:/usr/local/share:/usr/share"),
            ("PATH", "/tmp/.mount_karmaXYZ/usr/bin:/usr/bin:/bin"),
            ("GDK_BACKEND", "x11"),
            ("HOME", "/home/sam"),
        ]));
        assert_eq!(
            changes,
            [
                unset("APPDIR"),
                unset("APPIMAGE"),
                unset("ARGV0"),
                unset("GDK_BACKEND"),
                unset("GDK_PIXBUF_MODULE_FILE"),
                unset("GIO_MODULE_DIR"),
                unset("GST_PLUGIN_SYSTEM_PATH_1_0"),
                unset("LD_LIBRARY_PATH"),
                unset("OWD"),
                set("PATH", "/usr/bin:/bin"),
                set("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            ]
        );
    }

    #[test]
    fn what_the_person_set_beside_an_appimages_directories_is_kept() {
        let changes = changes(env(&[
            ("APPIMAGE", "/a.AppImage"),
            ("APPDIR", "/tmp/.mount_a"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_a/usr/lib:/opt/cuda/lib64"),
        ]));
        assert!(changes.contains(&set("LD_LIBRARY_PATH", "/opt/cuda/lib64")), "{changes:?}");
    }

    /// `/tmp/.mount_a` is the AppImage's; `/tmp/.mount_ab` is somebody
    /// else's directory that starts the same.
    #[test]
    fn a_directory_is_not_taken_for_one_that_starts_with_its_name() {
        assert_eq!(without_directory("/tmp/.mount_ab/lib", "/tmp/.mount_a"), Cleaned::Unchanged);
        assert_eq!(without_directory("/tmp/.mount_a/lib", "/tmp/.mount_a/"), Cleaned::Nothing);
        assert_eq!(without_directory("/tmp/.mount_a", "/tmp/.mount_a"), Cleaned::Nothing);
        assert_eq!(
            without_directory("/x:/tmp/.mount_a/lib:/y", "/tmp/.mount_a"),
            Cleaned::Left("/x:/y".to_string())
        );
    }

    #[test]
    fn an_appimage_that_does_not_say_where_it_is_loses_every_search_path() {
        let changes = changes(env(&[
            ("APPIMAGE", "/a.AppImage"),
            ("LD_LIBRARY_PATH", "/somewhere"),
            ("PATH", "/usr/bin"),
        ]));
        assert_eq!(changes, [unset("APPIMAGE"), unset("LD_LIBRARY_PATH")]);
    }

    /// Without `APPIMAGE` nothing in the AppImage's list is touched: the
    /// same variables are somebody's own settings on an ordinary install.
    #[test]
    fn outside_an_appimage_the_search_paths_are_left_alone() {
        let changes = changes(env(&[
            ("APPDIR", "/tmp/.mount_a"),
            ("GDK_BACKEND", "x11"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_a/lib"),
        ]));
        assert_eq!(changes, []);
    }
}
