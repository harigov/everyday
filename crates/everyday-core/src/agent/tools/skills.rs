//! Loading a skill's full instructions. See [`crate::agent`]'s module docs,
//! "Skills, and progressive disclosure".
//!
//! One tool, and it only reads: a skill is managed from Settings, not by the
//! assistant asking for one to be written or changed, so there is nothing
//! here to write or to propose while dreaming.

use serde_json::{Value, json};

use super::{Args, Tool, ToolContext, schema, text};
use crate::agent::Skill;
use crate::error::{Error, Result};

pub(super) static TOOLS: &[Tool] = &[tool!(
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
)];

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
}
