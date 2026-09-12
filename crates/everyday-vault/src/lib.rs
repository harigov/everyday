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

pub mod autounlock;
pub mod media;

use everyday_core::store::{BackendInfo, BackendRegistry, BackendSettings, SettingSpec};
use everyday_core::{Error, Result, Vault, VaultConfig};
use everyday_store_postgres::PostgresFactory;
use everyday_store_sqlite::SqliteFactory;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The backend the UI offers first.
pub const DEFAULT_BACKEND: &str = everyday_store_sqlite::BACKEND_ID;

/// Every storage backend this build knows how to open.
///
/// Registration order is picker order, and it is deliberate: the local
/// database first, because it is the right answer for one person on one
/// machine and needs nothing configured.
pub fn registry() -> Arc<BackendRegistry> {
    let mut reg = BackendRegistry::new();
    reg.register(SqliteFactory).register(PostgresFactory);
    Arc::new(reg)
}

/// What the vault-creation screen needs about each backend.
pub fn available_backends() -> Vec<BackendInfo> {
    registry().describe_all()
}

/// What `backend` must be configured with before it can be opened.
///
/// Empty for a backend that needs only a directory. The interface asks this
/// so it can render the fields for a chosen backend without knowing that
/// Postgres, or anything after it, exists.
pub fn backend_settings(backend: &str) -> Result<Vec<SettingSpec>> {
    registry().settings_for(backend).ok_or_else(|| Error::UnknownBackend(backend.to_string()))
}

/// Reject a backend configuration that would fail at open time.
///
/// Called before a vault is created, so that a missing connection URL is a
/// message on the setup screen rather than a half-made vault directory with
/// a header in it and no database behind it.
pub fn validate_settings(backend: &str, settings: &BackendSettings) -> Result<()> {
    for spec in backend_settings(backend)? {
        if spec.required && settings.get(spec.key).is_none() {
            return Err(Error::Invalid(format!(
                "the {backend} backend needs {}, and none was given",
                spec.label.to_lowercase()
            )));
        }
    }
    Ok(())
}

/// Where a vault lives when the user has not chosen somewhere else.
///
/// Uses the platform's data directory rather than the document directory:
/// the vault is an opaque store with its own internal layout, not a file the
/// user is expected to open by hand. It is still a real directory wherever
/// the records live -- a Postgres vault keeps its header, its lock and its
/// search index here and only its rows on the server -- so this is the path
/// to point at when someone asks where their journal is.
pub fn default_vault_dir() -> PathBuf {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".everyday"))
}

/// Where this application keeps its own settings, as opposed to a vault.
///
/// The pointer to the last vault, the server's configuration and certificate,
/// the list of paired devices, the servers this copy has paired *with*. None of
/// it is vault content and none of it belongs in a vault directory -- which
/// matters most for the two credentials among it, because `everyday backup`
/// copies a vault and a private key kept there would end up in every backup
/// somebody ever made.
pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".everyday-config"))
}

/// Where the shell records the vault it last had open.
///
/// A vault need not live in [`default_vault_dir`] -- someone may keep theirs
/// on an external disk or in a synced folder -- and being sent back to the
/// default location on every launch would make that unusable. This file is a
/// pointer and nothing else: it holds a path, never a key, a password or any
/// entry content. The default location is public knowledge anyway.
fn last_vault_pointer() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.config_dir().join("last-vault"))
}

/// The vault the previous session left open, if one was recorded.
///
/// The path comes back exactly as it was recorded. Whether a vault is still
/// there is the caller's question to ask -- an unplugged drive is not an
/// error here, it just means the default location should be used instead.
pub fn last_vault() -> Option<PathBuf> {
    read_pointer(&last_vault_pointer()?)
}

/// Record `path` as the vault to reopen on the next launch.
pub fn remember_vault(path: &Path) -> Result<()> {
    let file = last_vault_pointer().ok_or_else(|| {
        Error::Invalid("this platform has no config directory to record the vault in".into())
    })?;
    write_pointer(&file, path)
}

fn read_pointer(file: &Path) -> Option<PathBuf> {
    let recorded = std::fs::read_to_string(file).ok()?;
    let recorded = recorded.trim();
    if recorded.is_empty() {
        return None;
    }
    Some(PathBuf::from(recorded))
}

fn write_pointer(file: &Path, path: &Path) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(file, path.to_string_lossy().as_bytes()).map_err(|e| Error::io(file, e))
}

/// Open the vault at `path`, or report that there is nothing there.
///
/// An encrypted vault comes back **locked**; call
/// [`everyday_core::Vault::unlock`] with the password.
pub fn open(path: &Path) -> Result<Vault> {
    Vault::open(path, registry())
}

/// Open a vault's header without opening its storage backend.
///
/// For fixing a vault that cannot be opened the ordinary way: a database that
/// has moved, a password that has been rotated. See
/// [`everyday_core::Vault::open_dormant`].
pub fn open_dormant(path: &Path) -> Result<Vault> {
    Vault::open_dormant(path, registry())
}

/// Create a new vault. Fails if one already exists at `path`.
pub fn create(path: &Path, config: VaultConfig) -> Result<Vault> {
    validate_settings(&config.backend, &config.settings)?;
    Vault::create(path, config, registry())
}

/// Open the vault at `path`, creating it from `config` if absent.
///
/// Returns the vault and whether it was created, so the caller can show a
/// welcome screen rather than a password prompt on first run.
pub fn open_or_create(path: &Path, config: VaultConfig) -> Result<(Vault, bool)> {
    if Vault::exists(path) { Ok((open(path)?, false)) } else { Ok((create(path, config)?, true)) }
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

// The rest of this crate's tests moved to `tests/`, split by subject the way
// `everyday_core::agent::tools` is: they are end-to-end tests of a real vault
// over the real SQLite backend, and belong beside the other integration
// tests rather than inside the crate as unit tests. Three stay here because
// they exercise `read_pointer` and `write_pointer`, which are private and
// therefore unreachable from a test outside this module.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_vault_path_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("last-vault");
        let vault = dir.path().join("somewhere else/my vault");

        write_pointer(&file, &vault).unwrap();
        assert_eq!(read_pointer(&file), Some(vault));
    }

    #[test]
    fn recording_creates_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("never/made/yet/last-vault");

        write_pointer(&file, Path::new("/tmp/vault")).unwrap();
        assert!(file.exists(), "parent directories should be created");
    }

    #[test]
    fn nothing_recorded_means_no_last_vault() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_pointer(&dir.path().join("absent")), None);

        // A truncated or hand-emptied pointer is "nothing recorded", not a
        // vault at the empty path.
        let blank = dir.path().join("blank");
        std::fs::write(&blank, "  \n").unwrap();
        assert_eq!(read_pointer(&blank), None);
    }
}
