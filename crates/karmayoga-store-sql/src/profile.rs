//! The one row that says whose vault this is.
//!
//! A singleton pinned by a `CHECK (id = 1)`, like `agent_settings` beside it,
//! and the one table in this crate with *nothing* in the clear. There is no
//! index to build over a single row, and what is in it -- a name, a birthday,
//! where somebody lives, a paragraph about their family -- is the most
//! identifying thing in the vault. It goes in the envelope whole.

use karmayoga_core::error::Result;
use karmayoga_core::profile::{Profile, profile_aad};

use crate::SqlStore;

impl SqlStore {
    pub(crate) fn read_profile(&self) -> Result<Profile> {
        // Nothing written yet is not an error. It is the answer.
        Ok(self.get_singleton("profile", &profile_aad())?.unwrap_or_default())
    }

    pub(crate) fn write_profile(&self, profile: &Profile) -> Result<()> {
        self.put_singleton("profile", &profile_aad(), profile)
    }
}
