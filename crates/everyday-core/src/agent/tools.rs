//! What the assistant can actually do, and the arguments it must get right.
//!
//! This is the whole of the agent's capability, and it is here — in the core,
//! synchronous, with no model provider and no socket anywhere — for the same
//! reason [`ics`](crate::ics) and [`websearch`](crate::websearch) are. The
//! shell above wraps each of these in whatever shape its harness wants and
//! opens the connection; every question about what a tool *does* is answered
//! and tested in this file, offline, in microseconds.
//!
//! That split is worth being concrete about, because it is what makes the
//! risky part of this feature testable. "The assistant marked the wrong task
//! done" is a unit test here. It is not a recorded HTTP exchange, it does
//! not need an API key to run in CI, and it does not stop being tested the
//! day the harness is swapped.
//!
//! # Naming, and why it is not the command surface
//!
//! There are 80-odd Tauri commands and 30-odd tools, and the mapping is
//! deliberately not one to one. A command exists to serve one control in the
//! interface; a tool exists to be *chosen correctly by a model reading a
//! list of names*. So the four ways the interface has of saving an item are
//! one `update_item` here, `save_tasks` (a bulk reorder the board needs) has
//! no tool at all, and the names are verbs in the domain's own words rather
//! than in the storage layer's.
//!
//! # Effects, and what the confirmation gate is for
//!
//! Every tool declares an [`Effect`]. [`Effect::Destructive`] is not "writes
//! something" — creating a task writes something — it is "removes something
//! that cannot be reconstructed from what remains". This application has no
//! undo stack, so a delete the model chose on a misreading is gone, and
//! [`AgentSettings::confirm_destructive`](crate::agent::AgentSettings::confirm_destructive)
//! is what stands between a plausible misreading and a lost project. The
//! flag is data rather than a naming convention precisely so the gate cannot
//! be defeated by someone adding a tool and forgetting the prefix.
//!
//! # Ids, and why every tool takes one
//!
//! A model cannot invent a UUIDv7 and must not be encouraged to try: every
//! mutating tool takes an id, and the tool that finds ids is a different
//! call. The cost is a round trip before every edit. The benefit is that
//! "update the deck task" cannot silently become an update to whichever task
//! the model's first guess happened to name.

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::agent::{MAX_MEMORY_CHARS, Memory};
use crate::error::{Error, Result};
use crate::id::{
    BlockId, ConversationId, EntryId, GoalId, ItemId, JournalId, MemoryId, ProjectId, RoleId,
    TaskId, TrackerId,
};
use crate::library::{Item, ItemStatus, LogEntry, LogEvent};
use crate::model::{Entry, Journal};
use crate::purpose::{Goal, GoalActivity, GoalStatus, Purpose};
use crate::richtext::RichDoc;
use crate::store::calendars::EventQuery;
use crate::store::library::ItemQuery;
use crate::store::purpose::{GoalQuery, PurposeWindow};
use crate::store::tasks::{BlockQuery, ParentScope, ProjectScope, TaskQuery};
use crate::store::trackers::ReadingQuery;
use crate::store::{EntryQuery, SortOrder};
use crate::task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, Task, TaskStatus, TimeBlock,
};
use crate::tracker::Reading;
use crate::vault::Vault;
use jiff::Timestamp;
use jiff::civil::Date;
use std::collections::BTreeMap;

/// What running a tool does to the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Effect {
    /// Changes nothing. Always safe to run, never confirmed, and the
    /// majority of the catalogue — an agent that is any good spends most of
    /// its turns reading.
    Read,
    /// Creates or edits a record. Applied without asking, because it is
    /// recoverable by hand: a wrongly-created task can be deleted and a
    /// wrongly-edited one edited back.
    Write,
    /// Removes something. Gated on
    /// [`AgentSettings::confirm_destructive`](crate::agent::AgentSettings::confirm_destructive),
    /// because there is no undo in this application and no way back.
    Destructive,
}

impl Effect {
    pub fn is_write(self) -> bool {
        !matches!(self, Effect::Read)
    }
}

/// Which optional storage domain a tool needs.
///
/// Checked before the model is ever told a tool exists, so a vault on the
/// Markdown backend offers an assistant that can read and write entries and
/// is not offered — and cannot hallucinate having — a task list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Journals,
    Tasks,
    Calendars,
    Library,
    Trackers,
    Goals,
    Agent,
}

/// Whether a domain's tools may be put in front of a model at all.
///
/// Not a setting, and not a confirmation. The distinction it draws is between
/// data that is *private* -- which is all of it, and which the assistant is
/// specifically for -- and data whose disclosure is the whole of its harm.
///
/// The reason this exists before the domain that needs it: prompt injection
/// already has a path in. A fetched web page, an imported calendar, an entry
/// somebody else wrote -- all of them reach the model's context, and a model
/// that can be talked into calling a tool can be talked into calling that one.
/// For a task list the worst case is a wrongly-created task. For a stored
/// password it is exfiltration, and no amount of confirming makes that a risk
/// worth carrying for the convenience of asking an assistant about it.
///
/// So a `Secret` domain is absent from [`available`], which is what both the
/// model and the palette read. There is no flag to turn it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Private, like everything here, and reachable by the assistant.
    Ordinary,
    /// Never offered to a model, whatever the settings say.
    Secret,
}

impl Domain {
    fn available(self, vault: &Vault) -> bool {
        self.sensitivity() == Sensitivity::Ordinary
            && match self {
                Domain::Journals => true,
                Domain::Tasks => vault.supports_tasks(),
                Domain::Calendars => vault.supports_calendars(),
                Domain::Library => vault.supports_library(),
                Domain::Trackers => vault.supports_trackers(),
                Domain::Goals => vault.supports_goals(),
                Domain::Agent => vault.supports_agent(),
            }
    }

    /// Written out rather than defaulted, so adding a domain is a decision
    /// somebody made rather than one they inherited. A `Passwords` variant
    /// added here without a line in this match will not compile.
    pub fn sensitivity(self) -> Sensitivity {
        match self {
            Domain::Journals
            | Domain::Tasks
            | Domain::Calendars
            | Domain::Library
            | Domain::Trackers
            // Roles and goals are the shape of somebody's life rather than
            // its contents, and the assistant is specifically for reasoning
            // about them -- "what did I actually spend the week on" is the
            // question the Overview exists to answer.
            | Domain::Goals
            | Domain::Agent => Sensitivity::Ordinary,
        }
    }
}

/// One thing the assistant can do.
pub struct Tool {
    pub name: &'static str,
    /// What the model reads to decide whether this is the tool it wants.
    /// Written for that reader: what it does, when to reach for it, and what
    /// it will not do.
    pub description: &'static str,
    pub effect: Effect,
    pub domain: Domain,
    schema: fn() -> Value,
    run: fn(&ToolContext<'_>, &Args<'_>) -> Result<Value>,
}

impl Tool {
    /// JSON Schema for this tool's arguments, in the shape every provider's
    /// function-calling API wants.
    pub fn parameters(&self) -> Value {
        (self.schema)()
    }
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).field("effect", &self.effect).finish()
    }
}

/// Everything a tool needs that is not one of its arguments.
pub struct ToolContext<'a> {
    pub vault: &'a Vault,
    /// Today in the person's own time zone. Passed in rather than read from
    /// the clock so that "due tomorrow" is a test rather than a thing that
    /// works until midnight.
    pub today: Date,
    /// IANA zone, for filing an entry.
    pub tz: &'a str,
    /// The thread this call belongs to, so a memory can record where it came
    /// from. `None` outside a conversation — a scheduled run, or a test.
    pub conversation: Option<ConversationId>,
}

// ---- arguments ----------------------------------------------------------

/// A tool's arguments, as the model produced them.
///
/// The accessors exist for their *error messages*. A tool call is answered by
/// sending the failure back to the model, which then tries again, so an
/// error here is not a log line — it is a prompt. `"add_task: due_date must
/// be a date like 2026-09-14, got \"next friday\""` gets a corrected call on
/// the next turn; serde's `invalid type: string` does not.
pub struct Args<'a> {
    tool: &'static str,
    value: &'a Value,
}

impl<'a> Args<'a> {
    pub fn new(tool: &'static str, value: &'a Value) -> Self {
        Self { tool, value }
    }

    fn bad(&self, msg: impl std::fmt::Display) -> Error {
        Error::Invalid(format!("{}: {msg}", self.tool))
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        // `null` is treated as absent throughout. Models emit it constantly
        // for optional arguments they have nothing to say about, and a
        // schema-correct `{"project_id": null}` must mean "no project"
        // rather than "a project whose id is null".
        self.value.get(key).filter(|v| !v.is_null())
    }

    pub fn opt_str(&self, key: &str) -> Option<&'a str> {
        self.get(key)?.as_str().filter(|s| !s.trim().is_empty())
    }

    pub fn str(&self, key: &str) -> Result<&'a str> {
        self.opt_str(key).ok_or_else(|| self.bad(format!("`{key}` is required")))
    }

    pub fn opt_bool(&self, key: &str) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.opt_bool(key).unwrap_or(default)
    }

    pub fn opt_u32(&self, key: &str) -> Option<u32> {
        let v = self.get(key)?;
        // Numbers arrive as numbers, but a model that has been asked for a
        // string once will send `"30"` forever afterwards. Both are the
        // same intent and neither is worth a failed turn.
        v.as_u64()
            .or_else(|| v.as_str()?.trim().parse().ok())
            .map(|n| n.min(u32::MAX as u64) as u32)
    }

    pub fn opt_f64(&self, key: &str) -> Option<f64> {
        let v = self.get(key)?;
        v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
    }

    /// A capped, defaulted result count.
    ///
    /// Every listing tool goes through this. The cap is not politeness: each
    /// row is sent to the model and paid for, and a model that asks for ten
    /// thousand tasks has made a mistake this turns into a partial answer
    /// rather than a bill.
    pub fn limit(&self) -> u32 {
        self.opt_u32("limit").unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            // A single string where a list was asked for is the most common
            // shape error a model makes, and it is unambiguous.
            Some(Value::String(s)) => {
                s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn opt_date(&self, key: &str) -> Result<Option<Date>> {
        let Some(raw) = self.opt_str(key) else { return Ok(None) };
        raw.trim().parse::<Date>().map(Some).map_err(|_| {
            self.bad(format!(
                "`{key}` must be a calendar date like 2026-09-14, got {raw:?}. \
                 Work out relative dates yourself from today's date in your instructions."
            ))
        })
    }

    pub fn date(&self, key: &str) -> Result<Date> {
        self.opt_date(key)?.ok_or_else(|| self.bad(format!("`{key}` is required")))
    }

    /// Parse an id, saying what kind was wanted.
    ///
    /// The message matters more here than anywhere: a model that has been
    /// handed a project id and a task id in the same reply will eventually
    /// use one where the other belongs, and being told which is what makes
    /// the next turn right.
    pub fn opt_id<T>(&self, key: &str, kind: &str) -> Result<Option<T>>
    where
        T: std::str::FromStr,
    {
        let Some(raw) = self.opt_str(key) else { return Ok(None) };
        raw.trim().parse::<T>().map(Some).map_err(|_| {
            self.bad(format!(
                "`{key}` must be a {kind} id, got {raw:?}. \
                 List or search for the {kind} first and use the id it gives you."
            ))
        })
    }

    pub fn id<T>(&self, key: &str, kind: &str) -> Result<T>
    where
        T: std::str::FromStr,
    {
        self.opt_id(key, kind)?.ok_or_else(|| self.bad(format!("`{key}` is required")))
    }

    /// Parse an enum from its wire spelling, listing the alternatives.
    pub fn opt_enum<T: DeserializeOwned>(&self, key: &str, allowed: &[&str]) -> Result<Option<T>> {
        let Some(raw) = self.opt_str(key) else { return Ok(None) };
        // Case-insensitive: the schema says `todo` and models write `Todo`.
        let lowered = Value::String(raw.trim().to_lowercase());
        serde_json::from_value(lowered).map(Some).map_err(|_| {
            self.bad(format!("`{key}` must be one of {}, got {raw:?}", allowed.join(", ")))
        })
    }
}

/// Rows a listing tool returns when the model does not say.
pub const DEFAULT_LIMIT: u32 = 50;

/// Most rows any listing tool will return, whatever it was asked for.
pub const MAX_LIMIT: u32 = 200;

// ---- schema helpers -----------------------------------------------------

fn schema(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in props {
        map.insert(k.to_string(), v);
    }
    json!({
        "type": "object",
        "properties": Value::Object(map),
        "required": required,
        // Providers that support strict function calling reject unknown
        // arguments, which is what stops a model quietly inventing a filter
        // this code would ignore and reporting a result it did not get.
        "additionalProperties": false,
    })
}

fn empty_schema() -> Value {
    schema(vec![], &[])
}

fn text(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn one_of(desc: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values, "description": desc })
}

fn number(desc: &str) -> Value {
    json!({ "type": "integer", "description": desc })
}

fn decimal(desc: &str) -> Value {
    json!({ "type": "number", "description": desc })
}

fn flag(desc: &str) -> Value {
    json!({ "type": "boolean", "description": desc })
}

fn list(desc: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": desc })
}

fn day(desc: &str) -> Value {
    json!({ "type": "string", "description": format!("{desc} Format: YYYY-MM-DD.") })
}

fn limit_arg() -> (&'static str, Value) {
    ("limit", number("Most rows to return. Defaults to 50, capped at 200."))
}

const GOAL_STATUSES: [&str; 4] = ["active", "paused", "done", "dropped"];

const STATUSES: &[&str] = &["backlog", "todo", "doing", "blocked", "done", "cancelled"];
const PRIORITIES: &[&str] = &["none", "low", "medium", "high", "urgent"];
const PROJECT_STATUSES: &[&str] = &["active", "paused", "done", "archived"];
const ITEM_STATUSES: &[&str] = &["wishlist", "active", "paused", "done", "abandoned"];

// ---- the catalogue ------------------------------------------------------

/// Every tool that exists.
///
/// Order is the order the model sees them in, and it is not arbitrary:
/// orientation first, then the domains in the order someone would work
/// through them. A model choosing between thirty names does better when the
/// list reads like a table of contents than when it reads like a hash map.
pub fn catalog() -> &'static [Tool] {
    ALL
}

/// The tools this vault can actually offer, given what its backend stores.
pub fn available(vault: &Vault) -> Vec<&'static Tool> {
    ALL.iter().filter(|t| t.domain.available(vault)).collect()
}

/// Look a tool up by the name the model used.
pub fn find(name: &str) -> Option<&'static Tool> {
    ALL.iter().find(|t| t.name == name)
}

/// What a destructive call is about to act on, in the person's own words.
///
/// The confirmation gate exists so somebody can catch a misreading, and it
/// can only do that if it names the thing: "delete the deck" is a decision,
/// "delete 0192f8b2-…" is a coin toss. Every destructive tool takes an id
/// and nothing else, so the name has to be read out of the vault -- which
/// the tools themselves already do, just a moment too late to be asked
/// about.
///
/// `None` when there is nothing useful to say, which the interface draws as
/// a card with no subject rather than as an id nobody can check.
pub fn describe(ctx: &ToolContext<'_>, name: &str, arguments: &Value) -> Option<String> {
    let args = Args::new("describe", arguments);
    let vault = ctx.vault;
    match name {
        "delete_entry" => {
            let id: EntryId = args.opt_id("entry_id", "entry").ok()??;
            vault.entry(id).ok().map(|e| e.display_title())
        }
        "delete_project" => {
            let id: ProjectId = args.opt_id("project_id", "project").ok()??;
            vault.project(id).ok().map(|p| p.name)
        }
        "delete_task" => {
            let id: TaskId = args.opt_id("task_id", "task").ok()??;
            vault.task(id).ok().map(|t| t.title)
        }
        "delete_item" => {
            let id: ItemId = args.opt_id("item_id", "item").ok()??;
            vault.item(id).ok().map(|i| i.title)
        }
        "delete_goal" => {
            let id: GoalId = args.opt_id("goal_id", "goal").ok()??;
            vault.goal(id).ok().map(|g| g.title)
        }
        "delete_time_block" => {
            let id: BlockId = args.opt_id("block_id", "time block").ok()??;
            let block = vault.block(id).ok()?;
            // A block has no name of its own unless it is ad-hoc, so it is
            // described by what it is for and when -- which is what somebody
            // needs in order to recognise it.
            let subject = match &block.subject {
                BlockSubject::Task { id } => vault.task(*id).ok().map(|t| t.title),
                BlockSubject::Project { id } => vault.project(*id).ok().map(|p| p.name),
                BlockSubject::Adhoc => Some(block.title.clone()).filter(|t| !t.is_empty()),
            };
            Some(match subject {
                Some(what) => format!("{what} on {}", block.local_date),
                None => format!("the block on {}", block.local_date),
            })
        }
        "forget" => {
            let id: MemoryId = args.opt_id("memory_id", "memory").ok()??;
            vault.memories().ok()?.into_iter().find(|m| m.id == id).map(|m| m.text)
        }
        _ => None,
    }
}

/// Run one tool call.
///
/// Refuses a tool the vault cannot serve rather than failing somewhere
/// deeper with a message about storage backends, and refuses an unknown name
/// with the list of real ones — which is what a model that has invented a
/// tool needs in order to recover on the next turn.
pub fn dispatch(ctx: &ToolContext<'_>, name: &str, arguments: &Value) -> Result<Value> {
    let tool = find(name).ok_or_else(|| {
        Error::Invalid(format!(
            "there is no tool called {name:?}. Available tools: {}",
            available(ctx.vault).iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
        ))
    })?;

    if !tool.domain.available(ctx.vault) {
        return Err(Error::Unsupported("this vault's backend does not store that"));
    }
    if tool.effect.is_write() && !ctx.vault.is_writable() {
        return Err(Error::Invalid(
            "this vault is open read-only, so nothing can be changed right now".into(),
        ));
    }

    (tool.run)(ctx, &Args::new(tool.name, arguments))
}

macro_rules! tool {
    ($name:literal, $effect:ident, $domain:ident, $schema:expr, $desc:literal, $run:expr) => {
        Tool {
            name: $name,
            description: $desc,
            effect: Effect::$effect,
            domain: Domain::$domain,
            schema: || $schema,
            run: $run,
        }
    };
}

static ALL: &[Tool] = &[
    // ---- orientation ----------------------------------------------------
    tool!(
        "overview",
        Read,
        Journals,
        empty_schema(),
        "What this vault contains: its journals, projects, shelves and trackers, \
         with counts, plus what is due today. Call this first in a new conversation \
         when you do not yet know what exists.",
        run_overview
    ),
    tool!(
        "search",
        Read,
        Journals,
        schema(
            vec![
                ("query", text("Words to look for. Full-text over entry titles and bodies.")),
                ("journal_id", text("Restrict to one journal. Omit to search all of them.")),
                limit_arg(),
            ],
            &["query"]
        ),
        "Full-text search across journal entries. The way to find an entry when \
         you know roughly what it said but not when it was written. Searches \
         entries only \u{2014} use list_tasks or list_items to find those.",
        run_search
    ),
    // ---- journals and entries -------------------------------------------
    tool!(
        "list_journals",
        Read,
        Journals,
        empty_schema(),
        "Every journal in the vault, with its id, name and description.",
        run_list_journals
    ),
    tool!(
        "list_entries",
        Read,
        Journals,
        schema(
            vec![
                ("journal_id", text("Restrict to one journal.")),
                ("from", day("Earliest date, inclusive.")),
                ("to", day("Latest date, inclusive.")),
                ("tags", list("Keep only entries carrying every one of these tags.")),
                ("starred", flag("Keep only starred entries.")),
                limit_arg(),
            ],
            &[]
        ),
        "Journal entries matching a filter, newest first. Returns summaries \u{2014} \
         id, date, title, tags \u{2014} not the text. Call get_entry for the body.",
        run_list_entries
    ),
    tool!(
        "get_entry",
        Read,
        Journals,
        schema(vec![("entry_id", text("Id from list_entries or search."))], &["entry_id"]),
        "One entry in full, its body rendered as Markdown.",
        run_get_entry
    ),
    tool!(
        "create_entry",
        Write,
        Journals,
        schema(
            vec![
                ("journal_id", text("Which journal to file it in. From list_journals.")),
                ("body", text("The entry text. Markdown: paragraphs, headings, lists.")),
                ("title", text("Optional heading. Omit and the first line becomes one.")),
                ("date", day("File it under this day instead of today. Back-dating is fine.")),
                ("tags", list("Tags to attach.")),
            ],
            &["journal_id", "body"]
        ),
        "Write a new journal entry. Use this when asked to record, draft or write \
         something up \u{2014} not for a task, which is create_task.",
        run_create_entry
    ),
    tool!(
        "update_entry",
        Write,
        Journals,
        schema(
            vec![
                ("entry_id", text("Id of the entry to change.")),
                ("body", text("Replaces the whole body. Markdown. Omit to leave it alone.")),
                ("title", text("Replaces the title. Omit to leave it alone.")),
                ("date", day("Re-file under a different day.")),
                ("tags", list("Replaces the tags entirely. Omit to leave them alone.")),
                ("starred", flag("Star or unstar it.")),
            ],
            &["entry_id"]
        ),
        "Change an existing entry. Every field is optional and omitted fields are \
         left alone \u{2014} but `body` and `tags` replace rather than append, so read \
         the entry first if you mean to add to it.",
        run_update_entry
    ),
    tool!(
        "delete_entry",
        Destructive,
        Journals,
        schema(vec![("entry_id", text("Id of the entry to delete."))], &["entry_id"]),
        "Permanently delete a journal entry. There is no undo. Only do this when \
         explicitly asked to delete that specific entry.",
        run_delete_entry
    ),
    // ---- projects and tasks ---------------------------------------------
    tool!(
        "list_projects",
        Read,
        Tasks,
        schema(
            vec![
                ("status", one_of("Restrict to one status.", PROJECT_STATUSES)),
                ("include_archived", flag("Include archived projects. Off by default.")),
            ],
            &[]
        ),
        "Every project, with its id, name, status, deadline and how many tasks it \
         holds open.",
        run_list_projects
    ),
    tool!(
        "create_project",
        Write,
        Tasks,
        schema(
            vec![
                ("name", text("What to call it.")),
                ("notes", text("A description.")),
                ("status", one_of("Defaults to active.", PROJECT_STATUSES)),
                ("priority", one_of("Defaults to none.", PRIORITIES)),
                ("due_date", day("Deadline for the whole project.")),
                ("start_date", day("Earliest sensible start.")),
                ("tags", list("Tags to attach.")),
            ],
            &["name"]
        ),
        "Create a project \u{2014} a named body of work that tasks belong to.",
        run_create_project
    ),
    tool!(
        "update_project",
        Write,
        Tasks,
        schema(
            vec![
                ("project_id", text("Id of the project to change.")),
                ("name", text("Rename it.")),
                ("notes", text("Replace the description.")),
                ("status", one_of("Move it to this status.", PROJECT_STATUSES)),
                ("priority", one_of("Change its priority.", PRIORITIES)),
                ("due_date", day("Set the deadline.")),
                ("start_date", day("Set the start date.")),
                ("tags", list("Replaces the tags entirely.")),
            ],
            &["project_id"]
        ),
        "Change a project. Omitted fields are left alone. Marking a project done \
         does not touch its tasks.",
        run_update_project
    ),
    tool!(
        "delete_project",
        Destructive,
        Tasks,
        schema(vec![("project_id", text("Id of the project to delete."))], &["project_id"]),
        "Permanently delete a project. Its tasks are NOT deleted \u{2014} they fall back \
         to the inbox. Prefer update_project with status archived unless deletion \
         was actually asked for.",
        run_delete_project
    ),
    tool!(
        "list_tasks",
        Read,
        Tasks,
        schema(
            vec![
                ("project_id", text("Only tasks in this project.")),
                ("inbox_only", flag("Only tasks in no project.")),
                ("parent_id", text("Only the subtasks of this task.")),
                ("top_level_only", flag("Exclude subtasks.")),
                ("statuses", list("Keep only these statuses. Defaults to every status.")),
                ("open_only", flag("Shorthand for the statuses that are not done or cancelled.")),
                ("tags", list("Keep only tasks carrying every one of these tags.")),
                ("priority_at_least", one_of("Keep only tasks at or above this.", PRIORITIES)),
                ("due_from", day("Earliest deadline, inclusive.")),
                ("due_to", day("Latest deadline, inclusive. Use today's date for 'due now'.")),
                ("text", text("Case-insensitive substring of the title, notes or tags.")),
                limit_arg(),
            ],
            &[]
        ),
        "Tasks matching a filter. The main way to find a task before changing it. \
         For 'what is due today', pass due_to as today's date with open_only true.",
        run_list_tasks
    ),
    tool!(
        "get_task",
        Read,
        Tasks,
        schema(vec![("task_id", text("Id from list_tasks."))], &["task_id"]),
        "One task in full, including its notes and its subtasks.",
        run_get_task
    ),
    tool!(
        "create_task",
        Write,
        Tasks,
        schema(
            vec![
                ("title", text("What to do. A verb phrase reads best.")),
                ("project_id", text("Which project it belongs to. Omit for the inbox.")),
                ("parent_id", text("Make it a subtask of this task.")),
                ("notes", text("Detail that does not fit in the title.")),
                ("status", one_of("Defaults to todo.", STATUSES)),
                ("priority", one_of("Defaults to none.", PRIORITIES)),
                ("due_date", day("Deadline.")),
                ("start_date", day("Earliest sensible start.")),
                ("estimate_minutes", number("Expected effort in minutes.")),
                ("tags", list("Tags to attach.")),
            ],
            &["title"]
        ),
        "Create a task. This is the tool for 'add a task', 'remind me to', \
         'I need to'. Create several by calling it several times.",
        run_create_task
    ),
    tool!(
        "update_task",
        Write,
        Tasks,
        schema(
            vec![
                ("task_id", text("Id of the task to change.")),
                ("title", text("Rename it.")),
                ("notes", text("Replace the notes.")),
                ("status", one_of("Move it. Use done to complete a task.", STATUSES)),
                ("priority", one_of("Change its priority.", PRIORITIES)),
                ("project_id", text("Move it to this project.")),
                ("clear_project", flag("Move it back to the inbox.")),
                ("due_date", day("Set the deadline.")),
                ("clear_due_date", flag("Remove the deadline.")),
                ("start_date", day("Set the start date.")),
                ("estimate_minutes", number("Expected effort in minutes.")),
                ("tags", list("Replaces the tags entirely.")),
            ],
            &["task_id"]
        ),
        "Change a task. Omitted fields are left alone. This is how a task is \
         completed \u{2014} status done \u{2014} rescheduled, reprioritised or moved. \
         Never delete a task to mark it finished.",
        run_update_task
    ),
    tool!(
        "delete_task",
        Destructive,
        Tasks,
        schema(vec![("task_id", text("Id of the task to delete."))], &["task_id"]),
        "Permanently delete a task and its subtasks. There is no undo. To finish \
         a task use update_task with status done; to drop one you decided against, \
         status cancelled. Delete only what was never real.",
        run_delete_task
    ),
    // ---- time -----------------------------------------------------------
    tool!(
        "list_events",
        Read,
        Calendars,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to today.")),
                ("to", day("End of the window, inclusive. Defaults to a week out.")),
                limit_arg(),
            ],
            &[]
        ),
        "Calendar events in a date window, from subscribed calendars. These are \
         read-only: this app subscribes to other people's feeds and cannot write \
         to them. To schedule your own time, use create_time_block.",
        run_list_events
    ),
    tool!(
        "list_time_blocks",
        Read,
        Tasks,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to today.")),
                ("to", day("End of the window, inclusive. Defaults to a week out.")),
                ("task_id", text("Only blocks against this task.")),
                limit_arg(),
            ],
            &[]
        ),
        "Blocks of time in a window: planned ones (what you set aside) and actual \
         ones (what it took). The way to answer 'how long did that take'.",
        run_list_blocks
    ),
    tool!(
        "create_time_block",
        Write,
        Tasks,
        schema(
            vec![
                ("date", day("The day it falls on.")),
                ("start_time", text("Start, as HH:MM in 24-hour time.")),
                ("end_time", text("End, as HH:MM. Must be after the start.")),
                ("task_id", text("Which task this is time for.")),
                ("project_id", text("Or which project, if it is not one task.")),
                ("label", text("Or a free-text label, if it is neither.")),
                (
                    "kind",
                    one_of(
                        "planned (setting time aside) or actual (recording what it took). Defaults to planned.",
                        &["planned", "actual"]
                    )
                ),
                ("notes", text("Anything worth noting.")),
            ],
            &["date", "start_time", "end_time"]
        ),
        "Set aside time for a task, or record time that was spent. Give exactly one \
         of task_id, project_id or label to say what the time is for.",
        run_create_block
    ),
    tool!(
        "delete_time_block",
        Destructive,
        Tasks,
        schema(vec![("block_id", text("Id from list_time_blocks."))], &["block_id"]),
        "Permanently delete a block of time.",
        run_delete_block
    ),
    // ---- the library ----------------------------------------------------
    tool!(
        "list_shelves",
        Read,
        Library,
        empty_schema(),
        "The shelves in the library \u{2014} Books, Films, whatever else was made \u{2014} \
         with their ids and how many things are on them. Call this before \
         list_items or create_item, which need a shelf id.",
        run_list_kinds
    ),
    tool!(
        "list_items",
        Read,
        Library,
        schema(
            vec![
                ("shelf_id", text("Restrict to one shelf. From list_shelves.")),
                ("status", one_of("Restrict to one status.", ITEM_STATUSES)),
                ("text", text("Case-insensitive substring of the title or creator.")),
                ("favourite", flag("Keep only favourites.")),
                ("finished_from", day("Finished on or after this day.")),
                ("finished_to", day("Finished on or before this day.")),
                limit_arg(),
            ],
            &[]
        ),
        "Things on the shelves: books, films, games, whatever this vault tracks. \
         For 'what did I read this year', filter on finished_from and finished_to.",
        run_list_items
    ),
    tool!(
        "create_item",
        Write,
        Library,
        schema(
            vec![
                ("shelf_id", text("Which shelf. From list_shelves.")),
                ("title", text("What it is called.")),
                ("creator", text("Author, director, developer.")),
                ("status", one_of("Defaults to wishlist.", ITEM_STATUSES)),
                ("year", number("Year it came out.")),
                ("rating", decimal("Out of 10, so 8 means four stars.")),
                ("notes", text("Anything worth saying about it.")),
                ("tags", list("Tags to attach.")),
            ],
            &["shelf_id", "title"]
        ),
        "Put something on a shelf. Does not look anything up online \u{2014} it records \
         what you tell it.",
        run_create_item
    ),
    tool!(
        "update_item",
        Write,
        Library,
        schema(
            vec![
                ("item_id", text("Id from list_items.")),
                ("title", text("Rename it.")),
                ("creator", text("Change the author or director.")),
                (
                    "status",
                    one_of("Move it. `done` records that you got through it.", ITEM_STATUSES)
                ),
                ("rating", decimal("Out of 10.")),
                ("favourite", flag("Mark or unmark a favourite.")),
                ("notes", text("Replace the notes.")),
                ("started_on", day("The day you started.")),
                ("finished_on", day("The day you finished.")),
                ("tags", list("Replaces the tags entirely.")),
            ],
            &["item_id"]
        ),
        "Change something on a shelf: finish it, rate it, make it a favourite.",
        run_update_item
    ),
    tool!(
        "delete_item",
        Destructive,
        Library,
        schema(vec![("item_id", text("Id from list_items."))], &["item_id"]),
        "Permanently delete something from a shelf, and its whole history with it. \
         To record giving up on something, use update_item with status abandoned.",
        run_delete_item
    ),
    // ---- tracking -------------------------------------------------------
    tool!(
        "list_trackers",
        Read,
        Trackers,
        empty_schema(),
        "What this vault records in numbers rather than prose \u{2014} habits, doses, \
         symptoms, counts \u{2014} with what each one's values mean and how often it \
         is meant to happen.",
        run_list_trackers
    ),
    tool!(
        "log_reading",
        Write,
        Trackers,
        schema(
            vec![
                ("tracker_id", text("From list_trackers.")),
                ("value", decimal("The number. For a yes/no tracker, 1 or 0.")),
                ("date", day("The day it belongs to. Defaults to today.")),
                ("note", text("Anything worth noting alongside it.")),
            ],
            &["tracker_id", "value"]
        ),
        "Record one reading against a tracker. The value is clamped to whatever \
         the tracker's scale allows.",
        run_log_reading
    ),
    tool!(
        "tracker_summary",
        Read,
        Trackers,
        schema(
            vec![
                ("tracker_id", text("From list_trackers. Omit for every tracker.")),
                ("from", day("Start of the window, inclusive. Defaults to 30 days back.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
            ],
            &[]
        ),
        "One row per tracker per day over a window, already aggregated. Use this \
         rather than reading every reading: it is what answers 'how has my sleep \
         been' without pulling a year of rows through the conversation.",
        run_tracker_summary
    ),
    tool!(
        "list_readings",
        Read,
        Trackers,
        schema(
            vec![
                ("tracker_id", text("From list_trackers. Omit for every tracker.")),
                ("from", day("Start of the window, inclusive. Defaults to 30 days back.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
                ("timed_only", flag("Only readings that know their time of day.")),
                limit_arg(),
            ],
            &[]
        ),
        "Every individual reading over a window, with its day and \u{2014} where it is \
         known \u{2014} its time. Prefer tracker_summary for 'how has my sleep been': \
         this is for questions that need the readings themselves, such as lining up \
         the days one thing happened against what another said the day after.",
        run_list_readings
    ),
    // ---- roles and goals ------------------------------------------------
    tool!(
        "list_roles",
        Read,
        Goals,
        empty_schema(),
        "The parts of a life this vault is organised around \u{2014} parent, work, \
         yourself \u{2014} with how many goals sit under each.",
        run_list_roles
    ),
    tool!(
        "list_goals",
        Read,
        Goals,
        schema(
            vec![
                ("role_id", text("From list_roles. Omit for every role.")),
                ("status", one_of("Only goals in this state.", &GOAL_STATUSES)),
                ("include_activity", flag("Also say what has been recorded against each.")),
                limit_arg(),
            ],
            &[]
        ),
        "What somebody has said they want, under which part of their life. With \
         include_activity, each goal also reports its hours, its open tasks and \
         when it was last touched \u{2014} which is how to find the ones that have \
         gone quiet.",
        run_list_goals
    ),
    tool!(
        "create_goal",
        Write,
        Goals,
        schema(
            vec![
                ("role_id", text("From list_roles. Required: every goal sits under one.")),
                ("title", text("What is wanted.")),
                ("notes", text("What done would look like.")),
                ("horizon", day("When it would ideally be true by. Soft; nothing is notified.")),
            ],
            &["role_id", "title"]
        ),
        "Add a goal under a role.",
        run_create_goal
    ),
    tool!(
        "update_goal",
        Write,
        Goals,
        schema(
            vec![
                ("goal_id", text("From list_goals.")),
                ("title", text("A new title.")),
                ("notes", text("What done would look like.")),
                ("status", one_of("Where it has got to.", &GOAL_STATUSES)),
                ("horizon", day("When it would ideally be true by.")),
                ("role_id", text("Move it under a different role.")),
            ],
            &["goal_id"]
        ),
        "Change a goal. Only the fields given are touched. Setting status to done \
         stamps when it was finished; dropped does not, because giving up on \
         something is not finishing it.",
        run_update_goal
    ),
    tool!(
        "delete_goal",
        Destructive,
        Goals,
        schema(vec![("goal_id", text("From list_goals."))], &["goal_id"]),
        "Permanently delete a goal. Whatever was filed under it is kept but stops \
         being counted towards it. To record giving up on something, use \
         update_goal with status dropped.",
        run_delete_goal
    ),
    tool!(
        "set_purpose",
        Write,
        Goals,
        schema(
            vec![
                (
                    "kind",
                    one_of(
                        "What sort of record to file.",
                        &["project", "task", "block", "entry", "item", "tracker"]
                    )
                ),
                ("id", text("The record's id, from whichever list tool found it.")),
                ("goal_id", text("File it under this goal. From list_goals.")),
                ("role_id", text("Or under this role directly. From list_roles.")),
                ("clear", flag("Unfile it instead.")),
            ],
            &["kind", "id"]
        ),
        "Say what a record is for: a goal, or a part of a life directly. Filing a \
         project is worth more than filing its tasks \u{2014} everything under it \
         inherits, so one call attributes the lot. Pass exactly one of goal_id, \
         role_id or clear.",
        run_set_purpose
    ),
    tool!(
        "time_by_role",
        Read,
        Goals,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to 7 days back.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
            ],
            &[]
        ),
        "Where the hours went over a window, grouped by the part of a life they \
         served, planned beside recorded. Time filed against nothing is reported \
         as its own row rather than left out.",
        run_time_by_role
    ),
    // ---- memory ---------------------------------------------------------
    tool!(
        "remember",
        Write,
        Agent,
        schema(
            vec![(
                "fact",
                text("One sentence, in the third person: 'Plans the week on Sunday evening.'")
            )],
            &["fact"]
        ),
        "Keep one fact about this person across conversations \u{2014} a preference, a \
         habit, what a word of theirs means. Use it for things that will still be \
         true next month, not for what is happening today. Do not record anything \
         they told you in confidence about themselves unless they asked you to \
         remember it.",
        run_remember
    ),
    tool!(
        "list_memories",
        Read,
        Agent,
        empty_schema(),
        "Everything you have been asked to remember, with ids. You are already \
         given these in your instructions; call this only when asked to change \
         or review them.",
        run_list_memories
    ),
    tool!(
        "forget",
        Destructive,
        Agent,
        schema(vec![("memory_id", text("Id from list_memories."))], &["memory_id"]),
        "Drop one remembered fact.",
        run_forget
    ),
];

// ---- projections --------------------------------------------------------
//
// What a record looks like on its way to the model, and it is never the
// whole record. Every field costs tokens on the turn it is sent and on every
// turn afterwards, so these carry what is needed to *decide* something --
// enough to answer a question, and the id to act on it with -- and leave the
// rest to a `get_` call. An entry summary is the clearest case: a year of
// entries is a reasonable thing to list and an impossible thing to send.

fn task_json(t: &Task) -> Value {
    let mut v = json!({
        "id": t.id.to_string(),
        "title": t.title,
        "status": t.status.as_str(),
    });
    let m = v.as_object_mut().unwrap();
    if t.priority != Priority::None {
        m.insert("priority".into(), json!(priority_str(t.priority)));
    }
    if let Some(p) = t.project_id {
        m.insert("project_id".into(), json!(p.to_string()));
    }
    if let Some(p) = t.parent_id {
        m.insert("parent_id".into(), json!(p.to_string()));
    }
    if let Some(d) = t.due_date {
        m.insert("due_date".into(), json!(d.to_string()));
    }
    if let Some(d) = t.start_date {
        m.insert("start_date".into(), json!(d.to_string()));
    }
    if let Some(e) = t.estimate_minutes {
        m.insert("estimate_minutes".into(), json!(e));
    }
    if !t.tags.is_empty() {
        m.insert("tags".into(), json!(t.tags));
    }
    if !t.notes.trim().is_empty() {
        m.insert("notes".into(), json!(t.notes));
    }
    v
}

fn project_json(p: &Project, open_tasks: Option<u64>) -> Value {
    let mut v = json!({
        "id": p.id.to_string(),
        "name": p.name,
        "status": p.status.as_str(),
    });
    let m = v.as_object_mut().unwrap();
    if p.priority != Priority::None {
        m.insert("priority".into(), json!(priority_str(p.priority)));
    }
    if let Some(d) = p.due_date {
        m.insert("due_date".into(), json!(d.to_string()));
    }
    if !p.notes.trim().is_empty() {
        m.insert("notes".into(), json!(p.notes));
    }
    if !p.tags.is_empty() {
        m.insert("tags".into(), json!(p.tags));
    }
    if let Some(n) = open_tasks {
        m.insert("open_tasks".into(), json!(n));
    }
    v
}

/// A rating as the tools speak it, converted to how the vault stores it.
///
/// The library stores `0..=100` so that four stars from one site and 82%
/// from another are comparable. Nobody says "give it an eighty", so the tool
/// takes a score out of ten and multiplies -- which also means a model that
/// sends 8 and a model that sends 80 do not silently record wildly different
/// opinions, because the second is refused.
fn rating_out_of_ten(args: &Args<'_>) -> Result<Option<u8>> {
    let Some(raw) = args.opt_f64("rating") else { return Ok(None) };
    if !(0.0..=10.0).contains(&raw) {
        return Err(args.bad(format!("`rating` is out of 10, got {raw}")));
    }
    Ok(Some((raw * 10.0).round() as u8))
}

fn priority_str(p: Priority) -> &'static str {
    match p {
        Priority::None => "none",
        Priority::Low => "low",
        Priority::Medium => "medium",
        Priority::High => "high",
        Priority::Urgent => "urgent",
    }
}

fn entry_summary_json(e: &crate::model::EntrySummary) -> Value {
    let mut v = json!({
        "id": e.id.to_string(),
        "date": e.local_date.to_string(),
        "title": e.title,
        "journal_id": e.journal_id.to_string(),
    });
    let m = v.as_object_mut().unwrap();
    if !e.tags.is_empty() {
        m.insert("tags".into(), json!(e.tags));
    }
    if e.starred {
        m.insert("starred".into(), json!(true));
    }
    v
}

fn entry_json(e: &Entry) -> Value {
    json!({
        "id": e.id.to_string(),
        "journal_id": e.journal_id.to_string(),
        "date": e.local_date.to_string(),
        "title": e.display_title(),
        // Markdown rather than the ProseMirror document. The model reads and
        // writes prose; handing it a node tree would spend hundreds of
        // tokens on braces and invite it to edit them.
        "body": e.body.to_markdown(),
        "tags": e.tags,
        "starred": e.starred,
    })
}

fn item_json(i: &Item) -> Value {
    let mut v = json!({
        "id": i.id.to_string(),
        "shelf_id": i.kind_id.to_string(),
        "title": i.title,
        "status": i.status.as_str(),
    });
    let m = v.as_object_mut().unwrap();
    if !i.creator.trim().is_empty() {
        m.insert("creator".into(), json!(i.creator));
    }
    if let Some(y) = i.year {
        m.insert("year".into(), json!(y));
    }
    if let Some(r) = i.rating {
        m.insert("rating_out_of_10".into(), json!(f64::from(r) / 10.0));
    }
    if i.favourite {
        m.insert("favourite".into(), json!(true));
    }
    if let Some(d) = i.finished_on {
        m.insert("finished_on".into(), json!(d.to_string()));
    }
    if !i.tags.is_empty() {
        m.insert("tags".into(), json!(i.tags));
    }
    v
}

/// What a mutating tool says when it worked.
///
/// Uniform on purpose. The model has to tell the person what happened, and a
/// shape that always carries the human-readable name means it never has to
/// quote a UUID at somebody to prove it did something.
fn done(action: &str, kind: &str, name: &str, id: String) -> Result<Value> {
    Ok(json!({ "ok": true, "action": action, "kind": kind, "name": name, "id": id }))
}

// ---- implementations ----------------------------------------------------

fn run_overview(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let vault = ctx.vault;
    let mut out = json!({ "today": ctx.today.to_string(), "time_zone": ctx.tz });
    let m = out.as_object_mut().unwrap();

    m.insert(
        "journals".into(),
        json!(
            vault
                .journals()?
                .iter()
                .map(|j| json!({ "id": j.id.to_string(), "name": j.name }))
                .collect::<Vec<_>>()
        ),
    );

    if vault.supports_tasks() {
        let stats = vault.task_stats(ctx.today)?;
        m.insert(
            "tasks".into(),
            json!({
                "open": stats.open_tasks,
                "done": stats.done_tasks,
                // `overdue` is a subset of `due_today` rather than a
                // separate bucket, so it is reported as one -- adding them
                // would double-count every late task.
                "due_today_or_overdue": stats.due_today,
                "overdue": stats.overdue,
            }),
        );
        m.insert(
            "projects".into(),
            json!(
                vault
                    .projects()?
                    .iter()
                    .filter(|p| p.status.is_open())
                    .map(|p| json!({ "id": p.id.to_string(), "name": p.name, "status": p.status.as_str() }))
                    .collect::<Vec<_>>()
            ),
        );
    }

    if vault.supports_library() {
        m.insert(
            "shelves".into(),
            json!(
                vault
                    .kinds()?
                    .iter()
                    .map(|k| json!({ "id": k.id.to_string(), "name": k.name }))
                    .collect::<Vec<_>>()
            ),
        );
    }

    if vault.supports_trackers() {
        let trackers: Vec<Value> = vault
            .trackers()?
            .iter()
            .filter(|t| !t.archived)
            .map(|t| json!({ "id": t.id.to_string(), "name": t.name }))
            .collect();
        m.insert("trackers".into(), json!(trackers));
    }

    if vault.supports_goals() {
        // The parts of a life and what is wanted from each, so a model
        // knows the vocabulary before it is asked a question in it. Names
        // and ids only -- the hours are `time_by_role`'s answer and the
        // per-goal detail is `list_goals`'.
        let roles: Vec<Value> = vault
            .roles()?
            .iter()
            .filter(|r| !r.archived)
            .map(|r| json!({ "id": r.id.to_string(), "name": r.name }))
            .collect();
        let goals: Vec<Value> = vault
            .goals(&GoalQuery::open())?
            .iter()
            .map(|g| {
                json!({
                    "id": g.id.to_string(),
                    "title": g.title,
                    "role_id": g.role_id.to_string(),
                    "status": g.status.as_str(),
                })
            })
            .collect();
        m.insert("roles".into(), json!(roles));
        m.insert("open_goals".into(), json!(goals));
    }

    Ok(out)
}

fn run_search(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let journal: Option<JournalId> = args.opt_id("journal_id", "journal")?;
    let hits = ctx.vault.search(args.str("query")?, journal, args.limit() as usize)?;
    Ok(json!({
        "count": hits.len(),
        "results": hits
            .iter()
            .map(|h| json!({
                "id": h.id.to_string(),
                "date": h.local_date.to_string(),
                "title": h.title,
                "excerpt": h.snippet,
            }))
            .collect::<Vec<_>>(),
    }))
}

fn run_list_journals(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    Ok(json!(
        ctx.vault
            .journals()?
            .iter()
            .map(|j: &Journal| json!({
                "id": j.id.to_string(),
                "name": j.name,
                "description": j.description,
            }))
            .collect::<Vec<_>>()
    ))
}

fn run_list_entries(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = EntryQuery {
        journal_id: args.opt_id("journal_id", "journal")?,
        from: args.opt_date("from")?,
        to: args.opt_date("to")?,
        tags: args.strings("tags"),
        starred: args.opt_bool("starred"),
        sort: SortOrder::DateDesc,
        offset: 0,
        limit: Some(args.limit()),
    };
    let rows = ctx.vault.entries(&query)?;
    Ok(json!({
        "count": rows.len(),
        "entries": rows.iter().map(entry_summary_json).collect::<Vec<_>>(),
    }))
}

fn run_get_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: EntryId = args.id("entry_id", "entry")?;
    Ok(entry_json(&ctx.vault.entry(id)?))
}

fn run_create_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let journal_id: JournalId = args.id("journal_id", "journal")?;
    // Read the journal rather than trusting the id, so an entry cannot be
    // filed into a journal that does not exist and then be invisible.
    let journal = ctx.vault.journal(journal_id)?;

    let mut entry = Entry::new(journal_id, ctx.tz);
    entry.body = RichDoc::from_markdown(args.str("body")?);
    entry.title = args.opt_str("title").unwrap_or_default().to_string();
    entry.tags = args.strings("tags");
    if let Some(d) = args.opt_date("date")? {
        entry.local_date = d;
    }

    ctx.vault.save_entry(&entry, None)?;
    done(
        "created",
        "entry",
        &format!("{} in {}", entry.display_title(), journal.name),
        entry.id.to_string(),
    )
}

fn run_update_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: EntryId = args.id("entry_id", "entry")?;
    let mut entry = ctx.vault.entry(id)?;

    if let Some(body) = args.opt_str("body") {
        // A body arrives whole or not at all, so replacing one that holds
        // photographs would take them off the page -- the blobs survive, via
        // `Entry::attachments`, but the entry stops showing them. Refused
        // rather than silently done, because there is no undo and no way for
        // the model to know what it dropped: it was handed the body as
        // Markdown, in which an attachment is a link it cannot put back.
        if !entry.body.blob_refs().is_empty() {
            return Err(Error::Invalid(
                "this entry has photographs or files in it, and replacing its text would \
                 take them off the page. Edit it in the app, or say what to change and \
                 leave the body alone."
                    .into(),
            ));
        }
        entry.body = RichDoc::from_markdown(body);
    }
    if let Some(title) = args.opt_str("title") {
        entry.title = title.to_string();
    }
    if let Some(d) = args.opt_date("date")? {
        entry.local_date = d;
    }
    if args.get("tags").is_some() {
        entry.tags = args.strings("tags");
    }
    if let Some(starred) = args.opt_bool("starred") {
        entry.starred = starred;
    }

    // Stamped here because the vault does not do it: every writer owns its
    // own `updated_at`, and the interface stamps one before each autosave.
    // Without this the record's timestamp never moves, so an editor that
    // still has the entry open keeps matching on the version it loaded and
    // its next save overwrites the assistant's edit with no conflict
    // reported -- the exact loss `put_entry_if` exists to prevent.
    entry.updated_at = Timestamp::now();

    // Unconditional rather than optimistic. The optimistic path exists for
    // two people editing the same entry in two windows; here the other
    // writer is the person sitting in front of it, and a failed tool call
    // they would have to resolve by hand is worse than the last write
    // winning -- which is what the interface's own autosave does anyway.
    ctx.vault.overwrite_entry(&entry)?;
    done("updated", "entry", &entry.display_title(), entry.id.to_string())
}

fn run_delete_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: EntryId = args.id("entry_id", "entry")?;
    // Read it first, so the confirmation and the reply can name what went
    // rather than quoting an id at somebody.
    let entry = ctx.vault.entry(id)?;
    ctx.vault.delete_entry(id)?;
    done("deleted", "entry", &entry.display_title(), id.to_string())
}

// ---- projects and tasks -------------------------------------------------

fn run_list_projects(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let want: Option<ProjectStatus> = args.opt_enum("status", PROJECT_STATUSES)?;
    let include_archived = args.bool_or("include_archived", false);

    let counts = ctx.vault.task_stats(ctx.today)?.open_by_project;
    let rows: Vec<Value> = ctx
        .vault
        .projects()?
        .into_iter()
        .filter(|p| match want {
            Some(s) => p.status == s,
            None => include_archived || p.status != ProjectStatus::Archived,
        })
        .map(|p| {
            // Projects with nothing open are omitted from the counts rather
            // than listed as zero, so an absent row means none rather than
            // unknown.
            let open = counts.iter().find(|c| c.project_id == Some(p.id)).map_or(0, |c| c.open);
            project_json(&p, Some(open))
        })
        .collect();

    Ok(json!({ "count": rows.len(), "projects": rows }))
}

fn run_create_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut p = Project::new(args.str("name")?);
    p.notes = args.opt_str("notes").unwrap_or_default().to_string();
    if let Some(s) = args.opt_enum("status", PROJECT_STATUSES)? {
        p.set_status(s);
    }
    if let Some(pr) = args.opt_enum("priority", PRIORITIES)? {
        p.priority = pr;
    }
    p.due_date = args.opt_date("due_date")?;
    p.start_date = args.opt_date("start_date")?;
    p.tags = args.strings("tags");

    ctx.vault.save_project(&p)?;
    done("created", "project", &p.name, p.id.to_string())
}

fn run_update_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ProjectId = args.id("project_id", "project")?;
    let mut p = ctx.vault.project(id)?;

    if let Some(name) = args.opt_str("name") {
        p.name = name.to_string();
    }
    if let Some(notes) = args.opt_str("notes") {
        p.notes = notes.to_string();
    }
    // Through `set_status`, never by assignment: the helper is what keeps
    // `completed_at` honest -- set when a project is finished, cleared when
    // it is reopened. Assigning the field directly leaves a reopened project
    // wearing the date it was closed.
    if let Some(s) = args.opt_enum("status", PROJECT_STATUSES)? {
        p.set_status(s);
    }
    if let Some(pr) = args.opt_enum("priority", PRIORITIES)? {
        p.priority = pr;
    }
    if let Some(d) = args.opt_date("due_date")? {
        p.due_date = Some(d);
    }
    if let Some(d) = args.opt_date("start_date")? {
        p.start_date = Some(d);
    }
    if args.get("tags").is_some() {
        p.tags = args.strings("tags");
    }
    p.updated_at = Timestamp::now();

    ctx.vault.save_project(&p)?;
    done("updated", "project", &p.name, p.id.to_string())
}

fn run_delete_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ProjectId = args.id("project_id", "project")?;
    let p = ctx.vault.project(id)?;
    ctx.vault.delete_project(id)?;
    done("deleted", "project", &p.name, id.to_string())
}

fn run_list_tasks(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut query = TaskQuery {
        tags: args.strings("tags"),
        due_from: args.opt_date("due_from")?,
        due_to: args.opt_date("due_to")?,
        text: args.opt_str("text").unwrap_or_default().to_string(),
        limit: Some(args.limit()),
        ..Default::default()
    };

    if let Some(id) = args.opt_id::<ProjectId>("project_id", "project")? {
        query.project = ProjectScope::Project { id };
    } else if args.bool_or("inbox_only", false) {
        query.project = ProjectScope::Inbox;
    }

    if let Some(id) = args.opt_id::<TaskId>("parent_id", "task")? {
        query.parent = ParentScope::Of { id };
    } else if args.bool_or("top_level_only", false) {
        query.parent = ParentScope::TopLevel;
    }

    if let Some(p) = args.opt_enum::<Priority>("priority_at_least", PRIORITIES)? {
        query.priority_at_least = Some(p);
    }

    // `open_only` and an explicit list of statuses are two ways to say the
    // same kind of thing, and a model will sometimes send both. The explicit
    // list wins, because it is the more specific request.
    let named = args.strings("statuses");
    if !named.is_empty() {
        let mut statuses = Vec::new();
        for raw in &named {
            let parsed: TaskStatus =
                serde_json::from_value(json!(raw.to_lowercase())).map_err(|_| {
                    args.bad(format!(
                        "`statuses` must contain only {}, got {raw:?}",
                        STATUSES.join(", ")
                    ))
                })?;
            statuses.push(parsed);
        }
        query.statuses = statuses;
    } else if args.bool_or("open_only", false) {
        query.statuses = TaskStatus::ALL.into_iter().filter(|s| s.is_open()).collect();
    }

    let rows = ctx.vault.tasks(&query)?;
    Ok(json!({
        "count": rows.len(),
        "tasks": rows.iter().map(task_json).collect::<Vec<_>>(),
    }))
}

fn run_get_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: TaskId = args.id("task_id", "task")?;
    let task = ctx.vault.task(id)?;
    let children = ctx.vault.tasks(&TaskQuery::children_of(id))?;
    let mut out = task_json(&task);
    if !children.is_empty() {
        out.as_object_mut()
            .unwrap()
            .insert("subtasks".into(), json!(children.iter().map(task_json).collect::<Vec<_>>()));
    }
    Ok(out)
}

fn run_create_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut t = Task::new(args.str("title")?);
    t.project_id = resolve_project(ctx, args, "project_id")?;
    t.parent_id = resolve_parent(ctx, args)?;
    t.notes = args.opt_str("notes").unwrap_or_default().to_string();
    if let Some(s) = args.opt_enum("status", STATUSES)? {
        t.set_status(s);
    }
    if let Some(p) = args.opt_enum("priority", PRIORITIES)? {
        t.priority = p;
    }
    t.due_date = args.opt_date("due_date")?;
    t.start_date = args.opt_date("start_date")?;
    t.estimate_minutes = args.opt_u32("estimate_minutes");
    t.tags = args.strings("tags");

    ctx.vault.save_task(&t)?;
    done("created", "task", &t.title, t.id.to_string())
}

fn run_update_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: TaskId = args.id("task_id", "task")?;
    let mut t = ctx.vault.task(id)?;

    if let Some(title) = args.opt_str("title") {
        t.title = title.to_string();
    }
    if let Some(notes) = args.opt_str("notes") {
        t.notes = notes.to_string();
    }
    // See `run_update_project`: `set_status` owns `completed_at`. A task
    // finished by assignment has no completion date, so "what did I get done
    // this week" never sees it; one reopened by assignment keeps the old one
    // and renders as a todo that was finished on Tuesday.
    if let Some(s) = args.opt_enum("status", STATUSES)? {
        t.set_status(s);
    }
    if let Some(p) = args.opt_enum("priority", PRIORITIES)? {
        t.priority = p;
    }
    // Clearing needs its own flag: a model has no way to send "no project"
    // in a field typed as a string, and `""` would have to be guessed at.
    if args.bool_or("clear_project", false) {
        t.project_id = None;
    } else if let Some(p) = resolve_project(ctx, args, "project_id")? {
        t.project_id = Some(p);
    }
    if args.bool_or("clear_due_date", false) {
        t.due_date = None;
    } else if let Some(d) = args.opt_date("due_date")? {
        t.due_date = Some(d);
    }
    if let Some(d) = args.opt_date("start_date")? {
        t.start_date = Some(d);
    }
    if let Some(e) = args.opt_u32("estimate_minutes") {
        t.estimate_minutes = Some(e);
    }
    if args.get("tags").is_some() {
        t.tags = args.strings("tags");
    }
    t.updated_at = Timestamp::now();

    ctx.vault.save_task(&t)?;
    done("updated", "task", &t.title, t.id.to_string())
}

/// Read the project an argument names, so a task cannot be filed into one
/// that does not exist.
///
/// Ids are untagged UUIDs and there is no foreign key on `project_id` -- the
/// column is a denormalised copy beside a sealed payload -- so an id the
/// model half-remembered is stored without complaint and produces a task
/// that is in neither the inbox nor on any board, while the tool reports
/// success. One read closes that, and its error tells the model how to get
/// a real one.
fn resolve_project(ctx: &ToolContext<'_>, args: &Args<'_>, key: &str) -> Result<Option<ProjectId>> {
    let Some(id) = args.opt_id::<ProjectId>(key, "project")? else { return Ok(None) };
    ctx.vault.project(id).map_err(|_| {
        Error::Invalid(format!(
            "no project with id {id}. Call list_projects and use an id from it."
        ))
    })?;
    Ok(Some(id))
}

/// The same, for a parent task. See [`resolve_project`].
fn resolve_parent(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Option<TaskId>> {
    let Some(id) = args.opt_id::<TaskId>("parent_id", "task")? else { return Ok(None) };
    ctx.vault.task(id).map_err(|_| {
        Error::Invalid(format!("no task with id {id}. Call list_tasks and use an id from it."))
    })?;
    Ok(Some(id))
}

fn run_delete_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: TaskId = args.id("task_id", "task")?;
    let t = ctx.vault.task(id)?;
    ctx.vault.delete_task(id)?;
    done("deleted", "task", &t.title, id.to_string())
}

// ---- time ---------------------------------------------------------------

/// The window a time question defaults to: today, and the week after it.
fn window(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<(Date, Date)> {
    let from = args.opt_date("from")?.unwrap_or(ctx.today);
    let to = args
        .opt_date("to")?
        .unwrap_or_else(|| from.checked_add(jiff::Span::new().days(7)).unwrap_or(from));
    if to < from {
        return Err(args.bad("`to` is before `from`"));
    }
    Ok((from, to))
}

fn run_list_events(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = window(ctx, args)?;
    let mut rows = ctx.vault.events(&EventQuery::between(from, to))?;
    rows.truncate(args.limit() as usize);
    Ok(json!({
        "count": rows.len(),
        "from": from.to_string(),
        "to": to.to_string(),
        "events": rows
            .iter()
            .map(|e| json!({
                "id": e.id.to_string(),
                "title": e.title,
                "date": e.local_date.to_string(),
                "all_day": e.all_day,
                "starts": e.start.to_string(),
                "ends": e.end.to_string(),
                "location": e.location,
            }))
            .collect::<Vec<_>>(),
    }))
}

fn run_list_blocks(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = window(ctx, args)?;
    let mut query = BlockQuery::between(from, to);
    if let Some(id) = args.opt_id::<TaskId>("task_id", "task")? {
        query.task_id = Some(id);
    }
    let mut rows = ctx.vault.blocks(&query)?;
    rows.truncate(args.limit() as usize);

    Ok(json!({
        "count": rows.len(),
        "blocks": rows
            .iter()
            .map(|b| json!({
                "id": b.id.to_string(),
                "date": b.local_date.to_string(),
                "starts": b.start.to_string(),
                "ends": b.end.to_string(),
                "minutes": b.minutes(),
                "kind": b.kind.as_str(),
                "for": block_subject_label(b),
            }))
            .collect::<Vec<_>>(),
    }))
}

fn block_subject_label(b: &TimeBlock) -> Value {
    match &b.subject {
        BlockSubject::Task { id } => json!({ "task_id": id.to_string() }),
        BlockSubject::Project { id } => json!({ "project_id": id.to_string() }),
        // Time that belongs to no record carries its own name in `title`,
        // which is the only thing that identifies it.
        BlockSubject::Adhoc => json!({ "label": b.title }),
    }
}

fn run_create_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let date = args.date("date")?;
    let start = clock(args, "start_time")?;
    let end = clock(args, "end_time")?;

    // Exactly one subject, checked here rather than left to the model's
    // reading of the description: a block for both a task and a project is
    // not a thing this domain has, and silently preferring one would put the
    // time against something nobody chose.
    let task: Option<TaskId> = args.opt_id("task_id", "task")?;
    let project: Option<ProjectId> = args.opt_id("project_id", "project")?;
    let label = args.opt_str("label");
    let (subject, label) = match (task, project, label) {
        (Some(id), None, None) => (BlockSubject::Task { id }, None),
        (None, Some(id), None) => (BlockSubject::Project { id }, None),
        (None, None, Some(text)) => (BlockSubject::Adhoc, Some(text)),
        (None, None, None) => {
            return Err(args.bad("give one of `task_id`, `project_id` or `label`"));
        }
        _ => return Err(args.bad("give only one of `task_id`, `project_id` or `label`")),
    };

    // Named before the block is built, so the reply can say what the time is
    // for -- and so a block against a task that does not exist fails here
    // rather than becoming an hour against nothing.
    let name = match &subject {
        BlockSubject::Task { id } => ctx.vault.task(*id)?.title,
        BlockSubject::Project { id } => ctx.vault.project(*id)?.name,
        BlockSubject::Adhoc => label.unwrap_or_default().to_string(),
    };

    let kind = match args.opt_str("kind") {
        Some("actual") => BlockKind::Actual,
        Some("planned") | None => BlockKind::Planned,
        Some(other) => {
            return Err(args.bad(format!("`kind` must be planned or actual, got {other:?}")));
        }
    };

    // Local wall-clock times, resolved in the person's zone. A block is
    // stored as two absolute instants -- so that durations and overlaps are
    // unambiguous -- but "two till three" means two till three where they
    // are sitting, not in UTC.
    let zone = jiff::tz::TimeZone::get(ctx.tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let start_ts = date
        .at(start.hour(), start.minute(), 0, 0)
        .to_zoned(zone.clone())
        .map_err(|e| args.bad(format!("{date} {start} does not exist in {}: {e}", ctx.tz)))?
        .timestamp();
    let end_ts = date
        .at(end.hour(), end.minute(), 0, 0)
        .to_zoned(zone)
        .map_err(|e| args.bad(format!("{date} {end} does not exist in {}: {e}", ctx.tz)))?
        .timestamp();
    if end_ts <= start_ts {
        return Err(args.bad("`end_time` must be after `start_time`"));
    }
    let minutes = u32::try_from((end_ts.as_second() - start_ts.as_second()) / 60).unwrap_or(0);

    let mut block = TimeBlock::new(subject, start_ts, minutes, ctx.tz);
    block.kind = kind;
    block.title = label.unwrap_or_default().to_string();
    block.notes = args.opt_str("notes").unwrap_or_default().to_string();

    ctx.vault.save_block(&block)?;
    done("created", "time block", &format!("{name} on {date}"), block.id.to_string())
}

fn clock(args: &Args<'_>, key: &str) -> Result<jiff::civil::Time> {
    let raw = args.str(key)?;
    raw.trim()
        .parse::<jiff::civil::Time>()
        // `09:00` is what the schema asks for and what models send; the
        // parser wants seconds, so the common form is tried with them added.
        .or_else(|_| format!("{}:00", raw.trim()).parse::<jiff::civil::Time>())
        .map_err(|_| args.bad(format!("`{key}` must be a time like 09:30, got {raw:?}")))
}

fn run_delete_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: BlockId = args.id("block_id", "time block")?;
    let block = ctx.vault.block(id)?;
    ctx.vault.delete_block(id)?;
    done("deleted", "time block", &block.local_date.to_string(), id.to_string())
}

// ---- the library --------------------------------------------------------

fn run_list_kinds(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    Ok(json!(
        ctx.vault
            .kinds()?
            .iter()
            .map(|k| json!({
                "id": k.id.to_string(),
                "name": k.name,
                "singular": k.singular,
            }))
            .collect::<Vec<_>>()
    ))
}

fn run_list_items(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = ItemQuery {
        kind_id: args.opt_id("shelf_id", "shelf")?,
        statuses: args.opt_enum::<ItemStatus>("status", ITEM_STATUSES)?.into_iter().collect(),
        text: args.opt_str("text").unwrap_or_default().to_string(),
        favourite: args.opt_bool("favourite"),
        finished_from: args.opt_date("finished_from")?,
        finished_to: args.opt_date("finished_to")?,
        limit: Some(args.limit()),
        ..Default::default()
    };
    let rows = ctx.vault.items(&query)?;
    Ok(json!({
        "count": rows.len(),
        "items": rows.iter().map(item_json).collect::<Vec<_>>(),
    }))
}

fn run_create_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let kind_id = args.id("shelf_id", "shelf")?;
    let kind = ctx.vault.kind(kind_id)?;

    let mut item = Item::new(kind_id, args.str("title")?);
    item.creator = args.opt_str("creator").unwrap_or_default().to_string();
    if let Some(s) = args.opt_enum("status", ITEM_STATUSES)? {
        item.set_status(s, ctx.today);
    }
    item.year = args.opt_u32("year").map(|y| y as i16);
    item.rating = rating_out_of_ten(args)?;
    item.notes = args.opt_str("notes").unwrap_or_default().to_string();
    item.tags = args.strings("tags");

    ctx.vault.save_item(&item)?;
    done(
        "created",
        &kind.singular,
        &format!("{} on {}", item.title, kind.name),
        item.id.to_string(),
    )
}

fn run_update_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ItemId = args.id("item_id", "item")?;
    let mut item = ctx.vault.item(id)?;
    let was = item.status;

    if let Some(title) = args.opt_str("title") {
        item.title = title.to_string();
    }
    if let Some(creator) = args.opt_str("creator") {
        item.creator = creator.to_string();
    }
    if let Some(s) = args.opt_enum("status", ITEM_STATUSES)? {
        // `set_status` is what makes the dates mean something: starting
        // something records when, finishing it records when, and putting it
        // back on the wishlist clears both -- otherwise an item bounced
        // through "done" by a misreading keeps a finish date it never
        // earned and is counted in the year's tally forever.
        item.set_status(s, ctx.today);
    }
    if let Some(r) = rating_out_of_ten(args)? {
        item.rating = Some(r);
    }
    if let Some(f) = args.opt_bool("favourite") {
        item.favourite = f;
    }
    if let Some(notes) = args.opt_str("notes") {
        item.notes = notes.to_string();
    }
    if let Some(d) = args.opt_date("started_on")? {
        item.started_on = Some(d);
    }
    if let Some(d) = args.opt_date("finished_on")? {
        item.finished_on = Some(d);
    }
    if args.get("tags").is_some() {
        item.tags = args.strings("tags");
    }

    item.updated_at = Timestamp::now();
    ctx.vault.save_item(&item)?;

    // The same transitions the interface logs, and for its reason: the
    // library's year in review is built from the log, so a status change
    // that skips it is a book that silently missed the list. Going back to
    // the wishlist is a correction rather than an event and logs nothing.
    if item.status != was {
        let event = match item.status {
            ItemStatus::Active => Some(LogEvent::Started),
            ItemStatus::Done => Some(LogEvent::Finished),
            ItemStatus::Paused | ItemStatus::Abandoned => Some(LogEvent::Stopped),
            ItemStatus::Wishlist => None,
        };
        if let Some(event) = event {
            let when = item.finished_on.filter(|_| item.status == ItemStatus::Done);
            let log = LogEntry::new(item.id, event, when.unwrap_or(ctx.today), ctx.tz);
            // The item is already saved. Failing the whole call because the
            // history line did not land would report "nothing happened" for
            // a change that did.
            if let Err(e) = ctx.vault.save_log(&log) {
                tracing::warn!(error = %e, "could not log a status change the assistant made");
            }
        }
    }
    done("updated", "item", &item.title, item.id.to_string())
}

fn run_delete_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ItemId = args.id("item_id", "item")?;
    let item = ctx.vault.item(id)?;
    ctx.vault.delete_item(id)?;
    done("deleted", "item", &item.title, id.to_string())
}

// ---- tracking -----------------------------------------------------------

fn run_list_trackers(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let rows: Vec<Value> = ctx
        .vault
        .trackers()?
        .into_iter()
        .filter(|t| !t.archived)
        .map(|t| {
            json!({
                "id": t.id.to_string(),
                "name": t.name,
                "kind": t.kind.as_str(),
                "unit": t.unit,
                "scale_max": t.scale_max,
                "cadence": t.cadence.map(|c| c.describe()),
            })
        })
        .collect();
    Ok(json!({ "count": rows.len(), "trackers": rows }))
}

fn run_log_reading(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let tracker_id: TrackerId = args.id("tracker_id", "tracker")?;
    let value = args
        .opt_f64("value")
        .ok_or_else(|| args.bad("`value` is required and must be a number"))?;
    let date = args.opt_date("date")?.unwrap_or(ctx.today);

    let mut reading = Reading::on(tracker_id, date, value);
    reading.note = args.opt_str("note").unwrap_or_default().to_string();
    // The vault clamps to the tracker's scale on the way in, which is why
    // the stored value is read back rather than echoed: a model told it
    // logged 99 when the vault stored 10 would report the wrong thing.
    ctx.vault.save_reading(&reading)?;
    let stored = ctx.vault.reading(reading.id)?;

    let name = ctx.vault.tracker(tracker_id).ok().map(|t| t.name);
    Ok(json!({
        "ok": true,
        "action": "recorded",
        "kind": "reading",
        "name": name.unwrap_or_default(),
        "id": reading.id.to_string(),
        "date": date.to_string(),
        "value": stored.value,
    }))
}

fn run_tracker_summary(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let to = args.opt_date("to")?.unwrap_or(ctx.today);
    let from = args
        .opt_date("from")?
        .unwrap_or_else(|| to.checked_sub(jiff::Span::new().days(30)).unwrap_or(to));
    if to < from {
        return Err(args.bad("`to` is before `from`"));
    }

    let mut query = ReadingQuery::between(from, to);
    if let Some(id) = args.opt_id::<TrackerId>("tracker_id", "tracker")? {
        query.tracker_ids = vec![id];
    }

    let days = ctx.vault.tracker_days(&query)?;

    // A day's one number depends on what the tracker *is*: doses sum, a
    // severity scale takes the worst of the day, a habit counts. Picking one
    // here would make half the charts wrong, so each day is reduced by its
    // own tracker's aggregate -- and the aggregate is named in the reply, so
    // the model can say "3 doses" rather than "3".
    let aggregates: std::collections::BTreeMap<TrackerId, crate::tracker::Aggregate> =
        ctx.vault.trackers()?.iter().map(|t| (t.id, t.kind.aggregate())).collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "count": days.len(),
        "days": days
            .iter()
            .map(|d| {
                // A tracker that has been deleted still has readings; sum
                // is the least wrong answer for one whose definition is gone.
                let aggregate = aggregates
                    .get(&d.tracker_id)
                    .copied()
                    .unwrap_or(crate::tracker::Aggregate::Sum);
                json!({
                    "tracker_id": d.tracker_id.to_string(),
                    "date": d.date.to_string(),
                    "value": d.value_for(aggregate),
                    "aggregate": format!("{aggregate:?}").to_lowercase(),
                    "readings": d.count,
                })
            })
            .collect::<Vec<_>>(),
    }))
}

fn run_list_readings(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let to = args.opt_date("to")?.unwrap_or(ctx.today);
    let from = args
        .opt_date("from")?
        .unwrap_or_else(|| to.checked_sub(jiff::Span::new().days(30)).unwrap_or(to));
    if to < from {
        return Err(args.bad("`to` is before `from`"));
    }

    let mut query = ReadingQuery::between(from, to);
    query.limit = Some(args.limit());
    query.timed_only = args.bool_or("timed_only", false);
    if let Some(id) = args.opt_id::<TrackerId>("tracker_id", "tracker")? {
        query.tracker_ids = vec![id];
    }

    // Names, once, so a hundred rows do not each carry one -- and so the
    // model can say "swimming" rather than a uuid.
    let names: BTreeMap<TrackerId, String> =
        ctx.vault.trackers()?.into_iter().map(|t| (t.id, t.name)).collect();

    let rows: Vec<Value> = ctx
        .vault
        .readings(&query)?
        .into_iter()
        .map(|r| {
            let mut row = json!({
                "date": r.local_date.to_string(),
                "value": r.value,
                "tracker": names.get(&r.tracker_id).cloned().unwrap_or_default(),
                "tracker_id": r.tracker_id.to_string(),
            });
            let m = row.as_object_mut().expect("just built an object");
            // The time only when there is one. A reading ticked while
            // writing up yesterday knows its day and nothing about 23:04,
            // and a defaulted instant is the answer that poisons every
            // hour-of-day question anybody asks of this data.
            if let Some(at) = r.at {
                m.insert("at".into(), json!(at.to_string()));
            }
            if !r.note.is_empty() {
                m.insert("note".into(), json!(r.note));
            }
            row
        })
        .collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "count": rows.len(),
        "readings": rows,
    }))
}

// ---- roles and goals ----------------------------------------------------

fn run_list_roles(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let mut rows = Vec::new();
    for role in ctx.vault.roles()? {
        if role.archived {
            continue;
        }
        let (goals, open) = ctx.vault.count_goals(role.id).unwrap_or((0, 0));
        rows.push(json!({
            "id": role.id.to_string(),
            "name": role.name,
            "goals": goals,
            "open_goals": open,
        }));
    }
    Ok(json!({ "count": rows.len(), "roles": rows }))
}

fn goal_json(goal: &Goal, role: Option<&str>, activity: Option<&GoalActivity>) -> Value {
    let mut out = json!({
        "id": goal.id.to_string(),
        "title": goal.title,
        "status": goal.status.as_str(),
        "role_id": goal.role_id.to_string(),
    });
    let m = out.as_object_mut().expect("just built an object");
    if let Some(role) = role {
        m.insert("role".into(), json!(role));
    }
    if !goal.notes.is_empty() {
        m.insert("notes".into(), json!(goal.notes));
    }
    if let Some(horizon) = goal.horizon {
        m.insert("horizon".into(), json!(horizon.to_string()));
    }
    if let Some(a) = activity {
        // Only what happened. A row of seven zeroes tells a model nothing
        // and costs it the tokens to read them.
        let mut had = serde_json::Map::new();
        if a.actual_minutes > 0 {
            had.insert("minutes".into(), json!(a.actual_minutes));
        }
        if a.open_tasks > 0 {
            had.insert("open_tasks".into(), json!(a.open_tasks));
        }
        if a.done_tasks > 0 {
            had.insert("done_tasks".into(), json!(a.done_tasks));
        }
        if a.entries > 0 {
            had.insert("entries".into(), json!(a.entries));
        }
        if a.readings > 0 {
            had.insert("readings".into(), json!(a.readings));
        }
        if a.items > 0 {
            had.insert("shelf_items".into(), json!(a.items));
        }
        match a.last_touched {
            Some(at) => {
                had.insert("last_touched".into(), json!(at.to_string()));
            }
            // Said rather than omitted: "never touched" is the answer
            // somebody asking about a goal most wants to hear.
            None => {
                had.insert("last_touched".into(), json!("never"));
            }
        }
        m.insert("activity".into(), Value::Object(had));
    }
    out
}

fn run_list_goals(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut query = GoalQuery { limit: Some(args.limit()), ..Default::default() };
    query.role_id = args.opt_id("role_id", "role")?;
    if let Some(status) = args.opt_enum::<GoalStatus>("status", &GOAL_STATUSES)? {
        query.statuses = vec![status];
    }

    let roles = ctx.vault.roles()?;
    let with_activity = args.bool_or("include_activity", false);
    let rows: Vec<Value> = ctx
        .vault
        .goals(&query)?
        .into_iter()
        .map(|goal| {
            let role = roles.iter().find(|r| r.id == goal.role_id).map(|r| r.name.as_str());
            let activity = if with_activity { ctx.vault.goal_activity(goal.id).ok() } else { None };
            goal_json(&goal, role, activity.as_ref())
        })
        .collect();
    Ok(json!({ "count": rows.len(), "goals": rows }))
}

fn run_create_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let role_id: RoleId = args.id("role_id", "role")?;
    let title = args.str("title")?.trim().to_string();
    if title.is_empty() {
        return Err(args.bad("`title` cannot be empty"));
    }
    let mut goal = Goal::new(role_id, title);
    goal.notes = args.opt_str("notes").unwrap_or_default().to_string();
    goal.horizon = args.opt_date("horizon")?;
    ctx.vault.save_goal(&goal)?;
    done("created", "goal", &goal.title, goal.id.to_string())
}

fn run_update_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: GoalId = args.id("goal_id", "goal")?;
    let mut goal = ctx.vault.goal(id)?;

    if let Some(title) = args.opt_str("title") {
        let title = title.trim();
        if title.is_empty() {
            return Err(args.bad("`title` cannot be emptied"));
        }
        goal.title = title.to_string();
    }
    if let Some(notes) = args.opt_str("notes") {
        goal.notes = notes.to_string();
    }
    if let Some(role_id) = args.opt_id::<RoleId>("role_id", "role")? {
        goal.role_id = role_id;
    }
    if let Some(horizon) = args.opt_date("horizon")? {
        goal.horizon = Some(horizon);
    }
    // Through `set_status`, not by assignment: it is what keeps the finished
    // stamp honest, and dropping a goal must not leave one behind.
    if let Some(status) = args.opt_enum::<GoalStatus>("status", &GOAL_STATUSES)? {
        goal.set_status(status);
    }
    goal.updated_at = jiff::Timestamp::now();
    ctx.vault.save_goal(&goal)?;
    done("updated", "goal", &goal.title, id.to_string())
}

fn run_delete_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: GoalId = args.id("goal_id", "goal")?;
    let goal = ctx.vault.goal(id)?;
    ctx.vault.delete_goal(id)?;
    done("deleted", "goal", &goal.title, id.to_string())
}

fn run_set_purpose(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let kind = args.str("kind")?;
    let id = args.str("id")?;
    let clear = args.bool_or("clear", false);
    let goal_id = args.opt_id::<GoalId>("goal_id", "goal")?;
    let role_id = args.opt_id::<RoleId>("role_id", "role")?;

    // Exactly one. Two would be a silent choice between them, and none
    // would be a call that looks like it worked and did nothing.
    let given =
        usize::from(clear) + usize::from(goal_id.is_some()) + usize::from(role_id.is_some());
    if given != 1 {
        return Err(args.bad("pass exactly one of `goal_id`, `role_id` or `clear`"));
    }

    let purpose = match (goal_id, role_id) {
        (Some(id), _) => {
            // Checked before anything is written, so a bad id is a refusal
            // rather than a record filed under nothing.
            ctx.vault.goal(id)?;
            Some(Purpose::Goal { id })
        }
        (_, Some(id)) => {
            ctx.vault.role(id)?;
            Some(Purpose::Role { id })
        }
        _ => None,
    };

    let name = match kind {
        "project" => {
            let mut p = ctx.vault.project(parse_id(args, "id", "project", id)?)?;
            p.purpose = purpose;
            let name = p.name.clone();
            ctx.vault.save_project(&p)?;
            name
        }
        "task" => {
            let mut t = ctx.vault.task(parse_id(args, "id", "task", id)?)?;
            t.purpose = purpose;
            let name = t.title.clone();
            ctx.vault.save_task(&t)?;
            name
        }
        "block" => {
            let mut b = ctx.vault.block(parse_id(args, "id", "block", id)?)?;
            b.purpose = purpose;
            let name = if b.title.is_empty() { "that hour".to_string() } else { b.title.clone() };
            ctx.vault.save_block(&b)?;
            name
        }
        "entry" => {
            let mut e = ctx.vault.entry(parse_id(args, "id", "entry", id)?)?;
            e.purpose = purpose;
            let name = e.display_title();
            // The version it was read at, so a filing that raced an edit in
            // the window loses rather than silently overwriting it.
            let expect = Some(e.updated_at);
            ctx.vault.save_entry(&e, expect)?;
            name
        }
        "item" => {
            let mut i = ctx.vault.item(parse_id(args, "id", "item", id)?)?;
            i.purpose = purpose;
            let name = i.title.clone();
            ctx.vault.save_item(&i)?;
            name
        }
        "tracker" => {
            let mut t = ctx.vault.tracker(parse_id(args, "id", "tracker", id)?)?;
            t.purpose = purpose;
            let name = t.name.clone();
            ctx.vault.save_tracker(&t)?;
            name
        }
        other => {
            return Err(args.bad(format!(
                "`kind` must be one of project, task, block, entry, item, tracker \u{2014} not {other:?}"
            )));
        }
    };

    Ok(json!({
        "ok": true,
        "action": if clear { "unfiled" } else { "filed" },
        "kind": kind,
        "name": name,
        "id": id,
    }))
}

/// Parse an id of a named kind, reporting it the way `Args` reports its own.
fn parse_id<T: std::str::FromStr>(
    args: &Args<'_>,
    field: &str,
    kind: &str,
    raw: &str,
) -> Result<T> {
    raw.parse::<T>().map_err(|_| args.bad(format!("`{field}` is not a {kind} id: {raw:?}")))
}

fn run_time_by_role(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let to = args.opt_date("to")?.unwrap_or(ctx.today);
    let from = args
        .opt_date("from")?
        .unwrap_or_else(|| to.checked_sub(jiff::Span::new().days(7)).unwrap_or(to));
    if to < from {
        return Err(args.bad("`to` is before `from`"));
    }

    let window = PurposeWindow::new(from, to);
    let roles = ctx.vault.roles()?;
    let goals = ctx.vault.goals(&GoalQuery::default())?;

    /// Recorded, planned, blocks, and somebody else's meetings.
    #[derive(Default)]
    struct Row {
        actual: u64,
        planned: u64,
        blocks: u64,
        meetings: u64,
    }

    // Folded to one row per role here rather than in SQL, for the reason the
    // interface folds it too: a goal's role is inside a sealed payload, and
    // asking the database to open every one of them to group a report would
    // undo the whole point of the pointer being a clear column.
    let mut rows: BTreeMap<Option<String>, Row> = BTreeMap::new();
    let name_of = |role: Option<RoleId>| -> Option<String> {
        role.and_then(|id| roles.iter().find(|r| r.id == id)).map(|r| r.name.clone())
    };

    for entry in ctx.vault.time_by_purpose(window)? {
        let role = match entry.purpose {
            Some(Purpose::Role { id }) => Some(id),
            Some(Purpose::Goal { id }) => goals.iter().find(|g| g.id == id).map(|g| g.role_id),
            None => None,
        };
        let slot = rows.entry(name_of(role)).or_default();
        slot.actual += entry.actual_minutes;
        slot.planned += entry.planned_minutes;
        slot.blocks += entry.blocks;
    }

    // Meetings are counted apart from the hours and never summed into them:
    // an event is somebody else's claim on an hour and a block is your own
    // record of one, and adding them double-counts every meeting you logged.
    for entry in ctx.vault.events_by_role(window).unwrap_or_default() {
        rows.entry(name_of(entry.role_id)).or_default().meetings += entry.minutes;
    }

    let out: Vec<Value> = rows
        .into_iter()
        .map(|(name, row)| {
            json!({
                // The unattributed row is named rather than left as null:
                // most of a life is not booked against anything, and a
                // report that dropped that share would be flattering.
                "role": name.unwrap_or_else(|| "not filed".into()),
                "minutes": row.actual,
                "planned_minutes": row.planned,
                "blocks": row.blocks,
                "meeting_minutes": row.meetings,
            })
        })
        .collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "roles": out,
    }))
}

// ---- memory -------------------------------------------------------------

fn run_remember(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if !ctx.vault.agent_settings()?.remember {
        return Err(Error::Invalid("remembering is switched off in this vault's settings".into()));
    }

    let fact = args.str("fact")?.trim();
    if fact.chars().count() > MAX_MEMORY_CHARS {
        return Err(args.bad(format!(
            "a memory must be under {MAX_MEMORY_CHARS} characters. \
             Keep it to one sentence, or write an entry instead."
        )));
    }

    let memory = match ctx.conversation {
        Some(id) => Memory::from_conversation(fact, id),
        None => Memory::new(fact),
    };
    let evicted = ctx.vault.save_memory(&memory)?;

    Ok(json!({
        "ok": true,
        "action": "remembered",
        "kind": "memory",
        "name": fact,
        "id": memory.id.to_string(),
        // Said out loud rather than done quietly: an assistant that silently
        // forgot something to make room for a new fact is one whose memory
        // nobody can reason about.
        "forgotten_to_make_room": evicted.iter().map(|m| m.text.clone()).collect::<Vec<_>>(),
    }))
}

fn run_list_memories(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    Ok(json!(
        ctx.vault
            .memories()?
            .iter()
            .map(|m| json!({
                "id": m.id.to_string(),
                "fact": m.text,
                "pinned": m.pinned,
            }))
            .collect::<Vec<_>>()
    ))
}

fn run_forget(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: MemoryId = args.id("memory_id", "memory")?;
    let existing = ctx.vault.memories()?;
    let memory = existing
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| Error::Invalid(format!("no memory with id {id}")))?;
    let text = memory.text.clone();
    ctx.vault.delete_memory(id)?;
    done("forgotten", "memory", &text, id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule that has to hold before there is a domain it applies to.
    ///
    /// A tool from a secret domain must be absent from what the model is
    /// offered -- not present and refused, not present behind a confirmation.
    /// A model that is told a tool exists can be talked into calling it, and
    /// a refusal it can see is a refusal it can try to talk its way around.
    #[test]
    fn a_secret_domain_is_never_offered_to_a_model() {
        for tool in ALL {
            if tool.domain.sensitivity() == Sensitivity::Secret {
                panic!(
                    "{} is in a secret domain and is in the catalogue; \
                     `available` filters it out, but nothing should be there to filter",
                    tool.name
                );
            }
        }
    }

    /// Every domain has said which it is. The match in `sensitivity` is
    /// exhaustive, so this is really a check that the list below was updated
    /// when a variant was added -- which is the moment the decision is made.
    #[test]
    fn every_domain_has_answered_the_question() {
        for domain in [
            Domain::Journals,
            Domain::Tasks,
            Domain::Calendars,
            Domain::Library,
            Domain::Trackers,
            Domain::Goals,
            Domain::Agent,
        ] {
            assert_eq!(domain.sensitivity(), Sensitivity::Ordinary, "{domain:?}");
        }
    }

    #[test]
    fn every_tool_has_a_unique_name_and_a_usable_schema() {
        let mut seen = std::collections::BTreeSet::new();
        for tool in catalog() {
            assert!(seen.insert(tool.name), "two tools are called {:?}", tool.name);
            assert!(
                !tool.description.trim().is_empty(),
                "{} has no description, so no model can choose it",
                tool.name
            );

            let schema = tool.parameters();
            assert_eq!(schema["type"], "object", "{} has a non-object schema", tool.name);
            let props = schema["properties"].as_object().expect("properties");
            // Every required argument must actually be declared, or the
            // provider rejects the whole tool list and nothing works.
            for required in schema["required"].as_array().unwrap() {
                let key = required.as_str().unwrap();
                assert!(
                    props.contains_key(key),
                    "{}: `{key}` is required but not declared",
                    tool.name
                );
            }
            for (key, prop) in props {
                assert!(
                    prop.get("description").and_then(|d| d.as_str()).is_some_and(|d| !d.is_empty()),
                    "{}: `{key}` has no description",
                    tool.name
                );
            }
        }
    }

    /// The confirmation gate is only as good as this list. A tool that
    /// removes something and is not marked destructive is one the gate never
    /// sees.
    #[test]
    fn every_tool_that_deletes_says_so() {
        for tool in catalog() {
            let deletes = tool.name.starts_with("delete_") || tool.name == "forget";
            assert_eq!(
                deletes,
                tool.effect == Effect::Destructive,
                "{} is {:?} but its name says otherwise",
                tool.name,
                tool.effect
            );
        }
    }

    #[test]
    fn the_catalogue_covers_every_domain_the_vault_has() {
        for domain in [
            Domain::Journals,
            Domain::Tasks,
            Domain::Calendars,
            Domain::Library,
            Domain::Trackers,
            Domain::Goals,
            Domain::Agent,
        ] {
            assert!(
                catalog().iter().any(|t| t.domain == domain),
                "nothing in the catalogue serves {domain:?}"
            );
        }
    }

    #[test]
    fn an_unknown_tool_is_named_rather_than_shrugged_at() {
        assert!(find("add_task").is_none(), "the tool is called create_task");
        assert!(find("create_task").is_some());
    }

    // ---- argument parsing ------------------------------------------------

    fn args(v: Value) -> (&'static str, Value) {
        ("test_tool", v)
    }

    #[test]
    fn null_is_treated_as_absent() {
        // Models emit `null` constantly for optional arguments, and a
        // schema-correct `{"project_id": null}` means "no project".
        let (name, v) = args(json!({ "project_id": null, "title": "x" }));
        let a = Args::new(name, &v);
        assert!(a.opt_str("project_id").is_none());
        assert_eq!(a.str("title").unwrap(), "x");
        assert!(a.opt_id::<ProjectId>("project_id", "project").unwrap().is_none());
    }

    #[test]
    fn a_number_sent_as_a_string_is_still_a_number() {
        let (name, v) = args(json!({ "estimate_minutes": "30", "limit": 10, "value": "2.5" }));
        let a = Args::new(name, &v);
        assert_eq!(a.opt_u32("estimate_minutes"), Some(30));
        assert_eq!(a.limit(), 10);
        assert_eq!(a.opt_f64("value"), Some(2.5));
    }

    #[test]
    fn the_result_limit_is_clamped_at_both_ends() {
        let (name, v) = args(json!({}));
        assert_eq!(Args::new(name, &v).limit(), DEFAULT_LIMIT, "an unasked limit has a default");

        let (name, v) = args(json!({ "limit": 100_000 }));
        assert_eq!(Args::new(name, &v).limit(), MAX_LIMIT, "a huge ask is capped, not honoured");

        let (name, v) = args(json!({ "limit": 0 }));
        assert_eq!(Args::new(name, &v).limit(), 1, "zero rows is never what was meant");
    }

    #[test]
    fn a_single_string_where_a_list_was_asked_for_is_understood() {
        let (name, v) = args(json!({ "tags": "work, urgent" }));
        assert_eq!(Args::new(name, &v).strings("tags"), vec!["work", "urgent"]);

        let (name, v) = args(json!({ "tags": ["work", "  ", "urgent"] }));
        assert_eq!(Args::new(name, &v).strings("tags"), vec!["work", "urgent"]);

        let (name, v) = args(json!({}));
        assert!(Args::new(name, &v).strings("tags").is_empty());
    }

    #[test]
    fn a_relative_date_is_refused_with_advice_rather_than_a_serde_error() {
        // The error is a prompt: it is sent back to the model, which then
        // gets one more turn to be right.
        let (name, v) = args(json!({ "due_date": "next friday" }));
        let err = Args::new(name, &v).opt_date("due_date").unwrap_err().to_string();
        assert!(err.contains("2026-09-14"), "should show the format: {err}");
        assert!(err.contains("next friday"), "should quote what it got: {err}");
        assert!(err.contains("test_tool"), "should name the tool: {err}");
    }

    #[test]
    fn a_bad_id_says_which_kind_was_wanted_and_how_to_get_one() {
        let (name, v) = args(json!({ "task_id": "the deck one" }));
        let err = Args::new(name, &v).id::<TaskId>("task_id", "task").unwrap_err().to_string();
        assert!(err.contains("task id"), "got {err}");
        assert!(err.contains("List or search"), "should say how to recover: {err}");
    }

    #[test]
    fn an_enum_is_case_insensitive_and_lists_its_alternatives() {
        let (name, v) = args(json!({ "status": "Done" }));
        let parsed: TaskStatus = Args::new(name, &v).opt_enum("status", STATUSES).unwrap().unwrap();
        assert_eq!(parsed, TaskStatus::Done);

        let (name, v) = args(json!({ "status": "finished" }));
        let err =
            Args::new(name, &v).opt_enum::<TaskStatus>("status", STATUSES).unwrap_err().to_string();
        assert!(err.contains("backlog, todo"), "should list what is allowed: {err}");
    }

    #[test]
    fn a_missing_required_argument_names_itself() {
        let (name, v) = args(json!({}));
        let err = Args::new(name, &v).str("title").unwrap_err().to_string();
        assert!(err.contains("`title` is required"), "got {err}");
    }
}
