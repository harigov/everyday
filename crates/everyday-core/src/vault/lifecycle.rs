//! Unlock, lock, and the handful of settings that only make sense while a
//! vault is thought of as *open* or *closed*: the password, the backend
//! settings sealed under its key, and the two idle timers.
//!
//! See the module doc at the top of [`super`] for what locking actually
//! does and why the header's `key_check` exists.

use super::header::{
    cipher_for, from_hex, key_check_passes, open_settings, seal_key_check, seal_settings, to_hex,
    write_header,
};
use super::session::Unlocked;
use super::{STORE_DIRNAME, Vault};
use crate::crypto::{
    AeadCipher, Cipher, NullCipher, SUITE_XCHACHA20_POLY1305, SecretKey, derive_key, random_salt,
    unwrap_key, wrap_key,
};
use crate::error::{Error, Result};
use crate::search::SearchIndex;
use crate::store::{BackendSettings, StoreContext};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Vault {
    // ---- locking --------------------------------------------------------

    pub fn is_unlocked(&self) -> bool {
        self.state_read().is_some()
    }

    /// Unlock with a password. Pass `None` for an unencrypted vault.
    ///
    /// Returns [`Error::BadPassword`] on the wrong password — the AEAD tag on
    /// the wrapped key is what detects this, so there is no separate
    /// password verifier to leak.
    pub fn unlock(&self, password: Option<&str>) -> Result<()> {
        if self.is_unlocked() {
            return Ok(());
        }
        let dek = self.data_key(password)?;
        // Give a vault written before `key_check` existed one, now that we
        // are holding the key it describes. Best effort: a read-only session
        // simply carries on, and the only thing it costs is that this vault
        // cannot be told to open itself until somebody unlocks it writably.
        if let Some(key) = &dek
            && self.header_read().key_check.is_none()
            && self.writable().is_ok()
            && let Ok(check) = seal_key_check(key)
        {
            let mut header = self.header_write();
            header.key_check = Some(check);
            let _ = write_header(&self.root, &header);
        }
        self.activate(dek)
    }

    /// Check a password without opening or closing anything.
    ///
    /// This is what a *screen* lock asks. A client that has hidden what it
    /// was showing needs to know the person is who they were, and the vault
    /// behind it has stayed open the whole time -- for the other windows
    /// looking at it, and for the assistant. So this derives the key,
    /// compares the AEAD tag on the wrapped one, and throws the result away.
    ///
    /// It costs a full Argon2 derivation, which is the point: a screen
    /// unlock is exactly as expensive to guess at as a vault unlock, and the
    /// server puts both behind the same rate limit.
    pub fn verify_password(&self, password: Option<&str>) -> Result<()> {
        self.data_key(password).map(|_| ())
    }

    /// The vault's data key, hex-encoded, for a caller that means to keep it.
    ///
    /// Deliberately awkward to reach for, and named to say what it is. There
    /// is exactly one caller: the switch that lets a vault open itself when
    /// the machine starts, which puts this in the operating system's own
    /// keychain so that a routine set for seven in the morning happens on a
    /// laptop that rebooted overnight.
    ///
    /// That is a real trade and the interface states it where the switch is:
    /// a vault whose key is in the keychain is as safe as the login on that
    /// computer, rather than as safe as its password. Nothing else in this
    /// application ever asks for this, and nothing should: the key is not a
    /// value to pass around, it is the thing the whole envelope protects.
    pub fn export_data_key(&self, password: Option<&str>) -> Result<crate::crypto::KeyText> {
        match self.data_key(password)? {
            Some(key) => Ok(crate::crypto::KeyText::new(to_hex(key.expose()))),
            // A vault with no password has no key to keep, and opens by
            // itself already. Saying so is better than handing back an empty
            // string that would look like a key.
            None => Err(Error::Invalid(
                "this vault is not encrypted, so it already opens without a password".into(),
            )),
        }
    }

    /// Open the vault with a key rather than a password.
    ///
    /// The other half of [`Vault::export_data_key`]. A wrong key fails the
    /// same way a wrong password does -- the AEAD tag on the first record it
    /// reads -- so there is nothing here that a bad value can get past.
    pub fn unlock_with_key(&self, hex: &str) -> Result<()> {
        if self.is_unlocked() {
            return Ok(());
        }
        // Decoded into a buffer that is wiped whatever happens next, including
        // the failure paths: a value of the wrong length is still somebody's
        // key with a typo on the end, and `SecretKey`'s own zeroization does
        // not reach the `Vec` it was copied out of.
        let mut decoded = from_hex(hex)?;
        let taken = <[u8; crate::crypto::KEY_LEN]>::try_from(decoded.as_slice());
        zeroize::Zeroize::zeroize(&mut decoded);
        let bytes = taken.map_err(|_| Error::Invalid("that is not a key for this vault".into()))?;
        let key = SecretKey::from_bytes(bytes);

        // Checked *before* anything is opened, and refused when there is
        // nothing to check against. See `VaultHeader::key_check`: a wrong key
        // on an empty vault would otherwise open it and seal everything
        // written afterwards under a key the header does not hold.
        let check = self.header_read().key_check.clone().ok_or_else(|| {
            Error::Invalid(
                "this vault has nothing to check a key against; unlock it with its password \
                 once and it will"
                    .into(),
            )
        })?;
        if !key_check_passes(&key, &check) {
            return Err(Error::BadPassword);
        }
        self.activate(Some(key))
    }

    /// Unwrap the data key with `password`, without opening anything.
    ///
    /// Separate from [`Vault::unlock`] because unlocking is two steps that
    /// fail for unrelated reasons -- the password is wrong, or the backend
    /// cannot be reached -- and the repair path needs the first without the
    /// second. `None` for an unencrypted vault, which has no key.
    fn data_key(&self, password: Option<&str>) -> Result<Option<SecretKey>> {
        let header = self.header_read().clone();
        if !header.is_encrypted() {
            return Ok(None);
        }
        let password = password.ok_or(Error::BadPassword)?;
        let salt_hex = header
            .salt
            .as_deref()
            .ok_or_else(|| Error::Invalid("encrypted vault header is missing its salt".into()))?;
        let wrapped_hex = header.wrapped_key.as_deref().ok_or_else(|| {
            Error::Invalid("encrypted vault header is missing its wrapped key".into())
        })?;
        let kdf = header.kdf.ok_or_else(|| {
            Error::Invalid("encrypted vault header is missing its KDF parameters".into())
        })?;
        let kek = derive_key(password, &from_hex(salt_hex)?, kdf)?;
        Ok(Some(unwrap_key(&kek, &from_hex(wrapped_hex)?)?))
    }

    /// Open the backend and build the search index. Assumes the key is right.
    pub(super) fn activate(&self, dek: Option<SecretKey>) -> Result<()> {
        let header = self.header_read().clone();
        // The cipher owns the only copy of the key material from here on;
        // `dek` is dropped (and zeroized) at the end of this function.
        let cipher: Arc<dyn Cipher> = match &dek {
            Some(k) => Arc::new(AeadCipher::new(k)),
            None => Arc::new(NullCipher),
        };

        let store_root = self.root.join(STORE_DIRNAME);
        std::fs::create_dir_all(&store_root).map_err(|e| Error::io(&store_root, e))?;
        let settings = open_settings(cipher.as_ref(), header.backend_settings.as_deref())?;
        let store = self.registry.open(
            &header.backend,
            StoreContext { root: store_root, cipher: cipher.clone(), settings },
        )?;

        // Check the store before trusting it, but do not refuse to open on a
        // bad answer. A damaged vault is precisely the one someone needs to
        // get into -- to export what still reads, or to see how much of it
        // survived -- and locking them out would turn recoverable damage
        // into total loss. Say so loudly instead; `everyday check` reports
        // the same findings on demand.
        match store.check_integrity() {
            Ok(problems) if !problems.is_empty() => {
                tracing::error!(
                    count = problems.len(),
                    first = %problems[0],
                    "storage integrity check failed -- restore from a backup"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not run the integrity check"),
        }

        let notes = match store.notes() {
            Some(n) => n.all_notes()?,
            None => Vec::new(),
        };
        let index = SearchIndex::build(&store.all_entries()?, &notes);

        *self.state_write() = Some(Unlocked { store, index, cipher });
        self.touch();
        Ok(())
    }

    /// Lock the vault: close the backend, drop the index, zeroize the key.
    pub fn lock(&self) {
        // Take the state out and drop it outside the lock so that a slow
        // backend shutdown does not hold every reader.
        let previous = self.state_write().take();
        drop(previous); // SecretKey zeroizes here; SearchIndex frees plaintext
    }

    /// Change the password, or add/remove encryption entirely.
    ///
    /// Only the *wrapped data key* is rewritten, so this is instant even for
    /// a vault with tens of thousands of entries.
    pub fn change_password(&self, current: Option<&str>, new: Option<&str>) -> Result<()> {
        self.writable()?;
        let header = self.header_read().clone();

        // Verify the current password by unwrapping, whether or not the
        // vault happens to be unlocked already.
        let dek = if header.is_encrypted() {
            let current = current.ok_or(Error::BadPassword)?;
            let kdf = header.kdf.ok_or_else(|| Error::Invalid("missing KDF params".into()))?;
            let salt = from_hex(header.salt.as_deref().unwrap_or_default())?;
            let kek = derive_key(current, &salt, kdf)?;
            unwrap_key(&kek, &from_hex(header.wrapped_key.as_deref().unwrap_or_default())?)?
        } else {
            SecretKey::random()
        };

        let mut next = header.clone();
        match new {
            Some(password) if !password.is_empty() => {
                let kdf = header.kdf.unwrap_or_default();
                let salt = random_salt();
                let kek = derive_key(password, &salt, kdf)?;
                next.cipher = SUITE_XCHACHA20_POLY1305.into();
                next.kdf = Some(kdf);
                next.salt = Some(to_hex(&salt));
                next.wrapped_key = Some(to_hex(&wrap_key(&kek, &dek)?));
                // The data key survives a password change on an encrypted
                // vault, so the existing check value stays true. Going from
                // *unencrypted* to encrypted mints a fresh key above, and
                // that one needs a check value of its own.
                if !header.is_encrypted() || next.key_check.is_none() {
                    next.key_check = Some(seal_key_check(&dek)?);
                }
            }
            _ => {
                // Removing the password would leave the on-disk records
                // sealed under a key nobody holds. Rewriting the whole vault
                // is a different, much heavier operation than this one.
                return Err(Error::Unsupported(
                    "removing a vault password (re-encrypting the whole vault)",
                ));
            }
        }

        write_header(&self.root, &next)?;
        *self.header_write() = next;

        // Same data key, so an unlocked session stays valid.
        Ok(())
    }

    /// The cipher that seals this vault's records, from whichever source is
    /// available: the live one if it is unlocked, `password` if it is not.
    ///
    /// This is what lets the two methods below work on a vault opened with
    /// [`Vault::open_dormant`] -- the case they exist for, since a vault
    /// whose backend cannot be reached is exactly the one whose backend
    /// settings need changing.
    fn settings_cipher(&self, password: Option<&str>) -> Result<Arc<dyn Cipher>> {
        if let Some(unlocked) = self.state_read().as_ref() {
            return Ok(unlocked.cipher.clone());
        }
        Ok(cipher_for(self.data_key(password)?.as_ref()))
    }

    /// What this vault's backend was configured with.
    ///
    /// `password` is used only when the vault is locked; an unlocked one
    /// already holds the key and ignores it. Callers that mean to *show*
    /// these must remember what tends to be in them: a connection URL is a
    /// credential, and this is the one part of a header that is sealed
    /// precisely because it is one. See
    /// [`VaultHeader::backend_settings`].
    pub fn backend_settings(&self, password: Option<&str>) -> Result<BackendSettings> {
        let sealed = self.header_read().backend_settings.clone();
        open_settings(self.settings_cipher(password)?.as_ref(), sealed.as_deref())
    }

    /// Reconfigure the backend: a moved database, a rotated password.
    ///
    /// Takes effect the next time the store is opened rather than now.
    /// Swapping the connection under a live store would leave the search
    /// index -- and everything the interface has already loaded -- describing
    /// a database this vault is no longer talking to, so the honest thing is
    /// to write the header and let the caller lock and unlock.
    pub fn set_backend_settings(
        &self,
        password: Option<&str>,
        settings: BackendSettings,
    ) -> Result<()> {
        self.writable()?;
        let sealed = seal_settings(&self.settings_cipher(password)?, &settings)?;
        let mut header = self.header_write();
        header.backend_settings = sealed;
        write_header(&self.root, &header)
    }

    /// Set how long before this window hides what it is showing. 0 means
    /// never; the vault stays open either way.
    pub fn set_auto_lock(&self, seconds: u64) -> Result<()> {
        self.writable()?;
        let mut header = self.header_write();
        header.auto_lock_seconds = seconds;
        // Setting either half of the split is a choice about both, so the
        // migration in `open_inner` must not run afterwards. Without this, a
        // vault whose `auto_lock_seconds` was 0 -- so the migration's `> 0`
        // guard never fired and the flag was never stamped -- would have the
        // *screen* timeout chosen here copied into `forget_key_seconds` on the
        // next open. The machine would then start dropping the key on a timer
        // nobody asked for: the seven o'clock routine finds a locked vault,
        // and anything being served to a phone is shut out.
        header.migrated_lock = Some(true);
        write_header(&self.root, &header)
    }

    /// Set how long an idle vault keeps its key. 0 means until the process
    /// ends or somebody locks it.
    pub fn set_forget_key(&self, seconds: u64) -> Result<()> {
        self.writable()?;
        let mut header = self.header_write();
        header.forget_key_seconds = seconds;
        // Whatever was chosen here is a choice, including zero. Marked so the
        // migration in `open_inner` does not overrule it on the next open.
        header.migrated_lock = Some(true);
        write_header(&self.root, &header)
    }

    /// Record activity, deferring [`Vault::forget_key_if_idle`].
    ///
    /// Deliberately *not* called from [`Vault::read`] and [`Vault::write`],
    /// which is where it used to live. The assistant's scheduler reads and
    /// writes this vault every minute of every day; if that counted as
    /// activity, a vault with one routine on it would never let go of its
    /// key no matter what the timeout said. The service calls this after a
    /// command from a person -- a window, a phone, a script -- and not after
    /// one from the assistant.
    pub fn touch(&self) {
        let ms = self.epoch.elapsed().as_millis() as u64;
        self.last_activity_ms.store(ms, Ordering::Relaxed);
    }

    /// Seconds until the key is dropped for idleness, or `None` if that is
    /// disabled or the vault is already locked.
    pub fn seconds_until_forget_key(&self) -> Option<u64> {
        let timeout = self.header_read().forget_key_seconds;
        if timeout == 0 || !self.is_unlocked() {
            return None;
        }
        // Saturating, because the two loads are a moment apart and the
        // scheduler now polls this from its own thread while a request thread
        // is calling `touch`. Read `elapsed` first and `touch` lands after it,
        // and the plain subtraction underflows: a debug build panics, a
        // release build wraps to something enormous, `saturating_sub` then
        // answers `Some(0)`, and the vault locks in the instant somebody used
        // it.
        let idle_ms = (self.epoch.elapsed().as_millis() as u64)
            .saturating_sub(self.last_activity_ms.load(Ordering::Relaxed));
        Some(timeout.saturating_sub(idle_ms / 1000))
    }

    /// Lock the vault if nobody has used it for `forget_key_seconds`.
    /// Polled by a window and by the assistant's scheduler, so a machine
    /// serving a vault with no window attached still honours it. Returns
    /// whether it locked.
    pub fn forget_key_if_idle(&self) -> bool {
        if self.seconds_until_forget_key() == Some(0) {
            self.lock();
            return true;
        }
        false
    }
}
