//! The backend registry, and the settings a vault's backend needs before it
//! can be created, opened or repaired.
//!
//! Moved out of `everyday_vault::tests` along with the rest of this crate's
//! end-to-end coverage: these exercise `registry`, `create` and `open` as a
//! caller outside the crate would, over the real SQLite and Postgres
//! factories -- Postgres only as far as its settings, since reaching its
//! database is `everyday-store-postgres`'s job.

use everyday_core::{BackendSettings, VaultConfig};
use everyday_vault::{
    DEFAULT_BACKEND, available_backends, backend_settings, create, open, open_dormant, registry,
};

#[test]
fn both_shipped_backends_are_registered() {
    let ids = registry().ids();
    assert!(ids.contains(&"sqlite"), "sqlite backend missing: {ids:?}");
    assert!(ids.contains(&"postgres"), "postgres backend missing: {ids:?}");
}

#[test]
fn every_backend_has_a_name_and_a_description_for_the_picker() {
    for b in available_backends() {
        assert!(!b.name.is_empty(), "backend {} has no name", b.id);
        assert!(!b.description.is_empty(), "backend {} has no description", b.id);
    }
}

#[test]
fn a_backend_that_needs_configuring_says_so_before_a_vault_is_made() {
    // The setup screen reads this to decide whether to show a
    // connection field, and `create` reads it to refuse early.
    assert!(backend_settings("sqlite").unwrap().is_empty());
    let pg = backend_settings("postgres").unwrap();
    assert!(pg.iter().any(|s| s.key == "url" && s.required));
    assert_eq!(backend_settings("mysql").unwrap_err().code(), "unknown_backend");
}

#[test]
fn creating_a_vault_without_the_settings_its_backend_needs_is_refused() {
    // And refused *before* anything is written: a directory holding a
    // header and no database is worse than no directory at all.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let err = create(
        &path,
        VaultConfig { backend: "postgres".into(), password: None, ..Default::default() },
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid", "got {err}");
    assert!(!path.exists(), "a refused create must leave nothing behind");
}

#[test]
fn a_backend_settings_round_trip_survives_a_lock_and_unlock() {
    // The connection URL is sealed under the data key, so this is the
    // check that it can still be read after a real unlock -- and that it
    // is not sitting in the header in the clear.
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    // Any backend will do; what is under test is the sealing, not the
    // database. SQLite ignores settings it was not asked about.
    cfg.settings.set("url", "postgresql://someone:hunter2@example.com/db");
    let vault = create(dir.path(), cfg).unwrap();
    let pw = Some("correct horse battery");
    assert_eq!(
        vault.backend_settings(None).unwrap().get("url"),
        Some("postgresql://someone:hunter2@example.com/db"),
        "an unlocked vault already holds the key and needs no password",
    );
    drop(vault);

    let header = std::fs::read_to_string(dir.path().join("vault.json")).unwrap();
    assert!(!header.contains("hunter2"), "the password must not be in the plaintext header");

    let vault = open(dir.path()).unwrap();
    assert_eq!(
        vault.backend_settings(None).unwrap_err().code(),
        "bad_password",
        "a locked vault must not hand out its database credential",
    );
    assert_eq!(vault.backend_settings(Some("wrong")).unwrap_err().code(), "bad_password");
    assert_eq!(
        vault.backend_settings(pw).unwrap().get("url"),
        Some("postgresql://someone:hunter2@example.com/db"),
        "the password alone is enough: reading this must not need the store",
    );

    // And it can be changed without recreating the vault, which is what
    // a rotated database password needs -- while still locked, because a
    // vault whose credential is wrong is one that cannot be unlocked.
    let moved = everyday_core::BackendSettings::with_url("postgresql://elsewhere/db");
    vault.set_backend_settings(pw, moved).unwrap();
    assert_eq!(vault.backend_settings(pw).unwrap().get("url"), Some("postgresql://elsewhere/db"));
}

#[test]
fn a_vault_whose_backend_will_not_open_leaves_nothing_behind() {
    // The regression that made a hosted vault unpleasant to get wrong:
    // the header is written before the backend is opened, so a typo in a
    // connection URL used to leave a directory that counted as a vault,
    // and the corrected retry was refused as `already_initialised`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let cfg = VaultConfig {
        backend: "postgres".into(),
        // Nothing is listening on this port, and connecting is the last
        // thing `create` does.
        settings: BackendSettings::with_url("postgresql://postgres@127.0.0.1:59999/nope"),
        password: None,
        ..Default::default()
    };

    assert!(create(&path, cfg.clone()).is_err());
    assert!(!everyday_core::Vault::exists(&path), "a failed create leaves no vault behind");
    assert!(!path.exists(), "nor the directory it made for it");
    // And the retry, which is the whole point, gets as far as the same
    // failure rather than being refused before it starts.
    assert_ne!(create(&path, cfg).unwrap_err().code(), "already_initialised");
}

#[test]
fn the_backend_settings_of_a_vault_that_cannot_be_opened_are_still_reachable() {
    // The repair path. A vault pointing at a database that has moved
    // cannot be opened or unlocked -- both reach for the connection -- so
    // if this needed either, the documented way to fix a rotated password
    // would be unusable in exactly the case it is for.
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    cfg.settings.set("url", "postgresql://gone.example.com/db");
    drop(create(dir.path(), cfg).unwrap());

    let vault = open_dormant(dir.path()).unwrap();
    assert!(!vault.is_unlocked(), "a dormant vault has opened nothing");
    let pw = Some("correct horse battery");
    assert_eq!(
        vault.backend_settings(pw).unwrap().get("url"),
        Some("postgresql://gone.example.com/db"),
    );
    vault
        .set_backend_settings(pw, BackendSettings::with_url("postgresql://here.example.com/db"))
        .unwrap();
    drop(vault);

    assert_eq!(
        open_dormant(dir.path()).unwrap().backend_settings(pw).unwrap().get("url"),
        Some("postgresql://here.example.com/db"),
        "the repaired URL must survive to the next open",
    );
}

#[test]
fn the_default_backend_is_actually_registered() {
    assert!(registry().ids().contains(&DEFAULT_BACKEND));
}
