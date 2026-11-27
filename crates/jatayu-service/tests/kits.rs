//! Toolkits end to end against a fake model: a routed turn is offered a core
//! and the kits its words, screen and history point at; `load_tools` makes a
//! kit's tools callable on the very next step; and a turn nobody routed, a dream
//! and a brief keep the catalogue they always had.
//!
//! The harness is `tests/voice_end.rs`'s: a `TcpListener` on loopback speaking
//! the Chat Completions shape, one scripted answer per request, every request
//! kept.

use jatayu_core::ConversationId;
use jatayu_core::agent::onscreen::{App, OnScreen};
use jatayu_core::agent::{Message, Role, ToolCall};
use jatayu_service::Service;
use jatayu_service::agent::{Reach, Sink, Turn, run_turn, run_turn_routed};
use std::sync::{Arc, Mutex};

/// A vault with an assistant pointed at `endpoint`.
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

/// As [`service`], with a decision model at `decision_url`.
fn service_with_decision(endpoint: &str, decision_url: &str) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = service(endpoint);
    let vault = svc.get().unwrap();
    let mut models = vault.model_settings().unwrap();
    models.decision = Some(jatayu_core::llm::DecisionConfig {
        api: jatayu_core::llm::DecisionApi::TypeSafe,
        url: Some(decision_url.to_string()),
        model: jatayu_core::llm::DecisionApi::TypeSafe.default_model().into(),
    });
    vault.save_model_settings(&models).unwrap();
    vault.set_model_key(jatayu_core::llm::ModelSlot::Decision, "ds-key").unwrap();
    (svc, dir)
}

/// A decision server on loopback that answers every question with `choice` at
/// `probability`, after `delay`, and keeps every body it was sent.
struct FakeDecisions {
    url: String,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_decisions(
    choice: &'static str,
    probability: f64,
    delay: std::time::Duration,
) -> FakeDecisions {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    tokio::spawn({
        let bodies = bodies.clone();
        async move {
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = rx.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let bodies = bodies.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf: Vec<u8> = Vec::new();
                    let mut chunk = vec![0u8; 16 * 1024];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(at) = find_body(&buf)
                            && buf.len() - at >= content_length(&buf[..at])
                        {
                            break;
                        }
                    }
                    let at = find_body(&buf).unwrap();
                    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[at..]) {
                        bodies.lock().unwrap().push(json);
                    }
                    tokio::time::sleep(delay).await;
                    let answer = serde_json::json!({
                        "model": "jev-latest",
                        "answers": { "category": {
                            "type": "choice",
                            "choice": choice,
                            "confidence": probability,
                            "probabilities": { choice: probability },
                        } },
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        }
    });
    FakeDecisions { url: format!("http://127.0.0.1:{port}/v1/systemone"), bodies, _shutdown: tx }
}

/// Tool calls answered in turn, each with its arguments as JSON text, then a
/// final message with no more calls.
#[derive(Clone)]
struct Script {
    calls: Vec<(String, String)>,
    then: String,
}

fn calls(calls: &[(&str, &str)], then: &str) -> Script {
    Script {
        calls: calls.iter().map(|(t, a)| (t.to_string(), a.to_string())).collect(),
        then: then.to_string(),
    }
}

/// A model on loopback. Dropping the handle stops it. `requests` collects each
/// call's JSON body, in order.
struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let requests: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();

    tokio::spawn({
        let requests = requests.clone();
        async move {
            let mut asked = 0usize;
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = rx.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let script = script.clone();
                let requests = requests.clone();
                asked += 1;
                let turn = asked;
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
                    if let Some(body_at) = find_body(&buf)
                        && let Ok(json) =
                            serde_json::from_slice::<serde_json::Value>(&buf[body_at..])
                    {
                        requests.lock().unwrap().push(json);
                    }
                    let body = match script.calls.get(turn - 1) {
                        Some((tool, arguments)) => reply("", Some((tool, arguments))),
                        None => reply(&script.then, None),
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), requests, _shutdown: tx }
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

/// One answer, as a stream of Chat Completions chunks: a call, or `text`.
fn reply(text: &str, call: Option<(&str, &str)>) -> String {
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
    match call {
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

/// The names of the tools a request offered, in the order it listed them.
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

fn quiet() -> Sink {
    Arc::new(|_| {})
}

fn turn(svc: &Arc<Service>, conversation: ConversationId, prompt: &str) -> Turn {
    Turn {
        service: svc.clone(),
        vault: svc.get().unwrap(),
        pending: svc.pending(),
        conversation,
        prompt: prompt.into(),
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
        reach: Reach::default(),
        job: None,
        follow_up: None,
        about_job: None,
        max_steps: None,
        tier: None,
        read_only: false,
    }
}

/// Everything a request carried that a model reads as instructions, flattened:
/// its system prompt among the rest.
fn text_of(request: &serde_json::Value) -> String {
    request["messages"].to_string()
}

#[tokio::test]
async fn a_routed_turn_has_the_core_and_load_tools_makes_a_kit_callable_on_the_next_step() {
    let model = fake_model(calls(
        &[("load_tools", r#"{"kits":["health"]}"#), ("health_today", "{}")],
        "You are doing fine.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let conversation = ConversationId::new();

    let turned = run_turn_routed(turn(&svc, conversation, "How am I doing?")).await.unwrap();

    assert_eq!(turned.steps, 2, "the load and the call that needed it");
    assert!(turned.text.contains("fine"), "{}", turned.text);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 3, "a step to ask, a step to use it, a step to say");

    // The first step: the everyday tools, and the way to more.
    let first = offered(&requests[0]);
    for core in
        ["list_tasks", "create_task", "create_event", "create_note", "remember", "load_tools"]
    {
        assert!(first.contains(&core.to_string()), "{core} is core: {first:?}");
    }
    for kit in ["health_today", "sleep_report", "set_target", "list_items", "start_watch"] {
        assert!(!first.contains(&kit.to_string()), "{kit} is in a kit: {first:?}");
    }
    assert!(first.len() < 60, "{} tools on a first step", first.len());
    let words = text_of(&requests[0]);
    assert!(words.contains("## More tools") && words.contains("load_tools"), "{words}");
    assert!(words.contains("- health: activity, sleep, glucose"), "{words}");
    assert!(words.contains("- money: "), "kits that exist and are not loaded are listed");

    // The step after the load: the kit is there, and is no longer offered.
    let second = offered(&requests[1]);
    assert!(second.contains(&"health_today".to_string()), "{second:?}");
    assert!(second.contains(&"sleep_report".to_string()));
    assert!(second.contains(&"load_tools".to_string()), "still there to ask for more");
    assert!(first.iter().all(|t| second.contains(t)), "nothing the first step had is lost");
    let words = text_of(&requests[1]);
    assert!(!words.contains("- health: activity"), "a loaded kit is not offered again");
    assert!(words.contains("- money: "));
    // The model was told, in the call's own answer, what it had loaded.
    assert!(text_of(&requests[1]).contains("loaded"), "{}", text_of(&requests[1]));

    // The call that needed the kit ran, and ran for real.
    let vault = svc.get().unwrap();
    let thread = vault.messages(conversation).unwrap();
    let results: Vec<&Message> = thread.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(results.len(), 2, "{thread:?}");
    assert!(results.iter().all(|m| !m.failed), "neither call failed: {results:?}");
    // And the same agent's next step is not asked from scratch: the history
    // the third request carried has both calls in it.
    let last = text_of(&requests[2]);
    assert!(last.contains("load_tools") && last.contains("health_today"), "{last}");
}

#[tokio::test]
async fn a_turn_nobody_routed_is_offered_everything_as_before() {
    let model = fake_model(calls(&[], "Hello.")).await;
    let (svc, _dir) = service(&model.endpoint);

    run_turn(turn(&svc, ConversationId::new(), "Hello")).await.unwrap();

    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    for tool in ["list_tasks", "health_today", "list_items", "start_watch", "set_target"] {
        assert!(tools.contains(&tool.to_string()), "{tool}: {tools:?}");
    }
    assert!(!tools.contains(&"load_tools".to_string()), "nothing to load");
    assert!(!text_of(&requests[0]).contains("More tools"));
}

#[tokio::test]
async fn the_words_the_screen_and_the_history_each_bring_their_kit() {
    // Words: "sleep" names health.
    let model = fake_model(calls(&[], "Well.")).await;
    let (svc, _dir) = service(&model.endpoint);
    run_turn_routed(turn(&svc, ConversationId::new(), "How did I sleep last night?"))
        .await
        .unwrap();
    {
        let requests = model.requests.lock().unwrap();
        let tools = offered(&requests[0]);
        assert!(tools.contains(&"sleep_report".to_string()), "{tools:?}");
        assert!(!tools.contains(&"list_items".to_string()), "library was not asked for");
        assert!(!text_of(&requests[0]).contains("- health: "), "already loaded, so not listed");
    }

    // Screen: the Health app is open, and nothing in the words says so.
    let model = fake_model(calls(&[], "Fine.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let mut asked = turn(&svc, ConversationId::new(), "What about today?");
    asked.on_screen = Some(OnScreen {
        app: App::Health,
        view: None,
        within: Vec::new(),
        open: Vec::new(),
        query: None,
        reader: None,
        playing: None,
    });
    run_turn_routed(asked).await.unwrap();
    {
        let requests = model.requests.lock().unwrap();
        assert!(offered(&requests[0]).contains(&"health_today".to_string()));
    }

    // History: an earlier turn in this conversation called a health tool, and
    // this one says nothing about health at all.
    let model = fake_model(calls(&[], "You're welcome.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let conversation = ConversationId::new();
    vault
        .save_conversation(&jatayu_core::agent::Conversation {
            id: conversation,
            ..jatayu_core::agent::Conversation::new()
        })
        .unwrap();
    vault.save_message(&Message::user(conversation, "how is my glucose?".to_string())).unwrap();
    vault
        .save_message(&Message {
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                name: "glucose_report".into(),
                arguments: serde_json::json!({}),
            }],
            ..Message::assistant(conversation, "Steady.".to_string())
        })
        .unwrap();
    run_turn_routed(turn(&svc, conversation, "thanks")).await.unwrap();
    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(tools.contains(&"glucose_report".to_string()), "{tools:?}");
    assert!(!tools.contains(&"list_items".to_string()));
}

#[tokio::test]
async fn a_brief_keeps_every_read_it_always_had_and_is_offered_no_way_to_load_more() {
    let model = fake_model(calls(&[], "Here is the day.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let mut asked = turn(&svc, ConversationId::new(), "Write the brief.");
    asked.read_only = true;

    run_turn_routed(asked).await.unwrap();

    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    for read in ["list_tasks", "health_today", "list_items", "list_trackers"] {
        assert!(tools.contains(&read.to_string()), "{read}: {tools:?}");
    }
    assert!(!tools.contains(&"load_tools".to_string()));
    assert!(!tools.contains(&"create_task".to_string()), "a brief only reads");
    assert!(!text_of(&requests[0]).contains("More tools"));
}

#[tokio::test]
async fn asking_for_a_kit_that_is_not_there_says_so_and_the_turn_goes_on() {
    let model = fake_model(calls(&[("load_tools", r#"{"kits":["nonsense"]}"#)], "Right.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let conversation = ConversationId::new();

    let turned = run_turn_routed(turn(&svc, conversation, "Hello")).await.unwrap();

    assert!(turned.text.contains("Right."));
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        offered(&requests[0]),
        offered(&requests[1]),
        "nothing was loaded, so nothing moved"
    );
    let thread = svc.get().unwrap().messages(conversation).unwrap();
    assert!(thread.iter().any(|m| m.role == Role::Tool && m.failed), "{thread:?}");
}

#[tokio::test]
async fn a_decision_model_that_is_sure_adds_the_kit_it_names_and_is_asked_only_about_the_rest() {
    let decisions = fake_decisions("money", 0.9, std::time::Duration::ZERO).await;
    let model = fake_model(calls(&[], "Fine.")).await;
    let (svc, _dir) = service_with_decision(&model.endpoint, &decisions.url);

    // Nothing in the words or the screen says money.
    run_turn_routed(turn(
        &svc,
        ConversationId::new(),
        "Where did it all go this month, do you think?",
    ))
    .await
    .unwrap();

    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(tools.contains(&"spending_summary".to_string()), "{tools:?}");
    assert!(!tools.contains(&"health_today".to_string()));
    // It was asked once, with the request, and was not asked about a kit the
    // words had already chosen (none had), and was offered none.
    let bodies = decisions.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    let body = &bodies[0];
    assert!(body["state"]["request"].as_str().unwrap().contains("Where did it all go"));
    let criteria = body["questions"]["category"]["criteria"].as_object().unwrap();
    assert!(criteria.contains_key("none") && criteria.contains_key("money"), "{criteria:?}");
    assert!(criteria.contains_key("health"));
}

#[tokio::test]
async fn a_decision_model_that_is_not_sure_adds_nothing() {
    let decisions = fake_decisions("money", 0.4, std::time::Duration::ZERO).await;
    let model = fake_model(calls(&[], "Fine.")).await;
    let (svc, _dir) = service_with_decision(&model.endpoint, &decisions.url);

    run_turn_routed(turn(
        &svc,
        ConversationId::new(),
        "Where did it all go this month, do you think?",
    ))
    .await
    .unwrap();

    let requests = model.requests.lock().unwrap();
    assert!(!offered(&requests[0]).contains(&"spending_summary".to_string()));
    assert_eq!(decisions.bodies.lock().unwrap().len(), 1, "it was asked, and not believed");
}

#[tokio::test]
async fn a_decision_model_that_is_slow_is_not_waited_for_beyond_the_time_box() {
    let decisions = fake_decisions("money", 0.99, std::time::Duration::from_secs(6)).await;
    let model = fake_model(calls(&[], "Fine.")).await;
    let (svc, _dir) = service_with_decision(&model.endpoint, &decisions.url);

    let started = std::time::Instant::now();
    run_turn_routed(turn(
        &svc,
        ConversationId::new(),
        "Where did it all go this month, do you think?",
    ))
    .await
    .unwrap();

    // The time box is a second; the model is six.
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
    let requests = model.requests.lock().unwrap();
    assert!(!offered(&requests[0]).contains(&"spending_summary".to_string()));
    // The turn still has the way to ask for it.
    assert!(offered(&requests[0]).contains(&"load_tools".to_string()));
}

#[tokio::test]
async fn a_spoken_turn_is_not_made_to_wait_for_a_decision_model() {
    let decisions = fake_decisions("money", 0.99, std::time::Duration::ZERO).await;
    let model = fake_model(calls(&[], "Fine.")).await;
    let (svc, _dir) = service_with_decision(&model.endpoint, &decisions.url);
    let mut asked =
        turn(&svc, ConversationId::new(), "Where did it all go this month, do you think?");
    asked.spoken = true;

    run_turn_routed(asked).await.unwrap();

    assert!(decisions.bodies.lock().unwrap().is_empty(), "never asked: a voice turn cannot wait");
    let requests = model.requests.lock().unwrap();
    assert!(!offered(&requests[0]).contains(&"spending_summary".to_string()));
    assert!(offered(&requests[0]).contains(&"load_tools".to_string()), "it can still ask");
}

/// The prompt names the tools the turn was handed and no others: a watch's
/// look reads the web and writes nothing, so it is neither given a plan to
/// keep nor told of one.
#[tokio::test]
async fn a_watchs_look_is_told_of_no_plan_it_has_no_tool_for() {
    let model = fake_model(calls(&[], "Nothing new.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.web = true;
    vault.save_agent_settings(&settings).unwrap();
    let mut look = turn(&svc, ConversationId::new(), "Look at the page.");
    look.job = Some(jatayu_service::agent::JobTurn {
        id: jatayu_core::JobId::new(),
        question: "Has it changed?".into(),
        context: "This is a look at something being watched.",
    });
    look.read_only = true;

    run_turn(look).await.unwrap();

    {
        let requests = model.requests.lock().unwrap();
        let tools = offered(&requests[0]);
        assert!(tools.contains(&"web_search".to_string()), "a look reads the web: {tools:?}");
        assert!(!tools.contains(&"update_plan".to_string()), "{tools:?}");
        assert!(!tools.contains(&"create_task".to_string()), "a look writes nothing");
        let words = text_of(&requests[0]);
        assert!(!words.contains("update_plan"), "told of a plan with no way to keep one");
        assert!(words.contains("web_search"), "and told of the web it has");
    }

    // The same turn as a person's has both.
    let model = fake_model(calls(&[], "Hello.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.web = true;
    vault.save_agent_settings(&settings).unwrap();
    run_turn(turn(&svc, ConversationId::new(), "Plan my trip.")).await.unwrap();
    let requests = model.requests.lock().unwrap();
    assert!(offered(&requests[0]).contains(&"update_plan".to_string()));
    assert!(text_of(&requests[0]).contains("update_plan"));
}

/// The preamble names `list_calendars` wherever there is more than one place
/// for an event to go, so a routed turn has it without asking for a kit.
#[tokio::test]
async fn the_calendars_are_listed_on_a_routed_turn_that_has_loaded_nothing() {
    let model = fake_model(calls(&[], "Hello.")).await;
    let (svc, _dir) = service(&model.endpoint);

    run_turn_routed(turn(&svc, ConversationId::new(), "Hello")).await.unwrap();

    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(tools.contains(&"list_calendars".to_string()), "{tools:?}");
    assert!(!tools.contains(&"create_time_block".to_string()), "the time kit is not loaded");
    assert!(!tools.contains(&"delete_event".to_string()));
}
