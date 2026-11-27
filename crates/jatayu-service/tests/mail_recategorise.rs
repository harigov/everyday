//! Re-filing mail by hand, through the command layer against a real SQLite
//! vault: `set_thread_category`, which reads the sender off each thread a
//! person picked, and `set_sender_category`, which is handed the senders
//! outright (Quick Cleanup's rows).
//!
//! The sweep itself -- several senders in one pass, an exact address before
//! a domain, `Person` stamped on what it moves -- is pinned by the mail
//! conformance suite's `corrections_for_several_senders_are_one_sweep`, so
//! what is checked here is only what the commands add: which sender a
//! thread is taken to be from (never the person themselves), that every
//! thread picked moves even when its newest message is their own, that
//! senders are grouped per account and each account keeps its own rules,
//! and what they refuse.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use jatayu_core::account::{Account, Provider};
use jatayu_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use jatayu_core::mail::{
    Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
};
use jatayu_core::packstore::PackRef;
use jatayu_core::store::mail::IngestMessage;
use jatayu_service::Service;
use jatayu_service::ctx::Ctx;
use jatayu_service::error::codes;
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
fn seed_account(svc: &Arc<Service>, address: &str, mail: bool) -> AccountId {
    let mut account = Account::new(Provider::Custom, address);
    account.services.mail = mail;
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

/// One message from `from`, `hours_ago` hours back, in `thread_id` (which
/// may already hold others), filed under `category` by the rules.
#[allow(clippy::too_many_arguments)]
fn seed_into(
    svc: &Arc<Service>,
    thread_id: ThreadId,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    from: &str,
    hours_ago: i64,
    category: Category,
    draft: bool,
) -> MailMessageId {
    let id = MailMessageId::new();
    let message = Message {
        id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{id}@example.com>"),
        date: Timestamp::now() - SignedDuration::from_hours(hours_ago),
        from: Address::new("Somebody", from),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "A message".into(),
        snippet: String::new(),
        flags: MessageFlags { draft, ..MessageFlags::default() },
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: Some(category),
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
        unsubscribe: None,
    };
    svc.get().unwrap().ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    id
}

/// [`seed_into`] for a message in a thread of its own, answering the thread.
fn seed(
    svc: &Arc<Service>,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    from: &str,
    category: Category,
) -> ThreadId {
    let thread_id = ThreadId::new();
    seed_into(svc, thread_id, account, mailbox, uid, from, 24, category, false);
    thread_id
}

fn thread_category(svc: &Arc<Service>, thread: ThreadId) -> Option<Category> {
    svc.get().unwrap().thread(thread).unwrap().0.category
}

fn rule_for(svc: &Arc<Service>, account: AccountId, sender: &str) -> Option<Category> {
    svc.get().unwrap().category_rules(account).unwrap().for_sender(sender)
}

/// The bug this command had: it took each thread's newest message as the
/// sender, which in a conversation the person answered is their own reply
/// (or a draft of the next one) -- so it recorded a rule for their own
/// address and swept everything they had ever sent into the category. Now
/// the sender is the newest message that is neither a draft nor theirs, the
/// thread still moves, and a thread of nothing but their own mail moves
/// without anybody's rule being recorded.
#[tokio::test]
async fn moving_a_thread_you_answered_files_the_other_sender_not_you() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", true);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let sent = seed_mailbox(&svc, account, "Sent", MailboxRole::Sent);

    let (news, mine) = (Category::Newsletter, Category::Important);
    // Their message, your reply, and a draft of your next one, newest last.
    let answered = ThreadId::new();
    let theirs = seed_into(&svc, answered, account, inbox, 1, "news@example.com", 72, news, false);
    seed_into(&svc, answered, account, sent, 1, "Me@Example.com", 48, mine, false);
    let draft = seed_into(&svc, answered, account, sent, 2, "me@example.com", 24, mine, true);
    assert_eq!(thread_category(&svc, answered), Some(mine), "your draft is newest");

    // The same sender elsewhere, which the rule's sweep reaches.
    let elsewhere = seed(&svc, account, inbox, 2, "news@example.com", news);
    // Mail of your own only: no sender to correct, but it was picked, so it moves.
    let only_mine = seed(&svc, account, sent, 3, "me@example.com", mine);
    // Mail of your own nobody picked, which must stay exactly where it is.
    let unpicked = seed(&svc, account, sent, 4, "me@example.com", mine);

    call(
        &svc,
        "set_thread_category",
        json!({ "threads": [answered, only_mine], "category": "other" }),
    )
    .await;

    assert_eq!(rule_for(&svc, account, "news@example.com"), Some(Category::Other));
    assert_eq!(rule_for(&svc, account, "me@example.com"), None, "never a rule for yourself");

    let vault = svc.get().unwrap();
    let theirs = vault.mail_message(theirs).unwrap();
    assert_eq!(theirs.category, Some(Category::Other));
    assert_eq!(theirs.category_source, CategorySource::Person);
    assert_eq!(thread_category(&svc, elsewhere), Some(Category::Other), "their other mail moves");

    assert_eq!(
        thread_category(&svc, answered),
        Some(Category::Other),
        "the thread picked moves, though your draft is its newest message"
    );
    let draft = vault.mail_message(draft).unwrap();
    assert_eq!(draft.category_source, CategorySource::Person, "so the models leave it there");
    assert_eq!(thread_category(&svc, only_mine), Some(Category::Other));

    assert_eq!(thread_category(&svc, unpicked), Some(mine), "your own mail is not swept anywhere");
    let (_, unpicked_messages) = vault.thread(unpicked).unwrap();
    assert_eq!(unpicked_messages[0].category_source, CategorySource::Rules);
}

/// Senders named outright, across two accounts: each account's senders are
/// moved together and each account keeps its own rules, so a sender filed
/// differently in one inbox is not touched in the other. Addresses are
/// trimmed and lower-cased, a sender named twice counts once, and a row
/// with no address in it is passed over.
#[tokio::test]
async fn set_sender_category_moves_each_accounts_senders_and_keeps_rules() {
    let (svc, _dir) = service();
    let work = seed_account(&svc, "me@work.example", true);
    let work_inbox = seed_mailbox(&svc, work, "INBOX", MailboxRole::Inbox);
    let home = seed_account(&svc, "me@home.example", true);
    let home_inbox = seed_mailbox(&svc, home, "INBOX", MailboxRole::Inbox);

    let news_one = seed(&svc, work, work_inbox, 1, "news@lists.example", Category::Newsletter);
    let news_two = seed(&svc, work, work_inbox, 2, "News@Lists.Example", Category::Newsletter);
    let promo = seed(&svc, work, work_inbox, 3, "promo@shop.example", Category::Newsletter);
    let kept = seed(&svc, work, work_inbox, 4, "keep@lists.example", Category::Newsletter);
    let home_news = seed(&svc, home, home_inbox, 1, "news@lists.example", Category::Newsletter);
    let deals = seed(&svc, home, home_inbox, 2, "deals@home-shop.example", Category::Newsletter);

    let moved = call(
        &svc,
        "set_sender_category",
        json!({
            "senders": [
                { "account": work, "email": "  News@Lists.Example " },
                { "account": work, "email": "promo@shop.example" },
                { "account": work, "email": "PROMO@shop.example" },
                { "account": work, "email": "not an address" },
                { "account": work, "email": "" },
                { "account": home, "email": "deals@home-shop.example" },
            ],
            "category": "other",
        }),
    )
    .await;
    assert_eq!(moved, 4, "two from the list, one from the shop, one from the home shop");

    for thread in [news_one, news_two, promo, deals] {
        assert_eq!(thread_category(&svc, thread), Some(Category::Other));
        let (_, messages) = svc.get().unwrap().thread(thread).unwrap();
        assert_eq!(messages[0].category_source, CategorySource::Person);
    }
    assert_eq!(thread_category(&svc, kept), Some(Category::Newsletter), "nobody named them");
    assert_eq!(
        thread_category(&svc, home_news),
        Some(Category::Newsletter),
        "the list was named for the work account only"
    );

    assert_eq!(rule_for(&svc, work, "news@lists.example"), Some(Category::Other));
    assert_eq!(rule_for(&svc, work, "promo@shop.example"), Some(Category::Other));
    assert_eq!(rule_for(&svc, work, "deals@home-shop.example"), None);
    assert_eq!(rule_for(&svc, home, "deals@home-shop.example"), Some(Category::Other));
    assert_eq!(rule_for(&svc, home, "news@lists.example"), None);
    assert_eq!(
        svc.get().unwrap().category_rules(work).unwrap().rules.len(),
        2,
        "one rule per sender, however they were spelled: {:?}",
        svc.get().unwrap().category_rules(work).unwrap()
    );

    let again = call(
        &svc,
        "set_sender_category",
        json!({
            "senders": [{ "account": work, "email": "promo@shop.example" }],
            "category": "other",
        }),
    )
    .await;
    assert_eq!(again, 0, "already there");
}

/// Too many senders, an account that is not there, and a category that does
/// not exist are refused, and refused before anything is written; an empty
/// list and an account with mail turned off move nothing.
#[tokio::test]
async fn set_sender_category_refuses_what_it_cannot_do() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", true);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let thread = seed(&svc, account, inbox, 1, "news@lists.example", Category::Newsletter);
    let off = seed_account(&svc, "me@off.example", false);
    let off_inbox = seed_mailbox(&svc, off, "INBOX", MailboxRole::Inbox);
    let off_thread = seed(&svc, off, off_inbox, 1, "news@lists.example", Category::Newsletter);

    let too_many: Vec<Value> = (0..2001)
        .map(|i| json!({ "account": account, "email": format!("sender{i}@lists.example") }))
        .collect();
    let refusals = [
        json!({ "senders": too_many, "category": "other" }),
        json!({
            "senders": [
                { "account": account, "email": "news@lists.example" },
                { "account": AccountId::new(), "email": "news@lists.example" },
            ],
            "category": "other",
        }),
        json!({
            "senders": [{ "account": account, "email": "news@lists.example" }],
            "category": "spam",
        }),
    ];
    let codes_wanted = [codes::INVALID, codes::NOT_FOUND, codes::INVALID];
    for (args, code) in refusals.into_iter().zip(codes_wanted) {
        let err = svc.call(Ctx::local(), "set_sender_category", args).await.expect_err("refused");
        assert_eq!(err.code, code, "{err:?}");
    }
    assert_eq!(
        thread_category(&svc, thread),
        Some(Category::Newsletter),
        "nothing was moved by a request that was refused"
    );
    assert_eq!(rule_for(&svc, account, "news@lists.example"), None, "and nothing recorded");

    let nobody = json!({ "senders": [], "category": "other" });
    assert_eq!(call(&svc, "set_sender_category", nobody).await, 0);

    let off_moved = call(
        &svc,
        "set_sender_category",
        json!({
            "senders": [{ "account": off, "email": "news@lists.example" }],
            "category": "other",
        }),
    )
    .await;
    assert_eq!(off_moved, 0, "mail is off there");
    assert_eq!(thread_category(&svc, off_thread), Some(Category::Newsletter));
    assert_eq!(rule_for(&svc, off, "news@lists.example"), None);
}
