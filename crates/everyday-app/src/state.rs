//! Process-wide application state.

use everyday_core::Vault;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::error::{CommandError, CommandResult};

/// Holds the open vault, if any.
///
/// `None` means no vault has been opened this session. A vault that is open
/// but *locked* is still `Some`: the vault's own lock state governs access,
/// and keeping the handle lets the lock screen name the vault it is guarding.
#[derive(Default)]
pub struct AppState {
    vault: RwLock<Option<Arc<Vault>>>,
    last_path: RwLock<Option<PathBuf>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, vault: Vault) -> Arc<Vault> {
        *self.last_path.write().unwrap() = Some(vault.path().to_path_buf());
        let vault = Arc::new(vault);
        *self.vault.write().unwrap() = Some(vault.clone());
        vault
    }

    pub fn get(&self) -> Option<Arc<Vault>> {
        self.vault.read().unwrap().clone()
    }

    /// The open vault, or a `no_vault` error the UI can route on.
    pub fn require(&self) -> CommandResult<Arc<Vault>> {
        self.get().ok_or_else(|| CommandError::new("no_vault", "no vault is open"))
    }

    pub fn last_path(&self) -> Option<PathBuf> {
        self.last_path.read().unwrap().clone()
    }

    pub fn remember(&self, path: &Path) {
        *self.last_path.write().unwrap() = Some(path.to_path_buf());
    }
}
