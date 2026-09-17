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
use crate::ctx::{Caller, Ctx, Scope};
use crate::error::{CommandError, CommandResult, codes, mail_rate_limit_error};
use crate::events::{Kind, Op};
use crate::service::{PROTOCOL, Service, blocking};
use everyday_core::agent::tools::{self, Caller as ToolCaller};
use everyday_core::mail::Origin as MailOrigin;
use everyday_core::model::{local_date_in, system_tz};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use super::Nothing;

/// What a `list_tools` or `run_tool` request's own JSON *claims* about who
/// it is on behalf of.
///
/// # This is not where trust comes from any more
///
/// It used to be: the doc here previously argued that only `everyday-server`'s
/// `VaultHost` ever sets this field, so a body claiming `Mcp` could only ever
/// *narrow* what it could do. That argument had a hole exactly the size of
/// the field's `#[serde(default)]`: nothing made *omitting* the claim
/// exclusive to a caller entitled to omit it. `everyday-server`'s
/// `mcp::issue_token` records an MCP client's token in the very same
/// `devices.json` the ordinary `/v1/call/{name}` route authenticates
/// against, so a client holding nothing but that token could `POST
/// /v1/call/run_tool` with no `caller` at all and be read by
/// [`require_permission`](everyday_core::agent::tools::mail) as the vault's
/// owner acting directly -- skipping every per-account `mcp_access` switch,
/// including `send`, which defaults to off.
///
/// The fix is that a claim in this shape is now merely a hint the wire may
/// supply for attribution (the `client` label an MCP client's write is
/// stamped with, see [`everyday_core::mail::Origin::Mcp`]) and is no longer
/// what decides *whether* a call may claim to be MCP at all. [`resolve_caller`]
/// makes that decision from the request's *authenticated connection* --
/// `Ctx::caller`, built by `everyday-server`'s `auth::Registry::authenticate`
/// from the bearer token itself, specifically whether the token's own
/// device row was minted by `auth::Registry::issue` for an MCP client
/// (`auth::Device::mcp`) -- and refuses a body whose claim disagrees with
/// what the connection actually is, rather than trusting either side alone.
/// See [`resolve_caller`]'s own doc for exactly how.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WireCaller {
    Mcp { client: String },
}

/// Decide the [`ToolCaller`] this call may actually claim, from the
/// authenticated connection (`ctx.caller`) rather than from `claimed` --
/// the request body's own [`WireCaller`], which is no longer trusted alone
/// for anything more than an attribution label. See [`WireCaller`]'s module
/// doc for the bypass this closes.
///
/// - A connection authenticated as an MCP-issued device (its id carries
///   `everyday_core::agent::tools::MCP_DEVICE_ID_PREFIX`, stamped there by
///   `auth::Registry::authenticate` and nowhere else) can *never* be read as
///   the vault's owner acting directly, no matter what -- or whether -- the
///   body claims. `caller: None` on such a connection is not "the owner",
///   it is a body that simply did not bother to say what the connection
///   already proves; this manufactures `Mcp` regardless, keeping the body's
///   own `client` label if it supplied one purely for attribution.
/// - Any other connection (the local window, the local socket, an
///   ordinarily-paired device) is free to omit the claim, which reads as the
///   owner exactly as it always has. It may *not* claim `Mcp`: that claim
///   would not describe this connection, and a mismatch between what a
///   request says and what it is gets refused rather than silently resolved
///   in either direction -- see the module's `RunTool::caller` doc for why a
///   caller that could talk itself into a wider identity than its own
///   connection proves is exactly the shape of bug this whole function
///   exists to close.
fn resolve_caller(ctx: &Ctx, claimed: Option<WireCaller>) -> CommandResult<Option<ToolCaller>> {
    let device_is_mcp = matches!(
        &ctx.caller,
        Caller::Device(id) if everyday_core::agent::tools::is_mcp_device_id(id)
    );
    match (device_is_mcp, claimed) {
        (true, Some(WireCaller::Mcp { client })) => Ok(Some(ToolCaller::Mcp { client })),
        (true, None) => {
            let client = ctx.caller.origin().unwrap_or("mcp").to_string();
            Ok(Some(ToolCaller::Mcp { client }))
        }
        (false, None) => Ok(None),
        (false, Some(WireCaller::Mcp { .. })) => Err(CommandError::new(
            codes::UNSUPPORTED,
            "this connection was not authenticated as an MCP client and may not claim to be one",
        )),
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
    /// See [`WireCaller`] and [`resolve_caller`]. A hint only -- an
    /// attribution label an already-authenticated MCP connection may
    /// supply, never what decides whether this call may be treated as one.
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
    /// See [`WireCaller`] and [`resolve_caller`]. A hint only -- an
    /// attribution label an already-authenticated MCP connection may
    /// supply, never what decides whether this call may be treated as one.
    /// In particular, *omitting* this is not the same thing as being
    /// entitled to omit it: whether that reads as the vault's owner acting
    /// directly is decided from this call's `Ctx`, not from this field.
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
        // Its own scope, for the reason `Scope::Meetings`'s own doc gives:
        // a transcript is every word several people said, and a client
        // that reads a diary has no business inheriting a way to read one.
        tools::Domain::Meetings => Scope::Meetings,
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
    let caller = resolve_caller(&ctx, args.caller)?;
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
    // Stamped on the `Change` this call may raise -- see `mail_tool_change_kind`
    // -- exactly as `Command::invoke` stamps its own, so a client that made
    // this write itself does not reload because of it. Read off `ctx` here,
    // before it is shadowed below by the `ToolContext` built for the tool
    // itself, which has no `Ctx` of its own to read this from.
    let origin = ctx.caller.origin().map(str::to_string);
    let caller = resolve_caller(&ctx, args.caller.clone())?;
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
        // See `ToolContext::after_mail_write`'s own doc: the one door a
        // successful mail write here shares with a person's own click
        // through `domains::mail` -- wakes the account's sync task and
        // drops its cached unread counts, so a script or MCP archive is
        // seen exactly as promptly as a person's own.
        let svc_for_notify = svc.clone();
        let after_mail_write = move |account: everyday_core::id::AccountId| {
            svc_for_notify.notify_mail_write(account);
        };
        // See `agent::run_tool`'s own `invite_responder`: the same closure,
        // built here for the script/MCP path, which has no `Ctx` of its own
        // by the time a tool body runs -- see
        // `domains::mail::respond_to_invite_for_tool`'s doc for why
        // `Change::origin` is `None` from this path too.
        let svc_for_invite = svc.clone();
        let invite_responder = move |message_id: everyday_core::id::MailMessageId,
                                     response: everyday_core::mail::AttendeeResponse,
                                     comment: Option<String>,
                                     origin: MailOrigin|
              -> everyday_core::error::Result<()> {
            crate::domains::mail::respond_to_invite_for_tool(
                &svc_for_invite,
                message_id,
                response,
                comment,
                origin,
            )
        };
        // `unattended` stays the default `false`: a script, a palette entry
        // or MCP, never a scheduled run, which goes through `agent::run_turn`
        // instead. `assistant_provider` stays unset too -- only the chat
        // assistant's own acknowledgement gate reads it, and the chat
        // assistant never reaches this function; see this function's own doc.
        // Today through the service's own clock, the way `agent::run_turn`
        // builds its context: a tool run from a script, the palette or MCP
        // and the same tool run from chat must agree about what day it is,
        // and a proposal minted here gets an `expires_at` the scheduler
        // later judges against that same clock.
        let mut ctx = tools::ToolContext::new(&vault, local_date_in(svc.now(), &tz), &tz)
            .with_mail_search(mail_index.as_deref())
            .with_mail_rate_limit(&rate_limit)
            .with_after_mail_write(&after_mail_write)
            .with_invite_responder(&invite_responder);
        if let Some(caller) = caller {
            ctx = ctx.with_caller(caller);
        }
        // Collected locally, around this one call, rather than read from
        // `crate::touched`'s own task-local: this closure runs on the
        // blocking pool `blocking` already dispatched onto, a different
        // task from the one `Command::invoke` opened its own scope on, and
        // that scope is not reachable from here -- see `crate::touched`'s
        // module doc. `everyday_core::vault::touched::collect` needs
        // nothing from that scope; it is a plain synchronous call around a
        // plain synchronous call.
        let (result, dispatched) = everyday_core::vault::touched::collect(|| {
            tools::dispatch(&ctx, &args.name, &args.arguments)
        });
        let result = result.map_err(CommandError::from)?;
        // See `mail_tool_change_kind`'s own doc: this is `run_tool`'s
        // equivalent of the `change:` a row in `command::COMMANDS` declares
        // for itself, for the one row -- this one -- whose actual effect
        // depends on which tool it named rather than being fixed at the
        // table.
        if let Some((kind, op)) = mail_tool_change_kind(&args.name) {
            crate::events::emit_touched(
                svc.events().as_ref(),
                origin.clone(),
                &dispatched,
                kind,
                op,
            );
        }
        Ok(result)
    })
    .await
}

/// The `(Kind, Op)` equivalent, for a mail tool run through [`run_tool`], to
/// what the matching direct command in `domains::mail` declares on its own
/// row via `change:` -- see [`crate::command::Command::change`]'s own doc
/// for what that ordinarily does and why `run_tool` cannot lean on the same
/// mechanism: `Command::invoke` reads a `(Kind, Op)` fixed per row and an id
/// out of that row's own *arguments*, both fixed at compile time, whereas
/// `run_tool` is one row for the whole tool catalogue, each tool shaped
/// differently. This is `run_tool`'s own copy of the fact this function
/// pairs with [`crate::events::emit_touched`], which reads the touched
/// record's id back out of what the tool's own dispatch collected, the
/// same collector every other write in this application now goes through.
///
/// Before this existed, a mail write made through `run_tool` -- an MCP
/// archive, a scheduled auto-draft, `send_draft`'s own queued send --
/// raised nothing on [`Service::events`] at all, despite `run_tool` being
/// listed in `command`'s own `a_command_that_writes_says_what_it_touched`
/// test as an intentional exception on the theory that "its effect is
/// whatever tool it ran, which announces its own" -- a claim that was not
/// true until this function made it true. `everyday_server::mcp`'s module
/// doc rests its whole account of the undo window on exactly this: a change
/// event a window could act on the moment an MCP send is queued.
///
/// `None` for a read, for `respond_to_invite` (whose direct command
/// counterpart declares no `change:` of its own either -- an invitation's
/// reply lives on the message a thread already re-reads, not on a
/// [`Kind`] any list is drawn from), and for anything this table simply
/// does not yet name.
fn mail_tool_change_kind(name: &str) -> Option<(Kind, Op)> {
    match name {
        "draft_reply" | "draft_message" => Some((Kind::Draft, Op::Created)),
        "update_draft" | "send_draft" => Some((Kind::Draft, Op::Updated)),
        "mark_read" | "label_thread" | "move_thread" | "snooze_thread" | "archive_thread"
        | "trash_thread" => Some((Kind::Thread, Op::Updated)),
        _ => None,
    }
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
        assert_eq!(scope_of(Domain::Meetings), Scope::Meetings);
    }
}
