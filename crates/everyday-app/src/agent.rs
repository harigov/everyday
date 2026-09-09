//! The assistant's harness: the half that owns a socket.
//!
//! The other half is [`everyday_core::agent`], which decides what the
//! assistant *is* — its settings, its prompt, and the thirty-four tools it
//! may run — with no provider and no network anywhere in it. This file is
//! the adapter: it wraps those tools in the shape [`rig`](rig_agent) wants,
//! opens the connection, and turns what comes back into events the interface
//! can draw and records the vault can keep.
//!
//! That is the same split [`crate::feeds`] and [`crate::websearch`] make, and
//! it is worth restating because it is what makes this feature testable at
//! all: "the assistant marked the wrong task done" is a unit test in the
//! core, offline, with no API key. Nothing in *this* file decides what a tool
//! does, so nothing in this file can get that wrong.
//!
//! # Why the tools are registered dynamically
//!
//! Rig's typed [`PortableTool`](rig_agent::core::tool::PortableTool) wants one
//! Rust type per tool, with its argument struct and its schema derived. The
//! catalogue is a `&'static [Tool]` whose schemas are built at runtime and
//! whose *membership depends on the vault* — a Markdown backend stores no
//! tasks, so its assistant must not be told `create_task` exists. So each
//! entry becomes a
//! [`PortableDynamicTool`](rig_agent::core::tool::PortableDynamicTool), which
//! takes its name, description and schema as ordinary values. The typed path
//! would have bought compile-time argument structs and cost the ability to
//! offer a different toolset per vault, which is the wrong trade.
//!
//! # Chat Completions, not the Responses API
//!
//! Rig's default OpenAI client speaks the Responses API. This one asks for
//! [`CompletionsClient`](rig_agent::core::providers::openai::CompletionsClient)
//! instead, because the whole point of the base-URL override is that Ollama,
//! LM Studio, vLLM and OpenRouter can be pointed at — and what they all
//! implement is `/chat/completions`. Choosing the newer API here would make
//! the setting that exists for local models work everywhere except local
//! models.
//!
//! # Where the confirmation gate lives
//!
//! In [`ConfirmGate`], an [`AgentHook`] that runs before any tool body does.
//! Rig's `on_tool_call` can answer `Run` or `Skip(reason)`, and the skip's
//! reason is handed to the model — so a declined delete becomes something the
//! assistant is told about and can respond to, rather than an error it has to
//! interpret. The hook is `async`, which is what lets it wait for a person.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use everyday_core::agent::tools::{self, Effect, ToolContext};
use everyday_core::agent::{AgentSettings, Conversation, Message as VaultMessage, Role, ToolCall};
use everyday_core::model::{system_tz, today_local};
use everyday_core::{ConversationId, Vault};
use rig_agent::agent::hook::{
    ToolCall as HookToolCall, ToolCallAction, ToolResultAction, ToolResultEvent,
};
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::core::providers::openai;
use rig_agent::core::tool::{PortableDynamicTool, ToolExecutionError, ToolOutput};
use rig_agent::prelude::*;
use rig_agent::{Agent, AgentBuilder, AgentHook, HookContext};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::error::{CommandError, CommandResult};

/// One thing that happened during a turn, on its way to the panel.
///
/// Serialised over a Tauri channel rather than returned at the end, because a
/// reply that takes twenty seconds and arrives all at once reads as a hang.
/// The variants are what the panel has to draw differently, and no more.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    /// The assistant's message id, sent first so every delta that follows has
    /// something to be appended to.
    Started { message_id: String },
    /// A fragment of prose.
    Delta { text: String },
    /// The assistant has decided to run something. Drawn as a card.
    ToolStarted { call_id: String, name: String, arguments: serde_json::Value },
    /// That tool finished, or failed. `summary` is the human sentence the
    /// card shows; the model gets the full result separately.
    ToolFinished { call_id: String, name: String, ok: bool, summary: String },
    /// A destructive call is waiting on a person. The panel draws the
    /// confirm/decline buttons and answers with `confirm_tool_call`.
    ConfirmationRequired {
        call_id: String,
        name: String,
        /// What will be destroyed, named rather than identified: "the deck"
        /// rather than a UUID.
        subject: String,
        arguments: serde_json::Value,
    },
    /// The turn is over. Sent exactly once, whatever else happened, so the
    /// panel always has something to stop its spinner on.
    Finished { message_id: String },
    /// The turn ended badly. Also terminal.
    Failed { message: String },
}

/// Confirmations waiting on a person, and the vault they belong to.
///
/// Process-wide, held in Tauri's managed state beside [`crate::state::AppState`].
/// Keyed by the call id the panel was given, so an answer names exactly the
/// call it is answering -- two destructive calls in one turn is an ordinary
/// thing for a model to emit, and a bare "yes" could not be routed.
#[derive(Default)]
pub struct Pending {
    waiting: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl Pending {
    /// Register a call and hand back the half that waits for the answer.
    fn register(&self, call_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(call_id.to_string(), tx);
        rx
    }

    /// Answer a waiting call. False if nothing was waiting -- which happens
    /// when a turn was cancelled between the question and the click, and is
    /// not an error worth showing anybody.
    pub fn answer(&self, call_id: &str, approved: bool) -> bool {
        match self.waiting.lock().unwrap().remove(call_id) {
            Some(tx) => tx.send(approved).is_ok(),
            None => false,
        }
    }

    /// Drop every pending question. Called when a turn ends for any reason,
    /// so a confirmation card cannot outlive the run that raised it and
    /// answer a later one.
    fn clear(&self) {
        self.waiting.lock().unwrap().clear();
    }
}

/// Runs before any tool body, and stops the destructive ones to ask.
///
/// The list of what is destructive is
/// [`Effect::Destructive`](everyday_core::agent::tools::Effect) in the core --
/// data on the tool rather than a naming convention -- so a tool added later
/// cannot slip past this by being called something else.
struct ConfirmGate {
    pending: Arc<Pending>,
    channel: tauri::ipc::Channel<AgentEvent>,
    /// Off when the person has turned confirmation off in settings. The gate
    /// is still installed, because the events it emits are also how the panel
    /// draws what ran.
    enabled: bool,
    /// For naming what a destructive call is about to act on. Every such tool
    /// takes an id and nothing else, so the name has to be read.
    vault: Arc<Vault>,
    today: jiff::civil::Date,
    tz: String,
    /// What ran this turn, in call order, so the thread is written down with
    /// its tool calls rather than only its prose. See `run_turn`.
    ledger: Arc<Mutex<Vec<Ran>>>,
}

/// One tool call and what it returned, kept for the record.
#[derive(Clone)]
struct Ran {
    call: ToolCall,
    /// `None` until the result arrives -- a call the run abandoned keeps it.
    outcome: Option<std::result::Result<String, String>>,
}

impl AgentHook for ConfirmGate {
    async fn on_tool_call(&self, _ctx: &HookContext, event: HookToolCall<'_>) -> ToolCallAction {
        let name = event.tool_name.to_string();
        // Rig's own correlator rather than the provider's id: it is always
        // present, and it is what the result event carries -- so a card
        // raised here is closed by the right result.
        let call_id = event.internal_call_id.to_string();
        // Arguments arrive as the JSON text the model produced. Parsed for
        // the panel's sake; a model that emitted something unparseable is
        // rig's to reject, not this hook's.
        let arguments: serde_json::Value =
            serde_json::from_str(event.args).unwrap_or(serde_json::Value::Null);

        self.ledger.lock().unwrap().push(Ran {
            call: ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            },
            outcome: None,
        });

        let destructive = tools::find(&name).is_some_and(|t| t.effect == Effect::Destructive);
        if !destructive || !self.enabled {
            let _ = self.channel.send(AgentEvent::ToolStarted { call_id, name, arguments });
            return ToolCallAction::Run;
        }

        let waiter = self.pending.register(&call_id);
        let _ = self.channel.send(AgentEvent::ConfirmationRequired {
            call_id: call_id.clone(),
            name: name.clone(),
            subject: self.describe(&name, &arguments),
            arguments: arguments.clone(),
        });

        // A dropped sender means the turn was cancelled or the window went
        // away. Treated as a refusal, because the alternative is deleting
        // something nobody was left to agree to.
        match waiter.await {
            Ok(true) => {
                let _ = self.channel.send(AgentEvent::ToolStarted { call_id, name, arguments });
                ToolCallAction::Run
            }
            Ok(false) => ToolCallAction::Skip(
                "The person declined this. Do not try it again or work around it; \
                 tell them it was not done and ask what they would like instead."
                    .into(),
            ),
            Err(_) => ToolCallAction::Skip("This was not confirmed and has not been done.".into()),
        }
    }

    /// Close the card the call opened.
    ///
    /// Taken from a hook rather than from the stream because the stream's
    /// `ToolExecutionCommitted` carries no result, and because this fires for
    /// a failed call too -- which the panel must draw differently rather than
    /// leave spinning.
    async fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        let ok = event.raw_result.is_success();
        let summary = match event.raw_result.error() {
            Some(e) => e.to_string(),
            None => summarise(event.raw_result.output()),
        };

        // Recorded against the call it answers, so a reopened thread shows
        // what the assistant did rather than only what it said about it.
        let mut ledger = self.ledger.lock().unwrap();
        if let Some(ran) = ledger.iter_mut().find(|r| r.call.id == event.internal_call_id) {
            ran.outcome = Some(if ok { Ok(summary.clone()) } else { Err(summary.clone()) });
        }
        drop(ledger);

        let _ = self.channel.send(AgentEvent::ToolFinished {
            call_id: event.internal_call_id.to_string(),
            name: event.tool_name.to_string(),
            ok,
            summary,
        });
        ToolResultAction::Keep
    }
}

impl ConfirmGate {
    /// What this call is about to act on, named rather than identified.
    ///
    /// Falls back to nothing rather than to an id: a card that says "delete
    /// project" with no subject asks somebody to think, and one that says
    /// "delete 0192f8b2-..." asks them to guess.
    fn describe(&self, name: &str, arguments: &Value) -> String {
        let ctx =
            ToolContext { vault: &self.vault, today: self.today, tz: &self.tz, conversation: None };
        tools::describe(&ctx, name, arguments).unwrap_or_default()
    }
}

/// Wrap the core catalogue as rig tools and assemble the agent.
///
/// `vault` is captured by every tool callback, which is why it arrives as an
/// `Arc`: rig requires the callbacks to be `'static`, and the run outlives the
/// command that started it.
fn build(
    vault: Arc<Vault>,
    settings: &AgentSettings,
    key: Option<String>,
    conversation: ConversationId,
    context: Option<&str>,
) -> CommandResult<Agent> {
    let model = &settings.model;

    // A local model needs no credential and is usually configured without
    // one, and the endpoint ignores whatever is sent -- so a keyless
    // configuration sends an empty bearer rather than omitting the step,
    // which would leave the builder's auth type unresolved. See
    // `Provider::needs_key` for when a key is insisted on at all.
    let client = openai::CompletionsClient::builder()
        .base_url(model.endpoint())
        .api_key::<rig_agent::core::client::BearerAuth>(key.unwrap_or_default())
        .build()
        .map_err(|e| CommandError::new("agent", format!("could not start the assistant: {e}")))?;

    let memories = vault.memories()?;
    let preamble = everyday_core::agent::system_prompt(settings, &memories, today_local(), context);

    let mut builder = AgentBuilder::new(client.completion_model(&model.model))
        .preamble(&preamble)
        .default_max_turns(settings.max_steps as usize);
    if let Some(t) = model.temperature {
        builder = builder.temperature(t);
    }
    if let Some(m) = model.max_tokens {
        builder = builder.max_tokens(u64::from(m));
    }

    // Only the tools this vault can actually serve. A model is never told
    // about storage that does not exist, so it cannot claim to have used it.
    let wrap = |tool: &'static tools::Tool| {
        let vault = vault.clone();
        let name = tool.name;
        PortableDynamicTool::new(
            tool.name,
            tool.description,
            tool.parameters(),
            move |arguments: serde_json::Value| {
                let vault = vault.clone();
                Box::pin(async move { run_tool(vault, name, arguments, conversation).await })
            },
        )
    };

    // The builder is a typestate: the first tool moves it from "no tools" to
    // "tools", and the two states have different types. So the first is
    // registered on its own and the rest fold onto what that returns -- and a
    // vault with no tools at all, which no shipped backend produces, still
    // builds rather than being a case to handle.
    let available = tools::available(&vault);
    let Some((first, rest)) = available.split_first() else {
        return Ok(builder.build());
    };
    let mut builder = builder.portable_dynamic_tool(wrap(first));
    for tool in rest {
        builder = builder.portable_dynamic_tool(wrap(tool));
    }

    Ok(builder.build())
}

/// Run one tool call on the blocking pool.
///
/// Every tool touches the vault, which is synchronous and hits disk; running
/// it on an async worker would stall the runtime for the duration of a
/// decrypt. This is the same `spawn_blocking` discipline every command in
/// [`crate::commands`] follows, for the reason given there.
async fn run_tool(
    vault: Arc<Vault>,
    name: &'static str,
    arguments: serde_json::Value,
    conversation: ConversationId,
) -> Result<ToolOutput, ToolExecutionError> {
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let ctx = ToolContext {
            vault: &vault,
            // Read per call rather than once per turn: a conversation left
            // open overnight must not still think it is yesterday.
            today: today_local(),
            tz: &system_tz(),
            conversation: Some(conversation),
        };
        tools::dispatch(&ctx, name, &arguments)
    })
    .await;

    match outcome {
        Ok(Ok(value)) => Ok(ToolOutput::json(value)),
        // A tool that failed is not a turn that failed. The message goes back
        // to the model, which gets another go -- see `Args` in the core,
        // where these are written to be read by one.
        Ok(Err(e)) => Err(ToolExecutionError::invalid_args(e.to_string())),
        Err(e) => Err(ToolExecutionError::other(format!("the tool did not finish: {e}"))),
    }
}

/// Everything one turn needs from the caller.
pub struct Turn {
    pub vault: Arc<Vault>,
    pub pending: Arc<Pending>,
    pub conversation: ConversationId,
    pub prompt: String,
    /// What the person is looking at, if the interface said.
    pub context: Option<String>,
    pub channel: tauri::ipc::Channel<AgentEvent>,
}

/// Run one turn: send what was typed, stream what comes back, write it down.
///
/// The person's message is persisted *before* the model is called, so a
/// request that fails still leaves the thread showing what was asked. The
/// assistant's is written empty when the stream opens and rewritten when it
/// closes, which is why [`AgentStore::put_message`] exists -- a cancelled
/// stream must not leave a thread with no record that a reply was attempted.
///
/// [`AgentStore::put_message`]: everyday_core::store::agent::AgentStore::put_message
pub async fn run_turn(turn: Turn) -> CommandResult<()> {
    let Turn { vault, pending, conversation, prompt, context, channel } = turn;

    let (settings, key) = vault.agent_credentials()?;

    // The thread exists before the first message goes into it.
    if vault.conversation(conversation).is_err() {
        vault.save_conversation(&Conversation { id: conversation, ..Conversation::new() })?;
    }

    let asked = VaultMessage::user(conversation, prompt.clone());
    vault.save_message(&asked)?;

    let history = replay(&vault, conversation)?;
    let reply = VaultMessage::assistant(conversation, String::new());
    vault.save_message(&reply)?;
    let _ = channel.send(AgentEvent::Started { message_id: reply.id.to_string() });

    let agent = build(vault.clone(), &settings, key, conversation, context.as_deref())?;
    let ledger: Arc<Mutex<Vec<Ran>>> = Arc::default();
    let gate = ConfirmGate {
        pending: pending.clone(),
        channel: channel.clone(),
        enabled: settings.confirm_destructive,
        vault: vault.clone(),
        today: today_local(),
        tz: system_tz(),
        ledger: ledger.clone(),
    };

    let outcome =
        stream(&agent, gate, &prompt, history, &channel, settings.max_steps as usize).await;

    // Whatever happened, nothing may still be waiting on a person: a
    // confirmation card that outlived its run would answer the next one.
    pending.clear();

    // What ran, whether or not the turn as a whole succeeded. A run that
    // failed after deleting a project must still show the deletion.
    let ran = std::mem::take(&mut *ledger.lock().unwrap());

    match outcome {
        Ok(text) => {
            let finished = VaultMessage {
                content: text,
                tool_calls: ran.iter().map(|r| r.call.clone()).collect(),
                ..reply.clone()
            };
            vault.save_message(&finished)?;
            write_results(&vault, conversation, &ran);
            let _ = channel.send(AgentEvent::Finished { message_id: reply.id.to_string() });
            Ok(())
        }
        Err(e) => {
            if ran.is_empty() {
                // Nothing happened, so the empty assistant turn is removed
                // rather than left behind as a silent blank reply; the
                // failure is shown by the panel instead.
                let _ = vault.delete_message(reply.id);
            } else {
                // Something did happen. The turn is kept, holding what ran,
                // because a thread that shows no sign of a delete that
                // actually took place is worse than an empty reply.
                let kept = VaultMessage {
                    tool_calls: ran.iter().map(|r| r.call.clone()).collect(),
                    ..reply.clone()
                };
                let _ = vault.save_message(&kept);
                write_results(&vault, conversation, &ran);
            }
            let _ = channel.send(AgentEvent::Failed { message: e.message.clone() });
            Err(e)
        }
    }
}

/// Write one `Role::Tool` message per call that reported.
///
/// After the assistant turn that holds the calls, because that is the order
/// the panel folds them in -- a result looks backwards for the turn that
/// asked for it. A call with no outcome is one the run abandoned, and is
/// left without a result rather than given a made-up one.
///
/// Best effort: the reply is already saved, and failing the whole turn
/// because a history line did not land would report "nothing happened" for
/// something that did.
fn write_results(vault: &Vault, conversation: ConversationId, ran: &[Ran]) {
    for entry in ran {
        let Some(outcome) = entry.outcome.clone() else { continue };
        let message = VaultMessage::tool_result(conversation, &entry.call, outcome);
        if let Err(e) = vault.save_message(&message) {
            tracing::warn!(error = %e, "could not write down a tool result");
        }
    }
}

/// The thread so far, in rig's shape.
///
/// Only what a model needs to continue: the prose either party produced, in
/// order. Tool calls and their results are deliberately *not* replayed --
/// they are kept in the vault so the panel can draw what happened, but
/// feeding a model back its own calls from a previous turn invites it to
/// treat them as still pending. What it needs from a finished turn is the
/// sentence that summarised it, which is the assistant message beside them.
fn replay(vault: &Vault, id: ConversationId) -> CommandResult<Vec<rig_agent::completion::Message>> {
    let mut out = Vec::new();
    for m in vault.messages(id)? {
        if m.content.trim().is_empty() {
            continue;
        }
        match m.role {
            Role::User => out.push(rig_agent::completion::Message::user(m.content)),
            Role::Assistant => out.push(rig_agent::completion::Message::assistant(m.content)),
            // Tool results and the application's own notes are not the
            // model's to re-read. See above.
            Role::Tool | Role::System => {}
        }
    }
    // The turn being asked now is passed separately, so its own message --
    // already persisted above -- must not also appear in the history.
    out.pop();
    Ok(out)
}

/// Drive the stream, forwarding prose to the panel as it arrives.
async fn stream(
    agent: &Agent,
    gate: ConfirmGate,
    prompt: &str,
    history: Vec<rig_agent::completion::Message>,
    channel: &tauri::ipc::Channel<AgentEvent>,
    max_turns: usize,
) -> CommandResult<String> {
    use futures::StreamExt;
    use rig_agent::agent::MultiTurnStreamItem;
    use rig_agent::core::streaming::StreamedAssistantContent;

    let mut stream = agent.stream_chat(prompt, history).max_turns(max_turns).add_hook(gate).await;

    let mut text = String::new();
    while let Some(item) = stream.next().await {
        match item {
            // Prose, and only prose. Everything about a tool call reaches the
            // panel from the hooks, which see the calls stopped to ask as
            // well as the ones that ran.
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t))) => {
                text.push_str(&t.text);
                let _ = channel.send(AgentEvent::Delta { text: t.text });
            }
            Ok(_) => {}
            Err(e) => return Err(CommandError::new("agent", friendly(&e.to_string()))),
        }
    }

    Ok(text)
}

/// One line about what a tool did, for the card in the panel.
///
/// Reads the shape every mutating tool returns -- see `done` in the core --
/// and falls back to saying nothing rather than dumping JSON at somebody.
fn summarise(output: &ToolOutput) -> String {
    let Some(result) = output.as_json() else {
        return output.as_text().unwrap_or_default().chars().take(120).collect();
    };
    let action = result.get("action").and_then(|v| v.as_str()).unwrap_or_default();
    let kind = result.get("kind").and_then(|v| v.as_str()).unwrap_or_default();
    let name = result.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    if action.is_empty() {
        // A read. The count is the only interesting thing about it.
        return match result.get("count").and_then(|v| v.as_u64()) {
            Some(1) => "1 result".into(),
            Some(n) => format!("{n} results"),
            None => String::new(),
        };
    }
    format!("{action} {kind} {name}").trim().to_string()
}

/// Turn a provider's error into something worth showing a person.
///
/// The raw text is a transport error or a JSON body, and the three failures
/// that actually happen -- a wrong key, a wrong model name, nothing listening
/// -- all have an answer the person can act on.
fn friendly(raw: &str) -> String {
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
