//! Checks before sending end to end, against a fake model that keeps what it
//! was sent: which model is asked, what it is shown of a draft, and what the
//! person is told when it cannot be asked. The warning is only as good as what
//! reaches the model, and only the whole path -- the stored draft, the
//! account's consent, the model's own wire -- can say a signature and a quote
//! stayed out of it.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use karmayoga_core::account::{Account, MailAi, Provider};
use karmayoga_core::id::{BlobId, DraftId};
use karmayoga_core::llm::{DecisionApi, DecisionConfig, LLMConfig, LLMModelConfig, ModelSettings};
use karmayoga_core::mail::{Address, Draft, DraftAttachment, MailWriting, Origin, SendCheck};
use karmayoga_service::{Ctx, Service};
use serde_json::{Value, json};

// ---- a model that remembers ------------------------------------------------

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

/// A chat model on loopback that answers every request by calling `submit`
/// with `arguments`.
async fn fake_model(arguments: Value) -> FakeModel {
    use tokio::io::AsyncWriteExt;
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
                let Some(body) = read_request(&mut socket).await else { return };
                sent.lock().unwrap().push(serde_json::from_slice(&body).unwrap());
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

/// The body of one HTTP request, once all of it has arrived.
async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let length = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        if buf.len() >= head_end + 4 + length {
            return Some(buf[head_end + 4..head_end + 4 + length].to_vec());
        }
    }
}

// ---- a decision model that keeps what it was asked -------------------------

struct FakeDecisions {
    url: String,
    sent: Arc<Mutex<Vec<Value>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl FakeDecisions {
    fn asked(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// What each question was, in the order they arrived -- which, asked all at
    /// once, is no order a test can lean on, so sorted.
    fn questions(&self) -> Vec<Value> {
        let mut all: Vec<Value> =
            self.sent.lock().unwrap().iter().map(|r| r["questions"]["category"].clone()).collect();
        all.sort_by_key(|q| q["instructions"].as_str().unwrap_or_default().to_string());
        all
    }
}

/// A TypeSafe-shaped decision server on loopback that answers `fail` to a
/// question whose instructions contain `fails`, and `pass` to every other.
async fn fake_decisions(fails: &'static str) -> FakeDecisions {
    decision_server("200 OK", Some(fails)).await
}

/// One that is down: every request is answered 503.
async fn failing_decisions() -> FakeDecisions {
    decision_server("503 Service Unavailable", None).await
}

async fn decision_server(status: &'static str, fails: Option<&'static str>) -> FakeDecisions {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_for_task = sent.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let sent = sent_for_task.clone();
            tokio::spawn(async move {
                let Some(body) = read_request(&mut socket).await else { return };
                let request: Value = serde_json::from_slice(&body).unwrap();
                let instructions =
                    request["questions"]["category"]["instructions"].as_str().unwrap_or_default();
                let pick = match fails {
                    Some(fails) if instructions.contains(fails) => "fail",
                    _ => "pass",
                };
                sent.lock().unwrap().push(request);
                let payload = if status.starts_with("200") {
                    json!({
                        "model": "jev-latest",
                        "answers": { "category": { "type": "choice", "choice": pick, "confidence": 0.9 } },
                        "usage": { "input_tokens": 100, "output_tokens": 10 },
                    })
                } else {
                    json!({ "error": "down" })
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    FakeDecisions { url: format!("http://127.0.0.1:{port}/v1/systemone"), sent, _shutdown: tx }
}

// ---- fixtures --------------------------------------------------------------

struct Env {
    svc: Arc<Service>,
    account: Account,
    _dir: tempfile::TempDir,
}

fn typesafe(url: &str) -> DecisionConfig {
    DecisionConfig {
        api: DecisionApi::TypeSafe,
        url: Some(url.to_string()),
        model: DecisionApi::TypeSafe.default_model().into(),
    }
}

/// A vault whose Medium and Small models are the one at `endpoint`, and whose
/// one account has writing help on and has acknowledged them. `edit` changes
/// the models before they are saved -- and before the account acknowledges
/// them, which is of whatever is there then.
fn env_with(endpoint: &str, edit: impl FnOnce(&mut ModelSettings)) -> Env {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
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
    edit(&mut models);
    vault.save_model_settings(&models).unwrap();

    let mut account = Account::new(Provider::Custom, "hari@example.com");
    account.display_name = "Hari G".into();
    account.services.mail = true;
    account.mail_ai = MailAi { writing: true, ..Default::default() };
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(&account).unwrap();
    Env { svc, account, _dir: dir }
}

fn env(endpoint: &str) -> Env {
    env_with(endpoint, |_| {})
}

/// [`env`] with a decision model at `url`, keyed.
fn env_with_decision(endpoint: &str, url: &str) -> Env {
    let e = env_with(endpoint, |models| models.decision = Some(typesafe(url)));
    e.svc.get().unwrap().set_model_key(karmayoga_core::llm::ModelSlot::Decision, "ds-key").unwrap();
    e
}

/// Settings → Mail writing with these checks, each switched on.
fn check_for(e: &Env, texts: &[&str]) {
    let send_checks = texts.iter().map(|t| SendCheck { text: (*t).into(), on: true }).collect();
    let writing = MailWriting { send_checks, ..Default::default() };
    e.svc.get().unwrap().save_mail_writing(&writing).unwrap();
}

const SIGNED: &str = r#"<div data-signature=""><p>Hari G</p><p>ViyaMD</p></div>"#;
const QUOTE: &str = r#"<p>On Thu, 5 Nov 2026 at 08:00, Dana Scully wrote:</p><blockquote type="cite"><p>Do you have an hour next week?</p></blockquote>"#;

/// A reply to Dana, saved as compose saves one: what was typed, the signature
/// under it, and the quote of what is being answered.
fn save_reply(e: &Env) -> DraftId {
    let mut draft = Draft::new(e.account.id, &e.account.address, Origin::Person);
    draft.to = vec![Address::new("Dana Scully", "dana@example.com")];
    draft.subject = "Re: Catch up next week?".into();
    draft.body_html = format!("<p>Hi Dana,</p><p></p><p>Tuesday works for me.</p>{SIGNED}{QUOTE}");
    draft.attachments = vec![DraftAttachment {
        blob: BlobId::of(b"%PDF"),
        filename: "agenda.pdf".into(),
        mime_type: "application/pdf".into(),
    }];
    e.svc.get().unwrap().save_draft(&draft).unwrap();
    draft.id
}

async fn check(e: &Env, id: DraftId) -> Value {
    e.svc.call(Ctx::local(), "check_draft", json!({ "id": id })).await.expect("check_draft")
}

fn results(report: &Value) -> Vec<(String, bool)> {
    report["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["check"].as_str().unwrap().to_string(), r["passed"].as_bool().unwrap()))
        .collect()
}

/// What the Small model says when it passes the first check and fails the
/// second.
fn one_pass_one_fail() -> Value {
    json!({ "results": [
        { "check": 1, "passed": true, "note": "" },
        { "check": 2, "passed": false, "note": "Fix the missing comma after the greeting." },
    ]})
}

// ---- nothing to check ------------------------------------------------------

#[tokio::test]
async fn with_no_checks_nothing_is_asked_of_anybody() {
    let fake = fake_model(one_pass_one_fail()).await;
    let decisions = fake_decisions("anything").await;
    let e = env_with_decision(&fake.endpoint, &decisions.url);
    let id = save_reply(&e);

    assert_eq!(check(&e, id).await, json!({ "results": [] }));
    assert_eq!((fake.asked(), decisions.asked()), (0, 0), "no request at all");
}

#[tokio::test]
async fn a_check_switched_off_is_left_in_the_list_and_not_asked() {
    let fake = fake_model(json!({ "results": [{ "check": 1, "passed": true, "note": "" }] })).await;
    let e = env(&fake.endpoint);
    let writing = MailWriting {
        send_checks: vec![
            SendCheck { text: " The tone is friendly ".into(), on: true },
            SendCheck { text: "The language is formal".into(), on: false },
        ],
        ..Default::default()
    };
    e.svc.get().unwrap().save_mail_writing(&writing).unwrap();

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(results(&report), [("The tone is friendly".to_string(), true)]);
    let (system, _) = fake.prompt(0);
    assert!(system.contains("1. The tone is friendly"), "{system}");
    assert!(!system.contains("formal"), "{system}");
}

#[tokio::test]
async fn a_draft_that_is_not_there_is_an_error_and_asks_nobody() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly"]);

    let err = e.svc.call(Ctx::local(), "check_draft", json!({ "id": DraftId::new() })).await;

    assert_eq!(err.unwrap_err().code, "not_found");
    assert_eq!(fake.asked(), 0);
}

// ---- the Small model ---------------------------------------------------------

#[tokio::test]
async fn the_small_model_says_what_passed_and_what_to_change() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly", "The grammar is correct"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(
        report,
        json!({
            "results": [
                { "check": "The tone is friendly", "passed": true },
                {
                    "check": "The grammar is correct", "passed": false,
                    "note": "Fix the missing comma after the greeting."
                },
            ],
            "model": "small",
        })
    );
    // Every check in one exchange -- the tool call and its result -- not one
    // each.
    assert!(fake.asked() <= 2, "asked {} times", fake.asked());
}

#[tokio::test]
async fn the_model_reads_their_words_who_it_is_to_and_what_is_attached_and_no_more() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly", "The grammar is correct"]);

    check(&e, save_reply(&e)).await;

    let (system, user) = fake.prompt(0);
    assert!(system.contains("1. The tone is friendly\n2. The grammar is correct"), "{system}");
    assert!(user.contains("Tuesday works for me."), "{user}");
    assert!(user.contains("Dana Scully <dana@example.com>"), "{user}");
    assert!(user.contains("Re: Catch up next week?"), "{user}");
    assert!(user.contains("agenda.pdf"), "{user}");
    for left_out in ["hour next week", "wrote:"] {
        assert!(!user.contains(left_out), "{left_out:?} reached the model: {user}");
        assert!(!system.contains(left_out), "{left_out:?} reached the model: {system}");
    }
    // The signature compose added is given as what it is -- under the person's
    // own words, never among them -- so "signs off with my name" can be met by it
    // and the quote of what they answer cannot be judged as theirs.
    assert!(
        user.contains("signature, added under the body automatically: Hari G ViyaMD"),
        "{user}"
    );
    let words = user.split_once("body:\n").expect("the body").1;
    assert!(!words.contains("ViyaMD") && !words.contains("Hari G"), "{words}");
    assert!(!system.contains("ViyaMD"), "{system}");
    // And what day it is, for checks about dates.
    let today = jiff::Zoned::now().strftime("Today is %A %-d %B %Y.").to_string();
    assert!(user.starts_with(&today), "{user}");
}

#[tokio::test]
async fn nothing_attached_is_said_so_that_a_forgotten_attachment_can_be_caught() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly", "If I mention an attachment, it is attached"]);
    let mut draft = Draft::new(e.account.id, &e.account.address, Origin::Person);
    draft.to = vec![Address::new("Dana Scully", "dana@example.com")];
    draft.subject = "The agenda".into();
    draft.body_html = "<p>Hi Dana, the agenda is attached.</p>".into();
    e.svc.get().unwrap().save_draft(&draft).unwrap();

    check(&e, draft.id).await;

    let (_, user) = fake.prompt(0);
    assert!(user.contains("attached: nothing\n"), "{user}");
}

#[tokio::test]
async fn a_check_the_small_model_does_not_answer_fails_rather_than_passes() {
    let fake = fake_model(json!({ "results": [{ "check": 1, "passed": true, "note": "" }] })).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly", "The grammar is correct"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(
        report["results"][1],
        json!({
            "check": "The grammar is correct", "passed": false,
            "note": "The model did not answer this one."
        })
    );
    assert_eq!(report["results"][0]["passed"], true);
}

#[tokio::test]
async fn a_small_model_that_names_none_of_the_checks_could_not_run_them() {
    let fake = fake_model(json!({ "results": [] })).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(report["results"], json!([]));
    assert_eq!(report["model"], "small");
    let skipped = report["skipped"].as_str().unwrap();
    assert!(skipped.starts_with("The checks could not run: "), "{skipped}");
    assert!(report.get("off").is_none(), "a failure this time is not how things are set up");
}

// ---- consent and no model --------------------------------------------------------

#[tokio::test]
async fn an_account_without_writing_help_is_not_read_and_the_report_says_it_is_off() {
    let fake = fake_model(one_pass_one_fail()).await;
    let decisions = fake_decisions("anything").await;
    let e = env_with_decision(&fake.endpoint, &decisions.url);
    check_for(&e, &["The tone is friendly"]);
    let vault = e.svc.get().unwrap();
    let mut account = e.account.clone();
    account.mail_ai.writing = false;
    vault.save_account(&account).unwrap();

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(report["results"], json!([]));
    assert!(report.get("model").is_none(), "nothing was asked: {report}");
    assert!(report.get("skipped").is_none(), "not a failure of this time: {report}");
    let off = report["off"].as_str().unwrap();
    assert!(off.contains("writing help"), "{off}");
    assert!(off.contains("hari@example.com"), "{off}");
    assert_eq!((fake.asked(), decisions.asked()), (0, 0), "no request at all");
}

#[tokio::test]
async fn mail_not_acknowledged_for_the_model_is_not_sent_to_it() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env(&fake.endpoint);
    check_for(&e, &["The tone is friendly"]);
    let mut account = e.account.clone();
    account.assistant_provider_acknowledged = None;
    e.svc.get().unwrap().save_account(&account).unwrap();

    let report = check(&e, save_reply(&e)).await;

    assert!(report.get("skipped").is_none(), "{report}");
    let off = report["off"].as_str().unwrap();
    assert!(off.contains("may reach"), "{off}");
    assert_eq!(fake.asked(), 0);
}

#[tokio::test]
async fn with_no_model_switched_on_the_report_says_which_to_switch_on() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env_with(&fake.endpoint, |models| models.small = None);
    check_for(&e, &["The tone is friendly"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(
        report,
        json!({
            "results": [],
            "off": "No model can check mail before it is sent: switch on a decision \
                    model or the Small model in Settings \u{2192} Models."
        })
    );
    assert_eq!(fake.asked(), 0, "the Medium model is never asked in their place");
}

// ---- the decision model --------------------------------------------------------------

#[tokio::test]
async fn a_decision_model_is_asked_one_question_per_check() {
    let fake = fake_model(one_pass_one_fail()).await;
    let decisions = fake_decisions("grammar").await;
    let e = env_with_decision(&fake.endpoint, &decisions.url);
    check_for(&e, &["The tone is friendly", "The grammar is correct"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(
        report,
        json!({
            "results": [
                { "check": "The tone is friendly", "passed": true },
                { "check": "The grammar is correct", "passed": false },
            ],
            "model": "decision",
        }),
        "no note: a decision model does not give one"
    );
    assert_eq!(decisions.asked(), 2, "one each");
    assert_eq!(fake.asked(), 0, "and the Small model is not asked as well");

    let questions = decisions.questions();
    let grammar = &questions[0]["instructions"].as_str().unwrap();
    let tone = &questions[1]["instructions"].as_str().unwrap();
    assert!(grammar.contains("\"The grammar is correct\""), "{grammar}");
    assert!(tone.contains("\"The tone is friendly\""), "{tone}");
    for question in &questions {
        let mut answers: Vec<&str> =
            question["criteria"].as_object().unwrap().keys().map(String::as_str).collect();
        answers.sort();
        assert_eq!(answers, ["fail", "pass"]);
    }
}

#[tokio::test]
async fn a_decision_model_is_shown_the_draft_as_an_object_and_not_the_quote_or_signature() {
    let fake = fake_model(one_pass_one_fail()).await;
    let decisions = fake_decisions("anything").await;
    let e = env_with_decision(&fake.endpoint, &decisions.url);
    check_for(&e, &["The tone is friendly"]);

    check(&e, save_reply(&e)).await;

    let state = decisions.sent.lock().unwrap()[0]["state"].clone();
    assert_eq!(state["to"], "Dana Scully <dana@example.com>");
    assert_eq!(state["subject"], "Re: Catch up next week?");
    assert_eq!(state["attachments"], "agenda.pdf");
    let body = state["body"].as_str().unwrap();
    assert!(body.contains("Tuesday works for me."), "{body}");
    for left_out in ["ViyaMD", "hour next week", "wrote:"] {
        assert!(!body.contains(left_out), "{left_out:?} reached the model: {body}");
    }
    // The signature is a field of its own, not part of what they wrote; and the
    // day is there for checks about dates.
    assert_eq!(state["signature"], "Hari G ViyaMD");
    assert!(state["today"].as_str().unwrap().starts_with("Today is "), "{state}");
}

#[tokio::test]
async fn a_decision_model_that_is_down_is_a_warning_that_says_which_model_it_was() {
    let fake = fake_model(one_pass_one_fail()).await;
    let decisions = failing_decisions().await;
    let e = env_with_decision(&fake.endpoint, &decisions.url);
    check_for(&e, &["The tone is friendly", "The grammar is correct"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(report["results"], json!([]));
    assert_eq!(report["model"], "decision");
    let skipped = report["skipped"].as_str().unwrap();
    assert!(skipped.starts_with("The checks could not run: "), "{skipped}");
    assert!(skipped.ends_with('.'), "{skipped}");
    assert!(report.get("off").is_none(), "a model being down is not how things are set up");
    assert_eq!(fake.asked(), 0, "not handed to the Small model once it has been asked");
}

#[tokio::test]
async fn a_decision_model_that_cannot_be_used_gives_way_to_the_small_model() {
    let fake = fake_model(one_pass_one_fail()).await;
    // Switched on at an address that needs a key, and no key stored.
    let e = env_with(&fake.endpoint, |models| {
        models.decision = Some(typesafe("https://decide.example.com/v1/systemone"));
    });
    check_for(&e, &["The tone is friendly", "The grammar is correct"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(report["model"], "small");
    assert_eq!(results(&report).len(), 2);
    assert!(report.get("skipped").is_none(), "{report}");
}

#[tokio::test]
async fn a_decision_model_that_cannot_be_used_and_no_small_model_is_off_and_says_why() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env_with(&fake.endpoint, |models| {
        models.small = None;
        models.decision = Some(typesafe("https://decide.example.com/v1/systemone"));
    });
    check_for(&e, &["The tone is friendly"]);

    let report = check(&e, save_reply(&e)).await;

    assert_eq!(report["results"], json!([]));
    assert!(report.get("model").is_none(), "nothing was asked: {report}");
    assert!(report.get("skipped").is_none(), "{report}");
    let off = report["off"].as_str().unwrap();
    assert!(off.contains("decision model"), "{off}");
    assert!(off.contains("no API key"), "{off}");
    assert_eq!(fake.asked(), 0);
}

// ---- the limiter ----------------------------------------------------------------

#[tokio::test]
async fn too_many_checks_in_a_minute_warn_this_time_and_do_not_switch_them_off() {
    let fake = fake_model(one_pass_one_fail()).await;
    let e = env(&fake.endpoint);
    // A clock that stands still, so that no token comes back while it runs.
    e.svc.set_clock(Arc::new(support::clock::FakeClock::at(jiff::Timestamp::now())));
    check_for(&e, &["The tone is friendly"]);
    let id = save_reply(&e);

    for _ in 0..60 {
        let report = check(&e, id).await;
        assert!(report.get("skipped").is_none(), "{report}");
    }
    let asked = fake.asked();

    let report = check(&e, id).await;
    assert_eq!(
        report,
        json!({
            "results": [],
            "skipped": "The checks could not run: too many mail actions in the last minute; \
                        slow down."
        })
    );
    assert_eq!(fake.asked(), asked, "the model is not asked once the limiter has said no");
}
