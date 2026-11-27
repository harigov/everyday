//! Which model each size of work runs on, and where -- see
//! `jatayu_core::llm`.
//!
//! A singleton, as `profile` beside it is, with nothing in the clear: there
//! is no index to build over one row, and an endpoint can be a company's
//! private gateway. No key is in it; those are per endpoint, in
//! `record_secrets`.

use jatayu_core::error::Result;
use jatayu_core::llm::{ModelSettings, model_settings_aad};

use crate::SqlStore;

impl SqlStore {
    pub(crate) fn read_model_settings(&self) -> Result<Option<ModelSettings>> {
        // Never saved -- which is not the same answer as the defaults: it is
        // what tells the vault to look for the models where they used to be
        // kept. See `JournalStore::model_settings`.
        self.get_singleton("model_settings", &model_settings_aad())
    }

    pub(crate) fn write_model_settings(&self, settings: &ModelSettings) -> Result<()> {
        // Not validated here, unlike `put_settings`: the record carried over
        // from the assistant's settings is written as it was, and one that
        // fails a rule added since must still be stored, or the pane that
        // would fix it could never open. `Vault::save_model_settings`
        // validates everything a person saves, and `Vault::model_access`
        // everything a request is made with.
        self.put_singleton("model_settings", &model_settings_aad(), settings)
    }
}
