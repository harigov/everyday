//! The vault's owner.
//!
//! Small on purpose: a handful of facts that do not change, typed in
//! Settings once. Facts that *do* change are [`Memory`](crate::agent::Memory)
//! entries instead. See [`crate::profile`].

use super::Vault;
use crate::error::Result;
use crate::profile::Profile;
use jiff::Timestamp;

impl Vault {
    /// Who this vault belongs to. Never fails for want of a profile: an
    /// unfilled one is the answer.
    pub fn profile(&self) -> Result<Profile> {
        self.read(|u| u.store.profile())
    }

    /// Write the profile.
    ///
    /// Facts that change are what [`Memory`](crate::agent::Memory) is for;
    /// this is the handful that do not, typed in Settings, and changed by the
    /// assistant's `update_profile` only when the person says so in chat. See
    /// [`crate::profile`].
    pub fn save_profile(&self, profile: &Profile) -> Result<()> {
        self.writable()?;
        profile.validate()?;
        let mut stamped = profile.clone();
        stamped.updated_at = Some(Timestamp::now());
        self.write(|u| u.store.put_profile(&stamped))
    }
}
