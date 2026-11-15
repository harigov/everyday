//! Where this vault's backups go, and what became of the last one -- see
//! `karmayoga_core::backup`.
//!
//! A singleton with two sealed columns and nothing in the clear: the
//! settings, which a person saves, and the status, which a run writes. Kept
//! apart so neither write can carry a stale copy of the other over it.

use karmayoga_core::backup::{BackupSettings, BackupStatus, settings_aad, status_aad};
use karmayoga_core::error::Result;

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

impl SqlStore {
    pub(crate) fn read_backup_settings(&self) -> Result<Option<BackupSettings>> {
        match self.read().sealed(
            "SELECT settings FROM backup_settings WHERE id = 1 AND settings IS NOT NULL",
            &[],
        )? {
            Some(sealed) => Ok(Some(self.unseal(&settings_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn write_backup_settings(&self, settings: &BackupSettings) -> Result<()> {
        let data = self.seal(&settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO backup_settings (id, settings) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET settings = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    pub(crate) fn read_backup_status(&self) -> Result<Option<BackupStatus>> {
        match self
            .read()
            .sealed("SELECT status FROM backup_settings WHERE id = 1 AND status IS NOT NULL", &[])?
        {
            Some(sealed) => Ok(Some(self.unseal(&status_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn write_backup_status(&self, status: &BackupStatus) -> Result<()> {
        let data = self.seal(&status_aad(), status)?;
        self.write().execute(
            "INSERT INTO backup_settings (id, status) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET status = ?1",
            &vals![data],
        )?;
        Ok(())
    }
}
