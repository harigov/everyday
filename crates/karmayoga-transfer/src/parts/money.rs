//! Money: the accounts, every transaction, the categories with their
//! budgets, and the rules.
//!
//! ```text
//!   money/
//!     accounts.csv       one row per account
//!     transactions.csv   one row per transaction, amounts as plain decimals
//!     categories.csv     one row per category, with its monthly budget
//!     rules.json         the rules, whole
//! ```
//!
//! # What is not here, ever
//!
//! The Plaid keys, a bank's access token, a sync cursor, and every id a
//! provider gave -- Plaid's item, account and transaction ids, a file's
//! `FITID`, the keyed hash a transaction is deduplicated by. The keys and
//! tokens are not records and nothing here could reach them if it tried:
//! they are sealed secrets that only `Vault::plaid_access` and
//! `Vault::bank_link_access` open, and this part reads the store's records
//! and nothing else. The ids are records' fields, and are left out on
//! purpose: they mean something only to the bank and this vault, and a
//! leaked Plaid id is a thread somebody could pull. A bank is named by the
//! name the person gave it, and an account by its mask, which is all of its
//! number this application ever holds.
//!
//! # A spreadsheet will open this
//!
//! So a cell that starts the way a formula does -- `=`, `+`, `-`, `@`, a tab
//! or a carriage return -- is written with an apostrophe in front, which a
//! spreadsheet shows as text rather than running. Payees and descriptions
//! are the bank's words, and a transfer's memo is whoever sent the money's:
//! `=HYPERLINK(...)` in one is somebody else's idea of a cell. Only cells of
//! text get the apostrophe; amounts, dates and ids are written by this code
//! and are what they look like, so `-12.50` stays a number.
//!
//! # Not read back
//!
//! Transactions are not imported: a bank hands them over again, and an
//! account id from another vault means nothing in this one. Categories and
//! rules could be, and are not yet -- a rule names categories and accounts
//! by id, so bringing rules over faithfully needs the categories and
//! accounts they point at matched first, which is more than a sidecar can
//! promise. The files are for reading.

use crate::text::Csv;
use crate::{Files, Options, Portable, Spec};
use karmayoga_core::Result;
use karmayoga_core::money::report::main_currency;
use karmayoga_core::money::{
    BankAccount, BankLink, CategorizedBy, MoneyCategory, MoneyRule, Txn, TxnOrigin, format_minor,
};
use karmayoga_core::store::JournalStore;
use karmayoga_core::{BankLinkId, MoneyCategoryId};
use std::collections::HashMap;

pub struct MoneyPart;
pub static MONEY: MoneyPart = MoneyPart;

static SPEC: Spec = Spec {
    id: "money",
    label: "Money",
    summary: "Your accounts, every transaction with its category and note, your categories \
              and budgets, and your rules. Never your Plaid keys or a bank's sign-in.",
    format: "CSV, with the rules as JSON",
    media: false,
    imports: false,
};

const ACCOUNTS: &str = "accounts.csv";
const TRANSACTIONS: &str = "transactions.csv";
const CATEGORIES: &str = "categories.csv";
const RULES: &str = "rules.json";

const ACCOUNT_COLUMNS: &[&str] = &[
    "name",
    "bank",
    "official_name",
    "mask",
    "kind",
    "subtype",
    "currency",
    "balance",
    "available",
    "limit",
    "balance_at",
    "hidden",
    "order",
    "id",
];
const TRANSACTION_COLUMNS: &[&str] = &[
    "date",
    "account",
    "payee",
    "bank_payee",
    "description",
    "amount",
    "currency",
    "category",
    "group",
    "note",
    "pending",
    "excluded",
    "source",
    "bank_category",
    "categorized_by",
    "account_id",
    "category_id",
    "id",
];
const CATEGORY_COLUMNS: &[&str] =
    &["name", "group", "kind", "budget", "currency", "color", "archived", "order", "id"];

/// What `rules.json` holds.
#[derive(serde::Serialize)]
struct Sidecar<'a> {
    rules: &'a [MoneyRule],
}

impl Portable for MoneyPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    /// Accounts, transactions and rules: what a person made or brought.
    /// Not categories, which every vault starts with whether or not Money
    /// was ever opened, and which would make an untouched app look full.
    /// Transactions are counted by their keys, which decrypts none of them.
    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(money) = store.money() else { return Ok(None) };
        let accounts = money.list_accounts()?;
        let mut count = accounts.len() + money.list_rules()?.len();
        for account in &accounts {
            count += money.txn_keys(account.id)?.len();
        }
        Ok(Some(count as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(money) = store.money() else { return Ok(()) };
        let mut accounts = money.list_accounts()?;
        let mut categories = money.list_categories()?;
        let mut rules = money.list_rules()?;
        // The starter categories alone are not something anybody made. A
        // budget set before any account is, so it still counts.
        if accounts.is_empty() && rules.is_empty() && categories.iter().all(|c| c.budget.is_none())
        {
            return Ok(());
        }
        let links: HashMap<BankLinkId, BankLink> =
            money.list_links()?.into_iter().map(|l| (l.id, l)).collect();
        accounts.sort_by(|a, b| {
            a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
        });
        categories.sort_by(|a, b| {
            a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
        });
        karmayoga_core::money::rules::in_order(&mut rules);
        // Oldest first, the order a statement reads in -- and the same file
        // from two exports of one vault.
        let mut txns = money.txns(&[], None, None)?;
        txns.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));

        if !accounts.is_empty() {
            let mut csv = Csv::new(ACCOUNT_COLUMNS);
            for account in &accounts {
                csv.row(&account_row(account, &links));
            }
            out.records(ACCOUNTS, csv.finish(), accounts.len() as u64)?;
        }

        if !txns.is_empty() {
            let account_names: HashMap<_, _> =
                accounts.iter().map(|a| (a.id, a.name.as_str())).collect();
            let by_id: HashMap<MoneyCategoryId, &MoneyCategory> =
                categories.iter().map(|c| (c.id, c)).collect();
            let mut csv = Csv::new(TRANSACTION_COLUMNS);
            for txn in &txns {
                let account = account_names.get(&txn.account_id).copied().unwrap_or_default();
                csv.row(&txn_row(txn, account, &by_id));
            }
            out.records(TRANSACTIONS, csv.finish(), txns.len() as u64)?;
        }

        if !categories.is_empty() {
            // A budget is counted in the vault's main currency -- the one
            // the budget view adds spending up in.
            let currency = main_currency(&accounts);
            let mut csv = Csv::new(CATEGORY_COLUMNS);
            for category in &categories {
                csv.row(&category_row(category, &currency));
            }
            out.text(CATEGORIES, csv.finish())?;
        }

        if !rules.is_empty() {
            let json = serde_json::to_string_pretty(&Sidecar { rules: &rules })?;
            out.records(RULES, format!("{json}\n"), rules.len() as u64)?;
        }
        Ok(())
    }
}

fn account_row(account: &BankAccount, links: &HashMap<BankLinkId, BankLink>) -> Vec<String> {
    let bank = account.link_id.and_then(|id| links.get(&id)).map(|l| l.name.as_str()).unwrap_or("");
    let minor =
        |value: Option<i64>| value.map(|v| format_minor(v, &account.currency)).unwrap_or_default();
    vec![
        text(&account.name),
        text(bank),
        text(&account.official_name),
        text(&account.mask),
        word(&account.kind),
        text(&account.subtype),
        text(&account.currency),
        minor(account.balance),
        minor(account.available),
        minor(account.limit),
        account.balance_at.map(|at| at.to_string()).unwrap_or_default(),
        account.hidden.to_string(),
        account.sort_order.to_string(),
        account.id.to_string(),
    ]
}

fn txn_row(
    txn: &Txn,
    account: &str,
    categories: &HashMap<MoneyCategoryId, &MoneyCategory>,
) -> Vec<String> {
    let category = txn.category.and_then(|id| categories.get(&id)).copied();
    vec![
        txn.date.to_string(),
        text(account),
        text(txn.shown_payee()),
        text(&txn.payee),
        text(&txn.description),
        format_minor(txn.amount, &txn.currency),
        text(&txn.currency),
        text(category.map(|c| c.name.as_str()).unwrap_or("")),
        text(category.map(|c| c.group.as_str()).unwrap_or("")),
        text(&txn.note),
        txn.pending.to_string(),
        txn.excluded.to_string(),
        match txn.origin {
            TxnOrigin::Bank => "bank",
            TxnOrigin::Import => "file",
            TxnOrigin::Manual => "typed",
        }
        .to_string(),
        text(&txn.bank_category),
        match txn.categorized_by {
            CategorizedBy::Nobody => "",
            CategorizedBy::Bank => "bank",
            CategorizedBy::Rule { .. } => "rule",
            CategorizedBy::Person => "you",
        }
        .to_string(),
        txn.account_id.to_string(),
        // Only a category that still exists: a dangling id names nothing.
        category.map(|c| c.id.to_string()).unwrap_or_default(),
        txn.id.to_string(),
    ]
}

fn category_row(category: &MoneyCategory, currency: &str) -> Vec<String> {
    let budget = category.budget.map(|b| format_minor(b, currency));
    vec![
        text(&category.name),
        text(&category.group),
        word(&category.kind),
        budget.clone().unwrap_or_default(),
        if budget.is_some() { currency.to_string() } else { String::new() },
        text(&category.color),
        category.archived.to_string(),
        category.sort_order.to_string(),
        category.id.to_string(),
    ]
}

/// A cell of somebody's text, made safe to open in a spreadsheet: one that
/// starts the way a formula does gets an apostrophe in front, which a
/// spreadsheet shows as text. Leading spaces are looked past, since some
/// programs look past them too. See the module doc.
fn text(value: &str) -> String {
    match value.trim_start_matches(' ').chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{value}"),
        _ => value.to_string(),
    }
}

/// An enum's own word on the wire -- `cash`, `expense` -- so the file says
/// what the app and the interface say.
fn word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_that_would_run_as_a_formula_is_written_as_text() {
        for risky in ["=cmd|' /C calc'!A0", "+SUM(1+1)", "-2+3", "@cmd", "\t=1", "\r=1", "  =1"] {
            assert!(text(risky).starts_with('\''), "{risky:?}");
        }
        for plain in ["Whole Foods", "AT&T", "", "1-800-FLOWERS", "Café"] {
            assert_eq!(text(plain), plain);
        }
    }

    #[test]
    fn enums_are_written_in_the_words_the_app_uses() {
        assert_eq!(word(&karmayoga_core::money::AccountKind::Credit), "credit");
        assert_eq!(word(&karmayoga_core::money::CategoryKind::Transfer), "transfer");
    }
}
