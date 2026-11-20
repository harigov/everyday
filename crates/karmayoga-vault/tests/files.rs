//! The rules the vault applies to files, over a real SQLite backend and a
//! real folder on disk: importing plain and sealed, names that never clash,
//! the disk and the catalog moving together, a folder that is not there being
//! unavailable and never empty, uploads, what the person adds, stacks and
//! albums, which records refer to which files, what the reconciler found, and
//! bringing the old attachments in.
//!
//! Every test points the files folder at a directory of its own inside a
//! temporary one. None may ever set the folder up at the default place: that
//! is `~/Karma Yoga` on whoever runs the tests.

mod support;
use support::vault;

use jiff::{SignedDuration, Timestamp};
use karmayoga_core::files::disk::UploadStaging;
use karmayoga_core::files::{
    ChangedFile, MovedFile, NewFile, RefKind, RefOwner, ScanChanges, StatusFilter,
};
use karmayoga_core::richtext::RichDoc;
use karmayoga_core::{
    Attachment, Conversation, Entry, Error, FileEntry, FileFilter, FileId, FileKind, FileOrigin,
    FileQuery, FileStatus, FileStorage, FileUpdate, FilesSettings, FolderState, ImportOptions,
    Journal, MediaInfo, MediaKind, Message, Note, Vault,
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// A vault, and a files folder beside it that is set up and empty.
struct World {
    dir: tempfile::TempDir,
    vault: Vault,
    folder: PathBuf,
}

impl World {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// A path inside the files folder.
    fn at(&self, rel: &str) -> PathBuf {
        self.folder.join(rel)
    }

    /// What the folder holds, as relative paths, leaving out the folder's own
    /// internal one.
    fn listing(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk(&self.folder, &self.folder, &mut out);
        out.sort();
        out
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        if rel.starts_with(".karmayoga") || rel == ".stignore" {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            out.push(rel);
        }
    }
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(&dir.path().join("vault"));
    let folder = dir.path().join("files");
    let state = vault.set_up_files_folder(Some(&folder)).unwrap();
    assert!(state.is_available(), "{state:?}");
    World { dir, vault, folder }
}

fn opts(origin: FileOrigin) -> ImportOptions {
    ImportOptions::new(origin)
}

fn sealed(origin: FileOrigin) -> ImportOptions {
    ImportOptions { encrypted: Some(true), ..ImportOptions::new(origin) }
}

fn into_dir(dir: &str) -> ImportOptions {
    ImportOptions { dir: Some(dir.into()), ..ImportOptions::new(FileOrigin::Files) }
}

/// A plain file added the way the Files app adds one.
fn import(w: &World, name: &str, bytes: &[u8]) -> FileEntry {
    w.vault.import_bytes(bytes, name, &opts(FileOrigin::Files)).unwrap()
}

fn invalid<T: std::fmt::Debug>(result: karmayoga_core::Result<T>) -> String {
    match result {
        Err(Error::Invalid(message)) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

/// The private place, outside the files folder, where a vault stages the
/// uploads that will be sealed.
fn staging(w: &World) -> UploadStaging {
    UploadStaging::for_vault(w.vault.path())
}

/// How many things are directly inside `dir`; none if it is not there.
fn count_in(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|read| read.count()).unwrap_or(0)
}

/// Make `path` look as if it was last touched `hours` ago.
fn backdate(path: &Path, hours: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600))
        .unwrap();
}

fn path_of(f: &FileEntry) -> &str {
    f.path().unwrap_or_else(|| panic!("{} is sealed", f.name))
}

fn ts(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn query(filter: FileFilter) -> FileQuery {
    FileQuery { filter, ..Default::default() }
}

fn ids(files: &[FileEntry]) -> Vec<FileId> {
    files.iter().map(|f| f.id).collect()
}

// ---- the folder and its state ----------------------------------------------

#[test]
fn a_vault_that_never_set_the_folder_up_says_so_and_suggests_one() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(vault.supports_files());
    assert!(vault.files_folder().unwrap().is_none());
    assert!(matches!(vault.folder_state().unwrap(), FolderState::NotSetUp { .. }));
    // Nothing is created by asking.
    assert!(vault.list_folder("").unwrap().folders.is_empty());
}

#[test]
fn setting_the_folder_up_writes_the_marker_and_remembers_the_place() {
    let w = world();
    assert!(w.at(".karmayoga").is_dir());
    assert_eq!(
        w.vault.files_settings().unwrap().folder.as_deref(),
        Some(w.folder.to_str().unwrap())
    );
    assert_eq!(
        w.vault.folder_state().unwrap(),
        FolderState::Available { path: w.folder.display().to_string() }
    );
    let relative = w.vault.set_up_files_folder(Some(Path::new("relative/folder")));
    assert!(invalid(relative).contains("absolute"));
}

#[test]
fn settings_are_checked_and_found_files_are_never_sealed() {
    let w = world();
    let mut settings = w.vault.files_settings().unwrap();
    settings.trash_days = 3651;
    assert!(invalid(w.vault.save_files_settings(&settings)).contains("3650"));

    let mut settings = w.vault.files_settings().unwrap();
    settings.folder = Some("not/absolute".into());
    assert!(invalid(w.vault.save_files_settings(&settings)).contains("absolute"));

    let settings = FilesSettings {
        folder: Some(w.folder.display().to_string()),
        encrypted: vec![FileOrigin::Chat, FileOrigin::Found, FileOrigin::Chat],
        trash_days: 7,
        ..Default::default()
    };
    let saved = w.vault.save_files_settings(&settings).unwrap();
    assert_eq!(saved.encrypted, vec![FileOrigin::Chat], "sorted, once each, no found files");
    assert_eq!(saved.trash_days, 7);
}

// ---- importing -------------------------------------------------------------

#[test]
fn a_file_is_copied_into_the_folder_and_catalogued() {
    let w = world();
    let outside = w.root().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("notes.txt"), b"remember the milk").unwrap();

    let f = w.vault.import_file(&outside.join("notes.txt"), &opts(FileOrigin::Files)).unwrap();
    assert_eq!(f.name, "notes.txt");
    assert_eq!(path_of(&f), "notes.txt", "a file added in the Files app lands at the top");
    assert_eq!(f.size, 17);
    assert_eq!(f.hash.len(), 64);
    assert_eq!(f.kind, FileKind::Document);
    assert_eq!(f.mime, "text/plain");
    assert_eq!(f.origin, FileOrigin::Files);
    assert_eq!(f.status, FileStatus::Present);
    assert_eq!(std::fs::read(w.at("notes.txt")).unwrap(), b"remember the milk");
    assert!(outside.join("notes.txt").exists(), "importing copies, it never moves");
    assert_eq!(w.vault.file(f.id).unwrap(), f);

    let usage = w.vault.files_usage().unwrap();
    assert_eq!((usage.files, usage.bytes, usage.sealed_files), (1, 17, 0));
}

#[test]
fn an_app_files_its_pictures_under_its_own_folder_by_month() {
    let w = world();
    let f = w.vault.import_bytes(b"x", "shot.png", &opts(FileOrigin::Journal)).unwrap();
    let parts: Vec<&str> = path_of(&f).split('/').collect();
    assert_eq!(parts.len(), 4, "Journal/<year>/<month>/<name>, got {parts:?}");
    assert_eq!(parts[0], "Journal");
    assert_eq!(parts[1].len(), 4);
    assert_eq!(parts[2].len(), 2);
    assert_eq!(parts[3], "shot.png");

    let g = w.vault.import_bytes(b"x", "elsewhere.png", &into_dir("Trips/Lisbon")).unwrap();
    assert_eq!(path_of(&g), "Trips/Lisbon/elsewhere.png");
}

#[test]
fn a_sealed_file_has_no_place_in_the_folder_and_reads_back() {
    let w = world();
    let f = w.vault.import_bytes(b"secret plans", "plans.txt", &sealed(FileOrigin::Files)).unwrap();
    assert!(f.is_sealed());
    assert!(f.path().is_none());
    assert_eq!(f.size, 12);
    assert!(w.listing().is_empty(), "nothing was written to the folder");
    assert_eq!(w.vault.file_len(f.id).unwrap(), 12);
    assert_eq!(w.vault.read_file_range(f.id, 0, 100).unwrap(), b"secret plans");
    assert!(invalid(w.vault.plain_path(f.id)).contains("sealed"));
    assert_eq!(w.vault.files_usage().unwrap().sealed_files, 1);
}

#[test]
fn the_settings_decide_which_apps_seal_their_files() {
    let w = world();
    let settings = FilesSettings {
        folder: Some(w.folder.display().to_string()),
        encrypted: vec![FileOrigin::Chat],
        ..Default::default()
    };
    w.vault.save_files_settings(&settings).unwrap();
    let chat = w.vault.import_bytes(b"a", "a.txt", &opts(FileOrigin::Chat)).unwrap();
    let mail = w.vault.import_bytes(b"b", "b.txt", &opts(FileOrigin::Mail)).unwrap();
    assert!(chat.is_sealed());
    assert!(!mail.is_sealed());
    // The caller can still say otherwise either way.
    let plain_chat = ImportOptions { encrypted: Some(false), ..opts(FileOrigin::Chat) };
    assert!(!w.vault.import_bytes(b"c", "c.txt", &plain_chat).unwrap().is_sealed());
}

#[test]
fn a_name_never_clashes_and_nothing_is_overwritten() {
    let w = world();
    let a = import(&w, "a.txt", b"one");
    let b = import(&w, "a.txt", b"two");
    let c = import(&w, "a.txt", b"three");
    assert_eq!([path_of(&a), path_of(&b), path_of(&c)], ["a.txt", "a (2).txt", "a (3).txt"]);
    assert_eq!(std::fs::read(w.at("a.txt")).unwrap(), b"one");
    assert_eq!(std::fs::read(w.at("a (2).txt")).unwrap(), b"two");
    assert_eq!(std::fs::read(w.at("a (3).txt")).unwrap(), b"three");
}

#[test]
fn the_path_of_a_missing_file_is_still_taken() {
    let w = world();
    let a = import(&w, "a.txt", b"one");
    std::fs::remove_file(w.at("a.txt")).unwrap();
    w.vault.apply_scan(&ScanChanges { vanished: vec![a.id], ..Default::default() }).unwrap();
    assert!(matches!(w.vault.file(a.id).unwrap().status, FileStatus::Missing { .. }));

    let b = import(&w, "a.txt", b"two");
    assert_eq!(path_of(&b), "a (2).txt", "the catalog would refuse a second record at a.txt");
}

#[test]
fn names_the_folder_would_ignore_are_refused() {
    let w = world();
    let hidden = w.vault.import_bytes(b"x", ".bashrc", &opts(FileOrigin::Files));
    assert!(invalid(hidden).contains("ignores"));
    let partial = w.vault.import_bytes(b"x", "movie.part", &opts(FileOrigin::Files));
    assert!(invalid(partial).contains("ignores"));
    let slash = w.vault.import_bytes(b"x", "a/b.txt", &opts(FileOrigin::Files));
    assert!(matches!(slash, Err(Error::Invalid(_))));
    let up = w.vault.import_bytes(b"x", "x.txt", &into_dir("../escape"));
    assert!(matches!(up, Err(Error::Invalid(_))));
    assert!(w.listing().is_empty());
}

// ---- reading ---------------------------------------------------------------

#[test]
fn a_range_is_read_the_same_from_a_plain_and_a_sealed_file() {
    let w = world();
    let bytes: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    let plain = import(&w, "bytes.bin", &bytes);
    let sealed_one =
        w.vault.import_bytes(&bytes, "sealed.bin", &sealed(FileOrigin::Files)).unwrap();
    for f in [&plain, &sealed_one] {
        assert_eq!(w.vault.file_len(f.id).unwrap(), 1000);
        assert_eq!(w.vault.read_file_range(f.id, 10, 20).unwrap(), bytes[10..30]);
        assert_eq!(w.vault.read_file_range(f.id, 990, 100).unwrap(), bytes[990..], "clamped");
        assert!(w.vault.read_file_range(f.id, 5000, 10).unwrap().is_empty());
    }
}

#[test]
fn text_is_read_up_to_a_limit_and_only_when_it_is_text() {
    let w = world();
    let words = import(&w, "words.txt", "h\u{e9}llo w\u{f6}rld".as_bytes());
    assert_eq!(w.vault.read_file_text(words.id, 1000).unwrap(), "h\u{e9}llo w\u{f6}rld");
    // Two bytes would end in the middle of the second one's character.
    assert_eq!(w.vault.read_file_text(words.id, 2).unwrap(), "h");

    let binary = import(&w, "binary.txt", b"abc\0def");
    assert!(invalid(w.vault.read_file_text(binary.id, 100)).contains("not a text file"));
    let picture = import(&w, "picture.png", b"\x89PNG\r\n\x1a\n");
    assert!(invalid(w.vault.read_file_text(picture.id, 100)).contains("not a text file"));
}

#[test]
fn a_plain_files_path_is_inside_the_folder() {
    let w = world();
    let f = import(&w, "here.txt", b"x");
    let path = w.vault.plain_path(f.id).unwrap();
    assert!(path.starts_with(&w.folder));
    assert_eq!(std::fs::read(path).unwrap(), b"x");
}

#[test]
fn a_thumbnail_and_what_the_file_says_about_itself_are_kept_beside_it() {
    let w = world();
    let f = import(&w, "photo.jpg", b"not really a jpeg");
    assert!(w.vault.file_thumbnail(f.id).unwrap().is_none());

    let media = MediaInfo {
        width: Some(4000),
        height: Some(3000),
        taken_at: Some(ts("2021-06-01T12:00:00Z")),
        camera_make: Some("Canon".into()),
        camera_model: Some("Canon EOS R5".into()),
        ..Default::default()
    };
    let saved = w
        .vault
        .save_file_derived(f.id, media.clone(), Some(b"jpeg thumbnail"), Some("image/jpeg"))
        .unwrap();
    assert_eq!(saved.media, media);
    assert_eq!(saved.kind, FileKind::Image);
    assert_eq!(saved.mime, "image/jpeg");
    assert_eq!(w.vault.file_thumbnail(f.id).unwrap().unwrap(), b"jpeg thumbnail");
    assert_eq!(saved.when(), ts("2021-06-01T12:00:00Z"));

    // A thumbnail of `None` leaves the one there.
    w.vault.save_file_derived(f.id, MediaInfo::default(), None, None).unwrap();
    assert_eq!(w.vault.file_thumbnail(f.id).unwrap().unwrap(), b"jpeg thumbnail");
}

// ---- renaming and moving ---------------------------------------------------

#[test]
fn renaming_renames_the_file_and_the_record_together() {
    let w = world();
    let f = w.vault.import_bytes(b"x", "old.txt", &into_dir("Docs")).unwrap();
    let renamed = w.vault.rename_file(f.id, "new.txt").unwrap();
    assert_eq!(path_of(&renamed), "Docs/new.txt");
    assert_eq!(renamed.name, "new.txt");
    assert_eq!(w.listing(), ["Docs/new.txt"]);
    assert_eq!(w.vault.file(f.id).unwrap(), renamed);

    import(&w, "taken.txt", b"y");
    let moved = w.vault.import_bytes(b"z", "other.txt", &into_dir("Docs")).unwrap();
    assert!(invalid(w.vault.rename_file(moved.id, "new.txt")).contains("already there"));
    assert!(matches!(w.vault.rename_file(moved.id, "a/b"), Err(Error::Invalid(_))));
    assert!(matches!(w.vault.rename_file(moved.id, ".hidden"), Err(Error::Invalid(_))));
    assert_eq!(w.listing(), ["Docs/new.txt", "Docs/other.txt", "taken.txt"]);
}

#[test]
fn a_sealed_files_name_is_only_its_records() {
    let w = world();
    let f = w.vault.import_bytes(b"x", "before.txt", &sealed(FileOrigin::Files)).unwrap();
    let renamed = w.vault.rename_file(f.id, "after.txt").unwrap();
    assert_eq!(renamed.name, "after.txt");
    assert!(renamed.is_sealed());
    assert_eq!(w.vault.read_file_range(f.id, 0, 10).unwrap(), b"x");
}

#[test]
fn files_move_into_a_folder_under_free_names() {
    let w = world();
    let a = import(&w, "a.txt", b"a");
    let b = import(&w, "b.txt", b"b");
    let existing = w.vault.import_bytes(b"other", "a.txt", &into_dir("Archive")).unwrap();
    assert_eq!(path_of(&existing), "Archive/a.txt");

    let moved = w.vault.move_files(&[a.id, b.id], "Archive").unwrap();
    assert_eq!(path_of(&moved[0]), "Archive/a (2).txt");
    assert_eq!(path_of(&moved[1]), "Archive/b.txt");
    assert_eq!(w.listing(), ["Archive/a (2).txt", "Archive/a.txt", "Archive/b.txt"]);
    assert_eq!(std::fs::read(w.at("Archive/a.txt")).unwrap(), b"other");

    // Already there: left alone, not renumbered.
    let again = w.vault.move_files(&[b.id], "Archive").unwrap();
    assert_eq!(path_of(&again[0]), "Archive/b.txt");

    let secret = w.vault.import_bytes(b"s", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    assert!(invalid(w.vault.move_files(&[secret.id], "Archive")).contains("sealed"));
}

#[test]
fn renaming_or_moving_a_folder_re_paths_every_file_under_it() {
    let w = world();
    let a = w.vault.import_bytes(b"a", "a.jpg", &into_dir("Photos/Summer")).unwrap();
    let b = w.vault.import_bytes(b"b", "b.jpg", &into_dir("Photos")).unwrap();
    let c = w.vault.import_bytes(b"c", "c.jpg", &into_dir("Photographs")).unwrap();

    let renamed = w.vault.rename_folder("Photos", "Pictures").unwrap();
    assert_eq!(renamed, "Pictures");
    assert_eq!(path_of(&w.vault.file(a.id).unwrap()), "Pictures/Summer/a.jpg");
    assert_eq!(path_of(&w.vault.file(b.id).unwrap()), "Pictures/b.jpg");
    assert_eq!(
        path_of(&w.vault.file(c.id).unwrap()),
        "Photographs/c.jpg",
        "a sibling, not under it"
    );
    assert_eq!(w.listing(), ["Photographs/c.jpg", "Pictures/Summer/a.jpg", "Pictures/b.jpg"]);

    let moved = w.vault.move_folder("Pictures/Summer", "Archive").unwrap();
    assert_eq!(moved, "Archive/Summer");
    assert_eq!(path_of(&w.vault.file(a.id).unwrap()), "Archive/Summer/a.jpg");
    assert_eq!(w.vault.file(a.id).unwrap().dir(), Some("Archive/Summer"));

    assert!(invalid(w.vault.move_folder("Archive", "Archive/Summer")).contains("itself"));
    assert!(matches!(w.vault.rename_folder("", "x"), Err(Error::Invalid(_))));
    assert!(matches!(w.vault.rename_folder("Archive", "a/b"), Err(Error::Invalid(_))));
    assert_eq!(w.vault.rename_folder("Archive", "Archive").unwrap(), "Archive");
}

#[test]
fn a_folder_listing_has_its_subfolders_from_the_disk_and_its_files_from_the_catalog() {
    let w = world();
    import(&w, "top.txt", b"t");
    w.vault.import_bytes(b"1", "one.txt", &into_dir("Docs")).unwrap();
    w.vault.import_bytes(b"2", "b.txt", &into_dir("Docs")).unwrap();
    w.vault.import_bytes(b"3", "a.txt", &into_dir("Docs")).unwrap();
    assert_eq!(w.vault.create_folder("", "Empty").unwrap(), "Empty");
    assert!(invalid(w.vault.create_folder("", "Empty")).contains("already there"));

    let top = w.vault.list_folder("").unwrap();
    assert!(top.state.is_available());
    let names: Vec<&str> = top.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["Docs", "Empty"]);
    assert_eq!(top.folders[0].items, 3);
    assert_eq!(top.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["top.txt"]);

    let docs = w.vault.list_folder("Docs").unwrap();
    assert_eq!(
        docs.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["a.txt", "b.txt", "one.txt"]
    );
    assert!(docs.folders.is_empty());
}

#[test]
fn a_listing_includes_files_that_are_missing_from_the_disk() {
    let w = world();
    let f = import(&w, "gone.txt", b"x");
    std::fs::remove_file(w.at("gone.txt")).unwrap();
    w.vault.apply_scan(&ScanChanges { vanished: vec![f.id], ..Default::default() }).unwrap();
    let listing = w.vault.list_folder("").unwrap();
    assert_eq!(ids(&listing.files), vec![f.id]);
    assert!(matches!(listing.files[0].status, FileStatus::Missing { .. }));
}

// ---- the trash -------------------------------------------------------------

#[test]
fn a_file_in_the_trash_is_out_of_the_listings_and_comes_back_where_it_was() {
    let w = world();
    let f = w.vault.import_bytes(b"x", "keep.txt", &into_dir("Docs")).unwrap();
    let trashed = w.vault.trash_files(&[f.id]).unwrap();
    let FileStatus::Trashed { from, .. } = &trashed[0].status else { panic!("{:?}", trashed[0]) };
    assert_eq!(from, "Docs/keep.txt");
    assert!(path_of(&trashed[0]).starts_with(".karmayoga/trash/"));
    assert!(!w.at("Docs/keep.txt").exists());
    assert!(w.vault.files(&query(FileFilter::default())).unwrap().files.is_empty());
    let in_trash = query(FileFilter { status: StatusFilter::Trashed, ..Default::default() });
    assert_eq!(ids(&w.vault.files(&in_trash).unwrap().files), vec![f.id]);
    assert_eq!(w.vault.files_usage().unwrap().trashed_files, 1);
    // Trashing again is a no-op.
    assert_eq!(w.vault.trash_files(&[f.id]).unwrap()[0].status, trashed[0].status);

    let back = w.vault.restore_files(&[f.id]).unwrap();
    assert_eq!(path_of(&back[0]), "Docs/keep.txt");
    assert_eq!(back[0].status, FileStatus::Present);
    assert_eq!(std::fs::read(w.at("Docs/keep.txt")).unwrap(), b"x");
}

#[test]
fn restoring_under_a_taken_name_picks_a_free_one() {
    let w = world();
    let f = import(&w, "a.txt", b"original");
    w.vault.trash_files(&[f.id]).unwrap();
    import(&w, "a.txt", b"newcomer");
    let back = w.vault.restore_files(&[f.id]).unwrap();
    assert_eq!(path_of(&back[0]), "a (2).txt");
    assert_eq!(std::fs::read(w.at("a.txt")).unwrap(), b"newcomer");
    assert_eq!(std::fs::read(w.at("a (2).txt")).unwrap(), b"original");
}

#[test]
fn a_sealed_file_is_trashed_and_restored_without_touching_the_disk() {
    let w = world();
    let f = w.vault.import_bytes(b"x", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    let trashed = w.vault.trash_files(&[f.id]).unwrap();
    assert!(matches!(&trashed[0].status, FileStatus::Trashed { from, .. } if from.is_empty()));
    assert!(trashed[0].is_sealed());
    let back = w.vault.restore_files(&[f.id]).unwrap();
    assert_eq!(back[0].status, FileStatus::Present);
}

#[test]
fn deleting_for_good_needs_the_trash_and_takes_the_bytes_and_the_record() {
    let w = world();
    let f = import(&w, "doomed.txt", b"x");
    assert!(invalid(w.vault.delete_files_forever(&[f.id])).contains("not in the trash"));
    assert!(w.at("doomed.txt").exists());

    let trashed = w.vault.trash_files(&[f.id]).unwrap();
    let in_trash = w.at(path_of(&trashed[0]));
    assert!(in_trash.exists());
    assert_eq!(w.vault.delete_files_forever(&[f.id]).unwrap(), 1);
    assert!(!in_trash.exists());
    assert!(matches!(w.vault.file(f.id), Err(Error::NotFound { .. })));
    assert!(w.listing().is_empty());
}

#[test]
fn emptying_the_trash_deletes_everything_in_it() {
    let w = world();
    let a = import(&w, "a.txt", b"a");
    let b = w.vault.import_bytes(b"b", "b.txt", &sealed(FileOrigin::Files)).unwrap();
    let kept = import(&w, "kept.txt", b"k");
    w.vault.trash_files(&[a.id, b.id]).unwrap();
    assert_eq!(w.vault.empty_files_trash().unwrap(), 2);
    assert!(matches!(w.vault.file(a.id), Err(Error::NotFound { .. })));
    assert!(matches!(w.vault.file(b.id), Err(Error::NotFound { .. })));
    assert_eq!(w.vault.file(kept.id).unwrap().status, FileStatus::Present);
    assert_eq!(w.vault.empty_files_trash().unwrap(), 0);
}

#[test]
fn the_sweep_deletes_only_what_has_been_in_the_trash_longer_than_the_settings_say() {
    let w = world();
    let old = import(&w, "old.txt", b"o");
    w.vault.trash_files(&[old.id]).unwrap();

    let mut settings = w.vault.files_settings().unwrap();
    settings.trash_days = 30;
    w.vault.save_files_settings(&settings).unwrap();
    let now = Timestamp::now();
    assert_eq!(w.vault.purge_files_trash(now + SignedDuration::from_hours(24 * 29)).unwrap(), 0);
    assert_eq!(w.vault.purge_files_trash(now + SignedDuration::from_hours(24 * 31)).unwrap(), 1);
    assert!(matches!(w.vault.file(old.id), Err(Error::NotFound { .. })));

    // Zero keeps the trash until it is emptied by hand.
    let keep = import(&w, "keep.txt", b"k");
    w.vault.trash_files(&[keep.id]).unwrap();
    settings.trash_days = 0;
    w.vault.save_files_settings(&settings).unwrap();
    let far = now + SignedDuration::from_hours(24 * 3650);
    assert_eq!(w.vault.purge_files_trash(far).unwrap(), 0);
    assert!(w.vault.file(keep.id).is_ok());
}

#[test]
fn trashing_a_folder_takes_what_the_catalog_has_not_seen_yet_too() {
    let w = world();
    let known = w.vault.import_bytes(b"k", "known.jpg", &into_dir("Trip/Day1")).unwrap();
    // Put there by a sync tool a moment ago, and not yet catalogued.
    std::fs::write(w.at("Trip/new.jpg"), b"fresh").unwrap();
    let outside = import(&w, "outside.txt", b"o");

    assert_eq!(w.vault.trash_folder("Trip").unwrap(), 2);
    assert!(!w.at("Trip").exists(), "the emptied folders went with it");
    assert_eq!(w.listing(), ["outside.txt"]);
    assert!(w.vault.file(known.id).unwrap().status.is_trashed());
    assert_eq!(w.vault.file(outside.id).unwrap().status, FileStatus::Present);

    // Both are in the trash, with their places remembered.
    let in_trash = query(FileFilter { status: StatusFilter::Trashed, ..Default::default() });
    let trashed = w.vault.files(&in_trash).unwrap().files;
    assert_eq!(trashed.len(), 2);
    let restored = w.vault.restore_files(&ids(&trashed)).unwrap();
    let mut paths: Vec<&str> = restored.iter().map(path_of).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["Trip/Day1/known.jpg", "Trip/new.jpg"]);
    assert_eq!(std::fs::read(w.at("Trip/new.jpg")).unwrap(), b"fresh");
    assert!(restored.iter().any(|f| f.origin == FileOrigin::Found));
}

#[test]
fn only_a_missing_file_can_be_forgotten() {
    let w = world();
    let here = import(&w, "here.txt", b"h");
    let gone = import(&w, "gone.txt", b"g");
    std::fs::remove_file(w.at("gone.txt")).unwrap();
    w.vault.apply_scan(&ScanChanges { vanished: vec![gone.id], ..Default::default() }).unwrap();

    assert!(invalid(w.vault.forget_missing_files(&[here.id])).contains("not missing"));
    assert_eq!(w.vault.forget_missing_files(&[gone.id]).unwrap(), 1);
    assert!(matches!(w.vault.file(gone.id), Err(Error::NotFound { .. })));
    assert!(w.vault.file(here.id).is_ok());
    // And a missing file cannot be trashed or renamed: nothing is there.
    let again = import(&w, "again.txt", b"a");
    std::fs::remove_file(w.at("again.txt")).unwrap();
    w.vault.apply_scan(&ScanChanges { vanished: vec![again.id], ..Default::default() }).unwrap();
    assert!(matches!(w.vault.trash_files(&[again.id]), Err(Error::Invalid(_))));
    assert!(matches!(w.vault.rename_file(again.id, "x.txt"), Err(Error::Invalid(_))));
}

// ---- a folder that is not there ------------------------------------------------

#[test]
fn a_folder_that_is_not_there_is_unavailable_and_never_empty() {
    let w = world();
    let plain = import(&w, "plain.txt", b"plain");
    let secret = w.vault.import_bytes(b"secret", "secret.txt", &sealed(FileOrigin::Files)).unwrap();

    // A drive that is not mounted leaves an empty mount point behind.
    let away = w.root().join("files-away");
    std::fs::rename(&w.folder, &away).unwrap();
    std::fs::create_dir(&w.folder).unwrap();

    assert!(matches!(w.vault.folder_state().unwrap(), FolderState::Unavailable { .. }));

    // Writes of plain files are refused, and say why.
    let refused = w.vault.import_bytes(b"x", "new.txt", &opts(FileOrigin::Files));
    assert!(invalid(refused).contains("not available"));
    assert!(matches!(w.vault.trash_files(&[plain.id]), Err(Error::Invalid(_))));
    assert!(matches!(w.vault.rename_file(plain.id, "other.txt"), Err(Error::Invalid(_))));
    assert!(matches!(w.vault.create_folder("", "New"), Err(Error::Invalid(_))));
    assert!(matches!(
        w.vault.begin_upload("u.bin", 1, &opts(FileOrigin::Files)),
        Err(Error::Invalid(_))
    ));
    assert!(w.listing().is_empty(), "nothing was created in the empty mount point");
    assert!(!w.at(".karmayoga").exists());

    // Reads of plain files say the folder is not there; sealed ones work.
    assert!(invalid(w.vault.read_file_range(plain.id, 0, 10)).contains("not available"));
    assert_eq!(w.vault.read_file_range(secret.id, 0, 10).unwrap(), b"secret");
    let sealed_import =
        w.vault.import_bytes(b"still works", "s2.txt", &sealed(FileOrigin::Files)).unwrap();
    assert!(sealed_import.is_sealed());

    // The listing says so, and is empty, rather than failing.
    let listing = w.vault.list_folder("").unwrap();
    assert!(matches!(listing.state, FolderState::Unavailable { .. }));
    assert!(listing.folders.is_empty() && listing.files.is_empty());

    // A pass that finds nothing on disk does not mark anything missing...
    let applied = w
        .vault
        .apply_scan(&ScanChanges { vanished: vec![plain.id], ..Default::default() })
        .unwrap();
    assert!(applied.report.unavailable);
    assert_eq!(w.vault.file(plain.id).unwrap().status, FileStatus::Present);
    // ...and the trash sweep does nothing.
    w.vault.trash_files(&[secret.id]).unwrap();
    let far = Timestamp::now() + SignedDuration::from_hours(24 * 400);
    assert_eq!(w.vault.purge_files_trash(far).unwrap(), 0);

    // The catalog still answers, with everything it had.
    assert_eq!(w.vault.files_usage().unwrap().files, 2, "the plain file and the sealed import");
    assert!(w.vault.file(plain.id).is_ok());

    // The drive comes back.
    std::fs::remove_dir(&w.folder).unwrap();
    std::fs::rename(&away, &w.folder).unwrap();
    assert!(w.vault.folder_state().unwrap().is_available());
    assert_eq!(w.vault.read_file_range(plain.id, 0, 10).unwrap(), b"plain");
    assert_eq!(w.vault.purge_files_trash(far).unwrap(), 1, "now the sweep may run");
}

// ---- uploads ---------------------------------------------------------------

#[test]
fn an_upload_arrives_in_chunks_and_is_catalogued_when_finished() {
    let w = world();
    let ticket = w.vault.begin_upload("film.bin", 10, &into_dir("Films")).unwrap();
    assert_eq!(ticket.received, 0);
    assert!(
        ticket.upload.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "{}",
        ticket.upload
    );
    let staged = w.at(".karmayoga/uploads").join(&ticket.upload);
    assert!(staged.exists());

    assert_eq!(w.vault.upload_chunk(&ticket.upload, 0, b"0123").unwrap(), 4);
    // A retried chunk is harmless.
    assert_eq!(w.vault.upload_chunk(&ticket.upload, 0, b"0123").unwrap(), 4);
    // A chunk that leaves a gap is refused.
    assert!(matches!(w.vault.upload_chunk(&ticket.upload, 6, b"6789"), Err(Error::Invalid(_))));
    // Finishing early is refused, and the upload carries on.
    assert!(invalid(w.vault.finish_upload(&ticket.upload)).contains("received 4 of the 10"));
    // A chunk past the declared size is refused.
    assert!(invalid(w.vault.upload_chunk(&ticket.upload, 4, b"456789X")).contains("past the 10"));
    assert_eq!(w.vault.upload_chunk(&ticket.upload, 4, b"456789").unwrap(), 10);

    let f = w.vault.finish_upload(&ticket.upload).unwrap();
    assert_eq!(path_of(&f), "Films/film.bin");
    assert_eq!(f.size, 10);
    assert_eq!(std::fs::read(w.at("Films/film.bin")).unwrap(), b"0123456789");
    assert!(!staged.exists(), "the staging file moved into place");
    assert!(matches!(w.vault.upload_chunk(&ticket.upload, 10, b"x"), Err(Error::NotFound { .. })));
    assert!(matches!(w.vault.finish_upload(&ticket.upload), Err(Error::NotFound { .. })));
}

#[test]
fn a_cancelled_upload_leaves_nothing_behind() {
    let w = world();
    let ticket = w.vault.begin_upload("never.bin", 4, &opts(FileOrigin::Files)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"ab").unwrap();
    let staged = w.at(".karmayoga/uploads").join(&ticket.upload);
    assert!(staged.exists());
    w.vault.cancel_upload(&ticket.upload).unwrap();
    assert!(!staged.exists());
    assert!(matches!(w.vault.finish_upload(&ticket.upload), Err(Error::NotFound { .. })));
    w.vault.cancel_upload(&ticket.upload).unwrap();
    assert!(w.listing().is_empty());
    assert!(w.vault.files(&query(FileFilter::default())).unwrap().files.is_empty());
}

#[test]
fn a_sealed_upload_is_sealed_from_its_staging_file() {
    let w = world();
    let ticket = w.vault.begin_upload("v.bin", 6, &sealed(FileOrigin::Chat)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
    w.vault.upload_chunk(&ticket.upload, 3, b"def").unwrap();
    // While it arrives it is the plaintext of a sealed file, so it is not in
    // the files folder at all -- the folder may be synced -- but in a private
    // place of the vault's own.
    assert_eq!(count_in(&w.at(".karmayoga/uploads")), 0);
    let staged = staging(&w).upload_path(&ticket.upload).unwrap();
    assert!(!staged.starts_with(&w.folder), "{}", staged.display());
    assert_eq!(std::fs::read(&staged).unwrap(), b"abcdef");

    let f = w.vault.finish_upload(&ticket.upload).unwrap();
    assert!(f.is_sealed());
    assert_eq!(w.vault.read_file_range(f.id, 0, 10).unwrap(), b"abcdef");
    assert!(!staged.exists(), "the staging file goes once the file is sealed");
    assert!(w.listing().is_empty());
}

#[cfg(unix)]
#[test]
fn the_staging_of_a_sealed_upload_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let w = world();
    let ticket = w.vault.begin_upload("v.bin", 3, &sealed(FileOrigin::Files)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
    let staged = staging(&w).upload_path(&ticket.upload).unwrap();
    assert_eq!(mode(&staged), 0o600);
    assert_eq!(mode(staged.parent().unwrap()), 0o700);
}

#[test]
fn a_sealed_upload_needs_no_files_folder() {
    // A vault whose files folder was never set up: nothing is staged there, so
    // nothing needs it, and nothing sets it up.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(&dir.path().join("vault"));
    assert!(vault.files_settings().unwrap().folder.is_none());
    let ticket = vault.begin_upload("scan.pdf", 6, &sealed(FileOrigin::Files)).unwrap();
    vault.upload_chunk(&ticket.upload, 0, b"passpo").unwrap();
    let f = vault.finish_upload(&ticket.upload).unwrap();
    assert!(f.is_sealed());
    assert_eq!(vault.read_file_range(f.id, 0, 10).unwrap(), b"passpo");
    assert!(vault.files_settings().unwrap().folder.is_none());
}

#[test]
fn whether_an_upload_is_sealed_is_settled_when_it_begins() {
    let w = world();
    let settings = |encrypted| FilesSettings {
        folder: Some(w.folder.display().to_string()),
        encrypted,
        ..Default::default()
    };
    w.vault.save_files_settings(&settings(vec![FileOrigin::Chat])).unwrap();
    // Begun while Chat's files are sealed, with no say of its own...
    let ticket = w.vault.begin_upload("a.bin", 3, &opts(FileOrigin::Chat)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
    assert!(staging(&w).upload_path(&ticket.upload).is_ok());
    // ...and the setting turned off before it is finished: the bytes were
    // staged outside the folder, and the file is sealed as it was promised.
    w.vault.save_files_settings(&settings(vec![])).unwrap();
    let f = w.vault.finish_upload(&ticket.upload).unwrap();
    assert!(f.is_sealed());
    assert_eq!(w.vault.read_file_range(f.id, 0, 10).unwrap(), b"abc");
    assert!(w.listing().is_empty());

    // The other way round: begun plain, staged in the folder, placed there.
    let ticket = w.vault.begin_upload("b.bin", 3, &opts(FileOrigin::Chat)).unwrap();
    assert!(staging(&w).upload_path(&ticket.upload).is_err());
    assert!(w.at(".karmayoga/uploads").join(&ticket.upload).exists());
    w.vault.upload_chunk(&ticket.upload, 0, b"xyz").unwrap();
    w.vault.save_files_settings(&settings(vec![FileOrigin::Chat])).unwrap();
    let f = w.vault.finish_upload(&ticket.upload).unwrap();
    assert!(!f.is_sealed());
    assert_eq!(std::fs::read(w.at(path_of(&f))).unwrap(), b"xyz");
    assert_eq!(w.listing(), vec![path_of(&f).to_string()]);
}

#[test]
fn cancelling_an_upload_that_was_to_be_sealed_removes_its_staging() {
    let w = world();
    let ticket = w.vault.begin_upload("never.bin", 4, &sealed(FileOrigin::Files)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"ab").unwrap();
    let staged = staging(&w).upload_path(&ticket.upload).unwrap();
    w.vault.cancel_upload(&ticket.upload).unwrap();
    assert!(!staged.exists());
    assert!(matches!(w.vault.finish_upload(&ticket.upload), Err(Error::NotFound { .. })));
    assert!(matches!(w.vault.upload_chunk(&ticket.upload, 2, b"cd"), Err(Error::NotFound { .. })));
}

#[test]
fn an_upload_is_begun_with_a_name_the_folder_can_keep() {
    let w = world();
    assert!(matches!(
        w.vault.begin_upload("a/b", 1, &opts(FileOrigin::Files)),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        w.vault.begin_upload("ok.bin", 1, &into_dir("../up")),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(w.vault.upload_chunk("nonsense", 0, b"x"), Err(Error::NotFound { .. })));
}

// ---- what the person adds ----------------------------------------------------

#[test]
fn tags_and_people_are_trimmed_and_deduplicated_and_checked() {
    let w = world();
    let f = import(&w, "a.jpg", b"a");
    let updated = w
        .vault
        .update_files(
            &[f.id],
            &FileUpdate {
                tags: Some(vec![" Travel ".into(), "travel".into(), "Food".into(), "".into()]),
                people: Some(vec!["Asha".into(), "asha".into()]),
                caption: Some("by the harbour".into()),
                place: Some(" Lisbon ".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated[0].tags, ["Travel", "Food"], "the first spelling wins");
    assert_eq!(updated[0].people, ["Asha"]);
    assert_eq!(updated[0].caption, "by the harbour");
    assert_eq!(updated[0].place.as_deref(), Some("Lisbon"));

    let more = w
        .vault
        .update_files(
            &[f.id],
            &FileUpdate {
                add_tags: vec!["FOOD".into(), "Beach".into()],
                remove_tags: vec!["travel".into()],
                add_people: vec!["Ben".into()],
                remove_people: vec!["ASHA".into()],
                place: Some("".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(more[0].tags, ["Food", "Beach"]);
    assert_eq!(more[0].people, ["Ben"]);
    assert!(more[0].place.is_none(), "an empty place clears it");
    assert_eq!(more[0].caption, "by the harbour", "what is not mentioned is left alone");

    let long = "t".repeat(101);
    assert!(matches!(
        w.vault.update_files(&[f.id], &FileUpdate { add_tags: vec![long], ..Default::default() }),
        Err(Error::Invalid(_))
    ));
    let many: Vec<String> = (0..101).map(|i| format!("tag{i}")).collect();
    assert!(matches!(
        w.vault.update_files(&[f.id], &FileUpdate { tags: Some(many), ..Default::default() }),
        Err(Error::Invalid(_))
    ));
    let caption = "c".repeat(20_001);
    assert!(matches!(
        w.vault.update_files(&[f.id], &FileUpdate { caption: Some(caption), ..Default::default() }),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        w.vault.file(f.id).unwrap().tags,
        ["Food", "Beach"],
        "a refused change wrote nothing"
    );
}

#[test]
fn ratings_marks_and_properties_are_validated() {
    use karmayoga_core::files::PropValue;
    let w = world();
    let f = import(&w, "a.jpg", b"a");
    let set = |update: FileUpdate| w.vault.update_files(&[f.id], &update);

    let rated = set(FileUpdate {
        rating: Some(4),
        favourite: Some(true),
        hidden: Some(true),
        ..Default::default()
    })
    .unwrap();
    assert_eq!((rated[0].rating, rated[0].favourite, rated[0].hidden), (Some(4), true, true));
    assert_eq!(set(FileUpdate { rating: Some(0), ..Default::default() }).unwrap()[0].rating, None);
    assert!(matches!(
        set(FileUpdate { rating: Some(6), ..Default::default() }),
        Err(Error::Invalid(_))
    ));

    let mut props = std::collections::BTreeMap::new();
    props.insert("user.mood".to_string(), Some(PropValue::Text("calm".into())));
    props.insert("user.score".to_string(), Some(PropValue::Number(7.5)));
    props.insert("user.seen".to_string(), Some(PropValue::Date("2024-03-01".into())));
    let with = set(FileUpdate { properties: props, ..Default::default() }).unwrap();
    assert_eq!(with[0].properties.len(), 3);
    assert_eq!(with[0].properties["user.mood"], PropValue::Text("calm".into()));

    let mut remove = std::collections::BTreeMap::new();
    remove.insert("user.mood".to_string(), None);
    let without = set(FileUpdate { properties: remove, ..Default::default() }).unwrap();
    assert!(!without[0].properties.contains_key("user.mood"));
    assert_eq!(without[0].properties.len(), 2);

    for (key, value) in [
        ("mood", PropValue::Bool(true)),
        ("User.mood", PropValue::Bool(true)),
        ("user.mood", PropValue::Number(f64::NAN)),
        ("user.when", PropValue::Date("last tuesday".into())),
    ] {
        let mut bad = std::collections::BTreeMap::new();
        bad.insert(key.to_string(), Some(value));
        assert!(
            matches!(
                set(FileUpdate { properties: bad, ..Default::default() }),
                Err(Error::Invalid(_))
            ),
            "{key} was accepted"
        );
    }
}

#[test]
fn a_correction_to_when_a_picture_was_taken_can_be_undone() {
    let w = world();
    let f = import(&w, "a.jpg", b"a");
    w.vault
        .save_file_derived(
            f.id,
            MediaInfo { taken_at: Some(ts("2020-01-01T00:00:00Z")), ..Default::default() },
            None,
            None,
        )
        .unwrap();
    let corrected = w
        .vault
        .update_files(
            &[f.id],
            &FileUpdate { taken_at: Some("2019-07-04T10:00:00Z".into()), ..Default::default() },
        )
        .unwrap();
    assert_eq!(corrected[0].media.taken_at, Some(ts("2019-07-04T10:00:00Z")));

    // Reading the file again does not undo the correction.
    let reread = w
        .vault
        .save_file_derived(
            f.id,
            MediaInfo {
                taken_at: Some(ts("2020-01-01T00:00:00Z")),
                width: Some(10),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(reread.media.taken_at, Some(ts("2019-07-04T10:00:00Z")));
    assert_eq!(reread.media.width, Some(10));

    let back = w
        .vault
        .update_files(&[f.id], &FileUpdate { taken_at: Some("".into()), ..Default::default() })
        .unwrap();
    assert_eq!(back[0].media.taken_at, Some(ts("2020-01-01T00:00:00Z")));
    assert!(matches!(
        w.vault.update_files(
            &[f.id],
            &FileUpdate { taken_at: Some("yesterday".into()), ..Default::default() }
        ),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn a_change_to_several_files_is_all_or_nothing() {
    let w = world();
    let a = import(&w, "a.jpg", b"a");
    let b = import(&w, "b.jpg", b"b");
    let update = FileUpdate { favourite: Some(true), ..Default::default() };

    let both = w.vault.update_files(&[a.id, b.id, a.id], &update).unwrap();
    assert_eq!(both.len(), 2, "each once");
    assert!(both.iter().all(|f| f.favourite));

    let ghost = FileId::new();
    let unfavourite = FileUpdate { favourite: Some(false), ..Default::default() };
    assert!(matches!(
        w.vault.update_files(&[a.id, ghost], &unfavourite),
        Err(Error::NotFound { .. })
    ));
    assert!(w.vault.file(a.id).unwrap().favourite, "nothing was written");
}

// ---- finding files -----------------------------------------------------------

#[test]
fn a_query_filters_in_the_store_and_in_the_vault_and_pages_after_both() {
    let w = world();
    let a = import(&w, "harbour-morning.jpg", b"a");
    let b = import(&w, "harbour-evening.jpg", b"b");
    let c = import(&w, "market.jpg", b"c");
    let d = w.vault.import_bytes(b"d", "notes.txt", &sealed(FileOrigin::Files)).unwrap();
    w.vault
        .update_files(
            &[a.id, d.id],
            &FileUpdate { add_tags: vec!["Travel".into()], ..Default::default() },
        )
        .unwrap();
    w.vault
        .update_files(
            &[b.id],
            &FileUpdate {
                people: Some(vec!["Asha".into()]),
                caption: Some("Sunset".into()),
                ..Default::default()
            },
        )
        .unwrap();
    w.vault
        .update_files(
            &[c.id],
            &FileUpdate {
                place: Some("Lisbon".into()),
                favourite: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

    let by_name = FileFilter { sort: karmayoga_core::FileSort::Name, ..Default::default() };
    let all = w.vault.files(&query(by_name.clone())).unwrap();
    assert_eq!(all.total, 4);

    // The store's own filters.
    let fav = FileFilter { favourite: true, ..Default::default() };
    assert_eq!(ids(&w.vault.files(&query(fav)).unwrap().files), vec![c.id]);
    let images = FileFilter { kinds: vec![FileKind::Image], ..by_name.clone() };
    assert_eq!(w.vault.files(&query(images)).unwrap().total, 3);

    // The ones only the vault can answer, sealed files included.
    let tagged =
        FileQuery { tag: Some("travel".into()), filter: by_name.clone(), ..Default::default() };
    let page = w.vault.files(&tagged).unwrap();
    assert_eq!(page.total, 2);
    assert!(ids(&page.files).contains(&a.id) && ids(&page.files).contains(&d.id));

    let person = FileQuery { person: Some("ASHA".into()), ..Default::default() };
    assert_eq!(ids(&w.vault.files(&person).unwrap().files), vec![b.id]);
    let place = FileQuery { place: Some("lisbon".into()), ..Default::default() };
    assert_eq!(ids(&w.vault.files(&place).unwrap().files), vec![c.id]);

    // Every word, anywhere: name, caption, path.
    let words = FileQuery { text: Some("HARBOUR sunset".into()), ..Default::default() };
    assert_eq!(ids(&w.vault.files(&words).unwrap().files), vec![b.id]);
    let one =
        FileQuery { text: Some("harbour".into()), filter: by_name.clone(), ..Default::default() };
    assert_eq!(w.vault.files(&one).unwrap().total, 2);

    // Paging happens after the vault's filtering, and the total is every match.
    let paged = FileQuery {
        text: Some("jpg".into()),
        filter: FileFilter {
            sort: karmayoga_core::FileSort::Name,
            limit: Some(2),
            offset: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let page = w.vault.files(&paged).unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.files.len(), 2);
    let names: Vec<&str> = page.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["harbour-morning.jpg", "market.jpg"]);

    // And the store's paging when nothing needs the vault.
    let plain = FileFilter {
        sort: karmayoga_core::FileSort::Name,
        limit: Some(1),
        offset: 2,
        ..Default::default()
    };
    let page = w.vault.files(&query(plain)).unwrap();
    assert_eq!((page.total, page.files.len()), (4, 1));
}

#[test]
fn the_sidebars_facets_count_live_visible_files_most_used_first() {
    let w = world();
    let files: Vec<FileEntry> =
        (0..4).map(|i| import(&w, &format!("{i}.jpg"), &[i as u8])).collect();
    let tag = |f: &FileEntry, tags: &[&str], people: &[&str]| {
        w.vault
            .update_files(
                &[f.id],
                &FileUpdate {
                    tags: Some(tags.iter().map(|t| t.to_string()).collect()),
                    people: Some(people.iter().map(|p| p.to_string()).collect()),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    tag(&files[0], &["Travel", "Food"], &["Asha"]);
    tag(&files[1], &["travel"], &["Asha", "Ben"]);
    tag(&files[2], &["Food", "Travel"], &[]);
    tag(&files[3], &["Secret"], &["Hidden Person"]);
    w.vault
        .update_files(&[files[3].id], &FileUpdate { hidden: Some(true), ..Default::default() })
        .unwrap();

    let taken = |f: &FileEntry, at: &str, make: &str, model: &str| {
        w.vault
            .save_file_derived(
                f.id,
                MediaInfo {
                    taken_at: Some(ts(at)),
                    camera_make: Some(make.into()),
                    camera_model: Some(model.into()),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    };
    taken(&files[0], "2021-06-01T12:00:00Z", "Canon", "Canon EOS R5");
    taken(&files[1], "2023-06-01T12:00:00Z", "Canon", "Canon EOS R5");
    taken(&files[2], "2023-07-01T12:00:00Z", "Apple", "iPhone 15");
    taken(&files[3], "2019-07-01T12:00:00Z", "Hidden", "Camera");

    // Trashed files are not counted either.
    let trashed = import(&w, "trashed.jpg", b"t");
    w.vault
        .update_files(
            &[trashed.id],
            &FileUpdate { add_tags: vec!["Food".into(), "Food".into()], ..Default::default() },
        )
        .unwrap();
    w.vault.trash_files(&[trashed.id]).unwrap();

    let facets = w.vault.files_facets().unwrap();
    let pairs = |f: &[karmayoga_core::files::Facet]| -> Vec<(String, u64)> {
        f.iter().map(|x| (x.name.clone(), x.count)).collect()
    };
    assert_eq!(pairs(&facets.tags), [("Travel".to_string(), 3), ("Food".to_string(), 2)]);
    assert_eq!(pairs(&facets.people), [("Asha".to_string(), 2), ("Ben".to_string(), 1)]);
    assert!(facets.places.is_empty());
    assert_eq!(pairs(&facets.years), [("2023".to_string(), 2), ("2021".to_string(), 1)]);
    assert_eq!(
        pairs(&facets.cameras),
        [("Canon EOS R5".to_string(), 2), ("Apple iPhone 15".to_string(), 1)]
    );
}

// ---- stacks ------------------------------------------------------------------

#[test]
fn stacks_are_one_deep_and_a_top_brings_its_members() {
    let w = world();
    let f: Vec<FileEntry> = (0..6).map(|i| import(&w, &format!("{i}.jpg"), &[i as u8])).collect();
    let stack_of = |i: usize| w.vault.file(f[i].id).unwrap().stack;

    w.vault.stack_files(f[0].id, &[f[1].id, f[2].id]).unwrap();
    assert_eq!((stack_of(0), stack_of(1), stack_of(2)), (None, Some(f[0].id), Some(f[0].id)));
    let tops = query(FileFilter { stack_tops_only: true, ..Default::default() });
    assert_eq!(w.vault.files(&tops).unwrap().total, 4, "two are under another");

    // Stacking under a member stacks under its top: a stack is one deep.
    w.vault.stack_files(f[1].id, &[f[3].id]).unwrap();
    assert_eq!(stack_of(3), Some(f[0].id));

    // A file that is a top brings its members when it goes under another.
    w.vault.stack_files(f[4].id, &[f[0].id]).unwrap();
    assert_eq!(stack_of(0), Some(f[4].id));
    for i in [1, 2, 3] {
        assert_eq!(stack_of(i), Some(f[4].id), "{i} came along");
    }

    // Unstacking a member frees it alone; unstacking a top frees them all.
    w.vault.unstack_files(&[f[1].id]).unwrap();
    assert_eq!(stack_of(1), None);
    assert_eq!(stack_of(2), Some(f[4].id));
    w.vault.unstack_files(&[f[4].id]).unwrap();
    for i in 0..6 {
        assert_eq!(stack_of(i), None, "{i} is free");
    }
}

#[test]
fn deleting_a_stacks_top_for_good_frees_its_members() {
    let w = world();
    let top = import(&w, "top.jpg", b"t");
    let member = import(&w, "member.jpg", b"m");
    w.vault.stack_files(top.id, &[member.id]).unwrap();
    w.vault.trash_files(&[top.id]).unwrap();
    assert_eq!(w.vault.file(member.id).unwrap().stack, Some(top.id), "kept while it can come back");
    w.vault.delete_files_forever(&[top.id]).unwrap();
    assert_eq!(w.vault.file(member.id).unwrap().stack, None);
}

// ---- albums ------------------------------------------------------------------

#[test]
fn an_album_holds_files_and_deleting_it_deletes_none() {
    let w = world();
    let a = import(&w, "a.jpg", b"a");
    let b = import(&w, "b.jpg", b"b");

    let blank = karmayoga_core::Album {
        id: karmayoga_core::AlbumId::new(),
        name: "   ".into(),
        description: String::new(),
        cover: None,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    };
    assert!(matches!(w.vault.save_album(&blank), Err(Error::Invalid(_))));

    let album = w
        .vault
        .save_album(&karmayoga_core::Album { name: " Summer ".into(), ..blank.clone() })
        .unwrap();
    assert_eq!(album.name, "Summer");
    let renamed = w
        .vault
        .save_album(&karmayoga_core::Album {
            name: "Summer 2024".into(),
            created_at: Timestamp::UNIX_EPOCH,
            ..album.clone()
        })
        .unwrap();
    assert_eq!(renamed.created_at, album.created_at, "an album keeps when it was made");

    assert!(matches!(
        w.vault.add_to_album(album.id, &[a.id, FileId::new()]),
        Err(Error::NotFound { .. })
    ));
    w.vault.add_to_album(album.id, &[a.id, b.id, a.id]).unwrap();

    let views = w.vault.albums().unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].count, 2);
    assert_eq!(views[0].cover.as_ref().map(|c| c.id), Some(a.id), "the first, when none is chosen");
    assert_eq!(w.vault.file_albums(a.id).unwrap()[0].id, album.id);

    // A chosen cover is used while it is in the album...
    w.vault.save_album(&karmayoga_core::Album { cover: Some(b.id), ..renamed.clone() }).unwrap();
    assert_eq!(w.vault.albums().unwrap()[0].cover.as_ref().map(|c| c.id), Some(b.id));
    // ...and a file in the trash is neither counted nor drawn.
    w.vault.trash_files(&[b.id]).unwrap();
    let view = &w.vault.albums().unwrap()[0];
    assert_eq!(view.count, 1);
    assert_eq!(view.cover.as_ref().map(|c| c.id), Some(a.id));

    w.vault.remove_from_album(album.id, &[a.id]).unwrap();
    assert!(w.vault.file_albums(a.id).unwrap().is_empty());
    assert_eq!(w.vault.albums().unwrap()[0].count, 0);

    w.vault.add_to_album(album.id, &[a.id]).unwrap();
    w.vault.delete_album(album.id).unwrap();
    assert!(w.vault.albums().unwrap().is_empty());
    assert!(w.vault.file(a.id).is_ok(), "deleting an album never deletes a file");
}

// ---- references --------------------------------------------------------------

fn doc_with_file(file: FileId) -> RichDoc {
    RichDoc(json!({
        "type": "doc",
        "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "A day at the harbour" }] },
            { "type": "media", "attrs": { "file": file.to_string(), "kind": "image", "caption": "boats" } }
        ]
    }))
}

fn a_journal(w: &World) -> Journal {
    let journal = Journal::new("Days");
    w.vault.save_journal(&journal).unwrap();
    journal
}

#[test]
fn saving_an_entry_records_the_files_its_body_refers_to() {
    let w = world();
    let journal = a_journal(&w);
    let pic = w.vault.import_bytes(b"png", "boats.png", &opts(FileOrigin::Journal)).unwrap();
    let other = w.vault.import_bytes(b"png2", "other.png", &opts(FileOrigin::Journal)).unwrap();

    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = doc_with_file(pic.id);
    w.vault.save_entry(&entry, None).unwrap();

    let uses = w.vault.file_uses(pic.id).unwrap();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].owner, RefOwner::new(RefKind::Entry, entry.id));
    assert_eq!(uses[0].title, "A day at the harbour");
    assert!(w.vault.file_uses(other.id).unwrap().is_empty());

    // "Unused" is a file an app added and nothing refers to.
    let unused = FileQuery { unused: true, ..Default::default() };
    assert_eq!(ids(&w.vault.files(&unused).unwrap().files), vec![other.id]);
    // A file somebody put there themselves is never "unused".
    import(&w, "mine.txt", b"m");
    assert_eq!(w.vault.files(&unused).unwrap().total, 1);

    // Editing the body to point at the other one moves the reference.
    let loaded = entry.updated_at;
    entry.body = doc_with_file(other.id);
    entry.updated_at = Timestamp::now();
    w.vault.save_entry(&entry, Some(loaded)).unwrap();
    assert!(w.vault.file_uses(pic.id).unwrap().is_empty());
    assert_eq!(w.vault.file_uses(other.id).unwrap().len(), 1);

    // overwrite_entry records too, and deleting the entry forgets.
    entry.body = doc_with_file(pic.id);
    w.vault.overwrite_entry(&entry).unwrap();
    assert_eq!(w.vault.file_uses(pic.id).unwrap().len(), 1);
    assert!(w.vault.file_uses(other.id).unwrap().is_empty());
    w.vault.delete_entry(entry.id).unwrap();
    assert!(w.vault.file_uses(pic.id).unwrap().is_empty());
}

#[test]
fn deleting_a_journal_forgets_what_its_entries_referred_to() {
    let w = world();
    let journal = a_journal(&w);
    let pic = w.vault.import_bytes(b"png", "boats.png", &opts(FileOrigin::Journal)).unwrap();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = doc_with_file(pic.id);
    w.vault.save_entry(&entry, None).unwrap();
    assert_eq!(w.vault.file_uses(pic.id).unwrap().len(), 1);
    w.vault.delete_journal(journal.id).unwrap();
    assert!(w.vault.file_uses(pic.id).unwrap().is_empty());
}

#[test]
fn notes_and_assistant_messages_refer_to_files_too() {
    let w = world();
    let pic = w.vault.import_bytes(b"png", "boats.png", &opts(FileOrigin::Notes)).unwrap();

    let mut note = Note::new("Boat ideas");
    note.body = doc_with_file(pic.id);
    w.vault.save_note(&note, None).unwrap();

    let conversation = Conversation::new();
    w.vault.save_conversation(&conversation).unwrap();
    let mut message = Message::user(conversation.id, "what boat is this?");
    message.files = vec![pic.id];
    w.vault.save_message(&message).unwrap();

    let uses = w.vault.file_uses(pic.id).unwrap();
    assert_eq!(uses.len(), 2);
    let in_note = uses.iter().find(|u| u.owner.kind == RefKind::Note).unwrap();
    assert_eq!(in_note.title, "Boat ideas");
    let in_message = uses.iter().find(|u| u.owner.kind == RefKind::Message).unwrap();
    assert_eq!(in_message.owner.id, message.id.to_string());
    assert_eq!(in_message.title, "what boat is this?", "the conversation's title");
    assert_eq!(in_message.parent.as_deref(), Some(conversation.id.to_string().as_str()));

    w.vault.delete_note(note.id).unwrap();
    assert_eq!(w.vault.file_uses(pic.id).unwrap().len(), 1);
    w.vault.delete_message(message.id).unwrap();
    assert!(w.vault.file_uses(pic.id).unwrap().is_empty());

    // Deleting a conversation forgets what its messages referred to.
    let mut again = Message::user(conversation.id, "and this one?");
    again.files = vec![pic.id];
    w.vault.save_message(&again).unwrap();
    assert_eq!(w.vault.file_uses(pic.id).unwrap().len(), 1);
    w.vault.delete_conversation(conversation.id).unwrap();
    assert!(w.vault.file_uses(pic.id).unwrap().is_empty());
}

#[test]
fn a_reference_to_a_file_that_is_not_there_is_not_recorded() {
    let w = world();
    let journal = a_journal(&w);
    let ghost = FileId::new();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = doc_with_file(ghost);
    w.vault.save_entry(&entry, None).unwrap();
    assert!(w.vault.file_uses(ghost).unwrap().is_empty());
}

#[test]
fn rebuilding_the_references_restores_what_a_save_would_have_recorded() {
    let w = world();
    let journal = a_journal(&w);
    let pic = w.vault.import_bytes(b"png", "boats.png", &opts(FileOrigin::Journal)).unwrap();
    let other = w.vault.import_bytes(b"png2", "o.png", &opts(FileOrigin::Notes)).unwrap();
    let kept = w.vault.import_bytes(b"png3", "k.png", &opts(FileOrigin::Assistant)).unwrap();

    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = doc_with_file(pic.id);
    w.vault.save_entry(&entry, None).unwrap();
    let mut note = Note::new("n");
    note.body = doc_with_file(other.id);
    w.vault.save_note(&note, None).unwrap();
    let conversation = Conversation::new();
    w.vault.save_conversation(&conversation).unwrap();
    let mut message = Message::user(conversation.id, "hi");
    message.files = vec![kept.id];
    w.vault.save_message(&message).unwrap();

    // The table drifts: one is lost, one is made up.
    w.vault.set_file_refs(&RefOwner::new(RefKind::Entry, entry.id), &[]).unwrap();
    w.vault.set_file_refs(&RefOwner::new(RefKind::Note, note.id), &[pic.id]).unwrap();
    assert!(w.vault.file_uses(pic.id).unwrap().iter().all(|u| u.owner.kind != RefKind::Entry));

    assert_eq!(w.vault.rebuild_file_refs().unwrap(), 3);
    assert_eq!(w.vault.file_uses(pic.id).unwrap().len(), 1);
    assert_eq!(w.vault.file_uses(pic.id).unwrap()[0].owner.kind, RefKind::Entry);
    assert_eq!(w.vault.file_uses(other.id).unwrap()[0].owner.kind, RefKind::Note);
    assert_eq!(w.vault.file_uses(kept.id).unwrap()[0].owner.kind, RefKind::Message);
}

// ---- what the reconciler found -----------------------------------------------

fn new_file(path: &str) -> NewFile {
    NewFile {
        path: path.into(),
        size: 10,
        modified_us: 1_700_000_000_000_000,
        hash: format!("hash-of-{path}"),
        mime: String::new(),
    }
}

fn indexed(w: &World, path: &str) -> FileId {
    w.vault
        .file_index()
        .unwrap()
        .into_iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("{path}"))
        .id
}

#[test]
fn a_scan_catalogues_what_it_found_and_stacks_what_belongs_together() {
    let w = world();
    let applied = w
        .vault
        .apply_scan(&ScanChanges {
            new: vec![
                new_file("Trip/IMG_1.jpg"),
                new_file("Trip/IMG_1.dng"),
                new_file("notes.txt"),
            ],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(applied.report.added, 3);
    assert_eq!(applied.to_probe.len(), 3);
    assert!(!applied.report.unavailable);

    let jpg = w.vault.file(indexed(&w, "Trip/IMG_1.jpg")).unwrap();
    let raw = w.vault.file(indexed(&w, "Trip/IMG_1.dng")).unwrap();
    let text = w.vault.file(indexed(&w, "notes.txt")).unwrap();
    assert_eq!(jpg.origin, FileOrigin::Found);
    assert_eq!((jpg.kind, text.kind), (FileKind::Image, FileKind::Document));
    assert_eq!(jpg.name, "IMG_1.jpg");
    assert_eq!(jpg.hash, "hash-of-Trip/IMG_1.jpg");
    assert_eq!(jpg.status, FileStatus::Present);
    assert_eq!(jpg.modified_at, Timestamp::from_microsecond(1_700_000_000_000_000).unwrap());
    assert_eq!(raw.stack, Some(jpg.id), "a RAW file goes under the JPEG of the same name");
    assert_eq!(jpg.stack, None);

    // A path somebody else catalogued in the meantime is left alone.
    let again = w
        .vault
        .apply_scan(&ScanChanges { new: vec![new_file("notes.txt")], ..Default::default() })
        .unwrap();
    assert_eq!(again.report.added, 0);
    assert_eq!(w.vault.files_usage().unwrap().files, 3);
}

#[test]
fn a_raw_file_that_arrives_later_goes_under_the_jpeg_already_there() {
    let w = world();
    w.vault
        .apply_scan(&ScanChanges { new: vec![new_file("IMG_7.jpg")], ..Default::default() })
        .unwrap();
    w.vault
        .apply_scan(&ScanChanges { new: vec![new_file("IMG_7.dng")], ..Default::default() })
        .unwrap();
    let jpg = indexed(&w, "IMG_7.jpg");
    assert_eq!(w.vault.file(indexed(&w, "IMG_7.dng")).unwrap().stack, Some(jpg));
}

#[test]
fn a_scan_keeps_what_the_person_added_through_every_change() {
    let w = world();
    w.vault
        .apply_scan(&ScanChanges {
            new: vec![new_file("IMG_1.jpg"), new_file("b.txt")],
            ..Default::default()
        })
        .unwrap();
    let (jpg, txt) = (indexed(&w, "IMG_1.jpg"), indexed(&w, "b.txt"));
    w.vault
        .update_files(
            &[jpg, txt],
            &FileUpdate {
                caption: Some("keep me".into()),
                favourite: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

    // Moved: the same record at another path.
    let moved = w
        .vault
        .apply_scan(&ScanChanges {
            moved: vec![MovedFile {
                id: jpg,
                path: "Sorted/IMG_1.jpg".into(),
                size: 10,
                modified_us: 1_700_000_001_000_000,
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(moved.report.moved, 1);
    assert!(moved.to_probe.is_empty(), "the contents did not change");
    let f = w.vault.file(jpg).unwrap();
    assert_eq!((path_of(&f), f.name.as_str()), ("Sorted/IMG_1.jpg", "IMG_1.jpg"));
    assert_eq!((f.caption.as_str(), f.favourite), ("keep me", true));

    // Changed: new contents, to be read again.
    let changed = w
        .vault
        .apply_scan(&ScanChanges {
            changed: vec![ChangedFile {
                id: txt,
                size: 99,
                modified_us: 1_700_000_002_000_000,
                hash: "newhash".into(),
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!((changed.report.changed, changed.to_probe.clone()), (1, vec![txt]));
    let f = w.vault.file(txt).unwrap();
    assert_eq!((f.size, f.hash.as_str()), (99, "newhash"));
    assert_eq!(f.caption, "keep me");

    // Vanished: missing, with everything the person added.
    let gone =
        w.vault.apply_scan(&ScanChanges { vanished: vec![txt], ..Default::default() }).unwrap();
    assert_eq!(gone.report.missing, 1);
    let f = w.vault.file(txt).unwrap();
    assert!(matches!(f.status, FileStatus::Missing { .. }));
    assert_eq!(f.caption, "keep me");
    assert_eq!(w.vault.files_usage().unwrap().missing_files, 1);

    // Returned: present again.
    let back =
        w.vault.apply_scan(&ScanChanges { returned: vec![txt], ..Default::default() }).unwrap();
    assert_eq!(back.report.returned, 1);
    assert_eq!(w.vault.file(txt).unwrap().status, FileStatus::Present);
    assert_eq!(w.vault.files_usage().unwrap().missing_files, 0);
}

// ---- sealing and unsealing, exporting ---------------------------------------------

#[test]
fn a_file_can_be_sealed_into_the_vault_and_written_back_out_where_it_was() {
    let w = world();
    let f = w.vault.import_bytes(b"my diary", "diary.txt", &into_dir("Private")).unwrap();
    let hash = f.hash.clone();

    let sealed_f = w.vault.set_file_encrypted(f.id, true).unwrap();
    assert!(sealed_f.is_sealed());
    assert!(w.listing().is_empty(), "the plain copy is gone");
    assert_eq!(w.vault.read_file_range(f.id, 0, 100).unwrap(), b"my diary");
    assert_eq!(w.vault.set_file_encrypted(f.id, true).unwrap(), sealed_f, "already sealed");

    let plain = w.vault.set_file_encrypted(f.id, false).unwrap();
    assert_eq!(path_of(&plain), "Private/diary.txt", "it remembered its place");
    assert_eq!(plain.hash, hash);
    assert_eq!(std::fs::read(w.at("Private/diary.txt")).unwrap(), b"my diary");
    assert!(!plain.properties.keys().any(|k| k.starts_with("files.")), "{:?}", plain.properties);
}

#[test]
fn a_file_that_was_always_sealed_is_written_out_into_encrypted() {
    let w = world();
    let f = w.vault.import_bytes(b"x", "s.txt", &sealed(FileOrigin::Chat)).unwrap();
    let plain = w.vault.set_file_encrypted(f.id, false).unwrap();
    assert_eq!(path_of(&plain), "Encrypted/s.txt");
    assert_eq!(std::fs::read(w.at("Encrypted/s.txt")).unwrap(), b"x");
}

#[test]
fn a_file_is_copied_out_without_overwriting_and_sealed_ones_are_decrypted() {
    let w = world();
    let plain = import(&w, "p.txt", b"plain");
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();

    let out = w.root().join("out");
    std::fs::create_dir(&out).unwrap();
    w.vault.export_file(plain.id, &out.join("p.txt")).unwrap();
    w.vault.export_file(secret.id, &out.join("s.txt")).unwrap();
    assert_eq!(std::fs::read(out.join("p.txt")).unwrap(), b"plain");
    assert_eq!(std::fs::read(out.join("s.txt")).unwrap(), b"secret");
    assert!(matches!(w.vault.export_file(plain.id, &out.join("p.txt")), Err(Error::Invalid(_))));
    assert!(matches!(w.vault.export_file(secret.id, &out.join("s.txt")), Err(Error::Invalid(_))));
    assert_eq!(std::fs::read(out.join("s.txt")).unwrap(), b"secret", "left alone");
}

#[test]
fn another_program_opens_a_plain_file_in_place_and_a_sealed_one_from_a_private_copy() {
    let w = world();
    let plain = import(&w, "p.txt", b"plain");
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();

    assert_eq!(w.vault.open_file_copy(plain.id).unwrap(), w.vault.plain_path(plain.id).unwrap());
    let copy = w.vault.open_file_copy(secret.id).unwrap();
    // Outside the files folder altogether: a sync tool that ignores
    // `.stignore` must never see the decrypted file.
    assert!(!copy.starts_with(&w.folder), "{}", copy.display());
    assert_eq!(copy.file_name().unwrap(), "s.txt");
    assert_eq!(std::fs::read(&copy).unwrap(), b"secret");
    assert_eq!(w.vault.open_file_copy(secret.id).unwrap(), copy);
    // The plain file where it always was, and no unencrypted copy of the
    // sealed one anywhere in the folder, internal part included.
    assert_eq!(w.listing(), vec!["p.txt".to_string()]);
    let legacy = w.at(".karmayoga/open");
    assert!(!legacy.exists() || std::fs::read_dir(&legacy).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn the_decrypted_copy_of_a_sealed_file_is_readable_by_its_owner_alone() {
    use std::os::unix::fs::PermissionsExt;
    let w = world();
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    let copy = w.vault.open_file_copy(secret.id).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&copy), 0o600);
    assert_eq!(mode(copy.parent().unwrap()), 0o700);
    assert_eq!(mode(copy.parent().unwrap().parent().unwrap()), 0o700);
}

#[test]
fn decrypted_copies_are_swept_when_stale_and_so_is_what_an_older_build_left_in_the_folder() {
    let w = world();
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    let copy = w.vault.open_file_copy(secret.id).unwrap();
    // A copy opened just now is not stale.
    assert_eq!(w.vault.sweep_open_copies(std::time::Duration::from_secs(86_400)).unwrap(), 0);
    assert!(copy.exists());

    // An older build kept them in the folder's own `open` folder.
    let legacy = w.at(".karmayoga/open/old-id");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("passport.pdf"), b"decrypted").unwrap();
    assert_eq!(w.vault.sweep_open_copies(std::time::Duration::from_secs(86_400)).unwrap(), 1);
    assert!(!legacy.exists());
    assert!(copy.exists());

    // Everything older than nothing goes.
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(w.vault.sweep_open_copies(std::time::Duration::ZERO).unwrap(), 1);
    assert!(!copy.exists());
}

#[test]
fn a_stale_sealed_upload_staging_is_swept_with_the_decrypted_copies() {
    let w = world();
    let (old, new) = (
        w.vault.begin_upload("old.bin", 1, &sealed(FileOrigin::Files)).unwrap(),
        w.vault.begin_upload("new.bin", 1, &sealed(FileOrigin::Files)).unwrap(),
    );
    for ticket in [&old, &new] {
        w.vault.upload_chunk(&ticket.upload, 0, b"x").unwrap();
    }
    backdate(&staging(&w).upload_path(&old.upload).unwrap(), 2);

    let hour = std::time::Duration::from_secs(3600);
    assert_eq!(w.vault.sweep_open_copies(hour).unwrap(), 1);
    assert!(staging(&w).upload_path(&old.upload).is_err());
    assert!(staging(&w).upload_path(&new.upload).is_ok(), "one still arriving is not stale");
}

#[test]
fn locking_deletes_every_decrypted_copy_and_staged_sealed_upload() {
    let w = world();
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    let copy = w.vault.open_file_copy(secret.id).unwrap();
    let ticket = w.vault.begin_upload("v.bin", 6, &sealed(FileOrigin::Files)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
    let staged = staging(&w).upload_path(&ticket.upload).unwrap();
    assert!(copy.exists() && staged.exists());

    w.vault.lock();

    assert!(!copy.exists(), "a decrypted copy outlived the key");
    assert!(!copy.parent().unwrap().exists(), "and the folder it was made in");
    assert!(!staged.exists(), "a staged sealed upload outlived the key");
    assert!(matches!(w.vault.upload_chunk(&ticket.upload, 3, b"def"), Err(Error::NotFound { .. })));
    // Nothing else was touched: the sealed file is still there to be read.
    w.vault.unlock(None).unwrap();
    assert_eq!(w.vault.read_file_range(secret.id, 0, 10).unwrap(), b"secret");
    // And a copy can be made again.
    assert_eq!(std::fs::read(w.vault.open_file_copy(secret.id).unwrap()).unwrap(), b"secret");
}

#[test]
fn a_read_only_handle_locking_leaves_the_writers_copies_and_uploads_alone() {
    let w = world();
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    let copy = w.vault.open_file_copy(secret.id).unwrap();
    let ticket = w.vault.begin_upload("v.bin", 6, &sealed(FileOrigin::Files)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
    let staged = staging(&w).upload_path(&ticket.upload).unwrap();

    // `karmayoga serve`, or a second window that fell back to reading, shares
    // the vault's folders with the process that can write. Its lock drops its
    // own key and nothing else: the viewer still has its file, and the phone's
    // upload carries on.
    let beside = karmayoga_vault::open(w.vault.path()).unwrap();
    assert!(!beside.is_writable());
    beside.lock();
    assert!(!beside.is_unlocked());
    assert_eq!(std::fs::read(&copy).unwrap(), b"secret", "a viewer's file vanished");
    assert!(staged.exists(), "an upload in progress was pulled from under it");
    assert_eq!(w.vault.upload_chunk(&ticket.upload, 3, b"def").unwrap(), 6);
    let f = w.vault.finish_upload(&ticket.upload).unwrap();
    assert_eq!(w.vault.read_file_range(f.id, 0, 10).unwrap(), b"abcdef");

    // The one that holds the write lock still clears them when it locks.
    w.vault.lock();
    assert!(!copy.exists());
}

#[test]
fn plaintext_an_earlier_run_left_is_cleared_by_the_one_that_holds_the_vault() {
    let w = world();
    let secret = w.vault.import_bytes(b"secret", "s.txt", &sealed(FileOrigin::Files)).unwrap();
    let copy = w.vault.open_file_copy(secret.id).unwrap();
    let ticket = w.vault.begin_upload("v.bin", 3, &sealed(FileOrigin::Files)).unwrap();
    w.vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
    let staged = staging(&w).upload_path(&ticket.upload).unwrap();
    let hour = std::time::Duration::from_secs(3600);

    // A second copy of the program beside this one cannot write, and what it
    // finds is somebody else's live copies: it leaves them be.
    let beside = karmayoga_vault::open(w.vault.path()).unwrap();
    assert!(!beside.is_writable());
    assert_eq!(beside.sweep_left_behind_plaintext(hour).unwrap(), 0);
    assert!(copy.exists() && staged.exists());

    // The one that holds the write lock has no live copies yet, so what is
    // there, however new, is from a run that is gone.
    assert_eq!(w.vault.sweep_left_behind_plaintext(hour).unwrap(), 2);
    assert!(!copy.exists() && !staged.exists());
    assert_eq!(w.vault.sweep_left_behind_plaintext(hour).unwrap(), 0);
}

// ---- bringing the old attachments in -----------------------------------------------

/// A blob of this vault, and the attachment that describes it.
fn legacy_blob(w: &World, bytes: &[u8], filename: &str, mime: &str, caption: &str) -> Attachment {
    let blob = w.vault.put_blob(bytes).unwrap();
    Attachment {
        blob,
        kind: MediaKind::from_mime(mime),
        mime: mime.into(),
        filename: filename.into(),
        byte_len: bytes.len() as u64,
        width: Some(2),
        height: Some(2),
        duration_ms: None,
        caption: caption.into(),
    }
}

fn media_node(a: &Attachment) -> serde_json::Value {
    json!({ "type": "media", "attrs": {
        "blob": a.blob.to_hex(), "kind": "image", "mime": a.mime, "filename": a.filename, "caption": ""
    } })
}

#[test]
fn old_attachments_come_into_the_folder_and_the_records_refer_to_files() {
    let w = world();
    let journal = a_journal(&w);
    let png =
        legacy_blob(&w, b"\x89PNG\r\n\x1a\n inline", "harbour.png", "image/png", "boats at dawn");
    let pdf = legacy_blob(&w, b"%PDF-1.4 attached", "ticket.pdf", "application/pdf", "");
    let note_pic = legacy_blob(&w, b"\x89PNG\r\n\x1a\n for a note", "sketch.png", "image/png", "");
    let chat_pic = legacy_blob(&w, b"\xff\xd8\xff\xe0 chat picture", "", "image/jpeg", "");

    // An entry with one picture in its body and one the body never showed.
    let mut entry = Entry::new(journal.id, "UTC");
    entry.local_date = jiff::civil::date(2020, 3, 5);
    entry.created_at = ts("2020-03-05T09:00:00Z");
    entry.updated_at = ts("2020-03-05T09:30:00Z");
    entry.body = RichDoc(json!({ "type": "doc", "content": [
        { "type": "paragraph", "content": [{ "type": "text", "text": "Harbour day" }] },
        media_node(&png),
    ] }));
    entry.attachments = vec![png.clone(), pdf.clone()];
    w.vault.overwrite_entry(&entry).unwrap();

    let mut note = Note::new("Sketches");
    note.created_at = ts("2021-01-02T09:00:00Z");
    note.updated_at = ts("2021-01-03T09:00:00Z");
    note.body = RichDoc(json!({ "type": "doc", "content": [media_node(&note_pic)] }));
    note.attachments = vec![note_pic.clone()];
    w.vault.overwrite_note(&note).unwrap();

    let conversation = Conversation::new();
    w.vault.save_conversation(&conversation).unwrap();
    let message =
        Message::user(conversation.id, "look at this").with_attachments(vec![chat_pic.clone()]);
    w.vault.save_message(&message).unwrap();

    let summary = w.vault.legacy_attachments().unwrap();
    assert_eq!((summary.entries, summary.notes, summary.messages, summary.files), (1, 1, 1, 4));
    assert_eq!(
        summary.bytes,
        [&png, &pdf, &note_pic, &chat_pic].iter().map(|a| a.byte_len).sum::<u64>()
    );

    let report = w.vault.bring_in_attachments().unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!((report.files, report.entries, report.notes, report.messages), (4, 1, 1, 1));
    assert_eq!(report.bytes, summary.bytes);

    // The entry: the body names files, the list is gone, the times are as they were.
    let entry_now = w.vault.entry(entry.id).unwrap();
    assert!(entry_now.attachments.is_empty());
    assert_eq!((entry_now.created_at, entry_now.updated_at), (entry.created_at, entry.updated_at));
    let text = entry_now.body.0.to_string();
    assert!(!text.contains(&png.blob.to_hex()) && !text.contains("\"blob\""), "{text}");
    let in_body = entry_now.body.file_refs();
    assert_eq!(in_body.len(), 2, "the picture it showed and the file it only listed");
    assert!(text.contains("boats at dawn"), "the caption moved into the media node");
    let nodes = entry_now.body.0["content"].as_array().unwrap();
    assert_eq!(nodes.len(), 3, "the listed one was put at the end");
    assert_eq!(nodes[2]["attrs"]["filename"], "ticket.pdf");
    assert_eq!(nodes[2]["attrs"]["kind"], "file");

    // The files: in the folder by the date of the record, named for the attachment.
    let in_journal = query(FileFilter { origin: Some(FileOrigin::Journal), ..Default::default() });
    let mut journal_files = w.vault.files(&in_journal).unwrap().files;
    journal_files.sort_by(|a, b| a.name.cmp(&b.name));
    let paths: Vec<&str> = journal_files.iter().map(path_of).collect();
    assert_eq!(paths, ["Journal/2020/03/harbour.png", "Journal/2020/03/ticket.pdf"]);
    assert_eq!(
        std::fs::read(w.at("Journal/2020/03/harbour.png")).unwrap(),
        b"\x89PNG\r\n\x1a\n inline"
    );
    assert_eq!(journal_files[0].caption, "boats at dawn");
    assert_eq!(journal_files[0].media.width, Some(2));
    assert_eq!(journal_files[0].kind, FileKind::Image);
    assert_eq!(journal_files[1].mime, "application/pdf");
    assert_eq!(journal_files[0].modified_at, ts("2020-03-05T09:00:00Z"), "dated by its record");
    assert_eq!(w.vault.file_uses(journal_files[0].id).unwrap().len(), 1);

    // The note and the message.
    let note_now = w.vault.note(note.id).unwrap();
    assert!(note_now.attachments.is_empty());
    assert_eq!(note_now.updated_at, note.updated_at);
    let note_files = note_now.body.file_refs();
    assert_eq!(note_files.len(), 1);
    let sketch = w.vault.file(*note_files.iter().next().unwrap()).unwrap();
    assert_eq!((sketch.origin, path_of(&sketch)), (FileOrigin::Notes, "Notes/sketch.png"));

    let messages = w.vault.messages(conversation.id).unwrap();
    assert_eq!(messages[0].attachments, vec![chat_pic.clone()], "the model's copy stays");
    assert_eq!(messages[0].files.len(), 1);
    let picture = w.vault.file(messages[0].files[0]).unwrap();
    assert_eq!(picture.origin, FileOrigin::Assistant);
    assert!(path_of(&picture).starts_with("Assistant/"));
    assert!(
        path_of(&picture).contains("/picture-"),
        "no filename: named for the blob, {}",
        path_of(&picture)
    );
    assert!(path_of(&picture).ends_with(".jpg"));

    // Done is done: nothing is left to bring in, and a second run adds nothing.
    let after = w.vault.legacy_attachments().unwrap();
    assert_eq!((after.entries, after.notes, after.messages, after.files), (0, 0, 0, 0));
    let again = w.vault.bring_in_attachments().unwrap();
    assert_eq!((again.files, again.entries, again.notes, again.messages), (0, 0, 0, 0));
    assert_eq!(w.vault.files_usage().unwrap().files, 4);
}

#[test]
fn a_blob_two_records_hold_becomes_one_file() {
    let w = world();
    let journal = a_journal(&w);
    let shared = legacy_blob(&w, b"\x89PNG\r\n\x1a\n shared", "shared.png", "image/png", "");
    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = RichDoc(json!({ "type": "doc", "content": [media_node(&shared)] }));
    entry.attachments = vec![shared.clone()];
    w.vault.overwrite_entry(&entry).unwrap();
    let mut note = Note::new("same picture");
    note.body = RichDoc(json!({ "type": "doc", "content": [media_node(&shared)] }));
    note.attachments = vec![shared.clone()];
    w.vault.overwrite_note(&note).unwrap();

    assert_eq!(w.vault.legacy_attachments().unwrap().files, 1);
    let report = w.vault.bring_in_attachments().unwrap();
    assert_eq!((report.files, report.entries, report.notes), (1, 1, 1));
    let from_entry = w.vault.entry(entry.id).unwrap().body.file_refs();
    let from_note = w.vault.note(note.id).unwrap().body.file_refs();
    assert_eq!(from_entry, from_note);
    let file = w.vault.file(*from_entry.iter().next().unwrap()).unwrap();
    assert_eq!(w.vault.file_uses(file.id).unwrap().len(), 2);
}

#[test]
fn an_app_whose_files_are_sealed_keeps_its_old_attachments_sealed() {
    let w = world();
    let settings = FilesSettings {
        folder: Some(w.folder.display().to_string()),
        encrypted: vec![FileOrigin::Journal],
        ..Default::default()
    };
    w.vault.save_files_settings(&settings).unwrap();
    let journal = a_journal(&w);
    let png = legacy_blob(&w, b"\x89PNG\r\n\x1a\n private", "private.png", "image/png", "");
    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = RichDoc(json!({ "type": "doc", "content": [media_node(&png)] }));
    entry.attachments = vec![png.clone()];
    w.vault.overwrite_entry(&entry).unwrap();

    let report = w.vault.bring_in_attachments().unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!((report.files, report.entries), (1, 1));
    assert!(w.listing().is_empty(), "nothing was written to the folder");
    let id = *w.vault.entry(entry.id).unwrap().body.file_refs().iter().next().unwrap();
    let file = w.vault.file(id).unwrap();
    assert!(matches!(file.storage, FileStorage::Sealed { blob } if blob == png.blob));
    assert_eq!(w.vault.read_file_range(id, 0, 100).unwrap(), b"\x89PNG\r\n\x1a\n private");
}

#[test]
fn a_blob_the_vault_no_longer_holds_is_reported_and_its_record_left_alone() {
    let w = world();
    let journal = a_journal(&w);
    let ghost = Attachment {
        blob: karmayoga_core::BlobId::of(b"never stored"),
        kind: MediaKind::Image,
        mime: "image/png".into(),
        filename: "ghost.png".into(),
        byte_len: 12,
        width: None,
        height: None,
        duration_ms: None,
        caption: String::new(),
    };
    let mut entry = Entry::new(journal.id, "UTC");
    entry.body = RichDoc(json!({ "type": "doc", "content": [media_node(&ghost)] }));
    entry.attachments = vec![ghost.clone()];
    w.vault.overwrite_entry(&entry).unwrap();

    let report = w.vault.bring_in_attachments().unwrap();
    assert_eq!((report.files, report.entries), (0, 0));
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(w.vault.entry(entry.id).unwrap().attachments, vec![ghost]);
}
