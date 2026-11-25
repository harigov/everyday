//! The assistant acting on this computer, end to end: a scripted model asks
//! for a desk tool, and the turn reaches the [`Desk`] the app handed over --
//! for a turn at this computer, and for nobody else. See `docs/plans/desk.md`.

use futures::future::BoxFuture;
use karmayoga_core::ConversationId;
use karmayoga_service::Service;
use karmayoga_service::agent::Sink;
use karmayoga_service::ctx::{Caller, Ctx};
use karmayoga_service::desk::{
    Abilities, Desk, DeskSetting, InstalledApp, MediaAction, NowPlaying, Timer, Volume,
    VolumeChange,
};
use karmayoga_service::voice::VoiceAssistant;
use karmayoga_service::voice::assistant::ServiceAssistant;
use std::sync::{Arc, Mutex};

/// A desk that writes down what it was asked.
#[derive(Default)]
struct Notebook(Mutex<Vec<String>>);

impl Notebook {
    fn asked(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn note<T: Send + 'static>(
        &self,
        what: String,
        value: T,
    ) -> BoxFuture<'static, Result<T, String>> {
        self.0.lock().unwrap().push(what);
        Box::pin(async move { Ok(value) })
    }
}

impl Desk for Notebook {
    fn abilities(&self) -> Abilities {
        Abilities { media: true, ..Abilities::default() }
    }
    fn open_url(&self, url: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("open_url {url}"), ())
    }
    fn open_file(&self, id: karmayoga_core::FileId) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("open_file {id}"), ())
    }
    fn show_section(&self, section: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("show_section {section}"), ())
    }
    fn apps(&self) -> BoxFuture<'static, Result<Vec<InstalledApp>, String>> {
        self.note("apps".into(), Vec::new())
    }
    fn launch(&self, id: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("launch {id}"), ())
    }
    fn media(&self, action: MediaAction) -> BoxFuture<'static, Result<Option<NowPlaying>, String>> {
        self.note(format!("media {action:?}"), None)
    }
    fn volume(&self, change: VolumeChange) -> BoxFuture<'static, Result<Volume, String>> {
        self.note(format!("volume {change:?}"), Volume { percent: 50, muted: false })
    }
    fn setting(
        &self,
        setting: DeskSetting,
        on: Option<bool>,
    ) -> BoxFuture<'static, Result<bool, String>> {
        self.note(format!("setting {setting:?} {on:?}"), false)
    }
    fn lock_screen(&self) -> BoxFuture<'static, Result<(), String>> {
        self.note("lock_screen".into(), ())
    }
    fn copy(&self, text: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("copy {text}"), ())
    }
    fn ring(&self, timer: Timer) {
        self.0.lock().unwrap().push(format!("ring {}", timer.id));
    }
}

/// A vault with an assistant pointed at `endpoint`, acting on the desktop or
/// not as `desktop` says.
fn service(endpoint: &str, desktop: bool) -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = karmayoga_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = karmayoga_vault::create(dir.path(), config).unwrap();
    let settings = karmayoga_core::AgentSettings {
        enabled: true,
        timezone: Some("UTC".into()),
        desktop,
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = karmayoga_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

/// Tool calls (name, JSON arguments) answered in turn, then a final message.
#[derive(Clone)]
struct Script {
    calls: Vec<(String, String)>,
    then: String,
}

impl Script {
    fn calling(calls: &[(&str, serde_json::Value)]) -> Self {
        Self {
            calls: calls.iter().map(|(t, a)| (t.to_string(), a.to_string())).collect(),
            then: "Done.".into(),
        }
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
                        Some(tool) => reply("", Some(tool)),
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

/// One answer, as a stream of Chat Completions chunks: a call to `tool` with
/// `arguments`, or `text`.
fn reply(text: &str, tool: Option<&(String, String)>) -> String {
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

fn quiet() -> Sink {
    Arc::new(|_| {})
}

/// Typed in the window as `ctx`: what `send_message` does.
async fn typed(svc: &Arc<Service>, ctx: Ctx, prompt: &str) {
    let args = serde_json::json!({ "conversationId": ConversationId::new(), "prompt": prompt });
    svc.send_message(ctx, args, quiet()).await.unwrap();
}

const DESK_TOOLS: [&str; 3] = ["search_site", "set_timer", "control_media"];

#[tokio::test]
async fn search_amazon_typed_in_the_window_opens_its_results_in_the_browser() {
    let model = fake_model(Script::calling(&[(
        "search_site",
        serde_json::json!({ "site": "Amazon", "query": "noise cancelling headphones" }),
    )]))
    .await;
    let (svc, _dir) = service(&model.endpoint, true);
    let desk = Arc::new(Notebook::default());
    svc.set_desk(desk.clone());

    typed(&svc, Ctx::local(), "Search Amazon for noise cancelling headphones").await;

    assert_eq!(
        desk.asked(),
        ["open_url https://www.amazon.com/s?k=noise%20cancelling%20headphones"]
    );
    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    for tool in DESK_TOOLS {
        assert!(tools.contains(&tool.to_string()), "{tool} not offered: {tools:?}");
    }
    assert!(!tools.contains(&"set_volume".to_string()), "this desk has no volume");
}

#[tokio::test]
async fn a_timer_said_to_voice_mode_is_set_on_this_computer() {
    let model = fake_model(Script::calling(&[(
        "set_timer",
        serde_json::json!({ "minutes": 10, "label": "pasta" }),
    )]))
    .await;
    let (svc, _dir) = service(&model.endpoint, true);
    let desk = Arc::new(Notebook::default());
    svc.set_desk(desk.clone());
    let assistant = ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: None,
        typist: None,
        woken: false,
    };

    assistant
        .turn("Set a timer for ten minutes for the pasta".into(), true, None, quiet())
        .await
        .unwrap();

    let requests = model.requests.lock().unwrap();
    let answered = requests[1].to_string();
    assert!(answered.contains("pasta") && answered.contains("10 minutes"), "{answered}");
}

#[tokio::test]
async fn somebody_at_another_computer_is_not_offered_this_ones_desk() {
    let model = fake_model(Script::calling(&[])).await;
    let (svc, _dir) = service(&model.endpoint, true);
    svc.set_desk(Arc::new(Notebook::default()));

    let device = Ctx { caller: Caller::Device("phone".into()), ..Ctx::local() };
    typed(&svc, device, "Pause the music").await;
    typed(&svc, Ctx::window("app-mail"), "Pause the music").await;

    let requests = model.requests.lock().unwrap();
    let from_device = offered(&requests[0]);
    assert!(!from_device.iter().any(|t| DESK_TOOLS.contains(&t.as_str())), "{from_device:?}");
    let from_window = offered(&requests[1]);
    assert!(from_window.contains(&"control_media".to_string()), "an app window is this computer");
}

#[tokio::test]
async fn with_the_switch_off_or_no_desk_nothing_is_offered() {
    let model = fake_model(Script::calling(&[])).await;
    let (off, _dir) = service(&model.endpoint, false);
    off.set_desk(Arc::new(Notebook::default()));
    typed(&off, Ctx::local(), "Pause the music").await;
    let (none, _dir2) = service(&model.endpoint, true);
    typed(&none, Ctx::local(), "Pause the music").await;

    let requests = model.requests.lock().unwrap();
    for request in requests.iter() {
        let tools = offered(request);
        assert!(!tools.iter().any(|t| DESK_TOOLS.contains(&t.as_str())), "{tools:?}");
    }
}
