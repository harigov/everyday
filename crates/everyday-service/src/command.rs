//! The one public surface: a table of commands.
//!
//! Everything a client can ask a vault to do is an entry in this table. The
//! interface calls them, the CLI calls them, the server exposes them over a
//! socket and over TLS, and the assistant's tools are wrappers over them. There
//! is deliberately no second API: a browser extension saving an article calls
//! the same `add_item` the library's capture line calls, and gets the same
//! rules about what a lookup may overwrite.
//!
//! # A domain registers itself
//!
//! Each module under [`crate::domains`] exports one `COMMANDS` slice, and the
//! table is their concatenation. Nothing central has to be told that a new app
//! exists -- no match arm to extend, no list of names to keep in step, no map
//! from a command to the change it emits. That last one matters more than it
//! sounds: a central name-to-change map is exactly the sort of list the fifth
//! app forgets to update, and the symptom is a window that does not refresh
//! for one particular kind of write.
//!
//! # What an entry declares
//!
//! Beyond its handler: the [`Scope`] it needs, so authorisation is data rather
//! than a check each body has to remember; the [`Effect`] it has, which is the
//! same vocabulary the assistant's tools already use; the [`Kind`] it changes,
//! so the dispatcher emits the event rather than the body; and whether it is
//! `sensitive`, which is where step-up will attach when there is a domain that
//! needs it.
//!
//! # Why the arguments are a struct per command
//!
//! Because the wire is generated from them. The names in these structs are the
//! names on the wire and in the generated TypeScript client, so an argument
//! that is renamed in Rust shows up as a diff in a committed snapshot rather
//! than as a client that stopped working.

use crate::ctx::{Caller, Ctx, Scope};
use crate::error::{CommandError, CommandResult};
use crate::events::{Change, Kind, Op};
use crate::service::Service;
use everyday_core::agent::tools::Effect;
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;

/// What a command's body is, once the macro has wrapped it.
pub type Handler = fn(Arc<Service>, Ctx, Value) -> BoxFuture<'static, CommandResult<Value>>;

/// One thing a client can ask for.
pub struct Command {
    pub name: &'static str,
    pub scope: Scope,
    /// A second scope that will do instead.
    ///
    /// One command has one, and it is the reason this field exists rather
    /// than being a general facility: `search` answers over entries *and*
    /// notes, which are separate scopes on purpose, and a single required
    /// scope would mean either a notes client cannot search its notes or a
    /// journals client can read note bodies. The body narrows the search to
    /// whichever the caller actually holds.
    pub or_scope: Option<Scope>,
    pub effect: Effect,
    /// What a listener should reload after this succeeds, and what it did.
    /// `None` for a read, and for a write whose effect is invisible to any
    /// list -- deferring the auto-lock, say.
    pub change: Option<(Kind, Op)>,
    /// Requires a recent proof of the vault password, once step-up exists.
    /// Nothing sets it yet; the passwords app is what it is here for.
    pub sensitive: bool,
    /// Answers with a stream of events rather than one value. Such a command
    /// is not reachable through [`Service::call`]; see
    /// [`Service::send_message`], which is the only one so far.
    pub streams: bool,
    /// What this takes and gives back, for the generated client and for the
    /// snapshot that makes a wire change visible in review.
    pub signature: Signature,
    pub run: Handler,
}

/// A command's shape, in the terms a client generator needs.
///
/// Declared rather than derived. Deriving it would mean a JSON-schema
/// implementation on every record type in the core, which is a large
/// dependency in a crate that is deliberately small; what a client actually
/// needs is the command's name, its argument names and the TypeScript type of
/// each, and those are stated here beside the Rust struct they mirror. The
/// snapshot test in `tests/surface.rs` is what keeps the two honest.
pub struct Signature {
    /// `(name, TypeScript type, required)` for each argument.
    pub args: &'static [(&'static str, &'static str, bool)],
    /// The TypeScript type of the result. `"void"` for a command that
    /// answers with nothing.
    pub returns: &'static str,
}

/// Commands that are not somebody using the vault.
///
/// Every other command is taken as a person being present, which is what
/// defers the idle timeout on the key. These are the ones a client sends on a
/// timer or on the way in, with nobody necessarily at the keyboard:
///
/// * `poll_auto_lock` *is* the idle check. It must not reset what it reads.
/// * `status` is polled by the connection banner and read on every reconnect.
/// * `list_commands` is introspection, asked once by a generator or a client
///   working out what it is talking to.
/// * `sync_due_calendars` is the calendar app's five-minute refresh, which
///   runs for as long as that app has been opened once in this session. On a
///   vault whose screen lock is "Never" nothing ever stopped it, so it
///   deferred the key timer for ever -- the exact failure this list exists to
///   prevent, arriving from the one background timer that was not on it. A
///   refresh somebody *asked* for still counts as presence, because the press
///   that asked for it sends a `touch` of its own.
///
/// `touch` is deliberately absent: it is the command whose entire job is to
/// say somebody is here, and the interface sends it on real interaction.
const IDLE: &[&str] = &["poll_auto_lock", "status", "list_commands", "sync_due_calendars"];

impl Command {
    /// Run this command, having checked that the caller may.
    pub async fn invoke(&self, svc: Arc<Service>, ctx: Ctx, args: Value) -> CommandResult<Value> {
        match self.or_scope {
            Some(other) if ctx.holds(other) => {}
            _ => ctx.require(self.scope)?,
        }
        let origin = ctx.caller.origin().map(str::to_string);
        // A person using the vault is what defers the moment its key is
        // dropped. The assistant is not a person: its scheduler reads and
        // writes every minute of every day, and a vault with one routine on
        // it would otherwise never let go of the key whatever the timeout
        // said. This is why `Vault::read` and `Vault::write` no longer do it
        // themselves -- they cannot see who is asking, and this can.
        //
        // Nor is a *poll* a person, which is the subtler half. The window
        // asks `poll_auto_lock` every five seconds precisely to find out
        // whether the vault has been idle long enough to give up its key --
        // and a poll that deferred the timeout on its way to reading it would
        // reset the clock it was about to check, so the timeout would never
        // fire while any window was open at all. See `IDLE` for the rest.
        if !matches!(ctx.caller, Caller::Assistant(_))
            && !IDLE.contains(&self.name)
            && let Some(vault) = svc.get()
        {
            vault.touch();
        }
        let out = (self.run)(svc.clone(), ctx, args).await?;
        if let Some((kind, op)) = self.change {
            svc.events().changed(Change { kind, op, id: None, origin });
        }
        Ok(out)
    }
}

/// Deserialise a command's arguments, saying which command and which field.
///
/// `serde_json`'s own message is good and unattributed: "missing field `id`"
/// gives a client no way to find which of forty calls produced it.
pub fn parse<A: DeserializeOwned>(name: &str, raw: Value) -> CommandResult<A> {
    // A command taking no arguments is routinely called with `null` rather
    // than `{}` -- that is what an omitted argument bag looks like once it has
    // been through JSON -- and refusing it would make every no-argument call
    // site write `{}`.
    let raw = if raw.is_null() { Value::Object(Default::default()) } else { raw };
    serde_json::from_value(raw).map_err(|e| CommandError::new("invalid", format!("{name}: {e}")))
}

/// Serialise a command's result.
pub fn encode<R: Serialize>(value: R) -> CommandResult<Value> {
    serde_json::to_value(value)
        .map_err(|e| CommandError::new("internal", format!("could not encode a result: {e}")))
}

/// Build one table entry.
///
/// Written as a macro so an entry is a declaration rather than a paragraph of
/// boilerplate, and so the argument type appears exactly once. The nested `fn`
/// is what lets the whole table be a `static`: a closure would capture nothing
/// but would not coerce in const position.
#[macro_export]
macro_rules! command {
    (
        name: $name:literal,
        scope: $scope:ident,
        $(or_scope: $or_scope:ident,)?
        effect: $effect:ident,
        $(change: $kind:ident / $op:ident,)?
        $(sensitive: $sensitive:literal,)?
        $(streams: $streams:literal,)?
        args: $args:ty,
        returns: $returns:literal,
        signature: $sig:expr,
        run: $body:path $(,)?
    ) => {{
        fn run(
            svc: ::std::sync::Arc<$crate::service::Service>,
            ctx: $crate::ctx::Ctx,
            raw: ::serde_json::Value,
        ) -> ::futures::future::BoxFuture<'static, $crate::error::CommandResult<::serde_json::Value>>
        {
            ::std::boxed::Box::pin(async move {
                let args: $args = $crate::command::parse($name, raw)?;
                let out = $body(svc, ctx, args).await?;
                $crate::command::encode(out)
            })
        }
        $crate::command::Command {
            name: $name,
            scope: $crate::ctx::Scope::$scope,
            or_scope: $crate::command::or_scope!($($or_scope)?),
            effect: ::everyday_core::agent::tools::Effect::$effect,
            change: $crate::command::change!($($kind / $op)?),
            sensitive: $crate::command::flag!($($sensitive)?),
            streams: $crate::command::flag!($($streams)?),
            signature: $crate::command::Signature { args: $sig, returns: $returns },
            run,
        }
    }};
}

/// `Some((kind, op))` when the entry named one, `None` when it did not.
#[macro_export]
#[doc(hidden)]
macro_rules! or_scope {
    () => {
        None
    };
    ($scope:ident) => {
        Some($crate::ctx::Scope::$scope)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! change {
    () => {
        None
    };
    ($kind:ident / $op:ident) => {
        Some(($crate::events::Kind::$kind, $crate::events::Op::$op))
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! flag {
    () => {
        false
    };
    ($v:literal) => {
        $v
    };
}

pub use crate::{change, flag, or_scope};

/// Every command, in the order the domains are listed.
///
/// Looked up by name with a linear scan, which is nothing beside the work any
/// of them does and keeps the table something that can be read, diffed and
/// printed.
pub fn catalog() -> &'static [&'static Command] {
    crate::domains::catalog()
}

pub fn find(name: &str) -> Option<&'static Command> {
    catalog().iter().copied().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_command_is_declared_twice() {
        let mut seen = std::collections::HashSet::new();
        for command in catalog() {
            assert!(seen.insert(command.name), "{} is declared twice", command.name);
        }
    }

    #[test]
    fn a_read_never_claims_to_change_anything() {
        for command in catalog() {
            if matches!(command.effect, Effect::Read) {
                assert!(
                    command.change.is_none(),
                    "{} is a read and announces a change",
                    command.name
                );
            }
        }
    }

    #[test]
    fn a_command_that_writes_says_what_it_touched() {
        // The rule that keeps live refresh honest: if a write does not name a
        // kind, no other window learns about it. The exceptions are listed
        // rather than inferred, so adding one is a decision somebody made.
        const INVISIBLE: &[&str] = &[
            // Defers the moment the key is dropped. Changes nothing anybody
            // draws.
            "touch",
            // Answered by the `lockState` event instead, which every client
            // has to act on rather than merely reload a list for.
            "unlock",
            "lock",
            // Answers a question -- is this the right password -- and changes
            // nothing at all, including the lock state.
            "verify_password",
            "poll_auto_lock",
            // Housekeeping over storage, not over records.
            "collect_garbage",
            "flush",
            // Writes a blob. Invisible until an item references it, and the
            // save that does the referencing announces itself.
            "fetch_image",
            // Its effect is whatever tool it ran, which announces its own.
            "run_tool",
            // Answers a question a turn is parked on. What follows is the
            // turn's own writes, each of which announces itself.
            "confirm_tool_call",
            // The one write that touches many kinds at once. A single
            // `change:` would name one of them and leave every other list
            // stale, so it emits one event per kind an imported app could
            // have moved. See `domains::transfer::kinds_of`.
            "run_import",
        ];
        for command in catalog() {
            if command.effect.is_write() && !INVISIBLE.contains(&command.name) {
                assert!(
                    command.change.is_some(),
                    "{} writes but announces nothing; add a `change:` or list it in INVISIBLE",
                    command.name
                );
            }
        }
    }

    #[test]
    fn arguments_missing_from_a_call_name_the_command() {
        #[derive(Debug, serde::Deserialize)]
        struct Args {
            #[allow(dead_code)]
            id: String,
        }
        let e = parse::<Args>("get_entry", serde_json::json!({})).unwrap_err();
        assert_eq!(e.code, "invalid");
        assert!(e.message.contains("get_entry"), "{}", e.message);
    }

    #[test]
    fn a_command_with_no_arguments_may_be_called_with_null() {
        #[derive(Debug, serde::Deserialize)]
        struct Nothing {}
        assert!(parse::<Nothing>("status", Value::Null).is_ok());
    }
}
