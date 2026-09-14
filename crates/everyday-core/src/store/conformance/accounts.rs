//! The account half of the conformance suite.

use super::*;
use crate::account::{ACCOUNT_SECRET_OWNER_KIND, Account, AccountSecret};
use crate::store::accounts::AccountStore;

/// Everything a backend must do with accounts, including its one cascade:
/// deleting an account takes its secret with it.
pub fn run_account_suite(store: &dyn JournalStore) {
    eprintln!("--- account conformance suite ---");

    accounts_start_empty(store);
    account_round_trips_every_field(store);
    account_put_is_idempotent(store);
    missing_account_is_not_found(store);
    deleting_an_account_takes_its_secret_with_it(store);
    unicode_survives_an_account_round_trip(store);

    account_cleanup(store);
    eprintln!("--- account suite passed ---");
}

fn account_store(store: &dyn JournalStore) -> &dyn AccountStore {
    store.accounts().expect("the account suite needs an account store")
}

fn account_cleanup(store: &dyn JournalStore) {
    let a = account_store(store);
    for account in a.list_accounts().expect("list_accounts") {
        a.delete_account(account.id).expect("delete_account");
    }
    assert!(a.list_accounts().unwrap().is_empty(), "cleanup left accounts behind");
}

fn accounts_start_empty(store: &dyn JournalStore) {
    assert!(account_store(store).list_accounts().unwrap().is_empty(), "a fresh store has none");
}

fn account_round_trips_every_field(store: &dyn JournalStore) {
    let a = account_store(store);
    let mut account = Account::new(crate::account::Provider::Google, "me@gmail.com");
    account.display_name = "Work Gmail".into();
    account.identities.push(crate::account::Identity {
        name: "Work".into(),
        address: "work@gmail.com".into(),
        signature_html: "<p>Sent from my desk</p>".into(),
    });
    account.services.calendar = true;
    account.mcp_access = crate::account::AgentMailAccess::none();
    account.attachment_cap_bytes = Some(25_000_000);
    account.status = crate::account::AccountStatus::Error { message: "the server refused".into() };
    a.put_account(&account).expect("put_account");

    assert_eq!(a.get_account(account.id).unwrap(), account, "every field must survive");
    assert_eq!(a.list_accounts().unwrap().len(), 1);

    account_cleanup(store);
}

fn account_put_is_idempotent(store: &dyn JournalStore) {
    let a = account_store(store);
    let account = Account::new(crate::account::Provider::Fastmail, "me@fastmail.com");
    a.put_account(&account).expect("put_account");
    a.put_account(&account).expect("second put");
    assert_eq!(a.list_accounts().unwrap().len(), 1, "saving twice leaves one");

    a.delete_account(account.id).expect("delete_account");
    a.delete_account(account.id).expect("deleting a missing account is a no-op");
    account_cleanup(store);
}

fn missing_account_is_not_found(store: &dyn JournalStore) {
    super::assert_not_found(account_store(store).get_account(crate::id::AccountId::new()));
}

fn deleting_an_account_takes_its_secret_with_it(store: &dyn JournalStore) {
    let Some(secrets) = store.secrets() else {
        eprintln!("  (no secret store; skipping the cascade)");
        return;
    };
    let a = account_store(store);
    let account = Account::new(crate::account::Provider::Yahoo, "me@yahoo.com");
    a.put_account(&account).expect("put_account");

    let secret = AccountSecret { password: Some("hunter2".into()), ..Default::default() };
    let bytes = serde_json::to_vec(&secret).expect("a secret serialises");
    secrets
        .put_secret(ACCOUNT_SECRET_OWNER_KIND, &account.id.to_string(), &bytes)
        .expect("put_secret");
    assert!(
        secrets.get_secret(ACCOUNT_SECRET_OWNER_KIND, &account.id.to_string()).unwrap().is_some()
    );

    a.delete_account(account.id).expect("delete_account");
    assert!(
        secrets.get_secret(ACCOUNT_SECRET_OWNER_KIND, &account.id.to_string()).unwrap().is_none(),
        "the secret must not outlive the account it belongs to"
    );

    account_cleanup(store);
}

fn unicode_survives_an_account_round_trip(store: &dyn JournalStore) {
    let a = account_store(store);
    let mut account = Account::new(crate::account::Provider::Custom, "me@example.com");
    account.display_name = "\u{5de5}\u{4f5c}\u{90ae}\u{7bb1} \u{2600}".into();
    a.put_account(&account).expect("put_account");
    let back = a.get_account(account.id).expect("get_account");
    assert_eq!(back.display_name, account.display_name);
    a.delete_account(account.id).expect("delete_account");
}
