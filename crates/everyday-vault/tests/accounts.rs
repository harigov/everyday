//! The rules the vault applies to accounts, over a real SQLite backend:
//! round-tripping the record, keeping its secret apart from it, and the one
//! cascade -- a deleted account's secret does not outlive it.

mod support;
use everyday_core::account::{Account, AccountSecret, Provider};
use support::vault;

#[test]
fn an_account_and_its_secret_round_trip_through_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(vault.supports_accounts());

    let account = Account::new(Provider::Fastmail, "me@fastmail.com");
    vault.save_account(&account).unwrap();
    assert_eq!(vault.account(account.id).unwrap(), account);
    assert_eq!(vault.accounts().unwrap().len(), 1);

    // No secret yet: a freshly saved account has not signed in.
    assert_eq!(vault.account_secret(account.id).unwrap(), None);

    let secret = AccountSecret { password: Some("hunter2".into()), ..Default::default() };
    vault.save_account_secret(account.id, &secret).unwrap();
    assert_eq!(vault.account_secret(account.id).unwrap(), Some(secret));
}

#[test]
fn deleting_an_account_takes_its_secret_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let account = Account::new(Provider::Google, "me@gmail.com");
    vault.save_account(&account).unwrap();
    let secret =
        AccountSecret { refresh_token: Some("a-refresh-token".into()), ..Default::default() };
    vault.save_account_secret(account.id, &secret).unwrap();

    vault.delete_account(account.id).unwrap();
    assert!(vault.account(account.id).is_err(), "the account itself is gone");
    assert_eq!(
        vault.account_secret(account.id).unwrap(),
        None,
        "and so is the credential it signed in with"
    );

    // Deleting again is a no-op, the same contract every other domain makes.
    vault.delete_account(account.id).unwrap();
}

#[test]
fn saving_an_account_with_a_blank_address_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut account = Account::new(Provider::Yahoo, "me@yahoo.com");
    account.address = "   ".into();
    let err = vault.save_account(&account).unwrap_err();
    assert_eq!(err.code(), "invalid", "got {err}");
}

#[test]
fn every_provider_preset_mints_an_account_that_saves_and_loads() {
    // Not a test of the preset values themselves -- `everyday_core::account`
    // pins those -- but a check that every one of them, oauth or password,
    // custom or well-known, produces a record this vault can actually store.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    for provider in Provider::ALL {
        let account = Account::new(provider, "me@example.com");
        vault.save_account(&account).unwrap();
        assert_eq!(vault.account(account.id).unwrap().provider, provider);
    }
    assert_eq!(vault.accounts().unwrap().len(), Provider::ALL.len());
}
