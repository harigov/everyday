//! The models this application asks, and where each of them is.
//!
//! Application configuration rather than the assistant's. There are three
//! sizes of work here and three models for them:
//!
//! - **Large** writes up a research job: a long, unattended turn that reads
//!   pages and leaves a note behind, where the answer is worth paying for.
//! - **Medium** is the assistant: conversations, routines, dreams, meeting
//!   notes, and the mail features that write prose.
//! - **Small** is every one-shot extraction: the quick asks in the capture
//!   boxes, sorting mail, suggesting replies.
//!
//! Each has its own endpoint and its own model name. That is the change from
//! the shape this replaced, where one connection was shared by an assistant
//! model and a quick one: somebody running a research model on one gateway,
//! the assistant on another and the extractions on a model under their own
//! desk is the configuration this exists for.
//!
//! # A key belongs to an endpoint, not to a tier
//!
//! A tier finds its key by where it points. Two tiers on the same endpoint
//! share one key without anybody pasting it twice. And a tier moved to a
//! different endpoint does not take the old key with it: pointing Large at
//! somebody else's gateway finds no key there, so an OpenAI key can never be
//! sent to a host it was not stored for. A key held per tier would follow the
//! tier to whatever address it was given next.
//!
//! The keys are one sealed map, endpoint to key, in a single row of the
//! [`SecretStore`](crate::store::secrets::SecretStore) -- see
//! [`MODEL_KEYS_OWNER_KIND`]. Not a row per endpoint: a secret's owner is in
//! the clear, and an endpoint can be a company's private gateway. A key no
//! tier points at any more is forgotten when the models are saved, so one
//! cannot linger out of sight and come back into use months later.
//!
//! # Why Large falls back and Small does not
//!
//! [`ModelSettings::resolve`] answers Large with Medium when Large is not
//! set: research on the assistant's model is the same work done more
//! cheaply, and nothing about it surprises a bill. Small has no fallback, for
//! the reason `docs/plans/quick-model.md` gives: the quick asks run in
//! capture boxes, several per interaction, and a blank field that silently
//! billed at the assistant's rates would be a bill nobody could account for.
//! The failure mode of "no small model" is that the suggestion does not
//! appear.
//!
//! # What a mail acknowledgement names
//!
//! An account's owner says once that its mail may go to a model provider --
//! see [`crate::account::Account::assistant_provider_acknowledged`]. With
//! three endpoints that sentence has to name all of them, because the mail
//! tools run on Medium, a research job's on Large and sorting on Small.
//! [`ModelSettings::acknowledgement_name`] is that list, and it is spelled so
//! that a vault whose tiers all share one endpoint -- every vault upgraded
//! from the single-connection shape -- names exactly what it named before,
//! and nobody's acknowledgement is lost to the upgrade.

use crate::error::{Error, Result};
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
    /// [`LLMProviderConfig::base_url`] — this variant covers Azure,
    /// OpenRouter, vLLM, Ollama and LM Studio, because they all answer the
    /// same shapes.
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

    /// Where requests go when [`LLMProviderConfig::base_url`] is unset.
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
pub fn is_loopback(url: &str) -> bool {
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

/// Where a model is: a wire protocol and an address.
///
/// Split from [`LLMModelConfig`] because the two change on different
/// occasions and for different reasons. Which *provider* you talk to changes
/// when you move house — a new endpoint, a new key, everything downstream
/// invalidated. Which *model* you ask for changes whenever somebody ships
/// one, which is most weeks, and invalidates nothing. It is also the half a
/// key is found by -- see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LLMProviderConfig {
    #[serde(default)]
    pub provider: Provider,
    /// Overrides [`Provider::default_base_url`]. The whole reason this app
    /// can be pointed at a model running on the same machine.
    ///
    /// Stored with no trailing slash; see [`LLMProviderConfig::endpoint`].
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Default for LLMProviderConfig {
    fn default() -> Self {
        Self { provider: Provider::OpenAi, base_url: None }
    }
}

impl LLMProviderConfig {
    /// The base URL to actually use, trailing slash removed.
    ///
    /// The normalisation matters because a pasted URL routinely ends in one
    /// and the provider's own path is joined onto it — `…/v1//chat` is a 404
    /// at some gateways and silently fine at others, which is the worst kind
    /// of difference to debug. It is also what a key is stored under, so the
    /// same address typed with and without its slash finds the same key.
    ///
    /// Spelt exactly as it was before there were tiers -- trailing slashes
    /// off and nothing else -- because an account's mail acknowledgement
    /// stores this string, and one given before the upgrade must still match
    /// after it. The pane trims what is typed before it is saved.
    pub fn endpoint(&self) -> &str {
        self.base_url
            .as_deref()
            .map(|u| u.trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| self.provider.default_base_url())
    }

    /// Whether this connection needs an API key to be usable.
    pub fn needs_key(&self) -> bool {
        self.provider.needs_key(self.base_url.as_deref())
    }

    /// Whether the model is on this machine.
    ///
    /// Read to decide what to say about what leaves the machine, and whether
    /// a meeting's transcript may be summarised without asking.
    pub fn is_local(&self) -> bool {
        self.base_url.as_deref().is_some_and(is_loopback)
    }

    /// Reject a connection the request layer could not act on.
    ///
    /// Checked here rather than at the first message because the settings
    /// pane is where a person can still fix it.
    pub fn validate(&self) -> Result<()> {
        if let Some(url) = &self.base_url
            && !url.trim().is_empty()
        {
            let url = url.trim();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(Error::Invalid(
                    "the base URL must start with http:// or https://".into(),
                ));
            }
            // A URL has no spaces in it, and the endpoints are joined with
            // ", " into what a mail acknowledgement names -- so one with a
            // space could be made to read as two.
            if url.contains(char::is_whitespace) {
                return Err(Error::Invalid("the base URL cannot contain spaces".into()));
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
        Ok(())
    }
}

/// Which model to ask, and how to ask it.
///
/// It carries nothing about *where* the model is, which is
/// [`LLMProviderConfig`]'s business.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LLMModelConfig {
    /// The model name as the endpoint spells it: `gpt-5.1`, `qwen3:32b`,
    /// whatever the gateway in the middle calls it. Free text rather than an
    /// enum, because the list changes weekly and a stale enum is how an app
    /// stops being able to reach the current model.
    pub model: String,
    /// `None` leaves it to the endpoint, which is the right default: some
    /// models reject the parameter outright and others have a sensible one.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Cap on a single reply. `None` means the endpoint's own limit.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// How long a reasoning model thinks before it answers. `None` sends
    /// nothing and leaves it to the endpoint -- see [`ReasoningEffort`] for
    /// why that is not the same as a middling setting.
    ///
    /// A level this build does not know reads as `None` rather than failing:
    /// see [`known_effort`].
    #[serde(default, deserialize_with = "known_effort")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Read a stored effort, taking one this build has no variant for as unset.
///
/// The effort lives inside the one sealed record that holds every tier, so a
/// level written by a newer build -- or another branch's, against the same
/// vault -- would otherwise fail the whole record: no model for any tier, and
/// no Save to fix it with, since a save reads the record first. Unset is what
/// the field meant before it existed, so it is the safe reading. The string
/// is parsed by the derived `Deserialize`, so the spelling still lives in one
/// place.
fn known_effort<'de, D>(d: D) -> std::result::Result<Option<ReasoningEffort>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::IntoDeserializer;
    let Some(raw) = Option::<String>::deserialize(d)? else { return Ok(None) };
    let parsed: std::result::Result<ReasoningEffort, serde::de::value::Error> =
        ReasoningEffort::deserialize(raw.as_str().into_deserializer());
    Ok(parsed.ok())
}

/// How hard a reasoning model is asked to think, as `reasoning_effort` on a
/// chat completions request.
///
/// Settable because the endpoint's own default is not a middle setting. It
/// is whatever the provider last decided: OpenAI's `gpt-5.1` defaults to
/// `none`, which is no reasoning at all, so an assistant on it that was
/// never told otherwise plans a multi-step job without thinking first --
/// the "lazy" it reads as. Nothing is sent unless somebody picks a level,
/// because a model that does not reason can refuse the field outright
/// (Ollama does, for a model without thinking), and a setting that worked
/// yesterday must not start failing because this one was added.
///
/// One spelling for every endpoint, and every value any of them takes:
/// OpenRouter reads the same top-level field as OpenAI and maps it onto
/// Anthropic's and Gemini's own budgets, and a level a given model does not
/// support comes back as a 400 that `everyday_service::llm::friendly_for`
/// turns into a sentence pointing at the setting. No per-model list: model
/// names are free text, and which levels a model takes changes with every
/// release. The one limit is Small's -- see [`SMALL_MAX_EFFORT`].
///
/// Ordered from least thinking to most, so a ceiling is a comparison. Spelt
/// on the wire and in the record by serde alone; the request is built by
/// serialising this, not from a second table of names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Do not reason at all. `gpt-5.1` and later; Gemini.
    None,
    /// The least that still reasons. `gpt-5` and its sizes.
    Minimal,
    Low,
    Medium,
    High,
    /// `gpt-5.1-codex-max`, `gpt-5.2` and later; OpenRouter.
    Xhigh,
    /// OpenRouter's top level, for the models that take one.
    Max,
}

/// The most thinking the Small model may be asked for.
///
/// Its asks are capped at [`QUICK_MAX_TOKENS`] and a few seconds, and on
/// OpenAI's reasoning models the thinking comes out of the same token cap
/// as the answer. Above Low, the model spends the budget or the clock
/// thinking and never calls `submit`, and what a person sees is a
/// suggestion that quietly stops appearing. Refused when the models are
/// saved, where it can still be changed, rather than discovered keystroke
/// by keystroke.
pub const SMALL_MAX_EFFORT: ReasoningEffort = ReasoningEffort::Low;

impl Default for LLMModelConfig {
    fn default() -> Self {
        Self::assistant()
    }
}

/// What a fresh install proposes for the assistant -- the medium model.
/// Chosen because tool calling is the whole feature and this is the cheapest
/// model that does it reliably.
pub const DEFAULT_MODEL: &str = "gpt-5.1-mini";

/// What the large model starts as when somebody switches it on: the same
/// family one tier up, because a research job is the assistant's own loop
/// run for longer, and wants the same tools called better.
pub const DEFAULT_LARGE_MODEL: &str = "gpt-5.1";

/// What the small model starts as. Chosen on the same grounds one tier down:
/// the cheapest thing at this endpoint that reliably returns the schema it
/// was asked for.
pub const DEFAULT_QUICK_MODEL: &str = "gpt-5.1-nano";

/// Cap on a quick ask's reply.
///
/// These return a small object with a handful of fields, so the ceiling is
/// there to stop a model that has decided to write an essay from being paid
/// for it. A quick ask that needs more than this is not a quick ask.
pub const QUICK_MAX_TOKENS: u32 = 1_024;

impl LLMModelConfig {
    /// The assistant's default: the endpoint's own temperature, its own
    /// reply limit, and a model that can call tools.
    pub fn assistant() -> Self {
        Self {
            model: DEFAULT_MODEL.into(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
        }
    }

    /// The large model's default. The endpoint's own temperature and reply
    /// limit, as the assistant's: a write-up is prose, not an extraction.
    pub fn large() -> Self {
        Self {
            model: DEFAULT_LARGE_MODEL.into(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
        }
    }

    /// The small model's default.
    ///
    /// Temperature nought, and not a hidden field. Every quick ask is an
    /// extraction into a schema somebody else declared, and there is no
    /// reading of "pull the author out of this paragraph" under which a
    /// person wants more variety in the answer. Leaving it visible is also
    /// how somebody pointing this at a local model can turn it back up when
    /// their model refuses the parameter.
    pub fn quick() -> Self {
        Self {
            model: DEFAULT_QUICK_MODEL.into(),
            temperature: Some(0.0),
            max_tokens: Some(QUICK_MAX_TOKENS),
            reasoning_effort: None,
        }
    }

    /// Reject a model the request layer could not act on.
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty() {
            // An empty model name reaches the endpoint as a 400 whose text
            // names a field they never saw.
            return Err(Error::Invalid("a model name is required".into()));
        }
        if let Some(t) = self.temperature
            && !(0.0..=2.0).contains(&t)
        {
            return Err(Error::Invalid("temperature must be between 0 and 2".into()));
        }
        Ok(())
    }
}

/// Which of the three models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelTier {
    /// Deep research.
    Large,
    /// The assistant.
    Medium,
    /// The quick asks.
    Small,
}

impl ModelTier {
    pub const ALL: [ModelTier; 3] = [ModelTier::Large, ModelTier::Medium, ModelTier::Small];

    /// As Settings → Models titles it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Large => "Large",
            Self::Medium => "Medium",
            Self::Small => "Small",
        }
    }
}

/// One model: where it is, and which one to ask there.
///
/// Flat on the wire -- `{ provider, baseUrl, model, temperature, maxTokens,
/// reasoningEffort }`
/// -- because the pane edits it as one card; split in here because the
/// request layer wants the halves separately, and a key is found by the
/// first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LLMConfig {
    #[serde(flatten)]
    pub connection: LLMProviderConfig,
    #[serde(flatten)]
    pub model: LLMModelConfig,
}

impl LLMConfig {
    /// What a fresh install's medium model is.
    pub fn medium() -> Self {
        Self { connection: LLMProviderConfig::default(), model: LLMModelConfig::assistant() }
    }

    /// What the large model starts as when it is switched on.
    pub fn large() -> Self {
        Self { connection: LLMProviderConfig::default(), model: LLMModelConfig::large() }
    }

    /// What the small model starts as when it is switched on.
    pub fn small() -> Self {
        Self { connection: LLMProviderConfig::default(), model: LLMModelConfig::quick() }
    }

    pub fn endpoint(&self) -> &str {
        self.connection.endpoint()
    }

    pub fn needs_key(&self) -> bool {
        self.connection.needs_key()
    }

    pub fn validate(&self) -> Result<()> {
        self.connection.validate()?;
        self.model.validate()
    }
}

/// Which request shape a decision model takes.
///
/// A decision model ("System One", in TypeSafe's words) does not write. It
/// is handed some content and a set of typed questions -- pick one of these
/// options, is this true, where on this scale -- and answers each with a
/// probability. Two shapes of that request exist today:
///
/// - **TypeSafe's** `POST /v1/systemone`: a `state`, and `questions` keyed by
///   name, each with `criteria`. OpenRouter serves Jev at
///   `/api/alpha/decisions` with exactly this body.
/// - **OpenAI's** `POST /v1/decisions`: an `input`, and `questions` as a list,
///   each naming itself, with `choices`.
///
/// The address is separate ([`DecisionConfig::url`]): the same shape is
/// served at more than one place, and which place is somebody's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DecisionApi {
    /// TypeSafe's System One request: Jev, directly or through OpenRouter.
    #[default]
    TypeSafe,
    /// OpenAI's Decisions API.
    OpenAi,
}

impl DecisionApi {
    /// Where requests go when [`DecisionConfig::url`] is unset: the
    /// shape's own maker.
    pub fn default_url(self) -> &'static str {
        match self {
            Self::TypeSafe => "https://api.typesafe.ai/v1/systemone",
            Self::OpenAi => "https://api.openai.com/v1/decisions",
        }
    }

    /// What a decision model starts as when it is switched on.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::TypeSafe => "jev-latest",
            Self::OpenAi => "gpt-6-luna",
        }
    }
}

/// A decision model: which request shape, at which address, and which
/// model there.
///
/// Not an [`LLMConfig`]. There is no temperature, no token cap and no
/// reasoning effort to set -- a decision model answers with probabilities,
/// not text -- and its address is the full URL requests are posted to rather
/// than a base a path is joined onto, because the places that serve one do
/// not agree on a path (`/v1/systemone`, `/api/alpha/decisions`,
/// `/v1/decisions`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionConfig {
    #[serde(default)]
    pub api: DecisionApi,
    /// Overrides [`DecisionApi::default_url`]. Stored with no trailing
    /// slash; see [`DecisionConfig::endpoint`].
    #[serde(default)]
    pub url: Option<String>,
    /// As the endpoint spells it: `jev-latest`, `typesafe/jev-1.13`,
    /// `gpt-6-luna`.
    pub model: String,
}

impl DecisionConfig {
    /// What the card starts as when it is switched on: Jev, from TypeSafe.
    pub fn fresh() -> Self {
        let api = DecisionApi::default();
        Self { api, url: None, model: api.default_model().into() }
    }

    /// The URL requests are posted to, trailing slash removed -- and what
    /// its key is stored under, spelt as [`LLMProviderConfig::endpoint`]
    /// spells a base URL, so one address typed two ways finds one key.
    pub fn endpoint(&self) -> &str {
        self.url
            .as_deref()
            .map(|u| u.trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| self.api.default_url())
    }

    /// Whether a key is needed: always, except on this machine.
    pub fn needs_key(&self) -> bool {
        Provider::OpenAi.needs_key(self.url.as_deref())
    }

    /// The same address rules as a chat model's -- https unless local, one
    /// address, no spaces -- and a model name.
    pub fn validate(&self) -> Result<()> {
        LLMProviderConfig { provider: Provider::OpenAi, base_url: self.url.clone() }.validate()?;
        if self.model.trim().is_empty() {
            return Err(Error::Invalid("a model name is required".into()));
        }
        Ok(())
    }
}

/// One of the four cards in Settings → Models, for the commands that act on
/// a card's key: the three tiers, and the decision model.
///
/// Separate from [`ModelTier`] because a decision model is not a size of
/// the same work. Nothing resolves to it and it resolves to nothing; every
/// `match` over the tiers would otherwise grow an arm that had to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelSlot {
    Large,
    Medium,
    Small,
    Decision,
}

impl ModelSlot {
    pub fn label(self) -> &'static str {
        match self {
            Self::Large => "Large",
            Self::Medium => "Medium",
            Self::Small => "Small",
            Self::Decision => "Decision",
        }
    }
}

impl From<ModelTier> for ModelSlot {
    fn from(tier: ModelTier) -> Self {
        match tier {
            ModelTier::Large => Self::Large,
            ModelTier::Medium => Self::Medium,
            ModelTier::Small => Self::Small,
        }
    }
}

/// Every model this vault asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    /// What writes up a research job. `None` means research runs on
    /// [`medium`](Self::medium) -- see [`ModelSettings::resolve`].
    #[serde(default)]
    pub large: Option<LLMConfig>,
    /// The assistant's model: the one with the tools, the memories and the
    /// turn budget. Always set; a vault that has never been configured has
    /// the default here, and the assistant's own switch is what keeps it
    /// from being used.
    pub medium: LLMConfig,
    /// The cheap, fast model behind one-shot extraction and suggestion —
    /// pulling fields out of a search result, reading a tracker's number out
    /// of a sentence, sorting mail.
    ///
    /// `None` means those are simply not done. It deliberately does *not*
    /// fall back to [`medium`](Self::medium); see the module docs.
    #[serde(default)]
    pub small: Option<LLMConfig>,
    /// What sorts mail into the categories under Settings → Mail sorting, in
    /// place of Small. `None` leaves sorting on Small, as before there was
    /// one. See [`DecisionConfig`].
    #[serde(default)]
    pub decision: Option<DecisionConfig>,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self { large: None, medium: LLMConfig::medium(), small: None, decision: None }
    }
}

impl ModelSettings {
    /// Every model that is configured, with an error naming the tier it
    /// belongs to -- three cards in one pane share one Save, so "a model
    /// name is required" alone would not say which box is empty.
    pub fn validate(&self) -> Result<()> {
        for tier in ModelTier::ALL {
            if let Some(config) = self.get(tier) {
                config.validate().map_err(|e| match e {
                    Error::Invalid(why) => Error::Invalid(format!("{}: {why}", tier.label())),
                    other => other,
                })?;
            }
        }
        if let Some(small) = &self.small
            && small.model.reasoning_effort.is_some_and(|e| e > SMALL_MAX_EFFORT)
        {
            return Err(Error::Invalid(
                "Small: a reasoning effort above Low leaves a quick ask no room to answer in; \
                 choose Low or less, or the provider's default"
                    .into(),
            ));
        }
        if let Some(decision) = &self.decision {
            decision.validate().map_err(|e| match e {
                Error::Invalid(why) => Error::Invalid(format!("Decision: {why}")),
                other => other,
            })?;
        }
        Ok(())
    }

    /// Where `slot`'s card points, if it is switched on -- what a key set on
    /// that card is filed under.
    pub fn endpoint_of(&self, slot: ModelSlot) -> Option<&str> {
        match slot {
            ModelSlot::Large => self.large.as_ref().map(LLMConfig::endpoint),
            ModelSlot::Medium => Some(self.medium.endpoint()),
            ModelSlot::Small => self.small.as_ref().map(LLMConfig::endpoint),
            ModelSlot::Decision => self.decision.as_ref().map(DecisionConfig::endpoint),
        }
    }

    /// The tier as configured, with no fallback.
    pub fn get(&self, tier: ModelTier) -> Option<&LLMConfig> {
        match tier {
            ModelTier::Large => self.large.as_ref(),
            ModelTier::Medium => Some(&self.medium),
            ModelTier::Small => self.small.as_ref(),
        }
    }

    /// The model a tier's work actually runs on, and which tier that is.
    ///
    /// Large answers with Medium when it is not set; Small answers with
    /// nothing. See the module docs for why the two differ.
    pub fn resolve(&self, tier: ModelTier) -> Option<(ModelTier, &LLMConfig)> {
        match tier {
            ModelTier::Large => Some(match &self.large {
                Some(large) => (ModelTier::Large, large),
                None => (ModelTier::Medium, &self.medium),
            }),
            ModelTier::Medium => Some((ModelTier::Medium, &self.medium)),
            ModelTier::Small => self.small.as_ref().map(|small| (ModelTier::Small, small)),
        }
    }

    /// Every endpoint a configured tier -- or the decision model -- points
    /// at, sorted, each once.
    pub fn endpoints(&self) -> Vec<&str> {
        let mut all: Vec<&str> = ModelTier::ALL
            .iter()
            .filter_map(|t| self.get(*t))
            .map(LLMConfig::endpoint)
            .chain(self.decision.as_ref().map(DecisionConfig::endpoint))
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    /// What an account's mail acknowledgement must say for mail to reach
    /// any of these models: every configured endpoint, sorted, joined with
    /// `", "`.
    ///
    /// The whole list rather than the one a feature is about to use, because
    /// the sentence a person agreed to is "my mail may go to these" and every
    /// tier can carry mail -- the assistant's tools on Medium, a research
    /// job's on Large, sorting on Small or the decision model, whose address
    /// is in the list once it is switched on. A change to any endpoint stops it
    /// matching, which is the point: somebody who acknowledged OpenAI did
    /// not acknowledge the gateway Large was moved to afterwards. Unconfigured
    /// tiers are left out, so a vault whose tiers share one endpoint names
    /// that endpoint alone -- exactly what it named before there were tiers.
    ///
    /// The interface stores this string in an account when a box is ticked
    /// -- taken from `model_settings`'s own answer rather than worked out
    /// again there -- and the gate compares against it.
    pub fn acknowledgement_name(&self) -> String {
        self.endpoints().join(", ")
    }
}

/// The [`SecretStore`](crate::store::secrets::SecretStore) owner the model
/// keys are sealed under: one row, holding [`ModelKeys`]. Neither half says
/// anything about which endpoints there are -- see the module docs.
pub const MODEL_KEYS_OWNER_KIND: &str = "model-keys";
pub const MODEL_KEYS_OWNER_ID: &str = "vault";

/// Every model key, by the endpoint it was stored for, as
/// [`LLMProviderConfig::endpoint`] spells it. Sealed whole; never crosses to
/// the interface.
pub type ModelKeys = std::collections::BTreeMap<String, String>;

/// A model ready to be asked: what
/// [`Vault::model_access`](crate::Vault::model_access) hands the one layer
/// that opens a socket.
///
/// Holds the key, so it does not derive `Debug` -- see the manual impl,
/// which says whether there is one and nothing else.
#[derive(Clone)]
pub struct ModelAccess {
    /// The tier whose model this is, which is not always the tier that was
    /// asked for: Large resolves to Medium when it is not set.
    pub tier: ModelTier,
    pub config: LLMConfig,
    /// `None` only where none is needed -- a model on this machine.
    pub key: Option<String>,
    /// [`ModelSettings::acknowledgement_name`] for the settings this was
    /// read from, so the mail gate compares against the same snapshot the
    /// request is about to be made under.
    pub acknowledgement: String,
}

impl std::fmt::Debug for ModelAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelAccess")
            .field("tier", &self.tier)
            .field("config", &self.config)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("acknowledgement", &self.acknowledgement)
            .finish()
    }
}

/// The decision model ready to be asked -- [`ModelAccess`]'s counterpart,
/// from [`Vault::decision_access`](crate::Vault::decision_access).
#[derive(Clone)]
pub struct DecisionAccess {
    pub config: DecisionConfig,
    /// `None` only where none is needed -- a model on this machine.
    pub key: Option<String>,
    /// As [`ModelAccess::acknowledgement`].
    pub acknowledgement: String,
}

impl std::fmt::Debug for DecisionAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionAccess")
            .field("config", &self.config)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("acknowledgement", &self.acknowledgement)
            .finish()
    }
}

/// Associated data for the sealed model settings row. Constant, because there
/// is one per vault, and distinct from every other singleton's so that none
/// of them can be swapped for another.
pub fn model_settings_aad() -> Vec<u8> {
    b"everyday.model-settings.v1".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_defaults_to_the_provider_and_loses_a_trailing_slash() {
        let mut cfg = LLMProviderConfig::default();
        assert_eq!(cfg.endpoint(), "https://api.openai.com/v1");

        cfg.base_url = Some("http://localhost:11434/v1/".into());
        assert_eq!(cfg.endpoint(), "http://localhost:11434/v1");

        // An empty override is not an override.
        cfg.base_url = Some(String::new());
        assert_eq!(cfg.endpoint(), "https://api.openai.com/v1");
    }

    /// A key is stored under this string and the models pane says whether a
    /// card has one by spelling it the same way -- `endpointOf` in
    /// `ui/src/lib/accounts.ts`, whose test pins this same table. Change both
    /// or neither.
    #[test]
    fn the_endpoint_is_spelt_as_the_interface_spells_it() {
        for (base_url, want) in [
            (None, "https://api.openai.com/v1"),
            (Some(""), "https://api.openai.com/v1"),
            (Some("https://openrouter.ai/api/v1"), "https://openrouter.ai/api/v1"),
            (Some("https://openrouter.ai/api/v1/"), "https://openrouter.ai/api/v1"),
            (Some("http://localhost:11434/v1//"), "http://localhost:11434/v1"),
            // Not trimmed: a base URL saved with a space on the end before
            // the tiers was acknowledged with it, and must still match.
            (Some("https://openrouter.ai/api/v1 "), "https://openrouter.ai/api/v1 "),
            (Some("/"), "https://api.openai.com/v1"),
        ] {
            let cfg = LLMProviderConfig {
                provider: Provider::OpenAi,
                base_url: base_url.map(str::to_string),
            };
            assert_eq!(cfg.endpoint(), want, "{base_url:?}");
        }
    }

    #[test]
    fn a_local_model_needs_no_api_key() {
        let mut cfg = LLMProviderConfig::default();
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
        let mut cfg = LLMProviderConfig {
            base_url: Some("http://localhost:11434/v1".into()),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok(), "a local model over http is fine");

        cfg.base_url = Some("http://gateway.example.com/v1".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("unencrypted"), "should explain the risk, got: {err}");

        cfg.base_url = Some("gateway.example.com/v1".into());
        assert!(cfg.validate().is_err(), "a URL with no scheme is not a URL");

        cfg.base_url = Some("https://a.example.com/v1, https://b.example.com/v1".into());
        assert!(cfg.validate().is_err(), "one address, not a list of them");
    }

    #[test]
    fn a_local_host_is_recognised_however_it_was_typed() {
        // Not merely a missed optimisation: an unrecognised http:// host is
        // refused outright as unencrypted, so the setting will not save.
        for url in ["http://LocalHost:11434/v1", "http://LOCALHOST:1234", "http://[::1]:8080"] {
            let cfg = LLMProviderConfig { base_url: Some(url.into()), ..Default::default() };
            assert!(!cfg.needs_key(), "{url} runs on this machine");
            assert!(cfg.validate().is_ok(), "{url} should be saveable");
        }
    }

    #[test]
    fn a_new_vault_has_a_medium_model_and_nothing_else() {
        let s = ModelSettings::default();
        assert_eq!(s.medium.model.model, DEFAULT_MODEL);
        assert!(s.large.is_none(), "research runs on the assistant's model until somebody says");
        assert!(s.small.is_none(), "somebody has to ask for a paid feature");
        assert!(s.validate().is_ok());
    }

    #[test]
    fn research_falls_back_to_the_assistant_and_quick_asks_do_not() {
        let mut s = ModelSettings::default();
        let (tier, config) = s.resolve(ModelTier::Large).unwrap();
        assert_eq!(tier, ModelTier::Medium, "no large model: research uses the medium one");
        assert_eq!(config, &s.medium);
        assert!(
            s.resolve(ModelTier::Small).is_none(),
            "a missing small model must not fall back to a more expensive one"
        );

        s.large = Some(LLMConfig::large());
        s.small = Some(LLMConfig::small());
        assert_eq!(s.resolve(ModelTier::Large).unwrap().0, ModelTier::Large);
        assert_eq!(s.resolve(ModelTier::Small).unwrap().0, ModelTier::Small);
    }

    #[test]
    fn a_tier_travels_flat_on_the_wire() {
        let s = ModelSettings {
            large: None,
            medium: LLMConfig {
                connection: LLMProviderConfig {
                    provider: Provider::OpenAi,
                    base_url: Some("https://openrouter.ai/api/v1".into()),
                },
                model: LLMModelConfig {
                    model: "x/y".into(),
                    temperature: Some(0.3),
                    max_tokens: Some(2048),
                    reasoning_effort: Some(ReasoningEffort::Xhigh),
                },
            },
            small: Some(LLMConfig::small()),
            decision: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["medium"]["baseUrl"], "https://openrouter.ai/api/v1");
        assert_eq!(json["medium"]["model"], "x/y");
        assert_eq!(json["medium"]["maxTokens"], 2048);
        // Spelt as the request spells it, so the pane, the record and the
        // wire all say the same word.
        assert_eq!(json["medium"]["reasoningEffort"], "xhigh");
        assert!(json["large"].is_null());
        let back: ModelSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);

        // What the pane sends for a card it has only just switched on, with
        // nothing optional filled in.
        let sparse: LLMConfig = serde_json::from_value(serde_json::json!({
            "provider": "openAi", "model": "gpt-5.1-nano", "temperature": 0
        }))
        .unwrap();
        assert_eq!(sparse.connection.base_url, None);
        assert_eq!(sparse.model.temperature, Some(0.0));
        assert_eq!(sparse.model.max_tokens, None);
        // And every record saved before there was an effort to set: nothing
        // is sent, so nothing about what they ask changes.
        assert_eq!(sparse.model.reasoning_effort, None);
    }

    /// A level from a newer build must not cost the vault every model.
    #[test]
    fn an_effort_this_build_does_not_know_reads_as_unset() {
        let config: LLMConfig = serde_json::from_value(serde_json::json!({
            "provider": "openAi", "model": "m", "reasoningEffort": "ultra"
        }))
        .expect("the tier still reads");
        assert_eq!(config.model.reasoning_effort, None);

        let config: LLMConfig = serde_json::from_value(serde_json::json!({
            "provider": "openAi", "model": "m", "reasoningEffort": "xhigh"
        }))
        .unwrap();
        assert_eq!(config.model.reasoning_effort, Some(ReasoningEffort::Xhigh));
    }

    #[test]
    fn the_small_model_may_think_only_a_little() {
        let with = |effort| ModelSettings {
            small: Some(LLMConfig {
                model: LLMModelConfig { reasoning_effort: Some(effort), ..LLMModelConfig::quick() },
                ..LLMConfig::small()
            }),
            ..ModelSettings::default()
        };
        for ok in [ReasoningEffort::None, ReasoningEffort::Minimal, ReasoningEffort::Low] {
            assert!(with(ok).validate().is_ok(), "{ok:?}");
        }
        let err = with(ReasoningEffort::Medium).validate().unwrap_err().to_string();
        assert!(err.contains("Small") && err.contains("Low"), "{err}");

        // The others may think as hard as they are told to.
        let mut s = ModelSettings::default();
        s.medium.model.reasoning_effort = Some(ReasoningEffort::Max);
        s.large = Some(LLMConfig {
            model: LLMModelConfig {
                reasoning_effort: Some(ReasoningEffort::Xhigh),
                ..LLMModelConfig::large()
            },
            ..LLMConfig::large()
        });
        assert!(s.validate().is_ok());
    }

    #[test]
    fn a_validation_error_names_the_tier_it_is_about() {
        let s = ModelSettings {
            small: Some(LLMConfig {
                model: LLMModelConfig { model: " ".into(), ..LLMModelConfig::quick() },
                ..LLMConfig::small()
            }),
            ..ModelSettings::default()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("Small"), "{err}");
    }

    /// An account's acknowledgement stores this string -- the interface takes
    /// it from `model_settings`'s own answer -- and the mail gate compares
    /// against it, so its spelling is the contract.
    #[test]
    fn acknowledgement_name_is_every_endpoint() {
        let at = |url: Option<&str>| LLMConfig {
            connection: LLMProviderConfig {
                provider: Provider::OpenAi,
                base_url: url.map(str::to_string),
            },
            model: LLMModelConfig::assistant(),
        };
        for (large, medium, small, want) in [
            (None, None, None, "https://api.openai.com/v1"),
            (None, Some(""), None, "https://api.openai.com/v1"),
            (None, Some("/"), None, "https://api.openai.com/v1"),
            (None, Some("https://openrouter.ai/api/v1/"), None, "https://openrouter.ai/api/v1"),
            // Every tier on one endpoint names it once -- the shape every
            // vault from before the tiers has, whose acknowledgements must
            // go on matching.
            (Some(None), None, Some(None), "https://api.openai.com/v1"),
            (
                Some(Some("https://openrouter.ai/api/v1")),
                None,
                Some(Some("http://localhost:11434/v1//")),
                "http://localhost:11434/v1, https://api.openai.com/v1, https://openrouter.ai/api/v1",
            ),
        ] {
            let s = ModelSettings {
                large: large.map(at),
                medium: at(medium),
                small: small.map(at),
                decision: None,
            };
            assert_eq!(s.acknowledgement_name(), want);
        }
    }

    #[test]
    fn a_decision_model_is_an_endpoint_like_any_other() {
        let mut s = ModelSettings::default();
        assert_eq!(s.endpoint_of(ModelSlot::Decision), None);
        s.decision = Some(DecisionConfig::fresh());
        assert_eq!(
            s.endpoint_of(ModelSlot::Decision),
            Some("https://api.typesafe.ai/v1/systemone"),
            "Jev, from TypeSafe, when nothing else is said"
        );
        // Mail goes to it, so an acknowledgement has to name it.
        assert_eq!(
            s.acknowledgement_name(),
            "https://api.openai.com/v1, https://api.typesafe.ai/v1/systemone"
        );

        s.decision = Some(DecisionConfig {
            api: DecisionApi::TypeSafe,
            url: Some("https://openrouter.ai/api/alpha/decisions/".into()),
            model: "typesafe/jev-1.13".into(),
        });
        assert_eq!(
            s.endpoint_of(ModelSlot::Decision),
            Some("https://openrouter.ai/api/alpha/decisions")
        );
        assert!(s.validate().is_ok());

        s.decision =
            Some(DecisionConfig { api: DecisionApi::OpenAi, url: None, model: " ".into() });
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("Decision") && err.contains("model name"), "{err}");

        s.decision = Some(DecisionConfig {
            api: DecisionApi::OpenAi,
            url: Some("http://decisions.example.com/v1/decisions".into()),
            model: "gpt-6-luna".into(),
        });
        assert!(s.validate().is_err(), "a key is not sent over plain http");
    }

    #[test]
    fn a_vault_saved_before_decision_models_reads_without_one() {
        let s: ModelSettings = serde_json::from_value(serde_json::json!({
            "medium": { "provider": "openAi", "model": "gpt-5.1-mini" }
        }))
        .unwrap();
        assert_eq!(s.decision, None);

        let d: DecisionConfig =
            serde_json::from_value(serde_json::json!({ "model": "jev-latest" })).unwrap();
        assert_eq!(d.api, DecisionApi::TypeSafe);
        assert_eq!(serde_json::to_value(DecisionApi::OpenAi).unwrap(), "openAi");
    }

    #[test]
    fn the_key_is_not_printed() {
        let access = ModelAccess {
            tier: ModelTier::Medium,
            config: LLMConfig::medium(),
            key: Some("sk-secret".into()),
            acknowledgement: String::new(),
        };
        assert!(!format!("{access:?}").contains("sk-secret"));
        let access = DecisionAccess {
            config: DecisionConfig::fresh(),
            key: Some("ts-secret".into()),
            acknowledgement: String::new(),
        };
        assert!(!format!("{access:?}").contains("ts-secret"));
    }
}
