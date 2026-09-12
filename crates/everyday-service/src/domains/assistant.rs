//! Configuring the assistant, and reading its threads back.
//!
//! The smallest domain, and deliberately: almost everything the assistant can
//! do it does through its *tools*, which live in the core and are reached from
//! [`crate::agent`] rather than from here. What is left is configuration, the
//! threads, and the two halves of one exchange -- `send_message`, which answers
//! with a stream and so is served by [`crate::service::Service::send_message`],
//! and the confirmation that answers it.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use everyday_core::agent::{AgentSettings, Conversation, Memory, Message};
use everyday_core::store::agent::ConversationQuery;
use everyday_core::{ConversationId, MemoryId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::Nothing;

/// One row per thread for the history list, with how long each one is.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub messages: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettings {
    pub settings: AgentSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetKey {
    pub key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversations {
    #[serde(default)]
    pub limit: Option<u32>,
    /// Include the transcripts of routine runs.
    ///
    /// Off by default, which is what the rail's history list wants: a week of
    /// morning briefs is not a list of conversations somebody had. The
    /// Assistant app asks for a run's transcript by its run rather than by
    /// finding it in here.
    #[serde(default)]
    pub include_runs: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRef {
    pub id: ConversationId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirm {
    pub call_id: String,
    pub approved: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMemory {
    pub memory: Memory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRef {
    pub id: MemoryId,
}

/// What a turn needs. Not reachable through `call` -- see the module docs --
/// but declared here so the shape is in one place and appears in the catalogue.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessage {
    pub conversation_id: ConversationId,
    pub prompt: String,
    #[serde(default)]
    pub context: Option<String>,
}

/// How the assistant is configured. Never carries the API key; see
/// [`everyday_core::agent`] for why that is structural rather than a habit.
async fn agent_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<AgentSettings> {
    svc.on_vault(move |vault| vault.agent_settings()).await
}

async fn save_agent_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveSettings,
) -> CommandResult<AgentSettings> {
    let vault = svc.require()?;
    blocking(move || {
        vault.save_agent_settings(&args.settings)?;
        // Read back rather than echoing what was sent: `has_key` is derived
        // from the secret table, so the pane must be told what is true rather
        // than what it asked for.
        Ok(vault.agent_settings()?)
    })
    .await
}

/// Store the API key. There is no command that reads one back.
async fn set_agent_key(svc: Arc<Service>, _ctx: Ctx, args: SetKey) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.set_agent_key(&args.key)).await
}

async fn clear_agent_key(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.clear_agent_key()).await
}

async fn list_conversations(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Conversations,
) -> CommandResult<Vec<ConversationSummary>> {
    let vault = svc.require()?;
    blocking(move || {
        let query =
            ConversationQuery { limit: args.limit, offset: 0, chats_only: !args.include_runs };
        vault
            .conversations(&query)?
            .into_iter()
            .map(|c| {
                let messages = vault.message_count(c.id)?;
                Ok(ConversationSummary { conversation: c, messages })
            })
            .collect()
    })
    .await
}

/// Mint a thread, without saving it. The id is the core's to allocate, for the
/// reason given on `new_journal`.
async fn new_conversation(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Conversation> {
    let _ = svc.require()?;
    Ok(Conversation::new())
}

async fn conversation_messages(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConversationRef,
) -> CommandResult<Vec<Message>> {
    svc.on_vault(move |vault| vault.messages(args.id)).await
}

async fn delete_conversation(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConversationRef,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_conversation(args.id)).await
}

/// Answer a confirmation the assistant is waiting on.
///
/// Returns whether anything was still waiting: a turn that was cancelled
/// between the question and the click leaves a card on screen with nothing
/// behind it, and the panel dismisses it rather than showing an error.
async fn confirm_tool_call(svc: Arc<Service>, _ctx: Ctx, args: Confirm) -> CommandResult<bool> {
    Ok(svc.pending().answer(&args.call_id, args.approved))
}

async fn list_memories(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<Memory>> {
    svc.on_vault(move |vault| vault.memories()).await
}

/// Write a memory by hand, which also pins it: a fact somebody typed is not one
/// the assistant's own housekeeping may drop.
/// Mint a memory without saving it.
///
/// The id is the core's to allocate; see `new_routine` for the whole
/// argument. `pinned` is set, because the one caller is somebody typing a
/// fact by hand and a fact somebody typed is not one the assistant's own
/// housekeeping should evict to make room. They can clear it again.
async fn new_memory(_svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Memory> {
    Ok(Memory { pinned: true, ..Memory::new(String::new()) })
}

/// Write a memory.
///
/// This used to force `pinned: true`, on the argument that a fact somebody
/// typed is not one the assistant's own housekeeping may drop. The argument
/// still holds and has moved to where it belongs: the Memory pane sets the
/// flag when it adds one, and can clear it again. Forcing it here meant a
/// person could not unpin a fact they had pinned by accident, and meant the
/// pane's own switch did nothing.
async fn save_memory(svc: Arc<Service>, _ctx: Ctx, args: SaveMemory) -> CommandResult<Vec<Memory>> {
    svc.on_vault(move |vault| vault.save_memory(&args.memory)).await
}

async fn delete_memory(svc: Arc<Service>, _ctx: Ctx, args: MemoryRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_memory(args.id)).await
}

/// Placeholder for the one command that does not answer with a value.
///
/// In the catalogue so that a client generator, the introspection endpoint and
/// the surface snapshot all see the whole surface. Never reached: dispatch
/// refuses a streaming command before it gets here.
async fn send_message(_svc: Arc<Service>, _ctx: Ctx, _args: SendMessage) -> CommandResult<()> {
    Err(CommandError::new(codes::UNKNOWN_COMMAND, "send_message answers with a stream"))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "agent_settings", scope: Agent, effect: Read,
        args: Nothing, returns: "AgentSettings", signature: &[],
        run: agent_settings,
    },
    command! {
        name: "save_agent_settings", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SaveSettings, returns: "AgentSettings",
        signature: &[("settings", "AgentSettings", true)],
        run: save_agent_settings,
    },
    command! {
        name: "set_agent_key", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SetKey, returns: "void",
        signature: &[("key", "string", true)],
        run: set_agent_key,
    },
    command! {
        name: "clear_agent_key", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: Nothing, returns: "void", signature: &[],
        run: clear_agent_key,
    },
    command! {
        name: "list_conversations", scope: Agent, effect: Read,
        args: Conversations, returns: "ConversationSummary[]",
        signature: &[("limit", "number | null", false), ("includeRuns", "boolean", false)],
        run: list_conversations,
    },
    command! {
        name: "new_conversation", scope: Agent, effect: Read,
        args: Nothing, returns: "Conversation", signature: &[],
        run: new_conversation,
    },
    command! {
        name: "conversation_messages", scope: Agent, effect: Read,
        args: ConversationRef, returns: "AgentMessage[]",
        signature: &[("id", "ConversationId", true)],
        run: conversation_messages,
    },
    command! {
        name: "delete_conversation", scope: Agent, effect: Destructive,
        change: Conversation / Deleted,
        args: ConversationRef, returns: "void",
        signature: &[("id", "ConversationId", true)],
        run: delete_conversation,
    },
    command! {
        name: "send_message", scope: Agent, effect: Write,
        change: Conversation / Updated,
        streams: true,
        args: SendMessage, returns: "void",
        signature: &[
            ("conversationId", "ConversationId", true),
            ("prompt", "string", true),
            ("context", "string | null", false),
        ],
        run: send_message,
    },
    command! {
        name: "confirm_tool_call", scope: Agent, effect: Write,
        args: Confirm, returns: "boolean",
        signature: &[("callId", "string", true), ("approved", "boolean", true)],
        run: confirm_tool_call,
    },
    command! {
        name: "list_memories", scope: Agent, effect: Read,
        args: Nothing, returns: "Memory[]", signature: &[],
        run: list_memories,
    },
    command! {
        name: "new_memory", scope: Agent, effect: Read,
        args: Nothing, returns: "Memory", signature: &[],
        run: new_memory,
    },
    command! {
        name: "save_memory", scope: Agent, effect: Write,
        change: Memory / Updated,
        args: SaveMemory, returns: "Memory[]",
        signature: &[("memory", "Memory", true)],
        run: save_memory,
    },
    command! {
        name: "delete_memory", scope: Agent, effect: Destructive,
        change: Memory / Deleted,
        args: MemoryRef, returns: "void",
        signature: &[("id", "MemoryId", true)],
        run: delete_memory,
    },
];
