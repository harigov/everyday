//! Learning from the replies they sent, end to end, against a fake model that
//! keeps what it was sent: which of their messages reach the model as cases,
//! each beside what it answered and what was suggested for it; what is kept
//! of the answer; when the unattended pass runs at all; and that the next
//! suggestion is written from what was learned.

#[allow(dead_code)]
mod support;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};
use karmayoga_core::account::{Account, MailAi, Provider};
use karmayoga_core::id::{MailMessageId, MailboxId, PackId, ThreadId};
use karmayoga_core::llm::ModelSettings;
use karmayoga_core::mail::{
    Address, Body, Category, CategorySource, MailWriting, Mailbox, MailboxRole, Message,
    MessageFlags, OfferedReplies, VoiceLearning,
};
use karmayoga_core::packstore::PackRef;
use karmayoga_core::store::mail::IngestMessage;
use karmayoga_service::{Ctx, Service};
use serde_json::{Value, json};

// ---- a model that remembers ------------------------------------------------
//
// The same fake as `tests/mailwrite.rs`'s: each file in `tests/` is its own
// binary, and `support` is not this file's to grow.

struct FakeModel {
    endpoint: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
    sent: Arc<Mutex<Vec<Value>>>,
}

impl FakeModel {
    /// Requests so far. `run_prompt` makes more than one to get one answer --
    /// the tool call, then its result -- so a test can compare these but not
    /// count them.
    fn asked(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// The system and user messages of the `n`th request.
    fn prompt(&self, n: usize) -> (String, String) {
        let sent = self.sent.lock().unwrap();
        let messages = sent[n]["messages"].as_array().unwrap().clone();
        let of = |role: &str| {
            messages
                .iter()
                .filter(|m| m["role"] == role)
                .map(|m| match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => {
                        parts.iter().filter_map(|p| p["text"].as_str()).collect::<String>()
                    }
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        (of("system"), of("user"))
    }
}

async fn fake_model(arguments: Value) -> FakeModel {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_for_task = sent.clone();
    let reply = json!({
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
                    "function": { "name": "submit", "arguments": arguments.to_string() },
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
    .to_string();

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let sent = sent_for_task.clone();
            let reply = reply.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    let Some(head_end) = text.find("\r\n\r\n") else { continue };
                    let length = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + length {
                        let body = &buf[head_end + 4..head_end + 4 + length];
                        sent.lock().unwrap().push(serde_json::from_slice(body).unwrap());
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), _shutdown: tx, sent }
}

/// One answer for both questions the model is asked here: the notes a pass
/// wants, and the replies a suggestion wants. Each reads only its own half.
/// The notes come back numbered, bulleted and once twice over, as models do.
fn answer() -> Value {
    json!({
        "notes": [
            "- Says yes in a few words.",
            "2. No greeting or sign-off on a quick reply.",
            "says yes in a few words.",
        ],
        "replies": [
            { "label": "Yes", "body": "Yes, see you there." },
            { "label": "Probably", "body": "Probably, I'll confirm by Friday." },
            { "label": "Ask about the agenda", "body": "What's the agenda?" },
        ],
    })
}

/// What the notes are kept as, from [`answer`].
const NOTES: &str = "- Says yes in a few words.\n- No greeting or sign-off on a quick reply.";

/// A suggestion offered for Dana's message, unlike what was sent.
const OFFER: &str = "Absolutely, I'll have the Q3 numbers over to you by Friday afternoon. \
                     Let me know if you need anything else!";

// ---- fixtures --------------------------------------------------------------

fn now() -> Timestamp {
    "2026-11-05T16:00:00Z".parse().unwrap()
}

fn ago(hours: i64) -> Timestamp {
    now() - SignedDuration::from_hours(hours)
}

const OWN: &str = "hari@example.com";

struct Env {
    svc: Arc<Service>,
    account: Account,
    inbox: MailboxId,
    sent: MailboxId,
    drafts: MailboxId,
    clock: Arc<support::clock::FakeClock>,
    _dir: tempfile::TempDir,
}

/// An account with writing help on and agreed to, an empty Inbox, Sent and
/// Drafts, and the medium model pointed at `endpoint`.
fn env(endpoint: &str) -> Env {
    let (svc, dir) = support::vault::service(None);
    let clock = Arc::new(support::clock::FakeClock::at(now()));
    svc.set_clock(clock.clone());
    let vault = svc.get().unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let mut account = Account::new(Provider::Custom, OWN);
    account.services.mail = true;
    account.mail_ai = MailAi { writing: true, ..Default::default() };
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(&account).unwrap();

    let mailbox = |name: &str, role| {
        let mailbox = Mailbox::new(account.id, name, role);
        vault.save_mailbox(&mailbox).unwrap();
        mailbox.id
    };
    let (inbox, sent, drafts) = (
        mailbox("Inbox", MailboxRole::Inbox),
        mailbox("Sent", MailboxRole::Sent),
        mailbox("Drafts", MailboxRole::Drafts),
    );
    Env { svc, account, inbox, sent, drafts, clock, _dir: dir }
}

static UID: AtomicU32 = AtomicU32::new(1);

/// A message in `thread` from `from` to `to`, `hours` before [`now`].
fn message(e: &Env, thread: ThreadId, from: Address, to: &str, hours: i64) -> Message {
    Message {
        id: MailMessageId::new(),
        account_id: e.account.id,
        thread_id: thread,
        message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
        date: ago(hours),
        from,
        to: vec![Address::bare(to)],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Hello".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 64,
        category: Some(Category::Important),
        category_source: CategorySource::Rules,
        invite: None,
        pack: PackRef { account: e.account.id.to_string(), pack: PackId::new(), offset: 0, len: 1 },
        gmail: None,
    }
}

fn store(e: &Env, mailbox: MailboxId, message: &Message, text: &str) {
    let vault = e.svc.get().unwrap();
    let uid = UID.fetch_add(1, Ordering::Relaxed);
    vault
        .ingest_mail(e.account.id, vec![IngestMessage { message: message.clone(), mailbox, uid }])
        .unwrap();
    vault
        .save_body(&Body {
            message_id: message.id,
            html_sanitised: format!("<p>{text}</p>"),
            text: text.to_string(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        })
        .unwrap();
}

/// `who` writes `hours` before now and is answered an hour later: the message
/// they got, back.
fn answered(e: &Env, who: Address, theirs: &str, reply: &str, hours: i64) -> Message {
    let thread = ThreadId::new();
    let got = message(e, thread, who.clone(), OWN, hours);
    store(e, e.inbox, &got, theirs);
    let sent = message(e, thread, Address::bare(OWN), &who.email, hours - 1);
    store(e, e.sent, &sent, reply);
    got
}

async fn learn_from_replies(e: &Env) -> Result<Value, karmayoga_service::CommandError> {
    e.svc.call(Ctx::local(), "learn_from_replies", json!({})).await
}

// ---- learning, asked for ---------------------------------------------------

#[tokio::test]
async fn their_replies_are_read_beside_what_they_answered_and_what_was_offered() {
    let fake = fake_model(answer()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();

    let dana = answered(
        &e,
        Address::new("Dana Scully", "dana@example.com"),
        "Could you send the Q3 numbers by Friday?",
        "Yes, will do.",
        30,
    );
    answered(
        &e,
        Address::new("Sam Lee", "sam@example.com"),
        "Lunch next week? I'm free Tuesday or Wednesday.",
        "Wednesday works. Noon?",
        20,
    );
    answered(
        &e,
        Address::bare("alex@example.com"),
        "Thanks for the intro yesterday, really appreciated it.",
        "Glad it was useful.",
        10,
    );
    // One they started: it answers nothing.
    let kickoff = message(&e, ThreadId::new(), Address::bare(OWN), "team@example.com", 6);
    store(&e, e.sent, &kickoff, "Kickoff is moved to Monday morning.");
    // A reply, a thank-you for it, and a draft of an answer that never went.
    let lee = answered(
        &e,
        Address::bare("lee@example.com"),
        "Can you review my draft?",
        "Sure, sending notes tonight.",
        4,
    );
    let thanks = message(&e, lee.thread_id, Address::bare("lee@example.com"), OWN, 2);
    store(&e, e.inbox, &thanks, "Thanks!");
    let mut draft = message(&e, lee.thread_id, Address::bare(OWN), "lee@example.com", 1);
    draft.flags.draft = true;
    store(&e, e.drafts, &draft, "Never sent: an answer I gave up on.");

    let for_dana = OfferedReplies {
        account: e.account.id,
        thread: dana.thread_id,
        message: dana.id,
        at: ago(30),
        replies: vec![OFFER.into(), "Not by Friday, sorry.".into(), "Which numbers?".into()],
    };
    let unanswered = OfferedReplies {
        account: e.account.id,
        thread: ThreadId::new(),
        message: MailMessageId::new(),
        at: ago(1),
        replies: vec!["Sounds good.".into()],
    };
    let offered = vec![for_dana, unanswered.clone()];
    let waiting = VoiceLearning { offered, ..Default::default() };
    vault.save_voice_learning(&waiting).unwrap();

    let learned = learn_from_replies(&e).await.expect("learn_from_replies");

    // What the model was shown.
    let (system, user) = fake.prompt(0);
    assert!(system.contains("You keep a short set of notes on how one person replies"));
    assert!(user.contains("Current notes: none yet."), "{user}");
    for shown in [
        "from Dana Scully <dana@example.com>",
        "Could you send the Q3 numbers by Friday?",
        "Yes, will do.",
        "Wednesday works. Noon?",
        "Glad it was useful.",
        "Can you review my draft?",
        "Sure, sending notes tonight.",
    ] {
        assert!(user.contains(shown), "{shown}: {user}");
    }
    assert!(user.contains(OFFER), "what was suggested sits beside what was sent: {user}");
    assert!(!user.contains("Kickoff is moved"), "one they started answers nothing: {user}");
    assert!(!user.contains("Never sent"), "a draft is not a reply: {user}");
    assert!(!user.contains("Thanks!"), "a message they never answered is no case: {user}");
    let newest = user.find("Sure, sending notes tonight.").unwrap();
    assert!(newest < user.find("Yes, will do.").unwrap(), "newest first: {user}");

    // What was kept of the answer.
    assert_eq!(learned["notes"], NOTES);
    assert_eq!(learned["repliesRead"], 4);
    assert_eq!(learned["edited"], false);
    let stored = vault.voice_learning().unwrap();
    assert_eq!(stored.notes, NOTES);
    assert_eq!(stored.replies_read, 4);
    assert_eq!(stored.learned_at, Some(now()));
    assert_eq!(stored.learned_through, Some(ago(3)), "Lee's answer, the newest");
    assert!(!stored.edited);
    assert_eq!(stored.offered, vec![unanswered], "the offer read beside its reply is let go");

    // And the next suggestion is written from it, with their replies shown
    // beside what they answered.
    let thread = ThreadId::new();
    let pat = message(&e, thread, Address::new("Pat Doe", "pat@example.com"), OWN, 0);
    store(&e, e.inbox, &pat, "Are you coming to the offsite?");
    let before = fake.asked();
    e.svc
        .call(Ctx::local(), "suggest_replies", json!({ "id": thread }))
        .await
        .expect("suggest_replies");
    let (system, user) = fake.prompt(before);
    assert!(system.contains(&format!("---\n{NOTES}\n---")), "{system}");
    assert!(
        user.contains(
            "--- example 1: the message they were answering ---\n\
             Can you review my draft?\n\
             --- what they sent ---\n\
             Sure, sending notes tonight."
        ),
        "{user}"
    );
}

#[tokio::test]
async fn asking_again_reads_the_same_replies_without_counting_them_twice() {
    let fake = fake_model(answer()).await;
    let e = env(&fake.endpoint);
    answered(&e, Address::bare("a@example.com"), "Free Monday?", "Yes, all day.", 30);

    learn_from_replies(&e).await.expect("first");
    let asked = fake.asked();
    e.clock.set(now() + SignedDuration::from_mins(5));
    let again = learn_from_replies(&e).await.expect("asked for, so not held back a day");

    assert!(fake.asked() > asked);
    assert!(fake.prompt(asked).1.contains("Yes, all day."), "read again");
    assert!(fake.prompt(asked).1.contains(NOTES), "beside what was learned from it");
    assert_eq!(again["repliesRead"], 1);
}

#[tokio::test]
async fn learning_is_refused_saying_why() {
    let fake = fake_model(answer()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();

    let err = learn_from_replies(&e).await.expect_err("nothing sent yet");
    assert_eq!(err.code, "invalid");
    assert!(err.message.contains("no replies of yours"), "{err}");

    answered(&e, Address::bare("a@example.com"), "Free Monday?", "Yes, all day.", 30);
    vault.save_mail_writing(&MailWriting { learn: false, ..Default::default() }).unwrap();
    let err = learn_from_replies(&e).await.expect_err("switched off");
    assert_eq!(err.code, "invalid");
    assert!(err.message.contains("switched off"), "{err}");

    vault.save_mail_writing(&MailWriting::default()).unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.mail_ai.writing = false;
    vault.save_account(&account).unwrap();
    let err = learn_from_replies(&e).await.expect_err("no account allows writing help");
    assert_eq!(err.code, "forbidden", "{err}");

    assert_eq!(fake.asked(), 0, "and the model was never asked");
}

/// An edit made in Settings is the person's: the next pass is told so, and
/// what it writes replaces the edit only by being asked to keep it.
#[tokio::test]
async fn notes_they_edited_are_shown_to_the_next_pass_as_theirs() {
    let fake = fake_model(answer()).await;
    let e = env(&fake.endpoint);
    answered(&e, Address::bare("a@example.com"), "Free Monday?", "Yes, all day.", 30);
    e.svc
        .call(Ctx::local(), "save_voice_learning", json!({ "notes": "- Never says sorry." }))
        .await
        .unwrap();

    let learned = learn_from_replies(&e).await.expect("learn_from_replies");

    let (_, user) = fake.prompt(0);
    assert!(user.contains("edited by the person themselves"), "{user}");
    assert!(user.contains("---\n- Never says sorry.\n---"), "{user}");
    assert_eq!(learned["edited"], false, "written by a pass again");
}

// ---- learning, unasked -----------------------------------------------------

#[tokio::test]
async fn the_unattended_pass_waits_for_enough_replies_and_then_for_a_day() {
    let fake = fake_model(answer()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let tick = || karmayoga_service::mailvoicelearn::learn_tick(&e.svc);
    let at = |minutes: i64| e.clock.set(now() + SignedDuration::from_mins(minutes));

    answered(&e, Address::bare("a@example.com"), "Around Monday?", "Yes, all day.", 30);
    answered(&e, Address::bare("b@example.com"), "Can we move our call?", "Sure, pick a time.", 20);
    tick().await;
    assert_eq!(fake.asked(), 0, "two replies are too few to learn from");

    answered(&e, Address::bare("c@example.com"), "Did the files arrive?", "Got them, thanks.", 10);
    at(30);
    tick().await;
    assert_eq!(fake.asked(), 0, "not looked at again within the hour");
    at(61);
    tick().await;
    assert!(fake.asked() > 0, "three are enough the first time");
    let first = vault.voice_learning().unwrap();
    assert_eq!(first.notes, NOTES);
    assert_eq!(first.replies_read, 3);
    assert_eq!(first.learned_through, Some(ago(9)));

    let asked = fake.asked();
    at(3 * 60);
    tick().await;
    assert_eq!(fake.asked(), asked, "not again the same day");

    for (n, who) in ["d", "e", "f", "g"].into_iter().enumerate() {
        let who = Address::bare(format!("{who}@example.com"));
        answered(&e, who, "A question?", "An answer.", 8 - n as i64);
    }
    at(26 * 60);
    tick().await;
    assert_eq!(fake.asked(), asked, "a day on, four new replies are too few");

    answered(&e, Address::bare("h@example.com"), "One more?", "One more answer.", 4);
    at(26 * 60 + 61);
    tick().await;
    assert!(fake.asked() > asked, "five are enough");
    let second = vault.voice_learning().unwrap();
    assert_eq!(second.replies_read, 8, "only the new ones counted");
    assert_eq!(second.learned_through, Some(ago(3)));
    let (_, user) = fake.prompt(asked);
    assert!(user.contains("One more answer."), "{user}");
    assert!(!user.contains("Yes, all day."), "what was already learned from is not read again");
}

#[tokio::test]
async fn the_unattended_pass_does_nothing_while_learning_is_off() {
    let fake = fake_model(answer()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    for who in ["a", "b", "c", "d", "e"] {
        answered(&e, Address::bare(format!("{who}@example.com")), "Q?", "A, then.", 10);
    }
    vault.save_mail_writing(&MailWriting { learn: false, ..Default::default() }).unwrap();

    karmayoga_service::mailvoicelearn::learn_tick(&e.svc).await;

    assert_eq!(fake.asked(), 0);
    assert_eq!(vault.voice_learning().unwrap(), VoiceLearning::default());
}
