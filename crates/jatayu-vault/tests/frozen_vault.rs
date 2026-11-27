//! Phase 0.3's regression net: a vault built by a past build of this code,
//! read back by today's.
//!
//! Two checked-in vaults live under `tests/fixtures/frozen/` -- one
//! encrypted, one plaintext -- each holding one record of every kind the
//! `Vault` API can write. This test:
//!
//! 1. copies each into a fresh temp directory (never opens the checked-in
//!    copy in place -- a lock file, a `-wal`, anything this process wrote
//!    back would corrupt what is committed);
//! 2. opens it, dumps every record through the public `Vault` API, and
//!    compares the dump with a checked-in golden file;
//! 3. checks the one attachment's blob bytes independently of the dump;
//! 4. writes one more record of each kind into the copy, closes it,
//!    reopens it, dumps again, and compares with a second golden file.
//!
//! A later refactor that changes either golden file has changed
//! persistence -- see `docs/plans/architecture-refactor.md`'s "the one
//! rule". Regenerate both fixture and goldens with
//! `fixture_writer.rs`'s own doc comment.

mod support;

use std::path::{Path, PathBuf};
use support::fixture::{ENCRYPTED_PASSWORD, GEN_APPENDED, GEN_FROZEN, TS_APPENDED, blob_bytes_for};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frozen")
}

fn golden_path(name: &str, generation: &str) -> PathBuf {
    fixtures_dir().join(format!("{name}.{generation}.json"))
}

/// The entry attachment's blob, fetched and checked byte for byte -- the
/// dump only carries its UTF-8 text, so this is the independent check the
/// plan asks for. Found through the entries that hold it rather than
/// recomputed from its bytes: an encrypted vault's blob addresses are keyed
/// by its own key (see `jatayu_core::blobstore::address`), and the frozen
/// generation's predate that, so neither hash can be assumed.
fn assert_blob_bytes(vault: &jatayu_core::Vault, generation: u128) {
    let expected = blob_bytes_for(generation);
    let held = vault
        .all_entries()
        .unwrap()
        .into_iter()
        .flat_map(|entry| entry.attachments)
        .any(|attachment| vault.blob(attachment.blob).ok().as_deref() == Some(&expected[..]));
    assert!(held, "generation {generation:x}'s attachment blob");
}

/// How the copy's database is named before it is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
enum Layout {
    /// As the fixture was written.
    AsWritten,
    /// As Karma Yoga left a vault that Every Day made: the database renamed
    /// to `karmayoga.db`, and `everyday.db` a relative link to it.
    RenamedByKarmaYoga,
}

fn is_file(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_file())
}

fn run(name: &str, password: Option<&str>, layout: Layout) {
    let fixture = fixtures_dir().join(name);
    assert!(
        fixture.is_dir(),
        "no fixture at {}; see fixture_writer.rs to build one",
        fixture.display()
    );

    let work = tempfile::tempdir().unwrap();
    jatayu_core::fsutil::copy_tree(&fixture, work.path()).unwrap();

    let store = work.path().join("store");
    #[cfg(unix)]
    if layout == Layout::RenamedByKarmaYoga {
        std::fs::rename(store.join("everyday.db"), store.join("karmayoga.db")).unwrap();
        std::os::unix::fs::symlink("karmayoga.db", store.join("everyday.db")).unwrap();
    }
    #[cfg(not(unix))]
    assert_eq!(layout, Layout::AsWritten, "the other layouts are made of links");

    // A fixture written before the renames has the database under an
    // earlier name -- `everyday.db`, or `karmayoga.db` once laid out as Karma
    // Yoga left it -- which opening it, holding the vault, moves straight to
    // the new one. One regenerated since has the new name already.
    let database_was =
        ["karmayoga.db", "everyday.db"].into_iter().find(|name| is_file(&store.join(name)));
    let vault = jatayu_vault::open(work.path()).unwrap();
    if let Some(password) = password {
        vault.unlock(Some(password)).unwrap();
    }
    assert!(is_file(&store.join("jatayu.db")), "the database was not renamed");
    for old in ["everyday.db", "karmayoga.db"] {
        for log in ["-wal", "-shm"] {
            let old = format!("{old}{log}");
            assert!(!store.join(&old).exists(), "{old} is still there after the rename");
        }
    }
    // What is left at the old name is a link to the new one, for an old
    // build started again; a link an earlier rename left is as it was, and
    // leads there through it.
    #[cfg(unix)]
    {
        let link = |name: &str| std::fs::read_link(store.join(name)).ok();
        if let Some(was) = database_was {
            assert_eq!(link(was).as_deref(), Some(Path::new("jatayu.db")), "{was}");
        }
        if layout == Layout::RenamedByKarmaYoga {
            assert_eq!(link("everyday.db").as_deref(), Some(Path::new("karmayoga.db")));
            assert!(store.join("everyday.db").canonicalize().unwrap().ends_with("jatayu.db"));
        }
    }
    let _ = database_was;

    let dump1 = support::dump::dump(&vault);
    support::compare(
        &golden_path(name, "frozen"),
        &dump1,
        "the frozen fixture reads back differently than it did when it was written down. \
         If this branch is a refactor, it has changed persistence -- see \
         docs/plans/architecture-refactor.md's \"the one rule\". If it is meant to (a new \
         record kind, a deliberate migration), regenerate with fixture_writer.rs's own doc \
         comment and explain the diff in the PR.",
        "UPDATE_SURFACE=1 ./scripts/test.sh -p jatayu-vault --test frozen_vault",
    );
    assert_blob_bytes(&vault, GEN_FROZEN);

    // Write one more of everything, with today's code, into the same copy.
    support::fixture::build(&vault, GEN_APPENDED, TS_APPENDED);

    // Close and reopen, so the dump below reads back what was actually
    // committed rather than what this process still has cached.
    vault.lock();
    drop(vault);
    let vault = jatayu_vault::open(work.path()).unwrap();
    if let Some(password) = password {
        vault.unlock(Some(password)).unwrap();
    }

    let dump2 = support::dump::dump(&vault);
    support::compare(
        &golden_path(name, "appended"),
        &dump2,
        "today's write path no longer produces the same readable result as it did when this \
         golden file was written -- see docs/plans/architecture-refactor.md's \"the one rule\".",
        "UPDATE_SURFACE=1 ./scripts/test.sh -p jatayu-vault --test frozen_vault",
    );
    assert_blob_bytes(&vault, GEN_APPENDED);
}

#[test]
fn an_encrypted_vault_from_before_reads_back_the_same() {
    run("encrypted", Some(ENCRYPTED_PASSWORD), Layout::AsWritten);
}

#[test]
fn a_plaintext_vault_from_before_reads_back_the_same() {
    run("plaintext", None, Layout::AsWritten);
}

/// The owner's vault: made by Every Day, carried across by Karma Yoga, and
/// opened now by Jatayu -- whose database moves from `karmayoga.db`, the
/// file, never from `everyday.db`, the link to it.
#[test]
#[cfg(unix)]
fn a_vault_karma_yoga_carried_across_reads_back_the_same() {
    run("encrypted", Some(ENCRYPTED_PASSWORD), Layout::RenamedByKarmaYoga);
}

/// Never open the checked-in fixture directly -- a bug in this test finding
/// a path bypassing [`support::fixture`]'s temp-copy step would write
/// straight into what is committed. This is not what [`run`] does; it is a
/// second line of defence, asserting after the fact that the fixture
/// directory itself was not touched.
#[test]
fn the_checked_in_fixtures_are_left_alone() {
    for name in ["encrypted", "plaintext"] {
        let dir = fixtures_dir().join(name);
        // Whichever name the database was written under, nothing beside it
        // -- a log, a lock, or a renamed copy, which would mean it had been
        // opened in place.
        let renamed_in_place = Path::new(&dir).join("store/everyday.db").exists()
            && Path::new(&dir).join("store/jatayu.db").exists();
        assert!(!renamed_in_place, "{name}'s checked-in fixture was opened in place");
        for leftover in [
            "store/everyday.db-wal",
            "store/everyday.db-shm",
            "store/karmayoga.db",
            "store/karmayoga.db-wal",
            "store/karmayoga.db-shm",
            "store/jatayu.db-wal",
            "store/jatayu.db-shm",
            "vault.lock",
        ] {
            assert!(
                !Path::new(&dir).join(leftover).exists(),
                "{name}'s checked-in fixture has a {leftover} it should not: something opened \
                 it in place instead of copying it first"
            );
        }
    }
}
