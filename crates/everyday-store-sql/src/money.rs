//! Money: links, what sync learned about each, accounts, their daily
//! balances, transactions, categories and rules. See
//! `everyday_core::store::money` for the rules an implementation keeps and
//! `schema.rs`'s `v22` for the tables.
//!
//! What stays in the clear is only what an index is built from: which link
//! an account came through, which account a transaction or a balance belongs
//! to, a transaction's day and its dedupe key, a balance's day, when a link
//! is next due, sort orders and the two timestamps. Everything a person
//! would recognise -- a bank's name, an account's name and mask, every
//! amount and balance, a payee, a description, a note, a category, a rule,
//! Plaid's item, account and transaction ids, and its sync cursor -- is in
//! the sealed `data` beside them. The access token is not in these tables at
//! all: it is a `record_secrets` row, and this module never reads one.
//!
//! The dedupe key is a *keyed* hash (see `Vault::money_key`), so a database
//! holder cannot match it against a list of Plaid transaction ids, or
//! against a bank statement, to learn which transaction it is. What they
//! learn is that an account had eleven transactions on Tuesday.
//!
//! # Every row is bound to its own name
//!
//! Each sealed payload is bound by its associated data to the row it lives
//! in -- a link to its id, a link's state to the link's id under a label of
//! its own, a balance to its account *and* its day -- so a payload copied
//! into another row, by somebody who can write the database but does not
//! hold the key, will not open there. Nothing here seals under one record's
//! label and stores the result in another's row; the aad functions are all
//! `everyday_core::store::money`'s, and each is called with the id of the
//! row being written or read.
//!
//! # Where the filtering happens
//!
//! Only by the clear columns. A month's transactions are a range over `day`,
//! narrowed by `account_id` when asked, and ordered by `day` then `id`, both
//! clear, so a page is an index scan that decrypts exactly what it returns.
//! Category, text, amount and exclusion filters need the payload, and are
//! the vault's, after decrypting.
//!
//! # The dedupe key's uniqueness
//!
//! [`put_txns`](MoneyStore::put_txns) refuses -- writing nothing -- a batch
//! that would leave one `(account_id, dedupe_key)` pair under two ids. The
//! unique index is the backstop, but it is not the check: a violated index
//! answers with a driver's own error, different in each database, and in
//! Postgres it aborts the transaction it happened in. So the check is made
//! first, inside the same transaction as the writes, and answers with
//! [`Error::Invalid`] the same way on both. See [`check_keys`] for the
//! order it walks a batch in.

use everyday_core::error::{Error, Result};
use everyday_core::id::{BankAccountId, BankLinkId, MoneyCategoryId, MoneyRuleId, TxnId};
use everyday_core::money::{
    BalanceSnapshot, BankAccount, BankLink, LinkState, MoneyCategory, MoneyRule, Txn,
};
use everyday_core::store::money::{
    MoneyStore, balance_aad, bank_account_aad, bank_link_aad, link_state_aad, money_category_aad,
    money_rule_aad, txn_aad,
};
use jiff::Timestamp;
use jiff::civil::Date;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::str::FromStr;

use crate::conn::{Sql, SqlExt, ToValue, Value, Where};
use crate::record::{Record, upsert_stmt};
use crate::{SqlStore, from_us, placeholders, to_us, vals};

impl Record for BankLink {
    const TABLE: &'static str = "money_links";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the bank's name, and not Plaid's item id. A database that said
        // "Chase" and "Fidelity" would be saying where somebody keeps their
        // money; this says how many banks are linked and when.
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for BankAccount {
    const TABLE: &'static str = "money_accounts";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // The link is here so that disconnecting a bank can find its
        // accounts without opening every account. The name, the mask, the
        // kind, every balance and Plaid's account id are not.
        vec![
            ("link_id", self.link_id.map(|l| l.to_string()).to_value()),
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Txn {
    const TABLE: &'static str = "money_txns";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Which account, the keyed hash, the day. Never the amount, the
        // payee, the bank's line, the category or the note -- and never the
        // provider's own transaction id, which is what the key is a keyed
        // hash *of*, so that this column cannot be matched against it.
        vec![
            ("account_id", self.account_id.to_string().to_value()),
            ("dedupe_key", self.key.to_value()),
            ("day", day_text(self.date).to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for MoneyCategory {
    const TABLE: &'static str = "money_categories";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the name, the group, the kind or the budget.
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for MoneyRule {
    const TABLE: &'static str = "money_rules";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not what it matches, and not what it does: a rule's text is a
        // payee's name, and its account and amounts say what it is about.
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

/// How many ids go into one `IN (...)`. SQLite refuses a statement with more
/// than 32,766 bound parameters and Postgres more than 65,535, and a first
/// sync of two years of a busy card can bring more transactions than either.
/// `news.rs` and `mail/write.rs` keep the same number for the same reason.
const IN_CHUNK: usize = 500;

/// A day as the clear `day` columns hold it: `YYYY-MM-DD`, which sorts as
/// dates do in both dialects -- but only for years `0000` to `9999`. jiff's
/// dates reach back to year -9999, written with a sign and six digits
/// (`-000001-01-01`), and those sort among themselves backwards; [`check_day`]
/// keeps such a day out.
fn day_text(day: Date) -> String {
    day.to_string()
}

/// Refuse a day [`day_text`] could not store in order. No bank means one: a
/// date like that is a parser's mistake, and storing it would put the
/// transaction in the wrong place in every list rather than saying so.
fn check_day(day: Date) -> Result<()> {
    if (0..=9999).contains(&day.year()) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("{day} is not a date a bank account can hold")))
    }
}

fn day_of(text: &str) -> Result<Date> {
    text.parse::<Date>().map_err(|e| Error::Invalid(format!("a stored day is not a date: {e}")))
}

fn id_of<T: FromStr>(text: &str) -> Result<T>
where
    T::Err: Display,
{
    text.parse().map_err(|e: T::Err| Error::Invalid(e.to_string()))
}

/// Newest first, the order the trait promises: by date descending, then id
/// descending. The same order `ORDER BY day DESC, id DESC` gives, because
/// `day` is written from `date` and an id's text sorts as the id does -- the
/// fixed-width lower-case hex of a UUID orders exactly as its bytes.
fn newest_first(txns: &mut [Txn]) {
    txns.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
}

/// Refuse `txns` -- before any of it is written -- if writing it would
/// leave one `(account_id, dedupe_key)` pair under two ids.
///
/// The batch is walked in the order its rows will be written, against what
/// the database holds now. A transaction already stored frees the pair it
/// holds when it is rewritten, so a batch that moves one transaction off a
/// key and then gives that key to another is accepted: at no point does the
/// key have two holders. Two different ids wanting one pair, in the batch or
/// between the batch and what is stored, is refused. The same id twice is
/// two writes of one row, and the second wins, as it would in the table.
///
/// Two queries a chunk rather than one a row, because each statement is a
/// round trip to a Postgres server, and a first sync can bring thousands of
/// transactions: who holds every pair the batch wants, and which pair each
/// of the batch's ids holds today.
fn check_keys(tx: &mut dyn Sql, txns: &[Txn]) -> Result<()> {
    // (account, key) -> id, and id -> (account, key), as stored now.
    let mut holder: HashMap<(String, String), String> = HashMap::new();
    let mut pair_of: HashMap<String, (String, String)> = HashMap::new();

    let ids: Vec<String> =
        txns.iter().map(|t| t.id.to_string()).collect::<BTreeSet<_>>().into_iter().collect();
    for chunk in ids.chunks(IN_CHUNK) {
        let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
        let rows = tx.query(
            &format!(
                "SELECT id, account_id, dedupe_key FROM money_txns WHERE id IN ({})",
                placeholders(1, chunk.len())
            ),
            &args,
        )?;
        for row in &rows {
            let id = row.text(0)?;
            let pair = (row.text(1)?, row.text(2)?);
            holder.insert(pair.clone(), id.clone());
            pair_of.insert(id, pair);
        }
    }

    let mut wanted: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for t in txns {
        wanted.entry(t.account_id.to_string()).or_default().insert(t.key.as_str());
    }
    for (account, keys) in &wanted {
        let keys: Vec<&str> = keys.iter().copied().collect();
        for chunk in keys.chunks(IN_CHUNK) {
            let mut args = vec![Value::Text(account.clone())];
            args.extend(chunk.iter().map(|k| Value::Text((*k).to_string())));
            let rows = tx.query(
                &format!(
                    "SELECT dedupe_key, id FROM money_txns
                     WHERE account_id = ?1 AND dedupe_key IN ({})",
                    placeholders(2, chunk.len())
                ),
                &args,
            )?;
            for row in &rows {
                holder.insert((account.clone(), row.text(0)?), row.text(1)?);
            }
        }
    }

    for t in txns {
        let id = t.id.to_string();
        if let Some(old) = pair_of.remove(&id)
            && holder.get(&old) == Some(&id)
        {
            holder.remove(&old);
        }
        let pair = (t.account_id.to_string(), t.key.clone());
        if let Some(other) = holder.get(&pair)
            && *other != id
        {
            // The ids are clear columns already; the key, though only a
            // keyed hash, is left out because nothing reading this needs it.
            return Err(Error::Invalid(format!(
                "transaction {id} has the dedupe key of transaction {other}, \
                 already in the same account"
            )));
        }
        holder.insert(pair.clone(), id.clone());
        pair_of.insert(id, pair);
    }
    Ok(())
}

impl MoneyStore for SqlStore {
    // ---- links ----------------------------------------------------------

    fn list_links(&self) -> Result<Vec<BankLink>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM money_links ORDER BY sort_order, created_us, id", &[])?;
        self.collect(rows, bank_link_aad)
    }

    fn get_link(&self, id: BankLinkId) -> Result<BankLink> {
        self.get(id)
    }

    fn put_link(&self, link: &BankLink) -> Result<()> {
        self.upsert(link)
    }

    fn delete_link(&self, id: BankLinkId, keep_history: bool) -> Result<()> {
        // One transaction for the whole disconnect: a link gone with its
        // accounts still pointing at it, or accounts gone with their
        // transactions left behind, is a state nobody chose. Indexed
        // statements rather than a foreign-key cascade, for the reason
        // `delete_source` gives: they do not depend on a pragma staying on.
        // Deleting a link that is not there is not an error, as deleting a
        // source is not: a disconnect retried after it half-reached Plaid
        // must still finish here.
        let link = id.to_string();
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        if keep_history {
            // Detached, so that they are manual accounts from now on: no
            // link, and no provider id -- which is sealed, so each account
            // is opened, changed and resealed under its own aad, on the
            // writer, in this transaction. `rewrite_each` writes the row back
            // through `Record::columns`, which is what clears the `link_id`
            // column along with the sealed field. The timestamps are left as
            // they were: the store records what it is given, and when a
            // record was last edited is the vault's to say.
            self.rewrite_each::<BankAccount>(
                tx.as_mut(),
                "SELECT id, data FROM money_accounts WHERE link_id = ?1",
                &vals![link],
                |account| {
                    account.link_id = None;
                    account.provider_account_id.clear();
                },
            )?;
        } else {
            tx.execute(
                "DELETE FROM money_txns
                 WHERE account_id IN (SELECT id FROM money_accounts WHERE link_id = ?1)",
                &vals![link],
            )?;
            tx.execute(
                "DELETE FROM money_balances
                 WHERE account_id IN (SELECT id FROM money_accounts WHERE link_id = ?1)",
                &vals![link],
            )?;
            tx.execute("DELETE FROM money_accounts WHERE link_id = ?1", &vals![link])?;
        }
        tx.execute("DELETE FROM money_link_state WHERE link_id = ?1", &vals![link])?;
        tx.execute("DELETE FROM money_links WHERE id = ?1", &vals![link])?;
        tx.commit()
    }

    fn link_state(&self, id: BankLinkId) -> Result<Option<LinkState>> {
        let sealed = self.read().sealed(
            "SELECT data FROM money_link_state WHERE link_id = ?1",
            &vals![id.to_string()],
        )?;
        sealed.map(|sealed| self.unseal(&link_state_aad(id), &sealed)).transpose()
    }

    fn put_link_state(&self, state: &LinkState) -> Result<()> {
        // Under the state's own label, never the link's: a state row's data
        // copied over a link's (or the other way) will not open.
        let data = self.seal(&link_state_aad(state.link_id), state)?;
        self.write().execute(
            "INSERT INTO money_link_state (link_id, next_us, data) VALUES (?1, ?2, ?3)
             ON CONFLICT (link_id) DO UPDATE SET next_us = ?2, data = ?3",
            &vals![state.link_id.to_string(), to_us(state.next_at), data],
        )?;
        Ok(())
    }

    fn due_links(&self, now: Timestamp) -> Result<Vec<BankLinkId>> {
        // A link nobody has synced has no state to order by, so it is given
        // a group of its own ahead of the rest -- new, and wanted at once --
        // ordered among itself by when it was linked, oldest first. Then the
        // rest, soonest due first. Read from the links outward, so state left
        // behind for a link that is gone is never answered.
        let rows = self.read().query(
            "SELECT l.id FROM money_links l LEFT JOIN money_link_state s ON s.link_id = l.id
             WHERE s.link_id IS NULL OR s.next_us <= ?1
             ORDER BY CASE WHEN s.link_id IS NULL THEN 0 ELSE 1 END,
                      s.next_us, l.created_us, l.id",
            &vals![to_us(now)],
        )?;
        rows.iter().map(|r| id_of(&r.text(0)?)).collect()
    }

    fn next_link_due(&self) -> Result<Option<Timestamp>> {
        // Joined to the links, as `next_due` is to the Feed's sources: state
        // for a link deleted mid-sync must not hold the refresher awake for
        // a time nothing will ever be due at.
        let row = self.read().query_opt(
            "SELECT MIN(s.next_us) FROM money_link_state s JOIN money_links l ON l.id = s.link_id",
            &[],
        )?;
        Ok(row.map(|r| r.opt_i64(0)).transpose()?.flatten().map(from_us))
    }

    // ---- accounts -------------------------------------------------------

    fn list_accounts(&self) -> Result<Vec<BankAccount>> {
        let rows = self.read().records(
            "SELECT id, data FROM money_accounts ORDER BY sort_order, created_us, id",
            &[],
        )?;
        self.collect(rows, bank_account_aad)
    }

    fn get_account(&self, id: BankAccountId) -> Result<BankAccount> {
        self.get(id)
    }

    fn put_account(&self, account: &BankAccount) -> Result<()> {
        self.upsert(account)
    }

    fn delete_account(&self, id: BankAccountId) -> Result<()> {
        let account = id.to_string();
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM money_txns WHERE account_id = ?1", &vals![account])?;
        tx.execute("DELETE FROM money_balances WHERE account_id = ?1", &vals![account])?;
        tx.execute("DELETE FROM money_accounts WHERE id = ?1", &vals![account])?;
        tx.commit()
    }

    fn put_balance(&self, snapshot: &BalanceSnapshot) -> Result<()> {
        check_day(snapshot.day)?;
        // Bound to the account and the day, so one day's balance moved to
        // another day -- or another account -- by editing the clear columns
        // will not open.
        let data = self.seal(&balance_aad(snapshot.account_id, snapshot.day), snapshot)?;
        self.write().execute(
            "INSERT INTO money_balances (account_id, day, data) VALUES (?1, ?2, ?3)
             ON CONFLICT (account_id, day) DO UPDATE SET data = ?3",
            &vals![snapshot.account_id.to_string(), day_text(snapshot.day), data],
        )?;
        Ok(())
    }

    fn balances(&self, from: Date, to: Date) -> Result<Vec<BalanceSnapshot>> {
        let rows = self.read().query(
            "SELECT account_id, day, data FROM money_balances
             WHERE day >= ?1 AND day <= ?2 ORDER BY day, account_id",
            &vals![day_text(from), day_text(to)],
        )?;
        rows.iter()
            .map(|row| {
                let account: BankAccountId = id_of(&row.text(0)?)?;
                let day = day_of(&row.text(1)?)?;
                self.unseal(&balance_aad(account, day), &row.bytes(2)?)
            })
            .collect()
    }

    // ---- transactions ---------------------------------------------------

    fn txns(
        &self,
        accounts: &[BankAccountId],
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<Vec<Txn>> {
        let mut accounts: Vec<String> = accounts.iter().map(|a| a.to_string()).collect();
        accounts.sort();
        accounts.dedup();
        // Every account is one query with no account filter at all. A list
        // is one query a chunk -- in practice always one, since nobody keeps
        // five hundred accounts, but a list that long must still answer
        // rather than trip a parameter limit.
        let everything: [&[String]; 1] = [&[]];
        let chunks: Vec<&[String]> = if accounts.is_empty() {
            everything.to_vec()
        } else {
            accounts.chunks(IN_CHUNK).collect()
        };

        let mut out = Vec::new();
        for chunk in &chunks {
            let mut w = Where::new().in_list("account_id", chunk.iter());
            if let Some(from) = from {
                w = w.gte("day", day_text(from));
            }
            if let Some(to) = to {
                w = w.lte("day", day_text(to));
            }
            let (where_sql, args) = w.finish();
            let rows = self.read().records(
                &format!(
                    "SELECT id, data FROM money_txns WHERE {where_sql} ORDER BY day DESC, id DESC"
                ),
                &args,
            )?;
            let found: Vec<Txn> = self.collect(rows, txn_aad)?;
            out.extend(found);
        }
        if chunks.len() > 1 {
            newest_first(&mut out);
        }
        Ok(out)
    }

    fn get_txn(&self, id: TxnId) -> Result<Txn> {
        self.get(id)
    }

    fn get_txns(&self, ids: &[TxnId]) -> Result<Vec<Txn>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
            let rows = self.read().records(
                &format!(
                    "SELECT id, data FROM money_txns WHERE id IN ({})",
                    placeholders(1, chunk.len())
                ),
                &args,
            )?;
            let found: Vec<Txn> = self.collect(rows, txn_aad)?;
            out.extend(found);
        }
        Ok(out)
    }

    fn txn_by_key(&self, account: BankAccountId, key: &str) -> Result<Option<Txn>> {
        let rows = self.read().records(
            "SELECT id, data FROM money_txns WHERE account_id = ?1 AND dedupe_key = ?2",
            &vals![account.to_string(), key],
        )?;
        let found: Vec<Txn> = self.collect(rows, txn_aad)?;
        Ok(found.into_iter().next())
    }

    fn put_txns(&self, txns: &[Txn]) -> Result<()> {
        if txns.is_empty() {
            return Ok(());
        }
        // Sealed before the writer is taken: encryption is the expensive
        // part of a sync that brings five hundred transactions, and nothing
        // else needs to wait behind it.
        let mut sealed = Vec::with_capacity(txns.len());
        for txn in txns {
            check_day(txn.date)?;
            sealed.push((txn, self.seal(&txn_aad(txn.id), txn)?));
        }

        // Checked and written on the writer in one transaction, so a key the
        // check found free cannot be taken by another write before this one
        // lands. An early `?` drops the transaction, which rolls it back:
        // all of the batch or none of it.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        check_keys(tx.as_mut(), txns)?;
        for (txn, data) in sealed {
            let (sql, args) = upsert_stmt(txn, data);
            tx.execute(&sql, &args)?;
        }
        tx.commit()
    }

    fn delete_txns(&self, ids: &[TxnId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for chunk in ids.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
            tx.execute(
                &format!("DELETE FROM money_txns WHERE id IN ({})", placeholders(1, chunk.len())),
                &args,
            )?;
        }
        tx.commit()
    }

    fn txn_keys(&self, account: BankAccountId) -> Result<Vec<(String, TxnId)>> {
        // Two clear columns, by the unique index's own order: an import asks
        // this before it decrypts anything.
        self.read()
            .query(
                "SELECT dedupe_key, id FROM money_txns WHERE account_id = ?1 ORDER BY dedupe_key",
                &vals![account.to_string()],
            )?
            .iter()
            .map(|r| Ok((r.text(0)?, id_of(&r.text(1)?)?)))
            .collect()
    }

    // ---- categories and rules -------------------------------------------

    fn list_categories(&self) -> Result<Vec<MoneyCategory>> {
        let rows = self.read().records(
            "SELECT id, data FROM money_categories ORDER BY sort_order, created_us, id",
            &[],
        )?;
        self.collect(rows, money_category_aad)
    }

    fn get_category(&self, id: MoneyCategoryId) -> Result<MoneyCategory> {
        self.get(id)
    }

    fn put_category(&self, category: &MoneyCategory) -> Result<()> {
        self.upsert(category)
    }

    fn delete_category(&self, id: MoneyCategoryId) -> Result<()> {
        // Only the category. A transaction or a rule still naming it is the
        // vault's to tidy, under its own write lock -- the category id is in
        // their sealed payloads, which this could only reach by opening every
        // transaction in the vault.
        self.delete_by_id::<MoneyCategory>(id)?;
        Ok(())
    }

    fn list_rules(&self) -> Result<Vec<MoneyRule>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM money_rules ORDER BY sort_order, created_us, id", &[])?;
        self.collect(rows, money_rule_aad)
    }

    fn get_rule(&self, id: MoneyRuleId) -> Result<MoneyRule> {
        self.get(id)
    }

    fn put_rule(&self, rule: &MoneyRule) -> Result<()> {
        self.upsert(rule)
    }

    fn delete_rule(&self, id: MoneyRuleId) -> Result<()> {
        // Only the rule. A transaction it categorised keeps the category and
        // says a rule chose it; which rule no longer matters to anything.
        self.delete_by_id::<MoneyRule>(id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::schema::steps;
    use everyday_core::money::{
        AccountKind, CategorizedBy, CategoryKind, LinkProvider, PlaidEnvironment,
    };

    /// The columns `v22` declares for `table`, in order, `id` and `data` left
    /// out -- the list [`Record::columns`] has to match.
    fn declared(table: &str) -> Vec<String> {
        let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let ddl = steps(Dialect::Sqlite)
            .concat()
            .into_iter()
            .find(|sql| sql.contains(&head))
            .unwrap_or_else(|| panic!("no CREATE TABLE for {table}"));
        let open = ddl.find('(').expect("an open paren");
        let close = ddl.rfind(')').expect("a close paren");
        let body = &ddl[open + 1..close];
        body.lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(|word| word.trim_end_matches(',').to_string())
            .filter(|word| word != "id" && word != "data")
            .collect()
    }

    fn names<R: Record>(record: &R) -> Vec<String> {
        record.columns().into_iter().map(|(name, _)| name.to_string()).collect()
    }

    /// Records with something distinctive in every field a person would
    /// recognise, for the two tests below.
    const WORD: &str = "Platypus";
    const AMOUNT: i64 = -4_321;

    fn link() -> BankLink {
        let provider = LinkProvider::Plaid {
            item_id: format!("{WORD}-item"),
            institution_id: format!("{WORD}-institution"),
            environment: PlaidEnvironment::Production,
        };
        BankLink::new(provider, format!("{WORD} Bank"))
    }

    fn account(link: &BankLink) -> BankAccount {
        let name = format!("{WORD} checking");
        let mut account = BankAccount::manual(name, AccountKind::Credit, "USD");
        account.link_id = Some(link.id);
        account.provider_account_id = format!("{WORD}-account");
        account.official_name = format!("{WORD} Gold Card");
        account.mask = "4321".into();
        account.subtype = format!("{WORD} card");
        account.balance = Some(AMOUNT);
        account.available = Some(AMOUNT);
        account.limit = Some(AMOUNT);
        account
    }

    fn txn(account: &BankAccount) -> Txn {
        let mut txn = Txn::new(account.id, "0f3a9c", Date::constant(2026, 10, 14), AMOUNT, "USD");
        txn.provider_id = format!("{WORD}-txn");
        txn.payee = format!("{WORD} Cafe");
        txn.description = format!("SQ *{WORD} CAFE");
        txn.bank_category = format!("{WORD}_FOOD");
        txn.category = Some(MoneyCategoryId::new());
        txn.categorized_by = CategorizedBy::Rule { rule_id: MoneyRuleId::new() };
        txn.payee_override = format!("{WORD}'s");
        txn.note = format!("lunch at {WORD}");
        txn
    }

    fn category() -> MoneyCategory {
        let mut category = MoneyCategory::new(WORD, format!("{WORD} group"), CategoryKind::Expense);
        category.budget = Some(-AMOUNT);
        category.color = format!("{WORD}-green");
        category
    }

    fn rule(account: &BankAccount) -> MoneyRule {
        let mut rule = MoneyRule::new(WORD);
        rule.account_id = Some(account.id);
        rule.min_amount = Some(AMOUNT);
        rule.max_amount = Some(-AMOUNT);
        rule.category = Some(MoneyCategoryId::new());
        rule.rename = format!("{WORD} renamed");
        rule
    }

    #[test]
    fn the_clear_columns_are_exactly_the_ones_v22_declares() {
        // `upsert_stmt` writes `columns()` into the table by name, so a
        // column here that the table lacks fails every write, and one the
        // table has that is missing here is left at its default forever.
        let link = link();
        let account = account(&link);
        assert_eq!(names(&link), declared(BankLink::TABLE));
        assert_eq!(names(&account), declared(BankAccount::TABLE));
        assert_eq!(names(&txn(&account)), declared(Txn::TABLE));
        assert_eq!(names(&category()), declared(MoneyCategory::TABLE));
        assert_eq!(names(&rule(&account)), declared(MoneyRule::TABLE));
    }

    #[test]
    fn no_clear_column_holds_anything_a_person_would_recognise() {
        // What a database holder sees of each record is exactly its
        // `columns()`: ids, a link, an account, a keyed hash, a day, an order
        // and two times. Not a name, not a provider id, not an amount, and
        // never a sealed payload in a column that is not `data`.
        let link = link();
        let account = account(&link);
        let all: Vec<(&str, Vec<(&'static str, Value)>)> = vec![
            ("link", link.columns()),
            ("account", account.columns()),
            ("txn", txn(&account).columns()),
            ("category", category().columns()),
            ("rule", rule(&account).columns()),
        ];
        for (record, columns) in all {
            for (name, value) in columns {
                match &value {
                    Value::Text(text) => {
                        assert!(!text.contains(WORD), "{record}.{name} holds {text:?}");
                        // Equal, not contains: a UUID's hex may hold "4321".
                        assert_ne!(text, "4321", "{record}.{name} holds the mask");
                    }
                    Value::Int(n) => {
                        assert!(
                            *n != AMOUNT && *n != -AMOUNT,
                            "{record}.{name} holds an amount in the clear"
                        );
                    }
                    Value::Bytes(_) | Value::Real(_) => {
                        panic!("{record}.{name} is not a clear column of a kind v22 declares")
                    }
                    Value::Null | Value::Bool(_) => {}
                }
            }
        }
    }

    #[test]
    fn a_detached_account_clears_its_link_column() {
        // `delete_link`'s `keep_history` relies on this: the column follows
        // the record, so clearing the field is clearing the column.
        let mut account = account(&link());
        account.link_id = None;
        let column = account.columns().into_iter().find(|(name, _)| *name == "link_id");
        assert_eq!(column, Some(("link_id", Value::Null)));
    }

    #[test]
    fn a_day_outside_four_digit_years_is_refused_rather_than_misfiled() {
        assert!(check_day(Date::constant(2026, 11, 5)).is_ok());
        assert!(check_day(Date::constant(9999, 12, 31)).is_ok());
        assert!(check_day(Date::constant(0, 1, 1)).is_ok());
        assert!(check_day(Date::constant(-1, 1, 1)).is_err());
        assert_eq!(day_text(Date::constant(2026, 1, 5)), "2026-01-05");
        assert_eq!(day_of("2026-01-05").unwrap(), Date::constant(2026, 1, 5));
        // Why: before year 0 the text sorts backwards among itself.
        assert!(day_text(Date::constant(-1, 1, 1)) < day_text(Date::constant(-2, 1, 1)));
    }

    #[test]
    fn newest_first_breaks_a_tie_by_id_descending() {
        let account = BankAccountId::new();
        let day = Date::constant(2026, 10, 15);
        let older = Txn::new(account, "a", Date::constant(2026, 10, 1), -1, "USD");
        let mut tied = vec![
            Txn::new(account, "b", day, -2, "USD"),
            Txn::new(account, "c", day, -3, "USD"),
            older.clone(),
        ];
        newest_first(&mut tied);
        assert_eq!(tied[2].id, older.id);
        assert!(tied[0].id > tied[1].id);
        // And the text the SQL orders by agrees with the id's own order.
        assert!(tied[0].id.to_string() > tied[1].id.to_string());
    }
}
