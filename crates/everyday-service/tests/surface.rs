//! The command surface, written down.
//!
//! `surface.json` is the wire: every command, the scope it needs, what it
//! takes and what it gives back. It is committed, this test compares the
//! catalogue against it, and `make lint` runs the test -- so renaming an
//! argument or dropping a command shows up as a reviewable diff rather than as
//! a client that stopped working three weeks later.
//!
//! It is also what `ui/scripts/gen-api.mjs` reads to generate the typed client
//! the interface uses, which is why the file is data rather than an assertion
//! buried in a test.
//!
//! To accept a change: `UPDATE_SURFACE=1 cargo test -p everyday-service`, then
//! `npm --prefix ui run gen:api`, and read both diffs.

use std::path::PathBuf;

#[allow(dead_code)]
mod support;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("surface.json")
}

/// The catalogue as JSON, in a stable order.
fn current() -> String {
    let mut commands: Vec<serde_json::Value> = everyday_service::command::catalog()
        .iter()
        .map(|c| serde_json::to_value(everyday_service::command::describe(c)).unwrap())
        .collect();
    // Sorted, so reordering the domains is not a wire change.
    commands.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    let doc = serde_json::json!({
        "protocol": everyday_service::PROTOCOL,
        "commands": commands,
    });
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())
}

#[test]
fn the_command_surface_is_what_was_written_down() {
    support::compare(
        &snapshot_path(),
        &current(),
        "the command surface has changed.\n\n\
         This is the wire that every client is generated from, so the change is worth \
         looking at rather than accepting:\n\
         * removing a command, or an argument, or narrowing one, needs PROTOCOL bumped in \
           `service.rs`;\n\
         * adding a command, or an optional argument, does not.",
        "UPDATE_SURFACE=1 cargo test -p everyday-service --test surface\n\
         and:  npm --prefix ui run gen:api",
    );
}

/// Nothing on the wire may be spelled in snake_case.
///
/// Every argument crosses to JavaScript, where the convention is camelCase and
/// where `kind_id` would be silently `undefined` rather than an error. The
/// serde attribute on each argument struct is what makes this true; this is
/// what notices when one is forgotten.
#[test]
fn every_argument_is_spelled_the_way_a_client_will_send_it() {
    for command in everyday_service::command::catalog() {
        for (name, _, _) in command.signature.args {
            assert!(
                !name.contains('_'),
                "{}: argument {name:?} is snake_case; the wire is camelCase",
                command.name
            );
        }
    }
}

/// A value that satisfies `type` without knowing the Rust type behind it.
///
/// Used to fill in every argument of a command at once, so that removing one
/// of them is the *only* difference from a JSON object already known to
/// deserialise. `None` for anything whose shape this cannot guess -- a
/// domain record like `Note` or `Task`, or a `string` that is actually a
/// timestamp underneath -- which is why the test below checks a placeholder
/// object against the real struct before trusting anything it says: a wrong
/// guess fails there, honestly, rather than blaming the wrong argument.
fn placeholder(ty: &str) -> Option<serde_json::Value> {
    // `null` satisfies `Option<T>` whatever `T` is, so a nullable argument
    // never needs its own case below.
    if ty.ends_with(" | null") {
        return Some(serde_json::Value::Null);
    }
    // An empty array satisfies `Vec<T>` for the same reason: nothing about
    // *which* element type it holds matters to whether the field is present.
    if ty.ends_with("[]") {
        return Some(serde_json::json!([]));
    }
    match ty {
        "string" => Some(serde_json::json!("x")),
        "number" => Some(serde_json::json!(1)),
        "boolean" => Some(serde_json::json!(true)),
        "unknown" => Some(serde_json::Value::Null),
        // Every id on the wire is `#[serde(transparent)]` over a `Uuid` --
        // see `everyday_core::id::typed_id!` -- so any well-formed one does,
        // whichever kind of id it actually is.
        _ if ty.ends_with("Id") => Some(serde_json::json!("00000000-0000-0000-0000-000000000000")),
        // A query struct defaults every field of its own -- the same fact
        // that let five of these arguments go wrongly required in the first
        // place -- so `{}` is a real value rather than a guess.
        _ if ty.ends_with("Query") => Some(serde_json::json!({})),
        _ => None,
    }
}

/// The class of bug five arguments had at once: `signature` calling an
/// argument required while the struct behind it had quietly grown
/// `#[serde(default)]`, so a caller who left it out -- exactly what the
/// signature promised it could do -- got a deserialisation error instead.
///
/// `signature` is written by hand beside the struct it mirrors rather than
/// derived from it -- see the doc on [`everyday_service::Signature`] for why
/// -- and a hand-written fact drifts from the code it describes precisely
/// when nothing makes touching one touch the other. This asks the struct
/// itself, through [`Command::check_args`](everyday_service::command::Command),
/// rather than trusting a second description of it.
///
/// A command is skipped, rather than failed, when [`placeholder`] cannot
/// build a JSON object that the struct accepts with nothing missing: a
/// required argument this cannot guess a shape for (`new_block`'s
/// `start`, a `jiff::Timestamp` under a `"string"` on the wire) would
/// otherwise make every argument beside it look required too, which is a
/// false alarm rather than a finding.
#[test]
fn a_signature_says_what_the_struct_actually_requires() {
    for command in everyday_service::command::catalog() {
        let mut complete = serde_json::Map::new();
        let mut every_required_argument_has_a_placeholder = true;
        for (name, ty, required) in command.signature.args {
            match placeholder(ty) {
                Some(value) => {
                    complete.insert((*name).to_string(), value);
                }
                None if *required => every_required_argument_has_a_placeholder = false,
                None => {}
            }
        }
        if !every_required_argument_has_a_placeholder {
            continue;
        }
        // The object this builds is a real one, not an assumption: if the
        // struct refuses it, the placeholders are wrong for this command --
        // most often a `"string"` that is actually a date or a timestamp --
        // and nothing below can be trusted, so this command is left alone
        // rather than blaming whichever argument happened to be under test.
        if (command.check_args)(serde_json::Value::Object(complete.clone())).is_err() {
            continue;
        }

        for (name, _, required) in command.signature.args {
            let mut object = complete.clone();
            object.remove(*name);
            let outcome = (command.check_args)(serde_json::Value::Object(object));
            let missing_this =
                outcome.as_ref().is_err_and(|e| e.contains(&format!("missing field `{name}`")));

            if *required {
                assert!(
                    missing_this,
                    "{}: `{name}` is declared required but the struct accepts leaving it out \
                     ({outcome:?}); add `#[serde(default)]` there or `required: false` here",
                    command.name
                );
            } else {
                assert!(
                    !missing_this,
                    "{}: `{name}` is declared optional but the struct still requires it -- \
                     add `#[serde(default)]`",
                    command.name
                );
            }
        }
    }
}
