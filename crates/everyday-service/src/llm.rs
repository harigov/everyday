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

use everyday_core::llm::{
    LLMModelConfig, LLMProviderConfig, ModelAccess, Provider, ReasoningEffort,
};
use rig_agent::AgentBuilder;
use rig_agent::core::providers::openai;

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
pub fn client(
    conn: &LLMProviderConfig,
    key: Option<String>,
) -> Result<openai::CompletionsClient, String> {
    match conn.provider {
        // Chat Completions, not the Responses API. Rig's default OpenAI
        // client speaks the newer Responses API, but the whole point of the
        // base-URL override below is that Ollama, LM Studio, vLLM and
        // OpenRouter can be pointed at -- and what they all implement is
        // `/chat/completions`. Choosing the Responses API here would make the
        // setting that exists for local models work everywhere except local
        // models.
        Provider::OpenAi => openai::CompletionsClient::builder()
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
pub fn client_for(access: &ModelAccess) -> Result<openai::CompletionsClient, String> {
    client(&access.config.connection, access.key.clone())
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
