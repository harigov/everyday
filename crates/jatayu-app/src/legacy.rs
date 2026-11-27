//! What the desktop shell carries across from an installation made under a
//! former name -- Karma Yoga, or Every Day before it -- before it builds a
//! window.
//!
//! The settings and data folders are `jatayu_vault::legacy`'s, moved the
//! first time anything asks where they are; [`carry_over`] asks first, so
//! the move happens -- and is logged -- before anything else starts. What
//! is the shell's alone is the webview's own folder: the interface's
//! preferences (`localStorage`) and its caches live in a folder named after
//! the bundle id, which changed with each rename, so without this a renamed
//! build would open with every preference back at its default. The
//! interface then renames the keys themselves (`ui/src/lib/legacy.ts`).
//!
//! Like everything else carried across, the webview's folder goes straight
//! from whichever former name it has to the current one, newest first
//! ([`jatayu_core::legacy::FORMERS`]): a machine that went through both
//! renames has its Karma Yoga folder (which already holds what Every Day's
//! had) carried, never Every Day's to Karma Yoga's to Jatayu's.
//!
//! The screen-time GNOME extension under an old uuid is retired by
//! `screentime::gnome::retire_old`, and old keychain entries are carried
//! across where they are read (`jatayu_vault::autounlock`, `remotes`).

use jatayu_core::legacy::{FORMERS, Former};
use std::path::{Path, PathBuf};

/// What is using a folder about to move, if anything: see
/// [`jatayu_vault::legacy::in_use`].
type Busy<'a> = &'a dyn Fn(&[&Path]) -> Option<String>;

/// Carry across whatever has to move before a window exists.
pub fn carry_over() {
    if jatayu_vault::under_test() {
        return;
    }
    // Resolving a folder is what moves them; see `jatayu_vault::legacy`.
    let config = jatayu_vault::config_dir();
    carry_webview(&config, &webview_roots(), &jatayu_vault::legacy::in_use);
    #[cfg(target_os = "linux")]
    if let Some(cache) = cache_dir() {
        carry_panel_cache(&cache, &jatayu_vault::legacy::in_use);
    }
}

/// Carry the webview's folder across from the newest former name that has
/// not been carried yet, and say so in the settings folder (`config`).
///
/// Each former name has a marker of its own ([`Former::webview_marker`]),
/// written once that name's folder has been carried across or there was
/// none to carry. Until it is there, every launch tries again; after, an old
/// build started again and making a fresh folder under its old bundle id is
/// left alone. Every Day's marker is the file the first rename wrote, so a
/// machine that went through both renames has it already and has only Karma
/// Yoga's folder to carry.
///
/// Only one folder is carried: the newest unmarked name's that is there. A
/// name's carry-over marks every older name too -- what an older folder held
/// was carried into the newer one by the rename between them -- so the
/// unmarked names are always the newest few, and a folder of an older one
/// beside a newer one's can only have been left by a launch that could not
/// carry it. It stays where it is (nothing is deleted), and is marked. Once
/// the folder is across, or there was none, every name is marked.
fn carry_webview(config: &Path, roots: &[PathBuf], busy: Busy<'_>) {
    let unmarked: Vec<&Former> = FORMERS
        .iter()
        .copied()
        .filter(|f| !f.webview_marker.is_empty() && !config.join(f.webview_marker).exists())
        .collect();
    if unmarked.is_empty() {
        return;
    }
    if let Some(former) = unmarked.iter().find(|f| webview_present(f, roots)) {
        let mut done = true;
        for root in roots {
            let family = family(root);
            let (old, new) = (root.join(former.bundle_id), root.join(BUNDLE_ID));
            done &= carry_webview_folder(&old, &new, &family, busy);
        }
        if !done {
            return;
        }
    }
    let written = jatayu_core::fsutil::create_private_dir(config).and_then(|()| {
        unmarked.iter().try_for_each(|f| {
            jatayu_core::fsutil::write_atomic(&config.join(f.webview_marker), b"", "carried")
        })
    });
    if let Err(e) = written {
        tracing::warn!("carry-over: could not record that the webview's folder moved: {e}");
    }
}

/// Whether `former` left a webview folder of its own under any of `roots`:
/// a real folder, or a link out of the family, not a link to another name's.
fn webview_present(former: &Former, roots: &[PathBuf]) -> bool {
    !former.bundle_id.is_empty()
        && roots.iter().any(|root| {
            let old = root.join(former.bundle_id);
            old.is_dir() && !is_alias(&old, &family(root))
        })
}

/// Every name the webview's folder has had under `root`, the current one
/// first.
fn family(root: &Path) -> Vec<PathBuf> {
    std::iter::once(BUNDLE_ID)
        .chain(FORMERS.iter().map(|f| f.bundle_id))
        .filter(|id| !id.is_empty())
        .map(|id| root.join(id))
        .collect()
}

/// Whether `path` is a link to another name in `family`: an alias, which a
/// carry-over never moves -- moving the link rather than what it points at
/// would leave the new name pointing at the old one. A link anywhere else is
/// somebody's own arrangement and is moved like the folder it stands for.
fn is_alias(path: &Path, family: &[PathBuf]) -> bool {
    let is_link = path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink());
    let Some(target) = is_link.then(|| path.canonicalize().ok()).flatten() else {
        return false;
    };
    family
        .iter()
        .filter(|other| other.as_path() != path)
        .any(|other| other.canonicalize().is_ok_and(|c| c == target))
}

/// The folders the webview's folders are kept under, on this platform: Tauri
/// keeps them under the app's local data directory, and macOS's WebKit keeps
/// its own store under the bundle id as well.
fn webview_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(local) = local_data_dir() {
        roots.push(local);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Library").join("WebKit"));
    }
    roots
}

/// This build's bundle identifier, as `tauri.conf.json` gives it.
const BUNDLE_ID: &str = "app.jatayu.Jatayu";

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
/// to carry: no folder there, or only an alias of another name in `family`,
/// or one that is already the new folder under another name.
///
/// A new folder already holding something was made by a launch whose
/// carry-over was put off (the old build was still running), and holds only
/// what that launch changed: it is moved aside, beside the new name, and the
/// old folder -- every preference before the rename -- takes its place.
/// Nothing is deleted either way. Nothing is left at the old path: no other
/// program reads a webview's folder.
fn carry_webview_folder(old: &Path, new: &Path, family: &[PathBuf], busy: Busy<'_>) -> bool {
    if !old.is_dir() || is_alias(old, family) || same_place(old, new) {
        return true;
    }
    // Never the vault's own folder: on a case-insensitive disk a settings
    // folder spelled like the bundle id would be the webview's too, and the
    // carry-over would set the vault aside (`jatayu_vault::project_dirs`
    // keeps the two apart; this keeps it so).
    if [jatayu_vault::config_dir(), jatayu_vault::data_dir()]
        .iter()
        .any(|own| same_place(own, new) || same_place(own, old))
    {
        tracing::warn!(
            "carry-over: left the webview's folder {} alone: it is this app's own folder",
            new.display()
        );
        return true;
    }
    // Busy covers the new folder too: a launch whose carry-over was put off
    // is running on it, and a second launch (raising the first) must not move
    // it out from under that window.
    if let Some(why) = busy(&[old, new]) {
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

/// Whether `a` and `b` are one folder -- one a link to the other, say.
fn same_place(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
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

/// What the panel's child keeps its webview in, inside the folder named for
/// the application in the cache directory (`panel::child::webview_dir`).
const PANEL_WEBVIEW: &str = "panel-webview";

/// The folder Tauri's `cache_dir` names on Linux: `$XDG_CACHE_HOME`, or
/// `~/.cache`.
#[cfg(target_os = "linux")]
fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
}

/// The name the platform directories crate gives a former name's folders on
/// Linux (`~/.config/karmayoga`), which is also what the panel's cache folder
/// was under: the application's name, lower-cased, without its spaces.
fn linux_dir_name(former: &Former) -> String {
    former.project_dirs.2.to_lowercase().replace(' ', "")
}

/// Carry the panel's webview folder from a former name's cache folder (only
/// Karma Yoga had a panel) to the current one, and take the former folder
/// away once nothing is left in it.
///
/// It is a cache, so nothing in it is precious, but it is the panel's own
/// `localStorage` and moving it costs one rename; when the current name has
/// one already, the old one is the stale copy and is removed rather than
/// left in the cache directory for good. A former name's folder that is a
/// link (an alias, or somebody's own arrangement) is not looked inside:
/// through an alias of the current folder, "the old one" would be the live
/// one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn carry_panel_cache(cache: &Path, busy: Busy<'_>) {
    let new = cache.join("jatayu").join(PANEL_WEBVIEW);
    let is_real_dir = |p: &Path| p.symlink_metadata().is_ok_and(|m| m.is_dir());
    for former in FORMERS {
        let parent = cache.join(linux_dir_name(former));
        if !is_real_dir(&parent) {
            continue;
        }
        let old = parent.join(PANEL_WEBVIEW);
        if is_real_dir(&old) {
            if let Some(why) = busy(&[old.as_path()]) {
                tracing::debug!("carry-over: left {} for now: {why}", old.display());
                continue;
            }
            let moved = if has_content(&new) {
                std::fs::remove_dir_all(&old)
            } else {
                new.parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| if new.is_dir() { std::fs::remove_dir(&new) } else { Ok(()) })
                    .and_then(|()| std::fs::rename(&old, &new))
            };
            if let Err(e) = moved {
                tracing::warn!("carry-over: could not move {}: {e}", old.display());
                continue;
            }
        }
        // Only when nothing else is in it.
        let _ = std::fs::remove_dir(&parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::legacy::{EVERY_DAY, KARMA_YOGA};

    fn idle(_: &[&Path]) -> Option<String> {
        None
    }

    fn running(_: &[&Path]) -> Option<String> {
        Some("Karma Yoga is still running".into())
    }

    /// A settings folder and the folder the webview's folders are under.
    struct Machine {
        _dir: tempfile::TempDir,
        config: PathBuf,
        root: PathBuf,
    }

    impl Machine {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config/jatayu");
            let root = dir.path().join("share");
            std::fs::create_dir_all(&config).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            Machine { config, root, _dir: dir }
        }

        fn mark(&self, former: &Former) {
            std::fs::write(self.config.join(former.webview_marker), b"").unwrap();
        }

        fn marked(&self, former: &Former) -> bool {
            self.config.join(former.webview_marker).exists()
        }

        fn webview(&self, id: &str, prefs: &str) {
            let local = self.root.join(id).join("localstorage");
            std::fs::create_dir_all(&local).unwrap();
            std::fs::write(local.join("tauri_localhost_0.localstorage"), prefs).unwrap();
        }

        fn prefs(&self, id: &str) -> Option<String> {
            std::fs::read_to_string(
                self.root.join(id).join("localstorage/tauri_localhost_0.localstorage"),
            )
            .ok()
        }

        fn carry(&self, busy: Busy<'_>) {
            carry_webview(&self.config, std::slice::from_ref(&self.root), busy);
        }
    }

    #[test]
    fn the_bundle_id_here_is_the_one_tauri_is_given() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], BUNDLE_ID);
    }

    /// The Debian package replaces each former one, named as the bundler
    /// names a package: the product's name in kebab case.
    #[test]
    fn the_package_replaces_every_former_one() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        for former in FORMERS {
            let package = former.product.to_lowercase().replace(' ', "-");
            for key in ["conflicts", "replaces"] {
                let listed = conf["bundle"]["linux"]["deb"][key].as_array().unwrap();
                assert!(listed.iter().any(|p| p == package.as_str()), "{key} lacks {package}");
            }
        }
    }

    #[test]
    fn every_former_name_has_a_marker_of_its_own() {
        assert_eq!(EVERY_DAY.webview_marker, "webview-carried-over", "the first rename's file");
        let markers: std::collections::HashSet<_> =
            FORMERS.iter().map(|f| f.webview_marker).collect();
        assert_eq!(markers.len(), FORMERS.len());
    }

    /// The owner's machine: Every Day's folder was carried to Karma Yoga's
    /// by the first rename, which left its marker. That marker must not stop
    /// Karma Yoga's folder coming across.
    #[test]
    fn a_machine_through_both_renames_carries_karma_yogas_folder() {
        let m = Machine::new();
        m.mark(&EVERY_DAY);
        m.webview(KARMA_YOGA.bundle_id, "karma yoga's");

        m.carry(&idle);

        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("karma yoga's"));
        assert!(!m.root.join(KARMA_YOGA.bundle_id).exists());
        assert!(m.marked(&KARMA_YOGA) && m.marked(&EVERY_DAY));
    }

    /// Every Day straight to Jatayu, never having run Karma Yoga.
    #[test]
    fn every_days_folder_comes_straight_across() {
        let m = Machine::new();
        m.webview(EVERY_DAY.bundle_id, "every day's");

        m.carry(&idle);

        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("every day's"));
        assert!(!m.root.join(EVERY_DAY.bundle_id).exists());
        // Karma Yoga's too: a Karma Yoga build run later makes a fresh
        // folder, which must not be carried over this one.
        assert!(m.marked(&KARMA_YOGA) && m.marked(&EVERY_DAY));
        m.webview(KARMA_YOGA.bundle_id, "fresh");
        m.carry(&idle);
        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("every day's"));
    }

    #[test]
    fn the_newest_folder_is_the_one_carried() {
        let m = Machine::new();
        m.webview(EVERY_DAY.bundle_id, "every day's");
        m.webview(KARMA_YOGA.bundle_id, "karma yoga's");

        m.carry(&idle);

        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("karma yoga's"));
        // Left where it was, not deleted, and not carried on a later launch.
        assert_eq!(m.prefs(EVERY_DAY.bundle_id).as_deref(), Some("every day's"));
        assert!(m.marked(&KARMA_YOGA) && m.marked(&EVERY_DAY));
    }

    /// An Every Day build run again after the first rename made a fresh
    /// folder under its old id; Every Day is marked, so it is left alone.
    #[test]
    fn a_marked_names_folder_is_left_alone() {
        let m = Machine::new();
        m.mark(&EVERY_DAY);
        m.webview(EVERY_DAY.bundle_id, "fresh");

        m.carry(&idle);

        assert_eq!(m.prefs(EVERY_DAY.bundle_id).as_deref(), Some("fresh"));
        assert!(!m.root.join(BUNDLE_ID).exists());
        assert!(m.marked(&KARMA_YOGA));
    }

    #[test]
    fn nothing_moves_or_is_marked_while_an_old_build_runs() {
        let m = Machine::new();
        m.mark(&EVERY_DAY);
        m.webview(KARMA_YOGA.bundle_id, "karma yoga's");

        m.carry(&running);
        assert_eq!(m.prefs(KARMA_YOGA.bundle_id).as_deref(), Some("karma yoga's"));
        assert!(!m.marked(&KARMA_YOGA));

        m.carry(&idle);
        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("karma yoga's"));
    }

    #[test]
    fn a_machine_with_no_former_install_is_marked_and_nothing_made() {
        let m = Machine::new();
        m.carry(&idle);
        assert!(m.marked(&KARMA_YOGA) && m.marked(&EVERY_DAY));
        assert!(!m.root.join(BUNDLE_ID).exists());
    }

    /// A former name's folder that is a link to another name's is an alias:
    /// the link is never what moves.
    #[cfg(unix)]
    #[test]
    fn a_link_into_the_family_is_never_moved() {
        let m = Machine::new();
        m.webview(KARMA_YOGA.bundle_id, "karma yoga's");
        std::os::unix::fs::symlink(KARMA_YOGA.bundle_id, m.root.join(EVERY_DAY.bundle_id)).unwrap();
        assert!(!webview_present(&EVERY_DAY, std::slice::from_ref(&m.root)));

        m.carry(&idle);

        let new = m.root.join(BUNDLE_ID);
        assert!(!new.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("karma yoga's"));
    }

    /// Somebody linked the old name to the new one already: that is done.
    #[cfg(unix)]
    #[test]
    fn a_link_to_the_new_folder_counts_as_carried() {
        let m = Machine::new();
        m.webview(BUNDLE_ID, "jatayu's");
        std::os::unix::fs::symlink(BUNDLE_ID, m.root.join(KARMA_YOGA.bundle_id)).unwrap();

        m.carry(&idle);

        assert_eq!(m.prefs(BUNDLE_ID).as_deref(), Some("jatayu's"));
        assert!(!m.root.join(format!("{BUNDLE_ID}.before-carry-over")).exists());
        assert!(m.marked(&KARMA_YOGA));
    }

    #[test]
    fn a_webview_folder_moves_once_nothing_is_using_it() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        std::fs::create_dir_all(old.join("localstorage")).unwrap();
        std::fs::write(old.join("localstorage/tauri_localhost_0.localstorage"), b"prefs").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let family = [new.clone(), old.clone()];

        // Not while something has it open.
        assert!(!carry_webview_folder(&old, &new, &family, &running));
        assert!(old.join("localstorage").is_dir());

        assert!(carry_webview_folder(&old, &new, &family, &idle));
        assert!(!old.exists());
        assert_eq!(
            std::fs::read(new.join("localstorage/tauri_localhost_0.localstorage")).unwrap(),
            b"prefs"
        );
        // Nothing left to carry.
        assert!(carry_webview_folder(&old, &new, &family, &idle));
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

        assert!(carry_webview_folder(&old, &new, &[new.clone(), old.clone()], &idle));
        assert_eq!(std::fs::read(new.join("prefs")).unwrap(), b"months of them");
        let aside = dir.path().join("new.before-carry-over");
        assert_eq!(std::fs::read(aside.join("prefs")).unwrap(), b"one session's");
        assert!(!old.exists());
    }

    #[test]
    fn former_folder_names_are_the_ones_on_disk() {
        assert_eq!(linux_dir_name(&KARMA_YOGA), "karmayoga");
        assert_eq!(linux_dir_name(&EVERY_DAY), "everyday");
    }

    #[test]
    fn the_panels_cache_comes_across_and_the_old_folder_goes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        let old = cache.join("karmayoga").join(PANEL_WEBVIEW);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("state"), b"panel").unwrap();

        carry_panel_cache(cache, &running);
        assert!(old.join("state").exists());

        carry_panel_cache(cache, &idle);
        assert_eq!(std::fs::read(cache.join("jatayu/panel-webview/state")).unwrap(), b"panel");
        assert!(!cache.join("karmayoga").exists());
    }

    #[test]
    fn a_stale_panel_cache_beside_a_current_one_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        std::fs::create_dir_all(cache.join("karmayoga").join(PANEL_WEBVIEW)).unwrap();
        std::fs::write(cache.join("karmayoga/panel-webview/state"), b"old").unwrap();
        // Something else of the old name's stays, and so does its folder.
        std::fs::write(cache.join("karmayoga/other"), b"theirs").unwrap();
        std::fs::create_dir_all(cache.join("jatayu").join(PANEL_WEBVIEW)).unwrap();
        std::fs::write(cache.join("jatayu/panel-webview/state"), b"new").unwrap();

        carry_panel_cache(cache, &idle);

        assert_eq!(std::fs::read(cache.join("jatayu/panel-webview/state")).unwrap(), b"new");
        assert!(!cache.join("karmayoga").join(PANEL_WEBVIEW).exists());
        assert_eq!(std::fs::read(cache.join("karmayoga/other")).unwrap(), b"theirs");
    }

    /// Through a link, the old folder would be the live one.
    #[cfg(unix)]
    #[test]
    fn a_linked_cache_folder_is_not_looked_inside() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        std::fs::create_dir_all(cache.join("jatayu").join(PANEL_WEBVIEW)).unwrap();
        std::fs::write(cache.join("jatayu/panel-webview/state"), b"live").unwrap();
        std::os::unix::fs::symlink("jatayu", cache.join("karmayoga")).unwrap();

        carry_panel_cache(cache, &idle);

        assert_eq!(std::fs::read(cache.join("jatayu/panel-webview/state")).unwrap(), b"live");
    }
}
