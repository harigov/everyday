//! Process-wide application state.

use everyday_core::{CalendarId, Vault};
use std::collections::HashSet;
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
    /// Set once the interface has been told to save and close, so the second
    /// `CloseRequested` -- the one we ask for ourselves -- is let through.
    closing: std::sync::atomic::AtomicBool,
    /// Subscriptions whose background refresh is failing and which the user
    /// has already been told about.
    ///
    /// The background pass runs every few minutes for as long as the app is
    /// open, so a feed that has been revoked fails again, and again, and
    /// again. Notifying each time would turn one fact -- this calendar has
    /// stopped answering -- into a notification every five minutes until
    /// somebody muted the application. Held here rather than on the
    /// subscription because it is a fact about *this session's* telling, not
    /// about the calendar: the vault already records the failure itself, and
    /// reopening the app is a reasonable moment to be told again.
    reported_feeds: RwLock<HashSet<CalendarId>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the right to run the save-before-close handshake.
    ///
    /// True the first time and false afterwards, so the close that follows a
    /// completed flush is not intercepted a second time and turned into a
    /// window that will not shut.
    pub fn begin_closing(&self) -> bool {
        !self.closing.swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    /// Note that `id`'s refresh has failed. True the first time, so the
    /// caller says something once per outage rather than once per attempt.
    pub fn feed_failed(&self, id: CalendarId) -> bool {
        self.reported_feeds.write().unwrap().insert(id)
    }

    /// Note that `id`'s refresh worked, so the next outage is news again.
    pub fn feed_recovered(&self, id: CalendarId) {
        self.reported_feeds.write().unwrap().remove(&id);
    }

    pub fn set(&self, vault: Vault) -> Arc<Vault> {
        self.remember(vault.path());
        let vault = Arc::new(vault);
        *self.vault.write().unwrap() = Some(vault.clone());
        vault
    }

    /// Close the open vault, releasing its write lock.
    ///
    /// Must happen *before* another vault is opened, and matters even when
    /// the other vault is the same one. The lock is an OS lock on an open
    /// file description, so a second `open` of a path this process already
    /// holds conflicts with itself: without this, choosing the currently-open
    /// vault from the picker would quietly reopen it read-only.
    ///
    /// Only this handle is dropped. A command already running still holds its
    /// own `Arc`, and the lock goes when that finishes -- which is why the
    /// open that follows must tolerate losing the race and coming up
    /// read-only rather than failing.
    pub fn close(&self) {
        self.reported_feeds.write().unwrap().clear();
        let previous = self.vault.write().unwrap().take();
        if let Some(vault) = &previous {
            // Drop the key and the decrypted index now rather than whenever
            // the last `Arc` happens to go.
            vault.lock();
        }
        drop(previous);
    }

    pub fn get(&self) -> Option<Arc<Vault>> {
        self.vault.read().unwrap().clone()
    }

    /// The open vault, or a `no_vault` error the UI can route on.
    pub fn require(&self) -> CommandResult<Arc<Vault>> {
        self.get().ok_or_else(|| CommandError::new("no_vault", "no vault is open"))
    }

    /// The vault to open on startup: the one this session already touched,
    /// or the one the previous session left behind.
    pub fn last_path(&self) -> Option<PathBuf> {
        let in_memory = self.last_path.read().unwrap().clone();
        in_memory.or_else(everyday_vault::last_vault)
    }

    /// Record `path` as the vault to reopen, in memory and on disk.
    ///
    /// Failing to write the pointer is not worth failing the open that
    /// prompted it: the vault is fine, the next launch just starts at the
    /// default location.
    pub fn remember(&self, path: &Path) {
        *self.last_path.write().unwrap() = Some(path.to_path_buf());
        if let Err(e) = everyday_vault::remember_vault(path) {
            tracing::warn!(error = %e, "could not record the last vault path");
        }
    }
}
