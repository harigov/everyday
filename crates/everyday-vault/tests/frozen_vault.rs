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
/// plan asks for. Blobs are content-addressed (see
/// `everyday_core::id::BlobId::of`), so the id needs no lookup: it is
/// exactly the hash of the bytes [`blob_bytes_for`] fixes.
fn assert_blob_bytes(vault: &everyday_core::Vault, generation: u128) {
    let expected = blob_bytes_for(generation);
    let id = everyday_core::id::BlobId::of(&expected);
    assert_eq!(vault.blob(id).unwrap(), expected, "generation {generation:x}'s attachment blob");
}

fn run(name: &str, password: Option<&str>) {
    let fixture = fixtures_dir().join(name);
    assert!(
        fixture.is_dir(),
        "no fixture at {}; see fixture_writer.rs to build one",
        fixture.display()
    );

    let work = tempfile::tempdir().unwrap();
    everyday_core::fsutil::copy_tree(&fixture, work.path()).unwrap();

    let vault = everyday_vault::open(work.path()).unwrap();
    if let Some(password) = password {
        vault.unlock(Some(password)).unwrap();
    }

    let dump1 = support::dump::dump(&vault);
    support::compare(
        &golden_path(name, "frozen"),
        &dump1,
        "the frozen fixture reads back differently than it did when it was written down. \
         If this branch is a refactor, it has changed persistence -- see \
         docs/plans/architecture-refactor.md's \"the one rule\". If it is meant to (a new \
         record kind, a deliberate migration), regenerate with fixture_writer.rs's own doc \
         comment and explain the diff in the PR.",
        "UPDATE_SURFACE=1 ./scripts/test.sh -p everyday-vault --test frozen_vault",
    );
    assert_blob_bytes(&vault, GEN_FROZEN);

    // Write one more of everything, with today's code, into the same copy.
    support::fixture::build(&vault, GEN_APPENDED, TS_APPENDED);

    // Close and reopen, so the dump below reads back what was actually
    // committed rather than what this process still has cached.
    vault.lock();
    drop(vault);
    let vault = everyday_vault::open(work.path()).unwrap();
    if let Some(password) = password {
        vault.unlock(Some(password)).unwrap();
    }

    let dump2 = support::dump::dump(&vault);
    support::compare(
        &golden_path(name, "appended"),
        &dump2,
        "today's write path no longer produces the same readable result as it did when this \
         golden file was written -- see docs/plans/architecture-refactor.md's \"the one rule\".",
        "UPDATE_SURFACE=1 ./scripts/test.sh -p everyday-vault --test frozen_vault",
    );
    assert_blob_bytes(&vault, GEN_APPENDED);
}

#[test]
fn an_encrypted_vault_from_before_reads_back_the_same() {
    run("encrypted", Some(ENCRYPTED_PASSWORD));
}

#[test]
fn a_plaintext_vault_from_before_reads_back_the_same() {
    run("plaintext", None);
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
        for leftover in ["store/everyday.db-wal", "store/everyday.db-shm", "vault.lock"] {
            assert!(
                !Path::new(&dir).join(leftover).exists(),
                "{name}'s checked-in fixture has a {leftover} it should not: something opened \
                 it in place instead of copying it first"
            );
        }
    }
}
