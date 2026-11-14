//! The Money half of the conformance suite: links and their state, accounts
//! and balances, transactions, categories and rules, and the cascades.
//!
//! One check per rule in [`crate::store::money`]'s module doc, at least, and
//! a round trip of every field of every record. What it cannot check from
//! here is the first of those rules -- that nothing sealed sits in a clear
//! column -- because the trait offers no way to read a raw row, and inventing
//! one for a test would be a door the running app does not need. That rule is
//! checked where the rows are: the SQL backend's own tests tie each record's
//! clear columns to the schema and look for recognisable values in them.

use super::*;
use crate::id::{BankAccountId, BankLinkId, MoneyCategoryId, MoneyRuleId, TxnId};
use crate::money::{
    AccountKind, BalanceSnapshot, BankAccount, BankLink, CategorizedBy, CategoryKind, LinkProvider,
    LinkState, MoneyCategory, MoneyRule, PlaidEnvironment, Txn, TxnOrigin,
};
use crate::store::money::MoneyStore;
use jiff::civil::{Date, date};
use jiff::{SignedDuration, Timestamp};

/// The Money half of the suite. Called by [`run_all`] when the backend has a
/// [`MoneyStore`](crate::store::money::MoneyStore); public so a backend under
/// construction can run it alone.
///
/// The store must hold no money records on entry; it is left with none on
/// success.
pub fn run_money_suite(store: &dyn JournalStore) {
    eprintln!("--- money conformance suite ---");
    let money = store.money().expect("run_money_suite needs a money store");

    money_starts_empty(money);
    missing_money_records_are_not_found(money);
    a_link_round_trips_every_field(money);
    link_state_round_trips_and_says_what_is_due(money);
    an_account_round_trips_every_field(money);
    a_balance_is_one_per_account_per_day(money);
    a_transaction_round_trips_every_field(money);
    a_batch_is_refused_whole_when_a_key_is_held_by_another_id(money);
    rewriting_a_transaction_is_an_update_that_moves_its_day(money);
    transactions_come_back_newest_first_within_the_range(money);
    a_long_list_of_ids_is_one_call(money);
    deleting_an_account_takes_its_transactions_and_balances(money);
    deleting_a_link_takes_its_accounts_and_everything_under_them(money);
    deleting_a_link_keeping_history_detaches_its_accounts(money);
    deleting_a_category_touches_nothing_else(money);
    deleting_a_rule_touches_nothing_else(money);
    unicode_survives_a_money_round_trip(money);

    money_cleanup(money);
    eprintln!("--- money suite passed ---");
}

/// Every timestamp in this suite is whole seconds from this one, so a time
/// that comes back from a clear column (whole microseconds) compares equal
/// to the one that went in, and "an hour ago" means the same thing on every
/// run.
const NOW: i64 = 1_800_000_000;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("a representable time")
}

fn now() -> Timestamp {
    at(NOW)
}

fn hours(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n)
}

fn day(year: i16, month: i8, day: i8) -> Date {
    date(year, month, day)
}

/// Wide enough to hold every day this suite writes, so "every balance" is
/// one call.
fn every_balance(money: &dyn MoneyStore) -> Vec<BalanceSnapshot> {
    money.balances(day(1900, 1, 1), day(2999, 12, 31)).expect("balances")
}

fn every_txn(money: &dyn MoneyStore) -> Vec<Txn> {
    money.txns(&[], None, None).expect("txns")
}

fn plaid(item: &str, environment: PlaidEnvironment) -> LinkProvider {
    LinkProvider::Plaid { item_id: item.into(), institution_id: "ins_109508".into(), environment }
}

/// A Plaid link, stored, linked at `created`.
fn linked_at(money: &dyn MoneyStore, name: &str, created: Timestamp) -> BankLink {
    let mut link = BankLink::new(plaid(&format!("item-{name}"), PlaidEnvironment::Sandbox), name);
    link.created_at = created;
    link.updated_at = created;
    money.put_link(&link).expect("put_link");
    link
}

fn linked(money: &dyn MoneyStore, name: &str) -> BankLink {
    linked_at(money, name, now())
}

/// An account kept by hand, stored.
fn kept(money: &dyn MoneyStore, name: &str) -> BankAccount {
    let mut account = BankAccount::manual(name, AccountKind::Cash, "USD");
    account.created_at = now();
    account.updated_at = now();
    money.put_account(&account).expect("put_account");
    account
}

/// An account `link` brought, stored.
fn brought(money: &dyn MoneyStore, link: &BankLink, name: &str) -> BankAccount {
    let mut account = BankAccount::manual(name, AccountKind::Cash, "USD");
    account.link_id = Some(link.id);
    account.provider_account_id = format!("plaid-account-{name}");
    account.official_name = format!("{name}, as the bank calls it");
    account.mask = "0000".into();
    account.balance = Some(12_345);
    account.created_at = now();
    account.updated_at = now();
    money.put_account(&account).expect("put_account");
    account
}

/// A transaction with the bank's half filled in, not stored.
fn txn(account: &BankAccount, key: &str, on: Date, amount: i64) -> Txn {
    let mut txn = Txn::new(account.id, key, on, amount, "USD");
    txn.payee = format!("Payee {key}");
    txn.created_at = now();
    txn.updated_at = now();
    txn
}

fn put(money: &dyn MoneyStore, txns: &[&Txn]) {
    let owned: Vec<Txn> = txns.iter().map(|t| (*t).clone()).collect();
    money.put_txns(&owned).expect("put_txns");
}

fn snapshot(account: &BankAccount, on: Date, balance: i64) -> BalanceSnapshot {
    BalanceSnapshot { account_id: account.id, day: on, balance, available: None }
}

fn ids_of(txns: &[Txn]) -> Vec<TxnId> {
    txns.iter().map(|t| t.id).collect()
}

/// Ids in a fixed order, for comparing sets the store answers in no
/// particular order.
fn sorted<T: Ord>(mut ids: Vec<T>) -> Vec<T> {
    ids.sort();
    ids
}

fn money_starts_empty(money: &dyn MoneyStore) {
    assert!(money.list_links().unwrap().is_empty(), "a fresh store must link no banks");
    assert!(money.list_accounts().unwrap().is_empty(), "a fresh store must hold no accounts");
    assert!(every_txn(money).is_empty(), "a fresh store must hold no transactions");
    assert!(every_balance(money).is_empty(), "a fresh store must hold no balances");
    assert!(money.list_categories().unwrap().is_empty(), "a fresh store must hold no categories");
    assert!(money.list_rules().unwrap().is_empty(), "a fresh store must hold no rules");
    assert!(money.due_links(now()).unwrap().is_empty());
    assert_eq!(money.next_link_due().unwrap(), None);
}

fn money_cleanup(money: &dyn MoneyStore) {
    for link in money.list_links().expect("list_links") {
        money.delete_link(link.id, false).expect("delete_link");
    }
    for account in money.list_accounts().expect("list_accounts") {
        money.delete_account(account.id).expect("delete_account");
    }
    for category in money.list_categories().expect("list_categories") {
        money.delete_category(category.id).expect("delete_category");
    }
    for rule in money.list_rules().expect("list_rules") {
        money.delete_rule(rule.id).expect("delete_rule");
    }
    assert!(money.list_links().unwrap().is_empty(), "cleanup left links behind");
    assert!(money.list_accounts().unwrap().is_empty(), "cleanup left accounts behind");
    assert!(every_txn(money).is_empty(), "cleanup left transactions behind");
    assert!(every_balance(money).is_empty(), "cleanup left balances behind");
    assert!(money.list_categories().unwrap().is_empty(), "cleanup left categories behind");
    assert!(money.list_rules().unwrap().is_empty(), "cleanup left rules behind");
    assert_eq!(money.next_link_due().unwrap(), None, "cleanup left link state behind");
}

fn missing_money_records_are_not_found(money: &dyn MoneyStore) {
    super::assert_not_found(money.get_link(BankLinkId::new()));
    super::assert_not_found(money.get_account(BankAccountId::new()));
    super::assert_not_found(money.get_txn(TxnId::new()));
    super::assert_not_found(money.get_category(MoneyCategoryId::new()));
    super::assert_not_found(money.get_rule(MoneyRuleId::new()));
    assert!(money.link_state(BankLinkId::new()).unwrap().is_none(), "nobody has synced it");
    assert!(money.txn_by_key(BankAccountId::new(), "no such key").unwrap().is_none());
    assert!(money.txn_keys(BankAccountId::new()).unwrap().is_empty());
    assert!(money.get_txns(&[TxnId::new()]).unwrap().is_empty(), "missing ids are skipped");
    assert!(money.get_txns(&[]).unwrap().is_empty());
    assert!(money.txns(&[BankAccountId::new()], None, None).unwrap().is_empty());

    // Deleting what is not there is not an error: a disconnect retried, or
    // a list of ids that was right a moment ago, must not stop on it.
    money.delete_txns(&[TxnId::new()]).expect("delete_txns ignores unknown ids");
    money.delete_txns(&[]).expect("delete_txns of nothing");
    money.delete_link(BankLinkId::new(), false).expect("delete_link of nothing");
    money.delete_link(BankLinkId::new(), true).expect("delete_link of nothing, keeping history");
    money.delete_account(BankAccountId::new()).expect("delete_account of nothing");
    money.delete_category(MoneyCategoryId::new()).expect("delete_category of nothing");
    money.delete_rule(MoneyRuleId::new()).expect("delete_rule of nothing");
    money.put_txns(&[]).expect("an empty batch is nothing to write");
}

fn a_link_round_trips_every_field(money: &dyn MoneyStore) {
    let mut link = BankLink::new(
        LinkProvider::Plaid {
            item_id: "eVBnVMp7zdTJLkRNr33Rs6zr7KNJqBFL9DrE6".into(),
            institution_id: "ins_109508".into(),
            environment: PlaidEnvironment::Sandbox,
        },
        "First Platypus Bank",
    );
    link.paused = true;
    link.sort_order = 3;
    link.created_at = at(NOW - 86_400);
    link.updated_at = now();
    money.put_link(&link).expect("put_link");

    assert_eq!(money.get_link(link.id).unwrap(), link, "a link must survive unchanged");
    assert_eq!(money.list_links().unwrap(), vec![link.clone()]);

    let production = BankLink::new(
        plaid("item-production", PlaidEnvironment::Production),
        "Second Platypus Bank",
    );
    money.put_link(&production).unwrap();
    assert_eq!(money.get_link(production.id).unwrap(), production);
    assert_eq!(money.list_links().unwrap().len(), 2);

    // Idempotent: writing the same id twice updates rather than duplicates.
    link.name = "Platypus (renamed)".into();
    link.paused = false;
    link.sort_order = -1;
    money.put_link(&link).expect("put_link again");
    assert_eq!(money.list_links().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(money.get_link(link.id).unwrap(), link);

    money.delete_link(link.id, false).unwrap();
    money.delete_link(production.id, false).unwrap();
    super::assert_not_found(money.get_link(link.id));
    assert!(money.list_links().unwrap().is_empty());
}

fn link_state_round_trips_and_says_what_is_due(money: &dyn MoneyStore) {
    // Two links nobody has synced, linked a minute apart, and three that
    // have been: one due an hour ago, one two hours ago, one in an hour.
    let never = linked_at(money, "never-synced", at(NOW - 120));
    let never_too = linked_at(money, "never-synced-too", at(NOW - 60));
    let overdue = linked(money, "overdue");
    let very_overdue = linked(money, "very-overdue");
    let later = linked(money, "not-yet");

    assert!(money.link_state(never.id).unwrap().is_none(), "nobody has synced it yet");
    assert_eq!(money.next_link_due().unwrap(), None, "no state anywhere: nothing to sleep until");

    let state = |link: &BankLink, next: Timestamp| {
        let mut s = LinkState::new(link.id);
        s.next_at = next;
        s
    };
    let mut full = state(&overdue, now() - hours(1));
    full.cursor = "CAESJXZXaHF0b2RlY2tDQnlFcU9xS0VLVnFkSE1vZ1NnUhoMCLyO8+cGEJjZ1ZcC".into();
    full.last_ok = Some(now() - hours(9));
    full.last_error = Some("The bank did not answer in time.".into());
    full.needs_login = true;
    full.failures = 3;
    full.historical_done = true;
    money.put_link_state(&full).expect("put_link_state");
    assert_eq!(money.link_state(overdue.id).unwrap(), Some(full.clone()));

    money.put_link_state(&state(&very_overdue, now() - hours(2))).unwrap();
    money.put_link_state(&state(&later, now() + hours(1))).unwrap();

    // The links nobody has synced come first, oldest linked first -- they
    // are wanted at once -- and then the rest, soonest first. One not due
    // yet is left out.
    assert_eq!(
        money.due_links(now()).unwrap(),
        vec![never.id, never_too.id, very_overdue.id, overdue.id],
        "never synced (oldest first), then soonest first"
    );
    assert_eq!(money.next_link_due().unwrap(), Some(now() - hours(2)));

    // Exactly at its time counts as due.
    assert_eq!(
        money.due_links(now() + hours(1)).unwrap(),
        vec![never.id, never_too.id, very_overdue.id, overdue.id, later.id]
    );

    // A paused link is still answered: `paused` is sealed, and skipping it
    // is the caller's.
    let mut paused = never_too.clone();
    paused.paused = true;
    money.put_link(&paused).unwrap();
    assert!(money.due_links(now()).unwrap().contains(&never_too.id));

    // Replacing state moves a link: pushed out, it stops being due.
    full.next_at = now() + hours(3);
    full.needs_login = false;
    full.last_error = None;
    money.put_link_state(&full).unwrap();
    assert_eq!(money.link_state(overdue.id).unwrap(), Some(full));
    assert_eq!(money.due_links(now()).unwrap(), vec![never.id, never_too.id, very_overdue.id]);

    // Syncing a link nobody had synced gives it a time, and it too waits.
    money.put_link_state(&state(&never, now() + hours(2))).unwrap();
    money.put_link_state(&state(&never_too, now() + hours(4))).unwrap();
    assert_eq!(money.due_links(now()).unwrap(), vec![very_overdue.id]);
    assert_eq!(money.next_link_due().unwrap(), Some(now() - hours(2)));
    money.put_link_state(&state(&very_overdue, now() + hours(6))).unwrap();
    assert!(money.due_links(now()).unwrap().is_empty());
    assert_eq!(money.next_link_due().unwrap(), Some(now() + hours(1)), "the soonest left");

    for link in [&never, &never_too, &overdue, &very_overdue, &later] {
        money.delete_link(link.id, false).unwrap();
    }
    assert!(money.link_state(overdue.id).unwrap().is_none(), "deleting a link takes its state");
    assert_eq!(money.next_link_due().unwrap(), None, "and nothing is left to wait for");
    assert!(money.due_links(now() + hours(24)).unwrap().is_empty());
}

fn an_account_round_trips_every_field(money: &dyn MoneyStore) {
    let link = linked(money, "accounts");
    let mut card = BankAccount::manual("Everyday card", AccountKind::Credit, "USD");
    card.link_id = Some(link.id);
    card.provider_account_id = "BxBXxLj1m4HMXBm9WZZmCWVbPjX16EHwv99vp".into();
    card.official_name = "Plaid Diamond 12.5% APR Interest Credit Card".into();
    card.mask = "3333".into();
    card.subtype = "credit card".into();
    card.balance = Some(41_000);
    card.available = Some(159_000);
    card.limit = Some(200_000);
    card.balance_at = Some(now() - hours(3));
    card.hidden = true;
    card.sort_order = 7;
    card.created_at = at(NOW - 86_400);
    card.updated_at = now();
    money.put_account(&card).expect("put_account");

    assert_eq!(money.get_account(card.id).unwrap(), card, "an account must survive unchanged");
    assert_eq!(money.list_accounts().unwrap(), vec![card.clone()]);

    // And one kept by hand, with every optional field empty.
    let mut jar = BankAccount::manual("Cash jar", AccountKind::Other, "EUR");
    jar.created_at = now();
    jar.updated_at = now();
    money.put_account(&jar).unwrap();
    let back = money.get_account(jar.id).unwrap();
    assert_eq!(back, jar);
    assert!(back.is_manual());
    assert_eq!(money.list_accounts().unwrap().len(), 2);

    // Every kind survives.
    for kind in [AccountKind::Cash, AccountKind::Loan, AccountKind::Investment] {
        jar.kind = kind;
        money.put_account(&jar).unwrap();
        assert_eq!(money.get_account(jar.id).unwrap().kind, kind);
    }

    // Idempotent, and an emptied option comes back empty.
    card.name = "Card".into();
    card.balance = None;
    card.balance_at = None;
    card.hidden = false;
    money.put_account(&card).expect("put_account again");
    assert_eq!(money.list_accounts().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(money.get_account(card.id).unwrap(), card);

    money.delete_account(card.id).unwrap();
    money.delete_account(jar.id).unwrap();
    money.delete_link(link.id, false).unwrap();
    super::assert_not_found(money.get_account(card.id));
    assert!(money.list_accounts().unwrap().is_empty());
}

fn a_balance_is_one_per_account_per_day(money: &dyn MoneyStore) {
    let a = kept(money, "balances-a");
    let b = kept(money, "balances-b");
    let (oct1, oct2, oct3) = (day(2026, 10, 1), day(2026, 10, 2), day(2026, 10, 3));

    let mut a1 = snapshot(&a, oct1, 100_000);
    a1.available = Some(95_000);
    let b1 = snapshot(&b, oct1, -2_500);
    let a2 = snapshot(&a, oct2, 101_000);
    let a3 = snapshot(&a, oct3, 102_000);
    for s in [&a2, &b1, &a3, &a1] {
        money.put_balance(s).expect("put_balance");
    }

    // By day, then account id -- whichever account's id sorts first.
    let mut first_day = vec![a1.clone(), b1.clone()];
    first_day.sort_by_key(|s| s.account_id);
    let mut expected = first_day.clone();
    expected.push(a2.clone());
    assert_eq!(money.balances(oct1, oct2).unwrap(), expected, "inclusive at both ends");
    assert_eq!(money.balances(oct2, oct2).unwrap(), vec![a2.clone()]);
    assert_eq!(money.balances(oct3, day(2026, 12, 31)).unwrap(), vec![a3.clone()]);
    assert!(money.balances(day(2026, 11, 1), day(2026, 11, 30)).unwrap().is_empty());
    assert!(money.balances(oct3, oct1).unwrap().is_empty(), "a range ending before it starts");

    // A second snapshot the same day replaces the first.
    let replaced = snapshot(&a, oct1, 99_000);
    money.put_balance(&replaced).unwrap();
    let mut first_day = vec![replaced.clone(), b1.clone()];
    first_day.sort_by_key(|s| s.account_id);
    assert_eq!(money.balances(oct1, oct1).unwrap(), first_day, "one per account per day");
    assert_eq!(every_balance(money).len(), 4);

    money.delete_account(a.id).unwrap();
    money.delete_account(b.id).unwrap();
    assert!(every_balance(money).is_empty());
}

fn a_transaction_round_trips_every_field(money: &dyn MoneyStore) {
    let account = kept(money, "round-trip");
    let category = MoneyCategory::new("Eating out", "Food", CategoryKind::Expense);
    money.put_category(&category).unwrap();
    let rule = MoneyRule::new("SAMS CAFE");
    money.put_rule(&rule).unwrap();

    let key = "9f86d081884c7d659a2feaa0c55ad015";
    let mut full = Txn::new(account.id, key, day(2026, 10, 14), -1250, "USD");
    full.provider_id = "lPNjeW1nR6CDn5okmGQ6hEpMo4lLNoSrzqDje".into();
    full.payee = "Sam's Cafe".into();
    full.description = "SQ *SAMS CAFE 10/14 PURCHASE".into();
    full.pending = true;
    full.bank_category = "FOOD_AND_DRINK/FOOD_AND_DRINK_COFFEE".into();
    full.origin = TxnOrigin::Import;
    full.category = Some(category.id);
    full.categorized_by = CategorizedBy::Rule { rule_id: rule.id };
    full.payee_override = "Sam's".into();
    full.note = "with Alex, before the train".into();
    full.excluded = true;
    full.created_at = at(NOW - 3_600);
    full.updated_at = now();
    put(money, &[&full]);

    assert_eq!(money.get_txn(full.id).unwrap(), full, "a transaction must survive unchanged");
    assert_eq!(money.get_txns(&[full.id]).unwrap(), vec![full.clone()]);
    assert_eq!(money.txn_by_key(account.id, &full.key).unwrap(), Some(full.clone()));
    assert_eq!(money.txns(&[account.id], None, None).unwrap(), vec![full.clone()]);
    assert_eq!(money.txn_keys(account.id).unwrap(), vec![(full.key.clone(), full.id)]);

    // Every other way a category is decided, and every other origin.
    let others = [
        (CategorizedBy::Nobody, TxnOrigin::Bank, None),
        (CategorizedBy::Bank, TxnOrigin::Manual, Some(category.id)),
        (CategorizedBy::Person, TxnOrigin::Bank, Some(category.id)),
    ];
    for (n, (by, origin, chosen)) in others.into_iter().enumerate() {
        let mut t = txn(&account, &format!("k-variant-{n}"), day(2026, 10, 15), 5_000);
        t.categorized_by = by;
        t.origin = origin;
        t.category = chosen;
        put(money, &[&t]);
        assert_eq!(money.get_txn(t.id).unwrap(), t);
    }
    assert_eq!(money.txn_keys(account.id).unwrap().len(), 4);

    money.delete_account(account.id).unwrap();
    money.delete_category(category.id).unwrap();
    money.delete_rule(rule.id).unwrap();
    super::assert_not_found(money.get_txn(full.id));
}

fn a_batch_is_refused_whole_when_a_key_is_held_by_another_id(money: &dyn MoneyStore) {
    let account = kept(money, "unique");
    let other_account = kept(money, "unique-elsewhere");
    let oct = day(2026, 10, 20);

    let held = txn(&account, "k-held", oct, -100);
    put(money, &[&held]);

    // A new transaction, then one under a different id claiming the held
    // key: refused as invalid, and the new one before it is not written
    // either.
    let fresh = txn(&account, "k-fresh", oct, -200);
    let thief = txn(&account, "k-held", oct, -300);
    let err = money
        .put_txns(&[fresh.clone(), thief.clone()])
        .expect_err("a key held by another id must be refused");
    assert_eq!(err.code(), "invalid", "refused as invalid, got {err:?}");
    super::assert_not_found(money.get_txn(fresh.id));
    super::assert_not_found(money.get_txn(thief.id));
    assert_eq!(money.get_txn(held.id).unwrap(), held, "the holder is untouched");

    // Nor does an update earlier in a refused batch land.
    let mut changed = held.clone();
    changed.note = "this must not be stored".into();
    assert_eq!(money.put_txns(&[changed, thief.clone()]).unwrap_err().code(), "invalid");
    assert_eq!(money.get_txn(held.id).unwrap(), held, "all or nothing");

    // Two different ids wanting one key that nobody holds yet: the batch
    // disagrees with itself, and none of it is written.
    let one = txn(&account, "k-twin", oct, -1);
    let two = txn(&account, "k-twin", oct, -2);
    assert_eq!(money.put_txns(&[one.clone(), two.clone()]).unwrap_err().code(), "invalid");
    super::assert_not_found(money.get_txn(one.id));
    super::assert_not_found(money.get_txn(two.id));
    assert_eq!(money.txn_keys(account.id).unwrap(), vec![("k-held".to_string(), held.id)]);

    // The pair is the account *and* the key: the same key in another
    // account is another transaction.
    let elsewhere = txn(&other_account, "k-held", oct, -400);
    put(money, &[&elsewhere]);
    assert_eq!(money.txn_by_key(account.id, "k-held").unwrap(), Some(held.clone()));
    assert_eq!(money.txn_by_key(other_account.id, "k-held").unwrap(), Some(elsewhere.clone()));
    assert!(money.txn_by_key(account.id, "k-fresh").unwrap().is_none());

    // And the same id under its own key is an update, not a clash.
    let mut again = held.clone();
    again.amount = -150;
    put(money, &[&again]);
    assert_eq!(money.get_txn(held.id).unwrap(), again);
    assert_eq!(every_txn(money).len(), 2);

    money.delete_account(account.id).unwrap();
    money.delete_account(other_account.id).unwrap();
}

fn rewriting_a_transaction_is_an_update_that_moves_its_day(money: &dyn MoneyStore) {
    let account = kept(money, "rewrite");
    let (oct14, oct15, oct16) = (day(2026, 10, 14), day(2026, 10, 15), day(2026, 10, 16));
    let mut pending = txn(&account, "k-pending", oct14, -1250);
    pending.pending = true;
    put(money, &[&pending]);

    // Posted two days later, as the bank reports it: the same id and key, a
    // new date and amount, and a note the person added meanwhile.
    let mut posted = pending.clone();
    posted.pending = false;
    posted.date = oct16;
    posted.amount = -1300;
    posted.note = "tip included".into();
    posted.updated_at = now() + hours(1);
    put(money, &[&posted]);

    assert_eq!(money.get_txn(posted.id).unwrap(), posted);
    assert_eq!(every_txn(money).len(), 1, "an update, not a second row");
    assert_eq!(money.txn_keys(account.id).unwrap(), vec![("k-pending".to_string(), posted.id)]);
    assert_eq!(
        ids_of(&money.txns(&[], Some(oct15), None).unwrap()),
        vec![posted.id],
        "the day it is listed by moved with it"
    );
    assert!(money.txns(&[], None, Some(oct14)).unwrap().is_empty(), "and left the old day");

    // A new key for the same id is an update too, and frees the old one --
    // which another id may then take.
    let mut rekeyed = posted.clone();
    rekeyed.key = "k-posted".into();
    put(money, &[&rekeyed]);
    assert!(money.txn_by_key(account.id, "k-pending").unwrap().is_none());
    assert_eq!(money.txn_by_key(account.id, "k-posted").unwrap(), Some(rekeyed.clone()));
    let newcomer = txn(&account, "k-pending", oct15, -10);
    put(money, &[&newcomer]);
    assert_eq!(money.txn_by_key(account.id, "k-pending").unwrap(), Some(newcomer));

    money.delete_account(account.id).unwrap();
}

fn transactions_come_back_newest_first_within_the_range(money: &dyn MoneyStore) {
    let a = kept(money, "order-a");
    let b = kept(money, "order-b");
    let (sep30, oct1, oct15) = (day(2026, 9, 30), day(2026, 10, 1), day(2026, 10, 15));
    let (oct31, nov1) = (day(2026, 10, 31), day(2026, 11, 1));

    let early = txn(&a, "k-sep30", sep30, -1);
    let first = txn(&b, "k-oct1", oct1, -2);
    let tie_a = txn(&a, "k-oct15-a", oct15, -3);
    let tie_b = txn(&b, "k-oct15-b", oct15, -4);
    let tie_c = txn(&a, "k-oct15-c", oct15, -5);
    let last = txn(&b, "k-oct31", oct31, -6);
    let newest = txn(&a, "k-nov1", nov1, -7);
    // Stored in an order that is neither by date nor by id, so the answer
    // has to come from the store.
    put(money, &[&tie_b, &early, &newest, &first, &tie_c, &last, &tie_a]);

    let all = every_txn(money);
    let ids = ids_of(&all);
    assert_eq!(ids.len(), 7);
    assert_eq!(ids[0], newest.id);
    assert_eq!(ids[1], last.id);
    assert_eq!(
        sorted(ids[2..5].to_vec()),
        sorted(vec![tie_a.id, tie_b.id, tie_c.id]),
        "the three on one day sit in the middle"
    );
    assert!(ids[2..5].windows(2).all(|w| w[0] > w[1]), "a day's ties break by id, descending");
    assert_eq!(ids[5], first.id);
    assert_eq!(ids[6], early.id);
    assert!(all.windows(2).all(|w| w[0].date >= w[1].date));

    let listed = |accounts: &[BankAccountId], from: Option<Date>, to: Option<Date>| {
        ids_of(&money.txns(accounts, from, to).expect("txns"))
    };
    let of = |keep: &dyn Fn(&Txn) -> bool| -> Vec<TxnId> {
        all.iter().filter(|t| keep(t)).map(|t| t.id).collect()
    };

    // Accounts: both named is everything; one; one twice; one not there.
    assert_eq!(listed(&[a.id, b.id], None, None), ids);
    assert_eq!(listed(&[b.id], None, None), of(&|t| t.account_id == b.id));
    assert_eq!(listed(&[b.id], None, None).len(), 3);
    assert_eq!(listed(&[a.id, a.id], None, None), listed(&[a.id], None, None));
    assert!(listed(&[BankAccountId::new()], None, None).is_empty());

    // The range is inclusive at both ends, and unbounded on a side given
    // `None`.
    assert_eq!(
        listed(&[], Some(oct1), Some(oct31)),
        of(&|t| t.date >= oct1 && t.date <= oct31),
        "both ends inclusive"
    );
    assert_eq!(listed(&[], Some(oct1), Some(oct31)).len(), 5);
    assert_eq!(listed(&[], Some(oct15), None), of(&|t| t.date >= oct15));
    assert_eq!(listed(&[], Some(oct15), None).len(), 5);
    assert_eq!(listed(&[], None, Some(oct1)), vec![first.id, early.id]);
    assert_eq!(listed(&[], Some(oct15), Some(oct15)).len(), 3);
    assert!(listed(&[], Some(oct31), Some(oct1)).is_empty(), "a range ending before it starts");
    assert!(listed(&[], Some(day(2026, 12, 1)), None).is_empty());

    // An account and a range together.
    assert_eq!(
        listed(&[a.id], Some(oct1), Some(oct31)),
        of(&|t| t.account_id == a.id && t.date >= oct1 && t.date <= oct31)
    );
    assert_eq!(listed(&[a.id], Some(oct1), Some(oct31)).len(), 2);

    money.delete_account(a.id).unwrap();
    money.delete_account(b.id).unwrap();
    assert!(every_txn(money).is_empty());
}

fn a_long_list_of_ids_is_one_call(money: &dyn MoneyStore) {
    // More than any one `IN (...)` a backend would want to send, so a
    // backend that chunks has to chunk correctly, and one that does not
    // fails here rather than on somebody's first sync.
    let account = kept(money, "many");
    let many: Vec<Txn> = (0..1_203usize)
        .map(|i| {
            let on = day(2026, 1 + (i % 12) as i8, 1 + (i % 28) as i8);
            txn(&account, &format!("k-many-{i:04}"), on, -(i as i64))
        })
        .collect();
    money.put_txns(&many).expect("put_txns of a long batch");
    let ids: Vec<TxnId> = ids_of(&many);

    assert_eq!(sorted(ids_of(&money.get_txns(&ids).unwrap())), sorted(ids.clone()));
    assert_eq!(money.txn_keys(account.id).unwrap().len(), 1_203);
    let listed = money.txns(&[account.id], None, None).unwrap();
    assert_eq!(listed.len(), 1_203);
    assert!(
        listed.windows(2).all(|w| (w[0].date, w[0].id) > (w[1].date, w[1].id)),
        "newest first, ties by id, all the way down"
    );

    // A list of accounts longer than one `IN (...)` too, the real one last:
    // every transaction once, still newest first.
    let mut accounts: Vec<BankAccountId> = (0..700).map(|_| BankAccountId::new()).collect();
    accounts.push(account.id);
    let across = money.txns(&accounts, None, None).unwrap();
    assert_eq!(ids_of(&across), ids_of(&listed), "a long account list answers the same");

    // A long batch with a clash at the very end is still refused whole.
    let mut refused: Vec<Txn> = (0..600usize)
        .map(|i| txn(&account, &format!("k-late-{i:04}"), day(2026, 6, 1), 1))
        .collect();
    refused.push(txn(&account, "k-many-0000", day(2026, 6, 1), 1));
    assert_eq!(money.put_txns(&refused).unwrap_err().code(), "invalid");
    assert!(money.get_txns(&ids_of(&refused)).unwrap().is_empty(), "none of it written");
    assert_eq!(money.txn_keys(account.id).unwrap().len(), 1_203);

    // Unknown ids among known ones are skipped, by both calls.
    let mut with_unknown = ids.clone();
    with_unknown.push(TxnId::new());
    assert_eq!(money.get_txns(&with_unknown).unwrap().len(), 1_203);
    money.delete_txns(&with_unknown).expect("delete_txns of a long list");
    assert!(every_txn(money).is_empty());
    assert!(money.txn_keys(account.id).unwrap().is_empty());

    money.delete_account(account.id).unwrap();
}

fn deleting_an_account_takes_its_transactions_and_balances(money: &dyn MoneyStore) {
    let doomed = kept(money, "doomed");
    let spared = kept(money, "spared");
    let oct = day(2026, 10, 10);

    let gone = txn(&doomed, "k-shared", oct, -100);
    let also_gone = txn(&doomed, "k-doomed", oct, -200);
    let kept_txn = txn(&spared, "k-shared", oct, -300);
    put(money, &[&gone, &also_gone, &kept_txn]);
    money.put_balance(&snapshot(&doomed, oct, 1_000)).unwrap();
    let spared_balance = snapshot(&spared, oct, 2_000);
    money.put_balance(&spared_balance).unwrap();

    money.delete_account(doomed.id).expect("delete_account");

    super::assert_not_found(money.get_account(doomed.id));
    super::assert_not_found(money.get_txn(gone.id));
    super::assert_not_found(money.get_txn(also_gone.id));
    assert!(money.txns(&[doomed.id], None, None).unwrap().is_empty(), "its transactions go");
    assert!(money.txn_keys(doomed.id).unwrap().is_empty());
    assert!(money.txn_by_key(doomed.id, "k-shared").unwrap().is_none());
    assert_eq!(every_balance(money), vec![spared_balance.clone()], "its balances go");

    // The neighbour is exactly as it was.
    assert_eq!(money.get_account(spared.id).unwrap(), spared);
    assert_eq!(every_txn(money), vec![kept_txn.clone()]);
    assert_eq!(money.txn_by_key(spared.id, "k-shared").unwrap(), Some(kept_txn));

    money.delete_account(spared.id).unwrap();
}

fn deleting_a_link_takes_its_accounts_and_everything_under_them(money: &dyn MoneyStore) {
    let doomed = linked(money, "disconnected");
    let spared = linked(money, "still-connected");
    let checking = brought(money, &doomed, "checking");
    let savings = brought(money, &doomed, "savings");
    let other = brought(money, &spared, "other");
    let by_hand = kept(money, "by-hand");
    let oct = day(2026, 10, 12);

    let mut doomed_state = LinkState::new(doomed.id);
    doomed_state.next_at = now() - hours(5);
    let mut spared_state = LinkState::new(spared.id);
    spared_state.next_at = now() + hours(5);
    money.put_link_state(&doomed_state).unwrap();
    money.put_link_state(&spared_state).unwrap();

    let t1 = txn(&checking, "k-1", oct, -1);
    let t2 = txn(&savings, "k-2", oct, -2);
    let t3 = txn(&other, "k-3", oct, -3);
    let t4 = txn(&by_hand, "k-4", oct, -4);
    put(money, &[&t1, &t2, &t3, &t4]);
    for account in [&checking, &savings, &other, &by_hand] {
        money.put_balance(&snapshot(account, oct, 500)).unwrap();
    }

    money.delete_link(doomed.id, false).expect("delete_link");

    super::assert_not_found(money.get_link(doomed.id));
    assert!(money.link_state(doomed.id).unwrap().is_none(), "its state goes");
    super::assert_not_found(money.get_account(checking.id));
    super::assert_not_found(money.get_account(savings.id));
    super::assert_not_found(money.get_txn(t1.id));
    super::assert_not_found(money.get_txn(t2.id));
    assert!(money.txn_keys(checking.id).unwrap().is_empty());
    assert_eq!(
        sorted(every_balance(money).iter().map(|s| s.account_id).collect()),
        sorted(vec![other.id, by_hand.id]),
        "its accounts' balances go, and only theirs"
    );

    // Everything else is exactly as it was.
    assert_eq!(money.list_links().unwrap(), vec![spared.clone()]);
    assert_eq!(money.link_state(spared.id).unwrap(), Some(spared_state));
    assert_eq!(money.next_link_due().unwrap(), Some(now() + hours(5)));
    assert_eq!(money.get_account(other.id).unwrap(), other);
    assert_eq!(money.get_account(by_hand.id).unwrap(), by_hand);
    assert_eq!(sorted(ids_of(&every_txn(money))), sorted(vec![t3.id, t4.id]));

    money.delete_link(spared.id, false).unwrap();
    super::assert_not_found(money.get_account(other.id));
    money.delete_account(by_hand.id).unwrap();
    assert!(every_txn(money).is_empty());
}

fn deleting_a_link_keeping_history_detaches_its_accounts(money: &dyn MoneyStore) {
    let leaving = linked(money, "leaving");
    let staying = linked(money, "staying");
    let checking = brought(money, &leaving, "kept-checking");
    let savings = brought(money, &leaving, "kept-savings");
    let other = brought(money, &staying, "untouched");
    let (oct1, oct2) = (day(2026, 10, 1), day(2026, 10, 2));

    let mut state = LinkState::new(leaving.id);
    state.cursor = "a cursor nobody needs any more".into();
    state.next_at = now() - hours(1);
    money.put_link_state(&state).unwrap();

    let mut history = txn(&checking, "k-history", oct1, -900);
    history.note = "kept after the bank was disconnected".into();
    history.categorized_by = CategorizedBy::Person;
    let more = txn(&savings, "k-more", oct2, 900);
    put(money, &[&history, &more]);
    let balance = snapshot(&checking, oct1, 40_000);
    money.put_balance(&balance).unwrap();

    money.delete_link(leaving.id, true).expect("delete_link keeping history");

    super::assert_not_found(money.get_link(leaving.id));
    assert!(money.link_state(leaving.id).unwrap().is_none(), "its state goes all the same");
    assert!(!money.due_links(now()).unwrap().contains(&leaving.id));

    // The accounts stay, manual from now on: no link, no provider id, and
    // every other field as it was. (When it was last changed is left to the
    // backend.)
    for account in [&checking, &savings] {
        let back = money.get_account(account.id).expect("the account stays");
        let mut expected = account.clone();
        expected.link_id = None;
        expected.provider_account_id = String::new();
        expected.updated_at = back.updated_at;
        assert_eq!(back, expected, "detached, and otherwise unchanged");
        assert!(back.is_manual());
    }

    // Their history stays, untouched.
    assert_eq!(money.get_txn(history.id).unwrap(), history);
    assert_eq!(money.get_txn(more.id).unwrap(), more);
    assert_eq!(money.txn_by_key(checking.id, "k-history").unwrap(), Some(history.clone()));
    assert_eq!(money.balances(oct1, oct1).unwrap(), vec![balance]);

    // Another link's account is not detached with them.
    assert_eq!(money.get_account(other.id).unwrap(), other);
    assert_eq!(money.list_links().unwrap(), vec![staying.clone()]);
    assert_eq!(money.list_accounts().unwrap().len(), 3);

    // And once detached, deleting the other link leaves them be.
    money.delete_link(staying.id, false).unwrap();
    super::assert_not_found(money.get_account(other.id));
    assert!(money.get_account(checking.id).is_ok(), "a detached account belongs to no link");

    money.delete_account(checking.id).unwrap();
    money.delete_account(savings.id).unwrap();
    assert!(every_txn(money).is_empty());
    assert!(every_balance(money).is_empty());
}

fn deleting_a_category_touches_nothing_else(money: &dyn MoneyStore) {
    let mut groceries = MoneyCategory::new("Groceries", "Food", CategoryKind::Expense);
    groceries.budget = Some(60_000);
    groceries.color = "green".into();
    groceries.archived = true;
    groceries.sort_order = 2;
    groceries.created_at = at(NOW - 86_400);
    groceries.updated_at = now();
    money.put_category(&groceries).expect("put_category");
    assert_eq!(money.get_category(groceries.id).unwrap(), groceries, "unchanged by a round trip");
    assert_eq!(money.list_categories().unwrap(), vec![groceries.clone()]);

    let salary = MoneyCategory::new("Salary", "", CategoryKind::Income);
    let transfer = MoneyCategory::new("Card payment", "Transfers", CategoryKind::Transfer);
    money.put_category(&salary).unwrap();
    money.put_category(&transfer).unwrap();
    assert_eq!(money.get_category(salary.id).unwrap(), salary);
    assert_eq!(money.get_category(transfer.id).unwrap(), transfer);
    assert_eq!(money.list_categories().unwrap().len(), 3);

    // Idempotent, and an emptied budget comes back empty.
    groceries.budget = None;
    groceries.archived = false;
    money.put_category(&groceries).unwrap();
    assert_eq!(money.list_categories().unwrap().len(), 3, "put must not duplicate");
    assert_eq!(money.get_category(groceries.id).unwrap(), groceries);

    // A transaction and a rule naming it.
    let account = kept(money, "categorised");
    let mut shopping = txn(&account, "k-shopping", day(2026, 10, 3), -4_210);
    shopping.category = Some(groceries.id);
    shopping.categorized_by = CategorizedBy::Person;
    put(money, &[&shopping]);
    let mut rule = MoneyRule::new("TRADER JOE");
    rule.category = Some(groceries.id);
    money.put_rule(&rule).unwrap();

    money.delete_category(groceries.id).expect("delete_category");

    super::assert_not_found(money.get_category(groceries.id));
    assert_eq!(
        money.get_txn(shopping.id).unwrap(),
        shopping,
        "a transaction still naming it is the vault's to tidy, not the store's"
    );
    assert_eq!(money.get_rule(rule.id).unwrap(), rule, "and so is a rule");
    assert_eq!(
        sorted(money.list_categories().unwrap().iter().map(|c| c.id).collect()),
        sorted(vec![salary.id, transfer.id])
    );

    money.delete_account(account.id).unwrap();
    money.delete_rule(rule.id).unwrap();
    money.delete_category(salary.id).unwrap();
    money.delete_category(transfer.id).unwrap();
    assert!(money.list_categories().unwrap().is_empty());
}

fn deleting_a_rule_touches_nothing_else(money: &dyn MoneyStore) {
    let account = kept(money, "ruled");
    let category = MoneyCategory::new("Shopping", "", CategoryKind::Expense);
    money.put_category(&category).unwrap();

    let mut rule = MoneyRule::new("AMAZON MKTPL");
    rule.exact = true;
    rule.account_id = Some(account.id);
    rule.min_amount = Some(-50_000);
    rule.max_amount = Some(-100);
    rule.category = Some(category.id);
    rule.rename = "Amazon".into();
    rule.exclude = true;
    rule.sort_order = 5;
    rule.created_at = at(NOW - 86_400);
    rule.updated_at = now();
    money.put_rule(&rule).expect("put_rule");
    assert_eq!(money.get_rule(rule.id).unwrap(), rule, "a rule must survive unchanged");
    assert_eq!(money.list_rules().unwrap(), vec![rule.clone()]);

    let plain = MoneyRule::new("NETFLIX");
    money.put_rule(&plain).unwrap();
    assert_eq!(money.get_rule(plain.id).unwrap(), plain, "and one with nothing optional set");
    assert_eq!(money.list_rules().unwrap().len(), 2);

    // Idempotent, and emptied options come back empty.
    rule.account_id = None;
    rule.min_amount = None;
    rule.max_amount = None;
    rule.exact = false;
    money.put_rule(&rule).unwrap();
    assert_eq!(money.list_rules().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(money.get_rule(rule.id).unwrap(), rule);

    let mut sorted_by_rule = txn(&account, "k-amazon", day(2026, 10, 9), -2_999);
    sorted_by_rule.category = Some(category.id);
    sorted_by_rule.categorized_by = CategorizedBy::Rule { rule_id: rule.id };
    sorted_by_rule.payee_override = "Amazon".into();
    put(money, &[&sorted_by_rule]);

    money.delete_rule(rule.id).expect("delete_rule");

    super::assert_not_found(money.get_rule(rule.id));
    assert_eq!(money.get_txn(sorted_by_rule.id).unwrap(), sorted_by_rule, "what it sorted stays");
    assert_eq!(money.get_category(category.id).unwrap(), category, "and so does its category");
    assert_eq!(money.list_rules().unwrap(), vec![plain.clone()]);

    money.delete_rule(plain.id).unwrap();
    money.delete_account(account.id).unwrap();
    money.delete_category(category.id).unwrap();
    assert!(money.list_rules().unwrap().is_empty());
}

fn unicode_survives_a_money_round_trip(money: &dyn MoneyStore) {
    let link = BankLink::new(
        plaid("item-\u{00e9}t\u{00e9}", PlaidEnvironment::Production),
        "\u{1f3e6} Cr\u{00e9}dit Mutuel \u{2014} \u{4e09}\u{83f1}UFJ",
    );
    money.put_link(&link).unwrap();
    let mut account =
        BankAccount::manual("\u{20ac} Compte courant \u{00e0} vue", AccountKind::Cash, "EUR");
    account.link_id = Some(link.id);
    account.provider_account_id = "\u{30a2}\u{30ab}\u{30a6}\u{30f3}\u{30c8}".into();
    money.put_account(&account).unwrap();
    let (name, group) = ("Caf\u{00e9}s \u{2615}", "\u{98df}\u{3079}\u{7269}");
    let category = MoneyCategory::new(name, group, CategoryKind::Expense);
    money.put_category(&category).unwrap();
    let rule = MoneyRule::new("BOULANGERIE \u{00c9}TIENNE \u{1f950}");
    money.put_rule(&rule).unwrap();
    let mut coffee = txn(&account, "k-unicode", day(2026, 10, 2), -420);
    coffee.payee = "Caf\u{00e9} de Flore".into();
    coffee.description = "CB \u{00ab}CAF\u{00c9} DE FLORE\u{00bb} 02/10 \u{2022} Paris".into();
    coffee.note = "\u{1f600} avec \u{5c71}\u{7530}\u{3055}\u{3093}".into();
    coffee.currency = "EUR".into();
    put(money, &[&coffee]);
    let mut state = LinkState::new(link.id);
    state.last_error =
        Some("La banque n\u{2019}a pas r\u{00e9}pondu \u{2014} r\u{00e9}essayez".into());
    money.put_link_state(&state).unwrap();

    assert_eq!(money.get_link(link.id).unwrap(), link);
    assert_eq!(money.get_account(account.id).unwrap(), account);
    assert_eq!(money.get_category(category.id).unwrap(), category);
    assert_eq!(money.get_rule(rule.id).unwrap(), rule);
    assert_eq!(money.get_txn(coffee.id).unwrap(), coffee);
    assert_eq!(money.link_state(link.id).unwrap(), Some(state));

    money.delete_link(link.id, false).unwrap();
    super::assert_not_found(money.get_account(account.id));
    super::assert_not_found(money.get_txn(coffee.id));
    money.delete_category(category.id).unwrap();
    money.delete_rule(rule.id).unwrap();
}
