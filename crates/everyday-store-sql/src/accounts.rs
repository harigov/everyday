//! Accounts: the mailbox providers this vault has been signed in to.
//!
//! The smallest domain in this crate by table count -- one row shape,
//! `(id, created_us, updated_us, data)`, the same as `routines` -- but its
//! delete is the one method here that reaches outside its own table. See
//! [`everyday_core::store::accounts`] for why: an account's secret and the
//! rows in `account_calendars` pointing at it must not outlive the account
//! itself, and neither lives in the `accounts` table this file otherwise
//! owns.
//!
//! Nothing about an account is worth indexing on yet. There is no list to
//! filter by provider or status at a scale that would make a clear column
//! worth the leak -- a busy vault has a handful of these, not a hundred
//! thousand -- so `created_us` and `updated_us` exist only because every
//! table in this crate carries them, not because a query needs them.

use everyday_core::account::{ACCOUNT_SECRET_OWNER_KIND, Account};
use everyday_core::error::Result;
use everyday_core::id::AccountId;
use everyday_core::store::accounts::{AccountStore, account_aad};

use crate::conn::{SqlExt, ToValue, Value};
use crate::record::Record;
use crate::{SqlStore, to_us, vals};

impl Record for Account {
    const TABLE: &'static str = "accounts";
    const KIND: &'static str = "account";
    type Id = AccountId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        account_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl AccountStore for SqlStore {
    fn list_accounts(&self) -> Result<Vec<Account>> {
        let rows = self.read().records("SELECT id, data FROM accounts ORDER BY created_us", &[])?;
        self.collect(rows, account_aad)
    }

    fn get_account(&self, id: AccountId) -> Result<Account> {
        self.get(id)
    }

    fn put_account(&self, account: &Account) -> Result<()> {
        // The address, the host, every switch: all of it is inside `data`.
        // See the module docs for why an account has nothing worth a clear
        // column beyond the two timestamps every table here carries.
        self.upsert(account)
    }

    fn delete_account(&self, id: AccountId) -> Result<()> {
        // One transaction: an account whose secret survived it, or whose
        // calendar pointers did, is exactly the half-deleted state this
        // exists to avoid. Order does not matter for correctness -- none of
        // the three statements can fail because of another -- but the
        // account's own row goes last so a reader racing this transaction
        // never sees an account with no secret and no pointers, only an
        // account that still fully exists or one that is fully gone.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let owner = vals![ACCOUNT_SECRET_OWNER_KIND, id.to_string()];
        tx.execute("DELETE FROM record_secrets WHERE owner_kind = ?1 AND owner_id = ?2", &owner)?;
        tx.execute("DELETE FROM account_calendars WHERE account_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", &vals![id.to_string()])?;
        tx.commit()
    }
}
