//! What the desktop shell carries across from an installation of Every Day,
//! before it builds a window.
//!
//! The settings and data folders are `karmayoga_vault::legacy`'s, moved the
//! first time anything asks where they are; [`carry_over`] asks first, so
//! the move happens -- and is logged -- before anything else starts. What
//! is the shell's alone is the webview's own folder: the interface's
//! preferences (`localStorage`) and its caches live in a folder named after
//! the bundle id, which changed with the rename, so without this a renamed
//! build would open with every preference back at its default. The
//! interface then renames the keys themselves (`ui/src/lib/legacy.ts`).
//!
//! The screen-time GNOME extension under its old uuid is retired by
//! `screentime::gnome::retire_old`, and old keychain entries are carried
//! across where they are read (`karmayoga_vault::autounlock`, `remotes`).

use std::path::{Path, PathBuf};

/// Carry across whatever has to move before a window exists.
pub fn carry_over() {
    if karmayoga_vault::under_test() {
        return;
    }
    // Resolving a folder is what moves them; see `karmayoga_vault::legacy`.
    let config = karmayoga_vault::config_dir();
    let marker = config.join(WEBVIEW_CARRIED);
    if marker.exists() {
        return;
    }
    let mut done = true;
    for (old, new) in webview_folders() {
        done &= carry_webview_folder(&old, &new, &karmayoga_vault::legacy::in_use);
    }
    if done {
        let written = karmayoga_core::fsutil::create_private_dir(&config)
            .and_then(|()| karmayoga_core::fsutil::write_atomic(&marker, b"", "carried"));
        if let Err(e) = written {
            tracing::warn!("carry-over: could not record that the webview's folder moved: {e}");
        }
    }
}

/// Written in the settings folder once the webview's folder has been carried
/// across, or there was none to carry. Until it is there, every launch tries
/// again; after, an old build started again and making a fresh folder under
/// the old bundle id is left alone.
const WEBVIEW_CARRIED: &str = "webview-carried-over";

/// The webview's folders under the old bundle id and the new one, on this
/// platform. Tauri keeps them under the app's local data directory; macOS's
/// WebKit keeps its own store under the bundle id as well.
fn webview_folders() -> Vec<(PathBuf, PathBuf)> {
    let old = karmayoga_core::legacy::BUNDLE_ID;
    let new = BUNDLE_ID;
    let mut roots = Vec::new();
    if let Some(local) = local_data_dir() {
        roots.push(local);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Library").join("WebKit"));
    }
    roots.into_iter().map(|root| (root.join(old), root.join(new))).collect()
}

/// This build's bundle identifier, as `tauri.conf.json` gives it.
const BUNDLE_ID: &str = "app.karmayogi.KarmaYoga";

/// The directory Tauri's `app_local_data_dir` is under, worked out before
/// there is an app to ask: `$XDG_DATA_HOME` (or `~/.local/share`) on Linux,
/// `~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows.
fn local_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(
            || std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")),
        )
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// Move one webview folder to its new name, in one rename, once nothing is
/// using it -- `busy` says what is, if anything. `true` when nothing is left
/// to carry.
///
/// A new folder already holding something was made by a launch whose
/// carry-over was put off (the old build was still running), and holds only
/// what that launch changed: it is moved aside, beside the new name, and the
/// old folder -- every preference before the rename -- takes its place.
/// Nothing is deleted either way. Nothing is left at the old path: no other
/// program reads a webview's folder.
fn carry_webview_folder(old: &Path, new: &Path, busy: &dyn Fn(&[&Path]) -> Option<String>) -> bool {
    if !old.is_dir() {
        return true;
    }
    if let Some(why) = busy(&[old]) {
        tracing::warn!(
            "carry-over: left the webview's folder {} where it is for now: {why}",
            old.display()
        );
        return false;
    }
    let moved = (|| {
        if has_content(new) {
            let aside = set_aside_name(new);
            std::fs::rename(new, &aside)?;
            tracing::info!(
                "carry-over: kept a put-off launch's webview folder at {}",
                aside.display()
            );
        } else if new.is_dir() {
            std::fs::remove_dir(new)?;
        }
        std::fs::rename(old, new)
    })();
    match moved {
        Ok(()) => {
            tracing::info!(
                "carry-over: moved the webview's folder {} to {}",
                old.display(),
                new.display()
            );
            true
        }
        Err(e) => {
            tracing::warn!("carry-over: could not move {}: {e}", old.display());
            false
        }
    }
}

/// A free name beside `new` for the folder a put-off launch made.
fn set_aside_name(new: &Path) -> PathBuf {
    let name = new.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    (1..)
        .map(|n| {
            let suffix = if n == 1 { String::new() } else { format!("-{n}") };
            new.with_file_name(format!("{name}.before-carry-over{suffix}"))
        })
        .find(|candidate| !candidate.exists())
        .expect("an unbounded range always has a free name")
}

fn has_content(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_id_here_is_the_one_tauri_is_given() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], BUNDLE_ID);
    }

    #[test]
    fn a_webview_folder_moves_once_nothing_is_using_it() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        std::fs::create_dir_all(old.join("localstorage")).unwrap();
        std::fs::write(old.join("localstorage/tauri_localhost_0.localstorage"), b"prefs").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let idle = |_: &[&Path]| None;

        // Not while something has it open.
        assert!(!carry_webview_folder(&old, &new, &|_| Some("Every Day is still running".into())));
        assert!(old.join("localstorage").is_dir());

        assert!(carry_webview_folder(&old, &new, &idle));
        assert!(!old.exists());
        assert_eq!(
            std::fs::read(new.join("localstorage/tauri_localhost_0.localstorage")).unwrap(),
            b"prefs"
        );
        // Nothing left to carry.
        assert!(carry_webview_folder(&old, &new, &idle));
    }

    /// The first launch was put off, and made a folder of its own under the
    /// new name. The next one puts that aside and brings the old one in.
    #[test]
    fn a_folder_a_put_off_launch_made_is_set_aside_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("prefs"), b"months of them").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("prefs"), b"one session's").unwrap();

        assert!(carry_webview_folder(&old, &new, &|_| None));
        assert_eq!(std::fs::read(new.join("prefs")).unwrap(), b"months of them");
        let aside = dir.path().join("new.before-carry-over");
        assert_eq!(std::fs::read(aside.join("prefs")).unwrap(), b"one session's");
        assert!(!old.exists());
    }
}
