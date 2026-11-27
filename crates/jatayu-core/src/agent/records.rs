//! What the assistant is, what it has said and what it has been asked to
//! remember, as records: [`AgentSettings`], [`Conversation`] with its
//! [`Message`]s, [`Memory`] and [`Skill`].
//!
//! Everything here is stored, sealed, in the vault; what is built from it for
//! a model is in the sibling `prompt` module. `agent` re-exports all of it, so
//! callers say `agent::Memory`, not `agent::records::Memory`.

use super::{desk, sites};
use crate::error::{Error, Result};
use crate::id::{ConversationId, MemoryId, MessageId, SkillId};
use crate::llm::{LLMConfig, LLMModelConfig, LLMProviderConfig, ModelSettings, Provider};
use crate::quick::QuickPolicy;
use crate::timestamped::Timestamped;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Which dog the assistant is: three words, each from a short list the
/// interface keeps in `ui/src/lib/companion.ts`.
///
/// Words rather than enums, because nothing in the core draws a dog or needs
/// to know what a corgi is -- and because an enum would make a breed added in
/// a later build into a settings record an earlier build refuses to read,
/// taking the model, the key and everything else with it. A word the
/// interface does not know is drawn as its default; `None` is the same, so
/// which dog is the default is decided in one place, the one that draws it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Companion {
    /// The shape: ears, snout, tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breed: Option<String>,
    /// The main colour of the coat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coat: Option<String>,
    /// What is painted on it: a white bib and socks, spots, a dark mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markings: Option<String>,
}

/// Longest one of a [`Companion`]'s words may be. They are identifiers from a
/// list, not prose; this only stops the field from being a place to keep
/// something else.
pub const MAX_COMPANION_WORD_CHARS: usize = 24;

impl Companion {
    /// A dog with every choice left to the interface's default.
    pub fn default_dog() -> Option<Self> {
        Some(Self::default())
    }

    fn validate(&self) -> Result<()> {
        for word in [&self.breed, &self.coat, &self.markings].into_iter().flatten() {
            let fits = !word.is_empty()
                && word.chars().count() <= MAX_COMPANION_WORD_CHARS
                && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !fits {
                return Err(Error::Invalid(format!(
                    "{word:?} is not a kind of dog this app draws"
                )));
            }
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
    /// What to call it.
    ///
    /// Empty means it has not been named, and an unnamed assistant is
    /// "the assistant" everywhere -- in the rail's header, and in the first
    /// sentence of [`crate::agent::system_prompt`]. Naming it is not decoration: a model
    /// that has been told it is called Robin answers to "Robin, what did I
    /// say about the boat" instead of treating the word as a person in the
    /// journal, and it is the difference between a feature and a colleague
    /// for the people who want that.
    ///
    /// The application does not pick one on anybody's behalf. A default name
    /// would be this software introducing itself under a name its owner did
    /// not choose, in the one place that is most theirs.
    #[serde(default)]
    pub name: String,
    /// The dog drawn in the header of the rail and of the Assistant app, or
    /// `None` for no dog at all.
    ///
    /// Picked beside the name, and kept beside it for the same reason: it is
    /// who the assistant is, so it follows the vault to the next machine
    /// rather than staying behind in one browser's local storage.
    ///
    /// A record with no key at all -- every record written before there was
    /// a dog -- reads as the default dog, and `null` reads as none. The two
    /// must stay distinct, which is why this is not `skip_serializing_if`:
    /// somebody who said "no dog" must not have one back the next time the
    /// settings are saved.
    ///
    /// Unlike the name, a dog *is* offered by default. A name is the model
    /// introducing itself, in every reply; the dog says nothing to anybody
    /// and is the one place the rail shows, at a glance, that a turn is
    /// still working.
    #[serde(default = "Companion::default_dog")]
    pub companion: Option<Companion>,
    /// Which quick asks are allowed to run, by [`QuickAsk::name`].
    ///
    /// Not a single switch, because "AI features: on" is not consent to
    /// having the day's prose read. Empty means every ask whose default is
    /// on — see [`QuickPolicy`].
    ///
    /// Read under its old name too: this was `quickJobs` until "job" was
    /// wanted for the assistant's background work, and a vault written
    /// before the rename must not have every switch quietly reset.
    #[serde(default, alias = "quickJobs")]
    pub quick_asks: QuickPolicy,
    /// Where the models were before they became application settings --
    /// see [`crate::llm`]. The four `legacy_*` fields are the three shapes
    /// this record has stored a model in, newest first.
    ///
    /// Read, never written: `skip_serializing` means a save rewrites the
    /// record without them. They are folded into a [`ModelSettings`] by
    /// [`AgentSettings::legacy_models`], which
    /// [`Vault::model_settings`](crate::Vault::model_settings) calls the
    /// first time it finds no models saved -- a sealed payload cannot be
    /// migrated in SQL, because a migration step is handed a connection and
    /// not the cipher -- and [`Vault::save_agent_settings`] makes sure that
    /// has happened before a save can drop them.
    ///
    /// Public only because a private field would break
    /// `..Default::default()` in every crate that builds one of these, which
    /// is most of them.
    #[serde(default, rename = "providerConfig", skip_serializing)]
    pub legacy_provider: Option<LLMProviderConfig>,
    /// The assistant's model, from when it was the assistant's setting.
    #[serde(default, rename = "assistantModel", skip_serializing)]
    pub legacy_assistant_model: Option<LLMModelConfig>,
    /// The quick model, from when it was the assistant's setting. `null`
    /// and absent both mean there was none, which is what they meant then.
    #[serde(default, rename = "quickModel", skip_serializing)]
    pub legacy_quick_model: Option<LLMModelConfig>,
    /// The shape from before the endpoint and the model were separate
    /// records, when one model was the only model.
    #[serde(default, rename = "model", skip_serializing)]
    pub legacy_model: Option<LegacyModelConfig>,
    /// The person's own instructions: personality, preferences, house style,
    /// anything they want true of every reply. Prepended to the assistant's
    /// own operating instructions rather than replacing them — see
    /// [`crate::agent::system_prompt`].
    ///
    /// Settings calls this box the **soul**, which is the better name for
    /// what people write in it and the reason the order in `system_prompt`
    /// is what it is: a field labelled "system prompt" collects the
    /// operating manual the model is already given, restated worse, and a
    /// field labelled "soul" collects temperament. The key keeps the older
    /// name because it is sealed into every payload that has one, and a
    /// rename would be a migration handed a connection rather than a cipher
    /// — the same problem [`legacy_model`](Self::legacy_model) documents.
    pub instructions: String,
    /// Whether a destructive tool call stops and asks first.
    ///
    /// Defaults on, and the default worth defending: this application has no
    /// undo stack, so an agent that deletes a project on a misreading has
    /// destroyed something with no way back. Creates and edits apply
    /// directly because they are recoverable by hand; deletes and bulk
    /// updates are not.
    pub confirm_destructive: bool,
    /// Whether a page open that could carry something out of the vault stops
    /// and asks first.
    ///
    /// Defaults on, and a record written before the switch existed reads as
    /// on — that is what those turns did. On, a page whose address the model
    /// put together itself shows the address first when it has anything in
    /// it that could be data, or once anything a stranger wrote -- mail, a
    /// page, a search's results -- has come into the turn. A search never
    /// asks, and neither does an address the person gave or a search or page
    /// handed back. Off, those go ahead while somebody is in the conversation.
    /// Sending mail and deleting are not this switch, and neither is a turn
    /// with nobody watching: a routine or a background job still stops,
    /// because the switch means "don't ask me while I am here", and nobody is
    /// there to have meant it.
    #[serde(default = "confirm_web_default")]
    pub confirm_web: bool,
    /// Websites the person has said may be opened without asking -- see
    /// [`sites`]. Each covers the names under it. An address on one opens
    /// without the question [`confirm_web`](Self::confirm_web) would put,
    /// except one that carries what could be data once a stranger's words
    /// are in the turn, or with nobody watching: see `must_confirm_fetch` in
    /// `jatayu_service::agent`.
    ///
    /// Changed only by `trust_site` and `untrust_site` (and by "always allow"
    /// on a question, which is `trust_site`): `save_agent_settings` keeps
    /// whatever is stored, so a settings pane opened before a site was added
    /// cannot take it away again by saving.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safe_sites: Vec<String>,
    /// How the voice says words it would otherwise say wrong -- see
    /// [`Pronunciation`](crate::speech::Pronunciation). Here rather than with
    /// the voice in the models' settings because they are the person's, and
    /// outlive whichever voice is chosen; read by every voice, the Listen
    /// button's as much as the assistant's.
    ///
    /// Changed only by `set_pronunciation` and `remove_pronunciation` (the
    /// assistant's `remember_pronunciation` is the first):
    /// `save_agent_settings` keeps whatever is stored, as it does
    /// [`safe_sites`](Self::safe_sites).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pronunciations: Vec<crate::speech::Pronunciation>,
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
    /// Whether the assistant may search the web.
    ///
    /// Off until somebody says otherwise, and its own switch rather than a
    /// corner of `enabled`. It is the one tool that sends the words of a
    /// question -- and, preparing for a meeting, the names of the people in
    /// it -- to a computer somebody else runs. Everything else the assistant
    /// does happens between this machine and the model endpoint the person
    /// chose.
    #[serde(default)]
    pub web: bool,
    /// Whether the assistant may drive a browser: open pages in a private,
    /// throwaway copy of Chrome or Chromium on this computer, click, and
    /// type into forms. Only with [`web`](Self::web) on, and off until
    /// somebody turns it on -- it runs a stranger's scripts on this machine
    /// and can put words into a stranger's form, which reading a page cannot.
    /// Typing always asks first. See `jatayu_service::browser`.
    #[serde(default)]
    pub browser: bool,
    /// Whether the assistant dreams: reads the day overnight, revises what it
    /// has inferred about the person, and leaves proposals to accept or
    /// decline. Off until somebody turns it on, because it reads the journal.
    /// See `docs/plans/dreaming.md`.
    #[serde(default)]
    pub dreaming: bool,
    /// Which kinds of record the assistant may propose.
    #[serde(default)]
    pub proposals: crate::proposal::ProposalPolicy,
    /// Whether a scheduled routine, finding nobody there to confirm a delete
    /// or a send, leaves a proposal instead of giving up on the spot.
    ///
    /// Off by default: a routine that leaves deletions waiting is a
    /// different promise from one that refuses them, and the difference is
    /// worth its own sentence rather than folding into
    /// [`confirm_destructive`](Self::confirm_destructive), which is about
    /// whether anybody is asked at all. See `docs/plans/dreaming.md`'s
    /// Phase 5 -- this is the same park-instead-of-decline the rail's own
    /// "later" answer uses, offered here for a run nobody is watching.
    #[serde(default)]
    pub park_unattended: bool,
    /// Where reminders go: theirs, and what the assistant's own tasks come
    /// back with. See [`crate::task::ReminderSettings`].
    #[serde(default)]
    pub reminders: crate::task::ReminderSettings,
    /// When background updates are not spoken in a voice session -- a watch
    /// saying CI has finished, at three in the morning, on a call left open.
    /// Voice only: an update still goes to the chat and the thread at once,
    /// quiet hours or not, and is spoken once they are over if the session is
    /// still open. See `docs/plans/watches.md`.
    #[serde(default)]
    pub quiet_hours: QuietHours,
    /// Whether the assistant may act on the computer somebody is talking to
    /// it at: open a link, an application or a file there, search a website
    /// in their browser, pause the music, turn the volume down, switch Do
    /// Not Disturb, lock the screen, copy something, set a timer. Only ever
    /// from this computer -- the window, voice mode in it, the talk key and
    /// the wake word -- never from a chat, a routine or a job. On unless
    /// somebody turns it off: nothing here reaches anybody, and each thing
    /// it does is a thing they can see and undo at once. See
    /// `docs/plans/desk.md`.
    #[serde(default = "desktop_default")]
    pub desktop: bool,
    /// Websites somebody has added to search by name -- "search the wiki
    /// for" -- besides [`desk`]'s built-in ones, which one of the same name
    /// replaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_sites: Vec<desk::SearchSite>,
}

/// The hours in which updates are not spoken. Local times, in the person's
/// zone ([`AgentSettings::zone`]), as `"HH:MM"`. A span that wraps midnight --
/// 22:00 to 07:00 -- is the usual one; `start == end` is no quiet time at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QuietHours {
    pub enabled: bool,
    pub start: String,
    pub end: String,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self { enabled: false, start: "22:00".into(), end: "07:00".into() }
    }
}

impl QuietHours {
    /// Check both times read as `HH:MM` (`HH:MM:SS` is read too, and its
    /// seconds ignored). [`AgentSettings::validate`] asks only while they are
    /// on.
    pub fn validate(&self) -> Result<()> {
        for (name, time) in [("start", &self.start), ("end", &self.end)] {
            if parse_clock(time).is_none() {
                return Err(Error::Invalid(format!(
                    "quiet hours' {name} must be a time like 22:00, not {time:?}"
                )));
            }
        }
        Ok(())
    }

    /// Whether `now` falls in quiet hours. Off, or unreadable, is never.
    pub fn contains(&self, now: &jiff::Zoned) -> bool {
        let (Some(start), Some(end)) = (parse_clock(&self.start), parse_clock(&self.end)) else {
            return false;
        };
        if !self.enabled || start == end {
            return false;
        }
        let at = now.time();
        if start < end { at >= start && at < end } else { at >= start || at < end }
    }

    /// When the quiet hours `now` is in end, or `None` when `now` is not in
    /// them.
    pub fn ends_after(&self, now: &jiff::Zoned) -> Option<jiff::Zoned> {
        if !self.contains(now) {
            return None;
        }
        let end = parse_clock(&self.end)?;
        let today = now.date().to_datetime(end).to_zoned(now.time_zone().clone()).ok()?;
        if today > *now {
            Some(today)
        } else {
            let tomorrow = now.date().tomorrow().ok()?;
            tomorrow.to_datetime(end).to_zoned(now.time_zone().clone()).ok()
        }
    }
}

/// `"22:00"` as a time of day, or `None`. A time input may hand back
/// `"22:00:00"`; the seconds are read and ignored, since quiet hours are kept
/// to the minute.
pub(crate) fn parse_clock(text: &str) -> Option<jiff::civil::Time> {
    let mut parts = text.trim().split(':');
    let hour: i8 = parts.next()?.parse().ok()?;
    let minute: i8 = parts.next()?.parse().ok()?;
    if let Some(seconds) = parts.next() {
        let seconds: i8 = seconds.parse().ok()?;
        if !(0..60).contains(&seconds) {
            return None;
        }
    }
    if parts.next().is_some() {
        return None;
    }
    jiff::civil::Time::new(hour, minute, 0, 0).ok()
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            name: String::new(),
            companion: Companion::default_dog(),
            quick_asks: QuickPolicy::default(),
            legacy_provider: None,
            legacy_assistant_model: None,
            legacy_quick_model: None,
            legacy_model: None,
            instructions: String::new(),
            confirm_destructive: true,
            confirm_web: true,
            safe_sites: Vec::new(),
            pronunciations: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            remember: true,
            timezone: None,
            web: false,
            browser: false,
            dreaming: false,
            proposals: crate::proposal::ProposalPolicy::default(),
            park_unattended: false,
            reminders: crate::task::ReminderSettings::default(),
            quiet_hours: QuietHours::default(),
            desktop: true,
            search_sites: Vec::new(),
        }
    }
}

/// A settings record from before the switch acts on the desktop: the
/// switch is on unless somebody turned it off, so a missing key is on.
fn desktop_default() -> bool {
    true
}

/// A settings record with no `confirmWeb` key still asks. `bool`'s own
/// [`Default`] is `false`, which would quietly stop asking on every vault
/// written before the switch existed.
fn confirm_web_default() -> bool {
    true
}

/// Turns one request may take. Enough for a real multi-step task — read the
/// projects, read their tasks, write a plan — and short of a loop.
pub const DEFAULT_MAX_STEPS: u32 = 24;

/// Hard ceiling on [`AgentSettings::max_steps`], whatever the setting says.
pub const MAX_STEPS_LIMIT: u32 = 100;

/// Longest the assistant's name may be.
///
/// A name, not a biography. Anything a person wants said about how it should
/// behave belongs in the soul below, which is the field sized for prose; a
/// two-hundred-character "name" would be an instruction smuggled into the
/// one string that is drawn in a 240px header.
pub const MAX_NAME_CHARS: usize = 40;

/// Longest a person's own instructions — the soul — may be.
///
/// Generous — several pages — and finite, because this string is sent with
/// every single request. Someone who pastes a novel into it pays for that
/// novel on every turn of every conversation, and would have no way to see
/// why the bill grew.
pub const MAX_INSTRUCTIONS_BYTES: usize = 8_000;

impl AgentSettings {
    pub fn validate(&self) -> Result<()> {
        if self.name.chars().count() > MAX_NAME_CHARS {
            return Err(Error::Invalid(format!(
                "a name must be under {MAX_NAME_CHARS} characters"
            )));
        }
        // A name is written into the first line of every system prompt, so a
        // newline in it would be a way to append a line of instructions to
        // the house rules from a field that does not look like one.
        if self.name.contains(['\n', '\r']) {
            return Err(Error::Invalid("a name is one line".into()));
        }
        if let Some(companion) = &self.companion {
            companion.validate()?;
        }
        sites::validate(&self.safe_sites)?;
        desk::validate_sites(&self.search_sites)?;
        crate::speech::validate_pronunciations(&self.pronunciations)?;
        // "Soul" rather than the field name, because this sentence is shown
        // verbatim under the box it is about, and the box is labelled Soul.
        // The routine's own identical-looking limit still says
        // "instructions" -- see `routine::MAX_INSTRUCTIONS_BYTES` -- because
        // a routine's box really is labelled Instructions. Two messages, two
        // labels, each matching the thing the person is looking at.
        if self.instructions.len() > MAX_INSTRUCTIONS_BYTES {
            return Err(Error::Invalid(format!(
                "the soul is {} bytes; the limit is {MAX_INSTRUCTIONS_BYTES}",
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
        // Only when they are on. A time input that was cleared while quiet
        // hours are off is nothing to refuse every other setting for; the
        // times are kept as they are, and checked the moment they matter.
        if self.quiet_hours.enabled {
            self.quiet_hours.validate()?;
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

    /// The models this record named before they became application
    /// settings, or `None` if it named none.
    ///
    /// Every shape it has had, newest first: `providerConfig` with
    /// `assistantModel` and `quickModel`, and before those `model`, when one
    /// model was the only one. A record holding both (which nothing writes,
    /// but a hand-edited or half-migrated one could) keeps the newer answer.
    /// The quick model shared the assistant's endpoint, so it is given the
    /// same one here; the large model did not exist, so it is not set and
    /// research goes on running on the assistant's model, as it did.
    pub fn legacy_models(&self) -> Option<ModelSettings> {
        let old = self.legacy_model.as_ref();
        let connection = self.legacy_provider.clone().or_else(|| {
            old.map(|m| LLMProviderConfig { provider: m.provider, base_url: m.base_url.clone() })
        });
        let assistant = self.legacy_assistant_model.clone().or_else(|| {
            old.map(|m| LLMModelConfig {
                model: m.model.clone(),
                temperature: m.temperature,
                max_tokens: m.max_tokens,
                reasoning_effort: None,
            })
        });
        if connection.is_none() && assistant.is_none() && self.legacy_quick_model.is_none() {
            return None;
        }
        let connection = connection.unwrap_or_default();
        Some(ModelSettings {
            large: None,
            medium: LLMConfig {
                connection: connection.clone(),
                model: assistant.unwrap_or_else(LLMModelConfig::assistant),
            },
            small: self.legacy_quick_model.clone().map(|model| LLMConfig { connection, model }),
            decision: None,
            speech: None,
            voice: None,
            image: None,
        })
    }
}

/// The shape [`AgentSettings`] stored its model in before the endpoint and
/// the model became separate records. Deserialised, never written.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyModelConfig {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// Who said something.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Where a mail tool's own result named the thread it touched -- set
    /// only on a [`Role::Tool`] message answering a write in
    /// [`crate::agent::tools::Domain::Mail`], so the transcript can draw a
    /// link straight to it ("Archived: Plans for Saturday →") without
    /// re-parsing `content`, which is prose for a person to read, not data
    /// for a client to scrape. `None` for every other tool, and for a mail
    /// tool that only read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mail_link: Option<MailLink>,
    /// Pictures the person sent with this message. Only ever non-empty on
    /// [`Role::User`].
    ///
    /// Each is a picture already made the size the model is shown -- see
    /// `jatayu_service::pictures` -- and kept for as long as the message
    /// is: they are what the model was asked about, and the thread has to
    /// go on showing them. That is why [`JournalStore::collect_garbage`]
    /// walks every message's attachments; without it, the first sweep after
    /// a day would take every picture out of every conversation.
    ///
    /// [`JournalStore::collect_garbage`]: crate::store::JournalStore::collect_garbage
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::model::Attachment>,
    /// Files in the files catalog this message carries: what somebody handed
    /// the assistant in the composer or a chat, kept as the files they are
    /// (see [`crate::files`]). A picture among them also has a small copy in
    /// [`Message::attachments`], which is what a model is shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::id::FileId>,
    pub created_at: Timestamp,
}

/// What [`Message::mail_link`] carries: enough to open Mail at the thread a
/// write actually touched, and to say so before the click.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailLink {
    pub thread_id: String,
    pub subject: String,
}

impl Message {
    pub fn user(conversation_id: ConversationId, content: impl Into<String>) -> Self {
        Self::new(conversation_id, Role::User, content)
    }

    pub fn assistant(conversation_id: ConversationId, content: impl Into<String>) -> Self {
        Self::new(conversation_id, Role::Assistant, content)
    }

    /// A line the application put in the thread -- a finished job, say --
    /// attributed to neither party.
    pub fn system(conversation_id: ConversationId, content: impl Into<String>) -> Self {
        Self::new(conversation_id, Role::System, content)
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
            mail_link: None,
            attachments: Vec::new(),
            files: Vec::new(),
            created_at: Timestamp::now(),
        }
    }

    /// The result of running `call`, successful or not. `mail_link` is
    /// `Some` only for a successful mail write whose own JSON result named a
    /// thread -- see [`Message::mail_link`].
    pub fn tool_result(
        conversation_id: ConversationId,
        call: &ToolCall,
        outcome: std::result::Result<String, String>,
        mail_link: Option<MailLink>,
    ) -> Self {
        let (content, failed) = match outcome {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Self {
            tool_call_id: Some(call.id.clone()),
            failed,
            mail_link,
            ..Self::new(conversation_id, Role::Tool, content)
        }
    }

    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }

    pub fn with_attachments(mut self, attachments: Vec<crate::model::Attachment>) -> Self {
        self.attachments = attachments;
        self
    }
}

/// A titled thread.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// The routine run this thread is the transcript of, if it is one.
    ///
    /// Inside the payload rather than a column, because it is only ever read
    /// after the row has been decrypted anyway -- the history list opens every
    /// thread for its title. It is how the rail keeps a week of morning briefs
    /// out of a list of conversations somebody actually had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<crate::id::RoutineRunId>,
    /// The background job this thread is the transcript of, if it is one.
    /// Kept out of the rail's list for the reason `run_id` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<crate::id::JobId>,
    /// The chat this thread arrived through, if somebody started it from
    /// another device rather than in the rail. See [`crate::chat`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<crate::chat::Via>,
}

impl Conversation {
    pub fn new() -> Self {
        let now = Timestamp::now();
        Self {
            id: ConversationId::new(),
            title: String::new(),
            created_at: now,
            updated_at: now,
            run_id: None,
            job_id: None,
            via: None,
        }
    }

    /// A thread that is a routine run's transcript rather than a chat.
    pub fn for_run(run: crate::id::RoutineRunId, title: impl Into<String>) -> Self {
        Self { title: title.into(), run_id: Some(run), ..Self::new() }
    }

    /// A thread that is a background job's transcript rather than a chat.
    pub fn for_job(job: crate::id::JobId, title: impl Into<String>) -> Self {
        Self { title: title.into(), job_id: Some(job), ..Self::new() }
    }

    /// Whether somebody typed into this thread, rather than it being the
    /// transcript of a routine run or a job.
    pub fn is_chat(&self) -> bool {
        self.run_id.is_none() && self.job_id.is_none()
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Who stands behind it. Everything written before this field existed
    /// was told, which is what the default says.
    #[serde(default)]
    pub origin: MemoryOrigin,
    /// The last day the data still supported an inferred memory. A dream
    /// moves it forward or lets it lapse; eviction among inferred memories
    /// is oldest-supported first. Meaningless for any other origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_supported: Option<jiff::civil::Date>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Where a memory came from, and so how much weight it carries.
///
/// Told and confirmed memories are standing instructions. An inferred one is
/// an observation a dream made, used lightly and under a heading that says
/// it may be wrong. A rejected one is kept, as "do not assume", so that a
/// later dream cannot learn it again. See `docs/plans/dreaming.md`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOrigin {
    /// The person said it, or the assistant was asked to keep it.
    #[default]
    Told,
    /// A dream noticed it.
    Inferred,
    /// Inferred, and then the person agreed or rewrote it.
    Confirmed,
    /// Inferred, and then the person struck it out.
    Rejected,
}

/// How many inferred memories are kept, on top of [`MAX_MEMORIES`].
///
/// Smaller than the told list, and evicted oldest-supported first: the one
/// yesterday's data confirmed is the one to keep.
pub const MAX_INFERRED: usize = 24;

/// How many memories are kept, and how many are loaded into a prompt.
///
/// See the module docs for why this is small and fixed.
pub const MAX_MEMORIES: usize = 64;

/// How many rejected memories are kept, so a later dream cannot re-learn what
/// somebody already struck out.
///
/// A "do not assume" list has the same reason to be bounded as the other
/// two: it is loaded into every prompt, and the prompt must stay a fixed
/// size whatever the origin of the line. Evicted oldest first, same as the
/// told and confirmed group, and a pinned memory is never the one dropped --
/// see [`Vault::save_memory`](crate::vault::Vault::save_memory).
pub const MAX_REJECTED: usize = 32;

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
            origin: MemoryOrigin::Told,
            last_supported: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn from_conversation(text: impl Into<String>, source: ConversationId) -> Self {
        Self { source_id: Some(source), ..Self::new(text) }
    }

    /// A memory a dream inferred, supported by the data as of `on`.
    pub fn inferred(text: impl Into<String>, on: jiff::civil::Date) -> Self {
        Self { origin: MemoryOrigin::Inferred, last_supported: Some(on), ..Self::new(text) }
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

impl Timestamped for Memory {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// A process the assistant was given for a certain kind of request. See the
/// module docs' "Skills, and progressive disclosure".
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub id: SkillId,
    /// What to call it — "Plan a trip", "Weekly review". Shown in Settings
    /// and in the prompt's index, and what `read_skill` is given to find it
    /// by.
    pub name: String,
    /// One or two sentences on *when* to reach for this skill — "Use when
    /// asked to plan a trip or a multi-day journey." Read by a model
    /// choosing between however many skills exist, so it has to name the
    /// trigger rather than describe what happens once it is loaded; the
    /// latter is what [`instructions`](Self::instructions) is for.
    pub description: String,
    /// The process itself, in as much prose as it takes. Never sent with the
    /// prompt — only the name and description are — so this is free to be
    /// long. Markdown, like a note: numbered steps, a checklist, whatever
    /// reads clearly to the person who wrote it, because the model reading
    /// it back is the same one that will have read a note.
    pub instructions: String,
    /// Off skills are left out of the prompt's index and cannot be loaded —
    /// for a process written for an occasion that has passed, kept rather
    /// than deleted.
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Longest a skill's name may be. A label, not a sentence — matches
/// [`crate::routine::MAX_NAME_BYTES`], which a routine's own name is held to
/// for the same reason.
pub const MAX_SKILL_NAME_BYTES: usize = 200;

/// Longest a skill's description may be. Short on purpose: this is the one
/// part of a skill that is sent with every single prompt, however many
/// skills exist, so it has to stay a sentence or two rather than grow into a
/// summary of the instructions it stands in for.
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 400;

/// Longest a skill's instructions may be. Generous, and finite for the usual
/// reason: this is read in full the moment `read_skill` is called, and a
/// person who pastes a book into it pays for that book on every turn that
/// loads it.
pub const MAX_SKILL_INSTRUCTIONS_BYTES: usize = 20_000;

impl Skill {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: SkillId::new(),
            name: name.into(),
            description: String::new(),
            instructions: String::new(),
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Invalid("a skill needs a name".into()));
        }
        if self.name.len() > MAX_SKILL_NAME_BYTES {
            return Err(Error::Invalid(format!(
                "a skill's name must be under {MAX_SKILL_NAME_BYTES} bytes"
            )));
        }
        if self.description.trim().is_empty() {
            return Err(Error::Invalid(
                "a skill needs a description of when to use it, or the assistant has no way to \
                 recognise that it applies"
                    .into(),
            ));
        }
        if self.description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS {
            return Err(Error::Invalid(format!(
                "a skill's description must be under {MAX_SKILL_DESCRIPTION_CHARS} characters; \
                 say when to use it, and put the rest in the instructions"
            )));
        }
        if self.instructions.trim().is_empty() {
            return Err(Error::Invalid(
                "a skill needs instructions: what should the assistant actually do?".into(),
            ));
        }
        if self.instructions.len() > MAX_SKILL_INSTRUCTIONS_BYTES {
            return Err(Error::Invalid(format!(
                "instructions are {} bytes; the limit is {MAX_SKILL_INSTRUCTIONS_BYTES}",
                self.instructions.len()
            )));
        }
        Ok(())
    }
}

impl Timestamped for Skill {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}
