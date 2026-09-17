//! Accounts: the vault-level record of a mailbox provider signed in to.
//!
//! On exactly the terms every other domain in this module is: an optional
//! store, reached through [`Vault::with_domain`], refusing to mutate when the
//! vault holds no write claim. What is different from `purpose.rs` beside it
//! is the secret -- an account's credential is not part of the record at
//! all, and reaches storage through [`crate::store::secrets::SecretStore`]
//! instead, under its own [`Domain::Secrets`] lookup. See
//! [`crate::account`] for why the two are kept apart.

use super::Vault;
use super::session::Domain;
use crate::account::{ACCOUNT_SECRET_OWNER_KIND, Account, AccountSecret};
use crate::error::{Error, Result};
use crate::id::AccountId;
use crate::record::RecordKind;
use crate::store::accounts::AccountStore;
use crate::store::secrets::SecretStore;

impl Vault {
    /// Does this vault's backend store accounts?
    pub fn supports_accounts(&self) -> bool {
        self.with_accounts(|_| Ok(())).is_ok()
    }

    fn with_accounts<T>(&self, f: impl FnOnce(&dyn AccountStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Accounts, |s| s.accounts().map(f))
    }

    fn with_secrets<T>(&self, f: impl FnOnce(&dyn SecretStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Secrets, |s| s.secrets().map(f))
    }

    /// Every account, in the order they were added.
    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.with_accounts(|a| a.list_accounts())
    }

    pub fn account(&self, id: AccountId) -> Result<Account> {
        self.with_accounts(|a| a.get_account(id))
    }

    pub fn save_account(&self, account: &Account) -> Result<()> {
        self.writable()?;
        if account.address.trim().is_empty() {
            return Err(Error::Invalid("an account needs an address".into()));
        }
        self.with_accounts(|a| a.put_account(account))?;
        self.wrote(RecordKind::Account, account.id);
        Ok(())
    }

    /// Delete the account. Its secret and every calendar (and event) it
    /// brought into the vault go with it -- see
    /// [`crate::store::accounts::AccountStore::delete_account`]. Only the
    /// account itself is recorded as touched; the calendars it cascades away
    /// have no ids in hand here to name individually.
    pub fn delete_account(&self, id: AccountId) -> Result<()> {
        self.writable()?;
        self.with_accounts(|a| a.delete_account(id))?;
        self.wrote(RecordKind::Account, id);
        Ok(())
    }

    /// The credential this account signs in with, or `None` if none has ever
    /// been saved.
    ///
    /// Returns the whole thing, secret values included -- this is the one
    /// place in the application that is allowed to. Nothing above the vault
    /// calls this except the code that actually opens a connection; every
    /// service command reads the boolean view instead. See
    /// `everyday_service::domains::accounts`.
    pub fn account_secret(&self, id: AccountId) -> Result<Option<AccountSecret>> {
        self.with_secrets(|s| {
            let bytes = s.get_secret(ACCOUNT_SECRET_OWNER_KIND, &id.to_string())?;
            bytes
                .map(|b| {
                    serde_json::from_slice(&b).map_err(|e| {
                        Error::Invalid(format!("a stored account secret would not parse: {e}"))
                    })
                })
                .transpose()
        })
    }

    /// Replace whatever secret this account has stored.
    ///
    /// Whole-record rather than field-by-field, on the same reasoning
    /// `save_role` and every other `save_*` here takes: the caller read the
    /// current value first (or is deliberately starting fresh, as sign-in
    /// does), and a partial update would invite two writers to clobber each
    /// other's half of it.
    pub fn save_account_secret(&self, id: AccountId, secret: &AccountSecret) -> Result<()> {
        self.writable()?;
        let bytes = serde_json::to_vec(secret)
            .map_err(|e| Error::Invalid(format!("could not encode an account secret: {e}")))?;
        self.with_secrets(|s| s.put_secret(ACCOUNT_SECRET_OWNER_KIND, &id.to_string(), &bytes))
    }
}
