//! Sorting mail on a decision model (`everyday_service::mailsort`), end to
//! end against a fake decision server on loopback that speaks either
//! TypeSafe's or OpenAI's shape.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::llm::{DecisionApi, DecisionConfig, ModelSettings, ModelTier};
use everyday_core::mail::{
    Address, Body, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

// ---- a fake decision server -------------------------------------------------

/// One request the fake was sent: its `Authorization` header and body.
#[derive(Debug, Clone)]
struct Seen {
    authorization: Option<String>,
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
}

/// The subject a request was about, in either shape.
fn subject_of(api: DecisionApi, body: &Value) -> String {
    match api {
        DecisionApi::TypeSafe => body["state"]["subject"].as_str().unwrap_or("").to_string(),
        DecisionApi::OpenAi => body["input"]
            .as_str()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("subject: ")))
            .unwrap_or("")
            .to_string(),
    }
}

/// Answers "Receipts" for anything whose subject mentions an order and
/// "People" for everything else, in `api`'s shape.
async fn fake_decisions(api: DecisionApi) -> FakeDecisions {
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
                let (head, body) = loop {
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
                    break (head, raw[end + 4..].to_vec());
                };
                let authorization = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("authorization").then(|| v.trim().to_string())
                });
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                // A message the model will not take, every time it is asked.
                if subject_of(api, &body).to_lowercase().contains("refuse") {
                    seen.lock().unwrap().push(Seen { authorization, body });
                    let detail = r#"{"detail":[{"msg":"content not accepted"}]}"#;
                    let response = format!(
                        "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{detail}",
                        detail.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                    return;
                }
                let pick = if subject_of(api, &body).to_lowercase().contains("order") {
                    "Receipts"
                } else {
                    "People"
                };
                seen.lock().unwrap().push(Seen { authorization, body });
                let answer = match api {
                    DecisionApi::TypeSafe => json!({
                        "model": "jev-latest",
                        "answers": { "category": { "type": "choice", "choice": pick, "confidence": 0.9 } },
                        "usage": { "input_tokens": 100, "output_tokens": 10 },
                    }),
                    DecisionApi::OpenAi => json!({
                        "answers": [{ "type": "choice", "name": "category", "choice": pick, "confidence": 0.9 }],
                    }),
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    let path = match api {
        DecisionApi::TypeSafe => "/v1/systemone",
        DecisionApi::OpenAi => "/v1/decisions",
    };
    FakeDecisions { url: format!("http://127.0.0.1:{port}{path}"), seen, _shutdown: tx }
}

// ---- fixtures ---------------------------------------------------------------

/// A vault whose decision model is the fake, keyed, and an account that
/// allows sorting and has acknowledged exactly these endpoints.
fn env(api: DecisionApi, url: &str) -> (Arc<Service>, tempfile::TempDir, Account, MailboxId) {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let models = ModelSettings {
        decision: Some(DecisionConfig {
            api,
            url: Some(url.to_string()),
            model: api.default_model().into(),
        }),
        ..ModelSettings::default()
    };
    vault.save_model_settings(&models).unwrap();
    vault.set_model_key(everyday_core::llm::ModelSlot::Decision, "ds-key").unwrap();

    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    account.mail_ai = everyday_core::account::MailAi {
        categorize: true,
        summaries: false,
        auto_draft: false,
        writing: false,
    };
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(&account).unwrap();
    let inbox = Mailbox::new(account.id, "INBOX", MailboxRole::Inbox);
    vault.save_mailbox(&inbox).unwrap();
    (svc, dir, account, inbox.id)
}

fn seed(
    svc: &Arc<Service>,
    account: AccountId,
    inbox: MailboxId,
    uid: u32,
    subject: &str,
    age: SignedDuration,
) -> Message {
    let vault = svc.get().unwrap();
    let message = Message {
        id: MailMessageId::new(),
        account_id: account,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{}@example.com>", uuid::Uuid::new_v4()),
        date: Timestamp::now() - age,
        from: Address::bare("someone@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: format!("About: {subject}"),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        // What the rules said at ingest. The decision model sorts mail the
        // rules already filed too.
        category: Some(Category::Important),
        category_source: CategorySource::Rules,
        invite: None,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 1 },
        gmail: None,
    };
    vault
        .ingest_mail(account, vec![IngestMessage { message: message.clone(), mailbox: inbox, uid }])
        .unwrap();
    vault
        .save_body(&Body {
            message_id: message.id,
            html_sanitised: String::new(),
            text: format!("Body of {subject}"),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        })
        .unwrap();
    message
}

const DAY: SignedDuration = SignedDuration::from_hours(24);

// ---- the tests ----------------------------------------------------------------

#[tokio::test]
async fn recent_mail_is_sorted_into_the_persons_own_categories_over_typesafe() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::TypeSafe, &fake.url);
    let order = seed(&svc, account.id, inbox, 1, "Your order has shipped", DAY);
    let lunch = seed(&svc, account.id, inbox, 2, "Lunch on Friday?", DAY * 2);

    everyday_service::mailai::categorize_tick(&svc).await;

    let vault = svc.get().unwrap();
    let (thread, _) = vault.thread(order.thread_id).unwrap();
    assert_eq!(thread.category, Some(Category::Notification), "Receipts files under Notifications");
    assert_eq!(thread.sorted_as.as_ref().map(|s| s.category.as_str()), Some("receipts"));
    let (thread, _) = vault.thread(lunch.thread_id).unwrap();
    assert_eq!(thread.category, Some(Category::Important));
    assert_eq!(thread.sorted_as.as_ref().map(|s| s.category.as_str()), Some("people"));

    let seen = fake.seen();
    assert_eq!(seen.len(), 2, "one request per thread");
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer ds-key"));
    let q = &seen[0].body["questions"]["category"];
    assert_eq!(q["type"], "choice");
    // Every category, by name, with the person's description as criteria.
    for name in ["Needs you", "People", "Newsletters", "Receipts", "Notifications", "Other"] {
        assert!(q["criteria"][name].is_string(), "{name} offered: {q}");
    }
    assert!(seen[0].body["state"]["excerpt"].as_str().unwrap().starts_with("Body of"));

    // Asked once; a second tick has nothing new to ask about.
    everyday_service::mailai::categorize_tick(&svc).await;
    assert_eq!(fake.seen().len(), 2, "a sorted thread is not asked about again");
}

#[tokio::test]
async fn the_openai_shape_sorts_the_same_way() {
    let fake = fake_decisions(DecisionApi::OpenAi).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::OpenAi, &fake.url);
    let order = seed(&svc, account.id, inbox, 1, "Order 1234 receipt", DAY);

    everyday_service::mailai::categorize_tick(&svc).await;

    let (thread, _) = svc.get().unwrap().thread(order.thread_id).unwrap();
    assert_eq!(thread.category, Some(Category::Notification));
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body["model"], "gpt-6-luna");
    let q = &seen[0].body["questions"][0];
    assert_eq!(q["name"], "category");
    assert_eq!(q["choices"].as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn older_mail_waits_until_somebody_asks() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::TypeSafe, &fake.url);
    let recent = seed(&svc, account.id, inbox, 1, "Recent order", DAY * 3);
    let old = seed(&svc, account.id, inbox, 2, "Old order", DAY * 200);

    everyday_service::mailai::categorize_tick(&svc).await;
    let vault = svc.get().unwrap();
    assert!(vault.thread(recent.thread_id).unwrap().0.sorted_as.is_some());
    assert!(
        vault.thread(old.thread_id).unwrap().0.sorted_as.is_none(),
        "mail older than the window is not sorted unasked"
    );
    assert_eq!(fake.seen().len(), 1);

    let view = svc
        .call(Ctx::local(), "sort_older_mail", json!({ "days": 365 }))
        .await
        .expect("sort_older_mail");
    assert_eq!(view["older"].as_array().unwrap().len(), 1, "{view}");

    everyday_service::mailai::categorize_tick(&svc).await;
    let (thread, _) = vault.thread(old.thread_id).unwrap();
    assert_eq!(thread.sorted_as.map(|s| s.category), Some("receipts".to_string()));
    assert_eq!(fake.seen().len(), 2, "the recent thread was not asked again");

    // The sort reached the end of the year and finished.
    everyday_service::mailai::categorize_tick(&svc).await;
    let view = svc.call(Ctx::local(), "mail_sorting", json!({})).await.unwrap();
    assert!(view["older"].as_array().unwrap().is_empty(), "{view}");
}

#[tokio::test]
async fn a_persons_own_filing_is_left_alone_and_costs_no_request() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::TypeSafe, &fake.url);
    let msg = seed(&svc, account.id, inbox, 1, "Order from a friend", DAY);
    let vault = svc.get().unwrap();
    vault.set_thread_priority(msg.thread_id, true).unwrap();

    everyday_service::mailai::categorize_tick(&svc).await;

    assert_eq!(vault.thread(msg.thread_id).unwrap().0.category, Some(Category::Priority));
    assert!(fake.seen().is_empty(), "nothing to ask: the answer could not be used");
}

#[tokio::test]
async fn a_decision_model_with_no_key_sorts_nothing_rather_than_falling_back() {
    // The fake stands in for the small model here: anything posted to it
    // means sorting fell back to Small.
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::TypeSafe, &fake.url);
    let vault = svc.get().unwrap();
    let mut models = vault.model_settings().unwrap();
    models.decision = Some(DecisionConfig {
        api: DecisionApi::TypeSafe,
        url: Some("https://decisions.example.invalid/v1/systemone".into()),
        model: "jev-latest".into(),
    });
    let small_url = fake.url.trim_end_matches("/v1/systemone").to_string() + "/v1";
    models.small = Some(everyday_core::llm::LLMConfig {
        connection: everyday_core::llm::LLMProviderConfig {
            base_url: Some(small_url),
            ..Default::default()
        },
        model: everyday_core::llm::LLMModelConfig::quick(),
    });
    vault.save_model_settings(&models).unwrap();
    let mut account = vault.account(account.id).unwrap();
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(&account).unwrap();
    assert!(vault.decision_access().is_err(), "a remote decision model needs a key");
    assert!(vault.model_access(ModelTier::Small).is_ok(), "the small model could be asked");
    let msg = seed(&svc, account.id, inbox, 1, "Your order", DAY);

    everyday_service::mailai::categorize_tick(&svc).await;

    assert!(fake.seen().is_empty(), "the small model was not asked instead");
    let (thread, _) = vault.thread(msg.thread_id).unwrap();
    assert_eq!(thread.category, Some(Category::Important), "left as the rules filed it");
}

#[tokio::test]
async fn the_categories_are_saved_and_checked() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, _account, _inbox) = env(DecisionApi::TypeSafe, &fake.url);

    let view = svc.call(Ctx::local(), "mail_sorting", json!({})).await.unwrap();
    assert_eq!(view["sorting"]["windowDays"], 30);
    assert!(view["decisionModel"].as_str().unwrap().starts_with("jev-latest at http://127.0.0.1"));

    let mut sorting = view["sorting"].clone();
    sorting["categories"][1]["name"] = json!("Needs you");
    let err = svc
        .call(Ctx::local(), "save_mail_sorting", json!({ "sorting": sorting }))
        .await
        .unwrap_err();
    assert!(err.message.contains("two categories"), "{}", err.message);

    let mine = json!({
        "categories": [
            { "id": "travel", "name": "Travel", "description": "Flights and hotels.", "treatAs": "important" },
            { "id": "rest", "name": "Everything else", "treatAs": "other" },
        ],
        "windowDays": 7,
    });
    let view =
        svc.call(Ctx::local(), "save_mail_sorting", json!({ "sorting": mine })).await.unwrap();
    assert_eq!(view["sorting"]["categories"][0]["name"], "Travel");
    assert_eq!(view["sorting"]["windowDays"], 7);
}

#[tokio::test]
async fn a_message_the_model_will_not_take_is_passed_not_retried_forever() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::TypeSafe, &fake.url);
    let bad = seed(&svc, account.id, inbox, 1, "Please refuse me", DAY);
    let good = seed(&svc, account.id, inbox, 2, "Your order", DAY * 2);

    everyday_service::mailai::categorize_tick(&svc).await;
    let vault = svc.get().unwrap();
    assert!(vault.thread(good.thread_id).unwrap().0.sorted_as.is_some());
    let (thread, _) = vault.thread(bad.thread_id).unwrap();
    assert!(thread.sorted_as.is_none());
    assert!(thread.ai_sorted.is_some(), "marked asked, so it is not asked again");
    assert_eq!(fake.seen().len(), 2);

    everyday_service::mailai::categorize_tick(&svc).await;
    assert_eq!(fake.seen().len(), 2, "the refused message costs no second request");

    // Nor does it stop an older sort getting past it.
    svc.call(Ctx::local(), "sort_older_mail", json!({ "days": 365, "again": true })).await.unwrap();
    everyday_service::mailai::categorize_tick(&svc).await;
    everyday_service::mailai::categorize_tick(&svc).await;
    let view = svc.call(Ctx::local(), "mail_sorting", json!({})).await.unwrap();
    assert!(view["older"].as_array().unwrap().is_empty(), "the re-sort finished: {view}");
    assert_eq!(fake.seen().len(), 4, "each asked once more by the re-sort, then done");
}

#[tokio::test]
async fn a_thread_the_small_model_already_looked_at_is_still_sorted() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, inbox) = env(DecisionApi::TypeSafe, &fake.url);
    let msg = seed(&svc, account.id, inbox, 1, "Your order", DAY);
    let vault = svc.get().unwrap();
    // What months of sorting on Small leave behind.
    vault.set_thread_ai_categorize_asked(msg.thread_id, 1).unwrap();

    everyday_service::mailai::categorize_tick(&svc).await;

    assert_eq!(fake.seen().len(), 1);
    assert!(vault.thread(msg.thread_id).unwrap().0.sorted_as.is_some());
}

#[tokio::test]
async fn a_stopped_or_restarted_sort_is_not_written_back_over() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, _inbox) = env(DecisionApi::TypeSafe, &fake.url);
    svc.call(Ctx::local(), "sort_older_mail", json!({ "days": 365 })).await.unwrap();
    let first = svc.mail_older_sort(account.id).unwrap();

    // Restarted -- with `again` this time -- while a tick holds the first.
    svc.call(Ctx::local(), "sort_older_mail", json!({ "again": true })).await.unwrap();
    svc.update_mail_older_sort(account.id, first.run, |s| {
        s.asked += 40;
        true
    });
    let now = svc.mail_older_sort(account.id).unwrap();
    assert!(now.again && now.since.is_none() && now.asked == 0, "the new run stands: {now:?}");

    // Stopped while a tick holds it: it stays stopped.
    svc.call(Ctx::local(), "stop_sorting_older_mail", json!({})).await.unwrap();
    svc.update_mail_older_sort(account.id, now.run, |_| true);
    assert!(svc.mail_older_sort(account.id).is_none());
}

#[tokio::test]
async fn switching_the_decision_model_off_ends_a_sort_of_older_mail() {
    let fake = fake_decisions(DecisionApi::TypeSafe).await;
    let (svc, _dir, account, _inbox) = env(DecisionApi::TypeSafe, &fake.url);
    svc.call(Ctx::local(), "sort_older_mail", json!({})).await.unwrap();
    assert!(svc.mail_older_sort(account.id).is_some());

    let vault = svc.get().unwrap();
    let mut models = vault.model_settings().unwrap();
    models.decision = None;
    vault.save_model_settings(&models).unwrap();
    everyday_service::mailai::categorize_tick(&svc).await;

    assert!(svc.mail_older_sort(account.id).is_none(), "not shown as running any more");
}
