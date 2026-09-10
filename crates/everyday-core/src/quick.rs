//! The quick model: small jobs, one round trip, a schema on the way back.
//!
//! The assistant in [`agent`](crate::agent) is a reasoning engine with thirty
//! tools, a system prompt carrying the profile and every memory, and a budget
//! of twenty-four turns. It is the right shape for "look at my week and tell
//! me what I am dropping" and entirely the wrong shape for "this is a wine,
//! what are its fields". The second question wants no tools, no memories, no
//! conversation, one request, a rigid schema on the way back and an answer
//! inside a second — and it wants to cost approximately nothing, because it
//! is asked every time somebody types into a capture box.
//!
//! That is a different model, not a different prompt, which is why
//! [`AgentSettings`](crate::agent::AgentSettings) holds two of them against
//! one [`LLMProviderConfig`](crate::agent::LLMProviderConfig).
//!
//! # The split, for the fourth time
//!
//! ```text
//!   everyday_core::quick      builds the prompt, declares the schema,
//!                             parses and clamps the answer -- and cannot
//!                             open a socket, which is why all of it is
//!                             under test, offline, deterministically
//!   everyday_service::quick   owns the socket. One request, no tools, a
//!                             short timeout, no retry
//!   ui/src/lib/quick.ts       debouncing, cancellation, and a suggestion
//!                             you can dismiss
//! ```
//!
//! The same split [`ics`](crate::ics), [`websearch`](crate::websearch) and
//! [`agent::tools`](crate::agent::tools) make, and for the same payoff: "the
//! quick model put the author in the year field" is a test to write rather
//! than a network trace to capture.
//!
//! # A job is a record
//!
//! [`QuickJob`] is a name, a label, the sentence settings shows about what it
//! sends, a system instruction and a JSON schema. [`JOBS`] is the catalogue.
//! The service never interprets a job: it is handed a [`Prompt`] and posts
//! it. Adding the twentieth use of the quick model is a record in this file
//! and a test beside it, not a new command, not a new setting and not a
//! change to anything above.
//!
//! # The four rules
//!
//! The library's metadata feature earned four rules and they are the reason
//! it is a good feature rather than an intrusive one. Everything here
//! inherits all four, and they bind harder because this touches capture
//! boxes rather than a button somebody pressed.
//!
//! - **Nothing waits on it.** The task is created, the item is shelved, the
//!   entry is saved — and the suggestion arrives afterwards or does not
//!   arrive. There is no spinner in front of an Enter key. This is what
//!   decides the interaction everywhere: a chip you tap, never a field that
//!   fills itself while you look at it.
//! - **It fills gaps and never argues.** What you typed survives. Notes,
//!   ratings and statuses are not reachable from here at all.
//! - **It is not the assistant's switch.**
//!   [`quick_is_usable`](crate::agent::AgentSettings::quick_is_usable) does
//!   not consult `enabled`.
//! - **Every job is listable and refusable.** [`QuickPolicy`] is per-job,
//!   because "AI features: on" is not consent to having the day's prose
//!   read. The jobs that read a journal are
//!   [`default_on`](QuickJob::default_on) `false`.
//!
//! # What it is never allowed to do
//!
//! Nothing here deletes, merges or bulk-updates. Every answer in this module
//! is a *proposal* that some interface draws and a person accepts.
//! [`confirm_destructive`](crate::agent::AgentSettings::confirm_destructive)
//! defends the reasoning model from its own misreadings; the same argument
//! applies twice over to a model chosen for being cheap.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

// ── The catalogue ────────────────────────────────────────────────────────

/// Which app a job belongs to, so settings can group them the way the app
/// bar does rather than as one alphabetical list of twenty switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuickApp {
    Library,
    Todo,
    Calendar,
    Journal,
    Tracking,
    Notes,
    Purpose,
    Overview,
    Data,
}

impl QuickApp {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Todo => "Todo",
            Self::Calendar => "Calendar",
            Self::Journal => "Journal",
            Self::Tracking => "Tracking",
            Self::Notes => "Notes",
            Self::Purpose => "Roles and goals",
            Self::Overview => "Overview",
            Self::Data => "Data",
        }
    }
}

/// One thing the quick model is asked to do.
pub struct QuickJob {
    /// The stable id [`QuickPolicy`] stores. Never renamed: a rename is a
    /// switch somebody set silently reverting to its default.
    pub name: &'static str,
    /// What the settings pane calls it.
    pub label: &'static str,
    /// One line, in the settings pane, saying *what leaves the machine*.
    /// Written for somebody deciding whether to allow it, which is why every
    /// one of them names the text rather than the benefit.
    pub blurb: &'static str,
    pub app: QuickApp,
    /// Whether this job runs when nobody has said either way.
    ///
    /// `false` for everything that reads a journal entry. The prose of a day
    /// is the most private text this vault holds, and a feature that starts
    /// reading it because a different switch was turned on is the kind of
    /// default that makes people distrust the whole application.
    pub default_on: bool,
    /// The instruction. Written to be boring: what to extract, what to do
    /// when it is not there, and an explicit licence to answer with nothing.
    pub system: &'static str,
    schema: fn() -> Value,
}

impl QuickJob {
    /// JSON Schema for the answer, in the shape a structured-output request
    /// wants.
    pub fn schema(&self) -> Value {
        (self.schema)()
    }
}

impl std::fmt::Debug for QuickJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickJob").field("name", &self.name).finish()
    }
}

/// What the service posts: an instruction, an input, and the shape of the
/// answer. It carries no endpoint and no model name — those are settings,
/// and the layer that owns the socket owns them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// Which [`QuickJob`] this came from, so the service can check the
    /// policy without the caller passing it twice.
    pub job: String,
    pub system: String,
    pub user: String,
    pub schema: Value,
}

/// Everything the model is told about what it is looking at, other than the
/// job's own input.
///
/// Only ever this much: today's date and the person's zone. There is no
/// profile here, no memories and no other records — a job that needed those
/// is a job for the assistant.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickContext {
    pub today: jiff::civil::Date,
    pub tz: String,
}

impl QuickContext {
    pub fn new(today: jiff::civil::Date, tz: impl Into<String>) -> Self {
        Self { today, tz: tz.into() }
    }

    fn preamble(&self) -> String {
        format!("Today is {} ({}).", self.today, self.tz)
    }
}

/// Find a job by name.
pub fn job(name: &str) -> Option<&'static QuickJob> {
    JOBS.iter().find(|j| j.name == name)
}

/// Every job, grouped by app for the settings pane.
pub fn jobs_by_app() -> Vec<(QuickApp, Vec<&'static QuickJob>)> {
    let mut apps: Vec<QuickApp> = JOBS.iter().map(|j| j.app).collect();
    apps.sort();
    apps.dedup();
    apps.into_iter().map(|app| (app, JOBS.iter().filter(|j| j.app == app).collect())).collect()
}

// ── The policy ───────────────────────────────────────────────────────────

/// Which quick jobs may run.
///
/// Stored as the two sets that differ from the defaults rather than as a
/// list of what is on, so that a job added in a later build arrives at its
/// own default instead of arriving switched off because a settings record
/// written last year did not mention it. That is the same reasoning that
/// makes an unknown [`Source`](crate::websearch::Source) degrade to a plain
/// search rather than make a shelf unreadable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickPolicy {
    /// Jobs switched on that default off.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub allowed: BTreeSet<String>,
    /// Jobs switched off that default on.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub denied: BTreeSet<String>,
}

impl QuickPolicy {
    /// Whether a named job may run.
    ///
    /// An unknown name is refused. A job this build does not have is either
    /// a typo or a record from a newer version, and running *something* on
    /// the strength of a name we cannot look up is worse than doing nothing.
    pub fn allows(&self, name: &str) -> bool {
        let Some(job) = job(name) else { return false };
        if self.denied.contains(name) {
            return false;
        }
        job.default_on || self.allowed.contains(name)
    }

    /// Turn one job on or off, keeping the record to the difference from the
    /// defaults.
    pub fn set(&mut self, name: &str, on: bool) {
        let Some(job) = job(name) else { return };
        self.allowed.remove(name);
        self.denied.remove(name);
        if on && !job.default_on {
            self.allowed.insert(name.to_string());
        } else if !on && job.default_on {
            self.denied.insert(name.to_string());
        }
    }
}

// ── Building a prompt ────────────────────────────────────────────────────

impl Prompt {
    fn new(job: &'static QuickJob, ctx: &QuickContext, user: impl Into<String>) -> Self {
        Self {
            job: job.name.to_string(),
            system: format!("{}\n\n{}", job.system, HOUSE_RULES),
            user: format!("{}\n\n{}", ctx.preamble(), user.into()),
            schema: job.schema(),
        }
    }
}

/// Appended to every job's own instruction.
///
/// The second sentence is the load-bearing one. A model asked to extract a
/// year from a paragraph that does not contain one will supply a plausible
/// year unless it is told, in as many words, that saying nothing is a
/// correct answer — and a plausible wrong year in a shelf is worse than an
/// empty field, because nobody goes back to check a field that is filled.
const HOUSE_RULES: &str = "\
Answer only with the JSON object the schema describes, and nothing else.
Leave a field out, or null, when the input does not actually say. Guessing \
is worse than an empty field here: the answer is shown to somebody who will \
not re-check a field that looks filled in. Never invent an identifier, a \
date or a number that is not in what you were given. Do not repeat back \
anything the person already typed.";

/// How much of any one document is worth sending.
///
/// A cap rather than a nicety: these prompts are built from things a person
/// wrote, and a journal entry or a note has no length limit at all. Sending
/// the first few thousand characters answers every job in this file — the
/// title, the tags, the numbers — and sending a novel would cost real money
/// on a job advertised as free.
pub const MAX_INPUT_CHARS: usize = 6_000;

/// Trim a document to something a quick job can be asked about.
pub fn excerpt(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_INPUT_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_INPUT_CHARS).collect();
    match cut.rsplit_once(char::is_whitespace) {
        Some((head, _)) if head.chars().count() >= MAX_INPUT_CHARS / 2 => head.to_string(),
        _ => cut,
    }
}

// ── Answers ──────────────────────────────────────────────────────────────

/// A short piece of text the model was asked for: a title, a rephrasing, a
/// paragraph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextAnswer {
    /// Empty when the model correctly declined.
    #[serde(default)]
    pub text: String,
}

fn text_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": { "type": "string", "description": "The answer, or an empty string." }
        },
        "required": ["text"],
        "additionalProperties": false,
    })
}

/// One suggestion in a list of them, as the interface draws it: a label to
/// put on a chip and the payload accepting it applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion<T> {
    /// What the chip says. Short enough to sit in a row of them.
    pub label: String,
    pub value: T,
}

/// Parse a model's reply into `T`, with the job named in any error.
///
/// A quick job that comes back malformed is not an error a person should
/// see: the suggestion simply does not appear. The message is for the log.
pub fn parse<T: serde::de::DeserializeOwned>(job: &str, value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| Error::Invalid(format!("the quick model's answer to {job} did not fit: {e}")))
}

// ── Answer shapes ────────────────────────────────────────────────────────
//
// Deliberately few. Twenty-one jobs share eleven shapes, because a shape is
// a parser, a validator and a component in the interface, and twenty-one of
// each would be twenty-one places for the same clamp to be forgotten.
//
// None of them carries an identifier the model chose. Where a job has to
// name an existing record it is given a numbered list and answers with the
// number, and the mapping back to an id happens here — the same rule
// `agent::tools` follows for the same reason: "the model picked the wrong
// task" must not be able to become "the model invented a task id".

/// Fields pulled out of a page: what a shelf wants to know about a thing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldsAnswer {
    #[serde(default)]
    pub creator: String,
    #[serde(default)]
    pub year: Option<i16>,
    #[serde(default)]
    pub summary: String,
    /// Keyed to the field keys the kind declared. Anything else is dropped
    /// by [`FieldsAnswer::clamp`] rather than stored, because a fact with no
    /// field to live in is invisible in the interface and undeletable from
    /// it — the same rule `websearch::apply` enforces.
    #[serde(default)]
    pub facts: std::collections::BTreeMap<String, String>,
}

impl FieldsAnswer {
    /// Drop everything this kind has no field for, and everything blank.
    pub fn clamp(&mut self, field_keys: &[String]) {
        self.facts.retain(|k, v| field_keys.iter().any(|f| f == k) && !v.trim().is_empty());
        self.creator = self.creator.trim().to_string();
        self.summary = self.summary.trim().to_string();
        // A year outside this range is a page number or a run time that the
        // model put in the wrong slot, which is the commonest way this
        // particular field goes wrong.
        if self.year.is_some_and(|y| !(1000..=2200).contains(&y)) {
            self.year = None;
        }
    }
}

fn fields_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "creator": { "type": "string", "description": "Author, director, artist, studio — whoever the page names as responsible. Empty if it does not say." },
            "year": { "type": ["integer", "null"], "description": "Year of publication or release. Null unless the page states it." },
            "summary": { "type": "string", "description": "One or two sentences describing the thing itself. Not a description of the web page." },
            "facts": {
                "type": "object",
                "description": "Only the field keys you were given, and only where the page actually says.",
                "additionalProperties": { "type": "string" }
            }
        },
        "required": ["creator", "year", "summary", "facts"],
        "additionalProperties": false,
    })
}

/// Which of several candidates is the thing, if any of them is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickAnswer {
    /// Position in the list as it was numbered, from 1. `None` when none of
    /// them is the thing — which is the half of this job that earns its
    /// keep, because filling a film's fields from a Blu-ray listing is worse
    /// than filling nothing.
    #[serde(default)]
    pub choice: Option<usize>,
}

impl PickAnswer {
    /// The index into the original slice, or `None` if the model abstained
    /// or named a position that was not offered.
    pub fn index(&self, len: usize) -> Option<usize> {
        self.choice.filter(|n| *n >= 1 && *n <= len).map(|n| n - 1)
    }
}

fn pick_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "choice": {
                "type": ["integer", "null"],
                "description": "The number of the entry that is the thing being looked for, or null if none of them is."
            }
        },
        "required": ["choice"],
        "additionalProperties": false,
    })
}

/// A proposed shelf: what "Wines" should look like before anybody edits it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindDraft {
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub color: String,
    /// The four words this shelf uses instead of "in progress": the wishlist
    /// verb, the active verb, the done verb, and the singular noun.
    #[serde(default)]
    pub wishlist_verb: String,
    #[serde(default)]
    pub active_verb: String,
    #[serde(default)]
    pub done_verb: String,
    #[serde(default)]
    pub item_noun: String,
    #[serde(default)]
    pub fields: Vec<KindFieldDraft>,
    /// Which [`Source`](crate::websearch::Source) to ask, by slug.
    #[serde(default)]
    pub source: String,
}

/// One proposed extra field on a shelf.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindFieldDraft {
    pub key: String,
    pub label: String,
}

/// Most extra fields a draft may propose.
///
/// Six is a shelf; sixteen is a form. A model asked to describe wines will
/// happily produce Producer, Vintage, Region, Appellation, Grape, ABV,
/// Closure, Importer, Drink-by and Cellar Location, and the person who
/// wanted a wine list now has a data-entry job.
pub const MAX_DRAFT_FIELDS: usize = 6;

impl KindDraft {
    /// Make a draft safe to hand to the form: sane keys, a real colour, and
    /// no more fields than anybody will fill in.
    pub fn clamp(&mut self) {
        self.fields.truncate(MAX_DRAFT_FIELDS);
        for field in &mut self.fields {
            field.key = slug(&field.key);
            field.label = field.label.trim().to_string();
        }
        self.fields.retain(|f| !f.key.is_empty() && !f.label.is_empty());
        // Keys are what facts are stored under, so two fields sharing one
        // would be a field that silently overwrites its neighbour.
        let mut seen = BTreeSet::new();
        self.fields.retain(|f| seen.insert(f.key.clone()));
        if !self.color.starts_with('#') || self.color.len() != 7 {
            self.color.clear();
        }
        // An icon is one glyph. A model that answers "🍷 (a wine glass)"
        // would otherwise put the parenthesis in the sidebar.
        self.icon = self.icon.trim().chars().next().map(String::from).unwrap_or_default();
    }
}

/// Lowercase, alphanumeric and underscores: what a fact key has to be to be
/// matched against a kind's declared fields.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

fn kind_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "icon": { "type": "string", "description": "A single emoji." },
            "color": { "type": "string", "description": "A hex colour like #7A5CFF." },
            "wishlistVerb": { "type": "string", "description": "What the shelf calls something not started: 'To read', 'To try'." },
            "activeVerb": { "type": "string", "description": "In progress: 'Reading', 'Tasting'." },
            "doneVerb": { "type": "string", "description": "Finished: 'Read', 'Tasted'." },
            "itemNoun": { "type": "string", "description": "One of them, singular: 'book', 'wine'." },
            "fields": {
                "type": "array",
                "description": "At most six extra fields worth recording. Only ones somebody would actually fill in.",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string", "description": "lower_snake_case." },
                        "label": { "type": "string" }
                    },
                    "required": ["key", "label"],
                    "additionalProperties": false
                }
            },
            "source": {
                "type": "string",
                "description": "Which catalogue suits this shelf best.",
                "enum": ["web", "wikipedia", "openLibrary", "itunes", "nominatim"]
            }
        },
        "required": ["icon", "color", "wishlistVerb", "activeVerb", "doneVerb", "itemNoun", "fields", "source"],
        "additionalProperties": false,
    })
}

/// Tags, and which role or goal a record serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelsAnswer {
    #[serde(default)]
    pub tags: Vec<String>,
    /// Position in the numbered list of purposes it was offered, from 1.
    /// `None` when none of them fits, which is the common and correct answer
    /// — a great deal of what anybody does serves no stated goal.
    #[serde(default)]
    pub purpose: Option<usize>,
}

/// Most tags a suggestion may carry. A record with nine tags is a record
/// with none.
pub const MAX_SUGGESTED_TAGS: usize = 4;

impl LabelsAnswer {
    /// Keep only tags that are not already on the record, normalised the way
    /// the rest of the app writes them.
    pub fn clamp(&mut self, existing: &[String]) {
        for tag in &mut self.tags {
            *tag = tag.trim().trim_start_matches('#').to_lowercase();
        }
        self.tags.retain(|t| !t.is_empty() && !existing.iter().any(|e| e.eq_ignore_ascii_case(t)));
        let mut seen = BTreeSet::new();
        self.tags.retain(|t| seen.insert(t.clone()));
        self.tags.truncate(MAX_SUGGESTED_TAGS);
    }
}

fn labels_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "description": "At most four short lowercase tags. Prefer ones already in use.",
                "items": { "type": "string" }
            },
            "purpose": {
                "type": ["integer", "null"],
                "description": "The number of the role or goal this serves, or null if none of them plainly does."
            }
        },
        "required": ["tags", "purpose"],
        "additionalProperties": false,
    })
}

/// A task the model proposes: from a sentence, from a note, or as one step
/// of a bigger thing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDraft {
    pub title: String,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub due_time: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub estimate_minutes: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Most tasks one suggestion may propose.
///
/// A note that would become forty tasks is a note, and turning it into forty
/// rows somebody now has to delete is not help.
pub const MAX_DRAFT_TASKS: usize = 12;

impl TaskDraft {
    /// Reject a draft that is not usable, and clamp the parts that are.
    pub fn clamp(&mut self) -> bool {
        self.title = self.title.trim().to_string();
        if self.title.is_empty() {
            return false;
        }
        self.due_date = self.due_date.take().filter(|d| is_iso_date(d));
        self.due_time = self.due_time.take().filter(|t| is_clock(t));
        self.priority = self
            .priority
            .take()
            .map(|p| p.trim().to_lowercase())
            .filter(|p| matches!(p.as_str(), "none" | "low" | "medium" | "high" | "urgent"));
        // A week is not an estimate, it is a project. The cap is what stops
        // "write the book" coming back as 4800 minutes and being drawn on a
        // day grid.
        self.estimate_minutes = self.estimate_minutes.filter(|m| *m > 0 && *m <= 60 * 24);
        for tag in &mut self.tags {
            *tag = tag.trim().trim_start_matches('#').to_lowercase();
        }
        self.tags.retain(|t| !t.is_empty());
        self.tags.truncate(MAX_SUGGESTED_TAGS);
        true
    }
}

fn task_properties() -> Value {
    json!({
        "title": { "type": "string", "description": "The task, as a verb phrase. Do not include the #tag !priority ~estimate @date shorthand — use the fields." },
        "dueDate": { "type": ["string", "null"], "description": "YYYY-MM-DD. Null unless the input actually says when." },
        "dueTime": { "type": ["string", "null"], "description": "HH:MM. Null unless a clock time was given." },
        "priority": { "type": ["string", "null"], "enum": ["none", "low", "medium", "high", "urgent", null] },
        "estimateMinutes": { "type": ["integer", "null"] },
        "tags": { "type": "array", "items": { "type": "string" } }
    })
}

fn task_schema() -> Value {
    json!({
        "type": "object",
        "properties": task_properties(),
        "required": ["title", "dueDate", "dueTime", "priority", "estimateMinutes", "tags"],
        "additionalProperties": false,
    })
}

fn tasks_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "description": "At most twelve. Only things somebody actually has to do.",
                "items": {
                    "type": "object",
                    "properties": task_properties(),
                    "required": ["title", "dueDate", "dueTime", "priority", "estimateMinutes", "tags"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["tasks"],
        "additionalProperties": false,
    })
}

/// A list of task drafts, so the schema has an object at its root — which
/// several providers' structured-output modes insist on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksAnswer {
    #[serde(default)]
    pub tasks: Vec<TaskDraft>,
}

impl TasksAnswer {
    pub fn clamp(&mut self) {
        self.tasks.retain_mut(|t| t.clamp());
        self.tasks.truncate(MAX_DRAFT_TASKS);
    }
}

/// Something with a time on it: a meeting, an appointment, a block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDraft {
    pub title: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub location: String,
}

impl EventDraft {
    pub fn clamp(&mut self) -> bool {
        self.title = self.title.trim().to_string();
        self.location = self.location.trim().to_string();
        self.date = self.date.take().filter(|d| is_iso_date(d));
        self.start = self.start.take().filter(|t| is_clock(t));
        self.end = self.end.take().filter(|t| is_clock(t));
        // An end before its start is the commonest way a parsed time goes
        // wrong, and it draws as a block of negative height.
        if let (Some(s), Some(e)) = (&self.start, &self.end)
            && e <= s
        {
            self.end = None;
        }
        !self.title.is_empty() && self.date.is_some()
    }
}

fn event_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "date": { "type": ["string", "null"], "description": "YYYY-MM-DD." },
            "start": { "type": ["string", "null"], "description": "HH:MM." },
            "end": { "type": ["string", "null"], "description": "HH:MM. Null unless a duration or end time was given." },
            "location": { "type": "string" }
        },
        "required": ["title", "date", "start", "end", "location"],
        "additionalProperties": false,
    })
}

/// A number the model found in a sentence, against a tracker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingDraft {
    /// Position in the numbered list of existing trackers, from 1. `None`
    /// means it is proposing a new one, named by `name`.
    #[serde(default)]
    pub tracker: Option<usize>,
    /// Only read when `tracker` is `None`.
    #[serde(default)]
    pub name: String,
    pub value: f64,
    /// `HH:MM`, when the sentence said. Readings know their day always and
    /// their hour only sometimes — see [`Reading::at`](crate::tracker::Reading).
    #[serde(default)]
    pub at: Option<String>,
    /// What the chip says, so a person can accept it without reasoning about
    /// units: "Ibuprofen 400mg, 8am".
    #[serde(default)]
    pub label: String,
}

/// Most readings one entry may yield.
///
/// A day's prose mentioning six numbers is normal; a model finding fifteen
/// has started counting adjectives.
pub const MAX_DRAFT_READINGS: usize = 6;

impl ReadingDraft {
    pub fn clamp(&mut self) -> bool {
        self.name = self.name.trim().to_string();
        self.label = self.label.trim().to_string();
        self.at = self.at.take().filter(|t| is_clock(t));
        if !self.value.is_finite() {
            return false;
        }
        self.tracker.is_some() || !self.name.is_empty()
    }
}

/// Readings found in a piece of writing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingsAnswer {
    #[serde(default)]
    pub readings: Vec<ReadingDraft>,
}

impl ReadingsAnswer {
    pub fn clamp(&mut self) {
        self.readings.retain_mut(|r| r.clamp());
        self.readings.truncate(MAX_DRAFT_READINGS);
    }
}

fn reading_properties() -> Value {
    json!({
        "tracker": { "type": ["integer", "null"], "description": "The number of the existing tracker this belongs to, or null to propose a new one." },
        "name": { "type": "string", "description": "Only when tracker is null: what the new tracker should be called." },
        "value": { "type": "number", "description": "The number itself, in the tracker's own unit." },
        "at": { "type": ["string", "null"], "description": "HH:MM, only if the writing says when." },
        "label": { "type": "string", "description": "A short phrase for the button that accepts this, e.g. 'Ibuprofen 400mg, 08:00'." }
    })
}

fn readings_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "readings": {
                "type": "array",
                "description": "Only numbers the writing actually states. An empty list is the right answer for most writing.",
                "items": {
                    "type": "object",
                    "properties": reading_properties(),
                    "required": ["tracker", "name", "value", "at", "label"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["readings"],
        "additionalProperties": false,
    })
}

fn reading_schema() -> Value {
    json!({
        "type": "object",
        "properties": reading_properties(),
        "required": ["tracker", "name", "value", "at", "label"],
        "additionalProperties": false,
    })
}

/// A proposed tracker, for the one that gets made by the act of recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerDraft {
    pub name: String,
    /// `check`, `dose`, `scale` or `amount`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub scale_max: Option<u8>,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub color: String,
}

impl TrackerDraft {
    pub fn clamp(&mut self) -> bool {
        self.name = self.name.trim().to_string();
        self.unit = self.unit.trim().to_string();
        self.kind = self.kind.trim().to_lowercase();
        if !matches!(self.kind.as_str(), "check" | "dose" | "scale" | "amount") {
            self.kind = "amount".into();
        }
        // Only a scale has a bound, and a bound of nought or one is not a
        // scale.
        self.scale_max = match self.kind.as_str() {
            "scale" => self.scale_max.filter(|m| (2..=100).contains(m)).or(Some(10)),
            _ => None,
        };
        if !self.color.starts_with('#') || self.color.len() != 7 {
            self.color.clear();
        }
        !self.name.is_empty()
    }
}

fn tracker_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "kind": {
                "type": "string",
                "enum": ["check", "dose", "scale", "amount"],
                "description": "check: done or not. dose: a medication taken. scale: a severity out of scaleMax. amount: a quantity in a unit."
            },
            "unit": { "type": "string", "description": "mg, km, minutes. Empty for a check or a scale." },
            "scaleMax": { "type": ["integer", "null"], "description": "Only for a scale." },
            "icon": { "type": "string", "description": "A single emoji." },
            "color": { "type": "string", "description": "A hex colour like #7A5CFF." }
        },
        "required": ["name", "kind", "unit", "scaleMax", "icon", "color"],
        "additionalProperties": false,
    })
}

/// How long something will take.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateAnswer {
    #[serde(default)]
    pub minutes: Option<u32>,
}

impl EstimateAnswer {
    pub fn clamp(&mut self) {
        self.minutes = self.minutes.filter(|m| *m > 0 && *m <= 60 * 24);
    }
}

fn estimate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "minutes": { "type": ["integer", "null"], "description": "Whole minutes of focused work, or null if there is nothing to go on." }
        },
        "required": ["minutes"],
        "additionalProperties": false,
    })
}

/// Somebody else's column names, mapped onto ours.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingAnswer {
    /// Ours to theirs. Ours is the key because a field of ours can only be
    /// filled once, and two of their columns claiming one of ours is the
    /// ambiguity worth losing rather than resolving silently.
    #[serde(default)]
    pub columns: std::collections::BTreeMap<String, String>,
}

impl MappingAnswer {
    /// Drop anything naming a column or a field that was not offered. A
    /// mapping is applied over every row of somebody's entire history, so a
    /// hallucinated column name is thousands of wrong records rather than
    /// one.
    pub fn clamp(&mut self, ours: &[String], theirs: &[String]) {
        self.columns.retain(|k, v| ours.iter().any(|o| o == k) && theirs.iter().any(|t| t == v));
    }
}

fn mapping_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "columns": {
                "type": "object",
                "description": "Keys are our field names, values are their column names. Leave ours out entirely when nothing of theirs matches.",
                "additionalProperties": { "type": "string" }
            }
        },
        "required": ["columns"],
        "additionalProperties": false,
    })
}

// ── Small validators ─────────────────────────────────────────────────────
//
// A model asked for `YYYY-MM-DD` will occasionally answer `next Tuesday`, and
// a date that reaches the store as a string nobody parsed is a record that
// looks fine until something sorts by it.

fn is_iso_date(text: &str) -> bool {
    text.parse::<jiff::civil::Date>().is_ok()
}

fn is_clock(text: &str) -> bool {
    let Some((h, m)) = text.split_once(':') else { return false };
    let m = m.split_once(':').map(|(m, _)| m).unwrap_or(m);
    matches!((h.parse::<u8>(), m.parse::<u8>()), (Ok(h), Ok(m)) if h < 24 && m < 60)
}

// ── The jobs ─────────────────────────────────────────────────────────────

/// Every job the quick model has, in the order the settings pane lists them.
///
/// The blurbs are written for somebody deciding whether to allow one, which
/// is why each names *what leaves the machine* rather than what is gained.
pub static JOBS: &[QuickJob] = &[
    // -- library ----------------------------------------------------------
    QuickJob {
        name: "library.fields",
        label: "Fill in a shelf's fields",
        blurb: "Sends the title you looked up and the search results it found.",
        app: QuickApp::Library,
        default_on: true,
        system: "You read search results about one thing and pull out what a \
            catalogue would record about it. The thing is what the results are \
            *about* — not the web page, not the shop selling it. Use only the \
            field keys you are given.",
        schema: fields_schema,
    },
    QuickJob {
        name: "library.pick",
        label: "Choose the right search result",
        blurb: "Sends what you typed and the titles of the results found.",
        app: QuickApp::Library,
        default_on: true,
        system: "You choose which numbered search result is the thing somebody \
            meant, given what they typed and which shelf they are adding it to. \
            Answer null unless one of them plainly is it. A near-miss filled \
            into a shelf is worse than an empty shelf entry, because nobody \
            re-checks a field that looks filled in.",
        schema: pick_schema,
    },
    QuickJob {
        name: "library.kind",
        label: "Draft a new shelf",
        blurb: "Sends the shelf name you typed, and nothing else.",
        app: QuickApp::Library,
        default_on: true,
        system: "You design a shelf for a personal library. Given its name, \
            propose an icon, a colour, the words this shelf uses instead of \
            'in progress', and at most six extra fields worth recording. \
            The verbs matter: a person reads a book, watches a series, plays a \
            game and visits a place, and 'in progress' for all four reads like \
            a form. Propose fields somebody would actually fill in, not every \
            field that could exist.",
        schema: kind_schema,
    },
    QuickJob {
        name: "library.import_map",
        label: "Map an imported list's columns",
        blurb: "Sends the column names of the file you are importing, and one example row.",
        app: QuickApp::Library,
        default_on: true,
        system: "You map the columns of somebody's exported list onto the \
            fields of a shelf. Leave one of ours out entirely rather than \
            mapping it to a column that only nearly matches — this mapping is \
            applied to every row of their history.",
        schema: mapping_schema,
    },
    // -- todo -------------------------------------------------------------
    QuickJob {
        name: "todo.purpose",
        label: "Suggest a role or goal",
        blurb: "Sends the task's title and the names of your roles and goals.",
        app: QuickApp::Todo,
        default_on: true,
        system: "You say which of somebody's roles or goals a task serves, and \
            propose tags for it. Answer null for the purpose unless one \
            plainly fits — most of what anybody does serves no stated goal, and \
            a wrong purpose quietly corrupts the report that counts them.",
        schema: labels_schema,
    },
    QuickJob {
        name: "todo.parse",
        label: "Read a task written as a sentence",
        blurb: "Sends the line you typed into the capture box.",
        app: QuickApp::Todo,
        default_on: true,
        system: "You turn one sentence into one task. The person writes in \
            their own words; pull out when it is due, how long it will take and \
            how urgent it is, and leave the rest in the title. Never invent a \
            date the sentence does not imply.",
        schema: task_schema,
    },
    QuickJob {
        name: "todo.subtasks",
        label: "Break a task into steps",
        blurb: "Sends the task's title and description.",
        app: QuickApp::Todo,
        default_on: true,
        system: "You break one task into the steps it is actually made of. \
            Each step is a thing somebody does in one sitting. Do not restate \
            the task as its own first step, and do not pad the list to look \
            thorough — three real steps beat eight invented ones.",
        schema: tasks_schema,
    },
    QuickJob {
        name: "todo.estimate",
        label: "Suggest how long a task will take",
        blurb: "Sends the task's title and how long similar past tasks took.",
        app: QuickApp::Todo,
        default_on: true,
        system: "You estimate how long a task will take, in minutes of \
            focused work. You are given what this person's similar tasks \
            actually took — trust that over your own sense of the work. Answer \
            null when there is nothing to go on.",
        schema: estimate_schema,
    },
    // -- calendar ---------------------------------------------------------
    QuickJob {
        name: "calendar.parse",
        label: "Read an appointment written as a sentence",
        blurb: "Sends the line you typed into the capture box.",
        app: QuickApp::Calendar,
        default_on: true,
        system: "You turn one sentence into one appointment. Give an end time \
            only when a duration or an end was stated. Never invent a date the \
            sentence does not imply.",
        schema: event_schema,
    },
    QuickJob {
        name: "calendar.title",
        label: "Tidy a subscribed event's title",
        blurb: "Sends the titles of events from calendars you subscribe to.",
        app: QuickApp::Calendar,
        default_on: false,
        system: "You rewrite a calendar event's title as the thing it is, \
            stripping forwarding prefixes, reply markers, conferencing \
            boilerplate and bracketed tags. Keep the people and the subject. \
            Answer with an empty string if it is already clean — an unchanged \
            title is the common case and the right one.",
        schema: text_schema,
    },
    // -- journal ----------------------------------------------------------
    QuickJob {
        name: "journal.readings",
        label: "Find numbers worth tracking in an entry",
        blurb: "Sends the text of the journal entry you just wrote.",
        app: QuickApp::Journal,
        default_on: false,
        system: "You find the numbers a day's writing states — a dose taken, \
            hours slept, a distance, a severity out of ten — and match each to \
            one of the trackers already set up, by number. Only propose a new \
            tracker for something the person plainly records on purpose. An \
            empty list is the right answer for most writing, and a number \
            invented from a mood is worse than no number at all.",
        schema: readings_schema,
    },
    QuickJob {
        name: "journal.title",
        label: "Suggest a title for an entry",
        blurb: "Sends the text of the journal entry you just wrote.",
        app: QuickApp::Journal,
        default_on: false,
        system: "You title a day's writing in at most eight words, in the \
            writer's own register. Not a summary and not a headline: the phrase \
            they would use to find this day again.",
        schema: text_schema,
    },
    QuickJob {
        name: "journal.labels",
        label: "Suggest tags for an entry",
        blurb: "Sends the text of the journal entry you just wrote.",
        app: QuickApp::Journal,
        default_on: false,
        system: "You propose tags for a day's writing, and say which role or \
            goal the day served. Prefer tags already in use over new ones. \
            Answer null for the purpose unless one plainly fits.",
        schema: labels_schema,
    },
    // -- notes ------------------------------------------------------------
    QuickJob {
        name: "notes.title",
        label: "Suggest a title for a note",
        blurb: "Sends the text of the note.",
        app: QuickApp::Notes,
        default_on: true,
        system: "You title a note in at most eight words: the name somebody \
            would look for it under, not a summary of it.",
        schema: text_schema,
    },
    QuickJob {
        name: "notes.tasks",
        label: "Find the tasks in a note",
        blurb: "Sends the text of the note.",
        app: QuickApp::Notes,
        default_on: true,
        system: "You find the things somebody has to *do* in a page of notes \
            — commitments, actions, follow-ups — and write each as one task. \
            Ignore decisions, background and anything assigned to somebody \
            else. An empty list is a fine answer.",
        schema: tasks_schema,
    },
    QuickJob {
        name: "notes.labels",
        label: "Suggest tags for a note",
        blurb: "Sends the text of the note.",
        app: QuickApp::Notes,
        default_on: true,
        system: "You propose tags for a note, and say which role or goal it \
            serves. Prefer tags already in use. Answer null for the purpose \
            unless one plainly fits.",
        schema: labels_schema,
    },
    // -- trackers ---------------------------------------------------------
    QuickJob {
        name: "tracker.parse",
        label: "Read a reading written as a sentence",
        blurb: "Sends the line you typed, and the names of your trackers.",
        app: QuickApp::Tracking,
        default_on: true,
        system: "You turn one sentence into one recorded number, matched to \
            an existing tracker by its number where there is one. Convert to \
            the tracker's own unit when you can see what it is. If the \
            sentence names no number at all, the value is 1 — 'went for a \
            swim' is a thing done, not a quantity.",
        schema: reading_schema,
    },
    QuickJob {
        name: "tracker.draft",
        label: "Propose a new tracker's settings",
        blurb: "Sends the tracker name and the line you first recorded against it.",
        app: QuickApp::Tracking,
        default_on: true,
        system: "You propose how a new tracker should be set up, from its \
            name and the first thing recorded against it. A dose is \
            medication taken on purpose; anything else measured in a unit is \
            an amount. Say 'dose' only when it is plainly a medicine — a \
            wrong guess here is invisible until a chart is drawn a month \
            later.",
        schema: tracker_schema,
    },
    // -- roles and goals --------------------------------------------------
    QuickJob {
        name: "purpose.goal",
        label: "Sharpen a goal's wording",
        blurb: "Sends the goal you typed and the role it sits under.",
        app: QuickApp::Purpose,
        default_on: true,
        system: "You rewrite a goal as an outcome somebody could tell they had \
            reached, keeping their words and their scale. 'Get fitter' becomes \
            'Run 10k without stopping'. Do not add a deadline they did not \
            give, and answer with an empty string if it is already an outcome.",
        schema: text_schema,
    },
    QuickJob {
        name: "purpose.backfill",
        label: "Match existing records to a new goal",
        blurb: "Sends the goal's name and the titles of records that have no purpose yet.",
        app: QuickApp::Purpose,
        default_on: true,
        system: "You say which of somebody's existing records serve a goal \
            they have just written down. Be strict: a goal that starts life \
            claiming half the vault is worse than one that starts empty.",
        schema: labels_schema,
    },
    // -- overview ---------------------------------------------------------
    QuickJob {
        name: "overview.week",
        label: "Write the week in a sentence or two",
        blurb: "Sends this week's totals: hours by role, tasks done, entries written.",
        app: QuickApp::Overview,
        default_on: false,
        system: "You write two sentences about somebody's week from its \
            totals, naming the one thing that changed most against the week \
            before. State what the numbers say and stop. No encouragement, no \
            advice, and no conclusions the numbers do not support.",
        schema: text_schema,
    },
    // -- data -------------------------------------------------------------
    QuickJob {
        name: "data.import_map",
        label: "Map an imported folder's front matter",
        blurb: "Sends the front-matter keys found in the files you are importing.",
        app: QuickApp::Data,
        default_on: true,
        system: "You map the front-matter keys of somebody's exported notes \
            onto our fields. Leave ours out rather than mapping it to a key \
            that only nearly matches — this runs over every file they have.",
        schema: mapping_schema,
    },
];

// ── Building each job's prompt ───────────────────────────────────────────
//
// One function per job, taking domain types rather than a `Value`. That is
// what keeps the promise in the module docs: the service is handed a
// `Prompt` and posts it, and every decision about what the model is told
// lives here, where it is tested.
//
// Where a job must name an existing record, the record is offered as a
// *numbered list* and the answer is a number. Nothing in this file ever puts
// an id in front of a model, which is the same rule `agent::tools` follows
// and for the stronger reason: these answers are applied to a capture box
// without anybody reading a tool call first.

use crate::library::{Item, Kind};
use crate::purpose::{Goal, Role};
use crate::tracker::Tracker;
use crate::websearch::SearchResult;

/// Number a list the way every prompt in this file numbers one.
fn numbered(items: impl IntoIterator<Item = String>) -> String {
    items
        .into_iter()
        .enumerate()
        .map(|(i, line)| format!("{}. {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// L1/L4 — pull a shelf's fields out of what the search found.
pub fn library_fields(
    ctx: &QuickContext,
    kind: &Kind,
    item: &Item,
    results: &[SearchResult],
) -> Prompt {
    let fields = kind
        .fields
        .iter()
        .map(|f| format!("  {} — {}", f.key, f.label))
        .collect::<Vec<_>>()
        .join("\n");
    let found = results
        .iter()
        .take(3)
        .map(|r| format!("--- {}\n{}\n{}\n{}", r.title, r.byline(), r.url, excerpt(&r.summary)))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "Shelf: {} (one of them is a {})\nLooking up: {}\n\n\
         Field keys this shelf has:\n{}\n\n\
         What the search found:\n{}",
        kind.name,
        kind.singular,
        item.title,
        if fields.is_empty() { "  (none)" } else { &fields },
        found,
    );
    Prompt::new(job_or_panic("library.fields"), ctx, user)
}

/// L2 — which of these is the thing.
pub fn library_pick(
    ctx: &QuickContext,
    kind: &Kind,
    query: &str,
    results: &[SearchResult],
) -> Prompt {
    let list = numbered(results.iter().map(|r| {
        let byline = r.byline();
        if byline.is_empty() { r.title.clone() } else { format!("{} — {}", r.title, byline) }
    }));
    let user = format!(
        "They typed: {query}\nOn the shelf: {} (one of them is a {})\n\nResults:\n{list}",
        kind.name, kind.singular,
    );
    Prompt::new(job_or_panic("library.pick"), ctx, user)
}

/// L3 — design a shelf from its name.
pub fn library_kind(ctx: &QuickContext, name: &str) -> Prompt {
    Prompt::new(job_or_panic("library.kind"), ctx, format!("Shelf name: {name}"))
}

/// L5 — map an exported list's columns onto a shelf's fields.
pub fn library_import_map(
    ctx: &QuickContext,
    kind: &Kind,
    columns: &[String],
    sample: &[String],
) -> Prompt {
    let ours = std::iter::once("title".to_string())
        .chain(["subtitle", "creator", "year"].iter().map(|s| s.to_string()))
        .chain(kind.fields.iter().map(|f| f.key.clone()))
        .collect::<Vec<_>>()
        .join(", ");
    let user = format!(
        "Our fields: {ours}\nTheir columns: {}\nOne of their rows: {}",
        columns.join(", "),
        sample.join(" | "),
    );
    Prompt::new(job_or_panic("library.import_map"), ctx, user)
}

/// A role-and-goal list, numbered, as every purpose job offers it.
///
/// Roles come after goals so that the specific answer is the near one: a
/// model given "Parent" and "Viya rides without stabilisers" should reach for
/// the goal when the task is about the bike, and the role only when nothing
/// narrower fits.
fn purpose_list(roles: &[Role], goals: &[Goal]) -> String {
    numbered(
        goals
            .iter()
            .map(|g| format!("goal: {}", g.title))
            .chain(roles.iter().map(|r| format!("role: {}", r.name))),
    )
}

/// T1 — a purpose and tags for a task somebody just typed.
pub fn todo_purpose(
    ctx: &QuickContext,
    title: &str,
    roles: &[Role],
    goals: &[Goal],
    known_tags: &[String],
) -> Prompt {
    let user = format!(
        "Task: {title}\n\nTheir roles and goals:\n{}\n\nTags already in use: {}",
        purpose_list(roles, goals),
        tag_list(known_tags),
    );
    Prompt::new(job_or_panic("todo.purpose"), ctx, user)
}

fn tag_list(tags: &[String]) -> String {
    if tags.is_empty() { "(none yet)".into() } else { tags.join(", ") }
}

/// T2 — the line the sigil grammar could not read.
///
/// The grammar in `quickadd.ts` is deterministic, offline, instant and
/// tested, and it is *better* than a model for the lines it handles. This is
/// only ever asked about the residue — see `quick.ts`, which will not call it
/// for a line that parsed.
pub fn todo_parse(ctx: &QuickContext, line: &str) -> Prompt {
    Prompt::new(job_or_panic("todo.parse"), ctx, format!("The line: {line}"))
}

/// T3 — the steps a task is made of.
pub fn todo_subtasks(ctx: &QuickContext, title: &str, description: &str) -> Prompt {
    let user = match description.trim() {
        "" => format!("Task: {title}"),
        body => format!("Task: {title}\n\nIts description:\n{}", excerpt(body)),
    };
    Prompt::new(job_or_panic("todo.subtasks"), ctx, user)
}

/// T4 — how long, grounded in what this person's own work actually took.
pub fn todo_estimate(ctx: &QuickContext, title: &str, comparable: &[(String, u32)]) -> Prompt {
    let past = if comparable.is_empty() {
        "(nothing comparable recorded yet)".to_string()
    } else {
        comparable
            .iter()
            .map(|(t, mins)| format!("  {t} — took {mins} minutes"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Prompt::new(
        job_or_panic("todo.estimate"),
        ctx,
        format!("Task: {title}\n\nWhat their similar tasks actually took:\n{past}"),
    )
}

/// C1 — an appointment from a sentence.
pub fn calendar_parse(ctx: &QuickContext, line: &str) -> Prompt {
    Prompt::new(job_or_panic("calendar.parse"), ctx, format!("The line: {line}"))
}

/// C2 — a readable title for a subscribed event.
///
/// Display only. The feed's record is never rewritten: it will be re-fetched
/// and it is not ours.
pub fn calendar_title(ctx: &QuickContext, raw: &str) -> Prompt {
    Prompt::new(job_or_panic("calendar.title"), ctx, format!("The title: {raw}"))
}

/// J1 — the numbers a day's writing states.
pub fn journal_readings(ctx: &QuickContext, text: &str, trackers: &[Tracker]) -> Prompt {
    let list = numbered(trackers.iter().map(|t| {
        let unit = if t.unit.trim().is_empty() { String::new() } else { format!(" in {}", t.unit) };
        format!("{} ({}{})", t.name, t.kind.as_str(), unit)
    }));
    let list = if trackers.is_empty() { "(none set up yet)".to_string() } else { list };
    let user = format!("Their trackers:\n{list}\n\nWhat they wrote:\n{}", excerpt(text));
    Prompt::new(job_or_panic("journal.readings"), ctx, user)
}

/// J2 — a title for a day.
pub fn journal_title(ctx: &QuickContext, text: &str) -> Prompt {
    Prompt::new(job_or_panic("journal.title"), ctx, format!("The writing:\n{}", excerpt(text)))
}

/// J3 — tags and a purpose for a day.
pub fn journal_labels(
    ctx: &QuickContext,
    text: &str,
    roles: &[Role],
    goals: &[Goal],
    known_tags: &[String],
) -> Prompt {
    let user = format!(
        "Their roles and goals:\n{}\n\nTags already in use: {}\n\nWhat they wrote:\n{}",
        purpose_list(roles, goals),
        tag_list(known_tags),
        excerpt(text),
    );
    Prompt::new(job_or_panic("journal.labels"), ctx, user)
}

/// N1 — a title for a note.
pub fn notes_title(ctx: &QuickContext, text: &str) -> Prompt {
    Prompt::new(job_or_panic("notes.title"), ctx, format!("The note:\n{}", excerpt(text)))
}

/// N2 — the tasks buried in a page of notes.
pub fn notes_tasks(ctx: &QuickContext, title: &str, text: &str) -> Prompt {
    let user = format!("Note: {title}\n\n{}", excerpt(text));
    Prompt::new(job_or_panic("notes.tasks"), ctx, user)
}

/// N3 — tags and a purpose for a note.
pub fn notes_labels(
    ctx: &QuickContext,
    text: &str,
    roles: &[Role],
    goals: &[Goal],
    known_tags: &[String],
) -> Prompt {
    let user = format!(
        "Their roles and goals:\n{}\n\nTags already in use: {}\n\nThe note:\n{}",
        purpose_list(roles, goals),
        tag_list(known_tags),
        excerpt(text),
    );
    Prompt::new(job_or_panic("notes.labels"), ctx, user)
}

/// K1 — a reading from a sentence.
pub fn tracker_parse(ctx: &QuickContext, line: &str, trackers: &[Tracker]) -> Prompt {
    let list = numbered(trackers.iter().map(|t| {
        let unit = if t.unit.trim().is_empty() { String::new() } else { format!(" in {}", t.unit) };
        format!("{} ({}{})", t.name, t.kind.as_str(), unit)
    }));
    let list = if trackers.is_empty() { "(none set up yet)".to_string() } else { list };
    Prompt::new(
        job_or_panic("tracker.parse"),
        ctx,
        format!("Their trackers:\n{list}\n\nThe line: {line}"),
    )
}

/// K2 — how a tracker made by the act of recording should be set up.
pub fn tracker_draft(ctx: &QuickContext, name: &str, line: &str) -> Prompt {
    Prompt::new(
        job_or_panic("tracker.draft"),
        ctx,
        format!("Tracker name: {name}\nWhat they recorded: {line}"),
    )
}

/// P1 — a goal phrased as an outcome.
pub fn purpose_goal(ctx: &QuickContext, goal: &str, role: &str) -> Prompt {
    Prompt::new(job_or_panic("purpose.goal"), ctx, format!("Role: {role}\nGoal as written: {goal}"))
}

/// P2 — which existing records serve a goal that has just been written.
///
/// The answer is a [`LabelsAnswer`] whose `tags` are the numbers, which is
/// the one place a shape is reused for something it was not named for; see
/// [`backfill_picks`] for the read of it.
pub fn purpose_backfill(ctx: &QuickContext, goal: &str, candidates: &[String]) -> Prompt {
    let user = format!(
        "The goal: {goal}\n\nRecords with no purpose yet:\n{}\n\n\
         Answer with the numbers of the ones this goal plainly covers, as \
         strings in `tags`. An empty list is a fine answer.",
        numbered(candidates.iter().cloned()),
    );
    Prompt::new(job_or_panic("purpose.backfill"), ctx, user)
}

/// Read [`purpose_backfill`]'s answer as indices into the candidate list.
pub fn backfill_picks(answer: &LabelsAnswer, len: usize) -> Vec<usize> {
    let mut out: Vec<usize> = answer
        .tags
        .iter()
        .filter_map(|t| t.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1 && *n <= len)
        .map(|n| n - 1)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// O1 — the week, in a sentence or two.
pub fn overview_week(ctx: &QuickContext, this_week: &str, last_week: &str) -> Prompt {
    Prompt::new(
        job_or_panic("overview.week"),
        ctx,
        format!("This week:\n{this_week}\n\nThe week before:\n{last_week}"),
    )
}

/// X1 — map somebody else's front matter onto ours.
pub fn data_import_map(ctx: &QuickContext, ours: &[String], theirs: &[String]) -> Prompt {
    Prompt::new(
        job_or_panic("data.import_map"),
        ctx,
        format!("Our fields: {}\nTheir keys: {}", ours.join(", "), theirs.join(", ")),
    )
}

/// Look up a job this file names itself.
///
/// Panics, and should: every caller is a function above passing a literal
/// that is in [`JOBS`] three hundred lines up, so a failure here is a typo
/// caught by the first test that runs — and `all_named_jobs_exist` below runs
/// it for every one of them.
fn job_or_panic(name: &'static str) -> &'static QuickJob {
    job(name).unwrap_or_else(|| panic!("{name} is not in JOBS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> QuickContext {
        QuickContext::new(jiff::civil::date(2026, 9, 10), "Europe/London")
    }

    #[test]
    fn every_job_name_is_unique_and_every_builder_names_a_real_one() {
        let mut seen = BTreeSet::new();
        for job in JOBS {
            assert!(seen.insert(job.name), "{} is in JOBS twice", job.name);
            assert!(!job.system.trim().is_empty(), "{} has no instruction", job.name);
            assert!(!job.blurb.trim().is_empty(), "{} has no blurb", job.name);
            // The blurb is what somebody reads to decide. One that does not
            // say what is sent is a switch nobody can make a decision about.
            assert!(job.blurb.starts_with("Sends "), "{}'s blurb must say what it sends", job.name);
            let schema = job.schema();
            assert_eq!(schema["type"], "object", "{}'s schema must have an object root", job.name);
        }
        // Panics if any builder names a job that is not in the catalogue.
        let kind = Kind::new("books", "Books", "book");
        let item = Item::new(kind.id, "Dune");
        library_fields(&ctx(), &kind, &item, &[]);
        library_pick(&ctx(), &kind, "dune", &[]);
        library_kind(&ctx(), "Wines");
        library_import_map(&ctx(), &kind, &[], &[]);
        todo_purpose(&ctx(), "x", &[], &[], &[]);
        todo_parse(&ctx(), "x");
        todo_subtasks(&ctx(), "x", "");
        todo_estimate(&ctx(), "x", &[]);
        calendar_parse(&ctx(), "x");
        calendar_title(&ctx(), "x");
        journal_readings(&ctx(), "x", &[]);
        journal_title(&ctx(), "x");
        journal_labels(&ctx(), "x", &[], &[], &[]);
        notes_title(&ctx(), "x");
        notes_tasks(&ctx(), "t", "x");
        notes_labels(&ctx(), "x", &[], &[], &[]);
        tracker_parse(&ctx(), "x", &[]);
        tracker_draft(&ctx(), "n", "x");
        purpose_goal(&ctx(), "x", "r");
        purpose_backfill(&ctx(), "g", &[]);
        overview_week(&ctx(), "a", "b");
        data_import_map(&ctx(), &[], &[]);
    }

    #[test]
    fn everything_that_reads_a_journal_entry_is_off_by_default() {
        for job in JOBS.iter().filter(|j| j.app == QuickApp::Journal) {
            assert!(!job.default_on, "{} reads a journal entry and must default off", job.name);
        }
    }

    #[test]
    fn the_policy_records_only_the_difference_from_the_defaults() {
        let mut policy = QuickPolicy::default();
        assert!(policy.allows("library.fields"));
        assert!(!policy.allows("journal.title"));

        // Setting a job to what it already defaults to stores nothing, so a
        // person who toggles a switch twice does not leave a record behind
        // that would pin it against a later change of default.
        policy.set("library.fields", true);
        policy.set("journal.title", false);
        assert_eq!(policy, QuickPolicy::default());

        policy.set("journal.title", true);
        policy.set("library.fields", false);
        assert!(policy.allows("journal.title"));
        assert!(!policy.allows("library.fields"));
        assert_eq!(policy.allowed.len(), 1);
        assert_eq!(policy.denied.len(), 1);
    }

    #[test]
    fn a_job_this_build_does_not_have_is_refused() {
        let mut policy = QuickPolicy::default();
        // A name from a newer build, or a typo. Either way, running
        // *something* on the strength of a name we cannot look up is worse
        // than doing nothing.
        assert!(!policy.allows("library.invent_a_book"));
        policy.allowed.insert("library.invent_a_book".into());
        assert!(!policy.allows("library.invent_a_book"));
    }

    #[test]
    fn facts_the_shelf_has_no_field_for_are_dropped() {
        let mut answer = FieldsAnswer {
            creator: "  Frank Herbert  ".into(),
            year: Some(1965),
            facts: [
                ("author".to_string(), "Frank Herbert".to_string()),
                ("runtime".to_string(), "155 min".to_string()),
                ("pages".to_string(), "  ".to_string()),
            ]
            .into(),
            ..Default::default()
        };
        answer.clamp(&["author".to_string(), "pages".to_string()]);
        // `runtime` had no field: a fact with nowhere to live is invisible in
        // the interface and undeletable from it. `pages` was blank.
        assert_eq!(answer.facts.keys().collect::<Vec<_>>(), vec!["author"]);
        assert_eq!(answer.creator, "Frank Herbert");
    }

    #[test]
    fn a_year_that_is_really_a_page_count_is_dropped() {
        let mut answer = FieldsAnswer { year: Some(412), ..Default::default() };
        answer.clamp(&[]);
        assert_eq!(answer.year, None);

        let mut ok = FieldsAnswer { year: Some(1965), ..Default::default() };
        ok.clamp(&[]);
        assert_eq!(ok.year, Some(1965));
    }

    #[test]
    fn a_pick_outside_the_list_is_an_abstention() {
        assert_eq!(PickAnswer { choice: Some(1) }.index(3), Some(0));
        assert_eq!(PickAnswer { choice: Some(3) }.index(3), Some(2));
        assert_eq!(PickAnswer { choice: Some(4) }.index(3), None);
        // One-based everywhere, so a zero is a model that counted from zero
        // and must not become the first result.
        assert_eq!(PickAnswer { choice: Some(0) }.index(3), None);
        assert_eq!(PickAnswer { choice: None }.index(3), None);
    }

    #[test]
    fn a_kind_draft_is_cut_down_to_a_shelf() {
        let mut draft = KindDraft {
            icon: "🍷 (a wine glass)".into(),
            color: "purple".into(),
            fields: (0..9)
                .map(|i| KindFieldDraft { key: format!("Field {i}"), label: format!("Field {i}") })
                .chain([KindFieldDraft { key: "".into(), label: "x".into() }])
                .collect(),
            ..Default::default()
        };
        draft.clamp();
        assert_eq!(draft.icon, "🍷", "an icon is one glyph");
        assert!(draft.color.is_empty(), "a colour that is not a hex triple is no colour");
        assert_eq!(draft.fields.len(), MAX_DRAFT_FIELDS);
        assert_eq!(draft.fields[0].key, "field_0");
    }

    #[test]
    fn two_draft_fields_cannot_share_a_key() {
        // Keys are what facts are stored under, so a duplicate is a field
        // that silently overwrites its neighbour.
        let mut draft = KindDraft {
            fields: vec![
                KindFieldDraft { key: "Region".into(), label: "Region".into() },
                KindFieldDraft { key: "region".into(), label: "Where from".into() },
            ],
            ..Default::default()
        };
        draft.clamp();
        assert_eq!(draft.fields.len(), 1);
    }

    #[test]
    fn suggested_tags_never_repeat_what_is_already_there() {
        let mut answer = LabelsAnswer {
            tags: vec!["#Travel".into(), "travel".into(), " flights ".into(), "".into()],
            purpose: Some(2),
        };
        answer.clamp(&["Travel".to_string()]);
        assert_eq!(answer.tags, vec!["flights"]);
    }

    #[test]
    fn a_task_draft_refuses_a_date_it_did_not_parse() {
        let mut draft = TaskDraft {
            title: "  Book the flights  ".into(),
            due_date: Some("next Tuesday".into()),
            due_time: Some("16:30".into()),
            priority: Some("URGENT".into()),
            estimate_minutes: Some(90),
            tags: vec!["#Travel".into()],
        };
        assert!(draft.clamp());
        assert_eq!(draft.title, "Book the flights");
        // A date that reaches the store as a string nobody parsed is a record
        // that looks fine until something sorts by it.
        assert_eq!(draft.due_date, None);
        assert_eq!(draft.due_time.as_deref(), Some("16:30"));
        assert_eq!(draft.priority.as_deref(), Some("urgent"));
        assert_eq!(draft.tags, vec!["travel"]);

        let mut untitled = TaskDraft { title: "   ".into(), ..Default::default() };
        assert!(!untitled.clamp());
    }

    #[test]
    fn an_estimate_of_a_whole_week_is_not_an_estimate() {
        let mut draft = TaskDraft {
            title: "Write the book".into(),
            estimate_minutes: Some(4800),
            ..Default::default()
        };
        draft.clamp();
        assert_eq!(draft.estimate_minutes, None);
    }

    #[test]
    fn an_event_that_ends_before_it_starts_loses_its_end() {
        let mut draft = EventDraft {
            title: "Lunch with Sam".into(),
            date: Some("2026-09-10".into()),
            start: Some("13:00".into()),
            end: Some("12:00".into()),
            location: " the usual place ".into(),
        };
        assert!(draft.clamp());
        assert_eq!(draft.end, None, "a block of negative height is not a block");
        assert_eq!(draft.location, "the usual place");

        // No date is not an appointment.
        let mut undated = EventDraft { title: "Lunch".into(), ..Default::default() };
        assert!(!undated.clamp());
    }

    #[test]
    fn a_reading_must_name_a_tracker_or_propose_one() {
        let mut anonymous =
            ReadingDraft { tracker: None, name: "  ".into(), value: 1.0, ..Default::default() };
        assert!(!anonymous.clamp());

        let mut named = ReadingDraft {
            tracker: None,
            name: "Swimming".into(),
            value: 60.0,
            ..Default::default()
        };
        assert!(named.clamp());

        let mut nonsense = ReadingDraft { tracker: Some(1), value: f64::NAN, ..Default::default() };
        assert!(!nonsense.clamp(), "a NaN reaches a chart as a gap nobody can explain");
    }

    #[test]
    fn a_tracker_draft_never_invents_a_scale_bound_for_a_dose() {
        let mut dose = TrackerDraft {
            name: "Ibuprofen".into(),
            kind: "Dose".into(),
            unit: "mg".into(),
            scale_max: Some(10),
            ..Default::default()
        };
        assert!(dose.clamp());
        assert_eq!(dose.kind, "dose");
        assert_eq!(dose.scale_max, None);

        let mut scale = TrackerDraft {
            name: "Mood".into(),
            kind: "scale".into(),
            scale_max: None,
            ..Default::default()
        };
        assert!(scale.clamp());
        assert_eq!(scale.scale_max, Some(10));

        // An unrecognised kind becomes an amount rather than a check: an
        // amount stores the number that was said, which is at worst
        // incomplete, where a check would silently discard it.
        let mut odd =
            TrackerDraft { name: "Steps".into(), kind: "steps".into(), ..Default::default() };
        assert!(odd.clamp());
        assert_eq!(odd.kind, "amount");
    }

    #[test]
    fn a_mapping_cannot_name_a_column_that_was_not_offered() {
        let mut answer = MappingAnswer {
            columns: [
                ("title".to_string(), "Title".to_string()),
                ("creator".to_string(), "Author l-n".to_string()),
                ("invented".to_string(), "Title".to_string()),
            ]
            .into(),
        };
        answer.clamp(&["title".into(), "creator".into()], &["Title".into()]);
        // A mapping runs over every row of somebody's history, so a
        // hallucinated column is thousands of wrong records rather than one.
        assert_eq!(answer.columns.keys().collect::<Vec<_>>(), vec!["title"]);
    }

    #[test]
    fn backfill_picks_are_read_as_positions_and_bounded() {
        let answer = LabelsAnswer {
            tags: vec!["1".into(), "3".into(), "3".into(), "9".into(), "nine".into()],
            purpose: None,
        };
        assert_eq!(backfill_picks(&answer, 4), vec![0, 2]);
    }

    #[test]
    fn an_excerpt_is_cut_on_a_word_boundary() {
        let short = "one two three";
        assert_eq!(excerpt(short), short);

        let long = "word ".repeat(MAX_INPUT_CHARS);
        let cut = excerpt(&long);
        assert!(cut.chars().count() <= MAX_INPUT_CHARS);
        assert!(cut.ends_with("word"));
    }

    #[test]
    fn a_prompt_carries_the_house_rules_and_the_date() {
        let prompt = todo_parse(&ctx(), "book the flights friday");
        assert!(prompt.system.contains("Guessing"), "the house rules must be appended");
        assert!(prompt.user.contains("2026-09-10"), "a relative date needs today");
        assert!(prompt.user.contains("Europe/London"));
        assert_eq!(prompt.job, "todo.parse");
    }

    #[test]
    fn a_purpose_list_puts_goals_before_roles() {
        // The specific answer should be the near one: a model given both
        // should reach for the goal when the task is about the goal.
        let role = Role::new("Parent");
        let goal = Goal::new(role.id, "Viya rides without stabilisers");
        let list = purpose_list(std::slice::from_ref(&role), std::slice::from_ref(&goal));
        let goal_at = list.find("goal:").expect("the goal is listed");
        let role_at = list.find("role:").expect("the role is listed");
        assert!(goal_at < role_at);
    }
}
