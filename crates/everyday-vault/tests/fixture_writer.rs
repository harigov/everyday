//! Builds the frozen vaults `frozen_vault.rs` reads back.
//!
//! Run only on demand -- `#[ignore]`d, and gated by `EVERYDAY_WRITE_FIXTURE`
//! besides, so a stray `cargo test -- --ignored` cannot silently overwrite
//! the checked-in fixture. What it produces is *today's* answer to "what
//! does a vault holding one of everything look like", frozen at the moment
//! this ran; see `docs/plans/architecture-refactor.md`, Phase 0.3, for why
//! that answer is the thing worth pinning.
//!
//! To regenerate the fixture and both golden dumps in one pass:
//!
//! ```sh
//! export CARGO_TARGET_DIR=$HOME/.cache/ed-refactor/t2
//! EVERYDAY_WRITE_FIXTURE=1 ./scripts/test.sh -p everyday-vault --test fixture_writer -- --ignored
//! UPDATE_SURFACE=1 ./scripts/test.sh -p everyday-vault --test frozen_vault
//! ```
//!
//! Read both diffs. Per the plan's "one rule": on any branch other than the
//! one adding a *new* record kind to the vault, a diff here is not this
//! phase's to make.

mod support;

use everyday_core::VaultConfig;
use everyday_core::crypto::KdfParams;
use std::path::{Path, PathBuf};
use support::fixture::{ENCRYPTED_PASSWORD, GEN_FROZEN, TS_FROZEN, build};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frozen")
}

/// Build `vault` at a scratch path, fill it with [`build`], and back it up
/// -- checkpointed, `VACUUM INTO`'d, no `-wal`/`-shm` left over -- into
/// `fixtures/frozen/<name>`. See [`everyday_core::Vault::backup`] for why
/// that is the one path that produces a clean, reopenable directory rather
/// than a copy of files still shared with a live connection.
fn freeze(name: &str, config: VaultConfig) {
    let scratch = tempfile::tempdir().unwrap();
    let vault = everyday_vault::create(scratch.path(), config).unwrap();
    build(&vault, GEN_FROZEN, TS_FROZEN);

    let dest = fixtures_dir().join(name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).unwrap();
    }
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    vault.backup(&dest).unwrap();
    // `Vault::backup`'s `VACUUM INTO` leaves nothing to clean up, but a
    // reader must never mistake this checked-in copy for something to open
    // in place -- see `frozen_vault.rs`, which always copies it into a
    // fresh temp directory first.
    assert!(!Path::new(&dest).join("store/everyday.db-wal").exists());
    assert!(!Path::new(&dest).join("store/everyday.db-shm").exists());
}

#[test]
#[ignore = "writes the checked-in fixture; run on demand, see this file's own docs"]
fn write_the_frozen_fixture() {
    if std::env::var("EVERYDAY_WRITE_FIXTURE").is_err() {
        eprintln!(
            "skipping: set EVERYDAY_WRITE_FIXTURE=1 to actually (re)write \
             crates/everyday-vault/tests/fixtures/frozen/"
        );
        return;
    }

    freeze(
        "encrypted",
        VaultConfig {
            password: Some(ENCRYPTED_PASSWORD.into()),
            kdf: KdfParams::insecure_fast(),
            ..Default::default()
        },
    );
    freeze("plaintext", VaultConfig { password: None, ..Default::default() });
}
