//! Storage for Money: links, what sync learned about each, accounts, their
//! daily balances, transactions, categories and rules.
//!
//! # Why this is its own trait
//!
//! The Feed's reason: a backend that keeps journals as Markdown files has no
//! opinion about a bank, and folding one into
//! [`JournalStore`](super::JournalStore) would oblige it to grow one. So the
//! domain is reached through [`JournalStore::money`](super::JournalStore::money),
//! `None` by default, and the interface reads
//! [`Capabilities::money`](super::Capabilities::money) to know whether to
//! offer the app at all.
//!
//! # The rules an implementation must keep
//!
//! Every one of these is in the conformance suite (`conformance/money.rs`):
//!
//! - **Everything but the index is sealed.** Clear columns are ids, the
//!   owning link or account, sort order, the two timestamps, a transaction's
//!   date and its keyed dedupe key, a snapshot's day, and a state's next-due
//!   time. Never a name, an amount, a payee, a mask or a provider id.
//! - **A transaction's `(account_id, key)` is unique.** [`MoneyStore::put_txns`]
//!   writes all of a batch or none of it, and refuses -- with
//!   [`Error::Invalid`](crate::error::Error::Invalid), writing nothing -- a
//!   transaction whose pair is already held by a *different* id.
//! - **Deleting an account takes its transactions and balance snapshots.**
//! - **Deleting a link takes its state**, and then either its accounts with
//!   everything under them, or -- when asked to keep the history -- leaves
//!   them, detached: `link_id` cleared and `provider_account_id` emptied, so
//!   they are manual accounts from then on.
//! - **Deleting a category or a rule touches nothing else.** A transaction
//!   still pointing at a deleted category is the vault's to tidy.
//! - **Transactions come back newest first**: by date descending, then id
//!   descending, so a page is stable.

use crate::error::Result;
use crate::id::{BankAccountId, BankLinkId, MoneyCategoryId, MoneyRuleId, TxnId};
use crate::money::{
    BalanceSnapshot, BankAccount, BankLink, LinkState, MoneyCategory, MoneyRule, Txn,
};
use jiff::Timestamp;
use jiff::civil::Date;

/// The persistence contract for Money.
pub trait MoneyStore: Send + Sync {
    // ---- links ----------------------------------------------------------

    fn list_links(&self) -> Result<Vec<BankLink>>;
    fn get_link(&self, id: BankLinkId) -> Result<BankLink>;
    /// Insert or replace.
    fn put_link(&self, link: &BankLink) -> Result<()>;
    /// Delete the link and its state; then its accounts and everything under
    /// them, or with `keep_history` the accounts detached as manual ones.
    fn delete_link(&self, id: BankLinkId, keep_history: bool) -> Result<()>;

    /// `None` for a link nobody has synced.
    fn link_state(&self, id: BankLinkId) -> Result<Option<LinkState>>;
    /// Insert or replace.
    fn put_link_state(&self, state: &LinkState) -> Result<()>;
    /// Every link whose state's `next_at` is at or before `now`, plus every
    /// link with no state, soonest first. Paused links are the caller's to
    /// skip: `paused` is sealed.
    fn due_links(&self, now: Timestamp) -> Result<Vec<BankLinkId>>;
    /// The soonest `next_at` across every state, for the refresher to sleep
    /// until. `None` when no link has state.
    fn next_link_due(&self) -> Result<Option<Timestamp>>;

    // ---- accounts -------------------------------------------------------

    fn list_accounts(&self) -> Result<Vec<BankAccount>>;
    fn get_account(&self, id: BankAccountId) -> Result<BankAccount>;
    /// Insert or replace.
    fn put_account(&self, account: &BankAccount) -> Result<()>;
    /// Delete the account, its transactions and its balance snapshots.
    fn delete_account(&self, id: BankAccountId) -> Result<()>;

    /// Insert or replace by `(account_id, day)`.
    fn put_balance(&self, snapshot: &BalanceSnapshot) -> Result<()>;
    /// Every snapshot of every account with `from <= day <= to`, by day then
    /// account id.
    fn balances(&self, from: Date, to: Date) -> Result<Vec<BalanceSnapshot>>;

    // ---- transactions ---------------------------------------------------

    /// Every transaction in `accounts` (every account when empty) dated
    /// within `from..=to` (unbounded on a side given `None`), newest first.
    fn txns(
        &self,
        accounts: &[BankAccountId],
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<Vec<Txn>>;
    fn get_txn(&self, id: TxnId) -> Result<Txn>;
    /// Many at once, in no particular order; ids that do not exist are
    /// skipped rather than failing the call.
    fn get_txns(&self, ids: &[TxnId]) -> Result<Vec<Txn>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.get_txn(*id) {
                Ok(t) => out.push(t),
                Err(crate::error::Error::NotFound { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
    fn txn_by_key(&self, account: BankAccountId, key: &str) -> Result<Option<Txn>>;
    /// Insert or replace each by id, all or nothing. See the module doc for
    /// the uniqueness rule.
    fn put_txns(&self, txns: &[Txn]) -> Result<()>;
    /// Unknown ids are ignored.
    fn delete_txns(&self, ids: &[TxnId]) -> Result<()>;
    /// Every `(key, id)` stored for one account -- so an import can tell
    /// what is new before it does any work, decrypting nothing.
    fn txn_keys(&self, account: BankAccountId) -> Result<Vec<(String, TxnId)>>;

    // ---- categories and rules -------------------------------------------

    fn list_categories(&self) -> Result<Vec<MoneyCategory>>;
    fn get_category(&self, id: MoneyCategoryId) -> Result<MoneyCategory>;
    fn put_category(&self, category: &MoneyCategory) -> Result<()>;
    fn delete_category(&self, id: MoneyCategoryId) -> Result<()>;

    fn list_rules(&self) -> Result<Vec<MoneyRule>>;
    fn get_rule(&self, id: MoneyRuleId) -> Result<MoneyRule>;
    fn put_rule(&self, rule: &MoneyRule) -> Result<()>;
    fn delete_rule(&self, id: MoneyRuleId) -> Result<()>;
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn bank_link_aad(id: BankLinkId) -> Vec<u8> {
    format!("everyday.bank_link.v1:{id}").into_bytes()
}

/// A link's sync state, under a label of its own so it can never be passed
/// off as the link.
pub fn link_state_aad(id: BankLinkId) -> Vec<u8> {
    format!("everyday.bank_link_state.v1:{id}").into_bytes()
}

pub fn bank_account_aad(id: BankAccountId) -> Vec<u8> {
    format!("everyday.bank_account.v1:{id}").into_bytes()
}

/// A snapshot is bound to its account *and* its day, so one day's balance
/// cannot be moved to another.
pub fn balance_aad(account: BankAccountId, day: Date) -> Vec<u8> {
    format!("everyday.balance.v1:{account}:{day}").into_bytes()
}

pub fn txn_aad(id: TxnId) -> Vec<u8> {
    format!("everyday.txn.v1:{id}").into_bytes()
}

pub fn money_category_aad(id: MoneyCategoryId) -> Vec<u8> {
    format!("everyday.money_category.v1:{id}").into_bytes()
}

pub fn money_rule_aad(id: MoneyRuleId) -> Vec<u8> {
    format!("everyday.money_rule.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_is_distinct() {
        let id = uuid::Uuid::now_v7();
        let labels = [
            bank_link_aad(BankLinkId(id)),
            link_state_aad(BankLinkId(id)),
            bank_account_aad(BankAccountId(id)),
            balance_aad(BankAccountId(id), Date::constant(2026, 1, 1)),
            txn_aad(TxnId(id)),
            money_category_aad(MoneyCategoryId(id)),
            money_rule_aad(MoneyRuleId(id)),
        ];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_ne!(
            balance_aad(BankAccountId(id), Date::constant(2026, 1, 1)),
            balance_aad(BankAccountId(id), Date::constant(2026, 1, 2))
        );
    }
}
