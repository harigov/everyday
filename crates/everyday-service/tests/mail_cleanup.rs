//! Quick Cleanup's sender list and the unified Inbox's merged thread list,
//! through the command layer against a real SQLite vault.
//!
//! The counting and the merge are each pinned where they live --
//! `everyday_core::mail::cleanup`'s unit tests, and the mail conformance
//! suite's `listing_across_mailboxes_merges_them_newest_first` -- so what is
//! checked here is only what the commands add: finding each account's Inbox
//! (by role, or by the name `INBOX` on a server that gave no roles), counting
//! only what is still in it, leaving you out, narrowing it to one kind of
//! thread or one sender, and refusing what they say they refuse.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{
    Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use everyday_service::error::codes;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

fn service() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Saved straight into the vault rather than through `save_account`, so no
/// sync task is started for an account with no server behind it.
fn seed_account(svc: &Arc<Service>, address: &str) -> AccountId {
    let mut account = Account::new(Provider::Custom, address);
    account.services.mail = true;
    svc.get().unwrap().save_account(&account).unwrap();
    account.id
}

fn seed_mailbox(
    svc: &Arc<Service>,
    account: AccountId,
    name: &str,
    role: MailboxRole,
) -> MailboxId {
    let mailbox = Mailbox::new(account, name, role);
    svc.get().unwrap().save_mailbox(&mailbox).unwrap();
    mailbox.id
}

/// One message from `from`, `days_ago` days back, in a thread of its own.
fn seed(
    svc: &Arc<Service>,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    from: &str,
    days_ago: i64,
) -> ThreadId {
    seed_as(svc, account, mailbox, uid, from, days_ago, None).0
}

/// [`seed`], filed under `category`, answering the message's id too.
fn seed_as(
    svc: &Arc<Service>,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    from: &str,
    days_ago: i64,
    category: Option<Category>,
) -> (ThreadId, MailMessageId) {
    let thread_id = ThreadId::new();
    let id = seed_into(svc, thread_id, account, mailbox, uid, from, days_ago, category);
    (thread_id, id)
}

/// One message in `thread_id`, which may already hold others.
#[allow(clippy::too_many_arguments)]
fn seed_into(
    svc: &Arc<Service>,
    thread_id: ThreadId,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    from: &str,
    days_ago: i64,
    category: Option<Category>,
) -> MailMessageId {
    let id = MailMessageId::new();
    let message = Message {
        id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{id}@example.com>"),
        date: Timestamp::now() - SignedDuration::from_hours(days_ago * 24 + 1),
        from: Address::new("Sender", from),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "A message".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    svc.get().unwrap().ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    id
}

fn emails(senders: &Value) -> Vec<&str> {
    senders.as_array().unwrap().iter().map(|s| s["email"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn inbox_senders_counts_what_is_still_in_every_inbox_and_leaves_you_out() {
    let (svc, _dir) = service();
    let work = seed_account(&svc, "me@example.com");
    let work_inbox = seed_mailbox(&svc, work, "INBOX", MailboxRole::Inbox);
    let work_archive = seed_mailbox(&svc, work, "Archive", MailboxRole::Archive);
    // A server that said nothing about which folder is which: found by name.
    let home = seed_account(&svc, "me@home.example");
    let home_inbox = seed_mailbox(&svc, home, "INBOX", MailboxRole::Other);

    for uid in 1..=3 {
        seed(&svc, work, work_inbox, uid, "news@example.com", 1);
    }
    seed(&svc, work, work_inbox, 4, "bob@example.com", 2);
    seed(&svc, work, work_inbox, 5, "me@example.com", 1);
    seed(&svc, work, work_inbox, 6, "news@example.com", 40);
    seed(&svc, work, work_archive, 1, "news@example.com", 1);
    seed(&svc, work, work_archive, 2, "news@example.com", 1);
    seed(&svc, home, home_inbox, 1, "News@Example.com", 3);

    let senders = call(&svc, "inbox_senders", json!({ "days": 30 })).await;
    assert_eq!(
        emails(&senders),
        ["news@example.com", "bob@example.com"],
        "you are never a sender, and archived or out-of-window mail is not counted"
    );
    let news = &senders[0];
    assert_eq!(news["messages"], 4, "three in one Inbox and one in the other: {news}");
    assert_eq!(news["unread"], 4);
    assert_eq!(news["threads"].as_array().unwrap().len(), 4);
    assert_eq!(news["accounts"].as_array().unwrap().len(), 2, "one row across both accounts");
    assert_eq!(news["name"], "Sender");

    let top = call(&svc, "inbox_senders", json!({ "days": 30, "limit": 1 })).await;
    assert_eq!(emails(&top), ["news@example.com"]);

    let only_home =
        call(&svc, "inbox_senders", json!({ "days": 30, "accounts": [home, home] })).await;
    assert_eq!(emails(&only_home), ["news@example.com"]);
    assert_eq!(only_home[0]["messages"], 1, "an account named twice is still counted once");

    for days in [0, 366] {
        let err = svc
            .call(Ctx::local(), "inbox_senders", json!({ "days": days }))
            .await
            .expect_err("outside one day to a year");
        assert_eq!(err.code, codes::INVALID, "{days}: {err:?}");
    }
}

#[tokio::test]
async fn inbox_senders_without_a_window_counts_everything_still_in_the_inbox() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com");
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let archive = seed_mailbox(&svc, account, "Archive", MailboxRole::Archive);
    seed(&svc, account, inbox, 1, "news@example.com", 2);
    seed(&svc, account, inbox, 2, "news@example.com", 400);
    seed(&svc, account, inbox, 3, "old@example.com", 3000);
    seed(&svc, account, archive, 1, "news@example.com", 900);

    let year = call(&svc, "inbox_senders", json!({ "days": 365 })).await;
    assert_eq!(emails(&year), ["news@example.com"]);
    assert_eq!(year[0]["messages"], 1);

    for all_time in [json!({}), json!({ "days": null })] {
        let senders = call(&svc, "inbox_senders", all_time.clone()).await;
        assert_eq!(emails(&senders), ["news@example.com", "old@example.com"], "{all_time}");
        assert_eq!(
            senders[0]["messages"], 2,
            "however old, but still only what is in the Inbox: {all_time}"
        );
    }
}

#[tokio::test]
async fn inbox_senders_narrows_to_one_kind_of_thread_and_finds_a_sender_by_name() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com");
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    // One shop sending two kinds of mail: two promotions and a receipt.
    let newsletter = Some(Category::Newsletter);
    let (promo, promo_id) = seed_as(&svc, account, inbox, 1, "shop@example.com", 1, newsletter);
    let (other_promo, other_promo_id) =
        seed_as(&svc, account, inbox, 2, "shop@example.com", 2, newsletter);
    let (receipt, _) =
        seed_as(&svc, account, inbox, 3, "shop@example.com", 3, Some(Category::Notification));
    seed_as(&svc, account, inbox, 4, "news@example.com", 1, newsletter);
    seed_as(&svc, account, inbox, 5, "bob@example.com", 1, Some(Category::Important));
    let vault = svc.get().unwrap();
    for id in [promo_id, other_promo_id] {
        assert!(vault.sort_mail_message(id, Category::Newsletter, "marketing").unwrap());
    }

    let tab = call(&svc, "inbox_senders", json!({ "category": "newsletter" })).await;
    assert_eq!(emails(&tab), ["shop@example.com", "news@example.com"]);
    assert_eq!(tab[0]["messages"], 2, "the shop's receipt is in another tab: {}", tab[0]);
    let threads: Vec<&str> =
        tab[0]["threads"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert_eq!(threads, [promo.to_string(), other_promo.to_string()]);
    assert!(!threads.contains(&receipt.to_string().as_str()), "a delete would leave it be");

    let sorted = call(&svc, "inbox_senders", json!({ "sortedAs": "marketing" })).await;
    assert_eq!(emails(&sorted), ["shop@example.com"], "news@ was never sorted");
    assert_eq!(sorted[0]["messages"], 2);
    for nothing in [
        json!({ "sortedAs": "no-such-category" }),
        json!({ "sortedAs": "marketing", "category": "notification" }),
    ] {
        let senders = call(&svc, "inbox_senders", nothing.clone()).await;
        assert_eq!(emails(&senders), Vec::<&str>::new(), "{nothing}");
    }

    // Bob ranks last, and is found anyway: the words are looked for
    // before the list is cut to the limit.
    let bob = call(&svc, "inbox_senders", json!({ "query": " SENDER bob ", "limit": 1 })).await;
    assert_eq!(emails(&bob), ["bob@example.com"], "a word of the name and one of the address");
    let blank = call(&svc, "inbox_senders", json!({ "query": "  " })).await;
    assert_eq!(emails(&blank).len(), 3, "a blank query is no filter");

    let err = svc
        .call(Ctx::local(), "inbox_senders", json!({ "query": "x".repeat(255) }))
        .await
        .expect_err("longer than any address");
    assert_eq!(err.code, codes::INVALID, "{err:?}");
}

#[tokio::test]
async fn inbox_senders_older_than_leaves_recent_mail_and_its_conversations_alone() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com");
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    seed(&svc, account, inbox, 1, "news@example.com", 2);
    let old = seed(&svc, account, inbox, 2, "news@example.com", 40);
    let older = seed(&svc, account, inbox, 3, "news@example.com", 400);
    // Bob's conversation began two months ago, but he replied yesterday:
    // deleting it would delete the reply, so none of it is old mail.
    let talk = seed(&svc, account, inbox, 4, "bob@example.com", 60);
    seed_into(&svc, talk, account, inbox, 5, "bob@example.com", 1, None);
    seed(&svc, account, inbox, 6, "bob@example.com", 90);

    let senders = call(&svc, "inbox_senders", json!({ "olderThanDays": 30 })).await;
    assert_eq!(emails(&senders), ["news@example.com", "bob@example.com"]);
    let news = &senders[0];
    assert_eq!(news["messages"], 2, "the one from two days ago stays: {news}");
    assert_eq!(news["threads"], json!([old, older]));
    assert_eq!(senders[1]["messages"], 1, "only the conversation with nothing newer");
    assert!(!senders[1]["threads"].as_array().unwrap().contains(&json!(talk)));

    // Between a year and thirty days ago: one each, so by address.
    let window = call(&svc, "inbox_senders", json!({ "days": 365, "olderThanDays": 30 })).await;
    assert_eq!(emails(&window), ["bob@example.com", "news@example.com"]);
    assert_eq!(window[1]["threads"], json!([old]), "not the one from 400 days ago: {window}");

    for (args, why) in [
        (json!({ "olderThanDays": 0 }), "no days at all"),
        (json!({ "olderThanDays": 3_651 }), "more than ten years"),
        (json!({ "days": 30, "olderThanDays": 30 }), "a window that ends where it starts"),
    ] {
        let err = svc.call(Ctx::local(), "inbox_senders", args).await.expect_err(why);
        assert_eq!(err.code, codes::INVALID, "{why}: {err:?}");
    }
}

#[tokio::test]
async fn inbox_senders_lists_past_fifty_when_asked() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com");
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    for uid in 1..=60 {
        seed(&svc, account, inbox, uid, &format!("sender{uid}@example.com"), 1);
    }
    let senders = call(&svc, "inbox_senders", json!({ "limit": 500 })).await;
    assert_eq!(emails(&senders).len(), 60);
    let held = call(&svc, "inbox_senders", json!({ "limit": 100_000 })).await;
    assert_eq!(emails(&held).len(), 60, "held to the most, not refused");
    let default = call(&svc, "inbox_senders", json!({})).await;
    assert_eq!(emails(&default).len(), 20, "twenty when the limit is left out");
}

#[tokio::test]
async fn list_threads_across_merges_every_named_inbox_and_refuses_a_flood() {
    let (svc, _dir) = service();
    let work = seed_account(&svc, "me@example.com");
    let work_inbox = seed_mailbox(&svc, work, "INBOX", MailboxRole::Inbox);
    let home = seed_account(&svc, "me@home.example");
    let home_inbox = seed_mailbox(&svc, home, "INBOX", MailboxRole::Inbox);
    let older = seed(&svc, work, work_inbox, 1, "a@example.com", 2);
    let newer = seed(&svc, home, home_inbox, 1, "b@example.com", 1);

    let page =
        call(&svc, "list_threads_across", json!({ "mailboxes": [work_inbox, home_inbox] })).await;
    let ids: Vec<&str> =
        page["threads"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [newer.to_string(), older.to_string()], "newest first, across accounts");

    let empty = call(&svc, "list_threads_across", json!({ "mailboxes": [] })).await;
    assert!(empty["threads"].as_array().unwrap().is_empty());

    let flood: Vec<MailboxId> = (0..65).map(|_| MailboxId::new()).collect();
    let err = svc
        .call(Ctx::local(), "list_threads_across", json!({ "mailboxes": flood }))
        .await
        .expect_err("more than 64 mailboxes");
    assert_eq!(err.code, codes::INVALID, "{err:?}");
}
