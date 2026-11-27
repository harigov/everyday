//! What changed in the files folder since the catalog last looked, worked
//! out from two listings -- the catalog's [`IndexRow`]s and the disk's
//! [`DiskFile`]s -- without touching either.
//!
//! Pure on purpose: every rule here is a decision about somebody's files,
//! and each one is tested on lists, not on a disk.
//!
//! The pass, as `jatayu-service`'s `files::task` runs it:
//!
//! 1. [`plan`] pairs the two listings by path.
//! 2. The task hashes what [`plan`] says is new or changed.
//! 3. [`match_moves`] pairs vanished files with new ones by hash: a file
//!    renamed or moved while nobody watched keeps its id, its caption, its
//!    albums and every reference to it.
//! 4. [`held_back`] decides whether so much vanished that the folder is
//!    probably not what it was, in which case nothing is marked missing.
//! 5. [`crate::Vault::apply_scan`] writes the outcome.
//!
//! [`stacks`] is separate: which new files belong under another (a RAW file
//! under its JPEG, a Live Photo's film under its picture).

use super::IndexRow;
use super::disk::{DiskFile, split};
use crate::id::FileId;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The outcome of pairing the catalog with the disk by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Present in both, same size and time.
    pub unchanged: Vec<FileId>,
    /// On disk, not in the catalog at that path.
    pub new: Vec<DiskFile>,
    /// In both, size or time differs: to be hashed again.
    pub changed: Vec<(FileId, DiskFile)>,
    /// Catalogued as present, not on disk.
    pub vanished: Vec<FileId>,
    /// Catalogued as missing, back on disk at the same path.
    pub returned: Vec<(FileId, DiskFile)>,
}

/// Pair `index` with `disk` by path.
///
/// A missing file still absent is in none of the lists. Paths compare
/// exactly (byte for byte): a case-only rename on a case-insensitive disk
/// shows up as a vanish and a new file, which [`match_moves`] then pairs.
///
/// Everything comes out in the order it went in: the disk's for what is on
/// it, the catalog's for what vanished.
pub fn plan(index: &[IndexRow], disk: &[DiskFile]) -> Plan {
    // Should a path somehow be catalogued twice, the first row speaks for it.
    let mut by_path: HashMap<&str, &IndexRow> = HashMap::with_capacity(index.len());
    for row in index {
        by_path.entry(row.path.as_str()).or_insert(row);
    }

    let mut plan = Plan::default();
    let mut on_disk: HashSet<&str> = HashSet::with_capacity(disk.len());
    for file in disk {
        on_disk.insert(file.path.as_str());
        match by_path.get(file.path.as_str()) {
            None => plan.new.push(file.clone()),
            // Back, whatever it now looks like: the pass that sees it again
            // hashes it and finds out whether it is the same file.
            Some(row) if row.missing => plan.returned.push((row.id, file.clone())),
            Some(row) if row.size == file.size && row.modified_us == file.modified_us => {
                plan.unchanged.push(row.id);
            }
            Some(row) => plan.changed.push((row.id, file.clone())),
        }
    }
    for row in index {
        if !row.missing && !on_disk.contains(row.path.as_str()) {
            plan.vanished.push(row.id);
        }
    }
    plan
}

/// Pair vanished files with new ones by content hash. `vanished` is each
/// vanished file's `(id, catalogued hash, old path)` (files never hashed
/// cannot be matched and are left out by the caller); `new` is each new
/// file's `(path, fresh hash)`.
///
/// Each vanished file is matched at most once, and each new path at most
/// once. When several new files share a hash (copies), the one in the same
/// folder as the vanished file's old path wins, then the one with the same
/// name, then the first by path. Answers `(id, new path)` pairs, in the
/// order of `vanished`.
pub fn match_moves(
    vanished: &[(FileId, String, String)],
    new: &[(String, String)],
) -> Vec<(FileId, String)> {
    let mut by_hash: HashMap<&str, Vec<&str>> = HashMap::new();
    for (path, hash) in new {
        // An empty hash is "never read", which matches nothing.
        if !hash.is_empty() {
            by_hash.entry(hash.as_str()).or_default().push(path.as_str());
        }
    }

    let mut taken: HashSet<&str> = HashSet::new();
    let mut moves = Vec::new();
    for (id, hash, old_path) in vanished {
        let Some(candidates) = by_hash.get(hash.as_str()) else { continue };
        let (old_dir, old_name) = split(old_path);
        let best =
            candidates.iter().copied().filter(|path| !taken.contains(path)).min_by_key(|path| {
                let (dir, name) = split(path);
                // `false` sorts before `true`: the folder it was in, then the
                // name it had, then the path alone.
                (dir != old_dir, name != old_name, *path)
            });
        if let Some(path) = best {
            taken.insert(path);
            moves.push((*id, path.to_string()));
        }
    }
    moves
}

/// Whether to hold back from marking files missing: when more than half of
/// the present plain files vanished at once, and at least
/// [`HOLD_BACK_MIN`] of them. A folder that lost half its files in one
/// pass is far more likely to be the wrong folder, or a mount half there,
/// than a person who deleted them -- and a person who did gets them marked
/// missing on the first pass that agrees.
pub fn held_back(present: usize, vanished: usize) -> bool {
    vanished >= HOLD_BACK_MIN && vanished.saturating_mul(2) > present
}

/// See [`held_back`].
pub const HOLD_BACK_MIN: usize = 20;

/// Camera RAW extensions, lowercase.
const RAW: [&str; 10] = ["dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf", "pef", "srw"];

/// What one file is to the others of its name.
#[derive(Default)]
struct Siblings<'a> {
    /// A JPEG, the first by path: what a stack prefers as its top.
    jpeg: Option<(&'a str, FileId)>,
    /// A HEIC (or HEIF), the first by path: the top when there is no JPEG.
    heic: Option<(&'a str, FileId)>,
    /// RAW files and films, to go under whichever is the top.
    under: Vec<(&'a str, FileId)>,
}

/// Which files belong under another in a stack, by name alone: files in the
/// same folder with the same name before the extension (case-insensitive),
/// where
///
/// - a RAW file (`dng`, `cr2`, `cr3`, `nef`, `arw`, `orf`, `rw2`, `raf`,
///   `pef`, `srw`) goes under a JPEG or HEIC of the same name;
/// - a film (`mov`, `mp4`) goes under a HEIC or JPEG of the same name (a
///   Live Photo, or an Android motion photo's film);
///
/// `files` is `(id, path)`. Answers `(top, members)` for each stack found,
/// the top never among its own members.
///
/// When a name has both a JPEG and a HEIC, the JPEG is the top: it is the
/// one every program can show. A HEIC beside it is neither top nor member,
/// since nothing here says it belongs under a JPEG. Stacks come out ordered
/// by folder and name, members by path, so the same folder gives the same
/// answer however it was listed.
pub fn stacks(files: &[(FileId, String)]) -> Vec<(FileId, Vec<FileId>)> {
    let mut groups: BTreeMap<(&str, String), Siblings<'_>> = BTreeMap::new();
    for (id, path) in files {
        let (dir, name) = split(path);
        // A name that starts with its dot has no extension, and one that is
        // all extension has no stem to match on.
        let Some((stem, ext)) = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) else {
            continue;
        };
        let ext = ext.to_ascii_lowercase();
        let group = groups.entry((dir, stem.to_lowercase())).or_default();
        let path = path.as_str();
        let earlier = |slot: &Option<(&str, FileId)>| slot.is_none_or(|(p, _)| path < p);
        match ext.as_str() {
            "jpg" | "jpeg" if earlier(&group.jpeg) => group.jpeg = Some((path, *id)),
            "heic" | "heif" if earlier(&group.heic) => group.heic = Some((path, *id)),
            "mov" | "mp4" => group.under.push((path, *id)),
            ext if RAW.contains(&ext) => group.under.push((path, *id)),
            _ => {}
        }
    }

    groups
        .into_values()
        .filter_map(|mut group| {
            let (_, top) = group.jpeg.or(group.heic)?;
            if group.under.is_empty() {
                return None;
            }
            group.under.sort();
            Some((top, group.under.into_iter().map(|(_, id)| id).collect()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, size: u64, modified_us: i64, missing: bool) -> IndexRow {
        IndexRow {
            id: FileId::new(),
            path: path.to_string(),
            size,
            modified_us,
            hash: format!("hash-of-{path}"),
            missing,
        }
    }

    fn file(path: &str, size: u64, modified_us: i64) -> DiskFile {
        DiskFile { path: path.to_string(), size, modified_us }
    }

    // ---- plan -------------------------------------------------------------

    #[test]
    fn nothing_on_either_side_is_nothing_to_do() {
        assert_eq!(plan(&[], &[]), Plan::default());
    }

    #[test]
    fn a_file_that_matches_is_unchanged() {
        let a = row("a.jpg", 10, 100, false);
        let plan = plan(std::slice::from_ref(&a), &[file("a.jpg", 10, 100)]);
        assert_eq!(plan, Plan { unchanged: vec![a.id], ..Plan::default() });
    }

    #[test]
    fn a_new_size_or_time_is_a_change_and_comes_with_what_the_disk_says() {
        let by_size = row("size.txt", 10, 100, false);
        let by_time = row("time.txt", 10, 100, false);
        let both = row("both.txt", 10, 100, false);
        let index = [by_size.clone(), by_time.clone(), both.clone()];
        let disk = [file("size.txt", 11, 100), file("time.txt", 10, 101), file("both.txt", 1, 1)];
        let plan = plan(&index, &disk);
        assert_eq!(
            plan.changed,
            vec![
                (by_size.id, disk[0].clone()),
                (by_time.id, disk[1].clone()),
                (both.id, disk[2].clone()),
            ]
        );
        assert!(plan.unchanged.is_empty() && plan.new.is_empty() && plan.vanished.is_empty());
    }

    #[test]
    fn a_file_the_catalog_has_not_seen_is_new() {
        let known = row("a.jpg", 1, 1, false);
        let disk = [file("a.jpg", 1, 1), file("b/c.jpg", 2, 2)];
        let plan = plan(std::slice::from_ref(&known), &disk);
        assert_eq!(plan.new, vec![file("b/c.jpg", 2, 2)]);
        assert_eq!(plan.unchanged, vec![known.id]);
    }

    #[test]
    fn a_present_file_that_is_not_on_disk_has_vanished() {
        let gone = row("gone.jpg", 1, 1, false);
        let stays = row("stays.jpg", 1, 1, false);
        let plan = plan(&[gone.clone(), stays.clone()], &[file("stays.jpg", 1, 1)]);
        assert_eq!(plan.vanished, vec![gone.id]);
        assert_eq!(plan.unchanged, vec![stays.id]);
    }

    #[test]
    fn a_missing_file_stays_out_of_every_list_until_it_is_back() {
        let still = row("still.jpg", 1, 1, true);
        let back = row("back.jpg", 5, 5, true);
        let plan = plan(&[still, back.clone()], &[file("back.jpg", 9, 9)]);
        assert_eq!(plan.returned, vec![(back.id, file("back.jpg", 9, 9))]);
        assert!(plan.vanished.is_empty(), "not marked missing twice");
        assert!(plan.unchanged.is_empty() && plan.changed.is_empty() && plan.new.is_empty());
    }

    #[test]
    fn a_missing_file_that_comes_back_even_looking_the_same_is_returned() {
        let back = row("back.jpg", 5, 5, true);
        let plan = plan(std::slice::from_ref(&back), &[file("back.jpg", 5, 5)]);
        assert_eq!(plan.returned.len(), 1);
        assert!(plan.unchanged.is_empty());
    }

    #[test]
    fn paths_compare_byte_for_byte() {
        // A case-only rename: one vanished, one new, for `match_moves` to pair.
        let old = row("Photo.JPG", 1, 1, false);
        let plan = plan(std::slice::from_ref(&old), &[file("photo.jpg", 1, 1)]);
        assert_eq!(plan.vanished, vec![old.id]);
        assert_eq!(plan.new, vec![file("photo.jpg", 1, 1)]);
    }

    #[test]
    fn a_mixed_pass_sorts_everything_into_its_list() {
        let same = row("same.txt", 1, 1, false);
        let edited = row("edited.txt", 1, 1, false);
        let gone = row("gone.txt", 1, 1, false);
        let back = row("back.txt", 1, 1, true);
        let away = row("away.txt", 1, 1, true);
        let index = [same.clone(), edited.clone(), gone.clone(), back.clone(), away];
        let disk = [
            file("new.txt", 3, 3),
            file("edited.txt", 2, 2),
            file("back.txt", 1, 1),
            file("same.txt", 1, 1),
        ];
        let plan = plan(&index, &disk);
        assert_eq!(plan.new, vec![file("new.txt", 3, 3)]);
        assert_eq!(plan.changed, vec![(edited.id, file("edited.txt", 2, 2))]);
        assert_eq!(plan.returned, vec![(back.id, file("back.txt", 1, 1))]);
        assert_eq!(plan.unchanged, vec![same.id]);
        assert_eq!(plan.vanished, vec![gone.id]);
    }

    #[test]
    fn a_path_catalogued_twice_is_decided_by_its_first_row() {
        let first = row("a.txt", 1, 1, false);
        let second = row("a.txt", 1, 1, false);
        let plan = plan(&[first.clone(), second], &[file("a.txt", 1, 1)]);
        assert_eq!(plan.unchanged, vec![first.id]);
    }

    // ---- match_moves ------------------------------------------------------

    fn vanished(path: &str, hash: &str) -> (FileId, String, String) {
        (FileId::new(), hash.to_string(), path.to_string())
    }

    fn new(path: &str, hash: &str) -> (String, String) {
        (path.to_string(), hash.to_string())
    }

    #[test]
    fn a_vanished_file_follows_its_hash_to_its_new_path() {
        let v = vanished("old/a.jpg", "h1");
        let moves =
            match_moves(std::slice::from_ref(&v), &[new("new/b.jpg", "h1"), new("c.jpg", "h2")]);
        assert_eq!(moves, vec![(v.0, "new/b.jpg".to_string())]);
    }

    #[test]
    fn no_matching_hash_is_no_move() {
        let v = vanished("a.jpg", "h1");
        assert!(match_moves(std::slice::from_ref(&v), &[new("b.jpg", "h2")]).is_empty());
        assert!(match_moves(&[], &[new("b.jpg", "h2")]).is_empty());
        assert!(match_moves(std::slice::from_ref(&v), &[]).is_empty());
    }

    #[test]
    fn a_file_never_hashed_matches_nothing() {
        let v = vanished("a.jpg", "");
        assert!(match_moves(std::slice::from_ref(&v), &[new("b.jpg", "")]).is_empty());
    }

    #[test]
    fn among_copies_the_same_folder_wins_then_the_same_name_then_the_first_path() {
        let old = vanished("Trip/IMG_1.jpg", "h");
        let copies =
            [new("A/IMG_1.jpg", "h"), new("Trip/Copy of IMG_1.jpg", "h"), new("B/IMG_1.jpg", "h")];
        // Same folder beats same name.
        let moves = match_moves(std::slice::from_ref(&old), &copies);
        assert_eq!(moves, vec![(old.0, "Trip/Copy of IMG_1.jpg".to_string())]);

        // Without one in the same folder, the same name beats the order.
        let named = [new("A/other.jpg", "h"), new("Z/IMG_1.jpg", "h")];
        let moves = match_moves(std::slice::from_ref(&old), &named);
        assert_eq!(moves, vec![(old.0, "Z/IMG_1.jpg".to_string())]);

        // With nothing to tell them apart, the first by path, however listed.
        let plain = [new("Z/x.jpg", "h"), new("B/y.jpg", "h"), new("M/z.jpg", "h")];
        let moves = match_moves(std::slice::from_ref(&old), &plain);
        assert_eq!(moves, vec![(old.0, "B/y.jpg".to_string())]);
    }

    #[test]
    fn the_folder_counts_for_more_than_the_name() {
        // Same folder, different name, against different folder, same name.
        let old = vanished("a/x.jpg", "h");
        let moves =
            match_moves(std::slice::from_ref(&old), &[new("b/x.jpg", "h"), new("a/y.jpg", "h")]);
        assert_eq!(moves, vec![(old.0, "a/y.jpg".to_string())]);
    }

    #[test]
    fn each_side_is_used_once() {
        // Two copies vanished, one new: only one of them can have moved there.
        let a = vanished("a.jpg", "h");
        let b = vanished("b.jpg", "h");
        let moves = match_moves(&[a.clone(), b.clone()], &[new("c.jpg", "h")]);
        assert_eq!(moves, vec![(a.0, "c.jpg".to_string())]);

        // Two copies vanished and two new: each gets its own.
        let moves = match_moves(&[a.clone(), b.clone()], &[new("c.jpg", "h"), new("d.jpg", "h")]);
        assert_eq!(moves, vec![(a.0, "c.jpg".to_string()), (b.0, "d.jpg".to_string())]);

        // One vanished, two new: the other stays new.
        let moves = match_moves(std::slice::from_ref(&a), &[new("c.jpg", "h"), new("d.jpg", "h")]);
        assert_eq!(moves.len(), 1);
    }

    #[test]
    fn copies_that_vanished_together_each_find_the_one_in_their_own_folder() {
        let a = vanished("A/x.jpg", "h");
        let b = vanished("B/x.jpg", "h");
        // Listed the other way round: the folder, not the order, decides.
        let moves =
            match_moves(&[a.clone(), b.clone()], &[new("B/y/x.jpg", "h"), new("A/x.jpg", "h")]);
        assert_eq!(moves, vec![(a.0, "A/x.jpg".to_string()), (b.0, "B/y/x.jpg".to_string())]);
    }

    #[test]
    fn a_move_in_the_top_folder_works_without_a_folder() {
        let v = vanished("a.jpg", "h");
        let moves = match_moves(std::slice::from_ref(&v), &[new("b.jpg", "h")]);
        assert_eq!(moves, vec![(v.0, "b.jpg".to_string())]);
    }

    // ---- held_back --------------------------------------------------------

    #[test]
    fn a_pass_that_loses_most_of_the_catalog_is_held_back() {
        assert!(held_back(100, 51));
        assert!(held_back(40, 21));
        assert!(held_back(20, 20));
        assert!(held_back(1_000_000, 600_000));
    }

    #[test]
    fn exactly_half_or_fewer_than_the_minimum_is_not_held_back() {
        assert!(!held_back(100, 50), "exactly half is not more than half");
        assert!(!held_back(100, 49));
        assert!(!held_back(30, 19), "over half, but fewer than the minimum");
        assert!(held_back(30, HOLD_BACK_MIN));
        assert!(!held_back(30, HOLD_BACK_MIN - 1));
        assert!(!held_back(0, 0));
        assert!(!held_back(1_000_000, 0));
    }

    #[test]
    fn the_hold_back_arithmetic_cannot_overflow() {
        assert!(held_back(1, usize::MAX));
        assert!(!held_back(usize::MAX, HOLD_BACK_MIN));
    }

    // ---- stacks -----------------------------------------------------------

    fn entry(path: &str) -> (FileId, String) {
        (FileId::new(), path.to_string())
    }

    fn members(found: &[(FileId, Vec<FileId>)], top: FileId) -> Vec<FileId> {
        found.iter().find(|(t, _)| *t == top).map(|(_, m)| m.clone()).unwrap_or_default()
    }

    #[test]
    fn a_raw_file_goes_under_the_jpeg_of_its_name() {
        let jpg = entry("Trip/IMG_1.JPG");
        let raw = entry("Trip/IMG_1.CR2");
        let lone = entry("Trip/IMG_2.jpg");
        let found = stacks(&[raw.clone(), jpg.clone(), lone.clone()]);
        assert_eq!(found, vec![(jpg.0, vec![raw.0])]);
    }

    #[test]
    fn every_raw_extension_stacks() {
        for ext in ["dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf", "pef", "srw"] {
            let jpg = entry("a.jpg");
            let raw = entry(&format!("a.{ext}"));
            assert_eq!(stacks(&[jpg.clone(), raw.clone()]), vec![(jpg.0, vec![raw.0])], "{ext}");
        }
    }

    #[test]
    fn a_live_photos_film_goes_under_its_heic() {
        let heic = entry("IMG_0001.HEIC");
        let film = entry("IMG_0001.MOV");
        assert_eq!(stacks(&[film.clone(), heic.clone()]), vec![(heic.0, vec![film.0])]);
        // Heif too, and an Android film under its JPEG.
        let heif = entry("b.heif");
        let mp4 = entry("b.mp4");
        assert_eq!(stacks(&[heif.clone(), mp4.clone()]), vec![(heif.0, vec![mp4.0])]);
        let jpg = entry("c.jpeg");
        let mp4 = entry("c.MP4");
        assert_eq!(stacks(&[mp4.clone(), jpg.clone()]), vec![(jpg.0, vec![mp4.0])]);
    }

    #[test]
    fn a_jpeg_is_the_top_when_there_is_a_heic_as_well() {
        let heic = entry("a.heic");
        let jpg = entry("a.jpg");
        let raw = entry("a.dng");
        let film = entry("a.mov");
        for order in [
            vec![heic.clone(), jpg.clone(), raw.clone(), film.clone()],
            vec![film.clone(), raw.clone(), jpg.clone(), heic.clone()],
        ] {
            let found = stacks(&order);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].0, jpg.0, "the JPEG, not the HEIC, is the top");
            let mut under = found[0].1.clone();
            under.sort();
            let mut expected = vec![raw.0, film.0];
            expected.sort();
            assert_eq!(under, expected);
            // The HEIC is nobody's member and nobody's top here.
            assert!(!found[0].1.contains(&heic.0));
        }
    }

    #[test]
    fn names_match_without_regard_to_case_but_only_within_a_folder() {
        let jpg = entry("a/img_1.jpg");
        let raw = entry("a/IMG_1.DNG");
        let elsewhere = entry("b/IMG_1.dng");
        let found = stacks(&[jpg.clone(), raw.clone(), elsewhere.clone()]);
        assert_eq!(found, vec![(jpg.0, vec![raw.0])]);
        // A name in the top folder is not in the folder `a` or `b`.
        let top_jpg = entry("IMG_1.jpg");
        let found = stacks(&[top_jpg.clone(), elsewhere.clone(), jpg.clone()]);
        assert!(found.is_empty());
    }

    #[test]
    fn only_the_last_dot_starts_the_extension() {
        // `a.b.jpg` and `a.b.cr2` match; `a.jpg` and `a.b.cr2` do not.
        let jpg = entry("a.b.jpg");
        let raw = entry("a.b.cr2");
        let other = entry("a.jpg");
        assert_eq!(stacks(&[jpg.clone(), raw.clone()]), vec![(jpg.0, vec![raw.0])]);
        assert!(stacks(&[other, raw]).is_empty());
    }

    #[test]
    fn files_that_do_not_belong_together_are_left_alone() {
        // A RAW or a film with nothing to go under; a JPEG with only another
        // JPEG; a text file; a name without a stem or without an extension.
        let files = [
            entry("raw-only.dng"),
            entry("film-only.mov"),
            entry("a.jpg"),
            entry("A.jpeg"),
            entry("notes.txt"),
            entry("notes.jpg.txt"),
            entry(".jpg"),
            entry(".cr2"),
            entry("README"),
            entry("pic.png"),
            entry("pic.dng.png"),
        ];
        assert!(stacks(&files).is_empty());
        assert!(stacks(&[]).is_empty());
    }

    #[test]
    fn a_png_or_a_text_file_is_never_a_top() {
        let png = entry("a.png");
        let raw = entry("a.dng");
        assert!(stacks(&[png, raw]).is_empty());
    }

    #[test]
    fn several_stacks_come_out_ordered_and_each_member_is_in_one() {
        let (j1, r1) = (entry("2/b.jpg"), entry("2/b.nef"));
        let (j2, r2, f2) = (entry("1/a.jpg"), entry("1/a.arw"), entry("1/a.mp4"));
        let (j3, r3) = (entry("1/c.heic"), entry("1/c.mov"));
        let found = stacks(&[
            r1.clone(),
            j1.clone(),
            f2.clone(),
            r3.clone(),
            j2.clone(),
            j3.clone(),
            r2.clone(),
        ]);
        assert_eq!(
            found.iter().map(|(top, _)| *top).collect::<Vec<_>>(),
            vec![j2.0, j3.0, j1.0],
            "by folder, then name"
        );
        // Members by path, so the answer does not depend on the listing.
        assert_eq!(members(&found, j2.0), vec![r2.0, f2.0]);
        assert_eq!(members(&found, j3.0), vec![r3.0]);
        assert_eq!(members(&found, j1.0), vec![r1.0]);
        let mut seen = HashSet::new();
        for (top, under) in &found {
            assert!(!under.contains(top), "a top is never its own member");
            for member in under {
                assert!(seen.insert(*member), "a member is in one stack only");
            }
        }
    }

    #[test]
    fn two_jpegs_of_one_name_pick_the_first_path_as_top() {
        let lower = entry("a.jpg");
        let upper = entry("a.JPG");
        let raw = entry("a.dng");
        for order in [
            [lower.clone(), upper.clone(), raw.clone()],
            [upper.clone(), raw.clone(), lower.clone()],
        ] {
            // "a.JPG" sorts before "a.jpg".
            assert_eq!(stacks(&order), vec![(upper.0, vec![raw.0])]);
        }
    }
}
