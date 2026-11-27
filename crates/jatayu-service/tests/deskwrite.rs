//! Writing by voice, end to end against a fake model on loopback: what is put
//! to the model for a rewrite said aloud and for a message written from a few
//! words, whose mail is read for the voice and whose never is, and that every
//! use is kept. `tests/voice_writing.rs` covers the conversation around these;
//! this is the half that asks the model.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use jatayu_core::account::{Account, Identity, MailAi, Provider};
use jatayu_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use jatayu_core::llm::ModelSettings;
use jatayu_core::mail::{
    Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
};
use jatayu_core::packstore::PackRef;
use jatayu_core::store::mail::IngestMessage;
use jatayu_core::writing::{WritingAction, WritingAssist, WritingOutcome};
use jatayu_core::{Vault, WritingQuery};
use jatayu_service::Service;
use jatayu_service::deskwrite::{self, MessageKind};
use jiff::Timestamp;
use serde_json::{Value, json};

// ---- a fake model on loopback ----------------------------------------------

/// Answers every request with a `submit` tool call carrying `{"text": answer}`
/// and keeps the system and user messages of each request it was sent.
struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl FakeModel {
    /// The system and user messages of the first request: the ask itself.
    /// (`run_prompt` hands the tool's result back in a second.)
    fn first(&self) -> (String, String) {
        self.requests.lock().unwrap().first().cloned().expect("the model was asked")
    }

    /// Everything the model was ever sent, system and user alike.
    fn everything(&self) -> String {
        let all = self.requests.lock().unwrap();
        all.iter().map(|(s, u)| format!("{s}\n{u}")).collect::<Vec<_>>().join("\n")
    }

    fn asked(&self) -> bool {
        !self.requests.lock().unwrap().is_empty()
    }
}

async fn fake_model(answer: &str) -> FakeModel {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
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
                    "function": {
                        "name": "submit",
                        "arguments": json!({ "text": answer }).to_string(),
                    },
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
            let (seen, reply) = (seen.clone(), reply.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
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
                        let body: Value =
                            serde_json::from_slice(&buf[head_end + 4..head_end + 4 + length])
                                .unwrap();
                        let of = |role: &str| {
                            body["messages"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter(|m| m["role"] == role)
                                .map(|m| match &m["content"] {
                                    Value::String(s) => s.clone(),
                                    Value::Array(parts) => parts
                                        .iter()
                                        .filter_map(|p| p["text"].as_str())
                                        .collect::<String>(),
                                    _ => String::new(),
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        };
                        seen.lock().unwrap().push((of("system"), of("user")));
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), requests, _shutdown: tx }
}

// ---- fixtures ---------------------------------------------------------------

struct Env {
    svc: Arc<Service>,
    acknowledged: String,
    _dir: tempfile::TempDir,
}

impl Env {
    fn vault(&self) -> Arc<Vault> {
        self.svc.get().unwrap()
    }

    /// An account with a Sent mailbox holding one message of its own, whose
    /// words are `sent`. `agreed` says whether its writing help is on and the
    /// model acknowledged for it.
    fn account(&self, address: &str, name: &str, agreed: bool, sent: &str) -> Account {
        let vault = self.vault();
        let mut account = Account::new(Provider::Custom, address);
        account.display_name = name.into();
        account.services.mail = true;
        account.mail_ai = MailAi { writing: agreed, ..MailAi::default() };
        if agreed {
            account.assistant_provider_acknowledged = Some(self.acknowledged.clone());
        }
        // A signature a draft would carry, and a message must not.
        account.identities = vec![Identity {
            name: name.into(),
            address: address.into(),
            signature_html: "<p>Hari G.<br>ViyaMD Sigline</p>".into(),
        }];
        vault.save_account(&account).unwrap();
        let sent_box = Mailbox::new(account.id, "Sent", MailboxRole::Sent);
        vault.save_mailbox(&sent_box).unwrap();
        seed_sent(&vault, account.id, sent_box.id, address, sent);
        account
    }
}

fn env(endpoint: &str) -> Env {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted-medium".into();
    vault.save_model_settings(&models).unwrap();
    Env { svc, acknowledged: models.acknowledgement_name(), _dir: dir }
}

fn seed_sent(vault: &Vault, account: AccountId, mailbox: MailboxId, from: &str, text: &str) {
    let message = Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
        date: Timestamp::now(),
        from: Address::bare(from),
        to: vec![Address::bare("sam@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Re: Tuesday".to_string(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: text.len() as u64,
        category: None,
        category_source: CategorySource::Rules,
        invite: None,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 1 },
        gmail: None,
    };
    vault
        .ingest_mail(account, vec![IngestMessage { message: message.clone(), mailbox, uid: 1 }])
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

fn kept(env: &Env) -> Vec<WritingAssist> {
    env.vault().writing_assists(&WritingQuery::default()).unwrap()
}

const SAID: &str = "write that I am not feeling well and that I will reach out tomorrow";
const WRITTEN: &str = "Hi Sam,\n\nI am not feeling well, so I will reach out tomorrow.\n\nHari";

// ---- writing a message ---------------------------------------------------------

#[tokio::test]
async fn with_no_account_that_agreed_a_message_is_written_plain_and_no_mail_is_sent() {
    let model = fake_model(WRITTEN).await;
    let env = env(&model.endpoint);
    // Mail with a phrase that must stay where it is: the writing switch is off.
    env.account("hari@example.com", "Hari Govardhanam", false, "UNALLOWED-PHRASE-77 see you soon");

    let message =
        deskwrite::compose(&env.svc, SAID, MessageKind::Email, Some("Sam")).await.unwrap().text;
    assert_eq!(message, WRITTEN);

    let (system, user) = model.first();
    assert!(system.contains("warm and plain"), "{system}");
    assert!(system.contains("This is an email"), "{system}");
    assert!(user.starts_with("Message: an email to Sam."), "{user}");
    assert!(user.ends_with(&format!("What they said:\n{SAID}")), "{user}");
    assert!(!model.everything().contains("UNALLOWED-PHRASE-77"), "mail that may not be read");
    assert!(!model.everything().contains("Examples of emails"));

    let [assist] = kept(&env).try_into().expect("one record");
    assert_eq!(assist.action, WritingAction::Compose);
    assert_eq!(assist.original, SAID);
    assert_eq!(assist.instruction.as_deref(), Some("an email to Sam"));
    assert_eq!(assist.result, WRITTEN);
    assert_eq!(assist.model, "scripted-medium");
    assert_eq!(assist.outcome, WritingOutcome::Pending);
}

#[tokio::test]
async fn the_first_account_that_agreed_lends_its_voice_and_the_others_lend_nothing() {
    let model = fake_model(WRITTEN).await;
    let env = env(&model.endpoint);
    env.account("work@example.com", "Work Hari", false, "UNALLOWED-PHRASE-77 see you soon");
    env.account(
        "home@example.com",
        "Hari Govardhanam",
        true,
        "Hi Sam,\n\nSounds good, Tuesday it is.\n\nThanks,\nHari",
    );
    env.account("other@example.com", "Other Hari", true, "LATER-ACCOUNT-PHRASE-88 cheers");

    let message =
        deskwrite::compose(&env.svc, SAID, MessageKind::Email, Some("Sam")).await.unwrap().text;
    assert_eq!(message, WRITTEN);

    let (system, user) = model.first();
    assert!(system.contains("## Writing as them"), "their rules: {system}");
    assert!(system.contains("never curt, unless they have told you otherwise"), "{system}");
    assert!(user.contains("Sounds good, Tuesday it is."), "their own example: {user}");
    assert!(user.contains("They sign as Hari Govardhanam."), "{user}");
    assert!(user.ends_with(&format!("What they said:\n{SAID}")), "{user}");
    let everything = model.everything();
    assert!(!everything.contains("UNALLOWED-PHRASE-77"), "an account that did not agree");
    assert!(!everything.contains("LATER-ACCOUNT-PHRASE-88"), "an account after the first");
    assert!(!everything.contains("ViyaMD Sigline"), "no signature block is added to a message");
    assert!(!user.contains("written from"), "{user}");
}

#[tokio::test]
async fn a_chat_message_takes_their_voice_but_no_name_and_no_email_form() {
    let model = fake_model("Hey Sam, I'm not feeling well. I'll reach out tomorrow.").await;
    let env = env(&model.endpoint);
    env.account(
        "home@example.com",
        "Hari Govardhanam",
        true,
        "Hi Sam,\n\nSounds good.\n\nBest,\nHari",
    );

    let message =
        deskwrite::compose(&env.svc, SAID, MessageKind::Chat, Some("Sam")).await.unwrap().text;
    assert_eq!(message, "Hey Sam, I'm not feeling well. I'll reach out tomorrow.");

    let (system, user) = model.first();
    assert!(system.contains("This is a chat message"), "{system}");
    assert!(system.contains("none of their emails' greetings"), "{system}");
    assert!(user.starts_with("Message: a chat message to Sam."), "{user}");
    assert!(user.contains("no email formalities"), "{user}");
    assert!(!user.contains("They sign as"), "{user}");
    assert!(!user.contains("home@example.com"), "{user}");
    assert_eq!(kept(&env)[0].instruction.as_deref(), Some("a chat message to Sam"));
}

#[tokio::test]
async fn a_model_that_gives_back_nothing_is_a_failure_that_is_kept() {
    let model = fake_model("   ").await;
    let env = env(&model.endpoint);

    let failed = deskwrite::compose(&env.svc, SAID, MessageKind::Text, None).await.unwrap_err();
    assert!(!failed.message.is_empty());
    let [assist] = kept(&env).try_into().expect("one record");
    assert_eq!(assist.outcome, WritingOutcome::Failed);
    assert_eq!(assist.original, SAID);
    assert_eq!(assist.instruction.as_deref(), Some("a text message"));
}

#[tokio::test]
async fn nothing_said_asks_nobody_and_keeps_nothing() {
    let model = fake_model("unused").await;
    let env = env(&model.endpoint);

    let refused = deskwrite::compose(&env.svc, "  ", MessageKind::Email, None).await;
    assert!(refused.is_err());
    assert!(!model.asked());
    assert!(kept(&env).is_empty());
}

// ---- rewriting what is selected --------------------------------------------------

#[tokio::test]
async fn a_custom_rewrite_puts_their_words_to_the_model_and_keeps_the_record() {
    let model = fake_model("Milk and eggs,\nand some bread.").await;
    let env = env(&model.endpoint);

    let text = deskwrite::rewrite(
        &env.svc,
        "Milk, eggs\nand bread.\n",
        WritingAction::Custom,
        Some("make it friendlier"),
    )
    .await
    .unwrap()
    .text;
    // The line break the selection ended on is put back.
    assert_eq!(text, "Milk and eggs,\nand some bread.\n");

    let (system, user) = model.first();
    assert!(system.contains("careful editor"), "{system}");
    assert!(user.contains("Their words: make it friendlier"), "{user}");
    assert!(user.contains("Keep it exactly as it is"), "{user}");
    assert!(user.ends_with("The text to rewrite:\nMilk, eggs\nand bread."), "{user}");

    let [assist] = kept(&env).try_into().expect("one record");
    assert_eq!(assist.action, WritingAction::Custom);
    assert_eq!(assist.instruction.as_deref(), Some("make it friendlier"));
    assert_eq!(assist.original, "Milk, eggs\nand bread.\n");
    assert_eq!(assist.result, text);
}

/// Whether what they said is about the layout is the model's to read, and
/// nothing afterwards decides it was not: not a check that asks again, and not
/// the original's markers put back. These are the requests a list of layout
/// words got wrong, one way or the other.
#[tokio::test]
async fn whatever_a_custom_answer_does_to_the_layout_stands() {
    for (original, said, answer) in [
        ("Open the box\nPlug it in", "number these steps", "1. Open the box\n2. Plug it in"),
        (
            "Open the box\nPlug it in",
            "make each line a checkbox",
            "- [ ] Open the box\n- [ ] Plug it in",
        ),
        ("Open the box\nPlug it in", "quote it", "> Open the box\n> Plug it in"),
        ("Milk, eggs and bread.", "turn this into a bulleted list", "- Milk\n- Eggs\n- Bread"),
        ("One. Two. Three.", "one sentence per line", "One.\nTwo.\nThree."),
        (
            "Subject: Update\n\nNews inside.",
            "make the subject line punchier",
            "Subject: Big news\n\nNews inside.",
        ),
        ("1. Open\n2. Plug", "reword the steps, no numbers", "Open it\nPlug it in"),
    ] {
        let model = fake_model(answer).await;
        let env = env(&model.endpoint);

        let text = deskwrite::rewrite(&env.svc, original, WritingAction::Custom, Some(said))
            .await
            .unwrap()
            .text;
        assert_eq!(text, answer, "{said}");
        let everything = model.everything();
        assert!(!everything.contains("Your first answer was"), "{said}: not asked again");
        assert!(
            model.first().1.contains("unless what they ask is about the layout itself"),
            "{said}"
        );
    }
}

#[tokio::test]
async fn simplifying_is_asked_for_plainer_words_and_kept_as_its_own_action() {
    let model = fake_model("We will meet on Monday.").await;
    let env = env(&model.endpoint);

    let text = deskwrite::rewrite(
        &env.svc,
        "A gathering is to be convened on Monday.",
        WritingAction::Simplify,
        None,
    )
    .await
    .unwrap()
    .text;
    assert_eq!(text, "We will meet on Monday.");
    assert!(model.first().1.contains("plain, everyday"));
    let [assist] = kept(&env).try_into().expect("one record");
    assert_eq!(assist.action, WritingAction::Simplify);
    assert_eq!(assist.instruction, None);
}

#[tokio::test]
async fn a_custom_rewrite_with_nothing_to_change_asks_nobody_and_keeps_nothing() {
    let model = fake_model("unused").await;
    let env = env(&model.endpoint);

    for words in [None, Some("  "), Some(&*"x".repeat(301))] {
        let refused = deskwrite::rewrite(&env.svc, "some text", WritingAction::Custom, words).await;
        assert!(refused.is_err(), "{words:?}");
    }
    for action in [WritingAction::Dictate, WritingAction::Compose] {
        assert!(deskwrite::rewrite(&env.svc, "some text", action, None).await.is_err());
    }
    assert!(!model.asked());
    assert!(kept(&env).is_empty());
}
