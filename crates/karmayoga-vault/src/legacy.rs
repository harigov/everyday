//! Carrying an installation of Every Day across to Karma Yoga.
//!
//! The application's settings and its default vault lived in folders named
//! after the old product -- `~/.config/everyday` and `~/.local/share/everyday`
//! on Linux, the `ProjectDirs` triple in [`karmayoga_core::legacy`] on every
//! platform. [`carry_over`] moves them to their new names the first time a
//! renamed build asks where anything is: [`crate::config_dir`] and
//! [`crate::data_dir`] both call it before they answer, so no caller can
//! read or write a path before the move has had its chance.
//!
//! # How nothing is lost
//!
//! - **A move, never a copy.** Each folder is renamed in one call, inside the
//!   same parent folder, so it is the same files under a new name: nothing
//!   inside is read, rewritten or duplicated, a vault of several gigabytes
//!   moves as fast as an empty one, and there is no half-copied state for an
//!   interruption to leave behind.
//! - **Only when nothing is using them.** The default vault's write lock must
//!   be free, and on Linux no running process may be an old build
//!   ([`karmayoga_core::legacy::APP_BINARY`], `CLI_BINARY`) or have a file
//!   open beneath either folder. Otherwise nothing moves, the paths below
//!   answer with the old folders, and the move is tried again next launch.
//! - **The old paths keep working.** A link is left at each old path pointing
//!   at the new one, for anything outside this application still holding the
//!   old path -- the old GNOME extension, a script, an MCP client's config.
//! - **Nothing is guessed at.** If a new folder already holds something, the
//!   pair is left alone and reported; the paths prefer the new folder.
//!
//! After the folders, the pointer to the last vault is rewritten if it named
//! a path inside the old data folder, and a Postgres vault's caches -- kept
//! in the settings folder under a name made from the vault's path -- follow
//! the vault to its new path.
//!
//! Keychain entries are carried across where they are read: see
//! [`crate::autounlock`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use karmayoga_core::legacy as old;

/// The four folders a carry-over moves between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folders {
    pub old_config: PathBuf,
    pub new_config: PathBuf,
    pub old_data: PathBuf,
    pub new_data: PathBuf,
}

impl Folders {
    /// This platform's, for this user. `None` when the platform has no home
    /// directory to speak of, in which case there is nothing to carry over.
    pub fn of_this_user() -> Option<Self> {
        let (q, o, a) = old::PROJECT_DIRS;
        let before = directories::ProjectDirs::from(q, o, a)?;
        let after = crate::project_dirs()?;
        Some(Self {
            old_config: before.config_dir().to_path_buf(),
            new_config: after.config_dir().to_path_buf(),
            old_data: before.data_dir().to_path_buf(),
            new_data: after.data_dir().to_path_buf(),
        })
    }
}

/// What a carry-over did, or would do: one line per step, for the log and
/// for `karmayoga migrate`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub steps: Vec<String>,
    /// Why some or all of it was put off, if it was.
    pub put_off: Option<String>,
}

impl Report {
    fn step(&mut self, line: String) {
        tracing::info!("carry-over: {line}");
        self.steps.push(line);
    }
}

/// Move this user's folders across, once per process. A no-op inside a test
/// binary, which never sees this user's folders at all.
pub fn carry_over() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if crate::under_test() {
            return;
        }
        let Some(folders) = Folders::of_this_user() else { return };
        let report = migrate(&folders, &in_use, false);
        if let Some(why) = &report.put_off {
            tracing::warn!("carry-over from {} put off: {why}", old::PRODUCT);
        }
    });
}

/// [`carry_over`], now, with what it did. Nothing inside a test binary.
pub fn carry_over_now() -> Report {
    if crate::under_test() {
        return Report::default();
    }
    match Folders::of_this_user() {
        Some(folders) => migrate(&folders, &in_use, false),
        None => Report::default(),
    }
}

/// What [`carry_over`] would do for this user, without doing any of it.
pub fn preview() -> Report {
    match Folders::of_this_user() {
        Some(folders) => migrate(&folders, &in_use, true),
        None => {
            Report { put_off: Some("this platform has no home folder".into()), ..Report::default() }
        }
    }
}

/// The folder to use of `new` and `old`: the new one unless the move was put
/// off -- the new one absent or empty while the old one is there.
pub(crate) fn resolve(new: PathBuf, old: Option<PathBuf>) -> PathBuf {
    match old {
        Some(old) if !has_content(&new) && old.is_dir() => old,
        _ => new,
    }
}

/// Has the settings folder been carried across -- or was there never an old
/// one? `false` while its move is put off.
pub fn settings_moved() -> bool {
    match Folders::of_this_user() {
        Some(folders) => {
            !folders.old_config.exists() || links_to(&folders.old_config, &folders.new_config)
        }
        None => true,
    }
}

/// The old folder of the settings or the data folder, for [`resolve`].
pub(crate) fn old_counterpart(config: bool) -> Option<PathBuf> {
    let folders = Folders::of_this_user()?;
    Some(if config { folders.old_config } else { folders.old_data })
}

/// The move itself, over explicit folders so tests can give it their own.
///
/// `busy` names whatever is using any of the paths it is given, or `None`.
/// With `dry_run`, every step is reported and none is taken.
pub fn migrate(
    folders: &Folders,
    busy: &dyn Fn(&[&Path]) -> Option<String>,
    dry_run: bool,
) -> Report {
    let mut report = Report::default();
    // macOS keeps settings and data in one folder; it is moved once.
    let one_folder = folders.old_config == folders.old_data;
    let moves: Vec<(&Path, &Path, &str)> = if one_folder {
        vec![(&folders.old_data, &folders.new_data, "settings and data")]
    } else {
        vec![
            (&folders.old_config, &folders.new_config, "settings"),
            (&folders.old_data, &folders.new_data, "data"),
        ]
    };
    let config = Pair::of(&folders.old_config, &folders.new_config);
    let data = Pair::of(&folders.old_data, &folders.new_data);

    for &(from, to, what) in &moves {
        if Pair::of(from, to) == Pair::Conflict {
            report.put_off = Some(format!(
                "both the old and the new {what} folder hold files; left both alone ({} and {})",
                from.display(),
                to.display(),
            ));
        }
    }

    let pending: Vec<&Path> = moves
        .iter()
        .filter(|(from, to, _)| Pair::of(from, to) == Pair::Pending)
        .map(|(from, _, _)| *from)
        .collect();

    if !pending.is_empty() {
        if let Some(why) = busy(&pending) {
            report.put_off = Some(why);
            // A dry run still says what the move will be, once it can happen.
            if !dry_run {
                return report;
            }
        }
        // The vault's own lock, taken for the length of the move so nothing
        // can open it halfway. Released before the rename on Windows, which
        // refuses to rename a folder with a file open inside it.
        let mut lock = match data == Pair::Pending && folders.old_data.join("vault.json").exists() {
            false => None,
            true if dry_run => None,
            true => match karmayoga_core::lockfile::acquire(&folders.old_data) {
                Ok(Some(lock)) => Some(lock),
                Ok(None) => {
                    report.put_off = Some(format!(
                        "the vault in {} is open in another program",
                        folders.old_data.display()
                    ));
                    return report;
                }
                Err(e) => {
                    report.put_off = Some(format!("could not check the vault's lock: {e}"));
                    return report;
                }
            },
        };

        for &(from, to, what) in &moves {
            // Asked again right before each move, not trusted from above:
            // whatever moved first has changed what is on disk.
            if Pair::of(from, to) != Pair::Pending {
                continue;
            }
            report.step(format!("move the {what} folder {} to {}", from.display(), to.display()));
            if dry_run {
                continue;
            }
            if from == folders.old_data && cfg!(windows) {
                drop(lock.take());
            }
            if let Err(e) = move_folder(from, to) {
                report.put_off = Some(format!("could not move {}: {e}", from.display()));
                return report;
            }
            match link_back(from, to) {
                Ok(true) => {
                    report.step(format!("leave a link at {} to the new folder", from.display()))
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("carry-over: no link left at {}: {e}", from.display()),
            }
        }
        drop(lock);
    }

    // Checked whenever the data folder has moved, not only on the launch
    // that moved it: a pointer an old build wrote through the link after the
    // move names the old path too, and a cache a failed launch left behind
    // is still found.
    let config_dir =
        if dry_run && config == Pair::Pending { &folders.old_config } else { &folders.new_config };
    if matches!(data, Pair::Done | Pair::Pending) {
        repoint(config_dir, folders, dry_run, &mut report);
        move_postgres_caches(config_dir, folders, dry_run, &mut report);
    }
    report
}

/// Where one old folder stands against its new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pair {
    /// No old folder: a fresh install, or a platform that never had one.
    Nothing,
    /// The old folder is there and the new one is not (or is empty).
    Pending,
    /// Moved already: the old path is the link a move leaves, or gone with
    /// the new folder in place.
    Done,
    /// Both hold files. Left alone.
    Conflict,
}

impl Pair {
    fn of(old: &Path, new: &Path) -> Self {
        // A link somebody made themselves -- the folder kept on another disk
        // -- is moved like a folder: the new path becomes the same link.
        if links_to(old, new) || (!old.exists() && !is_link(old)) {
            return if new.exists() { Pair::Done } else { Pair::Nothing };
        }
        if has_content(new) { Pair::Conflict } else { Pair::Pending }
    }
}

fn is_link(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
}

/// Is `old` the link a finished move left, pointing at `new`?
fn links_to(old: &Path, new: &Path) -> bool {
    is_link(old) && std::fs::read_link(old).is_ok_and(|target| target == new)
}

/// A folder with at least one entry in it.
fn has_content(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn move_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.is_dir() {
        // Empty: `Pair::of` said so. Out of the way, or the rename fails.
        std::fs::remove_dir(to)?;
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)
}

/// A link at `old` to `new`. `Ok(false)` where links need privileges this
/// process does not have (Windows), which is not a failure of the move.
fn link_back(old: &Path, new: &Path) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(new, old)?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = (old, new);
        Ok(false)
    }
}

/// Rewrite the last-vault pointer if it names a path inside the old data
/// folder.
fn repoint(config_dir: &Path, folders: &Folders, dry_run: bool, report: &mut Report) {
    let file = config_dir.join("last-vault");
    let Some(recorded) = crate::read_pointer(&file) else { return };
    let Ok(rest) = recorded.strip_prefix(&folders.old_data) else { return };
    let moved = if rest.as_os_str().is_empty() {
        folders.new_data.clone()
    } else {
        folders.new_data.join(rest)
    };
    report.step(format!(
        "point the last vault at {} instead of {}",
        moved.display(),
        recorded.display()
    ));
    if dry_run {
        return;
    }
    if let Err(e) = crate::write_pointer(&file, &moved) {
        tracing::warn!("carry-over: could not rewrite {}: {e}", file.display());
    }
}

/// A Postgres vault keeps its Matrix store and its mail index in the
/// settings folder, under names made from the vault's path -- which the move
/// just changed. Each follows the vault, so the bot keeps its identity and the
/// index is not rebuilt. A SQLite vault keeps both inside the vault, which
/// moved with it.
fn move_postgres_caches(config_dir: &Path, folders: &Folders, dry_run: bool, report: &mut Report) {
    let header = if dry_run {
        folders.old_data.join("vault.json")
    } else {
        folders.new_data.join("vault.json")
    };
    let is_postgres = std::fs::read(&header)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|v| v.get("backend").and_then(|b| b.as_str()) == Some("postgres"));
    if !is_postgres {
        return;
    }
    let caches = [
        ("chat", chat_dir_name(&folders.old_data), chat_dir_name(&folders.new_data)),
        ("mail-index", index_dir_name(&folders.old_data), index_dir_name(&folders.new_data)),
    ];
    for (kind, from, to) in caches {
        let (from, to) = (config_dir.join(kind).join(from), config_dir.join(kind).join(to));
        if !from.is_dir() || to.exists() {
            continue;
        }
        report.step(format!(
            "move the vault's {kind} folder {} to {}",
            from.display(),
            to.display()
        ));
        if !dry_run && let Err(e) = std::fs::rename(&from, &to) {
            tracing::warn!("carry-over: could not move {}: {e}", from.display());
        }
    }
}

fn chat_dir_name(vault: &Path) -> String {
    karmayoga_core::fsutil::stable_path_name(vault)
}

fn index_dir_name(vault: &Path) -> String {
    karmayoga_core::fsutil::cache_path_name(vault)
}

/// What is using any of `paths`, if anything is: an old build still running,
/// or any process with a file open beneath one of them. Only Linux can be
/// asked; elsewhere the vault's lock is the whole of the check.
pub fn in_use(paths: &[&Path]) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux_in_use(paths)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = paths;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_in_use(paths: &[&Path]) -> Option<String> {
    old_build_running().or_else(|| open_beneath(paths))
}

/// The other processes this user can see, by pid and `/proc` entry.
#[cfg(target_os = "linux")]
fn other_processes() -> impl Iterator<Item = (u32, PathBuf)> {
    let me = std::process::id();
    std::fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(move |entry| {
        let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
        (pid != me).then(|| (pid, entry.path()))
    })
}

/// An old build of the app or the command line, still running.
#[cfg(target_os = "linux")]
fn old_build_running() -> Option<String> {
    other_processes().find_map(|(pid, proc)| {
        let exe = std::fs::read_link(proc.join("exe")).ok()?;
        let name = exe.file_name()?.to_str()?;
        let name = name.strip_suffix(" (deleted)").unwrap_or(name);
        (name == old::APP_BINARY || name == old::CLI_BINARY)
            .then(|| format!("{} is still running (process {pid}); quit it first", old::PRODUCT))
    })
}

/// Any process with a file open beneath one of `paths`.
#[cfg(target_os = "linux")]
fn open_beneath(paths: &[&Path]) -> Option<String> {
    let roots: Vec<PathBuf> =
        paths.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf())).collect();
    other_processes().find_map(|(pid, proc)| {
        std::fs::read_dir(proc.join("fd")).ok()?.flatten().find_map(|fd| {
            let target = std::fs::read_link(fd.path()).ok()?;
            roots
                .iter()
                .any(|root| target.starts_with(root))
                .then(|| format!("process {pid} has {} open", target.display()))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders(root: &Path) -> Folders {
        Folders {
            old_config: root.join("config/everyday"),
            new_config: root.join("config/karmayoga"),
            old_data: root.join("share/everyday"),
            new_data: root.join("share/karmayoga"),
        }
    }

    fn idle(_: &[&Path]) -> Option<String> {
        None
    }

    /// An installation of the old build: settings, a pointer to the vault in
    /// its default folder, and a vault with a database in it.
    fn old_install(f: &Folders) {
        std::fs::create_dir_all(&f.old_config).unwrap();
        std::fs::create_dir_all(f.old_data.join("store")).unwrap();
        std::fs::write(f.old_config.join("mcp.json"), b"{\"token\":\"t\"}").unwrap();
        std::fs::write(f.old_config.join("last-vault"), f.old_data.to_string_lossy().as_bytes())
            .unwrap();
        std::fs::write(f.old_data.join("vault.json"), b"{\"backend\":\"sqlite\"}").unwrap();
        std::fs::write(f.old_data.join("store/everyday.db"), b"the records").unwrap();
    }

    #[test]
    fn moves_both_folders_links_back_and_repoints() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");

        assert_eq!(std::fs::read(f.new_data.join("store/everyday.db")).unwrap(), b"the records");
        assert_eq!(std::fs::read(f.new_config.join("mcp.json")).unwrap(), b"{\"token\":\"t\"}");
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(f.new_data.clone()));
        // The old paths still lead to the same files.
        assert!(links_to(&f.old_config, &f.new_config) && links_to(&f.old_data, &f.new_data));
        assert_eq!(std::fs::read(f.old_data.join("store/everyday.db")).unwrap(), b"the records");

        // And a second run finds nothing left to do.
        let again = migrate(&f, &idle, false);
        assert_eq!(again, Report::default());
    }

    /// macOS: the settings and the data are one folder
    /// (`~/Library/Application Support/<id>`), moved once.
    #[test]
    fn one_folder_for_settings_and_data_moves_once() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Application Support/app.Every-Day.EveryDay");
        let new = dir.path().join("Application Support/app.Karma-Yoga.KarmaYoga");
        let f = Folders {
            old_config: old.clone(),
            new_config: new.clone(),
            old_data: old.clone(),
            new_data: new.clone(),
        };
        std::fs::create_dir_all(old.join("store")).unwrap();
        std::fs::write(old.join("vault.json"), b"{\"backend\":\"sqlite\"}").unwrap();
        std::fs::write(old.join("store/everyday.db"), b"the records").unwrap();
        std::fs::write(old.join("last-vault"), old.to_string_lossy().as_bytes()).unwrap();

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert_eq!(
            report.steps.iter().filter(|s| s.starts_with("move the")).count(),
            1,
            "{:?}",
            report.steps
        );
        assert!(links_to(&old, &new));
        assert_eq!(std::fs::read(new.join("store/everyday.db")).unwrap(), b"the records");
        assert_eq!(crate::read_pointer(&new.join("last-vault")), Some(new.clone()));
        assert_eq!(migrate(&f, &idle, false), Report::default());

        // Nothing in it at all: still one move, and no folder that links to
        // itself.
        let empty = tempfile::tempdir().unwrap();
        let old = empty.path().join("old");
        let new = empty.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        let f = Folders {
            old_config: old.clone(),
            new_config: new.clone(),
            old_data: old.clone(),
            new_data: new.clone(),
        };
        assert_eq!(migrate(&f, &idle, false).put_off, None);
        assert!(new.is_dir() && !is_link(&new));
        assert!(links_to(&old, &new));
    }

    #[test]
    fn a_dry_run_reports_the_steps_and_takes_none() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);

        let report = migrate(&f, &idle, true);
        assert_eq!(report.put_off, None);
        assert_eq!(report.steps.len(), 3, "{:?}", report.steps);
        assert!(report.steps[2].contains("point the last vault"), "{:?}", report.steps);
        assert!(f.old_config.is_dir() && !is_link(&f.old_config) && !f.new_config.exists());
        assert!(f.old_data.is_dir() && !f.new_data.exists());
        assert_eq!(crate::read_pointer(&f.old_config.join("last-vault")), Some(f.old_data.clone()));
    }

    #[test]
    fn something_in_use_puts_the_whole_move_off() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);

        let busy = |_: &[&Path]| Some("Every Day is still running".to_string());
        let preview = migrate(&f, &busy, true);
        assert_eq!(preview.put_off.as_deref(), Some("Every Day is still running"));
        assert_eq!(preview.steps.len(), 3, "a dry run still lists the steps: {preview:?}");

        let report = migrate(&f, &busy, false);
        assert_eq!(report.put_off.as_deref(), Some("Every Day is still running"));
        assert!(report.steps.is_empty());
        assert!(!f.new_config.exists() && !f.new_data.exists());
        // Until it is moved, the old folders are the ones to use.
        assert_eq!(resolve(f.new_data.clone(), Some(f.old_data.clone())), f.old_data);
        assert_eq!(resolve(f.new_config.clone(), Some(f.old_config.clone())), f.old_config);
    }

    #[test]
    fn an_open_vault_puts_the_move_off() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);
        let held = karmayoga_core::lockfile::acquire(&f.old_data).unwrap();
        assert!(held.is_some());

        let report = migrate(&f, &idle, false);
        assert!(
            report.put_off.as_deref().unwrap_or("").contains("open in another program"),
            "{report:?}"
        );
        assert!(f.old_data.is_dir() && !is_link(&f.old_data) && !f.new_data.exists());
    }

    #[test]
    fn a_new_folder_with_files_in_it_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);
        std::fs::create_dir_all(&f.new_data).unwrap();
        std::fs::write(f.new_data.join("something"), b"x").unwrap();

        let report = migrate(&f, &idle, false);
        assert!(
            report
                .put_off
                .as_deref()
                .unwrap_or("")
                .contains("both the old and the new data folder")
        );
        // The settings still moved; the data did not.
        assert!(links_to(&f.old_config, &f.new_config));
        assert!(f.old_data.join("store/everyday.db").exists() && !is_link(&f.old_data));
        assert!(f.new_data.join("something").exists());
    }

    #[test]
    fn an_empty_new_folder_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);
        std::fs::create_dir_all(&f.new_config).unwrap();
        std::fs::create_dir_all(&f.new_data).unwrap();

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert!(f.new_data.join("vault.json").exists());
        assert!(links_to(&f.old_data, &f.new_data));
        // And until it was moved, an empty new folder would not have hidden the old one.
        let fresh = tempfile::tempdir().unwrap();
        let g = folders(fresh.path());
        old_install(&g);
        std::fs::create_dir_all(&g.new_data).unwrap();
        assert_eq!(resolve(g.new_data.clone(), Some(g.old_data.clone())), g.old_data);
    }

    #[test]
    fn a_pointer_to_a_vault_elsewhere_is_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);
        let elsewhere = dir.path().join("external/Journal");
        std::fs::write(f.old_config.join("last-vault"), elsewhere.to_string_lossy().as_bytes())
            .unwrap();

        migrate(&f, &idle, false);
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(elsewhere));
    }

    #[test]
    fn a_pointer_written_through_the_link_after_the_move_is_fixed_next_time() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);
        migrate(&f, &idle, false);
        // An old build, run once more, records its vault by the old path.
        std::fs::write(f.old_config.join("last-vault"), f.old_data.to_string_lossy().as_bytes())
            .unwrap();

        migrate(&f, &idle, false);
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(f.new_data.clone()));
    }

    #[test]
    fn a_fresh_install_has_nothing_to_carry() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        assert_eq!(migrate(&f, &idle, false), Report::default());
        assert_eq!(resolve(f.new_data.clone(), Some(f.old_data.clone())), f.new_data);
    }

    #[test]
    fn a_postgres_vaults_caches_follow_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        old_install(&f);
        std::fs::write(f.old_data.join("vault.json"), b"{\"backend\":\"postgres\"}").unwrap();
        let chat = f.old_config.join("chat").join(chat_dir_name(&f.old_data));
        let index = f.old_config.join("mail-index").join(index_dir_name(&f.old_data));
        std::fs::create_dir_all(&chat).unwrap();
        std::fs::create_dir_all(&index).unwrap();
        std::fs::write(chat.join("matrix.sqlite3"), b"keys").unwrap();

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        let moved = f.new_config.join("chat").join(chat_dir_name(&f.new_data));
        assert_eq!(std::fs::read(moved.join("matrix.sqlite3")).unwrap(), b"keys");
        assert!(f.new_config.join("mail-index").join(index_dir_name(&f.new_data)).is_dir());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn an_open_file_beneath_a_folder_counts_as_in_use() {
        let dir = tempfile::tempdir().unwrap();
        // This process is skipped, so ask about a file a child holds open.
        let file = dir.path().join("held");
        std::fs::write(&file, b"x").unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::fs::File::open(&file).unwrap())
            .spawn()
            .unwrap();
        // Give the child a moment to exist with its descriptor.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let found = open_beneath(&[dir.path()]);
        child.kill().ok();
        child.wait().ok();
        assert!(found.is_some_and(|why| why.contains("held")));
        assert_eq!(open_beneath(&[&dir.path().join("nothing-here")]), None);
    }
}
