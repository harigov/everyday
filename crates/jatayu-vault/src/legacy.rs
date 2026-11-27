//! Carrying an installation made under an earlier name across to Jatayu.
//!
//! The application's settings and its default vault lived in folders named
//! after the product of the time: Every Day's `~/.config/everyday` and
//! `~/.local/share/everyday` on Linux, then Karma Yoga's `~/.config/karmayoga`
//! and `~/.local/share/karmayoga` -- each name's `ProjectDirs` triple is in
//! [`jatayu_core::legacy::FORMERS`], for every platform. [`carry_over`] moves
//! them to Jatayu's the first time a build asks where anything is:
//! [`crate::config_dir`] and [`crate::data_dir`] both call it before they
//! answer, so no caller can read or write a path before the move has had its
//! chance.
//!
//! # Straight across, newest first
//!
//! Every earlier name is looked at, newest first, and whichever holds the
//! folders is carried straight to Jatayu's -- never Every Day to Karma Yoga
//! and then on, which would make Karma Yoga folders and links on a machine
//! that never ran it. A machine that went through both renames has Karma
//! Yoga's folders and, at Every Day's paths, the links the first rename left
//! (`~/.config/everyday -> ~/.config/karmayoga`).
//!
//! A path that is a link to another of the family's folders -- an earlier
//! name's, a later one's, or Jatayu's own -- is an *alias*. It is never moved
//! itself: moving the link would make `~/.config/jatayu` a link to
//! `~/.config/karmayoga`, the wrong way round, with the files still under the
//! old name. It counts as carried once it leads to the new folder, which it
//! does through the link left where the folder it names used to be. A link
//! to anywhere else -- the folder kept on another disk -- is moved as the
//! folder would be: the new path becomes the same link.
//!
//! # How nothing is lost
//!
//! - **A move, never a copy.** Each folder is renamed in one call, inside the
//!   same parent folder, so it is the same files under a new name: nothing
//!   inside is read, rewritten or duplicated, a vault of several gigabytes
//!   moves as fast as an empty one, and there is no half-copied state for an
//!   interruption to leave behind.
//! - **Only when nothing is using them.** The default vault's write lock must
//!   be free, and on Linux no running process may be an old build (any
//!   earlier name's app or command line, [`jatayu_core::legacy::Former`]) or
//!   have a file open beneath a folder about to move. Otherwise nothing
//!   moves, the paths below answer with the old folders, and the move is
//!   tried again next launch.
//! - **The old paths keep working.** A link is left at each moved folder's
//!   old path pointing at the new one, for anything outside this application
//!   still holding the old path -- the old GNOME extension, a script, an MCP
//!   client's config. The links an earlier rename left keep working through
//!   it.
//! - **Nothing is guessed at.** If a new folder already holds something, an
//!   old folder beside it is left alone and reported; the paths prefer the
//!   new folder.
//!
//! After the folders, the pointer to the last vault is rewritten if it named
//! a path inside an old data folder, and a Postgres vault's caches -- kept in
//! the settings folder under a name made from the vault's path -- follow the
//! vault to its new path.
//!
//! Keychain entries are carried across where they are read: see
//! [`crate::autounlock`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jatayu_core::legacy as old;

/// One earlier name's settings and data folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldFolders {
    /// The product they were named after, for what a report says.
    pub product: &'static str,
    pub config: PathBuf,
    pub data: PathBuf,
}

/// The folders a carry-over moves between: each earlier name's, newest
/// first, and Jatayu's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folders {
    pub old: Vec<OldFolders>,
    pub new_config: PathBuf,
    pub new_data: PathBuf,
}

impl Folders {
    /// This platform's, for this user. `None` when the platform has no home
    /// directory to speak of, in which case there is nothing to carry over.
    pub fn of_this_user() -> Option<Self> {
        let after = crate::project_dirs()?;
        let old = old::FORMERS
            .iter()
            .filter_map(|former| {
                let (q, o, a) = former.project_dirs;
                let before = directories::ProjectDirs::from(q, o, a)?;
                Some(OldFolders {
                    product: former.product,
                    config: before.config_dir().to_path_buf(),
                    data: before.data_dir().to_path_buf(),
                })
            })
            .collect();
        Some(Self {
            old,
            new_config: after.config_dir().to_path_buf(),
            new_data: after.data_dir().to_path_buf(),
        })
    }

    /// What there is to move: the settings folder and the data folder, or on
    /// macOS -- which keeps both in one folder -- that folder, moved once.
    fn slots(&self) -> Vec<Slot<'_>> {
        let data = Slot {
            what: "data",
            holds_vault: true,
            new: &self.new_data,
            old: self.old.iter().map(|f| (f.product, f.data.as_path())).collect(),
        };
        if self.new_config == self.new_data {
            return vec![Slot { what: "settings and data", ..data }];
        }
        let config = Slot {
            what: "settings",
            holds_vault: false,
            new: &self.new_config,
            old: self.old.iter().map(|f| (f.product, f.config.as_path())).collect(),
        };
        vec![config, data]
    }

    fn old_configs(&self) -> Vec<PathBuf> {
        self.old.iter().map(|f| f.config.clone()).collect()
    }
}

/// One folder to carry across, under each name it has had.
struct Slot<'a> {
    what: &'static str,
    /// Whether it is the default vault's folder, whose lock the move takes.
    holds_vault: bool,
    new: &'a Path,
    /// Each earlier name's folder for it, newest first, with its product.
    old: Vec<(&'static str, &'a Path)>,
}

impl<'a> Slot<'a> {
    /// Every name the folder has had, Jatayu's among them: what a link has
    /// to name to be an alias.
    fn family(&self) -> Vec<&'a Path> {
        std::iter::once(self.new).chain(self.old.iter().map(|&(_, path)| path)).collect()
    }
}

/// What carrying one slot across will do, worked out before anything moves.
struct Plan<'a> {
    slot: Slot<'a>,
    /// The folders to move to the new one, in order: the newest name's, and
    /// an older one's only if what went before it was empty.
    moves: Vec<(&'static str, &'a Path)>,
    /// Folders that hold files and will be left where they are, because the
    /// new folder holds files already or will once a newer one has moved.
    left: Vec<(&'static str, &'a Path)>,
}

impl<'a> Plan<'a> {
    fn of(slot: Slot<'a>) -> Self {
        let family = slot.family();
        let mut filled = has_content(slot.new);
        let (mut moves, mut left) = (Vec::new(), Vec::new());
        for &(product, old) in &slot.old {
            // Two names leading to one folder elsewhere: once the first has
            // moved, the second leads to the new one too.
            if moves.iter().any(|&(_, moving)| same_place(old, moving)) {
                continue;
            }
            match pair(old, slot.new, &family) {
                Pair::Pending if !filled => {
                    moves.push((product, old));
                    filled = has_content(old);
                }
                // An empty one has nothing in it to lose, and is not worth
                // a warning on every launch.
                Pair::Pending | Pair::Conflict if has_content(old) => left.push((product, old)),
                Pair::Pending | Pair::Conflict | Pair::Nothing | Pair::Alias | Pair::Done => {}
            }
        }
        Self { slot, moves, left }
    }
}

/// What a carry-over did, or would do: one line per step, for the log and
/// for `jatayu migrate`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// The earlier names whose folders this carried across, or would have:
    /// newest first, and empty when nothing of theirs needed moving.
    pub from: Vec<&'static str>,
    pub steps: Vec<String>,
    /// Why some or all of it was put off, if it was.
    pub put_off: Option<String>,
}

impl Report {
    fn step(&mut self, line: String) {
        tracing::info!("carry-over: {line}");
        self.steps.push(line);
    }

    fn carrying(&mut self, product: &'static str) {
        if !self.from.contains(&product) {
            self.from.push(product);
            self.from.sort_by_key(|p| old::FORMERS.iter().position(|f| f.product == *p));
        }
    }

    /// Add `why` to what was put off: a folder left beside a new one and a
    /// process still running are both worth saying.
    fn defer(&mut self, why: String) {
        self.put_off = Some(match self.put_off.take() {
            Some(earlier) => format!("{earlier}; {why}"),
            None => why,
        });
    }

    /// The names this is about, for a sentence: `Karma Yoga`, or
    /// `Karma Yoga and Every Day`. Empty when nothing was found to carry.
    pub fn names(&self) -> String {
        spoken(&self.from, "and")
    }
}

/// Every earlier name, for a sentence: `Karma Yoga or Every Day`.
pub fn former_names() -> String {
    spoken(&old::FORMERS.map(|f| f.product), "or")
}

fn spoken(names: &[&str], joiner: &str) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} {joiner} {last}", rest.join(", ")),
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
            let from = report.names();
            let from = if from.is_empty() { "an earlier name" } else { from.as_str() };
            tracing::warn!("carry-over from {from} put off: {why}");
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

/// The folder to use of `new` and its earlier names `olds` (newest first):
/// the new one unless its move was put off -- the new one absent or empty
/// while an old one is a real folder.
///
/// Of the old ones, the newest that holds files, else the newest there at
/// all; an alias is never the answer, because what it leads to is among them
/// under its own name. Asked on every look-up, so the new folder is tried
/// first and the rest only while it is empty.
pub(crate) fn resolve(new: PathBuf, olds: &[PathBuf]) -> PathBuf {
    if has_content(&new) {
        return new;
    }
    let family: Vec<&Path> =
        std::iter::once(new.as_path()).chain(olds.iter().map(PathBuf::as_path)).collect();
    let real: Vec<&PathBuf> = olds
        .iter()
        .filter(|old| old.is_dir() && !same_place(old, &new) && !is_alias(old, &family))
        .collect();
    let found = real.iter().copied().find(|old| has_content(old)).or_else(|| real.first().copied());
    found.cloned().unwrap_or(new)
}

/// Has the settings folder been carried across -- or was there never an old
/// one? `false` while its move is put off, so the settings are still read
/// from an old folder.
pub fn settings_moved() -> bool {
    match Folders::of_this_user() {
        Some(folders) => settings_moved_in(&folders),
        None => true,
    }
}

/// [`settings_moved`], over explicit folders. True once the settings are
/// read from the new folder: every old one gone, an alias, or left beside a
/// new one that holds the settings already.
fn settings_moved_in(folders: &Folders) -> bool {
    resolve(folders.new_config.clone(), &folders.old_configs()) == folders.new_config
}

/// The earlier names' settings folders or data folders, newest first, for
/// [`resolve`].
///
/// With no home folder the paths are the relative fallbacks
/// ([`old::Former::home_fallbacks`]), which are read but never moved: relative
/// to wherever the program happens to have been started, a folder of that
/// name is only a guess at being this application's.
pub(crate) fn old_counterparts(config: bool) -> Vec<PathBuf> {
    match Folders::of_this_user() {
        Some(folders) => {
            folders.old.into_iter().map(|f| if config { f.config } else { f.data }).collect()
        }
        None => old::FORMERS
            .iter()
            .map(|f| if config { f.home_fallbacks.1 } else { f.home_fallbacks.0 })
            .filter(|name| !name.is_empty())
            .map(PathBuf::from)
            .collect(),
    }
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
    let plans: Vec<Plan> = folders.slots().into_iter().map(Plan::of).collect();

    for plan in &plans {
        for &(product, old) in &plan.left {
            report.defer(format!(
                "the {product} {what} folder and the new one both hold files; left both alone \
                 ({} and {})",
                old.display(),
                plan.slot.new.display(),
                what = plan.slot.what,
            ));
        }
        for &(product, _) in &plan.moves {
            report.carrying(product);
        }
    }

    let pending: Vec<&Path> =
        plans.iter().flat_map(|plan| plan.moves.iter().map(|&(_, old)| old)).collect();
    if !pending.is_empty() {
        if let Some(why) = busy(&pending) {
            report.defer(why);
            // A dry run still says what the move will be, once it can happen.
            if !dry_run {
                return report;
            }
        }
        // The vault's own lock, taken for the length of the move so nothing
        // can open it halfway. Released before the rename on Windows, which
        // refuses to rename a folder with a file open inside it.
        let mut locks = Vec::new();
        let vaults = plans
            .iter()
            .filter(|plan| plan.slot.holds_vault)
            .flat_map(|plan| plan.moves.iter().map(|&(_, old)| old))
            .filter(|old| !dry_run && old.join("vault.json").exists());
        for old in vaults {
            match jatayu_core::lockfile::acquire(old) {
                Ok(Some(lock)) => locks.push((old, lock)),
                Ok(None) => {
                    report.defer(format!(
                        "the vault in {} is open in another program",
                        old.display()
                    ));
                    return report;
                }
                Err(e) => {
                    report.defer(format!("could not check the vault's lock: {e}"));
                    return report;
                }
            }
        }

        for plan in &plans {
            let (to, family) = (plan.slot.new, plan.slot.family());
            for &(product, from) in &plan.moves {
                // Asked again right before each move, not trusted from above:
                // whatever moved first has changed what is on disk.
                if !dry_run && pair(from, to, &family) != Pair::Pending {
                    continue;
                }
                report.step(format!(
                    "move the {product} {} folder {} to {}",
                    plan.slot.what,
                    from.display(),
                    to.display()
                ));
                if dry_run {
                    continue;
                }
                if cfg!(windows) {
                    locks.retain(|(held, _)| *held != from);
                }
                if let Err(e) = move_folder(from, to) {
                    report.defer(format!("could not move {}: {e}", from.display()));
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
        }
        drop(locks);
    }

    // Checked whenever a data folder has moved, not only on the launch that
    // moved it: a pointer an old build wrote through a link after the move
    // names an old path too, and a cache a failed launch left behind is
    // still found. The settings are read from wherever they are now -- the
    // new folder, or in a dry run the old one about to move.
    let config_dir = resolve(folders.new_config.clone(), &folders.old_configs());
    let leading = leading_to_new_data(folders, &plans, dry_run);
    repoint(&config_dir, &leading, &folders.new_data, dry_run, &mut report);
    for old in &leading {
        move_postgres_caches(&config_dir, old, &folders.new_data, dry_run, &mut report);
    }
    report
}

/// The earlier names whose data folder leads to the new one: moved, an
/// alias that reaches it, or gone with the new one in place. In a dry run,
/// which moved nothing, also what is about to move and every alias of it.
fn leading_to_new_data<'a>(
    folders: &'a Folders,
    plans: &[Plan<'_>],
    dry_run: bool,
) -> Vec<&'a OldFolders> {
    let family: Vec<&Path> = std::iter::once(folders.new_data.as_path())
        .chain(folders.old.iter().map(|f| f.data.as_path()))
        .collect();
    let moving: Vec<&Path> = plans
        .iter()
        .filter(|plan| plan.slot.holds_vault)
        .flat_map(|plan| plan.moves.iter().map(|&(_, old)| old))
        .collect();
    folders
        .old
        .iter()
        .filter(|f| match pair(&f.data, &folders.new_data, &family) {
            Pair::Done => true,
            Pair::Pending | Pair::Alias => {
                dry_run && moving.iter().any(|m| *m == f.data.as_path() || same_place(&f.data, m))
            }
            Pair::Nothing | Pair::Conflict => false,
        })
        .collect()
}

/// Where one earlier name's folder stands against the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pair {
    /// No old folder: a fresh install, or a name this machine never had.
    Nothing,
    /// A link to another of the family's folders that does not reach the
    /// new one yet -- the folder it names has not moved. Never moved itself.
    Alias,
    /// Carried: the old path leads to the new folder (the link a move leaves,
    /// or an alias through one), or is gone with the new folder in place.
    Done,
    /// A real folder, or a link somewhere outside the family, with the new
    /// one absent or empty: to be moved.
    Pending,
    /// Both hold files. Left alone.
    Conflict,
}

/// `family` is every name the folder has had, `new` among them.
fn pair(old: &Path, new: &Path, family: &[&Path]) -> Pair {
    if !old.exists() && !is_link(old) {
        return if new.exists() { Pair::Done } else { Pair::Nothing };
    }
    if same_place(old, new) {
        return Pair::Done;
    }
    if is_alias(old, family) {
        return Pair::Alias;
    }
    if has_content(new) { Pair::Conflict } else { Pair::Pending }
}

fn is_link(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
}

/// Do both paths lead to the same folder, through whatever links?
fn same_place(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Is `old` a link naming another of `family`'s folders? Judged by the
/// link's own target, one step, rather than by where a chain of links ends:
/// `everyday -> karmayoga` is an alias whether `karmayoga` is the folder, a
/// link to `jatayu`, or a link to another disk that is about to move.
fn is_alias(old: &Path, family: &[&Path]) -> bool {
    let Ok(target) = std::fs::read_link(old) else { return false };
    let target = match old.parent() {
        Some(parent) if target.is_relative() => parent.join(target),
        _ => target,
    };
    let (target, own) = (spelled(&target), spelled(old));
    family.iter().map(|name| spelled(name)).any(|name| name != own && name == target)
}

/// `path` with its parent folder resolved, so two spellings of one name
/// compare equal -- a home folder reached through a link, a `..` in a
/// relative link -- without following a link at the name itself.
fn spelled(path: &Path) -> PathBuf {
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// A folder with at least one entry in it.
fn has_content(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn move_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.is_dir() && !is_link(to) {
        // Empty: the plan said so. Out of the way, or the rename fails.
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

/// Rewrite the last-vault pointer if it names a path inside one of the old
/// data folders that lead to the new one.
fn repoint(
    config_dir: &Path,
    leading: &[&OldFolders],
    new_data: &Path,
    dry_run: bool,
    report: &mut Report,
) {
    let file = config_dir.join("last-vault");
    let Some(recorded) = crate::read_pointer(&file) else { return };
    let Some((product, rest)) =
        leading.iter().find_map(|old| Some((old.product, recorded.strip_prefix(&old.data).ok()?)))
    else {
        return;
    };
    let moved =
        if rest.as_os_str().is_empty() { new_data.to_path_buf() } else { new_data.join(rest) };
    report.carrying(product);
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
/// moved with it. Asked for each earlier name in turn, newest first: a cache
/// already at the new name is never overwritten by an older one.
fn move_postgres_caches(
    config_dir: &Path,
    old: &OldFolders,
    new_data: &Path,
    dry_run: bool,
    report: &mut Report,
) {
    // The new folder's header once moved; the old one's in a dry run.
    let header = [new_data, old.data.as_path()]
        .map(|dir| dir.join("vault.json"))
        .into_iter()
        .find(|h| h.exists());
    let is_postgres = header
        .and_then(|header| std::fs::read(header).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|v| v.get("backend").and_then(|b| b.as_str()) == Some("postgres"));
    if !is_postgres {
        return;
    }
    let caches = [
        ("chat", chat_dir_name(&old.data), chat_dir_name(new_data)),
        ("mail-index", index_dir_name(&old.data), index_dir_name(new_data)),
    ];
    for (kind, from, to) in caches {
        let (from, to) = (config_dir.join(kind).join(from), config_dir.join(kind).join(to));
        if !from.is_dir() || to.exists() {
            continue;
        }
        report.carrying(old.product);
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
    jatayu_core::fsutil::stable_path_name(vault)
}

fn index_dir_name(vault: &Path) -> String {
    jatayu_core::fsutil::cache_path_name(vault)
}

/// What is using any of `paths`, if anything is: an old build still running,
/// under any earlier name, or any process with a file open beneath one of
/// them. Only Linux can be asked; elsewhere the vault's lock is the whole of
/// the check.
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

/// An old build of the app or the command line, under any earlier name,
/// still running.
#[cfg(target_os = "linux")]
fn old_build_running() -> Option<String> {
    other_processes().find_map(|(pid, proc)| {
        let exe = std::fs::read_link(proc.join("exe")).ok()?;
        old_build(&exe).map(|former| still_running(former, pid))
    })
}

/// The earlier name an executable is a build of, if it is one: the app or
/// the command line as any earlier name built it. Judged by the file's name,
/// which is all `/proc/<pid>/exe` says -- with ` (deleted)` on the end once
/// the binary has been rebuilt or removed under a process still running it.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn old_build(exe: &Path) -> Option<&'static old::Former> {
    let name = exe.file_name()?.to_str()?;
    let name = name.strip_suffix(" (deleted)").unwrap_or(name);
    old::FORMERS.into_iter().find(|former| {
        [former.app_binary, former.cli_binary]
            .iter()
            .any(|binary| !binary.is_empty() && *binary == name)
    })
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn still_running(former: &old::Former, pid: u32) -> String {
    format!("{} is still running (process {pid}); quit it first", former.product)
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

// Unix only: what is carried is mostly links, which Windows makes only with
// privileges a test does not have.
#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use jatayu_core::legacy::{EVERY_DAY, KARMA_YOGA};

    /// Both earlier names' folders and Jatayu's under `root`, laid out as on
    /// Linux. Nothing is made on disk.
    pub(crate) fn folders(root: &Path) -> Folders {
        Folders {
            old: vec![
                OldFolders {
                    product: KARMA_YOGA.product,
                    config: root.join("config/karmayoga"),
                    data: root.join("share/karmayoga"),
                },
                OldFolders {
                    product: EVERY_DAY.product,
                    config: root.join("config/everyday"),
                    data: root.join("share/everyday"),
                },
            ],
            new_config: root.join("config/jatayu"),
            new_data: root.join("share/jatayu"),
        }
    }

    fn karma_yoga(f: &Folders) -> &OldFolders {
        &f.old[0]
    }

    fn every_day(f: &Folders) -> &OldFolders {
        &f.old[1]
    }

    fn idle(_: &[&Path]) -> Option<String> {
        None
    }

    fn link(target: &Path, at: &Path) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    /// An installation under one earlier name: settings, a pointer to the
    /// vault in its default folder, and a vault with a database in it.
    fn install(old: &OldFolders, database: &str) {
        std::fs::create_dir_all(&old.config).unwrap();
        std::fs::create_dir_all(old.data.join("store")).unwrap();
        std::fs::write(old.config.join("mcp.json"), b"{\"token\":\"t\"}").unwrap();
        std::fs::write(old.config.join("last-vault"), old.data.to_string_lossy().as_bytes())
            .unwrap();
        std::fs::write(old.data.join("vault.json"), b"{\"backend\":\"sqlite\"}").unwrap();
        std::fs::write(old.data.join("store").join(database), b"the records").unwrap();
    }

    /// The machine this was written for: Karma Yoga's real folders, with
    /// what the first rename left at Every Day's paths -- absolute links to
    /// them, and `store/everyday.db` a relative link to `karmayoga.db` -- and
    /// that rename's webview marker in the settings.
    pub(crate) fn owners_install(f: &Folders) {
        let (ky, ed) = (karma_yoga(f), every_day(f));
        install(ky, "karmayoga.db");
        std::fs::write(ky.config.join("webview-carried-over"), b"").unwrap();
        std::os::unix::fs::symlink("karmayoga.db", ky.data.join("store/everyday.db")).unwrap();
        link(&ky.config, &ed.config);
        link(&ky.data, &ed.data);
    }

    fn read_link(path: &Path) -> PathBuf {
        std::fs::read_link(path).unwrap_or_else(|e| panic!("{} is not a link: {e}", path.display()))
    }

    fn is_real_dir(path: &Path) -> bool {
        path.symlink_metadata().is_ok_and(|m| m.is_dir())
    }

    fn olds(f: &Folders, config: bool) -> Vec<PathBuf> {
        f.old.iter().map(|o| if config { o.config.clone() } else { o.data.clone() }).collect()
    }

    #[test]
    fn the_owners_install_moves_from_karma_yoga_and_every_days_links_still_lead_there() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let (ky, ed) = (karma_yoga(&f), every_day(&f));

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert_eq!(report.from, [KARMA_YOGA.product]);
        assert_eq!(report.names(), "Karma Yoga");
        // Two folders moved, a link left at each, and the pointer moved.
        assert_eq!(report.steps.len(), 5, "{:?}", report.steps);
        assert!(report.steps[0].starts_with("move the Karma Yoga settings folder"));

        // The folders themselves are under the new names, not links to the old.
        assert!(is_real_dir(&f.new_config) && is_real_dir(&f.new_data));
        assert_eq!(std::fs::read(f.new_config.join("mcp.json")).unwrap(), b"{\"token\":\"t\"}");
        assert!(f.new_config.join("webview-carried-over").exists());
        assert_eq!(std::fs::read(f.new_data.join("store/karmayoga.db")).unwrap(), b"the records");
        // The database is renamed by the store when it is opened, not here.
        assert_eq!(read_link(&f.new_data.join("store/everyday.db")), Path::new("karmayoga.db"));
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(f.new_data.clone()));

        // Karma Yoga's paths are links to the new folders now; Every Day's
        // are the links they were, and lead there through Karma Yoga's.
        assert_eq!(read_link(&ky.config), f.new_config);
        assert_eq!(read_link(&ky.data), f.new_data);
        assert_eq!(read_link(&ed.config), ky.config);
        assert_eq!(read_link(&ed.data), ky.data);
        assert!(same_place(&ed.config, &f.new_config) && same_place(&ed.data, &f.new_data));
        assert_eq!(std::fs::read(ed.data.join("store/everyday.db")).unwrap(), b"the records");

        // Nothing left to do, and the new folders are the ones to use.
        assert_eq!(migrate(&f, &idle, false), Report::default());
        assert!(settings_moved_in(&f));
        assert_eq!(resolve(f.new_config.clone(), &olds(&f, true)), f.new_config);
        assert_eq!(resolve(f.new_data.clone(), &olds(&f, false)), f.new_data);
    }

    #[test]
    fn an_every_day_install_moves_straight_to_jatayu() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        install(ed, "everyday.db");

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert_eq!(report.from, [EVERY_DAY.product]);
        assert!(report.steps[0].starts_with("move the Every Day settings folder"));
        assert_eq!(std::fs::read(f.new_data.join("store/everyday.db")).unwrap(), b"the records");
        assert_eq!(read_link(&ed.config), f.new_config);
        assert_eq!(read_link(&ed.data), f.new_data);
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(f.new_data.clone()));
        // Never by way of Karma Yoga: nothing is made under its name.
        assert!(ky.config.symlink_metadata().is_err() && ky.data.symlink_metadata().is_err());
        assert_eq!(migrate(&f, &idle, false), Report::default());
    }

    #[test]
    fn a_fresh_machine_has_nothing_to_carry() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        assert_eq!(migrate(&f, &idle, false), Report::default());
        assert_eq!(migrate(&f, &idle, true), Report::default());
        assert_eq!(resolve(f.new_data.clone(), &olds(&f, false)), f.new_data);
        assert!(settings_moved_in(&f));
    }

    #[test]
    fn old_builds_are_known_under_every_earlier_name() {
        let named = |exe: &str| old_build(Path::new(exe)).map(|f| f.product);
        let release = "/home/someone/.cache/target/release";
        assert_eq!(named(&format!("{release}/karmayoga-app")), Some("Karma Yoga"));
        assert_eq!(named(&format!("{release}/karmayoga-app (deleted)")), Some("Karma Yoga"));
        assert_eq!(named("/usr/local/bin/karmayoga"), Some("Karma Yoga"));
        assert_eq!(named("/opt/everyday-app"), Some("Every Day"));
        assert_eq!(named("/usr/bin/everyday"), Some("Every Day"));
        // This build, and things that only share a prefix, are not old builds.
        assert_eq!(named(&format!("{release}/jatayu-app")), None);
        assert_eq!(named("/usr/bin/jatayu"), None);
        assert_eq!(named(&format!("{release}/deps/karmayoga_vault-0123")), None);
        assert_eq!(named("/usr/bin/karmayoga-docs"), None);
    }

    #[test]
    fn karma_yoga_still_running_puts_the_whole_move_off() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        // What `/proc` showed on the owner's machine.
        let running = Path::new("/home/someone/.cache/target/release/karmayoga-app");
        let busy = |_: &[&Path]| old_build(running).map(|former| still_running(former, 3320150));
        let why = "Karma Yoga is still running (process 3320150); quit it first";

        let preview = migrate(&f, &busy, true);
        assert_eq!(preview.put_off.as_deref(), Some(why));
        assert_eq!(preview.from, [KARMA_YOGA.product]);
        // Two moves and the pointer, said but not done.
        assert_eq!(preview.steps.len(), 3, "a dry run still lists the steps: {preview:?}");

        let report = migrate(&f, &busy, false);
        assert_eq!(report.put_off.as_deref(), Some(why));
        assert!(report.steps.is_empty());
        assert!(!f.new_config.exists() && !f.new_data.exists());
        assert!(is_real_dir(&ky.config) && is_real_dir(&ky.data));
        assert_eq!(read_link(&ed.config), ky.config);
        // Until it is moved, Karma Yoga's folders are the ones to use --
        // never Every Day's links to them -- and the settings have not moved.
        assert_eq!(resolve(f.new_config.clone(), &olds(&f, true)), ky.config);
        assert_eq!(resolve(f.new_data.clone(), &olds(&f, false)), ky.data);
        assert!(!settings_moved_in(&f));
    }

    #[test]
    fn a_dry_run_reports_the_steps_and_takes_none() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let ky = karma_yoga(&f);

        let report = migrate(&f, &idle, true);
        assert_eq!(report.put_off, None);
        assert_eq!(report.from, [KARMA_YOGA.product]);
        assert_eq!(report.steps.len(), 3, "{:?}", report.steps);
        assert!(report.steps[2].contains("point the last vault"), "{:?}", report.steps);
        assert!(is_real_dir(&ky.config) && !f.new_config.exists());
        assert!(is_real_dir(&ky.data) && !f.new_data.exists());
        assert_eq!(crate::read_pointer(&ky.config.join("last-vault")), Some(ky.data.clone()));
    }

    /// macOS: each name's settings and data are one folder
    /// (`~/Library/Application Support/<id>`), moved once.
    #[test]
    fn one_folder_for_settings_and_data_moves_once() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("Application Support");
        let ky = support.join("app.Karma-Yoga.KarmaYoga");
        let ed = support.join("app.Every-Day.EveryDay");
        let new = support.join("app.Jatayu-Vault.Jatayu");
        let f = Folders {
            old: vec![
                OldFolders { product: KARMA_YOGA.product, config: ky.clone(), data: ky.clone() },
                OldFolders { product: EVERY_DAY.product, config: ed.clone(), data: ed.clone() },
            ],
            new_config: new.clone(),
            new_data: new.clone(),
        };
        std::fs::create_dir_all(ky.join("store")).unwrap();
        std::fs::write(ky.join("vault.json"), b"{\"backend\":\"sqlite\"}").unwrap();
        std::fs::write(ky.join("store/karmayoga.db"), b"the records").unwrap();
        std::fs::write(ky.join("last-vault"), ky.to_string_lossy().as_bytes()).unwrap();
        link(&ky, &ed);

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        let moves: Vec<_> = report.steps.iter().filter(|s| s.starts_with("move the")).collect();
        assert_eq!(moves.len(), 1, "{:?}", report.steps);
        assert!(moves[0].starts_with("move the Karma Yoga settings and data folder"));
        assert_eq!(read_link(&ky), new);
        assert_eq!(read_link(&ed), ky);
        assert_eq!(std::fs::read(new.join("store/karmayoga.db")).unwrap(), b"the records");
        assert_eq!(crate::read_pointer(&new.join("last-vault")), Some(new.clone()));
        assert_eq!(migrate(&f, &idle, false), Report::default());

        // Nothing in it at all: still one move, and no folder that links to
        // itself.
        let empty = tempfile::tempdir().unwrap();
        let old = empty.path().join("old");
        let new = empty.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        let f = Folders {
            old: vec![OldFolders {
                product: KARMA_YOGA.product,
                config: old.clone(),
                data: old.clone(),
            }],
            new_config: new.clone(),
            new_data: new.clone(),
        };
        assert_eq!(migrate(&f, &idle, false).put_off, None);
        assert!(is_real_dir(&new));
        assert_eq!(read_link(&old), new);
    }

    #[test]
    fn an_open_vault_puts_the_move_off() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let ky = karma_yoga(&f);
        let held = jatayu_core::lockfile::acquire(&ky.data).unwrap();
        assert!(held.is_some());

        let report = migrate(&f, &idle, false);
        assert!(
            report.put_off.as_deref().unwrap_or("").contains("open in another program"),
            "{report:?}"
        );
        assert!(is_real_dir(&ky.data) && !f.new_data.exists());
    }

    #[test]
    fn a_new_folder_with_files_in_it_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        std::fs::create_dir_all(&f.new_data).unwrap();
        std::fs::write(f.new_data.join("something"), b"x").unwrap();

        let report = migrate(&f, &idle, false);
        let why = report.put_off.as_deref().unwrap_or("");
        assert!(
            why.contains("the Karma Yoga data folder and the new one both hold files"),
            "{why}"
        );
        // Every Day's link is an alias, not a second folder in the way.
        assert!(!why.contains("Every Day"), "{why}");
        // The settings still moved; the data did not, and neither did the
        // link to it.
        assert_eq!(read_link(&ky.config), f.new_config);
        assert!(is_real_dir(&ky.data) && ky.data.join("store/karmayoga.db").exists());
        assert_eq!(read_link(&ed.data), ky.data);
        assert!(f.new_data.join("something").exists());
        // Where there is a conflict the new folder is the one used.
        assert_eq!(resolve(f.new_data.clone(), &olds(&f, false)), f.new_data);
    }

    #[test]
    fn an_older_folder_beside_a_newer_one_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        // Both real: the first rename found Karma Yoga's folders full and
        // left Every Day's where they were.
        install(ky, "karmayoga.db");
        install(ed, "everyday.db");

        let preview = migrate(&f, &idle, true);
        let moves: Vec<_> = preview.steps.iter().filter(|s| s.starts_with("move the")).collect();
        assert_eq!(moves.len(), 2, "one per folder, both Karma Yoga's: {:?}", preview.steps);
        assert!(moves.iter().all(|s| s.contains("Karma Yoga")));
        assert!(preview.put_off.as_deref().unwrap_or("").contains("the Every Day settings folder"));

        let report = migrate(&f, &idle, false);
        assert_eq!(report.from, [KARMA_YOGA.product]);
        assert_eq!(read_link(&ky.data), f.new_data);
        assert!(is_real_dir(&ed.data) && ed.data.join("store/everyday.db").exists());
        assert_eq!(std::fs::read(f.new_data.join("store/karmayoga.db")).unwrap(), b"the records");
        // The settings are read from the new folder, so they count as moved.
        assert!(settings_moved_in(&f));
    }

    #[test]
    fn an_empty_newer_folder_gives_way_to_an_older_one_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        install(ed, "everyday.db");
        std::fs::create_dir_all(&ky.config).unwrap();
        std::fs::create_dir_all(&ky.data).unwrap();
        // While the move waits, the folder with something in it is used.
        assert_eq!(resolve(f.new_data.clone(), &olds(&f, false)), ed.data);

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert_eq!(std::fs::read(f.new_data.join("store/everyday.db")).unwrap(), b"the records");
        assert_eq!(read_link(&ky.data), f.new_data);
        assert_eq!(read_link(&ed.data), f.new_data);
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(f.new_data.clone()));
    }

    #[test]
    fn an_empty_new_folder_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        std::fs::create_dir_all(&f.new_config).unwrap();
        std::fs::create_dir_all(&f.new_data).unwrap();
        // Until it is moved, an empty new folder does not hide the old one.
        assert_eq!(resolve(f.new_data.clone(), &olds(&f, false)), karma_yoga(&f).data);

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert!(f.new_data.join("vault.json").exists());
        assert_eq!(read_link(&karma_yoga(&f).data), f.new_data);
    }

    #[test]
    fn a_link_somewhere_else_is_moved_like_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        // The vault kept on another disk, Karma Yoga's path a link to it, and
        // Every Day's a link to Karma Yoga's.
        let disk = dir.path().join("disk/Journal");
        install(&OldFolders { data: disk.clone(), ..ky.clone() }, "karmayoga.db");
        link(&disk, &ky.data);
        link(&ky.data, &ed.data);

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        assert_eq!(read_link(&f.new_data), disk);
        assert_eq!(read_link(&ky.data), f.new_data);
        assert_eq!(read_link(&ed.data), ky.data);
        assert!(is_real_dir(&disk));
        assert_eq!(migrate(&f, &idle, false), Report::default());
    }

    #[test]
    fn a_pointer_to_a_vault_elsewhere_is_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let elsewhere = dir.path().join("external/Journal");
        std::fs::write(
            karma_yoga(&f).config.join("last-vault"),
            elsewhere.to_string_lossy().as_bytes(),
        )
        .unwrap();

        migrate(&f, &idle, false);
        assert_eq!(crate::read_pointer(&f.new_config.join("last-vault")), Some(elsewhere));
    }

    #[test]
    fn a_pointer_written_through_a_link_after_the_move_is_fixed_next_time() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        migrate(&f, &idle, false);
        let pointer = f.new_config.join("last-vault");
        // An old build, run once more, records its vault by its own path...
        for old in [karma_yoga(&f), every_day(&f)] {
            std::fs::write(old.config.join("last-vault"), old.data.to_string_lossy().as_bytes())
                .unwrap();
            let report = migrate(&f, &idle, false);
            assert_eq!(report.from, [old.product], "{report:?}");
            assert_eq!(crate::read_pointer(&pointer), Some(f.new_data.clone()));
        }
        // ...or by a path inside it.
        let inside = every_day(&f).data.join("nested");
        std::fs::write(
            every_day(&f).config.join("last-vault"),
            inside.to_string_lossy().as_bytes(),
        )
        .unwrap();
        migrate(&f, &idle, false);
        assert_eq!(crate::read_pointer(&pointer), Some(f.new_data.join("nested")));
    }

    #[test]
    fn a_postgres_vaults_caches_follow_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let (ky, ed) = (karma_yoga(&f), every_day(&f));
        std::fs::write(ky.data.join("vault.json"), b"{\"backend\":\"postgres\"}").unwrap();
        let cache = |old: &OldFolders| ky.config.join("chat").join(chat_dir_name(&old.data));
        // Karma Yoga's caches, and a stale one an Every Day build left.
        for old in [ky, ed] {
            std::fs::create_dir_all(cache(old)).unwrap();
            std::fs::write(cache(old).join("matrix.sqlite3"), old.product.as_bytes()).unwrap();
        }
        std::fs::create_dir_all(ky.config.join("mail-index").join(index_dir_name(&ky.data)))
            .unwrap();

        let report = migrate(&f, &idle, false);
        assert_eq!(report.put_off, None, "{report:?}");
        let moved = f.new_config.join("chat").join(chat_dir_name(&f.new_data));
        // The newest name's cache is the one that follows; an older one never
        // overwrites it.
        assert_eq!(std::fs::read(moved.join("matrix.sqlite3")).unwrap(), b"Karma Yoga");
        assert!(f.new_config.join("chat").join(chat_dir_name(&ed.data)).is_dir());
        assert!(f.new_config.join("mail-index").join(index_dir_name(&f.new_data)).is_dir());
    }

    #[test]
    fn names_read_as_a_sentence() {
        assert_eq!(former_names(), "Karma Yoga or Every Day");
        let both = Report { from: vec!["Karma Yoga", "Every Day"], ..Report::default() };
        assert_eq!(both.names(), "Karma Yoga and Every Day");
        assert_eq!(Report::default().names(), "");
        let mut report = Report::default();
        report.carrying(EVERY_DAY.product);
        report.carrying(KARMA_YOGA.product);
        report.carrying(EVERY_DAY.product);
        assert_eq!(report.from, ["Karma Yoga", "Every Day"], "newest first, once each");
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
