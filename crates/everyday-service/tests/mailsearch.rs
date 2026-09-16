//! `search_mail` and `suggest_addresses`, end to end through the command
//! layer against a real vault, its real (if tiny) search index and its
//! real contact index.

use std::sync::Arc;

use everyday_core::MailDoc;
use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, PackId, ThreadId};
use everyday_core::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use jiff::Timestamp;
use serde_json::{Value, json};

#[allow(dead_code)]
mod support;

fn service() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn seed_account(svc: &Arc<Service>) -> AccountId {
    let vault = svc.get().unwrap();
    let account = Account::new(Provider::Custom, "me@example.com");
    let id = account.id;
    vault.save_account(&account).unwrap();
    id
}

/// One thread, one message, one mailbox row -- and the matching [`MailDoc`]
/// indexed and committed, so both `vault.thread` and `search_mail`'s own
/// index lookup find the same thing. Returns the thread id.
fn seed_searchable_thread(
    svc: &Arc<Service>,
    account: AccountId,
    subject: &str,
    body_text: &str,
) -> ThreadId {
    let vault = svc.get().unwrap();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    vault.save_mailbox(&mailbox).unwrap();

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
        subject: subject.into(),
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
    vault
        .ingest_mail(
            account,
            vec![IngestMessage { message: message.clone(), mailbox: mailbox.id, uid: 1 }],
        )
        .unwrap();

    let index = svc.mail_index().unwrap();
    index
        .index(&[MailDoc {
            message_key: message_id.to_string(),
            thread_key: thread_id.to_string(),
            account: account.to_string(),
            mailboxes: vec![mailbox.id.to_string()],
            from: "sender@example.com".into(),
            to: "me@example.com".into(),
            cc: String::new(),
            subject: subject.to_string(),
            body_text: body_text.to_string(),
            labels: Vec::new(),
            date: message.date,
            has_attachment: false,
            unread: true,
            starred: false,
        }])
        .unwrap();
    index.commit().unwrap();

    thread_id
}

#[tokio::test]
async fn search_mail_finds_a_thread_by_body_text() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let thread_id =
        seed_searchable_thread(&svc, account, "Quarterly report", "the marmalade figures are in");

    let result = call(&svc, "search_mail", json!({ "query": "marmalade" })).await;
    let threads = result["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1, "{result}");
    assert_eq!(threads[0]["id"], thread_id.to_string());
}

#[tokio::test]
async fn search_mail_finds_nothing_for_an_unrelated_word() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_searchable_thread(&svc, account, "Quarterly report", "the marmalade figures are in");

    let result = call(&svc, "search_mail", json!({ "query": "giraffe" })).await;
    assert!(result["threads"].as_array().unwrap().is_empty(), "{result}");
}

#[tokio::test]
async fn search_mail_honours_an_account_filter() {
    let (svc, _dir) = service();
    let account_a = seed_account(&svc);
    let account_b = {
        let vault = svc.get().unwrap();
        let account = Account::new(Provider::Custom, "someone-else@example.com");
        let id = account.id;
        vault.save_account(&account).unwrap();
        id
    };
    seed_searchable_thread(&svc, account_a, "From account A", "shared word appears here");
    seed_searchable_thread(&svc, account_b, "From account B", "shared word appears here too");

    let all = call(&svc, "search_mail", json!({ "query": "shared" })).await;
    assert_eq!(all["threads"].as_array().unwrap().len(), 2, "{all}");

    let scoped =
        call(&svc, "search_mail", json!({ "query": "shared", "accountIds": [account_a] })).await;
    let scoped_threads = scoped["threads"].as_array().unwrap();
    assert_eq!(scoped_threads.len(), 1, "{scoped}");
}

/// Regression for "`in:<mailbox>` can never match": `everyday-mailindex`'s
/// query translation matches `Op::In` against the indexed `mailboxes`
/// field as a raw term, and `seed_searchable_thread` indexes under the
/// mailbox's real id -- a bare UUID, exactly like the real sync passes do
/// -- never the role name `"inbox"` a person types. Before
/// `resolve_mailbox_names` existed, `in:inbox` built a query for the
/// literal term `"inbox"`, which nothing was ever indexed as.
#[tokio::test]
async fn search_mail_resolves_in_mailbox_name_to_its_id() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let thread_id =
        seed_searchable_thread(&svc, account, "Quarterly report", "the marmalade figures are in");

    let result = call(&svc, "search_mail", json!({ "query": "in:inbox" })).await;
    let threads = result["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1, "{result}");
    assert_eq!(threads[0]["id"], thread_id.to_string());
}

/// A name matching no mailbox at all must keep behaving exactly like
/// before this fix -- no matches, not an error a search box would have to
/// explain.
#[tokio::test]
async fn search_mail_in_an_unrecognised_mailbox_name_finds_nothing() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_searchable_thread(&svc, account, "Quarterly report", "the marmalade figures are in");

    let result = call(&svc, "search_mail", json!({ "query": "in:not-a-real-mailbox" })).await;
    assert!(result["threads"].as_array().unwrap().is_empty(), "{result}");
}

/// A name matching more than one mailbox -- here, two different accounts'
/// own inboxes -- must reach every one of them, not only the first one
/// resolution happened to find.
#[tokio::test]
async fn search_mail_in_inbox_reaches_every_accounts_own_inbox() {
    let (svc, _dir) = service();
    let account_a = seed_account(&svc);
    let account_b = {
        let vault = svc.get().unwrap();
        let account = Account::new(Provider::Custom, "someone-else@example.com");
        let id = account.id;
        vault.save_account(&account).unwrap();
        id
    };
    seed_searchable_thread(&svc, account_a, "From account A", "shared word appears here");
    seed_searchable_thread(&svc, account_b, "From account B", "shared word appears here too");

    let result = call(&svc, "search_mail", json!({ "query": "in:inbox" })).await;
    assert_eq!(result["threads"].as_array().unwrap().len(), 2, "{result}");
}

#[tokio::test]
async fn suggest_addresses_matches_contacts_by_prefix() {
    let (svc, _dir) = service();
    let contacts = svc.mail_contacts().expect("the contact index opens with the vault");
    contacts.record_sent_to("alice@example.com", "Alice Anderson");
    contacts.persist_if_dirty(&svc.get().unwrap());

    let result = call(&svc, "suggest_addresses", json!({ "prefix": "alic" })).await;
    let suggestions = result.as_array().unwrap();
    assert_eq!(suggestions.len(), 1, "{result}");
    assert_eq!(suggestions[0]["email"], "alice@example.com");
}

#[tokio::test]
async fn suggest_addresses_with_no_prefix_returns_the_most_written_to() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let contacts = svc.mail_contacts().unwrap();
    contacts.record_received_from("quiet@example.com", "Quiet");
    contacts.record_sent_to("frequent@example.com", "Frequent");
    contacts.record_sent_to("frequent@example.com", "Frequent");
    contacts.persist_if_dirty(&vault);

    let result = call(&svc, "suggest_addresses", json!({ "prefix": "" })).await;
    let suggestions = result.as_array().unwrap();
    assert_eq!(suggestions[0]["email"], "frequent@example.com", "{result}");
}

/// The plan's own speed budget: "a search: 150ms for the first page."
/// Ignored by default -- it indexes a few thousand synthetic documents
/// first, which is more than an ordinary `cargo test` run should pay for --
/// run explicitly with `cargo test --release -- --ignored`.
#[tokio::test]
#[ignore = "indexes a synthetic corpus; run explicitly to check the 150ms search budget"]
async fn search_mail_meets_the_150ms_budget() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let mailbox = {
        let vault = svc.get().unwrap();
        let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();
        mailbox.id
    };

    let index = svc.mail_index().unwrap();
    const N: usize = 20_000;
    let mut docs = Vec::with_capacity(N);
    for i in 0..N {
        docs.push(MailDoc {
            message_key: MailMessageId::new().to_string(),
            thread_key: ThreadId::new().to_string(),
            account: account.to_string(),
            mailboxes: vec![mailbox.to_string()],
            from: format!("sender{i}@example.com"),
            to: "me@example.com".into(),
            cc: String::new(),
            subject: format!("Report number {i}"),
            body_text: format!("the quarterly marmalade figures for batch {i} are attached"),
            labels: Vec::new(),
            date: Timestamp::now(),
            has_attachment: false,
            unread: i % 3 == 0,
            starred: false,
        });
    }
    for chunk in docs.chunks(1000) {
        index.index(chunk).unwrap();
    }
    index.commit().unwrap();

    // The synthetic docs above are indexed but deliberately never ingested
    // as real `Thread`/`Message` rows (twenty thousand of those would make
    // this bench about vault-write time, not search time) -- so
    // `search_mail`'s own `vault.thread` lookup finds nothing for any of
    // them and every hit is silently dropped. That is fine here: this bench
    // is about `MailSearch::search`'s own latency, which the timing below
    // measures regardless of what the thread-loading step finds afterwards.
    let started = std::time::Instant::now();
    let _ = call(&svc, "search_mail", json!({ "query": "marmalade batch" })).await;
    let elapsed = started.elapsed();
    eprintln!("search_mail over {N} docs took {elapsed:?}");
    assert!(elapsed.as_millis() < 150, "search_mail took {elapsed:?}, over the 150ms budget");
}
