//! Opening and creating a vault through the functions a front end actually
//! calls, rather than through `everyday_core::Vault` directly.

use everyday_core::VaultConfig;
use everyday_vault::{create, open_or_create};

#[test]
fn open_or_create_creates_then_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };

    let (vault, created) = open_or_create(&path, cfg.clone()).unwrap();
    assert!(created);
    assert!(vault.is_unlocked(), "a freshly created vault is already unlocked");
    drop(vault);

    let (vault, created) = open_or_create(&path, cfg).unwrap();
    assert!(!created, "the second call must open, not create");
    assert!(!vault.is_unlocked(), "an existing encrypted vault opens locked");
}

#[test]
fn a_vault_records_which_backend_made_it() {
    // Only the local backend is exercised here; the Postgres one needs a
    // server, and `everyday-store-postgres` is where that lives.
    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        backend: "sqlite".into(),
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = create(dir.path(), cfg).unwrap();
    assert_eq!(vault.status().backend, "sqlite");
}
