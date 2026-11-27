//! Scratch folders a build under a former name left in the cache or the
//! temporary folder, found by their names so they can be cleared.
//!
//! Each kind of scratch this crate makes -- a browser session's profile, the
//! documents' parsed text and index, recordings decrypted to be transcribed,
//! podcast episodes part-way down -- sits in a folder whose name starts with
//! the application's own (`jatayu-docs-…`). A build under a former name made
//! the same folders under that name instead
//! ([`jatayu_core::legacy::Former::scratch_prefixes`]), and nothing of
//! today's would ever look for them. Some hold decrypted copies, and none is
//! worth carrying across -- each is made again under today's name the first
//! time it is wanted -- so they are deleted, not moved.

use std::path::Path;

/// The name the current build's scratch begins with, which every prefix
/// [`former_prefixes`] is asked about starts with.
const CURRENT: &str = "jatayu-";

/// What each former name called the scratch `current` names today, newest
/// first: `jatayu-docs-` gives `karmayoga-docs-`, `jatayu-browser-` gives
/// `karmayoga-browser-` and `everyday-browser-`. Only names a former build
/// actually made are given, from its own list, never one built here.
pub(crate) fn former_prefixes(current: &str) -> impl Iterator<Item = &'static str> {
    let kind = current.strip_prefix(CURRENT).unwrap_or(current);
    jatayu_core::legacy::FORMERS
        .iter()
        .flat_map(|former| former.scratch_prefixes.iter().copied())
        .filter(move |prefix| prefix.split_once('-').is_some_and(|(_, rest)| rest == kind))
}

/// Remove everything directly in `base` whose name begins with what a former
/// name called `current`: the folder (or file) itself, with all it holds.
/// Answers how many went. A folder that cannot be removed is left, and tried
/// again the next time; nothing else in `base` is touched.
///
/// How long a former name's scratch must have gone untouched before it is
/// cleared: long enough that a running old build is not still writing it.
const IDLE_BEFORE_CLEARING: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Whether `path` was written within [`IDLE_BEFORE_CLEARING`]: its own time,
/// or for a folder the newest of what is directly in it. A time that cannot
/// be read counts as recent -- left for a later sweep, never cleared blind.
fn recently_touched(path: &Path) -> bool {
    let recent = |meta: std::io::Result<std::fs::Metadata>| {
        meta.and_then(|m| m.modified())
            .map(|at| at.elapsed().map_or(true, |age| age < IDLE_BEFORE_CLEARING))
            .unwrap_or(true)
    };
    // A link holds nothing anybody is writing: it goes as the link it is.
    if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    if recent(path.symlink_metadata()) {
        return true;
    }
    match std::fs::read_dir(path) {
        Ok(inside) => inside.flatten().any(|entry| recent(entry.metadata())),
        Err(_) => false,
    }
}

/// Only for scratch no running copy can still be using. A build under a
/// former name may still be running -- this one starts beside it, on the old
/// folders, when the carry-over is put off -- so nothing is cleared while one
/// is ([`jatayu_vault::legacy::in_use`], on Linux), and on every platform
/// only what has not been touched for an hour ([`IDLE_BEFORE_CLEARING`]): an
/// old build indexing a document, transcribing a recording or downloading an
/// episode is writing to its scratch as it goes.
pub(crate) fn remove_former(base: &Path, current: &str) -> usize {
    let prefixes: Vec<&str> = former_prefixes(current).collect();
    if prefixes.is_empty() {
        return 0;
    }
    if let Some(why) = jatayu_vault::legacy::in_use(&[]) {
        tracing::debug!("left a former name's scratch for now: {why}");
        return 0;
    }
    clear(base, &prefixes)
}

/// [`remove_former`] for a caller that has already made sure no build under
/// a former name is running.
pub(crate) fn remove_former_idle(base: &Path, current: &str) -> usize {
    let prefixes: Vec<&str> = former_prefixes(current).collect();
    clear(base, &prefixes)
}

/// The same scratch under a former name: `current`'s file name with its
/// prefix `kind` (`jatayu-docs-`) swapped for each former prefix of that kind,
/// newest first -- a cache named for a path that did not move.
pub(crate) fn former_names_of(current: &Path, kind: &str) -> Vec<std::path::PathBuf> {
    let (Some(dir), Some(name)) = (current.parent(), current.file_name().and_then(|n| n.to_str()))
    else {
        return Vec::new();
    };
    let Some(rest) = name.strip_prefix(kind) else { return Vec::new() };
    former_prefixes(kind).map(|prefix| dir.join(format!("{prefix}{rest}"))).collect()
}

/// Clear what in `base` begins with one of `prefixes` and has sat idle, now:
/// [`remove_former`] once it knows no former build is running.
fn clear(base: &Path, prefixes: &[&str]) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        if recently_touched(&entry.path()) {
            continue;
        }
        // Not followed through a link: a link named like scratch is removed
        // as a link, and what it points at stays where it is.
        let path = entry.path();
        let gone = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        match gone {
            Ok(()) => removed += 1,
            Err(e) => tracing::debug!(
                path = %path.display(),
                error = %e,
                "could not clear a former name's scratch"
            ),
        }
    }
    if removed > 0 {
        tracing::info!(removed, base = %base.display(), "cleared scratch a build under a former name left");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`remove_former`] without the check for a running old build -- the
    /// machine running these tests may well have one -- as it acts once
    /// there is none.
    fn sweep(base: &Path, current: &str) -> usize {
        let prefixes: Vec<&str> = former_prefixes(current).collect();
        clear(base, &prefixes)
    }

    /// Make `path`, and what is directly in it, look untouched for two hours.
    fn age(path: &Path) {
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
        let set = |p: &Path| std::fs::File::open(p).unwrap().set_modified(then).unwrap();
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            set(&entry.path());
        }
        set(path);
    }

    #[test]
    fn each_kind_of_scratch_has_its_former_names() {
        let of = |current| former_prefixes(current).collect::<Vec<_>>();
        assert_eq!(of("jatayu-browser-"), ["karmayoga-browser-", "everyday-browser-"]);
        assert_eq!(of("jatayu-docs-"), ["karmayoga-docs-"]);
        assert_eq!(of("jatayu-transcripts"), ["karmayoga-transcripts"]);
        assert_eq!(of("jatayu-downloads"), ["karmayoga-downloads"]);
        assert!(of("jatayu-image-").is_empty(), "no former build made one");
    }

    /// The owner's cache folder, as Karma Yoga left it, beside things that
    /// are not scratch of that kind: today's, another kind, a folder of
    /// somebody else's whose name merely begins with a former name.
    #[test]
    fn only_a_former_names_scratch_of_that_kind_is_removed() {
        let base = tempfile::tempdir().unwrap();
        let docs = base.path().join("karmayoga-docs-4189b2fcfb133b6f");
        std::fs::create_dir_all(docs.join("index")).unwrap();
        std::fs::create_dir_all(docs.join("text")).unwrap();
        std::fs::write(docs.join("text/entry"), b"sealed").unwrap();
        let downloads = base.path().join("karmayoga-downloads");
        std::fs::create_dir(&downloads).unwrap();
        age(&docs);
        age(&downloads);
        let ours = base.path().join("jatayu-docs-0123456789abcdef");
        let build = base.path().join("everyday-mail-target-loose");
        let other = base.path().join("karmayoga-notes");
        for dir in [&ours, &build, &other] {
            std::fs::create_dir(dir).unwrap();
        }

        assert_eq!(sweep(base.path(), "jatayu-docs-"), 1);
        assert!(!docs.exists());
        assert!(downloads.exists(), "another kind, cleared by its own owner");
        assert!(ours.exists() && build.exists() && other.exists());

        assert_eq!(sweep(base.path(), "jatayu-downloads"), 1);
        assert!(!downloads.exists());
        assert_eq!(sweep(base.path(), "jatayu-downloads"), 0, "nothing left to clear");
        assert_eq!(sweep(&base.path().join("missing"), "jatayu-docs-"), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_named_like_scratch_goes_and_what_it_points_at_stays() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("keep"), b"not scratch").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("karmayoga-transcripts"))
            .unwrap();
        assert_eq!(sweep(base.path(), "jatayu-transcripts"), 1);
        assert!(!base.path().join("karmayoga-transcripts").exists());
        assert!(elsewhere.path().join("keep").is_file());
    }

    /// An old build still writing its scratch -- a download growing, a
    /// recording being transcribed -- keeps it: only an hour's idleness
    /// clears it.
    #[test]
    fn scratch_still_being_written_is_kept() {
        let base = tempfile::tempdir().unwrap();
        let downloads = base.path().join("karmayoga-downloads");
        std::fs::create_dir(&downloads).unwrap();
        std::fs::write(downloads.join("episode.part"), b"half").unwrap();
        assert_eq!(sweep(base.path(), "jatayu-downloads"), 0);
        assert!(downloads.join("episode.part").is_file());
        age(&downloads);
        assert_eq!(sweep(base.path(), "jatayu-downloads"), 1);
    }
}
