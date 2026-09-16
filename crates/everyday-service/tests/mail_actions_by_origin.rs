//! `mail_actions_by_origin`: what Settings → Sharing's own "What connected
//! agents did with mail" and the assistant's matching settings section read.
//! Driven through `run_tool` for the write, exactly the path an MCP client
//! or the assistant actually takes, so the op this command later resolves is
//! a real one rather than one this file poked into the store by hand.

#[allow(dead_code)]
mod support;

use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_service::ctx::{Caller, Ctx};
use jiff::Timestamp;
use serde_json::json;

fn seed_account(svc: &std::sync::Arc<everyday_service::Service>) -> Account {
    let vault = svc.get().unwrap();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    account.mcp_access.archive = true;
    vault.save_account(&account).unwrap();
    account
}

fn seed_mailbox(
    svc: &std::sync::Arc<everyday_service::Service>,
    account: AccountId,
    role: MailboxRole,
) -> MailboxId {
    let vault = svc.get().unwrap();
    let mailbox = Mailbox::new(account, role.as_str(), role);
    vault.save_mailbox(&mailbox).unwrap();
    mailbox.id
}

fn seed_thread(
    svc: &std::sync::Arc<everyday_service::Service>,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
) -> ThreadId {
    let vault = svc.get().unwrap();
    let thread_id = ThreadId::new();
    let message_id = MailMessageId::new();
    let message = Message {
        id: message_id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{message_id}@example.com>"),
        date: Timestamp::now(),
        from: Address::bare("sender@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "A message".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault.ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    thread_id
}

fn mcp_caller(client: &str) -> serde_json::Value {
    json!({ "type": "mcp", "client": client })
}

/// See the identically-named helper in `mail_agent_tools.rs`: a `Ctx`
/// shaped like an MCP-authenticated connection, which is what `domains::
/// meta::resolve_caller` now actually keys a call's identity on.
fn mcp_ctx() -> Ctx {
    Ctx {
        caller: Caller::Device(format!(
            "{}test-device",
            everyday_core::agent::tools::MCP_DEVICE_ID_PREFIX
        )),
        ..Ctx::local()
    }
}

#[test]
fn lists_what_an_mcp_client_archived_newest_first_with_a_resolved_subject() {
    let (svc, _dir) = support::vault::service(None);
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread = seed_thread(&svc, account.id, inbox, 1);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        svc.call(
            mcp_ctx(),
            "run_tool",
            json!({
                "name": "archive_thread",
                "arguments": { "thread_id": thread.to_string() },
                "caller": mcp_caller("claude-code"),
            }),
        )
        .await
        .expect("mcp may archive: the account's own switch is on");

        let rows = svc
            .call(Ctx::local(), "mail_actions_by_origin", json!({ "kind": "mcp" }))
            .await
            .unwrap();
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["kind"], "mcp");
        assert_eq!(rows[0]["client"], "claude-code");
        assert_eq!(rows[0]["threadId"], thread.to_string());
        assert_eq!(rows[0]["subject"], "A message");
        assert_eq!(rows[0]["account"], "me@example.com");
        assert!(rows[0]["conversation"].is_null());
        assert!(rows[0]["run"].is_null());

        // The assistant's own list is untouched by an MCP client's action --
        // each `kind` reads only its own slice of the outbox.
        let assistant_rows = svc
            .call(Ctx::local(), "mail_actions_by_origin", json!({ "kind": "assistant" }))
            .await
            .unwrap();
        assert_eq!(assistant_rows.as_array().unwrap().len(), 0);
    });
}

#[test]
fn a_cursor_pages_strictly_after_the_row_it_names() {
    let (svc, _dir) = support::vault::service(None);
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let threads: Vec<ThreadId> =
        (0..3).map(|uid| seed_thread(&svc, account.id, inbox, uid)).collect();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        for thread in &threads {
            svc.call(
                mcp_ctx(),
                "run_tool",
                json!({
                    "name": "archive_thread",
                    "arguments": { "thread_id": thread.to_string() },
                    "caller": mcp_caller("claude-code"),
                }),
            )
            .await
            .expect("mcp may archive");
        }

        let first_page = svc
            .call(Ctx::local(), "mail_actions_by_origin", json!({ "kind": "mcp", "limit": 2 }))
            .await
            .unwrap();
        let first_page = first_page.as_array().unwrap();
        assert_eq!(first_page.len(), 2, "{first_page:?}");

        let cursor = first_page[1]["opId"].as_str().unwrap();
        let second_page = svc
            .call(
                Ctx::local(),
                "mail_actions_by_origin",
                json!({ "kind": "mcp", "limit": 2, "cursor": cursor }),
            )
            .await
            .unwrap();
        let second_page = second_page.as_array().unwrap();
        assert_eq!(second_page.len(), 1, "{second_page:?}");
        assert_ne!(second_page[0]["opId"], first_page[0]["opId"]);
        assert_ne!(second_page[0]["opId"], first_page[1]["opId"]);
    });
}
