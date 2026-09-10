//! The surface describing itself, and the assistant's tools without a model.
//!
//! Two things a client that is not this application's own interface needs. The
//! catalogue, so a generator can build a typed client and a person can see what
//! a token could reach; and the tools, so a palette, a shortcut or a script can
//! run one of the assistant's verbs directly.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::service::{PROTOCOL, Service, blocking};
use everyday_core::agent::tools;
use everyday_core::model::{system_tz, today_local};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use super::vault::Nothing;

/// One command, as a client generator sees it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: String,
    pub scope: String,
    pub effect: &'static str,
    pub sensitive: bool,
    pub streams: bool,
    pub args: Vec<ArgInfo>,
    pub returns: &'static str,
    /// What a listener should reload after this succeeds, if anything.
    pub changes: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgInfo {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub required: bool,
}

/// The surface, and what version of it this build speaks.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Surface {
    pub protocol: u32,
    pub commands: Vec<CommandInfo>,
}

/// One tool, with a label for a person rather than a description for a model.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: &'static str,
    pub title: String,
    pub description: &'static str,
    pub effect: &'static str,
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

fn effect_name(effect: tools::Effect) -> &'static str {
    match effect {
        tools::Effect::Read => "read",
        tools::Effect::Write => "write",
        tools::Effect::Destructive => "destructive",
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

async fn list_commands(_s: Arc<Service>, _c: Ctx, _a: Nothing) -> CommandResult<Surface> {
    let commands = crate::command::catalog()
        .iter()
        .map(|c| CommandInfo {
            name: c.name.to_string(),
            scope: c.scope.as_str().to_string(),
            effect: effect_name(c.effect),
            sensitive: c.sensitive,
            streams: c.streams,
            args: c
                .signature
                .args
                .iter()
                .map(|(name, ty, required)| ArgInfo { name, ty, required: *required })
                .collect(),
            returns: c.signature.returns,
            changes: c.change.map(|(kind, _)| match kind {
                crate::events::Kind::Journal => "journal",
                crate::events::Kind::Entry => "entry",
                crate::events::Kind::Note => "note",
                crate::events::Kind::Project => "project",
                crate::events::Kind::Task => "task",
                crate::events::Kind::Block => "block",
                crate::events::Kind::Calendar => "calendar",
                crate::events::Kind::Event => "event",
                crate::events::Kind::Shelf => "shelf",
                crate::events::Kind::Item => "item",
                crate::events::Kind::Log => "log",
                crate::events::Kind::Tracker => "tracker",
                crate::events::Kind::Reading => "reading",
                crate::events::Kind::Role => "role",
                crate::events::Kind::Goal => "goal",
                crate::events::Kind::Routine => "routine",
                crate::events::Kind::RoutineRun => "routineRun",
                crate::events::Kind::Conversation => "conversation",
                crate::events::Kind::Memory => "memory",
                crate::events::Kind::Settings => "settings",
            }),
        })
        .collect();
    Ok(Surface { protocol: PROTOCOL, commands })
}

/// Every tool this vault can offer, with a title.
///
/// The same filtered set the assistant is given, which matters for more than
/// tidiness: a domain classed as secret is absent from both, so a palette
/// cannot offer what a model is not allowed to know exists.
async fn list_tools(svc: Arc<Service>, _c: Ctx, _a: Nothing) -> CommandResult<Vec<ToolInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        Ok(tools::available(&vault)
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name,
                title: title_of(t.name),
                description: t.description,
                effect: effect_name(t.effect),
                schema: t.parameters(),
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
async fn run_tool(svc: Arc<Service>, _c: Ctx, args: RunTool) -> CommandResult<Value> {
    let vault = svc.require()?;
    let Some(tool) = tools::find(&args.name) else {
        return Err(CommandError::new(
            "unknown_tool",
            format!("there is no tool called {:?}", args.name),
        ));
    };
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
        name: "list_tools", scope: Journals, effect: Read,
        args: Nothing, returns: "ToolInfo[]", signature: &[],
        run: list_tools,
    },
    command! {
        name: "run_tool", scope: All, effect: Write,
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
}
