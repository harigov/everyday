//! Follow-ups (`karmayoga_service::mailfollowup`), end to end: mail seeded
//! into an Inbox and a Sent mailbox, the categorisation tick run against a
//! fake decision server on loopback -- which sorts first, then asks about
//! follow-ups, on the same model -- and what `follow_ups` then shows.
//!
//! The case that matters most is cold outreach: a stranger's pitch reads like
//! mail that wants an answer, and must not be shown as one.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};
use karmayoga_core::account::{Account, MailAi, Provider};
use karmayoga_core::id::{MailMessageId, MailboxId, PackId, ThreadId};
use karmayoga_core::llm::{DecisionApi, DecisionConfig, ModelSettings};
use karmayoga_core::mail::{
    Address, Body, Category, CategorySource, ContactBook, FollowUpKind, Mailbox, MailboxRole,
    Message, MessageFlags,
};
use karmayoga_core::packstore::PackRef;
use karmayoga_core::store::mail::IngestMessage;
use karmayoga_service::Service;
use karmayoga_service::mailfollowup::{NUDGE_OPTIONS, REPLY_OPTIONS};
use serde_json::{Value, json};

const ME: &str = "me@example.com";
const ADA: &str = "ada@friend.example";
const DAY: SignedDuration = SignedDuration::from_hours(24);

// ---- a fake decision server -------------------------------------------------

/// Which question a request put: sorting's, or one of the two follow-up ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Question {
    Sort,
    Reply,
    Nudge,
}

/// One request the fake was sent.
#[derive(Debug, Clone)]
struct Seen {
    question: Question,
    subject: String,
    body: Value,
}

struct FakeDecisions {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl FakeDecisions {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// The subjects `question` was put about, in order.
    fn subjects(&self, question: Question) -> Vec<String> {
        self.seen().into_iter().filter(|s| s.question == question).map(|s| s.subject).collect()
    }

    /// The body of the one request putting `question` about `subject`.
    fn request(&self, question: Question, subject: &str) -> Value {
        let found: Vec<Seen> = self
            .seen()
            .into_iter()
            .filter(|s| s.question == question && s.subject == subject)
            .collect();
        assert_eq!(found.len(), 1, "{question:?} about {subject:?} once: {found:?}");
        found[0].body.clone()
    }
}

/// Which question a TypeSafe request puts, by the answers it offers.
fn question_of(body: &Value) -> Question {
    let criteria = &body["questions"]["category"]["criteria"];
    if criteria.get(REPLY_OPTIONS[0].0).is_some() {
        Question::Reply
    } else if criteria.get(NUDGE_OPTIONS[0].0).is_some() {
        Question::Nudge
    } else {
        Question::Sort
    }
}

/// What the fake answers, and how sure it says it is. Sorting files the
/// digest under Newsletters and everything else under People. Of the reply
/// questions, the partnership pitch is cold outreach, the stranger's "quick
/// question" needs a reply but the model is not sure, and anything else needs
/// a reply. Every nudge question is waiting on them.
fn answer(question: Question, subject: &str) -> (&'static str, f64) {
    let subject = subject.to_lowercase();
    match question {
        Question::Sort if subject.contains("digest") => ("Newsletters", 0.9),
        Question::Sort => ("People", 0.9),
        Question::Reply if subject.contains("partnership") => (REPLY_OPTIONS[1].0, 0.95),
        Question::Reply if subject.contains("quick question") => (REPLY_OPTIONS[0].0, 0.6),
        Question::Reply => (REPLY_OPTIONS[0].0, 0.9),
        Question::Nudge => (NUDGE_OPTIONS[0].0, 0.9),
    }
}

/// A TypeSafe-shaped decision server on loopback, answering by [`answer`].
async fn fake_decisions() -> FakeDecisions {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let seen_task = seen.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let seen = seen_task.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut raw = Vec::new();
                let mut buf = vec![0u8; 16 * 1024];
                // Headers, then exactly Content-Length bytes of body.
                let body = loop {
                    let Ok(n) = socket.read(&mut buf).await else { return };
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&raw[..end]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    while raw.len() < end + 4 + len {
                        let Ok(n) = socket.read(&mut buf).await else { return };
                        if n == 0 {
                            break;
                        }
                        raw.extend_from_slice(&buf[..n]);
                    }
                    break raw[end + 4..].to_vec();
                };
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let question = question_of(&body);
                let subject = body["state"]["subject"].as_str().unwrap_or("").to_string();
                let (pick, confidence) = answer(question, &subject);
                seen.lock().unwrap().push(Seen { question, subject, body });
                let payload = json!({
                    "model": "jev-latest",
                    "answers": {
                        "category": { "type": "choice", "choice": pick, "confidence": confidence },
                    },
                    "usage": { "input_tokens": 100, "output_tokens": 10 },
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    FakeDecisions { url: format!("http://127.0.0.1:{port}/v1/systemone"), seen, _shutdown: tx }
}

// ---- fixtures ---------------------------------------------------------------

struct Env {
    svc: Arc<Service>,
    _dir: tempfile::TempDir,
    account: Account,
    inbox: MailboxId,
    sent: MailboxId,
    drafts: MailboxId,
}

/// A vault whose decision model is the fake, keyed, and an account that
/// allows sorting, has acknowledged exactly these endpoints, has written to
/// Ada before, and has an Inbox, a Sent mailbox and a Drafts mailbox.
fn env(url: &str) -> Env {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let models = ModelSettings {
        decision: Some(DecisionConfig {
            api: DecisionApi::TypeSafe,
            url: Some(url.to_string()),
            model: DecisionApi::TypeSafe.default_model().into(),
        }),
        ..ModelSettings::default()
    };
    vault.save_model_settings(&models).unwrap();
    vault.set_model_key(karmayoga_core::llm::ModelSlot::Decision, "ds-key").unwrap();

    let mut account = Account::new(Provider::Custom, ME);
    account.services.mail = true;
    account.mail_ai = MailAi { categorize: true, ..MailAi::default() };
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(&account).unwrap();
    let inbox = Mailbox::new(account.id, "INBOX", MailboxRole::Inbox);
    let sent = Mailbox::new(account.id, "Sent", MailboxRole::Sent);
    let drafts = Mailbox::new(account.id, "Drafts", MailboxRole::Drafts);
    for mailbox in [&inbox, &sent, &drafts] {
        vault.save_mailbox(mailbox).unwrap();
    }

    let mut contacts = ContactBook::default();
    contacts.record_sent_to(ADA, "Ada");
    vault.save_mail_contacts(&contacts).unwrap();

    let env = Env { svc, _dir: dir, account, inbox: inbox.id, sent: sent.id, drafts: drafts.id };
    // Something you sent Ada long ago, outside every window. Unlocking starts
    // a count of stored mail into the contact book, which is written over the
    // book saved above whenever it finishes; with this stored, Ada is
    // somebody you write to either way.
    let old = mail(&env, ThreadId::new(), ME, &[ADA], "Last spring", DAY * 50);
    store(&env, env.sent, 900, &old, "Good to see you.");
    env
}

/// A message in `thread`, sent `age` ago, filed by the rules as Important.
fn mail(
    env: &Env,
    thread: ThreadId,
    from: &str,
    to: &[&str],
    subject: &str,
    age: SignedDuration,
) -> Message {
    Message {
        id: MailMessageId::new(),
        account_id: env.account.id,
        thread_id: thread,
        message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
        date: Timestamp::now() - age,
        from: Address::bare(from),
        to: to.iter().map(|a| Address::bare(*a)).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: subject.into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: Some(Category::Important),
        category_source: CategorySource::Rules,
        invite: None,
        pack: PackRef {
            account: env.account.id.to_string(),
            pack: PackId::new(),
            offset: 0,
            len: 1,
        },
        gmail: None,
    }
}

/// Store `message` in `mailbox` with `text` for its body, as a sync would.
fn store(env: &Env, mailbox: MailboxId, uid: u32, message: &Message, text: &str) {
    let vault = env.svc.get().unwrap();
    vault
        .ingest_mail(env.account.id, vec![IngestMessage { message: message.clone(), mailbox, uid }])
        .unwrap();
    vault
        .save_body(&Body {
            message_id: message.id,
            html_sanitised: String::new(),
            text: text.into(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        })
        .unwrap();
}

/// What `follow_ups` shows, as subjects and kinds, in its order.
async fn shown(svc: &Arc<Service>) -> Vec<(String, FollowUpKind)> {
    karmayoga_service::mailfollowup::follow_ups(svc, None)
        .await
        .expect("follow_ups")
        .into_iter()
        .map(|t| (t.subject.clone(), t.follow_up.expect("a follow-up on every thread shown").kind))
        .collect()
}

// ---- the tests ----------------------------------------------------------------

#[tokio::test]
async fn follow_ups_show_who_waits_on_you_and_whom_you_wait_on_but_never_a_pitch() {
    let fake = fake_decisions().await;
    let env = env(&fake.url);
    let vault = env.svc.get().unwrap();

    // Somebody you write to, asking you something.
    let dinner = mail(&env, ThreadId::new(), ADA, &[ME], "Dinner on Saturday?", DAY);
    store(&env, env.inbox, 1, &dinner, "Are you free on Saturday? Let me know by Thursday.");
    // A stranger's pitch, to you by name and ending in a question.
    let pitch =
        mail(&env, ThreadId::new(), "growth@agency.example", &[ME], "Partnership opportunity", DAY);
    store(&env, env.inbox, 2, &pitch, "Loved your work! Do you have 15 minutes for a quick call?");
    // A stranger the model thinks wants an answer, without being sure.
    let unsure =
        mail(&env, ThreadId::new(), "founder@startup.example", &[ME], "Quick question", DAY);
    store(&env, env.inbox, 3, &unsure, "Would you be open to trying what we built?");
    // A newsletter, asking a question of everybody on its list.
    let mut digest =
        mail(&env, ThreadId::new(), "editor@letters.example", &[ME], "Weekly digest", DAY);
    digest.category = Some(Category::Newsletter);
    store(&env, env.inbox, 4, &digest, "This week's links. What did you think?");
    // Your own question, five days ago, unanswered.
    let contract =
        mail(&env, ThreadId::new(), ME, &["bob@partner.example"], "The contract", DAY * 5);
    store(&env, env.sent, 1, &contract, "Could you send me the signed contract by Friday?");
    // And one from yesterday: too soon to nudge about.
    let invoice = mail(&env, ThreadId::new(), ME, &["carol@partner.example"], "The invoice", DAY);
    store(&env, env.sent, 2, &invoice, "Did the invoice arrive?");

    karmayoga_service::mailai::categorize_tick(&env.svc).await;

    assert_eq!(
        shown(&env.svc).await,
        vec![
            ("Dinner on Saturday?".to_string(), FollowUpKind::Reply),
            ("The contract".to_string(), FollowUpKind::Nudge),
        ],
        "Ada's question and your own old one; not the pitch, not the unsure stranger"
    );

    // The three addressed to you were asked about, and your old question.
    // The newsletter never was, nor the message too new to nudge about.
    let mut replies = fake.subjects(Question::Reply);
    replies.sort();
    assert_eq!(replies, ["Dinner on Saturday?", "Partnership opportunity", "Quick question"]);
    assert_eq!(fake.subjects(Question::Nudge), ["The contract"]);

    // The model is told what a pitch cannot fake: whether you ever wrote.
    let pitch_asked = fake.request(Question::Reply, "Partnership opportunity");
    assert_eq!(pitch_asked["state"]["you_have_written_to_the_sender"], false);
    assert_eq!(pitch_asked["state"]["you_wrote_earlier_in_this_thread"], false);
    assert_eq!(pitch_asked["state"]["addressed_to_you"], "directly");
    let criteria = pitch_asked["questions"]["category"]["criteria"].as_object().unwrap();
    assert_eq!(criteria.len(), REPLY_OPTIONS.len(), "{criteria:?}");
    assert!(criteria.contains_key(REPLY_OPTIONS[1].0), "cold outreach is offered: {criteria:?}");
    let dinner_asked = fake.request(Question::Reply, "Dinner on Saturday?");
    assert_eq!(dinner_asked["state"]["you_have_written_to_the_sender"], true);
    assert!(
        dinner_asked["state"]["excerpt"].as_str().unwrap().starts_with("Are you free on Saturday?")
    );
    let nudge_asked = fake.request(Question::Nudge, "The contract");
    assert_eq!(nudge_asked["state"]["to"], "bob@partner.example");
    assert_eq!(nudge_asked["state"]["days_since_you_wrote"], 5);
    assert!(nudge_asked["state"]["their_last_email"].as_str().unwrap().starts_with("none"));

    // Asked, or plainly not worth asking: marked, with nothing waiting.
    for message in [&pitch, &unsure, &digest] {
        let (t, _) = vault.thread(message.thread_id).unwrap();
        assert!(t.follow_up_asked.is_some(), "{} marked", t.subject);
        assert!(t.follow_up.is_none(), "{} not waiting", t.subject);
    }
    // Too new to nudge about: not marked, so it is asked once it is old enough.
    let (t, _) = vault.thread(invoice.thread_id).unwrap();
    assert!(t.follow_up_asked.is_none());

    // Nothing is asked twice.
    let asked = |fake: &FakeDecisions| {
        fake.subjects(Question::Reply).len() + fake.subjects(Question::Nudge).len()
    };
    let before = asked(&fake);
    karmayoga_service::mailai::categorize_tick(&env.svc).await;
    assert_eq!(asked(&fake), before, "a second tick asks nothing: {:?}", fake.seen());
    assert_eq!(shown(&env.svc).await.len(), 2);
}

#[tokio::test]
async fn a_draft_keeps_a_follow_up_and_your_answer_ends_it() {
    let fake = fake_decisions().await;
    let env = env(&fake.url);
    let vault = env.svc.get().unwrap();
    let thread = ThreadId::new();
    let dinner = mail(&env, thread, ADA, &[ME], "Dinner on Saturday?", DAY);
    store(&env, env.inbox, 1, &dinner, "Are you free on Saturday?");

    karmayoga_service::mailai::categorize_tick(&env.svc).await;
    assert_eq!(
        shown(&env.svc).await,
        vec![("Dinner on Saturday?".to_string(), FollowUpKind::Reply)]
    );
    assert_eq!(fake.subjects(Question::Reply).len(), 1);

    // A reply being written answers nobody yet, and costs no new question.
    let mut draft =
        mail(&env, thread, ME, &[ADA], "Re: Dinner on Saturday?", SignedDuration::from_mins(2));
    draft.flags.draft = true;
    store(&env, env.drafts, 1, &draft, "Yes, I");
    karmayoga_service::mailai::categorize_tick(&env.svc).await;
    assert_eq!(
        shown(&env.svc).await,
        vec![("Dinner on Saturday?".to_string(), FollowUpKind::Reply)]
    );
    assert_eq!(fake.subjects(Question::Reply).len(), 1, "not asked again for a draft");
    let (t, _) = vault.thread(thread).unwrap();
    assert_eq!(t.follow_up_asked.map(|m| m.message_count), Some(2), "marked at the new count");

    // Sent: nothing waits on you, and your answer is too new to nudge about.
    let answer =
        mail(&env, thread, ME, &[ADA], "Re: Dinner on Saturday?", SignedDuration::from_mins(1));
    store(&env, env.sent, 1, &answer, "Yes, I would love to.");
    karmayoga_service::mailai::categorize_tick(&env.svc).await;
    assert!(shown(&env.svc).await.is_empty());
    assert_eq!(fake.subjects(Question::Reply).len(), 1);
    assert!(fake.subjects(Question::Nudge).is_empty());
}

#[tokio::test]
async fn nothing_is_asked_or_shown_with_follow_ups_off() {
    let fake = fake_decisions().await;
    let env = env(&fake.url);
    let vault = env.svc.get().unwrap();
    let mut sorting = vault.mail_sorting().unwrap();
    sorting.follow_up.on = false;
    vault.save_mail_sorting(&sorting).unwrap();
    let dinner = mail(&env, ThreadId::new(), ADA, &[ME], "Dinner on Saturday?", DAY);
    store(&env, env.inbox, 1, &dinner, "Are you free on Saturday?");

    karmayoga_service::mailai::categorize_tick(&env.svc).await;

    assert!(!fake.subjects(Question::Sort).is_empty(), "sorting still runs");
    assert!(fake.subjects(Question::Reply).is_empty());
    assert!(shown(&env.svc).await.is_empty());
}
