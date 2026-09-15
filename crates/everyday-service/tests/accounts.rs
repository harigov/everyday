//! Accounts: saving, signing in and deleting one must start or stop its
//! sync task immediately, and deleting one must take its local pack files
//! with it -- not only its vault rows.
//!
//! Every test here drives the command layer (`svc.call`), never the vault
//! directly, because the wiring under test lives in
//! `everyday_service::domains::accounts`'s command bodies, not in
//! `Vault::save_account`/`delete_account` themselves.

use std::sync::Arc;

use everyday_core::account::{Account, AccountSecret, AuthMethod, Provider};
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use everyday_service::mailsync::wiring::task_key;
use everyday_service::supervisor::TaskState;
use serde_json::{Value, json};

#[allow(dead_code)]
mod support;

fn service() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A password-authenticated account, `services.mail` as given -- never
/// signed in, so its task ends almost immediately with `NeedsSignIn`
/// (`crate::mailsync::credential::resolve` needs no network to know that),
/// which is exactly what makes it safe to drive through the real command
/// layer in a test with no server anywhere: the property under test is
/// *that* `ensure_account_task`/`stop_account_task` were called, which is
/// true the instant the supervisor's entry exists, however quickly the
/// task itself then finishes.
fn account(mail_on: bool) -> Account {
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.auth = AuthMethod::Password { username: "me@example.com".into() };
    account.services.mail = mail_on;
    account
}

#[tokio::test]
async fn saving_an_account_with_mail_on_starts_its_task() {
    let (svc, _dir) = service();
    let account = account(true);
    let id = account.id;
    assert_eq!(svc.supervisor().state(&task_key(id)), None, "nothing registered yet");

    call(&svc, "save_account", json!({ "account": account })).await;

    assert!(
        svc.supervisor().state(&task_key(id)).is_some(),
        "save_account must register this account's task immediately"
    );
}

#[tokio::test]
async fn switching_mail_off_stops_the_task_and_back_on_starts_it_again() {
    let (svc, _dir) = service();
    let mut account = account(true);
    let id = account.id;
    call(&svc, "save_account", json!({ "account": account.clone() })).await;
    assert!(svc.supervisor().state(&task_key(id)).is_some());

    account.services.mail = false;
    call(&svc, "save_account", json!({ "account": account.clone() })).await;
    assert_eq!(
        svc.supervisor().state(&task_key(id)),
        Some(TaskState::Stopped),
        "save_account must stop the task the moment services.mail goes off"
    );

    account.services.mail = true;
    call(&svc, "save_account", json!({ "account": account })).await;
    assert_ne!(
        svc.supervisor().state(&task_key(id)),
        Some(TaskState::Stopped),
        "switching mail back on must start the task again immediately, not wait for an unlock"
    );
}

#[tokio::test]
async fn saving_a_password_starts_the_task_for_an_account_with_mail_on() {
    let (svc, _dir) = service();
    let account = account(true);
    let id = account.id;
    let vault = svc.get().unwrap();
    vault.save_account(&account).unwrap(); // direct write: no command run yet, no task registered
    assert_eq!(svc.supervisor().state(&task_key(id)), None);

    call(&svc, "save_account_password", json!({ "id": id, "password": "hunter2" })).await;

    assert!(
        svc.supervisor().state(&task_key(id)).is_some(),
        "a freshly stored password must start the task immediately"
    );
}

#[tokio::test]
async fn deleting_an_account_stops_its_task_first() {
    let (svc, _dir) = service();
    let account = account(true);
    let id = account.id;
    call(&svc, "save_account", json!({ "account": account })).await;
    assert!(svc.supervisor().state(&task_key(id)).is_some());

    call(&svc, "delete_account", json!({ "id": id })).await;

    assert_eq!(
        svc.supervisor().state(&task_key(id)),
        Some(TaskState::Stopped),
        "delete_account must stop the task rather than leave it running against deleted rows"
    );
    let vault = svc.get().unwrap();
    assert!(vault.account(id).is_err(), "the account row itself must be gone");
}

/// Deleting an account must take its local pack files with it, not only the
/// SQL rows `Vault::delete_account` already removes.
#[tokio::test]
async fn deleting_an_account_deletes_its_local_pack_files() {
    let (svc, _dir) = service();
    let account = account(true);
    let id = account.id;
    let vault = svc.get().unwrap();
    vault.save_account(&account).unwrap();
    vault
        .save_account_secret(
            id,
            &AccountSecret { password: Some("hunter2".into()), ..Default::default() },
        )
        .unwrap();

    let packs = svc.packs().expect("the pack store opens whenever a vault does");
    let refs = packs.append_batch(&id.to_string(), &[b"a raw message"]).unwrap();
    assert!(packs.read(&refs[0]).is_ok(), "sanity: the pack is readable before the delete");

    call(&svc, "delete_account", json!({ "id": id })).await;

    assert!(
        packs.read(&refs[0]).is_err(),
        "the account's pack file must be gone after delete_account, not only its vault rows"
    );
}
