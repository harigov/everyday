//! The assistant's harness: the half that owns a socket.
//!
//! The other half is [`karmayoga_core::agent`], which decides what the
//! assistant *is* — its settings, its prompt, and the tools it may run —
//! with no provider and no network anywhere in it. This file is
//! the adapter: it wraps those tools in the shape [`rig`](rig_agent) wants,
//! opens the connection, and turns what comes back into events the interface
//! can draw and records the vault can keep.
//!
//! That is the same split [`crate::feeds`] and [`crate::websearch`] make, and
//! it is worth restating because it is what makes this feature testable at
//! all: "the assistant marked the wrong task done" is a unit test in the
//! core, offline, with no API key. Nothing in *this* file decides what a tool
//! does, so nothing in this file can get that wrong.
//!
//! # Why the tools are registered dynamically
//!
//! Rig's typed [`PortableTool`](rig_agent::core::tool::PortableTool) wants one
//! Rust type per tool, with its argument struct and its schema derived. The
//! catalogue is a `&'static [Tool]` whose schemas are built at runtime and
//! whose *membership depends on the vault* — a Markdown backend stores no
//! tasks, so its assistant must not be told `create_task` exists. So each
//! entry becomes a
//! [`PortableDynamicTool`](rig_agent::core::tool::PortableDynamicTool), which
//! takes its name, description and schema as ordinary values. The typed path
//! would have bought compile-time argument structs and cost the ability to
//! offer a different toolset per vault, which is the wrong trade.
//!
//! # Chat Completions, not the Responses API
//!
//! [`crate::llm::client`], which this opens its connection through, asks for
//! [`CompletionsClient`](rig_agent::core::providers::openai::CompletionsClient)
//! rather than rig's default -- the Responses API -- because the whole point
//! of the base-URL override is that Ollama, LM Studio, vLLM and OpenRouter
//! can be pointed at, and what they all implement is `/chat/completions`.
//! Choosing the newer API there would make the setting that exists for local
//! models work everywhere except local models.
//!
//! # Where the confirmation gate lives
//!
//! In [`ConfirmGate`], an [`AgentHook`] that runs before any tool body does.
//! Rig's `on_tool_call` can answer `Run` or `Skip(reason)`, and the skip's
//! reason is handed to the model — so a declined delete becomes something the
//! assistant is told about and can respond to, rather than an error it has to
//! interpret. The hook is `async`, which is what lets it wait for a person.
//!
//! # The tools that are not in the catalogue
//!
//! Nine tools are declared here rather than in the core, each for the reason
//! [`web_search_tool`] gives: seven of them open a socket, which the core
//! cannot -- the two that find places and the way to them are written in
//! [`crate::maps`] -- and `update_plan` touches nothing at all -- it is a
//! checklist the panel draws from the call's own arguments, so there is no
//! vault record for the core to own. The seven that reach the web are
//! offered only when the person has said so; `read_web_page` is further
//! gated on where its address came from -- see [`webpage::Provenance`] and
//! [`must_confirm_fetch`] -- and so is a cover a library tool names by
//! address. The ninth, `ask_owner`, waits on a person in another place --
//! see [`ask_owner_tool`] -- and is offered only to a scheduled run that has
//! one to ask, or to a project's own turn, which has the same one (see
//! [`Turn::job`]).
//!
//! The browser's tools (`browser_open`, `browser_click`, `browser_type` and
//! the rest) are declared in [`crate::browser`], on a switch of their own
//! under the web's, and judged by [`ConfirmGate::browser_call`] on what the
//! browser says each action would touch -- see that module's doc.
//!
//! A connected service's tools are reached through two of ours,
//! `find_connection_tools` and `use_connection_tool`, declared in
//! [`crate::connections::tools`]. The gate asks about a call to the second on
//! the person's policy for that one tool ([`ConfirmGate::connection_call`]),
//! and what comes back counts as a stranger's writing, as a mail read does.
//!
//! # Stopping a turn
//!
//! A turn can run for a minute and a dozen tools, and somebody watching it
//! go the wrong way must be able to say so. `cancel_turn` reaches
//! [`Pending::cancel`], which flips a flag [`stream`] is waiting on beside
//! the model; the stream is dropped where it stands, which also drops any
//! confirmation it was waiting on, and the turn ends as a *finished* one
//! with whatever it had said and done so far. Not a failure: nothing went
//! wrong, somebody changed their mind, and the thread should read that way.
//!
//! # Background jobs
//!
//! A background job (`docs/plans/jobs.md`) runs through [`run_turn`] like any
//! other turn, and is treated as an unattended one in every respect that
//! matters -- see [`Turn::job`]. Three things are particular to its own turn,
//! and one to the turn that comes after it:
//!
//! - **What it is offered.** The whole catalogue, bar the tools that start,
//!   inspect, change and stop jobs (see [`JobTools`]): a job that could start
//!   jobs is a way to find forty of them running that nobody asked for. A
//!   project's turn alone is also offered `report_progress`, to say how far
//!   it has got, and -- when it was given somebody to ask -- `ask_owner`.
//! - **How it reads a page.** A research job reads dozens, and the
//!   conversation is resent on every step, so `read_web_page` hands it what
//!   the quick model found on the page *for its question* rather than the page.
//!   Done in the hook that sees the result ([`ConfirmGate::research_view`]),
//!   not in the tool, so that what the page linked to is still recorded as
//!   the page said it.
//! - **How far it may go.** [`Turn::max_steps`] overrides the settings' own
//!   ceiling for this turn alone.
//! - **The turn that follows it** ([`Turn::follow_up`]) is begun by the
//!   application, not the person, and is kept in the thread as a
//!   [`Role::System`] line and shown to the model as a note from the
//!   application -- see [`application_note`].
//!
//! # Where things are
//!
//! The harness is one module tree; each file is one part of a turn.
//!
//! | File | What it holds |
//! |---|---|
//! | `agent.rs` | this page, the imports every part shares, [`AgentEvent`] and the live-turn counter |
//! | `confirm.rs` | the person's side of a confirmation -- [`Asker`], [`Typist`], [`Reach`], [`Pending`] -- and the gate's rules about what must be asked |
//! | `gate.rs` | `ConfirmGate`, the hook every tool call passes through: confirm, park, refuse, record |
//! | `trust.rs` | where words and addresses came from: provenance, saved words, the untrusted fence |
//! | `prompt.rs` | what a turn is built from: its meta, its tools, its system prompt and agent |
//! | `own_tools/` | the tools declared here rather than in the core, grouped by subject |
//! | `turn.rs` | `run_tool`, [`Turn`], [`JobTurn`], and the loop that runs one |
//! | `notes.rs` | the notes a turn is handed about what else is going on |
//! | `replay.rs` | the thread, turned back into the history a model is given |
//! | `stream.rs` | the streamed step loop, its retries and its last-word nudge |
//! | `summary.rs` | the one line a tool call's outcome is shown as |
//! | `kits.rs` | which kits a turn is offered, and `load_tools` |
//! | `tests.rs` | the tests, by the part they cover |

use crate::deskwrite::MessageKind;
use crate::events::Kind;
use crate::maps::{GET_DIRECTIONS, SEARCH_PLACES};
use crate::service::Service;
use crate::webpage::{self, Provenance, Trust};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use karmayoga_core::RoutineRunId;
use karmayoga_core::agent::onscreen::OnScreen;
use karmayoga_core::agent::tools::{self, Caller as ToolCaller, Drafting, Effect, ToolContext};
use karmayoga_core::agent::{
    AgentSettings, Conversation, MailLink, Message as VaultMessage, Role, ToolCall, sites,
};
use karmayoga_core::id::ThreadId;
use karmayoga_core::llm::{ModelAccess, ModelTier};
use karmayoga_core::mail::Origin as MailOrigin;
use karmayoga_core::model::{Attachment, system_tz};
use karmayoga_core::proposal::{ProposalKind, ProposalSource};
use karmayoga_core::quick::{ExtractAnswer, QuickContext, RESEARCH_EXTRACT};
use karmayoga_core::record::RecordKind;
use karmayoga_core::websearch::SearchResult;
use karmayoga_core::writing::{WritingAction, WritingOutcome};
use karmayoga_core::{ConversationId, FileId, KindId, MessageId, Vault};
use rig_agent::agent::hook::{
    InvalidToolCallAction, InvalidToolCallContext, ModelTurnAction, ModelTurnFinished,
    ToolCall as HookToolCall, ToolCallAction, ToolResultAction, ToolResultEvent,
};
use rig_agent::core::client::completion::CompletionClient;
use rig_agent::core::tool::{PortableDynamicTool, ToolExecutionError, ToolOutput};
use rig_agent::prelude::*;
use rig_agent::{Agent, AgentBuilder, AgentHook, HookContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::error::{CommandError, CommandResult, codes, mail_rate_limit_error};

// Which tools a turn is offered, and `load_tools`: see the module's own doc.
mod kits;

mod confirm;
mod gate;
mod notes;
mod own_tools;
mod prompt;
mod replay;
mod stream;
mod summary;
mod trust;
mod turn;

pub use confirm::*;
use notes::*;
use own_tools::*;
use prompt::*;
use replay::*;
use stream::*;
use summary::*;
pub(crate) use trust::*;
pub use turn::*;

/// How many turns -- the rail's or a routine's -- are running right now, in
/// this process.
///
/// A process-wide counter rather than a field on [`Service`], so a dream
/// deciding whether to start (`karmayoga_service::scheduler::execute`) can
/// ask the cheap question -- "is anybody typing" -- without the scheduler
/// having to reach back into a `Service` field it does not otherwise touch.
/// It counts every turn, not only interactive ones, but that is never a
/// problem in practice: the scheduler already runs routines one at a time
/// within a tick, so by the time a dream's own check runs, the only turn
/// that can still be live is the rail's.
///
/// Except a background job's own turn, which is deliberately not counted --
/// see [`run_turn`]. A research job runs for up to an hour, and it is
/// not somebody typing: counted, it would hold the nightly dream off for as
/// long as it took.
static LIVE_TURNS: AtomicU32 = AtomicU32::new(0);

/// Whether any turn is running right now. What the dream's idle guard reads;
/// see `karmayoga_service::scheduler::execute`'s own doc for why "no
/// conversation in flight" is the cheap definition of idle this uses. A
/// background job's turn does not count; the turn that *follows* a job, in
/// the conversation that asked, does.
pub fn turn_in_flight() -> bool {
    LIVE_TURNS.load(Ordering::SeqCst) > 0
}

/// Counts one turn in, and back out again on drop -- including an early
/// return from [`run_turn`], which is exactly the case a bare
/// increment/decrement pair would be easy to get wrong.
struct LiveTurnGuard;

impl LiveTurnGuard {
    fn start() -> Self {
        LIVE_TURNS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for LiveTurnGuard {
    fn drop(&mut self) {
        LIVE_TURNS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where a turn's events go.
///
/// A function rather than a channel type, because the three things that listen
/// are not the same shape: the desktop shell owns a Tauri channel, the server
/// writes a line of NDJSON per event, and a test pushes onto a vector. Any of
/// them is a closure.
///
/// It is not `async`. An event is a fragment of prose on its way to a panel and
/// the caller has somewhere to put it immediately -- a channel send, a
/// broadcast, a `Vec` -- so making this a future would oblige every listener to
/// have a runtime and buy nothing.
pub type Sink = std::sync::Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// One thing that happened during a turn, on its way to the panel.
///
/// Sent as it happens rather than returned at the end, because a
/// reply that takes twenty seconds and arrives all at once reads as a hang.
/// The variants are what the panel has to draw differently, and no more.
#[derive(Debug, Clone, Serialize, Deserialize)]
// See `karmayoga_core::search::Found` for the trap this second line avoids:
// `rename_all` renames the variants and leaves the fields alone. This one has
// been going out as `message_id` and `call_id` since the rail was written,
// against an interface that reads `messageId` and `callId` -- which happened
// to be harmless only because the fields it got wrong are ids the panel
// compares to each other rather than to anything stored.
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentEvent {
    /// The assistant's message id, sent first so every delta that follows has
    /// something to be appended to.
    Started { message_id: String },
    /// A fragment of prose.
    Delta { text: String },
    /// The model's last step failed in a way worth trying again -- a tool
    /// call whose arguments arrived cut off, a dropped connection, a busy
    /// provider -- and is being asked again from where that step began.
    /// `text` is the reply as it stood then, and replaces what the
    /// [`Delta`]s have drawn: whatever the failed step had begun to say will
    /// be said again. See [`stream`].
    ///
    /// [`Delta`]: AgentEvent::Delta
    Retrying { text: String },
    /// A fragment of the model's reasoning, from a model that shows it.
    ///
    /// Forwarded as it arrives so the panel can show that something is
    /// happening during the long silence before a reasoning model's first
    /// word. Never part of the reply: not added to the text a [`Delta`]
    /// builds, not written into the thread, and not replayed to the model
    /// on the next turn -- it is a view of the work, not the work. A model
    /// that sends its reasoning only as one finished block produces one of
    /// these with the whole of it; one that streams it and then restates it
    /// as a block produces only the stream, since rig's block supersedes
    /// the deltas it repeats. See [`stream`].
    ///
    /// [`Delta`]: AgentEvent::Delta
    Thinking { text: String },
    /// The model has started writing a call to `name` and has not finished
    /// its arguments yet.
    ///
    /// Sent once per call, the moment its name is known. Writing the
    /// arguments of `create_note` for a long note can take many seconds of
    /// streamed JSON, and without this the panel's status line has nothing
    /// to say for them but "Thinking". It draws no card: the card is the
    /// call's own [`ToolStarted`] or [`ConfirmationRequired`], which carries
    /// the same `call_id` when it arrives, and a call refused before it
    /// starts -- an unknown tool, a dream's second note -- leaves nothing
    /// behind to clear.
    ///
    /// [`ToolStarted`]: AgentEvent::ToolStarted
    /// [`ConfirmationRequired`]: AgentEvent::ConfirmationRequired
    ToolPreparing { call_id: String, name: String },
    /// The assistant has decided to run something. Drawn as a card.
    ToolStarted { call_id: String, name: String, arguments: serde_json::Value },
    /// That tool finished, or failed. `summary` is the human sentence the
    /// card shows; the model gets the full result separately.
    ToolFinished {
        call_id: String,
        name: String,
        ok: bool,
        summary: String,
        /// Set on a successful mail write whose own JSON result named the
        /// thread it touched, so the panel can draw a link into Mail beside
        /// `summary` rather than only the sentence. See
        /// [`karmayoga_core::agent::Message::mail_link`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mail_link: Option<MailLink>,
        /// The pictures a successful `generate_image` or `draw_illustration`
        /// made and kept in Files, which the panel draws with the reply and
        /// a chat sends as it arrives -- see [`crate::images::tools::files_of`].
        /// Empty for every other tool.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<FileId>,
    },
    /// A call is waiting on a person before it may run. The panel draws the
    /// confirm/decline buttons and answers with `confirm_tool_call`.
    ConfirmationRequired {
        call_id: String,
        name: String,
        /// What is about to happen, named rather than identified: "the
        /// deck" for a delete, "to alice@example.com — Re: dinner — ..."
        /// for a send, the query itself for a `web_search` asked about
        /// after mail was read this turn, the full address for a
        /// `read_web_page`.
        subject: String,
        arguments: serde_json::Value,
        /// Why this is being asked, so the panel can draw a different card
        /// for each: `"destructive"` (removes something, no undo),
        /// `"outward"` (reaches somebody who is not the vault's owner),
        /// `"search"` (mail was read this turn, and this would send its own
        /// query to a search provider), `"fetch"` (`read_web_page`,
        /// `browser_open`, a feed, a lookup or a cover with an address the
        /// model composed itself rather than one the person gave or a search
        /// or page returned, and not on a safe website -- or on one, when it
        /// carries what could be data once a stranger's words are in the
        /// turn or with nobody watching -- so the address, which is what
        /// would carry anything out, is shown before it is visited; `site`
        /// offers "always allow" for its website), `"type"` (the browser about to type into a page,
        /// always asked: the subject is the text, the field and the site) or
        /// `"click"` (a click in the browser once mail has been read this
        /// turn), `"routine"` (a routine naming an address nobody gave) or
        /// `"rewrite"` (a money rule, or a write over more than
        /// [`MONEY_SWEEP`] transactions, once a stranger's words were read
        /// this turn) or `"connection"` (a tool of a connected service whose
        /// policy is to ask: the subject is the tool and the connection,
        /// then the arguments in full) or `"keep"` (something that every
        /// later prompt, or a run nobody watches, would read as the person's
        /// own wishes -- a memory, the profile, a skill, a routine, a task
        /// for the assistant, an accepted proposal -- written after a
        /// stranger's words were read: the subject names it and then gives
        /// the text that would be saved, in full). See
        /// [`karmayoga_core::agent::tools::Effect::Outward`] and
        /// `agent::tools::mail`'s module docs for the first two, the
        /// `web_search` doc comment below for the third, and
        /// [`must_confirm_fetch`] for the fourth.
        ///
        /// An owned `String` rather than `&'static str`: this event round
        /// trips through `Deserialize` too (every event this rail emits is
        /// replayed from a line of NDJSON on the other side of a process
        /// boundary in `karmayoga-server`'s own client), and a borrowed
        /// `'static` field cannot be produced from bytes that live only as
        /// long as the line they arrived on.
        kind: String,
        /// The website "always allow" would add to
        /// [`AgentSettings::safe_sites`](karmayoga_core::agent::AgentSettings::safe_sites),
        /// as [`karmayoga_core::agent::sites::site_of`] names it -- set only
        /// on a `"fetch"` whose address is not on a safe site already. The
        /// card offers "Always allow {site}" beside yes and no when it is
        /// set, answered with [`ConfirmAnswer::TrustSite`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<String>,
        /// Whether the card may offer "later" beside confirm and decline --
        /// [`karmayoga_core::agent::tools::Tool::can_propose`], plus
        /// `send_draft`, which already has a builder of its own. `false` for
        /// `"search"` and `"fetch"`: neither tool is in the core catalogue
        /// at all, so neither has a proposal form to park into. See
        /// `docs/plans/dreaming.md`'s Phase 5 and [`ConfirmGate::park`].
        can_park: bool,
    },
    /// The turn is over. Sent exactly once, whatever else happened, so the
    /// panel always has something to stop its spinner on.
    ///
    /// Including a turn somebody stopped with `cancel_turn`: that is a turn
    /// that ended early, not one that went wrong, so it finishes rather
    /// than failing. If it had produced nothing at all by then -- no prose,
    /// no tool -- the empty reply `message_id` names has already been
    /// removed from the thread, exactly as it is after a failure.
    Finished { message_id: String },
    /// The turn ended badly. Also terminal.
    Failed { message: String },
}
#[cfg(test)]
mod tests;
