//! The rules the vault applies to Money, over a real SQLite backend: a
//! sync never undoing what a person decided, pending transactions handing
//! their person's half to the posted ones, imports that do not double up,
//! rules that know their place, the cascades, and the two doors a Plaid
//! credential leaves by.

mod support;
use support::vault;

use everyday_core::Vault;
use everyday_core::money::{
    AccessToken, AccountKind, BankLink, CategorizedBy, CategoryKind, ImportedTxn, LinkProvider,
    MoneyCategory, MoneyRule, NewTxn, PlaidCredentials, PlaidEnvironment, SyncDelta, SyncOutcome,
    SyncedAccount, SyncedTxn, Txn, TxnPatch, TxnQuery,
};
use jiff::civil::Date;

const TOKEN: &str = "access-sandbox-de3ce8ef-33f8-452c-a685-8671031fc0f6";

fn day(d: i8) -> Date {
    Date::constant(2026, 10, d)
}

/// A link to a sandbox bank, as `start_bank_link` would store one.
fn a_link(vault: &Vault) -> BankLink {
    let link = BankLink::new(
        LinkProvider::Plaid {
            item_id: "item-sandbox-1".into(),
            institution_id: "ins_109508".into(),
            environment: PlaidEnvironment::Sandbox,
        },
        "First Platypus Bank",
    );
    vault.add_bank_link(&link, &AccessToken(TOKEN.into())).unwrap();
    link
}

fn checking() -> SyncedAccount {
    SyncedAccount {
        provider_account_id: "acc-checking".into(),
        name: "Plaid Checking".into(),
        official_name: "Plaid Gold Standard 0% Interest Checking".into(),
        mask: "0000".into(),
        kind: AccountKind::Cash,
        subtype: "checking".into(),
        currency: "usd".into(),
        balance: Some(11_000),
        available: Some(10_000),
        limit: None,
    }
}

fn bought(id: &str, d: i8, amount: i64, payee: &str) -> SyncedTxn {
    SyncedTxn {
        provider_account_id: "acc-checking".into(),
        provider_id: id.into(),
        replaces_pending: None,
        date: day(d),
        amount,
        currency: "USD".into(),
        payee: payee.into(),
        description: String::new(),
        pending: false,
        bank_category: String::new(),
    }
}

fn sync(vault: &Vault, link: &BankLink, upserts: Vec<SyncedTxn>, removed: &[&str]) -> SyncOutcome {
    let delta = SyncDelta {
        accounts: vec![checking()],
        upserts,
        removed: removed.iter().map(|s| s.to_string()).collect(),
    };
    vault.apply_bank_sync(link.id, &delta).unwrap()
}

fn all(vault: &Vault) -> Vec<Txn> {
    vault.transactions(&TxnQuery::default()).unwrap().txns
}

fn only(vault: &Vault) -> Txn {
    let mut txns = all(vault);
    assert_eq!(txns.len(), 1, "{txns:?}");
    txns.remove(0)
}

fn by_payee(vault: &Vault, payee: &str) -> Txn {
    all(vault).into_iter().find(|t| t.payee == payee).unwrap_or_else(|| panic!("no {payee}"))
}

fn category_named(vault: &Vault, name: &str) -> MoneyCategory {
    vault
        .money_categories()
        .unwrap()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no category {name}"))
}

fn line(date: Date, amount: i64, payee: &str, fitid: &str) -> ImportedTxn {
    ImportedTxn {
        date,
        amount,
        payee: payee.into(),
        description: String::new(),
        provider_id: fitid.into(),
    }
}

fn typed(account: &everyday_core::BankAccount, d: i8, amount: i64, payee: &str) -> NewTxn {
    NewTxn {
        account_id: account.id,
        date: day(d),
        amount,
        payee: payee.into(),
        note: String::new(),
        category: None,
    }
}

// ---- sync ----------------------------------------------------------------

#[test]
fn a_persons_category_and_note_survive_a_resync() {
    // The whole reason a transaction is split into halves: the bank
    // rewriting its own description must not take the person's work with it.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let link = a_link(&vault);

    let first = sync(&vault, &link, vec![bought("t1", 3, -1250, "SAMS CAFE")], &[]);
    assert_eq!((first.added, first.new_accounts), (1, 1));
    let txn = only(&vault);
    let coffee = category_named(&vault, "Coffee");
    let patch = TxnPatch {
        category: Some(Some(coffee.id)),
        note: Some("  lunch with Sam ".into()),
        ..Default::default()
    };
    assert_eq!(vault.update_transactions(&[txn.id], &patch).unwrap(), [txn.id]);

    let mut changed = bought("t1", 4, -1300, "SAMS CAFE & BAKERY");
    changed.bank_category = "FOOD_AND_DRINK/FOOD_AND_DRINK_RESTAURANT".into();
    let again = sync(&vault, &link, vec![changed], &[]);
    assert_eq!((again.added, again.updated, again.new_accounts), (0, 1, 0));

    let after = only(&vault);
    assert_eq!(after.id, txn.id);
    assert_eq!((after.amount, after.date), (-1300, day(4)), "the bank's half is the bank's");
    assert_eq!(after.payee, "SAMS CAFE & BAKERY");
    assert_eq!(after.category, Some(coffee.id), "not the restaurant the bank now says");
    assert_eq!(after.categorized_by, CategorizedBy::Person);
    assert_eq!(after.note, "lunch with Sam");
    assert_eq!(after.key, vault.money_key("plaid:t1").unwrap(), "keyed as money_key says");
}

#[test]
fn a_posted_transaction_carries_the_persons_half_from_its_pending_one() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    let mut pending = bought("p1", 3, -4000, "SHELL");
    pending.pending = true;
    sync(&vault, &link, vec![pending], &[]);
    let was = only(&vault);
    assert!(was.pending);
    let patch = TxnPatch {
        payee_override: Some("Shell".into()),
        note: Some("road trip".into()),
        excluded: Some(true),
        ..Default::default()
    };
    vault.update_transactions(&[was.id], &patch).unwrap();

    // Plaid posts it under a new id, naming the pending one, and withdraws
    // the pending one in the same breath.
    let mut posted = bought("t9", 4, -4250, "SHELL OIL 1234");
    posted.replaces_pending = Some("p1".into());
    let outcome = sync(&vault, &link, vec![posted], &["p1"]);
    assert_eq!(
        (outcome.added, outcome.updated, outcome.removed),
        (0, 1, 0),
        "one transaction, changed -- not one gone and one new"
    );

    let now = only(&vault);
    assert_ne!(now.id, was.id);
    assert!(!now.pending);
    assert_eq!(now.amount, -4250);
    assert_eq!(now.payee_override, "Shell");
    assert_eq!(now.note, "road trip");
    assert!(now.excluded);
    assert_eq!(vault.transaction(was.id).unwrap_err().code(), "not_found");
}

#[test]
fn a_pending_transaction_posted_in_the_same_sync_is_not_kept_twice() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    let mut pending = bought("p1", 3, -900, "BAKERY");
    pending.pending = true;
    let mut posted = bought("t1", 4, -900, "BAKERY");
    posted.replaces_pending = Some("p1".into());
    let outcome = sync(&vault, &link, vec![pending, posted], &[]);
    assert_eq!((outcome.added, outcome.updated), (1, 0));
    assert!(!only(&vault).pending);
}

#[test]
fn a_transaction_the_bank_withdraws_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    sync(&vault, &link, vec![bought("t1", 3, -100, "ONE"), bought("t2", 3, -200, "TWO")], &[]);
    assert_eq!(all(&vault).len(), 2);

    let outcome = sync(&vault, &link, vec![], &["t1", "never-seen"]);
    assert_eq!(outcome.removed, 1);
    assert_eq!(only(&vault).payee, "TWO");
}

#[test]
fn a_transaction_both_changed_and_removed_in_one_sync_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    sync(&vault, &link, vec![bought("t1", 3, -100, "ONE")], &[]);
    let outcome = sync(&vault, &link, vec![bought("t1", 3, -150, "ONE")], &["t1"]);
    assert_eq!((outcome.updated, outcome.removed), (0, 1));
    assert!(all(&vault).is_empty());
}

#[test]
fn a_paused_link_gets_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut link = a_link(&vault);
    link.paused = true;
    vault.save_bank_link(&link).unwrap();

    let outcome = sync(&vault, &link, vec![bought("t1", 3, -100, "ONE")], &[]);
    assert_eq!(outcome, SyncOutcome::default());
    assert!(vault.bank_accounts().unwrap().is_empty());
    assert!(all(&vault).is_empty());
}

#[test]
fn a_sync_for_a_link_that_is_not_there_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let err =
        vault.apply_bank_sync(everyday_core::BankLinkId::new(), &SyncDelta::default()).unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
}

#[test]
fn a_sync_rewrites_the_banks_half_of_an_account_and_keeps_the_persons() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    sync(&vault, &link, vec![], &[]);
    let mut account = vault.bank_accounts().unwrap().remove(0);
    assert_eq!(account.name, "Plaid Checking");
    assert_eq!(account.currency, "USD", "upper-cased on the way in");
    assert_eq!(account.link_id, Some(link.id));
    account.name = "Everyday spending".into();
    account.hidden = true;
    account.balance = Some(1); // the bank's: ignored for a linked account
    vault.save_bank_account(&account).unwrap();

    let mut richer = checking();
    richer.balance = Some(52_500);
    richer.mask = "123456789".into();
    let delta = SyncDelta { accounts: vec![richer], ..Default::default() };
    vault.apply_bank_sync(link.id, &delta).unwrap();

    let after = vault.bank_account(account.id).unwrap();
    assert_eq!(after.name, "Everyday spending");
    assert!(after.hidden);
    assert_eq!(after.balance, Some(52_500));
    assert_eq!(after.mask, "6789", "never more than the last four");
    let today = jiff::Zoned::now().date();
    let history = vault.balance_history(today, today).unwrap();
    assert_eq!(history.len(), 1, "one snapshot a day, the second replacing the first");
    assert_eq!(history[0].balance, 52_500);
}

#[test]
fn the_banks_category_lands_on_a_starter_category_unless_a_rule_says_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let link = a_link(&vault);
    let mut rule = MoneyRule::new("costco");
    rule.category = Some(category_named(&vault, "General merchandise").id);
    vault.save_money_rule(&rule).unwrap();

    let mut groceries = bought("t1", 3, -6000, "TRADER JOES");
    groceries.bank_category = "FOOD_AND_DRINK/FOOD_AND_DRINK_GROCERIES".into();
    let mut warehouse = bought("t2", 3, -9000, "COSTCO WHSE");
    warehouse.bank_category = "FOOD_AND_DRINK/FOOD_AND_DRINK_GROCERIES".into();
    let mut deposit = bought("t3", 3, 20_000, "ATM DEPOSIT");
    deposit.bank_category = "TRANSFER_IN/TRANSFER_IN_DEPOSIT".into();
    sync(&vault, &link, vec![groceries, warehouse, deposit], &[]);

    let joes = by_payee(&vault, "TRADER JOES");
    assert_eq!(joes.category, Some(category_named(&vault, "Groceries").id));
    assert_eq!(joes.categorized_by, CategorizedBy::Bank);
    let costco = by_payee(&vault, "COSTCO WHSE");
    assert_eq!(costco.category, rule.category);
    assert_eq!(costco.categorized_by, CategorizedBy::Rule { rule_id: rule.id });
    let cash = by_payee(&vault, "ATM DEPOSIT");
    assert_eq!((cash.category, cash.categorized_by), (None, CategorizedBy::Nobody));
}

// ---- disconnecting -------------------------------------------------------

#[test]
fn keeping_history_detaches_the_accounts_as_manual_ones() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    sync(&vault, &link, vec![bought("t1", 3, -100, "ONE")], &[]);

    vault.delete_bank_link(link.id, true).unwrap();
    let accounts = vault.bank_accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert!(accounts[0].is_manual());
    assert!(accounts[0].provider_account_id.is_empty());
    let kept = only(&vault);
    // No sync can bring it back now, so it may go.
    vault.delete_transactions(&[kept.id]).unwrap();
    assert!(all(&vault).is_empty());
}

#[test]
fn disconnecting_without_history_takes_the_accounts_and_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    sync(&vault, &link, vec![bought("t1", 3, -100, "ONE")], &[]);
    assert_eq!(vault.bank_link_access(link.id).unwrap().expose(), TOKEN);

    vault.delete_bank_link(link.id, false).unwrap();
    assert!(vault.bank_link_access(link.id).is_err(), "the token went with the link");
    assert!(vault.bank_accounts().unwrap().is_empty());
    assert!(all(&vault).is_empty());
    assert_eq!(vault.bank_link(link.id).unwrap_err().code(), "not_found");
}

#[test]
fn a_connected_banks_account_and_transactions_cannot_be_deleted_or_typed_into() {
    // Each would come straight back with the next sync, or sit among the
    // bank's lines looking like one of them.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = a_link(&vault);
    sync(&vault, &link, vec![bought("t1", 3, -100, "ONE")], &[]);
    let account = vault.bank_accounts().unwrap().remove(0);
    let txn = only(&vault);

    assert_eq!(vault.delete_bank_account(account.id).unwrap_err().code(), "invalid");
    assert_eq!(vault.delete_transactions(&[txn.id]).unwrap_err().code(), "invalid");
    let err = vault.import_transactions(account.id, &[line(day(1), -1, "x", "")]).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert_eq!(
        vault.add_manual_transaction(&typed(&account, 1, -1, "x")).unwrap_err().code(),
        "invalid"
    );
    assert_eq!(all(&vault).len(), 1);
}

// ---- imports and typing --------------------------------------------------

#[test]
fn importing_the_same_file_twice_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let wallet = vault.create_manual_account("Wallet", AccountKind::Cash, " usd ").unwrap();
    assert_eq!(wallet.currency, "USD");
    let file = [line(day(1), -500, "Bakery", ""), line(day(2), -1200, "Books", "FIT-2")];

    let first = vault.import_transactions(wallet.id, &file).unwrap();
    assert_eq!((first.added, first.skipped), (2, 0));
    let second = vault.import_transactions(wallet.id, &file).unwrap();
    assert_eq!((second.added, second.skipped), (0, 2));
    assert_eq!(all(&vault).len(), 2);
}

#[test]
fn two_identical_lines_in_one_file_both_land() {
    // Two coffees on one morning are two coffees; the same statement
    // imported again, or one that overlaps it, is not four.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let wallet = vault.create_manual_account("Card", AccountKind::Credit, "USD").unwrap();
    let coffee = line(day(5), -450, "Blue Bottle", "");
    let both = [coffee.clone(), coffee.clone()];

    assert_eq!(vault.import_transactions(wallet.id, &both).unwrap().added, 2);
    assert_eq!(vault.import_transactions(wallet.id, &both).unwrap().added, 0);
    let overlapping = [coffee.clone(), line(day(6), -300, "Bagel", "")];
    let report = vault.import_transactions(wallet.id, &overlapping).unwrap();
    assert_eq!((report.added, report.skipped), (1, 1));
    let three = [coffee.clone(), coffee.clone(), coffee];
    assert_eq!(vault.import_transactions(wallet.id, &three).unwrap().added, 1, "the third");
    assert_eq!(all(&vault).len(), 4);
}

#[test]
fn a_file_that_repeats_an_id_lands_once_and_a_bad_line_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let wallet = vault.create_manual_account("Wallet", AccountKind::Cash, "EUR").unwrap();
    let file = [
        line(day(1), -500, "Bakery", "FIT-1"),
        line(day(1), -500, "Bakery", "FIT-1"),
        line(day(2), -100, "Kiosk", &"x".repeat(300)),
    ];
    let report = vault.import_transactions(wallet.id, &file).unwrap();
    assert_eq!((report.added, report.skipped), (1, 1));
    assert_eq!(report.problems.len(), 1);
    assert!(report.problems[0].starts_with("line 3"), "{:?}", report.problems);
    assert_eq!(only(&vault).currency, "EUR", "the account's currency");
}

#[test]
fn a_typed_transaction_gets_rules_unless_it_was_given_a_category() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let cash = vault.create_manual_account("Cash", AccountKind::Cash, "USD").unwrap();
    let mut rule = MoneyRule::new("farmers market");
    rule.category = Some(category_named(&vault, "Groceries").id);
    vault.save_money_rule(&rule).unwrap();

    let ruled = vault.add_manual_transaction(&typed(&cash, 1, -2000, "Farmers Market")).unwrap();
    assert_eq!(ruled.categorized_by, CategorizedBy::Rule { rule_id: rule.id });
    let mut chosen = typed(&cash, 2, -2000, "Farmers Market");
    chosen.category = Some(category_named(&vault, "Restaurants").id);
    let chosen = vault.add_manual_transaction(&chosen).unwrap();
    assert_eq!(chosen.categorized_by, CategorizedBy::Person);
    assert_eq!(chosen.key, vault.money_key(&format!("manual:{}", chosen.id)).unwrap());

    let mut nowhere = typed(&cash, 3, -1, "x");
    nowhere.category = Some(everyday_core::MoneyCategoryId::new());
    assert_eq!(vault.add_manual_transaction(&nowhere).unwrap_err().code(), "invalid");
}

// ---- rules and categories ------------------------------------------------

#[test]
fn rules_never_override_a_persons_choice() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let link = a_link(&vault);
    sync(
        &vault,
        &link,
        vec![bought("t1", 3, -1599, "NETFLIX.COM"), bought("t2", 4, -1599, "NETFLIX.COM ")],
        &[],
    );
    let entertainment = category_named(&vault, "Entertainment");
    let subscriptions = category_named(&vault, "Subscriptions");
    let mine = all(&vault).into_iter().find(|t| t.date == day(3)).unwrap();
    vault
        .update_transactions(
            &[mine.id],
            &TxnPatch { category: Some(Some(entertainment.id)), ..Default::default() },
        )
        .unwrap();

    let mut rule = MoneyRule::new("netflix");
    rule.category = Some(subscriptions.id);
    rule.rename = "Netflix".into();
    vault.save_money_rule(&rule).unwrap();
    assert_eq!(vault.apply_money_rules(None).unwrap(), 1, "only the open one");
    assert_eq!(vault.apply_money_rules(Some(rule.id)).unwrap(), 0, "and nothing twice");

    let after = vault.transaction(mine.id).unwrap();
    assert_eq!(after.category, Some(entertainment.id));
    assert_eq!(after.categorized_by, CategorizedBy::Person);
    assert_eq!(after.payee_override, "", "not renamed either");
    let other = all(&vault).into_iter().find(|t| t.date == day(4)).unwrap();
    assert_eq!(other.category, Some(subscriptions.id));
    assert_eq!(other.payee_override, "Netflix");

    // And as new ones arrive.
    sync(&vault, &link, vec![bought("t3", 5, -1599, "NETFLIX.COM")], &[]);
    let arrived = all(&vault).into_iter().find(|t| t.date == day(5)).unwrap();
    assert_eq!(arrived.categorized_by, CategorizedBy::Rule { rule_id: rule.id });
}

#[test]
fn deleting_a_category_clears_its_transactions_and_rules() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let snacks = MoneyCategory::new("Snacks", "Food", CategoryKind::Expense);
    vault.save_money_category(&snacks).unwrap();
    let cash = vault.create_manual_account("Cash", AccountKind::Cash, "USD").unwrap();
    let mut chips = typed(&cash, 1, -300, "Chips");
    chips.category = Some(snacks.id);
    let chips = vault.add_manual_transaction(&chips).unwrap();
    let mut rule = MoneyRule::new("chips");
    rule.category = Some(snacks.id);
    rule.exclude = true;
    vault.save_money_rule(&rule).unwrap();

    vault.delete_money_category(snacks.id).unwrap();
    let txn = vault.transaction(chips.id).unwrap();
    assert_eq!((txn.category, txn.categorized_by), (None, CategorizedBy::Nobody));
    let rules = vault.money_rules().unwrap();
    assert_eq!(rules.len(), 1, "the rule stays, for its other half");
    assert_eq!(rules[0].category, None);
    assert!(rules[0].exclude);
    assert!(vault.money_categories().unwrap().is_empty());
}

#[test]
fn a_category_name_is_unique_among_those_in_use_and_a_budget_is_for_spending() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let starters = vault.money_categories().unwrap().len();
    vault.ensure_money_defaults().unwrap();
    assert_eq!(vault.money_categories().unwrap().len(), starters, "defaults are written once");

    let twin = MoneyCategory::new(" groceries ", "Food", CategoryKind::Expense);
    assert_eq!(vault.save_money_category(&twin).unwrap_err().code(), "invalid");
    let mut groceries = category_named(&vault, "Groceries");
    groceries.archived = true;
    vault.save_money_category(&groceries).unwrap();
    vault.save_money_category(&twin).unwrap();
    assert!(vault.money_categories().unwrap().iter().any(|c| c.name == "groceries"), "trimmed");

    let mut paid = category_named(&vault, "Paycheck");
    paid.budget = Some(100);
    assert_eq!(vault.save_money_category(&paid).unwrap_err().code(), "invalid");
    let mut coffee = category_named(&vault, "Coffee");
    coffee.budget = Some(-1);
    assert_eq!(vault.save_money_category(&coffee).unwrap_err().code(), "invalid");
    coffee.budget = Some(5_000);
    vault.save_money_category(&coffee).unwrap();
    let long = MoneyCategory::new("x".repeat(201), "", CategoryKind::Expense);
    assert_eq!(vault.save_money_category(&long).unwrap_err().code(), "invalid");
}

// ---- listing and changing transactions -----------------------------------

#[test]
fn transactions_filter_sum_and_page() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let cash = vault.create_manual_account("Cash", AccountKind::Cash, "USD").unwrap();
    let coffee = category_named(&vault, "Coffee");
    for (d, amount, payee) in [
        (1, -450, "Blue Bottle"),
        (2, -500, "Blue Bottle"),
        (3, 10_000, "Refund desk"),
        (4, -2_000, "Hardware"),
    ] {
        vault.add_manual_transaction(&typed(&cash, d, amount, payee)).unwrap();
    }
    let hardware = by_payee(&vault, "Hardware");
    vault
        .update_transactions(
            &[hardware.id],
            &TxnPatch { excluded: Some(true), ..Default::default() },
        )
        .unwrap();
    let blue: Vec<_> =
        all(&vault).into_iter().filter(|t| t.payee == "Blue Bottle").map(|t| t.id).collect();
    vault
        .update_transactions(
            &blue,
            &TxnPatch { category: Some(Some(coffee.id)), ..Default::default() },
        )
        .unwrap();

    let everything = vault.transactions(&TxnQuery::default()).unwrap();
    assert_eq!(everything.total, 4);
    assert_eq!(everything.sums.len(), 1);
    assert_eq!(
        everything.sums[0].amount,
        -450 - 500 + 10_000,
        "the excluded one left out of the sum"
    );
    let dates: Vec<Date> = everything.txns.iter().map(|t| t.date).collect();
    assert_eq!(dates, [day(4), day(3), day(2), day(1)], "newest first");

    let q = |query: TxnQuery| vault.transactions(&query).unwrap();
    assert_eq!(q(TxnQuery { text: "BLUE".into(), ..Default::default() }).total, 2);
    assert_eq!(q(TxnQuery { categories: vec![coffee.id], ..Default::default() }).total, 2);
    assert_eq!(q(TxnQuery { uncategorized: true, ..Default::default() }).total, 2);
    assert_eq!(
        q(TxnQuery { categories: vec![coffee.id], uncategorized: true, ..Default::default() })
            .total,
        4
    );
    assert_eq!(q(TxnQuery { min_amount: Some(0), ..Default::default() }).total, 1);
    assert_eq!(q(TxnQuery { max_amount: Some(-460), ..Default::default() }).total, 2);
    assert_eq!(q(TxnQuery { excluded: Some(true), ..Default::default() }).total, 1);
    assert_eq!(q(TxnQuery { from: Some(day(2)), to: Some(day(3)), ..Default::default() }).total, 2);

    let first = q(TxnQuery { limit: 3, ..Default::default() });
    assert_eq!((first.txns.len(), first.total, first.next_offset), (3, 4, Some(3)));
    let rest = q(TxnQuery { limit: 3, offset: 3, ..Default::default() });
    assert_eq!((rest.txns.len(), rest.next_offset), (1, None));

    let long = TxnQuery { text: "x".repeat(201), ..Default::default() };
    assert_eq!(vault.transactions(&long).unwrap_err().code(), "invalid");
}

#[test]
fn a_hidden_account_is_left_out_unless_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut old = vault.create_manual_account("Old", AccountKind::Cash, "USD").unwrap();
    vault.add_manual_transaction(&typed(&old, 1, -100, "x")).unwrap();
    old.hidden = true;
    vault.save_bank_account(&old).unwrap();

    assert!(all(&vault).is_empty());
    let asked = TxnQuery { accounts: vec![old.id], ..Default::default() };
    assert_eq!(vault.transactions(&asked).unwrap().total, 1);
}

#[test]
fn a_change_naming_a_missing_category_or_too_long_a_note_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let cash = vault.create_manual_account("Cash", AccountKind::Cash, "USD").unwrap();
    let txn = vault.add_manual_transaction(&typed(&cash, 1, -100, "x")).unwrap();

    let nowhere = TxnPatch {
        category: Some(Some(everyday_core::MoneyCategoryId::new())),
        ..Default::default()
    };
    assert_eq!(vault.update_transactions(&[txn.id], &nowhere).unwrap_err().code(), "invalid");
    let essay = TxnPatch { note: Some("x".repeat(2001)), ..Default::default() };
    assert_eq!(vault.update_transactions(&[txn.id], &essay).unwrap_err().code(), "invalid");
    let stored = vault.transaction(txn.id).unwrap();
    assert_eq!((stored.category, stored.note.as_str()), (None, ""), "nothing written");

    let same = TxnPatch { note: Some(String::new()), ..Default::default() };
    assert!(vault.update_transactions(&[txn.id], &same).unwrap().is_empty(), "nothing changed");
}

#[test]
fn a_manual_account_keeps_its_balance_history_and_refuses_a_made_up_currency() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut house = vault.create_manual_account("House", AccountKind::Other, "USD").unwrap();
    house.balance = Some(45_000_000);
    vault.save_bank_account(&house).unwrap();
    let today = jiff::Zoned::now().date();
    assert_eq!(vault.balance_history(today, today).unwrap()[0].balance, 45_000_000);

    house.currency = "dollars".into();
    assert_eq!(vault.save_bank_account(&house).unwrap_err().code(), "invalid");
    house.currency = "US1".into();
    assert_eq!(vault.save_bank_account(&house).unwrap_err().code(), "invalid");
    assert_eq!(
        vault.create_manual_account(" ", AccountKind::Cash, "USD").unwrap_err().code(),
        "invalid"
    );
    assert_eq!(
        vault.create_manual_account("x", AccountKind::Cash, "€").unwrap_err().code(),
        "invalid"
    );
}

#[test]
fn reports_count_what_the_vault_holds() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let cash = vault.create_manual_account("Cash", AccountKind::Cash, "USD").unwrap();
    let pay = category_named(&vault, "Paycheck");
    let transfer = category_named(&vault, "Transfer");
    let mut paid = typed(&cash, 1, 300_000, "ACME");
    paid.category = Some(pay.id);
    vault.add_manual_transaction(&paid).unwrap();
    let mut moved = typed(&cash, 2, -50_000, "To savings");
    moved.category = Some(transfer.id);
    vault.add_manual_transaction(&moved).unwrap();
    vault.add_manual_transaction(&typed(&cash, 3, -2_500, "Lunch")).unwrap();

    let month = vault.money_month(day(20)).unwrap();
    assert_eq!(month.month, "2026-10");
    assert_eq!((month.income, month.spending, month.uncategorized), (300_000, 2_500, 1));

    let flow = vault.money_cash_flow(Date::constant(2026, 11, 2), 2).unwrap();
    let months: Vec<_> = flow.iter().map(|m| (m.month.as_str(), m.income, m.spending)).collect();
    assert_eq!(months, [("2026-10", 300_000, 2_500), ("2026-11", 0, 0)]);
    assert_eq!(vault.money_cash_flow(day(1), 0).unwrap().len(), 1, "clamped to at least one");
}

// ---- credentials ---------------------------------------------------------

#[test]
fn credentials_never_come_back_from_plaid_setup() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(!vault.plaid_setup().unwrap().configured);
    assert_eq!(
        vault.plaid_access().unwrap_err().code(),
        "invalid",
        "a caller about to dial Plaid cannot"
    );

    let creds = PlaidCredentials {
        client_id: " 64d2f0c1a9e8b70013abcd12 ".into(),
        secret: "f1e2d3c4b5a6978899sekrit".into(),
        environment: PlaidEnvironment::Sandbox,
    };
    vault.set_plaid_credentials(&creds).unwrap();
    let setup = vault.plaid_setup().unwrap();
    assert!(setup.configured);
    assert_eq!(setup.environment, Some(PlaidEnvironment::Sandbox));
    assert_eq!(setup.client_id_hint, "cd12");
    let shown = format!("{} {setup:?}", serde_json::to_string(&setup).unwrap());
    assert!(!shown.contains("sekrit"), "{shown}");
    assert!(!shown.contains("64d2f0c1a9e8b70013ab"), "{shown}");

    let back = vault.plaid_access().unwrap();
    assert_eq!(back.client_id, "64d2f0c1a9e8b70013abcd12", "trimmed");
    assert_eq!(back.secret, "f1e2d3c4b5a6978899sekrit");

    vault.clear_plaid_credentials().unwrap();
    assert!(!vault.plaid_setup().unwrap().configured);
    assert!(vault.plaid_access().is_err());
}

#[test]
fn half_a_plaid_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let creds = PlaidCredentials {
        client_id: "64d2f0c1a9e8b70013abcd12".into(),
        secret: "   ".into(),
        environment: PlaidEnvironment::Production,
    };
    assert_eq!(vault.set_plaid_credentials(&creds).unwrap_err().code(), "invalid");
    assert!(!vault.plaid_setup().unwrap().configured);
}

#[test]
fn a_memo_cannot_turn_its_own_text_round_with_invisible_characters() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_money_defaults().unwrap();
    let link = a_link(&vault);
    let mut zelle = bought("txn-bidi", 3, -2_500, "Zelle\u{202E}ecnaliS\u{202C}");
    zelle.description = "From Sam\u{200B}\u{2066} please\u{0007} pay".into();
    sync(&vault, &link, vec![zelle], &[]);
    let stored = only(&vault);
    assert_eq!(stored.payee, "ZelleecnaliS", "overrides and isolates are taken out");
    assert_eq!(stored.description, "From Sam please  pay", "a control character is a space");
}

#[test]
fn a_category_colour_is_a_hex_colour_and_nothing_a_style_could_run_on() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut category = MoneyCategory::new("Gifts", "", CategoryKind::Expense);
    category.color = "#c2410c".into();
    vault.save_money_category(&category).unwrap();
    for bad in ["red;position:fixed;inset:0", "url(data:x)", "#12345", "c2410c", "#zzzzzz"] {
        category.color = bad.into();
        assert!(vault.save_money_category(&category).is_err(), "{bad:?} was accepted");
    }
}
