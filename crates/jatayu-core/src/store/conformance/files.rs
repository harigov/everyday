//! The files half of the conformance suite: the catalog of files, which
//! records refer to which, albums, and the settings.
//!
//! One check per rule in [`crate::store::files`]'s module doc, and a round
//! trip of every field of every record. One of those rules cannot be checked
//! from here, for the reason Money's and screen time's suites give: that a
//! payload copied into another row will not open there. The trait offers no
//! way to read or write a raw row, and inventing one for a test would be a
//! door the running app does not need. It is checked where the rows are, in
//! the SQL backend's own tests and the SQLite driver's, which tie each clear
//! column to the schema and move a sealed record into another row to watch it
//! fail.
//!
//! What a sealed file keeps in the clear can only be seen from here by what
//! it is *not* found by: a sealed file has no path, so no path finds it; no
//! hash, so no hash does; and no time of its own, so it is filed by when it
//! was added. Those are what the clear-column policy comes to, and they are
//! checked as such.

use super::*;
use crate::files::{
    Album, FileEntry, FileFilter, FileKind, FileOrigin, FileSort, FileStatus, FileStorage,
    FilesSettings, FilesUsage, GeoPoint, IndexRow, KindUsage, MediaInfo, PropValue, RefKind,
    RefOwner, StatusFilter, StorageKind,
};
use crate::id::{AlbumId, FileId};
use crate::store::files::FilesStore;
use jiff::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

/// The files half of the suite. Called by [`run_all`] when the backend has a
/// [`FilesStore`]; public so a backend under construction can run it alone.
///
/// The store must hold no files, albums or references on entry; it is left
/// with none on success, and with the settings at their defaults.
pub fn run_files_suite(store: &dyn JournalStore) {
    eprintln!("--- files conformance suite ---");
    let files = store.files().expect("run_files_suite needs a files store");

    files_start_empty(files);
    missing_file_records_are_not_found(files);
    a_plain_file_round_trips_every_field(files);
    a_file_in_each_status_round_trips(files);
    a_sealed_file_round_trips_and_has_no_clear_path_hash_or_time(files);
    putting_a_file_again_replaces_it_by_id(files);
    a_plain_files_path_is_unique_among_files_not_in_the_trash(files);
    a_batch_is_all_or_nothing(files);
    a_large_batch_lands_whole(files);
    files_by_hash_are_plain_files_with_those_contents(files);
    every_filter_asks_for_exactly_what_it_says(files);
    every_sort_orders_both_ways_with_ties_broken_by_id(files);
    pages_neither_overlap_nor_skip(files);
    the_index_lists_plain_files_not_in_the_trash(files);
    usage_counts_what_the_catalog_holds(files);
    blob_refs_are_every_sealed_contents_and_thumbnail(files);
    references_are_replaced_one_owner_at_a_time(files);
    deleting_a_file_takes_its_references_and_memberships(files);
    albums_keep_their_files_in_the_order_they_were_added(files);
    deleting_an_album_never_deletes_a_file(files);
    settings_default_until_saved_and_round_trip(files);
    unicode_and_large_sizes_survive_a_files_round_trip(files);

    files_cleanup(files);
    eprintln!("--- files suite passed ---");
}

/// A sealed file's contents and every file's thumbnail are blobs held by
/// nothing but the file's record, so the sweep has to know about them.
///
/// Not part of the files suite, for the reason the library's and the
/// agent's are not: garbage collection walks every record that can hold a
/// blob, and that is a question about the *journal* store. Called by
/// [`run_all`].
pub(super) fn garbage_collection_keeps_sealed_files_and_thumbnails(store: &dyn JournalStore) {
    let Some(files) = store.files() else { return };

    let contents = store.put_blob(b"a film somebody keeps encrypted").unwrap();
    let poster = store.put_blob(b"the film's poster frame").unwrap();
    let thumb = store.put_blob(b"a plain file's thumbnail").unwrap();
    let orphan = store.put_blob(b"nobody's").unwrap();

    let film = with(sealed("film.mp4"), |f| {
        f.storage = FileStorage::Sealed { blob: contents };
        f.thumbnail = Some(poster);
    });
    let picture = with(plain("Photos/pic.jpg"), |f| f.thumbnail = Some(thumb));
    files.put_files(&[film.clone(), picture.clone()]).unwrap();

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(store.has_blob(contents).unwrap(), "a sealed file's contents are a live reference");
    assert!(store.has_blob(poster).unwrap(), "so is its thumbnail");
    assert!(store.has_blob(thumb).unwrap(), "and a plain file's thumbnail");
    assert!(!store.has_blob(orphan).unwrap());

    // A thumbnail the record no longer names is garbage again...
    let mut without = picture.clone();
    without.thumbnail = None;
    files.put_file(&without).unwrap();
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 1);
    assert!(!store.has_blob(thumb).unwrap());
    assert!(store.has_blob(contents).unwrap());

    // ...and so is everything when the files go.
    files.delete_file(film.id).unwrap();
    files.delete_file(picture.id).unwrap();
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 2);
    assert!(!store.has_blob(contents).unwrap() && !store.has_blob(poster).unwrap());
}

// ---- fixtures -----------------------------------------------------------

/// Every time in this suite is whole seconds from this one, so "an hour ago"
/// means the same thing on every run, and a time that comes back from a
/// clear column (whole microseconds) compares equal to the one that went in.
const NOW: i64 = 1_800_000_000;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("a representable time")
}

/// `NOW + seconds`.
fn t(seconds: i64) -> Timestamp {
    at(NOW + seconds)
}

/// A stand-in for the BLAKE3 hash of a file's contents: 64 hex digits that
/// are the same for the same `seed`.
fn hash_of(seed: &str) -> String {
    BlobId::of(seed.as_bytes()).to_hex()
}

/// The place in the trash a file called `name` is moved to.
fn trash_path(name: &str) -> String {
    format!(".jatayu/trash/ab/{name}")
}

fn blank(storage: FileStorage, name: &str) -> FileEntry {
    FileEntry {
        id: FileId::new(),
        storage,
        name: name.to_string(),
        kind: FileKind::Other,
        mime: "application/octet-stream".into(),
        size: 0,
        hash: String::new(),
        modified_at: t(0),
        added_at: t(0),
        updated_at: t(0),
        origin: FileOrigin::Files,
        status: FileStatus::Present,
        media: MediaInfo::default(),
        thumbnail: None,
        caption: String::new(),
        tags: Vec::new(),
        rating: None,
        favourite: false,
        hidden: false,
        people: Vec::new(),
        place: None,
        stack: None,
        properties: BTreeMap::new(),
    }
}

/// A plain file at `path`, called what the path's last segment is, with a
/// hash of its own.
fn plain(path: &str) -> FileEntry {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut file = blank(FileStorage::Plain { path: path.to_string() }, name);
    file.hash = hash_of(path);
    file
}

/// A file sealed in the vault, called `name`.
fn sealed(name: &str) -> FileEntry {
    blank(FileStorage::Sealed { blob: BlobId::of(name.as_bytes()) }, name)
}

fn with(mut file: FileEntry, edit: impl FnOnce(&mut FileEntry)) -> FileEntry {
    edit(&mut file);
    file
}

/// Move a plain file to `path`, keeping its name in step.
fn moved_to(file: &FileEntry, path: &str) -> FileEntry {
    let mut moved = file.clone();
    moved.name = path.rsplit('/').next().unwrap_or(path).to_string();
    moved.storage = FileStorage::Plain { path: path.to_string() };
    moved
}

fn trashed(file: &FileEntry, from: &str) -> FileEntry {
    let name = file.name.clone();
    with(moved_to(file, &trash_path(&name)), |f| {
        f.status = FileStatus::Trashed { at: t(5), from: from.to_string() };
    })
}

fn micros(ts: Timestamp) -> i64 {
    ts.as_microsecond()
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items
}

fn ids_of(files: &[&FileEntry]) -> Vec<FileId> {
    files.iter().map(|f| f.id).collect()
}

/// Every file there is, in any status.
fn everything() -> FileFilter {
    FileFilter { status: StatusFilter::All, include_hidden: true, ..FileFilter::default() }
}

fn every_file(files: &dyn FilesStore) -> Vec<FileEntry> {
    files.query_files(&everything()).expect("query_files")
}

/// What `filter` lists, as ids in the order listed -- after checking that
/// `count_files` counts what the same filter would list without its page.
fn found(files: &dyn FilesStore, filter: &FileFilter) -> Vec<FileId> {
    let page = files.query_files(filter).expect("query_files");
    let whole = FileFilter { limit: None, offset: 0, ..filter.clone() };
    let all = files.query_files(&whole).expect("query_files");
    assert_eq!(
        files.count_files(filter).expect("count_files"),
        all.len() as u64,
        "count_files must count what the same filter lists, ignoring limit and offset: {filter:?}"
    );
    page.iter().map(|f| f.id).collect()
}

/// `filter` lists exactly `want`, in any order.
fn expect(files: &dyn FilesStore, filter: FileFilter, want: &[&FileEntry], what: &str) {
    assert_eq!(sorted(found(files, &filter)), sorted(ids_of(want)), "{what}");
}

/// Every file, album, reference and the settings gone, and checked gone.
fn clear_files(files: &dyn FilesStore) {
    for file in every_file(files) {
        files.delete_file(file.id).expect("delete_file");
    }
    for album in files.list_albums().expect("list_albums") {
        files.delete_album(album.id).expect("delete_album");
    }
    files.replace_all_refs(&[]).expect("replace_all_refs");
    assert!(every_file(files).is_empty(), "a sweep of every file left files behind");
    assert!(files.list_albums().unwrap().is_empty(), "and albums");
    assert!(files.referenced_files().unwrap().is_empty(), "and references");
}

fn files_start_empty(files: &dyn FilesStore) {
    assert!(every_file(files).is_empty(), "a fresh store must hold no files");
    assert_eq!(files.count_files(&everything()).unwrap(), 0);
    assert!(files.file_index().unwrap().is_empty());
    assert_eq!(files.files_usage().unwrap(), FilesUsage::default());
    assert!(files.file_blob_refs().unwrap().is_empty());
    assert!(files.referenced_files().unwrap().is_empty());
    assert!(files.list_albums().unwrap().is_empty(), "nor any albums");
    assert!(files.album_counts().unwrap().is_empty());
    assert_eq!(files.files_settings().unwrap(), FilesSettings::default());
}

fn files_cleanup(files: &dyn FilesStore) {
    clear_files(files);
    files.put_files_settings(&FilesSettings::default()).expect("put_files_settings");
    assert_eq!(files.files_settings().unwrap(), FilesSettings::default());
    assert_eq!(files.count_files(&everything()).unwrap(), 0, "cleanup left files behind");
    assert!(files.file_index().unwrap().is_empty());
    assert!(files.file_blob_refs().unwrap().is_empty(), "cleanup left blob references behind");
    assert!(files.album_counts().unwrap().is_empty());
}

fn missing_file_records_are_not_found(files: &dyn FilesStore) {
    super::assert_not_found(files.get_file(FileId::new()));
    super::assert_not_found(files.get_album(AlbumId::new()));
    assert!(files.get_files(&[FileId::new()]).unwrap().is_empty(), "ids not found are left out");
    assert!(files.get_files(&[]).unwrap().is_empty());
    assert!(files.file_by_path("no/such/file.txt").unwrap().is_none());
    assert!(files.files_by_hash(&hash_of("nobody")).unwrap().is_empty());
    assert!(files.refs_to(FileId::new()).unwrap().is_empty());
    assert!(files.albums_of(FileId::new()).unwrap().is_empty());

    // Deleting what is not there is not an error, as it is not elsewhere.
    files.delete_file(FileId::new()).expect("delete_file of nothing");
    files.delete_album(AlbumId::new()).expect("delete_album of nothing");
    files.put_files(&[]).expect("an empty batch is nothing to do");
    files.remove_from_album(AlbumId::new(), &[FileId::new()]).expect("nothing to remove");
    files.add_to_album(AlbumId::new(), &[]).expect("nothing to add");
    files.set_refs(&RefOwner::new(RefKind::Entry, "nobody"), &[]).expect("nothing to forget");

    // A membership of an album that is not there would be one nothing could
    // list or delete.
    super::assert_not_found(files.add_to_album(AlbumId::new(), &[FileId::new()]));
    assert!(every_file(files).is_empty());
}

fn a_plain_file_round_trips_every_field(files: &dyn FilesStore) {
    let mut full = plain("Photos/2024/Trip to the lake.jpg");
    full.kind = FileKind::Image;
    full.mime = "image/jpeg".into();
    full.size = 3_456_789;
    full.modified_at = t(-86_400);
    full.added_at = t(-3_600);
    full.updated_at = t(-60);
    full.origin = FileOrigin::Journal;
    full.media = MediaInfo {
        width: Some(4032),
        height: Some(3024),
        duration_ms: Some(12_345),
        taken_at: Some(t(-172_800)),
        taken_local: Some("2026-03-01T14:22:05".into()),
        camera_make: Some("Fujifilm".into()),
        camera_model: Some("X100V".into()),
        lens: Some("23mm F2".into()),
        f_number: Some(2.8),
        exposure_time: Some("1/250".into()),
        iso: Some(400),
        focal_length_mm: Some(23.0),
        orientation: Some(6),
        gps: Some(GeoPoint { lat: 45.8992, lon: 6.1294, alt: Some(448.5) }),
        codec: Some("avc1".into()),
        pages: Some(3),
    };
    full.thumbnail = Some(BlobId::of(b"a thumbnail"));
    full.caption = "The first morning.\nIt rained, then it did not.".into();
    full.tags = vec!["lake".into(), "summer".into()];
    full.rating = Some(5);
    full.favourite = true;
    full.hidden = true;
    full.people = vec!["Asha".into(), "Ravi".into()];
    full.place = Some("Lac d'Annecy".into());
    full.stack = Some(FileId::new());
    full.properties = BTreeMap::from([
        ("user.mood".to_string(), PropValue::Text("calm".into())),
        ("photos.score".to_string(), PropValue::Number(0.875)),
        ("user.reviewed".to_string(), PropValue::Bool(true)),
        ("user.shot".to_string(), PropValue::Date("2026-03-01".into())),
        ("user.pals".to_string(), PropValue::List(vec!["a".into(), "b".into()])),
    ]);

    files.put_file(&full).unwrap();
    assert_eq!(files.get_file(full.id).unwrap(), full, "every field comes back");
    assert_eq!(files.get_files(&[full.id]).unwrap(), vec![full.clone()]);
    assert_eq!(files.file_by_path("Photos/2024/Trip to the lake.jpg").unwrap(), Some(full.clone()));
    assert_eq!(files.files_by_hash(&full.hash).unwrap(), vec![full.clone()]);
    assert_eq!(files.query_files(&everything()).unwrap(), vec![full.clone()]);
    assert!(
        found(files, &FileFilter::default()).is_empty(),
        "a hidden file is left out of a listing unless it is asked for"
    );

    // What the index reads of it comes from the clear columns, and is what
    // the disk says.
    assert_eq!(
        files.file_index().unwrap(),
        vec![IndexRow {
            id: full.id,
            path: "Photos/2024/Trip to the lake.jpg".into(),
            size: 3_456_789,
            modified_us: micros(full.modified_at),
            hash: full.hash.clone(),
            missing: false,
        }]
    );

    // A file with nothing optional set comes back with nothing set.
    let bare = plain("bare.txt");
    files.put_file(&bare).unwrap();
    assert_eq!(files.get_file(bare.id).unwrap(), bare);

    files.delete_file(full.id).unwrap();
    files.delete_file(bare.id).unwrap();
    assert!(every_file(files).is_empty());
}

fn a_file_in_each_status_round_trips(files: &dyn FilesStore) {
    let present = plain("Docs/present.txt");
    let missing = with(plain("Docs/missing.txt"), |f| {
        f.status = FileStatus::Missing { since: t(-7_200) };
    });
    let gone = trashed(&plain("Docs/gone.txt"), "Docs/gone.txt");
    let all = [&present, &missing, &gone];
    for file in all {
        files.put_file(file).unwrap();
    }
    for file in all {
        assert_eq!(&files.get_file(file.id).unwrap(), file, "{:?}", file.status);
    }
    // A trashed file's path is its place in the trash, and is found there.
    assert_eq!(files.file_by_path(&trash_path("gone.txt")).unwrap(), Some(gone.clone()));
    assert!(files.file_by_path("Docs/gone.txt").unwrap().is_none(), "it is not where it was");
    clear_files(files);
}

fn a_sealed_file_round_trips_and_has_no_clear_path_hash_or_time(files: &dyn FilesStore) {
    let mut passport = sealed("passport.pdf");
    passport.kind = FileKind::Document;
    passport.mime = "application/pdf".into();
    passport.size = 88_000;
    passport.hash = hash_of("the passport");
    passport.modified_at = t(-500);
    passport.added_at = t(0);
    passport.origin = FileOrigin::Mail;
    passport.media.pages = Some(2);
    // Taken long before anything here was added, so that a window around
    // when it was taken would find it if the store knew.
    passport.media.taken_at = Some(at(1_500_000_000));
    passport.thumbnail = Some(BlobId::of(b"the passport's first page"));
    passport.caption = "For the visa".into();
    passport.tags = vec!["travel".into()];
    passport.rating = Some(2);
    passport.favourite = true;
    passport.people = vec!["Asha".into()];

    files.put_file(&passport).unwrap();
    assert_eq!(files.get_file(passport.id).unwrap(), passport, "every field comes back");
    assert_eq!(files.query_files(&everything()).unwrap(), vec![passport.clone()]);

    // No path to find it by, in the folder or out of it.
    assert!(files.file_by_path("passport.pdf").unwrap().is_none());
    assert!(files.file_by_path("").unwrap().is_none());
    // No hash: a sealed file's contents are not something a database holder
    // may match against a file they already have.
    assert!(
        files.files_by_hash(&passport.hash).unwrap().is_empty(),
        "a sealed file's hash is sealed"
    );
    // So the reconciler never sees it.
    assert!(files.file_index().unwrap().is_empty());
    // It is in no folder, however a folder is asked for.
    for dir in [Some(""), Some("passport.pdf")] {
        let by_dir = FileFilter { dir: dir.map(String::from), ..FileFilter::default() };
        assert!(found(files, &by_dir).is_empty(), "a sealed file is in no folder: {dir:?}");
    }
    let under_top = FileFilter { under: Some(String::new()), ..FileFilter::default() };
    assert!(found(files, &under_top).is_empty(), "nor under the top one");

    // The clear marks it does keep: it is found by what it is...
    let sealed_only = FileFilter { storage: Some(StorageKind::Sealed), ..FileFilter::default() };
    assert_eq!(found(files, &sealed_only), vec![passport.id]);
    let plain_only = FileFilter { storage: Some(StorageKind::Plain), ..FileFilter::default() };
    assert!(found(files, &plain_only).is_empty());
    let marked = FileFilter { favourite: true, min_rating: Some(2), ..FileFilter::default() };
    assert_eq!(found(files, &marked), vec![passport.id], "rating and favourite are clear");

    // ...and its time is when it was added: a window around then finds it,
    // and a window around when it was taken does not, because that is sealed.
    let when_added = FileFilter { from: Some(t(-10)), to: Some(t(10)), ..FileFilter::default() };
    assert_eq!(found(files, &when_added), vec![passport.id]);
    let when_taken = FileFilter {
        from: Some(at(1_499_999_000)),
        to: Some(at(1_500_001_000)),
        ..FileFilter::default()
    };
    assert!(found(files, &when_taken).is_empty(), "a sealed file's time taken is not a column");

    // A plain file with the same hash is the only one that hash finds.
    let twin = with(plain("Docs/twin.pdf"), |f| f.hash = passport.hash.clone());
    files.put_file(&twin).unwrap();
    assert_eq!(files.files_by_hash(&passport.hash).unwrap(), vec![twin.clone()]);

    // Two sealed files have no path to clash over.
    let other = sealed("passport.pdf");
    files.put_file(&other).unwrap();
    assert_eq!(files.count_files(&sealed_only).unwrap(), 2);

    clear_files(files);
}

fn putting_a_file_again_replaces_it_by_id(files: &dyn FilesStore) {
    let mut file = plain("Photos/move.jpg");
    file.kind = FileKind::Image;
    files.put_file(&file).unwrap();

    // What the person adds, and what the clear columns follow.
    file.caption = "edited".into();
    file.rating = Some(2);
    file.favourite = true;
    file.hidden = true;
    file.origin = FileOrigin::Journal;
    file.updated_at = t(60);
    files.put_file(&file).unwrap();
    assert_eq!(files.count_files(&everything()).unwrap(), 1, "put must not duplicate");
    assert_eq!(files.get_file(file.id).unwrap(), file);

    let marks = |favourite, min_rating, include_hidden| FileFilter {
        favourite,
        min_rating,
        include_hidden,
        ..FileFilter::default()
    };
    assert_eq!(found(files, &marks(true, Some(2), true)), vec![file.id]);
    assert!(found(files, &marks(true, Some(3), true)).is_empty(), "its rating moved in the index");
    assert!(found(files, &marks(true, None, false)).is_empty(), "and so did hiding it");
    let by_origin = FileFilter {
        origin: Some(FileOrigin::Journal),
        include_hidden: true,
        ..FileFilter::default()
    };
    assert_eq!(found(files, &by_origin), vec![file.id]);

    // A move: the same id at another path, which frees the one it had.
    let moved = with(moved_to(&file, "Videos/moved.mp4"), |f| f.kind = FileKind::Video);
    files.put_file(&moved).unwrap();
    assert_eq!(files.count_files(&everything()).unwrap(), 1);
    assert!(files.file_by_path("Photos/move.jpg").unwrap().is_none());
    assert_eq!(files.file_by_path("Videos/moved.mp4").unwrap(), Some(moved.clone()));
    let in_old = FileFilter { dir: Some("Photos".into()), include_hidden: true, ..everything() };
    let in_new = FileFilter { dir: Some("Videos".into()), include_hidden: true, ..everything() };
    assert!(found(files, &in_old).is_empty(), "it left its folder");
    assert_eq!(found(files, &in_new), vec![file.id]);
    let by_kind = FileFilter { kinds: vec![FileKind::Video], ..everything() };
    assert_eq!(found(files, &by_kind), vec![file.id], "and its kind moved in the index");
    let free = plain("Photos/move.jpg");
    files.put_file(&free).expect("the path it left is somebody else's to take");

    // Sealed into the vault: it has no path and no hash any more, and its
    // contents are a blob nothing may collect.
    let blob = BlobId::of(b"moved.mp4 sealed");
    let vaulted = with(moved.clone(), |f| f.storage = FileStorage::Sealed { blob });
    files.put_file(&vaulted).unwrap();
    assert!(files.file_by_path("Videos/moved.mp4").unwrap().is_none());
    assert!(files.file_blob_refs().unwrap().contains(&blob));
    assert_eq!(files.file_index().unwrap().len(), 1, "only the file at Photos/move.jpg remains");
    // And back out into the folder.
    files.put_file(&moved).unwrap();
    assert_eq!(files.file_by_path("Videos/moved.mp4").unwrap(), Some(moved));
    assert!(!files.file_blob_refs().unwrap().contains(&blob));

    clear_files(files);
}

fn a_plain_files_path_is_unique_among_files_not_in_the_trash(files: &dyn FilesStore) {
    let first = plain("Photos/a.jpg");
    let second = plain("Photos/a.jpg");
    files.put_file(&first).unwrap();

    let err = files.put_file(&second).expect_err("a second file at one path");
    assert_eq!(err.code(), "conflict", "got {err:?}");
    super::assert_not_found(files.get_file(second.id));
    assert_eq!(files.file_by_path("Photos/a.jpg").unwrap().unwrap().id, first.id);
    assert_eq!(files.count_files(&everything()).unwrap(), 1, "the refused write left nothing");

    // The same file may be written again at its own path.
    let edited = with(first.clone(), |f| f.caption = "edited".into());
    files.put_file(&edited).unwrap();
    assert_eq!(files.get_file(first.id).unwrap(), edited);

    // Into the trash: its path is its place there, and the one it had is
    // free for another file.
    let in_trash = trashed(&first, "Photos/a.jpg");
    files.put_file(&in_trash).unwrap();
    files.put_file(&second).expect("a path whose only holder is in the trash is free");
    assert_eq!(files.file_by_path("Photos/a.jpg").unwrap().unwrap().id, second.id);
    assert_eq!(
        files.file_by_path(&trash_path("a.jpg")).unwrap().unwrap().id,
        first.id,
        "file_by_path finds a trashed file at its place in the trash"
    );

    // Putting it back is refused while somebody else holds the path, and the
    // trashed record is as it was.
    let restored = with(moved_to(&first, "Photos/a.jpg"), |f| f.caption = "edited".into());
    let err = files.put_file(&restored).expect_err("restoring over a file at the path");
    assert_eq!(err.code(), "conflict");
    assert_eq!(files.get_file(first.id).unwrap(), in_trash);
    files.delete_file(second.id).unwrap();
    files.put_file(&restored).expect("restoring to a free path");
    assert_eq!(files.file_by_path("Photos/a.jpg").unwrap().unwrap().id, first.id);

    // Different paths never clash, and neither do names in other folders.
    files.put_file(&plain("Photos/b.jpg")).unwrap();
    files.put_file(&plain("Other/a.jpg")).unwrap();
    // A path that differs only in case is another path.
    files.put_file(&plain("photos/a.jpg")).unwrap();
    assert_eq!(files.count_files(&everything()).unwrap(), 4);

    clear_files(files);
}

fn a_batch_is_all_or_nothing(files: &dyn FilesStore) {
    let held = plain("Held/x.jpg");
    files.put_file(&held).unwrap();

    // One of the batch clashes with a file already stored: none of it lands.
    let fresh = plain("Fresh/y.jpg");
    let err = files
        .put_files(&[fresh.clone(), plain("Held/x.jpg")])
        .expect_err("a batch with one file at a taken path");
    assert_eq!(err.code(), "conflict");
    super::assert_not_found(files.get_file(fresh.id));

    // Two of the batch want one path.
    let (one, two) = (plain("Twin/z.jpg"), plain("Twin/z.jpg"));
    let err = files.put_files(&[one.clone(), two.clone()]).expect_err("two files, one path");
    assert_eq!(err.code(), "conflict");
    super::assert_not_found(files.get_file(one.id));
    super::assert_not_found(files.get_file(two.id));

    // A file that moves off a path and another that takes it, in an order
    // that frees it first: accepted, as the table would take them one by one.
    let away = moved_to(&held, "Elsewhere/x.jpg");
    let taker = plain("Held/x.jpg");
    files.put_files(&[away.clone(), taker.clone()]).expect("free the path, then take it");
    assert_eq!(files.file_by_path("Held/x.jpg").unwrap().unwrap().id, taker.id);
    assert_eq!(files.file_by_path("Elsewhere/x.jpg").unwrap().unwrap().id, held.id);

    // The other way round it is a clash, and nothing is changed.
    let back = moved_to(&away, "Held/x.jpg");
    let ahead = moved_to(&taker, "Ahead/x.jpg");
    let err = files.put_files(&[back, ahead]).expect_err("taking a path before it is freed");
    assert_eq!(err.code(), "conflict");
    assert_eq!(files.get_file(held.id).unwrap(), away);
    assert_eq!(files.get_file(taker.id).unwrap(), taker);

    // Many at once, each a file of its own.
    let batch: Vec<FileEntry> = (0..5).map(|n| plain(&format!("Batch/{n}.dat"))).collect();
    files.put_files(&batch).unwrap();
    let ids: Vec<FileId> = batch.iter().map(|f| f.id).collect();
    assert_eq!(files.get_files(&ids).unwrap().len(), 5);

    clear_files(files);
}

fn a_large_batch_lands_whole(files: &dyn FilesStore) {
    // More than an `IN (...)` chunk, so a store that chunks its lookups has
    // to do it more than once, and has to get the answer the same.
    let batch: Vec<FileEntry> = (0..520).map(|n| plain(&format!("Bulk/{n:05}.dat"))).collect();
    files.put_files(&batch).unwrap();
    let ids: Vec<FileId> = batch.iter().map(|f| f.id).collect();

    assert_eq!(files.count_files(&everything()).unwrap(), 520);
    let stored: Vec<FileId> = files.get_files(&ids).unwrap().iter().map(|f| f.id).collect();
    assert_eq!(sorted(stored), sorted(ids));
    assert_eq!(files.file_index().unwrap().len(), 520);
    let under = FileFilter { under: Some("Bulk".into()), ..FileFilter::default() };
    assert_eq!(files.count_files(&under).unwrap(), 520);

    // The whole batch again, as the reconciler writes a pass it has seen
    // before: the same ids at the same paths, not a clash with themselves.
    let again: Vec<FileEntry> =
        batch.iter().map(|f| with(f.clone(), |f| f.caption = "seen".into())).collect();
    files.put_files(&again).expect("rewriting 520 files at their own paths");
    assert_eq!(files.count_files(&everything()).unwrap(), 520);

    clear_files(files);
}

fn files_by_hash_are_plain_files_with_those_contents(files: &dyn FilesStore) {
    let hash = hash_of("one photo, three copies");
    let original = with(plain("Photos/original.jpg"), |f| f.hash = hash.clone());
    let copy = with(plain("Backup/copy.jpg"), |f| f.hash = hash.clone());
    let lost = with(plain("Photos/lost.jpg"), |f| {
        f.hash = hash.clone();
        f.status = FileStatus::Missing { since: t(-10) };
    });
    let other = plain("Photos/other.jpg");
    for file in [&original, &copy, &lost, &other] {
        files.put_file(file).unwrap();
    }

    // Every file with those contents, whatever its status: a file that moved
    // while nobody watched is recognised by this, whether the one it was is
    // missing or not.
    let got = files.files_by_hash(&hash).unwrap();
    assert_eq!(
        sorted(got.iter().map(|f| f.id).collect::<Vec<FileId>>()),
        sorted(ids_of(&[&original, &copy, &lost]))
    );
    assert_eq!(files.files_by_hash(&other.hash).unwrap(), vec![other.clone()]);

    // A file nobody has read has no hash to match, and the empty hash is not
    // every such file's.
    let unread_a = with(plain("Unread/a.bin"), |f| f.hash.clear());
    let unread_b = with(plain("Unread/b.bin"), |f| f.hash.clear());
    files.put_files(&[unread_a.clone(), unread_b]).unwrap();
    assert!(files.files_by_hash("").unwrap().is_empty());
    assert_eq!(files.get_file(unread_a.id).unwrap(), unread_a, "and it comes back unread");
    let unread_index: Vec<IndexRow> =
        files.file_index().unwrap().into_iter().filter(|r| r.id == unread_a.id).collect();
    assert_eq!(unread_index.len(), 1);
    assert_eq!(unread_index[0].hash, "", "an unread file's hash is empty in the index");

    clear_files(files);
}

// ---- listings -----------------------------------------------------------

/// The files the filter and sort checks run against.
///
/// By `when`: `gone` 10, `clip` 50, `lake` and `raw` 100 (a tie), `scan` 150
/// (sealed, so by when it was added), `hills` 200, `notes` 250, `harbour`
/// 300, `report` 400, `film` 500 (sealed, and taken years before any of
/// this, which the store does not know).
struct Seed {
    lake: FileEntry,
    hills: FileEntry,
    harbour: FileEntry,
    report: FileEntry,
    clip: FileEntry,
    notes: FileEntry,
    gone: FileEntry,
    film: FileEntry,
    scan: FileEntry,
    raw: FileEntry,
    trip: Album,
}

impl Seed {
    /// Every file in the seed.
    fn all(&self) -> Vec<&FileEntry> {
        vec![
            &self.lake,
            &self.hills,
            &self.harbour,
            &self.report,
            &self.clip,
            &self.notes,
            &self.gone,
            &self.film,
            &self.scan,
            &self.raw,
        ]
    }

    /// What a listing with no filter shows: not in the trash, not hidden.
    fn visible(&self) -> Vec<&FileEntry> {
        vec![
            &self.lake,
            &self.hills,
            &self.harbour,
            &self.report,
            &self.notes,
            &self.film,
            &self.scan,
            &self.raw,
        ]
    }
}

fn seed(files: &dyn FilesStore) -> Seed {
    let lake = with(plain("Photos/2024/lake.jpg"), |f| {
        f.kind = FileKind::Image;
        f.size = 300;
        f.origin = FileOrigin::Files;
        f.media.taken_at = Some(t(100));
        f.modified_at = t(9_000);
        f.added_at = t(10);
        f.rating = Some(5);
        f.favourite = true;
    });
    let hills = with(plain("Photos/2024/Hills.jpg"), |f| {
        f.kind = FileKind::Image;
        f.size = 100;
        f.origin = FileOrigin::Files;
        f.media.taken_at = Some(t(200));
        f.modified_at = t(9_001);
        f.added_at = t(20);
        f.rating = Some(3);
    });
    let harbour = with(plain("Photos/harbour.png"), |f| {
        f.kind = FileKind::Image;
        f.size = 200;
        f.origin = FileOrigin::Journal;
        f.media.taken_at = Some(t(300));
        f.modified_at = t(9_002);
        f.added_at = t(30);
        f.rating = Some(1);
    });
    let report = with(plain("Docs/report.pdf"), |f| {
        f.kind = FileKind::Document;
        f.size = 500;
        f.origin = FileOrigin::Mail;
        f.modified_at = t(400);
        f.added_at = t(40);
    });
    let clip = with(plain("clip.mp4"), |f| {
        f.kind = FileKind::Video;
        f.size = 900;
        f.origin = FileOrigin::Found;
        f.modified_at = t(50);
        f.added_at = t(50);
        f.hidden = true;
    });
    let notes = with(plain("Docs/Old/notes.txt"), |f| {
        f.kind = FileKind::Document;
        f.size = 70;
        f.origin = FileOrigin::Notes;
        f.modified_at = t(250);
        f.added_at = t(60);
        f.status = FileStatus::Missing { since: t(100) };
    });
    let gone = with(trashed(&plain("Photos/gone.jpg"), "Photos/gone.jpg"), |f| {
        f.kind = FileKind::Image;
        f.size = 40;
        f.origin = FileOrigin::Files;
        f.media.taken_at = Some(t(10));
        f.modified_at = t(9_003);
        f.added_at = t(70);
    });
    let film = with(sealed("film.mp4"), |f| {
        f.kind = FileKind::Video;
        f.size = 5_000;
        f.origin = FileOrigin::Assistant;
        f.media.taken_at = Some(at(1_500_000_000));
        f.modified_at = t(1);
        f.added_at = t(500);
        f.rating = Some(4);
        f.favourite = true;
        f.thumbnail = Some(BlobId::of(b"film poster"));
    });
    let scan = with(sealed("scan.pdf"), |f| {
        f.kind = FileKind::Document;
        f.size = 70;
        f.origin = FileOrigin::Chat;
        f.modified_at = t(2);
        f.added_at = t(150);
    });
    let raw = with(plain("Photos/2024/lake.dng"), |f| {
        f.kind = FileKind::Image;
        f.size = 400;
        f.origin = FileOrigin::Found;
        f.media.taken_at = Some(t(100));
        f.modified_at = t(9_004);
        f.added_at = t(80);
        f.stack = Some(lake.id);
    });
    let trip = Album {
        id: AlbumId::new(),
        name: "Trip".into(),
        description: String::new(),
        cover: None,
        created_at: t(0),
        updated_at: t(0),
    };

    let seed = Seed { lake, hills, harbour, report, clip, notes, gone, film, scan, raw, trip };
    let batch: Vec<FileEntry> = seed.all().into_iter().cloned().collect();
    files.put_files(&batch).unwrap();
    files.put_album(&seed.trip).unwrap();
    files
        .add_to_album(seed.trip.id, &[seed.hills.id, seed.harbour.id, seed.film.id, seed.gone.id])
        .unwrap();
    seed
}

fn every_filter_asks_for_exactly_what_it_says(files: &dyn FilesStore) {
    let s = seed(files);
    let f = FileFilter::default;

    // Nothing asked: not in the trash, not hidden. Missing files are kept
    // with everything the person added, so they are listed.
    expect(files, f(), &s.visible(), "an unfiltered listing");

    // Kinds.
    expect(
        files,
        FileFilter { kinds: vec![FileKind::Image], ..f() },
        &[&s.lake, &s.hills, &s.harbour, &s.raw],
        "images",
    );
    expect(
        files,
        FileFilter {
            kinds: vec![FileKind::Video, FileKind::Document],
            include_hidden: true,
            ..f()
        },
        &[&s.clip, &s.film, &s.report, &s.notes, &s.scan],
        "films and documents, hidden ones too",
    );
    expect(files, FileFilter { kinds: vec![FileKind::Audio], ..f() }, &[], "no audio");

    // Folders: directly in one, and anywhere under one.
    let dir = |d: &str| FileFilter { dir: Some(d.into()), ..f() };
    expect(files, dir("Photos/2024"), &[&s.lake, &s.hills, &s.raw], "directly in Photos/2024");
    expect(files, dir("Photos/2024/"), &[&s.lake, &s.hills, &s.raw], "a trailing slash is no part");
    expect(files, dir("Photos"), &[&s.harbour], "directly in Photos, not below it");
    expect(files, dir("Docs"), &[&s.report], "directly in Docs, not in Docs/Old");
    expect(files, dir(""), &[], "the top folder holds only a hidden file");
    expect(
        files,
        FileFilter { include_hidden: true, ..dir("") },
        &[&s.clip],
        "the top folder, hidden files too",
    );
    expect(files, dir("Photo"), &[], "a folder is not a prefix of another's name");

    let under = |d: &str| FileFilter { under: Some(d.into()), ..f() };
    let in_photos = [&s.lake, &s.hills, &s.harbour, &s.raw];
    expect(files, under("Photos"), &in_photos, "under Photos");
    expect(files, under("Photos/"), &in_photos, "a trailing slash is no part");
    expect(files, under("Docs"), &[&s.report, &s.notes], "under Docs");
    expect(files, under("Docs/Old"), &[&s.notes], "under Docs/Old");
    expect(files, under("Photo"), &[], "under is not a prefix of a name");
    expect(files, under("photos"), &[], "folders are matched as they are written");
    expect(files, under("Photos/20"), &[], "nor a prefix of a folder's");
    expect(
        files,
        under(""),
        &[&s.lake, &s.hills, &s.harbour, &s.report, &s.notes, &s.raw],
        "under the top is every plain file, and no sealed one",
    );
    expect(files, under(".jatayu"), &[], "the trash is not listed...");
    expect(
        files,
        FileFilter { status: StatusFilter::All, ..under(".jatayu") },
        &[&s.gone],
        "...unless the trash is asked for",
    );

    // An album, and the trash within it.
    let in_trip = FileFilter { album: Some(s.trip.id), ..f() };
    expect(files, in_trip.clone(), &[&s.hills, &s.harbour, &s.film], "an album's files");
    expect(
        files,
        FileFilter { status: StatusFilter::All, ..in_trip },
        &[&s.hills, &s.harbour, &s.film, &s.gone],
        "an album's files, trashed ones too",
    );
    expect(files, FileFilter { album: Some(AlbumId::new()), ..f() }, &[], "an album not there");

    // Ids.
    let named = vec![s.lake.id, s.report.id, s.gone.id, s.clip.id, FileId::new()];
    let by_ids = FileFilter { ids: Some(named), ..f() };
    expect(files, by_ids.clone(), &[&s.lake, &s.report], "ids: not the trashed, hidden or unknown");
    expect(
        files,
        FileFilter { ids: Some(Vec::new()), ..everything() },
        &[],
        "no ids is no files, not every file",
    );
    assert!(files.query_files(&FileFilter { ids: Some(Vec::new()), ..f() }).unwrap().is_empty());
    expect(
        files,
        FileFilter { status: StatusFilter::All, include_hidden: true, ..by_ids },
        &[&s.lake, &s.report, &s.gone, &s.clip],
        "ids, whatever their status",
    );

    // Marks.
    expect(files, FileFilter { favourite: true, ..f() }, &[&s.lake, &s.film], "favourites");
    let stars = |n| FileFilter { min_rating: Some(n), ..f() };
    expect(files, stars(5), &[&s.lake], "five stars");
    expect(files, stars(4), &[&s.lake, &s.film], "four stars or more");
    expect(files, stars(3), &[&s.lake, &s.hills, &s.film], "three stars or more");
    expect(files, stars(1), &[&s.lake, &s.hills, &s.harbour, &s.film], "rated at all");

    // Origin.
    let from = |o| FileFilter { origin: Some(o), ..f() };
    expect(files, from(FileOrigin::Files), &[&s.lake, &s.hills], "added in the Files app");
    expect(files, from(FileOrigin::Found), &[&s.raw], "found in the folder");
    expect(files, from(FileOrigin::Assistant), &[&s.film], "from the assistant");
    expect(files, from(FileOrigin::Mail), &[&s.report], "from mail");
    expect(files, from(FileOrigin::Jobs), &[], "from no job");

    // Each status.
    let status = |st| FileFilter { status: st, include_hidden: true, ..f() };
    let present: Vec<&FileEntry> =
        s.all().into_iter().filter(|x| x.status == FileStatus::Present).collect();
    expect(files, status(StatusFilter::Present), &present, "present");
    expect(files, status(StatusFilter::Missing), &[&s.notes], "missing");
    expect(files, status(StatusFilter::Trashed), &[&s.gone], "in the trash");
    expect(files, status(StatusFilter::Live), &present_and_missing(&s), "live: not in the trash");
    expect(files, status(StatusFilter::All), &s.all(), "all of them");
    expect(files, FileFilter { status: StatusFilter::Missing, ..f() }, &[&s.notes], "visible ones");

    // Where the bytes are.
    let storage = |k| FileFilter { storage: Some(k), ..f() };
    expect(files, storage(StorageKind::Sealed), &[&s.film, &s.scan], "sealed");
    expect(
        files,
        storage(StorageKind::Plain),
        &[&s.lake, &s.hills, &s.harbour, &s.report, &s.notes, &s.raw],
        "plain",
    );

    // Hidden, and stacks.
    expect(
        files,
        FileFilter { include_hidden: true, ..f() },
        &[&s.lake, &s.hills, &s.harbour, &s.report, &s.clip, &s.notes, &s.film, &s.scan, &s.raw],
        "hidden files, asked for",
    );
    expect(
        files,
        FileFilter { stack_tops_only: true, ..f() },
        &[&s.lake, &s.hills, &s.harbour, &s.report, &s.notes, &s.film, &s.scan],
        "stack tops only: the RAW file under the picture is left out",
    );

    // Time: `when` is when it was taken, else when it was modified -- and a
    // sealed file's is when it was added. From is at or after, to is before.
    let window = |from, to| FileFilter { from, to, ..f() };
    expect(
        files,
        window(Some(t(200)), None),
        &[&s.hills, &s.notes, &s.harbour, &s.report, &s.film],
        "from 200: at or after, so the file at exactly 200 is in",
    );
    expect(
        files,
        window(None, Some(t(200))),
        &[&s.lake, &s.raw, &s.scan],
        "to 200: before, so the file at exactly 200 is out",
    );
    expect(
        files,
        window(Some(t(100)), Some(t(300))),
        &[&s.lake, &s.raw, &s.scan, &s.hills, &s.notes],
        "from 100 to 300",
    );
    expect(files, window(Some(t(200)), Some(t(200))), &[], "an empty window");
    expect(
        files,
        window(Some(t(450)), None),
        &[&s.film],
        "the film is by when it was added, not when it was taken",
    );
    expect(
        files,
        window(Some(at(1_499_999_000)), Some(at(1_500_001_000))),
        &[],
        "and a window around when it was taken does not find it",
    );
    // The time taken, not the time modified, files a picture.
    expect(files, window(Some(t(8_000)), None), &[], "modified times are not when");

    // Combined: every condition must hold.
    expect(
        files,
        FileFilter { kinds: vec![FileKind::Image], favourite: true, ..f() },
        &[&s.lake],
        "favourite images",
    );
    expect(
        files,
        FileFilter { origin: Some(FileOrigin::Files), min_rating: Some(4), ..f() },
        &[&s.lake],
        "from the Files app and four stars",
    );
    expect(
        files,
        FileFilter {
            under: Some("Photos".into()),
            kinds: vec![FileKind::Image],
            stack_tops_only: true,
            ..f()
        },
        &[&s.lake, &s.hills, &s.harbour],
        "pictures under Photos that top their stack",
    );
    expect(
        files,
        FileFilter { storage: Some(StorageKind::Plain), from: Some(t(200)), ..f() },
        &[&s.hills, &s.notes, &s.harbour, &s.report],
        "plain files from 200",
    );

    clear_files(files);
}

/// Every file in the seed that is live: present or missing.
fn present_and_missing(s: &Seed) -> Vec<&FileEntry> {
    s.all().into_iter().filter(|x| !x.status.is_trashed()).collect()
}

/// What a listing sorted by `sort` has to be, written out from the rules:
/// the key's order, ties by id, and the whole of it reversed when
/// descending. A sealed file's time and name are sealed, so it is by when it
/// was added -- among the rest for a time, and as a group after the plain
/// files for a name.
fn modelled(files: &[&FileEntry], sort: FileSort, descending: bool) -> Vec<FileId> {
    let mut list: Vec<&FileEntry> = files.to_vec();
    match sort {
        FileSort::When => {
            list.sort_by_key(|f| (if f.is_sealed() { f.added_at } else { f.when() }, f.id))
        }
        FileSort::Added => list.sort_by_key(|f| (f.added_at, f.id)),
        FileSort::Size => list.sort_by_key(|f| (f.size, f.id)),
        FileSort::Name => list.sort_by_key(|f| {
            if f.is_sealed() {
                (1u8, String::new(), f.added_at, f.id)
            } else {
                (0u8, f.name.to_lowercase(), Timestamp::UNIX_EPOCH, f.id)
            }
        }),
    }
    if descending {
        list.reverse();
    }
    list.into_iter().map(|f| f.id).collect()
}

fn every_sort_orders_both_ways_with_ties_broken_by_id(files: &dyn FilesStore) {
    let s = seed(files);

    for sort in [FileSort::When, FileSort::Added, FileSort::Name, FileSort::Size] {
        for descending in [false, true] {
            // The default listing, and every file there is: the second takes
            // in the trashed and the hidden, whose times and names are as
            // good as any.
            let visible = FileFilter { sort, descending, ..FileFilter::default() };
            assert_eq!(
                found(files, &visible),
                modelled(&s.visible(), sort, descending),
                "{sort:?}, descending: {descending}"
            );
            let all = FileFilter { sort, descending, ..everything() };
            assert_eq!(
                found(files, &all),
                modelled(&s.all(), sort, descending),
                "{sort:?} over every file, descending: {descending}"
            );
        }
    }

    // The ties the seed has: `lake` and `raw` share a time, `notes` and
    // `scan` a size. They come in id order, and the other way when
    // descending -- "in the same direction".
    let (low, high) = if s.lake.id < s.raw.id { (&s.lake, &s.raw) } else { (&s.raw, &s.lake) };
    let up = found(files, &FileFilter { sort: FileSort::When, ..FileFilter::default() });
    let at_low = up.iter().position(|id| *id == low.id).unwrap();
    assert_eq!(up[at_low + 1], high.id, "a tie on time is by id, ascending");
    let newest_first =
        FileFilter { sort: FileSort::When, descending: true, ..FileFilter::default() };
    let down = found(files, &newest_first);
    let at_high = down.iter().position(|id| *id == high.id).unwrap();
    assert_eq!(down[at_high + 1], low.id, "and descending, by id descending");

    let (low, high) =
        if s.notes.id < s.scan.id { (&s.notes, &s.scan) } else { (&s.scan, &s.notes) };
    let up = found(files, &FileFilter { sort: FileSort::Size, ..FileFilter::default() });
    assert_eq!(&up[..2], &[low.id, high.id], "the two smallest tie, by id");
    let largest_first =
        FileFilter { sort: FileSort::Size, descending: true, ..FileFilter::default() };
    let down = found(files, &largest_first);
    assert_eq!(&down[down.len() - 2..], &[high.id, low.id], "and by id descending");

    // The facts the model above is written from, pinned on their own.
    //
    // By when, a sealed file is by when it was added: the film, taken years
    // ago, is the newest; the scan sits between the pair at 100 and `hills`.
    let by_when = found(files, &FileFilter::default());
    assert_eq!(by_when.last(), Some(&s.film.id), "the film is last by when");
    assert_eq!(by_when[2], s.scan.id, "the scan falls between 100 and 200");
    // Names sort without regard to case, as a file manager lists them; the
    // sealed files, whose names are sealed, come after, in the order added.
    let by_name = found(files, &FileFilter { sort: FileSort::Name, ..FileFilter::default() });
    assert_eq!(
        by_name,
        vec![
            s.harbour.id,
            s.hills.id,
            s.raw.id,
            s.lake.id,
            s.notes.id,
            s.report.id,
            s.scan.id,
            s.film.id
        ],
        "by name, 'Hills' among the h's, the sealed files last"
    );
    let by_name_down = found(
        files,
        &FileFilter { sort: FileSort::Name, descending: true, ..FileFilter::default() },
    );
    let mut reversed = by_name;
    reversed.reverse();
    assert_eq!(by_name_down, reversed, "descending is the whole of it reversed");
    // By size, largest first.
    let by_size = found(
        files,
        &FileFilter { sort: FileSort::Size, descending: true, ..FileFilter::default() },
    );
    assert_eq!(by_size[0], s.film.id);
    assert_eq!(by_size[1], s.report.id);

    clear_files(files);
}

fn pages_neither_overlap_nor_skip(files: &dyn FilesStore) {
    let s = seed(files);
    for sort in [FileSort::When, FileSort::Name, FileSort::Size, FileSort::Added] {
        for descending in [false, true] {
            let base = FileFilter { sort, descending, ..FileFilter::default() };
            let whole = found(files, &base);
            assert_eq!(whole.len(), s.visible().len());

            let mut pages = Vec::new();
            for offset in (0..whole.len() as u32 + 3).step_by(3) {
                let page = found(files, &FileFilter { limit: Some(3), offset, ..base.clone() });
                assert!(page.len() <= 3);
                pages.extend(page);
            }
            assert_eq!(pages, whole, "pages of three, {sort:?}, descending: {descending}");

            // An offset with no limit is the rest of the list.
            assert_eq!(
                found(files, &FileFilter { offset: 2, ..base.clone() }),
                whole[2..],
                "{sort:?} from the third"
            );
        }
    }
    let base = FileFilter::default();
    assert!(found(files, &FileFilter { limit: Some(0), ..base.clone() }).is_empty());
    assert!(found(files, &FileFilter { offset: 1_000, ..base.clone() }).is_empty());
    assert!(found(files, &FileFilter { limit: Some(5), offset: 1_000, ..base }).is_empty());
    // `count_files` is the whole count whatever the page: `found` checked
    // that each time, and this is the plainest case of it.
    assert_eq!(
        files
            .count_files(&FileFilter { limit: Some(1), offset: 5, ..FileFilter::default() })
            .unwrap(),
        s.visible().len() as u64
    );

    clear_files(files);
}

fn the_index_lists_plain_files_not_in_the_trash(files: &dyn FilesStore) {
    let s = seed(files);

    // Present and missing, plain only; the trashed and the sealed are not the
    // reconciler's. Hidden files are: the folder has them.
    let mut want: Vec<IndexRow> = s
        .all()
        .into_iter()
        .filter(|f| !f.is_sealed() && !f.status.is_trashed())
        .map(|f| IndexRow {
            id: f.id,
            path: f.path().unwrap().to_string(),
            size: f.size,
            modified_us: micros(f.modified_at),
            hash: f.hash.clone(),
            missing: matches!(f.status, FileStatus::Missing { .. }),
        })
        .collect();
    let mut got = files.file_index().unwrap();
    want.sort_by_key(|r| r.id);
    got.sort_by_key(|r| r.id);
    assert_eq!(got, want);
    assert_eq!(got.len(), 7);
    assert!(got.iter().any(|r| r.missing), "a missing file is flagged");
    assert!(!got.iter().any(|r| r.id == s.gone.id || r.id == s.film.id));

    clear_files(files);
}

fn usage_counts_what_the_catalog_holds(files: &dyn FilesStore) {
    let s = seed(files);
    // Live: everything but the trash, hidden and missing and sealed files
    // included. Nine files, 300 + 100 + 200 + 500 + 900 + 70 + 5000 + 70 + 400.
    let usage = files.files_usage().unwrap();
    assert_eq!(
        usage,
        FilesUsage {
            files: 9,
            bytes: 7_540,
            by_kind: vec![
                KindUsage { kind: FileKind::Image, files: 4, bytes: 1_000 },
                KindUsage { kind: FileKind::Video, files: 2, bytes: 5_900 },
                KindUsage { kind: FileKind::Document, files: 3, bytes: 640 },
            ],
            sealed_files: 2,
            sealed_bytes: 5_070,
            trashed_files: 1,
            trashed_bytes: 40,
            missing_files: 1,
        }
    );

    // It follows the records: trash a picture, lose a file from the live
    // count and gain one in the trash's.
    let in_trash = trashed(&s.harbour, "Photos/harbour.png");
    files.put_file(&in_trash).unwrap();
    let usage = files.files_usage().unwrap();
    assert_eq!((usage.files, usage.bytes), (8, 7_340));
    assert_eq!((usage.trashed_files, usage.trashed_bytes), (2, 240));
    assert_eq!(
        usage.by_kind[0],
        KindUsage { kind: FileKind::Image, files: 3, bytes: 800 },
        "kinds are of live files"
    );

    clear_files(files);
    assert_eq!(files.files_usage().unwrap(), FilesUsage::default());
}

fn blob_refs_are_every_sealed_contents_and_thumbnail(files: &dyn FilesStore) {
    let [contents_a, thumb_a, contents_b, shared] =
        ["contents a", "thumbnail a", "contents b", "shared"].map(|s| BlobId::of(s.as_bytes()));
    let mut one = sealed("one.bin");
    one.storage = FileStorage::Sealed { blob: contents_a };
    one.thumbnail = Some(thumb_a);
    let mut two = sealed("two.bin");
    two.storage = FileStorage::Sealed { blob: contents_b };
    let a = with(plain("A/a.jpg"), |f| f.thumbnail = Some(shared));
    let b = with(plain("B/b.jpg"), |f| f.thumbnail = Some(shared));
    let c = plain("C/c.jpg");
    for file in [&one, &two, &a, &b, &c] {
        files.put_file(file).unwrap();
    }

    let want = |blobs: &[BlobId]| blobs.iter().copied().collect::<BTreeSet<BlobId>>();
    assert_eq!(
        files.file_blob_refs().unwrap(),
        want(&[contents_a, thumb_a, contents_b, shared]),
        "a sealed file's contents, and every file's thumbnail, however many share it"
    );

    // A thumbnail dropped from the record is dropped from the set.
    files.put_file(&with(one.clone(), |f| f.thumbnail = None)).unwrap();
    assert_eq!(files.file_blob_refs().unwrap(), want(&[contents_a, contents_b, shared]));
    files.delete_file(two.id).unwrap();
    assert_eq!(files.file_blob_refs().unwrap(), want(&[contents_a, shared]));
    // A blob two files share stays until both let go of it.
    files.delete_file(a.id).unwrap();
    assert_eq!(files.file_blob_refs().unwrap(), want(&[contents_a, shared]));
    files.delete_file(b.id).unwrap();
    assert_eq!(files.file_blob_refs().unwrap(), want(&[contents_a]));

    clear_files(files);
    assert!(files.file_blob_refs().unwrap().is_empty());
}

// ---- references ---------------------------------------------------------

fn references_are_replaced_one_owner_at_a_time(files: &dyn FilesStore) {
    let (f1, f2, f3) = (plain("R/1.jpg"), plain("R/2.jpg"), plain("R/3.jpg"));
    files.put_files(&[f1.clone(), f2.clone(), f3.clone()]).unwrap();
    let entry = RefOwner::new(RefKind::Entry, "entry-1");
    let note = RefOwner::new(RefKind::Note, "note-1");
    let message = RefOwner::new(RefKind::Message, "message-1");

    files.set_refs(&entry, &[f1.id, f2.id]).unwrap();
    assert_eq!(files.refs_to(f1.id).unwrap(), vec![entry.clone()]);
    assert_eq!(files.refs_to(f2.id).unwrap(), vec![entry.clone()]);
    assert!(files.refs_to(f3.id).unwrap().is_empty());
    assert_eq!(files.referenced_files().unwrap(), BTreeSet::from([f1.id, f2.id]));

    // Two owners of one file.
    files.set_refs(&note, &[f2.id, f3.id]).unwrap();
    assert_eq!(sorted(files.refs_to(f2.id).unwrap()), vec![entry.clone(), note.clone()]);
    assert_eq!(files.referenced_files().unwrap(), BTreeSet::from([f1.id, f2.id, f3.id]));

    // Replaced, not added to: what the owner no longer says is gone, what
    // another owner says is not touched, and a file named three times is one
    // reference.
    files.set_refs(&entry, &[f3.id, f3.id, f3.id]).unwrap();
    assert!(files.refs_to(f1.id).unwrap().is_empty(), "the entry no longer refers to it");
    assert_eq!(files.refs_to(f2.id).unwrap(), vec![note.clone()], "the note still does");
    assert_eq!(sorted(files.refs_to(f3.id).unwrap()), vec![entry.clone(), note.clone()]);

    // An empty set forgets the owner.
    files.set_refs(&entry, &[]).unwrap();
    assert_eq!(files.refs_to(f3.id).unwrap(), vec![note.clone()]);
    assert_eq!(files.referenced_files().unwrap(), BTreeSet::from([f2.id, f3.id]));
    files.set_refs(&note, &[]).unwrap();
    assert!(files.referenced_files().unwrap().is_empty());

    // The same id under two kinds is two owners.
    let as_entry = RefOwner::new(RefKind::Entry, "same");
    let as_note = RefOwner::new(RefKind::Note, "same");
    files.set_refs(&as_entry, &[f1.id]).unwrap();
    files.set_refs(&as_note, &[f1.id]).unwrap();
    assert_eq!(sorted(files.refs_to(f1.id).unwrap()), vec![as_entry.clone(), as_note.clone()]);
    files.set_refs(&as_entry, &[]).unwrap();
    assert_eq!(files.refs_to(f1.id).unwrap(), vec![as_note.clone()]);

    // The whole table replaced: the owners it does not name are forgotten.
    files
        .replace_all_refs(&[
            (note.clone(), f2.id),
            (message.clone(), f2.id),
            (message.clone(), f3.id),
        ])
        .unwrap();
    assert!(files.refs_to(f1.id).unwrap().is_empty(), "as_note is not in the new table");
    assert_eq!(sorted(files.refs_to(f2.id).unwrap()), vec![note.clone(), message.clone()]);
    assert_eq!(files.refs_to(f3.id).unwrap(), vec![message.clone()]);
    assert_eq!(files.referenced_files().unwrap(), BTreeSet::from([f2.id, f3.id]));
    // Repeats in it are one reference.
    files
        .replace_all_refs(&[(note.clone(), f1.id), (note.clone(), f1.id), (message, f1.id)])
        .unwrap();
    assert_eq!(files.refs_to(f1.id).unwrap().len(), 2);
    files.replace_all_refs(&[]).unwrap();
    assert!(files.referenced_files().unwrap().is_empty());
    assert!(files.refs_to(f1.id).unwrap().is_empty());

    clear_files(files);
}

fn deleting_a_file_takes_its_references_and_memberships(files: &dyn FilesStore) {
    let (a, b, c) = (plain("D/a.jpg"), plain("D/b.jpg"), plain("D/c.jpg"));
    files.put_files(&[a.clone(), b.clone(), c.clone()]).unwrap();
    let trip = album_named("Trip", 0);
    let other = album_named("Other", 10);
    files.put_album(&trip).unwrap();
    files.put_album(&other).unwrap();
    files.add_to_album(trip.id, &[a.id, b.id]).unwrap();
    files.add_to_album(other.id, &[a.id, c.id]).unwrap();
    let entry = RefOwner::new(RefKind::Entry, "entry-1");
    let note = RefOwner::new(RefKind::Note, "note-1");
    files.set_refs(&entry, &[a.id, b.id]).unwrap();
    files.set_refs(&note, &[a.id]).unwrap();

    files.delete_file(a.id).unwrap();

    super::assert_not_found(files.get_file(a.id));
    assert!(files.refs_to(a.id).unwrap().is_empty(), "its references went with it");
    assert_eq!(files.referenced_files().unwrap(), BTreeSet::from([b.id]));
    assert!(files.albums_of(a.id).unwrap().is_empty(), "and its memberships");
    assert_eq!(files.album_files(trip.id).unwrap(), vec![b.id]);
    assert_eq!(files.album_files(other.id).unwrap(), vec![c.id]);

    // Never the records that referred to it, or the albums it was in, or
    // what else they hold.
    assert_eq!(files.refs_to(b.id).unwrap(), vec![entry.clone()], "the entry still refers to b");
    assert_eq!(files.get_album(trip.id).unwrap(), trip);
    assert_eq!(files.get_album(other.id).unwrap(), other);
    assert_eq!(
        files.album_counts().unwrap(),
        BTreeMap::from([(trip.id, 1), (other.id, 1)]),
        "the albums count what is left"
    );
    files.delete_file(a.id).expect("deleting it again is nothing to do");

    clear_files(files);
}

// ---- albums -------------------------------------------------------------

fn album_named(name: &str, made: i64) -> Album {
    Album {
        id: AlbumId::new(),
        name: name.into(),
        description: format!("About {name}"),
        cover: None,
        created_at: t(made),
        updated_at: t(made),
    }
}

fn albums_keep_their_files_in_the_order_they_were_added(files: &dyn FilesStore) {
    let [f1, f2, f3, f4] = ["1", "2", "3", "4"].map(|n| plain(&format!("Al/{n}.jpg")));
    files.put_files(&[f1.clone(), f2.clone(), f3.clone(), f4.clone()]).unwrap();

    // A round trip of every field, listed in the order made.
    let mut trip = album_named("Trip", 0);
    trip.description = "Three days by the lake".into();
    trip.cover = Some(f2.id);
    let later = album_named("Later", 100);
    let empty = album_named("Empty", 200);
    files.put_album(&later).unwrap();
    files.put_album(&empty).unwrap();
    files.put_album(&trip).unwrap();
    assert_eq!(files.get_album(trip.id).unwrap(), trip);
    assert_eq!(files.list_albums().unwrap(), vec![trip.clone(), later.clone(), empty.clone()]);

    // Putting an album again replaces it, whatever its name.
    let renamed = Album { name: "Trip to Annecy".into(), updated_at: t(500), ..trip.clone() };
    files.put_album(&renamed).unwrap();
    assert_eq!(files.list_albums().unwrap().len(), 3, "an album put again is not a second one");
    assert_eq!(files.get_album(trip.id).unwrap(), renamed);

    // In the order they were added; one already there stays where it is.
    files.add_to_album(trip.id, &[f3.id, f1.id, f2.id]).unwrap();
    assert_eq!(files.album_files(trip.id).unwrap(), vec![f3.id, f1.id, f2.id]);
    files.add_to_album(trip.id, &[f2.id, f4.id, f4.id, f3.id]).unwrap();
    assert_eq!(
        files.album_files(trip.id).unwrap(),
        vec![f3.id, f1.id, f2.id, f4.id],
        "a file already in the album is left where it was, and one named twice added once"
    );
    files.add_to_album(later.id, &[f3.id, f3.id, f1.id]).unwrap();
    assert_eq!(files.album_files(later.id).unwrap(), vec![f3.id, f1.id]);
    // One that is not in the catalog is not a member of anything.
    files.add_to_album(later.id, &[FileId::new()]).unwrap();
    assert_eq!(files.album_files(later.id).unwrap(), vec![f3.id, f1.id]);

    // Removing keeps the order of the rest, and what is not there is nothing.
    files.remove_from_album(trip.id, &[f1.id, FileId::new()]).unwrap();
    assert_eq!(files.album_files(trip.id).unwrap(), vec![f3.id, f2.id, f4.id]);
    files.remove_from_album(trip.id, &[f1.id]).unwrap();
    assert_eq!(files.album_files(trip.id).unwrap(), vec![f3.id, f2.id, f4.id]);
    files.add_to_album(trip.id, &[f1.id]).unwrap();
    assert_eq!(
        files.album_files(trip.id).unwrap(),
        vec![f3.id, f2.id, f4.id, f1.id],
        "added again, it goes to the end"
    );

    // Which albums a file is in.
    assert_eq!(files.albums_of(f4.id).unwrap(), vec![trip.id]);
    assert_eq!(
        sorted(files.albums_of(f3.id).unwrap()),
        sorted(vec![trip.id, later.id]),
        "a file may be in many albums"
    );
    assert!(files.albums_of(f2.id).unwrap().contains(&trip.id));

    // Counts, every album's -- an empty one is nought -- and not the trash's.
    assert_eq!(
        files.album_counts().unwrap(),
        BTreeMap::from([(trip.id, 4), (later.id, 2), (empty.id, 0)])
    );
    let in_trash = trashed(&f4, "Al/4.jpg");
    files.put_file(&in_trash).unwrap();
    assert_eq!(files.album_counts().unwrap()[&trip.id], 3, "a trashed file is not counted");
    assert_eq!(
        files.album_files(trip.id).unwrap().len(),
        4,
        "though it is still a member, for when it comes back"
    );
    files.put_file(&f4).unwrap();
    assert_eq!(files.album_counts().unwrap()[&trip.id], 4, "and is again when it is restored");

    clear_files(files);
    assert!(files.album_counts().unwrap().is_empty());
}

fn deleting_an_album_never_deletes_a_file(files: &dyn FilesStore) {
    let [f1, f2, f3] = ["1", "2", "3"].map(|n| plain(&format!("Gone/{n}.jpg")));
    files.put_files(&[f1.clone(), f2.clone(), f3.clone()]).unwrap();
    let doomed = album_named("Doomed", 0);
    let kept = album_named("Kept", 1);
    files.put_album(&doomed).unwrap();
    files.put_album(&kept).unwrap();
    files.add_to_album(doomed.id, &[f1.id, f2.id]).unwrap();
    files.add_to_album(kept.id, &[f2.id, f3.id]).unwrap();

    files.delete_album(doomed.id).unwrap();

    super::assert_not_found(files.get_album(doomed.id));
    assert_eq!(files.list_albums().unwrap(), vec![kept.clone()]);
    for file in [&f1, &f2, &f3] {
        assert_eq!(&files.get_file(file.id).unwrap(), file, "the files are somebody's");
    }
    assert!(files.albums_of(f1.id).unwrap().is_empty(), "the memberships are gone");
    assert_eq!(files.albums_of(f2.id).unwrap(), vec![kept.id], "the others are not");
    assert!(files.album_files(doomed.id).unwrap().is_empty());
    assert_eq!(files.album_files(kept.id).unwrap(), vec![f2.id, f3.id]);
    assert_eq!(files.album_counts().unwrap(), BTreeMap::from([(kept.id, 2)]));
    let by_album = FileFilter { album: Some(doomed.id), ..FileFilter::default() };
    assert!(found(files, &by_album).is_empty());
    assert_eq!(files.count_files(&everything()).unwrap(), 3);

    clear_files(files);
}

// ---- settings and text --------------------------------------------------

fn settings_default_until_saved_and_round_trip(files: &dyn FilesStore) {
    assert_eq!(files.files_settings().unwrap(), FilesSettings::default());

    let custom = FilesSettings {
        folder: Some("/home/someone/Jatayu".into()),
        encrypted: vec![FileOrigin::Journal, FileOrigin::Mail],
        trash_days: 7,
        assistant_access: false,
        name_places: true,
        index_documents: false,
        index_folders: vec!["Books".into()],
        skip_folders: vec!["Books/Old".into()],
    };
    files.put_files_settings(&custom).unwrap();
    assert_eq!(files.files_settings().unwrap(), custom);

    // Written again, replaced: the last one wins and there is only one.
    let other = FilesSettings { folder: None, encrypted: Vec::new(), trash_days: 0, ..custom };
    files.put_files_settings(&other).unwrap();
    assert_eq!(files.files_settings().unwrap(), other);

    files.put_files_settings(&FilesSettings::default()).unwrap();
    assert_eq!(files.files_settings().unwrap(), FilesSettings::default());
}

fn unicode_and_large_sizes_survive_a_files_round_trip(files: &dyn FilesStore) {
    let path = "Fotos/\u{dc}n\u{ef}c\u{f6}d\u{e9}/\u{65e5}\u{672c}\u{8a9e} \u{1f338}.jpg";
    let mut file = plain(path);
    file.kind = FileKind::Image;
    // Five gigabytes: more than 32 bits can hold.
    file.size = 5_000_000_000;
    file.caption = "Bl\u{fc}ten \u{2014} \u{685}\u{6cc}\u{627}\u{644} \u{1f4f7}".into();
    file.tags = vec!["\u{5bb6}\u{65cf}".into(), "sp\u{e4}tsommer".into()];
    file.people = vec!["Zo\u{eb}".into()];
    file.place = Some("K\u{f8}benhavn".into());
    file.properties.insert("user.season".into(), PropValue::Text("\u{2603}".into()));
    files.put_file(&file).unwrap();

    assert_eq!(files.get_file(file.id).unwrap(), file);
    assert_eq!(files.file_by_path(path).unwrap(), Some(file.clone()));
    let dir = "Fotos/\u{dc}n\u{ef}c\u{f6}d\u{e9}";
    let in_dir = FileFilter { dir: Some(dir.into()), ..FileFilter::default() };
    assert_eq!(found(files, &in_dir), vec![file.id]);
    let under = FileFilter { under: Some("Fotos".into()), ..FileFilter::default() };
    assert_eq!(found(files, &under), vec![file.id]);
    let prefix = FileFilter { under: Some("Fotos/\u{dc}n".into()), ..FileFilter::default() };
    assert!(found(files, &prefix).is_empty(), "a prefix of a name is not a folder");
    let by_name = FileFilter { sort: FileSort::Name, ..FileFilter::default() };
    assert_eq!(found(files, &by_name), vec![file.id]);
    assert_eq!(files.files_usage().unwrap().bytes, 5_000_000_000);
    assert_eq!(files.file_index().unwrap()[0].size, 5_000_000_000);

    clear_files(files);
}
