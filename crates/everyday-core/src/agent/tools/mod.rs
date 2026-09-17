//! What the assistant can actually do, and the arguments it must get right.
//!
//! This is the whole of the agent's capability, and it is here — in the core,
//! synchronous, with no model provider and no socket anywhere — for the same
//! reason [`ics`](crate::ics) and [`websearch`](crate::websearch) are. The
//! shell above wraps each of these in whatever shape its harness wants and
//! opens the connection; every question about what a tool *does* is answered
//! and tested in this file, offline, in microseconds.
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
//!
//! # Layout
//!
//! Declaring a tool is a few lines; the run function and the JSON
//! projections behind it are a few hundred, so each domain has its own file
//! — `journals`, `notes`, `tasks`, `time`, `library`, `trackers`, `purpose`,
//! `routines`, `memory` — ending in a `TOOLS` slice this module concatenates
//! in catalogue order. Orientation (`overview`, `search`) travels with
//! `journals`, since both use [`Domain::Journals`] and neither is big
//! enough to want a file of its own.

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::id::{ConversationId, GoalId, RoleId};
use crate::purpose::Purpose;
use crate::vault::Vault;
use jiff::civil::Date;

/// Declare one entry in a domain's `TOOLS` slice.
///
/// The six-argument form is every tool that destroys nothing and cannot be
/// proposed while dreaming, or that has nothing worth saying about what it
/// would destroy; the seven-argument form adds a `describe` function for a
/// destructive tool the confirmation gate needs to name a subject for; the
/// eight-argument form adds a `build` function on top, for a tool that
/// [`dispatch`] may turn into a [`crate::proposal::Proposal`] instead of
/// running -- see [`Built`]. A tool with a `build` but nothing to `describe`
/// passes `None` explicitly in the seventh position rather than gaining a
/// ninth arity, since arity alone cannot tell "no describe" from "no build"
/// apart once both are optional. Splitting on arity rather than always
/// asking for every argument keeps the common case -- most tools have
/// neither -- free of the `None, None` nobody reads.
///
/// Defined before the domain modules below so that each of them can call it
/// by name: a `macro_rules!` macro is only visible to code that comes after
/// it textually, including a `mod` declared afterwards.
macro_rules! tool {
    ($name:literal, $effect:ident, $domain:ident, $schema:expr, $desc:literal, $run:expr) => {
        tool!($name, $effect, $domain, $schema, $desc, $run, None, None)
    };
    ($name:literal, $effect:ident, $domain:ident, $schema:expr, $desc:literal, $run:expr, $describe:expr) => {
        tool!($name, $effect, $domain, $schema, $desc, $run, $describe, None)
    };
    ($name:literal, $effect:ident, $domain:ident, $schema:expr, $desc:literal, $run:expr, $describe:expr, $build:expr) => {
        Tool {
            name: $name,
            description: $desc,
            // Fully qualified rather than `Effect::$effect`: a path whose
            // last segment is a metavariable captured from the call site
            // resolves its leading segment at the call site too, which is
            // fine when the macro is used in this file and breaks the
            // moment a domain module -- which does not import `Effect` or
            // `Domain`, and should not have to -- calls it instead.
            effect: crate::agent::tools::Effect::$effect,
            domain: crate::agent::tools::Domain::$domain,
            schema: || $schema,
            run: $run,
            describe: $describe,
            build: $build,
        }
    };
}

mod journals;
mod library;
mod mail;
mod meetings;
mod memory;
mod notes;
mod purpose;
mod routines;
mod tasks;
mod time;
mod trackers;

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
    /// Reaches somebody who is not the vault's owner. `send_draft` and
    /// `respond_to_invite` carry this effect: sending removes nothing -- a
    /// [`Destructive`](Effect::Destructive) call and this one are answering
    /// different questions -- but neither can be taken back either, and both
    /// put words in front of a stranger rather than only changing a
    /// record. So it is confirmed in chat unconditionally, independent of
    /// [`AgentSettings::confirm_destructive`](crate::agent::AgentSettings::confirm_destructive)
    /// -- there is no setting that sends without asking -- and an
    /// unattended routine run refuses it outright rather than asking a
    /// question nobody is there to answer, the same way `create_routine`
    /// already refuses to make more of itself. See
    /// `agent::tools::mail`'s module docs for the whole of the reasoning.
    Outward,
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
    Notes,
    Tasks,
    Calendars,
    Library,
    Trackers,
    Purpose,
    /// The assistant's own standing work.
    Routines,
    Agent,
    /// Mailboxes, threads, drafts and the outbox -- see `agent::tools::mail`.
    ///
    /// Availability here only asks what every other domain asks: does the
    /// backend store this at all. *Which* mail tools a given caller actually
    /// sees is a second, caller-shaped question -- whether any account
    /// permits them -- that [`available`] does not ask and [`available_for`]
    /// does; see that function and `agent::tools::mail::offered_to`.
    Mail,
    /// Meeting notes: the recording history and the transcripts kept beside
    /// them -- see `agent::tools::meetings`. Read-only: nothing here starts
    /// a recording or names a speaker, both of which need the pipeline
    /// `everyday_service::meeting` owns.
    Meetings,
}

impl Domain {
    /// Every domain there is, in no particular order. Exists so a test that
    /// means "every domain" can say so, rather than enumerating them by hand
    /// and silently going stale the day one is added.
    pub const ALL: [Domain; 11] = [
        Domain::Journals,
        Domain::Notes,
        Domain::Tasks,
        Domain::Calendars,
        Domain::Library,
        Domain::Trackers,
        Domain::Purpose,
        Domain::Routines,
        Domain::Agent,
        Domain::Mail,
        Domain::Meetings,
    ];

    fn available(self, vault: &Vault) -> bool {
        self.sensitivity() == Sensitivity::Ordinary
            && match self {
                Domain::Journals => true,
                Domain::Notes => vault.supports_notes(),
                Domain::Tasks => vault.supports_tasks(),
                Domain::Calendars => vault.supports_calendars(),
                Domain::Library => vault.supports_library(),
                Domain::Trackers => vault.supports_trackers(),
                Domain::Purpose => vault.supports_purpose(),
                Domain::Routines => vault.supports_routines(),
                Domain::Agent => vault.supports_agent(),
                Domain::Mail => vault.supports_mail() && vault.supports_accounts(),
                Domain::Meetings => vault.supports_meetings(),
            }
    }

    /// Written out rather than defaulted, so adding a domain is a decision
    /// somebody made rather than one they inherited. A `Passwords` variant
    /// added here without a line in this match will not compile.
    pub fn sensitivity(self) -> Sensitivity {
        match self {
            Domain::Journals
            // A note is writing, like an entry, and is the place the
            // assistant puts anything longer than a paragraph of its own.
            | Domain::Notes
            | Domain::Tasks
            | Domain::Calendars
            | Domain::Library
            | Domain::Trackers
            // Roles and goals are the shape of somebody's life rather than
            // its contents, and the assistant is specifically for reasoning
            // about them -- "what did I actually spend the week on" is the
            // question the Overview exists to answer.
            | Domain::Purpose
            // Its own work. A routine that made a routine is the one
            // recursion worth thinking about, and the answer is in
            // `run_create_routine`: an unattended run may not.
            | Domain::Routines
            | Domain::Agent
            // Mail is written by strangers and its tools reach strangers,
            // which is a real risk -- but it is not the risk `Secret`
            // exists for. `Secret` is about *disclosure*: a tool that
            // should never be offered at all, whatever the settings say.
            // Mail's risk is handled instead, and handled per call: the
            // per-account switch in `AgentMailAccess`, the assistant's own
            // acknowledgement gate, `Effect::Outward`'s unconditional
            // confirmation, and the refusal on an unattended run. Excluding
            // the whole domain would also remove the thing that makes those
            // gates worth having -- an assistant that can triage and draft.
            | Domain::Mail
            // A transcript is every word somebody said on a call, which
            // sounds like the argument for `Secret` -- but the assistant is
            // specifically for reasoning about what was discussed, the same
            // case `Purpose` already makes for the shape of somebody's life.
            // What a voiceprint actually protects -- that biometric data
            // never leaves the machine, and is never handed to a model at
            // all -- is a guarantee this catalogue does not carry in the
            // first place: no tool here returns one, or a `voiceprint_id`,
            // or anything an embedding could be reconstructed from.
            | Domain::Meetings => Sensitivity::Ordinary,
        }
    }
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

/// One thing the assistant can do.
#[derive(Clone, Copy)]
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
    /// What a destructive call is about to act on, in the person's own
    /// words. `None` for a tool that destroys nothing, and for one that
    /// does but has nothing useful to say -- see [`describe`].
    ///
    /// A field on the tool rather than a name-matched list beside the
    /// catalogue, which is what this used to be: a destructive tool added
    /// without setting it fell through to a blank confirmation card, and
    /// nothing said so until the day it shipped. Data, not a naming
    /// convention -- the same argument [`Effect::Destructive`] makes for
    /// itself, applied to the one thing about it that used to live apart.
    describe: Option<fn(&ToolContext<'_>, &Args<'_>) -> Option<String>>,
    /// Parse this call's arguments and construct the finished record --  or,
    /// for a deletion or a send, the payload that names what it would act
    /// on -- without saving anything. `None` for a tool [`dispatch`] cannot
    /// turn into a proposal: while drafting, calling one refuses outright
    /// rather than running it or silently doing nothing. See [`Built`] and
    /// `docs/plans/dreaming.md`.
    build: Option<fn(&ToolContext<'_>, &Args<'_>) -> Result<Built>>,
}

/// What a tool's builder produced, on its way to becoming a
/// [`crate::proposal::Proposal`].
///
/// Everything a proposal needs beyond what [`crate::proposal::Proposal::new`]
/// already works out from the payload -- the caption a confirmation card
/// would have shown, and what the call was reacting to, if anything.
pub struct Built {
    pub payload: crate::proposal::Payload,
    /// The sentence [`describe`] would have shown for this call, or its own
    /// equivalent for a tool that does not delete anything -- "Create task:
    /// Book the dentist", "Change task: …", "Delete task: …".
    pub caption: String,
    /// What this reacts to, if it acts on a record that already exists:
    /// the task or note or memory an update or delete names, or -- for a
    /// planned block -- the task it was for. `None` for a call that makes
    /// something with nothing behind it yet.
    pub about: Option<crate::proposal::About>,
}

impl Tool {
    /// JSON Schema for this tool's arguments, in the shape every provider's
    /// function-calling API wants.
    pub fn parameters(&self) -> Value {
        (self.schema)()
    }

    /// Whether this tool can become a [`crate::proposal::Proposal`] instead
    /// of running, while drafting. A tool with no builder refuses outright
    /// rather than running for real or silently doing nothing -- see
    /// [`dispatch`].
    pub fn can_propose(&self) -> bool {
        self.build.is_some()
    }
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).field("effect", &self.effect).finish()
    }
}

/// Which non-person caller is driving a tool call, if any.
///
/// `None` on [`ToolContext::caller`] means the vault's owner is acting
/// directly -- the interface's own palette, a keyboard shortcut, a script
/// run through `run_tool` with nothing else set -- and is unrestricted by
/// anything in [`crate::account::AgentMailAccess`], which exists to gate an
/// *agent*, not the person whose vault it is. `Some` names which of the two
/// agents this is, so `agent::tools::mail` -- the one domain that reads it
/// today -- can read the matching switch off each account and stamp the
/// matching [`crate::mail::Origin`] on every write it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The chat assistant, in a named conversation.
    Assistant { conversation: ConversationId },
    /// An external agent over MCP, named by the device id it paired as.
    /// A plain `String` rather than a typed id: MCP clients are rows in
    /// `everyday-server`'s own device registry, which this crate does not
    /// know about.
    Mcp { client: String },
}

/// The prefix `everyday-server`'s device registry (`auth::Registry`) stamps
/// on the id of a device it minted specifically for an MCP client, so that
/// anything downstream can tell an MCP-issued device apart from an
/// ordinarily-paired one *without* trusting whatever a request's own body
/// happens to claim about itself.
///
/// This has to live here, in `everyday-core`, rather than beside the
/// `Device` type it actually describes, in `everyday-server`'s `auth`
/// module: `everyday-service`'s `domains::meta::run_tool` -- the one place
/// that has to turn "which device authenticated this call" into "which
/// [`Caller`] this call may claim to be" -- cannot depend on
/// `everyday-server`, which is built on top of it. `everyday-core` is the
/// one crate both sides already share.
///
/// See `everyday_service::domains::meta::WireCaller`'s module doc for the
/// vulnerability this exists to close: a request's body was previously the
/// *only* thing that said whether a call was MCP's or the vault owner's
/// own, and a body is exactly the thing an untrusted caller controls.
pub const MCP_DEVICE_ID_PREFIX: &str = "mcp:";

/// Whether `id` -- the device id carried by `everyday_service::ctx::
/// Caller::Device`, as built by `everyday-server`'s `auth::Registry` -- names
/// a device minted for an MCP client. See [`MCP_DEVICE_ID_PREFIX`].
pub fn is_mcp_device_id(id: &str) -> bool {
    id.starts_with(MCP_DEVICE_ID_PREFIX)
}

/// The type behind [`ToolContext::mail_rate_limit`], named so the field
/// itself does not spell out a function pointer inline -- clippy's own
/// `type_complexity` lint, and a reader's, agree that a closure type is
/// worth a name once it has an argument and a `Result` in it.
pub type MailRateLimit<'a> = dyn Fn(&crate::mail::Origin) -> Result<()> + 'a;

/// The type behind [`ToolContext::invite_responder`].
///
/// `everyday-core` has no dependency on `everyday-mail` or `calcard` -- the
/// crates that actually know how to read a `text/calendar` part and build
/// an iTIP `REPLY` -- so `respond_to_invite` (in `agent::tools::mail`)
/// cannot do that work itself. Instead it calls back through this closure,
/// which the service builds over the very same function its own
/// `respond_to_invite` command wraps
/// (`everyday_service::domains::mail::respond_to_invite_inner`), so the
/// tool and a person's own click run identical code. The arguments are the
/// message carrying the invitation, the chosen response, an optional
/// comment for the organiser, and the [`crate::mail::Origin`] the write
/// should be stamped with -- everything [`crate::mail::AttendeeResponse`]
/// and the rest of this crate already know how to name, so the type is
/// nameable here without core learning what an `ICalendar` is.
pub type InviteResponder<'a> = dyn Fn(
        crate::id::MailMessageId,
        crate::mail::AttendeeResponse,
        Option<String>,
        crate::mail::Origin,
    ) -> Result<()>
    + 'a;

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
    /// from. `None` outside a conversation — a script, or a test.
    pub conversation: Option<ConversationId>,
    /// Whether this call is part of a scheduled run with nobody watching.
    ///
    /// One tool reads it: `create_routine`, which refuses. A routine that
    /// makes routines, running every morning, is a way to wake up owning
    /// forty of them that nobody asked for. `send_draft` reads it too, for
    /// the very same reason.
    pub unattended: bool,
    /// Who this call is being made on behalf of, if not the vault's owner
    /// acting directly. See [`Caller`].
    pub caller: Option<Caller>,
    /// Mail's search index, if this vault's mail storage opened cleanly this
    /// session -- `everyday_service::Service::mail_index`'s own view, handed
    /// down rather than reached for here, since the core has no notion of a
    /// running service to ask. `None` is read as "nothing to search" by
    /// `search_mail`, never as an error -- the same tolerance every other
    /// caller of [`crate::mailsearch::MailSearch::rebuild_needed`] already
    /// has for a derived structure that can always be rebuilt.
    pub mail_search: Option<&'a dyn crate::mailsearch::MailSearch>,
    /// The provider the assistant is actually configured to use right now,
    /// compared against
    /// [`crate::account::Account::assistant_provider_acknowledged`] by
    /// [`crate::agent::LLMProviderConfig::acknowledgement_name`]. `None` for
    /// every caller but the assistant, since nothing else reads it.
    pub assistant_provider: Option<String>,
    /// The one gate a mail write's enqueue passes through before it reaches
    /// the outbox, wired to
    /// `everyday_service::Service::check_mail_rate_limit` by whichever of
    /// `agent.rs` or `meta.rs` built this context. Takes the
    /// [`crate::mail::Origin`] the write is about to enqueue with. `None`
    /// for the vault's owner acting directly and for a test that does not
    /// need one -- see
    /// [`crate::mail::Origin::is_rate_limited`](crate::mail::Origin) for why
    /// a person is never checked against it at all.
    pub mail_rate_limit: Option<&'a MailRateLimit<'a>>,
    /// Called once, with the account touched, after every mail tool's write
    /// actually lands -- wired to
    /// `everyday_service::Service::notify_mail_write` by whichever of
    /// `agent.rs` or `meta.rs` built this context, exactly the way
    /// [`ToolContext::mail_rate_limit`] already is.
    ///
    /// What wakes that account's sync task the moment an assistant or MCP
    /// write enqueues an outbox op, and invalidates the cached unread
    /// counts `mail_unread_cache` answers from, the same two things
    /// `everyday_service::domains::mail`'s own `batch_op` and `send_draft`
    /// already do for a person's own click. Before this field existed the
    /// tools in `agent::tools::mail` wrote straight through the vault and
    /// stopped there, so a person watching the inbox would not see an
    /// assistant's archive, nor an unread count it changed, until whatever
    /// unrelated event next happened to refresh either. `None` for the
    /// vault's owner acting directly (nothing here needs waking on its own
    /// behalf -- the interface already reacts to its own writes) and for a
    /// test with nothing wired up.
    pub after_mail_write: Option<&'a dyn Fn(crate::id::AccountId)>,
    /// The one gate `respond_to_invite` passes an answer through to actually
    /// build and queue the iTIP `REPLY` -- see [`InviteResponder`]. `None`
    /// for the vault's owner acting directly (that path is
    /// `everyday_service::domains::mail::respond_to_invite`, which needs no
    /// tool at all) and for a test that has nothing wired up, in which case
    /// the tool refuses with "not available right now" rather than
    /// panicking on a missing hook.
    pub invite_responder: Option<&'a InviteResponder<'a>>,
    /// Set when this call is part of work nobody asked for -- today, a dream.
    ///
    /// While it is set, a `Write` or `Destructive` tool builds its record and
    /// hands it to the vault as a [`crate::proposal::Proposal`] instead of
    /// saving it. `None` for everyone else, which is every caller that
    /// existed before dreaming did. See `docs/plans/dreaming.md`.
    pub drafting: Option<Drafting>,
}

/// How a drafting call is made. See [`ToolContext::drafting`].
#[derive(Debug, Clone, Default)]
pub struct Drafting {
    /// Who the proposals are recorded as made by.
    pub source: Option<crate::proposal::ProposalSource>,
    /// Tools that run for real even while drafting -- a dream's one note.
    /// The caller is responsible for any budget on them.
    pub direct: Vec<&'static str>,
    /// How many proposals this source may make in total. `None` is no cap
    /// beyond the vault's own on pending proposals.
    pub max_proposals: Option<usize>,
}

/// The name of the optional argument every writing tool accepts while
/// drafting: one line on why the proposal is being made.
pub const WHY_ARG: &str = "why";

impl Tool {
    /// The tool's argument schema as offered to a caller that is drafting:
    /// the same, plus an optional [`WHY_ARG`] on every tool that writes.
    pub fn parameters_for(&self, drafting: bool) -> Value {
        let mut schema = self.parameters();
        if drafting
            && self.effect != Effect::Read
            && let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut)
        {
            props.insert(
                WHY_ARG.to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "One short sentence, shown to the person beside the \
                        proposal: why you are proposing this, with the evidence."
                }),
            );
        }
        schema
    }
}

impl<'a> ToolContext<'a> {
    /// Now, in the zone this context was built with.
    ///
    /// Built from `today` and the zone rather than from the clock, so a tool
    /// that says when something will next happen agrees with the one that
    /// says what is due today.
    pub fn now(&self) -> jiff::Zoned {
        jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::get(self.tz).unwrap_or(jiff::tz::TimeZone::UTC))
    }
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

    /// Whether `key` was actually named in this call, with a non-null
    /// value -- `[]` counts, `null` and an absent key do not.
    ///
    /// What tells "omitted" apart from "sent as an empty list" for a field
    /// like `update_draft`'s `bcc`, which [`Args::strings`] alone cannot:
    /// an omitted `bcc` and an explicit `"bcc": []` both read back as an
    /// empty `Vec` from that method, so a caller that needs to *clear* a
    /// list -- remove every Bcc a previous call added -- has no way to say
    /// so if the two are conflated. This checks the raw JSON for the key's
    /// presence instead of asking what it decoded to.
    pub fn has_key(&self, key: &str) -> bool {
        self.value.get(key).is_some_and(|v| !v.is_null())
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

    /// A time of day, as `HH:MM` in 24-hour time -- the same shape
    /// `create_time_block`'s `start_time` and `end_time` already ask for.
    pub fn opt_time(&self, key: &str) -> Result<Option<jiff::civil::Time>> {
        let Some(raw) = self.opt_str(key) else { return Ok(None) };
        raw.trim()
            .parse::<jiff::civil::Time>()
            // `09:00` is what the schema asks for and has no seconds; the
            // parser wants them, so the common form is tried with them added.
            .or_else(|_| format!("{}:00", raw.trim()).parse::<jiff::civil::Time>())
            .map(Some)
            .map_err(|_| self.bad(format!("`{key}` must be a time like 09:30, got {raw:?}")))
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

    /// A `from`/`to` date window, defaulted around today when the model
    /// leaves either end unsaid.
    ///
    /// Every reporting tool needs one of these and used to carry its own
    /// copy of the same three lines: default one end from the other, then
    /// refuse a window that runs backwards. `default` says which way an
    /// unset half grows -- forward from today for "what is coming up",
    /// back from today for "how has this been lately" -- which is the one
    /// thing that actually varies between callers.
    pub fn window(&self, ctx: &ToolContext<'_>, default: Window) -> Result<(Date, Date)> {
        let (from, to) = match default {
            Window::Ahead(days) => {
                let from = self.opt_date("from")?.unwrap_or(ctx.today);
                let to = self.opt_date("to")?.unwrap_or_else(|| {
                    from.checked_add(jiff::Span::new().days(i64::from(days))).unwrap_or(from)
                });
                (from, to)
            }
            Window::Back(days) => {
                let to = self.opt_date("to")?.unwrap_or(ctx.today);
                let from = self.opt_date("from")?.unwrap_or_else(|| {
                    to.checked_sub(jiff::Span::new().days(i64::from(days))).unwrap_or(to)
                });
                (from, to)
            }
        };
        if to < from {
            return Err(self.bad("`to` is before `from`"));
        }
        Ok((from, to))
    }
}

/// Which way an unset half of an [`Args::window`] grows from today.
pub enum Window {
    /// Forward: a calendar-shaped question about what is coming up.
    Ahead(u32),
    /// Back: a history-shaped question about how something has been.
    Back(u32),
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

/// Read `goal_id`/`role_id` off a call into the [`Purpose`] it names,
/// checking that whichever was given still exists before anything is built
/// against it -- the same reasoning `tasks::resolve_project` gives for a
/// project id. Shared by every tool that lets a model file a record against
/// a goal or a role directly: `create_task`, `update_task` and
/// `create_time_block`.
///
/// `Ok(None)` when neither is given, which the caller reads as "leave the
/// purpose alone" on an update and "no purpose" on a create.
fn resolve_purpose(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Option<Purpose>> {
    let goal_id: Option<GoalId> = args.opt_id("goal_id", "goal")?;
    let role_id: Option<RoleId> = args.opt_id("role_id", "role")?;
    match (goal_id, role_id) {
        (Some(_), Some(_)) => Err(args.bad("give only one of `goal_id` or `role_id`")),
        (Some(id), None) => {
            ctx.vault.goal(id).map_err(|_| {
                Error::Invalid(format!(
                    "no goal with id {id}. Call list_goals and use an id from it."
                ))
            })?;
            Ok(Some(Purpose::Goal { id }))
        }
        (None, Some(id)) => {
            ctx.vault.role(id).map_err(|_| {
                Error::Invalid(format!(
                    "no role with id {id}. Call list_roles and use an id from it."
                ))
            })?;
            Ok(Some(Purpose::Role { id }))
        }
        (None, None) => Ok(None),
    }
}

/// Cut a string to at most `max` characters -- never bytes, since a
/// multi-byte character sliced in half is not a shorter string but a
/// corrupt one. Shared by every cap on what a model writes freehand: a
/// proposal's `why` today.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// What a mutating tool says when it worked.
///
/// Uniform on purpose. The model has to tell the person what happened, and a
/// shape that always carries the human-readable name means it never has to
/// quote a UUID at somebody to prove it did something.
fn done(action: &str, kind: &str, name: &str, id: String) -> Result<Value> {
    Ok(json!({ "ok": true, "action": action, "kind": kind, "name": name, "id": id }))
}

// ---- the catalogue ------------------------------------------------------

/// Every tool that exists, in catalogue order: orientation first, then the
/// domains in the order someone would work through them. Built once, from
/// each domain's own `TOOLS` slice, so the concatenation happens a single
/// time no matter how often the assistant asks what it can do.
fn all() -> &'static [Tool] {
    static ALL: OnceLock<Vec<Tool>> = OnceLock::new();
    ALL.get_or_init(|| {
        [
            journals::TOOLS,
            notes::TOOLS,
            tasks::TOOLS,
            time::TOOLS,
            library::TOOLS,
            trackers::TOOLS,
            purpose::TOOLS,
            routines::TOOLS,
            memory::TOOLS,
            mail::TOOLS,
            meetings::TOOLS,
        ]
        .concat()
    })
}

/// Every tool that exists.
///
/// Order is the order the model sees them in, and it is not arbitrary:
/// orientation first, then the domains in the order someone would work
/// through them. A model choosing between thirty names does better when the
/// list reads like a table of contents than when it reads like a hash map.
pub fn catalog() -> &'static [Tool] {
    all()
}

/// The tools this vault can actually offer, given what its backend stores.
///
/// The vault's owner's own view: every domain the backend carries, mail
/// included with nothing further asked of it. A caller that is an *agent*
/// rather than the vault's owner wants [`available_for`] instead.
pub fn available(vault: &Vault) -> Vec<&'static Tool> {
    available_for(vault, None, None)
}

/// As [`available`], filtered further for one caller.
///
/// Every domain but [`Domain::Mail`] answers the whole question from the
/// backend alone, the same as [`available`] -- `caller` and
/// `assistant_provider` change nothing for them. Mail is different: a tool
/// no account permits `caller` to use at all is hidden rather than offered
/// and refused, on the same reasoning
/// [`Sensitivity::Secret`](Sensitivity::Secret) is hidden rather than
/// refused -- a model that is never told a tool exists cannot be talked
/// into calling it. See `agent::tools::mail::offered_to` for exactly what
/// "permits" means per tool.
///
/// `caller: None` reads as the vault's owner acting directly, unrestricted;
/// `assistant_provider` is only consulted when `caller` is
/// [`Caller::Assistant`] and is the string
/// [`crate::agent::LLMProviderConfig::acknowledgement_name`] produces for
/// whatever is configured right now.
pub fn available_for(
    vault: &Vault,
    caller: Option<&Caller>,
    assistant_provider: Option<&str>,
) -> Vec<&'static Tool> {
    all()
        .iter()
        .filter(|t| t.domain.available(vault))
        .filter(|t| {
            t.domain != Domain::Mail || mail::offered_to(vault, t.name, caller, assistant_provider)
        })
        .collect()
}

/// Look a tool up by the name the model used.
pub fn find(name: &str) -> Option<&'static Tool> {
    all().iter().find(|t| t.name == name)
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
/// a card with no subject rather than as an id nobody can check -- or when
/// `name` is not a tool, or is one with nothing to describe.
pub fn describe(ctx: &ToolContext<'_>, name: &str, arguments: &Value) -> Option<String> {
    let tool = find(name)?;
    let describe = tool.describe?;
    describe(ctx, &Args::new(tool.name, arguments))
}

/// Run one tool call.
///
/// Refuses a tool the vault cannot serve rather than failing somewhere
/// deeper with a message about storage backends, and refuses an unknown name
/// with the list of real ones — which is what a model that has invented a
/// tool needs in order to recover on the next turn.
///
/// While [`ToolContext::drafting`] is set, a call whose [`Effect`] is not
/// [`Effect::Read`] and whose name is not in [`Drafting::direct`] is turned
/// into a [`crate::proposal::Proposal`] instead of being run -- see
/// [`dispatch_drafting`] -- except for the two mail tools that write a
/// draft, which run for real and gain a second, linked proposal to send it.
/// See `agent::tools::mail`'s own module docs for why that pair is the one
/// documented exception rather than a rule this function has to guess at.
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

    let args = Args::new(tool.name, arguments);

    if let Some(drafting) = &ctx.drafting
        && tool.effect != Effect::Read
        && !drafting.direct.contains(&tool.name)
    {
        if mail::writes_a_draft(tool.name) {
            return mail::run_drafting_write(ctx, tool, &args, drafting);
        }
        return dispatch_drafting(ctx, tool, &args, drafting);
    }

    (tool.run)(ctx, &args)
}

/// The drafting half of [`dispatch`]: build the record this call would have
/// saved, and record it as a [`crate::proposal::Proposal`] instead of
/// saving it.
fn dispatch_drafting(
    ctx: &ToolContext<'_>,
    tool: &Tool,
    args: &Args<'_>,
    drafting: &Drafting,
) -> Result<Value> {
    let Some(build) = tool.build else {
        return Err(Error::Invalid(format!(
            "while dreaming you can only propose tasks, time on the calendar, memories, \
             routines and notes, or write a mail draft; {:?} is not something you can \
             propose; mention it in your note instead.",
            tool.name
        )));
    };
    let built = build(ctx, args)?;
    propose(ctx, drafting, args, built)
}

/// Turn a [`Built`] call into a saved, pending [`crate::proposal::Proposal`],
/// after the two checks every proposal passes regardless of which tool made
/// it: the policy, and the run's own budget.
///
/// Shared with `agent::tools::mail::run_drafting_write`, which reaches this
/// after writing a real `Draft` -- the one call that both does something and
/// proposes something in the same turn.
fn propose(
    ctx: &ToolContext<'_>,
    drafting: &Drafting,
    args: &Args<'_>,
    built: Built,
) -> Result<Value> {
    check_policy(ctx, built.payload.kind())?;
    check_cap(ctx, drafting)?;

    let why =
        args.opt_str(WHY_ARG).map(|s| truncate_chars(s.trim(), crate::proposal::MAX_WHY_CHARS));
    let now = jiff::Timestamp::now();
    let mut p = crate::proposal::Proposal::new(built.payload, built.caption, now, ctx.tz)
        .with_about(built.about);
    if let Some(why) = why {
        p = p.with_why(why);
    }
    if let Some(source) = drafting.source {
        p = p.made_by(source);
    }
    ctx.vault.save_proposal(&p)?;

    Ok(json!({
        "ok": true,
        "action": "proposed",
        "kind": p.kind.as_str(),
        "name": p.caption,
        "id": p.id.to_string(),
        "note": "Not done yet: the person will accept or decline it.",
    }))
}

/// Refuse up front when the vault's own ceiling on pending proposals has been
/// reached. [`crate::vault::Vault::save_proposal`] enforces the same
/// ceiling, but only at the end; a caller that writes something real
/// *before* it proposes -- `run_drafting_write` -- has to know first, or
/// every refused attempt leaves its real write behind.
fn check_pending_room(ctx: &ToolContext<'_>) -> Result<()> {
    let pending = ctx.vault.pending_proposals()?;
    if pending >= crate::proposal::MAX_PENDING_PROPOSALS {
        return Err(Error::Invalid(format!(
            "{pending} proposals are already waiting for an answer; make no more \
             until some are accepted or declined"
        )));
    }
    Ok(())
}

/// Refuse a kind of proposal the person has switched off, in words a model
/// can act on: not "forbidden", but which knob to stop reaching for.
fn check_policy(ctx: &ToolContext<'_>, kind: crate::proposal::ProposalKind) -> Result<()> {
    let settings = ctx.vault.agent_settings()?;
    if !settings.proposals.allows(kind) {
        return Err(Error::Invalid(format!(
            "the person has switched off proposals of {}",
            kind.as_str()
        )));
    }
    Ok(())
}

/// Refuse past a run's own budget on how many proposals it may make -- see
/// `crate::proposal::max_per_run`. `None` on either half of [`Drafting`]
/// means no cap beyond the vault's own on pending proposals, which
/// [`crate::vault::Vault::save_proposal`] already enforces.
fn check_cap(ctx: &ToolContext<'_>, drafting: &Drafting) -> Result<()> {
    let Some(max) = drafting.max_proposals else { return Ok(()) };
    let Some(crate::proposal::ProposalSource::Run { run_id }) = drafting.source else {
        return Ok(());
    };
    let made = ctx.vault.proposals(&crate::store::proposals::ProposalQuery {
        run_id: Some(run_id),
        ..Default::default()
    })?;
    if made.len() >= max {
        return Err(Error::Invalid(format!(
            "this run may make at most {max} proposals and has already made that many; \
             say what is left in your note instead"
        )));
    }
    Ok(())
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
        for tool in catalog() {
            if tool.domain.sensitivity() == Sensitivity::Secret {
                panic!(
                    "{} is in a secret domain and is in the catalogue; \
                     `available` filters it out, but nothing should be there to filter",
                    tool.name
                );
            }
        }
    }

    /// Every tool that deletes something can name what it is about to delete.
    ///
    /// The confirmation card is the whole of the protection here -- there is
    /// no undo in this application -- and a card with no subject on it asks
    /// somebody to approve the deletion of a record they cannot identify.
    /// `describe` is a field on the tool rather than a name-matched list
    /// beside the catalogue precisely so this cannot go quiet again: a
    /// destructive tool added without setting it fails this test rather
    /// than shipping a blank card.
    #[test]
    fn every_destructive_tool_can_name_what_it_would_delete() {
        for tool in catalog() {
            if tool.effect == Effect::Destructive {
                assert!(
                    tool.describe.is_some(),
                    "{} deletes something and has no describe fn, so its confirmation \
                     card would name nothing",
                    tool.name
                );
            }
        }
    }

    /// The sibling of the test above, for the fourth effect. `Effect::Outward`
    /// carries no undo stack either -- the confirmation card is the whole of
    /// the protection -- and a card that cannot say who a message is about to
    /// reach asks somebody to approve sending it blind.
    #[test]
    fn every_outward_tool_can_name_who_it_reaches() {
        for tool in catalog() {
            if tool.effect == Effect::Outward {
                assert!(
                    tool.describe.is_some(),
                    "{} reaches somebody outside the vault and has no describe fn, so its \
                     confirmation card would name nobody",
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
        for domain in Domain::ALL {
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
        for domain in Domain::ALL {
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
        assert!(a.opt_id::<crate::id::ProjectId>("project_id", "project").unwrap().is_none());
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
        let err =
            Args::new(name, &v).id::<crate::id::TaskId>("task_id", "task").unwrap_err().to_string();
        assert!(err.contains("task id"), "got {err}");
        assert!(err.contains("List or search"), "should say how to recover: {err}");
    }

    #[test]
    fn an_enum_is_case_insensitive_and_lists_its_alternatives() {
        let (name, v) = args(json!({ "status": "Done" }));
        let parsed: crate::task::TaskStatus =
            Args::new(name, &v).opt_enum("status", tasks::STATUSES).unwrap().unwrap();
        assert_eq!(parsed, crate::task::TaskStatus::Done);

        let (name, v) = args(json!({ "status": "finished" }));
        let err = Args::new(name, &v)
            .opt_enum::<crate::task::TaskStatus>("status", tasks::STATUSES)
            .unwrap_err()
            .to_string();
        assert!(err.contains("backlog, todo"), "should list what is allowed: {err}");
    }

    #[test]
    fn a_missing_required_argument_names_itself() {
        let (name, v) = args(json!({}));
        let err = Args::new(name, &v).str("title").unwrap_err().to_string();
        assert!(err.contains("`title` is required"), "got {err}");
    }

    // ---- drafting ---------------------------------------------------------

    /// While drafting, a model must be able to say *why* it is proposing a
    /// write -- but there is nothing to explain about a call that changes
    /// nothing, so a read tool's schema is left exactly as it is.
    #[test]
    fn a_write_tools_schema_gains_why_while_drafting_and_a_read_tools_does_not() {
        let create_task = find("create_task").expect("create_task is in the catalogue");
        let plain = create_task.parameters();
        assert!(
            plain["properties"].get(WHY_ARG).is_none(),
            "the ordinary schema should not carry it"
        );
        let drafting = create_task.parameters_for(true);
        assert!(drafting["properties"].get(WHY_ARG).is_some(), "{drafting}");
        assert!(!drafting["required"].as_array().unwrap().iter().any(|v| v == WHY_ARG), "optional");

        // `parameters_for(false)` is exactly `parameters()` -- no `why`
        // appears just because a caller asked the drafting question.
        assert_eq!(create_task.parameters_for(false), plain);

        let list_tasks = find("list_tasks").expect("list_tasks is in the catalogue");
        assert_eq!(list_tasks.effect, Effect::Read);
        assert_eq!(
            list_tasks.parameters_for(true),
            list_tasks.parameters(),
            "a read tool gains nothing while drafting"
        );
    }
}
