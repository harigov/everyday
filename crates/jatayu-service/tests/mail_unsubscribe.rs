//! Unsubscribing from mail, through the command layer against a real SQLite
//! vault: which message's header an offer names, and what `unsubscribe` does
//! with each way out -- queues an email from the address the list wrote to,
//! hands a page back to open, or says there was none -- for one
//! conversation, for chosen senders across many, and for each account a
//! sender writes to.
//!
//! A one-click POST goes to a sender's server and is not made here:
//! `jatayu_service::mail::unsubscribe`'s own tests walk `post_one_click`
//! against a server on this machine.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use jatayu_core::account::{Account, Identity, Provider};
use jatayu_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use jatayu_core::mail::{
    Address, CategorySource, DraftState, ListUnsubscribe, Mailbox, MailboxRole, Message,
    MessageFlags,
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
/// sync task is started -- and so nothing sends what an unsubscribe queues.
fn seed_account(svc: &Arc<Service>, address: &str, aliases: &[&str]) -> AccountId {
    let mut account = Account::new(Provider::Custom, address);
    account.services.mail = true;
    let alias = |address: &&str| Identity {
        name: String::new(),
        address: address.to_string(),
        signature_html: String::new(),
    };
    account.identities = aliases.iter().map(alias).collect();
    svc.get().unwrap().save_account(&account).unwrap();
    account.id
}

fn seed_inbox(svc: &Arc<Service>, account: AccountId) -> MailboxId {
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    svc.get().unwrap().save_mailbox(&mailbox).unwrap();
    mailbox.id
}

/// What a message's `List-Unsubscribe` header offered, as its record keeps it.
fn header(links: &str) -> Option<ListUnsubscribe> {
    Some(ListUnsubscribe::parse(links, None).expect("a usable header"))
}

/// A message from `from` to `to` in `thread`, `hours_ago` hours back, with
/// `unsubscribe` kept on its record and no bytes stored for it.
fn message(
    account: AccountId,
    thread: ThreadId,
    from: &str,
    to: &str,
    hours_ago: i64,
    unsubscribe: Option<ListUnsubscribe>,
) -> Message {
    let id = MailMessageId::new();
    Message {
        id,
        account_id: account,
        thread_id: thread,
        message_id_header: format!("<{id}@example.com>"),
        date: Timestamp::now() - SignedDuration::from_hours(hours_ago),
        from: Address::new("Sender", from),
        to: vec![Address::bare(to)],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "This week".into(),
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
        unsubscribe,
    }
}

fn ingest(svc: &Arc<Service>, mailbox: MailboxId, uid: u32, message: Message) -> MailMessageId {
    let (id, account) = (message.id, message.account_id);
    svc.get().unwrap().ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    id
}

fn outcomes_of(value: &Value) -> &Vec<Value> {
    value.as_array().unwrap_or_else(|| panic!("not a list: {value}"))
}

#[tokio::test]
async fn an_offer_names_the_newest_message_from_somebody_else_that_offers_a_way_out() {
    let (svc, _dir) = service();
    let (me, news) = ("me@example.com", "news@list.example");
    let account = seed_account(&svc, me, &[]);
    let inbox = seed_inbox(&svc, account);
    let thread = ThreadId::new();
    let older = header("<mailto:old@list.example>");
    ingest(&svc, inbox, 1, message(account, thread, news, me, 48, older));
    let both = header("<https://list.example/u/abc>, <mailto:leave@list.example?subject=stop>");
    let newest =
        ingest(&svc, inbox, 2, message(account, thread, "News@List.example", me, 24, both));
    // A reply of the person's own since, which a list may hand back with a
    // header of its own on.
    let echoed = header("<mailto:me-leave@list.example>");
    ingest(&svc, inbox, 3, message(account, thread, me, news, 1, echoed));

    let offer = call(&svc, "unsubscribe_offer", json!({ "thread": thread })).await;
    assert_eq!(offer["message"], json!(newest), "{offer}");
    assert_eq!(offer["account"], json!(account));
    assert_eq!(offer["email"], "news@list.example");
    assert_eq!(offer["name"], "Sender");
    assert_eq!(offer["via"], "mail", "an email comes before a page: {offer}");
    assert_eq!(offer["to"], "leave@list.example");
    assert!(offer.get("askedAt").is_none(), "{offer}");

    let quiet = ThreadId::new();
    ingest(&svc, inbox, 4, message(account, quiet, "bob@example.com", me, 2, None));
    let none = call(&svc, "unsubscribe_offer", json!({ "thread": quiet })).await;
    assert_eq!(none, Value::Null, "a person's mail offers no way out");

    let err = svc
        .call(Ctx::local(), "unsubscribe_offer", json!({ "thread": ThreadId::new() }))
        .await
        .expect_err("no such thread");
    assert_eq!(err.code, codes::NOT_FOUND, "{err:?}");
}

#[tokio::test]
async fn unsubscribing_by_email_queues_one_message_from_the_address_the_list_wrote_to() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", &["lists@example.com"]);
    let inbox = seed_inbox(&svc, account);
    let thread = ThreadId::new();
    let link = header("<mailto:leave@list.example?subject=Remove%20me&cc=else@evil.example>");
    let id = ingest(
        &svc,
        inbox,
        1,
        message(account, thread, "news@list.example", "Lists@Example.com", 2, link),
    );

    let outcomes = call(&svc, "unsubscribe", json!({ "threads": [thread] })).await;
    let outcomes = outcomes_of(&outcomes);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    let outcome = &outcomes[0];
    assert_eq!(outcome["done"], true, "{outcome}");
    assert_eq!(outcome["via"], "mail");
    assert_eq!(outcome["email"], "news@list.example");
    assert_eq!(outcome["account"], json!(account));
    assert!(outcome.get("url").is_none() && outcome.get("error").is_none(), "{outcome}");

    let vault = svc.get().unwrap();
    let drafts = vault.drafts(account).unwrap();
    assert_eq!(drafts.len(), 1, "exactly one email: {drafts:?}");
    let draft = &drafts[0];
    assert_eq!(draft.identity, "lists@example.com", "the alias the list wrote to, as set up");
    assert_eq!(draft.to, vec![Address::bare("leave@list.example")]);
    assert!(draft.cc.is_empty() && draft.bcc.is_empty(), "a link's cc is never taken");
    assert_eq!(draft.subject, "Remove me");
    assert_eq!(draft.body_html, "<p>Unsubscribe</p>");
    assert!(matches!(draft.state, DraftState::Queued { .. }), "{:?}", draft.state);

    let kept = vault.mail_message(id).unwrap().unsubscribe.expect("the header is still kept");
    assert!(kept.asked_at.is_some(), "the message used remembers its sender was asked");
    let offer = call(&svc, "unsubscribe_offer", json!({ "thread": thread })).await;
    assert!(offer["askedAt"].is_string(), "{offer}");
    let senders = call(&svc, "inbox_senders", json!({})).await;
    assert_eq!(senders[0]["email"], "news@list.example");
    assert!(senders[0]["unsubscribedAt"].is_string(), "{senders}");
}

#[tokio::test]
async fn a_page_is_handed_back_to_open_and_nobody_is_marked_as_asked() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", &[]);
    let inbox = seed_inbox(&svc, account);
    let thread = ThreadId::new();
    let page = header("<https://list.example/u/abc?t=1>");
    let newsletter = message(account, thread, "news@list.example", "me@example.com", 2, page);
    let id = ingest(&svc, inbox, 1, newsletter);

    let outcomes = call(&svc, "unsubscribe", json!({ "threads": [thread] })).await;
    let outcomes = outcomes_of(&outcomes);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    let outcome = &outcomes[0];
    assert_eq!(outcome["done"], false, "{outcome}");
    assert_eq!(outcome["via"], "web");
    assert_eq!(outcome["url"], "https://list.example/u/abc?t=1");
    assert!(outcome.get("error").is_none(), "nothing failed, it is just a page: {outcome}");

    let vault = svc.get().unwrap();
    assert!(vault.drafts(account).unwrap().is_empty(), "no email for a page");
    let kept = vault.mail_message(id).unwrap().unsubscribe.unwrap();
    assert_eq!(kept.asked_at, None, "what happens on a page is not known");
}

#[tokio::test]
async fn a_sender_whose_mail_names_no_way_out_is_reported_as_such() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", &[]);
    let inbox = seed_inbox(&svc, account);
    let thread = ThreadId::new();
    let personal = message(account, thread, "bob@example.com", "me@example.com", 2, None);
    ingest(&svc, inbox, 1, personal);

    let outcomes = call(&svc, "unsubscribe", json!({ "threads": [thread] })).await;
    let outcomes = outcomes_of(&outcomes);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    let outcome = &outcomes[0];
    assert_eq!(outcome["email"], "bob@example.com");
    assert_eq!(outcome["name"], "Sender");
    assert_eq!(outcome["done"], false);
    assert!(outcome.get("via").is_none() && outcome.get("url").is_none(), "{outcome}");
    assert!(outcome["error"].is_string(), "{outcome}");
    assert!(svc.get().unwrap().drafts(account).unwrap().is_empty());
}

#[tokio::test]
async fn senders_narrow_it_to_theirs_and_one_sender_is_asked_once_however_many_threads() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", &[]);
    let inbox = seed_inbox(&svc, account);
    let (first, second, third) = (ThreadId::new(), ThreadId::new(), ThreadId::new());
    let me = "me@example.com";
    let oldest = header("<mailto:leave@list.example?subject=oldest>");
    let newest = header("<mailto:leave@list.example?subject=newest>");
    ingest(&svc, inbox, 1, message(account, first, "news@list.example", me, 72, oldest));
    ingest(&svc, inbox, 2, message(account, second, "news@list.example", me, 24, newest));
    // A shop wrote later in the same conversation, and offers a way out too.
    let shop = header("<mailto:leave@shop.example>");
    ingest(&svc, inbox, 3, message(account, second, "shop@shop.example", me, 12, shop));
    ingest(&svc, inbox, 4, message(account, third, "news@list.example", me, 48, None));

    let threads = json!([first, second, third]);
    let asked = call(
        &svc,
        "unsubscribe",
        json!({ "threads": threads, "senders": [" NEWS@list.example ", "news@list.example"] }),
    )
    .await;
    let asked = outcomes_of(&asked);
    assert_eq!(asked.len(), 1, "one sender, asked once: {asked:?}");
    assert_eq!(asked[0]["email"], "news@list.example");
    assert_eq!(asked[0]["done"], true, "{}", asked[0]);
    let vault = svc.get().unwrap();
    let drafts = vault.drafts(account).unwrap();
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    assert_eq!(drafts[0].subject, "newest", "from their newest message's header");
    assert_eq!(drafts[0].to, vec![Address::bare("leave@list.example")]);

    // Without senders, a conversation is asked of whoever wrote in it last
    // with a way out: the shop.
    let conversation = call(&svc, "unsubscribe", json!({ "threads": [second] })).await;
    let conversation = outcomes_of(&conversation);
    assert_eq!(conversation.len(), 1, "{conversation:?}");
    assert_eq!(conversation[0]["email"], "shop@shop.example");
    assert_eq!(vault.drafts(account).unwrap().len(), 2);

    let nobody = call(&svc, "unsubscribe", json!({ "threads": threads, "senders": ["  "] })).await;
    assert_eq!(nobody, json!([]), "a blank sender is nobody, not everybody");
}

#[tokio::test]
async fn the_persons_own_replies_are_never_a_sender_to_leave() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", &[]);
    let inbox = seed_inbox(&svc, account);
    let thread = ThreadId::new();
    let news = "news@list.example";
    let list = header("<mailto:leave@list.example>");
    ingest(&svc, inbox, 1, message(account, thread, news, "me@example.com", 5, list));
    // The person's own reply, from an address that is the same mailbox,
    // handed back by the list with its header on.
    let echoed = header("<mailto:me-leave@list.example>");
    ingest(&svc, inbox, 2, message(account, thread, "Me+lists@example.com", news, 1, echoed));

    let outcomes = call(&svc, "unsubscribe", json!({ "threads": [thread] })).await;
    let outcomes = outcomes_of(&outcomes);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0]["email"], "news@list.example");
    let drafts = svc.get().unwrap().drafts(account).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].to, vec![Address::bare("leave@list.example")]);

    let mine = call(
        &svc,
        "unsubscribe",
        json!({ "threads": [thread], "senders": ["me+lists@example.com"] }),
    )
    .await;
    assert_eq!(mine, json!([]), "named outright, still not somebody to leave");
}

#[tokio::test]
async fn each_account_a_sender_writes_to_is_asked_from_that_account() {
    let (svc, _dir) = service();
    let work = seed_account(&svc, "me@work.example", &[]);
    let home = seed_account(&svc, "me@home.example", &[]);
    let (work_inbox, home_inbox) = (seed_inbox(&svc, work), seed_inbox(&svc, home));
    let (at_work, at_home) = (ThreadId::new(), ThreadId::new());
    let link = || header("<mailto:leave@list.example>");
    let news = "news@list.example";
    ingest(&svc, work_inbox, 1, message(work, at_work, news, "me@work.example", 3, link()));
    ingest(&svc, home_inbox, 1, message(home, at_home, news, "me@home.example", 2, link()));

    let outcomes = call(&svc, "unsubscribe", json!({ "threads": [at_work, at_home] })).await;
    let outcomes = outcomes_of(&outcomes);
    assert_eq!(outcomes.len(), 2, "one per account: {outcomes:?}");
    assert!(outcomes.iter().all(|o| o["done"] == true && o["email"] == news), "{outcomes:?}");
    let accounts: Vec<&Value> = outcomes.iter().map(|o| &o["account"]).collect();
    assert!(accounts.contains(&&json!(work)) && accounts.contains(&&json!(home)), "{accounts:?}");

    let vault = svc.get().unwrap();
    for (account, address) in [(work, "me@work.example"), (home, "me@home.example")] {
        let drafts = vault.drafts(account).unwrap();
        assert_eq!(drafts.len(), 1, "{address}: {drafts:?}");
        assert_eq!(drafts[0].identity, address, "each from its own account");
    }
}

#[tokio::test]
async fn a_header_kept_only_in_the_message_itself_is_read_from_its_bytes() {
    let (svc, _dir) = service();
    let account = seed_account(&svc, "me@example.com", &[]);
    let inbox = seed_inbox(&svc, account);
    let raw = b"From: News <news@list.example>\r\n\
To: me@example.com\r\n\
Subject: Digest\r\n\
List-Unsubscribe: <https://list.example/u/abc>,\r\n <mailto:leave@list.example>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hello.\r\n";
    let packs = svc.packs().expect("the pack store opens whenever a vault does");
    let refs = packs.append_batch(&account.to_string(), &[raw.as_slice()]).unwrap();
    let thread = ThreadId::new();
    // Synced before headers were kept: nothing on the record.
    let mut old = message(account, thread, "news@list.example", "me@example.com", 2, None);
    old.pack = refs[0].clone();
    ingest(&svc, inbox, 1, old);

    let offer = call(&svc, "unsubscribe_offer", json!({ "thread": thread })).await;
    assert_eq!(offer["via"], "oneClick", "{offer}");
    assert_eq!(offer["to"], "list.example", "the host, never the link: {offer}");

    let senders = call(&svc, "inbox_senders", json!({})).await;
    assert_eq!(senders[0]["email"], "news@list.example");
    assert_eq!(senders[0]["unsubscribe"], "oneClick", "{senders}");
}

#[tokio::test]
async fn more_conversations_than_one_press_reads_are_refused() {
    let (svc, _dir) = service();
    let flood: Vec<ThreadId> = (0..5_001).map(|_| ThreadId::new()).collect();
    let err = svc
        .call(Ctx::local(), "unsubscribe", json!({ "threads": flood }))
        .await
        .expect_err("more than five thousand");
    assert_eq!(err.code, codes::INVALID, "{err:?}");

    let gone = call(&svc, "unsubscribe", json!({ "threads": [ThreadId::new()] })).await;
    assert_eq!(gone, json!([]), "a conversation that has gone is skipped, not an error");
}
