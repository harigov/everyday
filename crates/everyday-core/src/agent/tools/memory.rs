//! The assistant's own memory: what it has been asked to keep across
//! conversations, and the handful of fixed facts it has been handed about
//! the person it answers to.
//!
//! Most of this file is [`Memory`]: a fact the assistant was told, or one a
//! dream inferred and the person then agreed with or struck out -- see
//! [`MemoryOrigin`]. `update_profile`, at the end, is the odd one out: it
//! touches no memory at all, but "About You" -- name, birthday, roughly
//! where they live -- which other parts of the application (`get_weather`,
//! a calendar's age-appropriate suggestions) read directly rather than
//! through a memory. See `crate::profile`'s module docs for why that split
//! exists.

use serde_json::{Value, json};

use super::{Args, Built, Tool, ToolContext, day, done, empty_schema, schema, text};
use crate::agent::{MAX_MEMORY_CHARS, Memory, MemoryOrigin};
use crate::error::{Error, Result};
use crate::id::MemoryId;
use crate::profile::Profile;
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
        "confirm_memory",
        Write,
        Agent,
        schema(vec![("memory_id", text("From list_memories."))], &["memory_id"]),
        "Turn an inferred memory into a standing fact, the way `remember` would have \
         if the person had said it outright \u{2014} use this when they agree in chat \
         that something you noticed on your own is actually right. Only works on a \
         memory whose origin is inferred; one that was already told, confirmed or \
         rejected is refused, since there is nothing left to agree to. This is not \
         how an overnight dream itself says a memory still holds: that happens \
         through its own \"Confirmed memories:\" note and never touches origin, \
         because noticing the same thing again is not the same act as the person \
         agreeing it is true.",
        run_confirm_memory
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
    // `Domain::Agent`, not a domain of its own, even though the profile is
    // not a memory: `Store::agent` (what `supports_agent` checks) and
    // `Store::put_profile` are overridden together, in the same backend's
    // impl block, and both default to "unsupported" everywhere else -- so
    // gating this tool on `Domain::Agent` offers it exactly where the vault
    // can actually keep what it is given, and nowhere it cannot.
    tool!(
        "update_profile",
        Write,
        Agent,
        schema(
            vec![
                ("first_name", text("Given name.")),
                ("last_name", text("Family name.")),
                ("born", day("Date of birth. Cannot be in the future.")),
                ("gender", text("One word, in their own words \u{2014} there is no fixed list.")),
                (
                    "location",
                    text(
                        "Roughly where they live now, e.g. a city. This is what get_weather \
                         reads."
                    )
                ),
                (
                    "about",
                    text(
                        "A paragraph in their own words about their work, family and what \
                         they care about. Replaces whatever is there now."
                    )
                ),
            ],
            &[]
        ),
        "Change one of the handful of facts about the person that do not shift from \
         day to day \u{2014} their name, birthday, roughly where they live, or the \
         paragraph in their own words about their life \u{2014} as opposed to \
         `remember`, which is for a preference, a habit or anything else that could \
         still change again. \"I've moved to Denver\" is this: location is what \
         get_weather reads. Give only the fields that changed; anything left out \
         keeps its current value. The usual limits still apply \u{2014} a birthday \
         cannot be in the future, and the about section is a paragraph or two, not \
         a document.",
        run_update_profile
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

fn run_confirm_memory(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if ctx.unattended {
        return Err(Error::Invalid(
            "confirming a memory is the person's own act, not something an unattended \
             run can do on their behalf \u{2014} an overnight dream says a memory still \
             holds through its own \"Confirmed memories:\" note instead"
                .into(),
        ));
    }
    let id: MemoryId = args.id("memory_id", "memory")?;
    let existing = ctx.vault.memories()?;
    let memory = existing
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| Error::Invalid(format!("no memory with id {id}")))?;
    check_confirmable(memory)?;
    let confirmed = ctx.vault.set_memory_origin(id, MemoryOrigin::Confirmed)?;
    done("confirmed", "memory", &confirmed.text, confirmed.id.to_string())
}

/// Refuse to confirm anything but an inferred memory: once a memory was
/// already told outright, already confirmed, or already struck out, there
/// is nothing left to agree to. Pulled out of [`run_confirm_memory`] so the
/// rule can be tested without a vault behind it.
fn check_confirmable(memory: &Memory) -> Result<()> {
    let state = match memory.origin {
        MemoryOrigin::Inferred => return Ok(()),
        MemoryOrigin::Told => "already told outright",
        MemoryOrigin::Confirmed => "already confirmed",
        MemoryOrigin::Rejected => "already rejected",
    };
    Err(Error::Invalid(format!("only an inferred memory can be confirmed; this one is {state}")))
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

fn run_update_profile(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if ctx.unattended {
        return Err(Error::Invalid(
            "updating the profile is the person's own business; nothing unattended \
             may change who the vault belongs to"
                .into(),
        ));
    }
    let mut profile = ctx.vault.profile()?;
    apply_profile_args(args, &mut profile)?;
    ctx.vault.save_profile(&profile)?;
    let saved = ctx.vault.profile()?;
    let name = saved.name();
    Ok(json!({
        "ok": true,
        "action": "updated",
        "kind": "profile",
        "name": if name.is_empty() { "the profile".to_string() } else { name },
    }))
}

/// Apply whatever fields `update_profile` was given to `profile`, leaving
/// everything else exactly as it was. Pulled out of `run_update_profile` so
/// this can be tested without a vault behind it --
/// [`Vault::save_profile`](crate::vault::Vault::save_profile) is what makes
/// the result stick and checks [`Profile::validate`], neither of which this
/// touches.
fn apply_profile_args(args: &Args<'_>, profile: &mut Profile) -> Result<()> {
    if let Some(v) = args.opt_str("first_name") {
        profile.first_name = v.to_string();
    }
    if let Some(v) = args.opt_str("last_name") {
        profile.last_name = v.to_string();
    }
    if let Some(v) = args.opt_str("gender") {
        profile.gender = v.to_string();
    }
    if let Some(v) = args.opt_str("location") {
        profile.location = v.to_string();
    }
    if let Some(v) = args.opt_str("about") {
        profile.about = v.to_string();
    }
    if let Some(d) = args.opt_date("born")? {
        profile.born = Some(d);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    // ---- confirm_memory ---------------------------------------------------

    #[test]
    fn only_an_inferred_memory_can_be_confirmed() {
        let mut m = Memory::inferred("Plans the week on Sunday evening", date(2026, 9, 1));
        assert!(check_confirmable(&m).is_ok(), "an inferred memory is exactly what this is for");

        m.origin = MemoryOrigin::Told;
        let err = check_confirmable(&m).unwrap_err().to_string();
        assert!(err.contains("already told outright"), "got {err}");

        m.origin = MemoryOrigin::Confirmed;
        let err = check_confirmable(&m).unwrap_err().to_string();
        assert!(err.contains("already confirmed"), "got {err}");

        m.origin = MemoryOrigin::Rejected;
        let err = check_confirmable(&m).unwrap_err().to_string();
        assert!(err.contains("already rejected"), "got {err}");
    }

    #[test]
    fn confirming_a_memory_is_refused_while_unattended() {
        let dir = tempfile::tempdir().unwrap();
        let v = crate::vault::Vault::create(
            dir.path(),
            crate::vault::VaultConfig {
                name: "Test".into(),
                backend: "memory".into(),
                password: None,
                ..Default::default()
            },
            crate::testing::registry(),
        )
        .unwrap();
        let ctx = ToolContext::new(&v, date(2026, 9, 16), "UTC").with_unattended(true);
        let value = json!({ "memory_id": crate::id::MemoryId::new().to_string() });
        let err = run_confirm_memory(&ctx, &Args::new("confirm_memory", &value))
            .expect_err("nobody is there to agree on the person's behalf");
        assert!(err.to_string().contains("person's own act"), "got {err}");
    }

    // ---- update_profile -----------------------------------------------------

    #[test]
    fn only_the_fields_given_to_update_profile_change() {
        let mut p = Profile {
            first_name: "Hari".into(),
            last_name: "Govardhanam".into(),
            location: "Seattle".into(),
            ..Profile::default()
        };
        let value = json!({ "location": "Denver" });
        apply_profile_args(&Args::new("update_profile", &value), &mut p).unwrap();
        assert_eq!(p.location, "Denver");
        assert_eq!(p.first_name, "Hari", "a field left out keeps its value");
        assert_eq!(p.last_name, "Govardhanam");
    }

    #[test]
    fn a_birthday_is_parsed_and_a_bad_one_is_refused_with_advice() {
        let mut p = Profile::default();
        let value = json!({ "born": "1985-03-14" });
        apply_profile_args(&Args::new("update_profile", &value), &mut p).unwrap();
        assert_eq!(p.born, Some(date(1985, 3, 14)));

        let value = json!({ "born": "a while back" });
        let err = apply_profile_args(&Args::new("update_profile", &value), &mut p)
            .unwrap_err()
            .to_string();
        assert!(err.contains("calendar date"), "should show the expected format: {err}");
    }

    #[test]
    fn updating_the_profile_is_refused_while_unattended() {
        let dir = tempfile::tempdir().unwrap();
        let v = crate::vault::Vault::create(
            dir.path(),
            crate::vault::VaultConfig {
                name: "Test".into(),
                backend: "memory".into(),
                password: None,
                ..Default::default()
            },
            crate::testing::registry(),
        )
        .unwrap();
        let ctx = ToolContext::new(&v, date(2026, 9, 16), "UTC").with_unattended(true);
        let value = json!({ "location": "Denver" });
        let err = run_update_profile(&ctx, &Args::new("update_profile", &value))
            .expect_err("nobody is there to speak for the vault's owner");
        assert!(err.to_string().contains("person's own business"), "got {err}");
    }
}
