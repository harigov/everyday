//! What we tell the calling model, once, before it has called anything.
//!
//! `instructions` is the one piece of this protocol that is prose rather
//! than schema, and it is read by exactly the audience `tools.rs` writes
//! its tool descriptions for — a model deciding what to do next, not a
//! person configuring a client. It is worth the same care as a tool
//! description, and it has to carry three things a schema cannot:
//!
//! - **What this is.** A vault is somebody's own journal, tasks and notes,
//!   not a shared workspace — the tone a model should bring to it is
//!   closer to "someone's diary" than "a project tracker".
//! - **Where ids come from.** `everyday_core::agent::tools` takes an id
//!   for almost every mutating tool and deliberately gives a model no way
//!   to invent one — see that module's "Ids, and why every tool takes
//!   one". The instructions are the only place this constraint can be
//!   stated before the model has made the mistake once and been corrected;
//!   after that it is just a wasted turn.
//! - **What an empty tool list means.** A locked or absent vault offers no
//!   tools at all rather than an error — see `docs/plans/mcp.md`'s "A
//!   locked vault offers nothing" — and a model that has not been told so
//!   will read that silence as a broken connection rather than as "try
//!   again shortly".
//!
//! This text is combined with whatever
//! [`Host::instructions`](crate::Host::instructions) adds — vault-specific
//! detail this crate has no way to know, such as the vault's own name —
//! rather than replaced by it, so every host gets the baseline whether or
//! not it has anything to add.

/// Guidance every host gets, regardless of what it adds of its own.
const BASE: &str = "\
This server puts one person's private journal, tasks, notes and calendar \
in front of you. Treat it the way you would treat someone's own diary: \
what you read here was written for its owner, not for an audience, and \
what you write should read as if they wrote it themselves.

Ids are not yours to invent. Almost every tool that changes something \
takes an id, and the only correct source for one is a search or list \
tool called first in this same conversation — never a guess, and never \
one you recall from an earlier turn, because it may no longer refer to \
the same thing. If you do not already have an id from a call you just \
made, make that call before the one that needs it.

If the tool list is empty, the vault is locked or not open right now. \
That is not a broken connection and not your fault \u{2014} it means \
nothing can be read or changed at the moment, and the list will fill in \
on its own once the vault is unlocked. There is nothing useful to retry \
immediately; say so and wait to be asked again.";

/// The full `instructions` string for `server/discover` and legacy
/// `initialize`: the baseline above, plus whatever the host has to add
/// about this particular vault.
pub(crate) fn full(host_instructions: Option<&str>) -> String {
    match host_instructions {
        Some(extra) if !extra.trim().is_empty() => format!("{BASE}\n\n{}", extra.trim()),
        _ => BASE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_the_three_things_it_must() {
        let text = full(None);
        assert!(text.contains("locked"));
        assert!(text.to_lowercase().contains("id"));
        assert!(text.to_lowercase().contains("diary") || text.to_lowercase().contains("private"));
    }

    #[test]
    fn appends_host_detail_without_losing_the_base() {
        let text = full(Some("This vault is called Fieldwork."));
        assert!(text.contains("Fieldwork"));
        assert!(text.contains("locked"));
    }
}
