//! Writing by voice, end to end against a fake model: a spoken turn at this
//! computer's desktop is offered `rewrite_selection` and `write_message` with
//! `type_text`, "improve this" rewrites what is selected in place, "write
//! that..." types a message of the kind asked for, and what the person must be
//! shown first is handed back to be typed through `type_text`, where it is.
//!
//! One fake model plays two parts, as the real one does: the assistant
//! holding the conversation, which streams a scripted tool call per request,
//! and the medium model `deskwrite` asks for the text, which is asked for its
//! answer as a `submit` call and replies with the next of `answers`. They are
//! told apart by the `submit` tool, which only the second is offered.
//!
//! The harness is `tests/voice_dictate.rs`'s, with that second part added.

use futures::future::BoxFuture;
use jatayu_core::ConversationId;
use jatayu_core::writing::WritingOutcome;
use jatayu_service::Service;
use jatayu_service::agent::{AgentEvent, ConfirmAnswer, Reach, Sink, Turn, Typist};
use jatayu_service::voice::assistant::ServiceAssistant;
use jatayu_service::voice::{DICTATE_TOOL, VoiceAssistant};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A vault with an assistant -- and a medium model for the writing -- pointed
/// at `endpoint`.
fn service(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = jatayu_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = jatayu_vault::create(dir.path(), config).unwrap();
    let settings = jatayu_core::AgentSettings {
        enabled: true,
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = jatayu_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

/// What the fake model says: the assistant's tool calls, answered in turn
/// -- the tool's name and its arguments as JSON -- then its final message;
/// and what the medium model gives back for each text it is asked to write,
/// the last one over again if it is asked more often than that.
#[derive(Clone)]
struct Script {
    calls: Vec<(String, String)>,
    then: String,
    answers: Vec<String>,
}

impl Script {
    fn new(calls: &[(&str, &str)], answers: &[&str]) -> Self {
        Self {
            calls: calls.iter().map(|(t, a)| (t.to_string(), a.to_string())).collect(),
            then: "Done.".into(),
            answers: answers.iter().map(|a| a.to_string()).collect(),
        }
    }
}

/// A model on loopback. Dropping the handle stops it. `requests` collects the
/// body of each of the assistant's, in order, and `writing` each of the medium
/// model's.
struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    writing: Arc<Mutex<Vec<serde_json::Value>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let requests: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let writing: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();

    tokio::spawn({
        let requests = requests.clone();
        let writing = writing.clone();
        async move {
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = rx.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let script = script.clone();
                let requests = requests.clone();
                let writing = writing.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf: Vec<u8> = Vec::new();
                    let mut chunk = vec![0u8; 64 * 1024];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(body_at) = find_body(&buf)
                            && buf.len() - body_at >= content_length(&buf[..body_at])
                        {
                            break;
                        }
                    }
                    let request = find_body(&buf)
                        .and_then(|at| serde_json::from_slice::<serde_json::Value>(&buf[at..]).ok())
                        .unwrap_or_default();
                    let (content_type, body) = if offered(&request).contains(&"submit".to_string())
                    {
                        let submitted_already = request["messages"]
                            .as_array()
                            .and_then(|m| m.last())
                            .is_some_and(|m| m["role"] == "tool");
                        if submitted_already {
                            // The exchange's own last step, once the answer
                            // is in: the model is done, and nothing is asked.
                            ("application/json", said("Submitted."))
                        } else {
                            // The medium model, asked to write: the next answer.
                            let mut asked = writing.lock().unwrap();
                            let answer = script
                                .answers
                                .get(asked.len())
                                .or(script.answers.last())
                                .cloned()
                                .unwrap_or_default();
                            asked.push(request);
                            ("application/json", submitted(&answer))
                        }
                    } else {
                        // The assistant: the next scripted call, or the end.
                        let mut asked = requests.lock().unwrap();
                        let turn = asked.len();
                        asked.push(request);
                        let body = match script.calls.get(turn) {
                            Some((tool, arguments)) => reply("", Some((tool, arguments))),
                            None => reply(&script.then, None),
                        };
                        ("text/event-stream", body)
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), requests, writing, _shutdown: tx }
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

fn find_body(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// A plain, not streamed, Chat Completions reply: the medium model finishing
/// once it has submitted its answer.
fn said(text: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "scripted",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
    .to_string()
}

/// The medium model's answer: a `submit` call carrying `text`, in one
/// ordinary (not streamed) Chat Completions reply.
fn submitted(text: &str) -> String {
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
                    "function": {
                        "name": "submit",
                        "arguments": serde_json::json!({ "text": text }).to_string(),
                    },
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
    .to_string()
}

/// One answer of the assistant's, as a stream of Chat Completions chunks: a
/// call to a tool with its arguments, or `text`.
fn reply(text: &str, tool: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    let mut frame = |delta: serde_json::Value, finish: Option<&str>| {
        let chunk = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "scripted",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        out.push_str(&format!("data: {chunk}\n\n"));
    };
    match tool {
        Some((tool, arguments)) => {
            frame(
                serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": tool, "arguments": arguments },
                    }],
                }),
                None,
            );
            frame(serde_json::json!({}), Some("tool_calls"));
        }
        None => {
            frame(serde_json::json!({ "role": "assistant", "content": text }), None);
            frame(serde_json::json!({}), Some("stop"));
        }
    }
    out.push_str("data: [DONE]\n\n");
    out
}

fn offered(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The messages of one request to the model, as one string to search.
fn said_to_the_model(request: &serde_json::Value) -> String {
    request["messages"].to_string()
}

/// What the person's desktop does: has something selected -- the first of
/// `selections` until it has been read, then the next, and so on, the last
/// for good -- counts how often it was read, and remembers what it was asked
/// to type.
struct Desk {
    selections: Vec<String>,
    reads: AtomicUsize,
    typed: Mutex<Vec<String>>,
}

impl Typist for Desk {
    fn type_text(&self, text: String) -> BoxFuture<'static, Result<(), String>> {
        self.typed.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }

    fn selected_text(&self) -> BoxFuture<'static, Result<String, String>> {
        let read = self.reads.fetch_add(1, Ordering::AcqRel);
        let selection = self.selections[read.min(self.selections.len() - 1)].clone();
        Box::pin(async move { Ok(selection) })
    }
}

fn selecting(selection: &str) -> Arc<Desk> {
    selecting_then(&[selection])
}

/// A selection that is `first` when it is read to be rewritten and something
/// else by the time the rewrite is ready.
fn selecting_then(selections: &[&str]) -> Arc<Desk> {
    Arc::new(Desk {
        selections: selections.iter().map(|s| s.to_string()).collect(),
        reads: AtomicUsize::new(0),
        typed: Mutex::new(Vec::new()),
    })
}

fn typed(desk: &Desk) -> Vec<String> {
    desk.typed.lock().unwrap().clone()
}

fn reads(desk: &Desk) -> usize {
    desk.reads.load(Ordering::Acquire)
}

/// What each of the model's requests said to it, one string each. A function
/// rather than a held guard, so that no lock is kept across the next turn.
fn told(model: &FakeModel, request: usize) -> String {
    said_to_the_model(&model.requests.lock().unwrap()[request])
}

/// How many times the medium model was asked to write.
fn written(model: &FakeModel) -> usize {
    model.writing.lock().unwrap().len()
}

/// What became of every text written, as the record says.
fn outcomes(svc: &Arc<Service>) -> Vec<WritingOutcome> {
    let query = jatayu_core::WritingQuery::default();
    let kept = svc.get().unwrap().writing_assists(&query).unwrap();
    kept.into_iter().map(|assist| assist.outcome).collect()
}

fn quiet() -> Sink {
    Arc::new(|_| {})
}

fn assistant(svc: &Arc<Service>, desk: &Arc<Desk>, woken: bool) -> ServiceAssistant {
    ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: None,
        typist: Some(desk.clone() as Arc<dyn Typist>),
        woken,
        away: false,
    }
}

const WRITING_TOOLS: [&str; 2] = ["rewrite_selection", "write_message"];

#[tokio::test]
async fn a_spoken_turn_with_a_typist_is_offered_both_and_told_which_is_which() {
    let model = fake_model(Script::new(&[], &[])).await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("");

    assistant(&svc, &desk, false).turn("Improve this.".into(), true, None, quiet()).await.unwrap();

    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    for tool in WRITING_TOOLS.into_iter().chain(["type_text", DICTATE_TOOL]) {
        assert!(tools.contains(&tool.to_string()), "{tool}: {tools:?}");
    }
    let said = said_to_the_model(&requests[0]);
    assert!(said.contains("wherever they are on their desktop"), "told it can type");
    assert!(said.contains("is write_message"), "told which tool is for a message to somebody");
    assert!(said.contains("call rewrite_selection"), "and which is for what is selected");
    assert!(said.contains("call type_text with exactly that"), "and which is for typing");
    assert!(said.contains("an address, a date, a name, a number, code"), "literals are typed");
    assert!(model.writing.lock().unwrap().is_empty(), "nothing was written");
}

#[tokio::test]
async fn neither_is_offered_without_a_typist_or_to_a_typed_turn_or_a_call() {
    let model = fake_model(Script::new(&[], &[])).await;
    let (svc, _dir) = service(&model.endpoint);

    // Nothing to type with.
    let alone = ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: None,
        typist: None,
        woken: false,
        away: false,
    };
    alone.turn("Improve this.".into(), true, None, quiet()).await.unwrap();

    // Typed, not talked, with a typist about.
    let keyboard = selecting("");
    jatayu_service::agent::run_turn(Turn {
        service: svc.clone(),
        vault: svc.get().unwrap(),
        pending: svc.pending(),
        conversation: ConversationId::new(),
        prompt: "Improve this.".into(),
        pictures: Vec::new(),
        files: Vec::new(),
        on_screen: None,
        channel: quiet(),
        unattended: None,
        drafting: None,
        via: None,
        spoken: false,
        voice_approves: false,
        woken: false,
        reach: Reach { typist: Some(keyboard.clone() as Arc<dyn Typist>), ..Reach::default() },
        job: None,
        follow_up: None,
        about_job: None,
        max_steps: None,
        tier: None,
        read_only: false,
    })
    .await
    .unwrap();

    // A call through a chat, which has no desktop.
    let via = jatayu_core::chat::Via {
        link: jatayu_core::id::ChatLinkId::new(),
        service: "Matrix".into(),
        chat: "!room:example.org".into(),
    };
    ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: Some(via),
        typist: Some(selecting("") as Arc<dyn Typist>),
        woken: false,
        away: false,
    }
    .turn("Improve this.".into(), true, None, quiet())
    .await
    .unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests.iter() {
        let tools = offered(request);
        for tool in WRITING_TOOLS {
            assert!(!tools.contains(&tool.to_string()), "{tool}: {tools:?}");
        }
    }
}

#[tokio::test]
async fn improve_this_rewrites_what_is_selected_without_telling_the_model_what_it_said() {
    let model = fake_model(Script::new(
        &[("rewrite_selection", r#"{"how":"improve"}"#)],
        &["The cat sat on the mat."],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("teh cat sat on teh mat");

    let outcome = assistant(&svc, &desk, false)
        .turn("Improve this.".into(), true, None, quiet())
        .await
        .unwrap();

    assert!(!outcome.stopped);
    // Over the selection, which the field kept.
    assert_eq!(typed(&desk), ["The cat sat on the mat."]);
    // The medium model was given the selection and asked to improve it.
    let writing = model.writing.lock().unwrap();
    assert_eq!(writing.len(), 1);
    let asked = said_to_the_model(&writing[0]);
    assert!(asked.contains("teh cat sat on teh mat"), "{asked}");
    // The assistant was told it is done, and neither text -- the selection may
    // be somebody else's words.
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "the call, then what it said after");
    let told = said_to_the_model(&requests[1]);
    assert!(told.contains("replaced"), "{told}");
    assert!(!told.contains("teh cat"), "{told}");
    assert!(!told.contains("The cat sat on the mat."), "{told}");
    assert!(!said_to_the_model(&requests[0]).contains("teh cat"));
    drop(requests);
    assert_eq!(outcomes(&svc), [WritingOutcome::Replaced], "kept as pasted over the selection");
    assert_eq!(reads(&desk), 2, "read to rewrite it, and again to see it had not moved");
}

#[tokio::test]
async fn any_other_change_goes_to_the_model_in_their_words() {
    let model = fake_model(Script::new(
        &[("rewrite_selection", r#"{"how":"other","instruction":"make it friendlier"}"#)],
        &["Hi! Could you send me the report when you get a moment?"],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("Send me the report.");

    assistant(&svc, &desk, false)
        .turn("Make this friendlier.".into(), true, None, quiet())
        .await
        .unwrap();

    assert_eq!(typed(&desk), ["Hi! Could you send me the report when you get a moment?"]);
    let writing = model.writing.lock().unwrap();
    let asked = said_to_the_model(&writing[0]);
    assert!(asked.contains("make it friendlier"), "{asked}");
    assert!(asked.contains("Send me the report."), "{asked}");
}

#[tokio::test]
async fn nothing_selected_is_told_to_the_model_to_ask_for_and_nothing_is_written() {
    let model =
        fake_model(Script::new(&[("rewrite_selection", r#"{"how":"concise"}"#)], &["unused"]))
            .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("");

    assistant(&svc, &desk, false)
        .turn("Make this concise.".into(), true, None, quiet())
        .await
        .unwrap();

    assert!(typed(&desk).is_empty());
    assert!(model.writing.lock().unwrap().is_empty(), "no model was asked about nothing");
    let requests = model.requests.lock().unwrap();
    let told = said_to_the_model(&requests[1]);
    assert!(told.contains("Nothing is selected"), "{told}");
    assert!(told.contains("select the text"), "{told}");
    assert!(outcomes(&svc).is_empty(), "nothing was written, so nothing is kept");
}

#[tokio::test]
async fn another_change_without_what_to_change_types_nothing() {
    let model =
        fake_model(Script::new(&[("rewrite_selection", r#"{"how":"other"}"#)], &["unused"])).await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("Send me the report.");

    assistant(&svc, &desk, false).turn("Change this.".into(), true, None, quiet()).await.unwrap();

    assert!(typed(&desk).is_empty());
    assert!(model.writing.lock().unwrap().is_empty());
    let requests = model.requests.lock().unwrap();
    assert!(said_to_the_model(&requests[1]).contains("instruction"));
}

#[tokio::test]
async fn write_that_types_a_message_of_the_kind_asked_for_and_hands_it_back_for_a_follow_up() {
    let message =
        "Hi Sam,\n\nI'm not feeling well today, and I'll reach out tomorrow.\n\nBest,\nHari";
    let model = fake_model(Script::new(
        &[(
            "write_message",
            r#"{"what":"that I am not feeling well and that I will reach out tomorrow","kind":"email","to":"Sam"}"#,
        )],
        &[message],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("");

    assistant(&svc, &desk, false)
        .turn(
            "Write an email to Sam that I am not feeling well and that I will reach out tomorrow."
                .into(),
            true,
            None,
            quiet(),
        )
        .await
        .unwrap();

    assert_eq!(typed(&desk), [message]);
    let writing = model.writing.lock().unwrap();
    assert_eq!(writing.len(), 1);
    let asked = said_to_the_model(&writing[0]);
    assert!(asked.contains("an email to Sam"), "{asked}");
    assert!(asked.contains("not feeling well"), "{asked}");
    // The assistant has the message to go on for "make it shorter", and is
    // told to say it is typed, not what it says.
    let requests = model.requests.lock().unwrap();
    let told = said_to_the_model(&requests[1]);
    assert!(told.contains("I'm not feeling well today"), "{told}");
    assert!(told.contains("Say so in a few words"), "{told}");
    // And that it cannot be written again on top of itself.
    assert!(told.contains("select it and say what to change"), "{told}");
    assert!(told.contains("rewrite_selection"), "{told}");
    drop(requests);
    assert_eq!(outcomes(&svc), [WritingOutcome::Replaced]);
}

#[tokio::test]
async fn a_message_after_words_that_may_be_an_echo_is_handed_back_not_typed() {
    // `heard_clear` false: the person began speaking over the assistant's own
    // voice, which may have been it speaking.
    let model = fake_model(Script::new(
        &[("write_message", r#"{"what":"that I am running late","kind":"chat"}"#)],
        &["Running a bit late, see you soon!"],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("");

    assistant(&svc, &desk, false)
        .turn("Write that I am running late.".into(), false, None, quiet())
        .await
        .unwrap();

    assert!(typed(&desk).is_empty(), "not typed by the tool");
    let told = told(&model, 1);
    assert!(told.contains("Running a bit late, see you soon!"), "{told}");
    assert!(told.contains("Call type_text now with exactly this text"), "{told}");
    // Left as it was: it is shown to them first, as `type_text` types it.
    assert_eq!(outcomes(&svc), [WritingOutcome::Pending]);
}

#[tokio::test]
async fn a_rewrite_after_words_that_may_be_an_echo_reads_nothing_and_asks_to_say_it_again() {
    let model = fake_model(Script::new(
        &[("rewrite_selection", r#"{"how":"improve"}"#)],
        &["The cat sat on the mat."],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("teh cat sat on teh mat");

    assistant(&svc, &desk, false).turn("Improve this.".into(), false, None, quiet()).await.unwrap();

    // Never typed straight in, so the selection was not even copied, and the
    // medium model was not given it.
    assert_eq!(reads(&desk), 0);
    assert!(typed(&desk).is_empty());
    assert_eq!(written(&model), 0);
    let told = told(&model, 1);
    assert!(told.contains("Ask them to say it again"), "{told}");
    assert!(!told.contains("teh cat"), "{told}");
    assert!(outcomes(&svc).is_empty());
}

#[tokio::test]
async fn an_instruction_that_is_too_long_reads_nothing() {
    let long = "make it friendlier ".repeat(30);
    let arguments = serde_json::json!({ "how": "other", "instruction": long }).to_string();
    let model =
        fake_model(Script::new(&[("rewrite_selection", arguments.as_str())], &["unused"])).await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("Send me the report.");

    assistant(&svc, &desk, false).turn("Change this.".into(), true, None, quiet()).await.unwrap();

    assert_eq!(reads(&desk), 0, "checked before the selection is copied");
    assert!(typed(&desk).is_empty());
    assert_eq!(written(&model), 0);
    assert!(told(&model, 1).contains("keep it to one sentence"));
}

#[tokio::test]
async fn a_selection_that_changed_before_the_rewrite_was_ready_is_left_alone() {
    let model = fake_model(Script::new(
        &[("rewrite_selection", r#"{"how":"improve"}"#)],
        &["The cat sat on the mat."],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    // They clicked into another window while it was being written.
    let desk = selecting_then(&["teh cat sat on teh mat", "Dear Sam, I am writing to say"]);

    assistant(&svc, &desk, false).turn("Improve this.".into(), true, None, quiet()).await.unwrap();

    assert!(typed(&desk).is_empty(), "nothing was typed over what is selected now");
    assert_eq!(written(&model), 1, "it was written, and thrown away");
    let told = told(&model, 1);
    assert!(told.contains("The selection changed before the rewrite was ready"), "{told}");
    assert!(told.contains("nothing was replaced"), "{told}");
    assert!(!told.contains("The cat sat on the mat."), "{told}");
    assert_eq!(outcomes(&svc), [WritingOutcome::Cancelled]);
}

#[tokio::test]
async fn from_the_wake_word_one_line_goes_straight_in_and_more_is_shown_first() {
    // One line, woken: typed by the tool as for `type_text`.
    let model = fake_model(Script::new(
        &[("rewrite_selection", r#"{"how":"grammar"}"#)],
        &["They are going home."],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk_one = selecting("they is going home");
    assistant(&svc, &desk_one, true)
        .turn("Correct the grammar.".into(), true, None, quiet())
        .await
        .unwrap();
    assert_eq!(typed(&desk_one), ["They are going home."]);

    // A line picked out by triple-click keeps the break it ended with, which
    // `type_text` would not count either: still one line.
    let model = fake_model(Script::new(
        &[("rewrite_selection", r#"{"how":"improve"}"#)],
        &["The cat sat on the mat."],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk_line = selecting("teh cat sat on teh mat\n");
    assistant(&svc, &desk_line, true)
        .turn("Improve this.".into(), true, None, quiet())
        .await
        .unwrap();
    assert_eq!(
        typed(&desk_line),
        ["The cat sat on the mat.\n"],
        "over the selection, break and all"
    );

    // Several lines, woken: a line break typed into a terminal or a chat is
    // Enter, and what said the name may have been a video. Handed back, the
    // assistant passes it to `type_text`, whose card shows exactly it, and
    // once they agree it is typed -- over the selection -- and not before.
    let rewritten = "Line one.\nLine two.";
    let model = fake_model(Script::new(
        &[
            ("rewrite_selection", r#"{"how":"improve"}"#),
            ("type_text", r#"{"text":"Line one.\nLine two."}"#),
        ],
        &[rewritten],
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let desk = selecting("line one\nline two");
    let cards: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
    let sink: Sink = {
        let pending = svc.pending();
        let cards = cards.clone();
        let desk = desk.clone();
        Arc::new(move |event| {
            if let AgentEvent::ConfirmationRequired { call_id, name, subject, kind, .. } = event {
                // Nothing is typed until the card is answered.
                assert!(typed(&desk).is_empty(), "typed before they agreed");
                cards.lock().unwrap().push((name, kind, subject));
                assert!(pending.answer(&call_id, ConfirmAnswer::Confirm));
            }
        })
    };
    assistant(&svc, &desk, true).turn("Improve this.".into(), true, None, sink).await.unwrap();

    let cards = cards.lock().unwrap().clone();
    assert_eq!(
        cards,
        [("type_text".to_string(), "desktop".to_string(), rewritten.to_string())],
        "one card, with the whole text"
    );
    assert_eq!(typed(&desk), [rewritten], "typed once they agreed");
    let told = told(&model, 1);
    assert!(told.contains("Call type_text now with exactly this text"), "{told}");
    assert!(told.contains("Line one."), "{told}");
    // The rewrite itself was not what was typed, so it is not settled as such.
    assert_eq!(outcomes(&svc), [WritingOutcome::Pending]);
}
