use std::sync::Arc;
use std::time::Duration;

use everyday_core::meeting::Track;
use everyday_core::{RecordingId, Vault};
use rig_agent::AgentBuilder;
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::prelude::*;

use crate::error::{CommandError, CommandResult, codes};

// ============================================================================
// The AudioSource seam
// ============================================================================

/// What the pipeline needs from the spool: the samples of one sealed chunk,
/// and permission to erase every chunk of one recording once its note
/// exists. Sync rather than `async` -- both are, in the production
/// implementation, a decrypt off disk, not a network call -- so a caller on
/// the async runtime is the one responsible for running it inside
/// [`blocking`], the same discipline [`crate::meeting::speech`] asks of its
/// own callers.
///
/// Two implementations: [`SpoolSource`], production, a thin adapter over
/// `meeting::spool`; and `FakeAudio`, in [`tests`], an in-memory map used by
/// every test in this file. Neither the transcribing nor the identifying
/// stage below ever names `meeting::spool` directly -- only this trait.
pub trait AudioSource: Send + Sync {
    fn read_chunk(&self, id: RecordingId, track: Track, seq: u32) -> CommandResult<Vec<i16>>;
    fn remove_audio(&self, id: RecordingId) -> CommandResult<()>;
}

/// The production [`AudioSource`]: a thin adapter over `meeting::spool`,
/// which owns the sealed chunk files on disk -- see that module's own doc.
pub struct SpoolSource {
    vault: Arc<Vault>,
}

impl SpoolSource {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }
}

impl AudioSource for SpoolSource {
    fn read_chunk(&self, id: RecordingId, track: Track, seq: u32) -> CommandResult<Vec<i16>> {
        crate::meeting::spool::read_chunk(&self.vault, id, track, seq)
    }

    fn remove_audio(&self, id: RecordingId) -> CommandResult<()> {
        crate::meeting::spool::remove_audio(&self.vault, id)
    }
}

// ============================================================================
// The Summariser seam
// ============================================================================

/// One call to the assistant's model: a system prompt and a user message in,
/// prose out. What [`summarise_stage`] is built against, so a test never
/// needs a network or an API key -- see [`tests::FakeSummariser`].
///
/// Not `async_trait`: this crate has no dependency on it, and the same
/// hand-written `BoxFuture` shape [`crate::meeting::transcribe::Transcriber`]
/// already uses is one pattern fewer to learn.
pub trait Summariser: Send + Sync {
    fn ask<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<String>>;
}

/// How long one summarising request may run before it is abandoned. Three
/// minutes, per the plan: a call transcript is a long document and the
/// assistant's own model may be a slow one somebody chose on purpose, but a
/// note that has not arrived by then has stopped being useful to wait for.
const SUMMARISE_TIMEOUT: Duration = Duration::from_secs(180);

/// The real [`Summariser`]: the assistant's own provider and model --
/// [`everyday_core::agent::AgentSettings::assistant_model`], never the quick
/// model, per the plan's "Summaries use the assistant's model, not the quick
/// model". No tools are registered: a summary is one piece of the transcript
/// in and one piece of prose out, not a turn that should be reading the
/// vault.
pub struct AssistantSummariser {
    vault: Arc<Vault>,
}

impl AssistantSummariser {
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }

    async fn attempt(&self, system: &str, user: &str) -> CommandResult<String> {
        let (settings, key) = self.vault.agent_credentials().map_err(|e| {
            CommandError::new(codes::AGENT, format!("the assistant is not usable: {e}"))
        })?;
        let model = settings.assistant_model.clone();
        let client = crate::llm::client(&settings.provider_config, key).map_err(|e| {
            CommandError::new(codes::AGENT, format!("could not start the assistant: {e}"))
        })?;
        let builder = AgentBuilder::new(client.completion_model(&model.model)).preamble(system);
        let builder = crate::llm::configure(builder, &model);
        let agent = builder.build();

        match tokio::time::timeout(SUMMARISE_TIMEOUT, agent.prompt(user).max_turns(1)).await {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => {
                Err(CommandError::new(codes::AGENT, crate::llm::friendly(&e.to_string())))
            }
            Err(_) => Err(CommandError::new(
                codes::TIMED_OUT,
                format!(
                    "the assistant took longer than {}s to summarise",
                    SUMMARISE_TIMEOUT.as_secs()
                ),
            )),
        }
    }
}

/// Whether `message` looks like a transport failure rather than the model
/// itself refusing the request -- the one case the plan's "one retry on
/// network error" applies to. Mirrors the substrings
/// [`crate::llm::friendly`] already keys on for "could not reach the model",
/// rather than depending on it directly: `friendly` turns the error into a
/// sentence for a person, and this has to see the raw text first.
fn looks_like_network_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("connection refused")
        || lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("timed out")
        || lower.contains("timeout")
}

impl Summariser for AssistantSummariser {
    fn ask<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> crate::meeting::transcribe::BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move {
            match self.attempt(system, user).await {
                Ok(text) => Ok(text),
                Err(e) if looks_like_network_error(&e.message) => self.attempt(system, user).await,
                Err(e) => Err(e),
            }
        })
    }
}
