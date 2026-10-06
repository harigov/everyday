//! Configuring the assistant, and reading its threads back.
//!
//! The smallest domain, and deliberately: almost everything the assistant can
//! do it does through its *tools*, which live in the core and are reached from
//! [`crate::agent`] rather than from here. What is left is configuration, the
//! threads, and the two halves of one exchange -- `send_message`, which answers
//! with a stream and so is served by [`crate::service::Service::send_message`],
//! and the two things a person can say back while it runs: the confirmation
//! that answers a card, and `cancel_turn`, which stops it.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Kind, Op};
use crate::service::{Service, blocking};
use everyday_core::agent::{AgentSettings, Conversation, Memory, Message, Skill};
use everyday_core::store::agent::ConversationQuery;
use everyday_core::{ConversationId, MemoryId, SkillId};
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
    /// Park the call as a proposal instead of running or refusing it. Added
    /// after `approved`, and defaulted, so an older client that has never
    /// heard of "later" keeps sending exactly what it always has and
    /// `approved` keeps its old meaning. See `docs/plans/dreaming.md`'s
    /// Phase 5 and `everyday_service::agent::ConfirmAnswer`.
    #[serde(default)]
    pub later: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelTurn {
    pub conversation_id: ConversationId,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryOriginArgs {
    pub id: MemoryId,
    pub origin: everyday_core::MemoryOrigin,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSkill {
    pub skill: Skill,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRef {
    pub id: SkillId,
}

/// What a turn needs. Not reachable through `call` -- see the module docs --
/// but declared here so the shape is in one place and appears in the catalogue.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessage {
    pub conversation_id: ConversationId,
    pub prompt: String,
    /// What the person has on screen, by reference -- see
    /// [`everyday_core::agent::onscreen`] for why it is ids and not a
    /// sentence. Turned into prose by the turn itself, where the vault and
    /// the mail gate are.
    ///
    /// Held as JSON until then and read with
    /// [`everyday_core::agent::onscreen::OnScreen::lenient`], so a
    /// reference this build cannot read -- an app or a kind added by a newer
    /// interface -- costs the context rather than refusing the message.
    #[serde(default)]
    pub on_screen: Option<serde_json::Value>,
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
    ctx: Ctx,
    args: SaveSettings,
) -> CommandResult<AgentSettings> {
    let vault = svc.require()?;
    let settings = blocking(move || {
        let previous = vault.agent_settings()?;
        // Compared before the write below overwrites `previous`, on
        // `acknowledgement_name`'s own rule -- the endpoint actually
        // reached, not the provider label -- so switching between two
        // gateways that both happen to be `Provider::OpenAi` still counts
        // as a change.
        let provider_changed = previous.provider_config.acknowledgement_name()
            != args.settings.provider_config.acknowledgement_name();
        let dreaming_before = previous.dreaming;
        vault.save_agent_settings(&args.settings)?;

        // A provider change must *visibly* clear mail access, not merely
        // disable it. `Account::assistant_acknowledged_for` already refuses
        // every mail tool the moment `acknowledgement_name` stops matching
        // -- that part was never silent -- but the account's own
        // `assistant_provider_acknowledged` field, and so the ticked
        // checkbox beside it in Settings → Accounts, would otherwise go on
        // naming a provider that is no longer the one configured. Cleared
        // here, in the same write, on every account that had it set, rather
        // than left for whoever next opens that account to notice it is
        // stale.
        if provider_changed {
            for mut account in vault.accounts()? {
                if account.assistant_provider_acknowledged.take().is_some() {
                    vault.save_account(&account)?;
                }
            }
        }

        // The dream routines. `set_dreaming` is idempotent, so it is only
        // worth calling when something might actually change: the switch
        // moved, or dreaming is on but the three are somehow missing --
        // which happens the very first time anybody turns it on, since
        // nothing else in this application ever creates them.
        if vault.supports_routines() {
            let missing_while_on =
                args.settings.dreaming && !vault.routines()?.iter().any(|r| r.kind.is_dream());
            if dreaming_before != args.settings.dreaming || missing_while_on {
                vault.set_dreaming(args.settings.dreaming)?;
            }
        }

        // Read back rather than echoing what was sent: `has_key` is derived
        // from the secret table, so the pane must be told what is true rather
        // than what it asked for.
        Ok(vault.agent_settings()?)
    })
    .await?;

    // A batch `Change` rather than one per account, matching `events.rs`'s
    // own "the board reorder" rule: several accounts losing their
    // acknowledgement in one save is one thing that happened, not several.
    // The closure above no longer collects which accounts and routines it
    // touched for itself -- the collector already saw the same writes, in
    // the same order, so `emit_touched` raises the identical batch from
    // that: one `Change` per kind, `ids` rather than `id` the moment there
    // is more than one, nothing at all when there is nothing to report.
    let origin = ctx.caller.origin().map(str::to_string);
    let touched = crate::touched::current();
    crate::events::emit_touched(
        svc.events().as_ref(),
        origin.clone(),
        &touched,
        Kind::Account,
        Op::Updated,
    );
    crate::events::emit_touched(
        svc.events().as_ref(),
        origin,
        &touched,
        Kind::Routine,
        Op::Updated,
    );
    Ok(settings)
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
    let answer = if args.later {
        crate::agent::ConfirmAnswer::Later
    } else if args.approved {
        crate::agent::ConfirmAnswer::Confirm
    } else {
        crate::agent::ConfirmAnswer::Decline
    };
    Ok(svc.pending().answer(&args.call_id, answer))
}

/// Stop whatever the assistant is doing on a conversation.
///
/// Returns whether a turn was running there. The turn itself does the rest:
/// it stops reading from the model, keeps what it had said and done by
/// then, and ends with `finished` rather than `failed` -- see
/// `everyday_service::agent`'s "Stopping a turn". No change is raised here,
/// because nothing has changed yet: the turn's own `send_message` raises
/// the thread's change when it winds up, exactly as it would have anyway.
async fn cancel_turn(svc: Arc<Service>, _ctx: Ctx, args: CancelTurn) -> CommandResult<bool> {
    Ok(svc.pending().cancel(args.conversation_id))
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
///
/// Who stands behind a memory is not this command's to change. That is
/// `set_memory_origin`'s job, and it refuses what this must refuse too: a
/// struck-out memory quietly becoming a standing instruction again, or a
/// person's own fact being relabelled as something a dream noticed. So the
/// origin, and the date an inference was last supported, are taken from
/// what is stored -- whatever the request says -- with one exception and one
/// default, both about *who this request is* rather than about storage:
///
/// - If what is stored was `Inferred` and the text arriving now differs, the
///   save becomes `Confirmed`. Rewriting what a dream noticed is standing
///   behind it, which is a stronger claim than the dream itself made.
/// - If nothing is stored under this id yet, it is `Told`, whatever it
///   arrived as. Only a dream infers, and only a person's answer to a dream
///   confirms or rejects; a fact typed into the pane is exactly what `Told`
///   means. Coerced rather than refused, so the pane never has to retype it.
async fn save_memory(svc: Arc<Service>, _ctx: Ctx, args: SaveMemory) -> CommandResult<Vec<Memory>> {
    svc.on_vault(move |vault| {
        let mut memory = args.memory;
        let stored = vault.memories()?.into_iter().find(|m| m.id == memory.id);
        let (origin, last_supported) = provenance_for_save(stored.as_ref(), &memory);
        memory.origin = origin;
        memory.last_supported = last_supported;
        vault.save_memory(&memory)
    })
    .await
}

/// The origin and last-supported date a hand-made save may carry. See
/// `save_memory`.
fn provenance_for_save(
    stored: Option<&Memory>,
    incoming: &Memory,
) -> (everyday_core::MemoryOrigin, Option<jiff::civil::Date>) {
    use everyday_core::MemoryOrigin;
    match stored {
        None => (MemoryOrigin::Told, None),
        Some(old) if old.origin == MemoryOrigin::Inferred && old.text != incoming.text => {
            (MemoryOrigin::Confirmed, old.last_supported)
        }
        Some(old) => (old.origin, old.last_supported),
    }
}

/// Confirm an inferred memory, or strike it out. See
/// `everyday_core::agent::MemoryOrigin`.
async fn set_memory_origin(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MemoryOriginArgs,
) -> CommandResult<Memory> {
    svc.on_vault(move |vault| vault.set_memory_origin(args.id, args.origin)).await
}

async fn delete_memory(svc: Arc<Service>, _ctx: Ctx, args: MemoryRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_memory(args.id)).await
}

async fn list_skills(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<Skill>> {
    svc.on_vault(move |vault| vault.skills()).await
}

/// A blank skill with an id, switched on. The core allocates it, the same
/// reason `new_memory` and `new_routine` do: an id a client minted itself
/// would be a second place one could be generated from, and the only one
/// not guaranteed to be a UUIDv7.
async fn new_skill(_svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Skill> {
    Ok(Skill::new(String::new()))
}

/// Save a skill, stamping `updated_at` here rather than trusting whatever
/// the editor last had in the field -- the same reason `save_routine`
/// stamps its own clock rather than the client's.
async fn save_skill(svc: Arc<Service>, _ctx: Ctx, args: SaveSkill) -> CommandResult<Skill> {
    let vault = svc.require()?;
    let now = svc.now();
    blocking(move || {
        let mut skill = args.skill;
        skill.updated_at = now;
        vault.save_skill(&skill)?;
        Ok(skill)
    })
    .await
}

async fn delete_skill(svc: Arc<Service>, _ctx: Ctx, args: SkillRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_skill(args.id)).await
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
        id: |a: &ConversationRef| Some(a.id.to_string()),
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
            ("onScreen", "OnScreen | null", false),
        ],
        run: send_message,
    },
    command! {
        name: "confirm_tool_call", scope: Agent, effect: Write,
        args: Confirm, returns: "boolean",
        signature: &[
            ("callId", "string", true),
            ("approved", "boolean", true),
            ("later", "boolean", false),
        ],
        run: confirm_tool_call,
    },
    command! {
        name: "cancel_turn", scope: Agent, effect: Write,
        args: CancelTurn, returns: "boolean",
        signature: &[("conversationId", "ConversationId", true)],
        run: cancel_turn,
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
        id: |a: &SaveMemory| Some(a.memory.id.to_string()),
        args: SaveMemory, returns: "Memory[]",
        signature: &[("memory", "Memory", true)],
        run: save_memory,
    },
    command! {
        name: "set_memory_origin", scope: Agent, effect: Write,
        change: Memory / Updated,
        id: |a: &MemoryOriginArgs| Some(a.id.to_string()),
        args: MemoryOriginArgs, returns: "Memory",
        signature: &[("id", "MemoryId", true), ("origin", "MemoryOrigin", true)],
        run: set_memory_origin,
    },
    command! {
        name: "delete_memory", scope: Agent, effect: Destructive,
        change: Memory / Deleted,
        id: |a: &MemoryRef| Some(a.id.to_string()),
        args: MemoryRef, returns: "void",
        signature: &[("id", "MemoryId", true)],
        run: delete_memory,
    },
    command! {
        name: "list_skills", scope: Agent, effect: Read,
        args: Nothing, returns: "Skill[]", signature: &[],
        run: list_skills,
    },
    command! {
        name: "new_skill", scope: Agent, effect: Read,
        args: Nothing, returns: "Skill", signature: &[],
        run: new_skill,
    },
    command! {
        name: "save_skill", scope: Agent, effect: Write,
        change: Skill / Updated,
        id: |a: &SaveSkill| Some(a.skill.id.to_string()),
        args: SaveSkill, returns: "Skill",
        signature: &[("skill", "Skill", true)],
        run: save_skill,
    },
    command! {
        name: "delete_skill", scope: Agent, effect: Destructive,
        change: Skill / Deleted,
        id: |a: &SkillRef| Some(a.id.to_string()),
        args: SkillRef, returns: "void",
        signature: &[("id", "SkillId", true)],
        run: delete_skill,
    },
];

#[cfg(test)]
mod tests {
    use super::provenance_for_save;
    use everyday_core::{Memory, MemoryOrigin};

    #[test]
    fn a_hand_save_never_changes_who_stands_behind_a_memory() {
        let day = jiff::civil::date(2026, 9, 15);
        let incoming = |origin| Memory { origin, ..Memory::new("Runs on Tuesdays") };

        // New: always told, whatever it claims.
        for claimed in [
            MemoryOrigin::Told,
            MemoryOrigin::Inferred,
            MemoryOrigin::Confirmed,
            MemoryOrigin::Rejected,
        ] {
            assert_eq!(provenance_for_save(None, &incoming(claimed)), (MemoryOrigin::Told, None));
        }

        // Stored and rejected: stays rejected, even when sent as told.
        let rejected = Memory { origin: MemoryOrigin::Rejected, ..Memory::new("Runs on Tuesdays") };
        assert_eq!(
            provenance_for_save(Some(&rejected), &incoming(MemoryOrigin::Told)),
            (MemoryOrigin::Rejected, None)
        );

        // Stored and told: cannot be relabelled as inferred.
        let told = Memory::new("Runs on Tuesdays");
        assert_eq!(
            provenance_for_save(Some(&told), &incoming(MemoryOrigin::Inferred)),
            (MemoryOrigin::Told, None)
        );

        // Stored and inferred: unchanged text keeps it inferred, with its date.
        let inferred = Memory::inferred("Runs on Tuesdays", day);
        assert_eq!(
            provenance_for_save(Some(&inferred), &incoming(MemoryOrigin::Told)),
            (MemoryOrigin::Inferred, Some(day))
        );
        // ...and rewritten text confirms it.
        let rewritten =
            Memory { text: "Runs on Tuesday mornings".into(), ..incoming(MemoryOrigin::Told) };
        assert_eq!(
            provenance_for_save(Some(&inferred), &rewritten),
            (MemoryOrigin::Confirmed, Some(day))
        );
    }
}
