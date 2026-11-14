//! Skills: a process the assistant was given for a certain kind of request,
//! and the one record in [`Domain::Agent`](super::Domain::Agent) the
//! assistant may now write to as well as read. See [`crate::agent`]'s module
//! docs, "Skills, and progressive disclosure".
//!
//! `read_skill` is the only tool that *loads* one -- the index in the prompt
//! already carries every enabled skill's name and
//! [`Skill::description`](crate::agent::Skill::description), and the
//! instructions themselves are kept out of the prompt until a request
//! actually matches one. Writing is a separate decision, made here:
//! `list_skills` is how the assistant finds a skill's id before changing or
//! removing it (or decides there is no close match before writing a new
//! one), and `create_skill`, `update_skill` and `delete_skill` are how it
//! acts on what it finds.
//!
//! Every write refuses an unattended run (see [`ToolContext::unattended`]):
//! a routine rewriting the very processes it follows, with nobody watching,
//! is the same recursion `create_routine` already refuses to make more
//! routines, and it is refused for the same reason. A dream may still
//! *propose* one -- each write below has a `build`, so `dispatch_drafting`
//! reaches it before the unattended check in its `run` fn is ever asked --
//! and the person decides from the proposal, the same way they decide
//! everything else a dream leaves behind.
//!
//! `update_skill` carries one further rule that is not about dreaming at
//! all: a skill the person switched off is theirs to turn back on, not the
//! assistant's, so an update refuses to touch a disabled skill outright
//! rather than editing it quietly or switching it back on as a side effect.

use serde_json::{Value, json};

use super::{Args, Built, Tool, ToolContext, done, empty_schema, schema, text};
use crate::agent::Skill;
use crate::error::{Error, Result};
use crate::id::SkillId;
use crate::proposal::{Payload, ProposalKind, ProposedRecord};
use crate::timestamped::Timestamped;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "read_skill",
        Read,
        Agent,
        schema(
            vec![(
                "name",
                text("A skill's name, exactly as it was given to you in your instructions.")
            )],
            &["name"]
        ),
        "Load the full instructions for one of the skills named in your instructions \u{2014} a \
         process you were given for a certain kind of request, such as planning a trip or \
         running a weekly review. Call this before acting on a request that matches one; the \
         description in your instructions is only enough to recognise that it applies, not what \
         to do.",
        run_read_skill
    ),
    tool!(
        "list_skills",
        Read,
        Agent,
        empty_schema(),
        "Every skill that exists in this vault, switched off ones included, with id, name, \
         description and when each last changed \u{2014} but not its instructions; call \
         read_skill for those. Use this before writing a new skill, to check there is no close \
         match already, and to find the id an update or a delete needs.",
        run_list_skills
    ),
    tool!(
        "create_skill",
        Write,
        Agent,
        schema(
            vec![
                ("name", text("What to call it. Short: it is matched by this name.")),
                (
                    "description",
                    text(
                        "One or two sentences on when to reach for this \u{2014} the trigger, \
                         not the process. This is what every other conversation is told; say \
                         what kind of request it applies to."
                    )
                ),
                (
                    "instructions",
                    text(
                        "The process itself, in as much detail as it takes. Markdown. Never \
                         sent with the prompt \u{2014} only read_skill sees it, and only once a \
                         request actually matches."
                    )
                ),
            ],
            &["name", "description", "instructions"]
        ),
        "Write a new skill: a process to follow whenever a certain kind of request comes up, so \
         it does not have to be worked out fresh each time. Use this for something you expect \
         to do again in roughly the same way, not for a one-off plan. Saved switched on. Refused \
         if a skill with this name \u{2014} ignoring case and surrounding spaces \u{2014} \
         already exists; call list_skills first and use update_skill instead.",
        run_create_skill,
        None,
        Some(build_create_skill)
    ),
    tool!(
        "update_skill",
        Write,
        Agent,
        schema(
            vec![
                ("skill_id", text("Id from list_skills.")),
                ("name", text("Replaces the name. Omit to leave it alone.")),
                ("description", text("Replaces the description. Omit to leave it alone.")),
                ("instructions", text("Replaces the instructions. Omit to leave them alone.")),
            ],
            &["skill_id"]
        ),
        "Change an existing skill. Every field is optional and omitted fields are left alone. \
         This never switches a skill on or off \u{2014} that is the person's own call, from \
         Settings \u{2014} and it refuses outright on a skill that is currently switched off, \
         since editing one they turned off is not yours to do quietly.",
        run_update_skill,
        None,
        Some(build_update_skill)
    ),
    tool!(
        "delete_skill",
        Destructive,
        Agent,
        schema(vec![("skill_id", text("Id of the skill to delete."))], &["skill_id"]),
        "Permanently delete a skill. There is no undo. Only do this when explicitly asked to \
         remove that specific process.",
        run_delete_skill,
        Some(describe_delete_skill),
        Some(build_delete_skill)
    ),
];

fn run_read_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let name = args.str("name")?;
    let skills = ctx.vault.skills()?;
    let skill = find_skill(&skills, name)?;
    Ok(json!({
        "name": skill.name,
        "instructions": skill.instructions,
    }))
}

/// Find an enabled skill by name, case- and whitespace-insensitive since a
/// model copying a name out of its own instructions routinely pads or
/// re-cases it. Pulled out of [`run_read_skill`] so the matching and the
/// error message a model recovers from can be tested without a vault behind
/// them.
///
/// A disabled skill is never matched and never named in the "available"
/// list -- the same reasoning [`available_for`](super::available_for) gives
/// for leaving a whole tool out rather than offering and refusing it: a
/// skill nobody is told about cannot be asked for by name either.
fn find_skill<'a>(skills: &'a [Skill], name: &str) -> Result<&'a Skill> {
    let name = name.trim();
    // Lower-cased rather than `eq_ignore_ascii_case`, so a skill called
    // "Équipe" is found by "équipe" too.
    let wanted = name.to_lowercase();
    let enabled = || skills.iter().filter(|s| s.enabled);
    enabled().find(|s| s.name.trim().to_lowercase() == wanted).ok_or_else(|| {
        let available: Vec<&str> = enabled().map(|s| s.name.as_str()).collect();
        Error::Invalid(if available.is_empty() {
            format!("there is no skill called {name:?}, and none are defined in this vault yet.")
        } else {
            format!("there is no skill called {name:?}. Available skills: {}", available.join(", "))
        })
    })
}

/// Refuse a name that already belongs to another skill, matching case- and
/// whitespace-insensitively the way [`find_skill`] does -- the same name is
/// how `read_skill` finds a skill, so two skills that read alike cannot be
/// told apart by it. Checked against every skill, not only the enabled
/// ones: a name shared with a switched-off skill is still confusing on
/// `list_skills` and still a name that skill might get back the day it is
/// switched on again.
///
/// `except` leaves one skill out of its own clash check, so renaming
/// "Weekly review" to the name it already has is not a conflict with
/// itself.
fn refuse_duplicate_name(skills: &[Skill], name: &str, except: Option<SkillId>) -> Result<()> {
    let wanted = name.trim().to_lowercase();
    let clashes = skills
        .iter()
        .filter(|s| Some(s.id) != except)
        .any(|s| s.name.trim().to_lowercase() == wanted);
    if clashes {
        return Err(Error::Invalid(format!(
            "there is already a skill called {name:?}. Give the new one a different name, or \
             call update_skill on the existing one instead."
        )));
    }
    Ok(())
}

/// Find a skill by id, in plain words when there is no such skill -- the
/// same shape `agent::tools::memory`'s `run_forget` reads a memory with,
/// since a skill has no single-record getter of its own either, only
/// [`crate::vault::Vault::skills`]'s whole list.
fn load_skill(ctx: &ToolContext<'_>, id: SkillId) -> Result<Skill> {
    ctx.vault
        .skills()?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| Error::Invalid(format!("no skill with id {id}")))
}

fn describe_delete_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: SkillId = args.opt_id("skill_id", "skill").ok()??;
    load_skill(ctx, id).ok().map(|s| s.name)
}

fn skill_summary_json(s: &Skill) -> Value {
    json!({
        "id": s.id.to_string(),
        "name": s.name,
        "description": s.description,
        "enabled": s.enabled,
        "updated": s.updated_at.to_string(),
    })
}

fn run_list_skills(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let skills = ctx.vault.skills()?;
    Ok(json!({
        "count": skills.len(),
        "skills": skills.iter().map(skill_summary_json).collect::<Vec<_>>(),
    }))
}

/// Everything `create_skill` does to build the record, without saving it or
/// refusing an unattended run -- the half `run_create_skill` and
/// `build_create_skill` share. The unattended refusal lives in
/// `run_create_skill` alone, for the reason `routine_from_create_args`'s own
/// doc gives: drafting exists precisely to bypass it.
fn skill_from_create_args(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Skill> {
    let name = args.str("name")?;
    refuse_duplicate_name(&ctx.vault.skills()?, name, None)?;
    let mut skill = Skill::new(name);
    skill.description = args.str("description")?.to_string();
    skill.instructions = args.str("instructions")?.to_string();
    skill.validate()?;
    Ok(skill)
}

fn run_create_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    // The one recursion this refuses, for the same reason `create_routine`
    // does: a scheduled run teaching itself a new process, with nobody
    // watching, is a way to wake up to a vault full of them nobody asked
    // for. Asked for in a conversation, it is exactly right.
    if ctx.unattended {
        return Err(Error::Invalid(
            "a scheduled run may not create a skill unwatched. If a process is worth teaching \
             yourself, say so in your reply and propose it instead."
                .into(),
        ));
    }
    let skill = skill_from_create_args(ctx, args)?;
    ctx.vault.save_skill(&skill)?;
    done("created", "skill", &skill.name, skill.id.to_string())
}

fn build_create_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let skill = skill_from_create_args(ctx, args)?;
    let caption = format!("Create skill: {}", skill.name);
    Ok(Built {
        payload: Payload::Create { record: ProposedRecord::Skill(skill) },
        caption,
        about: None,
    })
}

/// Refuse to touch a skill the person switched off. Turning one off was
/// their decision, not a pause waiting for the assistant to fill in the
/// rest -- so an update that rewrote its words, or quietly flipped
/// `enabled` back on, would undo that decision without being asked.
/// `update_skill` and `build_update_skill` share this; `delete_skill` does
/// not need it -- removing a switched-off skill is no different from
/// removing any other.
fn refuse_if_disabled(skill: &Skill) -> Result<()> {
    if !skill.enabled {
        return Err(Error::Invalid(format!(
            "{:?} is switched off. The person turned it off on purpose, and it is theirs to \
             turn back on or change \u{2014} not something to edit quietly.",
            skill.name
        )));
    }
    Ok(())
}

/// Everything `update_skill` does to the loaded record, without saving it,
/// checking whether it is disabled, or checking a renamed skill against the
/// rest of the vault -- the half `run_update_skill` and `build_update_skill`
/// share. Kept clear of the vault entirely, the same reason
/// `agent::tools::memory`'s `check_confirmable` is its own function: the
/// rule here is "which fields change", and that much is testable without a
/// vault behind it.
fn apply_update_skill_args(args: &Args<'_>, mut skill: Skill) -> Result<Skill> {
    if let Some(name) = args.opt_str("name") {
        skill.name = name.to_string();
    }
    if let Some(description) = args.opt_str("description") {
        skill.description = description.to_string();
    }
    if let Some(instructions) = args.opt_str("instructions") {
        skill.instructions = instructions.to_string();
    }
    skill.touch();
    skill.validate()?;
    Ok(skill)
}

fn run_update_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if ctx.unattended {
        return Err(Error::Invalid(
            "a scheduled run may not change a skill unwatched. Propose the change instead.".into(),
        ));
    }
    let id: SkillId = args.id("skill_id", "skill")?;
    let existing = ctx.vault.skills()?;
    let original = existing
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| Error::Invalid(format!("no skill with id {id}")))?;
    refuse_if_disabled(&original)?;
    if let Some(name) = args.opt_str("name") {
        refuse_duplicate_name(&existing, name, Some(id))?;
    }
    let skill = apply_update_skill_args(args, original)?;
    ctx.vault.save_skill(&skill)?;
    done("updated", "skill", &skill.name, skill.id.to_string())
}

fn build_update_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let id: SkillId = args.id("skill_id", "skill")?;
    let existing = ctx.vault.skills()?;
    let original = existing
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| Error::Invalid(format!("no skill with id {id}")))?;
    refuse_if_disabled(&original)?;
    if let Some(name) = args.opt_str("name") {
        refuse_duplicate_name(&existing, name, Some(id))?;
    }
    let expected_updated_at = original.updated_at;
    let skill = apply_update_skill_args(args, original)?;
    let caption = format!("Change skill: {}", skill.name);
    Ok(Built {
        payload: Payload::Replace { record: ProposedRecord::Skill(skill), expected_updated_at },
        caption,
        about: None,
    })
}

fn run_delete_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if ctx.unattended {
        return Err(Error::Invalid(
            "a scheduled run may not delete a skill unwatched. Propose the deletion instead."
                .into(),
        ));
    }
    let id: SkillId = args.id("skill_id", "skill")?;
    // Read it first, so the confirmation card and the reply can name what
    // went rather than quoting an id at somebody.
    let skill = load_skill(ctx, id)?;
    ctx.vault.delete_skill(id)?;
    done("deleted", "skill", &skill.name, id.to_string())
}

fn build_delete_skill(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let id: SkillId = args.id("skill_id", "skill")?;
    let skill = load_skill(ctx, id)?;
    Ok(Built {
        payload: Payload::Delete { kind: ProposalKind::Skill, id: id.to_string() },
        caption: format!("Delete skill: {}", skill.name),
        about: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, enabled: bool) -> Skill {
        Skill {
            description: "Use it sometimes.".into(),
            instructions: "Do the thing.".into(),
            enabled,
            ..Skill::new(name)
        }
    }

    #[test]
    fn it_finds_a_skill_by_its_exact_name() {
        let skills = vec![skill("Plan a trip", true), skill("Weekly review", true)];
        let found = find_skill(&skills, "Plan a trip").unwrap();
        assert_eq!(found.name, "Plan a trip");
    }

    #[test]
    fn matching_ignores_case_and_surrounding_whitespace() {
        let skills = vec![skill("Plan a trip", true)];
        let found = find_skill(&skills, "  plan a TRIP  ").unwrap();
        assert_eq!(found.name, "Plan a trip");
    }

    #[test]
    fn an_unknown_name_lists_what_is_actually_available() {
        let skills = vec![skill("Plan a trip", true), skill("Weekly review", true)];
        let err = find_skill(&skills, "Nonexistent").unwrap_err().to_string();
        assert!(err.contains("Plan a trip"), "got: {err}");
        assert!(err.contains("Weekly review"), "got: {err}");
    }

    #[test]
    fn a_disabled_skill_cannot_be_loaded_and_is_not_offered_as_an_alternative() {
        let skills = vec![skill("Retired", false)];
        let err = find_skill(&skills, "Retired").unwrap_err().to_string();
        assert!(err.contains("no skill called"), "got: {err}");
        assert!(!err.contains("Available skills"), "an off skill leaves nothing to suggest");
    }

    #[test]
    fn an_empty_list_says_so_plainly_rather_than_an_empty_suggestion() {
        let err = find_skill(&[], "Anything").unwrap_err().to_string();
        assert!(err.contains("none are defined"), "got: {err}");
        assert!(!err.contains("Available skills"), "nothing to list");
    }

    // ---- refuse_duplicate_name ---------------------------------------------

    #[test]
    fn a_duplicate_name_is_refused_ignoring_case_and_surrounding_whitespace() {
        let skills = vec![skill("Plan a trip", true)];
        let err = refuse_duplicate_name(&skills, "  PLAN a Trip  ", None).unwrap_err().to_string();
        assert!(err.contains("already a skill called"), "got: {err}");
    }

    #[test]
    fn a_duplicate_name_is_refused_even_when_the_existing_skill_is_switched_off() {
        // A name is confusing on list_skills whether or not the skill it
        // already belongs to is enabled, and it is the name that skill
        // would get back the day it is switched on again.
        let skills = vec![skill("Retired process", false)];
        assert!(refuse_duplicate_name(&skills, "Retired process", None).is_err());
    }

    #[test]
    fn a_rename_is_not_a_clash_with_the_skill_being_renamed() {
        let mut existing = skill("Weekly review", true);
        existing.id = SkillId::new();
        let skills = vec![existing.clone()];
        refuse_duplicate_name(&skills, "Weekly review", Some(existing.id))
            .expect("renaming to the name it already has is not a conflict with itself");
    }

    #[test]
    fn a_different_skill_with_the_same_name_is_still_a_clash_on_rename() {
        let one = skill("Weekly review", true);
        let two = skill("Monthly review", true);
        let skills = vec![one.clone(), two.clone()];
        let err = refuse_duplicate_name(&skills, "Weekly review", Some(two.id)).unwrap_err();
        assert!(err.to_string().contains("already a skill called"));
    }

    // ---- refuse_if_disabled -------------------------------------------------

    #[test]
    fn an_enabled_skill_is_not_refused() {
        refuse_if_disabled(&skill("Plan a trip", true)).expect("an enabled skill may be edited");
    }

    #[test]
    fn a_disabled_skill_is_refused_for_editing() {
        let err = refuse_if_disabled(&skill("Retired", false)).unwrap_err().to_string();
        assert!(err.contains("switched off"), "got: {err}");
        assert!(err.contains("Retired"), "should name the skill: {err}");
    }

    // ---- apply_update_skill_args --------------------------------------------

    fn args(v: serde_json::Value) -> (&'static str, serde_json::Value) {
        ("update_skill", v)
    }

    #[test]
    fn omitted_fields_are_left_exactly_as_they_were() {
        let original = skill("Plan a trip", true);
        let (name, v) = args(json!({ "skill_id": original.id.to_string() }));
        let a = Args::new(name, &v);
        let updated = apply_update_skill_args(&a, original.clone()).unwrap();
        assert_eq!(updated.name, original.name);
        assert_eq!(updated.description, original.description);
        assert_eq!(updated.instructions, original.instructions);
        assert_eq!(updated.enabled, original.enabled, "a partial update never touches enabled");
        assert!(updated.updated_at >= original.updated_at);
    }

    #[test]
    fn only_the_given_fields_change() {
        let original = skill("Plan a trip", true);
        let (name, v) = args(json!({
            "skill_id": original.id.to_string(),
            "description": "Use when asked to plan a multi-day trip.",
        }));
        let a = Args::new(name, &v);
        let updated = apply_update_skill_args(&a, original.clone()).unwrap();
        assert_eq!(updated.name, original.name, "name was not given, so it stays");
        assert_eq!(updated.description, "Use when asked to plan a multi-day trip.");
        assert_eq!(updated.instructions, original.instructions);
    }

    #[test]
    fn a_full_rewrite_replaces_every_field_given() {
        let original = skill("Plan a trip", true);
        let (name, v) = args(json!({
            "skill_id": original.id.to_string(),
            "name": "Plan a journey",
            "description": "Use when asked to plan travel of any length.",
            "instructions": "Check the calendar, then the weather, then propose blocks.",
        }));
        let a = Args::new(name, &v);
        let updated = apply_update_skill_args(&a, original).unwrap();
        assert_eq!(updated.name, "Plan a journey");
        assert_eq!(updated.description, "Use when asked to plan travel of any length.");
        assert_eq!(
            updated.instructions,
            "Check the calendar, then the weather, then propose blocks."
        );
    }

    #[test]
    fn an_all_whitespace_field_is_treated_as_omitted_rather_than_blanked_out() {
        let original = skill("Plan a trip", true);
        let (name, v) = args(json!({ "skill_id": original.id.to_string(), "description": "   " }));
        let a = Args::new(name, &v);
        // `opt_str` treats an all-whitespace string as absent, so this is
        // read as "nothing given" rather than "blank it out" -- the same
        // rule every other optional text argument in this crate follows.
        // `validate` is never even asked to refuse an empty description,
        // because the field it would refuse was never changed.
        let updated = apply_update_skill_args(&a, original.clone()).unwrap();
        assert_eq!(updated.description, original.description);
    }
}
