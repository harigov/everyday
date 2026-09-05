//! The assembly point.
//!
//! [`everyday_core`] deliberately knows nothing about which storage backends
//! exist, and the backends know nothing about each other. This crate is
//! where they are introduced, so that both front ends -- the desktop shell
//! and the CLI -- get an identical set of backends, an identical default
//! vault location, and identical open/create semantics.
//!
//! Adding a storage backend means implementing
//! [`everyday_core::StoreFactory`] and adding one line to [`registry`].

pub mod media;

use everyday_core::store::BackendRegistry;
use everyday_core::{Error, Result, Vault, VaultConfig};
use everyday_store_markdown::MarkdownFactory;
use everyday_store_sqlite::SqliteFactory;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The backend the UI offers first.
pub const DEFAULT_BACKEND: &str = everyday_store_sqlite::BACKEND_ID;

/// Every storage backend this build knows how to open.
pub fn registry() -> Arc<BackendRegistry> {
    let mut reg = BackendRegistry::new();
    reg.register(SqliteFactory).register(MarkdownFactory);
    Arc::new(reg)
}

/// `(id, human description)` for each backend, for the vault-creation UI.
pub fn available_backends() -> Vec<(&'static str, &'static str)> {
    registry().describe_all()
}

/// Where a vault lives when the user has not chosen somewhere else.
///
/// Uses the platform's data directory rather than the document directory:
/// the vault is an opaque store with its own internal layout, not a file the
/// user is expected to open by hand. (A Markdown vault *is* meant to be
/// browsed, which is exactly why it is worth pointing somewhere memorable at
/// creation time.)
pub fn default_vault_dir() -> PathBuf {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".everyday"))
}

/// Open the vault at `path`, or report that there is nothing there.
///
/// An encrypted vault comes back **locked**; call
/// [`everyday_core::Vault::unlock`] with the password.
pub fn open(path: &Path) -> Result<Vault> {
    Vault::open(path, registry())
}

/// Create a new vault. Fails if one already exists at `path`.
pub fn create(path: &Path, config: VaultConfig) -> Result<Vault> {
    Vault::create(path, config, registry())
}

/// Open the vault at `path`, creating it from `config` if absent.
///
/// Returns the vault and whether it was created, so the caller can show a
/// welcome screen rather than a password prompt on first run.
pub fn open_or_create(path: &Path, config: VaultConfig) -> Result<(Vault, bool)> {
    if Vault::exists(path) {
        Ok((open(path)?, false))
    } else {
        Ok((create(path, config)?, true))
    }
}

/// Does a vault exist at `path`?
pub fn exists(path: &Path) -> bool {
    Vault::exists(path)
}

/// Reject a password the user would regret.
///
/// Deliberately minimal: length is the only property that reliably predicts
/// resistance to offline attack, and composition rules mostly push people
/// toward `Password1!`. There is no password recovery in Every Day -- the
/// data key is wrapped by this password and nothing else -- so the UI must
/// say so loudly rather than lean on a strength meter.
pub fn validate_password(password: &str) -> Result<()> {
    let chars = password.chars().count();
    if chars < 8 {
        return Err(Error::Invalid(format!(
            "password must be at least 8 characters (got {chars})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shipped_backends_are_registered() {
        let ids = registry().ids();
        assert!(ids.contains(&"sqlite"), "sqlite backend missing: {ids:?}");
        assert!(ids.contains(&"markdown"), "markdown backend missing: {ids:?}");
    }

    #[test]
    fn every_backend_has_a_description_for_the_picker() {
        for (id, desc) in available_backends() {
            assert!(!desc.is_empty(), "backend {id} has no description");
        }
    }

    #[test]
    fn the_default_backend_is_actually_registered() {
        assert!(registry().ids().contains(&DEFAULT_BACKEND));
    }

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
    fn a_vault_can_be_created_on_either_backend() {
        for backend in ["sqlite", "markdown"] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = VaultConfig {
                backend: backend.into(),
                password: Some("correct horse battery".into()),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            };
            let vault = create(dir.path(), cfg).unwrap();
            assert_eq!(vault.status().backend, backend);
        }
    }

    #[test]
    fn the_default_vault_directory_is_absolute_and_named() {
        let dir = default_vault_dir();
        assert!(dir.is_absolute(), "got a relative path: {dir:?}");
        // Each platform spells it differently -- `~/.local/share/everyday`,
        // `~/Library/Application Support/app.Every-Day.EveryDay`,
        // `%APPDATA%\\Every Day\\EveryDay` -- so match case-insensitively.
        let s = dir.to_string_lossy().to_lowercase().replace([' ', '-'], "");
        assert!(s.contains("everyday"), "got {dir:?}");
    }

    #[test]
    fn short_passwords_are_rejected() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("").is_err());
        assert!(validate_password("12345678").is_ok());
        // Length is counted in characters, not bytes.
        assert!(validate_password("\u{1f600}\u{1f600}\u{1f600}").is_err());
    }
}
