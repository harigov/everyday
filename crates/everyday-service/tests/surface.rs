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

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("surface.json")
}

/// The catalogue as JSON, in a stable order.
fn current() -> String {
    let mut commands: Vec<serde_json::Value> = everyday_service::command::catalog()
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "scope": c.scope.as_str(),
                "effect": match c.effect {
                    everyday_core::agent::tools::Effect::Read => "read",
                    everyday_core::agent::tools::Effect::Write => "write",
                    everyday_core::agent::tools::Effect::Destructive => "destructive",
                },
                "sensitive": c.sensitive,
                "streams": c.streams,
                "changes": c.change.map(|(kind, op)| {
                    serde_json::json!({
                        "kind": serde_json::to_value(kind).unwrap(),
                        "op": serde_json::to_value(op).unwrap(),
                    })
                }),
                "args": c.signature.args.iter().map(|(name, ty, required)| {
                    serde_json::json!({ "name": name, "type": ty, "required": required })
                }).collect::<Vec<_>>(),
                "returns": c.signature.returns,
            })
        })
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
    let path = snapshot_path();
    let current = current();

    if std::env::var("UPDATE_SURFACE").is_ok() {
        std::fs::write(&path, &current).expect("could not write the surface snapshot");
        return;
    }

    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    if recorded != current {
        panic!(
            "the command surface has changed.\n\n\
             This is the wire that every client is generated from, so the change is worth \
             looking at rather than accepting:\n\
             * removing a command, or an argument, or narrowing one, needs PROTOCOL bumped in \
               `service.rs`;\n\
             * adding a command, or an optional argument, does not.\n\n\
             Then: UPDATE_SURFACE=1 cargo test -p everyday-service --test surface\n\
             and:  npm --prefix ui run gen:api\n"
        );
    }
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
