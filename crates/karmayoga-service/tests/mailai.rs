//! `karmayoga_service::mail::ai`'s three model-assisted features, end to end
//! against a fake model on loopback.
//!
//! The fake here always answers the one `submit` tool call it is scripted
//! with, for every connection it accepts, rather than `tests/routines.rs`'s
//! turn-numbered script: every call this module makes is a single
//! structured request (`karmayoga_service::quick::run_prompt`), never a
//! multi-turn conversation, so there is no second turn's text to script
//! separately.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use jiff::Timestamp;
use karmayoga_core::account::{Account, Provider};
use karmayoga_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use karmayoga_core::llm::{LLMConfig, LLMModelConfig, ModelSettings};
use karmayoga_core::mail::{
    Address, Body, Category, CategorySource, DraftState, Mailbox, MailboxRole, Message,
    MessageFlags, Origin,
};
use karmayoga_core::packstore::PackRef;
use karmayoga_core::store::mail::{IngestMessage, ThreadFilter};
use karmayoga_service::{Change, EventSink, Kind, Service};

// ---- a fake model on loopback ---------------------------------------------

struct FakeModel {
    endpoint: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
    /// Every connection this model accepted -- one per `quick::run_prompt`
    /// call -- for the tests that need to know not just *what* the model
    /// was told but *how many times* it was ever asked at all (findings 4
    /// and 5: a real, paid model call is exactly the thing the per-minute
    /// budget exists to bound).
    calls: Arc<std::sync::atomic::AtomicU32>,
    /// What every request said, as text -- the system prompt and the message --
    /// for the tests that check what a model was *told*.
    requests: Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeModel {
    fn call_count(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every question asked so far, one request body each. The harness's second
    /// turn -- the same prompt with the tool's result added, for the model's own
    /// closing words -- is the same question again, and is left out.
    fn requests(&self) -> Vec<String> {
        let all = self.requests.lock().unwrap().clone();
        all.into_iter().filter(|r| !r.contains("\"role\":\"tool\"")).collect()
    }
}

/// A model that answers every connection it accepts with a tool call to
/// `submit`, carrying `arguments_json`. See the module docs for why one
/// fixed answer, forever, is enough for every test here.
async fn fake_model(arguments_json: String) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let arguments_json = Arc::new(arguments_json);
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let calls_for_task = calls.clone();
    let requests: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let requests_for_task = requests.clone();

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let arguments_json = arguments_json.clone();
            let requests = requests_for_task.clone();
            calls_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // Headers, then exactly Content-Length bytes of body, so that a
                // prompt that does not fit in one read is seen whole.
                let mut raw = Vec::new();
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + length {
                        break;
                    }
                }
                requests.lock().unwrap().push(String::from_utf8_lossy(&raw).to_string());
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

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), _shutdown: tx, calls, requests }
}

/// A single, whole Chat Completions JSON response -- not the streamed
/// `text/event-stream` shape `tests/routines.rs`'s fake speaks. The quick
/// harness (`karmayoga_service::quick::run_prompt`) calls rig's plain
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
    let settings = karmayoga_core::AgentSettings { enabled: true, ..Default::default() };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    models.small = Some(LLMConfig {
        connection: models.medium.connection.clone(),
        model: LLMModelConfig {
            model: "scripted".into(),
            temperature: Some(0.0),
            max_tokens: Some(512),
            reasoning_effort: None,
        },
    });
    vault.save_model_settings(&models).unwrap();
    (svc, dir)
}

fn seed_account(svc: &Arc<Service>, mail_ai: karmayoga_core::account::MailAi) -> Account {
    let vault = svc.get().unwrap();
    let models = vault.model_settings().unwrap();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    account.mail_ai = mail_ai;
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
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
    seed_message_at(
        svc,
        account,
        mailbox,
        uid,
        thread_id,
        from,
        to,
        subject,
        snippet,
        category,
        Timestamp::now(),
    )
}

/// [`seed_message`], dated `date`.
#[allow(clippy::too_many_arguments)]
fn seed_message_at(
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
    date: Timestamp,
) -> Message {
    let vault = svc.get().unwrap();
    let message = Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id,
        message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
        date,
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
        category_source: CategorySource::Rules,
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

fn mail_ai(categorize: bool, summaries: bool, auto_draft: bool) -> karmayoga_core::account::MailAi {
    karmayoga_core::account::MailAi { categorize, summaries, auto_draft, writing: false }
}

// ---- model-assisted categorisation ----------------------------------------

/// How many `Thread` changes went out: what tells an open list to redraw.
#[derive(Default)]
struct ThreadChanges(std::sync::Mutex<usize>);

impl EventSink for ThreadChanges {
    fn changed(&self, change: Change) {
        if change.kind == Kind::Thread {
            *self.0.lock().unwrap() += 1;
        }
    }
}

impl ThreadChanges {
    fn count(&self) -> usize {
        *self.0.lock().unwrap()
    }
}

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
    let changes = Arc::new(ThreadChanges::default());
    svc.set_events(changes.clone());

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let updated = svc.get().unwrap().mail_message(msg.id).unwrap();
    assert_eq!(updated.category, Some(Category::Important));
    assert_eq!(changes.count(), 1, "an open list is told it moved tabs");
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

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let unchanged = svc.get().unwrap().mail_message(msg.id).unwrap();
    assert_eq!(unchanged.category, Some(Category::Other), "the switch was off");
}

/// Point every account's agent settings at a different fake model, in
/// place -- what the finding-2 tests below use to prove a second tick never
/// asked at all: if it had, this new model's very different answer would
/// show up in the result, and it never does.
///
/// Re-acknowledges `account` for the new endpoint in the same call:
/// `ModelSettings::acknowledgement_name` is the endpoints themselves (see
/// its own docs), so changing `base_url` without this would make
/// `mail_ai_allowed` refuse every account all over again, for a reason that
/// has nothing to do with what these tests are proving. Both the medium and
/// the small model move, as they did when they shared one connection.
fn repoint_model(svc: &Arc<Service>, account: &mut Account, endpoint: &str) {
    let vault = svc.get().unwrap();
    let mut models = vault.model_settings().unwrap();
    models.medium.connection.base_url = Some(endpoint.to_string());
    if let Some(small) = &mut models.small {
        small.connection.base_url = Some(endpoint.to_string());
    }
    vault.save_model_settings(&models).unwrap();
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(account).unwrap();
}

/// Finding 2: a thread the model has already labelled "other" must not be
/// asked again next tick. Proved the same way `repoint_model` is meant to
/// be used everywhere below: the *second* tick is pointed at a model that
/// would answer differently, and the thread's category must not move,
/// because nothing should ever have asked it.
#[tokio::test]
async fn categorize_tick_does_not_reask_a_thread_it_already_labelled_other() {
    let still_other =
        fake_model(r#"{"labels":[{"index":1,"category":"other"}]}"#.to_string()).await;
    let (svc, _dir) = env(&still_other.endpoint);
    let mut account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        thread_id,
        "stranger@example.com",
        &["me@example.com"],
        "Hello",
        "Just checking in.",
        Some(Category::Other),
    );

    let changes = Arc::new(ThreadChanges::default());
    svc.set_events(changes.clone());
    karmayoga_service::mail::ai::categorize_tick(&svc).await;
    let (thread, _) = svc.get().unwrap().thread(thread_id).unwrap();
    assert_eq!(thread.ai_categorize_asked_at_count, Some(1), "asked once, and recorded as such");
    assert_eq!(svc.get().unwrap().mail_message(msg.id).unwrap().category, Some(Category::Other));
    assert_eq!(changes.count(), 0, "kept where it was, so no list has anything to redraw");

    // A model that would say "important" this time, if it were ever asked.
    let would_promote =
        fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    repoint_model(&svc, &mut account, &would_promote.endpoint);
    wait_for_categorize_budget().await;

    karmayoga_service::mail::ai::categorize_tick(&svc).await;
    assert_eq!(
        svc.get().unwrap().mail_message(msg.id).unwrap().category,
        Some(Category::Other),
        "unasked, since nothing about this thread changed"
    );
}

/// `Service::mail_categorize_take` spends up to `CATEGORIZE_PAGE` tokens
/// from a per-*minute* budget on every call, win or not -- fine for the
/// real minute scheduler it is built for, since a whole minute passes
/// between ticks there and the bucket is full again regardless, but a
/// second `categorize_tick` called immediately after the first, as these
/// tests do, would otherwise find the budget still spent and refuse to
/// fetch a page at all -- which would make a test pass by doing nothing,
/// not by proving the fix. Waiting for one token's worth of real refill
/// (`60 / MAIL_CATEGORIZE_PER_MINUTE` seconds) is the honest way to give
/// the second tick the one thread's worth of budget these tests need it to
/// actually have.
async fn wait_for_categorize_budget() {
    let seconds = 60.0 / f64::from(Service::MAIL_CATEGORIZE_PER_MINUTE);
    tokio::time::sleep(std::time::Duration::from_secs_f64(seconds + 0.5)).await;
}

/// The other half: a new message arriving does move `message_count` past
/// the marker, so the *next* tick does ask again.
#[tokio::test]
async fn categorize_tick_reasks_a_thread_once_a_new_message_arrives() {
    let still_other =
        fake_model(r#"{"labels":[{"index":1,"category":"other"}]}"#.to_string()).await;
    let (svc, _dir) = env(&still_other.endpoint);
    let mut account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        thread_id,
        "stranger@example.com",
        &["me@example.com"],
        "Hello",
        "Just checking in.",
        Some(Category::Other),
    );
    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let would_promote =
        fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    repoint_model(&svc, &mut account, &would_promote.endpoint);
    wait_for_categorize_budget().await;
    let msg2 = seed_message(
        &svc,
        account.id,
        mailbox,
        2,
        thread_id,
        "stranger@example.com",
        &["me@example.com"],
        "Hello",
        "Following up -- still checking in.",
        Some(Category::Other),
    );

    karmayoga_service::mail::ai::categorize_tick(&svc).await;
    assert_eq!(
        svc.get().unwrap().mail_message(msg2.id).unwrap().category,
        Some(Category::Important),
        "a new message makes the thread eligible again"
    );
}

/// The requests that asked a model to label threads: the categoriser's own,
/// not the follow-up questions the same tick goes on to put to the same model.
fn labelling(fake: &FakeModel) -> Vec<String> {
    fake.requests().into_iter().filter(|r| r.contains("You sort email into the tabs")).collect()
}

/// Finding 2's own "walk further back" requirement: once a page's worth of
/// threads are all already asked, the tick must still move on past them rather
/// than reading that same, exhausted page forever.
///
/// The newest page is read first every tick, and the walk -- from its own
/// cursor, behind that page -- gets whatever room is left: so with a newest page
/// of threads all asked and one older thread behind it, the older thread is
/// asked in the very same tick, not a minute later. With two pages of asked
/// threads in front of it, the first tick reads the newest page and the second
/// page of the walk, leaves the cursor behind them, and the cursor it leaves
/// reaches the older thread -- exactly what the *next* tick, whenever its own
/// budget allows, would see. (A second real tick cannot prove that within one
/// test: the per-minute budget a fresh page costs is exactly what the first tick
/// just spent, and refilling it for real would mean this test sleeping for
/// however long that takes.)
///
/// Seeded to [`mail::ai::CATEGORIZE_PAGE`]: since the fix for finding 5 (one
/// account's tick could spend the whole per-minute budget on a guess at its page
/// size, starving every account after it), `categorize_account` reads a
/// fixed-size page independent of the budget and only spends the budget on
/// however many of that page turn out to need asking.
#[tokio::test]
async fn categorize_tick_walks_past_an_already_asked_page_to_reach_older_threads() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let vault = svc.get().unwrap();

    // Two pages' worth of threads, all already marked asked at their current
    // message count, plus one older thread left unasked.
    let page = karmayoga_service::mail::ai::CATEGORIZE_PAGE;
    let mut oldest = None;
    for i in 0..=(2 * page) {
        let thread_id = ThreadId::new();
        let message = seed_message(
            &svc,
            account.id,
            mailbox,
            i + 1,
            thread_id,
            "stranger@example.com",
            &["me@example.com"],
            "Hello",
            "Just checking in.",
            Some(Category::Other),
        );
        if i == 0 {
            // Ingested first, so the oldest -- everything ingested after it
            // fills the newer pages and is marked asked below.
            oldest = Some((thread_id, message.id));
        } else {
            vault.set_thread_ai_categorize_asked(thread_id, 1).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let (oldest_id, _) = oldest.unwrap();

    karmayoga_service::mail::ai::categorize_tick(&svc).await;
    let cursor = svc.mail_categorize_cursor(account.id);
    assert!(cursor.is_some(), "an exhausted page still advances the cursor, not just a hit page");
    assert!(
        labelling(&fake).is_empty(),
        "nothing was unasked on the pages read: no labelling request was made"
    );

    let filter = ThreadFilter { category: Some(Category::Other), ..Default::default() };
    let next_page = vault.list_threads(mailbox, &filter, cursor.as_deref(), page).unwrap();
    assert!(
        next_page.threads.iter().any(|t| t.id == oldest_id),
        "the stored cursor reaches the older, unasked thread"
    );
}

/// The same with one page of asked threads and an older one behind it: the walk
/// has room on the same tick, so the older thread is asked at once.
#[tokio::test]
async fn categorize_tick_reaches_a_thread_behind_the_newest_page_on_the_same_tick() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let vault = svc.get().unwrap();

    let page = karmayoga_service::mail::ai::CATEGORIZE_PAGE;
    let mut oldest = None;
    for i in 0..=page {
        let thread_id = ThreadId::new();
        let message = seed_message(
            &svc,
            account.id,
            mailbox,
            i + 1,
            thread_id,
            "stranger@example.com",
            &["me@example.com"],
            "Hello",
            "Just checking in.",
            Some(Category::Other),
        );
        if i == 0 {
            oldest = Some(message.id);
        } else {
            vault.set_thread_ai_categorize_asked(thread_id, 1).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    assert_eq!(
        vault.mail_message(oldest.unwrap()).unwrap().category,
        Some(Category::Important),
        "asked on the tick that read the newest page, which left room"
    );
}

/// New mail is read before the archive is: with more unasked threads than one
/// request takes, it is the newest that go, whatever the walk's cursor says.
#[tokio::test]
async fn categorize_tick_asks_about_the_newest_mail_first() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let vault = svc.get().unwrap();

    // Thirty unasked threads, oldest first; the model labels only the first it
    // is shown, which must be the newest. (The fake is on loopback, so it is asked
    // five at a time, as a model on this computer is.)
    let mut ids = Vec::new();
    for i in 0..30u32 {
        ids.push(
            seed_message(
                &svc,
                account.id,
                mailbox,
                i + 1,
                ThreadId::new(),
                "stranger@example.com",
                &["me@example.com"],
                &format!("Hello {i}"),
                "Just checking in.",
                Some(Category::Other),
            )
            .id,
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    assert_eq!(
        vault.mail_message(*ids.last().unwrap()).unwrap().category,
        Some(Category::Important),
        "the newest message was number 1 in the list the model saw"
    );
    assert_eq!(vault.mail_message(ids[0]).unwrap().category, Some(Category::Other));
    let requests = labelling(&fake);
    assert_eq!(requests.len(), 1, "one request for the one tick");
    let prompt = &requests[0];
    assert!(prompt.contains("Hello 29") && prompt.contains("Hello 25"), "{prompt}");
    assert!(
        !prompt.contains("Hello 24"),
        "no more than a small model on this computer takes: {prompt}"
    );
    assert!(prompt.find("Hello 29") < prompt.find("Hello 28"), "newest first: {prompt}");
}

/// The model labelled one thread of two and left the other out. Both were asked
/// about, so both are marked: a thread a model keeps leaving out must not go back
/// into every request, every tick, and take a Small-model slot for ever. It
/// stays where it was until a new message arrives.
#[tokio::test]
async fn categorize_tick_marks_every_thread_it_asked_about_even_one_the_model_left_out() {
    let fake = fake_model(r#"{"labels":[{"index":2,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let (first, second) = (ThreadId::new(), ThreadId::new());
    let mut messages = Vec::new();
    for (i, thread_id) in [first, second].into_iter().enumerate() {
        messages.push(seed_message(
            &svc,
            account.id,
            mailbox,
            i as u32 + 1,
            thread_id,
            "stranger@example.com",
            &["me@example.com"],
            "Hello",
            "Just checking in.",
            Some(Category::Other),
        ));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    // Newest first: `second` was number 1 and was left out, `first` number 2.
    let vault = svc.get().unwrap();
    assert_eq!(vault.mail_message(messages[0].id).unwrap().category, Some(Category::Important));
    assert_eq!(
        vault.mail_message(messages[1].id).unwrap().category,
        Some(Category::Other),
        "no label, so it stays where it was"
    );
    assert_eq!(vault.thread(first).unwrap().0.ai_categorize_asked_at_count, Some(1));
    assert_eq!(
        vault.thread(second).unwrap().0.ai_categorize_asked_at_count,
        Some(1),
        "asked about, and so marked: not put to the model again until something new arrives"
    );
}

/// What the model is shown of each thread is the app's own record of it, then
/// the sender's words; and a thread in the archive, or from before the window,
/// is never put to it.
#[tokio::test]
async fn categorize_tick_tells_the_model_how_it_was_addressed_and_leaves_old_mail_alone() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"other"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let inbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let archive = seed_mailbox(&svc, account.id, MailboxRole::Archive);
    let old = Timestamp::now() - jiff::SignedDuration::from_hours(24 * 60);
    let old_one = seed_message_at(
        &svc,
        account.id,
        inbox,
        1,
        ThreadId::new(),
        "stranger@example.com",
        &["me@example.com"],
        "From long ago",
        "A deadline that passed.",
        Some(Category::Other),
        old,
    );
    let filed_away = seed_message(
        &svc,
        account.id,
        archive,
        1,
        ThreadId::new(),
        "stranger@example.com",
        &["me@example.com"],
        "In the archive",
        "Filed away.",
        Some(Category::Other),
    );
    // To a tagged address of the person's: still to them.
    seed_message(
        &svc,
        account.id,
        inbox,
        2,
        ThreadId::new(),
        "stranger@example.com",
        &["me+shop@example.com"],
        "Hello",
        "Just checking in.",
        Some(Category::Other),
    );

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let requests = labelling(&fake);
    assert_eq!(requests.len(), 1, "one request, about one thread: {requests:?}");
    let prompt = &requests[0];
    assert!(prompt.contains("addressed to you: directly"), "{prompt}");
    assert!(prompt.contains("you have written to the sender: no"), "{prompt}");
    assert!(
        fake.requests()
            .iter()
            .all(|r| !r.contains("From long ago") && !r.contains("In the archive")),
        "neither was put to any model: {:?}",
        fake.requests()
    );
    let vault = svc.get().unwrap();
    assert_eq!(vault.thread(old_one.thread_id).unwrap().0.ai_categorize_asked_at_count, None);
    assert_eq!(vault.thread(filed_away.thread_id).unwrap().0.ai_categorize_asked_at_count, None);
}

// ---- model-assisted prioritisation (rides along in `categorize_tick`) -----

/// A direct ask, already `Important` (so `categorize_account` itself has
/// nothing to do and makes no call of its own -- isolating this test to
/// `prioritize_account`'s own one), gets moved into `Category::Priority`
/// when the model says it needs the person's attention soon.
#[tokio::test]
async fn categorize_tick_marks_a_direct_ask_as_priority() {
    let fake = fake_model(r#"{"priorities":[{"index":1,"priority":true}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "boss@example.com",
        &["me@example.com"],
        "Need your sign-off",
        "Can you approve this by end of day?",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let updated = svc.get().unwrap().mail_message(msg.id).unwrap();
    assert_eq!(updated.category, Some(Category::Priority));
}

/// The other answer: an `Important` thread the model says does not need
/// attention soon is left exactly where it was.
#[tokio::test]
async fn categorize_tick_leaves_an_important_thread_alone_when_priority_says_no() {
    let fake = fake_model(r#"{"priorities":[{"index":1,"priority":false}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "newsletter@example.com",
        &["me@example.com"],
        "This week's digest",
        "Here is what happened this week.",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let updated = svc.get().unwrap().mail_message(msg.id).unwrap();
    assert_eq!(updated.category, Some(Category::Important), "the model said no priority");
}

/// As the categorisation half's own "does not reask" test: a thread the
/// priority pass has already asked about, with nothing new since, must
/// not be asked again -- proved by repointing the model between ticks at
/// one that would answer differently, the same way `repoint_model` is
/// used throughout this file.
#[tokio::test]
async fn categorize_tick_does_not_reask_a_thread_it_already_answered_for_priority() {
    let says_no = fake_model(r#"{"priorities":[{"index":1,"priority":false}]}"#.to_string()).await;
    let (svc, _dir) = env(&says_no.endpoint);
    let mut account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    let msg = seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        thread_id,
        "boss@example.com",
        &["me@example.com"],
        "Need your sign-off",
        "Can you approve this by end of day?",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::categorize_tick(&svc).await;
    let (thread, _) = svc.get().unwrap().thread(thread_id).unwrap();
    assert_eq!(thread.ai_priority_asked_at_count, Some(1), "asked once, and recorded as such");
    assert_eq!(
        svc.get().unwrap().mail_message(msg.id).unwrap().category,
        Some(Category::Important)
    );

    let would_mark =
        fake_model(r#"{"priorities":[{"index":1,"priority":true}]}"#.to_string()).await;
    repoint_model(&svc, &mut account, &would_mark.endpoint);
    wait_for_categorize_budget().await;

    karmayoga_service::mail::ai::categorize_tick(&svc).await;
    assert_eq!(
        svc.get().unwrap().mail_message(msg.id).unwrap().category,
        Some(Category::Important),
        "unasked, since nothing about this thread changed"
    );
}

/// What the priority model is told: today's date first, then for each thread
/// the app's own records (when it arrived, how it was addressed, whether the
/// person has written to the sender) and only then the sender's words -- and a
/// thread from before the window is not put to it.
#[tokio::test]
async fn the_priority_model_is_told_the_date_and_what_the_app_knows_of_each_thread() {
    let fake = fake_model(r#"{"priorities":[{"index":1,"priority":false}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(true, false, false));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    seed_message_at(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "boss@example.com",
        &["me@example.com"],
        "A deadline from last spring",
        "Can you approve this by Friday?",
        Some(Category::Important),
        Timestamp::now() - jiff::SignedDuration::from_hours(24 * 90),
    );
    seed_message(
        &svc,
        account.id,
        mailbox,
        2,
        ThreadId::new(),
        "boss@example.com",
        &["me@example.com"],
        "Need your sign-off",
        "Can you approve this by Friday?",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    let asked: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.contains("You choose which threads go on the person"))
        .collect();
    assert_eq!(asked.len(), 1, "one request, about the thread inside the window");
    let request = &asked[0];
    let today = jiff::Zoned::now().strftime("Today is %A %-d %B %Y.").to_string();
    assert!(request.contains(&today), "{request}");
    for record in [
        "(today)",
        "addressed to you: directly",
        "you have written to the sender: no",
        "you wrote earlier in this thread: no",
        "messages in thread: 1",
    ] {
        assert!(request.contains(record), "{record}: {request}");
    }
    assert!(request.contains("Need your sign-off"), "{request}");
    assert!(
        !request.contains("last spring"),
        "a 2024-style deadline is never put to it: {request}"
    );
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

    let summary = karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap();
    assert_eq!(summary, "They are asking about dinner on Friday.");

    // What the model was told: today's date, the subject (which often carries the
    // ask and the deadline), and the day the message was written.
    let request = &fake.requests()[0];
    let today = jiff::Zoned::now().strftime("Today is %A %-d %B %Y.").to_string();
    assert!(request.contains(&today), "{today} is opened with: {request}");
    assert!(request.contains("Subject: Dinner?"), "{request}");
    assert!(request.contains("wrote, to you, on "), "{request}");

    // The model is gone; a cached answer must not need it.
    drop(fake);
    let cached = karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap();
    assert_eq!(cached, summary);
}

/// A model that writes nothing has not summarised the thread: the panel is told
/// so, and the empty answer is not kept for the session -- the next ask goes to
/// the model again.
#[tokio::test]
async fn an_empty_summary_is_an_error_and_is_not_cached() {
    let fake = fake_model(r#"{"summary":"  "}"#.to_string()).await;
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

    let error =
        karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap_err();
    assert!(error.message.contains("wrote no summary"), "{}", error.message);
    let asked = fake.call_count();
    assert!(asked > 0);
    assert!(karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.is_err());
    assert!(fake.call_count() > asked, "asked again, since nothing was kept");
}

/// A thread with nothing written in it is a sentence, not an empty paragraph --
/// and no request.
#[tokio::test]
async fn a_thread_with_nothing_in_it_is_summarised_as_a_sentence() {
    let fake = fake_model(r#"{"summary":"Should never be asked."}"#.to_string()).await;
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
        "Photo",
        "",
        Some(Category::Important),
    );

    let summary = karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap();
    assert_eq!(summary, "There is nothing written in this thread to summarise.");
    assert_eq!(fake.call_count(), 0);
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
            karmayoga_service::mail::ai::summarize_thread(&svc, thread_id).await.unwrap_or_else(
                |e| panic!("call {i} (message_count now {i}) was refused: {}", e.message),
            );
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

    let summary = karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap();

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
    let cached = karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap();
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

    let err = karmayoga_service::mail::ai::summarize_thread(&svc, msg.thread_id).await.unwrap_err();
    assert!(err.message.contains("AI summaries"), "{}", err.message);
}

// ---- auto-drafts ------------------------------------------------------

#[tokio::test]
async fn auto_draft_writes_at_most_one_draft_per_thread_and_never_a_send_op() {
    let fake = fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
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

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;
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
    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;
    assert_eq!(vault.drafts(account.id).unwrap().len(), 1);

    // Never a Send op, from either tick.
    assert!(vault.ops_by_origin("assistant", 100).unwrap().is_empty());
}

#[tokio::test]
async fn auto_draft_skips_a_thread_the_person_has_already_started_a_draft_on() {
    let fake = fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
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
        karmayoga_core::mail::Draft::new(account.id, account.address.clone(), Origin::Person);
    own_draft.in_reply_to = Some(msg.id);
    vault.save_draft(&own_draft).unwrap();

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    let drafts = vault.drafts(account.id).unwrap();
    assert_eq!(drafts.len(), 1, "only the person's own draft; none added");
    assert_eq!(drafts[0].id, own_draft.id);
}

#[tokio::test]
async fn auto_draft_is_a_no_op_when_the_account_has_not_turned_it_on() {
    let fake = fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
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

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    assert!(svc.get().unwrap().drafts(account.id).unwrap().is_empty());
}

/// As [`wait_for_categorize_budget`], for the auto-draft pass's own,
/// smaller per-minute budget (`Service::MAIL_AUTODRAFT_PER_MINUTE`).
async fn wait_for_autodraft_budget() {
    let seconds = 60.0 / f64::from(Service::MAIL_AUTODRAFT_PER_MINUTE);
    tokio::time::sleep(std::time::Duration::from_secs_f64(seconds + 0.5)).await;
}

/// Finding 2's auto-draft half: a thread the model has already answered
/// `reply: false` must not be asked again next tick. Proved the same way
/// the categorisation tests above are: the second tick is pointed at a
/// model that would now say yes, and no draft appears, because nothing
/// should ever have asked it a second time.
#[tokio::test]
async fn auto_draft_does_not_reask_a_thread_it_already_answered_reply_false() {
    let says_no = fake_model(r#"{"reply":false,"body_text":""}"#.to_string()).await;
    let (svc, _dir) = env(&says_no.endpoint);
    let mut account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        thread_id,
        "friend@example.com",
        &[account.address.as_str()],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;
    let (thread, _) = svc.get().unwrap().thread(thread_id).unwrap();
    assert_eq!(thread.ai_auto_draft_asked_at_count, Some(1), "asked once, and recorded as such");
    assert!(svc.get().unwrap().drafts(account.id).unwrap().is_empty());

    // A model that would now say yes, if it were ever asked again.
    let would_draft =
        fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
    repoint_model(&svc, &mut account, &would_draft.endpoint);
    wait_for_autodraft_budget().await;

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;
    assert!(
        svc.get().unwrap().drafts(account.id).unwrap().is_empty(),
        "unasked, since nothing about this thread changed"
    );
}

/// The other half: a new message moves `message_count` past the marker, so
/// the next tick does ask again.
#[tokio::test]
async fn auto_draft_reasks_a_thread_once_a_new_message_arrives() {
    let says_no = fake_model(r#"{"reply":false,"body_text":""}"#.to_string()).await;
    let (svc, _dir) = env(&says_no.endpoint);
    let mut account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        thread_id,
        "friend@example.com",
        &[account.address.as_str()],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );
    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    let would_draft =
        fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
    repoint_model(&svc, &mut account, &would_draft.endpoint);
    wait_for_autodraft_budget().await;
    seed_message(
        &svc,
        account.id,
        mailbox,
        2,
        thread_id,
        "friend@example.com",
        &[account.address.as_str()],
        "Dinner?",
        "Still free Friday? Let me know!",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;
    let drafts = svc.get().unwrap().drafts(account.id).unwrap();
    assert_eq!(drafts.len(), 1, "a new message makes the thread eligible again");
}

/// Finding 4: the model was told to say no more often than yes, and the
/// budget used to be spent only on a *saved draft* -- so a tick where every
/// eligible thread was declined spent nothing at all, no matter how many
/// real, paid calls it made along the way. Seeded with more eligible
/// threads than `Service::MAIL_AUTODRAFT_PER_MINUTE`, all answered `false`,
/// this asserts the fake model was never actually called more times than
/// the budget allows -- which the old rule could not promise, since
/// `budget` only ever decremented on a save that a `reply: false` answer
/// never produces.
#[tokio::test]
async fn auto_draft_never_calls_the_model_more_than_the_per_minute_budget_allows() {
    let says_no = fake_model(r#"{"reply":false,"body_text":""}"#.to_string()).await;
    let (svc, _dir) = env(&says_no.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);

    // More eligible, never-yet-asked Important threads than one tick's
    // budget could ever afford to ask about.
    let candidates = Service::MAIL_AUTODRAFT_PER_MINUTE + 3;
    for i in 0..candidates {
        seed_message(
            &svc,
            account.id,
            mailbox,
            i + 1,
            ThreadId::new(),
            "friend@example.com",
            &[account.address.as_str()],
            "Dinner?",
            "Are you free Friday for dinner?",
            Some(Category::Important),
        );
    }

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    // `quick::run_prompt` costs up to `QUICK_MAX_TURNS_PER_CALL` raw
    // connections per logical call -- one per turn the underlying agent
    // framework's `MAX_TURNS` allows, since a tool call still gets a
    // follow-up turn for the model's own closing reply -- so the bound to
    // check the fake model's raw connection count against is scaled by
    // that, not compared one to one against the budget. What matters is
    // the comparison against the *uncapped* count: the old rule
    // (decrementing only on a saved draft, which `reply: false` never
    // produces) would have let every one of `candidates` threads reach a
    // real call, so this must land well under `candidates *
    // QUICK_MAX_TURNS_PER_CALL`.
    const QUICK_MAX_TURNS_PER_CALL: u32 = 2;
    let capped = Service::MAIL_AUTODRAFT_PER_MINUTE * QUICK_MAX_TURNS_PER_CALL;
    let uncapped = candidates * QUICK_MAX_TURNS_PER_CALL;
    assert!(
        says_no.call_count() <= capped,
        "the model was called {} times, more than the budget of {} should have allowed",
        says_no.call_count(),
        Service::MAIL_AUTODRAFT_PER_MINUTE
    );
    assert!(
        says_no.call_count() < uncapped,
        "the budget must actually have bounded something: {} calls out of an uncapped {}",
        says_no.call_count(),
        uncapped
    );
    assert!(says_no.call_count() > 0, "the budget should have allowed at least one real call");
}

/// Finding 5: taking a whole page's worth of tokens up front -- rather than
/// only as many as this account's page actually turned out to need asking
/// about -- let one account with a small, real backlog spend the *entire*
/// per-minute categorisation budget on the strength of a guess, leaving
/// nothing for any other account in the same tick. Two accounts, each with
/// a single real candidate thread (well under the per-minute budget), must
/// both be asked in the same tick: the first account's own small need must
/// not starve the second's.
#[tokio::test]
async fn categorize_tick_does_not_let_one_accounts_small_backlog_starve_another_accounts() {
    let fake = fake_model(r#"{"labels":[{"index":1,"category":"important"}]}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let vault = svc.get().unwrap();

    let mut accounts = Vec::new();
    for n in 0..2 {
        let models = vault.model_settings().unwrap();
        let mut account = Account::new(Provider::Custom, format!("me{n}@example.com"));
        account.services.mail = true;
        account.mail_ai = mail_ai(true, false, false);
        account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
        vault.save_account(&account).unwrap();
        let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
        seed_message(
            &svc,
            account.id,
            mailbox,
            1,
            ThreadId::new(),
            "stranger@example.com",
            &[account.address.as_str()],
            "Hello",
            "Just checking in.",
            Some(Category::Other),
        );
        accounts.push(account);
    }

    karmayoga_service::mail::ai::categorize_tick(&svc).await;

    for account in &accounts {
        let (_thread, messages) = {
            let threads = vault
                .list_threads(
                    vault.mailboxes(account.id).unwrap()[0].id,
                    &ThreadFilter::default(),
                    None,
                    10,
                )
                .unwrap();
            let thread = threads.threads.first().cloned().unwrap();
            vault.thread(thread.id).unwrap()
        };
        assert_eq!(
            messages[0].category,
            Some(Category::Important),
            "account {} was starved of the shared categorisation budget",
            account.address
        );
    }
}

/// The auto-draft schema now asks the model for `body_text`, plain text
/// only -- never `body_html` -- specifically so that nothing the model
/// writes can reach a draft as raw markup. Proved directly: a model that
/// puts HTML-looking tags inside its `body_text` answer must not see them
/// survive into the saved draft unescaped.
#[tokio::test]
async fn auto_draft_never_lets_the_models_own_markup_through_as_html() {
    let fake =
        fake_model(r#"{"reply":true,"body_text":"Sure, <b>see you then</b>!"}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
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

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    let vault = svc.get().unwrap();
    let drafts = vault.drafts(account.id).unwrap();
    assert_eq!(drafts.len(), 1);
    assert!(
        !drafts[0].body_html.contains("<b>see you then</b>"),
        "the model's own markup must never reach the draft unescaped: {}",
        drafts[0].body_html
    );
}

// ---- auto-draft: what it is told, and when it is not asked ---------------------

/// What the draft model is told is the app's own record of the thread -- the
/// day, how long ago the newest message arrived, whether the person has ever
/// written to the sender -- and the sender's words come last, the subject among
/// them.
#[tokio::test]
async fn auto_draft_tells_the_model_the_date_and_what_the_app_knows_of_the_sender() {
    let fake = fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    seed_message(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &["me+dinner@example.com"],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    let request = &fake.requests()[0];
    let today = jiff::Zoned::now().strftime("Today is %A %-d %B %Y.").to_string();
    assert!(request.contains(&today), "{request}");
    assert!(request.contains("The newest message arrived on "), "{request}");
    assert!(request.contains("(today)"), "{request}");
    assert!(request.contains("written to its sender before: no"), "{request}");
    assert!(request.contains("written earlier in this thread: no"), "{request}");
    let label = request.find("untrusted_text:").expect("labelled");
    assert!(request[label..].contains("Subject: Dinner?"), "the subject is inside it: {request}");
    assert!(request.find("The newest message arrived").unwrap() < label, "{request}");
    assert!(request.contains("unsolicited pitch"), "the decision text: {request}");
}

/// Where the follow-up pass has said nothing waits on the newest message, no
/// draft is written for it and the model is not asked: one judgement drives the
/// list at the top of the Inbox and the drafts under it.
#[tokio::test]
async fn auto_draft_follows_the_follow_up_passes_no_reply_needed() {
    let fake = fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    let msg = seed_message(
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
    let vault = svc.get().unwrap();
    let mark = karmayoga_core::mail::FollowUpMark {
        message: msg.id,
        message_count: 1,
        at: Timestamp::now(),
    };
    // Asked, and nothing is waiting.
    vault.set_thread_follow_up(thread_id, mark, None).unwrap();

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    assert!(vault.drafts(account.id).unwrap().is_empty(), "no draft");
    assert_eq!(fake.call_count(), 0, "and the model was not asked");
    assert_eq!(
        vault.thread(thread_id).unwrap().0.ai_auto_draft_asked_at_count,
        Some(1),
        "marked asked, so it is not looked at again until something new arrives"
    );
}

/// And where it said a reply is waited for, the draft is written -- the other
/// half of "one judgement".
#[tokio::test]
async fn auto_draft_writes_the_draft_the_follow_up_pass_asked_for() {
    // The draft model flags `reply: false` but wrote something; the follow-up
    // answer, which is the better calibrated, stands.
    let fake = fake_model(r#"{"reply":false,"body_text":"Sure, Friday works."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let thread_id = ThreadId::new();
    let msg = seed_message(
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
    let vault = svc.get().unwrap();
    let mark = karmayoga_core::mail::FollowUpMark {
        message: msg.id,
        message_count: 1,
        at: Timestamp::now(),
    };
    let waiting = karmayoga_core::mail::FollowUp {
        kind: karmayoga_core::mail::FollowUpKind::Reply,
        message: msg.id,
        since: msg.date,
    };
    vault.set_thread_follow_up(thread_id, mark, Some(waiting)).unwrap();

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    assert_eq!(vault.drafts(account.id).unwrap().len(), 1);
}

/// A reply to a question that is weeks old is a draft nobody wanted: only
/// threads inside the follow-up window (two weeks) are looked at -- and the
/// newer thread behind an old one is not kept waiting.
#[tokio::test]
async fn auto_draft_leaves_old_threads_alone() {
    let fake = fake_model(r#"{"reply":true,"body_text":"Sure, Friday works."}"#.to_string()).await;
    let (svc, _dir) = env(&fake.endpoint);
    let account = seed_account(&svc, mail_ai(false, false, true));
    let mailbox = seed_mailbox(&svc, account.id, MailboxRole::Inbox);
    let old = Timestamp::now() - jiff::SignedDuration::from_hours(24 * 20);
    let stale = seed_message_at(
        &svc,
        account.id,
        mailbox,
        1,
        ThreadId::new(),
        "friend@example.com",
        &["me@example.com"],
        "Dinner last month?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
        old,
    );
    let fresh = seed_message(
        &svc,
        account.id,
        mailbox,
        2,
        ThreadId::new(),
        "friend@example.com",
        &["me@example.com"],
        "Dinner?",
        "Are you free Friday for dinner?",
        Some(Category::Important),
    );

    karmayoga_service::mail::ai::auto_draft_tick(&svc).await;

    let vault = svc.get().unwrap();
    let drafts = vault.drafts(account.id).unwrap();
    assert_eq!(drafts.len(), 1, "one draft: the fresh thread's");
    assert_eq!(drafts[0].in_reply_to, Some(fresh.id));
    assert_eq!(vault.thread(stale.thread_id).unwrap().0.ai_auto_draft_asked_at_count, None);
}
