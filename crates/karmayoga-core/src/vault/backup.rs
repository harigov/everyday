//! Backups, as the vault sees them: the settings and the last run's outcome,
//! the keys a copy is sealed under, and the files a copy is made of.
//!
//! Making the copy is `karmayoga_service::backup`'s. What only the vault can
//! answer is here: it holds the key, it knows its own layout, and it owns the
//! store that knows how to copy a database that is being written to.

use super::Vault;
use crate::backup::{
    BACKUP_SECRET_OWNER_ID, BACKUP_SECRET_OWNER_KIND, BackupKeys, BackupSettings, BackupStatus,
    S3Keys, StagedFile, list_files,
};
use crate::error::{Error, Result};
use std::path::Path;

/// The folder under the vault directory a backup's database copies are made
/// in: on the same disk as the database, so the copy is a fast local write,
/// and somewhere a backup itself never reads from. Emptied before and after
/// every run.
pub const BACKUP_STAGING_DIRNAME: &str = ".backup-staging";

/// The folder beside the store that holds recordings not yet written up. See
/// `karmayoga_service::meeting::spool`.
const SPOOL_DIRNAME: &str = "spool";

impl Vault {
    /// Where backups go and how often. The defaults -- off, nowhere chosen,
    /// daily, seven copies -- until somebody saves something.
    pub fn backup_settings(&self) -> Result<BackupSettings> {
        self.read(|u| Ok(u.store.backup_settings()?.unwrap_or_default()))
    }

    /// Save Settings → Backup, tidied (see
    /// [`crate::backup::Destination::normalized`]) and checked. Answers with
    /// what was stored.
    pub fn save_backup_settings(&self, settings: &BackupSettings) -> Result<BackupSettings> {
        self.writable()?;
        let mut settings = settings.clone();
        settings.destination = settings.destination.map(|d| d.normalized());
        settings.validate()?;
        self.read(|u| u.store.put_backup_settings(&settings))?;
        Ok(settings)
    }

    /// What became of the last backup.
    pub fn backup_status(&self) -> Result<BackupStatus> {
        self.read(|u| Ok(u.store.backup_status()?.unwrap_or_default()))
    }

    /// Record what became of a backup. Only a run calls this.
    pub fn save_backup_status(&self, status: &BackupStatus) -> Result<()> {
        self.writable()?;
        self.read(|u| u.store.put_backup_status(status))
    }

    /// Store, or with `None` forget, the access key an S3 destination signs
    /// its requests with when it was pasted in rather than read from this
    /// computer. Sealed apart from the settings, as every credential in the
    /// vault is, so it is never handed back out with them.
    pub fn set_backup_s3_keys(&self, keys: Option<&S3Keys>) -> Result<()> {
        self.writable()?;
        match keys {
            Some(keys) => {
                if keys.access_key_id.trim().is_empty() || keys.secret_access_key.trim().is_empty()
                {
                    return Err(Error::Invalid(
                        "both the access key id and the secret are needed".into(),
                    ));
                }
                let trimmed = S3Keys {
                    access_key_id: keys.access_key_id.trim().to_string(),
                    secret_access_key: keys.secret_access_key.trim().to_string(),
                };
                let bytes = serde_json::to_vec(&trimmed)?;
                self.with_secrets(|s| {
                    s.put_secret(BACKUP_SECRET_OWNER_KIND, BACKUP_SECRET_OWNER_ID, &bytes)
                })
            }
            None => self.with_secrets(|s| {
                s.delete_secret(BACKUP_SECRET_OWNER_KIND, BACKUP_SECRET_OWNER_ID)
            }),
        }
    }

    /// The pasted S3 access key, if there is one. For the one layer that
    /// signs a request; never sent to the interface.
    pub fn backup_s3_keys(&self) -> Result<Option<S3Keys>> {
        let bytes =
            self.with_secrets(|s| s.get_secret(BACKUP_SECRET_OWNER_KIND, BACKUP_SECRET_OWNER_ID))?;
        bytes.map(|b| serde_json::from_slice(&b).map_err(Error::from)).transpose()
    }

    pub fn has_backup_s3_keys(&self) -> Result<bool> {
        Ok(self.backup_s3_keys()?.is_some())
    }

    /// The keys this vault's backups are sealed and named under. Needs the
    /// vault unlocked.
    pub fn backup_keys(&self) -> Result<BackupKeys> {
        let identity = crate::backup::vault_identity(&self.header_read());
        self.read(|u| Ok(BackupKeys::from_cipher(u.cipher.as_ref(), &identity)))
    }

    /// The vault's header as it stands, to be kept beside a backup: what
    /// turns the password into the key that opens it. Holds nothing that
    /// opens without the password -- see [`crate::backup`].
    pub fn backup_header(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(&*self.header_read())?)
    }

    /// Whether this vault's storage can be backed up from here: true for a
    /// vault whose database is on this machine, false for one on a server,
    /// which whoever runs the server backs up. Needs the vault unlocked to
    /// say.
    pub fn supports_backup(&self) -> bool {
        self.read(|u| Ok(u.store.backup_job().is_ok())).unwrap_or(false)
    }

    /// Everything a backup of this vault copies, with each database first
    /// copied consistently into `staging` -- an empty directory, which the
    /// caller makes and removes; see [`BACKUP_STAGING_DIRNAME`].
    ///
    /// The store's files, then the meeting spool beside it. The header is
    /// not among them: a backup keeps the latest one apart (see
    /// [`Vault::backup_header`]), and a restore writes it last, so a
    /// restore interrupted halfway is not yet a vault.
    ///
    /// The vault's lock is held only long enough to ask the store for its
    /// job; the copying runs outside it, so saves carry on while a large
    /// database is copied.
    pub fn stage_for_backup(&self, staging: &Path) -> Result<Vec<StagedFile>> {
        let job = self.read(|u| u.store.backup_job())?;
        let mut files = job(staging)?;
        let spool = self.root.join(SPOOL_DIRNAME);
        if spool.is_dir() {
            files.extend(list_files(&spool, SPOOL_DIRNAME, &|_| false)?);
        }
        Ok(files)
    }
}
