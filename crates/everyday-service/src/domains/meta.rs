//! The surface describing itself, and the assistant's tools without a model.
//!
//! Two things a client that is not this application's own interface needs. The
//! catalogue, so a generator can build a typed client and a person can see what
//! a token could reach; and the tools, so a palette, a shortcut or a script can
//! run one of the assistant's verbs directly.
//!
//! # Scope, twice
//!
//! One row in the command table carries one scope, and a tool needs one *per
//! domain*: `add_task` wants `Tasks`, `delete_note` wants `Notes`. So the two
//! tool rows declare [`Scope::Any`], which asks only that the caller be
//! somebody, and the real check lives in the handlers.
//!
//! Getting this wrong is easy and quiet, so it is worth being exact about the
//! order. [`Command::invoke`] enforces the row's scope *before* the handler
//! runs. A row naming a domain -- `Journals`, say -- therefore refuses a
//! token scoped to Tasks alone before any finer check can run, and the narrow
//! tokens this whole arrangement exists for would never reach a tool they are
//! entitled to. A row naming `All` refuses them just as flatly.
//!
//! The cost is that these two rows make a weaker promise than every other row
//! in the table, and the table can no longer be read on its own to see what
//! `run_tool` needs. That is only sound because the handler's check is not
//! optional and not last: [`scope_of`] is consulted in `run_tool` before it
//! will say anything at all about what this vault holds.
//!
//! [`Command::invoke`]: crate::command::Command::invoke
//! [`Scope::Any`]: crate::ctx::Scope::Any

use crate::command;
use crate::ctx::{Ctx, Scope};
use crate::error::{CommandError, CommandResult};
use crate::service::{PROTOCOL, Service, blocking};
use everyday_core::agent::tools;
use everyday_core::model::{system_tz, today_local};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use super::Nothing;

/// The surface, and what version of it this build speaks.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Surface {
    pub protocol: u32,
    pub commands: Vec<crate::command::CommandInfo>,
}

/// One tool, with a label for a person rather than a description for a model.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: &'static str,
    pub title: String,
    pub description: &'static str,
    pub effect: &'static str,
    /// The scope this tool is gated behind, so a settings panel can offer it
    /// as a checkbox rather than a person having to know the domain names
    /// `everyday_core` uses internally.
    pub scope: &'static str,
    pub schema: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTool {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    /// Answer a destructive tool's question in advance. A caller that cannot
    /// ask a person -- a script, a scheduled job -- has to say so explicitly
    /// rather than have the refusal quietly skipped.
    #[serde(default)]
    pub confirm_destructive: bool,
}

/// The scope a tool's domain is gated behind.
///
/// `Domain` and `Scope` are nearly the same enum, because a tool's domain
/// *is* the resource a scope names -- but they are declared in different
/// crates, for different readers, so the mapping is written out here rather
/// than derived. Exhaustive and without a wildcard arm, for the reason
/// `Domain::sensitivity` in `tools.rs` gives for doing the same: a domain
/// added to that enum without a line in this match must fail to compile
/// rather than silently inherit whatever scope the wildcard would have
/// picked.
///
/// Routines map to [`Scope::Agent`] rather than a scope of their own, because
/// the command table already agrees: every command in `domains::routines`
/// -- the same standing work these tools reach -- is declared `scope: Agent`.
/// A routine tool and the command that lists it stay reachable under the
/// same scope rather than gaining a second one for the same resource.
pub fn scope_of(domain: tools::Domain) -> Scope {
    match domain {
        tools::Domain::Journals => Scope::Journals,
        tools::Domain::Notes => Scope::Notes,
        tools::Domain::Tasks => Scope::Tasks,
        tools::Domain::Calendars => Scope::Calendars,
        tools::Domain::Library => Scope::Library,
        tools::Domain::Trackers => Scope::Trackers,
        tools::Domain::Purpose => Scope::Purpose,
        tools::Domain::Routines => Scope::Agent,
        tools::Domain::Agent => Scope::Agent,
    }
}

/// A label for a person, from the name a model reads.
///
/// Derived rather than declared. Every tool and command in this application is
/// named `verb_noun`, which is one underscore away from a sentence, and a
/// second string on thirty-four entries would be thirty-four more places for
/// the two to disagree. A tool whose derived title reads badly is a tool whose
/// *name* reads badly.
pub fn title_of(name: &str) -> String {
    let mut words = name.replace('_', " ");
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    words
}

async fn list_commands(_svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Surface> {
    let commands = crate::command::catalog().iter().map(|c| crate::command::describe(c)).collect();
    Ok(Surface { protocol: PROTOCOL, commands })
}

/// Every tool this vault can offer and this caller may reach, with a title.
///
/// Filtered twice, for two different reasons. `tools::available` is the same
/// filtered set the assistant is given, which matters for more than
/// tidiness: a domain classed as secret is absent from both, so a palette
/// cannot offer what a model is not allowed to know exists. The scope filter
/// on top of it is about the caller rather than the vault: a token issued
/// for Tasks alone has no business being shown a Notes tool it could not run.
async fn list_tools(svc: Arc<Service>, ctx: Ctx, _args: Nothing) -> CommandResult<Vec<ToolInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        Ok(tools::available(&vault)
            .into_iter()
            .filter_map(|t| {
                // Mapped once and then both filtered on and recorded. Two
                // calls would be two answers to one question, which is one
                // more than a filter and its result should ever disagree on.
                let scope = scope_of(t.domain);
                ctx.holds(scope).then(|| ToolInfo {
                    name: t.name,
                    title: title_of(t.name),
                    description: t.description,
                    effect: crate::command::effect_name(t.effect),
                    scope: scope.as_str(),
                    schema: t.parameters(),
                })
            })
            .collect())
    })
    .await
}

/// Run one of the assistant's tools, with no model in the loop.
///
/// What a palette, a keyboard shortcut or a script calls. The confirmation rule
/// is the assistant's own: a destructive tool is refused unless the caller has
/// said, in this call, that it means it. There is no undo in this application,
/// so a script that deletes a project has to be a script that asked to.
async fn run_tool(svc: Arc<Service>, ctx: Ctx, args: RunTool) -> CommandResult<Value> {
    let vault = svc.require()?;
    let Some(tool) = tools::find(&args.name) else {
        return Err(CommandError::new(
            "unknown_tool",
            format!("there is no tool called {:?}", args.name),
        ));
    };
    // Before anything is said about what this vault holds. A caller scoped to
    // Tasks alone must not be able to tell a Notes tool this backend cannot
    // carry from one it can, and asking about the domain first is what keeps
    // the two answers indistinguishable from outside.
    ctx.require(scope_of(tool.domain))?;
    // Checked against what this vault offers rather than against the whole
    // catalogue, so a tool from a domain the backend cannot carry -- or one
    // classed as secret -- is not reachable by naming it.
    let offered = {
        let vault = vault.clone();
        let name = args.name.clone();
        blocking(move || Ok(tools::available(&vault).iter().any(|t| t.name == name))).await?
    };
    if !offered {
        return Err(CommandError::new(
            "unsupported",
            format!("{} is not available on this vault", args.name),
        ));
    }
    if matches!(tool.effect, tools::Effect::Destructive) && !args.confirm_destructive {
        return Err(CommandError::new(
            "confirm_required",
            format!("{} deletes something; call it again with confirmDestructive", args.name),
        ));
    }

    blocking(move || {
        let tz = system_tz();
        let ctx = tools::ToolContext {
            vault: &vault,
            today: today_local(),
            tz: &tz,
            conversation: None,
            // A script or a palette entry. Somebody pressed something.
            unattended: false,
        };
        tools::dispatch(&ctx, &args.name, &args.arguments).map_err(CommandError::from)
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_commands", scope: Journals, effect: Read,
        args: Nothing, returns: "Surface", signature: &[],
        run: list_commands,
    },
    command! {
        // `Any`, not a domain: this answers with whatever the caller may
        // reach, so a token holding one narrow scope gets a short list
        // rather than a refusal. The filtering is in the handler.
        name: "list_tools", scope: Any, effect: Read,
        args: Nothing, returns: "ToolInfo[]", signature: &[],
        run: list_tools,
    },
    command! {
        // `Any` is the whole of what this row can honestly ask -- see the
        // note on `Scope::Any`. What a call actually needs depends on which
        // tool it names, which one field cannot express, so the real check
        // is the first thing `run_tool` does.
        name: "run_tool", scope: Any, effect: Write,
        args: RunTool, returns: "unknown",
        signature: &[
            ("name", "string", true),
            ("arguments", "unknown", false),
            ("confirmDestructive", "boolean", false),
        ],
        run: run_tool,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_the_name_read_aloud() {
        assert_eq!(title_of("create_task"), "Create task");
        assert_eq!(title_of("overview"), "Overview");
        assert_eq!(title_of("list_time_blocks"), "List time blocks");
    }

    #[test]
    fn a_domain_maps_to_the_scope_its_own_commands_are_declared_under() {
        use tools::Domain;
        assert_eq!(scope_of(Domain::Journals), Scope::Journals);
        assert_eq!(scope_of(Domain::Notes), Scope::Notes);
        assert_eq!(scope_of(Domain::Tasks), Scope::Tasks);
        assert_eq!(scope_of(Domain::Calendars), Scope::Calendars);
        assert_eq!(scope_of(Domain::Library), Scope::Library);
        assert_eq!(scope_of(Domain::Trackers), Scope::Trackers);
        assert_eq!(scope_of(Domain::Purpose), Scope::Purpose);
        // Both the assistant's own memory and its standing routines are
        // gated behind Agent, matching `domains::assistant` and
        // `domains::routines`.
        assert_eq!(scope_of(Domain::Routines), Scope::Agent);
        assert_eq!(scope_of(Domain::Agent), Scope::Agent);
    }
}
