//! The MCP servers the assistant may use, and what was learned about each --
//! see `karmayoga_core::connection`.
//!
//! A singleton with two sealed columns and nothing in the clear: the list a
//! person saves, and the states the runtime writes. Kept apart so neither
//! write can carry a stale copy of the other over it.

use karmayoga_core::connection::{ConnectionSettings, ConnectionStates, settings_aad, states_aad};
use karmayoga_core::error::Result;

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

impl SqlStore {
    pub(crate) fn read_connection_settings(&self) -> Result<Option<ConnectionSettings>> {
        match self
            .read()
            .sealed("SELECT settings FROM connections WHERE id = 1 AND settings IS NOT NULL", &[])?
        {
            Some(sealed) => Ok(Some(self.unseal(&settings_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn write_connection_settings(&self, settings: &ConnectionSettings) -> Result<()> {
        let data = self.seal(&settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO connections (id, settings) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET settings = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    pub(crate) fn read_connection_states(&self) -> Result<Option<ConnectionStates>> {
        match self
            .read()
            .sealed("SELECT states FROM connections WHERE id = 1 AND states IS NOT NULL", &[])?
        {
            Some(sealed) => Ok(Some(self.unseal(&states_aad(), &sealed)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn write_connection_states(&self, states: &ConnectionStates) -> Result<()> {
        let data = self.seal(&states_aad(), states)?;
        self.write().execute(
            "INSERT INTO connections (id, states) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET states = ?1",
            &vals![data],
        )?;
        Ok(())
    }
}
