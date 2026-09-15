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
use crate::error::{CommandError, CommandResult, codes, mail_rate_limit_error};
use crate::service::{PROTOCOL, Service, blocking};
use everyday_core::agent::tools::{self, Caller as ToolCaller};
use everyday_core::mail::Origin as MailOrigin;
use everyday_core::model::{system_tz, today_local};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use super::Nothing;

/// Who a `list_tools` or `run_tool` call is really on behalf of, named by
/// whoever built the JSON this deserialises from -- never taken at face
/// value from an arbitrary caller, which is what makes it safe to trust.
///
/// # Where this comes from, and why it is not a security hole
///
/// The only two things that ever set this field are `everyday-server`'s
/// `VaultHost`, which mints it itself from the authenticated MCP caller's
/// own device id (`everyday_mcp`'s wire protocol carries no such field for
/// this to merely proxy), and nothing else -- a palette entry or a script
/// calling `run_tool` over the ordinary command surface leaves it unset,
/// which reads as the vault's owner acting directly.
///
/// A device that *could* set this by hand on an ordinary `/v1` call already
/// has to hold [`Scope::Mail`] to reach a single mail tool at all -- exactly
/// as much as it needs to read mail through `domains::mail`'s own commands
/// -- and claiming to be MCP only ever *narrows* what it may do next, to
/// whatever `mcp_access` allows, never widens it past what the vault's
/// owner already has. See `everyday_core::agent::tools::mail`'s module docs
/// for what each switch actually gates.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WireCaller {
    Mcp { client: String },
}

impl WireCaller {
    fn into_tool_caller(self) -> ToolCaller {
        match self {
            WireCaller::Mcp { client } => ToolCaller::Mcp { client },
        }
    }
}

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

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListTools {
    /// See [`WireCaller`]. Absent for every caller but `VaultHost`.
    #[serde(default)]
    pub caller: Option<WireCaller>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTool {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    /// Answer a destructive *or* outward tool's question in advance. A
    /// caller that cannot ask a person -- a script, a scheduled job, an MCP
    /// client with no confirmation UI of its own -- has to say so
    /// explicitly rather than have the refusal quietly skipped. See
    /// [`run_tool`] for what each effect actually requires this to be true
    /// for, since the two are not gated by the same thing: a destructive
    /// call needs this server's `allow_destructive` switch to ever set it,
    /// an outward one (`send_draft`) needs only the caller to say so,
    /// because its real gate is the per-account `send` switch a tool checks
    /// for itself.
    #[serde(default)]
    pub confirm_destructive: bool,
    /// See [`WireCaller`].
    #[serde(default)]
    pub caller: Option<WireCaller>,
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
        // Mail's own scope, not a domain-shaped one of its own reasoning:
        // see `Scope::Mail`'s doc for why a client that could read mail
        // through the ordinary command surface must ask for it by name,
        // never inherit it from a broader grant.
        tools::Domain::Mail => Scope::Mail,
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
async fn list_tools(svc: Arc<Service>, ctx: Ctx, args: ListTools) -> CommandResult<Vec<ToolInfo>> {
    let vault = svc.require()?;
    let caller = args.caller.map(WireCaller::into_tool_caller);
    blocking(move || {
        // `assistant_provider: None` throughout this module: the two
        // callers that ever reach `list_tools`/`run_tool` are a script or
        // palette entry (`caller: None`, unrestricted) and MCP (`caller:
        // Some(Mcp { .. })`, which needs no acknowledgement) -- never the
        // chat assistant, which is wired up entirely inside `agent.rs` and
        // never touches this command at all.
        Ok(tools::available_for(&vault, caller.as_ref(), None)
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
/// What a palette, a keyboard shortcut, a script or MCP's own `VaultHost`
/// calls. The confirmation rule mirrors the chat assistant's: a destructive
/// call is refused unless the caller has said, in this call, that it means
/// it, and so -- for a different reason -- is an outward one. There is no
/// chat window here to ask in, so both effects reuse `confirmDestructive`
/// as one "yes, I mean it" flag rather than growing a second that would
/// mean the same thing.
///
/// For `Effect::Outward` specifically, that flag is not what actually keeps
/// a send safe: `send_draft` checks the account's own `send` switch for
/// this call's `caller` regardless of what this function does. What this
/// function's check stops is a *silent* send -- one from a caller with no
/// way to be asked and nothing set explicitly -- not an *unpermitted* one,
/// which the tool refuses on its own. `VaultHost` always sets this for an
/// outward call, on exactly that reasoning: see its own module docs for why
/// that is the right call for MCP, which has no confirmation UI at all.
async fn run_tool(svc: Arc<Service>, ctx: Ctx, args: RunTool) -> CommandResult<Value> {
    let vault = svc.require()?;
    let Some(tool) = tools::find(&args.name) else {
        return Err(CommandError::new(
            codes::UNKNOWN_TOOL,
            format!("there is no tool called {:?}", args.name),
        ));
    };
    // Before anything is said about what this vault holds. A caller scoped to
    // Tasks alone must not be able to tell a Notes tool this backend cannot
    // carry from one it can, and asking about the domain first is what keeps
    // the two answers indistinguishable from outside.
    ctx.require(scope_of(tool.domain))?;
    let caller = args.caller.clone().map(WireCaller::into_tool_caller);
    // Checked against what this vault offers rather than against the whole
    // catalogue, so a tool from a domain the backend cannot carry -- or one
    // no account permits this caller to use at all -- is not reachable by
    // naming it. See `tools::available_for`.
    let offered = {
        let vault = vault.clone();
        let name = args.name.clone();
        let caller = caller.clone();
        blocking(move || {
            Ok(tools::available_for(&vault, caller.as_ref(), None).iter().any(|t| t.name == name))
        })
        .await?
    };
    if !offered {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            format!("{} is not available on this vault", args.name),
        ));
    }
    if matches!(tool.effect, tools::Effect::Destructive | tools::Effect::Outward)
        && !args.confirm_destructive
    {
        let verb = if tool.effect == tools::Effect::Outward {
            "sends something"
        } else {
            "deletes something"
        };
        return Err(CommandError::new(
            codes::CONFIRM_REQUIRED,
            format!("{} {verb}; call it again with confirmDestructive", args.name),
        ));
    }

    blocking(move || {
        let tz = system_tz();
        // Held for the call: `Service::mail_index` hands back an `Arc`, and
        // `mail_search` below borrows from it.
        let mail_index = svc.mail_index();
        let svc_for_rl = svc.clone();
        // Every call through this command is its own "turn" for
        // rate-limiting purposes -- there is no multi-call model turn here,
        // only ever one call per `run_tool` invocation -- so a fresh,
        // never-repeated string is used each time. Only the per-minute
        // budget can ever bind as a result, which is the intended shape for
        // MCP: each `tools/call` is independent, and the per-turn cap exists
        // to stop one *model* turn spending a whole minute's budget in one
        // go, a concept this call site has no equivalent of.
        let call_turn = uuid::Uuid::now_v7().to_string();
        let rate_limit = move |origin: &MailOrigin| -> everyday_core::error::Result<()> {
            svc_for_rl.check_mail_rate_limit(origin, &call_turn).map_err(mail_rate_limit_error)
        };
        let ctx = tools::ToolContext {
            vault: &vault,
            today: today_local(),
            tz: &tz,
            conversation: None,
            // A script, a palette entry or MCP -- never a scheduled run,
            // which goes through `agent::run_turn` instead.
            unattended: false,
            caller,
            mail_search: mail_index.as_deref(),
            // Only the chat assistant's own acknowledgement gate reads
            // this, and the chat assistant never reaches this function --
            // see `run_tool`'s own doc.
            assistant_provider: None,
            mail_rate_limit: Some(&rate_limit),
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
        args: ListTools, returns: "ToolInfo[]",
        // `caller` is deliberately absent from the documented signature --
        // see `WireCaller`'s own doc -- even though the struct accepts it:
        // this is what `everyday-server`'s `VaultHost` sends, never what the
        // interface's own client is meant to set.
        signature: &[],
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
        assert_eq!(scope_of(Domain::Mail), Scope::Mail);
    }
}
