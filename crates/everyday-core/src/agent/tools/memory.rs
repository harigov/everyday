//! The assistant's own memory: what it has been asked to keep across
//! conversations.

use serde_json::{Value, json};

use super::{Args, Built, Tool, ToolContext, done, empty_schema, schema, text};
use crate::agent::{MAX_MEMORY_CHARS, Memory};
use crate::error::{Error, Result};
use crate::id::MemoryId;
use crate::proposal::{About, AboutKind, Payload, ProposalKind, ProposedRecord};

pub(super) static TOOLS: &[Tool] = &[
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
         remember it. Only remember what you were told; what you notice for \
         yourself belongs to the overnight dream.",
        run_remember,
        None,
        Some(build_remember)
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
        run_forget,
        Some(describe_forget),
        Some(build_forget)
    ),
];

fn describe_forget(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: MemoryId = args.opt_id("memory_id", "memory").ok()??;
    ctx.vault.memories().ok()?.into_iter().find(|m| m.id == id).map(|m| m.text)
}

/// Read and check `fact`, the half `run_remember` and `build_remember` share
/// before they each decide what kind of [`Memory`] it becomes.
fn parse_fact<'a>(args: &Args<'a>) -> Result<&'a str> {
    let fact = args.str("fact")?.trim();
    if fact.chars().count() > MAX_MEMORY_CHARS {
        return Err(args.bad(format!(
            "a memory must be under {MAX_MEMORY_CHARS} characters. \
             Keep it to one sentence, or write an entry instead."
        )));
    }
    Ok(fact)
}

fn run_remember(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if !ctx.vault.agent_settings()?.remember {
        return Err(Error::Invalid("remembering is switched off in this vault's settings".into()));
    }
    let fact = parse_fact(args)?;

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

/// While drafting, `remember` proposes an *inferred* memory rather than a
/// told one -- the one kind of draft the assistant may act on before it is
/// accepted (see `docs/plans/dreaming.md`), and the reason a dream's own
/// noticing is never stamped [`crate::agent::MemoryOrigin::Told`].
fn build_remember(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    if !ctx.vault.agent_settings()?.remember {
        return Err(Error::Invalid("remembering is switched off in this vault's settings".into()));
    }
    let fact = parse_fact(args)?;
    let memory = Memory::inferred(fact, ctx.today);
    Ok(Built {
        payload: Payload::Create { record: ProposedRecord::Memory(memory) },
        caption: format!("Remember: {fact}"),
        about: None,
    })
}

fn run_list_memories(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    Ok(json!(
        ctx.vault
            .memories()?
            .iter()
            .map(|m| {
                let mut row = json!({
                    "id": m.id.to_string(),
                    "fact": m.text,
                    "pinned": m.pinned,
                    // What a person or a later dream needs before trusting a
                    // line it did not write itself: who stands behind it,
                    // and -- for one a dream inferred -- the last day the
                    // data still supported it.
                    "origin": m.origin,
                });
                if let Some(d) = m.last_supported {
                    row.as_object_mut()
                        .expect("built as an object")
                        .insert("last_supported".into(), json!(d.to_string()));
                }
                row
            })
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

fn build_forget(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let id: MemoryId = args.id("memory_id", "memory")?;
    let existing = ctx.vault.memories()?;
    let memory = existing
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| Error::Invalid(format!("no memory with id {id}")))?;
    Ok(Built {
        payload: Payload::Delete { kind: ProposalKind::Memory, id: id.to_string() },
        caption: format!("Forget: {}", memory.text),
        about: Some(About { kind: AboutKind::Memory, id: id.to_string() }),
    })
}
