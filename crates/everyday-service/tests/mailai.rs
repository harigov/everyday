//! `everyday_service::mailai`'s three model-assisted features, end to end
//! against a fake model on loopback.
//!
//! The fake here always answers the one `submit` tool call it is scripted
//! with, for every connection it accepts, rather than `tests/routines.rs`'s
//! turn-numbered script: every call this module makes is a single
//! structured request (`everyday_service::quick::run_prompt`), never a
//! multi-turn conversation, so there is no second turn's text to script
//! separately.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use everyday_core::account::{Account, Provider};
use everyday_core::agent::LLMModelConfig;
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{
    Address, Body, Category, DraftState, Mailbox, MailboxRole, Message, MessageFlags, Origin,
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_service::Service;
use jiff::Timestamp;

// ---- a fake model on loopback ---------------------------------------------

struct FakeModel {
    endpoint: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

/// A model that answers every connection it accepts with a tool call to
/// `submit`, carrying `arguments_json`. See the module docs for why one
/// fixed answer, forever, is enough for every test here.
async fn fake_model(arguments_json: String) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let arguments_json = Arc::new(arguments_json);

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let arguments_json = arguments_json.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                let body = reply_tool_call("submit", &arguments_json);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), _shutdown: tx }
}

/// A single, whole Chat Completions JSON response -- not the streamed
/// `text/event-stream` shape `tests/routines.rs`'s fake speaks. The quick
/// harness (`everyday_service::quick::run_prompt`) calls rig's plain
/// `.prompt()`, a single non-streaming request, so this is what it expects
/// back.
fn reply_tool_call(tool: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "scripted",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": tool, "arguments": arguments },
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
    .to_string()
}

// ---- fixtures ---------------------------------------------------------

fn env(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut settings = everyday_core::AgentSettings { enabled: true, ..Default::default() };
    settings.provider_config.base_url = Some(endpoint.to_string());
    settings.quick_model = Some(LLMModelConfig {
        model: "scripted".into(),
        temperature: Some(0.0),
        max_tokens: Some(512),
    });
    settings.assistant_model.model = "scripted".into();
    vault.save_agent_settings(&settings).unwrap();
    (svc, dir)
}

fn seed_account(svc: &Arc<Service>, mail_ai: everyday_core::account::MailAi) -> Account {
    let vault = svc.get().unwrap();
    let settings = vault.agent_settings().unwrap();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    account.mail_ai = mail_ai;
    account.assistant_provider_acknowledged = Some(settings.provider_config.acknowledgement_name());
    vault.save_account(&account).unwrap();
    account
}

fn seed_mailbox(svc: &Arc<Service>, account: AccountId, role: MailboxRole) -> MailboxId {
    let vault = svc.get().unwrap();
    let mailbox = Mailbox::new(account, role.as_str(), role);
    vault.save_mailbox(&mailbox).unwrap();
    mailbox.id
}

/// Ingest one message directly -- bypassing the sync engine and its
/// categorisation hook entirely, the same shortcut
/// `tests/mail_agent_tools.rs` takes, so `category` here is exactly whatever
/// the fixture sets rather than whatever the rules would have computed.
#[allow(clippy::too_many_arguments)]
fn seed_message(
    svc: &Arc<Service>,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    thread_id: ThreadId,
    from: &str,
    to: &[&str],
    subject: &str,
    snippet: &str,
    category: Option<Category>,
) -> Message {
    let vault = svc.get().unwrap();
    let message = Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id,
        message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
        date: Timestamp::now(),
        from: Address::bare(from),
        to: to.iter().map(|a| Address::bare(*a)).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: snippet.into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category,
        invite: None,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 1 },
        gmail: None,
    };
    vault
        .ingest_mail(account, vec![IngestMessage { message: message.clone(), mailbox, uid }])
        .unwrap();
    vault
        .save_body(&Body {
            message_id: message.id,
            html_sanitised: format!("<p>{snippet}</p>"),
            text: snippet.to_string(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        })
        .unwrap();
    message
}

fn mail_ai(categorize: bool, summaries: bool, auto_draft: bool) -> everyday_core::account::MailAi {
    everyday_core::account::MailAi { categorize, summaries, auto_draft }
}

// ---- model-assisted categorisation ----------------------------------------

#[tokio::test]
async fn categorize_tick_applies_the_models_label_to_an_other_bucket_thread() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "stranger@example.com",
        &["me@example.com"],
        "Hello",
        "Just checking in.",
        Some(Category::Other),
    );

    everyday_service::mailai::categorize_tick(&svc).await;

    let updated = svc.get().unwrap().mail_message(msg.id).unwrap();
    assert_eq!(updated.category, Some(Category::Important));
}

#[tokio::test]
async fn categorize_tick_is_a_no_op_when_the_account_has_not_turned_it_on() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    // Every other switch on, but not this one.
    let account = seed_account(&svc, mail_ai(false, true, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "stranger@example.com",
        &["me@example.com"],
        "Hello",
        "Just checking in.",
        Some(Category::Other),
    );

    everyday_service::mailai::categorize_tick(&svc).await;

    let unchanged = svc.get().unwrap().mail_message(msg.id).unwrap();
    assert_eq!(unchanged.category, Some(Category::Other), "the switch was off");
}

// ---- summaries --------------------------------------------------------

#[tokio::test]
async fn summarize_thread_asks_the_configured_model_and_caches_the_answer() {
    let fake =
        fake_model(r#"{"summary":"They are asking about dinner on Friday."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, true, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &["me@example.com"],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    let summary = everyday_service::mailai::summarize_thread(&svc, msg.thread_id).await.unwrap();
    assert_eq!(summary, "They are asking about dinner on Friday.");

    // The model is gone; a cached answer must not need it.
    drop(fake);
    let cached = everyday_service::mailai::summarize_thread(&svc, msg.thread_id).await.unwrap();
    assert_eq!(cached, summary);
}

/// Reproduces finding 1 directly: repeated summaries of the *same* thread,
/// each forced to actually ask the model by ingesting one more message
/// first (so the cache -- keyed by `message_count` -- misses every time),
/// must not start refusing after twenty of them. Before the fix, the
/// constant turn string `"mail-summarize"` meant `RateLimitState`'s
/// per-turn counter for this thread's key never reset, so call 21 (the
/// per-turn cap is 20) and every one after it came back
/// `RATE_LIMITED` -- forever, since nothing ever looked like a new turn
/// again.
#[tokio::test]
async fn summarize_thread_survives_more_than_twenty_uncached_calls_on_one_thread() {
    let fake =
        fake_model(r#"{"summary":"They are asking about dinner on Friday."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, true, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    let mut msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        thread_id,
        "friend@example.com",
        &["me@example.com"],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    for i in 2..=25u32 {
        let summary =
            everyday_service::mailai::summarize_thread(&svc, thread_id).await.unwrap_or_else(|e| {
                panic!("call {i} (message_count now {i}) was refused: {}", e.message)
            });
        assert_eq!(summary, "They are asking about dinner on Friday.");

        // One more message, so the thread's `message_count` moves and the
        // next call is a genuine cache miss rather than a free hit.
        msg = seed_message(
            &svc,
            account.id,
            mailbox,
            i,
            thread_id,
            "friend@example.com",
            &["me@example.com"],
            "Dinner?",
            "Still free Friday?",
            Some(Category::Important),
        );
    }
    let _ = msg;
}

/// The other half of finding 1: once an answer is cached, repeating the
/// call must never touch the rate limiter at all -- proved here by
/// exhausting the limiter directly first, then showing a cached call still
/// succeeds.
#[tokio::test]
async fn a_cached_summary_never_touches_the_rate_limiter() {
    let fake =
        fake_model(r#"{"summary":"They are asking about dinner on Friday."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, true, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &["me@example.com"],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    let summary = everyday_service::mailai::summarize_thread(&svc, msg.thread_id).await.unwrap();

    // Exhaust this thread's own per-turn budget directly, the way the old,
    // buggy code would have on its own -- twenty calls on the same turn
    // string this thread's conversation key would always resolve to.
    let origin = Origin::Assistant { conversation: format!("mail-summarize:{}", msg.thread_id) };
    for _ in 0..20 {
        svc.check_mail_rate_limit(&origin, "mail-summarize").unwrap();
    }
    assert!(svc.check_mail_rate_limit(&origin, "mail-summarize").is_err(), "the budget is spent");

    // The model is gone, too, so a call that reached it would fail outright.
    drop(fake);
    let cached = everyday_service::mailai::summarize_thread(&svc, msg.thread_id).await.unwrap();
    assert_eq!(cached, summary, "a cache hit needs neither the limiter nor the model");
}
#[tokio::test]
async fn summarize_thread_refuses_when_summaries_are_not_switched_on() {
    let fake = fake_model(r#"{"summary":"anything"}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &["me@example.com"],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    let err = everyday_service::mailai::summarize_thread(&svc, msg.thread_id).await.unwrap_err();
    assert!(err.message.contains("AI summaries"), "{}", err.message);
}

// ---- auto-drafts ------------------------------------------------------

#[tokio::test]
async fn auto_draft_writes_at_most_one_draft_per_thread_and_never_a_send_op() {
    let fake =
        fake_model(r#"{"reply":true,"body_html":"<p>Sure, Friday works.</p>"}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &[account.address.as_str()],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    everyday_service::mailai::auto_draft_tick(&svc).await;
    let vault = svc.get().unwrap();
    let drafts = vault.drafts(account.id).unwrap();
    assert_eq!(drafts.len(), 1, "one auto-draft for the one eligible thread");
    let draft = &drafts[0];
    assert_eq!(draft.in_reply_to, Some(msg.id));
    assert_eq!(draft.state, DraftState::Editing);
    assert!(
        matches!(&draft.origin, Origin::Assistant { conversation } if conversation == "auto-draft")
    );

    // A second tick must not write a second draft for the same thread.
    everyday_service::mailai::auto_draft_tick(&svc).await;
    assert_eq!(vault.drafts(account.id).unwrap().len(), 1);

    // Never a Send op, from either tick.
    assert!(vault.ops_by_origin("assistant", 100).unwrap().is_empty());
}

#[tokio::test]
async fn auto_draft_skips_a_thread_the_person_has_already_started_a_draft_on() {
    let fake =
        fake_model(r#"{"reply":true,"body_html":"<p>Sure, Friday works.</p>"}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &[account.address.as_str()],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );
    let vault = svc.get().unwrap();
    let mut own_draft =
        everyday_core::mail::Draft::new(account.id, account.address.clone(), Origin::Person);
    own_draft.in_reply_to = Some(msg.id);
    vault.save_draft(&own_draft).unwrap();

    everyday_service::mailai::auto_draft_tick(&svc).await;

    let drafts = vault.drafts(account.id).unwrap();
    assert_eq!(drafts.len(), 1, "only the person's own draft; none added");
    assert_eq!(drafts[0].id, own_draft.id);
}

#[tokio::test]
async fn auto_draft_is_a_no_op_when_the_account_has_not_turned_it_on() {
    let fake =
        fake_model(r#"{"reply":true,"body_html":"<p>Sure, Friday works.</p>"}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, true, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &[account.address.as_str()],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    everyday_service::mailai::auto_draft_tick(&svc).await;

    assert!(svc.get().unwrap().drafts(account.id).unwrap().is_empty());
}
