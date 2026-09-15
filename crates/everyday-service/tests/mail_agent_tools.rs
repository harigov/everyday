//! The two mail-tool tests that need a running [`Service`] rather than a
//! bare vault: the confirmation gate on an [`Effect::Outward`] call made
//! through `run_tool` with no chat window to ask in, and the rate limiter
//! wired to a real `Service::check_mail_rate_limit`.
//!
//! `crates/everyday-vault/tests/mail_tools.rs` covers everything else in
//! `docs/plans/mail.md`'s phase 5 that only needs `tools::dispatch` -- the
//! permissions matrix and the hostile corpus, chiefly.

#[allow(dead_code)]
mod support;

use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{
    Address, CategorySource, Draft, Mailbox, MailboxRole, Message, MessageFlags,
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_service::ctx::Ctx;
use everyday_service::error::codes;
use jiff::Timestamp;
use serde_json::json;

fn seed_account(svc: &std::sync::Arc<everyday_service::Service>) -> Account {
    let vault = svc.get().unwrap();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
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

// ---- Effect::Outward's confirmation gate, with no chat window -----------

#[test]
fn send_draft_through_run_tool_needs_confirm_destructive_even_for_the_vaults_owner() {
    let (svc, _dir) = support::vault::service(None);
    let account = seed_account(&svc);
    let vault = svc.get().unwrap();
    let mut draft =
        Draft::new(account.id, account.address.clone(), everyday_core::mail::Origin::Person);
    draft.to = vec![Address::bare("friend@example.com")];
    draft.subject = "hi".into();
    vault.save_draft(&draft).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let err = svc
            .call(
                Ctx::local(),
                "run_tool",
                json!({ "name": "send_draft", "arguments": { "draft_id": draft.id.to_string() } }),
            )
            .await
            .expect_err("an outward call must stop and ask, exactly like a destructive one");
        assert_eq!(err.code, codes::CONFIRM_REQUIRED, "{err:?}");

        // Confirmed, it goes through and is queued -- never sent on the
        // spot, undo window and all.
        let out = svc
            .call(
                Ctx::local(),
                "run_tool",
                json!({
                    "name": "send_draft",
                    "arguments": { "draft_id": draft.id.to_string() },
                    "confirmDestructive": true,
                }),
            )
            .await
            .expect("confirmed, the send proceeds");
        assert_eq!(out["action"], "queued to send");
    });
}

/// What `VaultHost::call` in `everyday-server` actually sends for an
/// outward call: `confirmDestructive` always `true`, because MCP has no
/// confirmation UI to ask in and the real gate is the account's own
/// `mcp_access.send` switch. Built here the same way that module builds it,
/// rather than importing it, since `everyday-service` cannot depend on
/// `everyday-server`.
#[test]
fn mcp_may_send_once_an_account_turns_it_on_and_is_queued_through_the_undo_window() {
    let (svc, _dir) = support::vault::service(None);
    let mut account = seed_account(&svc);
    account.mcp_access.send = true;
    let vault = svc.get().unwrap();
    vault.save_account(&account).unwrap();

    let mut draft =
        Draft::new(account.id, account.address.clone(), everyday_core::mail::Origin::Person);
    draft.to = vec![Address::bare("friend@example.com")];
    draft.subject = "hi".into();
    vault.save_draft(&draft).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let out = svc
            .call(
                Ctx::local(),
                "run_tool",
                json!({
                    "name": "send_draft",
                    "arguments": { "draft_id": draft.id.to_string() },
                    "confirmDestructive": true,
                    "caller": mcp_caller("client-1"),
                }),
            )
            .await
            .expect("this account allows mcp to send");
        assert_eq!(out["action"], "queued to send");
        assert!(out["note"].as_str().unwrap_or_default().contains("undo window"), "{out:?}");
    });
}

/// `respond_to_invite` is the catalogue's second `Effect::Outward` tool, and
/// carries exactly the same confirmation requirement `send_draft` does --
/// see `must_confirm` in `everyday_service::agent`, which is not keyed on
/// the tool's *name*. A random, unseeded message id is enough here: the
/// refusal fires before `run_tool` ever reaches the tool's own body, the
/// same way `send_draft`'s equivalent test needs no recipient either.
#[test]
fn respond_to_invite_through_run_tool_needs_confirm_destructive_too() {
    let (svc, _dir) = support::vault::service(None);
    let _account = seed_account(&svc);
    let message_id = MailMessageId::new();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let err = svc
            .call(
                Ctx::local(),
                "run_tool",
                json!({
                    "name": "respond_to_invite",
                    "arguments": { "message_id": message_id.to_string(), "response": "accepted" },
                }),
            )
            .await
            .expect_err("an outward call must stop and ask, exactly like send_draft's own");
        assert_eq!(err.code, codes::CONFIRM_REQUIRED, "{err:?}");
    });
}

#[test]
fn mcp_send_is_still_refused_for_an_account_that_has_not_turned_it_on() {
    let (svc, _dir) = support::vault::service(None);
    let account = seed_account(&svc); // mcp_access.send defaults to false
    let vault = svc.get().unwrap();
    let mut draft =
        Draft::new(account.id, account.address.clone(), everyday_core::mail::Origin::Person);
    draft.to = vec![Address::bare("friend@example.com")];
    vault.save_draft(&draft).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let err = svc
            .call(
                Ctx::local(),
                "run_tool",
                json!({
                    "name": "send_draft",
                    "arguments": { "draft_id": draft.id.to_string() },
                    "confirmDestructive": true,
                    "caller": mcp_caller("client-1"),
                }),
            )
            .await
            .expect_err("mcp has not been permitted to send on this account");
        assert!(err.message.to_lowercase().contains("send"), "{err:?}");
    });
}

// ---- the rate limit ------------------------------------------------------

/// `docs/plans/mail.md`'s risk table: "the assistant floods the outbox...
/// exceeding it is an error the model reads." Run through the very path an
/// MCP client's rapid archiving takes -- `run_tool`, `Service::
/// check_mail_rate_limit`, the per-minute bucket -- rather than calling the
/// limiter directly, since what phase 5 actually promises is that this
/// path is wired end to end.
#[test]
fn the_rate_limit_trips_after_rapid_archives_from_mcp_and_answers_with_something_the_model_reads() {
    let (svc, _dir) = support::vault::service(None);
    let mut account = seed_account(&svc);
    account.mcp_access.archive = true;
    let vault = svc.get().unwrap();
    vault.save_account(&account).unwrap();
    let inbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);

    // One more than `Service::check_mail_rate_limit`'s per-minute budget --
    // see that method's own `PER_MINUTE` constant.
    let threads: Vec<ThreadId> =
        (0..65).map(|uid| seed_thread(&svc, account.id, inbox, uid)).collect();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut refused = None;
        for thread in &threads {
            let outcome = svc
                .call(
                    Ctx::local(),
                    "run_tool",
                    json!({
                        "name": "archive_thread",
                        "arguments": { "thread_id": thread.to_string() },
                        "caller": mcp_caller("flood-client"),
                    }),
                )
                .await;
            if let Err(e) = outcome {
                refused = Some(e);
                break;
            }
        }
        let err = refused.expect("65 rapid archives from one mcp client must trip the limit");
        assert_eq!(err.code, codes::RATE_LIMITED, "{err:?}");
        assert!(err.message.to_lowercase().contains("mail"), "{err:?}");
    });
}

// ---- the after-write hook (finding 3) -------------------------------------

/// `archive_thread`, called through `run_tool` exactly the way an MCP
/// client's own would be, must wake the account's sync task and invalidate
/// its cached unread counts -- the same two things a person's own archive,
/// through `domains::mail::batch_op`, already does (`Service::
/// notify_mail_write`). Before `ToolContext::after_mail_write` existed, the
/// tools in `agent::tools::mail` wrote straight through the vault and
/// stopped there, so neither ever happened for an assistant's or MCP's own
/// write -- an inbox left open elsewhere would not see the archive, nor its
/// own badge update, until something unrelated happened to refresh either.
#[test]
fn a_tool_archive_wakes_the_outbox_and_invalidates_the_unread_cache() {
    let (svc, _dir) = support::vault::service(None);
    let mut account = seed_account(&svc);
    account.mcp_access.archive = true;
    let vault = svc.get().unwrap();
    vault.save_account(&account).unwrap();
    let inbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread = seed_thread(&svc, account.id, inbox, 1);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Held from before the call, so a `notify_one()` during it leaves a
        // permit this `.notified()` picks up immediately afterwards --
        // `tokio::sync::Notify`'s own contract for a waiter that asks for
        // the handle before the wake happens.
        let notify = svc.outbox_notify(account.id);
        let cache = svc.mail_unread_cache().expect("mail is open for this vault");
        // Prime the cache so there is something for the write to
        // invalidate -- an empty vault's own real count is fine, since only
        // whether this answer got thrown away is under test here.
        let _ = cache.get_or_compute(account.id, || Ok(Vec::new()));

        svc.call(
            Ctx::local(),
            "run_tool",
            json!({
                "name": "archive_thread",
                "arguments": { "thread_id": thread.to_string() },
                "caller": mcp_caller("client-1"),
            }),
        )
        .await
        .expect("archiving is allowed");

        tokio::time::timeout(std::time::Duration::from_millis(200), notify.notified())
            .await
            .expect("archiving through a tool must wake the account's sync task");

        let recomputed = std::sync::atomic::AtomicU32::new(0);
        let _ = cache.get_or_compute(account.id, || {
            recomputed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Vec::new())
        });
        assert_eq!(
            recomputed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "archiving through a tool must invalidate the cached unread counts"
        );
    });
}
