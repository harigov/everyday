//! The quick model's harness: the half that owns a socket.
//!
//! The other half is [`karmayoga_core::quick`], which decides everything about
//! what an ask *is* — its instruction, its schema, the prompt it builds and
//! the clamps on the answer — with no provider and no network in it. This
//! file opens the connection and nothing else. It never looks at an ask's
//! name to decide what to do; it is handed a
//! [`Prompt`](karmayoga_core::quick::Prompt) and posts it.
//!
//! Same split as [`crate::agent`], [`crate::feeds`] and [`crate::websearch`],
//! and the payoff is the one that matters for a feature that runs in a
//! capture box: "the quick model put the author in the year field" is a unit
//! test in the core, offline, with no API key.
//!
//! # Why a `submit` tool rather than `rig`'s `Extractor`
//!
//! [`rig_agent::extractor::Extractor`] wants one Rust type per answer with
//! `JsonSchema` derived, resolved at compile time. The schemas here are
//! values built at runtime from [`ASKS`](karmayoga_core::quick::ASKS), for the
//! same reason [`crate::agent`] registers dynamic tools rather than typed
//! ones: the catalogue is data, and an ask added to it must not require a
//! match arm in this file. So this does by hand what `Extractor` does
//! internally — one tool called `submit` carrying the ask's schema, tool
//! choice forced, and the arguments of the call *are* the answer.
//!
//! # What bounds a quick ask
//!
//! Two turns and a short timeout, neither of them configurable. A quick ask
//! that needs a third turn has misunderstood the question, and one that takes
//! eight seconds has already lost to the person typing the field in
//! themselves — the interaction it serves is a suggestion appearing beside
//! something already saved, so being late is the same as being wrong.
//!
//! There is no retry. `Extractor` retries because an extraction is the whole
//! of its caller's request; here the caller is a chip that either appears or
//! does not, and a second attempt doubles the bill on the failure that is
//! most likely to fail again — a model too small to call a tool.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use karmayoga_core::Vault;
use karmayoga_core::llm::{LLMModelConfig, ReasoningEffort};
// Aliased: `rig_agent::prelude` brings its own `Prompt`, and that one is the
// trait carrying `.prompt()`.
use karmayoga_core::quick::Prompt as QuickPrompt;
use rig_agent::AgentBuilder;
use rig_agent::agent::OutputMode;
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::core::message::ToolChoice;
use rig_agent::core::tool::{PortableDynamicTool, ToolOutput};
use rig_agent::prelude::*;
use serde_json::{Value, json};

use crate::error::{CommandError, CommandResult, codes};

/// How long a quick ask may take before it is abandoned.
///
/// Generous for a nano-tier model answering a small schema, and short enough
/// that a suggestion which has not arrived by now has stopped being a
/// suggestion. The clock covers the whole exchange, not one request.
pub const TIMEOUT: Duration = Duration::from_secs(12);

/// How long an ask to `model` may take: [`TIMEOUT`], unless the model was
/// told to think at length.
///
/// The same exchange carries the mail features that run on Medium --
/// summaries, drafts, suggested replies -- and Medium may be set to think
/// hard, which a person chose knowing it is slower. Holding that to twelve
/// seconds would make the setting break the features rather than improve
/// them. Small cannot be set above Low (`SMALL_MAX_EFFORT`), so the asks
/// that run as somebody types keep the short clock.
pub fn timeout_for(model: &LLMModelConfig) -> Duration {
    match model.reasoning_effort {
        None | Some(ReasoningEffort::None | ReasoningEffort::Minimal | ReasoningEffort::Low) => {
            TIMEOUT
        }
        Some(ReasoningEffort::Medium) => Duration::from_secs(30),
        Some(ReasoningEffort::High) => Duration::from_secs(60),
        Some(ReasoningEffort::Xhigh | ReasoningEffort::Max) => Duration::from_secs(120),
    }
}

/// Turns a quick ask may take.
///
/// Two: one to call `submit`, and one spare for a model that opens with a
/// sentence before doing as it was asked. Not a setting — see the module
/// docs.
const MAX_TURNS: usize = 2;

/// The name of the one tool a quick ask is given.
const SUBMIT: &str = "submit";

/// Run one ask and return the raw answer, for the core to parse and clamp.
///
/// The gate is [`Vault::quick_credentials`], which refuses an ask the policy
/// has not allowed — so a caller cannot reach the endpoint by forgetting to
/// check, and there is exactly one place the check lives. The exchange
/// itself is [`run_prompt`]; this is only what gets a client and a model out
/// of the vault first.
pub async fn run(vault: Arc<Vault>, prompt: QuickPrompt) -> CommandResult<Value> {
    let (client, model) = open(&vault, &prompt)?;
    run_prompt(client, &model, &prompt.system, &prompt.user, prompt.schema.clone()).await
}

/// [`run`] with a clock of the caller's, for an ask that writes more than a
/// chip's worth and has nobody waiting on it: a page read for a research job
/// is up to four sentences, eight quotes and a handful of links from twenty
/// thousand characters, which the chip's twelve seconds is too short for on a
/// slow afternoon. Still never shorter than the model is owed
/// ([`timeout_for`]); see [`run_prompt_within`].
pub async fn run_within(
    vault: Arc<Vault>,
    prompt: QuickPrompt,
    within: Duration,
) -> CommandResult<Value> {
    let (client, model) = open(&vault, &prompt)?;
    run_prompt_within(client, &model, &prompt.system, &prompt.user, prompt.schema.clone(), within)
        .await
}

/// The client and the model one ask is posted to, once the policy has let it
/// through: the gate [`run`] and [`run_within`] share.
fn open(
    vault: &Vault,
    prompt: &QuickPrompt,
) -> CommandResult<(crate::llm::Client, LLMModelConfig)> {
    let access = vault
        .quick_credentials(&prompt.ask)
        .map_err(|e| CommandError::new(codes::QUICK, e.to_string()))?;
    let client = crate::llm::client_for(&access)
        .map_err(|e| CommandError::new(codes::QUICK, format!("could not reach the model: {e}")))?;
    Ok((client, access.config.model))
}

/// What [`run_prompt`] fails with when the model replied without calling
/// `submit` -- in prose, or not at all. A caller asking one question per
/// message tells it apart from a model that is down: it is about the message,
/// and asking again would get the same.
pub const NO_ANSWER: &str = "the model did not answer";

/// One request, no tools but `submit`, tool choice forced — the exchange
/// every one-shot structured ask in this application makes, whichever model
/// it is aimed at. Split out of [`run`] so that `karmayoga_service::mailai`
/// can post the same shape of request against a *different* model —
/// [`karmayoga_core::mail`]'s model-assisted categorisation runs against the
/// quick model exactly as a quick ask would, but is not itself a
/// [`karmayoga_core::quick::QuickAsk`] and so is never gated by
/// [`Vault::quick_credentials`]'s policy check; its own gate is
/// [`karmayoga_core::mail::mail_ai_allowed`]. Saying the request-building rule
/// once here, rather than copying it, is the whole point.
pub async fn run_prompt(
    client: crate::llm::Client,
    model: &LLMModelConfig,
    system: &str,
    user: &str,
    schema: Value,
) -> CommandResult<Value> {
    run_prompt_within(client, model, system, user, schema, timeout_for(model)).await
}

/// [`run_prompt`] with a clock of the caller's, for what runs with nobody
/// waiting on it. Never shorter than `model` is owed: see [`timeout_for`].
pub async fn run_prompt_within(
    client: crate::llm::Client,
    model: &LLMModelConfig,
    system: &str,
    user: &str,
    schema: Value,
    within: Duration,
) -> CommandResult<Value> {
    // The answer arrives as the arguments of the tool call rather than as
    // prose, which is the whole point: there is no fenced block to find, no
    // preamble to strip and no chance of "Sure! Here's the JSON:".
    let answer: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let sink = answer.clone();
    let schema_for_text = schema.clone();
    let submit = PortableDynamicTool::new(
        SUBMIT,
        "Submit the answer. Call this exactly once.",
        schema,
        move |arguments: Value| {
            let sink = sink.clone();
            Box::pin(async move {
                // First call wins. A model that submits twice has changed its
                // mind, and the second answer is the one that came after it
                // was told to stop.
                let mut slot = sink.lock().expect("the quick answer slot is never poisoned");
                if slot.is_none() {
                    *slot = Some(arguments);
                }
                Ok(ToolOutput::from(json!({ "ok": true })))
            })
        },
    );

    let builder = AgentBuilder::new(client.completion_model(&model.model))
        // Neutral on purpose: what an empty answer means is the asking
        // prompt's to say (a suggestion with nothing worth suggesting is
        // empty; a summary or a yes/no never is), and a sentence here saying
        // "empty is fine" told every prompt it was.
        .preamble(&format!("{system}\n\nCall the `submit` function exactly once with your answer."))
        .default_max_turns(MAX_TURNS)
        .tool_choice(ToolChoice::Required)
        .output_mode(OutputMode::Tool);
    let builder = crate::llm::configure(builder, model);
    // Registering the tool last: the builder is a typestate and this is the
    // move from "no tools" to "tools", so everything set by `if let` has to
    // happen while the type is still the first one.
    let agent = builder.portable_dynamic_tool(submit).build();

    // The timeout wraps the whole exchange rather than one request, because
    // the failure it exists for is a model that answers slowly twice.
    let timeout = within.max(timeout_for(model));
    let turn = tokio::time::timeout(timeout, agent.prompt(user).max_turns(MAX_TURNS));
    match turn.await {
        // A model or a server that ignored the forced tool choice and wrote
        // its answer out as JSON text instead -- some OpenAI-compatible
        // servers accept only `auto`, and many local models are weak at
        // tools -- has still answered: the outermost object in the text is
        // taken when it carries what the schema requires.
        Ok(Ok(text)) => {
            let mut slot = answer.lock().expect("the quick answer slot is never poisoned");
            if slot.is_none() {
                *slot = json_in_text(&text, &schema_for_text);
            }
        }
        // A model that ran out of turns may still have called `submit` on the
        // way, so the answer is checked before the error is believed.
        Ok(Err(e)) => {
            if answer.lock().expect("not poisoned").is_none() {
                return Err(CommandError::new(
                    codes::QUICK,
                    crate::llm::friendly_for(&e.to_string(), model),
                ));
            }
        }
        Err(_) => {
            // Said when the effort is what made it slow, because that is the
            // one cause here a person can do something about.
            let why = if timeout > TIMEOUT {
                "; a lower reasoning effort in Settings → Models answers sooner"
            } else {
                ""
            };
            return Err(CommandError::new(
                codes::QUICK,
                format!("the model took longer than {}s{why}", timeout.as_secs()),
            ));
        }
    }

    answer
        .lock()
        .expect("the quick answer slot is never poisoned")
        .take()
        .ok_or_else(|| CommandError::new(codes::QUICK, NO_ANSWER))
}

/// The answer in a reply written as text: the outermost `{…}`, once any
/// reasoning before it is taken out, when it parses as an object holding
/// every key `schema` requires (or, for a schema that requires none, any
/// object at all).
fn json_in_text(text: &str, schema: &Value) -> Option<Value> {
    let text = karmayoga_core::prompt::strip_reasoning(text);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let value: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let object = value.as_object()?;
    let required = schema.get("required").and_then(Value::as_array);
    let has_all = required
        .is_none_or(|keys| keys.iter().filter_map(Value::as_str).all(|k| object.contains_key(k)));
    has_all.then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_written_as_text_is_taken_when_it_has_what_is_required() {
        let schema = json!({ "type": "object", "required": ["title"] });
        let said = "<think>let me see</think>Sure! {\"title\": \"Dentist\"} hope that helps";
        assert_eq!(json_in_text(said, &schema), Some(json!({ "title": "Dentist" })));
        assert_eq!(json_in_text("{\"other\": 1}", &schema), None, "a required key is missing");
        assert_eq!(json_in_text("no json here", &schema), None);
        assert_eq!(json_in_text("} backwards {", &schema), None);
        let open = json!({ "type": "object" });
        assert_eq!(json_in_text("{\"a\": 1}", &open), Some(json!({ "a": 1 })));
    }

    #[test]
    fn a_model_told_to_think_longer_is_given_longer() {
        let at = |effort| LLMModelConfig { reasoning_effort: effort, ..LLMModelConfig::quick() };
        assert_eq!(timeout_for(&at(None)), TIMEOUT);
        assert_eq!(timeout_for(&at(Some(ReasoningEffort::Low))), TIMEOUT, "what Small may be");
        let mut last = TIMEOUT;
        for effort in [ReasoningEffort::Medium, ReasoningEffort::High, ReasoningEffort::Max] {
            let t = timeout_for(&at(Some(effort)));
            assert!(t > last, "{effort:?} gets longer than the level below it");
            last = t;
        }
    }
}
