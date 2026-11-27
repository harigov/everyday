//! Noticing changes to the files folder as they happen, with `notify`
//! (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows).
//!
//! The watcher does not interpret events: any change under the folder, bar
//! [`jatayu_core::files::INTERNAL_DIR`], wakes the task
//! ([`super::task::wake`]), which runs a pass a few seconds later -- by
//! which time a sync tool's burst of writes has settled. A folder the
//! platform cannot watch (some network and FUSE mounts) is simply not
//! watched; the periodic pass still runs.

use std::path::Path;

use jatayu_core::files::{INTERNAL_DIR, disk};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// A running watch. Dropping it stops watching.
pub struct Watch {
    _watcher: RecommendedWatcher,
}

/// Watch `root` recursively. `None` when it cannot be watched.
pub fn start(root: &Path) -> Option<Watch> {
    let base = root.to_path_buf();
    let handler = move |event: notify::Result<notify::Event>| match event {
        Ok(event) => {
            if matters(&base, &event) {
                super::task::wake();
            }
        }
        // The platform lost track -- a queue overflowed -- and what changed
        // is anybody's guess: a whole pass is the answer.
        Err(_) => super::task::wake(),
    };
    let mut watcher = match notify::recommended_watcher(handler) {
        Ok(watcher) => watcher,
        Err(e) => {
            tracing::debug!(error = %e, "files: the folder cannot be watched");
            return None;
        }
    };
    // Recursive, so a folder made later is watched too. A tree with more
    // folders than the system allows watches for fails here, and is left to
    // the periodic pass.
    if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
        tracing::debug!(folder = %root.display(), error = %e, "files: the folder cannot be watched");
        return None;
    }
    Some(Watch { _watcher: watcher })
}

/// Whether `event` is a change the catalog could care about: not somebody
/// merely reading a file, and not about Jatayu's own folder, the sync
/// tools' scratch files or the operating system's litter -- the names a pass
/// leaves out anyway, and a sync in progress writes a great many of them.
fn matters(root: &Path, event: &notify::Event) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    // No path at all is the platform saying only that something happened.
    event.paths.is_empty() || event.paths.iter().any(|path| !left_out(root, path))
}

/// Whether `path`, somewhere under `root`, is under a name a pass skips.
fn left_out(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative.components().any(|part| {
        part.as_os_str().to_str().is_some_and(|name| name == INTERNAL_DIR || disk::is_skipped(name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};
    use std::path::PathBuf;

    fn event(kind: EventKind, paths: &[&str]) -> notify::Event {
        let mut event = notify::Event::new(kind);
        for path in paths {
            event = event.add_path(PathBuf::from(path));
        }
        event
    }

    #[test]
    fn a_change_to_a_file_wakes_the_task() {
        let root = Path::new("/home/me/Jatayu");
        let created = event(EventKind::Create(CreateKind::File), &["/home/me/Jatayu/Photos/a.jpg"]);
        assert!(matters(root, &created));
        let renamed = event(
            EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)),
            &["/home/me/Jatayu/a.jpg", "/home/me/Jatayu/Trips/a.jpg"],
        );
        assert!(matters(root, &renamed));
    }

    #[test]
    fn the_internal_folder_and_what_is_only_read_do_not() {
        let root = Path::new("/home/me/Jatayu");
        let trash =
            event(EventKind::Create(CreateKind::File), &["/home/me/Jatayu/.jatayu/trash/x/a.jpg"]);
        assert!(!matters(root, &trash));
        let read = event(EventKind::Access(AccessKind::Read), &["/home/me/Jatayu/a.jpg"]);
        assert!(!matters(root, &read));
    }

    #[test]
    fn a_move_out_of_the_internal_folder_still_does() {
        // Restoring a file from the trash: one end is inside the folder
        // Jatayu keeps for itself, the other is where it came back to.
        let root = Path::new("/home/me/Jatayu");
        let restored = event(
            EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)),
            &["/home/me/Jatayu/.jatayu/trash/x/a.jpg", "/home/me/Jatayu/a.jpg"],
        );
        assert!(matters(root, &restored));
    }

    #[test]
    fn an_event_that_names_nothing_is_taken_for_news() {
        let root = Path::new("/home/me/Jatayu");
        assert!(matters(root, &event(EventKind::Other, &[])));
    }
}
