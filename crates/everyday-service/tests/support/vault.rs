//! A vault to run commands against, in a directory nobody has to clean up.
//!
//! `tests/call.rs` and `tests/transfer.rs` each built this by hand: a vault
//! in a temporary directory, wrapped in a [`Service`] with nothing else.
//! Shared here for the same reason `support::compare` is shared rather than
//! written twice -- the second copy is where the drift starts, and a vault
//! fixture is exactly the kind of thing a later test would build slightly
//! differently from the first without meaning anything by it.

use std::sync::Arc;

use everyday_service::Service;

/// A vault in a temporary directory, with a service around it and one
/// journal to file an entry under.
///
/// A vault with no journal is a dead end -- and it is whoever creates one,
/// the shell's `create_vault` or a test's fixture, that gives it its first.
///
/// `password` is `None` for the tests that are not about locking at all.
/// Where it is `Some`, the KDF is the cheap one: these tests are about which
/// door is being knocked on, not about how long knocking takes.
pub fn service(password: Option<&str>) -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: password.map(str::to_string),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    vault.save_journal(&everyday_core::Journal::new("Test")).unwrap();
    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}
