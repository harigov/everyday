//! The command surface exposed to the interface.
//!
//! Every command runs its storage work on the blocking pool rather than on an
//! async worker. Vault operations touch disk and, when unlocking, deliberately
//! burn ~64 MiB of memory in Argon2; doing that on the runtime's async threads
//! would stall every other task, and doing it on the webview's thread would
//! freeze the window mid-keystroke.

use everyday_core::model::local_date_in;
use everyday_core::store::{EntryQuery, StoreStats};
use everyday_core::{
    BlobId, Entry, EntryId, Journal, JournalId, Vault, VaultConfig, VaultStatus,
};
use everyday_core::search::SearchHit;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

/// Run blocking vault work off the async runtime.
async fn blocking<T, F>(f: F) -> CommandResult<T>
where
    F: FnOnce() -> CommandResult<T> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("panic", format!("background task failed: {e}")))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    pub id: String,
    pub description: String,
}

/// What the interface needs before any vault is open.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub vault_exists: bool,
    /// The vault location in play: the one last opened if it is still there,
    /// otherwise where a new vault would be created.
    pub default_path: PathBuf,
    pub backends: Vec<BackendInfo>,
    pub status: Option<VaultStatus>,
}

#[tauri::command]
pub async fn bootstrap(state: State<'_, AppState>) -> CommandResult<Bootstrap> {
    // Prefer the vault this user last had open. Only fall back to the default
    // location when nothing was recorded or what was recorded is gone -- an
    // external disk that is not plugged in should show the setup screen, not
    // an error about a path the user cannot see.
    let path = state
        .last_path()
        .filter(|p| everyday_vault::exists(p))
        .unwrap_or_else(everyday_vault::default_vault_dir);
    let backends = everyday_vault::available_backends()
        .into_iter()
        .map(|(id, description)| BackendInfo {
            id: id.to_string(),
            description: description.to_string(),
        })
        .collect();

    // Open eagerly so an unencrypted vault is usable immediately and an
    // encrypted one can name itself on the lock screen.
    if state.get().is_none() && everyday_vault::exists(&path) {
        let opened = {
            let path = path.clone();
            blocking(move || everyday_vault::open(&path).map_err(CommandError::from)).await
        };
        match opened {
            Ok(vault) => {
                state.set(vault);
            }
            // A vault we cannot open is not fatal: the interface should still
            // start and be able to say why.
            Err(e) => tracing::warn!(error = %e, "could not open the vault at startup"),
        }
    }

    Ok(Bootstrap {
        vault_exists: everyday_vault::exists(&path),
        default_path: path,
        backends,
        status: state.get().map(|v| v.status()),
    })
}

#[tauri::command]
pub async fn create_vault(
    state: State<'_, AppState>,
    path: PathBuf,
    name: String,
    backend: String,
    password: Option<String>,
) -> CommandResult<VaultStatus> {
    if let Some(p) = password.as_deref() {
        everyday_vault::validate_password(p)?;
    }
    let config = VaultConfig {
        name,
        backend,
        password,
        kdf: Default::default(),
        auto_lock_seconds: 15 * 60,
    };
    let created = {
        let path = path.clone();
        blocking(move || {
            let vault = everyday_vault::create(&path, config)?;
            // A vault with no journal is a dead end; give it one.
            vault.save_journal(&Journal::new("Journal"))?;
            Ok(vault)
        })
        .await?
    };
    let vault = state.set(created);
    Ok(vault.status())
}

#[tauri::command]
pub async fn open_vault(state: State<'_, AppState>, path: PathBuf) -> CommandResult<VaultStatus> {
    let opened = {
        let path = path.clone();
        blocking(move || everyday_vault::open(&path).map_err(CommandError::from)).await?
    };
    let vault = state.set(opened);
    Ok(vault.status())
}

#[tauri::command]
pub async fn unlock(state: State<'_, AppState>, password: String) -> CommandResult<VaultStatus> {
    let vault = state.require()?;
    let v = vault.clone();
    blocking(move || v.unlock(Some(&password)).map_err(CommandError::from)).await?;
    Ok(vault.status())
}

#[tauri::command]
pub fn lock(state: State<'_, AppState>) -> CommandResult<VaultStatus> {
    let vault = state.require()?;
    vault.lock();
    Ok(vault.status())
}

#[tauri::command]
pub fn status(state: State<'_, AppState>) -> CommandResult<VaultStatus> {
    Ok(state.require()?.status())
}

#[tauri::command]
pub async fn change_password(
    state: State<'_, AppState>,
    current: String,
    next: String,
) -> CommandResult<()> {
    everyday_vault::validate_password(&next)?;
    let vault = state.require()?;
    blocking(move || {
        vault.change_password(Some(&current), Some(&next)).map_err(CommandError::from)
    })
    .await
}

#[tauri::command]
pub fn set_auto_lock(state: State<'_, AppState>, seconds: u64) -> CommandResult<()> {
    Ok(state.require()?.set_auto_lock(seconds)?)
}

/// Defer the idle auto-lock. Called on real interaction, so it must be cheap.
#[tauri::command]
pub fn touch(state: State<'_, AppState>) -> CommandResult<()> {
    if let Some(v) = state.get() {
        v.touch();
    }
    Ok(())
}

/// Returns true if the vault just locked itself. Polled by the interface.
#[tauri::command]
pub fn poll_auto_lock(state: State<'_, AppState>) -> CommandResult<bool> {
    Ok(state.get().is_some_and(|v| v.auto_lock_if_idle()))
}

// ---- journals -----------------------------------------------------------

#[tauri::command]
pub fn list_journals(state: State<'_, AppState>) -> CommandResult<Vec<Journal>> {
    Ok(state.require()?.journals()?)
}

#[tauri::command]
pub fn save_journal(state: State<'_, AppState>, journal: Journal) -> CommandResult<()> {
    Ok(state.require()?.save_journal(&journal)?)
}

#[tauri::command]
pub fn delete_journal(state: State<'_, AppState>, id: JournalId) -> CommandResult<()> {
    Ok(state.require()?.delete_journal(id)?)
}

// ---- entries ------------------------------------------------------------

#[tauri::command]
pub fn list_entries(
    state: State<'_, AppState>,
    query: EntryQuery,
) -> CommandResult<Vec<everyday_core::EntrySummary>> {
    Ok(state.require()?.entries(&query)?)
}

#[tauri::command]
pub fn get_entry(state: State<'_, AppState>, id: EntryId) -> CommandResult<Entry> {
    Ok(state.require()?.entry(id)?)
}

/// A blank entry, filed under today in the machine's own time zone.
#[tauri::command]
pub fn new_entry(state: State<'_, AppState>, journal_id: JournalId) -> CommandResult<Entry> {
    let _ = state.require()?;
    let tz = jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string();
    let mut entry = Entry::new(journal_id, &tz);
    entry.local_date = local_date_in(entry.created_at, &tz);
    Ok(entry)
}

#[tauri::command]
pub fn save_entry(state: State<'_, AppState>, entry: Entry) -> CommandResult<()> {
    Ok(state.require()?.save_entry(&entry)?)
}

#[tauri::command]
pub fn delete_entry(state: State<'_, AppState>, id: EntryId) -> CommandResult<()> {
    Ok(state.require()?.delete_entry(id)?)
}

// ---- search -------------------------------------------------------------

#[tauri::command]
pub fn search(
    state: State<'_, AppState>,
    query: String,
    journal_id: Option<JournalId>,
    limit: usize,
) -> CommandResult<Vec<SearchHit>> {
    Ok(state.require()?.search(&query, journal_id, limit.min(200))?)
}

#[tauri::command]
pub fn list_tags(state: State<'_, AppState>) -> CommandResult<Vec<String>> {
    let vault = state.require()?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for e in vault.entries(&EntryQuery::default())? {
        for t in e.tags {
            *counts.entry(t).or_default() += 1;
        }
    }
    let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(tags.into_iter().map(|(t, _)| t).collect())
}

// ---- media --------------------------------------------------------------

/// Largest file accepted as an attachment.
///
/// Not a storage limit -- the blob store chunks and streams happily past this
/// -- but a bound on the single allocation this command makes, since the
/// bytes arrive as one JSON array from the webview.
const MAX_ATTACHMENT_BYTES: usize = 512 * 1024 * 1024;

#[tauri::command]
pub async fn put_blob(state: State<'_, AppState>, bytes: Vec<u8>) -> CommandResult<String> {
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(CommandError::new(
            "too_large",
            format!(
                "attachment is {} MB; the limit is {} MB",
                bytes.len() / 1_048_576,
                MAX_ATTACHMENT_BYTES / 1_048_576
            ),
        ));
    }
    let vault = state.require()?;
    blocking(move || Ok(vault.put_blob(&bytes)?.to_hex())).await
}

// ---- maintenance --------------------------------------------------------

#[tauri::command]
pub async fn collect_garbage(state: State<'_, AppState>) -> CommandResult<u64> {
    let vault = state.require()?;
    blocking(move || Ok(vault.collect_garbage()?)).await
}

#[tauri::command]
pub fn vault_stats(state: State<'_, AppState>) -> CommandResult<StoreStats> {
    Ok(state.require()?.stats()?)
}

/// Read a blob for the media protocol handler.
pub fn read_blob_range(
    vault: &Arc<Vault>,
    id: BlobId,
    offset: u64,
    len: u64,
) -> everyday_core::Result<Vec<u8>> {
    vault.with_store(|s| s.get_blob_range(id, offset, len))
}

pub fn blob_len(vault: &Arc<Vault>, id: BlobId) -> everyday_core::Result<u64> {
    vault.with_store(|s| s.blob_len(id))
}
