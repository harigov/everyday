//! One place that turns a vault's stored connection into a client rig can
//! drive.
//!
//! [`crate::agent`] and [`crate::quick`] each open a connection to the model
//! a person configured, and until this existed each did it by hand: both
//! called `openai::CompletionsClient::builder()` directly without ever
//! reading [`Provider`], both wrote out the same `if let` dance to apply a
//! model's temperature and token ceiling, and both wrote out the same
//! empty-bearer trick for a local endpoint that needs no credential. Naming
//! the two steps here is what makes the day a second [`Provider`] arrives a
//! matter of one arm in [`client`], rather than two builders in two files
//! that have to be found and changed together.

use bytes::Bytes;
use everyday_core::llm::{
    LLMModelConfig, LLMProviderConfig, ModelAccess, Provider, ReasoningEffort,
};
use rig_agent::AgentBuilder;
use rig_agent::core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_agent::core::providers::openai;
use rig_agent::core::wasm_compat::WasmCompatSend;
use serde_json::Value;

/// The connection every model request in this crate goes out on.
pub type Client = openai::CompletionsClient<CacheMarks>;

/// Open a connection for `conn`, authenticated with `key` if the provider
/// wants one.
///
/// Returns the builder's own error as text rather than a [`CommandError`],
/// because the two callers give a person two different sentences about the
/// same failure -- "could not start the assistant" against "could not reach
/// the model" -- and which one is right is a fact about the caller, not
/// about the connection.
///
/// [`CommandError`]: crate::error::CommandError
pub fn client(conn: &LLMProviderConfig, key: Option<String>) -> Result<Client, String> {
    open(conn, key, CacheMarks::default())
}

/// Open a connection for a conversation with `access`'s model -- the
/// assistant's turns, a routine's, a research job's -- whose requests repeat
/// one another from the start, and so are worth marking for a provider that
/// caches only what it is told to. See [`CacheMarks`].
///
/// Not for a one-shot request, which is never repeated: a mark on it buys a
/// cache write at a premium that nothing ever reads.
pub fn conversation_client(access: &ModelAccess) -> Result<Client, String> {
    let conn = &access.config.connection;
    let marks = CacheMarks::for_conversation(conn.endpoint(), &access.config.model.model);
    open(conn, access.key.clone(), marks)
}

fn open(
    conn: &LLMProviderConfig,
    key: Option<String>,
    marks: CacheMarks,
) -> Result<Client, String> {
    match conn.provider {
        // Chat Completions, not the Responses API. Rig's default OpenAI
        // client speaks the newer Responses API, but the whole point of the
        // base-URL override below is that Ollama, LM Studio, vLLM and
        // OpenRouter can be pointed at -- and what they all implement is
        // `/chat/completions`. Choosing the Responses API here would make the
        // setting that exists for local models work everywhere except local
        // models.
        Provider::OpenAi => openai::CompletionsClient::builder()
            .http_client(marks)
            .base_url(conn.endpoint())
            // A local model needs no credential and is usually configured
            // without one, and the endpoint ignores whatever is sent -- so a
            // keyless configuration sends an empty bearer rather than
            // omitting the step, which would leave the builder's auth type
            // unresolved. See `Provider::needs_key` for when a key is
            // insisted on at all.
            .api_key::<rig_agent::core::client::BearerAuth>(key.unwrap_or_default())
            .build()
            .map_err(|e| e.to_string()),
    }
}

/// Open a connection to the model the vault handed out -- what every caller
/// holding a [`ModelAccess`] wants, which is all of them.
pub fn client_for(access: &ModelAccess) -> Result<Client, String> {
    client(&access.config.connection, access.key.clone())
}

/// rig's HTTP client, writing prompt-cache breakpoints into a conversation's
/// chat requests when its provider wants them.
///
/// Most providers cache on their own, from the start of a request up to the
/// first thing that differs from one they have seen: OpenAI, DeepSeek, Z.ai's
/// GLM, xAI, Moonshot, and Gemini 2.5 and later. All they need is a request
/// that starts the same way each time, which is `system_prompt`'s and
/// `turn_note`'s business, not this one's. Anthropic's Claude, and Qwen on
/// Alibaba, cache nothing unless the request marks where a cacheable part
/// ends -- a `cache_control` on a content block -- and reading a request
/// that does not costs the full price every turn.
///
/// So the marks go where somebody reads them, which is decided once, from
/// the endpoint and the model, when the connection is opened:
///
/// - a Claude model, wherever it is served -- only a gateway that speaks
///   Anthropic's dialect serves one under that name;
/// - anything sent to OpenRouter, which passes them to the providers that
///   want them, translates them for OpenAI's newest models and lets the
///   rest cache as they would have -- except Gemini, which reads only the
///   last mark, the turn's own message, and so would be made to write a
///   cache every turn that the next turn cannot use;
/// - anything sent to Alibaba's own endpoints, which is Qwen.
///
/// Not to OpenAI, DeepSeek or Z.ai directly, whose own APIs have no such
/// field, cache without one, and may refuse a field they do not know -- and
/// not to a model on this machine, which has no cache to fill. Where nothing
/// is marked, a request goes as it came, unread.
///
/// See [`mark_cache_breakpoints`] for where they go.
#[derive(Clone, Debug, Default)]
pub struct CacheMarks {
    inner: reqwest::Client,
    /// Whether this connection's chat requests are marked.
    on: bool,
}

impl CacheMarks {
    /// The marks for a conversation with `model` at `endpoint`.
    pub fn for_conversation(endpoint: &str, model: &str) -> Self {
        let host = reqwest::Url::parse(endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        let on_host = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        let model = model.to_lowercase();
        let on = model.contains("claude")
            || (on_host("openrouter.ai") && !model.contains("gemini"))
            || on_host("aliyuncs.com");
        Self { inner: reqwest::Client::new(), on }
    }

    /// `req`, with its breakpoints written in if this connection marks and
    /// it is a chat request. Anything else -- or a body that is not JSON --
    /// goes as it came.
    fn marked<T: Into<Bytes>>(&self, req: Request<T>) -> Request<Bytes> {
        let (mut parts, body) = req.into_parts();
        let body: Bytes = body.into();
        if !self.on || !parts.uri.path().ends_with("/chat/completions") {
            return Request::from_parts(parts, body);
        }
        let Ok(mut json) = serde_json::from_slice::<Value>(&body) else {
            return Request::from_parts(parts, body);
        };
        if !mark_cache_breakpoints(&mut json) {
            return Request::from_parts(parts, body);
        }
        // The length changed; reqwest works it out again from the body.
        parts.headers.remove(http::header::CONTENT_LENGTH);
        let body = serde_json::to_vec(&json).map(Bytes::from).unwrap_or(body);
        Request::from_parts(parts, body)
    }
}

impl HttpClientExt for CacheMarks {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes>,
        T: WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        self.inner.send::<Bytes, U>(self.marked(req))
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        self.inner.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        self.inner.send_streaming(self.marked(req))
    }
}

/// Mark where the parts of a chat request that the next one will repeat
/// end, for a provider that caches only what it is told to. Whether it
/// marked anything.
///
/// Three marks, of the four Anthropic allows:
///
/// - **The system prompt.** Tool schemas are read ahead of it, so this one
///   covers the twenty thousand tokens or so that are the same in every
///   conversation, and survives the history window moving on.
/// - **The thread so far** -- the message before this turn's own, usually
///   the last reply. The next turn's thread runs on past it, and a provider
///   looks back from a mark for a place it has cached before.
/// - **This turn's message.** Every step of a turn that calls tools repeats
///   everything up to it, so the second step onwards reads it from the
///   cache. The next turn does not: it replays this message without its
///   `turn_note`. So a turn that calls nothing pays the write premium on
///   its own message -- only that, the rest having been read from the mark
///   before it -- for the steps a turn that does call something saves.
///
/// Nothing after this turn's message is marked. rig sends a tool result as
/// a string, which not every provider takes as blocks, and a call made
/// without words would need an empty text block, which Anthropic refuses.
pub fn mark_cache_breakpoints(body: &mut Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let role = |m: &Value| m["role"].as_str().unwrap_or_default().to_string();
    let system = messages.iter().position(|m| role(m) == "system");
    let this_turn = messages.iter().rposition(|m| role(m) == "user");
    let before = this_turn
        .and_then(|i| i.checked_sub(1))
        .filter(|&i| matches!(role(&messages[i]).as_str(), "user" | "assistant"));
    let mut marked = false;
    for i in [system, before, this_turn].into_iter().flatten() {
        marked |= mark_message(&mut messages[i]);
    }
    marked
}

/// Put a breakpoint on the last block of `message`, turning text sent as a
/// plain string into the one block it stands for. Nothing to mark -- no
/// content, or an empty string -- is left alone.
fn mark_message(message: &mut Value) -> bool {
    let breakpoint = serde_json::json!({ "type": "ephemeral" });
    match message.get_mut("content") {
        Some(Value::String(text)) if !text.is_empty() => {
            let text = std::mem::take(text);
            message["content"] = serde_json::json!([
                { "type": "text", "text": text, "cache_control": breakpoint }
            ]);
            true
        }
        Some(Value::Array(blocks)) => match blocks.last_mut().and_then(Value::as_object_mut) {
            Some(block) => {
                block.insert("cache_control".into(), breakpoint);
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Apply a model's temperature, token ceiling and reasoning effort to a
/// builder.
///
/// Generic over rig's tool-registration typestate so this can run whichever
/// side of tool registration a caller is on; every caller today runs it
/// before, which is why the builder's typestate note travels with them
/// rather than with this. `None` on any field leaves the provider's own
/// default alone rather than forcing one.
///
/// Every request to a model goes through here -- the assistant's turns, a
/// research job's, the quick asks and the mail features through
/// [`crate::quick::run_prompt`], a meeting's summary -- so this is the one
/// place a tier's settings reach the wire.
pub fn configure<T>(mut builder: AgentBuilder<T>, model: &LLMModelConfig) -> AgentBuilder<T> {
    // A model told to reason is not also sent a temperature. OpenAI's
    // reasoning models take one only at effort `none` and refuse the request
    // otherwise -- and the Small model's temperature of nought is a default
    // the models pane does not show, so somebody who picked Low there would
    // have no way to find what broke. A model that reasons picks its own
    // sampling anyway.
    let reasons = model.reasoning_effort.is_some_and(|e| e != ReasoningEffort::None);
    if let Some(temperature) = model.temperature
        && !reasons
    {
        builder = builder.temperature(temperature);
    }
    if let Some(max_tokens) = model.max_tokens {
        builder = builder.max_tokens(u64::from(max_tokens));
    }
    // Rig has no field of its own for this on chat completions, so it rides
    // in the additional parameters, which are flattened into the request
    // body. Nothing else here sets them; a second `additional_params` call
    // would replace this one rather than merge with it.
    if let Some(effort) = model.reasoning_effort {
        builder = builder.additional_params(serde_json::json!({ "reasoning_effort": effort }));
    }
    builder
}

/// Turn a provider's error into something worth showing a person.
///
/// The raw text is a transport error or a JSON body, and the three failures
/// that actually happen -- a wrong key, a wrong model name, nothing
/// listening -- all have an answer the person can act on. Shared by the
/// assistant and the quick model, which reach the same providers and fail
/// the same ways. Where the model that failed is at hand, use
/// [`friendly_for`], which can also say when its reasoning effort was refused.
pub fn friendly(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") || lower.contains("invalid_api_key")
    {
        return "The API key was refused. Check it in Settings.".into();
    }
    if lower.contains("404") || lower.contains("model_not_found") {
        return "That endpoint does not know the model you have configured. \
                Check the model name in Settings."
            .into();
    }
    if lower.contains("connection refused") || lower.contains("dns") || lower.contains("connect") {
        return "Could not reach the model. If it runs on this machine, check it is started; \
                otherwise check the base URL in Settings."
            .into();
    }
    if lower.contains("429") || lower.contains("rate limit") {
        return "The provider is rate limiting this key. Try again shortly.".into();
    }
    raw.to_string()
}

/// [`friendly`], for an error from asking `model` -- which can also tell a
/// refused reasoning effort from the rest.
pub fn friendly_for(raw: &str, model: &LLMModelConfig) -> String {
    refused_effort(raw, model).unwrap_or_else(|| friendly(raw))
}

/// The sentence for a provider refusing `model`'s reasoning effort, if that
/// is what `raw` is.
///
/// Only when an effort was sent: an error that merely mentions the parameter
/// -- a model whose own default effort clashes with something else -- must
/// not send somebody to change a setting they never touched. Checked ahead
/// of [`friendly`]'s cases because the refusal is a 400 that can name the
/// model too, and "check the model name" would send them to the wrong field.
/// OpenAI and OpenRouter name the parameter; Ollama says the model cannot
/// think, or that the level is not one it knows.
pub fn refused_effort(raw: &str, model: &LLMModelConfig) -> Option<String> {
    model.reasoning_effort?;
    let lower = raw.to_lowercase();
    let refused = [
        "reasoning_effort",
        "reasoning.effort",
        "does not support thinking",
        "invalid reasoning",
        "invalid think value",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    refused.then(|| {
        "That model does not take the reasoning effort you have set. \
         Choose another, or the provider's default, in Settings → Models."
            .into()
    })
}

/// The sentence for a provider refusing the pictures a turn sent `model`, if
/// that is what `raw` is.
///
/// Only asked about a turn that did send one, so an error that merely
/// mentions images cannot send somebody looking for a setting that is fine.
/// Every provider words it differently: OpenAI says `image_url` is only for
/// certain models, OpenRouter that no endpoint takes image input, LM Studio
/// and Ollama that the model does not support images, vLLM that it is not a
/// multimodal model.
///
/// Some local servers say nothing at all and drop the picture, and the
/// model then answers as though it had not been sent. Nothing here can
/// tell that apart from an answer.
pub fn refused_pictures(raw: &str, model: &LLMModelConfig) -> Option<String> {
    let lower = raw.to_lowercase();
    let refused = [
        "image_url",
        "image input",
        "support image",
        "supports image",
        "multimodal",
        "multi-modal",
        "vision",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    refused.then(|| {
        format!(
            "The model the assistant runs on ({}) cannot look at pictures. Choose one that can \
             for Medium in Settings → Models, or send the message without them.",
            model.model
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thinking(effort: Option<ReasoningEffort>) -> LLMModelConfig {
        LLMModelConfig { reasoning_effort: effort, ..LLMModelConfig::assistant() }
    }

    fn chat(model: &str, messages: Value) -> Value {
        serde_json::json!({ "model": model, "messages": messages, "stream": true })
    }

    fn sent(marks: &CacheMarks, body: &Value) -> Value {
        let req = Request::builder()
            .method("POST")
            .uri("https://example.org/api/v1/chat/completions")
            .header(http::header::CONTENT_LENGTH, "1")
            .body(serde_json::to_vec(body).unwrap())
            .unwrap();
        let req = marks.marked(req);
        if req.headers().contains_key(http::header::CONTENT_LENGTH) {
            assert_eq!(req.body().as_ref(), serde_json::to_vec(body).unwrap(), "untouched");
        }
        serde_json::from_slice(req.body()).unwrap()
    }

    fn marked_at(body: &Value) -> Vec<usize> {
        let messages = body["messages"].as_array().unwrap();
        (0..messages.len()).filter(|&i| messages[i].to_string().contains("cache_control")).collect()
    }

    #[test]
    fn the_system_prompt_the_thread_so_far_and_this_turn_are_marked() {
        let mut body = chat(
            "anthropic/claude-sonnet-4.5",
            serde_json::json!([
                { "role": "system", "content": "You are the assistant." },
                { "role": "user", "content": "one" },
                { "role": "assistant", "content": "Noted." },
                { "role": "user", "content": [
                    { "type": "text", "text": "<application_note>…</application_note> two" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } },
                ] },
                { "role": "assistant", "content": null, "tool_calls": [] },
                { "role": "tool", "tool_call_id": "call_1", "content": "{}" },
            ]),
        );
        assert!(mark_cache_breakpoints(&mut body));
        assert_eq!(marked_at(&body), [0, 2, 3]);
        let system = &body["messages"][0]["content"][0];
        assert_eq!(system["text"], "You are the assistant.", "a string becomes its one block");
        assert_eq!(system["cache_control"]["type"], "ephemeral");
        let this_turn = body["messages"][3]["content"].as_array().unwrap();
        assert!(this_turn[0].get("cache_control").is_none(), "only the last block");
        assert_eq!(this_turn[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"][5]["content"], "{}", "a tool result goes as it came");
    }

    #[test]
    fn a_first_turn_marks_the_system_prompt_and_its_own_message_once_each() {
        let mut body = chat(
            "claude",
            serde_json::json!([
                { "role": "system", "content": "rules" },
                { "role": "user", "content": "hello" },
            ]),
        );
        assert!(mark_cache_breakpoints(&mut body));
        assert_eq!(marked_at(&body), [0, 1]);
        assert_eq!(body.to_string().matches("cache_control").count(), 2);

        let mut empty = chat("claude", serde_json::json!([]));
        assert!(!mark_cache_breakpoints(&mut empty));
    }

    #[test]
    fn marks_go_to_openrouter_and_to_claude_and_nowhere_else() {
        let messages = serde_json::json!([
            { "role": "system", "content": "rules" },
            { "role": "user", "content": "hello" },
        ]);
        let marks = CacheMarks::for_conversation;
        for model in ["deepseek/deepseek-v3.2", "z-ai/glm-4.6", "anthropic/claude-opus-4.1"] {
            let openrouter = marks("https://openrouter.ai/api/v1", model);
            assert_eq!(marked_at(&sent(&openrouter, &chat(model, messages.clone()))), [0, 1]);
        }
        let qwen = marks("https://dashscope-intl.aliyuncs.com/compatible-mode/v1", "qwen-max");
        assert_eq!(marked_at(&sent(&qwen, &chat("qwen-max", messages.clone()))), [0, 1]);

        // DeepSeek, Z.ai and OpenAI cache by themselves, and have no field
        // for a mark.
        for (endpoint, model) in [
            ("https://api.deepseek.com/v1", "deepseek-chat"),
            ("https://api.z.ai/api/paas/v4", "glm-4.6"),
            ("https://api.openai.com/v1", "gpt-5.1"),
            ("http://127.0.0.1:11434/v1", "qwen3:32b"),
            // Gemini reads only the last mark, which never repeats.
            ("https://openrouter.ai/api/v1", "google/gemini-2.5-pro"),
            ("https://notopenrouter.ai/v1", "deepseek-chat"),
        ] {
            let body = chat(model, messages.clone());
            assert_eq!(sent(&marks(endpoint, model), &body), body, "{endpoint} {model}");
        }

        // A gateway serving Claude by that name speaks Anthropic's dialect.
        let gateway = marks("https://llm.example.org/v1", "claude-sonnet-4-5");
        assert_eq!(
            marked_at(&sent(&gateway, &chat("claude-sonnet-4-5", messages.clone()))),
            [0, 1]
        );

        // And a one-shot request is never marked, wherever it goes.
        let one_shot = CacheMarks::default();
        let body = chat("anthropic/claude-opus-4.1", messages);
        assert_eq!(sent(&one_shot, &body), body);
    }

    #[test]
    fn a_request_that_is_not_a_chat_goes_as_it_came() {
        let marks = CacheMarks::for_conversation("https://openrouter.ai/api/v1", "z-ai/glm-4.6");
        let req = Request::builder()
            .uri("https://openrouter.ai/api/v1/embeddings")
            .body(b"{\"messages\":[{\"role\":\"user\",\"content\":\"x\"}]}".to_vec())
            .unwrap();
        let body = marks.marked(req).into_body();
        assert!(!String::from_utf8_lossy(&body).contains("cache_control"));
    }

    #[test]
    fn a_refused_effort_points_at_the_effort_not_the_model_name() {
        let set = thinking(Some(ReasoningEffort::High));
        for raw in [
            // OpenAI, on a level the model does not take.
            "CompletionError: ProviderError: 400 Unsupported value: 'reasoning_effort' does \
             not support 'none' with this model. Supported values are: 'low', 'medium', and \
             'high'.",
            // Ollama, on a model without thinking.
            r#"400 {"error":"\"llama3:latest\" does not support thinking"}"#,
            // Ollama, on a level it does not know.
            r#"400 {"error":"invalid reasoning value: 'max'"}"#,
        ] {
            let said = friendly_for(raw, &set);
            assert!(said.contains("reasoning effort"), "{raw} -> {said}");
        }
        assert!(friendly_for("404 model_not_found", &set).contains("model name"));
    }

    #[test]
    fn a_model_that_cannot_see_is_named_when_it_refuses_a_picture() {
        let model = LLMModelConfig { model: "llama3:8b".into(), ..LLMModelConfig::assistant() };
        for raw in [
            // OpenAI, on a model without vision.
            "CompletionError: ProviderError: 400 Invalid content type. image_url is only \
             supported by certain models.",
            // OpenRouter, with no endpoint for the model that takes pictures.
            r#"404 {"error":{"message":"No endpoints found that support image input"}}"#,
            // LM Studio.
            r#"400 {"error":"Model does not support images. Please use a model that does."}"#,
            // vLLM.
            "400 llama3 is not a multimodal model",
        ] {
            let said = refused_pictures(raw, &model).unwrap_or_else(|| panic!("{raw}"));
            assert!(said.contains("llama3:8b") && said.contains("pictures"), "{said}");
        }
        assert_eq!(refused_pictures("401 invalid_api_key", &model), None);
    }

    #[test]
    fn an_effort_nobody_set_is_never_blamed() {
        let raw = "400 Unsupported parameter: 'temperature' is not supported with \
                   reasoning_effort 'medium' on this model.";
        assert_eq!(friendly_for(raw, &thinking(None)), raw, "passed through as it came");
        assert!(friendly_for(raw, &thinking(Some(ReasoningEffort::Low))).contains("Settings"));
    }
}
