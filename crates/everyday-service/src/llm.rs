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

use everyday_core::agent::{LLMModelConfig, LLMProviderConfig, Provider};
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

/// Apply a model's temperature and token ceiling to a builder.
///
/// Generic over rig's tool-registration typestate so this can run whichever
/// side of tool registration a caller is on; both callers today run it
/// before, which is why the builder's typestate note travels with them
/// rather than with this. `None` on either field leaves the provider's own
/// default alone rather than forcing one.
pub fn configure<T>(mut builder: AgentBuilder<T>, model: &LLMModelConfig) -> AgentBuilder<T> {
    if let Some(temperature) = model.temperature {
        builder = builder.temperature(temperature);
    }
    if let Some(max_tokens) = model.max_tokens {
        builder = builder.max_tokens(u64::from(max_tokens));
    }
    builder
}

/// Turn a provider's error into something worth showing a person.
///
/// The raw text is a transport error or a JSON body, and the three failures
/// that actually happen -- a wrong key, a wrong model name, nothing
/// listening -- all have an answer the person can act on. Shared by the
/// assistant and the quick model, which reach the same providers and fail
/// the same ways.
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
