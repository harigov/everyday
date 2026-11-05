//! What the numbers add up to: a month's spending by category against its
//! budget, income and spending month by month, and net worth over time.
//!
//! Pure: handed the transactions, categories and accounts, never a store.
//! The vault reads what a report needs and calls these, so every rule about
//! what counts is here and under test.
//!
//! # What counts
//!
//! - Only transactions in `currency`. Others are counted in
//!   `other_currencies` and otherwise left out: adding yen to dollars is a
//!   number that means nothing.
//! - Not `excluded` ones, and nothing in a hidden account.
//! - A [`CategoryKind::Transfer`] category is neither spending nor income.
//! - A [`CategoryKind::Income`] category is income (its sign as it comes).
//! - An expense category is spending: money out adds to it, a refund in the
//!   same category takes from it.
//! - No category: money out is spending, money in is income.
//! - A category id that no longer exists counts as no category.
//! - Pending transactions count. They are real money already promised.

use super::{BalanceSnapshot, BankAccount, CategoryKind, MoneyCategory, Txn};
use crate::error::{Error, Result};
use crate::id::{BankAccountId, MoneyCategoryId};
use jiff::civil::Date;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

/// One category's month.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategorySpend {
    /// `None` is "no category".
    pub category_id: Option<MoneyCategoryId>,
    /// Spending in it, positive (refunds already taken off; can go below
    /// zero in a month of refunds).
    pub spent: i64,
    /// Its monthly budget, if it has one.
    pub budget: Option<i64>,
    pub count: u32,
}

/// One month, in one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthSummary {
    /// `YYYY-MM`.
    pub month: String,
    pub currency: String,
    /// Money in, positive.
    pub income: i64,
    /// Money out, positive.
    pub spending: i64,
    /// `income - spending`.
    pub net: i64,
    /// Every expense category with spending or a budget, plus "no
    /// category" when anything had none -- most spent first, budgeted ones
    /// with nothing spent after, by category order.
    pub categories: Vec<CategorySpend>,
    /// The sum of every expense category's budget.
    pub budgeted: i64,
    /// Counted transactions with no category.
    pub uncategorized: u32,
    /// Currencies seen and left out, sorted.
    pub other_currencies: Vec<String>,
}

/// Income and spending for one month.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CashFlowMonth {
    pub month: String,
    pub income: i64,
    pub spending: i64,
}

/// Net worth on one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetWorthPoint {
    pub day: Date,
    /// What the asset accounts held.
    pub assets: i64,
    /// What the liability accounts owed, positive.
    pub liabilities: i64,
}

/// The first day of the month a `YYYY-MM` string names.
pub fn parse_month(month: &str) -> Result<Date> {
    let invalid = || Error::Invalid(format!("not a month (YYYY-MM): {month:?}"));
    let (y, m) = month.trim().split_once('-').ok_or_else(invalid)?;
    let year: i16 = y.parse().map_err(|_| invalid())?;
    let month: i8 = m.parse().map_err(|_| invalid())?;
    Date::new(year, month, 1).map_err(|_| invalid())
}

/// `YYYY-MM` for the month `day` falls in.
pub fn month_key(day: Date) -> String {
    format!("{:04}-{:02}", day.year(), day.month())
}

/// How one transaction counts, by the rules in the module doc. Decided in
/// one place so the month view, the cash-flow bars and the budget can never
/// disagree about a refund or a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Counted {
    /// Excluded, or in an account that is hidden or not among those given.
    Out,
    /// In another currency: named, and otherwise left out.
    Foreign(String),
    /// In a transfer category: neither spending nor income.
    Transfer,
    /// Income, signed as it came. `uncategorized` when it had no category
    /// (or one that no longer exists) and counts as income by direction.
    Income { amount: i64, uncategorized: bool },
    /// Spending, positive for money out; a refund is negative. `category`
    /// is `None` for money out with no category.
    Spending { amount: i64, category: Option<MoneyCategoryId> },
}

/// What [`Counted`] is decided against: the currency, the categories by id,
/// and which accounts are counted at all.
struct Counter<'a> {
    currency: String,
    categories: HashMap<MoneyCategoryId, &'a MoneyCategory>,
    visible: HashSet<BankAccountId>,
}

impl<'a> Counter<'a> {
    fn new(currency: &str, categories: &'a [MoneyCategory], accounts: &[BankAccount]) -> Self {
        Self {
            currency: currency.trim().to_ascii_uppercase(),
            categories: categories.iter().map(|c| (c.id, c)).collect(),
            // An account that is not in the list is as good as hidden: the
            // vault hands over the visible ones, and a transaction whose
            // account is gone has nowhere to be shown.
            visible: accounts.iter().filter(|a| !a.hidden).map(|a| a.id).collect(),
        }
    }

    fn count(&self, txn: &Txn) -> Counted {
        if txn.excluded || !self.visible.contains(&txn.account_id) {
            return Counted::Out;
        }
        let currency = txn.currency.trim();
        if !currency.eq_ignore_ascii_case(&self.currency) {
            return Counted::Foreign(currency.to_ascii_uppercase());
        }
        match txn.category.and_then(|id| self.categories.get(&id)) {
            Some(category) => match category.kind {
                CategoryKind::Transfer => Counted::Transfer,
                CategoryKind::Income => {
                    Counted::Income { amount: txn.amount, uncategorized: false }
                }
                CategoryKind::Expense => Counted::Spending {
                    amount: txn.amount.saturating_neg(),
                    category: Some(category.id),
                },
            },
            None if txn.amount < 0 => {
                Counted::Spending { amount: txn.amount.saturating_neg(), category: None }
            }
            None => Counted::Income { amount: txn.amount, uncategorized: true },
        }
    }
}

/// The order categories are listed in when nothing else decides:
/// `sort_order`, then name, then id. A category's position in it, by id.
fn category_ranks(categories: &[MoneyCategory]) -> HashMap<MoneyCategoryId, usize> {
    let mut sorted: Vec<&MoneyCategory> = categories.iter().collect();
    sorted.sort_by(|a, b| {
        a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
    });
    sorted.into_iter().enumerate().map(|(i, c)| (c.id, i)).collect()
}

fn in_month(day: Date, month: Date) -> bool {
    day.year() == month.year() && day.month() == month.month()
}

/// One month's summary. `txns` may hold transactions from other months;
/// only those in `month` (any day of it) count.
///
/// Only transactions in one of `accounts` that is not hidden count; see
/// the module doc for the rest.
pub fn month_summary(
    month: Date,
    currency: &str,
    txns: &[Txn],
    categories: &[MoneyCategory],
    accounts: &[BankAccount],
) -> MonthSummary {
    let counter = Counter::new(currency, categories, accounts);
    let mut income: i64 = 0;
    let mut spending: i64 = 0;
    let mut uncategorized: u32 = 0;
    let mut per_category: HashMap<Option<MoneyCategoryId>, (i64, u32)> = HashMap::new();
    let mut others: BTreeSet<String> = BTreeSet::new();

    for txn in txns.iter().filter(|t| in_month(t.date, month)) {
        match counter.count(txn) {
            Counted::Out | Counted::Transfer => {}
            Counted::Foreign(code) => {
                if !code.is_empty() {
                    others.insert(code);
                }
            }
            Counted::Income { amount, uncategorized: no_category } => {
                income = income.saturating_add(amount);
                if no_category {
                    uncategorized = uncategorized.saturating_add(1);
                }
            }
            Counted::Spending { amount, category } => {
                spending = spending.saturating_add(amount);
                if category.is_none() {
                    uncategorized = uncategorized.saturating_add(1);
                }
                let slot = per_category.entry(category).or_default();
                slot.0 = slot.0.saturating_add(amount);
                slot.1 = slot.1.saturating_add(1);
            }
        }
    }

    // Every expense category that had something happen in it, or that has
    // a budget (unless archived: a budget nobody can see in the picker is
    // not one they are keeping). Then "no category", if money left that way.
    let ranks = category_ranks(categories);
    let mut rows: Vec<(usize, CategorySpend)> = Vec::new();
    for category in categories.iter().filter(|c| c.kind == CategoryKind::Expense) {
        let (spent, count) = per_category.get(&Some(category.id)).copied().unwrap_or((0, 0));
        if count > 0 || (category.budget.is_some() && !category.archived) {
            rows.push((
                ranks.get(&category.id).copied().unwrap_or(usize::MAX - 1),
                CategorySpend {
                    category_id: Some(category.id),
                    spent,
                    budget: category.budget,
                    count,
                },
            ));
        }
    }
    if let Some((spent, count)) = per_category.get(&None).copied() {
        rows.push((usize::MAX, CategorySpend { category_id: None, spent, budget: None, count }));
    }
    // Most spent first among those with anything in them; then budgeted ones
    // with nothing spent, in category order.
    rows.sort_by(|(rank_a, a), (rank_b, b)| {
        (b.count > 0)
            .cmp(&(a.count > 0))
            .then_with(|| if a.count > 0 { b.spent.cmp(&a.spent) } else { Ordering::Equal })
            .then(rank_a.cmp(rank_b))
    });
    let budgeted =
        rows.iter().filter_map(|(_, r)| r.budget).fold(0_i64, |sum, b| sum.saturating_add(b));

    MonthSummary {
        month: month_key(month),
        currency: counter.currency.clone(),
        income,
        spending,
        net: income.saturating_sub(spending),
        categories: rows.into_iter().map(|(_, r)| r).collect(),
        budgeted,
        uncategorized,
        other_currencies: others.into_iter().collect(),
    }
}

/// Income and spending for each of `months` (any day of each), in the
/// order given, by the same rules as [`month_summary`].
pub fn cash_flow(
    months: &[Date],
    currency: &str,
    txns: &[Txn],
    categories: &[MoneyCategory],
    accounts: &[BankAccount],
) -> Vec<CashFlowMonth> {
    let counter = Counter::new(currency, categories, accounts);
    let mut by_month: HashMap<(i16, i8), (i64, i64)> =
        months.iter().map(|m| ((m.year(), m.month()), (0, 0))).collect();
    for txn in txns {
        let Some(slot) = by_month.get_mut(&(txn.date.year(), txn.date.month())) else {
            continue;
        };
        match counter.count(txn) {
            Counted::Income { amount, .. } => slot.0 = slot.0.saturating_add(amount),
            Counted::Spending { amount, .. } => slot.1 = slot.1.saturating_add(amount),
            Counted::Out | Counted::Foreign(_) | Counted::Transfer => {}
        }
    }
    months
        .iter()
        .map(|m| {
            let (income, spending) =
                by_month.get(&(m.year(), m.month())).copied().unwrap_or((0, 0));
            CashFlowMonth { month: month_key(*m), income, spending }
        })
        .collect()
}

/// Net worth on every day from `from` to `to` that any account has a
/// snapshot for, each account carried forward at its last known balance --
/// and on `to` itself, from the accounts' current balances. Only accounts in
/// `currency`, not hidden.
///
/// Snapshots from before `from` may be passed, and are the balances carried
/// into the range; when there are any, `from` gets a point of its own, so
/// the line starts where it was asked to rather than at the first day
/// something changed. An account with no balance known by a day adds
/// nothing to it. Empty when `from` is after `to`.
pub fn net_worth(
    from: Date,
    to: Date,
    currency: &str,
    accounts: &[BankAccount],
    snapshots: &[BalanceSnapshot],
) -> Vec<NetWorthPoint> {
    if from > to {
        return Vec::new();
    }
    let wanted = currency.trim();
    let counted: HashMap<BankAccountId, &BankAccount> = accounts
        .iter()
        .filter(|a| !a.hidden && a.currency.trim().eq_ignore_ascii_case(wanted))
        .map(|a| (a.id, a))
        .collect();
    let mut snaps: Vec<&BalanceSnapshot> =
        snapshots.iter().filter(|s| s.day <= to && counted.contains_key(&s.account_id)).collect();
    snaps.sort_by(|a, b| a.day.cmp(&b.day).then(a.account_id.cmp(&b.account_id)));

    let mut days: BTreeSet<Date> = snaps.iter().map(|s| s.day).filter(|d| *d >= from).collect();
    if snaps.iter().any(|s| s.day < from) {
        days.insert(from);
    }
    days.insert(to);

    let mut latest: HashMap<BankAccountId, i64> = HashMap::new();
    let mut next = 0;
    let mut points = Vec::with_capacity(days.len());
    for day in days {
        while let Some(snap) = snaps.get(next) {
            if snap.day > day {
                break;
            }
            latest.insert(snap.account_id, snap.balance);
            next += 1;
        }
        if day == to {
            for account in counted.values() {
                if let Some(balance) = account.balance {
                    latest.insert(account.id, balance);
                }
            }
        }
        let (mut assets, mut liabilities) = (0_i64, 0_i64);
        for (id, balance) in &latest {
            match counted.get(id) {
                Some(account) if account.kind.is_liability() => {
                    liabilities = liabilities.saturating_add(*balance);
                }
                Some(_) => assets = assets.saturating_add(*balance),
                None => {}
            }
        }
        points.push(NetWorthPoint { day, assets, liabilities });
    }
    points
}

/// The currency most of the visible accounts are in -- what reports and
/// budgets are counted in. `"USD"` when there are no accounts.
pub fn main_currency(accounts: &[BankAccount]) -> String {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for a in accounts.iter().filter(|a| !a.hidden) {
        *counts.entry(a.currency.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        .map(|(c, _)| c.to_string())
        .unwrap_or_else(|| "USD".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{AccountKind, CategorizedBy};

    const NOV: Date = Date::constant(2026, 11, 1);

    fn checking() -> BankAccount {
        BankAccount::manual("Checking", AccountKind::Cash, "USD")
    }

    fn spend(account: &BankAccount, day: i8, amount: i64, category: Option<&MoneyCategory>) -> Txn {
        let mut t = Txn::new(account.id, "k", Date::constant(2026, 11, day), amount, "USD");
        if let Some(c) = category {
            t.category = Some(c.id);
            t.categorized_by = CategorizedBy::Person;
        }
        t
    }

    fn category(name: &str, kind: CategoryKind) -> MoneyCategory {
        MoneyCategory::new(name, "", kind)
    }

    #[test]
    fn a_transfer_is_neither_spending_nor_income() {
        let account = checking();
        let transfer = category("Transfer", CategoryKind::Transfer);
        let txns = [
            spend(&account, 3, -50_000, Some(&transfer)),
            spend(&account, 4, 50_000, Some(&transfer)),
        ];
        let month = month_summary(NOV, "USD", &txns, &[transfer], &[account]);
        assert_eq!((month.income, month.spending, month.uncategorized), (0, 0, 0));
        assert!(month.categories.is_empty());
    }

    #[test]
    fn a_refund_takes_from_its_categorys_spending() {
        let account = checking();
        let clothes = category("Clothing", CategoryKind::Expense);
        let txns =
            [spend(&account, 3, -8_000, Some(&clothes)), spend(&account, 9, 3_000, Some(&clothes))];
        let month = month_summary(NOV, "USD", &txns, std::slice::from_ref(&clothes), &[account]);
        assert_eq!(month.spending, 5_000);
        assert_eq!(month.income, 0, "a refund is not income");
        assert_eq!(month.categories.len(), 1);
        assert_eq!(month.categories[0].spent, 5_000);
        assert_eq!(month.categories[0].count, 2);
    }

    #[test]
    fn an_income_category_counts_its_sign_as_it_comes() {
        let account = checking();
        let pay = category("Paycheck", CategoryKind::Income);
        let txns =
            [spend(&account, 1, 300_000, Some(&pay)), spend(&account, 2, -10_000, Some(&pay))];
        let month = month_summary(NOV, "USD", &txns, &[pay], &[account]);
        assert_eq!((month.income, month.spending, month.net), (290_000, 0, 290_000));
    }

    #[test]
    fn no_category_counts_by_direction() {
        let account = checking();
        let txns = [spend(&account, 1, -2_500, None), spend(&account, 2, 1_000, None)];
        let month = month_summary(NOV, "USD", &txns, &[], &[account]);
        assert_eq!((month.income, month.spending), (1_000, 2_500));
        assert_eq!(month.uncategorized, 2);
        assert_eq!(month.categories.len(), 1);
        assert_eq!(month.categories[0].category_id, None);
        assert_eq!(month.categories[0].spent, 2_500);
    }

    #[test]
    fn a_category_that_no_longer_exists_counts_as_none() {
        let account = checking();
        let gone = category("Deleted", CategoryKind::Transfer);
        let txns = [spend(&account, 1, -4_000, Some(&gone))];
        // `gone` is not among the categories handed over.
        let month = month_summary(NOV, "USD", &txns, &[], &[account]);
        assert_eq!(month.spending, 4_000, "not a transfer any more");
        assert_eq!(month.uncategorized, 1);
        assert_eq!(month.categories[0].category_id, None);
    }

    #[test]
    fn other_currencies_are_named_and_left_out() {
        let account = checking();
        let mut yen = spend(&account, 5, -150_000, None);
        yen.currency = "JPY".into();
        let mut euro = spend(&account, 6, -2_000, None);
        euro.currency = "eur".into();
        let txns = [yen, euro, spend(&account, 7, -1_000, None)];
        let month = month_summary(NOV, "USD", &txns, &[], &[account]);
        assert_eq!(month.spending, 1_000);
        assert_eq!(month.other_currencies, ["EUR", "JPY"]);
    }

    #[test]
    fn hidden_accounts_and_excluded_transactions_do_not_count() {
        let shown = checking();
        let mut hidden = BankAccount::manual("Old", AccountKind::Cash, "USD");
        hidden.hidden = true;
        let mut excluded = spend(&shown, 2, -9_000, None);
        excluded.excluded = true;
        let txns = [spend(&hidden, 1, -7_000, None), excluded, spend(&shown, 3, -1_000, None)];
        let month = month_summary(NOV, "USD", &txns, &[], &[shown, hidden]);
        assert_eq!(month.spending, 1_000);
        assert_eq!(month.uncategorized, 1);
    }

    #[test]
    fn a_transaction_in_an_account_not_given_does_not_count() {
        let given = checking();
        let elsewhere = checking();
        let txns = [spend(&elsewhere, 1, -7_000, None)];
        let month = month_summary(NOV, "USD", &txns, &[], &[given]);
        assert_eq!(month.spending, 0);
    }

    #[test]
    fn only_the_asked_month_counts() {
        let account = checking();
        let mut october = spend(&account, 1, -5_000, None);
        october.date = Date::constant(2026, 10, 31);
        let txns = [october, spend(&account, 30, -1_000, None)];
        let month = month_summary(Date::constant(2026, 11, 17), "USD", &txns, &[], &[account]);
        assert_eq!(month.month, "2026-11");
        assert_eq!(month.spending, 1_000);
    }

    #[test]
    fn pending_transactions_count() {
        let account = checking();
        let mut t = spend(&account, 1, -1_234, None);
        t.pending = true;
        let month = month_summary(NOV, "USD", &[t], &[], &[account]);
        assert_eq!(month.spending, 1_234);
    }

    #[test]
    fn categories_list_most_spent_first_then_budgeted_ones_in_order() {
        let account = checking();
        let mut groceries = category("Groceries", CategoryKind::Expense);
        groceries.sort_order = 1;
        groceries.budget = Some(40_000);
        let mut coffee = category("Coffee", CategoryKind::Expense);
        coffee.sort_order = 2;
        let mut travel = category("Travel", CategoryKind::Expense);
        travel.sort_order = 3;
        travel.budget = Some(10_000);
        let mut gym = category("Gym", CategoryKind::Expense);
        gym.sort_order = 0;
        gym.budget = Some(5_000);
        let mut archived = category("Old", CategoryKind::Expense);
        archived.archived = true;
        archived.budget = Some(99_999);
        let unbudgeted_idle = category("Idle", CategoryKind::Expense);
        let txns = [
            spend(&account, 1, -1_500, Some(&coffee)),
            spend(&account, 2, -20_000, Some(&groceries)),
            spend(&account, 3, -3_000, None),
        ];
        let categories = [
            groceries.clone(),
            coffee.clone(),
            travel.clone(),
            gym.clone(),
            archived,
            unbudgeted_idle,
        ];
        let month = month_summary(NOV, "USD", &txns, &categories, &[account]);
        let order: Vec<_> = month.categories.iter().map(|c| c.category_id).collect();
        assert_eq!(
            order,
            [Some(groceries.id), None, Some(coffee.id), Some(gym.id), Some(travel.id)],
            "spent first by amount, then budgeted-and-idle by category order; archived and \
             unbudgeted idle ones left out"
        );
        assert_eq!(month.budgeted, 55_000);
        assert_eq!(month.categories[0].budget, Some(40_000));
    }

    #[test]
    fn cash_flow_follows_the_same_rules_month_by_month() {
        let account = checking();
        let transfer = category("Transfer", CategoryKind::Transfer);
        let mut sep = spend(&account, 1, -1_000, None);
        sep.date = Date::constant(2026, 9, 10);
        let mut oct_in = spend(&account, 1, 5_000, None);
        oct_in.date = Date::constant(2026, 10, 2);
        let mut oct_moved = spend(&account, 1, -9_000, Some(&transfer));
        oct_moved.date = Date::constant(2026, 10, 3);
        let mut aug = spend(&account, 1, -7_000, None);
        aug.date = Date::constant(2026, 8, 3);
        let months = [Date::constant(2026, 9, 1), Date::constant(2026, 10, 1), NOV];
        let flow =
            cash_flow(&months, "USD", &[sep, oct_in, oct_moved, aug], &[transfer], &[account]);
        assert_eq!(
            flow,
            [
                CashFlowMonth { month: "2026-09".into(), income: 0, spending: 1_000 },
                CashFlowMonth { month: "2026-10".into(), income: 5_000, spending: 0 },
                CashFlowMonth { month: "2026-11".into(), income: 0, spending: 0 },
            ]
        );
    }

    fn snap(account: &BankAccount, day: i8, balance: i64) -> BalanceSnapshot {
        BalanceSnapshot {
            account_id: account.id,
            day: Date::constant(2026, 11, day),
            balance,
            available: None,
        }
    }

    #[test]
    fn net_worth_carries_each_account_forward_and_subtracts_what_is_owed() {
        let mut cash = checking();
        cash.balance = Some(2_000);
        let mut card = BankAccount::manual("Card", AccountKind::Credit, "USD");
        card.balance = Some(400);
        let snaps = [snap(&cash, 1, 1_000), snap(&card, 2, 300), snap(&cash, 3, 1_500)];
        let to = Date::constant(2026, 11, 5);
        let points = net_worth(NOV, to, "USD", &[cash, card], &snaps);
        let d = |day| Date::constant(2026, 11, day);
        assert_eq!(
            points,
            [
                NetWorthPoint { day: d(1), assets: 1_000, liabilities: 0 },
                NetWorthPoint { day: d(2), assets: 1_000, liabilities: 300 },
                NetWorthPoint { day: d(3), assets: 1_500, liabilities: 300 },
                NetWorthPoint { day: d(5), assets: 2_000, liabilities: 400 },
            ]
        );
    }

    #[test]
    fn net_worth_starts_from_balances_known_before_the_range() {
        let cash = checking();
        let mut before = snap(&cash, 1, 7_000);
        before.day = Date::constant(2026, 10, 15);
        let points = net_worth(
            NOV,
            Date::constant(2026, 11, 3),
            "USD",
            std::slice::from_ref(&cash),
            &[before],
        );
        assert_eq!(points.len(), 2);
        assert_eq!(points[0], NetWorthPoint { day: NOV, assets: 7_000, liabilities: 0 });
        assert_eq!(points[1].assets, 7_000, "carried to the end, with no current balance known");
    }

    #[test]
    fn net_worth_leaves_out_hidden_accounts_and_other_currencies() {
        let mut cash = checking();
        cash.balance = Some(1_000);
        let mut hidden = checking();
        hidden.hidden = true;
        hidden.balance = Some(50_000);
        let mut euros = BankAccount::manual("Euro", AccountKind::Cash, "EUR");
        euros.balance = Some(70_000);
        let snaps = [snap(&hidden, 2, 50_000), snap(&euros, 2, 70_000)];
        let points =
            net_worth(NOV, Date::constant(2026, 11, 3), "USD", &[cash, hidden, euros], &snaps);
        assert_eq!(
            points,
            [NetWorthPoint { day: Date::constant(2026, 11, 3), assets: 1_000, liabilities: 0 }]
        );
    }

    #[test]
    fn a_backwards_range_has_no_net_worth() {
        assert!(net_worth(NOV, Date::constant(2026, 10, 1), "USD", &[checking()], &[]).is_empty());
    }

    #[test]
    fn the_main_currency_is_the_most_common_visible_one() {
        let mut hidden = BankAccount::manual("a", AccountKind::Cash, "EUR");
        hidden.hidden = true;
        let accounts = [
            BankAccount::manual("b", AccountKind::Cash, "GBP"),
            BankAccount::manual("c", AccountKind::Cash, "GBP"),
            BankAccount::manual("d", AccountKind::Cash, "USD"),
            hidden.clone(),
            hidden.clone(),
            hidden,
        ];
        assert_eq!(main_currency(&accounts), "GBP");
        assert_eq!(main_currency(&[]), "USD");
    }
}
