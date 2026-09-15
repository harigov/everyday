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
//!
//! # Calendars are part of the cascade, not a pointer left dangling
//!
//! `everyday_core::store::accounts`'s own module doc, written in phase 1
//! before `CalendarOrigin::Account` existed, says the cascade "deliberately
//! does not take the calendars themselves" -- that was true when there was
//! nothing yet to take. Phase 6 changes it: an account calendar's events are
//! read-only copies of somebody else's data, exactly like a feed's, and a
//! calendar with no account behind it any more is not a calendar anybody can
//! still read from, subscribe to again with a saved address, or usefully
//! keep. So `delete_account` now reads `account_calendars` for the ids it
//! points at and removes those calendars' events and the calendars
//! themselves in the same transaction, before dropping the pointer rows and
//! the account. A calendar record with no account and no events -- the
//! half-deleted state this exists to avoid -- is never observable.

use everyday_core::account::{ACCOUNT_SECRET_OWNER_KIND, Account};
use everyday_core::error::Result;
use everyday_core::id::AccountId;
use everyday_core::store::accounts::{AccountStore, account_aad};

use crate::conn::{SqlExt, ToValue, Value};
use crate::purpose::{RecordKind, forget_purposes};
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
        //
        // Mail joins the cascade here rather than in its own store's
        // `delete_account` -- there is no such method; `AccountStore` is the
        // one place this trait promises the whole cascade happens, per its
        // own module docs, and every mail table names `account_id` in the
        // clear, which is what lets each of the eight statements below run
        // as a plain indexed delete rather than a join back into `accounts`.
        // Deleted inside-out -- bodies and `message_mailboxes` before the
        // messages they point at, `thread_mailboxes` before the threads --
        // so that no statement's subquery ever reads a row a later statement
        // in the same transaction has already removed.
        let account = vals![id.to_string()];
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        // Read the ids first, not as a subquery on the deletes below: by the
        // time the calendar and event rows are gone, `account_calendars`'
        // own row for each is still there to say whose it was -- this is the
        // one moment that is still true, and `forget_purposes` needs the ids
        // as plain strings, which a subquery cannot hand it.
        let calendar_ids: Vec<String> = tx
            .query("SELECT calendar_id FROM account_calendars WHERE account_id = ?1", &account)?
            .into_iter()
            .map(|row| row.text(0))
            .collect::<Result<_>>()?;
        for calendar_id in &calendar_ids {
            tx.execute("DELETE FROM events WHERE calendar_id = ?1", &vals![calendar_id.as_str()])?;
            tx.execute("DELETE FROM calendars WHERE id = ?1", &vals![calendar_id.as_str()])?;
        }
        forget_purposes(tx.as_mut(), RecordKind::Calendar, &calendar_ids)?;
        tx.execute(
            "DELETE FROM bodies WHERE message_id IN
                 (SELECT id FROM mail_messages WHERE account_id = ?1)",
            &account,
        )?;
        tx.execute(
            "DELETE FROM message_mailboxes WHERE message_id IN
                 (SELECT id FROM mail_messages WHERE account_id = ?1)",
            &account,
        )?;
        tx.execute(
            "DELETE FROM thread_mailboxes WHERE thread_id IN
                 (SELECT id FROM threads WHERE account_id = ?1)",
            &account,
        )?;
        tx.execute("DELETE FROM mail_messages WHERE account_id = ?1", &account)?;
        tx.execute("DELETE FROM threads WHERE account_id = ?1", &account)?;
        tx.execute("DELETE FROM mailboxes WHERE account_id = ?1", &account)?;
        tx.execute("DELETE FROM drafts WHERE account_id = ?1", &account)?;
        tx.execute("DELETE FROM ops WHERE account_id = ?1", &account)?;
        // `mail_packs` is the Postgres-only, table-backed home for raw
        // messages -- see `everyday_core::packstore` and `crate::packs`. A
        // SQLite vault has no rows here to delete; the statement is simply a
        // no-op on that backend, exactly as `blobs` would be for a vault
        // whose media live in files instead.
        tx.execute("DELETE FROM mail_packs WHERE account_id = ?1", &account)?;
        let owner = vals![ACCOUNT_SECRET_OWNER_KIND, id.to_string()];
        tx.execute("DELETE FROM record_secrets WHERE owner_kind = ?1 AND owner_id = ?2", &owner)?;
        tx.execute("DELETE FROM account_calendars WHERE account_id = ?1", &account)?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", &account)?;
        tx.commit()
    }
}
