//! Money, out of a vault.
//!
//! Its own file for the Feed's reason and one more: what matters most about
//! this export is what is *not* in it. A vault with Plaid keys, a linked
//! bank's access token, a sync cursor and every kind of provider id is
//! exported, and every byte of the archive is searched for each of them. And
//! a payee that is a spreadsheet formula -- a bank's payee is the merchant's
//! words, and a transfer's memo the sender's -- must open as text.

use everyday_core::money::{
    AccessToken, AccountKind, BankLink, CategoryKind, ImportedTxn, LinkProvider, LinkState,
    MoneyCategory, MoneyRule, NewTxn, PlaidCredentials, PlaidEnvironment, SyncDelta, SyncedAccount,
    SyncedTxn, TxnQuery,
};
use everyday_core::{Vault, VaultConfig};
use everyday_transfer::{Options, zip};
use jiff::civil::Date;

const SECRET: &str = "secret-DO-NOT-EXPORT-5f4e3d";
const CLIENT_ID: &str = "client-id-ABCDEFGH-WXYZ";
const TOKEN: &str = "access-sandbox-TOKEN-NEVER-EXPORTED";
const ITEM_ID: &str = "item-PLAID-ITEM-777";
const INSTITUTION_ID: &str = "ins_PLAID_INST_42";
const PLAID_ACCOUNT_ID: &str = "PLAID-ACCOUNT-555";
const PLAID_TXN_ID: &str = "PLAID-TXN-999";
const FITID: &str = "FITID-IMPORT-31337";
const CURSOR: &str = "CURSOR-OPAQUE-abc123";
const FORMULA: &str = "=cmd|' /C calc'!A0";

fn vault(dir: &std::path::Path) -> Vault {
    everyday_vault::create(
        dir,
        VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
            forget_key_seconds: 0,
        },
    )
    .unwrap()
}

fn only_money() -> Options {
    Options { parts: vec!["money".into()], media: false }
}

fn text<'a>(archive: &'a zip::Reader, name: &str) -> &'a str {
    std::str::from_utf8(archive.get(name).unwrap_or_else(|| panic!("no {name} in the archive")))
        .unwrap()
}

/// Keys, a linked bank with a token, a cursor and one synced account and
/// transaction (whose payee is a formula); an account kept by hand with a
/// typed transaction and an imported one; a budgeted category and a rule.
fn fill(vault: &Vault) -> MoneyCategory {
    vault
        .set_plaid_credentials(&PlaidCredentials {
            client_id: CLIENT_ID.into(),
            secret: SECRET.into(),
            environment: PlaidEnvironment::Sandbox,
        })
        .unwrap();

    let link = BankLink::new(
        LinkProvider::Plaid {
            item_id: ITEM_ID.into(),
            institution_id: INSTITUTION_ID.into(),
            environment: PlaidEnvironment::Sandbox,
        },
        "My Credit Union",
    );
    vault.add_bank_link(&link, &AccessToken(TOKEN.into())).unwrap();
    vault
        .apply_bank_sync(
            link.id,
            &SyncDelta {
                accounts: vec![SyncedAccount {
                    provider_account_id: PLAID_ACCOUNT_ID.into(),
                    name: "Everyday Checking".into(),
                    official_name: "Everyday Checking Plus".into(),
                    mask: "0042".into(),
                    kind: AccountKind::Cash,
                    subtype: "checking".into(),
                    currency: "USD".into(),
                    balance: Some(123_456),
                    available: Some(120_000),
                    limit: None,
                }],
                upserts: vec![SyncedTxn {
                    provider_account_id: PLAID_ACCOUNT_ID.into(),
                    provider_id: PLAID_TXN_ID.into(),
                    replaces_pending: None,
                    date: Date::constant(2026, 10, 3),
                    amount: -1250,
                    currency: "USD".into(),
                    payee: FORMULA.into(),
                    description: "@SUM(A1)".into(),
                    pending: false,
                    bank_category: "FOOD_AND_DRINK".into(),
                }],
                removed: Vec::new(),
            },
        )
        .unwrap();
    vault
        .save_bank_link_state(&LinkState { cursor: CURSOR.into(), ..LinkState::new(link.id) })
        .unwrap();

    let mut groceries = MoneyCategory::new("Groceries", "Food", CategoryKind::Expense);
    groceries.budget = Some(40_000);
    vault.save_money_category(&groceries).unwrap();
    let mut rule = MoneyRule::new("market");
    rule.category = Some(groceries.id);
    vault.save_money_rule(&rule).unwrap();

    let jar = vault.create_manual_account("Cash jar", AccountKind::Cash, "USD").unwrap();
    vault
        .add_manual_transaction(&NewTxn {
            account_id: jar.id,
            date: Date::constant(2026, 10, 4),
            amount: -500,
            payee: "Farmers market".into(),
            note: "peaches, plums".into(),
            category: Some(groceries.id),
        })
        .unwrap();
    vault
        .import_transactions(
            jar.id,
            &[ImportedTxn {
                date: Date::constant(2026, 10, 5),
                amount: 2000,
                payee: "Refund".into(),
                description: "from a file".into(),
                provider_id: FITID.into(),
            }],
        )
        .unwrap();
    groceries
}

#[test]
fn the_export_holds_every_record_and_never_a_key_a_token_a_cursor_or_a_provider_id() {
    let dir = tempfile::tempdir().unwrap();
    let source = vault(dir.path());
    fill(&source);
    let keys: Vec<String> = source
        .transactions(&TxnQuery::default())
        .unwrap()
        .txns
        .into_iter()
        .map(|t| t.key)
        .collect();
    assert_eq!(keys.len(), 3);

    let (bytes, manifest) = everyday_transfer::export(&source, &only_money(), Vec::new()).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    assert_eq!(manifest.parts[0].id, "money");
    let archive = zip::Reader::open(&bytes).unwrap();
    for name in
        ["money/accounts.csv", "money/transactions.csv", "money/categories.csv", "money/rules.json"]
    {
        assert!(archive.contains(name), "{name} is missing");
    }

    // Every byte of every file, the manifest and the README included.
    let mut everything = String::new();
    for name in archive.names() {
        everything.push_str(&String::from_utf8_lossy(archive.get(name).unwrap()));
        everything.push('\n');
    }
    let client_id_head = &CLIENT_ID[..CLIENT_ID.len() - 4];
    for leak in [
        SECRET,
        client_id_head,
        TOKEN,
        ITEM_ID,
        INSTITUTION_ID,
        PLAID_ACCOUNT_ID,
        PLAID_TXN_ID,
        FITID,
        CURSOR,
    ] {
        assert!(!everything.contains(leak), "{leak} reached the export");
    }
    for key in &keys {
        assert!(!everything.contains(key.as_str()), "a dedupe key reached the export");
    }

    let accounts = text(&archive, "money/accounts.csv");
    assert!(accounts.starts_with("name,bank,official_name,mask,kind"), "{accounts}");
    assert!(
        accounts.contains("Everyday Checking,My Credit Union,Everyday Checking Plus,0042,cash"),
        "{accounts}"
    );
    assert!(accounts.contains(",1234.56,1200.00,"), "{accounts}");
    assert!(accounts.contains("Cash jar,,"), "{accounts}");

    let txns = text(&archive, "money/transactions.csv");
    assert_eq!(txns.lines().count(), 4, "a header and three transactions: {txns}");
    // The formula is there, as text a spreadsheet will not run.
    assert!(txns.contains(&format!("'{FORMULA}")), "{txns}");
    assert!(txns.contains("'@SUM(A1)"), "{txns}");
    for line in txns.lines() {
        for cell in line.split(',') {
            let bare = cell.trim_start_matches('"');
            assert!(!bare.starts_with('=') && !bare.starts_with('@'), "a live formula: {line}");
        }
    }
    // Amounts stay numbers, oldest first.
    let first = txns.lines().nth(1).unwrap();
    assert!(first.starts_with("2026-10-03,Everyday Checking,"), "{first}");
    assert!(first.contains(",-12.50,USD,"), "{first}");
    assert!(txns.contains("\"peaches, plums\""), "{txns}");
    assert!(txns.contains("Groceries,Food,"), "{txns}");
    assert!(txns.contains(",file,"), "{txns}");

    let categories = text(&archive, "money/categories.csv");
    assert!(categories.contains("Groceries,Food,expense,400.00,USD,"), "{categories}");

    let rules: serde_json::Value =
        serde_json::from_str(text(&archive, "money/rules.json")).unwrap();
    assert_eq!(rules["rules"].as_array().unwrap().len(), 1);
    assert_eq!(rules["rules"][0]["text"], "market");

    // The settings dialog counts what a person made: two accounts, three
    // transactions and a rule.
    let survey = everyday_transfer::survey(&source).unwrap();
    let money = survey.iter().find(|p| p.id == "money").expect("money is offered");
    assert_eq!(money.records, 6);
    assert!(!money.imports, "Money is not read back in");
}

#[test]
fn a_vault_that_never_used_money_writes_nothing_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // The starter categories every vault is given are not something anybody
    // made.
    vault.ensure_money_defaults().unwrap();

    let (bytes, manifest) = everyday_transfer::export(&vault, &only_money(), Vec::new()).unwrap();
    assert!(manifest.parts.is_empty(), "{:?}", manifest.parts);
    let archive = zip::Reader::open(&bytes).unwrap();
    assert!(archive.names().all(|n| !n.starts_with("money/")));
    let survey = everyday_transfer::survey(&vault).unwrap();
    assert_eq!(survey.iter().find(|p| p.id == "money").map(|p| p.records), Some(0));
}

#[test]
fn a_budget_set_before_any_account_is_still_exported() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut rent = MoneyCategory::new("Rent", "Home", CategoryKind::Expense);
    rent.budget = Some(150_000);
    vault.save_money_category(&rent).unwrap();

    let (bytes, _) = everyday_transfer::export(&vault, &only_money(), Vec::new()).unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();
    let categories = text(&archive, "money/categories.csv");
    assert!(categories.contains("Rent,Home,expense,1500.00,USD,"), "{categories}");
    assert!(!archive.contains("money/transactions.csv"));
}
