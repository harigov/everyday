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
        // clear, which is what lets each of the statements below run
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
        // The "hidden ahead of the server confirming it" marker an
        // in-flight archive/trash/move left behind -- see
        // `everyday-store-sql::mail::write::hide_thread_from_mailbox`. Gone
        // with the threads it was keyed by; nothing will ever revert an op
        // belonging to an account that no longer exists.
        tx.execute(
            "DELETE FROM hidden_thread_mailboxes WHERE thread_id IN
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
        // Phase 7's split-inbox corrections -- `everyday_core::mail::CategoryRules`
        // -- are keyed by `account_id` exactly like every table above, and
        // were the one mail table this cascade forgot: a deleted account's
        // corrections used to survive it, ready to reapply to whatever
        // unrelated account happened to reuse the same sender address later.
        // `mail_contacts` and `mail_remote_image_settings`, by contrast, are
        // deliberately *not* here -- see `schema::v9`'s own docs -- because
        // neither carries an `account_id` at all: both are one-row,
        // vault-wide singletons, so there is no account-scoped slice of
        // either for a delete to narrow to.
        tx.execute("DELETE FROM mail_category_rules WHERE account_id = ?1", &account)?;
        let owner = vals![ACCOUNT_SECRET_OWNER_KIND, id.to_string()];
        tx.execute("DELETE FROM record_secrets WHERE owner_kind = ?1 AND owner_id = ?2", &owner)?;
        tx.execute("DELETE FROM account_calendars WHERE account_id = ?1", &account)?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", &account)?;
        tx.commit()
    }
}

/// Every mail table this crate's `schema::v9` keys directly by `account_id`
/// -- named here, by hand, so [`run_account_delete_cascade_regression`] can
/// check each one is actually empty after a delete, rather than trusting
/// `delete_account`'s own statement list to be complete. A table added to
/// the schema later without both a line in that cascade *and* a line here
/// still fails this test -- it just fails differently (the seeding loop
/// below asserts a row landed in every table this list names, so an
/// omission here is caught the moment the list and the schema disagree),
/// the same day `mail_category_rules` should have been caught four
/// versions ago.
///
/// `mail_contacts` and `mail_remote_image_settings` are deliberately absent
/// -- see `schema::v9`'s own docs -- both are one-row, vault-wide
/// singletons with no `account_id` column at all, so there is no
/// account-scoped slice of either for a cascade, or this test, to narrow
/// to. `account_calendars` (and the `calendars`/`events` rows it points at)
/// is calendar territory, already covered by its own delete-cascade
/// coverage; this list is the mail domain's alone.
#[cfg(any(test, feature = "testing"))]
const ACCOUNT_KEYED_MAIL_TABLES: &[&str] = &[
    "mailboxes",
    "mail_messages",
    "threads",
    "drafts",
    "ops",
    "mail_packs",
    "mail_category_rules",
];

/// Regression coverage for "deleting an account leaves its category rules
/// behind" (and, more generally, for any future mail table the cascade
/// forgets): seed one row for a fresh account in every table
/// [`ACCOUNT_KEYED_MAIL_TABLES`] names, delete the account, and assert every
/// one of those tables holds zero rows for it afterward. Enumerating the
/// tables by name, rather than asserting through the domain traits the way
/// [`everyday_core::store::conformance::mail::run_mail_suite`] already does,
/// is the point: a table this list names but the cascade does not clear
/// fails here even if nothing yet reads it back through a trait method,
/// exactly the gap that let `mail_category_rules` go unnoticed.
#[cfg(any(test, feature = "testing"))]
pub fn run_account_delete_cascade_regression(store: &SqlStore) {
    use everyday_core::account::Provider;
    use everyday_core::id::{MailMessageId, PackId, ThreadId};
    use everyday_core::mail::{
        Address, Category, CategoryRules, CategorySource, Draft, Mailbox, MailboxRole, Message,
        MessageFlags, Op, OpKind, OpTarget, Origin,
    };
    use everyday_core::packstore::{PackRef, PackStore};
    use everyday_core::store::mail::{IngestMessage, MailStore};

    eprintln!("--- account delete cascade regression ---");

    let account = Account::new(Provider::Custom, "cascade@example.com");
    let account_id = account.id;
    store.put_account(&account).unwrap();

    let mailbox = Mailbox::new(account_id, "INBOX", MailboxRole::Inbox);
    store.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let message_id = MailMessageId::new();
    let message = Message {
        id: message_id,
        account_id,
        thread_id,
        message_id_header: format!("<{message_id}@cascade.example>"),
        date: jiff::Timestamp::now(),
        from: Address::bare("someone@example.com"),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "cascade regression".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 3,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account_id.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    store.ingest(account_id, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }]).unwrap();

    store.put_draft(&Draft::new(account_id, "me@example.com", Origin::Person)).unwrap();
    store
        .enqueue_op(&Op::new(
            account_id,
            OpKind::Archive,
            OpTarget::Thread(thread_id),
            Origin::Person,
        ))
        .unwrap();
    // `SqlStore` answers `PackStore` on every dialect -- see `crate::packs`'
    // own docs -- so this seeds a real `mail_packs` row even on SQLite,
    // where nothing in the running app ever calls it (a local vault uses
    // `FilePackStore` there instead); the table, and the column this
    // cascade must clear, exist regardless of which backing a vault's
    // packs actually live in.
    store.append_batch(&account_id.to_string(), &[b"raw pack bytes".as_slice()]).unwrap();
    let mut rules = CategoryRules::default();
    rules.set_sender("someone@example.com", Category::Important);
    store.put_category_rules(account_id, &rules).unwrap();

    for table in ACCOUNT_KEYED_MAIL_TABLES {
        let count = store
            .with_read(|c| {
                c.scalar_i64(
                    &format!("SELECT COUNT(*) FROM {table} WHERE account_id = ?1"),
                    &vals![account_id.to_string()],
                )
            })
            .unwrap();
        assert!(count > 0, "seeding must have put a row of {account_id}'s in {table}");
    }

    AccountStore::delete_account(store, account_id).unwrap();

    for table in ACCOUNT_KEYED_MAIL_TABLES {
        let count = store
            .with_read(|c| {
                c.scalar_i64(
                    &format!("SELECT COUNT(*) FROM {table} WHERE account_id = ?1"),
                    &vals![account_id.to_string()],
                )
            })
            .unwrap();
        assert_eq!(count, 0, "{table} must have no rows left for a deleted account");
    }

    eprintln!("--- account delete cascade regression passed ---");
}
