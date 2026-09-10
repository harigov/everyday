//! The one row that says whose vault this is.
//!
//! A singleton pinned by a `CHECK (id = 1)`, like `agent_settings` beside it,
//! and the one table in this crate with *nothing* in the clear. There is no
//! index to build over a single row, and what is in it -- a name, a birthday,
//! where somebody lives, a paragraph about their family -- is the most
//! identifying thing in the vault. It goes in the envelope whole.

use everyday_core::error::Result;
use everyday_core::profile::{Profile, profile_aad};

use crate::conn::SqlExt;
use crate::{SqlStore, vals};

impl SqlStore {
    pub(crate) fn read_profile(&self) -> Result<Profile> {
        match self.read().sealed("SELECT data FROM profile WHERE id = 1", &[])? {
            Some(sealed) => self.unseal(&profile_aad(), &sealed),
            // Nothing written yet is not an error. It is the answer.
            None => Ok(Profile::default()),
        }
    }

    pub(crate) fn write_profile(&self, profile: &Profile) -> Result<()> {
        let data = self.seal(&profile_aad(), profile)?;
        self.write().execute(
            "INSERT INTO profile (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }
}
