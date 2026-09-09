//! The assistant: what it is configured to be, what it has said, and what it
//! has been asked to remember.
//!
//! This module is the sixth domain, and the first one that reaches outward
//! for *reasoning* rather than for content. It is built on the same principle
//! as [`ics`](crate::ics) and [`websearch`](crate::websearch): this crate
//! decides everything about what the assistant is, what it may do and what it
//! is told, and something above it owns the socket. The whole of it is
//! testable offline, and there is no model provider anywhere in this file.
//!
//! # What a conversation is made of
//!
//! [`Conversation`] is a titled thread; [`Message`] is one turn in it. A turn
//! is not always prose — a model that decides to add a task emits a
//! [`ToolCall`], the application runs it, and the result comes back as a
//! further message with [`Role::Tool`]. Storing all four roles rather than
//! only the two a person sees is what makes a reopened conversation resumable
//! instead of merely readable: the model needs its own tool calls back, in
//! order, or it re-runs them.
//!
//! # Why the API key is not in [`AgentSettings`]
//!
//! Settings cross the bridge to the interface every time the settings pane
//! opens, and are handed to the interface as a whole struct. A credential in
//! that struct would be a credential in the webview's memory, in every
//! serialised copy of it, and in any log line that ever debug-printed it. So
//! the key is stored on its own — see
//! [`AgentStore::put_secret`](crate::store::agent::AgentStore::put_secret) —
//! and the only thing settings carry about it is
//! [`AgentSettings::has_key`], a boolean the pane needs in order to draw
//! "configured" instead of an empty box.
//!
//! # Memory, and why it is a list of sentences
//!
//! [`Memory`] is a fact the assistant was told to keep: that you plan on
//! Sundays, that "the deck" means one particular project, that you want
//! tasks phrased as verbs. They are loaded into the system prompt in full at
//! the start of every conversation, which is the entire reason they are
//! short sentences and capped in number rather than an embedded vector
//! store. A few dozen lines of text costs a few hundred tokens and no
//! infrastructure; the alternative is an embedding model, a vector index and
//! a similarity threshold, all to search a corpus that fits on one screen.
//!
//! That ceiling is deliberate and enforced ([`MAX_MEMORIES`]). An assistant
//! that may remember without limit eventually spends its whole context
//! recalling, and no one ever prunes it.

pub mod tools;

use crate::error::{Error, Result};
use crate::id::{ConversationId, MemoryId, MessageId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Which family of API the model is spoken to over.
///
/// An enum with one variant, and not a mistake. Every place that needs to
/// know a provider takes this type, so adding Anthropic later is a variant
/// and the compiler's list of what to fix — rather than a string comparison
/// somebody forgets in the fourth of five places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    /// OpenAI's chat completions API, or anything that imitates it. See
    /// [`ModelConfig::base_url`] — this variant covers Azure, OpenRouter,
    /// vLLM, Ollama and LM Studio, because they all answer the same shapes.
    #[default]
    OpenAi,
}

impl Provider {
    pub fn slug(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
        }
    }

    /// Where requests go when [`ModelConfig::base_url`] is unset.
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
        }
    }

    /// Whether a key is required to talk to `base_url`.
    ///
    /// False for a loopback address, which is how a local model is reached.
    /// Ollama and LM Studio accept — and ignore — any key, so demanding one
    /// before the settings pane will save would be a made-up requirement
    /// that stops the most private configuration this app offers from being
    /// the easiest to set up.
    pub fn needs_key(&self, base_url: Option<&str>) -> bool {
        match base_url {
            Some(url) => !is_loopback(url),
            None => true,
        }
    }
}

/// Whether a URL points at this machine.
///
/// Textual rather than resolved on purpose: this decides whether to *insist*
/// on a credential, and a DNS lookup that says "localhost" resolves
/// somewhere else is not a reason to refuse to save a setting. The check
/// that actually protects anything is the one at request time, in the shell.
fn is_loopback(url: &str) -> bool {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    // Credentials in the URL are rare but legal, and `user@host` must not be
    // read as a host called `user@host`.
    let host = authority.rsplit('@').next().unwrap_or("");
    // An IPv6 literal keeps its brackets and its colons; anything else is
    // split from its port on the last colon it has.
    let host = if host.starts_with('[') {
        host.split_once(']').map(|(h, _)| h.trim_start_matches('[')).unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or("")
    };
    // Host names are case-insensitive, and `LocalHost` is a thing people
    // type. Getting this wrong does not merely fail to recognise a local
    // model -- it refuses to save the setting at all, because an http://
    // endpoint that is not this machine is rejected as unencrypted.
    let host = host.to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "0.0.0.0" | "::1")
        // The whole 127.0.0.0/8 block, which is where a second local model
        // ends up when the first one already has 127.0.0.1.
        || host.strip_prefix("127.").is_some_and(|rest| {
            let octets = rest.split('.');
            octets.clone().count() == 3
                && octets.into_iter().all(|o| !o.is_empty() && o.parse::<u8>().is_ok())
        })
}

/// Which model, spoken to how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub provider: Provider,
    /// The model name as the endpoint spells it: `gpt-5.1`, `qwen3:32b`,
    /// whatever the gateway in the middle calls it. Free text rather than an
    /// enum, because the list changes weekly and a stale enum is how an app
    /// stops being able to reach the current model.
    pub model: String,
    /// Overrides [`Provider::default_base_url`]. The whole reason this app
    /// can be pointed at a model running on the same machine.
    ///
    /// Stored with no trailing slash; see [`ModelConfig::endpoint`].
    pub base_url: Option<String>,
    /// `None` leaves it to the endpoint, which is the right default: some
    /// models reject the parameter outright and others have a sensible one.
    pub temperature: Option<f64>,
    /// Cap on a single reply. `None` means the endpoint's own limit.
    pub max_tokens: Option<u32>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: Provider::OpenAi,
            model: DEFAULT_MODEL.into(),
            base_url: None,
            temperature: None,
            max_tokens: None,
        }
    }
}

/// What a fresh install proposes. Chosen because tool calling is the whole
/// feature and this is the cheapest model that does it reliably.
pub const DEFAULT_MODEL: &str = "gpt-5.1-mini";

impl ModelConfig {
    /// The base URL to actually use, trailing slash removed.
    ///
    /// The normalisation matters because a pasted URL routinely ends in one
    /// and the provider's own path is joined onto it — `…/v1//chat` is a 404
    /// at some gateways and silently fine at others, which is the worst kind
    /// of difference to debug.
    pub fn endpoint(&self) -> &str {
        self.base_url
            .as_deref()
            .map(|u| u.trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| self.provider.default_base_url())
    }

    /// Whether this configuration needs an API key to be usable.
    pub fn needs_key(&self) -> bool {
        self.provider.needs_key(self.base_url.as_deref())
    }

    /// Reject a configuration the request layer could not act on.
    ///
    /// Checked here rather than at the first message because the settings
    /// pane is where a person can still fix it. An empty model name reaches
    /// the endpoint as a 400 whose text names a field they never saw.
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty() {
            return Err(Error::Invalid("a model name is required".into()));
        }
        if let Some(url) = &self.base_url
            && !url.trim().is_empty()
        {
            let url = url.trim();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(Error::Invalid(
                    "the base URL must start with http:// or https://".into(),
                ));
            }
            // Plain HTTP carries the key in the clear. Fine to this machine,
            // where there is no network to read it off, and not fine to
            // anywhere else -- and the mistake it catches is a real one:
            // pasting a company gateway's address without its scheme and
            // having something helpfully prepend the wrong one.
            if url.starts_with("http://") && !is_loopback(url) {
                return Err(Error::Invalid(
                    "an http:// endpoint would send your API key unencrypted; \
                     use https:// unless the model runs on this machine"
                        .into(),
                ));
            }
        }
        if let Some(t) = self.temperature
            && !(0.0..=2.0).contains(&t)
        {
            return Err(Error::Invalid("temperature must be between 0 and 2".into()));
        }
        Ok(())
    }
}

/// Everything the assistant is configured to be.
///
/// Crosses to the interface whole, which is why it holds no credential. See
/// the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    /// Off until somebody turns it on, and off is the state a new vault is
    /// in. This is the only feature in the application that sends what you
    /// wrote to a computer you do not own, so it does not begin switched on
    /// and it does not begin switched on quietly.
    pub enabled: bool,
    pub model: ModelConfig,
    /// The person's own instructions: personality, preferences, house style,
    /// anything they want true of every reply. Prepended to the assistant's
    /// own operating instructions rather than replacing them — see
    /// [`system_prompt`].
    pub instructions: String,
    /// Whether a destructive tool call stops and asks first.
    ///
    /// Defaults on, and the default worth defending: this application has no
    /// undo stack, so an agent that deletes a project on a misreading has
    /// destroyed something with no way back. Creates and edits apply
    /// directly because they are recoverable by hand; deletes and bulk
    /// updates are not.
    pub confirm_destructive: bool,
    /// How many model turns one request may take before the loop gives up.
    ///
    /// A ceiling rather than a target. Its job is to bound a model that has
    /// started calling the same tool forever — it costs money per turn, and
    /// the person watching it has no other way to stop it.
    pub max_steps: u32,
    /// Whether the assistant may write [`Memory`] rows.
    pub remember: bool,
    /// The person's own time zone, as an IANA name. `None` means the
    /// machine's.
    ///
    /// Here rather than read from the host, because the host may not be where
    /// the person is. A vault served from a machine under a desk, or from a
    /// container, has whatever zone that machine was installed with -- and
    /// "seven in the morning" for a routine, or "is it too late to ring them"
    /// in a conversation, has to mean seven where the *person* is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Whether a key is stored. Never the key itself.
    #[serde(default)]
    pub has_key: bool,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: ModelConfig::default(),
            instructions: String::new(),
            confirm_destructive: true,
            max_steps: DEFAULT_MAX_STEPS,
            remember: true,
            timezone: None,
            has_key: false,
        }
    }
}

/// Turns one request may take. Enough for a real multi-step task — read the
/// projects, read their tasks, write a plan — and short of a loop.
pub const DEFAULT_MAX_STEPS: u32 = 24;

/// Hard ceiling on [`AgentSettings::max_steps`], whatever the setting says.
pub const MAX_STEPS_LIMIT: u32 = 100;

/// Longest a person's own instructions may be.
///
/// Generous — several pages — and finite, because this string is sent with
/// every single request. Someone who pastes a novel into it pays for that
/// novel on every turn of every conversation, and would have no way to see
/// why the bill grew.
pub const MAX_INSTRUCTIONS_BYTES: usize = 8_000;

impl AgentSettings {
    pub fn validate(&self) -> Result<()> {
        self.model.validate()?;
        if self.instructions.len() > MAX_INSTRUCTIONS_BYTES {
            return Err(Error::Invalid(format!(
                "instructions are {} bytes; the limit is {MAX_INSTRUCTIONS_BYTES}",
                self.instructions.len()
            )));
        }
        if self.max_steps == 0 || self.max_steps > MAX_STEPS_LIMIT {
            return Err(Error::Invalid(format!(
                "steps per request must be between 1 and {MAX_STEPS_LIMIT}"
            )));
        }
        // Checked against the platform's own database rather than a pattern.
        // A zone this machine cannot resolve is one every routine on it would
        // silently fall back to UTC for, which is a scheduler that runs at
        // the wrong hour and never says why.
        if let Some(tz) = &self.timezone
            && jiff::tz::TimeZone::get(tz).is_err()
        {
            return Err(Error::Invalid(format!("{tz:?} is not a time zone this machine knows")));
        }
        Ok(())
    }

    /// The zone to reckon in: the person's, else the machine's.
    pub fn zone(&self) -> jiff::tz::TimeZone {
        match &self.timezone {
            Some(name) => jiff::tz::TimeZone::get(name).unwrap_or_else(|_| {
                // Validation refuses an unknown zone on the way in, so this
                // is a vault whose zone was valid on the machine that wrote
                // it and is not on this one. UTC and a log line, rather than
                // refusing to run.
                tracing::warn!(zone = %name, "unknown time zone; reckoning in UTC");
                jiff::tz::TimeZone::UTC
            }),
            None => jiff::tz::TimeZone::system(),
        }
    }

    /// Now, in that zone.
    pub fn now(&self) -> jiff::Zoned {
        jiff::Timestamp::now().to_zoned(self.zone())
    }

    /// Whether a request could actually be made right now.
    ///
    /// What the interface reads to decide between an input box and a link to
    /// settings. Enabled but keyless is the common half-configured state.
    pub fn is_usable(&self) -> bool {
        self.enabled && self.model.validate().is_ok() && (self.has_key || !self.model.needs_key())
    }
}

/// Who said something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The person.
    User,
    /// The model, whether it produced prose, tool calls or both.
    Assistant,
    /// The result of running one of the assistant's tool calls. Paired to
    /// the call by [`Message::tool_call_id`].
    Tool,
    /// A note the application inserted into the thread — "the vault was
    /// locked", "this conversation was resumed". Shown differently and never
    /// attributed to either party.
    System,
}

/// One tool the model asked to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// The provider's id for this call. Opaque, and echoed back verbatim on
    /// the [`Role::Tool`] message that answers it — the model matches
    /// results to calls by this string and by nothing else.
    pub id: String,
    pub name: String,
    /// Arguments as the model produced them. Stored as parsed JSON rather
    /// than the raw string so that a replayed conversation is not at the
    /// mercy of the provider's whitespace.
    pub arguments: serde_json::Value,
}

/// One turn in a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub role: Role,
    /// The prose. Empty on an assistant turn that only called tools, which
    /// is normal and must round-trip as empty rather than as absent.
    pub content: String,
    /// Tool calls this turn asked for. Only ever non-empty on
    /// [`Role::Assistant`].
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Which call this message answers. Only ever set on [`Role::Tool`].
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Set on a [`Role::Tool`] message whose tool failed.
    ///
    /// The failure is still sent to the model — that is the point, it is how
    /// the assistant learns the id it guessed does not exist and tries
    /// something else — but the interface needs to draw it as a failure
    /// rather than as a result, and "did the string look like an error" is
    /// not a test worth writing.
    #[serde(default)]
    pub failed: bool,
    pub created_at: Timestamp,
}

impl Message {
    pub fn user(conversation_id: ConversationId, content: impl Into<String>) -> Self {
        Self::new(conversation_id, Role::User, content)
    }

    pub fn assistant(conversation_id: ConversationId, content: impl Into<String>) -> Self {
        Self::new(conversation_id, Role::Assistant, content)
    }

    pub fn new(conversation_id: ConversationId, role: Role, content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            conversation_id,
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            failed: false,
            created_at: Timestamp::now(),
        }
    }

    /// The result of running `call`, successful or not.
    pub fn tool_result(
        conversation_id: ConversationId,
        call: &ToolCall,
        outcome: std::result::Result<String, String>,
    ) -> Self {
        let (content, failed) = match outcome {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Self {
            tool_call_id: Some(call.id.clone()),
            failed,
            ..Self::new(conversation_id, Role::Tool, content)
        }
    }

    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }
}

/// A titled thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: ConversationId,
    /// Drawn in the history list. Starts empty and is filled from the first
    /// user message rather than by asking the model to name it — a title is
    /// not worth a second round trip, and the first thing someone typed is a
    /// better label than a model's summary of it anyway.
    pub title: String,
    pub created_at: Timestamp,
    /// Bumped on every message, so the history list sorts by recency of use
    /// rather than of creation.
    pub updated_at: Timestamp,
}

impl Conversation {
    pub fn new() -> Self {
        let now = Timestamp::now();
        Self { id: ConversationId::new(), title: String::new(), created_at: now, updated_at: now }
    }

    /// Longest a derived title may be.
    pub const MAX_TITLE_CHARS: usize = 60;

    /// Name an untitled conversation after its opening message.
    ///
    /// Cut on a word boundary where there is one within reach of the limit,
    /// because a title severed mid-word reads as a bug rather than as an
    /// abbreviation. Newlines collapse first: a pasted paragraph would
    /// otherwise make a title with a line break in it.
    pub fn title_from(text: &str) -> String {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() <= Self::MAX_TITLE_CHARS {
            return flat;
        }
        let cut: String = flat.chars().take(Self::MAX_TITLE_CHARS).collect();
        let trimmed = match cut.rsplit_once(' ') {
            // Only honour the word boundary if it is not so far back that
            // the title loses most of its content.
            Some((head, _)) if head.chars().count() >= Self::MAX_TITLE_CHARS / 2 => head,
            _ => cut.trim_end(),
        };
        format!("{}\u{2026}", trimmed.trim_end())
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

/// A fact the assistant keeps between conversations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: MemoryId,
    /// One fact, as a sentence.
    pub text: String,
    /// Which conversation produced it, if it is still around. Kept so the
    /// memory list can answer "why does it think that", which is the first
    /// question anyone asks of a remembered fact they disagree with.
    #[serde(default)]
    pub source_id: Option<ConversationId>,
    /// Set when a person wrote or edited the memory by hand.
    ///
    /// Protects it from the assistant's own housekeeping: an agent trimming
    /// its memories to fit [`MAX_MEMORIES`] must not drop the line somebody
    /// typed themselves.
    #[serde(default)]
    pub pinned: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// How many memories are kept, and how many are loaded into a prompt.
///
/// See the module docs for why this is small and fixed.
pub const MAX_MEMORIES: usize = 64;

/// Longest a single memory may be. A sentence, not a document — anything
/// longer belongs in an entry, which the assistant can also write.
pub const MAX_MEMORY_CHARS: usize = 400;

impl Memory {
    pub fn new(text: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: MemoryId::new(),
            text: text.into(),
            source_id: None,
            pinned: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn from_conversation(text: impl Into<String>, source: ConversationId) -> Self {
        Self { source_id: Some(source), ..Self::new(text) }
    }

    pub fn validate(&self) -> Result<()> {
        if self.text.trim().is_empty() {
            return Err(Error::Invalid("a memory cannot be empty".into()));
        }
        if self.text.chars().count() > MAX_MEMORY_CHARS {
            return Err(Error::Invalid(format!(
                "a memory must be under {MAX_MEMORY_CHARS} characters"
            )));
        }
        Ok(())
    }
}

/// The assistant's own operating instructions, with the person's prepended.
///
/// Order matters and is the opposite of the obvious one. The person's
/// instructions come *first* so that a model which weights early context
/// heavily reads "be terse, I am a nurse, plan on Sundays" before it reads
/// the house rules — and the house rules come second so that the rules about
/// destructive actions are the most recent thing in the prompt. Neither
/// order is a security boundary: an instruction that the assistant must not
/// delete things is enforced by [`AgentSettings::confirm_destructive`] and
/// by the tool layer, not by asking politely here.
///
/// `now` is passed in rather than read from the clock because a prompt builder
/// that knows what time it is cannot be tested, and "what is due this week" is
/// exactly the question that goes wrong when the model assumes its training
/// cutoff is today. It is a [`Zoned`] rather than a date because the *hour*
/// matters to half of what an assistant is asked -- "what is left today", "is
/// it too late to ring them" -- and because a service running in a container
/// under a desk has the wrong zone, so the zone has to travel with the
/// instant rather than being read from the host.
pub fn system_prompt(
    settings: &AgentSettings,
    profile: &crate::profile::Profile,
    memories: &[Memory],
    now: &jiff::Zoned,
    context: Option<&str>,
) -> String {
    let mut out = String::new();

    if !settings.instructions.trim().is_empty() {
        out.push_str(settings.instructions.trim());
        out.push_str("\n\n---\n\n");
    }

    out.push_str(
        "You are the assistant built into Every Day, a private journal, task \
         manager, calendar, library and habit tracker. You are talking to its \
         owner about their own data.\n\n\
         Use your tools rather than guessing. Ids are UUIDs and you will not \
         invent a working one: list or search for a record before you act on \
         it. When you have changed something, say plainly what you changed \
         and give the title, not the id.\n\n\
         Prefer doing the work to describing how it could be done. If a \
         request is genuinely ambiguous in a way that changes what you would \
         write, ask; otherwise pick the sensible reading, act, and say which \
         reading you took.\n\n\
         This is someone's journal. Do not moralise about what you read in \
         it, and do not summarise it back to them unless they asked.",
    );

    // Who, then when. Both before the memories, because a memory is a
    // standing instruction and reads better against a person who has already
    // been introduced.
    if let Some(said) = profile.describe(now.date()) {
        out.push_str("\n\n");
        out.push_str(&said);
    }

    out.push_str(&format!(
        "\n\nIt is {}, {} in {}.",
        now.strftime("%A %-d %B %Y"),
        now.strftime("%H:%M"),
        now.time_zone().iana_name().unwrap_or("an unknown time zone"),
    ));

    if !memories.is_empty() {
        out.push_str(
            "\n\nThings you have been asked to remember, most recent last. \
             Treat them as standing instructions:\n",
        );
        // The newest, not the first. `memories` arrives oldest-first -- the
        // order they are read in, so that a later instruction wins -- and the
        // list can exceed the cap even though the vault trims on write,
        // because pinned memories are never evicted. Taking from the front
        // would drop the newest facts, including the one just remembered,
        // which is the single most surprising thing an assistant's memory
        // could do.
        let skip = memories.len().saturating_sub(MAX_MEMORIES);
        for m in memories.iter().skip(skip) {
            out.push_str("- ");
            out.push_str(m.text.trim());
            out.push('\n');
        }
    }

    // What the person is looking at. Appended last, and labelled, because it
    // is the one part of the prompt that changes between two otherwise
    // identical turns -- and because "this" and "here" in a question mean
    // this, and the model has no other way to know it.
    if let Some(context) = context.map(str::trim).filter(|c| !c.is_empty()) {
        out.push_str("\n\nThe person is currently looking at: ");
        out.push_str(context);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn a_new_vault_has_the_assistant_switched_off() {
        let s = AgentSettings::default();
        assert!(!s.enabled, "the assistant must not be on before anyone asks for it");
        assert!(!s.is_usable());
        assert!(s.confirm_destructive, "destructive calls must confirm by default");
    }

    #[test]
    fn the_endpoint_defaults_to_the_provider_and_loses_a_trailing_slash() {
        let mut cfg = ModelConfig::default();
        assert_eq!(cfg.endpoint(), "https://api.openai.com/v1");

        cfg.base_url = Some("http://localhost:11434/v1/".into());
        assert_eq!(cfg.endpoint(), "http://localhost:11434/v1");

        // An empty override is not an override.
        cfg.base_url = Some(String::new());
        assert_eq!(cfg.endpoint(), "https://api.openai.com/v1");
    }

    #[test]
    fn a_local_model_needs_no_api_key() {
        let mut cfg = ModelConfig::default();
        assert!(cfg.needs_key(), "a remote endpoint needs a credential");

        for url in ["http://localhost:11434/v1", "http://127.0.0.1:1234/v1", "http://[::1]:8080"] {
            cfg.base_url = Some(url.into());
            assert!(!cfg.needs_key(), "{url} runs on this machine");
        }

        cfg.base_url = Some("https://openrouter.ai/api/v1".into());
        assert!(cfg.needs_key(), "someone else's gateway still needs a key");
    }

    #[test]
    fn plain_http_is_refused_anywhere_but_this_machine() {
        let mut cfg = ModelConfig {
            base_url: Some("http://localhost:11434/v1".into()),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok(), "a local model over http is fine");

        cfg.base_url = Some("http://gateway.example.com/v1".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("unencrypted"), "should explain the risk, got: {err}");

        cfg.base_url = Some("gateway.example.com/v1".into());
        assert!(cfg.validate().is_err(), "a URL with no scheme is not a URL");
    }

    #[test]
    fn an_enabled_assistant_without_a_key_is_not_yet_usable() {
        let mut s = AgentSettings { enabled: true, ..Default::default() };
        assert!(!s.is_usable(), "half-configured is not configured");

        s.has_key = true;
        assert!(s.is_usable());

        // ...unless the model is on this machine, where there is no key to
        // have and the feature must still work.
        let mut local = AgentSettings { enabled: true, ..Default::default() };
        local.model.base_url = Some("http://localhost:11434/v1".into());
        assert!(local.is_usable(), "a local model should need no setup beyond its address");
    }

    #[test]
    fn settings_reject_a_step_ceiling_that_would_not_bound_anything() {
        let mut s = AgentSettings { max_steps: 0, ..Default::default() };
        assert!(s.validate().is_err(), "zero steps could never answer");
        s.max_steps = MAX_STEPS_LIMIT + 1;
        assert!(s.validate().is_err(), "a ceiling above the hard limit is not a ceiling");
        s.max_steps = 8;
        assert!(s.validate().is_ok());
    }

    #[test]
    fn instructions_are_capped_because_they_are_sent_every_turn() {
        let s = AgentSettings {
            instructions: "x".repeat(MAX_INSTRUCTIONS_BYTES + 1),
            ..Default::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn a_title_is_cut_on_a_word_boundary() {
        let short = "Plan my week";
        assert_eq!(Conversation::title_from(short), short);

        let long = "I would like you to go through every project I have open \
                    and tell me which ones have gone stale";
        let title = Conversation::title_from(long);
        assert!(title.chars().count() <= Conversation::MAX_TITLE_CHARS + 1);
        assert!(title.ends_with('\u{2026}'));
        assert!(!title.contains("  "));

        // A newline in the source must not reach the title.
        assert_eq!(Conversation::title_from("one\ntwo"), "one two");
    }

    #[test]
    fn a_title_from_one_enormous_word_still_gets_cut() {
        let title = Conversation::title_from(&"a".repeat(200));
        assert_eq!(title.chars().count(), Conversation::MAX_TITLE_CHARS + 1);
    }

    /// A fixed instant in a named zone, so the prompt tests are about the
    /// wording rather than about what time it happens to be.
    fn at(hour: i8, minute: i8, zone: &str) -> jiff::Zoned {
        date(2026, 9, 8)
            .at(hour, minute, 0, 0)
            .in_tz(zone)
            .expect("a real zone and a real local time")
    }

    #[test]
    fn the_prompt_carries_the_clock_because_the_model_does_not_know_it() {
        let prompt = system_prompt(
            &AgentSettings::default(),
            &crate::profile::Profile::default(),
            &[],
            &at(14, 5, "America/Los_Angeles"),
            None,
        );
        assert!(prompt.contains("8 September 2026"), "got: {prompt}");
        assert!(prompt.contains("14:05"), "the hour matters to half of what is asked: {prompt}");
        assert!(prompt.contains("America/Los_Angeles"), "and so does the zone: {prompt}");
    }

    #[test]
    fn the_prompt_introduces_the_person_and_says_nothing_when_it_cannot() {
        let none = system_prompt(
            &AgentSettings::default(),
            &crate::profile::Profile::default(),
            &[],
            &at(9, 0, "UTC"),
            None,
        );
        assert!(!none.contains("Its owner is"), "an empty profile adds nothing");

        let profile = crate::profile::Profile {
            first_name: "Hari".into(),
            born: Some(date(1985, 3, 14)),
            location: "Seattle".into(),
            ..Default::default()
        };
        let said = system_prompt(&AgentSettings::default(), &profile, &[], &at(9, 0, "UTC"), None);
        assert!(said.contains("Its owner is Hari, 41, in Seattle."), "got: {said}");
        assert!(
            said.find("Its owner is Hari").unwrap() < said.find("It is Tuesday").unwrap(),
            "who, then when"
        );
    }

    #[test]
    fn the_persons_instructions_come_before_the_house_rules() {
        let s =
            AgentSettings { instructions: "Be terse. I am a nurse.".into(), ..Default::default() };
        let prompt =
            system_prompt(&s, &crate::profile::Profile::default(), &[], &at(9, 0, "UTC"), None);
        let mine = prompt.find("I am a nurse").unwrap();
        let house = prompt.find("You are the assistant").unwrap();
        assert!(mine < house, "the person's own instructions should be read first");
    }

    #[test]
    fn a_local_host_is_recognised_however_it_was_typed() {
        // Not merely a missed optimisation: an unrecognised http:// host is
        // refused outright as unencrypted, so the setting will not save.
        for url in ["http://LocalHost:11434/v1", "http://LOCALHOST:1234", "http://[::1]:8080"] {
            let cfg = ModelConfig { base_url: Some(url.into()), ..Default::default() };
            assert!(!cfg.needs_key(), "{url} runs on this machine");
            assert!(cfg.validate().is_ok(), "{url} should be saveable");
        }
    }

    #[test]
    fn the_prompt_keeps_the_newest_memories_when_there_are_too_many() {
        // Pinned memories are never evicted, so the stored list can exceed
        // the cap. Trimming from the wrong end would silently drop the fact
        // that was just remembered.
        let memories: Vec<Memory> =
            (0..MAX_MEMORIES + 5).map(|i| Memory::new(format!("fact {i}"))).collect();
        let prompt = system_prompt(
            &AgentSettings::default(),
            &crate::profile::Profile::default(),
            &memories,
            &at(9, 0, "UTC"),
            None,
        );

        assert!(prompt.contains(&format!("fact {}", MAX_MEMORIES + 4)), "the newest must survive");
        assert!(!prompt.contains("- fact 0\n"), "the oldest is the one to drop");
        assert_eq!(
            prompt.matches("\n- fact ").count(),
            MAX_MEMORIES,
            "exactly the cap should reach the prompt"
        );
    }

    #[test]
    fn memories_reach_the_prompt_as_standing_instructions() {
        let memories = vec![Memory::new("Plans the week on Sunday evening")];
        let prompt = system_prompt(
            &AgentSettings::default(),
            &crate::profile::Profile::default(),
            &memories,
            &at(9, 0, "UTC"),
            None,
        );
        assert!(prompt.contains("Plans the week on Sunday evening"));
    }

    #[test]
    fn blank_context_and_instructions_add_no_empty_sections() {
        let s = AgentSettings { instructions: "   \n ".into(), ..Default::default() };
        let prompt = system_prompt(
            &s,
            &crate::profile::Profile::default(),
            &[],
            &at(9, 0, "UTC"),
            Some("  "),
        );
        assert!(!prompt.contains("---"), "whitespace is not instructions");
        assert!(!prompt.contains("currently looking at"), "whitespace is not context");
        assert!(prompt.starts_with("You are the assistant"));
    }

    #[test]
    fn a_memory_must_be_a_sentence_rather_than_a_document() {
        assert!(Memory::new("  ").validate().is_err());
        assert!(Memory::new("x".repeat(MAX_MEMORY_CHARS + 1)).validate().is_err());
        assert!(Memory::new("Calls the deck project 'the deck'").validate().is_ok());
    }

    #[test]
    fn a_tool_result_carries_the_id_of_the_call_it_answers() {
        let cid = ConversationId::new();
        let call = ToolCall {
            id: "call_abc".into(),
            name: "add_task".into(),
            arguments: serde_json::json!({ "title": "Ring the vet" }),
        };
        let ok = Message::tool_result(cid, &call, Ok("added".into()));
        assert_eq!(ok.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(ok.role, Role::Tool);
        assert!(!ok.failed);

        let bad = Message::tool_result(cid, &call, Err("no such project".into()));
        assert!(bad.failed, "a failure must be drawable as one without parsing its prose");
        assert_eq!(bad.content, "no such project");
    }
}
