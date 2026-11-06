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
async fn an_oauth_account_saves_in_the_shape_the_add_account_sheet_sends() {
    // `auth` written out by hand, as `AddAccount.svelte` builds it, rather
    // than serialised from Rust: the bug was the two sides disagreeing, so a
    // round trip through Rust's own spelling would have passed all along.
    // This failed with "missing field `client_id`".
    let (svc, _dir) = service();
    let mut account = serde_json::to_value(Account::new(Provider::Google, "me@gmail.com")).unwrap();
    account["auth"] = json!({
        "type": "oAuth",
        "clientId": "me.apps.googleusercontent.com",
        "authUrl": "https://accounts.google.com/o/oauth2/v2/auth",
        "tokenUrl": "https://oauth2.googleapis.com/token",
        "scopes": ["https://mail.google.com/"],
    });
    let id = account["id"].clone();

    call(&svc, "save_account", json!({ "account": account })).await;

    let saved = call(&svc, "get_account", json!({ "id": id })).await;
    assert_eq!(saved["auth"]["clientId"], "me.apps.googleusercontent.com", "got {saved}");
    assert!(saved["auth"]["tokenUrl"].is_string(), "camelCase back out too: got {saved}");
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

#[tokio::test]
async fn changing_the_servers_restarts_the_task_clears_the_error_and_retries_sends_now() {
    use everyday_core::account::{AccountStatus, Endpoint, EndpointSecurity};
    use everyday_core::mail::{Draft, Op, OpKind, OpState, OpTarget, Origin};

    let (svc, _dir) = service();
    let mut account = account(true);
    account.imap = Endpoint::tls("imap.example.com", 993);
    account.smtp = Endpoint::tls("smtp.wrong.example", 465);
    let id = account.id;
    call(&svc, "save_account", json!({ "account": account.clone() })).await;

    // Refused with the old settings: the account says so, and a send waits.
    let vault = svc.get().unwrap();
    let mut stored = vault.account(id).unwrap();
    stored.status = AccountStatus::Error { message: "connection timed out".into() };
    vault.save_account(&stored).unwrap();
    let draft = Draft::new(id, "me@example.com", Origin::Person);
    vault.save_draft_and_append(&draft, false, Origin::Person).unwrap();
    let later = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    let mut op =
        Op::new(id, OpKind::Send, OpTarget::Draft(draft.id), Origin::Person).not_before(later);
    op.attempts = 7;
    vault.enqueue_op(&op).unwrap();

    // A blank host or a space in one is refused, a pasted space around one is not.
    let mut edited = serde_json::to_value(&stored).unwrap();
    edited["smtp"] = json!({ "host": "smtp server", "port": 587, "security": "startTls" });
    let refused = svc.call(Ctx::local(), "save_account", json!({ "account": edited })).await;
    assert!(refused.unwrap_err().message.contains("outgoing server"));

    let mut edited = serde_json::to_value(&stored).unwrap();
    edited["smtp"] = json!({ "host": " smtp.gmail.com ", "port": 587, "security": "startTls" });
    call(&svc, "save_account", json!({ "account": edited })).await;

    let saved = vault.account(id).unwrap();
    assert_eq!(
        saved.smtp,
        Endpoint { host: "smtp.gmail.com".into(), port: 587, security: EndpointSecurity::StartTls }
    );
    // Cleared by the save; the restarted task (no password here) then says
    // what it finds now, never the old refusal.
    assert!(
        !matches!(&saved.status, AccountStatus::Error { message } if message == "connection timed out"),
        "{:?}",
        saved.status
    );
    let op = vault.op(op.id).unwrap();
    assert_eq!(op.state, OpState::Pending);
    assert!(op.not_before <= jiff::Timestamp::now(), "due again now, not in an hour");
    assert!(svc.supervisor().state(&task_key(id)).is_some());

    // Saving something else leaves a status alone. Mail off first, so no
    // task is there writing statuses of its own.
    let mut off = vault.account(id).unwrap();
    off.services.mail = false;
    call(&svc, "save_account", json!({ "account": off })).await;
    let mut stored = vault.account(id).unwrap();
    stored.status = AccountStatus::Error { message: "still down".into() };
    vault.save_account(&stored).unwrap();
    let mut renamed = stored.clone();
    renamed.display_name = "Work".into();
    call(&svc, "save_account", json!({ "account": renamed })).await;
    assert!(matches!(vault.account(id).unwrap().status, AccountStatus::Error { .. }));
}
