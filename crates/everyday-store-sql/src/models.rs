//! Which model each size of work runs on, and where -- see
//! `everyday_core::llm`.
//!
//! A singleton, as `profile` beside it is, with nothing in the clear: there
//! is no index to build over one row, and an endpoint can be a company's
//! private gateway. No key is in it; those are per endpoint, in
//! `record_secrets`.

use everyday_core::error::Result;
use everyday_core::llm::{ModelSettings, model_settings_aad};

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

impl SqlStore {
    pub(crate) fn read_model_settings(&self) -> Result<Option<ModelSettings>> {
        match self.read().sealed("SELECT data FROM model_settings WHERE id = 1", &[])? {
            Some(sealed) => Ok(Some(self.unseal(&model_settings_aad(), &sealed)?)),
            // Never saved -- which is not the same answer as the defaults:
            // it is what tells the vault to look for the models where they
            // used to be kept. See `JournalStore::model_settings`.
            None => Ok(None),
        }
    }

    pub(crate) fn write_model_settings(&self, settings: &ModelSettings) -> Result<()> {
        // Not validated here, unlike `put_settings`: the record carried over
        // from the assistant's settings is written as it was, and one that
        // fails a rule added since must still be stored, or the pane that
        // would fix it could never open. `Vault::save_model_settings`
        // validates everything a person saves, and `Vault::model_access`
        // everything a request is made with.
        let data = self.seal(&model_settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO model_settings (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }
}
