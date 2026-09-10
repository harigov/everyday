//! The quick model's harness: the half that owns a socket.
//!
//! The other half is [`everyday_core::quick`], which decides everything about
//! what a job *is* — its instruction, its schema, the prompt it builds and
//! the clamps on the answer — with no provider and no network in it. This
//! file opens the connection and nothing else. It never looks at a job's
//! name to decide what to do; it is handed a
//! [`Prompt`](everyday_core::quick::Prompt) and posts it.
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
//! values built at runtime from [`JOBS`](everyday_core::quick::JOBS), for the
//! same reason [`crate::agent`] registers dynamic tools rather than typed
//! ones: the catalogue is data, and a job added to it must not require a
//! match arm in this file. So this does by hand what `Extractor` does
//! internally — one tool called `submit` carrying the job's schema, tool
//! choice forced, and the arguments of the call *are* the answer.
//!
//! # What bounds a quick job
//!
//! Two turns and a short timeout, neither of them configurable. A quick job
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

use everyday_core::Vault;
// Aliased: `rig_agent::prelude` brings its own `Prompt`, and that one is the
// trait carrying `.prompt()`.
use everyday_core::quick::Prompt as QuickPrompt;
use rig_agent::AgentBuilder;
use rig_agent::agent::OutputMode;
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::core::message::ToolChoice;
use rig_agent::core::providers::openai;
use rig_agent::core::tool::{PortableDynamicTool, ToolOutput};
use rig_agent::prelude::*;
use serde_json::{Value, json};

use crate::error::{CommandError, CommandResult};

/// How long a quick job may take before it is abandoned.
///
/// Generous for a nano-tier model answering a small schema, and short enough
/// that a suggestion which has not arrived by now has stopped being a
/// suggestion. The clock covers the whole exchange, not one request.
pub const TIMEOUT: Duration = Duration::from_secs(12);

/// Turns a quick job may take.
///
/// Two: one to call `submit`, and one spare for a model that opens with a
/// sentence before doing as it was asked. Not a setting — see the module
/// docs.
const MAX_TURNS: usize = 2;

/// The name of the one tool a quick job is given.
const SUBMIT: &str = "submit";

/// Run one job and return the raw answer, for the core to parse and clamp.
///
/// The gate is [`Vault::quick_credentials`], which refuses a job the policy
/// has not allowed — so a caller cannot reach the endpoint by forgetting to
/// check, and there is exactly one place the check lives.
pub async fn run(vault: Arc<Vault>, prompt: QuickPrompt) -> CommandResult<Value> {
    let (settings, key) = vault
        .quick_credentials(&prompt.job)
        .map_err(|e| CommandError::new("quick", e.to_string()))?;
    let model = settings
        .quick_model
        .clone()
        .ok_or_else(|| CommandError::new("quick", "no quick model is configured"))?;

    let client = openai::CompletionsClient::builder()
        .base_url(settings.provider_config.endpoint())
        // A local endpoint needs no credential and is usually configured
        // without one; an empty bearer rather than no bearer, for the reason
        // `agent::build` gives.
        .api_key::<rig_agent::core::client::BearerAuth>(key.unwrap_or_default())
        .build()
        .map_err(|e| CommandError::new("quick", format!("could not reach the model: {e}")))?;

    // The answer arrives as the arguments of the tool call rather than as
    // prose, which is the whole point: there is no fenced block to find, no
    // preamble to strip and no chance of "Sure! Here's the JSON:".
    let answer: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let sink = answer.clone();
    let submit = PortableDynamicTool::new(
        SUBMIT,
        "Submit the answer. Call this exactly once.",
        prompt.schema.clone(),
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

    let mut builder = AgentBuilder::new(client.completion_model(&model.model))
        .preamble(&format!(
            "{}\n\nCall the `submit` function with your answer. Call it even when \
             the answer is empty — an empty answer is a real answer here, and \
             saying so is how the suggestion is correctly not shown.",
            prompt.system
        ))
        .default_max_turns(MAX_TURNS)
        .tool_choice(ToolChoice::Required)
        .output_mode(OutputMode::Tool);
    if let Some(t) = model.temperature {
        builder = builder.temperature(t);
    }
    if let Some(m) = model.max_tokens {
        builder = builder.max_tokens(u64::from(m));
    }
    // Registering the tool last: the builder is a typestate and this is the
    // move from "no tools" to "tools", so everything set by `if let` has to
    // happen while the type is still the first one.
    let agent = builder.portable_dynamic_tool(submit).build();

    // The timeout wraps the whole exchange rather than one request, because
    // the failure it exists for is a model that answers slowly twice.
    let turn = tokio::time::timeout(TIMEOUT, agent.prompt(prompt.user.as_str()).max_turns(MAX_TURNS));
    match turn.await {
        Ok(Ok(_)) => {}
        // A model that ran out of turns may still have called `submit` on the
        // way, so the answer is checked before the error is believed.
        Ok(Err(e)) => {
            if answer.lock().expect("not poisoned").is_none() {
                return Err(CommandError::new("quick", format!("the quick model failed: {e}")));
            }
        }
        Err(_) => {
            return Err(CommandError::new(
                "quick",
                format!("the quick model took longer than {}s", TIMEOUT.as_secs()),
            ));
        }
    }

    answer
        .lock()
        .expect("the quick answer slot is never poisoned")
        .take()
        .ok_or_else(|| CommandError::new("quick", "the quick model did not answer"))
}
