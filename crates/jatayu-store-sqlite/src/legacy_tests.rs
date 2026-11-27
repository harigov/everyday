//! The database carried across from the names it had before -- Karma Yoga's
//! `karmayoga.db`, Every Day's `everyday.db` -- straight to `jatayu.db`,
//! whichever a vault has, and the links an earlier rename left still leading
//! to it. Unix only: what is checked is mostly links.

use super::{DB_FILENAME, db_file, is_the_database_or_a_log};
use jatayu_core::crypto::{Cipher, NullCipher};
use jatayu_core::store::StoreContext;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn ctx(root: &Path, exclusive: bool) -> StoreContext {
    let cipher: Arc<dyn Cipher> = Arc::new(NullCipher);
    StoreContext::new(root, cipher).exclusive(exclusive)
}

/// A database with one row in it at `path`, in WAL mode as the app keeps it,
/// closed again.
fn database(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch("CREATE TABLE t(v TEXT); INSERT INTO t VALUES ('the records');").unwrap();
}

/// The row, read through whatever `path` is.
fn read(path: &Path) -> String {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT v FROM t", [], |row| row.get(0)).unwrap()
}

fn link(target: &str, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

fn read_link(path: &Path) -> PathBuf {
    std::fs::read_link(path).unwrap_or_else(|e| panic!("{} is not a link: {e}", path.display()))
}

fn is_file(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_file())
}

/// The store laid out as Karma Yoga left it on a machine that had Every Day
/// before: `karmayoga.db` the database, `everyday.db` a relative link to it.
fn owners_store(root: &Path) {
    database(&root.join("karmayoga.db"));
    link("karmayoga.db", &root.join("everyday.db"));
}

#[test]
fn the_owners_database_moves_from_karma_yoga_and_every_days_link_still_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    owners_store(root);

    // A process that does not hold the vault reads the database itself,
    // under the name it has, and renames nothing.
    assert_eq!(db_file(&ctx(root, false)), root.join("karmayoga.db"));
    assert!(is_file(&root.join("karmayoga.db")) && !root.join(DB_FILENAME).exists());

    assert_eq!(db_file(&ctx(root, true)), root.join(DB_FILENAME));
    // The file moved, not a link to it.
    assert!(is_file(&root.join(DB_FILENAME)));
    assert_eq!(read_link(&root.join("karmayoga.db")), Path::new(DB_FILENAME));
    // Every Day's link is as it was, and reaches the database through
    // Karma Yoga's.
    assert_eq!(read_link(&root.join("everyday.db")), Path::new("karmayoga.db"));
    for name in [DB_FILENAME, "karmayoga.db", "everyday.db"] {
        assert_eq!(read(&root.join(name)), "the records", "through {name}");
    }
    for leftover in ["karmayoga.db-wal", "karmayoga.db-shm", "jatayu.db-wal", "jatayu.db-shm"] {
        assert!(!root.join(leftover).exists(), "{leftover}");
    }

    // And once is enough.
    assert_eq!(db_file(&ctx(root, true)), root.join(DB_FILENAME));
    assert_eq!(read_link(&root.join("karmayoga.db")), Path::new(DB_FILENAME));
}

#[test]
fn an_every_day_database_moves_straight_to_jatayu() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    database(&root.join("everyday.db"));

    assert_eq!(db_file(&ctx(root, true)), root.join(DB_FILENAME));
    assert!(is_file(&root.join(DB_FILENAME)));
    assert_eq!(read_link(&root.join("everyday.db")), Path::new(DB_FILENAME));
    assert_eq!(read(&root.join("everyday.db")), "the records");
    // Never by way of Karma Yoga's name.
    assert!(root.join("karmayoga.db").symlink_metadata().is_err());
}

#[test]
fn a_fresh_store_gets_the_new_name() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(db_file(&ctx(dir.path(), true)), dir.path().join(DB_FILENAME));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing made or moved");
}

#[test]
fn a_database_still_open_elsewhere_keeps_its_name_until_it_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    owners_store(root);
    // An old build with it open: a connection that has read keeps the log's
    // index mapped, so closing the one that folded the log in does not
    // delete it.
    let held = rusqlite::Connection::open(root.join("karmayoga.db")).unwrap();
    let _: i64 = held.query_row("SELECT count(*) FROM t", [], |row| row.get(0)).unwrap();

    assert_eq!(db_file(&ctx(root, true)), root.join("karmayoga.db"));
    assert!(is_file(&root.join("karmayoga.db")) && !root.join(DB_FILENAME).exists());

    drop(held);
    assert_eq!(db_file(&ctx(root, true)), root.join(DB_FILENAME));
    assert_eq!(read(&root.join("everyday.db")), "the records");
}

#[test]
fn a_link_to_a_database_elsewhere_moves_as_the_link_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let disk = tempfile::tempdir().unwrap();
    let root = dir.path();
    let elsewhere = disk.path().join("journal.db");
    database(&elsewhere);
    link(elsewhere.to_str().unwrap(), &root.join("karmayoga.db"));
    link("karmayoga.db", &root.join("everyday.db"));

    assert_eq!(db_file(&ctx(root, true)), root.join(DB_FILENAME));
    assert_eq!(read_link(&root.join(DB_FILENAME)), elsewhere);
    assert_eq!(read_link(&root.join("karmayoga.db")), Path::new(DB_FILENAME));
    assert!(is_file(&elsewhere));
    assert_eq!(read(&root.join("everyday.db")), "the records");
}

#[test]
fn every_name_the_database_had_is_left_out_of_the_files_a_backup_copies() {
    for database in [DB_FILENAME, "karmayoga.db", "everyday.db"] {
        assert!(is_the_database_or_a_log(database), "{database}");
        assert!(is_the_database_or_a_log(&format!("{database}-wal")), "{database}-wal");
    }
    assert!(is_the_database_or_a_log("chat/abc/matrix-sdk-crypto.sqlite3-shm"));
    // Only at the top of the store: another database deeper down is copied
    // as a database of its own.
    assert!(!is_the_database_or_a_log("chat/abc/karmayoga.db"));
    assert!(!is_the_database_or_a_log("media/ab/cdef"));
}
