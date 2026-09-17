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
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::service::Service;
use everyday_core::agent::tools::Effect;
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;

/// What a command's body is, once the macro has wrapped it.
pub type Handler = fn(Arc<Service>, Ctx, Value) -> BoxFuture<'static, CommandResult<Outcome>>;

/// What a generated `run` function actually answers with, before
/// [`Command::invoke`] separates the value a caller sees from the id or ids
/// that go on the [`Change`] it raises.
///
/// # Why the id travels this way rather than in the result
///
/// The obvious place to look for "what did this write" is the JSON a command
/// returns, and [`encode`] is right there to read it back out of -- but it is
/// the wrong place. Every `save_*` and `delete_*` command in the table
/// answers `void`: the record a save wrote is the one the caller already
/// had, echoing it back would be wasted bytes on every save in the
/// application, and a delete has nothing left to echo. What every one of
/// them *does* have is arguments that already name the record -- a `Save*`
/// struct wraps the record itself, whose id the core minted before the
/// client ever saw it; a `*Ref` struct wrapped by a delete is nothing but an
/// id. So the macro reads the id out of the typed arguments it has already
/// parsed, immediately before handing them to the command's body, rather
/// than out of a result that in most cases does not carry it.
///
/// This is why `id:` and `ids:` in the [`command!`] table take a function of
/// `&$args`, not of the result: the args are what a save or a delete
/// commands actually has an id in hand for, and reading them costs nothing
/// extra -- the macro's generated `run` already deserialised them once, and
/// this borrows that same value before moving it into the body.
///
/// Declaring `id:` or `ids:` is opt-in per command rather than derived from
/// the argument type automatically, because automatic derivation would need
/// either a trait every argument struct in the table implements -- read
/// commands and batch commands included, for a fact only a handful of them
/// have -- or a naming convention over field names that a `Save*` struct
/// would have to keep matching for ever. A two-line closure beside the
/// command it describes is less machinery than either, and is exactly as
/// visible in review as the `change:` line right beside it.
pub struct Outcome {
    pub value: Value,
    pub id: Option<String>,
    pub ids: Vec<String>,
}

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
    /// Deserialise a raw args object against the real struct, discarding the
    /// result.
    ///
    /// `signature` above is written by hand beside the struct it describes --
    /// see the module doc on why -- and the two are free to disagree about
    /// which arguments are required: a field can grow `#[serde(default)]`
    /// without anyone remembering to loosen the `true` beside it here. This
    /// is what lets `tests/surface.rs` notice, by asking the struct itself
    /// rather than a second hand-written description of it. `run` cannot
    /// answer the same question without a live `Service`, a `Ctx` and an
    /// async runtime to poll it -- none of which two pieces of Rust agreeing
    /// with each other should need.
    pub check_args: fn(Value) -> Result<(), String>,
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

/// [`Effect`], spelled the way every snapshot on the wire already agrees to
/// spell it.
///
/// `Effect` lives in `everyday_core` beside the tools it classifies, and
/// nothing there needs a string form of it -- only the handful of places that
/// put a command or a tool on the wire do. This used to be written out three
/// times by hand, once each in `domains::meta`, `tests/surface.rs` and
/// `tests/mcp.rs`, which is exactly the shape of drift a fourth `Effect`
/// variant would have hit: three edits that have to land together, made by
/// whoever remembered all three existed.
pub fn effect_name(effect: Effect) -> &'static str {
    match effect {
        Effect::Read => "read",
        Effect::Write => "write",
        Effect::Destructive => "destructive",
        // Reaches somebody outside the vault; see `Effect::Outward`'s own
        // docs in the core. No row in this table declares it -- only the
        // tool catalogue does -- but `effect_name` is shared with
        // `domains::meta::list_tools`, which reads a tool's real effect
        // straight from there.
        Effect::Outward => "outward",
    }
}

/// One command, as a client generator or an introspecting caller sees it.
///
/// Built by [`describe`] and used for two things that used to build this
/// shape independently: `list_commands`, which answers with it at runtime,
/// and the snapshot in `tests/surface.rs`, which is the wire every generated
/// client is built from. The two had quietly grown different shapes --
/// `changes` a bare kind name in one and `{kind, op}` in the other, `orScope`
/// present in only one of them -- because nothing made changing one change
/// the other.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: &'static str,
    pub scope: &'static str,
    pub or_scope: Option<&'static str>,
    pub effect: &'static str,
    pub sensitive: bool,
    pub streams: bool,
    pub changes: Option<ChangeInfo>,
    pub args: Vec<ArgInfo>,
    pub returns: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgInfo {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub required: bool,
}

/// What a listener should reload after a write, and what it did.
///
/// Serialised through [`Kind`] and [`Op`]'s own `Serialize` rather than
/// matched out by hand into a string: they already know how to spell
/// themselves on the wire, which is what let the twenty-arm match this
/// replaced go quietly out of step the day a twentieth [`Kind`] arrived.
#[derive(Debug, Serialize)]
pub struct ChangeInfo {
    pub kind: Kind,
    pub op: Op,
}

/// Everything [`list_commands`](crate::domains::meta) and
/// `tests/surface.rs`'s snapshot say about one command, computed once.
///
/// A command only knows itself as `scope`, `effect`, a `(Kind, Op)` pair and
/// so on -- the shapes those types want in Rust, not the strings and objects
/// a client reads off the wire. This is the one place that turns one into the
/// other, so the two readers of it cannot disagree about what a command looks
/// like without disagreeing with this function instead.
pub fn describe(command: &Command) -> CommandInfo {
    CommandInfo {
        name: command.name,
        scope: command.scope.as_str(),
        or_scope: command.or_scope.map(Scope::as_str),
        effect: effect_name(command.effect),
        sensitive: command.sensitive,
        streams: command.streams,
        changes: command.change.map(|(kind, op)| ChangeInfo { kind, op }),
        args: command
            .signature
            .args
            .iter()
            .map(|(name, ty, required)| ArgInfo { name, ty, required: *required })
            .collect(),
        returns: command.signature.returns,
    }
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
        // See `crate::touched`'s own doc: everything `blocking` does inside
        // `self.run`, however many times it calls it, lands in `touched`
        // here -- collected on whichever blocking-pool thread actually did
        // the writing, carried back across each `.await` by the task-local
        // scope this opens, never by anything thread-affine.
        let (result, touched) = crate::touched::scope((self.run)(svc.clone(), ctx, args)).await;
        // Read only by `assert_declared_matches_touched`, which a release
        // build compiles away entirely -- see that function's own doc. This
        // keeps `touched` from being an unused binding there instead of
        // naming it `_touched`, which would read as "nobody wants this" in
        // the build that actually does.
        #[cfg(not(any(debug_assertions, test)))]
        let _ = &touched;
        let out = result?;
        if let Some((kind, op)) = self.change {
            #[cfg(any(debug_assertions, test))]
            assert_declared_matches_touched(self.name, kind, &touched, out.id.as_deref(), &out.ids);
            svc.events().changed(Change { kind, op, id: out.id, ids: out.ids, origin });
        }
        Ok(out.value)
    }
}

/// Debug/test-only: what [`Command::invoke`] declares for a write --
/// `change:`'s `Kind` and the `id`/`ids` its own args closures computed --
/// checked against what the collector actually saw land in the vault while
/// the command ran. See `docs/plans/architecture-refactor.md`'s Phase 7.
///
/// Never fails the *build*: this runs, but proves nothing, in release,
/// where `debug_assertions` is off and a vault crash is worse than a stale
/// window. It is a regression net for review and CI, not a runtime guard.
///
/// Deliberately one-directional -- every id the command declares must be
/// among what was touched, but the collector is allowed to have seen *more*.
/// That asymmetry is not slack in the check; it is what a cascade, a
/// second write the command makes on purpose (`accept_proposal`'s own
/// record beside the `Proposal` it closes), or a coarser `Kind` than
/// `RecordKind` (`Kind::Thread` covers a mail message and an `Op` too --
/// see `Kind`'s own `TryFrom` impls) actually look like from here. A
/// command that touched *nothing* the declaration named, by contrast, is
/// always a bug: either the declaration is wrong, or this vault method
/// still needs its own `wrote` call.
#[cfg(any(debug_assertions, test))]
fn assert_declared_matches_touched(
    name: &str,
    kind: Kind,
    touched: &[(everyday_core::record::RecordKind, String)],
    id: Option<&str>,
    ids: &[String],
) {
    // Not a record at all -- the vault's own settings, or a supervisor
    // task's status -- so there is nothing in `touched` that could ever
    // name one. See `Kind`'s own `TryFrom<Kind> for RecordKind`.
    if matches!(kind, Kind::Settings | Kind::BackgroundTask) {
        return;
    }
    if KNOWN_MISMATCHES.contains(&name) {
        return;
    }

    let relevant: Vec<&str> = touched
        .iter()
        .filter(|(k, _)| crate::events::kind_covers(kind, *k))
        .map(|(_, id)| id.as_str())
        .collect();

    if let Some(id) = id {
        assert!(
            relevant.contains(&id),
            "{name}: declares {kind:?}/{id:?}, but the collector never saw a matching write \
             (touched: {touched:?})"
        );
    }
    for wanted in ids {
        assert!(
            relevant.contains(&wanted.as_str()),
            "{name}: declares {kind:?} for {wanted}, but the collector never saw a matching \
             write (touched: {touched:?})"
        );
    }
    if id.is_none() && ids.is_empty() {
        assert!(
            !relevant.is_empty(),
            "{name}: declares {kind:?} but the collector saw nothing of that kind at all \
             (touched: {touched:?})"
        );
    }
}

/// Commands whose declared `change:` this assertion cannot check against the
/// collector, each for a reason recorded beside it rather than in this list
/// alone -- see the doc on the vault method or the command each one names.
#[cfg(any(debug_assertions, test))]
const KNOWN_MISMATCHES: &[&str] = &[
    // Declares `Role/Created` unconditionally because it cannot know ahead
    // of running whether the vault already has roles -- `Vault::seed_roles`
    // is a no-op, touching nothing, on every call after the first. This
    // command has fired its event on an empty seed since long before this
    // phase; the assertion is what is new, not the behaviour.
    "seed_roles",
    // Declares `Thread/Updated` with the clicked threads' own ids, but its
    // body (`Vault::correct_mail_category`) never touches a `Thread` row at
    // all: it rewrites the sender's `CategoryRules` (no `RecordKind` of its
    // own) and sweeps `MailMessage.category` for every message from that
    // sender, which can be a different, larger set than the threads a
    // person actually clicked. The declared ids are the plan's own choice
    // -- "as a batch, to that sender's existing threads" -- not a byproduct
    // of what got written; see `set_thread_category`'s own doc.
    "set_thread_category",
    // Declares `Draft/Created` with no id at all -- unlike `new_journal`,
    // `new_task` and every other `new_*`, which are plain reads that mint a
    // record in memory for the caller to fill in and `save_*` later.
    // `new_draft` fires its event without ever writing one: nothing calls
    // `Vault::save_draft` until the person's own first autosave does. The
    // coarse, id-less `change:` is what tells a drafts list to show the
    // in-progress compose window before that first save lands, and has
    // fired on every call since long before this phase.
    "new_draft",
    // "An empty list means every pending one" -- `Vault::mark_proposals_seen`
    // takes the same `&[ProposalId]` its `ids:` closure reads straight off
    // the args, so calling it with none named touches every pending
    // proposal in the store but records nothing here: there is no id in
    // hand to `wrote` for "all of them" without a second read this method
    // has no reason to make otherwise. `run_routine`'s `mark_runs_seen`
    // shares the exact shape (see that command's own doc) but happens not
    // to be exercised empty by any test that also asserts here.
    "mark_proposals_seen",
    // Declares `RoutineRun/Created` unconditionally, but its body refuses to
    // queue a second run behind one that has not finished yet -- "pressing
    // it twice does not pay for two model calls" -- and answers the
    // already-waiting run instead of minting and saving a new one. Nothing
    // is touched on that path, the same shape `seed_roles` already
    // documents above.
    "run_routine",
];

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
    serde_json::from_value(raw)
        .map_err(|e| CommandError::new(codes::INVALID, format!("{name}: {e}")))
}

/// Serialise a command's result.
pub fn encode<R: Serialize>(value: R) -> CommandResult<Value> {
    serde_json::to_value(value)
        .map_err(|e| CommandError::new(codes::INTERNAL, format!("could not encode a result: {e}")))
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
        $(id: $idfn:expr,)?
        $(ids: $idsfn:expr,)?
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
        ) -> ::futures::future::BoxFuture<'static, $crate::error::CommandResult<$crate::command::Outcome>>
        {
            ::std::boxed::Box::pin(async move {
                let args: $args = $crate::command::parse($name, raw)?;
                // Read before the body consumes `args` -- see `Outcome`'s
                // doc for why this, rather than the result, is where a
                // save or a delete's id actually lives.
                let id = ($crate::command::id_fn!($($idfn)?))(&args);
                let ids = ($crate::command::ids_fn!($($idsfn)?))(&args);
                let out = $body(svc, ctx, args).await?;
                ::std::result::Result::Ok($crate::command::Outcome {
                    value: $crate::command::encode(out)?,
                    id,
                    ids,
                })
            })
        }
        fn check_args(raw: ::serde_json::Value) -> ::std::result::Result<(), ::std::string::String> {
            ::serde_json::from_value::<$args>(raw).map(|_| ()).map_err(|e| e.to_string())
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
            check_args,
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

/// A closure that reads the one id out of a command's parsed arguments, or
/// the default that says there is none -- see [`Outcome`] for why arguments
/// rather than a result.
#[macro_export]
#[doc(hidden)]
macro_rules! id_fn {
    () => {
        |_: &_| -> ::std::option::Option<::std::string::String> { ::std::option::Option::None }
    };
    ($f:expr) => {
        $f
    };
}

/// The batch equivalent of [`id_fn!`].
#[macro_export]
#[doc(hidden)]
macro_rules! ids_fn {
    () => {
        |_: &_| -> ::std::vec::Vec<::std::string::String> { ::std::vec::Vec::new() }
    };
    ($f:expr) => {
        $f
    };
}

pub use crate::{change, flag, id_fn, ids_fn, or_scope};

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
            // Writes a blob and updates one message's own `Body.parts`
            // entry to name it -- on `fetch_image`'s own reasoning above.
            // Neither a message nor a thread's clear columns change, so
            // there is no `Kind` for this to announce; the caller already
            // has the updated `MailAttachment` back in the command's own
            // result and patches its copy of the thread directly.
            "fetch_attachment",
            // Its effect is whatever tool it ran, which announces its own
            // -- literally, now: `domains::meta::run_tool` raises the
            // matching `Change` itself, off a table keyed by tool name
            // (`domains::meta::mail_tool_change_kind`), because this row's own
            // `(Kind, Op)` cannot be fixed at the table the way every other
            // row's can. Listed here anyway, not given a `change:`, because
            // this test checks the table `Command::invoke` reads to decide
            // *whether* to raise one -- `self.change`, always `None` for
            // this row -- and that mechanism genuinely does not apply to a
            // row whose actual effect depends on an argument rather than on
            // which row it is.
            "run_tool",
            // Answers a question a turn is parked on. What follows is the
            // turn's own writes, each of which announces itself.
            "confirm_tool_call",
            // The one write that touches many kinds at once. A single
            // `change:` would name one of them and leave every other list
            // stale, so it emits one event per kind an imported app could
            // have moved. See `domains::transfer::kinds_of`.
            "run_import",
            // OAuth sign-in. Its writes are entirely inside
            // `crate::signin::SignIns` -- a loopback socket, a background
            // task, a map of tokens waiting to be claimed -- none of which
            // is a vault record any list is drawn from. The moment that
            // changes something a list can show is `save_account`, in the
            // accounts domain, and that command names its own `change:`.
            "begin_oauth_sign_in",
            "cancel_oauth_sign_in",
            // Mail sync. `sync_account` only starts or nudges a background
            // task -- see `crate::mailsync::wiring` -- and `rebuild_mail_index`
            // rewrites the search index, a derived structure no list is
            // drawn from; neither touches a vault record a `Kind` names.
            "sync_account",
            "rebuild_mail_index",
            // The one-off categorisation backfill: like `run_import`, it can
            // touch every thread of an account (or every account), and a
            // single `change:` would name one thread and leave the rest of
            // the list stale. Unlike `run_import`, there is no small set of
            // `Kind`s to enumerate instead -- see its own doc comment in
            // `domains::mail`.
            "recategorize_mail",
            // Answers a calendar invitation. `id`/`ids` can only ever read
            // `RespondToInvite`'s own arguments, which name a message, not
            // the thread a list actually redraws for -- the thread id is
            // only known once the handler has loaded the message. So, like
            // `run_import`, it emits its own `Kind::Thread` by hand once it
            // has that id, rather than through a `change:` this table could
            // declare ahead of running.
            "respond_to_invite",
            // One chunk of a call still being recorded, roughly once every
            // thirty seconds per track. Nothing draws a list from a single
            // chunk landing -- the pill showing a call is live reads
            // `meeting_status`, not a `Kind::Recording` change -- so this
            // stays quiet while `begin_recording`, `finish_recording`,
            // `retry_recording`, `discard_recording` and `delete_recording`
            // announce `Kind::Recording` for the moments a list actually
            // needs to notice, and the pipeline's own background stage
            // moves (see `meeting::pipeline::fail` and
            // `do_summarise_and_write`) raise it by hand once a call ends
            // in `Done` or `Failed`.
            "append_recording_chunk",
            // "Not now" writes only session state (see
            // `Service::meeting_offer_seen`), which is not a vault record at
            // all; "Never for this meeting" does write `skipped_series` into
            // `MeetingSettings`, but the caller already dropped the offer
            // from its own list on the strength of the press, the same way
            // `dismissOffer` in `meetings.svelte.ts` does before this
            // command's answer is even back, so there is no list left
            // waiting on a `Settings` change to notice it.
            "dismiss_meeting_offer",
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
